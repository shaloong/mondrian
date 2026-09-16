//! Qualification admission over the production-selected Viewer scale.

use mondrian_playback::PreviewResolutionScale;

/// GPU extent required by a validation scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewGpuExtentRequirement {
    /// Exercise the exact runtime scale selected by the product coordinator.
    ProductionSelected,
    /// Require unscaled execution of the authored Viewer extent.
    AuthoredFull,
}

/// Admission failure before a qualification scenario starts playback work.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PreviewGpuExtentAdmissionError {
    /// Physical capacity forced the production runtime below authored Full.
    #[error(
        "NotRun: authored-Full Viewer qualification requires Full runtime scale, but the production resource coordinator selected {selected:?}"
    )]
    CapacityLimited {
        /// Scale selected by the production resource coordinator.
        selected: PreviewResolutionScale,
    },
}

/// Admit a validation scenario against the production coordinator's frozen
/// Viewer scale without reimplementing its resource policy.
pub fn admit_preview_gpu_extent(
    requirement: PreviewGpuExtentRequirement,
    runtime_minimum_scale: PreviewResolutionScale,
) -> Result<(), PreviewGpuExtentAdmissionError> {
    if requirement == PreviewGpuExtentRequirement::AuthoredFull
        && runtime_minimum_scale != PreviewResolutionScale::Full
    {
        return Err(PreviewGpuExtentAdmissionError::CapacityLimited {
            selected: runtime_minimum_scale,
        });
    }
    Ok(())
}
