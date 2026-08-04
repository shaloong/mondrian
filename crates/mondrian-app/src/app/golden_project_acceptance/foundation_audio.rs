//! First executable Golden slice: PCM Clip authoring and durable reopen.

use super::audio_authoring_evidence::{
    capture_audio_track_authoring, GoldenAudioTrackAuthoringAnchor,
};
use super::fixture::{resolve_fixture, CorpusManifest, FixtureEvidence};
use super::harness::{
    dispatch_author_transition, ensure_exact_requirement_evidence, fixture_root,
    wait_for_media_imports, AuthorCheckpoint, AuthorTransitionEvidence,
};
#[cfg(test)]
use super::harness::{new_run_directory, rooted_env_path, write_report};
use super::workflow::{
    GoldenProductWorkflowDriver, GoldenProjectOpenEvidence, GoldenSequenceStageEvidence,
};
#[cfg(test)]
use super::{load_golden_contract, repository_root};
use super::{load_json, parse_rational, sequence_settings_from_contract, GoldenProjectContract};
use crate::app::ui_actions::{
    audio_component_edit_action, timeline_drop_asset_action, TimelineDropAssetPayload,
};
use crate::app::AppState;
use anyhow::{bail, ensure, Context};
use mondrian_assets::AssetKind;
use mondrian_core::{
    AssetId, AudioComponentEditId, ClipId, FramePosition, SequenceId, TimelineTime, TrackId,
};
use mondrian_editor_state::Action;
use mondrian_media::info::ChannelLayout;
use mondrian_timeline::audio::{AudioComponentEdit, AudioFade, AudioFadeCurve};
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::SequenceSettings;
use mondrian_timeline::{AudioComponentAddress, AudioComponentEditRequest, AudioComponentMutation};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(super) const FOUNDATION_SLICE_ID: &str = "foundation-audio-authoring-v1";
#[cfg(test)]
const RUN_ROOT_ENV: &str = "MONDRIAN_GOLDEN_FOUNDATION_RUN_ROOT";
#[cfg(test)]
const OUTPUT_ENV: &str = "MONDRIAN_GOLDEN_FOUNDATION_OUTPUT";

#[derive(Debug, Clone, PartialEq, Serialize)]
struct AudioEditObservation {
    edit_id: AudioComponentEditId,
    enabled: bool,
    volume_db: f64,
    pan: f64,
    fade_in: Option<AudioFade>,
    fade_out: Option<AudioFade>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "id")]
enum OperationEvidence {
    #[serde(rename = "open")]
    Open {
        lifecycle: GoldenProjectOpenEvidence,
        duration_frames: i64,
        settings: SequenceSettings,
    },
    #[serde(rename = "undo-redo")]
    UndoRedo {
        undo_steps: Vec<AuthorTransitionEvidence>,
        after_undo: AudioEditObservation,
        redo_steps: Vec<AuthorTransitionEvidence>,
        after_redo: AudioEditObservation,
    },
    #[serde(rename = "save-reopen")]
    SaveReopen {
        persistence_request_id: u64,
        saved_session: AuthorCheckpoint,
        reopened_session: AuthorCheckpoint,
        session_identity_changed: bool,
        project_identity_preserved: bool,
        reopened_edit: AudioEditObservation,
        project_archive_sha256: String,
    },
}

impl OperationEvidence {
    const fn id(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::UndoRedo { .. } => "undo-redo",
            Self::SaveReopen { .. } => "save-reopen",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ContentEvidence {
    id: &'static str,
    author_steps: Vec<AuthorTransitionEvidence>,
    observed_edit: AudioEditObservation,
}

#[derive(Debug, Clone, Serialize)]
struct GoldenSetupEvidence {
    project_path: PathBuf,
    stage: GoldenSequenceStageEvidence,
    asset_id: AssetId,
    clip_id: ClipId,
    audio_track_id: TrackId,
    edit_id: AudioComponentEditId,
    duration_frames: i64,
    imported_audio: ImportedAudioObservation,
}

#[derive(Debug, Clone, Serialize)]
struct ImportedAudioObservation {
    stream_index: u32,
    sample_rate: u32,
    channels: u8,
    channel_layout: ChannelLayout,
    bit_depth: u16,
}

#[derive(Debug, Serialize)]
pub(super) struct GoldenFoundationReport {
    schema_version: u32,
    profile: &'static str,
    contract_id: String,
    corpus_revision: String,
    status: GoldenRunStatus,
    complete_golden_project: bool,
    fixture: FixtureEvidence,
    setup: GoldenSetupEvidence,
    operations: Vec<OperationEvidence>,
    content: Vec<ContentEvidence>,
}

impl GoldenFoundationReport {
    pub(super) fn primary_sequence_id(&self) -> SequenceId {
        self.setup.stage.sequence_id()
    }

    pub(super) fn capture_audio_anchor(
        &self,
        state: &AppState,
    ) -> anyhow::Result<GoldenAudioTrackAuthoringAnchor> {
        let sequence = state
            .sequence_by_id(self.primary_sequence_id())
            .context("Foundation Hero Sequence is absent")?;
        let track = sequence
            .audio_tracks
            .iter()
            .find(|track| track.id == self.setup.audio_track_id)
            .context("Foundation audio Track is absent")?;
        let clip = track
            .clips
            .iter()
            .find(|clip| clip.id == self.setup.clip_id)
            .context("Foundation audio Clip is absent")?;
        ensure!(
            clip.media_asset_id() == Some(self.setup.asset_id),
            "Foundation audio Clip changed Asset identity"
        );
        ensure!(
            clip.audio_components.iter().any(|edit| edit.id == self.setup.edit_id),
            "Foundation audio Component Edit is absent"
        );
        ensure!(
            state
                .asset_library()
                .context("Foundation Asset Library is absent")?
                .get_asset(self.setup.asset_id)?
                .is_some(),
            "Foundation PCM Asset is absent"
        );
        capture_audio_track_authoring(state, sequence.id, &[track.id])
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum GoldenRunStatus {
    Passed,
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
    let directory = new_run_directory(root, RUN_ROOT_ENV, "golden-foundation")?;
    let report = rooted_env_path(root, OUTPUT_ENV, || {
        directory.join("golden-foundation-report.json")
    });
    Ok(GoldenRunPaths {
        project: directory.join("windows-alpha-golden-foundation.mdp"),
        directory,
        report,
    })
}

fn observe_audio_edit(edit: &AudioComponentEdit) -> AudioEditObservation {
    AudioEditObservation {
        edit_id: edit.id,
        enabled: edit.enabled,
        volume_db: edit.volume_db,
        pan: edit.pan,
        fade_in: edit.fades.fade_in,
        fade_out: edit.fades.fade_out,
    }
}

fn find_audio_clip(
    state: &AppState,
    clip_id: mondrian_core::ClipId,
) -> anyhow::Result<(&Clip, mondrian_core::TrackId)> {
    let sequence = state.active_sequence().context("active sequence missing")?;
    for track in &sequence.audio_tracks {
        if let Some(clip) = track.clips.iter().find(|clip| clip.id == clip_id) {
            return Ok((clip, track.id));
        }
    }
    bail!("audio Clip is absent: {clip_id}")
}

fn find_audio_edit(
    state: &AppState,
    clip_id: mondrian_core::ClipId,
    edit_id: mondrian_core::AudioComponentEditId,
) -> anyhow::Result<&AudioComponentEdit> {
    find_audio_clip(state, clip_id)?
        .0
        .audio_components
        .iter()
        .find(|edit| edit.id == edit_id)
        .context("audio Component Edit disappeared")
}

fn assert_edit_defaults(edit: &AudioComponentEdit) -> anyhow::Result<()> {
    ensure!(edit.enabled, "audio Component Edit unexpectedly disabled");
    ensure!(edit.volume_db == 0.0, "audio volume did not Undo to 0 dB");
    ensure!(edit.pan == 0.0, "audio pan did not Undo to center");
    ensure!(edit.fades.fade_in.is_none(), "fade-in did not Undo to None");
    ensure!(
        edit.fades.fade_out.is_none(),
        "fade-out did not Undo to None"
    );
    Ok(())
}

fn assert_edit_authored(edit: &AudioComponentEdit, fade: AudioFade) -> anyhow::Result<()> {
    ensure!(
        edit.volume_db == -6.0,
        "authored -6 dB volume was not retained"
    );
    ensure!(edit.pan == 0.25, "authored pan was not retained");
    ensure!(
        edit.fades.fade_in == Some(fade),
        "authored fade-in was not retained"
    );
    ensure!(
        edit.fades.fade_out == Some(fade),
        "authored fade-out was not retained"
    );
    Ok(())
}

pub(super) fn execute_foundation_stage(
    root: &Path,
    contract: &GoldenProjectContract,
    workflow: &mut GoldenProductWorkflowDriver,
) -> anyhow::Result<GoldenFoundationReport> {
    let slice = contract
        .execution_slices
        .iter()
        .find(|slice| slice.id == FOUNDATION_SLICE_ID)
        .context("foundation Golden execution slice is missing")?;
    ensure!(
        slice.required_fixture_roles == ["pcm-audio"],
        "foundation slice fixture contract drifted"
    );
    ensure!(
        slice.required_operations == ["open", "undo-redo", "save-reopen"],
        "foundation slice operation contract drifted"
    );
    ensure!(
        slice.required_content == ["clip-audio-gain-pan-fades"],
        "foundation slice content contract drifted"
    );
    ensure!(
        slice.required_exports.is_empty() && slice.timeline_window.is_none(),
        "foundation slice must not claim export coverage"
    );

    let manifest: CorpusManifest = load_json(&root.join("tests/validation/corpus-manifest.json"))?;
    let fixture_root = fixture_root(root);
    let fixture = resolve_fixture(root, &fixture_root, contract, &manifest, "pcm-audio")?;
    let settings = sequence_settings_from_contract(&contract.timeline)?;

    let open_lifecycle = workflow.reopen_created_project()?;
    let opened = workflow.app().active_sequence().context("open produced no active sequence")?;
    ensure!(
        opened.settings == settings,
        "opened Sequence settings differ from the complete Golden timeline contract"
    );
    ensure!(
        opened.settings.frame_rate == parse_rational(&contract.timeline.frame_rate)?,
        "opened frame rate differs from the textual Golden contract"
    );
    let open_evidence = OperationEvidence::Open {
        lifecycle: open_lifecycle,
        duration_frames: contract.timeline.duration_frames,
        settings: opened.settings.clone(),
    };
    let stage = workflow.bind_slice_primary_sequence(contract, FOUNDATION_SLICE_ID)?;

    let state = workflow.app_mut();
    state.dispatch_action(Action::ImportMedia(vec![fixture.path.clone()]))?;
    wait_for_media_imports(state)?;
    let library = state.asset_library().context("asset library missing after import")?;
    let assets = library.list_assets()?;
    let asset = assets
        .into_iter()
        .find(|asset| asset.file_path() == Some(fixture.path.as_path()))
        .context("imported PCM fixture is absent from the Asset Library")?;
    ensure!(
        asset.kind == AssetKind::Audio,
        "PCM fixture imported as a non-audio asset"
    );
    let audio = asset
        .media_probe()
        .context("imported PCM has no coherent media probe")?
        .primary_audio()
        .context("imported PCM has no audio stream")?;
    ensure!(
        audio.sample_rate == contract.timeline.audio_sample_rate
            && audio.channels == 2
            && audio.channel_layout == ChannelLayout::Stereo
            && audio.bit_depth == 16,
        "imported PCM stream contract differs from the Golden timeline"
    );
    let imported_audio = ImportedAudioObservation {
        stream_index: audio.index,
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        channel_layout: audio.channel_layout.clone(),
        bit_depth: audio.bit_depth,
    };

    let audio_track_id = state.active_sequence().context("sequence missing")?.audio_tracks[0].id;
    let clips_before = state.active_sequence().context("sequence missing")?.audio_tracks[0]
        .clips
        .iter()
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    state.dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
        asset_id: asset.id,
        target_track_id: audio_track_id,
        is_video_track: false,
        frame: 0,
    }))?;
    let clip_id = state.active_sequence().context("sequence missing")?.audio_tracks[0]
        .clips
        .iter()
        .find(|clip| !clips_before.contains(&clip.id))
        .map(|clip| clip.id)
        .context("timeline drop did not create an audio Clip")?;

    let time_base = state.active_sequence().context("sequence missing")?.time_base();
    state.dispatch_action(Action::TrimClipEnd {
        clip_id,
        new_source_out: FramePosition::new(contract.timeline.duration_frames, time_base),
    })?;
    let (clip, actual_audio_track_id) = find_audio_clip(state, clip_id)?;
    ensure!(
        clip.duration
            == TimelineTime::from_frame_position(FramePosition::new(
                contract.timeline.duration_frames,
                time_base,
            ))?,
        "foundation Clip does not exactly cover the Golden timeline"
    );
    let edit_id = clip.audio_components.first().context("audio Clip has no Component Edit")?.id;
    let fade = AudioFade {
        duration: TimelineTime::new(1, 1)?,
        curve: AudioFadeCurve::EqualPower,
    };

    let mut content_steps = Vec::new();
    for (intent, mutation) in [
        (
            "set-clip-audio-volume",
            AudioComponentMutation::SetVolumeDb { value: -6.0 },
        ),
        (
            "set-clip-audio-pan",
            AudioComponentMutation::SetPan { value: 0.25 },
        ),
        (
            "set-clip-audio-fade-in",
            AudioComponentMutation::SetFadeIn { value: Some(fade) },
        ),
        (
            "set-clip-audio-fade-out",
            AudioComponentMutation::SetFadeOut { value: Some(fade) },
        ),
    ] {
        content_steps.push(dispatch_author_transition(
            state,
            intent,
            audio_component_edit_action(AudioComponentEditRequest {
                address: AudioComponentAddress {
                    track_id: actual_audio_track_id,
                    clip_id,
                    edit_id,
                },
                mutation,
            }),
        )?);
    }
    let authored_edit = find_audio_edit(state, clip_id, edit_id)?;
    assert_edit_authored(authored_edit, fade)?;
    let content_evidence = ContentEvidence {
        id: "clip-audio-gain-pan-fades",
        author_steps: content_steps,
        observed_edit: observe_audio_edit(authored_edit),
    };

    let mut undo_steps = Vec::new();
    for intent in ["undo-fade-out", "undo-fade-in", "undo-pan", "undo-volume"] {
        undo_steps.push(dispatch_author_transition(state, intent, Action::Undo)?);
    }
    let undone_edit = find_audio_edit(state, clip_id, edit_id)?;
    assert_edit_defaults(undone_edit)?;
    let after_undo = observe_audio_edit(undone_edit);
    let mut redo_steps = Vec::new();
    for intent in ["redo-volume", "redo-pan", "redo-fade-in", "redo-fade-out"] {
        redo_steps.push(dispatch_author_transition(state, intent, Action::Redo)?);
    }
    let redone_edit = find_audio_edit(state, clip_id, edit_id)?;
    assert_edit_authored(redone_edit, fade)?;
    let after_redo = observe_audio_edit(redone_edit);
    let undo_evidence =
        OperationEvidence::UndoRedo { undo_steps, after_undo, redo_steps, after_redo };

    let persistence = workflow.durable_save_reopen_for(&stage)?;
    let state = workflow.app();
    let reopened_clip = find_audio_clip(state, clip_id)?.0;
    ensure!(
        reopened_clip.media_asset_id() == Some(asset.id),
        "save/reopen changed Clip asset identity"
    );
    let reopened_edit = reopened_clip
        .audio_components
        .iter()
        .find(|edit| edit.id == edit_id)
        .context("save/reopen changed audio Component Edit identity")?;
    assert_edit_authored(reopened_edit, fade)?;
    ensure!(
        state
            .asset_library()
            .context("reopened Asset Library missing")?
            .get_asset(asset.id)?
            .is_some(),
        "save/reopen lost the imported asset"
    );
    let save_evidence = OperationEvidence::SaveReopen {
        persistence_request_id: persistence.persistence_request_id,
        saved_session: persistence.saved_session,
        reopened_session: persistence.reopened_session,
        session_identity_changed: persistence.session_identity_changed,
        project_identity_preserved: persistence.project_identity_preserved,
        reopened_edit: observe_audio_edit(reopened_edit),
        project_archive_sha256: persistence.project_archive_sha256,
    };

    let operations = vec![open_evidence, undo_evidence, save_evidence];
    let content = vec![content_evidence];
    ensure_exact_requirement_evidence(
        &slice.required_operations,
        operations.iter().map(OperationEvidence::id),
        "operation",
    )?;
    ensure_exact_requirement_evidence(
        &slice.required_content,
        content.iter().map(|evidence| evidence.id),
        "content",
    )?;
    workflow.verify_binding()?;

    Ok(GoldenFoundationReport {
        schema_version: 5,
        profile: FOUNDATION_SLICE_ID,
        contract_id: contract.id.clone(),
        corpus_revision: manifest.corpus_revision,
        status: GoldenRunStatus::Passed,
        complete_golden_project: false,
        fixture,
        setup: GoldenSetupEvidence {
            project_path: workflow.project_path().to_path_buf(),
            stage,
            asset_id: asset.id,
            clip_id,
            audio_track_id: actual_audio_track_id,
            edit_id,
            duration_frames: contract.timeline.duration_frames,
            imported_audio,
        },
        operations,
        content,
    })
}

#[cfg(test)]
fn execute_foundation_slice(
    root: &Path,
    paths: &GoldenRunPaths,
) -> anyhow::Result<GoldenFoundationReport> {
    let contract = load_golden_contract(root)?;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    let mut workflow = GoldenProductWorkflowDriver::create(
        paths.project.clone(),
        "Windows Alpha Golden Foundation",
        settings,
        mondrian_core::ProjectColorEnvironment::default(),
        mondrian_core::ProjectSettings::default(),
    )?;
    execute_foundation_stage(root, &contract, &mut workflow)
}

#[test]
#[ignore = "Golden foundation gate requires the generated canonical PCM fixture"]
fn golden_project_foundation_audio_authoring_gate() -> anyhow::Result<()> {
    let root = repository_root();
    let paths = new_run_paths(&root)?;
    match execute_foundation_slice(&root, &paths) {
        Ok(report) => {
            write_report(&paths.report, &report)?;
            eprintln!(
                "MONDRIAN_GOLDEN_FOUNDATION_REPORT_JSON={}",
                serde_json::to_string(&report)?
            );
            eprintln!(
                "MONDRIAN_GOLDEN_FOUNDATION_REPORT_PATH={}",
                paths.report.display()
            );
            eprintln!(
                "MONDRIAN_GOLDEN_FOUNDATION_RUN_DIRECTORY={}",
                paths.directory.display()
            );
            Ok(())
        }
        Err(error) => {
            let failure = serde_json::json!({
                "schema_version": 5,
                "profile": FOUNDATION_SLICE_ID,
                "status": "failed",
                "complete_golden_project": false,
                "error": format!("{error:#}")
            });
            write_report(&paths.report, &failure)?;
            Err(error)
        }
    }
}
