//! Proxy/original selection and offline relink Golden slice over product Interfaces.

use super::fixture::{resolve_fixture, sha256_bytes, sha256_file, CorpusManifest, FixtureEvidence};
use super::harness::{
    author_checkpoint, author_transition, ensure_exact_requirement_evidence, fixture_root,
    project_author_transition, wait_for_media_imports, AuthorCheckpoint, AuthorTransitionEvidence,
    ProjectAuthorTransitionEvidence,
};
#[cfg(test)]
use super::harness::{new_run_directory, rooted_env_path, write_report};
use super::retime_media_evidence::{execute_retime_media_evidence, GoldenRetimeMediaEvidence};
use super::workflow::{GoldenProductWorkflowDriver, GoldenSequenceStageEvidence};
#[cfg(test)]
use super::{load_golden_contract, repository_root, sequence_settings_from_contract};
use super::{load_json, GoldenProjectContract};
use crate::app::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;
use crate::app::preview_media_source::{
    resolve_preview_media_source, PreviewMediaDecodePathResolution, PreviewMediaSourceOutcome,
    PreviewMediaSourceRequest,
};
use crate::app::proxy_generation::resolve_app_state_proxy_color_contract;
use crate::app::ui_actions::{
    assets_relink_asset_action, assets_set_proxy_mode_action, timeline_drop_asset_action,
    timeline_trim_clips_action, track_add_action, AssetsRelinkAssetPayload,
    AssetsSetProxyModePayload, TimelineDropAssetPayload, TimelineTrimClipsPayload,
    TimelineTrimPayloadEdge, TrackAddKind, TrackAddPayload,
};
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::events::AppEvent;
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::{
    AssetId, ClipId, ExecutionTerminalDisposition, FramePosition, Rational, SequenceId, TimeScale,
    TimelineTime, TrackId,
};
use mondrian_editor_state::Action;
use mondrian_media::info::{PixelFormat, VideoCodec, VideoCodecProfile};
use mondrian_media::{DecodedVideoRange, MediaFileFingerprint, ProxyGenerator, ProxyStatus};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) const PROXY_RELINK_SLICE_ID: &str = "proxy-relink-v1";
#[cfg(test)]
const RUN_ROOT_ENV: &str = "MONDRIAN_GOLDEN_PROXY_RELINK_RUN_ROOT";
#[cfg(test)]
const OUTPUT_ENV: &str = "MONDRIAN_GOLDEN_PROXY_RELINK_OUTPUT";
const PROXY_TIMEOUT: Duration = Duration::from_secs(120);
const CFR_WINDOW_START_FRAME: i64 = 200;
const CFR_WINDOW_END_FRAME_EXCLUSIVE: i64 = 350;
const VFR_WINDOW_START_FRAME: i64 = 350;
const VFR_WINDOW_END_FRAME_EXCLUSIVE: i64 = 500;

#[derive(Debug, Clone, Serialize)]
struct ImportedVideoObservation {
    codec: VideoCodec,
    codec_profile: VideoCodecProfile,
    pixel_format: PixelFormat,
    bit_depth: u8,
    width: u32,
    height: u32,
    frame_rate: String,
    total_frames: Option<u64>,
    color_space: String,
    range: DecodedVideoRange,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ResolvedPathEvidence {
    pub(super) path: PathBuf,
    pub(super) path_sha256: String,
    pub(super) resolution: &'static str,
    pub(super) fingerprint: MediaFileFingerprint,
}

#[derive(Debug, Clone, Serialize)]
struct ProxyExecutionEvidence {
    attempt_id: u64,
    generation: u64,
    disposition: ExecutionTerminalDisposition,
    executed: bool,
    source_fingerprint: MediaFileFingerprint,
    elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
struct LibraryRelinkTransitionEvidence {
    before: AuthorCheckpoint,
    after: AuthorCheckpoint,
    operation_id: u64,
    generation: u64,
    disposition: ExecutionTerminalDisposition,
    elapsed_ms: u128,
    asset_library_revision_before: u64,
    asset_library_revision_after: u64,
    reload_event_published: bool,
}

#[derive(Debug, Serialize)]
struct ProxyOriginalSwitchEvidence {
    initial_generation: ProxyExecutionEvidence,
    proxy_before: ResolvedPathEvidence,
    disable_proxy: ProjectAuthorTransitionEvidence,
    original: ResolvedPathEvidence,
    enable_proxy: ProjectAuthorTransitionEvidence,
    proxy_after: ResolvedPathEvidence,
    return_to_original: ProjectAuthorTransitionEvidence,
}

#[derive(Debug, Serialize)]
struct OfflineRelinkEvidence {
    unavailable_source_path: PathBuf,
    unavailable_reason: String,
    relink: LibraryRelinkTransitionEvidence,
    retained_asset_id: AssetId,
    retained_clip_id: ClipId,
    replacement_source: ResolvedPathEvidence,
    prior_source_proxy_path: PathBuf,
    replacement_proxy_path: PathBuf,
    replacement_proxy_was_missing: bool,
    enable_relinked_proxy: ProjectAuthorTransitionEvidence,
    replacement_generation: ProxyExecutionEvidence,
    replacement_proxy: ResolvedPathEvidence,
}

#[derive(Debug, Serialize)]
#[serde(tag = "id")]
enum OperationEvidence {
    #[serde(rename = "proxy-original-switch")]
    ProxyOriginalSwitch(Box<ProxyOriginalSwitchEvidence>),
    #[serde(rename = "offline-relink")]
    OfflineRelink(Box<OfflineRelinkEvidence>),
    #[serde(rename = "constant-retime")]
    ConstantRetime {
        evidence: Box<GoldenRetimeMediaEvidence>,
    },
}

impl OperationEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::ProxyOriginalSwitch(_) => "proxy-original-switch",
            Self::OfflineRelink(_) => "offline-relink",
            Self::ConstantRetime { .. } => "constant-retime",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ProxyRelinkSetupEvidence {
    stage: GoldenSequenceStageEvidence,
    video_track_id: TrackId,
    add_video_track: AuthorTransitionEvidence,
    placements: Vec<ProxyRelinkPlacementEvidence>,
    replacement_copy_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct ProxyRelinkPlacementEvidence {
    role: &'static str,
    asset_id: AssetId,
    clip_id: ClipId,
    window: super::GoldenTimelineWindow,
    imported_video: ImportedVideoObservation,
    place_clip: AuthorTransitionEvidence,
    trim_to_window: AuthorTransitionEvidence,
    source_copy_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ProxyRelinkClipAnchor {
    role: &'static str,
    clip_id: ClipId,
    asset_id: AssetId,
    asset_sha256: String,
    position: TimelineTime,
    duration: TimelineTime,
    source_origin: TimelineTime,
    source_scale: TimeScale,
    source_terminal_boundary: TimelineTime,
    asset_proxy_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GoldenProxyRelinkAuthoringAnchor {
    sequence_id: SequenceId,
    video_track_id: TrackId,
    track_sha256: String,
    clips: Vec<ProxyRelinkClipAnchor>,
    project_proxy_enabled: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct GoldenProxyRelinkReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    corpus_revision: String,
    status: &'static str,
    complete_golden_project: bool,
    fixtures: Vec<FixtureEvidence>,
    setup: ProxyRelinkSetupEvidence,
    operations: Vec<OperationEvidence>,
    authoring: GoldenProxyRelinkAuthoringAnchor,
}

impl GoldenProxyRelinkReport {
    pub(super) fn primary_sequence_id(&self) -> SequenceId {
        self.setup.stage.sequence_id()
    }

    pub(super) fn asset_id(&self) -> anyhow::Result<AssetId> {
        self.setup
            .placements
            .first()
            .map(|placement| placement.asset_id)
            .context("Proxy/Relink report has no primary placement")
    }

    fn capture_authoring_anchor(
        &self,
        state: &AppState,
    ) -> anyhow::Result<GoldenProxyRelinkAuthoringAnchor> {
        capture_proxy_relink_authoring_anchor(
            state,
            self.primary_sequence_id(),
            self.setup.video_track_id,
            &self
                .setup
                .placements
                .iter()
                .map(|placement| (placement.role, placement.clip_id, placement.asset_id))
                .collect::<Vec<_>>(),
        )
    }

    pub(super) fn verify_retained_authoring(&self, state: &AppState) -> anyhow::Result<()> {
        ensure!(
            self.capture_authoring_anchor(state)? == self.authoring,
            "Proxy/Relink Track, Clip, Asset, or proxy author intent changed after the stage"
        );
        Ok(())
    }
}

#[derive(Debug)]
#[cfg(test)]
struct GoldenRunPaths {
    directory: PathBuf,
    project: PathBuf,
    report: PathBuf,
}

#[cfg(test)]
fn new_run_paths(root: &Path) -> anyhow::Result<GoldenRunPaths> {
    let directory = new_run_directory(root, RUN_ROOT_ENV, "golden-proxy-relink")?;
    let report = rooted_env_path(root, OUTPUT_ENV, || {
        directory.join("golden-proxy-relink-report.json")
    });
    Ok(GoldenRunPaths {
        project: directory.join("windows-alpha-golden-proxy-relink.mdp"),
        directory,
        report,
    })
}

pub(super) fn path_resolution_label(resolution: PreviewMediaDecodePathResolution) -> &'static str {
    match resolution {
        PreviewMediaDecodePathResolution::Source => "source",
        PreviewMediaDecodePathResolution::Proxy => "proxy",
        PreviewMediaDecodePathResolution::ProxyColorIncompatible => "proxy_color_incompatible",
        PreviewMediaDecodePathResolution::ProxyMissing => "proxy_missing",
        PreviewMediaDecodePathResolution::ProxyStale => "proxy_stale",
    }
}

fn resolve_media_path(
    state: &AppState,
    asset: &AssetRecord,
) -> anyhow::Result<ResolvedPathEvidence> {
    let prefer_proxy =
        state.project_settings().proxy_enabled && state.is_asset_proxy_mode(asset.id);
    resolve_media_path_for_preference(state, asset, prefer_proxy)
}

pub(super) fn resolve_media_path_for_preference(
    state: &AppState,
    asset: &AssetRecord,
    prefer_proxy: bool,
) -> anyhow::Result<ResolvedPathEvidence> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .media_input(sequence.settings.color.input.auto_tone_map_media);
    let proxy_config = state.proxy_config();
    let proxy_color = resolve_app_state_proxy_color_contract(state, asset).ok();
    let outcome = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: sequence.settings.resolution,
        input_color: &input_color,
        prefer_proxy,
        request_missing_proxy_generation: false,
        proxy_config: &proxy_config,
        proxy_color,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
        cpu_working_required: false,
    });
    let PreviewMediaSourceOutcome::Ready(resolved) = outcome else {
        anyhow::bail!("product Preview media resolution did not produce a decode path");
    };
    let decode_source = resolved.key.decode.source();
    Ok(ResolvedPathEvidence {
        path_sha256: sha256_file(decode_source.path())?,
        path: decode_source.path().to_path_buf(),
        resolution: path_resolution_label(resolved.path_resolution),
        fingerprint: decode_source.fingerprint(),
    })
}

fn resolve_unavailable_reason(state: &AppState, asset: &AssetRecord) -> anyhow::Result<String> {
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .media_input(sequence.settings.color.input.auto_tone_map_media);
    let proxy_config = state.proxy_config();
    let outcome = resolve_preview_media_source(PreviewMediaSourceRequest {
        asset,
        color_space_override: None,
        alpha_interpretation: AlphaInterpretation::Straight,
        source_sample: mondrian_core::SourceSampleTarget::covering(TimelineTime::ZERO),
        target_resolution: sequence.settings.resolution,
        input_color: &input_color,
        prefer_proxy: false,
        request_missing_proxy_generation: false,
        proxy_config: &proxy_config,
        proxy_color: None,
        hardware_admission: PreviewHardwareDecodeAdmissionState::default(),
        cpu_working_required: false,
    });
    let PreviewMediaSourceOutcome::Unavailable(unavailable) = outcome else {
        anyhow::bail!("offline original did not become explicitly unavailable");
    };
    Ok(unavailable.reason.to_string())
}

fn latest_proxy_attempt_id(state: &AppState) -> u64 {
    state
        .proxy_generation_diagnostics()
        .terminal_records
        .iter()
        .map(|record| record.attempt_id)
        .max()
        .unwrap_or(0)
}

fn latest_asset_mutation_operation_id(state: &AppState) -> u64 {
    state
        .media_asset_mutation_diagnostics()
        .terminals
        .iter()
        .map(|record| record.operation_id)
        .max()
        .unwrap_or(0)
}

fn wait_for_relink_completion(
    state: &mut AppState,
    asset_id: AssetId,
    after_operation_id: u64,
) -> anyhow::Result<crate::app::media_asset_mutation::MediaAssetMutationTerminalRecord> {
    let deadline = Instant::now() + PROXY_TIMEOUT;
    loop {
        state.poll_media_asset_mutations();
        if let Some(record) = state
            .media_asset_mutation_diagnostics()
            .terminals
            .into_iter()
            .rev()
            .find(|record| {
                record.asset_id == asset_id
                    && record.operation_id > after_operation_id
                    && record.kind
                        == crate::app::media_asset_mutation::MediaAssetMutationKind::Relink
            })
        {
            ensure!(
                record.evidence.disposition == ExecutionTerminalDisposition::Completed
                    && record.detail.is_none(),
                "relink ended as {:?}: {:?}",
                record.evidence.disposition,
                record.detail
            );
            return Ok(record);
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for Asset relink completion"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_proxy_completion(
    state: &mut AppState,
    asset_id: AssetId,
    after_attempt_id: u64,
) -> anyhow::Result<ProxyExecutionEvidence> {
    let deadline = Instant::now() + PROXY_TIMEOUT;
    loop {
        state.poll_proxy_generation();
        if let Some(record) = state
            .proxy_generation_diagnostics()
            .terminal_records
            .into_iter()
            .rev()
            .find(|record| record.asset_id == asset_id && record.attempt_id > after_attempt_id)
        {
            ensure!(
                record.evidence.disposition == ExecutionTerminalDisposition::Completed
                    && record.executed
                    && record.failure.is_none(),
                "proxy generation ended as {:?}: {:?}",
                record.evidence.disposition,
                record.failure_detail
            );
            return Ok(ProxyExecutionEvidence {
                attempt_id: record.attempt_id,
                generation: record.evidence.generation,
                disposition: record.evidence.disposition,
                executed: record.executed,
                source_fingerprint: record.source_fingerprint,
                elapsed_ms: record.elapsed.as_millis(),
            });
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for proxy generation for asset {asset_id}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn copy_fixture_pair(
    fixture: &FixtureEvidence,
    output_directory: &Path,
) -> anyhow::Result<(PathBuf, PathBuf, String)> {
    let file_name = fixture.path.file_name().context("Golden H.264 fixture has no file name")?;
    let original_directory = output_directory.join("media").join("original");
    let replacement_directory = output_directory.join("media").join("replacement");
    std::fs::create_dir_all(&original_directory)?;
    std::fs::create_dir_all(&replacement_directory)?;
    let original = original_directory.join(file_name);
    let replacement = replacement_directory.join(file_name);
    std::fs::copy(&fixture.path, &original)?;
    std::fs::copy(&fixture.path, &replacement)?;
    let fixture_hash = sha256_file(&fixture.path)?;
    ensure!(
        sha256_file(&original)? == fixture_hash && sha256_file(&replacement)? == fixture_hash,
        "run-local relink copies differ from the attested fixture"
    );
    Ok((
        mondrian_assets::canonical_asset_file_path(&original)?,
        mondrian_assets::canonical_asset_file_path(&replacement)?,
        fixture_hash,
    ))
}

fn copy_fixture_once(
    fixture: &FixtureEvidence,
    output_directory: &Path,
    directory_name: &str,
) -> anyhow::Result<(PathBuf, String)> {
    let file_name = fixture.path.file_name().context("Golden fixture has no file name")?;
    let directory = output_directory.join("media").join(directory_name);
    std::fs::create_dir_all(&directory)?;
    let destination = directory.join(file_name);
    std::fs::copy(&fixture.path, &destination)?;
    let fixture_hash = sha256_file(&fixture.path)?;
    ensure!(
        sha256_file(&destination)? == fixture_hash,
        "run-local media copy differs from its attested fixture"
    );
    Ok((
        mondrian_assets::canonical_asset_file_path(&destination)?,
        fixture_hash,
    ))
}

fn observe_imported_video(
    asset: &AssetRecord,
    expected_frame_rate: Rational,
    expected_total_frames: u64,
    label: &str,
) -> anyhow::Result<ImportedVideoObservation> {
    let video = asset
        .media_probe()
        .with_context(|| format!("{label} fixture has no coherent media probe"))?
        .primary_video()
        .with_context(|| format!("{label} fixture has no video stream"))?;
    ensure!(
        asset.kind == AssetKind::Video
            && video.codec == VideoCodec::H264
            && video.codec_profile == VideoCodecProfile::H264High
            && video.pixel_format == PixelFormat::Yuv420p
            && video.bit_depth == 8
            && video.width == 1920
            && video.height == 1080
            && video.frame_rate == expected_frame_rate
            && video.frame_rate_proven
            && video.total_frames == Some(expected_total_frames)
            && video.executable_color_space() == Some(mondrian_core::ColorSpace::Rec709)
            && video.color_range == DecodedVideoRange::Limited,
        "imported {label} media differs from the editorial fixture contract"
    );
    Ok(ImportedVideoObservation {
        codec: video.codec.clone(),
        codec_profile: video.codec_profile,
        pixel_format: video.pixel_format,
        bit_depth: video.bit_depth,
        width: video.width,
        height: video.height,
        frame_rate: video.frame_rate.to_string(),
        total_frames: video.total_frames,
        color_space: "rec709".to_owned(),
        range: video.color_range,
    })
}

fn find_new_video_clip(
    state: &AppState,
    track_id: TrackId,
    before: &BTreeSet<ClipId>,
) -> anyhow::Result<ClipId> {
    let created = state
        .active_sequence()
        .context("active Sequence is absent")?
        .video_tracks
        .iter()
        .find(|track| track.id == track_id)
        .context("Golden video Track is absent")?
        .clips
        .iter()
        .filter(|clip| !before.contains(&clip.id))
        .map(|clip| clip.id)
        .collect::<Vec<_>>();
    ensure!(
        created.len() == 1,
        "video placement created {} new Clips",
        created.len()
    );
    Ok(created[0])
}

fn asset_by_id(state: &AppState, asset_id: AssetId) -> anyhow::Result<AssetRecord> {
    state
        .asset_library()
        .context("Asset Library is absent")?
        .get_asset(asset_id)?
        .with_context(|| format!("Golden asset is absent: {asset_id}"))
}

fn capture_proxy_relink_authoring_anchor(
    state: &AppState,
    sequence_id: SequenceId,
    video_track_id: TrackId,
    expected_clips: &[(&'static str, ClipId, AssetId)],
) -> anyhow::Result<GoldenProxyRelinkAuthoringAnchor> {
    let sequence = state
        .sequence_by_id(sequence_id)
        .context("Proxy/Relink Hero Sequence is absent")?;
    let track = sequence
        .video_tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .context("Proxy/Relink video Track is absent")?;
    ensure!(
        track.clips.len() == expected_clips.len(),
        "Proxy/Relink video Track contains {} Clips instead of its {} owned placements",
        track.clips.len(),
        expected_clips.len()
    );
    let mut clips = Vec::with_capacity(expected_clips.len());
    for &(role, clip_id, asset_id) in expected_clips {
        let clip = track
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .with_context(|| format!("Proxy/Relink {role} Clip is absent"))?;
        ensure!(
            clip.library_asset_id() == Some(asset_id),
            "Proxy/Relink {role} Clip changed its Asset identity"
        );
        let asset = asset_by_id(state, asset_id)?;
        clips.push(ProxyRelinkClipAnchor {
            role,
            clip_id,
            asset_id,
            asset_sha256: sha256_bytes(&serde_json::to_vec(&asset)?),
            position: clip.position,
            duration: clip.duration,
            source_origin: clip.source_origin(),
            source_scale: clip.source_time_scale(),
            source_terminal_boundary: clip.source_terminal_boundary()?,
            asset_proxy_enabled: state.is_asset_proxy_mode(asset_id),
        });
    }
    Ok(GoldenProxyRelinkAuthoringAnchor {
        sequence_id,
        video_track_id,
        track_sha256: sha256_bytes(&serde_json::to_vec(track)?),
        clips,
        project_proxy_enabled: state.project_settings().proxy_enabled,
    })
}

pub(super) fn execute_proxy_relink_stage(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
    output_directory: &Path,
) -> anyhow::Result<GoldenProxyRelinkReport> {
    let slice = contract
        .execution_slices
        .iter()
        .find(|slice| slice.id == PROXY_RELINK_SLICE_ID)
        .context("proxy/relink Golden execution slice is missing")?;
    ensure!(
        slice.required_fixture_roles == ["rec709-h264-picture", "rec709-h264-vfr-picture"]
            && slice.required_operations
                == ["proxy-original-switch", "offline-relink", "constant-retime"]
            && slice.required_content.is_empty()
            && slice.required_exports == ["h264-aac-sdr"],
        "proxy/relink slice contract drifted"
    );
    let window = slice.timeline_window.context("proxy/relink slice has no timeline window")?;
    ensure!(
        window.start_frame == CFR_WINDOW_START_FRAME
            && window.end_frame_exclusive == VFR_WINDOW_END_FRAME_EXCLUSIVE,
        "proxy/relink slice timeline window drifted"
    );
    let cfr_window = super::GoldenTimelineWindow {
        start_frame: CFR_WINDOW_START_FRAME,
        end_frame_exclusive: CFR_WINDOW_END_FRAME_EXCLUSIVE,
    };
    let vfr_window = super::GoldenTimelineWindow {
        start_frame: VFR_WINDOW_START_FRAME,
        end_frame_exclusive: VFR_WINDOW_END_FRAME_EXCLUSIVE,
    };

    let manifest: CorpusManifest = load_json(&root.join("tests/validation/corpus-manifest.json"))?;
    let cfr_fixture = resolve_fixture(
        root,
        &fixture_root(root),
        contract,
        &manifest,
        "rec709-h264-picture",
    )?;
    let vfr_fixture = resolve_fixture(
        root,
        &fixture_root(root),
        contract,
        &manifest,
        "rec709-h264-vfr-picture",
    )?;
    let (original_path, replacement_path, cfr_fixture_hash) =
        copy_fixture_pair(&cfr_fixture, output_directory)?;
    let (vfr_path, vfr_fixture_hash) = copy_fixture_once(&vfr_fixture, output_directory, "vfr")?;
    let stage = workflow.bind_slice_primary_sequence(contract, PROXY_RELINK_SLICE_ID)?;
    let sequence_id = stage.sequence_id();
    ensure!(
        sequence_id == workflow.hero_sequence_id(),
        "proxy/relink did not bind the Hero Sequence"
    );
    let state = workflow.app_mut();
    let sequence = state.active_sequence().context("active Sequence is absent")?;
    let cfr_expected_position = TimelineTime::from_frame_position(FramePosition::new(
        cfr_window.start_frame,
        sequence.time_base(),
    ))?;
    let cfr_expected_end = TimelineTime::from_frame_position(FramePosition::new(
        cfr_window.end_frame_exclusive,
        sequence.time_base(),
    ))?;
    let cfr_expected_duration = cfr_expected_end.checked_sub(cfr_expected_position)?;
    let vfr_expected_position = TimelineTime::from_frame_position(FramePosition::new(
        vfr_window.start_frame,
        sequence.time_base(),
    ))?;
    let vfr_expected_end = TimelineTime::from_frame_position(FramePosition::new(
        vfr_window.end_frame_exclusive,
        sequence.time_base(),
    ))?;
    let vfr_expected_duration = vfr_expected_end.checked_sub(vfr_expected_position)?;
    let tracks_before = sequence.video_tracks.iter().map(|track| track.id).collect::<BTreeSet<_>>();
    let (_, add_video_track) = author_transition(state, "add-proxy-relink-track", |state| {
        state.dispatch_action(track_add_action(TrackAddPayload {
            kind: TrackAddKind::Video,
        }))?;
        Ok(())
    })?;
    let created_tracks = state
        .active_sequence()
        .context("Proxy/Relink Hero Sequence is absent")?
        .video_tracks
        .iter()
        .filter(|track| !tracks_before.contains(&track.id))
        .map(|track| track.id)
        .collect::<Vec<_>>();
    ensure!(
        created_tracks.len() == 1,
        "proxy/relink created {} Hero video Tracks instead of one",
        created_tracks.len()
    );
    let video_track_id = created_tracks[0];

    let import_attempt_floor = latest_proxy_attempt_id(state);
    state.dispatch_action(Action::ImportMedia(vec![
        original_path.clone(),
        vfr_path.clone(),
    ]))?;
    wait_for_media_imports(state)?;
    let imported_assets = state
        .asset_library()
        .context("Asset Library is absent after CFR/VFR H.264 import")?
        .list_assets()?;
    let asset = imported_assets
        .iter()
        .find(|asset| asset.file_path() == Some(original_path.as_path()))
        .cloned()
        .context("run-local CFR H.264 import is absent")?;
    let vfr_asset = imported_assets
        .iter()
        .find(|asset| asset.file_path() == Some(vfr_path.as_path()))
        .cloned()
        .context("run-local VFR H.264 import is absent")?;
    ensure!(
        state.is_asset_proxy_mode(asset.id) && state.is_asset_proxy_mode(vfr_asset.id),
        "CFR/VFR H.264 imports did not enter the enabled project proxy workflow"
    );
    let cfr_imported_video = observe_imported_video(&asset, Rational::FPS_25, 200, "CFR H.264")?;
    let vfr_imported_video =
        observe_imported_video(&vfr_asset, Rational::new(5_000, 199), 200, "VFR H.264")?;
    let initial_generation = wait_for_proxy_completion(state, asset.id, import_attempt_floor)?;
    let _vfr_initial_generation =
        wait_for_proxy_completion(state, vfr_asset.id, import_attempt_floor)?;

    let clips_before = BTreeSet::new();
    let (_, place_clip) = author_transition(state, "place-proxy-relink-cfr-video", |state| {
        let time_base = state
            .active_sequence()
            .context("proxy/relink Sequence is absent before placement")?
            .time_base();
        state.dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: asset.id,
            target_track_id: video_track_id,
            position: FramePosition::new(cfr_window.start_frame, time_base),
        }))?;
        Ok(())
    })?;
    let clip_id = find_new_video_clip(state, video_track_id, &clips_before)?;
    let (_, trim_to_window) = author_transition(state, "trim-proxy-relink-cfr-window", |state| {
        let time_base = state
            .active_sequence()
            .context("proxy/relink Sequence is absent before trim")?
            .time_base();
        state.dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![clip_id],
            edge: TimelineTrimPayloadEdge::Out,
            position: FramePosition::new(cfr_window.end_frame_exclusive, time_base),
        }))?;
        Ok(())
    })?;
    let clips_after_cfr = BTreeSet::from([clip_id]);
    let (_, place_vfr_clip) = author_transition(state, "place-proxy-relink-vfr-video", |state| {
        let time_base = state
            .active_sequence()
            .context("proxy/relink Sequence is absent before VFR placement")?
            .time_base();
        state.dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: vfr_asset.id,
            target_track_id: video_track_id,
            position: FramePosition::new(vfr_window.start_frame, time_base),
        }))?;
        Ok(())
    })?;
    let vfr_clip_id = find_new_video_clip(state, video_track_id, &clips_after_cfr)?;
    let (_, trim_vfr_to_window) =
        author_transition(state, "trim-proxy-relink-vfr-window", |state| {
            let time_base = state
                .active_sequence()
                .context("proxy/relink Sequence is absent before VFR trim")?
                .time_base();
            state.dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
                clip_ids: vec![vfr_clip_id],
                edge: TimelineTrimPayloadEdge::Out,
                position: FramePosition::new(vfr_window.end_frame_exclusive, time_base),
            }))?;
            Ok(())
        })?;
    let track = state
        .active_sequence()
        .context("Proxy/Relink Hero Sequence is absent")?
        .video_tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .context("Proxy/Relink Track is absent")?;
    let placed_clip = state
        .active_sequence()
        .context("Proxy/Relink Hero Sequence is absent")?
        .video_tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == clip_id))
        .context("trimmed Proxy/Relink Clip is absent")?;
    let placed_vfr_clip = track
        .clips
        .iter()
        .find(|clip| clip.id == vfr_clip_id)
        .context("trimmed Proxy/Relink VFR Clip is absent")?;
    ensure!(
        placed_clip.position == cfr_expected_position
            && placed_clip.duration == cfr_expected_duration
            && placed_clip.source_origin() == TimelineTime::ZERO
            && placed_clip.source_terminal_boundary()? == cfr_expected_duration
            && placed_clip.end_position()? == cfr_expected_end
            && placed_vfr_clip.position == vfr_expected_position
            && placed_vfr_clip.duration == vfr_expected_duration
            && placed_vfr_clip.source_origin() == TimelineTime::ZERO
            && placed_vfr_clip.source_terminal_boundary()? == vfr_expected_duration
            && placed_vfr_clip.end_position()? == vfr_expected_end,
        "Proxy/Relink CFR/VFR Clips did not occupy their exact Hero windows"
    );

    let proxy_before = resolve_media_path(state, &asset)?;
    ensure!(
        proxy_before.resolution == "proxy" && proxy_before.path != original_path,
        "fresh generated proxy was not selected by the product Preview resolver"
    );
    let proxy_config = state.proxy_config();
    let proxy_color =
        resolve_app_state_proxy_color_contract(state, &asset).map_err(anyhow::Error::msg)?;
    let original_proxy_path =
        ProxyGenerator::new(proxy_config.clone()).proxy_path(&original_path, proxy_color)?;
    ensure!(
        proxy_before.path == original_proxy_path,
        "Preview selected a proxy that differs from the canonical generator identity"
    );

    let (_, disable_proxy) = project_author_transition(state, "disable-original-proxy", |state| {
        state.dispatch_action(assets_set_proxy_mode_action(AssetsSetProxyModePayload {
            asset_id: asset.id,
            enabled: false,
        }))?;
        Ok(())
    })?;
    let original = resolve_media_path(state, &asset)?;
    ensure!(
        original.resolution == "source" && original.path == original_path,
        "disabling proxy mode did not select the original source"
    );

    let (_, enable_proxy) = project_author_transition(state, "enable-original-proxy", |state| {
        state.dispatch_action(assets_set_proxy_mode_action(AssetsSetProxyModePayload {
            asset_id: asset.id,
            enabled: true,
        }))?;
        Ok(())
    })?;
    let proxy_after = resolve_media_path(state, &asset)?;
    ensure!(
        proxy_after.resolution == "proxy"
            && proxy_after.path == proxy_before.path
            && proxy_after.path_sha256 == proxy_before.path_sha256,
        "re-enabling proxy mode did not reuse the exact fresh proxy artifact"
    );

    let (_, return_to_original) =
        project_author_transition(state, "return-to-original-before-offline", |state| {
            state.dispatch_action(assets_set_proxy_mode_action(AssetsSetProxyModePayload {
                asset_id: asset.id,
                enabled: false,
            }))?;
            Ok(())
        })?;
    let proxy_switch =
        OperationEvidence::ProxyOriginalSwitch(Box::new(ProxyOriginalSwitchEvidence {
            initial_generation,
            proxy_before,
            disable_proxy,
            original,
            enable_proxy,
            proxy_after,
            return_to_original,
        }));

    let offline_path = original_path.with_extension("offline.mp4");
    std::fs::rename(&original_path, &offline_path)?;
    ensure!(
        !original_path.exists() && offline_path.exists(),
        "run-local source did not become offline"
    );
    let offline_asset = asset_by_id(state, asset.id)?;
    let unavailable_reason = resolve_unavailable_reason(state, &offline_asset)?;
    let library = state.asset_library_handle().context("Asset Library is absent")?;
    let asset_library_revision_before = library.database_revision()?;
    let before_relink = author_checkpoint(state)?;
    let events = state.event_bus.subscribe();
    let relink_operation_floor = latest_asset_mutation_operation_id(state);
    state.dispatch_action(assets_relink_asset_action(AssetsRelinkAssetPayload {
        asset_id: asset.id,
        path: replacement_path.clone(),
    }))?;
    let relink_terminal = wait_for_relink_completion(state, asset.id, relink_operation_floor)?;
    let after_relink = author_checkpoint(state)?;
    let asset_library_revision_after = library.database_revision()?;
    ensure!(
        before_relink == after_relink,
        "Asset Library relink changed Project or Sequence author state"
    );
    ensure!(
        asset_library_revision_before.checked_add(1) == Some(asset_library_revision_after),
        "relink did not advance the Asset Library revision exactly once"
    );
    let reload_event_published =
        events.try_iter().any(|event| matches!(event, AppEvent::AssetLibraryReloaded));
    ensure!(
        reload_event_published,
        "relink did not publish the Asset Library reload boundary"
    );
    let relinked_asset = asset_by_id(state, asset.id)?;
    ensure!(
        relinked_asset.id == asset.id
            && relinked_asset.file_path() == Some(replacement_path.as_path())
            && relinked_asset.name == asset.name
            && !state.is_asset_proxy_mode(asset.id),
        "offline relink changed asset identity/name or proxy author intent"
    );
    let retained_clip = state
        .active_sequence()
        .context("proxy/relink Sequence is absent")?
        .video_tracks
        .iter()
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == clip_id)
        .context("offline relink removed the timeline Clip")?;
    ensure!(
        retained_clip.library_asset_id() == Some(asset.id),
        "offline relink changed the Clip-to-Asset identity"
    );
    let replacement_source = resolve_media_path(state, &relinked_asset)?;
    ensure!(
        replacement_source.resolution == "source"
            && replacement_source.path == replacement_path
            && replacement_source.path_sha256 == cfr_fixture_hash,
        "relinked original did not resolve through the replacement path"
    );

    let replacement_proxy_path =
        ProxyGenerator::new(proxy_config.clone()).proxy_path(&replacement_path, proxy_color)?;
    ensure!(
        replacement_proxy_path != original_proxy_path,
        "source-path change did not produce a distinct proxy artifact identity"
    );
    let replacement_proxy_was_missing = ProxyGenerator::new(proxy_config.clone())
        .proxy_status(&replacement_path, proxy_color)
        == ProxyStatus::Missing;
    ensure!(
        replacement_proxy_was_missing,
        "relink incorrectly treated the old source proxy as fresh for the replacement path"
    );
    let replacement_attempt_floor = latest_proxy_attempt_id(state);
    let (_, enable_relinked_proxy) =
        project_author_transition(state, "enable-relinked-proxy", |state| {
            state.dispatch_action(assets_set_proxy_mode_action(AssetsSetProxyModePayload {
                asset_id: asset.id,
                enabled: true,
            }))?;
            Ok(())
        })?;
    let replacement_generation =
        wait_for_proxy_completion(state, asset.id, replacement_attempt_floor)?;
    let replacement_proxy = resolve_media_path(state, &relinked_asset)?;
    ensure!(
        replacement_proxy.resolution == "proxy"
            && replacement_proxy.path == replacement_proxy_path
            && replacement_proxy.path != original_proxy_path,
        "relinked proxy generation did not publish the replacement source artifact"
    );

    let offline_relink = OperationEvidence::OfflineRelink(Box::new(OfflineRelinkEvidence {
        unavailable_source_path: original_path,
        unavailable_reason,
        relink: LibraryRelinkTransitionEvidence {
            before: before_relink,
            after: after_relink,
            operation_id: relink_terminal.operation_id,
            generation: relink_terminal.evidence.generation,
            disposition: relink_terminal.evidence.disposition,
            elapsed_ms: relink_terminal.elapsed.as_millis(),
            asset_library_revision_before,
            asset_library_revision_after,
            reload_event_published,
        },
        retained_asset_id: asset.id,
        retained_clip_id: clip_id,
        replacement_source,
        prior_source_proxy_path: original_proxy_path,
        replacement_proxy_path,
        replacement_proxy_was_missing,
        enable_relinked_proxy,
        replacement_generation,
        replacement_proxy,
    }));
    let retime = execute_retime_media_evidence(
        state,
        contract,
        output_directory,
        clip_id,
        asset.id,
        cfr_window,
        vfr_clip_id,
        vfr_asset.id,
        vfr_window,
    )?;
    ensure_exact_requirement_evidence(
        &slice.required_exports,
        std::iter::once(retime.export_id()),
        "export",
    )?;
    let operations = vec![
        proxy_switch,
        offline_relink,
        OperationEvidence::ConstantRetime { evidence: Box::new(retime) },
    ];
    ensure_exact_requirement_evidence(
        &slice.required_operations,
        operations.iter().map(OperationEvidence::id),
        "operation",
    )?;
    let authoring = capture_proxy_relink_authoring_anchor(
        state,
        sequence_id,
        video_track_id,
        &[
            ("cfr", clip_id, asset.id),
            ("vfr", vfr_clip_id, vfr_asset.id),
        ],
    )?;
    let cfr_authoring = authoring
        .clips
        .iter()
        .find(|clip| clip.role == "cfr")
        .context("Proxy/Relink CFR authoring anchor is absent")?;
    let vfr_authoring = authoring
        .clips
        .iter()
        .find(|clip| clip.role == "vfr")
        .context("Proxy/Relink VFR authoring anchor is absent")?;
    ensure!(
        cfr_authoring.position == cfr_expected_position
            && cfr_authoring.duration == cfr_expected_duration
            && cfr_authoring.source_origin == TimelineTime::ZERO
            && cfr_authoring.source_scale == TimeScale::new(1, 2)?
            && cfr_authoring.source_terminal_boundary
                == cfr_expected_duration.checked_scale(TimeScale::new(1, 2)?)?
            && cfr_authoring.asset_proxy_enabled
            && vfr_authoring.position == vfr_expected_position
            && vfr_authoring.duration == vfr_expected_duration
            && vfr_authoring.source_origin == TimelineTime::ZERO
            && vfr_authoring.source_scale == TimeScale::new(1, 2)?
            && vfr_authoring.source_terminal_boundary
                == vfr_expected_duration.checked_scale(TimeScale::new(1, 2)?)?
            && vfr_authoring.asset_proxy_enabled
            && authoring.project_proxy_enabled,
        "Proxy/Relink retained CFR/VFR authoring differs from exact Hero windows or proxy intent"
    );
    workflow.verify_binding()?;

    Ok(GoldenProxyRelinkReport {
        schema_version: 5,
        profile: PROXY_RELINK_SLICE_ID,
        contract_id: contract.id.clone(),
        corpus_revision: manifest.corpus_revision,
        status: "passed",
        complete_golden_project: false,
        fixtures: vec![cfr_fixture, vfr_fixture],
        setup: ProxyRelinkSetupEvidence {
            stage,
            video_track_id,
            add_video_track,
            placements: vec![
                ProxyRelinkPlacementEvidence {
                    role: "cfr",
                    asset_id: asset.id,
                    clip_id,
                    window: cfr_window,
                    imported_video: cfr_imported_video,
                    place_clip,
                    trim_to_window,
                    source_copy_sha256: cfr_fixture_hash.clone(),
                },
                ProxyRelinkPlacementEvidence {
                    role: "vfr",
                    asset_id: vfr_asset.id,
                    clip_id: vfr_clip_id,
                    window: vfr_window,
                    imported_video: vfr_imported_video,
                    place_clip: place_vfr_clip,
                    trim_to_window: trim_vfr_to_window,
                    source_copy_sha256: vfr_fixture_hash,
                },
            ],
            replacement_copy_sha256: cfr_fixture_hash,
        },
        operations,
        authoring,
    })
}

#[cfg(test)]
fn execute_proxy_relink_slice(
    root: &Path,
    paths: &GoldenRunPaths,
) -> anyhow::Result<GoldenProxyRelinkReport> {
    let contract = load_golden_contract(root)?;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let project_settings = mondrian_core::ProjectSettings {
        cache_dir: Some(paths.directory.join("cache")),
        ..mondrian_core::ProjectSettings::default()
    };
    let mut workflow = GoldenProductWorkflowDriver::create(
        paths.project.clone(),
        "Windows Alpha Golden Proxy + Relink",
        settings,
        mondrian_core::ProjectColorEnvironment::default(),
        project_settings,
    )?;
    super::foundation_audio::execute_foundation_stage(root, &contract, &mut workflow)?;
    execute_proxy_relink_stage(root, &contract, &mut workflow, &paths.directory)
}

#[test]
#[ignore = "Golden proxy/relink gate requires the generated canonical H.264 fixture and FFmpeg proxy encoder"]
fn golden_project_proxy_original_offline_relink_gate() -> anyhow::Result<()> {
    let root = repository_root();
    let paths = new_run_paths(&root)?;
    match execute_proxy_relink_slice(&root, &paths) {
        Ok(report) => {
            write_report(&paths.report, &report)?;
            eprintln!(
                "MONDRIAN_GOLDEN_PROXY_RELINK_REPORT_JSON={}",
                serde_json::to_string(&report)?
            );
            eprintln!(
                "MONDRIAN_GOLDEN_PROXY_RELINK_REPORT_PATH={}",
                paths.report.display()
            );
            eprintln!(
                "MONDRIAN_GOLDEN_PROXY_RELINK_RUN_DIRECTORY={}",
                paths.directory.display()
            );
            Ok(())
        }
        Err(error) => {
            let failure = serde_json::json!({
                "schema_version": 5,
                "profile": PROXY_RELINK_SLICE_ID,
                "status": "failed",
                "complete_golden_project": false,
                "error": format!("{error:#}")
            });
            write_report(&paths.report, &failure)?;
            Err(error)
        }
    }
}
