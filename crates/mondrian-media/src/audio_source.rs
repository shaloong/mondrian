//! Bounded, fingerprinted decoded-audio source windows.

mod mapping;
mod session;

pub use mapping::AudioSourceSelection;

use crate::audio::AudioBuffer;
use mondrian_core::{AudioChannelLayout, ExecutionCancellationToken, MondrianError, Result};
use parking_lot::{Condvar, Mutex};
use session::PersistentFfmpegAudioWindowDecoder;
use std::collections::VecDeque;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const AUDIO_SOURCE_WINDOW_SECONDS: usize = 10;
const AUDIO_SOURCE_CACHE_ENTRY_CAPACITY: usize = 128;
const AUDIO_SOURCE_CACHE_BYTE_BUDGET: usize = 256 * 1024 * 1024;
const AUDIO_SOURCE_FAILURE_CAPACITY: usize = 64;

/// Shared weighted-LRU owner for decoded PCM windows at one output contract.
pub struct AudioSourceCache {
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    window_frames: usize,
    entry_capacity: usize,
    byte_budget: usize,
    state: Mutex<AudioSourceCacheState>,
    window_ready: Condvar,
    decoder: Arc<dyn AudioWindowDecoder>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct AudioSourceIdentity {
    pub(super) path: PathBuf,
    pub(super) len: u64,
    pub(super) modified_secs: Option<u64>,
    pub(super) modified_nanos: Option<u32>,
    pub(super) selection: AudioSourceSelection,
}

impl AudioSourceIdentity {
    fn capture(path: &Path, selection: AudioSourceSelection) -> Result<Self> {
        let metadata = std::fs::metadata(path).map_err(|error| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!("读取音频源元数据失败: {error}"),
        })?;
        let current_fingerprint = crate::MediaFileFingerprint::from_metadata(&metadata);
        if current_fingerprint != selection.source_fingerprint() {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "audio source revision changed after stream selection".to_owned(),
            });
        }
        Ok(Self::from_metadata(path, &metadata, selection))
    }

    fn from_metadata(path: &Path, metadata: &Metadata, selection: AudioSourceSelection) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        Self {
            path: path.to_path_buf(),
            len: metadata.len(),
            modified_secs: modified.map(|duration| duration.as_secs()),
            modified_nanos: modified.map(|duration| duration.subsec_nanos()),
            selection,
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
    in_flight: Vec<AudioSourceWindowKey>,
    peak_in_flight: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AudioWindowDecoderDiagnostics {
    pub(super) sessions: usize,
    pub(super) session_capacity: usize,
    pub(super) peak_sessions: usize,
    pub(super) session_opens: u64,
    pub(super) sequential_reuses: u64,
    pub(super) random_seek_restarts: u64,
    pub(super) session_evictions: u64,
    pub(super) cancellations: u64,
    pub(super) cold_window_max_duration_us: u64,
    pub(super) sequential_window_max_duration_us: u64,
    pub(super) random_seek_window_max_duration_us: u64,
}

pub(super) trait AudioWindowDecoder: Send + Sync {
    fn decode_window(
        &self,
        source: &AudioSourceIdentity,
        start_frame: i64,
        frame_count: usize,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioBuffer>;

    fn diagnostics(&self) -> AudioWindowDecoderDiagnostics {
        AudioWindowDecoderDiagnostics::default()
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
    /// Distinct source windows currently owned by decode leaders.
    pub in_flight_decodes: usize,
    /// Peak simultaneous distinct source-window decodes.
    pub peak_in_flight_decodes: usize,
    /// Resident or admitted persistent decode-session slots.
    pub decoder_sessions: usize,
    /// Configured persistent decode-session slot capacity.
    pub decoder_session_capacity: usize,
    /// Peak resident or admitted persistent decode-session slots.
    pub decoder_peak_sessions: usize,
    /// Persistent decoder process opens.
    pub decoder_session_opens: u64,
    /// Windows supplied by an already-positioned sequential session.
    pub decoder_sequential_reuses: u64,
    /// Non-contiguous requests that restarted an existing source session.
    pub decoder_random_seek_restarts: u64,
    /// Sessions evicted by bounded decoder-pool pressure.
    pub decoder_session_evictions: u64,
    /// Decode sessions terminated by generation cancellation.
    pub decoder_cancellations: u64,
    /// Slowest first window from a newly opened decode session.
    pub decoder_cold_window_max_duration_us: u64,
    /// Slowest window from an already-positioned sequential session.
    pub decoder_sequential_window_max_duration_us: u64,
    /// Slowest first window after a random-seek session restart.
    pub decoder_random_seek_window_max_duration_us: u64,
}

impl AudioSourceCache {
    /// Create the product cache: ten-second decode windows, 128 entries, and a
    /// 256 MiB global PCM payload budget across every open source.
    pub fn new(sample_rate: u32, channel_layout: AudioChannelLayout) -> Self {
        Self::with_decoder(
            sample_rate,
            channel_layout,
            AUDIO_SOURCE_WINDOW_SECONDS,
            AUDIO_SOURCE_CACHE_ENTRY_CAPACITY,
            AUDIO_SOURCE_CACHE_BYTE_BUDGET,
            Arc::new(PersistentFfmpegAudioWindowDecoder::default()),
        )
    }

    /// Semantic layout shared by every decoded window in this cache.
    pub const fn channel_layout(&self) -> AudioChannelLayout {
        self.channel_layout
    }

    /// Create an independently scheduled source cache with explicit hard limits.
    ///
    /// Derived-media services such as waveform analysis use this constructor so
    /// their sequential decode windows cannot consume the realtime playback or
    /// export cache budget. Limits are normalized to at least one window, entry,
    /// and byte; callers should expose the effective values through diagnostics.
    pub fn new_bounded(
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        window_seconds: usize,
        entry_capacity: usize,
        byte_budget: usize,
    ) -> Self {
        Self::with_decoder(
            sample_rate,
            channel_layout,
            window_seconds,
            entry_capacity,
            byte_budget,
            Arc::new(PersistentFfmpegAudioWindowDecoder::default()),
        )
    }

    fn with_decoder(
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        window_seconds: usize,
        entry_capacity: usize,
        byte_budget: usize,
        decoder: Arc<dyn AudioWindowDecoder>,
    ) -> Self {
        let sample_rate = sample_rate.max(8_000);
        Self {
            sample_rate,
            channel_layout,
            window_frames: (sample_rate as usize).saturating_mul(window_seconds.max(1)),
            entry_capacity: entry_capacity.max(1),
            byte_budget: byte_budget.max(1),
            state: Mutex::new(AudioSourceCacheState::default()),
            window_ready: Condvar::new(),
            decoder,
        }
    }

    /// Open one source identity without decoding its complete duration.
    pub fn open(
        self: &Arc<Self>,
        path: &Path,
        selection: AudioSourceSelection,
    ) -> Result<AudioSourceReader> {
        if mapping::standard_pan_filter(selection.source_layout(), self.channel_layout).is_none() {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "no explicit standard channel mapping from {:?} to {:?}",
                    selection.source_layout(),
                    self.channel_layout
                ),
            });
        }
        Ok(AudioSourceReader {
            cache: Arc::clone(self),
            source: AudioSourceIdentity::capture(path, selection)?,
        })
    }

    /// Capture bounded residency and execution evidence.
    pub fn diagnostics(&self) -> AudioSourceCacheDiagnostics {
        let state = self.state.lock();
        let decoder = self.decoder.diagnostics();
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
            in_flight_decodes: state.in_flight.len(),
            peak_in_flight_decodes: state.peak_in_flight,
            decoder_sessions: decoder.sessions,
            decoder_session_capacity: decoder.session_capacity,
            decoder_peak_sessions: decoder.peak_sessions,
            decoder_session_opens: decoder.session_opens,
            decoder_sequential_reuses: decoder.sequential_reuses,
            decoder_random_seek_restarts: decoder.random_seek_restarts,
            decoder_session_evictions: decoder.session_evictions,
            decoder_cancellations: decoder.cancellations,
            decoder_cold_window_max_duration_us: decoder.cold_window_max_duration_us,
            decoder_sequential_window_max_duration_us: decoder.sequential_window_max_duration_us,
            decoder_random_seek_window_max_duration_us: decoder.random_seek_window_max_duration_us,
        }
    }

    fn window(
        &self,
        key: AudioSourceWindowKey,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Arc<AudioBuffer>> {
        if cancellation.is_canceled() {
            return Err(canceled_audio_decode(&key.source.path));
        }
        loop {
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
            if state.in_flight.iter().any(|in_flight| in_flight == &key) {
                self.window_ready.wait_for(&mut state, Duration::from_millis(5));
                drop(state);
                if cancellation.is_canceled() {
                    return Err(canceled_audio_decode(&key.source.path));
                }
                continue;
            }
            state.misses = state.misses.saturating_add(1);
            state.in_flight.push(key.clone());
            state.peak_in_flight = state.peak_in_flight.max(state.in_flight.len());
            break;
        }

        let decode_started = Instant::now();
        let decoded = self
            .decoder
            .decode_window(
                &key.source,
                key.start_frame,
                self.window_frames,
                self.sample_rate,
                self.channel_layout,
                cancellation,
            )
            .and_then(|buffer| self.validate_window(&key, buffer));
        let decode_duration_us = decode_started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        if cancellation.is_canceled() {
            self.finish_in_flight(&key);
            return Err(canceled_audio_decode(&key.source.path));
        }
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
                    state.in_flight.retain(|in_flight| in_flight != &key);
                    state.entries.push_front(AudioSourceWindowEntry {
                        key,
                        buffer: Arc::clone(&buffer),
                        bytes,
                    });
                } else {
                    state.oversize_windows = state.oversize_windows.saturating_add(1);
                    state.in_flight.retain(|in_flight| in_flight != &key);
                }
                self.window_ready.notify_all();
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
                state.in_flight.retain(|in_flight| in_flight != &key);
                state
                    .failures
                    .push_front(AudioSourceFailureEntry { key, reason: reason.clone() });
                while state.failures.len() > AUDIO_SOURCE_FAILURE_CAPACITY {
                    state.failures.pop_back();
                }
                self.window_ready.notify_all();
                Err(error)
            }
        }
    }

    fn finish_in_flight(&self, key: &AudioSourceWindowKey) {
        self.state.lock().in_flight.retain(|in_flight| in_flight != key);
        self.window_ready.notify_all();
    }

    fn validate_window(
        &self,
        key: &AudioSourceWindowKey,
        buffer: AudioBuffer,
    ) -> Result<AudioBuffer> {
        let channels = self.channel_layout.channel_count();
        if buffer.sample_rate != self.sample_rate
            || buffer.channel_layout != self.channel_layout
            || !buffer.samples.len().is_multiple_of(channels)
            || buffer.frame_count() > self.window_frames
        {
            return Err(MondrianError::DecodeFailed {
                asset_id: key.source.path.display().to_string(),
                reason: format!(
                    "decoded audio window violated contract: rate={}/{} layout={:?}/{:?} frames={}/{}",
                    buffer.sample_rate,
                    self.sample_rate,
                    buffer.channel_layout,
                    self.channel_layout,
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
        destination: &mut [f32],
    ) -> Result<()> {
        self.read_interleaved_cancellable(
            start_frame,
            frames,
            destination,
            &ExecutionCancellationToken::new(),
        )
    }

    /// Fill one exact interleaved block with generation cancellation authority.
    pub fn read_interleaved_cancellable(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<()> {
        if cancellation.is_canceled() {
            return Err(canceled_audio_decode(&self.source.path));
        }
        let channels = self.cache.channel_layout.channel_count();
        let expected_samples = frames.checked_mul(channels).ok_or_else(|| {
            MondrianError::Other(anyhow::anyhow!("audio source block extent overflow"))
        })?;
        if destination.len() != expected_samples {
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
            let window = self.cache.window(key, cancellation)?;
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

pub(super) fn canceled_audio_decode(path: &Path) -> MondrianError {
    MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "audio render generation was canceled".to_owned(),
    }
}

pub(super) fn audio_frame_timestamp(frame: i64, sample_rate: u32) -> String {
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
    use crate::info::ChannelLayout;
    use crate::MediaFileFingerprint;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn stereo_selection(path: &Path, stream_index: u32) -> AudioSourceSelection {
        AudioSourceSelection::new(
            stream_index,
            ChannelLayout::Stereo,
            MediaFileFingerprint::capture(path),
        )
    }

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
            source: &AudioSourceIdentity,
            start_frame: i64,
            frame_count: usize,
            sample_rate: u32,
            channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let channels_usize = channel_layout.channel_count();
            let stream_offset = source.selection.stream_index() as f32 * 1_000_000.0;
            let mut samples = vec![0.0; frame_count * channels_usize];
            for frame in 0..frame_count {
                for channel in 0..channels_usize {
                    samples[frame * channels_usize + channel] =
                        stream_offset + (start_frame + frame as i64) as f32 * 10.0 + channel as f32;
                }
            }
            Ok(AudioBuffer { samples, sample_rate, channel_layout })
        }
    }

    struct MalformedWindowDecoder {
        calls: AtomicU64,
    }

    struct BlockingWindowDecoder {
        entered: AtomicBool,
    }

    struct SingleFlightWindowDecoder {
        calls: AtomicU64,
        entered: AtomicBool,
        release: AtomicBool,
    }

    impl AudioWindowDecoder for MalformedWindowDecoder {
        fn decode_window(
            &self,
            _source: &AudioSourceIdentity,
            _start_frame: i64,
            frame_count: usize,
            sample_rate: u32,
            _channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(AudioBuffer {
                samples: vec![0.0; frame_count],
                sample_rate,
                channel_layout: AudioChannelLayout::Mono,
            })
        }
    }

    impl AudioWindowDecoder for BlockingWindowDecoder {
        fn decode_window(
            &self,
            source: &AudioSourceIdentity,
            _start_frame: i64,
            _frame_count: usize,
            _sample_rate: u32,
            _channel_layout: AudioChannelLayout,
            cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            self.entered.store(true, Ordering::Release);
            while !cancellation.is_canceled() {
                std::thread::yield_now();
            }
            Err(canceled_audio_decode(&source.path))
        }
    }

    impl AudioWindowDecoder for SingleFlightWindowDecoder {
        fn decode_window(
            &self,
            _source: &AudioSourceIdentity,
            _start_frame: i64,
            frame_count: usize,
            sample_rate: u32,
            channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.entered.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            Ok(AudioBuffer {
                samples: vec![0.0; frame_count * channel_layout.channel_count()],
                sample_rate,
                channel_layout,
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
            AudioChannelLayout::Stereo,
            1,
            entry_capacity,
            byte_budget,
            decoder,
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
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

        reader.read_interleaved(7_998, 4, &mut destination).expect("cross-window read");

        assert_eq!(
            destination,
            vec![79_980.0, 79_981.0, 79_990.0, 79_991.0, 80_000.0, 80_001.0, 80_010.0, 80_011.0]
        );
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.diagnostics().entries, 2);
    }

    #[test]
    fn physical_stream_selection_is_part_of_cache_identity() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let decoder = Arc::new(RampWindowDecoder::new());
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            AudioChannelLayout::Stereo,
            1,
            4,
            4 * 8_000 * 2 * std::mem::size_of::<f32>(),
            decoder.clone(),
        ));
        let first = cache.open(file.path(), stereo_selection(file.path(), 1)).expect("stream one");
        let second =
            cache.open(file.path(), stereo_selection(file.path(), 3)).expect("stream three");
        let mut first_samples = [0.0; 2];
        let mut second_samples = [0.0; 2];

        first.read_interleaved(0, 1, &mut first_samples).expect("first stream read");
        second.read_interleaved(0, 1, &mut second_samples).expect("second stream read");

        assert_eq!(first_samples, [1_000_000.0, 1_000_001.0]);
        assert_eq!(second_samples, [3_000_000.0, 3_000_001.0]);
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.diagnostics().entries, 2);
    }

    #[test]
    fn source_window_observes_generation_cancellation() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source bytes");
        file.flush().expect("flush source");
        let decoder = Arc::new(BlockingWindowDecoder { entered: AtomicBool::new(false) });
        let cache = Arc::new(AudioSourceCache::with_decoder(
            48_000,
            AudioChannelLayout::Stereo,
            1,
            2,
            1_000_000,
            decoder.clone(),
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
        let cancellation = ExecutionCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker = std::thread::spawn(move || {
            let mut destination = vec![0.0; 2_048 * 2];
            reader.read_interleaved_cancellable(0, 2_048, &mut destination, &worker_cancellation)
        });
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while Instant::now() < deadline && !decoder.entered.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(decoder.entered.load(Ordering::Acquire));
        let canceled_at = Instant::now();
        cancellation.cancel();
        let error = worker.join().expect("worker returns").expect_err("canceled source fails");
        assert!(canceled_at.elapsed() <= std::time::Duration::from_millis(50));
        assert!(error.to_string().contains("canceled"));
        assert_eq!(
            cache.diagnostics(),
            AudioSourceCacheDiagnostics {
                byte_budget: 1_000_000,
                entry_capacity: 2,
                misses: 1,
                peak_in_flight_decodes: 1,
                ..AudioSourceCacheDiagnostics::default()
            }
        );
    }

    #[test]
    fn concurrent_same_window_miss_has_one_decode_leader() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source bytes");
        file.flush().expect("flush source");
        let decoder = Arc::new(SingleFlightWindowDecoder {
            calls: AtomicU64::new(0),
            entered: AtomicBool::new(false),
            release: AtomicBool::new(false),
        });
        let cache = Arc::new(AudioSourceCache::with_decoder(
            48_000,
            AudioChannelLayout::Stereo,
            1,
            2,
            1_000_000,
            decoder.clone(),
        ));
        let first_reader = cache
            .open(file.path(), stereo_selection(file.path(), 0))
            .expect("open first reader");
        let second_reader = first_reader.clone();
        let first = std::thread::spawn(move || {
            let mut destination = vec![0.0; 2_048 * 2];
            first_reader.read_interleaved(0, 2_048, &mut destination)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !decoder.entered.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(decoder.entered.load(Ordering::Acquire));
        let second = std::thread::spawn(move || {
            let mut destination = vec![0.0; 2_048 * 2];
            second_reader.read_interleaved(0, 2_048, &mut destination)
        });
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 1);
        decoder.release.store(true, Ordering::Release);
        first.join().expect("first reader returns").expect("first read");
        second.join().expect("second reader returns").expect("second read");

        let diagnostics = cache.diagnostics();
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.in_flight_decodes, 0);
        assert_eq!(diagnostics.peak_in_flight_decodes, 1);
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

        reader.read_interleaved(-2, 4, &mut destination).expect("negative source read");
        reader
            .read_interleaved(128, 2, &mut destination[..4])
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

        reader.read_interleaved(0, 1, &mut destination).expect("first window");
        reader.read_interleaved(8_000, 1, &mut destination).expect("second window");
        reader.read_interleaved(0, 1, &mut destination).expect("evicted window reload");

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.reserved_bytes, window_bytes);
        assert_eq!(diagnostics.evictions, 2);
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn independently_bounded_cache_reports_effective_hard_limits() {
        let cache = AudioSourceCache::new_bounded(
            48_000,
            AudioChannelLayout::Mono,
            10,
            4,
            16 * 1024 * 1024,
        );
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entry_capacity, 4);
        assert_eq!(diagnostics.byte_budget, 16 * 1024 * 1024);
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.reserved_bytes, 0);
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
        first_reader.read_interleaved(0, 1, &mut destination).expect("first identity");
        file.write_all(b"-replacement").expect("replace source identity");
        file.flush().expect("flush replacement");

        let second_reader = cache
            .open(file.path(), stereo_selection(file.path(), 0))
            .expect("reopen replaced source");
        second_reader.read_interleaved(0, 1, &mut destination).expect("second identity");

        assert_eq!(decoder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.diagnostics().entries, 2);
    }

    #[test]
    fn open_rejects_a_selection_from_an_obsolete_file_revision() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        file.flush().expect("flush source");
        let selection = stereo_selection(file.path(), 0);
        file.write_all(b"-replacement").expect("replace source identity");
        file.flush().expect("flush replacement");
        let cache = Arc::new(AudioSourceCache::new(48_000, AudioChannelLayout::Stereo));

        let error = cache
            .open(file.path(), selection)
            .err()
            .expect("obsolete stream selection must fail");

        assert!(error.to_string().contains("revision changed"));
    }

    #[test]
    fn malformed_window_fails_closed_and_uses_bounded_failure_memory() {
        let decoder = Arc::new(MalformedWindowDecoder { calls: AtomicU64::new(0) });
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            AudioChannelLayout::Stereo,
            1,
            2,
            128 * 1024,
            decoder.clone(),
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
        let mut destination = vec![0.0; 4];

        for _ in 0..2 {
            reader
                .read_interleaved(0, 2, &mut destination)
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
        let channel_layout = AudioChannelLayout::Stereo;
        let channels = channel_layout.channel_count_u8();
        let full = decode_audio_file_with_ffmpeg_cli(&path, sample_rate, channel_layout)
            .expect("sequential reference decode");
        assert!(
            full.frame_count() >= sample_rate as usize * 2 + 2_048,
            "manual parity source must contain at least two seconds of audio"
        );
        let cache = Arc::new(AudioSourceCache::with_decoder(
            sample_rate,
            channel_layout,
            1,
            1,
            sample_rate as usize * usize::from(channels) * std::mem::size_of::<f32>(),
            Arc::new(PersistentFfmpegAudioWindowDecoder::default()),
        ));
        let stream = crate::MediaInfo::probe(&path)
            .expect("probe external source")
            .primary_audio()
            .expect("primary audio stream")
            .clone();
        let reader = cache
            .open(
                &path,
                AudioSourceSelection::from_stream(&stream, MediaFileFingerprint::capture(&path)),
            )
            .expect("open bounded source");
        let frames = 2_048usize;
        for start in [0usize, sample_rate as usize, 0] {
            if start.saturating_add(frames) > full.frame_count() {
                continue;
            }
            let mut actual = vec![0.0; frames * usize::from(channels)];
            reader
                .read_interleaved(start as i64, frames, &mut actual)
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
        assert_eq!(diagnostics.entries, 1);
        assert!(diagnostics.reserved_bytes <= diagnostics.byte_budget);
        assert_eq!(diagnostics.decoder_session_opens, 2);
        assert_eq!(diagnostics.decoder_sequential_reuses, 1);
        assert_eq!(diagnostics.decoder_random_seek_restarts, 1);
        assert_eq!(diagnostics.decoder_sessions, 1);
        assert!(diagnostics.decoder_peak_sessions <= diagnostics.decoder_session_capacity);
    }

    #[test]
    #[ignore = "manual real-media cancellation gate; requires MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH"]
    fn external_persistent_audio_session_observes_cancellation() {
        let Some(path) = std::env::var_os("MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH").map(PathBuf::from)
        else {
            eprintln!("skipped: MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH not set");
            return;
        };
        let cache = Arc::new(AudioSourceCache::new(48_000, AudioChannelLayout::Stereo));
        let stream = crate::MediaInfo::probe(&path)
            .expect("probe external source")
            .primary_audio()
            .expect("primary audio stream")
            .clone();
        let reader = cache
            .open(
                &path,
                AudioSourceSelection::from_stream(&stream, MediaFileFingerprint::capture(&path)),
            )
            .expect("open bounded source");
        let cancellation = ExecutionCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker = std::thread::spawn(move || {
            let mut destination = vec![0.0; 2_048 * 2];
            reader.read_interleaved_cancellable(0, 2_048, &mut destination, &worker_cancellation)
        });
        let admission_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < admission_deadline && cache.diagnostics().decoder_sessions == 0 {
            std::thread::yield_now();
        }
        assert_eq!(cache.diagnostics().decoder_sessions, 1);
        let canceled_at = Instant::now();
        cancellation.cancel();
        let error = worker.join().expect("decode worker returns").expect_err("decode cancels");
        assert!(
            canceled_at.elapsed() <= Duration::from_millis(50),
            "persistent decode cancellation exceeded 50 ms: {:?}",
            canceled_at.elapsed()
        );
        assert!(error.to_string().contains("canceled"));
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.failures, 0);
        assert_eq!(diagnostics.in_flight_decodes, 0);
        assert_eq!(diagnostics.decoder_sessions, 0);
        assert_eq!(diagnostics.decoder_cancellations, 1);
    }
}
