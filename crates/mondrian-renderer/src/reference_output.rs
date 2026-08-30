//! Full-raster clean-feed lowering into the professional Reference Output Module.
//!
//! This seam intentionally starts from the canonical working composite and
//! applies only Program Output. Viewer spatial scaling, comparison overlays,
//! monitor adaptation, ICC calibration, Scopes, false color, zebra, and gamut
//! alarms cannot enter this path.

use mondrian_reference_output::{
    pack_encoded_rgb_to_rgb12, pack_encoded_rgb_to_v210, pack_f32_audio_to_s24,
    ReferenceAudioFrame, ReferenceOutputBundle, ReferenceOutputPayloadError,
    ReferenceOutputPixelFormat, ReferenceOutputSignal, ReferenceVideoFrame,
    ReferenceVideoPackingError,
};
use mondrian_timeline::sequence::ProgramColorContext;

use crate::color::{
    ProgramOutputBoundary, ProgramOutputBoundaryError, ProgramOutputModule, ProgramOutputRole,
};
use crate::{CpuColorFrame, RenderColorTransformError, RenderCpuColorExecutionSession};

/// Immutable Program Output + physical-signal lowering contract.
pub struct ReferenceOutputProgram {
    signal: ReferenceOutputSignal,
    boundary: ProgramOutputBoundary,
}

impl ReferenceOutputProgram {
    /// Prepare one full-raster clean-feed contract from canonical Timeline color semantics.
    pub fn prepare(
        context: &ProgramColorContext,
        signal: ReferenceOutputSignal,
    ) -> Result<Self, ReferenceOutputProgramError> {
        signal.validate()?;
        if context.output_color_space().color() != Some(signal.color_space) {
            return Err(ReferenceOutputProgramError::ColorIdentityMismatch);
        }
        let boundary = ProgramOutputModule::boundary(ProgramOutputRole::ReferenceOutput, context)?;
        Ok(Self { signal, boundary })
    }

    /// Exact physical signal bound to this Program.
    pub const fn signal(&self) -> &ReferenceOutputSignal {
        &self.signal
    }

    /// Renderer Program Output boundary, with no monitor adaptation.
    pub const fn boundary(&self) -> &ProgramOutputBoundary {
        &self.boundary
    }

    /// Lower one canonical working composite plus its Audio Program window.
    pub fn execute_cpu(
        &self,
        working_frame: &CpuColorFrame,
        frame_index: u64,
        audio_f32: &[f32],
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<ReferenceOutputBundle, ReferenceOutputProgramError> {
        let descriptor = working_frame.descriptor();
        if descriptor.width != self.signal.width || descriptor.height != self.signal.height {
            return Err(ReferenceOutputProgramError::RasterMismatch {
                expected_width: self.signal.width,
                expected_height: self.signal.height,
                actual_width: descriptor.width,
                actual_height: descriptor.height,
            });
        }
        let expected_audio_frames = self.signal.audio_frames_for_video_frame(frame_index)? as usize;
        let expected_audio_samples = expected_audio_frames
            .checked_mul(self.signal.audio_layout.channel_count())
            .ok_or(ReferenceOutputProgramError::AudioExtentOverflow)?;
        if audio_f32.len() != expected_audio_samples {
            return Err(ReferenceOutputProgramError::AudioExtent {
                expected: expected_audio_samples,
                actual: audio_f32.len(),
            });
        }
        let output =
            ProgramOutputModule::execute_cpu_float(working_frame, &self.boundary, session)?;
        if output.output_descriptor.color_space.color() != Some(self.signal.color_space) {
            return Err(ReferenceOutputProgramError::ColorIdentityMismatch);
        }
        let encoded = output.frame.rgba_f32();
        let (row_bytes, bytes) = match self.signal.pixel_format {
            ReferenceOutputPixelFormat::Yuv422TenV210 => {
                pack_encoded_rgb_to_v210(&self.signal, &encoded.data)?
            }
            ReferenceOutputPixelFormat::Rgb444TwelveIn16Le => {
                pack_encoded_rgb_to_rgb12(&self.signal, &encoded.data)?
            }
        };
        let video =
            ReferenceVideoFrame::from_program_output(&self.signal, frame_index, row_bytes, bytes)?;
        let audio =
            ReferenceAudioFrame::new(&self.signal, frame_index, pack_f32_audio_to_s24(audio_f32)?)?;
        Ok(ReferenceOutputBundle { video, audio })
    }
}

/// Renderer clean-feed preparation/execution failure.
#[derive(Debug, thiserror::Error)]
pub enum ReferenceOutputProgramError {
    /// Signal validation failed.
    #[error(transparent)]
    Signal(#[from] mondrian_reference_output::ReferenceOutputSignalError),
    /// Program Output boundary cannot be built.
    #[error(transparent)]
    Boundary(#[from] ProgramOutputBoundaryError),
    /// Program Output execution failed.
    #[error(transparent)]
    Color(#[from] RenderColorTransformError),
    /// Packed video conversion failed.
    #[error(transparent)]
    VideoPacking(#[from] ReferenceVideoPackingError),
    /// Audio conversion failed.
    #[error(transparent)]
    AudioPacking(#[from] mondrian_reference_output::ReferenceAudioPackingError),
    /// Constructed payload failed its exact contract.
    #[error(transparent)]
    Payload(#[from] ReferenceOutputPayloadError),
    /// Frame/audio cadence could not be represented.
    #[error(transparent)]
    AudioCadence(#[from] mondrian_reference_output::ReferenceAudioCadenceError),
    /// Timeline Program Output identity and device signal differ.
    #[error("reference output signal color identity differs from Timeline Program Output")]
    ColorIdentityMismatch,
    /// Full-raster Program Output must retain Sequence dimensions.
    #[error(
        "reference output raster expected {expected_width}x{expected_height}, got {actual_width}x{actual_height}"
    )]
    RasterMismatch {
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    /// Audio extent arithmetic overflowed.
    #[error("reference output audio extent overflow")]
    AudioExtentOverflow,
    /// Audio window differs from exact frame-boundary cadence.
    #[error("reference output audio has {actual} samples, expected {expected}")]
    AudioExtent { expected: usize, actual: usize },
}
