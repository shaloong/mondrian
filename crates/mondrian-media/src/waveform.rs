//! Audio waveform computation and caching for timeline display.
//!
//! Produces peak amplitude data at multiple resolutions so the waveform
//! renders correctly regardless of zoom level.

use crate::audio::AudioBuffer;
use mondrian_core::types::AssetId;
use std::collections::HashMap;

/// Pre-computed waveform peaks for a single resolution level.
#[derive(Debug, Clone)]
pub struct WaveformData {
    /// Positive peaks, normalized to 0..1. One value per output column.
    pub peaks: Vec<f32>,
    /// Number of audio samples aggregated into each column.
    pub samples_per_column: u32,
}

/// Maximum pixel width for waveform computation.
/// Clips wider than this get downsampled to avoid OOM on extreme zoom.
pub const MAX_WAVEFORM_WIDTH: u32 = 4096;

/// Multi-resolution waveform cache keyed by (asset_id, pixel_width).
#[derive(Debug, Default)]
pub struct WaveformCache {
    entries: HashMap<(AssetId, u32), WaveformData>,
    /// Tracks insertion order for bounded eviction (oldest first).
    order: Vec<(AssetId, u32)>,
    max_entries: usize,
}

impl WaveformCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            max_entries: 64,
        }
    }

    pub fn get_or_compute(
        &mut self,
        asset_id: AssetId,
        buffer: &AudioBuffer,
        pixel_width: u32,
    ) -> &WaveformData {
        let capped_width = pixel_width.clamp(1, MAX_WAVEFORM_WIDTH);
        let key = (asset_id, capped_width);

        // Fast path: already cached.
        if self.entries.contains_key(&key) {
            // SAFETY: key exists, get returns Some. The reference is valid
            // because we only evict before inserting, never during reads.
            return self.entries.get(&key).unwrap();
        }

        // Evict oldest entries before inserting to stay within budget.
        while self.order.len() >= self.max_entries {
            if let Some(oldest) = self.order.first().copied() {
                self.entries.remove(&oldest);
                self.order.remove(0);
            } else {
                break;
            }
        }

        let data = compute_waveform(buffer, capped_width);
        self.entries.insert(key, data);
        self.order.push(key);
        self.entries.get(&key).unwrap()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

/// Compute waveform peaks from an audio buffer at the given pixel resolution.
///
/// For each output column, records the maximum absolute sample value as the
/// positive peak. Returns `WaveformData` with one peak per column.
pub fn compute_waveform(buffer: &AudioBuffer, pixel_width: u32) -> WaveformData {
    let width = pixel_width.clamp(1, MAX_WAVEFORM_WIDTH) as usize;
    let mut peaks = vec![0.0f32; width];

    if buffer.samples.is_empty() || buffer.frame_count() == 0 {
        return WaveformData { peaks, samples_per_column: 0 };
    }

    let total_frames = buffer.frame_count();
    let channels = buffer.channels as usize;
    let samples_per_column = (total_frames as u32).div_ceil(width as u32).max(1);
    let frames_per_column = samples_per_column as usize;

    for (col, peak) in peaks.iter_mut().enumerate() {
        let start_frame = col * frames_per_column;
        let end_frame = (start_frame + frames_per_column).min(total_frames);

        let mut max_abs = 0.0f32;
        for frame in start_frame..end_frame {
            let base = frame * channels;
            for ch in 0..channels {
                let s = buffer.samples.get(base + ch).copied().unwrap_or(0.0);
                max_abs = max_abs.max(s.abs());
            }
        }
        *peak = max_abs.clamp(0.0, 1.0);
    }

    WaveformData { peaks, samples_per_column }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_returns_zero_peaks() {
        let buffer = AudioBuffer { samples: vec![], sample_rate: 48000, channels: 2 };
        let data = compute_waveform(&buffer, 100);
        assert_eq!(data.peaks.len(), 100);
        assert!(data.peaks.iter().all(|&p| p == 0.0));
    }

    #[test]
    fn silent_buffer_returns_near_zero_peaks() {
        let buffer = AudioBuffer::silent(48000, 2, 48000);
        let data = compute_waveform(&buffer, 50);
        assert_eq!(data.peaks.len(), 50);
        assert!(data.peaks.iter().all(|&p| (p - 0.0).abs() < f32::EPSILON));
    }

    #[test]
    fn full_scale_tone_produces_measurable_peaks() {
        // 1kHz sine at -3dBFS in 48kHz stereo for 1 second
        let sample_rate = 48000u32;
        let frames = sample_rate as usize;
        let mut samples = vec![0.0f32; frames * 2];
        for f in 0..frames {
            let t = f as f32 / sample_rate as f32;
            let val = (std::f32::consts::TAU * 1000.0 * t).sin() * 0.707;
            samples[f * 2] = val;
            samples[f * 2 + 1] = val * 0.9;
        }
        let buffer = AudioBuffer { samples, sample_rate, channels: 2 };

        let data = compute_waveform(&buffer, 100);
        assert_eq!(data.peaks.len(), 100);
        let max_peak = data.peaks.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            max_peak > 0.5,
            "sine tone should produce peaks > 0.5, got {max_peak}"
        );
    }

    #[test]
    fn single_column_aggregates_all_samples() {
        let buffer = AudioBuffer {
            samples: vec![0.9, -0.9, 0.5, -0.5],
            sample_rate: 48000,
            channels: 1,
        };
        let data = compute_waveform(&buffer, 1);
        assert_eq!(data.peaks.len(), 1);
        assert!((data.peaks[0] - 0.9).abs() < 1e-5);
    }

    #[test]
    fn waveform_cache_hits_return_same_data() {
        let mut cache = WaveformCache::new();
        let buffer = AudioBuffer::silent(44100, 1, 4410);
        let asset_id = AssetId::new();

        let len_a = cache.get_or_compute(asset_id, &buffer, 100).peaks.len();
        let len_b = cache.get_or_compute(asset_id, &buffer, 100).peaks.len();
        assert_eq!(len_a, len_b);
    }

    #[test]
    fn width_capped_to_max_avoids_oom() {
        let buffer = AudioBuffer::silent(48000, 2, 48000);
        let data = compute_waveform(&buffer, 100_000);
        assert!(data.peaks.len() <= MAX_WAVEFORM_WIDTH as usize);
    }
}
