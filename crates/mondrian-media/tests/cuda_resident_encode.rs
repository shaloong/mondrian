#![cfg(target_os = "linux")]

use mondrian_media::{
    CudaResidentHevcEncoderSession, RendererHwAccelDeviceContext, ResidentEncodeBitDepth,
    ResidentEncodeColorimetry, ResidentHevcEncoderConfig,
};

#[test]
#[ignore = "manual NVIDIA qualification; requires a physical CUDA/NVENC device"]
fn exact_cuda_device_root_opens_nvenc_and_allocates_native_surface() {
    let directory = tempfile::tempdir().expect("temporary output directory");
    let root = RendererHwAccelDeviceContext::from_cuda_device_ordinal(0)
        .expect("renderer-qualified CUDA root");
    let mut session = CudaResidentHevcEncoderSession::open(
        &root,
        ResidentHevcEncoderConfig {
            output_path: directory.path().join("resident-hevc.mkv"),
            width: 320,
            height: 180,
            frame_rate_num: 24,
            frame_rate_den: 1,
            sample_aspect_ratio_num: 1,
            sample_aspect_ratio_den: 1,
            bit_depth: ResidentEncodeBitDepth::Eight,
            colorimetry: ResidentEncodeColorimetry::Rec709,
            full_range: false,
            chroma_location: mondrian_media::ResidentEncodeChromaLocation::Left,
            keyframe_interval_frames: 24,
            max_b_frames: 0,
            quantizer: 18,
            surface_pool_size: 3,
        },
    )
    .expect("same-device NVENC Session");
    let frame = session.acquire_input_frame().expect("CUDA input surface");
    let surface = frame.surface();
    assert!(!surface.context.is_null());
    assert_ne!(surface.planes[0].0, 0);
    assert_ne!(surface.planes[1].0, 0);
    assert!(surface.planes[0].1 >= 320);
    assert!(surface.planes[1].1 >= 320);
    assert_eq!(surface.bit_depth, ResidentEncodeBitDepth::Eight);
    assert_eq!(session.diagnostics().surfaces_acquired, 1);
}
