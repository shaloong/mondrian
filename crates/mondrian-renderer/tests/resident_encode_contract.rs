use mondrian_media::{ResidentEncodeBitDepth, ResidentEncodeColorimetry};
use mondrian_renderer::{
    GpuColorFrameTextureFormat, ResidentEncodeAdapterContract, ResidentEncodeAdapterCreateError,
};

#[test]
fn contract_rejects_unrepresentable_signal_before_device_access() {
    let base = ResidentEncodeAdapterContract {
        width: 1920,
        height: 1080,
        frame_rate_num: 24,
        frame_rate_den: 1,
        bit_depth: ResidentEncodeBitDepth::Ten,
        colorimetry: ResidentEncodeColorimetry::Rec2100Pq,
        full_range: false,
        chroma_location: mondrian_media::ResidentEncodeChromaLocation::Left,
        max_frames_in_flight: 4,
    };
    assert_eq!(
        base.source_texture_format(),
        GpuColorFrameTextureFormat::Rgba16Float
    );
    assert!(base.validate_d3d12().is_ok());
    assert!(matches!(
        ResidentEncodeAdapterContract { full_range: true, ..base }.validate_d3d12(),
        Err(ResidentEncodeAdapterCreateError::UnsupportedSignal { .. })
    ));
    assert!(matches!(
        ResidentEncodeAdapterContract {
            colorimetry: ResidentEncodeColorimetry::Rec2100Hlg,
            ..base
        }
        .validate_d3d12(),
        Err(ResidentEncodeAdapterCreateError::UnsupportedSignal { .. })
    ));
}
