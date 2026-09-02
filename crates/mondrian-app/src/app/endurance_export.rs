//! Phase-scoped repeated Export execution over one immutable Timeline snapshot.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mondrian_core::{JobId, SequenceId};
use mondrian_export::preset::{
    ExportConfig, ExportOutputPolicy, ExportPreset, TimelineExportRange,
};
use mondrian_export::queue::{
    ExportArtifactPublicationEvidence, ExportPublicationState, JobStatus, RenderJob, RenderQueue,
};
use mondrian_export::{verify_export_artifact, IndependentExportArtifactPolicy};
use thiserror::Error;

use super::endurance_campaign::EnduranceCampaignEvent;
use super::exporting::{export_preset_extension, TimelineExportRequest};
use super::AppState;

const MAXIMUM_ARTIFACT_PREFIX_BYTES: usize = 64;

/// Exact App inputs used to freeze one repeated-Export phase.
#[derive(Debug, Clone)]
pub struct FrozenRepeatedExportRequest {
    /// Single-file delivery preset frozen for every attempt.
    pub preset: ExportPreset,
    /// Exact Sequence; `None` selects the active Sequence at capture.
    pub sequence_id: Option<SequenceId>,
    /// Exact Timeline range frozen into the execution snapshot.
    pub range: TimelineExportRange,
    /// Existing real directory that receives create-only artifacts.
    pub output_directory: PathBuf,
    /// Link-free ASCII prefix used with the monotonically increasing ordinal.
    pub artifact_prefix: String,
    /// Optional broadcaster QC contract frozen with every attempt.
    pub broadcast_qc: Option<mondrian_broadcast::BroadcastQcProfile>,
    /// Independent full-decode verification resource bounds.
    pub verification_policy: IndependentExportArtifactPolicy,
}

#[derive(Debug, Clone)]
struct FrozenExportPlan {
    base_config: ExportConfig,
    output_directory: PathBuf,
    artifact_prefix: String,
    extension: &'static str,
}

impl FrozenExportPlan {
    fn capture(
        app: &AppState,
        request: FrozenRepeatedExportRequest,
    ) -> Result<(Self, ProductionFrozenExportBackend), FrozenRepeatedExportError> {
        validate_single_file_preset(&request.preset)?;
        validate_artifact_prefix(&request.artifact_prefix)?;
        let output_directory = canonical_existing_directory(&request.output_directory)?;
        let extension = export_preset_extension(&request.preset);
        let first_output = artifact_path(&output_directory, &request.artifact_prefix, extension, 1);
        let base_config = app
            .build_timeline_export_config(TimelineExportRequest {
                preset: request.preset,
                sequence_id: request.sequence_id,
                range: request.range,
                output_path: first_output,
                output_policy: ExportOutputPolicy::CreateNew,
                broadcast_qc: request.broadcast_qc,
            })
            .map_err(FrozenRepeatedExportError::InvalidPlan)?;
        Ok((
            Self {
                base_config,
                output_directory,
                artifact_prefix: request.artifact_prefix,
                extension,
            },
            ProductionFrozenExportBackend {
                queue: Arc::clone(&app.render_queue),
                verification_policy: request.verification_policy,
            },
        ))
    }

    fn config(&self, ordinal: u64) -> ExportConfig {
        let mut config = self.base_config.clone();
        config.output_path = artifact_path(
            &self.output_directory,
            &self.artifact_prefix,
            self.extension,
            ordinal,
        );
        config
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrozenExportAttemptObservation {
    Active,
    Completed { output_path: PathBuf },
    Failed(String),
    Cancelled,
}

trait FrozenExportBackend {
    fn retained_jobs(&self) -> usize;
    fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String>;
    fn observe(&mut self, id: JobId) -> Result<FrozenExportAttemptObservation, String>;
    fn verify(
        &mut self,
        id: JobId,
        output_path: &Path,
        completed_at_us: u64,
    ) -> Result<EnduranceCampaignEvent, String>;
    fn clear_terminal_history(&mut self) -> usize;
}

struct ProductionFrozenExportBackend {
    queue: Arc<RenderQueue>,
    verification_policy: IndependentExportArtifactPolicy,
}

impl FrozenExportBackend for ProductionFrozenExportBackend {
    fn retained_jobs(&self) -> usize {
        self.queue.list_jobs().len()
    }

    fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String> {
        self.queue.enqueue(RenderJob::new(config)).map_err(|error| error.to_string())
    }

    fn observe(&mut self, id: JobId) -> Result<FrozenExportAttemptObservation, String> {
        let snapshot = self
            .queue
            .list_jobs()
            .into_iter()
            .find(|snapshot| snapshot.id == id)
            .ok_or_else(|| format!("phase-owned Export job {id} disappeared"))?;
        match snapshot.status {
            JobStatus::Pending | JobStatus::Running { .. } | JobStatus::Cancelling { .. } => {
                Ok(FrozenExportAttemptObservation::Active)
            }
            JobStatus::Failed(error) => {
                Ok(FrozenExportAttemptObservation::Failed(error.to_string()))
            }
            JobStatus::Cancelled => Ok(FrozenExportAttemptObservation::Cancelled),
            JobStatus::Completed => match (snapshot.publication, snapshot.artifact_publication) {
                (
                    ExportPublicationState::Published,
                    Some(ExportArtifactPublicationEvidence::Durable { output_path }),
                ) if output_path == snapshot.output_path => {
                    Ok(FrozenExportAttemptObservation::Completed { output_path })
                }
                _ => Err(format!(
                    "phase-owned Export job {id} completed without exact durable publication"
                )),
            },
        }
    }

    fn verify(
        &mut self,
        id: JobId,
        output_path: &Path,
        completed_at_us: u64,
    ) -> Result<EnduranceCampaignEvent, String> {
        let receipt = verify_export_artifact(
            output_path,
            format!("endurance-export-{id}"),
            self.verification_policy,
        )
        .map_err(|error| error.to_string())?;
        Ok(EnduranceCampaignEvent::export_artifact_verified(
            completed_at_us,
            &receipt,
        ))
    }

    fn clear_terminal_history(&mut self) -> usize {
        self.queue.clear_terminal_history()
    }
}

#[derive(Debug, Clone)]
struct ActiveFrozenExportAttempt {
    id: JobId,
    output_path: PathBuf,
}

struct FrozenRepeatedExportState<B> {
    plan: FrozenExportPlan,
    backend: B,
    active: Option<ActiveFrozenExportAttempt>,
    next_ordinal: u64,
    verified_artifacts: u64,
    closing: bool,
    fault: Option<String>,
}

/// Sequential phase owner that repeats one frozen Export and verifies every artifact.
pub struct FrozenRepeatedExportPhase {
    state: FrozenRepeatedExportState<ProductionFrozenExportBackend>,
}

impl FrozenRepeatedExportPhase {
    /// Capture one immutable snapshot and admit the first phase-owned attempt.
    pub fn start(
        app: &AppState,
        request: FrozenRepeatedExportRequest,
    ) -> Result<Self, FrozenRepeatedExportError> {
        let (plan, backend) = FrozenExportPlan::capture(app, request)?;
        Ok(Self {
            state: FrozenRepeatedExportState::start_with_backend(plan, backend)?,
        })
    }

    /// Observe one attempt, seal a verified event, or admit the next serial attempt.
    pub fn poll(
        &mut self,
        completed_at_us: u64,
    ) -> Result<Vec<EnduranceCampaignEvent>, FrozenRepeatedExportError> {
        self.state.poll(completed_at_us)
    }

    /// Stop new admission while allowing the active attempt to publish and verify.
    pub fn begin_close(&mut self) {
        self.state.begin_close();
    }

    /// Whether no current attempt or terminal queue evidence remains.
    pub fn is_quiescent(&self) -> bool {
        self.state.is_quiescent()
    }

    /// Number of independently verified artifacts completed by this phase owner.
    pub const fn verified_artifacts(&self) -> u64 {
        self.state.verified_artifacts()
    }
}

impl<B: FrozenExportBackend> FrozenRepeatedExportState<B> {
    fn start_with_backend(
        plan: FrozenExportPlan,
        backend: B,
    ) -> Result<Self, FrozenRepeatedExportError> {
        if backend.retained_jobs() != 0 {
            return Err(FrozenRepeatedExportError::ContaminatedQueue);
        }
        let mut phase = Self {
            plan,
            backend,
            active: None,
            next_ordinal: 1,
            verified_artifacts: 0,
            closing: false,
            fault: None,
        };
        phase.enqueue_next()?;
        Ok(phase)
    }

    /// Observe one attempt, seal a verified event, or admit the next serial attempt.
    fn poll(
        &mut self,
        completed_at_us: u64,
    ) -> Result<Vec<EnduranceCampaignEvent>, FrozenRepeatedExportError> {
        if let Some(detail) = &self.fault {
            return Err(FrozenRepeatedExportError::Faulted(detail.clone()));
        }
        let Some(active) = self.active.clone() else {
            if !self.closing {
                self.enqueue_next()?;
            }
            return Ok(Vec::new());
        };
        let retained_jobs = self.backend.retained_jobs();
        if retained_jobs != 1 {
            return Err(self.latch_fault(format!(
                "repeated Export phase expected exactly one owned job, found {retained_jobs}"
            )));
        }
        let observation =
            self.backend.observe(active.id).map_err(|detail| self.latch_fault(detail))?;
        match observation {
            FrozenExportAttemptObservation::Active => Ok(Vec::new()),
            FrozenExportAttemptObservation::Failed(detail) => {
                Err(self.latch_fault(format!("phase-owned Export failed: {detail}")))
            }
            FrozenExportAttemptObservation::Cancelled => {
                Err(self.latch_fault("continuous Export attempt was cancelled".to_owned()))
            }
            FrozenExportAttemptObservation::Completed { output_path } => {
                if output_path != active.output_path {
                    return Err(self.latch_fault(
                        "durable Export path differs from the admitted attempt".to_owned(),
                    ));
                }
                let event = self
                    .backend
                    .verify(active.id, &output_path, completed_at_us)
                    .map_err(|detail| self.latch_fault(detail))?;
                let removed = self.backend.clear_terminal_history();
                if removed != 1 {
                    return Err(self.latch_fault(format!(
                        "phase-owned Export terminal cleanup removed {removed} jobs"
                    )));
                }
                self.active = None;
                self.verified_artifacts =
                    self.verified_artifacts.checked_add(1).ok_or_else(|| {
                        self.latch_fault("verified Export artifact counter overflow".to_owned())
                    })?;
                Ok(vec![event])
            }
        }
    }

    /// Stop new admission while allowing the active attempt to publish and verify.
    fn begin_close(&mut self) {
        self.closing = true;
    }

    /// Whether no current attempt or terminal queue evidence remains.
    fn is_quiescent(&self) -> bool {
        self.closing
            && self.active.is_none()
            && self.fault.is_none()
            && self.backend.retained_jobs() == 0
    }

    /// Number of independently verified artifacts completed by this phase owner.
    const fn verified_artifacts(&self) -> u64 {
        self.verified_artifacts
    }

    fn enqueue_next(&mut self) -> Result<(), FrozenRepeatedExportError> {
        let retained_jobs = self.backend.retained_jobs();
        if retained_jobs != 0 {
            return Err(self.latch_fault(format!(
                "repeated Export phase cannot admit beside {retained_jobs} retained jobs"
            )));
        }
        let ordinal = self.next_ordinal;
        let config = self.plan.config(ordinal);
        let output_path = config.output_path.clone();
        let id = self.backend.enqueue(config).map_err(|detail| self.latch_fault(detail))?;
        self.next_ordinal = ordinal
            .checked_add(1)
            .ok_or_else(|| self.latch_fault("Export attempt ordinal overflow".to_owned()))?;
        self.active = Some(ActiveFrozenExportAttempt { id, output_path });
        Ok(())
    }

    fn latch_fault(&mut self, detail: String) -> FrozenRepeatedExportError {
        self.fault = Some(detail.clone());
        FrozenRepeatedExportError::Faulted(detail)
    }
}

/// Stable repeated-Export phase failure.
#[derive(Debug, Error)]
pub enum FrozenRepeatedExportError {
    /// Frozen request, author snapshot, output route, or delivery contract was invalid.
    #[error("invalid frozen Export phase plan: {0}")]
    InvalidPlan(String),
    /// The App queue retained work before this phase attempted to start.
    #[error("repeated Export requires a fresh empty phase queue")]
    ContaminatedQueue,
    /// A started attempt, publication, verification, or exact cleanup failed.
    #[error("repeated Export phase failed: {0}")]
    Faulted(String),
}

fn validate_single_file_preset(preset: &ExportPreset) -> Result<(), FrozenRepeatedExportError> {
    if preset.media_file().is_none()
        || preset.professional_delivery().is_some()
        || preset.image_sequence_format().is_some()
        || preset.audio_stem_format().is_some()
    {
        return Err(FrozenRepeatedExportError::InvalidPlan(
            "independent repeated verification currently requires one media-file artifact"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_artifact_prefix(prefix: &str) -> Result<(), FrozenRepeatedExportError> {
    if prefix.is_empty()
        || prefix.len() > MAXIMUM_ARTIFACT_PREFIX_BYTES
        || matches!(prefix, "." | "..")
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(FrozenRepeatedExportError::InvalidPlan(
            "artifact prefix must be a non-empty link-free ASCII token".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_existing_directory(path: &Path) -> Result<PathBuf, FrozenRepeatedExportError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| FrozenRepeatedExportError::InvalidPlan(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(FrozenRepeatedExportError::InvalidPlan(
            "output directory must be an existing real directory".to_owned(),
        ));
    }
    path.canonicalize()
        .map_err(|error| FrozenRepeatedExportError::InvalidPlan(error.to_string()))
}

fn artifact_path(directory: &Path, prefix: &str, extension: &str, ordinal: u64) -> PathBuf {
    directory.join(format!("{prefix}-{ordinal:08}.{extension}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use mondrian_export::preset::{BuiltinExportPreset, TimelineExportSnapshot};
    use mondrian_timeline::sequence::Sequence;

    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Debug, Clone)]
    enum FakeTerminal {
        Completed,
        Failed,
        Cancelled,
        WrongPath,
    }

    struct FakeBackend {
        configs: Vec<ExportConfig>,
        active: Option<JobId>,
        terminal: FakeTerminal,
        verify_fails: bool,
        cleanup_count: usize,
        timeline_bytes: Vec<Vec<u8>>,
    }

    impl FakeBackend {
        fn clean() -> Self {
            Self {
                configs: Vec::new(),
                active: None,
                terminal: FakeTerminal::Completed,
                verify_fails: false,
                cleanup_count: 1,
                timeline_bytes: Vec::new(),
            }
        }
    }

    impl FrozenExportBackend for FakeBackend {
        fn retained_jobs(&self) -> usize {
            usize::from(self.active.is_some())
        }

        fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String> {
            let id = JobId::new();
            self.timeline_bytes.push(
                serde_json::to_vec(config.timeline.as_ref()).map_err(|error| error.to_string())?,
            );
            self.configs.push(config);
            self.active = Some(id);
            Ok(id)
        }

        fn observe(&mut self, id: JobId) -> Result<FrozenExportAttemptObservation, String> {
            if self.active != Some(id) {
                return Err("unknown fake attempt".to_owned());
            }
            let path = self
                .configs
                .last()
                .map(|config| config.output_path.clone())
                .ok_or_else(|| "missing fake config".to_owned())?;
            Ok(match self.terminal {
                FakeTerminal::Completed => {
                    FrozenExportAttemptObservation::Completed { output_path: path }
                }
                FakeTerminal::Failed => {
                    FrozenExportAttemptObservation::Failed("injected failure".to_owned())
                }
                FakeTerminal::Cancelled => FrozenExportAttemptObservation::Cancelled,
                FakeTerminal::WrongPath => FrozenExportAttemptObservation::Completed {
                    output_path: path.with_extension("wrong"),
                },
            })
        }

        fn verify(
            &mut self,
            id: JobId,
            _output_path: &Path,
            completed_at_us: u64,
        ) -> Result<EnduranceCampaignEvent, String> {
            if self.verify_fails {
                return Err("injected verifier failure".to_owned());
            }
            Ok(EnduranceCampaignEvent::test_export_artifact_verified(
                completed_at_us,
                format!("fake-{id}"),
                SHA,
                "fake-verifier",
                SHA,
            ))
        }

        fn clear_terminal_history(&mut self) -> usize {
            self.active = None;
            self.cleanup_count
        }
    }

    fn test_plan() -> (tempfile::TempDir, FrozenExportPlan) {
        let directory = tempfile::tempdir().expect("temporary output directory");
        let preset = BuiltinExportPreset::H264AacSdr1080p.preset();
        let sequence = Sequence::new("Frozen");
        let timeline = TimelineExportSnapshot::unprepared(
            Default::default(),
            sequence,
            Vec::new(),
            HashMap::new(),
            TimelineExportRange::SequenceInOut,
        );
        let plan = FrozenExportPlan {
            base_config: ExportConfig {
                preset,
                timeline: Box::new(timeline),
                output_path: directory.path().join("placeholder.mp4"),
                output_policy: ExportOutputPolicy::CreateNew,
                smart_render: mondrian_export::ExportSmartRenderPolicy::Automatic,
                broadcast_qc: None,
            },
            output_directory: directory.path().to_path_buf(),
            artifact_prefix: "endurance".to_owned(),
            extension: "mp4",
        };
        (directory, plan)
    }

    #[test]
    fn repeats_one_frozen_snapshot_with_unique_create_new_artifacts() {
        let (_directory, plan) = test_plan();
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, FakeBackend::clean())
            .expect("start phase");

        assert_eq!(phase.poll(10).expect("verify first").len(), 1);
        assert!(phase.poll(11).expect("enqueue second").is_empty());
        assert_eq!(phase.poll(20).expect("verify second").len(), 1);
        assert_eq!(phase.verified_artifacts(), 2);
        assert_eq!(phase.backend.configs.len(), 2);
        assert_ne!(
            phase.backend.configs[0].output_path,
            phase.backend.configs[1].output_path
        );
        assert_eq!(
            phase.backend.timeline_bytes[0],
            phase.backend.timeline_bytes[1]
        );
        assert!(phase
            .backend
            .configs
            .iter()
            .all(|config| config.output_policy == ExportOutputPolicy::CreateNew));
    }

    #[test]
    fn close_stops_new_admission_after_current_artifact_is_verified() {
        let (_directory, plan) = test_plan();
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, FakeBackend::clean())
            .expect("start phase");
        phase.begin_close();

        assert_eq!(phase.poll(10).expect("verify terminal attempt").len(), 1);
        assert!(phase.is_quiescent());
        assert!(phase.poll(11).expect("remain closed").is_empty());
        assert_eq!(phase.backend.configs.len(), 1);
    }

    #[test]
    fn failure_cancellation_wrong_path_and_verifier_failure_latch_terminal_fault() {
        for terminal in [
            FakeTerminal::Failed,
            FakeTerminal::Cancelled,
            FakeTerminal::WrongPath,
        ] {
            let (_directory, plan) = test_plan();
            let mut backend = FakeBackend::clean();
            backend.terminal = terminal;
            let mut phase =
                FrozenRepeatedExportState::start_with_backend(plan, backend).expect("start phase");
            assert!(phase.poll(10).is_err());
            assert!(phase.poll(11).is_err());
            assert_eq!(phase.backend.configs.len(), 1);
        }

        let (_directory, plan) = test_plan();
        let mut backend = FakeBackend::clean();
        backend.verify_fails = true;
        let mut phase =
            FrozenRepeatedExportState::start_with_backend(plan, backend).expect("start phase");
        assert!(phase.poll(10).is_err());
        assert!(phase.poll(11).is_err());
        assert_eq!(phase.backend.configs.len(), 1);
    }

    #[test]
    fn contaminated_queue_and_non_exact_cleanup_are_rejected() {
        let (_directory, plan) = test_plan();
        let mut contaminated = FakeBackend::clean();
        contaminated.active = Some(JobId::new());
        assert!(matches!(
            FrozenRepeatedExportState::start_with_backend(plan, contaminated),
            Err(FrozenRepeatedExportError::ContaminatedQueue)
        ));

        let (_directory, plan) = test_plan();
        let mut backend = FakeBackend::clean();
        backend.cleanup_count = 0;
        let mut phase =
            FrozenRepeatedExportState::start_with_backend(plan, backend).expect("start phase");
        assert!(phase.poll(10).is_err());
        assert_eq!(phase.verified_artifacts(), 0);

        let (_directory, plan) = test_plan();
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, FakeBackend::clean())
            .expect("start phase");
        assert_eq!(phase.poll(10).expect("verify first").len(), 1);
        phase.backend.active = Some(JobId::new());
        assert!(phase.poll(11).is_err());
        assert_eq!(phase.backend.configs.len(), 1);
    }

    #[test]
    fn policy_rejects_directory_artifacts_and_unsafe_prefixes() {
        let directory_preset = BuiltinExportPreset::PngSequence.preset();
        assert!(validate_single_file_preset(&directory_preset).is_err());
        for prefix in ["", ".", "..", "contains/slash", "包含非 ASCII"] {
            assert!(validate_artifact_prefix(prefix).is_err());
        }
        let policy = IndependentExportArtifactPolicy::new(1, Duration::from_secs(1))
            .expect("nonzero test policy");
        assert_eq!(policy.maximum_artifact_bytes(), 1);
    }
}
