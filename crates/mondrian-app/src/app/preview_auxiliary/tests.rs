use super::*;
use mondrian_assets::{AssetLibrary, AssetLibraryMembership, AssetMediaProbeCandidate};
use mondrian_core::{ColorSpace, Rational, VisualSourceBoundary, VisualSourceRetime};
use mondrian_media::info::{PixelFormat, VideoCodec};
use mondrian_media::{
    DecodedVideoRange, DetectedColorInterpretation, MediaFileFingerprint, MediaInfo,
    VideoCodecProfile, VideoColorDetectionMethod, VideoColorInterpretationConfidence,
    VideoColorSpaceSource, VideoStreamInfo,
};
use std::time::Duration;

pub(crate) fn asset_fixture(duration: Option<Duration>) -> (tempfile::TempDir, AssetRecord) {
    asset_fixture_with_picture(duration, mondrian_core::PictureStreamMetadata::default())
}

pub(crate) fn asset_fixture_with_picture(
    duration: Option<Duration>,
    picture: mondrian_core::PictureStreamMetadata,
) -> (tempfile::TempDir, AssetRecord) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.media");
    std::fs::write(&path, b"frozen source revision for auxiliary planning").unwrap();
    let fingerprint = MediaFileFingerprint::capture(&path);
    let info = MediaInfo {
        // The audio/container may outlast the picture; never use this for retime.
        duration: Duration::from_secs(100),
        file_size: fingerprint.len.unwrap(),
        container: "fixture".into(),
        video_streams: vec![VideoStreamInfo {
            index: 0,
            codec: VideoCodec::H264,
            duration,
            codec_profile: VideoCodecProfile::H264High,
            width: 4,
            height: 2,
            picture,
            frame_rate: Rational::new(24, 1),
            frame_rate_proven: true,
            pixel_format: PixelFormat::Rgba,
            pixel_format_proven: true,
            color_range: DecodedVideoRange::Full,
            color_interpretation: DetectedColorInterpretation {
                candidate_color_space: None,
                confidence: VideoColorInterpretationConfidence::None,
                source: VideoColorSpaceSource::MissingMetadata,
                method: VideoColorDetectionMethod::MissingMetadata,
                evidence: Vec::new(),
                warnings: Vec::new(),
                user_overridable: true,
            },
            color_metadata: None,
            color_metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
            camera_raw: None,
            bit_depth: 8,
            has_alpha: true,
            avg_bitrate: 0,
            total_frames: None,
        }],
        audio_streams: Vec::new(),
        has_video: true,
        has_audio: false,
    };
    let library = AssetLibrary::open(root.path().join("library")).unwrap();
    let id = library
        .commit_media_probe(
            AssetMediaProbeCandidate::new(path, fingerprint, info).unwrap(),
            None,
        )
        .unwrap();
    let mut asset = library.get_asset(id).unwrap().unwrap();
    asset.interpretation.color = mondrian_core::timeline_data::MediaColorInterpretation::Override {
        color_space: ColorSpace::Rec709,
    };
    (root, asset)
}

pub(crate) fn request(
    asset: &AssetRecord,
    time: TimelineTime,
    mode: VisualSourceBoundary,
) -> PreviewAuxiliaryAssetRequest {
    let sequence = mondrian_timeline::Sequence::new("auxiliary source facts");
    let context = sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .unwrap();
    PreviewAuxiliaryAssetRequest {
        reference: VisualSourceReference::Asset {
            asset_id: asset.id,
            retime: VisualSourceRetime { time_offset: TimelineTime::ZERO, mode },
        },
        sequence_time: time,
        target_resolution: Resolution { width: 1, height: 1 },
        input_color: context.media_input(false),
    }
}

fn resolve(
    request: &PreviewAuxiliaryAssetRequest,
    asset: &AssetRecord,
) -> Result<PreviewAuxiliaryAssetPlan, PreviewAuxiliaryAssetError> {
    resolve_preview_auxiliary_asset(
        request,
        asset,
        PreviewHardwareDecodeAdmissionState::default(),
    )
}

#[test]
fn auxiliary_asset_plan_uses_exact_video_duration_and_strict_end_boundary() {
    let (_root, asset) = asset_fixture(Some(Duration::new(2, 333)));
    let duration = TimelineTime::new(2_000_000_333, 1_000_000_000).unwrap();
    for mode in [VisualSourceBoundary::Clamp, VisualSourceBoundary::Hold] {
        let plan = resolve(&request(&asset, duration, mode), &asset).unwrap();
        assert!(plan.time.out_of_range);
        assert_eq!(
            plan.time.source_sample,
            Some(SourceSampleTarget::strict_predecessor(duration))
        );
        let media = plan.media_request.unwrap();
        assert_eq!(
            media.source_sample,
            SourceSampleTarget::strict_predecessor(duration)
        );
        assert!(media.cpu_working_required);
        assert_eq!(
            media.image_scale_requirement,
            mondrian_effects::EffectImageScaleRequirement::Unit
        );
        assert_eq!(
            plan.validated_key.residency_resolution(),
            Resolution { width: 4, height: 2 }
        );
    }
    let mut intent = request(
        &asset,
        TimelineTime::new(1, 3).unwrap(),
        VisualSourceBoundary::Clamp,
    );
    intent.reference = VisualSourceReference::Asset {
        asset_id: asset.id,
        retime: VisualSourceRetime {
            time_offset: TimelineTime::new(1, 7).unwrap(),
            mode: VisualSourceBoundary::Clamp,
        },
    };
    assert_eq!(
        resolve(&intent, &asset).unwrap().time.requested_time,
        TimelineTime::new(10, 21).unwrap()
    );
}

#[test]
fn auxiliary_transparency_still_validates_the_current_source_revision() {
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let intent = request(
        &asset,
        TimelineTime::new(50, 1).unwrap(),
        VisualSourceBoundary::Transparent,
    );
    let plan = resolve(&intent, &asset).unwrap();
    assert!(plan.time.out_of_range);
    assert!(plan.media_request.is_none());
    std::fs::write(
        asset.file_path().unwrap(),
        b"replaced source with different bytes and length",
    )
    .unwrap();
    assert!(
        resolve(&intent, &asset).is_err(),
        "old duration cannot authorize a transparent substitute after relink/replacement"
    );
    std::fs::remove_file(asset.file_path().unwrap()).unwrap();
    assert!(resolve(&intent, &asset).is_err());
}

#[test]
fn auxiliary_still_images_are_timeless_and_retired_strong_references_remain_valid() {
    let (_root, mut asset) = asset_fixture(None);
    asset.kind = AssetKind::StillImage;
    asset.membership = AssetLibraryMembership::Retired { at: "fixture".into() };
    for time in [
        TimelineTime::new(-999, 1).unwrap(),
        TimelineTime::new(999, 1).unwrap(),
    ] {
        let plan = resolve(
            &request(&asset, time, VisualSourceBoundary::Transparent),
            &asset,
        )
        .unwrap();
        assert_eq!(plan.time.requested_time, time);
        assert!(!plan.time.out_of_range);
        assert_eq!(
            plan.media_request.unwrap().source_sample,
            SourceSampleTarget::covering(TimelineTime::ZERO)
        );
    }
}

#[test]
fn auxiliary_unknown_video_ranges_and_data_payloads_are_refused() {
    for duration in [None, Some(Duration::ZERO)] {
        let (_root, asset) = asset_fixture(duration);
        assert!(resolve(
            &request(&asset, TimelineTime::ZERO, VisualSourceBoundary::Clamp),
            &asset
        )
        .is_err());
    }
    let (_root, mut asset) = asset_fixture(Some(Duration::from_secs(2)));
    asset.interpretation.payload = mondrian_core::timeline_data::AssetColorPayload::NonColorData;
    assert!(resolve(
        &request(&asset, TimelineTime::ZERO, VisualSourceBoundary::Clamp),
        &asset
    )
    .is_err());
}

#[test]
fn auxiliary_source_absence_is_typed_separately_from_invalid_contracts() {
    let (_root, mut asset) = asset_fixture(Some(Duration::from_secs(2)));
    let intent = request(&asset, TimelineTime::ZERO, VisualSourceBoundary::Clamp);
    let mut wrong = intent.clone();
    wrong.reference = VisualSourceReference::Asset {
        asset_id: mondrian_core::AssetId::new(),
        retime: VisualSourceRetime::default(),
    };
    assert!(matches!(
        resolve(&wrong, &asset),
        Err(PreviewAuxiliaryAssetError::Invalid(_))
    ));
    std::fs::remove_file(asset.file_path().unwrap()).unwrap();
    assert!(matches!(
        resolve(&intent, &asset),
        Err(PreviewAuxiliaryAssetError::SourceUnavailable(_))
    ));
    // A missing file cannot hide an invalid payload interpretation.
    asset.interpretation.payload = mondrian_core::timeline_data::AssetColorPayload::NonColorData;
    assert!(matches!(
        resolve(&intent, &asset),
        Err(PreviewAuxiliaryAssetError::Invalid(_))
    ));
}

#[test]
fn auxiliary_generic_terminal_errors_do_not_authorize_source_substitution() {
    use crate::app::preview_unavailability::PreviewUnavailabilityDisposition;
    let stopped = PreviewUnavailability::stopped(PreviewOutputStage::MediaResolution, "canceled");
    let error = PreviewAuxiliaryAssetError::from(stopped.clone());
    assert!(matches!(error, PreviewAuxiliaryAssetError::Invalid(_)));
    let original: PreviewUnavailability = error.into();
    assert_eq!(original, stopped);
    assert_eq!(
        original.disposition(),
        PreviewUnavailabilityDisposition::Stopped
    );
}
