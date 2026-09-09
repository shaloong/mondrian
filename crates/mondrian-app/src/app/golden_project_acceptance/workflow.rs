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

#[derive(Debug, thiserror::Error)]
#[error("{primary}; Golden Project startup App closure qualified={qualified}", qualified = .app.all_resources_released())]
pub(super) struct GoldenWorkflowStartupClosedFailure {
    primary: anyhow::Error,
    pub(super) app: crate::app::endurance_shutdown::AppEnduranceShutdownEvidence,
}

/// Focused operation output published only with its complete owning App receipt.
#[cfg(test)]
#[derive(Debug, Serialize)]
pub(super) struct GoldenOwnedOperation<T> {
    #[serde(flatten)]
    pub(super) value: T,
    app_shutdown_json: String,
    app_shutdown_sha256: String,
    #[serde(skip)]
    pub(super) app_shutdown: crate::app::endurance_shutdown::AppEnduranceShutdownEvidence,
}

#[cfg(test)]
impl<T> std::ops::Deref for GoldenOwnedOperation<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
#[derive(Debug, thiserror::Error)]
#[error("{primary}; Golden operation App closure qualified={qualified}", qualified = .app.all_resources_released())]
pub(super) struct GoldenWorkflowClosedFailure {
    primary: anyhow::Error,
    pub(super) app: crate::app::endurance_shutdown::AppEnduranceShutdownEvidence,
}

/// Close an independent focused App with the same complete Golden receipt protocol.
#[cfg(test)]
pub(super) fn run_golden_app_operation<T>(
    mut state: AppState,
    operation: impl FnOnce(&mut AppState) -> anyhow::Result<T>,
) -> anyhow::Result<GoldenOwnedOperation<T>> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&mut state)))
        .unwrap_or_else(|payload| {
            Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
        });
    let app = state
        .shutdown_for_endurance(std::time::Instant::now() + std::time::Duration::from_secs(30));
    finish_golden_app_operation(result, app, None)
}

#[cfg(test)]
fn finish_golden_app_operation<T>(
    result: anyhow::Result<T>,
    app: crate::app::endurance_shutdown::AppEnduranceShutdownEvidence,
    mut cleanup: Option<DirectoryCleanup>,
) -> anyhow::Result<GoldenOwnedOperation<T>> {
    if !app.all_resources_released()
        && let Some(cleanup) = cleanup.as_mut()
    {
        cleanup.retain();
    }
    let result = result.and_then(|value| {
        ensure!(
            app.all_resources_released(),
            "Golden operation retained App owners"
        );
        let sealed = crate::app::endurance_shutdown::AppEnduranceShutdownReceipt::seal(&app)?;
        Ok((value, sealed))
    });
    match result {
        Ok((value, sealed)) => Ok(GoldenOwnedOperation {
            value,
            app_shutdown_json: sealed.canonical_json().to_owned(),
            app_shutdown_sha256: sealed.sha256().to_owned(),
            app_shutdown: app,
        }),
        Err(primary) => Err(GoldenWorkflowClosedFailure { primary, app }.into()),
    }
}

impl GoldenProductWorkflowDriver {
    /// Keep the owner outside all focused-operation error and unwind paths.
    #[cfg(test)]
    pub(super) fn run_with<T>(
        self,
        operation: impl FnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<GoldenOwnedOperation<T>> {
        self.run_with_cleanup(None, operation)
    }

    #[cfg(test)]
    pub(super) fn run_with_cleanup<T>(
        mut self,
        cleanup: Option<DirectoryCleanup>,
        operation: impl FnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<GoldenOwnedOperation<T>> {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&mut self)))
                .unwrap_or_else(|payload| {
                    Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
                });
        self.finish_with_cleanup(result, cleanup)
    }

    #[cfg(test)]
    pub(super) fn finish_with_cleanup<T>(
        self,
        result: anyhow::Result<T>,
        cleanup: Option<DirectoryCleanup>,
    ) -> anyhow::Result<GoldenOwnedOperation<T>> {
        let app =
            self.shutdown_until(std::time::Instant::now() + std::time::Duration::from_secs(30));
        finish_golden_app_operation(result, app, cleanup)
    }

    /// Consume the actual App before allowing its runtime directory to be removed.
    pub(super) fn shutdown_until(
        self,
        deadline: std::time::Instant,
    ) -> crate::app::endurance_shutdown::AppEnduranceShutdownEvidence {
        let Self { state, mut _runtime_cleanup, .. } = self;
        let receipt = state.shutdown_for_endurance(deadline);
        if !receipt.all_resources_released() {
            _runtime_cleanup.retain();
        }
        receipt
    }

    /// Create one persisted Project that all later Golden stages must share.
    pub(super) fn create(
        project_path: PathBuf,
        name: &str,
        sequence_settings: SequenceSettings,
        color_environment: ProjectColorEnvironment,
        project_settings: ProjectSettings,
    ) -> anyhow::Result<Self> {
        Self::create_with_deadline(
            project_path,
            name,
            sequence_settings,
            color_environment,
            project_settings,
            None,
        )
    }

    /// Bind construction failures and their consuming App closure to one deadline.
    #[cfg(feature = "validation")]
    pub(super) fn create_until(
        project_path: PathBuf,
        name: &str,
        sequence_settings: SequenceSettings,
        color_environment: ProjectColorEnvironment,
        project_settings: ProjectSettings,
        deadline: std::time::Instant,
    ) -> anyhow::Result<Self> {
        ensure!(
            std::time::Instant::now() < deadline,
            "Golden Project deadline elapsed before admission"
        );
        Self::create_with_deadline(
            project_path,
            name,
            sequence_settings,
            color_environment,
            project_settings,
            Some(deadline),
        )
    }

    fn create_with_deadline(
        project_path: PathBuf,
        name: &str,
        sequence_settings: SequenceSettings,
        color_environment: ProjectColorEnvironment,
        project_settings: ProjectSettings,
        deadline: Option<std::time::Instant>,
    ) -> anyhow::Result<Self> {
        let close_deadline = || {
            let local = std::time::Instant::now() + std::time::Duration::from_secs(30);
            deadline.map_or(local, |original| original.min(local))
        };
        let mut state = AppState::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.create_new_project_with_settings_at(
                project_path.clone(),
                name,
                sequence_settings.clone(),
                color_environment.clone(),
                project_settings,
            )?;
            ensure!(
                deadline.is_none_or(|limit| std::time::Instant::now() < limit),
                "Golden Project construction exceeded original deadline"
            );
            let project_id = state.project_id().context("created Project has no identity")?;
            let hero_sequence_id =
                state.active_sequence().context("created Project has no initial Sequence")?.id;
            Ok::<_, anyhow::Error>((project_id, hero_sequence_id))
        }))
        .unwrap_or_else(|payload| {
            Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
        });
        let (project_id, hero_sequence_id) = match result {
            Ok(identities) => identities,
            Err(primary) => {
                return Err(GoldenWorkflowStartupClosedFailure {
                    primary,
                    app: state.shutdown_for_endurance(close_deadline()),
                }
                .into())
            }
        };
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
        if let Err(primary) = driver.verify_binding() {
            return Err(GoldenWorkflowStartupClosedFailure {
                primary,
                app: driver.shutdown_until(close_deadline()),
            }
            .into());
        }
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
        self.state.close_project()?;
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
    let workflow = GoldenProductWorkflowDriver::create(
        project_path.clone(),
        "Single Project Golden Workflow",
        sequence_settings_from_contract(&contract.timeline)?,
        ProjectColorEnvironment::default(),
        ProjectSettings::default(),
    )?;
    let closed = workflow.run_with(|workflow| {
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

        let editorial = workflow.bind_slice_primary_sequence(
            &contract,
            super::editorial_transport::EDITORIAL_SLICE_ID,
        )?;
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

        workflow.app_mut().close_project()?;
        let binding_error = workflow
            .verify_binding()
            .expect_err("closed Project must fail the workflow binding");
        assert!(binding_error.to_string().contains("Project identity"));

        Ok(())
    })?;
    assert!(closed.app_shutdown.all_resources_released());
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
