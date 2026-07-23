//! Generated-picture Golden slice: author, export, validate, and reimport.

use super::fixture::{resolve_fixture, sha256_file, CorpusManifest, FixtureEvidence};
use super::harness::{
    fixture_root, new_run_directory, rooted_env_path, wait_for_export_job, wait_for_media_imports,
    write_report, DirectoryCleanup,
};
use super::{
    builtin_preset, load_golden_contract, load_json, repository_root,
    sequence_settings_from_contract, GoldenExportContract,
};
use crate::app::ui_actions::{
    assets_create_solid_color_action, export_enqueue_action, inspector_set_clip_opacity_action,
    inspector_set_clip_transform_field_action, timeline_drop_asset_action,
    timeline_trim_clips_action, AssetsCreateAssetPayload, ExportEnqueuePayload,
    InspectorClipRefPayload, InspectorClipTransformField, InspectorSetClipOpacityPayload,
    InspectorSetClipTransformFieldPayload, TimelineDropAssetPayload, TimelineTrimClipsPayload,
    TimelineTrimPayloadEdge,
};
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::{ExecutionTerminalDisposition, FramePosition, JobId, TimelineTime};
use mondrian_editor_state::Action;
use mondrian_export::preset::TimelineExportRange;
use mondrian_export::queue::JobStatus;
use mondrian_export::validator::{probe_export_output, ExportOutputProbe};
use mondrian_media::info::{AudioCodec, ChannelLayout, PixelFormat, VideoCodec, VideoCodecProfile};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DELIVERY_SLICE_ID: &str = "generated-delivery-roundtrip-v1";
const RUN_ROOT_ENV: &str = "MONDRIAN_GOLDEN_DELIVERY_RUN_ROOT";
const OUTPUT_ENV: &str = "MONDRIAN_GOLDEN_DELIVERY_OUTPUT";
const EXPORT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug)]
struct GoldenRunPaths {
    directory: PathBuf,
    project: PathBuf,
    report: PathBuf,
}

#[derive(Debug, Serialize)]
struct GoldenDeliveryReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    corpus_revision: String,
    status: &'static str,
    fixture: FixtureEvidence,
    setup: DeliverySetupEvidence,
    operations: Vec<OperationEvidence>,
    content: Vec<ContentEvidence>,
}

#[derive(Debug, Serialize)]
struct DeliverySetupEvidence {
    project_path: PathBuf,
    video_track_id: String,
    audio_track_id: String,
    solid_asset_id: String,
    audio_asset_id: String,
    solid_clip_id: String,
    audio_clip_id: String,
    start_frame: i64,
    end_frame_exclusive: i64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum OperationEvidence {
    Trim {
        clip_ids: Vec<String>,
        end_frame_exclusive: i64,
    },
    Export {
        deliveries: Vec<ExportEvidence>,
    },
    Reimport {
        assets: Vec<ReimportEvidence>,
    },
}

impl OperationEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::Trim { .. } => "trim",
            Self::Export { .. } => "export",
            Self::Reimport { .. } => "reimport",
        }
    }
}

#[derive(Debug, Serialize)]
struct ExportEvidence {
    export_id: String,
    job_id: String,
    generation: u64,
    executed: bool,
    terminal_disposition: ExecutionTerminalDisposition,
    output_path: PathBuf,
    output_sha256: String,
    probe: ExportOutputProbe,
}

#[derive(Debug, Serialize)]
struct ReimportEvidence {
    export_id: String,
    asset_id: String,
    container: String,
    video_codec: VideoCodec,
    video_profile: VideoCodecProfile,
    width: u32,
    height: u32,
    frame_rate: mondrian_core::Rational,
    pixel_format: PixelFormat,
    pixel_format_proven: bool,
    bit_depth: u8,
    audio_codec: AudioCodec,
    audio_sample_rate: u32,
    audio_channels: u8,
    audio_channel_layout: ChannelLayout,
}

#[derive(Debug, Serialize)]
#[serde(tag = "id", rename_all = "kebab-case")]
enum ContentEvidence {
    Transform {
        position_x: f32,
        position_y: f32,
        scale_x: f32,
        scale_y: f32,
    },
    Opacity {
        value: f32,
    },
}

impl ContentEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::Transform { .. } => "transform",
            Self::Opacity { .. } => "opacity",
        }
    }
}

fn new_run_paths(root: &Path) -> anyhow::Result<GoldenRunPaths> {
    let directory = new_run_directory(root, RUN_ROOT_ENV, "golden-delivery")?;
    let report = rooted_env_path(root, OUTPUT_ENV, || {
        directory.join("golden-delivery-report.json")
    });
    Ok(GoldenRunPaths {
        project: directory.join("windows-alpha-golden-delivery.mdp"),
        directory,
        report,
    })
}

fn find_new_job_id(state: &AppState, before: &BTreeSet<JobId>) -> anyhow::Result<JobId> {
    let created = state
        .export_jobs_snapshot()
        .into_iter()
        .filter(|snapshot| !before.contains(&snapshot.id))
        .map(|snapshot| snapshot.id)
        .collect::<Vec<_>>();
    ensure!(
        created.len() == 1,
        "export action admitted {} jobs instead of one",
        created.len()
    );
    Ok(created[0])
}

fn normalized_identity(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn expect_probe_string(field: &str, actual: Option<&str>, expected: &str) -> anyhow::Result<()> {
    ensure!(
        actual == Some(expected),
        "{field} mismatch: expected {expected}, actual {}",
        actual.unwrap_or("<missing>")
    );
    Ok(())
}

fn assert_probe_matches_contract(
    probe: &ExportOutputProbe,
    export: &GoldenExportContract,
    frame_rate: mondrian_core::Rational,
    expected_duration_frames: i64,
    duration_error_max_frames: u32,
    audio_sample_rate: u32,
) -> anyhow::Result<()> {
    let expected = &export.expected_delivery;
    let container_format =
        probe.container_format.as_deref().context("container format is unproven")?;
    ensure!(
        expected.container == "mp4"
            && container_format.split(',').any(|identity| identity.trim() == "mp4")
            && probe
                .container_major_brand
                .as_deref()
                .is_some_and(|brand| !brand.eq_ignore_ascii_case("qt")),
        "mux/container identity differs from the Golden delivery contract"
    );
    let video = probe.video.as_ref().context("validated export probe has no video stream")?;
    expect_probe_string(
        "video codec",
        video.codec_name.as_deref(),
        &expected.video_codec,
    )?;
    let actual_profile = video.profile.as_deref().context("video profile is unproven")?;
    ensure!(
        normalized_identity(actual_profile) == normalized_identity(&expected.video_profile),
        "video profile mismatch: expected {}, actual {actual_profile}",
        expected.video_profile
    );
    ensure!(
        video.width == Some(expected.width) && video.height == Some(expected.height),
        "encoded dimensions differ from Golden delivery contract"
    );
    ensure!(
        video.frame_rate_num == Some(frame_rate.num)
            && video.frame_rate_den == Some(frame_rate.den),
        "encoded frame rate differs from Golden timeline"
    );
    ensure!(
        video.bit_depth == Some(expected.bit_depth),
        "encoded bit depth differs from Golden delivery contract"
    );
    expect_probe_string(
        "pixel format",
        video.pixel_format.as_deref(),
        &expected.pixel_format,
    )?;
    expect_probe_string(
        "color primaries",
        video.color_primaries.as_deref(),
        &expected.color_primaries,
    )?;
    expect_probe_string(
        "color transfer",
        video.color_transfer.as_deref(),
        &expected.color_transfer,
    )?;
    expect_probe_string(
        "color matrix",
        video.color_matrix.as_deref(),
        &expected.color_matrix,
    )?;
    let expected_range = match expected.range.as_str() {
        "legal" => "tv",
        "full" => "pc",
        other => anyhow::bail!("unsupported Golden range identity: {other}"),
    };
    expect_probe_string("video range", video.color_range.as_deref(), expected_range)?;
    ensure!(
        expected.static_hdr_metadata == "absent"
            && !video.mastering_display_metadata_present
            && !video.content_light_metadata_present,
        "Golden SDR delivery unexpectedly contains static HDR metadata"
    );
    let actual_duration = probe.duration_secs.context("export duration is unproven")?;
    let expected_duration =
        expected_duration_frames as f64 * frame_rate.den as f64 / frame_rate.num as f64;
    let duration_error_frames = (actual_duration - expected_duration).abs() * frame_rate.to_f64();
    ensure!(
        duration_error_frames <= f64::from(duration_error_max_frames),
        "export duration error is {duration_error_frames:.3} frames"
    );
    let audio = probe.audio.as_ref().context("validated export probe has no audio stream")?;
    expect_probe_string(
        "audio codec",
        audio.codec_name.as_deref(),
        &expected.audio_codec,
    )?;
    ensure!(
        audio.sample_rate == Some(audio_sample_rate),
        "encoded audio sample rate differs from Golden timeline"
    );
    ensure!(
        audio.channels == Some(2) && audio.channel_layout.as_deref() == Some("stereo"),
        "encoded audio layout differs from Golden timeline"
    );
    Ok(())
}

fn export_and_probe(
    state: &mut AppState,
    export: &GoldenExportContract,
    output_path: PathBuf,
    range: TimelineExportRange,
    frame_rate: mondrian_core::Rational,
    duration_frames: i64,
    duration_error_max_frames: u32,
    audio_sample_rate: u32,
) -> anyhow::Result<ExportEvidence> {
    let before = state.export_jobs_snapshot().into_iter().map(|snapshot| snapshot.id).collect();
    state.dispatch_action(export_enqueue_action(ExportEnqueuePayload {
        preset: builtin_preset(&export.builtin_preset_id)?.preset(),
        sequence_id: state.active_sequence().map(|sequence| sequence.id),
        range,
        output_path: output_path.clone(),
    }))?;
    let job_id = find_new_job_id(state, &before)?;
    let snapshot = wait_for_export_job(state, job_id, EXPORT_TIMEOUT)?;
    ensure!(
        matches!(snapshot.status, JobStatus::Completed),
        "export {} ended as {:?}",
        export.id,
        snapshot.status
    );
    ensure!(
        snapshot.executed,
        "export {} never crossed the worker boundary",
        export.id
    );
    let terminal = snapshot
        .terminal_evidence
        .context("completed export has no terminal evidence")?;
    ensure!(
        terminal.generation == snapshot.generation
            && terminal.disposition == ExecutionTerminalDisposition::Completed,
        "export terminal evidence disagrees with completed queue state"
    );
    let output_path = output_path.canonicalize().with_context(|| {
        format!(
            "completed export is not present at {}",
            output_path.display()
        )
    })?;
    let probe = probe_export_output(&output_path).map_err(anyhow::Error::msg)?;
    assert_probe_matches_contract(
        &probe,
        export,
        frame_rate,
        duration_frames,
        duration_error_max_frames,
        audio_sample_rate,
    )?;
    Ok(ExportEvidence {
        export_id: export.id.clone(),
        job_id: job_id.to_string(),
        generation: snapshot.generation,
        executed: snapshot.executed,
        terminal_disposition: terminal.disposition,
        output_sha256: sha256_file(&output_path)?,
        output_path,
        probe,
    })
}

fn reimport_export(
    state: &mut AppState,
    evidence: &ExportEvidence,
    expected: &GoldenExportContract,
    frame_rate: mondrian_core::Rational,
    audio_sample_rate: u32,
) -> anyhow::Result<ReimportEvidence> {
    let before = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(Action::ImportMedia(vec![evidence.output_path.clone()]))?;
    wait_for_media_imports(state)?;
    let imported = state
        .asset_library()
        .context("Asset Library is absent after reimport")?
        .list_assets()?
        .into_iter()
        .filter(|asset| !before.contains(&asset.id))
        .collect::<Vec<_>>();
    ensure!(
        imported.len() == 1,
        "reimport created {} assets instead of one",
        imported.len()
    );
    let asset = &imported[0];
    ensure!(
        asset.kind == AssetKind::Video
            && asset.path == evidence.output_path
            && asset.media_info.container.split(',').any(|identity| identity.trim() == "mp4"),
        "reimport did not retain the finished deliverable identity"
    );
    let video = asset.media_info.primary_video().context("reimported asset has no video")?;
    let audio = asset.media_info.primary_audio().context("reimported asset has no audio")?;
    let expected_codec_profile = match (
        expected.expected_delivery.video_codec.as_str(),
        expected.expected_delivery.video_profile.as_str(),
    ) {
        ("h264", "high") => (VideoCodec::H264, VideoCodecProfile::H264High),
        ("hevc", "main10") => (VideoCodec::H265, VideoCodecProfile::HevcMain10),
        (codec, profile) => {
            anyhow::bail!("unsupported Golden reimport codec/profile: {codec}/{profile}")
        }
    };
    let expected_pixel_format = match expected.expected_delivery.pixel_format.as_str() {
        "yuv420p" => PixelFormat::Yuv420p,
        "yuv420p10le" => PixelFormat::Yuv420p10le,
        pixel_format => {
            anyhow::bail!("unsupported Golden reimport pixel format: {pixel_format}")
        }
    };
    ensure!(
        video.codec == expected_codec_profile.0
            && video.codec_profile == expected_codec_profile.1
            && video.width == expected.expected_delivery.width
            && video.height == expected.expected_delivery.height
            && video.frame_rate == frame_rate
            && video.frame_rate_proven
            && video.pixel_format == expected_pixel_format
            && video.pixel_format_proven
            && video.bit_depth == expected.expected_delivery.bit_depth,
        "reimported video representation differs from the validated output"
    );
    ensure!(
        audio.codec == AudioCodec::Aac
            && audio.sample_rate == audio_sample_rate
            && audio.channels == 2
            && audio.channel_layout == ChannelLayout::Stereo,
        "reimported audio representation differs from the validated output"
    );
    Ok(ReimportEvidence {
        export_id: evidence.export_id.clone(),
        asset_id: asset.id.to_string(),
        container: asset.media_info.container.clone(),
        video_codec: video.codec.clone(),
        video_profile: video.codec_profile,
        width: video.width,
        height: video.height,
        frame_rate: video.frame_rate,
        pixel_format: video.pixel_format,
        pixel_format_proven: video.pixel_format_proven,
        bit_depth: video.bit_depth,
        audio_codec: audio.codec.clone(),
        audio_sample_rate: audio.sample_rate,
        audio_channels: audio.channels,
        audio_channel_layout: audio.channel_layout.clone(),
    })
}

fn execute_delivery_slice(
    root: &Path,
    paths: &GoldenRunPaths,
) -> anyhow::Result<GoldenDeliveryReport> {
    let contract = load_golden_contract(root)?;
    let slice = contract
        .execution_slices
        .iter()
        .find(|slice| slice.id == DELIVERY_SLICE_ID)
        .context("delivery Golden execution slice is missing")?;
    ensure!(
        slice.required_fixture_roles == ["pcm-audio"]
            && slice.required_operations == ["trim", "export", "reimport"]
            && slice.required_content == ["transform", "opacity"]
            && slice.required_exports == ["h264-aac-sdr", "hevc-main10"],
        "delivery slice contract drifted"
    );
    let window = slice.timeline_window.context("delivery slice has no timeline window")?;
    let duration_frames = window.end_frame_exclusive - window.start_frame;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let manifest: CorpusManifest = load_json(&root.join("tests/validation/corpus-manifest.json"))?;
    let fixture = resolve_fixture(root, &fixture_root(root), &contract, &manifest, "pcm-audio")?;

    // Declared before AppState so unwinding drops all open SQLite handles first.
    let mut runtime_cleanup = DirectoryCleanup::default();
    let mut state = AppState::new();
    state.create_new_project_with_settings_at(
        paths.project.clone(),
        "Windows Alpha Golden Delivery",
        settings.clone(),
        mondrian_core::ProjectColorEnvironment::default(),
        mondrian_core::ProjectSettings::default(),
    )?;
    runtime_cleanup.track(state.project_runtime_dir().map(Path::to_path_buf));

    state.dispatch_action(Action::ImportMedia(vec![fixture.path.clone()]))?;
    wait_for_media_imports(&mut state)?;
    let audio_asset = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .find(|asset| asset.path == fixture.path)
        .context("PCM fixture import is absent")?;
    ensure!(
        audio_asset.kind == AssetKind::Audio,
        "PCM fixture is not audio"
    );

    let assets_before = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(assets_create_solid_color_action(AssetsCreateAssetPayload {
        folder_id: None,
    }))?;
    let solid_asset = state
        .asset_library()
        .context("Asset Library is absent")?
        .list_assets()?
        .into_iter()
        .filter(|asset| !assets_before.contains(&asset.id))
        .find(|asset| asset.kind == AssetKind::SolidColor)
        .context("solid-color product action created no asset")?;

    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let video_track_id = sequence.video_tracks[0].id;
    let audio_track_id = sequence.audio_tracks[0].id;
    let video_before = sequence.video_tracks[0]
        .clips
        .iter()
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    let audio_before = sequence.audio_tracks[0]
        .clips
        .iter()
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
        asset_id: solid_asset.id,
        target_track_id: video_track_id,
        is_video_track: true,
        frame: window.start_frame,
    }))?;
    state.dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
        asset_id: audio_asset.id,
        target_track_id: audio_track_id,
        is_video_track: false,
        frame: window.start_frame,
    }))?;
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let solid_clip_id = sequence.video_tracks[0]
        .clips
        .iter()
        .find(|clip| !video_before.contains(&clip.id))
        .map(|clip| clip.id)
        .context("solid-color timeline drop created no Clip")?;
    let audio_clip_id = sequence.audio_tracks[0]
        .clips
        .iter()
        .find(|clip| !audio_before.contains(&clip.id))
        .map(|clip| clip.id)
        .context("PCM timeline drop created no Clip")?;
    state.dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
        clip_ids: vec![solid_clip_id, audio_clip_id],
        edge: TimelineTrimPayloadEdge::Out,
        frame: window.end_frame_exclusive,
    }))?;
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let time_base = sequence.time_base();
    let expected_start =
        TimelineTime::from_frame_position(FramePosition::new(window.start_frame, time_base))?;
    let expected_end = TimelineTime::from_frame_position(FramePosition::new(
        window.end_frame_exclusive,
        time_base,
    ))?;
    for clip in [
        sequence.video_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == solid_clip_id)
            .context("trim lost solid Clip")?,
        sequence.audio_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == audio_clip_id)
            .context("trim lost audio Clip")?,
    ] {
        ensure!(
            clip.position == expected_start && clip.end_position()? == expected_end,
            "trim did not produce the exact Golden window"
        );
    }

    let solid_clip_ref = InspectorClipRefPayload {
        track_id: video_track_id,
        is_video_track: true,
        clip_id: solid_clip_id,
    };
    for (field, value) in [
        (InspectorClipTransformField::PositionX, 96.0),
        (InspectorClipTransformField::PositionY, 54.0),
        (InspectorClipTransformField::ScalePercent, 90.0),
    ] {
        state.dispatch_action(inspector_set_clip_transform_field_action(
            InspectorSetClipTransformFieldPayload { clip: solid_clip_ref, field, value },
        ))?;
    }
    state.dispatch_action(inspector_set_clip_opacity_action(
        InspectorSetClipOpacityPayload { clip: solid_clip_ref, opacity_percent: 80.0 },
    ))?;
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let solid_clip = sequence.video_tracks[0]
        .clips
        .iter()
        .find(|clip| clip.id == solid_clip_id)
        .context("authored solid Clip is absent")?;
    let position = solid_clip.transform.get_position(expected_start);
    let scale = solid_clip.transform.get_scale(expected_start);
    let opacity = solid_clip.transform.evaluate_opacity(expected_start);
    ensure!(
        position.x == 96.0
            && position.y == 54.0
            && scale.x == 0.9
            && scale.y == 0.9
            && (opacity - 0.8).abs() < 1.0e-6,
        "authored transform/opacity did not survive the product actions"
    );
    let content = vec![
        ContentEvidence::Transform {
            position_x: position.x,
            position_y: position.y,
            scale_x: scale.x,
            scale_y: scale.y,
        },
        ContentEvidence::Opacity { value: opacity },
    ];

    let range = TimelineExportRange::WorkArea {
        start_frame: window.start_frame,
        end_frame_exclusive: window.end_frame_exclusive,
    };
    let mut exports = Vec::new();
    for export_id in &slice.required_exports {
        let export = contract
            .exports
            .iter()
            .find(|export| &export.id == export_id)
            .with_context(|| format!("Golden export contract is absent: {export_id}"))?;
        exports.push(export_and_probe(
            &mut state,
            export,
            paths.directory.join(format!("{export_id}.mp4")),
            range,
            settings.frame_rate,
            duration_frames,
            contract.acceptance.duration_error_max_frames,
            settings.audio_sample_rate,
        )?);
    }
    let mut reimports = Vec::new();
    for evidence in &exports {
        let expected = contract
            .exports
            .iter()
            .find(|export| export.id == evidence.export_id)
            .context("export evidence lost its Golden contract")?;
        reimports.push(reimport_export(
            &mut state,
            evidence,
            expected,
            settings.frame_rate,
            settings.audio_sample_rate,
        )?);
    }
    let operations = vec![
        OperationEvidence::Trim {
            clip_ids: vec![solid_clip_id.to_string(), audio_clip_id.to_string()],
            end_frame_exclusive: window.end_frame_exclusive,
        },
        OperationEvidence::Export { deliveries: exports },
        OperationEvidence::Reimport { assets: reimports },
    ];
    let operation_ids = operations.iter().map(OperationEvidence::id).collect::<BTreeSet<_>>();
    let required_operations =
        slice.required_operations.iter().map(String::as_str).collect::<BTreeSet<_>>();
    ensure!(
        operation_ids == required_operations,
        "operation evidence does not exactly cover the delivery slice"
    );
    let content_ids = content.iter().map(ContentEvidence::id).collect::<BTreeSet<_>>();
    let required_content =
        slice.required_content.iter().map(String::as_str).collect::<BTreeSet<_>>();
    ensure!(
        content_ids == required_content,
        "content evidence does not exactly cover the delivery slice"
    );

    Ok(GoldenDeliveryReport {
        schema_version: 3,
        profile: DELIVERY_SLICE_ID,
        contract_id: contract.id,
        corpus_revision: manifest.corpus_revision,
        status: "passed",
        fixture,
        setup: DeliverySetupEvidence {
            project_path: paths.project.clone(),
            video_track_id: video_track_id.to_string(),
            audio_track_id: audio_track_id.to_string(),
            solid_asset_id: solid_asset.id.to_string(),
            audio_asset_id: audio_asset.id.to_string(),
            solid_clip_id: solid_clip_id.to_string(),
            audio_clip_id: audio_clip_id.to_string(),
            start_frame: window.start_frame,
            end_frame_exclusive: window.end_frame_exclusive,
        },
        operations,
        content,
    })
}

#[test]
#[ignore = "Golden delivery gate requires generated PCM plus production FFmpeg encoders"]
fn golden_project_generated_delivery_roundtrip_gate() -> anyhow::Result<()> {
    let root = repository_root();
    let paths = new_run_paths(&root)?;
    match execute_delivery_slice(&root, &paths) {
        Ok(report) => {
            write_report(&paths.report, &report)?;
            eprintln!(
                "MONDRIAN_GOLDEN_DELIVERY_REPORT_JSON={}",
                serde_json::to_string(&report)?
            );
            eprintln!(
                "MONDRIAN_GOLDEN_DELIVERY_REPORT_PATH={}",
                paths.report.display()
            );
            eprintln!(
                "MONDRIAN_GOLDEN_DELIVERY_RUN_DIRECTORY={}",
                paths.directory.display()
            );
            Ok(())
        }
        Err(error) => {
            let failure = serde_json::json!({
                "schema_version": 3,
                "profile": DELIVERY_SLICE_ID,
                "status": "failed",
                "error": format!("{error:#}")
            });
            write_report(&paths.report, &failure)?;
            Err(error)
        }
    }
}
