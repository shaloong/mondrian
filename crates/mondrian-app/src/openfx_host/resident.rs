//! Exclusive supervised native session, bounded control packets and shared frames.
use super::*;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::Child;
use std::thread;
pub(super) mod child_diagnostic;
mod close_observation;
mod diagnostic;
use child_diagnostic::ChildProbe;
pub use child_diagnostic::{
    OpenFxChildRenderDiagnostic, OpenFxChildRenderPhase, OpenFxNativeRenderPhase,
};
#[cfg(test)]
mod image_sampling_tests;
pub(super) mod pump;
pub(super) mod question;
pub(super) mod question_budget;
#[cfg(windows)]
mod retirement_wait;
#[cfg(any(test, feature = "validation"))]
pub use diagnostic::OpenFxRenderDiagnostic;
pub use diagnostic::OpenFxRenderPhase;
use diagnostic::{ParentProbe, ProbeEvent};

/// Hidden executable mode for a context-bound resident native CPU session.
pub const OPENFX_RESIDENT_WORKER_ARGUMENT: &str = "--internal-openfx-session-v11";
const ENDPOINT_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_ENDPOINT";
const NONCE_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_NONCE";
const VERSION: u16 = 11;
const POLL: Duration = Duration::from_millis(20);
const RETIRE: Duration = Duration::from_millis(100);
const FRAME_TRANSFER_BYTES: usize = 1024 * 1024;
const FRAME_TRANSFER_PIXELS: usize = FRAME_TRANSFER_BYTES / std::mem::size_of::<[f32; 4]>();

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    context: OpenFxContext,
    version: u16,
    nonce: String,
    binary_path: PathBuf,
    bundle_path: PathBuf,
    binary_sha256: String,
    plugin_identifier: String,
    project_geometry: EffectProjectGeometry,
    width: u32,
    height: u32,
    shared_path: PathBuf,
    input_alpha: mondrian_core::ColorFrameAlpha,
    owner: mondrian_core::EffectProcessorOwnerId,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Render {
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
        gui_open: bool,
        interactive_render: bool,
        question_work: bool,
        timing: OpenFxRenderTiming,
        sampling: mondrian_core::ImageSampling,
        parameters: BTreeMap<String, OpenFxParameterBinding>,
        diagnostic: bool,
    },
    Cancel {
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
    },
    QuestionReply {
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
        identity: question::QuestionIdentity,
        reply: question::QuestionReply,
    },
    CloseGui {
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
    },
    Shutdown {},
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(
    clippy::large_enum_variant,
    reason = "fixed bounded diagnostic POD avoids a per-frame boxed allocation; inline size is charged by the host ledger"
)]
enum Response {
    Connected {
        version: u16,
        nonce: String,
    },
    Ready {
        version: u16,
        nonce: String,
    },
    Rendered {
        sequence: u64,
        alpha: mondrian_core::ColorFrameAlpha,
        feedback: mondrian_effects::EffectProcessorFeedbackSnapshot,
        #[serde(deserialize_with = "child_diagnostic::required_optional")]
        diagnostic: Option<OpenFxChildRenderDiagnostic>,
    },
    UserBlocked {
        sequence: u64,
        feedback: mondrian_effects::EffectProcessorFeedbackSnapshot,
        #[serde(deserialize_with = "child_diagnostic::required_optional")]
        diagnostic: Option<OpenFxChildRenderDiagnostic>,
    },
    Rejected {
        sequence: u64,
        status: i32,
    },
    Progress {
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
        revision: u64,
        native_owner: u64,
        progress: mondrian_effects::EffectProcessorProgress,
    },
    Question {
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
        identity: question::QuestionIdentity,
        text: mondrian_effects::EffectProcessorNotice,
    },
    Canceled {
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
    },
    GuiClosed {
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
        #[serde(deserialize_with = "required_gui_feedback")]
        feedback: Option<mondrian_effects::EffectProcessorFeedbackSnapshot>,
    },
    Closed {},
}

fn required_gui_feedback<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<mondrian_effects::EffectProcessorFeedbackSnapshot>, D::Error> {
    Deserialize::deserialize(deserializer)
}

fn check_owner(
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    deadline: Instant,
) -> Result<(), OpenFxHostError> {
    checkpoint()?;
    if Instant::now() >= deadline {
        return Err(OpenFxHostError::DeadlineExceeded);
    }
    Ok(())
}

// A real exit still settles the sole Child when its observation is late, but
// only a status observed strictly before the original deadline is graceful.
struct ObservedRetirementExit {
    status: std::process::ExitStatus,
    graceful: Result<(), OpenFxHostError>,
}

// One nonblocking query of the existing owner, not a second waiter. The private
// clock/query seam also exercises the production deadline ordering in tests.
fn observe_retirement_exit(
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut query: impl FnMut() -> io::Result<Option<std::process::ExitStatus>>,
) -> Result<Option<ObservedRetirementExit>, OpenFxHostError> {
    if now() >= deadline {
        return Err(OpenFxHostError::DeadlineExceeded);
    }
    let status = query().map_err(control_error)?;
    let observed_at = now();
    match status {
        Some(status) => {
            let graceful = if observed_at >= deadline {
                Err(OpenFxHostError::DeadlineExceeded)
            } else if status.success() {
                Ok(())
            } else {
                Err(OpenFxHostError::WorkerFailed(status.to_string()))
            };
            Ok(Some(ObservedRetirementExit { status, graceful }))
        }
        None if observed_at >= deadline => Err(OpenFxHostError::DeadlineExceeded),
        None => Ok(None),
    }
}

#[derive(Debug)]
struct ProcessOwner {
    child: Child,
    state: ProcessState,
    close_trace: close_observation::Trace,
    #[cfg(test)]
    cleanup_hooks: Option<CleanupHooks>,
}

// Per-owner OS refusal only; the real Child and transports remain unchanged.
#[cfg(test)]
#[derive(Debug, Default)]
struct CleanupHooks {
    observations: std::cell::Cell<usize>,
    kills: std::cell::Cell<usize>,
}

/// Admission is revoked before cleanup. Only observed exit establishes settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    Running,
    Retiring,
    Settled,
}
impl ProcessOwner {
    /// The existing sole Child observer has already confirmed native exit.
    /// Diagnostics borrow its retained handle and cannot change settlement.
    fn confirmed_exit(&mut self) {
        self.state = ProcessState::Settled;
        self.close_trace.record_process_exit(&self.child);
    }

    fn check(
        &mut self,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
        deadline: Instant,
    ) -> Result<(), OpenFxHostError> {
        check_owner(owner_checkpoint, deadline)?;
        self.check_liveness()
    }

    fn check_liveness(&mut self) -> Result<(), OpenFxHostError> {
        if self.state != ProcessState::Running {
            return Err(unavailable("native session has been retired"));
        }
        match self.child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => {
                self.confirmed_exit();
                Err(OpenFxHostError::WorkerFailed(format!(
                    "native session exited ({status})"
                )))
            }
            Err(error) => {
                self.state = ProcessState::Retiring;
                Err(unavailable(format!(
                    "native session observation failed (pid={}, kind={:?}, raw_os={:?}): {error}",
                    self.child.id(),
                    error.kind(),
                    error.raw_os_error()
                )))
            }
        }
    }
    fn terminate(&mut self) -> Result<(), OpenFxHostError> {
        #[cfg(test)]
        if let Some(hooks) = self.cleanup_hooks.take() {
            let result = self.terminate_with(
                |_| {
                    hooks.observations.set(hooks.observations.get() + 1);
                    Err(io::Error::from_raw_os_error(87))
                },
                |_| {
                    hooks.kills.set(hooks.kills.get() + 1);
                    Err(io::Error::from_raw_os_error(5))
                },
                |_| panic!("a refused kill must not wait"),
            );
            self.cleanup_hooks = Some(hooks);
            return result;
        }
        self.terminate_with(Child::try_wait, Child::kill, Child::wait)
    }

    // The original operation already failed. Cleanup remains mandatory, but
    // an OS refusal is secondary evidence, never a replacement primary error.
    fn terminate_reported(&mut self, operation: &'static str) {
        if let Err(error) = self.terminate() {
            tracing::warn!(process_id = self.child.id(), state = ?self.state, operation,
                error = %error, "OpenFX secondary cleanup failed");
        }
    }

    fn terminate_with(
        &mut self,
        observe: impl FnMut(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
        kill: impl FnMut(&mut Child) -> io::Result<()>,
        wait: impl FnMut(&mut Child) -> io::Result<std::process::ExitStatus>,
    ) -> Result<(), OpenFxHostError> {
        let mut probe = self.close_trace.cleanup_probe();
        let result = self.terminate_with_probe(observe, kill, wait, &mut probe);
        self.close_trace.publish_cleanup(probe);
        result
    }

    // Only captures the results of these original callbacks. The probe cannot
    // observe another Child, retry a callback, or replace the cleanup result.
    fn terminate_with_probe(
        &mut self,
        mut observe: impl FnMut(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
        mut kill: impl FnMut(&mut Child) -> io::Result<()>,
        mut wait: impl FnMut(&mut Child) -> io::Result<std::process::ExitStatus>,
        probe: &mut close_observation::CleanupProbe,
    ) -> Result<(), OpenFxHostError> {
        use close_observation::CleanupStep;
        if self.state == ProcessState::Settled {
            probe.already_settled();
            return Ok(());
        }
        self.state = ProcessState::Retiring;
        let observation_error =
            match probe.observe(CleanupStep::InitialObservation, || observe(&mut self.child)) {
                Ok(Some(_)) => {
                    self.confirmed_exit();
                    return Ok(());
                }
                Ok(None) => None,
                Err(error) => Some(format!(
                    "failed (kind={:?}, raw_os={:?}): {error}",
                    error.kind(),
                    error.raw_os_error()
                )),
            };
        if let Err(error) = probe.kill(|| kill(&mut self.child)) {
            let final_observation =
                match probe.observe(CleanupStep::FinalObservation, || observe(&mut self.child)) {
                    Ok(Some(_)) => {
                        self.confirmed_exit();
                        return Ok(());
                    }
                    Ok(None) => "post-kill try_wait: child still running".to_owned(),
                    Err(error) => format!(
                        "post-kill try_wait failed (kind={:?}, raw_os={:?}): {error}",
                        error.kind(),
                        error.raw_os_error()
                    ),
                };
            return Err(unavailable(format!(
                "native session termination unconfirmed (pid={}): kill failed (kind={:?}, raw_os={:?}): {error}; {final_observation}; initial try_wait={}",
                self.child.id(), error.kind(), error.raw_os_error(),
                observation_error.as_deref().unwrap_or("child still running")
            )));
        }
        match probe.wait(|| wait(&mut self.child)) {
            Ok(_) => {
                self.confirmed_exit();
                Ok(())
            }
            Err(error) => Err(unavailable(format!(
                "native session reap failed (pid={}, kind={:?}, raw_os={:?}): {error}; initial try_wait={}",
                self.child.id(), error.kind(), error.raw_os_error(),
                observation_error.as_deref().unwrap_or("child still running")
            ))),
        }
    }
}
impl Drop for ProcessOwner {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            tracing::warn!(process_id = self.child.id(), state = ?self.state, error = %error,
                "OpenFX native process cleanup failed");
        }
    }
}

/// One exclusive child and immutable pair of shared Float32 frame ranges.
///
/// Calls are serialized through mutable ownership. Only this owner writes input
/// before a request or reads output after its matching response. Drop attempts
/// cleanup before releasing storage; an unconfirmed OS refusal is diagnosed,
/// never reported as settlement. Native code never runs in the application process.
#[derive(Debug)]
pub struct OpenFxFilterSession {
    process: ProcessOwner,
    control: TcpStream,
    shared: memmap2::MmapMut,
    _file: tempfile::NamedTempFile,
    inspection: OpenFxBinaryInspection,
    width: u32,
    height: u32,
    frame_bytes: usize,
    sequence: u64,
    input_format: mondrian_effects::EffectValueFormat,
    context: OpenFxContext,
    owner: mondrian_core::EffectProcessorOwnerId,
    process_id: std::num::NonZeroU32,
    native_owner: Option<u64>,
    last_invocation: u64,
    persistent_revision: u64,
    progress_generation: u64,
    feedback: Option<mondrian_effects::EffectProcessorFeedback>,
    response_reader: pump::PacketReader,
    request_writer: pump::PacketWriter,
}

/// Owner-thread observations from the sole native request transport.
/// Borrowed questions are never moved to a foreign native callback thread.
#[derive(Debug)]
pub enum OpenFxRenderObservation<'a> {
    /// Poll the canonical owner cancellation/deadline before further work.
    Checkpoint,
    /// Actual coalesced native progress, bound to the current request.
    Progress(mondrian_effects::EffectProcessorLiveProgress),
    /// Publish/sample the same exact currently waiting user service.
    Question(&'a mondrian_effects::EffectProcessorLiveQuestion),
    /// Release only this original service after reply, stop, fault or teardown.
    QuestionEnded(&'a mondrian_effects::EffectProcessorLiveQuestion),
}

struct ActiveQuestion {
    identity: question::QuestionIdentity,
    presented: bool,
    question: mondrian_effects::EffectProcessorLiveQuestion,
}

#[derive(Clone, Copy)]
struct FrameRequest<'a> {
    extent: mondrian_effects::EffectFrameExtent,
    format: mondrian_effects::EffectValueFormat,
    sampling: mondrian_core::ImageSampling,
    input: Option<&'a EffectFloatFrame>,
}
impl<'a> FrameRequest<'a> {
    fn image(frame: &'a EffectFloatFrame) -> Self {
        Self {
            extent: frame.extent(),
            format: frame.format(),
            sampling: frame.sampling(),
            input: Some(frame),
        }
    }
}

struct RenderObservation<'a, P> {
    owner_control: &'a mut dyn FnMut(
        OpenFxRenderObservation<'_>,
    ) -> Result<
        mondrian_effects::EffectProcessorQuestionState,
        OpenFxHostError,
    >,
    gui: mondrian_effects::EffectProcessorGuiAdmission,
    probe: &'a mut P,
}

impl OpenFxFilterSession {
    /// Reconcile disappearance of an actually opened GUI on this same owner.
    /// No image/DSP/stateful render is run, and no replacement instance is made.
    pub(crate) fn close_gui(
        &mut self,
    ) -> Result<Option<(u64, mondrian_effects::EffectProcessorFeedback)>, OpenFxHostError> {
        let deadline = Instant::now()
            .checked_add(DEADLINE)
            .ok_or_else(|| unavailable("native GUI action deadline overflow"))?;
        let result = self.close_gui_inner(deadline);
        if result.is_err() {
            self.process.terminate_reported("close GUI");
        }
        result
    }

    fn close_gui_inner(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<(u64, mondrian_effects::EffectProcessorFeedback)>, OpenFxHostError> {
        self.process.check(&mut || Ok(()), deadline)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| unavailable("native GUI action sequence exhausted"))?;
        let request = Request::CloseGui { owner: self.owner, sequence: self.sequence };
        self.request_writer.write(
            &mut self.control,
            &request,
            deadline,
            &mut || self.process.check(&mut || Ok(()), deadline),
            &mut false,
        )?;
        self.control.set_nonblocking(true).map_err(control_error)?;
        loop {
            self.process.check(&mut || Ok(()), deadline)?;
            match self.response_reader.step::<Response>(&mut self.control)? {
                Some(Response::GuiClosed { owner, sequence, feedback })
                    if owner == self.owner && sequence == self.sequence =>
                {
                    let Some(snapshot) = feedback else {
                        return Ok(None);
                    };
                    if self.native_owner != Some(snapshot.native_owner())
                        || self.last_invocation == 0
                        || snapshot.invocation() != self.last_invocation
                        || snapshot.persistent_revision() < self.persistent_revision
                        || snapshot.progress().is_some_and(|progress| {
                            progress.generation() < self.progress_generation
                        })
                    {
                        return Err(unavailable(
                            "GUI feedback lost its actual native owner/action",
                        ));
                    }
                    self.persistent_revision = snapshot.persistent_revision();
                    if let Some(progress) = snapshot.progress() {
                        self.progress_generation = progress.generation();
                    }
                    publish_native_log_notices(self.owner, self.process_id, &snapshot);
                    // This is an End action, not another completed frame. It
                    // neither advances native Render count nor replaces the
                    // last-render take_feedback slot.
                    return Ok(Some((
                        sequence,
                        mondrian_effects::EffectProcessorFeedback {
                            owner: self.owner,
                            origin:
                                mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild {
                                    process_id: self.process_id,
                                },
                            snapshot,
                        },
                    )));
                }
                Some(_) => return Err(invalid("GUI teardown lost its exact owner/action")),
                None if self.response_reader.made_io_progress() => {}
                None => thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(1)),
                ),
            }
        }
    }

    /// Conservative processor-owned active and retained logical storage.
    ///
    /// Both phases own two shared rasters, one reserved callback stack/guard,
    /// fixed thread/mutex and bounded auxiliary/transport state, and three typed
    /// namespace POD values. Active
    /// admission additionally covers the single pre-DSP output Vec. Success
    /// moves that Vec to the caller; the session retains no output raster owner.
    /// Effects owns and separately charges its source/staging/cache rasters.
    /// Shared physical mapping pages are charged once.
    ///
    /// Executable mappings, SDK descriptor/cache/property allocations, the two
    /// FrameState UID strings and SDK-owned UID copies, OS/CRT bookkeeping,
    /// arbitrary plugin allocations and allocator usable-size overhead are
    /// excluded. Stack reservation is logical, not committed physical RSS. The
    /// output Vec's actual capacity must fit its separate pre-DSP frame grant.
    pub fn storage(
        width: u32,
        height: u32,
    ) -> Result<mondrian_effects::EffectFloatProcessorStorage, OpenFxHostError> {
        Self::storage_for_context(width, height, OpenFxContext::Filter)
    }

    pub(crate) fn storage_for_context(
        width: u32,
        height: u32,
        context: OpenFxContext,
    ) -> Result<mondrian_effects::EffectFloatProcessorStorage, OpenFxHostError> {
        let count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| invalid("frame dimensions overflow"))?;
        let frame_bytes = validate_frame(width, height, count)?;
        let fixed = native_thread_resident_bytes()?
            .checked_add(native_auxiliary_resident_bytes()?)
            .and_then(|bytes| {
                bytes.checked_add(3 * std::mem::size_of::<mondrian_core::EffectProcessorOwnerId>())
            })
            .ok_or_else(|| invalid("native fixed storage overflows"))?;
        let retained = frame_bytes
            .checked_mul(if context == OpenFxContext::Filter {
                2
            } else {
                1
            })
            .and_then(|bytes| bytes.checked_add(fixed))
            .ok_or_else(|| invalid("native retained storage overflows"))?;
        let active = retained
            .checked_add(frame_bytes)
            .ok_or_else(|| invalid("native active storage overflows"))?;
        mondrian_effects::EffectFloatProcessorStorage::new(active, retained)
            .map_err(|error| invalid(error.to_string()))
    }

    /// Prepare a pinned filter and fixed frame extent without loading it in the
    /// application. The child creates its native instance on the first render.
    /// The borrowed owner checkpoint is polled during preparation and every
    /// bounded transport wait; its cancellation/deadline authority is preserved.
    pub fn prepare(
        helper: &Path,
        inspection: &OpenFxBinaryInspection,
        identifier: &str,
        width: u32,
        height: u32,
        input_format: mondrian_effects::EffectValueFormat,
        project_geometry: EffectProjectGeometry,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<Self, OpenFxHostError> {
        Self::prepare_in_context(
            helper,
            inspection,
            identifier,
            width,
            height,
            input_format,
            project_geometry,
            OpenFxContext::Filter,
            owner_checkpoint,
        )
    }

    /// Prepare a true Output-only Generator with explicit output-staging format.
    pub fn prepare_generator(
        helper: &Path,
        inspection: &OpenFxBinaryInspection,
        identifier: &str,
        width: u32,
        height: u32,
        output_format: mondrian_effects::EffectValueFormat,
        project_geometry: EffectProjectGeometry,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<Self, OpenFxHostError> {
        Self::prepare_in_context(
            helper,
            inspection,
            identifier,
            width,
            height,
            output_format,
            project_geometry,
            OpenFxContext::Generator,
            owner_checkpoint,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_in_context(
        helper: &Path,
        inspection: &OpenFxBinaryInspection,
        identifier: &str,
        width: u32,
        height: u32,
        input_format: mondrian_effects::EffectValueFormat,
        project_geometry: EffectProjectGeometry,
        context: OpenFxContext,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<Self, OpenFxHostError> {
        owner_checkpoint()?;
        if input_format.precision() != mondrian_effects::EffectWorkingPrecision::Float32 {
            return Err(invalid("native input format is not Float32"));
        }
        if !helper.is_absolute() || !helper.is_file() {
            return Err(invalid("helper must be an absolute regular file"));
        }
        let count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| invalid("frame dimensions overflow"))?;
        let frame_bytes = validate_frame(width, height, count)?;
        let bundle = verified_selection(inspection, identifier, &mut || owner_checkpoint())?;
        if context == OpenFxContext::Generator
            && (input_format.domain() != mondrian_effects::EffectColorDomain::SceneLinearRgb
                || !mondrian_effects::EffectAlphaContract::SOURCE_FLOAT
                    .admits_source_output(input_format.alpha()))
        {
            return Err(invalid(
                "Generator output-staging association is not admitted",
            ));
        }
        let shared_bytes = frame_bytes
            * if context == OpenFxContext::Filter {
                2
            } else {
                1
            };
        let file = tempfile::NamedTempFile::new()
            .map_err(|error| unavailable(format!("shared frame creation failed: {error}")))?;
        file.as_file()
            .set_len(shared_bytes as u64)
            .map_err(|error| unavailable(format!("shared frame allocation failed: {error}")))?;
        // SAFETY: this exact-sized file is retained by the owner until child reap.
        // Disjoint ranges are accessed exclusively under request/response ownership.
        let shared = unsafe { MmapOptions::new().len(shared_bytes).map_mut(file.as_file()) }
            .map_err(|error| unavailable(format!("shared frame mapping failed: {error}")))?;
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(control_error)?;
        listener.set_nonblocking(true).map_err(control_error)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let mut command = Command::new(helper);
        command
            .arg(OPENFX_RESIDENT_WORKER_ARGUMENT)
            .env(
                ENDPOINT_ENV,
                listener.local_addr().map_err(control_error)?.to_string(),
            )
            .env(NONCE_ENV, &nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        // One retained, fallibly acquired packet owner precedes child/native
        // creation and every DSP request. Partial cursors survive live controls.
        let response_reader = pump::PacketReader::acquire()?;
        let request_writer = pump::PacketWriter::acquire()?;
        let close_trace = close_observation::Trace::parent(&mut command, owner, &nonce)?;
        let mut process = ProcessOwner {
            child: command.spawn().map_err(control_error)?,
            state: ProcessState::Running,
            close_trace,
            #[cfg(test)]
            cleanup_hooks: None,
        };
        process.close_trace.set_child(&process.child);
        let deadline = Instant::now() + DEADLINE;
        let mut control = loop {
            process.check(owner_checkpoint, deadline)?;
            match listener.accept() {
                Ok((stream, address)) if address.ip().is_loopback() => break stream,
                Ok(_) => return Err(unavailable("native session peer is not local")),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                Err(error) => return Err(control_error(error)),
            }
        };
        configure(&control, POLL)?;
        let prepare = Prepare {
            context,
            version: VERSION,
            nonce: nonce.clone(),
            binary_path: inspection.binary_path.clone(),
            bundle_path: bundle,
            binary_sha256: inspection.binary_sha256.clone(),
            plugin_identifier: identifier.to_owned(),
            project_geometry,
            width,
            height,
            shared_path: file.path().to_path_buf(),
            input_alpha: input_format.alpha(),
            owner,
        };
        let mut checkpoint = || process.check(owner_checkpoint, deadline);
        let connected: Response = read_packet(&mut control, &mut checkpoint)?;
        if !matches!(connected, Response::Connected { version: VERSION, nonce: ref value } if value == &nonce)
        {
            return Err(unavailable("native session peer did not authenticate"));
        }
        write_packet(&mut control, &prepare, &mut checkpoint)?;
        let ready: Response = read_packet(&mut control, &mut checkpoint)?;
        if !matches!(ready, Response::Ready { version: VERSION, nonce: ref value } if value == &nonce)
        {
            return Err(unavailable(
                "native session readiness envelope does not match",
            ));
        }
        checkpoint()?;
        let process_id = std::num::NonZeroU32::new(process.child.id())
            .ok_or_else(|| unavailable("native process identity is zero"))?;
        Ok(Self {
            process,
            control,
            shared,
            _file: file,
            inspection: inspection.clone(),
            width,
            height,
            frame_bytes,
            sequence: 0,
            input_format,
            context,
            owner,
            process_id,
            native_owner: None,
            last_invocation: 0,
            persistent_revision: 0,
            progress_generation: 0,
            feedback: None,
            response_reader,
            request_writer,
        })
    }

    /// Parent-issued native instance namespace, stable until actual retirement.
    /// A reprepare/reopen receives a fresh identity even if the OS recycles PID.
    pub const fn processor_owner(&self) -> mondrian_core::EffectProcessorOwnerId {
        self.owner
    }

    /// Child identity for lifecycle diagnostics; it remains stable across frames.
    pub fn worker_process_id(&self) -> u32 {
        self.process.child.id()
    }

    /// Execute one complete authored frame. The one returned raster is acquired
    /// before any request/native processing. A typed host-resource refusal keeps
    /// this valid worker alive; Effects owns its actual optional retention.
    /// Cancellation, deadline, native or
    /// protocol failure retires the worker before an output can be published.
    /// The owner checkpoint can return Canceled or DeadlineExceeded without
    /// creating an adapter-owned token or cancellation thread.
    pub fn render(
        &mut self,
        timing: &OpenFxRenderTiming,
        parameters: &BTreeMap<String, OpenFxParameterBinding>,
        frame: &EffectFloatFrame,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        self.render_observed(
            timing,
            parameters,
            frame,
            None,
            mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
            &mut |_| {
                owner_checkpoint()?;
                Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable)
            },
        )
    }

    /// Execute with actual live native progress on this owning thread.
    /// The callback is never moved to a pump/native callback thread. Its exact
    /// scheduler deadline clips both processing and cooperative cancel grace.
    pub fn render_observed(
        &mut self,
        timing: &OpenFxRenderTiming,
        parameters: &BTreeMap<String, OpenFxParameterBinding>,
        frame: &EffectFloatFrame,
        owner_deadline: Option<Instant>,
        gui: mondrian_effects::EffectProcessorGuiAdmission,
        owner_control: &mut dyn FnMut(
            OpenFxRenderObservation<'_>,
        ) -> Result<
            mondrian_effects::EffectProcessorQuestionState,
            OpenFxHostError,
        >,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        self.render_owned(
            timing,
            parameters,
            FrameRequest::image(frame),
            owner_deadline,
            RenderObservation { owner_control, gui, probe: &mut () },
            reserve_native_output,
        )
    }

    /// Render an Output-only Generator without any incoming image storage.
    /// The caller supplies the actual output sampling. Its PAR is the native
    /// image authority; project geometry remains the immutable owner binding.
    pub fn generate(
        &mut self,
        timing: &OpenFxRenderTiming,
        sampling: mondrian_core::ImageSampling,
        parameters: &BTreeMap<String, OpenFxParameterBinding>,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        self.generate_observed(
            timing,
            sampling,
            parameters,
            None,
            mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
            &mut |_| {
                owner_checkpoint()?;
                Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable)
            },
        )
    }

    pub(crate) fn generate_observed(
        &mut self,
        timing: &OpenFxRenderTiming,
        sampling: mondrian_core::ImageSampling,
        parameters: &BTreeMap<String, OpenFxParameterBinding>,
        owner_deadline: Option<Instant>,
        gui: mondrian_effects::EffectProcessorGuiAdmission,
        owner_control: &mut dyn FnMut(
            OpenFxRenderObservation<'_>,
        ) -> Result<
            mondrian_effects::EffectProcessorQuestionState,
            OpenFxHostError,
        >,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        self.render_owned(
            timing,
            parameters,
            FrameRequest {
                extent: mondrian_effects::EffectFrameExtent::new(self.width, self.height),
                format: self.input_format,
                sampling,
                input: None,
            },
            owner_deadline,
            RenderObservation { owner_control, gui, probe: &mut () },
            reserve_native_output,
        )
    }

    /// Attribute parent phases of the same complete owned render path.
    /// This validation-only report owns no work/cancellation authority. Normal
    /// render uses a zero-sized no-op probe and performs no diagnostic clocks.
    /// Instrumented timing is diagnosis, never performance acceptance.
    #[cfg(any(test, feature = "validation"))]
    pub fn render_diagnostic(
        &mut self,
        timing: &OpenFxRenderTiming,
        parameters: &BTreeMap<String, OpenFxParameterBinding>,
        frame: &EffectFloatFrame,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
        report: &mut OpenFxRenderDiagnostic,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        *report = OpenFxRenderDiagnostic::default();
        report.owner = Some(self.owner);
        report.worker_process_id = Some(self.process_id);
        let begin = Instant::now();
        let result = self.render_owned(
            timing,
            parameters,
            FrameRequest::image(frame),
            None,
            RenderObservation {
                owner_control: &mut |_| {
                    owner_checkpoint()?;
                    Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable)
                },
                gui: mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
                probe: &mut *report,
            },
            reserve_native_output,
        );
        report.owned_invocation_elapsed = Some(begin.elapsed());
        report.accepted_output = result.is_ok();
        result
    }

    #[cfg(test)]
    fn render_with_output_reservation(
        &mut self,
        timing: &OpenFxRenderTiming,
        parameters: &BTreeMap<String, OpenFxParameterBinding>,
        frame: &EffectFloatFrame,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
        reserve: impl FnOnce(&mut Vec<[f32; 4]>, usize) -> Result<(), std::collections::TryReserveError>,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        self.render_owned(
            timing,
            parameters,
            FrameRequest::image(frame),
            None,
            RenderObservation {
                owner_control: &mut |_| {
                    owner_checkpoint()?;
                    Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable)
                },
                gui: mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
                probe: &mut (),
            },
            reserve,
        )
    }

    fn render_owned(
        &mut self,
        timing: &OpenFxRenderTiming,
        parameters: &BTreeMap<String, OpenFxParameterBinding>,
        frame: FrameRequest<'_>,
        owner_deadline: Option<Instant>,
        observation: RenderObservation<'_, impl ParentProbe>,
        reserve: impl FnOnce(&mut Vec<[f32; 4]>, usize) -> Result<(), std::collections::TryReserveError>,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        let RenderObservation { owner_control, gui, probe } = observation;
        self.feedback = None;
        let native_deadline = Instant::now() + DEADLINE;
        let mut budget =
            question_budget::QuestionExecutionBudget::new(native_deadline, owner_deadline);
        let deadline = budget
            .deadline()
            .ok_or_else(|| unavailable("new native work has no active budget"))?;
        let mut owner_checkpoint =
            || owner_control(OpenFxRenderObservation::Checkpoint).map(|_| ());
        let phase = probe.start();
        let admission = (|| {
            self.process.check(&mut owner_checkpoint, deadline)?;
            if frame.extent.width() != self.width
                || frame.extent.height() != self.height
                || frame.format != self.input_format
            {
                return Err(invalid("frame extent differs from the native session"));
            }
            validate_native_sampling(self.width, frame.sampling)?;
            match (self.context, frame.input) {
                (OpenFxContext::Filter, Some(input)) => {
                    validate_render_request(timing, parameters, input, &mut || {
                        self.process.check(&mut owner_checkpoint, deadline)
                    })
                }
                (OpenFxContext::Generator, None) => {
                    validate_timing(timing)?;
                    validate_parameters(parameters)?;
                    self.process.check(&mut owner_checkpoint, deadline)
                }
                _ => Err(invalid(
                    "frame input role differs from the selected native context",
                )),
            }
        })();
        probe.finish(OpenFxRenderPhase::InputAdmission, phase);
        if let Err(error) = admission {
            // An observed owner stop retires this invocation's instance even
            // before submission. Invalid authored inputs remain recoverable;
            // they have not changed the native session or its request serial.
            if matches!(
                error,
                OpenFxHostError::Canceled | OpenFxHostError::DeadlineExceeded
            ) {
                self.process.terminate_reported("render admission");
            }
            return Err(error);
        }
        let phase = probe.start();
        let allocation = {
            let mut checkpoint = || self.process.check(&mut owner_checkpoint, deadline);
            NativeOutputAllocation::acquire(frame.extent, &mut checkpoint, reserve)
        };
        probe.finish(OpenFxRenderPhase::OutputAllocation, phase);
        let output_owner = match allocation {
            Ok(owner) => owner,
            Err(error @ OpenFxHostError::HostResourceRefused(_)) => return Err(error),
            Err(error) => {
                self.process.terminate_reported("render output allocation");
                return Err(error);
            }
        };
        let result = self.render_inner(
            timing,
            parameters,
            frame,
            RenderObservation { owner_control, gui, probe },
            &mut budget,
            output_owner,
        );
        if result
            .as_ref()
            .is_err_and(|error| !matches!(error, OpenFxHostError::UserBlocked(_)))
        {
            self.process.terminate_reported("render");
        }
        result
    }

    fn render_inner(
        &mut self,
        timing: &OpenFxRenderTiming,
        parameters: &BTreeMap<String, OpenFxParameterBinding>,
        frame: FrameRequest<'_>,
        observation: RenderObservation<'_, impl ParentProbe>,
        budget: &mut question_budget::QuestionExecutionBudget,
        output_owner: NativeOutputAllocation,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        let RenderObservation { owner_control, gui, probe } = observation;
        let deadline = budget
            .deadline()
            .ok_or_else(|| unavailable("unsubmitted native work has no active budget"))?;
        self.process.check(
            &mut || owner_control(OpenFxRenderObservation::Checkpoint).map(|_| ()),
            deadline,
        )?;
        let phase = probe.start();
        let identity = sha256_file(&self.inspection.binary_path);
        probe.finish(OpenFxRenderPhase::IdentityBefore, phase);
        if identity? != self.inspection.binary_sha256 {
            return Err(invalid("OpenFX binary changed before render"));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| unavailable("native session sequence exhausted"))?;
        probe.event(ProbeEvent::Sequence(self.sequence));
        let mut copy_checkpoint = || {
            check_owner(
                &mut || owner_control(OpenFxRenderObservation::Checkpoint).map(|_| ()),
                deadline,
            )
        };
        let phase = probe.start();
        let copied = if let Some(input) = frame.input {
            copy_native_input(
                &mut self.shared[..self.frame_bytes],
                input.pixels(),
                &mut copy_checkpoint,
            )
        } else {
            Ok(())
        };
        probe.finish(OpenFxRenderPhase::InputCopy, phase);
        copied?;
        probe.event(ProbeEvent::InputCopied(if frame.input.is_some() {
            self.frame_bytes
        } else {
            0
        }));
        let phase = probe.start();
        let request = Request::Render {
            owner: self.owner,
            sequence: self.sequence,
            gui_open: gui.is_open(),
            interactive_render: gui.is_user_edited(),
            question_work: gui.allows_question(),
            timing: timing.clone(),
            sampling: frame.sampling,
            parameters: parameters.clone(),
            diagnostic: probe.measure_child(),
        };
        let mut submitted = false;
        let submission = self.request_writer.write(
            &mut self.control,
            &request,
            deadline,
            &mut || {
                self.process.check(
                    &mut || owner_control(OpenFxRenderObservation::Checkpoint).map(|_| ()),
                    deadline,
                )
            },
            &mut submitted,
        );
        probe.finish(OpenFxRenderPhase::RequestSubmission, phase);
        if submitted {
            probe.event(ProbeEvent::RequestCommitted);
        }
        let initial_stop = match submission {
            Ok(()) => None,
            Err(OpenFxHostError::Canceled) if submitted => Some(OpenFxHostError::Canceled),
            Err(error) => return Err(error), // Partial packet can only be retired.
        };
        let phase = probe.start();
        let response = self.await_render(owner_control, budget, gui, initial_stop, probe);
        probe.finish(OpenFxRenderPhase::AwaitCompletion, phase);
        let response = response?;
        let deadline = budget
            .deadline()
            .ok_or_else(|| unavailable("native completed with an unreleased human wait"))?;
        let phase = probe.start();
        let mut checkpoint = || {
            self.process.check(
                &mut || owner_control(OpenFxRenderObservation::Checkpoint).map(|_| ()),
                deadline,
            )
        };
        let (alpha, snapshot) = match response {
            Response::Rendered { sequence, alpha, feedback, diagnostic }
                if sequence == self.sequence
                    && matches!(
                        alpha,
                        mondrian_core::ColorFrameAlpha::StraightFloat
                            | mondrian_core::ColorFrameAlpha::PremultipliedFloat
                            | mondrian_core::ColorFrameAlpha::Opaque
                    )
                    && !feedback.is_blocked() =>
            {
                admit_child_diagnostic(
                    diagnostic,
                    self.owner,
                    self.sequence,
                    &feedback,
                    Some(alpha),
                    self.frame_bytes,
                    probe,
                )?;
                (Some(alpha), feedback)
            }
            Response::UserBlocked { sequence, feedback, diagnostic }
                if sequence == self.sequence && feedback.is_blocked() =>
            {
                admit_child_diagnostic(
                    diagnostic,
                    self.owner,
                    self.sequence,
                    &feedback,
                    None,
                    self.frame_bytes,
                    probe,
                )?;
                (None, feedback)
            }
            Response::Rejected { sequence, status } if sequence == self.sequence => {
                return Err(OpenFxHostError::WorkerFailed(format!(
                    "native status {status}"
                )))
            }
            _ => {
                return Err(unavailable(
                    "native session response sequence/disposition does not match",
                ))
            }
        };
        checkpoint()?;
        probe.finish(OpenFxRenderPhase::FeedbackAdmission, phase);
        let phase = probe.start();
        let identity = sha256_file(&self.inspection.binary_path);
        probe.finish(OpenFxRenderPhase::IdentityAfter, phase);
        if identity? != self.inspection.binary_sha256 {
            return Err(invalid("OpenFX binary changed during render"));
        }
        let phase = probe.start();
        if self.native_owner.is_some_and(|owner| owner != snapshot.native_owner())
            || snapshot.invocation()
                != self
                    .last_invocation
                    .checked_add(1)
                    .ok_or_else(|| unavailable("native Render invocation exhausted"))?
            || snapshot.invocation() <= self.last_invocation
            || snapshot.persistent_revision() < self.persistent_revision
            || snapshot
                .progress()
                .is_some_and(|progress| progress.generation() < self.progress_generation)
        {
            return Err(unavailable(
                "native feedback owner or revision does not match",
            ));
        }
        self.native_owner = Some(snapshot.native_owner());
        self.last_invocation = snapshot.invocation();
        self.persistent_revision = snapshot.persistent_revision();
        if let Some(progress) = snapshot.progress() {
            self.progress_generation = progress.generation();
        }
        // Log is a diagnostic contract, not a user popup or frame-delivery
        // claim. Both Viewer and export share this exact trusted supervisor.
        // The response's work, owner/revision and binary checks above have
        // completed before any unmodified plugin text reaches product tracing.
        publish_native_log_notices(self.owner, self.process_id, &snapshot);
        let feedback = mondrian_effects::EffectProcessorFeedback {
            owner: self.owner,
            origin: mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild {
                process_id: self.process_id,
            },
            snapshot,
        };
        let Some(alpha) = alpha else {
            probe.finish(OpenFxRenderPhase::FeedbackAdmission, phase);
            return Err(OpenFxHostError::UserBlocked(Box::new(feedback)));
        };
        self.feedback = Some(feedback);
        probe.finish(OpenFxRenderPhase::FeedbackAdmission, phase);
        probe.event(ProbeEvent::OutputAlpha(alpha));
        let format = frame.format.with_alpha(alpha);
        let output = materialize_native_output_profiled(
            &self.shared[if self.context == OpenFxContext::Filter {
                self.frame_bytes
            } else {
                0
            }..],
            output_owner,
            format,
            frame.sampling,
            &mut checkpoint,
            probe,
        )?;
        let phase = probe.start();
        let publication = checkpoint();
        probe.finish(OpenFxRenderPhase::FinalPublication, phase);
        publication?;
        Ok(output)
    }

    fn await_render(
        &mut self,
        owner_control: &mut dyn FnMut(
            OpenFxRenderObservation<'_>,
        ) -> Result<
            mondrian_effects::EffectProcessorQuestionState,
            OpenFxHostError,
        >,
        budget: &mut question_budget::QuestionExecutionBudget,
        gui: mondrian_effects::EffectProcessorGuiAdmission,
        mut stopped: Option<OpenFxHostError>,
        probe: &mut impl ParentProbe,
    ) -> Result<Response, OpenFxHostError> {
        use mondrian_effects::{
            EffectProcessorQuestionReply as Reply, EffectProcessorQuestionState as State,
        };
        self.control.set_nonblocking(true).map_err(control_error)?;
        let mut cancel_sent = false;
        let mut drain_deadline = None;
        let mut progress_revision = 0;
        let mut last_service = 0;
        let render_invocation = self
            .last_invocation
            .checked_add(1)
            .ok_or_else(|| unavailable("native Render invocation exhausted"))?;
        let mut active: Option<ActiveQuestion> = None;
        // Every exit clears only its original GUI service, including protocol,
        // native, cancellation and timeout errors. No borrowed native data is
        // retained in the product and no completed DSP invocation is replayed.
        let result = (|| loop {
            if stopped.is_none() {
                match owner_control(OpenFxRenderObservation::Checkpoint) {
                    Ok(_) => {}
                    Err(error @ OpenFxHostError::Canceled) => stopped = Some(error),
                    Err(error) => return Err(error),
                }
                self.process.check_liveness()?;
                if stopped.is_none() {
                    budget.check(Instant::now()).map_err(|_| OpenFxHostError::DeadlineExceeded)?;
                }
            }
            if stopped.is_some() && !cancel_sent {
                let deadline = budget
                    .cancellation_deadline(Instant::now(), RETIRE)
                    .map_err(|_| OpenFxHostError::DeadlineExceeded)?;
                drain_deadline = Some(deadline);
                let request = Request::Cancel { owner: self.owner, sequence: self.sequence };
                let result = self.request_writer.write(
                    &mut self.control,
                    &request,
                    deadline,
                    &mut || {
                        if Instant::now() >= deadline {
                            Err(OpenFxHostError::DeadlineExceeded)
                        } else {
                            Ok(())
                        }
                    },
                    &mut false,
                );
                if result.is_err() {
                    return Err(stopped
                        .take()
                        .ok_or_else(|| unavailable("cancel lost stop reason"))?);
                }
                cancel_sent = true;
            }
            let deadline = drain_deadline.or_else(|| budget.deadline());
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(stopped.take().unwrap_or(OpenFxHostError::DeadlineExceeded));
            }
            if stopped.is_none()
                && let Some(question) = active.as_mut()
            {
                match owner_control(OpenFxRenderObservation::Question(&question.question)) {
                    Err(error @ OpenFxHostError::Canceled) => {
                        stopped = Some(error);
                        continue;
                    }
                    Err(error) => return Err(error),
                    Ok(State::Pending) => {}
                    Ok(State::Presented) => {
                        if !question.presented {
                            budget
                                .presented(question.identity, Instant::now())
                                .map_err(|_| OpenFxHostError::DeadlineExceeded)?;
                            question.presented = true;
                        }
                    }
                    Ok(state @ (State::Unavailable | State::Answered(_))) => {
                        let reply = match state {
                            State::Answered(Reply::Yes) => question::QuestionReply::Yes,
                            State::Answered(Reply::No) => question::QuestionReply::No,
                            _ => question::QuestionReply::Dismissed,
                        };
                        // A Yes/No without an actually painted original modal
                        // is not a real user reply and cannot unblock native.
                        let reply =
                            if !question.presented && reply != question::QuestionReply::Dismissed {
                                // The suite cannot invent a user answer. A product
                                // response without actual presentation is Failed;
                                // the plugin may legally continue its Render.
                                question::QuestionReply::Dismissed
                            } else {
                                reply
                            };
                        if question.presented {
                            budget
                                .answered(question.identity, Instant::now())
                                .map_err(|_| OpenFxHostError::DeadlineExceeded)?;
                        }
                        let deadline = budget.deadline().ok_or_else(|| {
                            unavailable("Question reply did not resume its active budget")
                        })?;
                        let request = Request::QuestionReply {
                            owner: self.owner,
                            sequence: self.sequence,
                            identity: question.identity,
                            reply,
                        };
                        self.request_writer.write(
                            &mut self.control,
                            &request,
                            deadline,
                            &mut || {
                                self.process.check(
                                    &mut || {
                                        owner_control(OpenFxRenderObservation::Checkpoint)
                                            .map(|_| ())
                                    },
                                    deadline,
                                )
                            },
                            &mut false,
                        )?;
                        let original = active
                            .take()
                            .ok_or_else(|| unavailable("Question reply lost its service"))?;
                        let _ = owner_control(OpenFxRenderObservation::QuestionEnded(
                            &original.question,
                        ));
                        continue;
                    }
                }
            }
            probe.event(ProbeEvent::PollStep);
            let response = match self.response_reader.step::<Response>(&mut self.control) {
                Ok(Some(response)) => response,
                Ok(None) => {
                    if self.response_reader.made_io_progress() {
                        probe.event(ProbeEvent::ReadMadeProgress);
                        continue;
                    }
                    let wait = deadline.map_or(Duration::from_millis(1), |deadline| {
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(1))
                    });
                    if !wait.is_zero() {
                        let start = probe.start();
                        thread::sleep(wait);
                        probe.slept(wait, start);
                    }
                    continue;
                }
                Err(error) => return Err(stopped.take().unwrap_or(error)),
            };
            if let Response::Question { owner, sequence, identity, text } = response {
                if owner != self.owner
                    || sequence != self.sequence
                    || identity.invocation != render_invocation
                    || identity.native_owner == 0
                    || identity.service <= last_service
                    || self.native_owner.is_some_and(|old| old != identity.native_owner)
                    || !gui.is_open()
                    || !gui.allows_question()
                    || text.kind() != mondrian_effects::EffectProcessorNoticeKind::Information
                    || active.is_some()
                {
                    return Err(unavailable(
                        "Question lost its exact current GUI/native work identity",
                    ));
                }
                last_service = identity.service;
                self.native_owner = Some(identity.native_owner);
                if stopped.is_some() {
                    continue;
                }
                let id = mondrian_effects::EffectProcessorQuestionId::new(
                    identity.native_owner,
                    identity.invocation,
                    identity.service,
                )
                .ok_or_else(|| unavailable("Question service coordinates are invalid"))?;
                active = Some(ActiveQuestion {
                    identity,
                    presented: false,
                    question: mondrian_effects::EffectProcessorLiveQuestion {
                        owner: self.owner,
                        origin: mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild {
                            process_id: self.process_id,
                        },
                        id,
                        text,
                    },
                });
                continue;
            }
            if let Response::Progress { owner, sequence, revision, native_owner, progress } =
                response
            {
                if owner != self.owner
                    || sequence != self.sequence
                    || revision <= progress_revision
                    || native_owner == 0
                    || self.native_owner.is_some_and(|old| old != native_owner)
                    || progress.generation() < self.progress_generation
                {
                    return Err(unavailable(
                        "live progress lost its exact native work identity",
                    ));
                }
                probe.event(ProbeEvent::ProgressPacket);
                progress_revision = revision;
                self.progress_generation = progress.generation();
                self.native_owner = Some(native_owner);
                if stopped.is_none() {
                    match owner_control(OpenFxRenderObservation::Checkpoint) {
                        Ok(_) => {}
                        Err(error @ OpenFxHostError::Canceled) => stopped = Some(error),
                        Err(error) => return Err(error),
                    }
                }
                if stopped.is_none() {
                    let progress = mondrian_effects::EffectProcessorLiveProgress {
                        owner: self.owner,
                        origin: mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild {
                            process_id: self.process_id,
                        },
                        native_owner,
                        invocation: render_invocation,
                        revision,
                        progress,
                    };
                    match owner_control(OpenFxRenderObservation::Progress(progress)) {
                        Ok(_) => {}
                        Err(error @ OpenFxHostError::Canceled) => stopped = Some(error),
                        Err(error) => return Err(error),
                    }
                }
                continue;
            }
            if let Some(reason) = stopped.take() {
                match response {
                    Response::Canceled { owner, sequence }
                        if owner == self.owner && sequence == self.sequence => {}
                    Response::Rendered { sequence, .. }
                    | Response::Rejected { sequence, .. }
                    | Response::UserBlocked { sequence, .. }
                        if sequence == self.sequence => {}
                    _ => return Err(unavailable("cancel completion lost its current work")),
                }
                return Err(reason);
            }
            if active.is_some() {
                return Err(unavailable(
                    "native completed while its Question was unacknowledged",
                ));
            }
            return Ok(response);
        })();
        if let Some(original) = active.take() {
            let _ = owner_control(OpenFxRenderObservation::QuestionEnded(&original.question));
        }
        result
    }

    /// Consume the exact feedback accompanying the last accepted output.
    pub fn take_feedback(&mut self) -> Option<mondrian_effects::EffectProcessorFeedback> {
        self.feedback.take()
    }

    /// Retire the native instance gracefully under a short deadline, then verify
    /// child exit. Cleanup failures retain a non-reusable owner for retry.
    pub fn shutdown(&mut self) -> Result<(), OpenFxHostError> {
        match self.process.state {
            ProcessState::Settled => return Ok(()),
            ProcessState::Retiring => return self.process.terminate(),
            ProcessState::Running => {}
        }
        let deadline = Instant::now() + RETIRE;
        self.process.close_trace.record(close_observation::Stage::ParentBegin, 0);
        let mut exit_queries = self.process.close_trace.exit_query_probe();
        let graceful = (|| {
            let mut checkpoint = || {
                if Instant::now() >= deadline {
                    return Err(OpenFxHostError::DeadlineExceeded);
                }
                Ok(())
            };
            self.process.close_trace.record(close_observation::Stage::ParentSendBegin, 0);
            let mut committed = false;
            let send = self.request_writer.write(
                &mut self.control,
                &Request::Shutdown {},
                deadline,
                &mut checkpoint,
                &mut committed,
            );
            // The existing writer sets this before its final deadline check.
            // Preserve actual complete-send evidence even if that check fails.
            if committed {
                self.process
                    .close_trace
                    .record(close_observation::Stage::ParentSendCommittedObserved, 0);
            }
            send?;
            let response = loop {
                checkpoint()?;
                if let Some(response) = self.response_reader.step::<Response>(&mut self.control)? {
                    break response;
                }
                if self.response_reader.made_io_progress() {
                    continue;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    thread::sleep(remaining.min(Duration::from_millis(1)));
                }
            };
            if !matches!(response, Response::Closed {}) {
                return Err(unavailable("native session did not close"));
            }
            self.process
                .close_trace
                .record(close_observation::Stage::ParentClosedObserved, 0);
            loop {
                if let Some(observed) = observe_retirement_exit(deadline, Instant::now, || {
                    #[cfg(windows)]
                    {
                        retirement_wait::wait_for_exit(
                            &mut self.process.child,
                            deadline.saturating_duration_since(Instant::now()),
                            &mut exit_queries,
                        )
                    }
                    #[cfg(not(windows))]
                    {
                        exit_queries.observe(|| self.process.child.try_wait())
                    }
                })? {
                    self.process.close_trace.record(
                        close_observation::Stage::ParentOsExitObserved,
                        observed.status.code().unwrap_or(-1) as u32,
                    );
                    self.process.confirmed_exit();
                    return observed.graceful;
                }
                #[cfg(not(windows))]
                thread::sleep(Duration::from_millis(1));
            }
        })();
        self.process.close_trace.record(
            close_observation::Stage::ParentGracefulEnd,
            close_observation::result_code(&graceful),
        );
        self.process.close_trace.publish_exit_queries(exit_queries);
        self.process
            .close_trace
            .record(close_observation::Stage::ParentSettlementBegin, 0);
        let settlement = self.process.terminate();
        self.process.close_trace.record(
            close_observation::Stage::ParentSettlementEnd,
            close_observation::result_code(&settlement),
        );
        match graceful {
            Err(primary) => {
                if let Err(error) = settlement {
                    tracing::warn!(process_id = self.process.child.id(), state = ?self.process.state,
                        error = %error, "OpenFX shutdown secondary cleanup failed");
                }
                Err(primary)
            }
            Ok(()) => settlement,
        }
    }
}
impl Drop for OpenFxFilterSession {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            tracing::warn!(process_id = self.process.child.id(), state = ?self.process.state,
                error = %error, "OpenFX session shutdown failed");
        }
    }
}

// The existing product subscriber writes rotated bounded JSONL and stderr,
// never the child control transport/stdout. Its ordinary filtering/backpressure
// policy applies; the native host journal is not a crash-durability guarantee.
fn publish_native_log_notices(
    owner: mondrian_core::EffectProcessorOwnerId,
    process_id: std::num::NonZeroU32,
    snapshot: &mondrian_effects::EffectProcessorFeedbackSnapshot,
) {
    for notice in snapshot.notices() {
        if notice.kind() == mondrian_effects::EffectProcessorNoticeKind::Log {
            tracing::info!(
                target: "mondrian.openfx.plugin",
                processor_owner = %owner,
                worker_process_id = process_id.get(),
                native_invocation = snapshot.invocation(),
                notice_id = notice.id(),
                plugin_message = notice.text(),
                "OpenFX plugin log"
            );
        }
    }
}

// Raw transport owns no color interpretation. The parent's admitted frame is
// immutable during this borrow; the native worker is idle until its request.
fn copy_native_input(
    destination: &mut [u8],
    pixels: &[[f32; 4]],
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<(), OpenFxHostError> {
    let source: &[u8] = bytemuck::cast_slice(pixels);
    if destination.len() != source.len() {
        return Err(invalid(
            "shared input range does not match admitted frame storage",
        ));
    }
    for (destination, source) in destination
        .chunks_mut(FRAME_TRANSFER_BYTES)
        .zip(source.chunks(FRAME_TRANSFER_BYTES))
    {
        checkpoint()?;
        destination.copy_from_slice(source);
    }
    checkpoint()
}

// One parent-owned raster acquired before a native request can advance DSP.
// It is still uninitialized storage: there is no frame/association authority
// until the matching completion fills and admits the complete signal.
#[derive(Debug)]
struct NativeOutputAllocation {
    pixels: Vec<[f32; 4]>,
    extent: mondrian_effects::EffectFrameExtent,
    count: usize,
    bytes: usize,
}

fn reserve_native_output(
    pixels: &mut Vec<[f32; 4]>,
    count: usize,
) -> Result<(), std::collections::TryReserveError> {
    pixels.try_reserve_exact(count)
}

impl NativeOutputAllocation {
    fn acquire(
        extent: mondrian_effects::EffectFrameExtent,
        checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
        reserve: impl FnOnce(&mut Vec<[f32; 4]>, usize) -> Result<(), std::collections::TryReserveError>,
    ) -> Result<Self, OpenFxHostError> {
        let count = (extent.width() as usize)
            .checked_mul(extent.height() as usize)
            .ok_or_else(|| invalid("native output dimensions overflow"))?;
        let bytes = validate_frame(extent.width(), extent.height(), count)?;
        checkpoint()?;
        let mut pixels = Vec::new();
        let acquired = reserve(&mut pixels, count);
        // An owner stop observed during allocation wins over resource refusal.
        checkpoint()?;
        acquired.map_err(|_| {
            OpenFxHostError::HostResourceRefused(
                mondrian_effects::EffectProcessorHostResourceRefusal::OutputAllocation {
                    required_bytes: bytes,
                },
            )
        })?;
        if !pixels.is_empty() || pixels.capacity() < count {
            return Err(invalid(
                "native output reservation did not acquire empty complete storage",
            ));
        }
        // reserve_exact need not promise equality in the public Vec contract.
        // Observe the actual logical owner, not allocator usable-size/RSS, and
        // refuse before DSP if it exceeds the already admitted output charge.
        let capacity_bytes = pixels
            .capacity()
            .checked_mul(std::mem::size_of::<[f32; 4]>())
            .ok_or_else(|| invalid("native output capacity byte size overflows"))?;
        if capacity_bytes > bytes {
            return Err(OpenFxHostError::HostResourceRefused(
                mondrian_effects::EffectProcessorHostResourceRefusal::OutputCapacity {
                    required_bytes: capacity_bytes,
                    admitted_bytes: bytes,
                },
            ));
        }
        Ok(Self { pixels, extent, count, bytes })
    }
}

// Fill the already acquired owner only after proved native completion. Copying
// bytes into spare capacity creates no foreign f32 reference and requires no
// source alignment. The sole Effects admission remains the numeric boundary.
#[cfg(test)]
fn materialize_native_output(
    source: &[u8],
    owner: NativeOutputAllocation,
    format: mondrian_effects::EffectValueFormat,
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<EffectFloatFrame, OpenFxHostError> {
    materialize_native_output_profiled(
        source,
        owner,
        format,
        mondrian_core::ImageSampling::UNIT_SQUARE,
        checkpoint,
        &mut (),
    )
}

fn materialize_native_output_profiled(
    source: &[u8],
    owner: NativeOutputAllocation,
    format: mondrian_effects::EffectValueFormat,
    sampling: mondrian_core::ImageSampling,
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    probe: &mut impl ParentProbe,
) -> Result<EffectFloatFrame, OpenFxHostError> {
    let phase = probe.start();
    let NativeOutputAllocation { mut pixels, extent, count, bytes } = owner;
    if source.len() != bytes {
        return Err(invalid(
            "shared output range does not match admitted frame storage",
        ));
    }
    checkpoint()?;
    let destination = pixels.spare_capacity_mut().as_mut_ptr().cast::<u8>();
    let mut offset = 0usize;
    for block in source.chunks(FRAME_TRANSFER_BYTES) {
        checkpoint()?;
        // SAFETY: reserve acquired at least count aligned RGBA slots, whose
        // combined byte size was checked above. These consecutive blocks cover
        // only that range. Destination is a new exclusive allocation, never
        // overlaps the borrowed shared source. Length remains zero until every
        // slot is initialized, so a cancellation drops no uninitialized value.
        unsafe {
            std::ptr::copy_nonoverlapping(block.as_ptr(), destination.add(offset), block.len());
        }
        offset += block.len();
        probe.event(ProbeEvent::OutputCopied(block.len()));
    }
    checkpoint()?;
    // SAFETY: the entire checked RGBA range is now initialized. f32 permits all
    // bit patterns; frame validation below is the numeric/association boundary.
    unsafe {
        pixels.set_len(count);
    }
    probe.finish(OpenFxRenderPhase::OutputCopy, phase);
    if format.alpha() == mondrian_core::ColorFrameAlpha::Opaque {
        // OFX ignores the stored A lane for opaque images. The sole public
        // Opaque representation owns A=1. Dispatch once for the complete frame,
        // preserve all RGB bits, and keep bounded owner checkpoints.
        let phase = probe.start();
        for block in pixels.chunks_mut(FRAME_TRANSFER_PIXELS) {
            checkpoint()?;
            let count = block.len();
            for pixel in block {
                pixel[3] = 1.0;
            }
            probe.event(ProbeEvent::OpaquePixels(count));
        }
        probe.finish(OpenFxRenderPhase::OpaqueCanonicalization, phase);
    }
    let phase = probe.start();
    let admitted =
        EffectFloatFrame::from_pixels_controlled(extent, format, sampling, pixels, &mut || {
            checkpoint()
        });
    probe.finish(OpenFxRenderPhase::OutputAdmission, phase);
    admitted
}

fn configure(stream: &TcpStream, timeout: Duration) -> Result<(), OpenFxHostError> {
    stream.set_nodelay(true).map_err(control_error)?;
    stream.set_read_timeout(Some(timeout)).map_err(control_error)?;
    stream.set_write_timeout(Some(timeout)).map_err(control_error)
}
fn control_error(error: io::Error) -> OpenFxHostError {
    unavailable(format!("native session control failed: {error}"))
}
fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn read_exact_controlled(
    stream: &mut TcpStream,
    mut bytes: &mut [u8],
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<(), OpenFxHostError> {
    while !bytes.is_empty() {
        checkpoint()?;
        match stream.read(bytes) {
            Ok(0) => return Err(unavailable("native session control closed mid-packet")),
            Ok(count) => {
                bytes = &mut bytes[count..];
            }
            Err(error) if retry(&error) => {
                if error.kind() == io::ErrorKind::WouldBlock {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Err(error) => return Err(control_error(error)),
        }
    }
    checkpoint()
}
fn read_packet<T: DeserializeOwned>(
    stream: &mut TcpStream,
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<T, OpenFxHostError> {
    let mut header = [0; 4];
    read_exact_controlled(stream, &mut header, checkpoint)?;
    read_body(stream, header, checkpoint)
}
fn read_body<T: DeserializeOwned>(
    stream: &mut TcpStream,
    header: [u8; 4],
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<T, OpenFxHostError> {
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_WORKER_REQUEST_BYTES {
        return Err(unavailable("native session packet exceeds its bound"));
    }
    let mut bytes = vec![0; length];
    read_exact_controlled(stream, &mut bytes, checkpoint)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| unavailable(format!("native session packet is malformed: {error}")))
}
fn write_packet<T: Serialize>(
    stream: &mut TcpStream,
    value: &T,
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<(), OpenFxHostError> {
    write_packet_committed(stream, value, checkpoint, &mut false)
}

fn write_packet_committed<T: Serialize>(
    stream: &mut TcpStream,
    value: &T,
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    committed: &mut bool,
) -> Result<(), OpenFxHostError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| invalid(format!("native session packet encoding failed: {error}")))?;
    if bytes.is_empty() || bytes.len() > MAX_WORKER_REQUEST_BYTES {
        return Err(invalid("native session packet exceeds its bound"));
    }
    let mut packet = Vec::with_capacity(bytes.len() + 4);
    packet.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    packet.extend_from_slice(&bytes);
    let mut remaining = packet.as_slice();
    while !remaining.is_empty() {
        checkpoint()?;
        match stream.write(remaining) {
            Ok(0) => return Err(unavailable("native session control closed mid-packet")),
            Ok(count) => remaining = &remaining[count..],
            Err(error) if retry(&error) => {
                if error.kind() == io::ErrorKind::WouldBlock {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Err(error) => return Err(control_error(error)),
        }
    }
    *committed = true;
    checkpoint()
}

fn map_frames(
    prepare: &Prepare,
) -> Result<(fs::File, Option<memmap2::Mmap>, memmap2::MmapMut), OpenFxHostError> {
    let count = (prepare.width as usize)
        .checked_mul(prepare.height as usize)
        .ok_or_else(|| invalid("frame dimensions overflow"))?;
    let bytes = validate_frame(prepare.width, prepare.height, count)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&prepare.shared_path)
        .map_err(control_error)?;
    let metadata = file.metadata().map_err(control_error)?;
    let has_input = prepare.context == OpenFxContext::Filter;
    if !metadata.is_file() || metadata.len() != (bytes * if has_input { 2 } else { 1 }) as u64 {
        return Err(invalid("shared frame has an invalid size"));
    }
    // SAFETY: the parent keeps this exact file/extent and transfers exclusive
    // access through sequenced control messages. Native pointers never escape a call.
    // The parent's immutable published input is copied before each request.
    // Child Source pages are read-only; a violating native plugin is isolated
    // by the supervised process without a per-frame hash or extra raster.
    let source = if has_input {
        Some(unsafe { MmapOptions::new().len(bytes).map(&file) }.map_err(control_error)?)
    } else {
        None
    };
    // SAFETY: this validated disjoint Output range has the same sequenced owner
    // and fixed file lifetime as Source; no parent access occurs during render.
    let output = unsafe {
        MmapOptions::new()
            .offset(if has_input { bytes as u64 } else { 0 })
            .len(bytes)
            .map_mut(&file)
    }
    .map_err(control_error)?;
    Ok((file, source, output))
}

// Admit the entire new schema before shared mapping, binary verification or
// any native initialization. There is one version, with no ownerless fallback.
fn validate_prepare_envelope(prepare: &Prepare, nonce: &str) -> Result<(), OpenFxHostError> {
    if prepare.version != VERSION || prepare.nonce != nonce || prepare.owner.0.is_nil() {
        return Err(invalid(
            "native session preparation envelope does not match",
        ));
    }
    Ok(())
}

/// Run a resident native session before media, graphics or application startup.
pub fn run_openfx_resident_worker() -> Result<(), OpenFxHostError> {
    let mut close_trace = close_observation::Trace::default();
    let result = run_openfx_resident_worker_inner(&mut close_trace);
    // All inner locals, including its pump/curves/native/session frame mappings,
    // have dropped. This is not main, TLS/CRT, module or OS-process exit.
    close_trace.record(
        close_observation::Stage::ResidentInnerReturned,
        close_observation::result_code(&result),
    );
    result
}

fn run_openfx_resident_worker_inner(
    close_trace: &mut close_observation::Trace,
) -> Result<(), OpenFxHostError> {
    let endpoint: SocketAddr = std::env::var(ENDPOINT_ENV)
        .map_err(|_| invalid("native session endpoint is missing"))?
        .parse()
        .map_err(|_| invalid("native session endpoint is invalid"))?;
    if !endpoint.ip().is_loopback() {
        return Err(invalid("native session endpoint is not local"));
    }
    let nonce = std::env::var(NONCE_ENV).map_err(|_| invalid("native session nonce is missing"))?;
    if uuid::Uuid::parse_str(&nonce).is_err() {
        return Err(invalid("native session nonce is invalid"));
    }
    let mut control = TcpStream::connect_timeout(&endpoint, DEADLINE).map_err(control_error)?;
    configure(&control, POLL)?;
    let deadline = Instant::now() + DEADLINE;
    write_packet(
        &mut control,
        &Response::Connected { version: VERSION, nonce: nonce.clone() },
        &mut || {
            if Instant::now() >= deadline {
                Err(OpenFxHostError::DeadlineExceeded)
            } else {
                Ok(())
            }
        },
    )?;
    let prepare: Prepare = read_packet(&mut control, &mut || {
        if Instant::now() >= deadline {
            Err(OpenFxHostError::DeadlineExceeded)
        } else {
            Ok(())
        }
    })?;
    validate_prepare_envelope(&prepare, &nonce)?;
    *close_trace = close_observation::Trace::child(prepare.owner, &nonce)?;
    let (_file, input, mut output) = map_frames(&prepare)?;
    verify_worker_binary(
        &prepare.binary_path,
        &prepare.bundle_path,
        &prepare.binary_sha256,
    )?;
    let binary = native_string(&prepare.binary_path)?;
    let bundle = native_string(&prepare.bundle_path)?;
    let identifier = CString::new(prepare.plugin_identifier.as_str())
        .map_err(|_| invalid("plugin identifier contains a null byte"))?;
    let mut pump = pump::WorkerPump::start_observed(
        control,
        prepare.owner,
        Response::Ready { version: VERSION, nonce },
        close_trace.clone(),
    )?;
    // Declared before the session: native destruction may query its last snapshot.
    let mut curves: BTreeMap<String, std::sync::Arc<curves::CurveContext>> = BTreeMap::new();
    let mut session: Option<NativeSession> = None;
    let mut sequence = 0_u64;
    loop {
        // Idle read is interrupted by parent EOF. Processing is supervised by
        // the parent, whose cancellation/deadline kills and reaps this process.
        let request = pump.shared.take_request()?;
        match request {
            Request::Shutdown {} => {
                let native_present = u32::from(session.is_some());
                close_trace.record(close_observation::Stage::NativeDropBegin, native_present);
                drop(session.take());
                close_trace.record(close_observation::Stage::NativeDropEnd, native_present);
                let completion = pump.shared.complete(Response::Closed {})?;
                pump.shared.wait_acknowledged(completion)?;
                close_trace.record(close_observation::Stage::AckReturned, 0);
                close_trace.record(close_observation::Stage::PumpStopJoinBegin, 0);
                pump.stop_and_join();
                close_trace.record(close_observation::Stage::PumpStopJoinEnd, 0);
                return Ok(());
            }
            Request::Cancel { .. } | Request::QuestionReply { .. } => {
                return Err(invalid("control service reached native dispatch"))
            }
            Request::CloseGui { owner, sequence: received } => {
                if owner != prepare.owner || sequence.checked_add(1) != Some(received) {
                    return Err(invalid(
                        "GUI action lost its parent instance namespace or serial",
                    ));
                }
                sequence = received;
                let feedback = session
                    .as_mut()
                    .map(|instance| instance.close_gui())
                    .transpose()
                    .map_err(|status| {
                        unavailable(format!("native GUI teardown rejected ({status})"))
                    })?
                    .flatten();
                let completion =
                    pump.shared.complete(Response::GuiClosed { owner, sequence, feedback })?;
                pump.shared.wait_acknowledged(completion)?;
            }
            Request::Render {
                owner,
                sequence: received,
                timing,
                sampling,
                parameters,
                diagnostic,
                ..
            } => {
                if owner != prepare.owner {
                    return Err(invalid("render lost parent instance namespace"));
                }
                if pump.shared.is_canceled() {
                    let completion =
                        pump.shared.complete(Response::Canceled { owner, sequence: received })?;
                    pump.shared.wait_acknowledged(completion)?;
                    return Ok(());
                }
                sequence = sequence
                    .checked_add(1)
                    .ok_or_else(|| invalid("native session sequence exhausted"))?;
                if received != sequence {
                    return Err(invalid("native session request sequence does not match"));
                }
                let context = WorkerRenderContext {
                    prepare: &prepare,
                    shared: &pump.shared,
                    binary: &binary,
                    bundle: &bundle,
                    identifier: &identifier,
                    input: input.as_deref().unwrap_or(&[]),
                    output: &mut output,
                    session: &mut session,
                    curves: &mut curves,
                };
                let result = if diagnostic {
                    let mut report = OpenFxChildRenderDiagnostic::new(
                        owner,
                        sequence,
                        input.as_ref().map_or(0, |input| input.len()),
                    );
                    let started = Instant::now();
                    let mut result = render_worker_request(
                        context,
                        sequence,
                        timing,
                        sampling,
                        parameters,
                        &mut report,
                    )?;
                    result.response = report.attach(result.response, started);
                    result
                } else {
                    render_worker_request(context, sequence, timing, sampling, parameters, &mut ())?
                };
                let completion = pump.shared.complete(result.response)?;
                pump.shared.wait_acknowledged(completion)?;
                if result.stop {
                    return Ok(());
                }
            }
        }
    }
}

struct WorkerRenderContext<'a> {
    prepare: &'a Prepare,
    shared: &'a pump::Shared,
    binary: &'a CString,
    bundle: &'a CString,
    identifier: &'a CString,
    input: &'a [u8],
    output: &'a mut [u8],
    session: &'a mut Option<NativeSession>,
    curves: &'a mut BTreeMap<String, std::sync::Arc<curves::CurveContext>>,
}
struct WorkerRenderResult {
    response: Response,
    stop: bool,
}
// Both mode instantiations execute this same parameter/native/retirement law.
fn render_worker_request(
    context: WorkerRenderContext<'_>,
    sequence: u64,
    timing: OpenFxRenderTiming,
    sampling: mondrian_core::ImageSampling,
    mut parameters: BTreeMap<String, OpenFxParameterBinding>,
    probe: &mut impl ChildProbe,
) -> Result<WorkerRenderResult, OpenFxHostError> {
    let WorkerRenderContext {
        prepare,
        shared,
        binary,
        bundle,
        identifier,
        input,
        output,
        session,
        curves,
    } = context;
    let phase = probe.start();
    validate_timing(&timing)?;
    validate_native_sampling(prepare.width, sampling)?;
    validate_parameters(&parameters)?;
    let mut candidate_curves = BTreeMap::new();
    for (name, binding) in &mut parameters {
        if let OpenFxParameterQueryMode::Curve { time_scale, .. } = &binding.query_mode {
            if mondrian_core::TimelineTime::from(*time_scale).to_f64() != timing.frame_rate {
                return Err(invalid("curve grid does not match the render frame rate"));
            }
            let kind = binding.value.native_fields().0;
            let prepared = if let Some(cached) = curves.get(name)
                && cached.source == binding.query_mode
                && cached.kind == kind
            {
                std::sync::Arc::clone(cached)
            } else {
                std::sync::Arc::new(curves::CurveContext::prepare(binding, sequence)?)
            };
            binding.value = prepared.value_at(timing.frame)?;
            candidate_curves.insert(name.clone(), prepared);
        }
    }
    validate_parameters(&parameters)?;
    probe.finish(OpenFxChildRenderPhase::RequestAdmission, phase);
    let phase = probe.start();
    verify_worker_binary(
        &prepare.binary_path,
        &prepare.bundle_path,
        &prepare.binary_sha256,
    )?;
    probe.finish(OpenFxChildRenderPhase::BinaryIdentity, phase);
    let phase = probe.start();
    let strings = parameters
        .iter()
        .map(|(name, value)| {
            let name =
                CString::new(name.as_str()).map_err(|_| invalid("parameter name contains null"))?;
            let text = if let OpenFxParameterValue::StrChoice(key)
            | OpenFxParameterValue::String(key) = &value.value
            {
                Some(
                    CString::new(key.as_str())
                        .map_err(|_| invalid("parameter string contains null"))?,
                )
            } else {
                None
            };
            Ok((name, text))
        })
        .collect::<Result<Vec<_>, OpenFxHostError>>()?;
    let native = parameters
        .iter()
        .zip(&strings)
        .map(|((key, value), (name, text))| {
            let (kind, channels) = value.value.native_fields();
            NativeParameter {
                query_mode: match &value.query_mode {
                    OpenFxParameterQueryMode::Constant {} => 0,
                    OpenFxParameterQueryMode::CurrentFrame {} => 1,
                    OpenFxParameterQueryMode::Curve { .. } => 2,
                },
                name: name.as_ptr(),
                kind,
                value: channels,
                string_value: text.as_ref().map_or(std::ptr::null(), |text| text.as_ptr()),
                string_length: text.as_ref().map_or(0, |text| text.as_bytes().len()),
                query: candidate_curves.contains_key(key).then_some(curves::query),
                query_context: candidate_curves.get(key).map_or(std::ptr::null(), |curve| {
                    std::sync::Arc::as_ptr(curve).cast()
                }),
                query_revision: candidate_curves.get(key).map_or(0, |curve| curve.revision),
            }
        })
        .collect::<Vec<_>>();
    let info = NativeFrameInfo {
        source_alpha: if prepare.input_alpha.is_premultiplied() {
            1
        } else if prepare.input_alpha == mondrian_core::ColorFrameAlpha::Opaque {
            2
        } else {
            0
        },
        width: prepare.width as c_int,
        height: prepare.height as c_int,
        project_width: prepare.project_geometry.width(),
        project_height: prepare.project_geometry.height(),
        project_pixel_aspect_ratio: prepare.project_geometry.pixel_aspect_ratio().to_f64(),
        frame: timing.frame,
        frame_rate: timing.frame_rate,
        first_frame: timing.first_frame,
        last_frame: timing.last_frame,
        pixel_aspect_ratio: sampling.sample_aspect_ratio().to_f64(),
    };

    probe.finish(OpenFxChildRenderPhase::ParameterArena, phase);
    let dispatched = if let Some(instance) = session.as_mut() {
        dispatch_prepared_native_render_profiled(
            shared, instance, input, output, &info, &native, probe,
        )
    } else {
        let phase = probe.start();
        let created = NativeSession::create_in_context(
            prepare.owner,
            prepare.context,
            binary,
            bundle,
            identifier,
            input,
            &info,
            &native,
        );
        probe.finish(OpenFxChildRenderPhase::NativeCreate, phase);
        match created {
            Ok(mut instance) => {
                let dispatched = dispatch_prepared_native_render_profiled(
                    shared,
                    &mut instance,
                    input,
                    output,
                    &info,
                    &native,
                    probe,
                );
                *session = Some(instance);
                dispatched
            }
            Err(status) => NativeRenderDispatch::Invoked(Err(NativeRenderError::Rejected(status))),
        }
    };
    let phase = probe.start();
    let result = match dispatched {
        NativeRenderDispatch::Invoked(result) => result,
        NativeRenderDispatch::Canceled => {
            drop(session.take());
            probe.finish(OpenFxChildRenderPhase::TerminalPreparation, phase);
            return Ok(WorkerRenderResult {
                response: Response::Canceled { owner: prepare.owner, sequence },
                stop: true,
            });
        }
    };
    if shared.is_canceled() {
        drop(session.take());
        probe.finish(OpenFxChildRenderPhase::TerminalPreparation, phase);
        return Ok(WorkerRenderResult {
            response: Response::Canceled { owner: prepare.owner, sequence },
            stop: true,
        });
    }
    let alpha = match result {
        Ok(alpha) => alpha,
        Err(NativeRenderError::UserBlocked(feedback)) => {
            *curves = candidate_curves;
            probe.finish(OpenFxChildRenderPhase::TerminalPreparation, phase);
            return Ok(WorkerRenderResult {
                response: Response::UserBlocked { sequence, feedback: *feedback, diagnostic: None },
                stop: false,
            });
        }
        Err(NativeRenderError::Rejected(status)) => {
            // Old and candidate query sources remain live during native teardown.
            drop(session.take());
            probe.finish(OpenFxChildRenderPhase::TerminalPreparation, phase);
            return Ok(WorkerRenderResult {
                response: Response::Rejected { sequence, status },
                stop: true,
            });
        }
    };
    let feedback = match session.as_mut().and_then(NativeSession::take_feedback) {
        Some(feedback) => feedback,
        None => {
            // Even impossible host-contract failure must retire before candidate
            // query arenas leave this dispatch scope; no SDK pointer can dangle.
            drop(session.take());
            return Err(unavailable(
                "accepted native frame lost its feedback report",
            ));
        }
    };
    *curves = candidate_curves;
    probe.finish(OpenFxChildRenderPhase::TerminalPreparation, phase);
    Ok(WorkerRenderResult {
        response: Response::Rendered { sequence, alpha, feedback, diagnostic: None },
        stop: false,
    })
}
fn admit_child_diagnostic(
    value: Option<OpenFxChildRenderDiagnostic>,
    owner: mondrian_core::EffectProcessorOwnerId,
    sequence: u64,
    feedback: &mondrian_effects::EffectProcessorFeedbackSnapshot,
    alpha: Option<mondrian_core::ColorFrameAlpha>,
    bytes: usize,
    probe: &mut impl ParentProbe,
) -> Result<(), OpenFxHostError> {
    if value.is_some() != probe.measure_child() {
        return Err(unavailable(
            "child diagnostic selection disagrees with original request",
        ));
    }
    if let Some(value) = value {
        value.validate(owner, sequence, feedback, alpha, bytes)?;
        probe.record_child(value);
    }
    Ok(())
}

/// One action boundary after real native construction, using the same work
/// owner/control. This is not another instance, graph or SDK interpretation.
enum NativeRenderDispatch {
    Canceled,
    Invoked(Result<mondrian_core::ColorFrameAlpha, NativeRenderError>),
}

#[cfg(test)]
fn dispatch_prepared_native_render(
    shared: &pump::Shared,
    instance: &mut NativeSession,
    input: &[u8],
    output: &mut [u8],
    info: &NativeFrameInfo,
    parameters: &[NativeParameter],
) -> NativeRenderDispatch {
    dispatch_prepared_native_render_profiled(
        shared,
        instance,
        input,
        output,
        info,
        parameters,
        &mut (),
    )
}

fn dispatch_prepared_native_render_profiled(
    shared: &pump::Shared,
    instance: &mut NativeSession,
    input: &[u8],
    output: &mut [u8],
    info: &NativeFrameInfo,
    parameters: &[NativeParameter],
    probe: &mut impl ChildProbe,
) -> NativeRenderDispatch {
    // Construction remains externally supervised; it has no invented SDK
    // abort authority. A stop accepted during it wins before entering Render.
    if shared.is_canceled() {
        return NativeRenderDispatch::Canceled;
    }
    let native_control = shared.native_control();
    let phase = probe.start();
    let result = instance.render_with_native_diagnostic(
        input,
        output,
        info,
        parameters,
        Some(&native_control),
        probe.native_target(),
    );
    probe.finish(OpenFxChildRenderPhase::NativeDispatch, phase);
    NativeRenderDispatch::Invoked(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn empty_feedback() -> mondrian_effects::EffectProcessorFeedbackSnapshot {
        mondrian_effects::EffectProcessorFeedbackSnapshot::new(1, 1, 0, None, None, Vec::new())
            .expect("bounded empty protocol fixture")
    }

    #[test]
    fn successful_native_response_requires_exact_typed_alpha_metadata() {
        for representation in ["straight_float", "premultiplied_float", "opaque"] {
            let packet = serde_json::json!({
                "kind": "rendered", "sequence": 7, "alpha": representation,
                "feedback": empty_feedback(), "diagnostic": null,
            })
            .to_string();
            assert!(serde_json::from_str::<Response>(&packet).is_ok());
        }
        for packet in [
            r#"{"kind":"rendered","sequence":7}"#,
            r#"{"kind":"rendered","sequence":7,"alpha":"unknown"}"#,
            r#"{"kind":"rendered","sequence":7,"alpha":"opaque","status":0}"#,
        ] {
            assert!(serde_json::from_str::<Response>(packet).is_err());
        }
    }

    #[cfg(any(windows, unix))]
    fn retirement_status(code: u32) -> std::process::ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code)
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw((code as i32) << 8)
        }
    }

    #[test]
    fn retirement_exit_expired_before_query_never_observes_or_accepts_a_child() {
        let deadline = Instant::now();
        for resumed_at in [deadline, deadline + Duration::from_nanos(1)] {
            assert!(matches!(
                observe_retirement_exit(
                    deadline,
                    || resumed_at,
                    || panic!("expired observation must not query the Child"),
                ),
                Err(OpenFxHostError::DeadlineExceeded)
            ));
        }
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn retirement_exit_query_crossing_deadline_retains_status_but_never_succeeds() {
        let start = Instant::now();
        let deadline = start + RETIRE;
        for code in [0, 7] {
            for observed_at in [deadline, deadline + Duration::from_nanos(1)] {
                let mut times = [start, observed_at].into_iter();
                let mut queries = 0;
                let observed = observe_retirement_exit(
                    deadline,
                    || times.next().expect("only pre/post query clock reads"),
                    || {
                        queries += 1;
                        Ok(Some(retirement_status(code)))
                    },
                )
                .expect("actual late OS exit remains available for settlement")
                .expect("same queried Child has exited");
                assert_eq!(queries, 1);
                assert!(times.next().is_none());
                assert_eq!(observed.status.code(), Some(code as i32));
                assert!(matches!(
                    observed.graceful,
                    Err(OpenFxHostError::DeadlineExceeded)
                ));
            }
        }
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn retirement_exit_before_deadline_preserves_success_and_nonzero_failure() {
        let start = Instant::now();
        let deadline = start + RETIRE;
        for code in [0, 7] {
            let mut times = [start, deadline - Duration::from_nanos(1)].into_iter();
            let observed = observe_retirement_exit(
                deadline,
                || times.next().expect("pre/post query clock reads"),
                || Ok(Some(retirement_status(code))),
            )
            .expect("query succeeds")
            .expect("actual OS exit");
            assert!(times.next().is_none());
            assert_eq!(observed.status.code(), Some(code as i32));
            if code == 0 {
                assert!(observed.graceful.is_ok());
            } else {
                assert!(matches!(observed.graceful,
                    Err(OpenFxHostError::WorkerFailed(detail))
                        if detail == observed.status.to_string()));
            }
        }
    }

    #[test]
    fn retirement_exit_pending_uses_original_deadline_without_an_extra_query() {
        let start = Instant::now();
        let deadline = start + RETIRE;
        let mut times = [
            start,
            start + Duration::from_millis(1),
            deadline - Duration::from_nanos(1),
            deadline,
        ]
        .into_iter();
        let mut queries = 0;
        for pending in [true, false] {
            let result = observe_retirement_exit(
                deadline,
                || times.next().expect("bounded original clock sequence"),
                || {
                    queries += 1;
                    Ok(None)
                },
            );
            if pending {
                assert!(matches!(result, Ok(None)));
            } else {
                assert!(matches!(result, Err(OpenFxHostError::DeadlineExceeded)));
            }
        }
        assert_eq!(queries, 2);
        assert!(times.next().is_none());
    }

    #[test]
    fn retirement_exit_query_error_keeps_original_control_error() {
        let start = Instant::now();
        let mut clocks = 0;
        let mut queries = 0;
        let result = observe_retirement_exit(
            start + RETIRE,
            || {
                clocks += 1;
                start
            },
            || {
                queries += 1;
                Err(io::Error::from_raw_os_error(6))
            },
        );
        assert_eq!(
            clocks, 1,
            "query error is not reclassified by a later clock read"
        );
        assert_eq!(queries, 1);
        assert!(matches!(result, Err(OpenFxHostError::Unavailable(detail))
            if detail == format!("native session control failed: {}", io::Error::from_raw_os_error(6))));
    }

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let sender = TcpStream::connect(listener.local_addr().expect("address")).expect("sender");
        let (receiver, _) = listener.accept().expect("receiver");
        configure(&receiver, POLL).expect("bounded receiver");
        (sender, receiver)
    }

    #[test]
    fn partial_packets_survive_poll_timeouts_without_losing_bytes() {
        let (mut sender, mut receiver) = pair();
        let writer = thread::spawn(move || {
            let body = serde_json::to_vec(&Response::Rendered {
                sequence: 17,
                alpha: mondrian_core::ColorFrameAlpha::StraightFloat,
                feedback: empty_feedback(),
                diagnostic: None,
            })
            .expect("body");
            let header = (body.len() as u32).to_be_bytes();
            sender.write_all(&header[..2]).expect("partial header");
            thread::sleep(POLL * 2);
            sender.write_all(&header[2..]).expect("remaining header");
            for bytes in body.chunks(3) {
                sender.write_all(bytes).expect("fragment");
                thread::sleep(Duration::from_millis(1));
            }
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let response: Response = read_packet(&mut receiver, &mut || {
            if Instant::now() >= deadline {
                Err(OpenFxHostError::DeadlineExceeded)
            } else {
                Ok(())
            }
        })
        .expect("complete fragmented response");
        assert!(matches!(
            response,
            Response::Rendered {
                sequence: 17,
                alpha: mondrian_core::ColorFrameAlpha::StraightFloat,
                ..
            }
        ));
        writer.join().expect("writer");
    }

    #[test]
    fn packet_bounds_truncation_and_unknown_fields_fail_closed() {
        for length in [0_u32, MAX_WORKER_REQUEST_BYTES as u32 + 1] {
            let (mut sender, mut receiver) = pair();
            sender.write_all(&length.to_be_bytes()).expect("header");
            assert!(
                matches!(read_packet::<Response>(&mut receiver, &mut || Ok(())), Err(OpenFxHostError::Unavailable(reason)) if reason.contains("bound"))
            );
        }
        let (mut sender, mut receiver) = pair();
        sender.write_all(&[0, 0]).expect("truncated header");
        drop(sender);
        assert!(
            matches!(read_packet::<Response>(&mut receiver, &mut || Ok(())), Err(OpenFxHostError::Unavailable(reason)) if reason.contains("mid-packet"))
        );
        let (mut sender, mut receiver) = pair();
        let body = br#"{"kind":"closed","unexpected":1}"#;
        sender.write_all(&(body.len() as u32).to_be_bytes()).expect("header");
        sender.write_all(body).expect("body");
        assert!(
            matches!(read_packet::<Response>(&mut receiver, &mut || Ok(())), Err(OpenFxHostError::Unavailable(reason)) if reason.contains("malformed"))
        );
    }

    #[test]
    fn cancellation_interrupts_a_stalled_partial_packet() {
        let (mut sender, mut receiver) = pair();
        sender.write_all(&100_u32.to_be_bytes()).expect("header");
        sender.write_all(b"partial").expect("partial body");
        let cancellation = mondrian_core::ExecutionCancellationToken::new();
        let worker_token = cancellation.clone();
        let canceler = thread::spawn(move || {
            thread::sleep(POLL * 3);
            worker_token.cancel();
        });
        let started = Instant::now();
        let result = read_packet::<Response>(&mut receiver, &mut || {
            if cancellation.is_canceled() {
                Err(OpenFxHostError::Canceled)
            } else if started.elapsed() > Duration::from_secs(2) {
                Err(OpenFxHostError::DeadlineExceeded)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(OpenFxHostError::Canceled)));
        canceler.join().expect("canceler");
    }

    #[test]
    fn worker_rejects_truncated_and_oversized_shared_frames_before_native_loading() {
        for size in [0, 16, 31, 33, 64] {
            let file = tempfile::NamedTempFile::new().expect("shared file");
            file.as_file().set_len(size).expect("malformed extent");
            let prepare = Prepare {
                context: OpenFxContext::Filter,
                version: VERSION,
                nonce: String::new(),
                binary_path: PathBuf::from("never-loaded.ofx"),
                bundle_path: PathBuf::from("never-loaded.ofx.bundle"),
                binary_sha256: String::new(),
                plugin_identifier: "test".to_owned(),
                width: 1,
                height: 1,
                shared_path: file.path().to_path_buf(),
                input_alpha: mondrian_core::ColorFrameAlpha::StraightFloat,
                owner: mondrian_core::EffectProcessorOwnerId::new(),

                project_geometry: mondrian_effects::EffectProjectGeometry::new(
                    1,
                    1,
                    mondrian_core::SampleAspectRatio::SQUARE,
                )
                .expect("explicit owner project geometry"),
            };
            assert!(
                matches!(map_frames(&prepare), Err(OpenFxHostError::Invalid(reason)) if reason == "shared frame has an invalid size")
            );
        }
    }
    #[test]
    fn parent_namespace_schema_has_one_generation_and_refuses_stale_or_missing_identity() {
        // Nonexistent paths prove envelope admission is before any map/load.
        let mut prepare = Prepare {
            context: OpenFxContext::Filter,
            version: VERSION,
            nonce: "actual nonce".to_owned(),
            binary_path: PathBuf::from("must-never-load.ofx"),
            bundle_path: PathBuf::from("must-never-load.ofx.bundle"),
            binary_sha256: String::new(),
            plugin_identifier: "must-never-execute".to_owned(),
            width: 1,
            height: 1,
            shared_path: PathBuf::from("must-never-map"),
            input_alpha: mondrian_core::ColorFrameAlpha::StraightFloat,
            owner: mondrian_core::EffectProcessorOwnerId::new(),

            project_geometry: mondrian_effects::EffectProjectGeometry::new(
                1,
                1,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
        };
        assert!(validate_prepare_envelope(&prepare, "actual nonce").is_ok());
        for stale in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, u16::MAX] {
            prepare.version = stale;
            assert!(matches!(
                validate_prepare_envelope(&prepare, "actual nonce"),
                Err(OpenFxHostError::Invalid(_))
            ));
        }
        prepare.version = VERSION;
        assert!(validate_prepare_envelope(&prepare, "wrong nonce").is_err());
        for field in ["owner", "context", "project_geometry"] {
            let mut missing = serde_json::to_value(&prepare).expect("canonical Prepare schema");
            missing.as_object_mut().expect("schema object").remove(field);
            assert!(
                serde_json::from_value::<Prepare>(missing).is_err(),
                "no schema fallback without {field}"
            );
        }
        for geometry in [
            serde_json::json!({"width":0,"height":7,"pixel_aspect_ratio":{"numerator":2,"denominator":1}}),
            serde_json::json!({"width":10,"height":0,"pixel_aspect_ratio":{"numerator":2,"denominator":1}}),
            serde_json::json!({"width":10,"height":7,"pixel_aspect_ratio":{"numerator":0,"denominator":1}}),
            serde_json::json!({"width":10,"height":7,"pixel_aspect_ratio":{"numerator":2,"denominator":0}}),
            serde_json::json!({"width":10,"height":7,"pixel_aspect_ratio":{"numerator":2,"denominator":1},"offset":[1,0]}),
        ] {
            let mut invalid = serde_json::to_value(&prepare).unwrap();
            invalid["project_geometry"] = geometry;
            assert!(
                serde_json::from_value::<Prepare>(invalid).is_err(),
                "invalid geometry cannot reach map/load"
            );
        }
        prepare.owner = mondrian_core::EffectProcessorOwnerId(uuid::Uuid::nil());
        assert!(validate_prepare_envelope(&prepare, "actual nonce").is_err());
        assert_eq!(
            OPENFX_RESIDENT_WORKER_ARGUMENT,
            "--internal-openfx-session-v11",
            "sole worker dispatch uses the current generation"
        );
    }
}

#[cfg(test)]
mod bulk_transport_tests {
    use super::*;

    // The two-phase production path acquires this owner before native work.
    // These bit-transport contracts exercise its fill/publication phase only.
    fn materialize_native_output(
        source: &[u8],
        extent: mondrian_effects::EffectFrameExtent,
        format: mondrian_effects::EffectValueFormat,
        checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<EffectFloatFrame, OpenFxHostError> {
        let owner = NativeOutputAllocation::acquire(extent, &mut || Ok(()), reserve_native_output)?;
        super::materialize_native_output(source, owner, format, checkpoint)
    }

    use mondrian_core::ColorFrameAlpha;
    use mondrian_effects::{
        EffectColorDomain, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
    };

    fn format(alpha: ColorFrameAlpha) -> EffectValueFormat {
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            alpha,
        )
    }
    fn bits(pixels: &[[f32; 4]]) -> Vec<[u32; 4]> {
        pixels.iter().map(|pixel| pixel.map(f32::to_bits)).collect()
    }

    #[test]
    fn native_bulk_input_preserves_raw_native_float_bits_and_full_tail() {
        let signal = [
            [f32::MAX, -f32::MAX, f32::from_bits(1), -0.0],
            [-f32::from_bits(1), 16.0, -2.0, 1.5],
            [7.0, -3.0, 11.0, 0.0],
        ];
        let mut shared = vec![0; std::mem::size_of_val(&signal)];
        let mut calls = 0;
        copy_native_input(&mut shared, &signal, &mut || {
            calls += 1;
            Ok(())
        })
        .expect("complete raw transfer");
        let independent: Vec<u8> =
            signal.iter().flatten().flat_map(|value| value.to_ne_bytes()).collect();
        assert_eq!(shared, independent);
        assert_eq!(calls, 2, "one block plus completed-transfer checkpoint");
    }

    #[test]
    fn native_bulk_input_refuses_length_and_never_copies_past_cancel_boundary() {
        let signal = vec![[0.25, -2.0, 8.0, 0.5]; FRAME_TRANSFER_PIXELS * 2 + 1];
        let length = signal.len() * 16;
        assert!(copy_native_input(&mut vec![0; length - 1], &signal, &mut || Ok(())).is_err());
        for stop_at in 1..=4 {
            let mut shared = vec![0xa5; length];
            let mut calls = 0;
            let result = copy_native_input(&mut shared, &signal, &mut || {
                calls += 1;
                if calls == stop_at {
                    Err(OpenFxHostError::Canceled)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(result, Err(OpenFxHostError::Canceled)));
            assert_eq!(calls, stop_at);
            let copied = ((stop_at - 1) * FRAME_TRANSFER_BYTES).min(length);
            let raw: &[u8] = bytemuck::cast_slice(&signal);
            assert_eq!(&shared[..copied], &raw[..copied]);
            assert!(shared[copied..].iter().all(|byte| *byte == 0xa5));
        }
    }

    #[test]
    fn native_bulk_output_retains_raw_finite_association_and_owns_independent_storage() {
        let pixels = [
            [f32::MAX, -f32::MAX, -0.0, -0.5],
            [f32::from_bits(1), -f32::from_bits(1), 16.0, 1.5],
            [7.0, -3.0, 11.0, 0.0],
        ];
        for alpha in [
            ColorFrameAlpha::StraightFloat,
            ColorFrameAlpha::PremultipliedFloat,
        ] {
            let expected = bits(&pixels);
            let mut shared: Vec<u8> = bytemuck::cast_slice(&pixels).to_vec();
            let frame = materialize_native_output(
                &shared,
                EffectFrameExtent::new(3, 1),
                format(alpha),
                &mut || Ok(()),
            )
            .expect("complete typed raw float output");
            assert_eq!(frame.alpha(), alpha);
            assert_eq!(bits(frame.pixels()), expected);
            shared.fill(0);
            assert_eq!(
                bits(frame.pixels()),
                expected,
                "next shared-frame work cannot overwrite the returned owner"
            );
        }
    }

    #[test]
    fn native_bulk_opaque_output_canonicalizes_ignored_alpha_only() {
        let pixels = [
            [f32::MAX, -f32::MAX, -0.0, -0.5],
            [f32::from_bits(1), -f32::from_bits(1), 16.0, 1.5],
            [7.0, -3.0, 11.0, 0.0],
        ];
        let frame = materialize_native_output(
            bytemuck::cast_slice(&pixels),
            EffectFrameExtent::new(3, 1),
            format(ColorFrameAlpha::Opaque),
            &mut || Ok(()),
        )
        .expect("canonical opaque output");
        assert_eq!(frame.alpha(), ColorFrameAlpha::Opaque);
        for (actual, original) in frame.pixels().iter().zip(&pixels) {
            for lane in 0..3 {
                assert_eq!(actual[lane].to_bits(), original[lane].to_bits());
            }
            assert_eq!(actual[3].to_bits(), 1.0f32.to_bits());
        }
    }

    #[test]
    fn native_bulk_output_accepts_unaligned_raw_source_without_borrowing_float_references() {
        let pixels = [[-0.0, f32::from_bits(1), f32::MAX, 1.5]];
        let mut raw = vec![0xff];
        raw.extend_from_slice(bytemuck::cast_slice(&pixels));
        let frame = materialize_native_output(
            &raw[1..],
            EffectFrameExtent::new(1, 1),
            format(ColorFrameAlpha::StraightFloat),
            &mut || Ok(()),
        )
        .expect("byte copy needs no foreign float alignment");
        assert_eq!(bits(frame.pixels()), bits(&pixels));
    }

    #[test]
    fn native_bulk_output_preserves_foreign_boundary_shape_finite_and_alpha_refusals() {
        let extent = EffectFrameExtent::new(1, 1);
        let valid = [[0.25, -2.0, 8.0, 0.5]];
        for length in [0, 15, 17, 32] {
            assert!(materialize_native_output(
                &vec![0; length],
                extent,
                format(ColorFrameAlpha::StraightFloat),
                &mut || Ok(())
            )
            .is_err());
        }
        for lane in 0..4 {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut samples = valid;
                samples[0][lane] = value;
                assert!(matches!(
                    materialize_native_output(
                        bytemuck::cast_slice(&samples),
                        extent,
                        format(ColorFrameAlpha::StraightFloat),
                        &mut || Ok(())
                    ),
                    Err(OpenFxHostError::Frame(
                        mondrian_effects::EffectFloatFrameError::InvalidSample {
                            index: 0,
                            alpha: ColorFrameAlpha::StraightFloat
                        }
                    ))
                ));
            }
        }
        for alpha in [
            ColorFrameAlpha::StraightCoverage,
            ColorFrameAlpha::PremultipliedCoverage,
        ] {
            for extended in [-0.5_f32, 1.5_f32] {
                let invalid = [[0.25, -2.0, 8.0, extended]];
                assert_eq!(
                    std::mem::size_of_val(&invalid),
                    16,
                    "one raw Float32 RGBA pixel"
                );
                assert!(matches!(
                    materialize_native_output(
                        bytemuck::cast_slice(&invalid),
                        extent,
                        format(alpha),
                        &mut || Ok(())
                    ),
                    Err(OpenFxHostError::Frame(
                        mondrian_effects::EffectFloatFrameError::InvalidSample { index: 0, .. }
                    ))
                ));
            }
        }
        let wrong_precision = EffectValueFormat::new(
            EffectWorkingPrecision::NormalizedU8,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        );
        assert!(matches!(
            materialize_native_output(
                bytemuck::cast_slice(&valid),
                extent,
                wrong_precision,
                &mut || Ok(())
            ),
            Err(OpenFxHostError::Frame(
                mondrian_effects::EffectFloatFrameError::WrongPrecision { .. }
            ))
        ));
    }

    #[test]
    fn native_bulk_output_cancel_and_deadline_never_publish_partial_or_unvalidated_pixels() {
        let extent = EffectFrameExtent::new(1024, 129);
        let pixels = vec![[0.25, -2.0, 8.0, 0.5]; 1024 * 129];
        // Opaque also traverses three bounded canonical-A chunks. Exercise
        // every ordinary/opaque copy/admission/final-publication frontier.
        for (alpha, frontiers) in [
            (ColorFrameAlpha::StraightFloat, 9),
            (ColorFrameAlpha::Opaque, 12),
        ] {
            for stop_at in 1..=frontiers {
                let mut calls = 0;
                let result = materialize_native_output(
                    bytemuck::cast_slice(&pixels),
                    extent,
                    format(alpha),
                    &mut || {
                        calls += 1;
                        if calls != stop_at {
                            Ok(())
                        } else if matches!(stop_at, 3 | 7) {
                            Err(OpenFxHostError::DeadlineExceeded)
                        } else {
                            Err(OpenFxHostError::Canceled)
                        }
                    },
                );
                if matches!(stop_at, 3 | 7) {
                    assert!(matches!(result, Err(OpenFxHostError::DeadlineExceeded)));
                } else {
                    assert!(matches!(result, Err(OpenFxHostError::Canceled)));
                }
                assert_eq!(calls, stop_at);
            }
            let frame = materialize_native_output(
                bytemuck::cast_slice(&pixels),
                extent,
                format(alpha),
                &mut || Ok(()),
            )
            .expect("complete tail through the same path");
            let expected = pixels
                .iter()
                .copied()
                .map(|mut pixel| {
                    if alpha == ColorFrameAlpha::Opaque {
                        pixel[3] = 1.0;
                    }
                    pixel.map(f32::to_bits)
                })
                .collect::<Vec<_>>();
            assert_eq!(bits(frame.pixels()), expected);
        }
    }
}

#[cfg(test)]
mod output_resource_tests {
    use super::*;
    use mondrian_core::ColorFrameAlpha;
    use mondrian_effects::{
        EffectColorDomain, EffectFrameExtent, EffectProcessorHostResourceRefusal,
        EffectValueFormat, EffectWorkingPrecision,
    };

    fn refused_reservation(
        pixels: &mut Vec<[f32; 4]>,
        _: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        // Deterministic capacity overflow, not artificial allocator pressure or
        // a product fault flag; no bytes or native action are needed to fail.
        pixels.try_reserve_exact(usize::MAX)
    }

    #[test]
    fn output_reservation_is_typed_and_checks_owner_before_and_after_allocation() {
        let extent = EffectFrameExtent::new(3, 2);
        let mut polls = 0;
        let result = NativeOutputAllocation::acquire(
            extent,
            &mut || {
                polls += 1;
                Ok(())
            },
            refused_reservation,
        );
        assert!(matches!(
            result,
            Err(OpenFxHostError::HostResourceRefused(
                EffectProcessorHostResourceRefusal::OutputAllocation { required_bytes: 96 }
            ))
        ));
        assert_eq!(polls, 2);
        for reason in [false, true] {
            let mut polls = 0;
            let result = NativeOutputAllocation::acquire(
                extent,
                &mut || {
                    polls += 1;
                    if polls == 2 {
                        if reason {
                            Err(OpenFxHostError::DeadlineExceeded)
                        } else {
                            Err(OpenFxHostError::Canceled)
                        }
                    } else {
                        Ok(())
                    }
                },
                refused_reservation,
            );
            if reason {
                assert!(matches!(result, Err(OpenFxHostError::DeadlineExceeded)));
            } else {
                assert!(matches!(result, Err(OpenFxHostError::Canceled)));
            }
        }
        let mut allocator_called = false;
        let result = NativeOutputAllocation::acquire(
            extent,
            &mut || Err(OpenFxHostError::Canceled),
            |pixels, count| {
                allocator_called = true;
                reserve_native_output(pixels, count)
            },
        );
        assert!(matches!(result, Err(OpenFxHostError::Canceled)));
        assert!(!allocator_called);
    }

    #[test]
    fn completed_output_uses_the_pre_admitted_owner_without_a_second_raster_allocation() {
        let extent = EffectFrameExtent::new(2, 1);
        let owner = NativeOutputAllocation::acquire(extent, &mut || Ok(()), reserve_native_output)
            .expect("empty admitted owner");
        let pointer = owner.pixels.as_ptr();
        let capacity = owner.pixels.capacity();
        let signal = [
            [f32::MAX, -0.0, f32::from_bits(1), -0.5],
            [7.0, -3.0, 11.0, 0.0],
        ];
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::PremultipliedFloat,
        );
        let frame =
            materialize_native_output(bytemuck::cast_slice(&signal), owner, format, &mut || Ok(()))
                .expect("one same owner through publication");
        assert_eq!(frame.pixels().as_ptr(), pointer);
        assert!(capacity >= frame.len());
        let actual: Vec<[u32; 4]> =
            frame.pixels().iter().map(|sample| sample.map(f32::to_bits)).collect();
        assert_eq!(actual, signal.map(|sample| sample.map(f32::to_bits)));
    }

    #[test]
    fn oversized_output_owner_is_refused_against_actual_capacity_before_native_work() {
        let extent = EffectFrameExtent::new(3, 2);
        let mut actual_capacity = 0;
        let result = NativeOutputAllocation::acquire(extent, &mut || Ok(()), |pixels, count| {
            pixels.try_reserve_exact(count + 1)?;
            actual_capacity = pixels.capacity();
            Ok(())
        });
        assert!(
            matches!(result, Err(OpenFxHostError::HostResourceRefused(EffectProcessorHostResourceRefusal::OutputCapacity { required_bytes, admitted_bytes: 96 })) if required_bytes == actual_capacity * 16)
        );
        assert!(actual_capacity > 6);
    }

    #[test]
    #[ignore = "requires a fresh pinned MONDRIAN_OPENFX_TEST_HELPER and official Basic MONDRIAN_OPENFX_REFERENCE_BINARY"]
    fn supervised_output_refusal_keeps_actual_worker_and_never_advances_request_or_native_frame() {
        const TEST: &str = "openfx_host::resident::output_resource_tests::supervised_output_refusal_keeps_actual_worker_and_never_advances_request_or_native_frame";
        if !super::super::shared_frame_tests::qualification_child(TEST, &[]) {
            return;
        }
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("pinned product worker"),
        );
        let binary = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic"),
        );
        let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary)
            .expect("actual independent binary inspection");
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightFloat,
        );
        let input = EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(1, 1),
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.25, 0.5, 0.75, 0.5]],
        )
        .expect("input signal");
        let mut session = OpenFxFilterSession::prepare(
            &helper,
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            1,
            1,
            format,
            mondrian_effects::EffectProjectGeometry::new(
                1,
                1,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("actual owned worker");
        let worker = session.worker_process_id();
        session.shared[..session.frame_bytes].fill(0xa5);
        let timing = OpenFxRenderTiming {
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 24.0,
        };
        let parameters = BTreeMap::from([(
            "scale".to_owned(),
            OpenFxParameterBinding::constant(OpenFxParameterValue::Double(1.0)),
        )]);
        for _ in 0..4 {
            let result = session.render_with_output_reservation(
                &timing,
                &parameters,
                &input,
                &mut || Ok(()),
                refused_reservation,
            );
            assert!(matches!(
                result,
                Err(OpenFxHostError::HostResourceRefused(
                    EffectProcessorHostResourceRefusal::OutputAllocation { required_bytes: 16 }
                ))
            ));
            assert_eq!(session.sequence, 0, "no native request serial was issued");
            assert!(
                session.shared[..session.frame_bytes].iter().all(|byte| *byte == 0xa5),
                "no shared input or native parameter/render work occurred"
            );
            assert_eq!(session.worker_process_id(), worker);
            assert_eq!(session.process.state, ProcessState::Running);
            assert!(session.process.child.try_wait().expect("observe actual worker").is_none());
        }
        let result = session.render_with_output_reservation(
            &timing,
            &parameters,
            &input,
            &mut || Ok(()),
            |pixels, count| reserve_native_output(pixels, count + 1),
        );
        assert!(matches!(
            result,
            Err(OpenFxHostError::HostResourceRefused(
                EffectProcessorHostResourceRefusal::OutputCapacity { admitted_bytes: 16, .. }
            ))
        ));
        assert_eq!(session.sequence, 0);
        assert!(session.shared[..session.frame_bytes].iter().all(|byte| *byte == 0xa5));
        assert!(session
            .process
            .child
            .try_wait()
            .expect("oversized owner did not retire actual worker")
            .is_none());
        let output = session
            .render(&timing, &parameters, &input, &mut || Ok(()))
            .expect("first native invocation after allocator recovery");
        assert_eq!(session.sequence, 1);
        assert_eq!(session.worker_process_id(), worker);
        assert_eq!(output.pixels(), input.pixels());
        session.shutdown().expect("same owner destroys/reaps worker");
    }
}

#[cfg(test)]
mod ingress_owner_boundary_tests {
    use super::*;
    use mondrian_core::ColorFrameAlpha;
    use mondrian_effects::{
        EffectColorDomain, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
    };

    fn actual_ingress_stop(reason: bool, stop_at: usize) {
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("pinned current product worker"),
        );
        let binary = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("pinned official Basic"),
        );
        let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary)
            .expect("actual independent inspection");
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        );
        let mut frame = EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(256, 257),
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.25, -0.5, 1.5, 0.25]; 256 * 257],
        )
        .expect("initial valid complete image");
        let mut session = OpenFxFilterSession::prepare(
            &helper,
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            256,
            257,
            format,
            mondrian_effects::EffectProjectGeometry::new(
                256,
                257,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("actual isolated ready owner");
        let last = frame.len() - 1;
        frame.pixels_mut()[last][0] = f32::NAN;
        session.shared[..session.frame_bytes].fill(0xa5);
        let timing = OpenFxRenderTiming {
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 24.0,
        };
        let parameters = BTreeMap::from([(
            "scale".to_owned(),
            OpenFxParameterBinding::constant(OpenFxParameterValue::Double(1.0)),
        )]);
        let mut polls = 0;
        let mut reserved = false;
        let result = session.render_with_output_reservation(
            &timing,
            &parameters,
            &frame,
            &mut || {
                polls += 1;
                if polls == stop_at {
                    if reason {
                        Err(OpenFxHostError::DeadlineExceeded)
                    } else {
                        Err(OpenFxHostError::Canceled)
                    }
                } else {
                    Ok(())
                }
            },
            |pixels, count| {
                reserved = true;
                reserve_native_output(pixels, count)
            },
        );
        if reason {
            assert!(
                matches!(result, Err(OpenFxHostError::DeadlineExceeded)),
                "current deadline must precede invalid tail: {result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(OpenFxHostError::Canceled)),
                "current cancel must precede invalid tail: {result:?}"
            );
        }
        assert_eq!(polls, stop_at);
        assert!(!reserved, "stopped ingress must precede output allocation");
        assert_eq!(
            session.sequence, 0,
            "no request/native invocation may advance"
        );
        assert!(
            session.shared[..session.frame_bytes].iter().all(|byte| *byte == 0xa5),
            "no native source/image mutation"
        );
        session.shutdown().expect("join the sole ready worker");
    }
    #[test]
    #[ignore = "requires a matching fresh pinned product/helper and official Basic; no sleeps"]
    fn resident_ingress_current_cancel_wins_before_invalid_raster_scan() {
        const TEST: &str = "openfx_host::resident::ingress_owner_boundary_tests::resident_ingress_current_cancel_wins_before_invalid_raster_scan";
        if !super::super::shared_frame_tests::qualification_child(TEST, &[]) {
            return;
        }
        actual_ingress_stop(false, 1);
    }
    #[test]
    #[ignore = "requires a matching fresh pinned product/helper and official Basic; no sleeps"]
    fn resident_ingress_deadline_polls_before_nonfinite_second_block() {
        const TEST: &str = "openfx_host::resident::ingress_owner_boundary_tests::resident_ingress_deadline_polls_before_nonfinite_second_block";
        if !super::super::shared_frame_tests::qualification_child(TEST, &[]) {
            return;
        }
        actual_ingress_stop(true, 4);
    }
}

#[cfg(all(test, windows))]
mod process_retirement_tests {
    use super::*;
    use std::cell::RefCell;
    use std::process::{Command, ExitStatus, Stdio};

    const READY_ENV: &str = "MONDRIAN_OPENFX_RETIREMENT_FIXTURE_READY";
    const EXIT_ENV: &str = "MONDRIAN_OPENFX_RETIREMENT_FIXTURE_EXIT";
    const FIXTURE: &str =
        "openfx_host::resident::process_retirement_tests::owned_sleeping_child_fixture";

    fn require_actual_job() {
        let mut inside = 0;
        // SAFETY: the current-process pseudo handle is valid; a null job asks
        // about any Job. `inside` is a live writable BOOL for this query only.
        // No handles are acquired or closed. The outer runner identifies the
        // actual owner Job; this local guard proves membership, not its identity.
        let ok = unsafe {
            windows_sys::Win32::System::JobObjects::IsProcessInJob(
                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                std::ptr::null_mut(),
                &mut inside,
            )
        };
        assert_ne!(ok, 0, "Job query failed: {}", io::Error::last_os_error());
        assert_ne!(
            inside, 0,
            "requires an actual outer owner Job before spawning"
        );
    }

    #[test]
    #[ignore = "direct child fixture; invoke only through a Job-contained retirement test"]
    fn owned_sleeping_child_fixture() {
        require_actual_job();
        let ready = PathBuf::from(std::env::var_os(READY_ENV).expect("exclusive ready path"));
        let exit = PathBuf::from(std::env::var_os(EXIT_ENV).expect("exclusive exit path"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(ready)
            .expect("create-only readiness marker");
        writeln!(file, "{}", std::process::id()).expect("complete readiness marker");
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if exit.exists() {
                std::process::exit(7);
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("outer owner failed to settle its sleeping child");
    }

    pub(super) fn sleeping_owner() -> (ProcessOwner, tempfile::TempDir) {
        require_actual_job();
        let directory = tempfile::tempdir().expect("exclusive child markers");
        let ready = directory.path().join("ready");
        let child = Command::new(std::env::current_exe().expect("current libtest executable"))
            .args([
                "--exact",
                FIXTURE,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(READY_ENV, &ready)
            .env(EXIT_ENV, directory.path().join("exit"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn actual inherited-Job child");
        // Wrap immediately: all later assertion/unwind paths use the same
        // production cleanup, never ignored kill followed by unconditional wait.
        let mut owner = ProcessOwner {
            child,
            state: ProcessState::Running,
            close_trace: close_observation::Trace::default(),
            cleanup_hooks: None,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "child readiness deadline");
            assert!(owner.child.try_wait().expect("observe startup").is_none());
            if let Ok(marker) = std::fs::read_to_string(&ready)
                && let Ok(pid) = marker.trim().parse::<u32>()
            {
                assert_eq!(
                    pid,
                    owner.child.id(),
                    "marker belongs to exact retained Child"
                );
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        (owner, directory)
    }

    fn await_exit(child: &mut Child) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "actual exit observation deadline"
            );
            if let Some(status) = child.try_wait().expect("nonblocking exit observation") {
                return status;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub(super) fn request_natural_exit(directory: &tempfile::TempDir) {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.path().join("exit"))
            .expect("create-only natural exit request");
    }

    fn refusal_text(result: Result<(), OpenFxHostError>, pid: u32) -> String {
        let Err(OpenFxHostError::Unavailable(text)) = result else {
            panic!("expected exact typed unconfirmed cleanup refusal: {result:?}");
        };
        assert!(text.contains(&format!("pid={pid}")), "{text}");
        text
    }

    pub(super) fn assert_settled_idempotent(owner: &mut ProcessOwner) {
        assert_eq!(owner.state, ProcessState::Settled);
        owner
            .terminate_with(
                |_| panic!("settled owner must not observe again"),
                |_| panic!("settled owner must not kill again"),
                |_| panic!("settled owner must not wait again"),
            )
            .expect("idempotent settlement");
        assert!(owner.child.try_wait().expect("actual child exit retained").is_some());
    }

    #[test]
    #[ignore = "requires actual outer owner Job; existing held-child fixture only"]
    fn resident_event_wait_keeps_held_child_then_observes_same_natural_exit() {
        let (mut owner, directory) = sleeping_owner();
        let pid = owner.child.id();
        let mut queries = close_observation::ExitQueryProbe::default();
        assert!(
            retirement_wait::wait_for_exit(&mut owner.child, Duration::ZERO, &mut queries)
                .expect("finite held-child timeout")
                .is_none()
        );
        assert_eq!(owner.state, ProcessState::Running);
        assert_eq!(owner.child.id(), pid);
        assert!(owner.child.try_wait().expect("timeout did not kill the child").is_none());
        request_natural_exit(&directory);
        // The existing fixture uses this five-second exit boundary; this is not
        // a new product shutdown budget or an exit-performance qualification.
        let deadline = Instant::now() + Duration::from_secs(5);
        let observed = loop {
            if let Some(observed) = observe_retirement_exit(deadline, Instant::now, || {
                retirement_wait::wait_for_exit(
                    &mut owner.child,
                    deadline.saturating_duration_since(Instant::now()),
                    &mut queries,
                )
            })
            .expect("same borrowed handle waits only until the original fixture deadline")
            {
                break observed;
            }
        };
        assert_eq!(owner.child.id(), pid);
        assert_eq!(observed.status.code(), Some(7));
        owner.confirmed_exit();
        assert!(
            matches!(observed.graceful, Err(OpenFxHostError::WorkerFailed(detail))
            if detail == observed.status.to_string())
        );
        assert_settled_idempotent(&mut owner);
    }

    #[test]
    #[ignore = "requires actual outer owner Job; spawns one owned native OS child"]
    fn resident_process_initial_exit_settles_without_kill_or_wait() {
        let (mut owner, directory) = sleeping_owner();
        request_natural_exit(&directory);
        assert_eq!(await_exit(&mut owner.child).code(), Some(7));
        let trace = RefCell::new(Vec::new());
        let mut cleanup_probe = close_observation::CleanupProbe::new(true);
        owner
            .terminate_with_probe(
                |child| {
                    trace.borrow_mut().push("observe");
                    child.try_wait()
                },
                |_| panic!("initial exit must not kill"),
                |_| panic!("initial exit must not wait"),
                &mut cleanup_probe,
            )
            .expect("actual initially exited child");
        assert_eq!(trace.into_inner(), ["observe"]);
        let (result, flags, value) =
            cleanup_probe.operation(close_observation::CleanupStep::InitialObservation);
        assert_eq!((result, flags & !1, value), (2, 2, 7));
        for step in [
            close_observation::CleanupStep::Kill,
            close_observation::CleanupStep::FinalObservation,
            close_observation::CleanupStep::Wait,
        ] {
            assert_eq!(cleanup_probe.operation(step), (0, 0, 0));
        }
        assert_settled_idempotent(&mut owner);
    }

    #[test]
    #[ignore = "requires actual outer owner Job; refusal injection with actual live retained child"]
    fn resident_process_refused_live_kill_retains_child_and_retry_settles() {
        let (mut owner, _directory) = sleeping_owner();
        let pid = owner.child.id();
        let trace = RefCell::new(Vec::new());
        let result = owner.terminate_with(
            |child| {
                trace.borrow_mut().push("observe");
                child.try_wait()
            },
            |_| {
                trace.borrow_mut().push("kill");
                Err(io::Error::from_raw_os_error(5))
            },
            |_| panic!("unconfirmed failed kill must never wait"),
        );
        let text = refusal_text(result, pid);
        assert!(
            text.contains("raw_os=Some(5)") && text.contains("child still running"),
            "{text}"
        );
        assert_eq!(trace.into_inner(), ["observe", "kill", "observe"]);
        assert_eq!(owner.state, ProcessState::Retiring);
        assert_eq!(owner.child.id(), pid);
        assert!(owner.child.try_wait().expect("actual refusal retained live child").is_none());
        assert!(matches!(
            owner.check_liveness(),
            Err(OpenFxHostError::Unavailable(_))
        ));
        assert_eq!(owner.state, ProcessState::Retiring, "no re-admission");
        owner.terminate().expect("real kill/reap retry");
        assert_settled_idempotent(&mut owner);
    }

    #[test]
    #[ignore = "requires actual outer owner Job; actual natural exit after controlled kill refusal"]
    fn resident_process_refused_kill_exit_race_observes_settlement_without_wait() {
        let (mut owner, directory) = sleeping_owner();
        let trace = RefCell::new(Vec::new());
        owner
            .terminate_with(
                |child| {
                    trace.borrow_mut().push("observe");
                    child.try_wait()
                },
                |child| {
                    trace.borrow_mut().push("kill");
                    request_natural_exit(&directory);
                    assert_eq!(await_exit(child).code(), Some(7));
                    Err(io::Error::from_raw_os_error(5))
                },
                |_| panic!("refused kill with confirmed exit must not wait"),
            )
            .expect("same actual child exited naturally before post-kill observation");
        assert_eq!(trace.into_inner(), ["observe", "kill", "observe"]);
        assert_settled_idempotent(&mut owner);
    }

    #[test]
    #[ignore = "requires actual outer owner Job; observation/kill failure injection"]
    fn resident_process_observation_and_kill_errors_preserve_stages_and_retirement() {
        let (mut owner, _directory) = sleeping_owner();
        let trace = RefCell::new(Vec::new());
        let mut cleanup_probe = close_observation::CleanupProbe::new(true);
        let mut observations = 0;
        let result = owner.terminate_with_probe(
            |_| {
                trace.borrow_mut().push("observe");
                observations += 1;
                Err(io::Error::from_raw_os_error(if observations == 1 {
                    87
                } else {
                    6
                }))
            },
            |_| {
                trace.borrow_mut().push("kill");
                Err(io::Error::from_raw_os_error(5))
            },
            |_| panic!("failed observations/refused kill must not wait"),
            &mut cleanup_probe,
        );
        let text = refusal_text(result, owner.child.id());
        for evidence in [
            "initial try_wait=failed",
            "post-kill try_wait failed",
            "raw_os=Some(87)",
            "raw_os=Some(6)",
            "raw_os=Some(5)",
        ] {
            assert!(text.contains(evidence), "missing {evidence}: {text}");
        }
        assert_eq!(trace.into_inner(), ["observe", "kill", "observe"]);
        for (step, raw_os) in [
            (close_observation::CleanupStep::InitialObservation, 87),
            (close_observation::CleanupStep::Kill, 5),
            (close_observation::CleanupStep::FinalObservation, 6),
        ] {
            let (result, flags, value) = cleanup_probe.operation(step);
            assert_eq!((result, flags & !1, value), (4, 8, raw_os));
        }
        assert_eq!(
            cleanup_probe.operation(close_observation::CleanupStep::Wait),
            (0, 0, 0)
        );
        assert_eq!(owner.state, ProcessState::Retiring);
        assert!(owner.child.try_wait().expect("injection retained actual live child").is_none());
        assert!(matches!(
            owner.check_liveness(),
            Err(OpenFxHostError::Unavailable(_))
        ));
        owner.terminate().expect("real cleanup after controlled OS errors");
        assert_settled_idempotent(&mut owner);
    }

    #[test]
    #[ignore = "requires actual outer owner Job; real kill with controlled reap failure"]
    fn resident_process_successful_kill_failed_wait_retains_then_observes_exit() {
        let (mut owner, _directory) = sleeping_owner();
        let trace = RefCell::new(Vec::new());
        let mut cleanup_probe = close_observation::CleanupProbe::new(true);
        let result = owner.terminate_with_probe(
            |child| {
                trace.borrow_mut().push("observe");
                child.try_wait()
            },
            |child| {
                trace.borrow_mut().push("kill");
                child.kill()
            },
            |_| {
                trace.borrow_mut().push("wait");
                Err(io::Error::from_raw_os_error(6))
            },
            &mut cleanup_probe,
        );
        let text = refusal_text(result, owner.child.id());
        assert!(
            text.contains("reap failed") && text.contains("raw_os=Some(6)"),
            "{text}"
        );
        assert_eq!(trace.into_inner(), ["observe", "kill", "wait"]);
        let (result, flags, value) = cleanup_probe.operation(close_observation::CleanupStep::Wait);
        assert_eq!((result, flags & !1, value), (4, 8, 6));
        assert_eq!(
            cleanup_probe.operation(close_observation::CleanupStep::FinalObservation),
            (0, 0, 0)
        );
        assert_eq!(owner.state, ProcessState::Retiring);
        await_exit(&mut owner.child);
        owner
            .terminate_with(
                Child::try_wait,
                |_| panic!("exit retry must not kill"),
                |_| panic!("exit retry must not wait"),
            )
            .expect("actual exit observation settles failed reap");
        assert_settled_idempotent(&mut owner);
    }

    #[test]
    #[ignore = "requires actual outer owner Job; real kill and real reap"]
    fn resident_process_successful_kill_and_wait_settles_once() {
        let (mut owner, _directory) = sleeping_owner();
        let trace = RefCell::new(Vec::new());
        let mut cleanup_probe = close_observation::CleanupProbe::new(true);
        owner
            .terminate_with_probe(
                |child| {
                    trace.borrow_mut().push("observe");
                    child.try_wait()
                },
                |child| {
                    trace.borrow_mut().push("kill");
                    child.kill()
                },
                |child| {
                    trace.borrow_mut().push("wait");
                    child.wait()
                },
                &mut cleanup_probe,
            )
            .expect("real actual child kill and reap");
        assert_eq!(trace.into_inner(), ["observe", "kill", "wait"]);
        assert_eq!(
            cleanup_probe.operation(close_observation::CleanupStep::InitialObservation).0,
            1
        );
        assert_eq!(
            cleanup_probe.operation(close_observation::CleanupStep::Kill).0,
            3
        );
        assert_eq!(
            cleanup_probe.operation(close_observation::CleanupStep::Wait).0,
            2
        );
        assert_eq!(
            cleanup_probe.operation(close_observation::CleanupStep::FinalObservation),
            (0, 0, 0)
        );
        assert_settled_idempotent(&mut owner);
    }

    // Real shutdown transport/mapping and an actual retained Child. This does
    // not claim native OFX Ready/Render for the controlled TCP session.
    fn shutdown_primary_after_cleanup_refusal(wrong_response: bool) {
        let (process, _directory) = sleeping_owner();
        let pid = process.child.id();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("exclusive shutdown endpoint");
        let control = TcpStream::connect(listener.local_addr().expect("endpoint"))
            .expect("real shutdown control");
        let (mut peer, _) = listener.accept().expect("real shutdown peer");
        configure(&control, POLL).expect("bounded session control");
        configure(&peer, POLL).expect("bounded peer control");
        let file = tempfile::NamedTempFile::new().expect("retained frame file");
        file.as_file().set_len(32).expect("one input and output pixel");
        // SAFETY: this same exact-sized file remains owned by the session.
        let mut shared = unsafe { MmapOptions::new().len(32).map_mut(file.as_file()) }
            .expect("real shared mapping");
        shared.fill(0xa5);
        let mut session = OpenFxFilterSession {
            process,
            control,
            shared,
            _file: file,
            inspection: OpenFxBinaryInspection {
                binary_path: PathBuf::new(),
                binary_sha256: String::new(),
                plugins: Vec::new(),
            },
            width: 1,
            height: 1,
            frame_bytes: 16,
            sequence: 0,
            input_format: mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            context: OpenFxContext::Filter,
            owner: mondrian_core::EffectProcessorOwnerId::new(),
            process_id: std::num::NonZeroU32::new(pid).expect("actual child PID"),
            native_owner: None,
            last_invocation: 0,
            persistent_revision: 0,
            progress_generation: 0,
            feedback: None,
            response_reader: pump::PacketReader::acquire().expect("original response reader"),
            request_writer: pump::PacketWriter::acquire().expect("original request writer"),
        };
        if wrong_response {
            write_packet(
                &mut peer,
                &Response::Connected { version: VERSION, nonce: "not-closed".to_owned() },
                &mut || Ok(()),
            )
            .expect("real wrong response packet");
        }
        session.process.cleanup_hooks = Some(CleanupHooks::default());
        let result = session.shutdown();
        let hooks = session.process.cleanup_hooks.take().expect("cleanup refusal installed");
        let state_after_failure = session.process.state;
        let pid_after_failure = session.process.child.id();
        let live_after_failure = session.process.child.try_wait().expect("observe exact child");
        let refused_readmission = session.process.check_liveness();
        // Before the intentional baseline assertion, reap this same original
        // owner for real. A target RED must not create a second Job/outlive RED.
        session.process.terminate().expect("same owner actual kill/reap retry");
        assert_settled_idempotent(&mut session.process);
        let read_deadline = Instant::now() + Duration::from_secs(5);
        let request: Request = read_packet(&mut peer, &mut || {
            check_owner(&mut || Ok(()), read_deadline)
        })
        .expect("original Shutdown actually crossed TCP");
        assert!(matches!(request, Request::Shutdown {}));
        assert_eq!(state_after_failure, ProcessState::Retiring);
        assert_eq!(pid_after_failure, pid);
        assert!(
            live_after_failure.is_none(),
            "refusal retained the same live child"
        );
        assert!(matches!(
            refused_readmission,
            Err(OpenFxHostError::Unavailable(_))
        ));
        assert_eq!(hooks.observations.get(), 2);
        assert_eq!(hooks.kills.get(), 1);
        assert_eq!(session.sequence, 0);
        assert_eq!(session.native_owner, None);
        assert_eq!(session.last_invocation, 0);
        assert!(session.shared.iter().all(|byte| *byte == 0xa5));
        if wrong_response {
            assert!(
                matches!(result, Err(OpenFxHostError::Unavailable(ref detail))
                    if detail == "native session did not close"),
                "OFX shutdown primary response detail must survive cleanup refusal: {result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(OpenFxHostError::DeadlineExceeded)),
                "OFX shutdown primary DeadlineExceeded must survive cleanup refusal: {result:?}"
            );
        }
        session.shutdown().expect("settled shutdown remains idempotent");
    }

    #[test]
    #[ignore = "requires actual outer owner Job; real TCP shutdown deadline plus OS cleanup refusal"]
    fn resident_shutdown_deadline_preserves_primary_after_cleanup_refusal() {
        shutdown_primary_after_cleanup_refusal(false);
    }

    #[test]
    #[ignore = "requires actual outer owner Job; real wrong shutdown response plus OS cleanup refusal"]
    fn resident_shutdown_wrong_response_preserves_primary_after_cleanup_refusal() {
        shutdown_primary_after_cleanup_refusal(true);
    }

    #[test]
    #[ignore = "requires actual outer owner Job, pinned product worker and official Basic"]
    fn resident_render_cancel_preserves_primary_after_cleanup_refusal() {
        require_actual_job();
        let helper =
            PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("pinned product"));
        let binary = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic"),
        );
        let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary)
            .expect("actual inspection");
        let format = mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        );
        let input = EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(1, 1),
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.25, 0.5, 0.75, 0.5]],
        )
        .expect("authored frame");
        let mut session = OpenFxFilterSession::prepare(
            &helper,
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            1,
            1,
            format,
            mondrian_effects::EffectProjectGeometry::new(
                1,
                1,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("actual native Ready");
        let pid = session.process.child.id();
        session.shared.fill(0xa5);
        session.process.cleanup_hooks = Some(CleanupHooks::default());
        let result = session.render(
            &OpenFxRenderTiming {
                frame: 0.0,
                frame_rate: 24.0,
                first_frame: 0.0,
                last_frame: 24.0,
            },
            &BTreeMap::new(),
            &input,
            &mut || Err(OpenFxHostError::Canceled),
        );
        let hooks = session.process.cleanup_hooks.take().expect("cleanup refusal installed");
        let state_after_failure = session.process.state;
        let pid_after_failure = session.process.child.id();
        let live_after_failure = session.process.child.try_wait().expect("observe exact child");
        let refused_readmission = session.process.check_liveness();
        session.process.terminate().expect("same owner actual native kill/reap retry");
        assert_settled_idempotent(&mut session.process);
        assert_eq!(state_after_failure, ProcessState::Retiring);
        assert_eq!(pid_after_failure, pid);
        assert!(live_after_failure.is_none());
        assert!(matches!(
            refused_readmission,
            Err(OpenFxHostError::Unavailable(_))
        ));
        assert_eq!(hooks.observations.get(), 2);
        assert_eq!(hooks.kills.get(), 1);
        assert_eq!(session.sequence, 0);
        assert_eq!(session.native_owner, None);
        assert_eq!(session.last_invocation, 0);
        assert!(session.shared.iter().all(|byte| *byte == 0xa5));
        assert_eq!(input.pixels(), &[[0.25, 0.5, 0.75, 0.5]]);
        assert!(
            matches!(result, Err(OpenFxHostError::Canceled)),
            "OFX render admission primary Canceled must survive cleanup refusal: {result:?}"
        );
        session.shutdown().expect("settled shutdown remains idempotent");
    }

    #[test]
    #[ignore = "requires actual outer owner Job, pinned product worker and official Basic"]
    fn resident_retiring_session_refuses_render_and_shutdown_skips_control_packet() {
        require_actual_job();
        let helper =
            PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("pinned product"));
        let binary = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic"),
        );
        let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary)
            .expect("actual inspection");
        let format = mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        );
        let input = EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(1, 1),
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.25, 0.5, 0.75, 0.5]],
        )
        .expect("authored frame");
        let mut session = OpenFxFilterSession::prepare(
            &helper,
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            1,
            1,
            format,
            mondrian_effects::EffectProjectGeometry::new(
                1,
                1,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("actual native session");
        let pid = session.process.child.id();
        let native_owner = session.native_owner;
        let last_invocation = session.last_invocation;
        session.shared.fill(0xa5);
        let result = session.process.terminate_with(
            Child::try_wait,
            |_| Err(io::Error::from_raw_os_error(5)),
            |_| panic!("refused live kill must not wait"),
        );
        refusal_text(result, pid);
        assert_eq!(session.process.state, ProcessState::Retiring);
        assert!(session
            .process
            .child
            .try_wait()
            .expect("actual retained native child")
            .is_none());
        let timing = OpenFxRenderTiming {
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 24.0,
        };
        let parameters = BTreeMap::from([(
            "scale".to_owned(),
            OpenFxParameterBinding::constant(OpenFxParameterValue::Double(1.0)),
        )]);
        let output = session.render(&timing, &parameters, &input, &mut || Ok(()));
        assert!(
            matches!(output, Err(OpenFxHostError::Unavailable(_))),
            "retired work refused: {output:?}"
        );
        assert_eq!(session.sequence, 0);
        assert_eq!(session.process.child.id(), pid);
        assert_eq!(session.native_owner, native_owner);
        assert_eq!(session.last_invocation, last_invocation);
        assert!(
            session.shared.iter().all(|byte| *byte == 0xa5),
            "no native/shared frame mutation"
        );
        // Closing this write half makes a mistakenly submitted Shutdown packet
        // fail. Retiring cleanup must go straight to its retained Child instead.
        session
            .control
            .shutdown(std::net::Shutdown::Write)
            .expect("close local control writer");
        session.shutdown().expect("retiring cleanup sends no control packet");
        assert_settled_idempotent(&mut session.process);
        session.shutdown().expect("session shutdown is idempotent");
    }
}

#[cfg(test)]
mod generator_storage_tests {
    use super::*;
    #[test]
    fn generator_has_one_output_mapping_no_source_and_exact_per_context_charge() {
        let filter = OpenFxFilterSession::storage_for_context(3, 2, OpenFxContext::Filter).unwrap();
        let generator =
            OpenFxFilterSession::storage_for_context(3, 2, OpenFxContext::Generator).unwrap();
        assert_eq!(
            filter.retained_bytes() - generator.retained_bytes(),
            3 * 2 * 16
        );
        assert_eq!(
            filter.active_peak_bytes() - generator.active_peak_bytes(),
            3 * 2 * 16
        );
        assert_eq!(
            generator.active_peak_bytes() - generator.retained_bytes(),
            3 * 2 * 16,
            "returned Vec overlaps resident mapping"
        );
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(3 * 2 * 16).unwrap();
        let mut prepare = Prepare {
            context: OpenFxContext::Generator,
            version: VERSION,
            nonce: "nonce".to_owned(),
            binary_path: PathBuf::new(),
            bundle_path: PathBuf::new(),
            binary_sha256: String::new(),
            plugin_identifier: "test.generator".to_owned(),
            width: 3,
            height: 2,
            shared_path: file.path().to_path_buf(),
            input_alpha: mondrian_core::ColorFrameAlpha::StraightFloat,
            owner: mondrian_core::EffectProcessorOwnerId::new(),

            project_geometry: mondrian_effects::EffectProjectGeometry::new(
                3,
                2,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
        };
        {
            let (_, source, output) = map_frames(&prepare).expect("only Output mapping");
            assert!(source.is_none());
            assert_eq!(output.len(), 3 * 2 * 16);
        }
        prepare.context = OpenFxContext::Filter;
        assert!(
            map_frames(&prepare).is_err(),
            "one mapping cannot impersonate Filter input/output"
        );
        let mut encoded = serde_json::to_value(&prepare).unwrap();
        encoded.as_object_mut().unwrap().remove("context");
        assert!(
            serde_json::from_value::<Prepare>(encoded.clone()).is_err(),
            "no contextless fallback"
        );
        encoded["context"] = serde_json::json!("general");
        assert!(
            serde_json::from_value::<Prepare>(encoded).is_err(),
            "unimplemented context cannot map/load"
        );
    }
}
