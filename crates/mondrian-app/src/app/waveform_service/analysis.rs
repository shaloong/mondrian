//! Dedicated worker and streaming waveform-analysis Adapter.

use std::sync::{mpsc, Arc};
use std::time::Instant;

use mondrian_media::{AudioSourceCache, WaveformEnvelopeBuilder};

use super::state::{WaveformFailure, WaveformJob, WaveformResult, WaveformSource};
use super::{
    WaveformFailureReason, WAVEFORM_DECODE_WINDOW_SECONDS, WAVEFORM_LAYOUT, WAVEFORM_MAX_WIDTH,
    WAVEFORM_SAMPLE_RATE,
};

pub(super) fn waveform_worker(
    jobs: mpsc::Receiver<WaveformJob>,
    results: mpsc::SyncSender<WaveformResult>,
    source_cache: Arc<AudioSourceCache>,
) {
    for job in jobs {
        let started = Instant::now();
        let source = build_waveform_source(&job, &source_cache);
        let result = WaveformResult {
            key: job.key,
            generation: job.generation,
            source,
            elapsed: started.elapsed(),
        };
        if results.send(result).is_err() {
            break;
        }
    }
}

fn build_waveform_source(
    job: &WaveformJob,
    source_cache: &Arc<AudioSourceCache>,
) -> Result<WaveformSource, WaveformFailure> {
    if job.cancellation.is_canceled() {
        return Err(canceled_failure());
    }
    let reader = source_cache.open(&job.path, job.selection.clone()).map_err(|error| {
        WaveformFailure::new(WaveformFailureReason::DecodeFailed, error.to_string())
    })?;
    let width = usize::try_from(job.total_frames.min(u64::from(WAVEFORM_MAX_WIDTH)))
        .unwrap_or(WAVEFORM_MAX_WIDTH as usize)
        .max(1);
    let mut envelope =
        WaveformEnvelopeBuilder::new(job.total_frames, width as u32).map_err(|error| {
            WaveformFailure::new(
                WaveformFailureReason::DurationUnavailable,
                error.to_string(),
            )
        })?;
    let window_frames = WAVEFORM_SAMPLE_RATE as usize * WAVEFORM_DECODE_WINDOW_SECONDS;
    let mut samples = vec![0.0_f32; window_frames];
    let mut start = 0_u64;
    while start < job.total_frames {
        if job.cancellation.is_canceled() {
            return Err(canceled_failure());
        }
        let frames = usize::try_from((job.total_frames - start).min(window_frames as u64))
            .unwrap_or(window_frames);
        reader
            .read_interleaved_cancellable(
                i64::try_from(start).map_err(|_| {
                    WaveformFailure::new(
                        WaveformFailureReason::DurationUnavailable,
                        "waveform source duration exceeds signed sample coordinates",
                    )
                })?,
                frames,
                &mut samples[..frames],
                &job.cancellation,
            )
            .map_err(|error| {
                if job.cancellation.is_canceled() {
                    canceled_failure()
                } else {
                    WaveformFailure::new(WaveformFailureReason::DecodeFailed, error.to_string())
                }
            })?;
        for (chunk_index, chunk) in samples[..frames].chunks(4096).enumerate() {
            if job.cancellation.is_canceled() {
                return Err(canceled_failure());
            }
            let chunk_start = start + (chunk_index * 4096) as u64;
            envelope
                .accumulate_interleaved(chunk_start, WAVEFORM_LAYOUT.channel_count(), chunk)
                .map_err(|error| {
                    WaveformFailure::new(WaveformFailureReason::DecodeFailed, error.to_string())
                })?;
        }
        start = start.saturating_add(frames as u64);
    }
    let envelope = envelope.finish();
    Ok(WaveformSource {
        envelope: envelope.peaks,
        total_frames: envelope.total_frames,
        sample_rate: WAVEFORM_SAMPLE_RATE,
    })
}

fn canceled_failure() -> WaveformFailure {
    WaveformFailure::new(
        WaveformFailureReason::Canceled,
        "waveform execution generation was canceled",
    )
}

pub(super) fn slice_and_resample(
    source: &WaveformSource,
    start_secs: f64,
    end_secs: f64,
    pixel_width: u32,
) -> Vec<f32> {
    let total_secs = source.total_frames as f64 / source.sample_rate as f64;
    let start = (start_secs.max(0.0) / total_secs.max(f64::EPSILON)).clamp(0.0, 1.0);
    let end =
        (end_secs.max(start_secs).min(total_secs) / total_secs.max(f64::EPSILON)).clamp(start, 1.0);
    let len = source.envelope.len();
    let start_index = ((start * len as f64).floor() as usize).min(len.saturating_sub(1));
    let end_index = ((end * len as f64).ceil() as usize).clamp(start_index + 1, len);
    resample_peaks(&source.envelope[start_index..end_index], pixel_width)
}

pub(super) fn resample_peaks(peaks: &[f32], target_width: u32) -> Vec<f32> {
    let target = target_width.clamp(1, WAVEFORM_MAX_WIDTH) as usize;
    if peaks.len() == target {
        return peaks.to_vec();
    }
    if peaks.len() > target {
        return (0..target)
            .map(|index| {
                let start = index.saturating_mul(peaks.len()) / target;
                let end = (index + 1).saturating_mul(peaks.len()).div_ceil(target).min(peaks.len());
                peaks[start..end].iter().copied().fold(0.0_f32, f32::max)
            })
            .collect();
    }
    (0..target)
        .map(|index| {
            let source_index = index.saturating_mul(peaks.len()) / target;
            peaks.get(source_index).copied().unwrap_or(0.0)
        })
        .collect()
}
