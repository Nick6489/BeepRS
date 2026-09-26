//! File-backed decoding and a fixed-size music queue. Only the producer does I/O.

use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use symphonia::core::checksum::Crc32;
use symphonia::core::codecs::{CodecRegistry, Decoder, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, Monitor};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia_adapter_libopus::OpusDecoder;

use super::{Mixer, append_interleaved, lock};

// 256 KiB regardless of device rate or soundtrack duration. At 48 kHz this
// covers 683 ms. Decode in small batches without holding the mixer lock.
pub(super) const MUSIC_FRAMES: usize = 32_768;
const BATCH_FRAMES: usize = 1024;
type AudioResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
pub(super) struct StreamFailure {
    failed: AtomicBool,
    underrun: AtomicBool,
    message: Mutex<Option<String>>,
}

impl StreamFailure {
    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub(super) fn report(&self, message: String) {
        if self.failed.swap(true, Ordering::AcqRel) {
            return;
        }
        *lock(&self.message) = Some(message);
    }

    pub(super) fn underrun(&self) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            self.underrun.store(true, Ordering::Release);
        }
    }

    pub(super) fn take_error(&self) -> Option<String> {
        let message = lock(&self.message).take();
        let underrun = self.underrun.swap(false, Ordering::AcqRel);
        message.or_else(|| underrun.then(|| "The music buffer ran out of audio.".to_owned()))
    }
}

pub(super) struct OpusFile {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track: u32,
    channels: usize,
    pub(super) rate: u32,
    packet: Vec<f32>,
    cursor: usize,
    frames: u64,
    expected_frames: u64,
}

impl OpusFile {
    pub(super) fn open(path: &Path) -> AudioResult<Self> {
        Self::open_inner(path).map_err(|error| format!("{}: {error}", path.display()).into())
    }

    fn open_inner(path: &Path) -> AudioResult<Self> {
        let mut file = File::open(path)?;
        // Symphonia can recover from bad pages and reports both truncation and
        // normal EOF as UnexpectedEof. Verify every page before accepting it.
        let end_granule = validate_container(&mut file)?;
        file.rewind()?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("opus");
        let format = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )?
            .format;
        let track = format
            .default_track()
            .ok_or("the Opus file has no audio track")?;
        let mut registry = CodecRegistry::new();
        registry.register_all::<OpusDecoder>();
        let params = track.codec_params.clone();
        // The adapter applies OpusHead pre-skip. Trim the final padding using
        // the container's last granule ourselves: our shipped files omit EOS,
        // so the demuxer's gapless end trimming cannot recognize their end.
        let header = params.extra_data.as_ref().ok_or("missing Opus header")?;
        if header.len() < 19 || &header[..8] != b"OpusHead" {
            return Err("invalid Opus header".into());
        }
        let pre_skip = u16::from_le_bytes([header[10], header[11]]);
        let expected_frames = end_granule
            .checked_sub(u64::from(pre_skip))
            .ok_or("invalid Opus duration")?;
        let decoder = registry.make(&params, &DecoderOptions::default())?;
        let channels = track
            .codec_params
            .channels
            .ok_or("missing channel count")?
            .count();
        let rate = track
            .codec_params
            .sample_rate
            .ok_or("missing sample rate")?;
        if !(1..=2).contains(&channels) || rate == 0 {
            return Err("unsupported audio format".into());
        }
        let track = track.id;
        Ok(Self {
            path: path.to_owned(),
            format,
            decoder,
            track,
            channels,
            rate,
            packet: Vec::new(),
            cursor: 0,
            frames: 0,
            expected_frames,
        })
    }

    pub(super) fn next_frame(&mut self) -> AudioResult<Option<[f32; 2]>> {
        self.next_inner()
            .map_err(|error| format!("{}: {error}", self.path.display()).into())
    }

    fn next_inner(&mut self) -> AudioResult<Option<[f32; 2]>> {
        if self.frames == self.expected_frames {
            // Remaining samples in the final packet are encoder padding.
            self.cursor = self.packet.len();
        }
        while self.cursor >= self.packet.len() {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    if self.frames == 0 {
                        return Err("sound has no playable audio".into());
                    }
                    if self.frames != self.expected_frames {
                        return Err("sound ended before its declared audio was decoded".into());
                    }
                    return Ok(None);
                }
                // A reset may change tracks or formats; never silently truncate
                // the sound or reset a decoder with stale codec parameters.
                Err(error) => return Err(error.into()),
            };
            if packet.track_id() != self.track {
                continue;
            }
            if self.frames == self.expected_frames {
                return Err("audio packets follow the declared end of the sound".into());
            }
            let decoded = self.decoder.decode(&packet)?;
            if decoded.spec().rate != self.rate || decoded.spec().channels.count() != self.channels
            {
                return Err("sound changes audio format mid-stream".into());
            }
            self.packet.clear();
            append_interleaved(decoded, &mut self.packet)?;
            self.cursor = 0;
        }
        self.frames += 1;
        if self.frames > u64::from(self.rate) * 60 * 15 {
            return Err("sound is longer than 15 minutes".into());
        }
        let left = self.packet[self.cursor];
        let right = self.packet[self.cursor + self.channels - 1];
        self.cursor += self.channels;
        Ok(Some([left, right]))
    }

    pub(super) fn validate(path: &Path) -> AudioResult<u64> {
        let mut file = Self::open(path)?;
        while file.next_frame()?.is_some() {}
        Ok(file.frames)
    }
}

/// BeepRS assets are single-stream Ogg Opus files. Check page checksums,
/// sequence continuity and complete pages with at most one page in memory.
/// Legacy shipped assets omit EOS; for those, EOF at a complete page boundary
/// is accepted and the last granule supplies the intended sample count.
fn validate_container(file: &mut File) -> AudioResult<u64> {
    let mut serial = None;
    let mut sequence = 0u32;
    let mut body = Vec::new();
    let mut end_granule = None;
    loop {
        let mut header = [0u8; 27];
        if file.read(&mut header[..1])? == 0 {
            return end_granule.ok_or_else(|| "sound has no complete audio pages".into());
        }
        file.read_exact(&mut header[1..])?;
        if &header[..4] != b"OggS" || header[4] != 0 {
            return Err("invalid Ogg page header".into());
        }
        let page_serial = u32::from_le_bytes(header[14..18].try_into()?);
        let page_sequence = u32::from_le_bytes(header[18..22].try_into()?);
        if serial.is_none() {
            if header[5] & 2 == 0 {
                return Err("missing first Ogg page".into());
            }
            serial = Some(page_serial);
        } else if header[5] & 2 != 0 {
            return Err("chained Ogg streams are not supported".into());
        }
        if serial != Some(page_serial) || page_sequence != sequence {
            return Err("discontinuous Ogg stream".into());
        }
        sequence = sequence.wrapping_add(1);
        let expected_crc = u32::from_le_bytes(header[22..26].try_into()?);
        header[22..26].fill(0);
        let mut crc = Crc32::new(0);
        crc.process_buf_bytes(&header);
        let mut segments = [0u8; 255];
        let segments = &mut segments[..header[26] as usize];
        file.read_exact(segments)?;
        crc.process_buf_bytes(segments);
        body.resize(segments.iter().map(|&size| size as usize).sum(), 0);
        file.read_exact(&mut body)?;
        crc.process_buf_bytes(&body);
        if crc.crc() != expected_crc {
            return Err("Ogg page checksum mismatch".into());
        }
        let granule = u64::from_le_bytes(header[6..14].try_into()?);
        end_granule = if segments.last().is_some_and(|&size| size < 255) && granule != u64::MAX {
            Some(granule)
        } else {
            None
        };
        if header[5] & 4 != 0 {
            if file.read(&mut [0u8; 1])? != 0 {
                return Err("data follows final Ogg page".into());
            }
            return end_granule.ok_or_else(|| "incomplete final Ogg page".into());
        }
    }
}

// Linear interpolation with a rational position accumulator. Packet and
// buffer boundaries do not reset interpolation or accumulate timing drift.
struct MusicSource {
    file: OpusFile,
    rate: u32,
    phase: u64,
    current: [f32; 2],
    next: [f32; 2],
}

impl MusicSource {
    fn open(path: &Path, rate: u32) -> AudioResult<Self> {
        if rate == 0 {
            return Err("invalid output sample rate".into());
        }
        let file = OpusFile::open(path)?;
        let mut source = Self {
            file,
            rate,
            phase: 0,
            current: [0.0; 2],
            next: [0.0; 2],
        };
        source.current = source.read_looped()?;
        source.next = source.read_looped()?;
        Ok(source)
    }

    fn read_looped(&mut self) -> AudioResult<[f32; 2]> {
        if let Some(frame) = self.file.next_frame()? {
            return Ok(frame);
        }
        let file = OpusFile::open(&self.file.path)?;
        if file.rate != self.file.rate || file.channels != self.file.channels {
            return Err("music format changed while playing".into());
        }
        self.file = file;
        self.file
            .next_frame()?
            .ok_or_else(|| "music has no playable audio".into())
    }

    fn fill(&mut self, output: &mut [[f32; 2]]) -> AudioResult<()> {
        for frame in output {
            let fraction = self.phase as f32 / self.rate as f32;
            *frame = std::array::from_fn(|ch| {
                self.current[ch] + (self.next[ch] - self.current[ch]) * fraction
            });
            self.phase += u64::from(self.file.rate);
            while self.phase >= u64::from(self.rate) {
                self.phase -= u64::from(self.rate);
                self.current = self.next;
                self.next = self.read_looped()?;
            }
        }
        Ok(())
    }
}

pub(super) struct MusicWorker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MusicWorker {
    pub(super) fn start(
        path: &Path,
        rate: u32,
        mixer: Arc<Mutex<Mixer>>,
        failure: Arc<StreamFailure>,
    ) -> AudioResult<Self> {
        let mut source = MusicSource::open(path, rate)?;
        let mut batch = [[0.0; 2]; BATCH_FRAMES];
        // Prefill while voices are stopped, before making playback visible.
        for _ in 0..MUSIC_FRAMES / BATCH_FRAMES {
            source.fill(&mut batch)?;
            lock(&mixer).music.extend(batch);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("BeepRS music".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) && !failure.failed() {
                    let space = MUSIC_FRAMES - lock(&mixer).music.len();
                    if space < BATCH_FRAMES {
                        thread::park_timeout(Duration::from_millis(5));
                        continue;
                    }
                    if let Err(error) = source.fill(&mut batch) {
                        failure.report(format!("Music decoding failed: {error}"));
                        break;
                    }
                    lock(&mixer).music.extend(batch);
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for MusicWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            // Completion releases the decoder and its file before update handoff.
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{decode_opus_file, to_device};

    fn sound(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("sounds")
            .join(name)
    }

    #[test]
    fn streaming_resampling_matches_cached_effects_across_batches() {
        for name in ["beep.opus", "intro.opus"] {
            let decoded = decode_opus_file(&sound(name)).unwrap();
            for rate in [8_000, 44_100, 48_000, 96_000] {
                let reference = to_device(&decoded, rate);
                let count = 4096.min(reference.len() - 2);
                let mut source = MusicSource::open(&sound(name), rate).unwrap();
                let mut streamed = vec![[0.0; 2]; count];
                for chunk in streamed.chunks_mut(137) {
                    source.fill(chunk).unwrap();
                }
                for (actual, expected) in streamed.iter().zip(&reference) {
                    for ch in 0..2 {
                        assert!((actual[ch] - expected[ch]).abs() < 1e-5);
                    }
                }
            }
        }
    }

    #[test]
    fn loops_without_inserting_or_dropping_frames() {
        let path = sound("beep.opus");
        let decoded = decode_opus_file(&path).unwrap();
        let reference = to_device(&decoded, decoded.rate);
        let mut source = MusicSource::open(&path, decoded.rate).unwrap();
        let mut block = [[0.0; 2]; 137];
        let mut index = 0;
        while index < reference.len() * 2 + 100 {
            source.fill(&mut block).unwrap();
            for frame in block {
                assert_eq!(frame, reference[index % reference.len()]);
                index += 1;
            }
        }
    }

    #[test]
    fn rejects_truncation_and_late_corruption_before_playback() {
        let bytes = std::fs::read(sound("bed.opus")).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("broken.opus");
        std::fs::write(&path, &bytes[..bytes.len() - 50]).unwrap();
        assert!(OpusFile::validate(&path).is_err());
        let mut corrupt = bytes;
        let last = corrupt.len() - 50;
        corrupt[last] ^= 0x80;
        std::fs::write(&path, corrupt).unwrap();
        assert!(OpusFile::validate(&path).is_err());
        std::fs::write(&path, []).unwrap();
        assert!(OpusFile::validate(&path).is_err());
    }

    #[test]
    fn worker_prefills_refills_stays_bounded_and_releases_file_on_join() {
        use std::os::windows::fs::OpenOptionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("music.opus");
        std::fs::copy(sound("beep.opus"), &path).unwrap();
        let mixer = Arc::new(Mutex::new(Mixer::new(vec![])));
        let failure = Arc::new(StreamFailure::default());
        for _ in 0..3 {
            lock(&mixer).music.clear();
            let worker = MusicWorker::start(&path, 48_000, mixer.clone(), failure.clone()).unwrap();
            assert_eq!(lock(&mixer).music.len(), MUSIC_FRAMES);
            assert!(
                File::options()
                    .read(true)
                    .share_mode(0)
                    .open(&path)
                    .is_err()
            );
            lock(&mixer).music.drain(..MUSIC_FRAMES / 2);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while lock(&mixer).music.len() != MUSIC_FRAMES {
                assert!(std::time::Instant::now() < deadline);
                assert!(!failure.failed());
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(lock(&mixer).music.capacity(), MUSIC_FRAMES);
            drop(worker); // Also exercises cancellation with a full queue.
            File::options()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap();
        }
    }

    #[test]
    fn producer_reports_a_file_failure_on_loop() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("music.opus");
        std::fs::copy(sound("beep.opus"), &path).unwrap();
        let mixer = Arc::new(Mutex::new(Mixer::new(vec![])));
        let failure = Arc::new(StreamFailure::default());
        let worker = MusicWorker::start(&path, 48_000, mixer.clone(), failure.clone()).unwrap();
        std::fs::remove_file(&path).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !failure.failed() {
            assert!(std::time::Instant::now() < deadline);
            lock(&mixer).music.clear();
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            failure
                .take_error()
                .unwrap()
                .contains("Music decoding failed")
        );
        drop(worker);
    }
}
