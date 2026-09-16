#![cfg(feature = "validation")]

use mondrian_app::app::preview_validation_extent::{
    admit_preview_gpu_extent, PreviewGpuExtentAdmissionError, PreviewGpuExtentRequirement,
};
use mondrian_playback::PreviewResolutionScale;

#[test]
fn production_smoke_accepts_the_coordinator_scale_without_claiming_full() {
    assert!(admit_preview_gpu_extent(
        PreviewGpuExtentRequirement::ProductionSelected,
        PreviewResolutionScale::Half,
    )
    .is_ok());
}

#[test]
fn authored_full_qualification_is_not_run_when_capacity_selects_half() {
    assert_eq!(
        admit_preview_gpu_extent(
            PreviewGpuExtentRequirement::AuthoredFull,
            PreviewResolutionScale::Half,
        ),
        Err(PreviewGpuExtentAdmissionError::CapacityLimited {
            selected: PreviewResolutionScale::Half,
        })
    );
}

#[test]
fn authored_full_qualification_admits_only_full() {
    assert!(admit_preview_gpu_extent(
        PreviewGpuExtentRequirement::AuthoredFull,
        PreviewResolutionScale::Full,
    )
    .is_ok());
}
