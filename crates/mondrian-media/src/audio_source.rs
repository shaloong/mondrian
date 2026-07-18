//! Bounded, fingerprinted decoded-audio source windows.

use crate::audio::AudioBuffer;
use mondrian_core::{MondrianError, Result};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

const AUDIO_SOURCE_WINDOW_SECONDS: usize = 10;
const AUDIO_SOURCE_CACHE_ENTRY_CAPACITY: usize = 128;
const AUDIO_SOURCE_CACHE_BYTE_BUDGET: usize = 256 * 1024 * 1024;
const AUDIO_SOURCE_FAILURE_CAPACITY: usize = 64;

/// Shared weighted-LRU owner for decoded PCM windows at one output contract.
pub struct AudioSourceCache {
    sample_rate: u32,
    channels: u8,
    window_frames: usize,
    entry_capacity: usize,
    byte_budget: usize,
    state: Mutex<AudioSourceCacheState>,
    decoder: Arc<dyn AudioWindowDecoder>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AudioSourceIdentity {
    path: PathBuf,
    len: u64,
    modified_secs: Option<u64>,
    modified_nanos: Option<u32>,
}

impl AudioSourceIdentity {
    fn capture(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path).map_err(|error| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!("读取音频源元数据失败: {error}"),
        })?;
        Ok(Self::from_metadata(path, &metadata))
    }

    fn from_metadata(path: &Path, metadata: &Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        Self {
            path: path.to_path_buf(),
            len: metadata.len(),
            modified_secs: modified.map(|duration| duration.as_secs()),
            modified_nanos: modified.map(|duration| duration.subsec_nanos()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AudioSourceWindowKey {
    source: AudioSourceIdentity,
    start_frame: i64,
}

struct AudioSourceWindowEntry {
    key: AudioSourceWindowKey,
    buffer: Arc<AudioBuffer>,
    bytes: usize,
}

struct AudioSourceFailureEntry {
    key: AudioSourceWindowKey,
    reason: String,
}

#[derive(Default)]
struct AudioSourceCacheState {
    entries: VecDeque<AudioSourceWindowEntry>,
    failures: VecDeque<AudioSourceFailureEntry>,
    reserved_bytes: usize,
    hits: u64,
    misses: u64,
    decode_successes: u64,
    decode_failures: u64,
    decode_total_duration_us: u64,
    decode_max_duration_us: u64,
    evictions: u64,
    oversize_windows: u64,
}

trait AudioWindowDecoder: Send + Sync {
    fn decode_window(
        &self,
        path: &Path,
        start_frame: i64,
        frame_count: usize,
        sample_rate: u32,
        channels: u8,
    ) -> Result<AudioBuffer>;
}

struct FfmpegCliAudioWindowDecoder;

impl AudioWindowDecoder for FfmpegCliAudioWindowDecoder {
    fn decode_window(
        &self,
        path: &Path,
        start_frame: i64,
        frame_count: usize,
        sample_rate: u32,
        channels: u8,
    ) -> Result<AudioBuffer> {
        decode_audio_window_with_ffmpeg_cli(path, start_frame, frame_count, sample_rate, channels)
    }
}

/// Stable reader for one fingerprinted media source at one output contract.
///
/// Readers are cheap handles. PCM ownership remains in the shared weighted LRU
/// and a file replacement creates a different identity on the next `open`.
#[derive(Clone)]
pub struct AudioSourceReader {
    cache: Arc<AudioSourceCache>,
    source: AudioSourceIdentity,
}

/// Point-in-time bounded audio-source cache evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AudioSourceCacheDiagnostics {
    /// Resident decoded PCM windows.
    pub entries: usize,
    /// Bounded terminal decode failures retained for the current source identities.
    pub failures: usize,
    /// Resident decoded PCM payload bytes.
    pub reserved_bytes: usize,
    /// Configured global PCM payload budget.
    pub byte_budget: usize,
    /// Configured global entry capacity.
    pub entry_capacity: usize,
    /// Cache hits across all source readers.
    pub hits: u64,
    /// Cache misses across all source readers.
    pub misses: u64,
    /// Successfully decoded windows.
    pub decode_successes: u64,
    /// Failed window decodes.
    pub decode_failures: u64,
    /// Total wall time spent in concrete window decode Adapters.
    pub decode_total_duration_us: u64,
    /// Slowest concrete window decode Adapter call.
    pub decode_max_duration_us: u64,
    /// LRU evictions caused by entry or byte pressure.
    pub evictions: u64,
    /// Decoded windows too large for the configured byte budget and therefore not retained.
    pub oversize_windows: u64,
}

impl AudioSourceCache {
    /// Create the product cache: ten-second decode windows, 128 entries, and a
    /// 256 MiB global PCM payload budget across every open source.
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        Self::with_decoder(
            sample_rate,
            channels,
            AUDIO_SOURCE_WINDOW_SECONDS,
            AUDIO_SOURCE_CACHE_ENTRY_CAPACITY,
            AUDIO_SOURCE_CACHE_BYTE_BUDGET,
            Arc::new(FfmpegCliAudioWindowDecoder),
        )
    }

    fn with_decoder(
        sample_rate: u32,
        channels: u8,
        window_seconds: usize,
        entry_capacity: usize,
        byte_budget: usize,
        decoder: Arc<dyn AudioWindowDecoder>,
    ) -> Self {
        let sample_rate = sample_rate.max(8_000);
        Self {
            sample_rate,
            channels: channels.max(1),
            window_frames: (sample_rate as usize).saturating_mul(window_seconds.max(1)),
            entry_capacity: entry_capacity.max(1),
            byte_budget: byte_budget.max(1),
            state: Mutex::new(AudioSourceCacheState::default()),
            decoder,
        }
    }

    /// Open one source identity without decoding its complete duration.
    pub fn open(self: &Arc<Self>, path: &Path) -> Result<AudioSourceReader> {
        Ok(AudioSourceReader {
            cache: Arc::clone(self),
            source: AudioSourceIdentity::capture(path)?,
        })
    }

    /// Capture bounded residency and execution evidence.
    pub fn diagnostics(&self) -> AudioSourceCacheDiagnostics {
        let state = self.state.lock();
        AudioSourceCacheDiagnostics {
            entries: state.entries.len(),
            failures: state.failures.len(),
            reserved_bytes: state.reserved_bytes,
            byte_budget: self.byte_budget,
            entry_capacity: self.entry_capacity,
            hits: state.hits,
            misses: state.misses,
            decode_successes: state.decode_successes,
            decode_failures: state.decode_failures,
            decode_total_duration_us: state.decode_total_duration_us,
            decode_max_duration_us: state.decode_max_duration_us,
            evictions: state.evictions,
            oversize_windows: state.oversize_windows,
        }
    }

    /// Drop every decoded window and remembered failure.
    pub fn clear(&self) {
        *self.state.lock() = AudioSourceCacheState::default();
    }

    fn window(&self, key: AudioSourceWindowKey) -> Result<Arc<AudioBuffer>> {
        {
            let mut state = self.state.lock();
            if let Some(index) = state.entries.iter().position(|entry| entry.key == key) {
                if let Some(entry) = state.entries.remove(index) {
                    let buffer = Arc::clone(&entry.buffer);
                    state.entries.push_front(entry);
                    state.hits = state.hits.saturating_add(1);
                    return Ok(buffer);
                }
            }
            if let Some(failure) = state.failures.iter().find(|failure| failure.key == key) {
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: failure.reason.clone(),
                });
            }
            state.misses = state.misses.saturating_add(1);
        }

        let decode_started = Instant::now();
        let decoded = self
            .decoder
            .decode_window(
                &key.source.path,
                key.start_frame,
                self.window_frames,
                self.sample_rate,
                self.channels,
            )
            .and_then(|buffer| self.validate_window(&key, buffer));
        let decode_duration_us = decode_started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        match decoded {
            Ok(buffer) => {
                let buffer = Arc::new(buffer);
                let bytes = buffer.samples.len().saturating_mul(std::mem::size_of::<f32>());
                let mut state = self.state.lock();
                state.decode_successes = state.decode_successes.saturating_add(1);
                state.decode_total_duration_us =
                    state.decode_total_duration_us.saturating_add(decode_duration_us);
                state.decode_max_duration_us = state.decode_max_duration_us.max(decode_duration_us);
                state.failures.retain(|failure| failure.key != key);
                if let Some(index) = state.entries.iter().position(|entry| entry.key == key) {
                    if let Some(entry) = state.entries.remove(index) {
                        let existing = Arc::clone(&entry.buffer);
                        state.entries.push_front(entry);
                        return Ok(existing);
                    }
                }
                while !state.entries.is_empty()
                    && (state.entries.len() >= self.entry_capacity
                        || state.reserved_bytes.saturating_add(bytes) > self.byte_budget)
                {
                    if let Some(evicted) = state.entries.pop_back() {
                        state.reserved_bytes = state.reserved_bytes.saturating_sub(evicted.bytes);
                        state.evictions = state.evictions.saturating_add(1);
                    }
                }
                if bytes <= self.byte_budget {
                    state.reserved_bytes = state.reserved_bytes.saturating_add(bytes);
                    state.entries.push_front(AudioSourceWindowEntry {
                        key,
                        buffer: Arc::clone(&buffer),
                        bytes,
                    });
                } else {
                    state.oversize_windows = state.oversize_windows.saturating_add(1);
                }
                Ok(buffer)
            }
            Err(error) => {
                let reason = error.to_string();
                let mut state = self.state.lock();
                state.decode_failures = state.decode_failures.saturating_add(1);
                state.decode_total_duration_us =
                    state.decode_total_duration_us.saturating_add(decode_duration_us);
                state.decode_max_duration_us = state.decode_max_duration_us.max(decode_duration_us);
                state.failures.retain(|failure| failure.key != key);
                state
                    .failures
                    .push_front(AudioSourceFailureEntry { key, reason: reason.clone() });
                while state.failures.len() > AUDIO_SOURCE_FAILURE_CAPACITY {
                    state.failures.pop_back();
                }
                Err(error)
            }
        }
    }

    fn validate_window(
        &self,
        key: &AudioSourceWindowKey,
        buffer: AudioBuffer,
    ) -> Result<AudioBuffer> {
        let channels = usize::from(self.channels);
        if buffer.sample_rate != self.sample_rate
            || buffer.channels != self.channels
            || !buffer.samples.len().is_multiple_of(channels)
            || buffer.frame_count() > self.window_frames
        {
            return Err(MondrianError::DecodeFailed {
                asset_id: key.source.path.display().to_string(),
                reason: format!(
                    "decoded audio window violated contract: rate={}/{} channels={}/{} frames={}/{}",
                    buffer.sample_rate,
                    self.sample_rate,
                    buffer.channels,
                    self.channels,
                    buffer.frame_count(),
                    self.window_frames,
                ),
            });
        }
        Ok(buffer)
    }
}

impl AudioSourceReader {
    /// Fill one exact interleaved output block from bounded decoded windows.
    ///
    /// Negative and post-EOF coordinates remain silence. The method never
    /// retains a whole source in process memory.
    pub fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        channels: usize,
        destination: &mut [f32],
    ) -> Result<()> {
        let expected_samples = frames.checked_mul(channels).ok_or_else(|| {
            MondrianError::Other(anyhow::anyhow!("audio source block extent overflow"))
        })?;
        if destination.len() != expected_samples || channels != usize::from(self.cache.channels) {
            return Err(MondrianError::Other(anyhow::anyhow!(
                "audio source block does not match the opened channel contract"
            )));
        }
        destination.fill(0.0);
        if frames == 0 {
            return Ok(());
        }

        let leading_silence = if start_frame < 0 {
            usize::try_from(start_frame.saturating_abs()).unwrap_or(usize::MAX).min(frames)
        } else {
            0
        };
        let mut destination_frame = leading_silence;
        let mut source_frame = start_frame.saturating_add(leading_silence as i64).max(0);
        let window_frames_i64 = i64::try_from(self.cache.window_frames).unwrap_or(i64::MAX);

        while destination_frame < frames {
            let window_start =
                source_frame.div_euclid(window_frames_i64).saturating_mul(window_frames_i64);
            let key = AudioSourceWindowKey {
                source: self.source.clone(),
                start_frame: window_start,
            };
            let window = self.cache.window(key)?;
            let local_frame =
                usize::try_from(source_frame.saturating_sub(window_start)).unwrap_or(usize::MAX);
            let available_frames = window.frame_count().saturating_sub(local_frame);
            if available_frames == 0 {
                break;
            }
            let copy_frames = available_frames.min(frames - destination_frame);
            let source_sample = local_frame.saturating_mul(channels);
            let destination_sample = destination_frame.saturating_mul(channels);
            let copy_samples = copy_frames.saturating_mul(channels);
            destination[destination_sample..destination_sample + copy_samples]
                .copy_from_slice(&window.samples[source_sample..source_sample + copy_samples]);
            destination_frame += copy_frames;
            source_frame = source_frame.saturating_add(copy_frames as i64);
            if window.frame_count() < self.cache.window_frames {
                break;
            }
        }
        Ok(())
    }
}

fn decode_audio_window_with_ffmpeg_cli(
    path: &Path,
    start_frame: i64,
    frame_count: usize,
    sample_rate: u32,
    channels: u8,
) -> Result<AudioBuffer> {
    let sample_rate = sample_rate.max(8_000);
    let channels = channels.max(1);
    let start_frame = start_frame.max(0);
    let output = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-nostdin")
        .arg("-accurate_seek")
        .arg("-ss")
        .arg(audio_frame_timestamp(start_frame, sample_rate))
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("0:a:0")
        .arg("-vn")
        .arg("-sn")
        .arg("-dn")
        .arg("-t")
        .arg(audio_frame_timestamp(
            i64::try_from(frame_count).unwrap_or(i64::MAX),
            sample_rate,
        ))
        .arg("-f")
        .arg("f32le")
        .arg("-acodec")
        .arg("pcm_f32le")
        .arg("-ac")
        .arg(channels.to_string())
        .arg("-ar")
        .arg(sample_rate.to_string())
        .arg("pipe:1")
        .output()
        .map_err(|error| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!("调用 ffmpeg 窗口解码失败: {error}"),
        })?;

    if !output.status.success() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "ffmpeg 音频窗口解码失败: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    if output.stdout.len() % std::mem::size_of::<f32>() != 0 {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "ffmpeg returned a truncated f32le audio window".to_owned(),
        });
    }

    let maximum_samples = frame_count.saturating_mul(usize::from(channels));
    let mut samples = Vec::with_capacity((output.stdout.len() / 4).min(maximum_samples));
    for chunk in output.stdout.chunks_exact(4).take(maximum_samples) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(AudioBuffer { samples, sample_rate, channels })
}

fn audio_frame_timestamp(frame: i64, sample_rate: u32) -> String {
    let frame = frame.max(0) as u128;
    let sample_rate = u128::from(sample_rate.max(1));
    let seconds = frame / sample_rate;
    let fractional_nanos = (frame % sample_rate).saturating_mul(1_000_000_000) / sample_rate;
    format!("{seconds}.{fractional_nanos:09}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::decode_audio_file_with_ffmpeg_cli;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct RampWindowDecoder {
        calls: AtomicU64,
    }

    impl RampWindowDecoder {
        fn new() -> Self {
            Self { calls: AtomicU64::new(0) }
        }
    }

    impl AudioWindowDecoder for RampWindowDecoder {
        fn decode_window(
            &self,
            _path: &Path,
            start_frame: i64,
            frame_count: usize,
            sample_rate: u32,
            channels: u8,
        ) -> Result<AudioBuffer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let channels_usize = usize::from(channels);
            let mut samples = vec![0.0; frame_count * channels_usize];
            for frame in 0..frame_count {
                for channel in 0..channels_usize {
                    samples[frame * channels_usize + channel] =
                        (start_frame + frame as i64) as f32 * 10.0 + channel as f32;
                }
            }
            Ok(AudioBuffer { samples, sample_rate, channels })
        }
    }

    struct MalformedWindowDecoder {
        calls: AtomicU64,
    }

    impl AudioWindowDecoder for MalformedWindowDecoder {
        fn decode_window(
            &self,
            _path: &Path,
            _start_frame: i64,
            frame_count: usize,
            sample_rate: u32,
            _channels: u8,
        ) -> Result<AudioBuffer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(AudioBuffer {
                samples: vec![0.0; frame_count],
                sample_rate,
                channels: 1,
            })
        }
    }

    fn test_audio_source(
        decoder: Arc<RampWindowDecoder>,
        entry_capacity: usize,
        byte_budget: usize,
    ) -> (
        tempfile::NamedTempFile,
        Arc<AudioSourceCache>,
        AudioSourceReader,
    ) {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            2,
            1,
            entry_capacity,
            byte_budget,
            decoder,
        ));
        let reader = cache.open(file.path()).expect("open source");
        (file, cache, reader)
    }

    #[test]
    fn bounded_audio_source_reads_exactly_across_aligned_windows() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let (_file, cache, reader) = test_audio_source(
            Arc::clone(&decoder),
            4,
            4 * 8_000 * 2 * std::mem::size_of::<f32>(),
        );
        let mut destination = vec![0.0; 8];

        reader
            .read_interleaved(7_998, 4, 2, &mut destination)
            .expect("cross-window read");

        assert_eq!(
            destination,
            vec![79_980.0, 79_981.0, 79_990.0, 79_991.0, 80_000.0, 80_001.0, 80_010.0, 80_011.0]
        );
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.diagnostics().entries, 2);
    }

    #[test]
    fn bounded_audio_source_preserves_negative_silence_and_reuses_seek_window() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let (_file, cache, reader) = test_audio_source(
            Arc::clone(&decoder),
            2,
            2 * 8_000 * 2 * std::mem::size_of::<f32>(),
        );
        let mut destination = vec![1.0; 8];

        reader
            .read_interleaved(-2, 4, 2, &mut destination)
            .expect("negative source read");
        reader
            .read_interleaved(128, 2, 2, &mut destination[..4])
            .expect("same-window seek");

        assert_eq!(&destination[4..], &[0.0, 1.0, 10.0, 11.0]);
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 1);
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.misses, 1);
    }

    #[test]
    fn bounded_audio_source_evicts_by_global_pcm_byte_budget() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let window_bytes = 8_000 * 2 * std::mem::size_of::<f32>();
        let (_file, cache, reader) = test_audio_source(Arc::clone(&decoder), 8, window_bytes);
        let mut destination = vec![0.0; 2];

        reader.read_interleaved(0, 1, 2, &mut destination).expect("first window");
        reader.read_interleaved(8_000, 1, 2, &mut destination).expect("second window");
        reader
            .read_interleaved(0, 1, 2, &mut destination)
            .expect("evicted window reload");

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.reserved_bytes, window_bytes);
        assert_eq!(diagnostics.evictions, 2);
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn reopening_replaced_audio_source_uses_a_new_fingerprint() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let (mut file, cache, first_reader) = test_audio_source(
            Arc::clone(&decoder),
            4,
            4 * 8_000 * 2 * std::mem::size_of::<f32>(),
        );
        let mut destination = vec![0.0; 2];
        first_reader
            .read_interleaved(0, 1, 2, &mut destination)
            .expect("first identity");
        file.write_all(b"-replacement").expect("replace source identity");
        file.flush().expect("flush replacement");

        let second_reader = cache.open(file.path()).expect("reopen replaced source");
        second_reader
            .read_interleaved(0, 1, 2, &mut destination)
            .expect("second identity");

        assert_eq!(decoder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.diagnostics().entries, 2);
    }

    #[test]
    fn malformed_window_fails_closed_and_uses_bounded_failure_memory() {
        let decoder = Arc::new(MalformedWindowDecoder { calls: AtomicU64::new(0) });
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            2,
            1,
            2,
            128 * 1024,
            decoder.clone(),
        ));
        let reader = cache.open(file.path()).expect("open source");
        let mut destination = vec![0.0; 4];

        for _ in 0..2 {
            reader
                .read_interleaved(0, 2, 2, &mut destination)
                .expect_err("malformed channel contract must fail");
        }

        assert_eq!(decoder.calls.load(Ordering::Relaxed), 1);
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.failures, 1);
        assert_eq!(diagnostics.decode_failures, 1);
    }

    #[test]
    #[ignore = "manual real-media parity gate; requires MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH"]
    fn external_audio_windows_match_sequential_decode_at_seek_positions() {
        let Some(path) = std::env::var_os("MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH").map(PathBuf::from)
        else {
            eprintln!("skipped: MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH not set");
            return;
        };
        let sample_rate = 48_000;
        let channels = 2;
        let full = decode_audio_file_with_ffmpeg_cli(&path, sample_rate, channels)
            .expect("sequential reference decode");
        let cache = Arc::new(AudioSourceCache::new(sample_rate, channels));
        let reader = cache.open(&path).expect("open bounded source");
        let frames = 2_048usize;
        let final_start = full.frame_count().saturating_sub(frames);
        for start in [0usize, sample_rate as usize / 2, final_start] {
            if start.saturating_add(frames) > full.frame_count() {
                continue;
            }
            let mut actual = vec![0.0; frames * usize::from(channels)];
            reader
                .read_interleaved(start as i64, frames, usize::from(channels), &mut actual)
                .expect("window decode");
            let expected_start = start * usize::from(channels);
            let expected = &full.samples[expected_start..expected_start + actual.len()];
            let max_error = actual
                .iter()
                .zip(expected)
                .map(|(actual, expected)| (actual - expected).abs())
                .fold(0.0_f32, f32::max);
            assert!(
                max_error <= 1.0e-4,
                "window at frame {start} differs from sequential decode: max_error={max_error}"
            );
        }
        let diagnostics = cache.diagnostics();
        assert!(diagnostics.entries > 0);
        assert!(diagnostics.reserved_bytes <= diagnostics.byte_budget);
    }
}
