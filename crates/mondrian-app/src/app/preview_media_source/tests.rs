use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mondrian_assets::{AssetLibrary, AssetMediaProbeCandidate, AssetRecord};
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{ColorSpace, Rational};
use mondrian_core::{Resolution, TimelineTime};
use mondrian_media::info::{PixelFormat, VideoCodec};
use mondrian_media::{
    DecodedVideoRange, DetectedColorInterpretation, MediaInfo, PreviewHardwareDecodeRequest,
    PreviewNativeSurfaceHint, VideoCodecProfile, VideoColorDetectionMethod,
    VideoColorInterpretationConfidence, VideoColorInterpretationEvidence,
    VideoColorMetadataHintScope, VideoColorSpaceSource, VideoStreamInfo,
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

fn preview_source_color() -> PreviewSourceColorContract {
    PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited)
}

fn install_proxy_manifest(generator: &ProxyGenerator, source: &Path, proxy: &Path) {
    let manifest = generator
        .expected_manifest(source, proxy_color(8))
        .expect("expected proxy manifest");
    let bytes = serde_json::to_vec_pretty(&manifest).expect("serialize proxy manifest");
    std::fs::write(ProxyGenerator::manifest_path(proxy), bytes).expect("write proxy manifest");
}

fn video_asset(path: PathBuf) -> AssetRecord {
    video_asset_with_detected_color(path, Some(ColorSpace::Rec709))
}

fn video_asset_with_detected_color(
    path: PathBuf,
    executable_color_space: Option<ColorSpace>,
) -> AssetRecord {
    video_asset_with_physical_stream(
        path,
        executable_color_space,
        0,
        PixelFormat::P010,
        true,
        10,
        false,
    )
}

fn video_asset_with_sampling_evidence(
    path: PathBuf,
    executable_color_space: Option<ColorSpace>,
    pixel_format_proven: bool,
    bit_depth: u8,
    has_alpha: bool,
) -> AssetRecord {
    let pixel_format = match (bit_depth, has_alpha) {
        (8, true) => PixelFormat::Rgba,
        (8, false) => PixelFormat::Nv12,
        (10, false) => PixelFormat::P010,
        _ => PixelFormat::P010,
    };
    video_asset_with_physical_stream(
        path,
        executable_color_space,
        0,
        pixel_format,
        pixel_format_proven,
        bit_depth,
        has_alpha,
    )
}

fn video_asset_with_physical_stream(
    path: PathBuf,
    executable_color_space: Option<ColorSpace>,
    stream_index: u32,
    pixel_format: PixelFormat,
    pixel_format_proven: bool,
    bit_depth: u8,
    has_alpha: bool,
) -> AssetRecord {
    let color_interpretation = DetectedColorInterpretation {
        candidate_color_space: executable_color_space,
        confidence: VideoColorInterpretationConfidence::High,
        source: executable_color_space
            .map(|_| VideoColorSpaceSource::Metadata)
            .unwrap_or(VideoColorSpaceSource::MissingMetadata),
        method: executable_color_space
            .map(|_| VideoColorDetectionMethod::CicpTags)
            .unwrap_or(VideoColorDetectionMethod::MissingMetadata),
        evidence: executable_color_space
            .map(
                |color_space| VideoColorInterpretationEvidence::ExactCicpTags {
                    primaries: mondrian_media::VideoColorTag {
                        code: 1,
                        name: Some("bt709".to_owned()),
                        specified: true,
                    },
                    transfer: mondrian_media::VideoColorTag {
                        code: 1,
                        name: Some("bt709".to_owned()),
                        specified: true,
                    },
                    matrix: mondrian_media::VideoColorTag {
                        code: 1,
                        name: Some("bt709".to_owned()),
                        specified: true,
                    },
                    detected_color_space: color_space,
                },
            )
            .into_iter()
            .collect(),
        warnings: Vec::new(),
        user_overridable: true,
    };
    video_asset_with_physical_stream_and_interpretation(
        path,
        color_interpretation,
        stream_index,
        pixel_format,
        pixel_format_proven,
        bit_depth,
        has_alpha,
    )
}

fn video_asset_with_physical_stream_and_interpretation(
    path: PathBuf,
    color_interpretation: DetectedColorInterpretation,
    stream_index: u32,
    pixel_format: PixelFormat,
    pixel_format_proven: bool,
    bit_depth: u8,
    has_alpha: bool,
) -> AssetRecord {
    let color_metadata = color_interpretation.evidence.iter().find_map(|evidence| {
        let VideoColorInterpretationEvidence::ExactCicpTags { primaries, transfer, matrix, .. } =
            evidence
        else {
            return None;
        };
        Some(mondrian_media::VideoColorMetadata {
            primaries: primaries.clone(),
            transfer: transfer.clone(),
            matrix: matrix.clone(),
        })
    });
    let path = path.canonicalize().expect("canonical fixture media path");
    let fingerprint = mondrian_media::MediaFileFingerprint::capture(&path);
    let media_info = MediaInfo {
        duration: Duration::from_secs(2),
        file_size: fingerprint.len.expect("fixture file size"),
        container: "mp4".to_owned(),
        video_streams: vec![VideoStreamInfo {
            index: stream_index,
            codec: VideoCodec::H265,
            duration: Some(Duration::from_secs(2)),
            codec_profile: VideoCodecProfile::HevcMain10,
            width: 3840,
            height: 2160,
            frame_rate: Rational::new(25, 1),
            frame_rate_proven: true,
            pixel_format,
            pixel_format_proven,
            color_range: DecodedVideoRange::Limited,
            color_interpretation,
            color_metadata,
            color_metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
            bit_depth,
            has_alpha,
            avg_bitrate: 20_000_000,
            total_frames: Some(50),
        }],
        audio_streams: Vec::new(),
        has_video: true,
        has_audio: false,
    };
    let library_root = path.parent().expect("fixture media parent").join("asset-library");
    let library = AssetLibrary::open(library_root).expect("fixture Asset Library");
    let candidate = AssetMediaProbeCandidate::new(path, fingerprint, media_info)
        .expect("valid media candidate");
    let asset_id = library.commit_media_probe(candidate, None).expect("commit fixture media");
    library.get_asset(asset_id).expect("read fixture Asset").expect("fixture Asset")
}

fn color_context() -> MediaInputColorContext {
    Sequence::new("preview media source")
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .media_input(true)
}

fn gpu_admission() -> PreviewHardwareDecodeAdmissionState {
    PreviewHardwareDecodeAdmissionState::reported(PlaybackHardwareDecodeAdmission {
        request: PreviewHardwareDecodeRequest::PreferGpuResident,
        hardware_decode_device_selector: None,
        renderer_native_import_ready: true,
        renderer_import_mode: Some(mondrian_renderer::GpuNativeDecodedFrameImportMode::ZeroCopy),
        native_import_admission_ready: true,
        admission_blocker: None,
        renderer_supported_handle_kinds: 1,
        renderer_supported_source_texture_formats: 1,
        renderer_supports_nv12: true,
        renderer_supports_p010: true,
    })
}

#[test]
fn fresh_h264_proxy_owns_stream_zero_and_nv12_while_original_keeps_absolute_stream() {
    let root = unique_root("mondrian-preview-proxy-hit");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let config = proxy_config(root.join("proxy"));
    let generator = ProxyGenerator::new(config.clone());
    let asset = video_asset_with_physical_stream(
        source.clone(),
        Some(ColorSpace::Rec709),
        7,
        PixelFormat::Nv12,
        true,
        8,
        false,
    );
    let canonical_source = asset.file_path().expect("file-backed fixture Asset").to_path_buf();
    let primary_video = asset
        .media_probe()
        .expect("fixture probe")
        .primary_video()
        .expect("video")
        .clone();
    let proxy_path = generator.proxy_path(&source, proxy_color(8)).expect("proxy path");
    std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
    std::fs::write(&proxy_path, b"proxy").expect("proxy");
    install_proxy_manifest(&generator, &source, &proxy_path);

    let resolved = resolve_preview_media_decode_path(
        true,
        false,
        &source,
        &primary_video,
        preview_source_color(),
        &config,
        Some(proxy_color(8)),
    )
    .expect("fresh proxy path");
    assert_eq!(resolved.source.path(), proxy_path);
    assert_eq!(resolved.resolution, PreviewMediaDecodePathResolution::Proxy);
    assert_eq!(resolved.source.fingerprint().len, Some(5));
    assert_eq!(resolved.source.video_stream_index(), 0);
    assert_eq!(
        resolved.source.native_surface_hint(),
        Some(PreviewNativeSurfaceHint::Nv12)
    );

    let original = resolve_preview_media_decode_path(
        false,
        false,
        &source,
        &primary_video,
        preview_source_color(),
        &config,
        Some(proxy_color(8)),
    )
    .expect("original source path");
    assert_eq!(original.source.path(), source);
    assert_eq!(
        original.resolution,
        PreviewMediaDecodePathResolution::Source
    );
    assert_eq!(original.source.video_stream_index(), 7);
    assert_eq!(
        original.source.native_surface_hint(),
        Some(PreviewNativeSurfaceHint::Nv12)
    );

    let context = color_context();
    let PreviewMediaSourceOutcome::Ready(keyed) =
        resolve_preview_media_source(PreviewMediaSourceRequest {
            asset: &asset,
            color_space_override: None,
            alpha_interpretation: AlphaInterpretation::Straight,
            source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ONE_THIRD),
            target_resolution: Resolution { width: 960, height: 540 },
            input_color: &context,
            prefer_proxy: true,
            request_missing_proxy_generation: false,
            proxy_config: &config,
            proxy_color: Some(proxy_color(8)),
            hardware_admission: gpu_admission(),
            cpu_working_required: false,
        })
    else {
        panic!("fresh H.264 proxy should resolve into the canonical App key");
    };
    assert_eq!(keyed.key.decode.source().path(), proxy_path);
    assert_eq!(keyed.key.decode.source().video_stream_index(), 0);
    assert_eq!(
        keyed.key.native_surface_hint(),
        Some(PreviewNativeSurfaceHint::Nv12)
    );
    assert_eq!(
        keyed.key.decode.geometry(),
        PreviewDecodeGeometry::NativeSource { target: Resolution { width: 960, height: 540 } }
    );
    assert_eq!(
        keyed.key.decode.source_color().color_space,
        proxy_color(8).source_color_space()
    );
    assert_eq!(
        keyed.key.decode.source_color().range.baseline(),
        proxy_color(8).source_range()
    );

    let PreviewMediaSourceOutcome::Ready(overridden) =
        resolve_preview_media_source(PreviewMediaSourceRequest {
            asset: &asset,
            color_space_override: Some(ColorSpace::Rec2020),
            alpha_interpretation: AlphaInterpretation::Straight,
            source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ONE_THIRD),
            target_resolution: Resolution { width: 960, height: 540 },
            input_color: &context,
            prefer_proxy: true,
            request_missing_proxy_generation: true,
            proxy_config: &config,
            proxy_color: Some(proxy_color(8)),
            hardware_admission: gpu_admission(),
            cpu_working_required: false,
        })
    else {
        panic!("a color-incompatible proxy must safely fall back to the original");
    };
    assert_eq!(
        overridden.path_resolution,
        PreviewMediaDecodePathResolution::ProxyColorIncompatible
    );
    assert_eq!(
        overridden.key.decode.source().path(),
        canonical_source,
        "complete Preview resolution must use the canonical path admitted by the Asset Library"
    );
    assert_eq!(overridden.key.decode.source().video_stream_index(), 7);
    assert_eq!(
        overridden.key.decode.source_color().color_space,
        ColorSpace::Rec2020
    );
    assert!(
        overridden.proxy_generation.is_none(),
        "a mismatched proxy contract cannot authorize generation under that stale identity"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn filename_log_suggestion_cannot_change_preview_color_plan_but_override_does() {
    let root = unique_root("mondrian-preview-filename-color-authority");
    let plain_path = root.join("camera-original.mp4");
    let suggested_path = root.join("camera-S-Log3_S-Gamut3.Cine.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    let bytes = b"identical synthetic media revision";
    std::fs::write(&plain_path, bytes).expect("plain source");
    std::fs::write(&suggested_path, bytes).expect("renamed source");
    assert_eq!(
        std::fs::read(&plain_path).expect("read plain source"),
        std::fs::read(&suggested_path).expect("read renamed source")
    );

    let missing_metadata = mondrian_media::VideoColorMetadata {
        primaries: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
        transfer: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
        matrix: mondrian_media::VideoColorTag { code: 2, name: None, specified: false },
    };
    let suggestion = mondrian_media::parse_video_color_metadata_hint(
        VideoColorMetadataHintScope::FileName,
        "filename",
        suggested_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 test name"),
    )
    .expect("complete pair remains visible as a suggestion");
    let plain_interpretation =
        mondrian_media::interpret_video_color_metadata(&missing_metadata, None, &[]);
    let suggested_interpretation = mondrian_media::interpret_video_color_metadata(
        &missing_metadata,
        None,
        std::slice::from_ref(&suggestion),
    );
    assert_eq!(
        suggested_interpretation.candidate_color_space,
        Some(ColorSpace::SonySLog3SGamut3Cine)
    );
    assert_eq!(
        suggested_interpretation.executable_color_space_from_probe(
            Some(mondrian_media::ProvenVideoSampling {
                pixel_format: PixelFormat::P010,
                bit_depth: 10,
                has_alpha: false,
            }),
            Some(&missing_metadata),
            std::slice::from_ref(&suggestion),
        ),
        None
    );

    let plain = video_asset_with_physical_stream_and_interpretation(
        plain_path,
        plain_interpretation,
        0,
        PixelFormat::P010,
        true,
        10,
        false,
    );
    let suggested = video_asset_with_physical_stream_and_interpretation(
        suggested_path,
        suggested_interpretation,
        0,
        PixelFormat::P010,
        true,
        10,
        false,
    );
    let config = proxy_config(root.join("proxy"));
    let context = color_context();
    let resolve = |asset: &AssetRecord, color_space_override| {
        let PreviewMediaSourceOutcome::Ready(resolved) =
            resolve_preview_media_source(PreviewMediaSourceRequest {
                asset,
                color_space_override,
                alpha_interpretation: AlphaInterpretation::Straight,
                source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
                target_resolution: Resolution { width: 320, height: 180 },
                input_color: &context,
                prefer_proxy: false,
                request_missing_proxy_generation: false,
                proxy_config: &config,
                proxy_color: None,
                hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
                cpu_working_required: true,
            })
        else {
            panic!("synthetic source should resolve");
        };
        resolved
    };

    let plain_plan = resolve(&plain, None);
    let suggested_plan = resolve(&suggested, None);
    assert_eq!(
        plain_plan.input_color_resolution, suggested_plan.input_color_resolution,
        "renaming identical bytes must not change the semantic Preview color plan"
    );
    assert_eq!(
        plain_plan.key.decode.source_color(),
        suggested_plan.key.decode.source_color()
    );
    assert_eq!(
        suggested_plan.key.decode.source_color().color_space,
        ColorSpace::Rec709
    );

    let overridden = resolve(&suggested, Some(ColorSpace::SonySLog3SGamut3Cine));
    assert_eq!(
        overridden.key.decode.source_color().color_space,
        ColorSpace::SonySLog3SGamut3Cine
    );
    assert_eq!(
        overridden.input_color_resolution.source,
        mondrian_timeline::sequence::InputColorResolutionSource::Override
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_and_stale_proxy_fall_back_to_fingerprinted_source() {
    let root = unique_root("mondrian-preview-proxy-fallback");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let config = proxy_config(root.join("proxy"));
    let asset = video_asset_with_physical_stream(
        source.clone(),
        Some(ColorSpace::Rec709),
        7,
        PixelFormat::Nv12,
        true,
        8,
        false,
    );
    let primary_video = asset
        .media_probe()
        .expect("fixture probe")
        .primary_video()
        .expect("video")
        .clone();

    let missing = resolve_preview_media_decode_path(
        true,
        false,
        &source,
        &primary_video,
        preview_source_color(),
        &config,
        Some(proxy_color(8)),
    )
    .expect("missing proxy falls back");
    assert_eq!(missing.source.path(), source);
    assert_eq!(
        missing.resolution,
        PreviewMediaDecodePathResolution::ProxyMissing
    );
    assert_eq!(missing.source.fingerprint().len, Some(6));

    let proxy_path = ProxyGenerator::new(config.clone())
        .proxy_path(&source, proxy_color(8))
        .expect("proxy path");
    std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
    std::fs::write(&proxy_path, b"proxy").expect("proxy");
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&source, b"newer source").expect("newer source");

    let stale = resolve_preview_media_decode_path(
        true,
        false,
        &source,
        &primary_video,
        preview_source_color(),
        &config,
        Some(proxy_color(8)),
    )
    .expect("stale proxy falls back");
    assert_eq!(stale.source.path(), source);
    assert_eq!(
        stale.resolution,
        PreviewMediaDecodePathResolution::ProxyStale
    );
    assert_eq!(stale.source.fingerprint().len, Some(12));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn complete_resolution_emits_one_canonical_key_and_proxy_intent() {
    let root = unique_root("mondrian-preview-source-resolution");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let asset = video_asset_with_physical_stream(
        source.clone(),
        Some(ColorSpace::Rec709),
        7,
        PixelFormat::P010,
        true,
        10,
        false,
    );
    let config = proxy_config(root.join("proxy"));
    let context = color_context();

    let outcome = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(
            TimelineTime::new(1, 2).expect("exact source time"),
        ),
        target_resolution: Resolution { width: 960, height: 540 },
        input_color: &context,
        prefer_proxy: true,
        request_missing_proxy_generation: true,
        proxy_config: &config,
        proxy_color: Some(proxy_color(10)),
        hardware_admission: gpu_admission(),
        cpu_working_required: false,
    });
    let PreviewMediaSourceOutcome::Ready(resolved) = outcome else {
        panic!("valid media source should resolve");
    };

    assert_eq!(
        resolved.path_resolution,
        PreviewMediaDecodePathResolution::ProxyMissing
    );
    assert_eq!(
        resolved.key.decode.source().path(),
        asset.file_path().expect("file-backed fixture Asset")
    );
    assert_eq!(
        resolved.key.source_sample().time(),
        TimelineTime::new(1, 2).expect("exact source time")
    );
    assert_eq!(resolved.key.decode.source().video_stream_index(), 7);
    assert_eq!(
        resolved.key.decode.geometry(),
        PreviewDecodeGeometry::NativeSource { target: Resolution { width: 960, height: 540 } }
    );
    assert_eq!(
        resolved.key.native_surface_hint(),
        Some(PreviewNativeSurfaceHint::P010)
    );
    assert!(resolved.proxy_generation.is_some());

    let cpu_outcome = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(
            TimelineTime::new(1, 2).expect("exact source time"),
        ),
        target_resolution: Resolution { width: 960, height: 540 },
        input_color: &context,
        prefer_proxy: true,
        request_missing_proxy_generation: false,
        proxy_config: &config,
        proxy_color: Some(proxy_color(10)),
        hardware_admission: gpu_admission(),
        cpu_working_required: true,
    });
    let PreviewMediaSourceOutcome::Ready(cpu_resolved) = cpu_outcome else {
        panic!("CPU-working media source should resolve");
    };
    assert_eq!(
        cpu_resolved.key.decode.geometry(),
        PreviewDecodeGeometry::FitWithin(Resolution { width: 960, height: 540 }),
        "CPU-working requests retain the sampled extent instead of native-surface geometry"
    );
    assert_ne!(
        resolved.key, cpu_resolved.key,
        "opaque-native and CPU-working requests must never share scheduler/cache identity"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn undiscovered_hardware_or_unmapped_surface_keeps_cpu_geometry() {
    let root = unique_root("mondrian-preview-native-admission-evidence");
    let p010_path = root.join("p010.mp4");
    let rgb_path = root.join("rgb.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&p010_path, b"p010").expect("P010 source");
    std::fs::write(&rgb_path, b"rgb").expect("RGB source");
    let p010 = video_asset_with_physical_stream(
        p010_path,
        Some(ColorSpace::Rec709),
        7,
        PixelFormat::P010,
        true,
        10,
        false,
    );
    let rgb = video_asset_with_physical_stream(
        rgb_path,
        Some(ColorSpace::Rec709),
        3,
        PixelFormat::Rgb24,
        true,
        8,
        false,
    );
    let config = proxy_config(root.join("proxy"));
    let context = color_context();
    let target = Resolution { width: 960, height: 540 };
    let resolve = |asset: &AssetRecord, hardware_admission| {
        resolve_preview_media_source(PreviewMediaSourceRequest {
            asset,
            color_space_override: None,
            alpha_interpretation: AlphaInterpretation::Straight,
            source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
            target_resolution: target,
            input_color: &context,
            prefer_proxy: false,
            request_missing_proxy_generation: false,
            proxy_config: &config,
            proxy_color: None,
            hardware_admission,
            cpu_working_required: false,
        })
    };

    let PreviewMediaSourceOutcome::Ready(undiscovered) =
        resolve(&p010, PreviewHardwareDecodeAdmissionState::default())
    else {
        panic!("P010 source should resolve without hardware discovery");
    };
    assert_eq!(
        undiscovered.key.decode.geometry(),
        PreviewDecodeGeometry::FitWithin(target)
    );

    let PreviewMediaSourceOutcome::Ready(unmapped) = resolve(&rgb, gpu_admission()) else {
        panic!("RGB source should resolve through CPU geometry");
    };
    assert_eq!(unmapped.key.native_surface_hint(), None);
    assert_eq!(
        unmapped.key.decode.geometry(),
        PreviewDecodeGeometry::FitWithin(target)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unproven_sampling_blocks_preview_proxy_precision_and_native_surface_admission() {
    let root = unique_root("mondrian-preview-unproven-sampling");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let asset = video_asset_with_sampling_evidence(
        source.clone(),
        Some(ColorSpace::Rec709),
        false,
        8,
        false,
    );
    let config = proxy_config(root.join("proxy"));
    let generator = ProxyGenerator::new(config.clone());
    let proxy_path = generator.proxy_path(&source, proxy_color(8)).expect("proxy path");
    std::fs::create_dir_all(proxy_path.parent().expect("proxy parent")).expect("proxy root");
    std::fs::write(&proxy_path, b"proxy").expect("proxy");
    install_proxy_manifest(&generator, &source, &proxy_path);
    let context = color_context();

    let outcome = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: Resolution { width: 960, height: 540 },
        input_color: &context,
        prefer_proxy: true,
        request_missing_proxy_generation: true,
        proxy_config: &config,
        proxy_color: Some(proxy_color(8)),
        hardware_admission: gpu_admission(),
        cpu_working_required: false,
    });
    assert!(matches!(
        &outcome,
        PreviewMediaSourceOutcome::Unavailable(UnavailablePreviewMediaSource {
            reason: PreviewMediaSourceUnavailableReason::SourceSamplingUnavailable,
            ..
        })
    ));
    let PreviewMediaSourceOutcome::Unavailable(unavailable) = outcome else {
        panic!("missing sampling must be unavailable")
    };
    assert!(unavailable.reason.to_string().contains("Interpret Asset"));
    assert!(
        crate::app::proxy_generation::resolve_asset_proxy_color_contract(&asset, &context).is_err()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unavailable_and_color_rejected_sources_are_explicit_outcomes() {
    let root = unique_root("mondrian-preview-source-failures");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let asset = video_asset(source.clone());
    std::fs::remove_file(&source).expect("remove source for unavailable fixture");
    let config = proxy_config(root.join("proxy"));
    let mut context = color_context();

    let unavailable = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: Resolution { width: 320, height: 180 },
        input_color: &context,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &config,
        proxy_color: None,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
        cpu_working_required: false,
    });
    assert!(matches!(
        unavailable,
        PreviewMediaSourceOutcome::Unavailable(UnavailablePreviewMediaSource {
            reason: PreviewMediaSourceUnavailableReason::SourceMetadataUnavailable { .. },
            ..
        })
    ));

    std::fs::write(&source, b"source").expect("source");
    let untagged = video_asset_with_detected_color(source, None);
    context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
    let rejected = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &untagged,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: Resolution { width: 320, height: 180 },
        input_color: &context,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &config,
        proxy_color: None,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
        cpu_working_required: false,
    });
    assert!(matches!(
        rejected,
        PreviewMediaSourceOutcome::ColorRejected(_)
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn assume_rec709_policy_binds_yuv_matrix_into_decode_identity() {
    let root = unique_root("mondrian-preview-source-rec709-matrix-policy");
    let source = root.join("untagged-source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let asset = video_asset_with_detected_color(source, None);
    let config = proxy_config(root.join("proxy"));
    let mut context = color_context();
    context.missing_metadata_policy = MissingColorMetadataPolicy::AssumeRec709;

    let outcome = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: Resolution { width: 320, height: 180 },
        input_color: &context,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &config,
        proxy_color: None,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
        cpu_working_required: true,
    });
    let PreviewMediaSourceOutcome::Ready(ready) = outcome else {
        panic!("assumed Rec.709 source must produce a decode identity")
    };
    assert_eq!(
        ready.key.decode.source_color().yuv_matrix_fallback,
        Some(DecodedVideoMatrix::Bt709)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn changed_source_revision_is_rejected_before_decode_uses_stale_probe_facts() {
    let root = unique_root("mondrian-preview-source-revision-drift");
    let source = root.join("source.mp4");
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::write(&source, b"source").expect("source");
    let asset = video_asset(source.clone());
    std::fs::write(&source, b"changed source revision").expect("replace source");
    let config = proxy_config(root.join("proxy"));
    let context = color_context();

    let outcome = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset: &asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: Resolution { width: 320, height: 180 },
        input_color: &context,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &config,
        proxy_color: None,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
        cpu_working_required: false,
    });
    assert!(matches!(
        outcome,
        PreviewMediaSourceOutcome::Unavailable(UnavailablePreviewMediaSource {
            reason: PreviewMediaSourceUnavailableReason::SourceRevisionChanged,
            ..
        })
    ));

    let _ = std::fs::remove_dir_all(&root);
}
