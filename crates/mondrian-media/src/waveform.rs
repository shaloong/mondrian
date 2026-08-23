//! Streaming audio-waveform analysis primitives.
//!
//! This Module owns only deterministic PCM-to-envelope math. Asset identity,
//! source revision, execution admission, cancellation, cache policy, and
//! terminal evidence belong to the calling service.

use std::sync::Arc;

/// Maximum number of retained columns in one waveform envelope.
pub const MAX_WAVEFORM_WIDTH: u32 = 4096;

/// Immutable positive-peak envelope over one complete source duration.
#[derive(Debug, Clone, PartialEq)]
pub struct WaveformEnvelope {
    /// Positive absolute peaks in source-time order.
    pub peaks: Arc<[f32]>,
    /// Exact source-frame span represented by the envelope.
    pub total_frames: u64,
}

/// Invalid streaming input supplied to waveform analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WaveformAnalysisError {
    /// A complete-source envelope requires a positive frame span.
    #[error("waveform source duration must contain at least one frame")]
    EmptySource,
    /// Interleaved PCM requires at least one channel.
    #[error("waveform PCM channel count must be positive")]
    InvalidChannelCount,
    /// The sample slice did not end on an interleaved frame boundary.
    #[error("waveform PCM sample count is not divisible by its channel count")]
    PartialInterleavedFrame,
    /// The submitted source-frame range exceeded the declared source span.
    #[error("waveform PCM range exceeds the declared source duration")]
    SourceRangeExceeded,
}

/// Bounded streaming peak accumulator for one complete source.
///
/// Chunks may be submitted at arbitrary non-overlapping or overlapping source
/// coordinates. Every frame maps directly to its final column, so partitioning
/// the same PCM into different decode windows produces the same envelope.
#[derive(Debug)]
pub struct WaveformEnvelopeBuilder {
    total_frames: u64,
    peaks: Vec<f32>,
}

impl WaveformEnvelopeBuilder {
    /// Create an accumulator for one positive source span and requested width.
    pub fn new(total_frames: u64, requested_width: u32) -> Result<Self, WaveformAnalysisError> {
        if total_frames == 0 {
            return Err(WaveformAnalysisError::EmptySource);
        }
        let width = requested_width.clamp(1, MAX_WAVEFORM_WIDTH) as usize;
        Ok(Self { total_frames, peaks: vec![0.0; width] })
    }

    /// Accumulate an interleaved PCM chunk at its exact source-frame position.
    pub fn accumulate_interleaved(
        &mut self,
        start_frame: u64,
        channels: usize,
        samples: &[f32],
    ) -> Result<(), WaveformAnalysisError> {
        if channels == 0 {
            return Err(WaveformAnalysisError::InvalidChannelCount);
        }
        if !samples.len().is_multiple_of(channels) {
            return Err(WaveformAnalysisError::PartialInterleavedFrame);
        }
        let frame_count = samples.len() / channels;
        let end_frame = start_frame
            .checked_add(
                u64::try_from(frame_count)
                    .map_err(|_| WaveformAnalysisError::SourceRangeExceeded)?,
            )
            .ok_or(WaveformAnalysisError::SourceRangeExceeded)?;
        if end_frame > self.total_frames {
            return Err(WaveformAnalysisError::SourceRangeExceeded);
        }

        let width = self.peaks.len();
        for (frame_offset, frame) in samples.chunks_exact(channels).enumerate() {
            let absolute_frame = start_frame
                + u64::try_from(frame_offset)
                    .map_err(|_| WaveformAnalysisError::SourceRangeExceeded)?;
            let column = usize::try_from(
                (u128::from(absolute_frame) * width as u128) / u128::from(self.total_frames),
            )
            .unwrap_or(width - 1)
            .min(width - 1);
            let peak = frame.iter().copied().map(f32::abs).fold(0.0_f32, f32::max).clamp(0.0, 1.0);
            self.peaks[column] = self.peaks[column].max(peak);
        }
        Ok(())
    }

    /// Finish the immutable envelope without copying its peak payload.
    pub fn finish(self) -> WaveformEnvelope {
        WaveformEnvelope {
            peaks: Arc::from(self.peaks),
            total_frames: self.total_frames,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitioning_does_not_change_envelope() {
        let samples = [0.1_f32, -0.4, 0.8, -0.2, 1.0, 0.3, -0.7, 0.2];
        let mut whole = WaveformEnvelopeBuilder::new(4, 2).expect("valid envelope");
        whole.accumulate_interleaved(0, 2, &samples).expect("valid whole chunk");

        let mut partitioned = WaveformEnvelopeBuilder::new(4, 2).expect("valid envelope");
        partitioned
            .accumulate_interleaved(0, 2, &samples[..4])
            .expect("valid first chunk");
        partitioned
            .accumulate_interleaved(2, 2, &samples[4..])
            .expect("valid second chunk");

        assert_eq!(whole.finish(), partitioned.finish());
    }

    #[test]
    fn rejects_invalid_interleaving_and_source_range() {
        let mut builder = WaveformEnvelopeBuilder::new(2, 2).expect("valid envelope");
        assert_eq!(
            builder.accumulate_interleaved(0, 2, &[0.0]),
            Err(WaveformAnalysisError::PartialInterleavedFrame)
        );
        assert_eq!(
            builder.accumulate_interleaved(2, 1, &[0.0]),
            Err(WaveformAnalysisError::SourceRangeExceeded)
        );
    }

    #[test]
    fn width_is_bounded_and_peak_is_preserved() {
        let mut builder =
            WaveformEnvelopeBuilder::new(3, u32::MAX).expect("valid bounded envelope");
        builder.accumulate_interleaved(0, 1, &[0.25, -1.2, 0.5]).expect("valid samples");
        let envelope = builder.finish();
        assert_eq!(envelope.peaks.len(), MAX_WAVEFORM_WIDTH as usize);
        assert!(envelope.peaks.contains(&1.0));
    }
}
