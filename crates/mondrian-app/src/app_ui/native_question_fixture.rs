//! Canonical saved-project creator for the actual native Question Window gate.
//!
//! No native render, GUI capability, project JSON, archive writer, or renderer is
//! created here. Inspection uses the product's supervised native registration;
//! author edits and durable save/reopen use the ordinary App/Session boundaries.

use super::native_question_validation::{
    NativeQuestionWindowInteraction, NativeQuestionWindowRequest,
};
use crate::app::product_action::{
    ProductAction, VisualEffectAddToClipPayload, VisualEffectProductAction,
};
use crate::app::AppState;
use mondrian_core::{AssetId, ClipId, Color, EffectId, Rational, SequenceId, TimelineTime};
use mondrian_timeline::clip::Clip;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Explicit inputs for one newly saved canonical project and its driver requests.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeQuestionFixtureRequest {
    /// New absolute `.mdp` path; an existing project is never overwritten.
    pub project: PathBuf,
    /// Real owned SessionProbe binary or bundle inspected by the current product.
    pub bundle: PathBuf,
    /// Exact identifier exported by that binary, not an invented definition key.
    pub plugin_identifier: String,
    /// Absolute directory receiving three new strict typed Window request files.
    pub requests_directory: PathBuf,
    /// Each later visible Window run's nonzero timeout, in milliseconds.
    pub timeout_ms: u64,
}

/// Actual saved/reopened author data and request paths, with consuming cleanup.
#[derive(Debug, Default, serde::Serialize)]
pub struct NativeQuestionFixtureReport {
    /// Save, reopen, exact author-data comparison and consuming App cleanup passed.
    pub passed: bool,
    /// Real canonical archive produced by App persistence.
    pub project: Option<PathBuf>,
    /// Resolved supervised native binary and its validated byte identity.
    pub native_binary: Option<PathBuf>,
    /// Complete native binary SHA-256 observed by the normal inspection boundary.
    pub native_binary_sha256: Option<String>,
    /// Actual definition's native identifier.
    pub plugin_identifier: Option<String>,
    /// Strong identities read back from the genuinely reopened author project.
    pub sequence: Option<SequenceId>,
    /// Strong Clip identity of the visible five-second generated color source.
    pub clip: Option<ClipId>,
    /// Strong authored native effect occurrence retained through save/reopen.
    pub effect: Option<EffectId>,
    /// Separate process requests in keyboard Yes, pointer No, Escape order.
    pub requests: Vec<PathBuf>,
    /// Full Clip/effect/schema bytes at the author level survived canonical reopen.
    pub saved_and_reopened: bool,
    /// The reopened project is paused, clean and at the source's exact start.
    pub paused_clean_at_zero: bool,
    /// Every actually constructed App execution owner was consumed successfully.
    pub app_owners_released: bool,
    /// Unmodified diagnostic rendering of the actual consuming shutdown evidence.
    pub app_shutdown: Option<String>,
    /// Setup/persistence/native/cleanup failures; none is a compatibility PASS.
    pub failures: Vec<String>,
}

fn prepare(
    app: &mut AppState,
    request: &NativeQuestionFixtureRequest,
) -> anyhow::Result<NativeQuestionFixtureReport> {
    anyhow::ensure!(
        request.project.is_absolute()
            && request.bundle.is_absolute()
            && request.requests_directory.is_absolute(),
        "fixture project, native bundle and request directory must be absolute"
    );
    anyhow::ensure!(
        request.project.extension().is_some_and(|ext| ext == "mdp"),
        "fixture project must be an .mdp archive"
    );
    anyhow::ensure!(!request.project.exists(), "fixture project already exists");
    anyhow::ensure!(
        !request.plugin_identifier.is_empty(),
        "actual native identifier is required"
    );
    anyhow::ensure!(request.timeout_ms != 0, "Window timeout must be nonzero");
    let interactions = [
        (
            "keyboard-yes.json",
            NativeQuestionWindowInteraction::KeyboardYes,
        ),
        (
            "pointer-no.json",
            NativeQuestionWindowInteraction::PointerNo,
        ),
        (
            "escape-dismiss.json",
            NativeQuestionWindowInteraction::EscapeDismiss,
        ),
    ];
    for (name, _) in interactions {
        anyhow::ensure!(
            !request.requests_directory.join(name).exists(),
            "a Window request already exists"
        );
    }
    let helper = std::env::current_exe()?;
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &request.bundle)?;
    let kind = crate::openfx_effect::register_selected_openfx_filter(
        &helper,
        &inspection,
        &request.plugin_identifier,
    )?;
    app.create_new_project_at(
        request.project.clone(),
        "Owned native Question",
        320,
        180,
        Rational::FPS_24,
    )?;
    let duration = TimelineTime::new(5, 1)?;
    let mut clip = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(40, 100, 180, 255),
        TimelineTime::ZERO,
        duration,
    )?;
    clip.label = Some("Owned Question source".to_owned());
    let clip_id = clip.id;
    app.authoring
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("canonical author Session missing"))?
        .edit_active_sequence("Create native Question source", |sequence| {
            let track = sequence.video_tracks.first_mut().ok_or_else(|| {
                mondrian_core::MondrianError::Other(anyhow::anyhow!(
                    "canonical video Track missing"
                ))
            })?;
            track.add_clip(clip)?;
            Ok(())
        })?;
    app.dispatch_action(
        ProductAction::VisualEffect(VisualEffectProductAction::AddToClip(
            VisualEffectAddToClipPayload { clip_id, effect_type: kind.clone() },
        ))
        .into_external_action(),
    )?;
    let selected = app
        .primary_selected_effect()
        .ok_or_else(|| anyhow::anyhow!("actual AddToClip selection missing"))?;
    anyhow::ensure!(
        selected.clip.clip_id == clip_id,
        "actual effect belongs to another Clip"
    );
    let sequence_id = app
        .active_sequence_id()
        .ok_or_else(|| anyhow::anyhow!("active Sequence missing"))?;
    // This is a qualification comparison, never an author mutation/model source.
    let authored_clip = app
        .active_sequence()
        .and_then(|sequence| sequence.find_clip(clip_id))
        .ok_or_else(|| anyhow::anyhow!("actual authored Clip missing"))?
        .clone();
    let authored_settings = app
        .active_sequence()
        .ok_or_else(|| anyhow::anyhow!("Sequence missing"))?
        .settings
        .clone();
    anyhow::ensure!(
        authored_clip.effects.iter().any(|effect| effect.id == selected.effect_id
            && effect.effect_type == kind
            && effect.is_enabled),
        "registered native occurrence missing"
    );
    app.save_project_file()?;
    app.open_project_file(request.project.clone())?;
    let reopened = app
        .active_sequence()
        .ok_or_else(|| anyhow::anyhow!("canonical reopen lacks Sequence"))?;
    anyhow::ensure!(
        reopened.id == sequence_id && reopened.settings == authored_settings,
        "reopen changed exact Sequence identity/settings"
    );
    anyhow::ensure!(
        reopened.find_clip(clip_id) == Some(&authored_clip),
        "reopen changed exact Clip/native effect/schema data"
    );
    anyhow::ensure!(
        reopened.playhead == TimelineTime::ZERO && !app.is_playing(),
        "fixture is not paused at its visible source start"
    );
    anyhow::ensure!(
        !app.authoring
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("reopened Session missing"))?
            .is_dirty(),
        "reopened project is dirty"
    );
    std::fs::create_dir_all(&request.requests_directory)?;
    let mut paths = Vec::with_capacity(interactions.len());
    for (name, interaction) in interactions {
        let path = request.requests_directory.join(name);
        let driver = NativeQuestionWindowRequest {
            project: request.project.clone(),
            bundle: request.bundle.clone(),
            sequence: sequence_id,
            clip: clip_id,
            effect: selected.effect_id,
            interaction,
            timeout_ms: request.timeout_ms,
        };
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
        serde_json::to_writer_pretty(&mut file, &driver)?;
        file.write_all(b"\n")?;
        file.flush()?;
        paths.push(path);
    }
    Ok(NativeQuestionFixtureReport {
        project: Some(request.project.clone()),
        native_binary: Some(inspection.binary_path),
        native_binary_sha256: Some(inspection.binary_sha256),
        plugin_identifier: Some(request.plugin_identifier.clone()),
        sequence: Some(sequence_id),
        clip: Some(clip_id),
        effect: Some(selected.effect_id),
        requests: paths,
        saved_and_reopened: true,
        paused_clean_at_zero: true,
        ..Default::default()
    })
}

/// Create/save/reopen through the ordinary product APIs and emit real driver inputs.
///
/// This command does not Render the effect, open an editor, answer a Question,
/// mint UI authority, or claim physical/native Window qualification.
pub fn prepare_native_question_fixture_file(
    request_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    File::open(request_path)?.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err("fixture request exceeds 16 KiB".into());
    }
    let request: NativeQuestionFixtureRequest = serde_json::from_slice(&bytes)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(request.timeout_ms))
        .ok_or("fixture cleanup deadline overflow")?;
    let mut app = AppState::new();
    let mut report = match prepare(&mut app, &request) {
        Ok(report) => report,
        Err(error) => NativeQuestionFixtureReport {
            failures: vec![error.to_string()],
            ..Default::default()
        },
    };
    let shutdown = app.shutdown_for_endurance(deadline);
    report.app_owners_released = shutdown.all_resources_released();
    report.app_shutdown = Some(format!("{shutdown:?}"));
    if !report.app_owners_released {
        report.failures.push("actual App consuming cleanup failed".to_owned());
    }
    report.passed =
        report.saved_and_reopened && report.paused_clean_at_zero && report.failures.is_empty();
    println!("{}", serde_json::to_string(&report)?);
    if report.passed {
        Ok(())
    } else {
        Err("canonical native Question fixture preparation failed; see JSON report".into())
    }
}
