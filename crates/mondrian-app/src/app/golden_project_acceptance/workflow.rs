//! Single-Project product workflow driver for complete Golden execution.
//!
//! Focused Golden stages may use different diagnostic Sequences, but they must
//! never construct independent Projects and later union their reports. Complete
//! acceptance additionally requires every declared obligation to use one Hero
//! Sequence identity. This driver owns one production `AppState` plus an
//! immutable Project identity/path binding. Stage adapters use ordinary product
//! actions and are checked at every lifecycle boundary.

use super::harness::{
    author_checkpoint, durable_save_reopen, project_author_transition, AuthorCheckpoint,
    DirectoryCleanup, DurableReopenEvidence, ProjectAuthorTransitionEvidence,
};
use super::GoldenProjectContract;
use crate::app::ui_actions::{
    sequence_new_action, sequence_switch_active_action, SequenceTargetPayload,
};
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_core::{ProjectColorEnvironment, ProjectId, ProjectSettings, SequenceId};
use mondrian_editor_state::Action;
use mondrian_timeline::SequenceSettings;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Evidence that the newly created archive was closed and opened as the same Project.
#[derive(Debug, Clone, Serialize)]
pub(super) struct GoldenProjectOpenEvidence {
    pub created_session: AuthorCheckpoint,
    pub opened_session: AuthorCheckpoint,
    pub session_identity_changed: bool,
    pub project_identity_preserved: bool,
}

/// Private proof of the immutable Hero Sequence contract for one workflow.
struct GoldenHeroSequenceBinding {
    sequence_id: SequenceId,
    expected_settings: SequenceSettings,
    expected_color_environment: ProjectColorEnvironment,
}

/// How one Golden slice obtained its primary Sequence.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum GoldenSequenceStageBindingEvidence {
    /// The slice reused the one stable Hero Sequence.
    ExistingHero {
        active_before: SequenceId,
        switched: bool,
    },
    /// The slice created a focused diagnostic Sequence through one transaction.
    CreatedDiagnostic {
        author_step: ProjectAuthorTransitionEvidence,
    },
}

/// Evidence binding one declared Golden slice to its primary Sequence.
#[derive(Debug, Clone, Serialize)]
pub(super) struct GoldenSequenceStageEvidence {
    slice_id: String,
    sequence_role: String,
    sequence_id: SequenceId,
    binding: GoldenSequenceStageBindingEvidence,
}

impl GoldenSequenceStageEvidence {
    pub(super) const fn sequence_id(&self) -> SequenceId {
        self.sequence_id
    }
}

/// Headless driver over the same product App Interfaces used by the Window.
pub(super) struct GoldenProductWorkflowDriver {
    // Field order is intentional: AppState drops SQLite/runtime handles before cleanup.
    state: AppState,
    _runtime_cleanup: DirectoryCleanup,
    project_id: ProjectId,
    project_path: PathBuf,
    hero: GoldenHeroSequenceBinding,
}

impl GoldenProductWorkflowDriver {
    /// Create one persisted Project that all later Golden stages must share.
    pub(super) fn create(
        project_path: PathBuf,
        name: &str,
        sequence_settings: SequenceSettings,
        color_environment: ProjectColorEnvironment,
        project_settings: ProjectSettings,
    ) -> anyhow::Result<Self> {
        let mut state = AppState::new();
        state.create_new_project_with_settings_at(
            project_path.clone(),
            name,
            sequence_settings.clone(),
            color_environment.clone(),
            project_settings,
        )?;
        let project_id = state.project_id().context("created Project has no identity")?;
        let hero_sequence_id =
            state.active_sequence().context("created Project has no initial Sequence")?.id;
        let mut runtime_cleanup = DirectoryCleanup::default();
        runtime_cleanup.track(state.project_runtime_dir().map(Path::to_path_buf));
        let driver = Self {
            state,
            _runtime_cleanup: runtime_cleanup,
            project_id,
            project_path,
            hero: GoldenHeroSequenceBinding {
                sequence_id: hero_sequence_id,
                expected_settings: sequence_settings,
                expected_color_environment: color_environment,
            },
        };
        driver.verify_binding()?;
        Ok(driver)
    }

    pub(super) const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub(super) fn project_path(&self) -> &Path {
        &self.project_path
    }

    pub(super) const fn hero_sequence_id(&self) -> SequenceId {
        self.hero.sequence_id
    }

    pub(super) fn app(&self) -> &AppState {
        &self.state
    }

    /// Stage Adapter access to production product Interfaces.
    ///
    /// The coordinator verifies the immutable Project binding at stage
    /// boundaries; callers may not replace or close the Project themselves.
    pub(super) fn app_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    pub(super) fn verify_binding(&self) -> anyhow::Result<()> {
        ensure!(
            self.state.project_id() == Some(self.project_id),
            "Golden workflow changed Project identity"
        );
        ensure!(
            self.state.current_project_path() == Some(self.project_path.as_path()),
            "Golden workflow changed Project path"
        );
        ensure!(
            self.state
                .sequence_by_id(self.hero.sequence_id)
                .is_some_and(|sequence| sequence.settings == self.hero.expected_settings),
            "Golden workflow lost its Hero Sequence"
        );
        ensure!(
            self.state.project_color_environment() == &self.hero.expected_color_environment,
            "Golden workflow changed the Project Color Environment"
        );
        Ok(())
    }

    /// Prove the initial archive crosses a fresh product open boundary.
    pub(super) fn reopen_created_project(&mut self) -> anyhow::Result<GoldenProjectOpenEvidence> {
        self.verify_binding()?;
        let created_session = author_checkpoint(&self.state)?;
        self.state.close_project();
        ensure!(
            self.state.authoring_session_id().is_none(),
            "close retained the created Authoring Session"
        );
        self.state.dispatch_action(Action::OpenProject(self.project_path.clone()))?;
        self.verify_binding()?;
        let opened_session = author_checkpoint(&self.state)?;
        ensure!(
            created_session.session_id != opened_session.session_id,
            "initial reopen reused the created Authoring Session"
        );
        ensure!(
            opened_session.author_generation == 1,
            "initial reopen did not begin with Author Generation one"
        );
        ensure!(
            created_session.project_id == opened_session.project_id
                && created_session.project_path == opened_session.project_path,
            "initial reopen changed Project identity"
        );
        ensure!(
            created_session.active_sequence_id == opened_session.active_sequence_id
                && created_session.sequence_revision == opened_session.sequence_revision,
            "initial reopen changed the active Sequence"
        );
        Ok(GoldenProjectOpenEvidence {
            created_session,
            opened_session,
            session_identity_changed: true,
            project_identity_preserved: true,
        })
    }

    /// Bind one declared slice to the primary Sequence required by its role.
    ///
    /// Hero slices reuse one stable identity. Focused diagnostic slices create
    /// an isolated Sequence through the ordinary product authoring Interface.
    pub(super) fn bind_slice_primary_sequence(
        &mut self,
        contract: &GoldenProjectContract,
        slice_id: &str,
    ) -> anyhow::Result<GoldenSequenceStageEvidence> {
        self.verify_binding()?;
        let slice = contract
            .execution_slices
            .iter()
            .find(|slice| slice.id == slice_id)
            .with_context(|| format!("Golden execution slice is absent: {slice_id}"))?;
        if slice.sequence_role == contract.hero_sequence.role {
            let active_before = self
                .state
                .active_sequence_id()
                .context("Golden workflow has no active Sequence")?;
            let switched = active_before != self.hero.sequence_id;
            if switched {
                self.state.dispatch_action(sequence_switch_active_action(
                    SequenceTargetPayload { sequence_id: self.hero.sequence_id },
                ))?;
            }
            self.verify_binding()?;
            ensure!(
                self.state.active_sequence_id() == Some(self.hero.sequence_id),
                "Hero-assigned slice {slice_id} did not activate the Hero Sequence"
            );
            return Ok(GoldenSequenceStageEvidence {
                slice_id: slice.id.clone(),
                sequence_role: slice.sequence_role.clone(),
                sequence_id: self.hero.sequence_id,
                binding: GoldenSequenceStageBindingEvidence::ExistingHero {
                    active_before,
                    switched,
                },
            });
        }

        let expected_settings = self.state.new_sequence_defaults().clone();
        let before_ids = self
            .state
            .sequences()
            .iter()
            .map(|sequence| sequence.id)
            .collect::<BTreeSet<_>>();
        let (sequence_id, author_step) =
            project_author_transition(&mut self.state, "create-golden-stage-sequence", |state| {
                state.dispatch_action(sequence_new_action())?;
                let created = state
                    .sequences()
                    .iter()
                    .filter(|sequence| !before_ids.contains(&sequence.id))
                    .map(|sequence| sequence.id)
                    .collect::<Vec<_>>();
                ensure!(
                    created.len() == 1,
                    "Sequence product action created {} Sequences",
                    created.len()
                );
                Ok(created[0])
            })?;
        self.verify_binding()?;
        let sequence = self
            .state
            .active_sequence()
            .context("new Golden stage has no active Sequence")?;
        ensure!(
            sequence.id == sequence_id && sequence.settings == expected_settings,
            "new Golden stage Sequence differs from the Project template"
        );
        Ok(GoldenSequenceStageEvidence {
            slice_id: slice.id.clone(),
            sequence_role: slice.sequence_role.clone(),
            sequence_id,
            binding: GoldenSequenceStageBindingEvidence::CreatedDiagnostic { author_step },
        })
    }

    /// Save and reopen while proving that one slice retains its primary Sequence.
    pub(super) fn durable_save_reopen_for(
        &mut self,
        stage: &GoldenSequenceStageEvidence,
    ) -> anyhow::Result<DurableReopenEvidence> {
        self.verify_binding()?;
        ensure!(
            self.state.active_sequence_id() == Some(stage.sequence_id),
            "slice {} is not active before durable reopen",
            stage.slice_id
        );
        let evidence = durable_save_reopen(&mut self.state, &self.project_path)?;
        self.verify_binding()?;
        ensure!(
            self.state.active_sequence_id() == Some(stage.sequence_id),
            "slice {} changed its primary Sequence across durable reopen",
            stage.slice_id
        );
        Ok(evidence)
    }

    /// Save and reopen while retaining the workflow's immutable Project binding.
    pub(super) fn durable_save_reopen(&mut self) -> anyhow::Result<DurableReopenEvidence> {
        self.verify_binding()?;
        let evidence = durable_save_reopen(&mut self.state, &self.project_path)?;
        self.verify_binding()?;
        Ok(evidence)
    }
}

#[test]
fn golden_product_workflow_binds_hero_and_diagnostic_sequences_explicitly() -> anyhow::Result<()> {
    use super::harness::new_run_directory;
    use super::{load_golden_contract, repository_root, sequence_settings_from_contract};

    let root = repository_root();
    let mut contract = load_golden_contract(&root)?;
    let directory = new_run_directory(
        &root,
        "MONDRIAN_GOLDEN_WORKFLOW_RUN_ROOT",
        "golden-workflow",
    )?;
    let project_path = directory.join("single-project-workflow.mdp");
    let mut workflow = GoldenProductWorkflowDriver::create(
        project_path.clone(),
        "Single Project Golden Workflow",
        sequence_settings_from_contract(&contract.timeline)?,
        ProjectColorEnvironment::default(),
        ProjectSettings::default(),
    )?;
    let project_id = workflow.project_id();

    let opened = workflow.reopen_created_project()?;
    assert!(opened.session_identity_changed);
    assert!(opened.project_identity_preserved);
    assert_eq!(workflow.project_id(), project_id);
    assert_eq!(workflow.project_path(), project_path);

    let hero = workflow
        .bind_slice_primary_sequence(&contract, super::foundation_audio::FOUNDATION_SLICE_ID)?;
    assert_eq!(hero.sequence_id(), workflow.hero_sequence_id());
    assert_eq!(hero.sequence_role, contract.hero_sequence.role);
    assert!(matches!(
        hero.binding,
        GoldenSequenceStageBindingEvidence::ExistingHero { switched: false, .. }
    ));
    assert_eq!(workflow.app().sequences().len(), 1);

    let proxy = workflow
        .bind_slice_primary_sequence(&contract, super::proxy_relink::PROXY_RELINK_SLICE_ID)?;
    assert!(matches!(
        proxy.binding,
        GoldenSequenceStageBindingEvidence::ExistingHero { switched: false, .. }
    ));
    assert_eq!(proxy.sequence_id(), workflow.hero_sequence_id());
    assert_eq!(workflow.app().sequences().len(), 1);

    contract
        .execution_slices
        .iter_mut()
        .find(|slice| slice.id == super::color_media_roundtrip::COLOR_MEDIA_SLICE_ID)
        .context("Color Media slice is absent")?
        .sequence_role = "diagnostic-color-media".to_owned();
    let diagnostic = workflow.bind_slice_primary_sequence(
        &contract,
        super::color_media_roundtrip::COLOR_MEDIA_SLICE_ID,
    )?;
    assert!(matches!(
        diagnostic.binding,
        GoldenSequenceStageBindingEvidence::CreatedDiagnostic { .. }
    ));
    assert_eq!(
        workflow.app().active_sequence().map(|sequence| sequence.id),
        Some(diagnostic.sequence_id())
    );
    assert_eq!(workflow.app().sequences().len(), 2);

    let editorial = workflow
        .bind_slice_primary_sequence(&contract, super::editorial_transport::EDITORIAL_SLICE_ID)?;
    assert_eq!(editorial.sequence_id(), workflow.hero_sequence_id());
    assert!(matches!(
        editorial.binding,
        GoldenSequenceStageBindingEvidence::ExistingHero { switched: true, .. }
    ));
    let durability = workflow.durable_save_reopen_for(&editorial)?;
    assert!(durability.session_identity_changed);
    assert!(durability.project_identity_preserved);
    assert_eq!(workflow.project_id(), project_id);
    assert_eq!(workflow.app().sequences().len(), 2);
    assert_eq!(
        workflow.app().project_id(),
        Some(project_id),
        "reopen must retain persisted ProjectId"
    );

    workflow.app_mut().close_project();
    let binding_error = workflow
        .verify_binding()
        .expect_err("closed Project must fail the workflow binding");
    assert!(binding_error.to_string().contains("Project identity"));

    drop(workflow);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
