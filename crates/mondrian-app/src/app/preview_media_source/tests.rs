use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::timeline_data::{AlphaInterpretation, AssetMediaInterpretation};
use mondrian_core::types::{AssetId, ColorSpace, Rational};
use mondrian_core::{ProjectColorManagement, Resolution, TimelineTime};
use mondrian_media::info::{PixelFormat, VideoCodec};
use mondrian_media::{
    DetectedColorInterpretation, MediaInfo, VideoCodecProfile, VideoColorDetectionMethod,
    VideoColorInterpretationConfidence, VideoColorSpaceSource, VideoStreamInfo,
};
use mondrian_timeline::sequence::{MissingColorMetadataPolicy, Sequence};

use super::*;
use crate::app::native_video_import::PlaybackHardwareDecodeAdmission;

fn unique_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{name}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH).expect("system time").as_nanos()
    ))
}

fn proxy_config(cache_dir: PathBuf) -> ProxyConfig {
    ProxyConfig { cache_dir, ..ProxyConfig::default() }
}

fn proxy_color(bit_depth: u8) -> ProxyColorContract {
    ProxyColorContract::try_new(ColorSpace::Rec709, bit_depth, DecodedVideoRange::Limited)
        .expect("valid proxy color contract")
}

fn install_proxy_manifest(generator: &ProxyGenerator, source: &Path, proxy: &Path) {
    let manifest = generator
        .expected_manifest(source, proxy_color(8))
        .expect("expected proxy manifest");
    let bytes = serde_json::to_vec_pretty(&manifest).expect("serialize proxy manifest");
    std::fs::write(ProxyGenerator::manifest_path(proxy), bytes).expect("write proxy manifest");
}

fn video_asset(path: PathBuf) -> AssetRecord {
    AssetRecord {
        id: AssetId::new(),
        name: "source.mp4".to_owned(),
        kind: AssetKind::Video,
        path: path.clone(),
        source: None,
        folder_id: None,
        interpretation: AssetMediaInterpretation::default(),
        media_info: MediaInfo {
            path,
            duration: Duration::from_secs(2),
            file_size: 6,
            container: "mp4".to_owned(),
            video_streams: vec![VideoStreamInfo {
                index: 0,
                codec: VideoCodec::H265,
                duration: Some(Duration::from_secs(2)),
                codec_profile: VideoCodecProfile::HevcMain10,
                width: 3840,
                height: 2160,
                frame_rate: Rational::new(25, 1),
                frame_rate_proven: true,
                pixel_format: PixelFormat::P010,
                pixel_format_proven: true,
                color_range: DecodedVideoRange::Limited,
                detected_color_space: Some(ColorSpace::Rec709),
                color_interpretation: DetectedColorInterpretation {
                    color_space: Some(ColorSpace::Rec709),
                    confidence: VideoColorInterpretationConfidence::High,
                    source: VideoColorSpaceSource::Metadata,
                    method: VideoColorDetectionMethod::CicpTags,
                    evidence: Vec::new(),
                    warnings: Vec::new(),
                    user_overridable: true,
                },
                color_space_source: VideoColorSpaceSource::Metadata,
                color_detection_method: VideoColorDetectionMethod::CicpTags,
                color_metadata: None,
                color_metadata_hints: Vec::new(),
                hdr_metadata: Vec::new(),
                bit_depth: 10,
                has_alpha: false,
                avg_bitrate: 20_000_000,
                total_frames: Some(50),
            }],
            audio_streams: Vec::new(),
            has_video: true,
            has_audio: false,
        },
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn color_context() -> ColorContext {
    Sequence::new("preview media source")
        .settings
        .root_preview_color_context(&ProjectColorManagement::default(), ColorSpace::Rec709)
}

fn gpu_admission() -> PreviewHardwareDecodeAdmissionState {
    PreviewHardwareDecodeAdmissionState::reported(PlaybackHardwareDecodeAdmission {
        request: PreviewHardwareDecodeRequest::PreferGpuResident,
        hardware_decode_device_selector: None,
        renderer_native_import_ready: true,
        platform_native_import_ready: true,
        native_import_admission_ready: true,
        admission_blocker: None,
        platform_discovery_available: true,
        platform_zero_copy_supported: true,
        platform_low_copy_fallback_supported: false,
        renderer_supported_handle_kinds: 1,
        renderer_supported_source_texture_formats: 1,
        renderer_supports_nv12: true,
        renderer_supports_p010: true,
    })
}

#[test]
fn fresh_proxy_is_selected_but_alpha_source_is_not() {
    let root = unique_root("mondrian-preview-proxy-hit");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let config = proxy_config(root.join("proxy"));
    let generator = ProxyGenerator::new(config.clone());
    let proxy_path = generator.proxy_path(&source, proxy_color(8)).expect("proxy path");
    std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
    std::fs::write(&proxy_path, b"proxy").expect("proxy");
    install_proxy_manifest(&generator, &source, &proxy_path);

    let resolved =
        resolve_preview_media_decode_path(true, false, &source, &config, Some(proxy_color(8)))
            .expect("fresh proxy path");
    assert_eq!(resolved.path, proxy_path);
    assert_eq!(resolved.resolution, PreviewMediaDecodePathResolution::Proxy);
    assert_eq!(resolved.fingerprint.len, Some(5));

    let alpha =
        resolve_preview_media_decode_path(true, true, &source, &config, Some(proxy_color(8)))
            .expect("alpha source path");
    assert_eq!(alpha.path, source);
    assert_eq!(alpha.resolution, PreviewMediaDecodePathResolution::Source);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_and_stale_proxy_fall_back_to_fingerprinted_source() {
    let root = unique_root("mondrian-preview-proxy-fallback");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let config = proxy_config(root.join("proxy"));

    let missing =
        resolve_preview_media_decode_path(true, false, &source, &config, Some(proxy_color(8)))
            .expect("missing proxy falls back");
    assert_eq!(missing.path, source);
    assert_eq!(
        missing.resolution,
        PreviewMediaDecodePathResolution::ProxyMissing
    );
    assert_eq!(missing.fingerprint.len, Some(6));

    let proxy_path = ProxyGenerator::new(config.clone())
        .proxy_path(&source, proxy_color(8))
        .expect("proxy path");
    std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
    std::fs::write(&proxy_path, b"proxy").expect("proxy");
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&source, b"newer source").expect("newer source");

    let stale =
        resolve_preview_media_decode_path(true, false, &source, &config, Some(proxy_color(8)))
            .expect("stale proxy falls back");
    assert_eq!(stale.path, source);
    assert_eq!(
        stale.resolution,
        PreviewMediaDecodePathResolution::ProxyStale
    );
    assert_eq!(stale.fingerprint.len, Some(12));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn complete_resolution_emits_one_canonical_key_and_proxy_intent() {
    let root = unique_root("mondrian-preview-source-resolution");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let asset = video_asset(source.clone());
    let config = proxy_config(root.join("proxy"));
    let context = color_context();

    let outcome = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_time: TimelineTime::new(1, 2).expect("exact source time"),
        target_resolution: Resolution { width: 960, height: 540 },
        color_context: &context,
        prefer_proxy: true,
        request_missing_proxy_generation: true,
        proxy_config: &config,
        proxy_color: Some(proxy_color(10)),
        hardware_admission: gpu_admission(),
    });
    let PreviewMediaSourceOutcome::Ready(resolved) = outcome else {
        panic!("valid media source should resolve");
    };

    assert_eq!(
        resolved.path_resolution,
        PreviewMediaDecodePathResolution::ProxyMissing
    );
    assert_eq!(resolved.key.path, source);
    assert_eq!(
        resolved.key.source_time,
        TimelineTime::new(1, 2).expect("exact source time")
    );
    assert_eq!(
        (resolved.key.target_width, resolved.key.target_height),
        (3840, 2160)
    );
    assert_eq!(
        resolved.key.native_surface_hint,
        Some(MediaPreviewNativeSurfaceHint::P010)
    );
    assert!(resolved.proxy_generation.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unavailable_and_color_rejected_sources_are_explicit_outcomes() {
    let root = unique_root("mondrian-preview-source-failures");
    let source = root.join("source.mp4");
    let asset = video_asset(source.clone());
    let config = proxy_config(root.join("proxy"));
    let mut context = color_context();

    let unavailable = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_time: TimelineTime::ZERO,
        target_resolution: Resolution { width: 320, height: 180 },
        color_context: &context,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &config,
        proxy_color: None,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
    });
    assert!(matches!(
        unavailable,
        PreviewMediaSourceOutcome::Unavailable(_)
    ));

    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let mut untagged = video_asset(source);
    untagged.media_info.video_streams[0].detected_color_space = None;
    context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
    let rejected = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &untagged,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_time: TimelineTime::ZERO,
        target_resolution: Resolution { width: 320, height: 180 },
        color_context: &context,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &config,
        proxy_color: None,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
    });
    assert!(matches!(
        rejected,
        PreviewMediaSourceOutcome::ColorRejected(_)
    ));
    let _ = std::fs::remove_dir_all(&root);
}
