//! Cache short effects and stream the music through a bounded worker-fed buffer.
//! The output device never sees a container format.

mod streaming;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use streaming::{MUSIC_FRAMES, MusicWorker, OpusFile, StreamFailure};
use symphonia::core::audio::{AudioBufferRef, Signal};

const BEEP: usize = 0;
const BED: usize = 1;
const INTRO: usize = 2;
const DIE_FIRST: usize = 3;
const DIE_COUNT: usize = 9;
const _: () = assert!(DIE_FIRST + DIE_COUNT == CLIP_NAMES.len());
/// The bed is eight bars of 4/4. Dividing the loop into these beats keeps the
/// beep on the record; 140 BPM is not a whole number of samples.
const BED_BEATS: u64 = 32;

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
    "die7.opus",
    "die8.opus",
    "die9.opus",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    None,
    /// Introduction finished: start the bed and the beep on the same sample.
    Begin,
    /// A death finished. The bed keeps its place, and the beep returns on the next beat.
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Follow {
    /// Introduction, a death, or stopped. Beat edges do not start the beep.
    Silent,
    /// Retrigger the beep on every beat.
    Playing,
    /// The death has finished between beats. Start on the next one.
    Waiting,
}

struct Groove {
    /// Bed samples heard since this game started the bed. Prefill does not count.
    played: u64,
    bed_frames: u64,
    file_rate: u32,
    device_rate: u32,
    follow: Follow,
}

impl Groove {
    fn new(bed_frames: u64, file_rate: u32, device_rate: u32) -> Self {
        Self {
            played: 0,
            bed_frames,
            file_rate,
            device_rate,
            follow: Follow::Silent,
        }
    }

    fn configure(&mut self, bed_frames: u64, file_rate: u32, device_rate: u32) {
        self.bed_frames = bed_frames;
        self.file_rate = file_rate;
        self.device_rate = device_rate;
    }

    fn start(&mut self) {
        self.played = 0;
        self.follow = Follow::Playing;
    }

    fn reset(&mut self) {
        self.played = 0;
        self.follow = Follow::Silent;
    }

    fn silence(&mut self) {
        self.follow = Follow::Silent;
    }

    fn wait(&mut self) {
        self.follow = Follow::Waiting;
    }

    /// True when this bed sample is a downbeat and the beep should attack.
    fn tick(&mut self) -> bool {
        let downbeat = self.is_downbeat(self.played);
        let trigger = downbeat && self.follow != Follow::Silent;
        if trigger {
            self.follow = Follow::Playing;
        }
        self.played = self.played.saturating_add(1);
        trigger
    }

    fn is_downbeat(&self, played: u64) -> bool {
        if self.bed_frames == 0 || self.file_rate == 0 || self.device_rate == 0 {
            return played == 0;
        }
        if played == 0 {
            return true;
        }
        self.beat(played) != self.beat(played - 1)
    }

    /// Floor of the rational source position, in beats of this bed loop.
    fn beat(&self, played: u64) -> u64 {
        let den = u128::from(self.device_rate) * u128::from(self.bed_frames);
        if den == 0 {
            return 0;
        }
        let num = u128::from(played) * u128::from(self.file_rate) * u128::from(BED_BEATS);
        (num / den) as u64
    }
}

struct Mixer {
    clips: Vec<Vec<[f32; 2]>>,
    music: VecDeque<[f32; 2]>,
    bed: Voice,
    beep: Voice,
    shot: Voice,
    /// Death-sound draws for this process. Empty until the first alien dies.
    deaths: DeathPicker,
    /// Sample counter for the bed. A few integers, not a rendered click track.
    groove: Groove,
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
        let mut bed_frames = 0u64;
        let mut bed_rate = 0u32;
        for name in CLIP_NAMES {
            if *name == "bed.opus" {
                // Decode to completion without retaining the soundtrack. Freshen
                // must not accept an update with a broken late music packet.
                let bed = OpusFile::validate(&sounds_dir.join(name))?;
                if bed.frames < BED_BEATS || bed.rate == 0 {
                    return Err("the music bed is too short to keep a beat".into());
                }
                bed_frames = bed.frames;
                bed_rate = bed.rate;
                clips.push(Vec::new());
                continue;
            }
            let decoded = decode_opus_file(&sounds_dir.join(name))?;
            clips.push(to_device(&decoded, rate));
        }
        let mut mixer = Mixer::new(clips);
        mixer.groove.configure(bed_frames, bed_rate, rate);
        let mixer = Arc::new(Mutex::new(mixer));
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
            mixer.groove.start();
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
            mixer.groove.reset();
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

    /// Start a death sound. Returns false while the intro or another death
    /// is still playing.
    pub fn destroy_alien(&self) -> bool {
        lock(&self.mixer).kill()
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
            // The groove retriggers this. A free loop would walk off the bed.
            beep: Voice::idle(BEEP, 0.55, false),
            shot: Voice::idle(INTRO, 0.9, false),
            deaths: DeathPicker::new(),
            groove: Groove::new(0, 0, 0),
        }
    }

    /// Start a death on the next sample. False during the introduction, a death,
    /// or the wait for the beep to catch the next beat.
    fn kill(&mut self) -> bool {
        if self.shot.active || !self.bed.active || self.groove.follow != Follow::Playing {
            return false;
        }
        self.beep.active = false;
        self.groove.silence();
        self.shot.clip = DIE_FIRST + self.deaths.pick(DIE_COUNT);
        self.shot.gain = 0.95;
        self.shot.after = After::Beep;
        self.shot.restart();
        true
    }

    fn frame(&mut self) -> [f32; 2] {
        let shot = sample_voice(&self.clips, &mut self.shot);
        if !self.shot.active {
            match self.shot.after {
                After::Begin => {
                    self.shot.after = After::None;
                    self.bed.restart();
                    self.groove.start();
                }
                After::Beep => {
                    self.shot.after = After::None;
                    self.groove.wait();
                }
                After::None => {}
            }
        }
        let bed = if self.bed.active {
            if let Some(sample) = self.music.pop_front() {
                if self.groove.tick() {
                    self.beep.restart();
                }
                [sample[0] * self.bed.gain, sample[1] * self.bed.gain]
            } else {
                [0.0; 2]
            }
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

/// Session-scoped death-sound selector.
///
/// Owns the xorshift state and the two most recent choices. Those two are
/// excluded when that still leaves a sound to play. With fewer than three
/// sounds the oldest exclusion is dropped, so a small catalog cannot spin.
struct DeathPicker {
    /// Zero until the first draw, which seeds it from the clock. A live
    /// xorshift state is never zero, because that state never changes.
    state: u64,
    /// Newest choice, then the one before it. Empty after process start.
    recent: [Option<usize>; 2],
}

impl DeathPicker {
    fn new() -> Self {
        Self {
            state: 0,
            recent: [None, None],
        }
    }

    #[cfg(test)]
    fn seeded(seed: u64) -> Self {
        Self {
            state: seed | 1,
            recent: [None, None],
        }
    }

    fn pick(&mut self, count: usize) -> usize {
        if count == 0 {
            return 0;
        }
        let ban = self.exclusions(count);
        let allowed = count - ban.iter().flatten().count();
        let choice = if allowed == 0 {
            (self.roll() % count as u64) as usize
        } else {
            let slot = (self.roll() % allowed as u64) as usize;
            (0..count)
                .filter(|candidate| !ban.contains(&Some(*candidate)))
                .nth(slot)
                .unwrap_or(0)
        };
        self.recent = [Some(choice), self.recent[0]];
        choice
    }

    /// Up to two in-range choices that can be skipped without emptying the catalog.
    fn exclusions(&self, count: usize) -> [Option<usize>; 2] {
        let mut ban = [None; 2];
        if count <= 1 {
            return ban;
        }
        let mut blocked = 0;
        for previous in self.recent {
            let Some(previous) = previous else {
                continue;
            };
            if previous >= count || ban.contains(&Some(previous)) {
                continue;
            }
            if blocked + 1 >= count {
                break;
            }
            ban[blocked] = Some(previous);
            blocked += 1;
        }
        ban
    }

    fn roll(&mut self) -> u64 {
        if self.state == 0 {
            let seeded = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos() as u64)
                .unwrap_or(0xA5A5_5A5A_1234_5678);
            self.state = seeded | 1;
        }
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }
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
                // Validate without retaining the PCM. The bed is a loop now,
                // not the old multi-minute render.
                let bed = OpusFile::validate(&dir.join(name)).unwrap();
                assert!(
                    bed.frames > 48_000,
                    "the music bed is shorter than one second"
                );
                continue;
            }
            let decoded = decode_opus_file(&dir.join(name)).unwrap();
            assert!(decoded.interleaved.len() > decoded.channels);
            assert!(decoded.rate >= 8_000);
        }
    }

    #[test]
    fn beep_attacks_on_the_bed_and_waits_out_an_offbeat_death() {
        // Equal rates, 32 beats, one beat every 8 output frames.
        let bed_frames = BED_BEATS * 8;
        let mut clips = vec![vec![[0.1; 2]; 10]; DIE_FIRST + DIE_COUNT];
        clips[BEEP] = vec![[0.5; 2]; 20];
        clips[INTRO] = vec![[0.1; 2]; 2];
        let mut mixer = Mixer::new(clips);
        mixer.groove = Groove::new(bed_frames, 1, 1);
        mixer.music.extend([[0.2; 2]; 64]);

        mixer.shot.clip = INTRO;
        mixer.shot.gain = 0.9;
        mixer.shot.after = After::Begin;
        mixer.shot.restart();
        assert!(!mixer.kill());
        assert!((mixer.frame()[0] - 0.09).abs() < 1e-6);
        assert!((mixer.frame()[0] - 0.09).abs() < 1e-6);
        assert_eq!(mixer.music.len(), 64);
        assert!(!mixer.beep.active);

        let downbeat = mixer.frame();
        assert!((downbeat[0] - (0.2 * 0.22 + 0.5 * 0.55)).abs() < 1e-6);
        assert_eq!(mixer.beep.frame, 1);
        assert_eq!(mixer.groove.played, 1);
        assert_eq!(mixer.groove.follow, Follow::Playing);
        mixer.frame();
        assert_eq!(mixer.beep.frame, 2);

        while mixer.groove.played <= 8 {
            mixer.frame();
        }
        assert_eq!(mixer.beep.frame, 1);
        assert_eq!(mixer.groove.played, 9);
        let played_at_kill = mixer.groove.played;
        assert!(mixer.kill());
        assert!(!mixer.kill());
        assert_eq!(mixer.groove.played, played_at_kill);
        assert_eq!(mixer.groove.follow, Follow::Silent);
        assert!(!mixer.beep.active);

        // The death clip is 10 samples. Crossing the next beat must not wake the beep.
        for _ in 0..10 {
            mixer.frame();
            assert!(!mixer.beep.active);
            assert_eq!(mixer.groove.follow, Follow::Silent);
        }
        mixer.frame();
        assert_eq!(mixer.groove.follow, Follow::Waiting);
        assert!(!mixer.beep.active);
        assert!(!mixer.kill());

        let mut steps = 0;
        while mixer.groove.follow == Follow::Waiting {
            mixer.frame();
            steps += 1;
            assert!(steps < 8, "the beep never caught a beat");
            if mixer.groove.follow == Follow::Waiting {
                assert!(!mixer.beep.active);
            }
        }
        assert_eq!(mixer.beep.frame, 1);
        assert_eq!(mixer.groove.played % 8, 1);
        assert!(mixer.groove.played > played_at_kill);
    }

    #[test]
    fn beep_starts_immediately_when_a_death_ends_on_a_beat() {
        let mut clips = vec![vec![[0.1; 2]; 1]; DIE_FIRST + DIE_COUNT];
        clips[BEEP] = vec![[0.5; 2]; 4];
        let mut mixer = Mixer::new(clips);
        mixer.groove = Groove::new(BED_BEATS * 8, 1, 1);
        mixer.music.extend([[0.2; 2]; 16]);
        mixer.bed.restart();
        mixer.groove.start();
        mixer.groove.silence();
        mixer.groove.played = 7;
        mixer.shot.clip = DIE_FIRST;
        mixer.shot.after = After::Beep;
        mixer.shot.restart();
        mixer.frame();
        assert!(!mixer.beep.active);
        mixer.frame();
        assert_eq!(mixer.groove.follow, Follow::Playing);
        assert_eq!(mixer.beep.frame, 1);
        assert_eq!(mixer.groove.played, 9);
    }

    #[test]
    fn bed_loop_contains_thirty_two_beats() {
        let bed = OpusFile::validate(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("sounds")
                .join("bed.opus"),
        )
        .unwrap();
        for device in [bed.rate, 44_100, 48_000] {
            let groove = Groove::new(bed.frames, bed.rate, device);
            let frames = (bed.frames * u64::from(device)).div_ceil(u64::from(bed.rate));
            let mut edges = 0u64;
            let mut previous = 0u64;
            for played in 0..frames {
                if !groove.is_downbeat(played) {
                    continue;
                }
                let beat = groove.beat(played);
                if edges > 0 {
                    assert_eq!(beat, previous + 1, "device {device} skipped a beat");
                }
                edges += 1;
                previous = beat;
            }
            assert_eq!(edges, BED_BEATS, "device {device}");
            assert!(groove.is_downbeat(frames));
            assert_eq!(groove.beat(frames), BED_BEATS);
        }
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

    fn draw_deaths(seed: u64, count: usize, draws: usize) -> Vec<usize> {
        let mut picker = DeathPicker::seeded(seed);
        (0..draws).map(|_| picker.pick(count)).collect()
    }

    fn repeats_on_a_fixed_step(samples: &[usize], count: usize) -> bool {
        if samples.len() < 2 || count < 2 {
            return false;
        }
        let step = (samples[1] + count - samples[0]) % count;
        samples
            .windows(2)
            .all(|pair| (pair[0] + step) % count == pair[1])
    }

    fn assert_organic_deaths(count: usize) {
        assert!(count >= 3, "this check needs enough sounds to skip two");
        let samples = draw_deaths(0x1234_5678, count, 256);
        assert!(samples.iter().all(|die| *die < count));
        assert!(samples.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(samples.windows(3).all(|window| window[0] != window[2]));
        assert!(!repeats_on_a_fixed_step(&samples, count));
        for die in 0..count {
            assert!(samples.contains(&die), "die {die} was never chosen");
        }
    }

    #[test]
    fn death_picker_indices_stay_inside_die_count() {
        let samples = draw_deaths(0x1234_5678, DIE_COUNT, 96);
        assert!(samples.iter().all(|die| *die < DIE_COUNT));
    }

    #[test]
    fn death_picker_skips_the_previous_two_sounds() {
        let samples = draw_deaths(0x1234_5678, DIE_COUNT, 96);
        assert!(samples.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(samples.windows(3).all(|window| window[0] != window[2]));
    }

    #[test]
    fn death_picker_is_not_round_robin() {
        let samples = draw_deaths(0x1234_5678, DIE_COUNT, 96);
        let turn: Vec<_> = (0..samples.len()).map(|index| index % DIE_COUNT).collect();
        assert_ne!(samples, turn);
        assert!(!repeats_on_a_fixed_step(&samples, DIE_COUNT));
    }

    #[test]
    fn death_picker_reaches_every_death_sound() {
        let samples = draw_deaths(0x1234_5678, DIE_COUNT, 96);
        for die in 0..DIE_COUNT {
            assert!(samples.contains(&die), "die {die} was never chosen");
        }
    }

    #[test]
    fn death_picker_keeps_working_for_a_larger_catalog() {
        assert_organic_deaths(DIE_COUNT + 3);
    }

    #[test]
    fn death_picker_stops_excluding_when_the_catalog_is_small() {
        let mut none = DeathPicker::seeded(1);
        assert_eq!((0..8).map(|_| none.pick(0)).collect::<Vec<_>>(), vec![0; 8]);

        let mut only = DeathPicker::seeded(1);
        assert!((0..8).map(|_| only.pick(1)).all(|die| die == 0));

        let two = draw_deaths(1, 2, 32);
        assert!(two.iter().all(|die| *die < 2));
        assert!(two.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(two.contains(&0) && two.contains(&1));

        // A later, smaller catalog must ignore history that no longer exists.
        let mut shrunk = DeathPicker::seeded(0x1234_5678);
        for _ in 0..4 {
            assert!(shrunk.pick(6) < 6);
        }
        let narrow: Vec<_> = (0..16).map(|_| shrunk.pick(2)).collect();
        assert!(narrow.iter().all(|die| *die < 2));
        assert!(narrow.windows(2).all(|pair| pair[0] != pair[1]));
    }
}
