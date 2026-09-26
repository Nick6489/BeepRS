//! Cache short effects and stream the music through a bounded worker-fed buffer.
//! The output device never sees a container format.

mod streaming;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use streaming::{MUSIC_FRAMES, MusicWorker, OpusFile, StreamFailure};
use symphonia::core::audio::{AudioBufferRef, Signal};

const BEEP: usize = 0;
const BED: usize = 1;
const INTRO: usize = 2;
const DIE_FIRST: usize = 3;
const DIE_COUNT: usize = 6;
const _: () = assert!(DIE_FIRST + DIE_COUNT == CLIP_NAMES.len());

const CLIP_NAMES: &[&str] = &[
    "beep.opus",
    "bed.opus",
    "intro.opus",
    "die1.opus",
    "die2.opus",
    "die3.opus",
    "die4.opus",
    "die5.opus",
    "die6.opus",
];

static DIE_ROLL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    None,
    /// Introduction finished: start the bed and the beep on the same sample.
    Begin,
    /// A death finished: the bed is already playing, so only the beep returns.
    Beep,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Intro,
    Playing,
    Dying,
}

struct Voice {
    clip: usize,
    frame: usize,
    active: bool,
    looping: bool,
    gain: f32,
    after: After,
}

pub struct Audio {
    _stream: cpal::Stream,
    mixer: std::sync::Arc<Mutex<Mixer>>,
    worker: Mutex<Option<MusicWorker>>,
    music_path: PathBuf,
    rate: u32,
    failure: Arc<StreamFailure>,
}

struct Mixer {
    clips: Vec<Vec<[f32; 2]>>,
    music: VecDeque<[f32; 2]>,
    bed: Voice,
    beep: Voice,
    shot: Voice,
}

impl Audio {
    /// Validate every sound, cache only effects, and open the output device.
    /// A failure here must happen before Freshen is told the update started.
    pub fn open(sounds_dir: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device is available")?;
        let output = device.default_output_config()?;
        let rate = output.sample_rate().0;
        let channels = output.channels() as usize;
        if rate == 0 || channels == 0 {
            return Err("the audio device returned an invalid output format".into());
        }
        let mut clips = Vec::with_capacity(CLIP_NAMES.len());
        for name in CLIP_NAMES {
            if *name == "bed.opus" {
                // Decode to completion without retaining the soundtrack. Freshen
                // must not accept an update with a broken late music packet.
                OpusFile::validate(&sounds_dir.join(name))?;
                clips.push(Vec::new());
                continue;
            }
            let decoded = decode_opus_file(&sounds_dir.join(name))?;
            clips.push(to_device(&decoded, rate));
        }
        let mixer = Arc::new(Mutex::new(Mixer::new(clips)));
        let callback_mixer = mixer.clone();
        let failure = Arc::new(StreamFailure::default());
        let callback_failure = failure.clone();
        let device_failure = failure.clone();
        let config = output.config();
        let stream = match output.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _| fill(data, channels, &callback_mixer, &callback_failure),
                move |error| device_failure.report(format!("Audio output failed: {error}")),
                None,
            )?,
            cpal::SampleFormat::I16 => {
                let callback_mixer = mixer.clone();
                device.build_output_stream(
                    &config,
                    move |data: &mut [i16], _| {
                        let Ok(mut mixer) = callback_mixer.try_lock() else {
                            data.fill(0);
                            return;
                        };
                        for chunk in data.chunks_mut(channels) {
                            let frame = next_frame(&mut mixer, &callback_failure);
                            for (channel, slot) in chunk.iter_mut().enumerate() {
                                let sample = output_channel(frame, channel, channels);
                                *slot = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                            }
                        }
                    },
                    move |error| device_failure.report(format!("Audio output failed: {error}")),
                    None,
                )?
            }
            other => {
                return Err(
                    format!("the audio device uses {other:?}, which BeepRS does not play").into(),
                );
            }
        };
        stream.play()?;
        Ok(Self {
            _stream: stream,
            mixer,
            worker: Mutex::new(None),
            music_path: sounds_dir.join("bed.opus"),
            rate,
            failure,
        })
    }

    pub fn start_game(&self, intro: bool) -> Result<(), Box<dyn std::error::Error>> {
        self.stop();
        if self.failure.failed() {
            return Err("Audio stopped working. Please restart BeepRS to reopen the audio device and sounds.".into());
        }
        let worker = MusicWorker::start(
            &self.music_path,
            self.rate,
            self.mixer.clone(),
            self.failure.clone(),
        )?;
        *lock(&self.worker) = Some(worker);
        let mut mixer = lock(&self.mixer);
        mixer.beep.active = false;
        mixer.shot.active = false;
        mixer.shot.after = After::None;
        if intro {
            mixer.bed.active = false;
            mixer.shot.clip = INTRO;
            mixer.shot.gain = 0.95;
            mixer.shot.after = After::Begin;
            mixer.shot.restart();
        } else {
            mixer.bed.restart();
            mixer.beep.restart();
        }
        Ok(())
    }

    pub fn stop(&self) {
        {
            let mut mixer = lock(&self.mixer);
            mixer.bed.active = false;
            mixer.beep.active = false;
            mixer.shot.active = false;
            mixer.shot.after = After::None;
        }
        // Never join with the mixer locked: the producer also needs that lock.
        drop(lock(&self.worker).take());
        lock(&self.mixer).music.clear();
    }

    pub fn take_error(&self) -> Option<String> {
        self.failure.take_error()
    }

    pub fn phase(&self) -> Phase {
        let mixer = lock(&self.mixer);
        if mixer.shot.active && mixer.shot.clip == INTRO {
            Phase::Intro
        } else if mixer.shot.active {
            Phase::Dying
        } else {
            Phase::Playing
        }
    }

    /// Start one of the six death sounds. Returns false while the intro or
    /// another death is still playing.
    pub fn destroy_alien(&self) -> bool {
        let mut mixer = lock(&self.mixer);
        if mixer.shot.active {
            return false;
        }
        if !mixer.beep.active {
            return false;
        }
        mixer.beep.active = false;
        mixer.shot.clip = DIE_FIRST + random_die();
        mixer.shot.gain = 0.95;
        mixer.shot.after = After::Beep;
        mixer.shot.restart();
        true
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Voice {
    fn idle(clip: usize, gain: f32, looping: bool) -> Self {
        Self {
            clip,
            frame: 0,
            active: false,
            looping,
            gain,
            after: After::None,
        }
    }

    fn restart(&mut self) {
        self.frame = 0;
        self.active = true;
    }
}

impl Mixer {
    fn new(clips: Vec<Vec<[f32; 2]>>) -> Self {
        Self {
            clips,
            music: VecDeque::with_capacity(MUSIC_FRAMES),
            bed: Voice::idle(BED, 0.22, true),
            beep: Voice::idle(BEEP, 0.55, true),
            shot: Voice::idle(INTRO, 0.9, false),
        }
    }

    fn frame(&mut self) -> [f32; 2] {
        let shot = sample_voice(&self.clips, &mut self.shot);
        if !self.shot.active {
            match self.shot.after {
                After::Begin => {
                    self.shot.after = After::None;
                    self.bed.restart();
                    self.beep.restart();
                }
                After::Beep => {
                    self.shot.after = After::None;
                    self.beep.restart();
                }
                After::None => {}
            }
        }
        let bed = if self.bed.active {
            let sample = self.music.pop_front().unwrap_or([0.0; 2]);
            [sample[0] * self.bed.gain, sample[1] * self.bed.gain]
        } else {
            [0.0; 2]
        };
        let beep = sample_voice(&self.clips, &mut self.beep);
        [
            (bed[0] + shot[0] + beep[0]).clamp(-1.0, 1.0),
            (bed[1] + shot[1] + beep[1]).clamp(-1.0, 1.0),
        ]
    }
}

fn sample_voice(clips: &[Vec<[f32; 2]>], voice: &mut Voice) -> [f32; 2] {
    if !voice.active {
        return [0.0, 0.0];
    }
    let clip = &clips[voice.clip];
    if clip.is_empty() {
        voice.active = false;
        return [0.0, 0.0];
    }
    if voice.frame >= clip.len() {
        if voice.looping {
            voice.frame = 0;
        } else {
            voice.active = false;
            return [0.0, 0.0];
        }
    }
    let sample = clip[voice.frame];
    voice.frame += 1;
    [sample[0] * voice.gain, sample[1] * voice.gain]
}

fn next_frame(mixer: &mut Mixer, failure: &StreamFailure) -> [f32; 2] {
    if failure.failed() {
        return [0.0; 2];
    }
    // No waiting, allocation, decoding, or disk access on the output thread.
    if (mixer.bed.active || mixer.shot.after == After::Begin) && mixer.music.is_empty() {
        failure.underrun();
        return [0.0; 2];
    }
    mixer.frame()
}

fn output_channel([left, right]: [f32; 2], channel: usize, channels: usize) -> f32 {
    match (channels, channel) {
        (1, _) => (left + right) * 0.5,
        (_, 0) => left,
        (_, 1) => right,
        _ => 0.0,
    }
}

fn fill(output: &mut [f32], channels: usize, mixer: &Mutex<Mixer>, failure: &StreamFailure) {
    let Ok(mut mixer) = mixer.try_lock() else {
        output.fill(0.0);
        return;
    };
    if channels == 0 {
        return;
    }
    for frame in output.chunks_mut(channels) {
        let sample = next_frame(&mut mixer, failure);
        for (channel, slot) in frame.iter_mut().enumerate() {
            *slot = output_channel(sample, channel, channels);
        }
    }
}

fn random_die() -> usize {
    let mut state = DIE_ROLL.load(Ordering::Relaxed);
    if state == 0 {
        let seeded = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
            .unwrap_or(0xA5A5_5A5A_1234_5678);
        state = seeded | 1;
    }
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    DIE_ROLL.store(state, Ordering::Relaxed);
    (state % DIE_COUNT as u64) as usize
}

fn lock<T>(mixer: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mixer.lock().unwrap_or_else(|poison| poison.into_inner())
}

struct Decoded {
    interleaved: Vec<f32>,
    channels: usize,
    rate: u32,
}

fn decode_opus_file(path: &Path) -> Result<Decoded, Box<dyn std::error::Error>> {
    let mut source = OpusFile::open(path)?;
    let mut interleaved = Vec::new();
    while let Some(frame) = source.next_frame()? {
        interleaved.extend_from_slice(&frame);
    }
    interleaved.shrink_to_fit();
    Ok(Decoded {
        interleaved,
        channels: 2,
        rate: source.rate,
    })
}
fn append_interleaved(
    decoded: AudioBufferRef<'_>,
    output: &mut Vec<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    match decoded {
        AudioBufferRef::F32(buffer) => copy_buffer(&buffer, output, |sample| sample),
        AudioBufferRef::S16(buffer) => {
            copy_buffer(&buffer, output, |sample| f32::from(sample) / 32768.0)
        }
        AudioBufferRef::S32(buffer) => {
            copy_buffer(&buffer, output, |sample| sample as f32 / i32::MAX as f32)
        }
        _ => return Err("the Opus decoder produced an unexpected sample format".into()),
    }
    Ok(())
}

fn copy_buffer<S, F>(
    buffer: &symphonia::core::audio::AudioBuffer<S>,
    output: &mut Vec<f32>,
    convert: F,
) where
    S: Copy + symphonia::core::sample::Sample,
    F: Fn(S) -> f32,
{
    let channels = buffer.spec().channels.count();
    for frame in 0..buffer.frames() {
        for channel in 0..channels {
            output.push(convert(buffer.chan(channel)[frame]));
        }
    }
}

fn to_device(decoded: &Decoded, device_rate: u32) -> Vec<[f32; 2]> {
    let frames = decoded.interleaved.len() / decoded.channels.max(1);
    let mut stereo = Vec::with_capacity(frames);
    for frame in decoded.interleaved.chunks(decoded.channels.max(1)) {
        let left = frame.first().copied().unwrap_or(0.0);
        let right = frame.get(1).copied().unwrap_or(left);
        stereo.push([left, right]);
    }
    if decoded.rate == device_rate || stereo.is_empty() {
        return stereo;
    }
    let out_len = ((stereo.len() as u64 * u64::from(device_rate)) / u64::from(decoded.rate.max(1)))
        .max(1) as usize;
    let mut output = Vec::with_capacity(out_len);
    for index in 0..out_len {
        let position = index as f64 * f64::from(decoded.rate) / f64::from(device_rate.max(1));
        let base = position.floor() as usize;
        let fraction = (position - base as f64) as f32;
        let current = stereo[base.min(stereo.len() - 1)];
        let next = stereo[(base + 1).min(stereo.len() - 1)];
        output.push([
            current[0] + (next[0] - current[0]) * fraction,
            current[1] + (next[1] - current[1]) * fraction,
        ]);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_opus_files_decode() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sounds");
        for name in CLIP_NAMES {
            if *name == "bed.opus" {
                let frames = OpusFile::validate(&dir.join(name)).unwrap();
                assert!(frames > 48_000 * 270);
                continue;
            }
            let decoded = decode_opus_file(&dir.join(name)).unwrap();
            assert!(decoded.interleaved.len() > decoded.channels);
            assert!(decoded.rate >= 8_000);
        }
    }

    #[test]
    fn intro_and_death_transitions_keep_music_and_beep_synchronized() {
        let mut mixer = Mixer::new(vec![vec![[0.1; 2]; 2]; 6]);
        mixer.music.extend([[0.2; 2]; 16]);
        mixer.shot.after = After::Begin;
        mixer.shot.restart();
        assert!((mixer.frame()[0] - 0.09).abs() < 1e-6);
        assert!((mixer.frame()[0] - 0.09).abs() < 1e-6);
        assert_eq!(mixer.music.len(), 16);
        assert!(!mixer.beep.active);
        let playing = mixer.frame();
        assert!((playing[0] - (0.2 * 0.22 + 0.1 * 0.55)).abs() < 1e-6);
        assert_eq!(mixer.beep.frame, 1);
        assert_eq!(mixer.music.len(), 15);

        mixer.beep.active = false;
        mixer.shot.clip = DIE_FIRST;
        mixer.shot.after = After::Beep;
        mixer.shot.restart();
        mixer.frame();
        mixer.frame();
        assert!(!mixer.beep.active);
        assert_eq!(mixer.music.len(), 13);
        mixer.frame();
        assert_eq!(mixer.beep.frame, 1);
        assert_eq!(mixer.music.len(), 12);
    }

    #[test]
    fn underrun_silences_all_voices_and_reports_once() {
        let mut mixer = Mixer::new(vec![vec![[0.5; 2]]; 6]);
        mixer.bed.restart();
        mixer.beep.restart();
        let failure = StreamFailure::default();
        assert_eq!(next_frame(&mut mixer, &failure), [0.0; 2]);
        assert!(failure.failed());
        assert!(failure.take_error().is_some());
        assert!(failure.take_error().is_none());
        mixer.music.push_back([0.5; 2]);
        assert_eq!(next_frame(&mut mixer, &failure), [0.0; 2]);
    }

    #[test]
    fn callback_does_not_wait_for_producer_and_clears_extra_channels() {
        let mixer = Mutex::new(Mixer::new(vec![vec![[0.1; 2]]; 6]));
        let failure = StreamFailure::default();
        let mut output = [1.0; 12];
        let guard = lock(&mixer);
        fill(&mut output, 12, &mixer, &failure);
        assert_eq!(output, [0.0; 12]);
        drop(guard);
        lock(&mixer).beep.restart();
        fill(&mut output, 12, &mixer, &failure);
        assert!(output[0] > 0.0);
        assert_eq!(&output[2..], &[0.0; 10]);
    }

    #[test]
    #[ignore = "plays audio through the default Windows output device"]
    fn live_device_start_stop_and_restart() {
        let audio = Audio::open(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sounds")).unwrap();
        // The music slot must never hold a fully decoded soundtrack.
        assert!(lock(&audio.mixer).clips[BED].is_empty());
        for intro in [false, true, false] {
            audio.start_game(intro).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
            assert!(audio.take_error().is_none());
            if !intro {
                assert!(audio.destroy_alien());
            }
            audio.stop();
            assert!(lock(&audio.worker).is_none());
            assert!(lock(&audio.mixer).music.is_empty());
        }
    }

    #[test]
    fn death_sounds_are_not_taken_in_turn() {
        let rolls: Vec<usize> = (0..80).map(|_| random_die()).collect();
        assert!(rolls.iter().all(|roll| *roll < DIE_COUNT));
        assert_ne!(
            rolls,
            (0..80).map(|index| index % DIE_COUNT).collect::<Vec<_>>()
        );
        for die in 0..DIE_COUNT {
            assert!(rolls.contains(&die), "die {die} was never chosen");
        }
    }
}
