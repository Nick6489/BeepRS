//! Decode the Opus files once, then mix a bed, a beep, and one foreground sound.
//! The output device never sees a container format.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicU32, Ordering},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{CodecRegistry, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia_adapter_libopus::OpusDecoder;

const BEEP: usize = 0;
const BED: usize = 1;
const INTRO: usize = 2;
const DIE_FIRST: usize = 3;

static DIE_CURSOR: AtomicU32 = AtomicU32::new(1);

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
    then_beep: bool,
}

pub struct Audio {
    _stream: cpal::Stream,
    mixer: std::sync::Arc<Mutex<Mixer>>,
    pub sounds_dir: PathBuf,
}

struct Mixer {
    clips: Vec<Vec<[f32; 2]>>,
    bed: Voice,
    beep: Voice,
    shot: Voice,
}

impl Audio {
    /// Decode every owned sound and open the default output device.
    /// A failure here must happen before Freshen is told the update started.
    pub fn open(sounds_dir: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device is available")?;
        let output = device.default_output_config()?;
        let rate = output.sample_rate().0;
        let channels = output.channels() as usize;
        let names = [
            "beep.opus",
            "bed.opus",
            "intro.opus",
            "die1.opus",
            "die2.opus",
            "die3.opus",
        ];
        let mut clips = Vec::with_capacity(names.len());
        for name in names {
            let decoded = decode_opus_file(&sounds_dir.join(name))?;
            clips.push(to_device(&decoded, rate));
        }
        let mixer = std::sync::Arc::new(Mutex::new(Mixer {
            clips,
            bed: Voice::idle(BED, 0.22, true),
            beep: Voice::idle(BEEP, 0.55, true),
            shot: Voice::idle(INTRO, 0.9, false),
        }));
        let callback_mixer = mixer.clone();
        let config = output.config();
        let stream = match output.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _| fill(data, channels, &callback_mixer),
                |_| {},
                None,
            )?,
            cpal::SampleFormat::I16 => {
                let callback_mixer = mixer.clone();
                device.build_output_stream(
                    &config,
                    move |data: &mut [i16], _| {
                        let mut samples = [0.0f32; 8];
                        let width = channels.min(samples.len());
                        for chunk in data.chunks_mut(channels) {
                            fill(&mut samples[..width], width, &callback_mixer);
                            for (slot, sample) in chunk.iter_mut().zip(samples) {
                                *slot = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                            }
                        }
                    },
                    |_| {},
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
            sounds_dir,
        })
    }

    pub fn start_game(&self, intro: bool) {
        let mut mixer = lock(&self.mixer);
        mixer.bed.restart();
        mixer.beep.active = false;
        mixer.shot.active = false;
        mixer.shot.then_beep = false;
        if intro {
            mixer.shot.clip = INTRO;
            mixer.shot.gain = 0.95;
            mixer.shot.then_beep = true;
            mixer.shot.restart();
        } else {
            mixer.beep.restart();
        }
    }

    pub fn stop(&self) {
        let mut mixer = lock(&self.mixer);
        mixer.bed.active = false;
        mixer.beep.active = false;
        mixer.shot.active = false;
        mixer.shot.then_beep = false;
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

    /// Start one of the three death sounds. Returns false while the intro or
    /// another death is still playing.
    pub fn destroy_alien(&self) -> bool {
        let mut mixer = lock(&self.mixer);
        if mixer.shot.active {
            return false;
        }
        if !mixer.beep.active {
            return false;
        }
        let which = DIE_CURSOR.fetch_add(1, Ordering::Relaxed) as usize % 3;
        mixer.beep.active = false;
        mixer.shot.clip = DIE_FIRST + which;
        mixer.shot.gain = 0.95;
        mixer.shot.then_beep = true;
        mixer.shot.restart();
        true
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
            then_beep: false,
        }
    }

    fn restart(&mut self) {
        self.frame = 0;
        self.active = true;
    }
}

impl Mixer {
    fn frame(&mut self) -> [f32; 2] {
        let bed = sample_voice(&self.clips, &mut self.bed);
        let shot = sample_voice(&self.clips, &mut self.shot);
        if !self.shot.active && self.shot.then_beep {
            self.shot.then_beep = false;
            self.beep.restart();
        }
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

fn fill(output: &mut [f32], channels: usize, mixer: &Mutex<Mixer>) {
    let Ok(mut mixer) = mixer.lock() else {
        output.fill(0.0);
        return;
    };
    if channels == 0 {
        return;
    }
    for frame in output.chunks_mut(channels) {
        let [left, right] = mixer.frame();
        if frame.len() == 1 {
            frame[0] = (left + right) * 0.5;
        } else {
            frame[0] = left;
            frame[1] = right;
            for sample in &mut frame[2..] {
                *sample = 0.0;
            }
        }
    }
}

fn lock(mixer: &Mutex<Mixer>) -> std::sync::MutexGuard<'_, Mixer> {
    mixer.lock().unwrap_or_else(|poison| poison.into_inner())
}

struct Decoded {
    interleaved: Vec<f32>,
    channels: usize,
    rate: u32,
}

fn decode_opus_file(path: &Path) -> Result<Decoded, Box<dyn std::error::Error>> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    decode_opus(&bytes).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn decode_opus(bytes: &[u8]) -> Result<Decoded, Box<dyn std::error::Error>> {
    let source = std::io::Cursor::new(bytes.to_vec());
    let stream = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("opus");
    let probed = symphonia::default::get_probe().format(
        &hint,
        stream,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .cloned()
        .ok_or("the Opus file has no audio track")?;
    let mut registry = CodecRegistry::new();
    registry.register_all::<OpusDecoder>();
    let mut decoder = registry.make(&track.codec_params, &DecoderOptions::default())?;
    let rate = track.codec_params.sample_rate.unwrap_or(48_000);
    let mut interleaved = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => break,
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = decoder.decode(&packet)?;
        append_interleaved(decoded, &mut interleaved)?;
        if interleaved.len() > 48_000 * 2 * 60 * 15 {
            return Err("sound is longer than 15 minutes".into());
        }
    }
    if interleaved.is_empty() {
        return Err("sound has no playable audio".into());
    }
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count())
        .filter(|count| *count > 0)
        .unwrap_or(1);
    Ok(Decoded {
        interleaved,
        channels,
        rate,
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
        for name in [
            "beep.opus",
            "bed.opus",
            "intro.opus",
            "die1.opus",
            "die2.opus",
            "die3.opus",
        ] {
            let decoded = decode_opus_file(&dir.join(name)).unwrap();
            assert!(decoded.interleaved.len() > decoded.channels);
            assert!(decoded.rate >= 8_000);
        }
    }
}
