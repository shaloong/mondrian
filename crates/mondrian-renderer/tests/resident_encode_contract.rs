use mondrian_media::{ResidentEncodeBitDepth, ResidentEncodeColorimetry};
use mondrian_renderer::{
    D3D12ResidentEncodeAdapterContract, D3D12ResidentEncodeAdapterCreateError,
    GpuColorFrameTextureFormat,
};

#[test]
fn contract_rejects_unrepresentable_signal_before_device_access() {
    let base = D3D12ResidentEncodeAdapterContract {
        width: 1920,
        height: 1080,
        frame_rate_num: 24,
        frame_rate_den: 1,
        bit_depth: ResidentEncodeBitDepth::Ten,
        colorimetry: ResidentEncodeColorimetry::Rec2100Pq,
        full_range: false,
        max_frames_in_flight: 4,
    };
    assert_eq!(
        base.source_texture_format(),
        GpuColorFrameTextureFormat::Rgba16Float
    );
    assert!(base.validate_static().is_ok());
    assert!(matches!(
        D3D12ResidentEncodeAdapterContract { full_range: true, ..base }.validate_static(),
        Err(D3D12ResidentEncodeAdapterCreateError::UnsupportedSignal { .. })
    ));
    assert!(matches!(
        D3D12ResidentEncodeAdapterContract {
            colorimetry: ResidentEncodeColorimetry::Rec2100Hlg,
            ..base
        }
        .validate_static(),
        Err(D3D12ResidentEncodeAdapterCreateError::UnsupportedSignal { .. })
    ));
}
