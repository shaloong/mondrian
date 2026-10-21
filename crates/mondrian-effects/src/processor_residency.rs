//! Execution-owned, bounded reuse of frame-independent Float32 processors.

use mondrian_core::{EffectId, MondrianError, Result};
use std::collections::VecDeque;
use std::sync::Arc;

/// Authoritative reason the owning scheduler stopped a processor invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectProcessorStopReason {
    /// The execution generation was canceled or superseded.
    Canceled,
    /// The owning scheduler's deadline expired.
    DeadlineExpired,
}

/// Borrowed cooperative control for processor preparation and processing.
///
/// The scheduler remains the authority for cancellation and deadlines. A stop
/// is sticky for this invocation and must be returned as MondrianError::Cancelled;
/// it is not a plugin fault and must not enter runtime quarantine.
pub struct EffectProcessorControl<'a> {
    checkpoint: Option<&'a mut dyn FnMut() -> Option<EffectProcessorStopReason>>,
    stopped: Option<EffectProcessorStopReason>,
    feedback: Vec<crate::EffectProcessorFeedbackReceipt>,
    planned_feedback_bytes: usize,
    observation: crate::EffectProcessorObservation,
    current_occurrence: Option<(EffectFloatProcessorPreparation, u64)>,
}

impl<'a> EffectProcessorControl<'a> {
    /// Bind the owning scheduler's cooperative checkpoint.
    pub fn new(checkpoint: &'a mut dyn FnMut() -> Option<EffectProcessorStopReason>) -> Self {
        Self {
            checkpoint: Some(checkpoint),
            stopped: None,
            feedback: Vec::new(),
            planned_feedback_bytes: 0,
            observation: crate::EffectProcessorObservation { sink: None, deadline: None },
            current_occurrence: None,
        }
    }

    /// Control for explicitly uncancelled scalar/offline references.
    pub const fn uncancelled() -> Self {
        Self {
            checkpoint: None,
            stopped: None,
            feedback: Vec::new(),
            planned_feedback_bytes: 0,
            observation: crate::EffectProcessorObservation { sink: None, deadline: None },
            current_occurrence: None,
        }
    }

    /// Bind the current task's observer and exact absolute deadline. The bank
    /// still owns every native instance and final retention decision.
    pub fn bind_observation(&mut self, observation: crate::EffectProcessorObservation) {
        self.observation = observation;
    }

    /// Exact owner deadline; the native cancel grace may never extend it.
    pub const fn deadline(&self) -> Option<std::time::Instant> {
        self.observation.deadline
    }

    /// Publish actual progress through this current bank-bound occurrence.
    /// This callback stays on the owning execution thread, not a foreign native
    /// callback thread. It reports no pixels or retention disposition.
    pub fn observe_progress(&mut self, progress: crate::EffectProcessorLiveProgress) -> Result<()> {
        self.check()?;
        let (preparation, provider_revision) =
            self.current_occurrence.ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_progress".to_owned(),
                reason: "live progress has no executing occurrence".to_owned(),
            })?;
        if progress.owner.0.is_nil()
            || progress.native_owner == 0
            || progress.invocation == 0
            || progress.revision == 0
        {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_progress".to_owned(),
                reason: "live progress has an invalid actual work identity".to_owned(),
            });
        }
        if let Some(sink) = &self.observation.sink {
            sink.observe(&crate::EffectProcessorProgressReceipt {
                preparation,
                provider_revision,
                progress,
            });
        }
        self.check()
    }

    /// Admit current actual-GUI facts from this same executing bank binding.
    /// A progress observer, author address or unbound callback cannot open a GUI.
    pub fn native_gui_admission(&self) -> crate::EffectProcessorGuiAdmission {
        let Some((preparation, revision)) = self.current_occurrence else {
            return crate::EffectProcessorGuiAdmission::CLOSED;
        };
        let Some(service) = self.observation.sink.as_ref().and_then(|sink| sink.question_service())
        else {
            return crate::EffectProcessorGuiAdmission::CLOSED;
        };
        crate::EffectProcessorGuiAdmission::new(service.gui_state(preparation, revision))
    }

    /// Factual GUI lifetime admitted through this same bank-bound occurrence.
    pub fn native_gui_presence(
        &self,
        owner: mondrian_core::EffectProcessorOwnerId,
    ) -> Option<std::sync::Weak<dyn crate::EffectProcessorGuiPresence>> {
        let (preparation, revision) = self.current_occurrence?;
        self.observation.sink.as_ref()?.question_service()?.gui_presence(
            preparation,
            revision,
            owner,
        )
    }

    /// Poll one actual bounded native Question after binding the current bank
    /// occurrence/provider. The native callback itself cannot invoke this borrow.
    pub fn observe_question(
        &mut self,
        question: &crate::EffectProcessorLiveQuestion,
    ) -> Result<crate::EffectProcessorQuestionState> {
        self.check()?;
        let (preparation, provider_revision) =
            self.current_occurrence.ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_question".to_owned(),
                reason: "Question has no executing occurrence".to_owned(),
            })?;
        if question.owner.0.is_nil()
            || question.text.kind() != crate::EffectProcessorNoticeKind::Information
        {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_question".to_owned(),
                reason: "Question has invalid actual work identity or bounded text kind".to_owned(),
            });
        }
        let Some(service) = self.observation.sink.as_ref().and_then(|sink| sink.question_service())
        else {
            return Ok(crate::EffectProcessorQuestionState::Unavailable);
        };
        let receipt =
            crate::EffectProcessorQuestionView { preparation, provider_revision, question };
        let state = service.question(receipt);
        self.check()?;
        Ok(state)
    }

    /// Complete only the original current service without changing bank-owned
    /// instance retention. Cleanup also runs on stop/fault and may not mutate
    /// author state, extend a scheduler deadline or publish a raster.
    pub fn finish_question(&self, question: &crate::EffectProcessorLiveQuestion) {
        if let Some((preparation, provider_revision)) = self.current_occurrence
            && let Some(service) =
                self.observation.sink.as_ref().and_then(|sink| sink.question_service())
        {
            service.finish_question(crate::EffectProcessorQuestionView {
                preparation,
                provider_revision,
                question,
            });
        }
    }

    /// Poll the owner and preserve its exact sticky stop reason.
    pub fn checkpoint(&mut self) -> std::result::Result<(), EffectProcessorStopReason> {
        if self.stopped.is_none() {
            self.stopped = self.checkpoint.as_mut().and_then(|checkpoint| checkpoint());
            if self.stopped.is_none()
                && self
                    .observation
                    .deadline
                    .is_some_and(|deadline| std::time::Instant::now() >= deadline)
            {
                self.stopped = Some(EffectProcessorStopReason::DeadlineExpired);
            }
        }
        match self.stopped {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// Poll before allocation, during bounded native waits and before publication.
    pub fn check(&mut self) -> Result<()> {
        self.checkpoint().map_err(|_| MondrianError::Cancelled)
    }

    /// Take bounded facts from this actual invocation; no raster or author state.
    pub fn take_feedback(
        &mut self,
    ) -> std::result::Result<crate::EffectProcessorFeedbackBatch, crate::EffectProcessorFeedbackError>
    {
        let mut batch = crate::EffectProcessorFeedbackBatch::new();
        // Drain preserves the pre-admitted capacity even if an in-process
        // provider observes metadata during its callback. No later push grows it.
        for receipt in self.feedback.drain(..) {
            batch.push(receipt)?;
        }
        Ok(batch)
    }

    /// Exact logical feedback currently owned by this invocation.
    pub fn feedback_retained_bytes(&self) -> usize {
        self.feedback.capacity() * std::mem::size_of::<crate::EffectProcessorFeedbackReceipt>()
            + self
                .feedback
                .iter()
                .map(|receipt| {
                    receipt.retained_bytes()
                        - std::mem::size_of::<crate::EffectProcessorFeedbackReceipt>()
                })
                .sum::<usize>()
    }

    pub(crate) fn feedback_count(&self) -> usize {
        self.feedback.len()
    }

    /// Admit all compiled occurrences in one operation before its first DSP.
    /// This stores only the bounded metadata requirement, never another plan.
    pub fn admit_feedback_route(
        &mut self,
        requirements: crate::EffectProcessorFeedbackRequirements,
    ) -> std::result::Result<(), crate::EffectProcessorFeedbackError> {
        if requirements.occurrences().saturating_add(self.feedback.len())
            > crate::EFFECT_PROCESSOR_FEEDBACK_SLOTS
        {
            return Err(crate::EffectProcessorFeedbackError::Capacity);
        }
        self.planned_feedback_bytes = self.feedback_route_bytes(
            requirements.maximum_owned_bytes(),
            requirements.occurrences() != 0,
        );
        Ok(())
    }

    pub(crate) fn feedback_route_bytes(
        &self,
        additional_owned_bytes: usize,
        has_feedback: bool,
    ) -> usize {
        let slot_bytes = if has_feedback {
            crate::EFFECT_PROCESSOR_FEEDBACK_SLOTS
                * std::mem::size_of::<crate::EffectProcessorFeedbackReceipt>()
        } else {
            0
        };
        let current_slots =
            self.feedback.capacity() * std::mem::size_of::<crate::EffectProcessorFeedbackReceipt>();
        self.feedback_retained_bytes()
            .saturating_sub(current_slots)
            .saturating_add(slot_bytes.max(current_slots))
            .saturating_add(additional_owned_bytes)
            .saturating_add(self.observation.maximum_storage_bytes())
            .max(self.planned_feedback_bytes)
    }

    fn reserve_feedback(&mut self) -> std::result::Result<(), EffectProcessorHostResourceRefusal> {
        let slots = crate::EFFECT_PROCESSOR_FEEDBACK_SLOTS;
        let admitted_bytes = slots * std::mem::size_of::<crate::EffectProcessorFeedbackReceipt>();
        if self.feedback.capacity() < slots {
            self.feedback.try_reserve_exact(slots - self.feedback.len()).map_err(|_| {
                EffectProcessorHostResourceRefusal::FeedbackAllocation {
                    required_bytes: admitted_bytes,
                }
            })?;
        }
        let required_bytes = self
            .feedback
            .capacity()
            .checked_mul(std::mem::size_of::<crate::EffectProcessorFeedbackReceipt>())
            .ok_or(EffectProcessorHostResourceRefusal::FeedbackCapacity {
                required_bytes: usize::MAX,
                admitted_bytes,
            })?;
        if required_bytes > admitted_bytes {
            return Err(EffectProcessorHostResourceRefusal::FeedbackCapacity {
                required_bytes,
                admitted_bytes,
            });
        }
        Ok(())
    }

    /// Exact scheduler reason observed by this invocation, when stopped.
    pub const fn stop_reason(&self) -> Option<EffectProcessorStopReason> {
        self.stopped
    }
}

/// Conservative sizing across every association admitted by the provider.
///
/// Cold admission precedes negotiation; it must not invent one input format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectFloatProcessorSizing {
    /// Author-owned compiled occurrence.
    pub effect_id: EffectId,
    /// Raster width.
    pub width: u32,
    /// Raster height.
    pub height: u32,
    /// Every admitted association whose resident resources are covered.
    pub alpha_formats: crate::ColorFrameAlphaFormats,
}

/// Identity and exact format for one reusable processor occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectFloatProcessorPreparation {
    /// Exact immutable input format, including association and alpha numeric domain.
    pub format: crate::EffectValueFormat,
    /// Author-owned effect identity retained by the compiler.
    pub effect_id: EffectId,
    /// Input/output width in pixels.
    pub width: u32,
    /// Input/output height in pixels.
    pub height: u32,
}

/// A host resource denied before pixels or processor state advance.
///
/// This is not a plugin fault. Providers may return it only before executing
/// native/DSP actions and before changing the supplied complete frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectProcessorHostResourceRefusal {
    /// The actual caller could not pre-admit bounded receipt slots before DSP.
    #[error("host could not allocate {required_bytes} bytes for processor feedback")]
    FeedbackAllocation {
        /// Checked complete slot storage, before invoking a provider.
        required_bytes: usize,
    },
    /// Actual allocator capacity exceeded the explicit metadata storage policy.
    #[error("feedback capacity {required_bytes} bytes exceeds admitted {admitted_bytes} bytes")]
    FeedbackCapacity {
        /// Actual vector capacity bytes, excluding allocator usable-size.
        required_bytes: usize,
        /// Explicit complete slot-storage policy.
        admitted_bytes: usize,
    },
    /// The parent could not acquire its one complete output raster.
    #[error("host could not allocate {required_bytes} bytes for the output frame")]
    OutputAllocation {
        /// Checked byte size of the refused owned raster.
        required_bytes: usize,
    },
    /// The allocator returned a larger logical capacity than the output grant.
    #[error(
        "output storage capacity {required_bytes} bytes exceeds admitted {admitted_bytes} bytes"
    )]
    OutputCapacity {
        /// Actual Vec capacity, measured in RGBA storage bytes.
        required_bytes: usize,
        /// Output storage charged by this immutable processor request.
        admitted_bytes: usize,
    },
}

/// The resource bank's actual ownership after a recoverable invocation refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectProcessorRetention {
    /// The bank still owns the same valid instance within its optional grant.
    Retained,
    /// The instance was actually dropped; a later call must prepare again.
    Evicted,
}

/// Typed processor result, with one residency authority for host refusals.
#[derive(Debug, thiserror::Error)]
pub enum EffectProcessorInvocationError {
    /// A valid instance requires authored repair; this is not a native fault.
    #[error("processor is blocked by a persistent user error")]
    UserBlocked(Box<crate::EffectProcessorFeedback>),
    /// An execution/preparation invariant failed or the owner stopped.
    #[error(transparent)]
    Failed(#[from] MondrianError),
    /// No native/DSP action or staged pixel change occurred.
    #[error(transparent)]
    HostResourceRefused(EffectProcessorHostResourceRefusal),
}

/// Exact occurrence and actual optional-residency decision for a host refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectProcessorHostResourceRefusalReceipt {
    /// Immutable compiled occurrence, extent, and input format.
    pub preparation: EffectFloatProcessorPreparation,
    /// Immutable provider recipe revision used for the bank key.
    pub provider_revision: u64,
    /// Actual bank ownership after this invocation returned.
    pub retention: EffectProcessorRetention,
    /// Resource refused before native/DSP state advance.
    pub refusal: EffectProcessorHostResourceRefusal,
}

impl std::fmt::Display for EffectProcessorHostResourceRefusalReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.refusal, formatter)
    }
}

impl std::error::Error for EffectProcessorHostResourceRefusalReceipt {}

/// A reusable processor owned exclusively by one execution session.
///
/// Results must be independent of prior invocations. Instances may be evicted
/// or recreated between any two frames; sequential state requires the separate
/// continuity execution contract. Drop must retire every owned native worker.
pub trait EffectFloatProcessorInstance: std::fmt::Debug + Send {
    /// Reconcile closed GUI facts on this instance's exclusive owning worker.
    /// No render/DSP runs. Failure retires this owner through the same bank;
    /// it does not fabricate author work or quarantine an uninvolved provider.
    fn reconcile_gui(&mut self) -> Result<Option<crate::EffectProcessorGuiActionFeedback>> {
        Ok(None)
    }

    /// Conservative complete temporary GUI-action metadata, excluding this
    /// instance's already charged retained storage. The bank admits this before
    /// SDK action/decoding; unsupported resource grants retire through the same
    /// bank, never quarantine a valid provider or replay DSP.
    fn gui_action_storage_bytes(&self) -> usize {
        0
    }

    /// Process the complete frame with the current authored parameters.
    ///
    /// HostResourceRefused is permitted only before changing the supplied frame
    /// or advancing processor/native state. The scheduler still owns stop
    /// precedence, and the residency bank alone decides optional retention.
    fn process(
        &mut self,
        pixels: &mut crate::EffectFloatFrame,
        params: &serde_json::Value,
        frame_seed: i64,
        control: &mut EffectProcessorControl<'_>,
    ) -> std::result::Result<Option<crate::EffectProcessorFeedback>, EffectProcessorInvocationError>;
}

/// Checked conservative storage owned by one reusable processor recipe.
///
/// The caller's input, Effects staging/cache rasters and resident sibling
/// processors are charged separately by the sole execution ledger. Active peak
/// includes this instance's retained backing and its temporary/returned owners.
/// Retained bytes cover only what the instance still owns between invocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectFloatProcessorStorage {
    active_peak_bytes: usize,
    retained_bytes: usize,
}

impl EffectFloatProcessorStorage {
    /// Declare checked active and retained logical storage for one recipe.
    ///
    /// Retained storage cannot exceed the same instance's active peak. Both
    /// bounds must cover every admitted association and be deterministic for
    /// the exact sizing request and immutable provider revision.
    pub fn new(active_peak_bytes: usize, retained_bytes: usize) -> Result<Self> {
        if retained_bytes > active_peak_bytes {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_storage".to_owned(),
                reason: "retained processor storage exceeds its active peak".to_owned(),
            });
        }
        Ok(Self { active_peak_bytes, retained_bytes })
    }

    /// Maximum logical bytes owned during prepare or one invocation.
    pub const fn active_peak_bytes(self) -> usize {
        self.active_peak_bytes
    }

    /// Maximum logical bytes still owned between invocations.
    pub const fn retained_bytes(self) -> usize {
        self.retained_bytes
    }
}

/// Immutable recipe retained by a prepared program, without shared mutable
/// native instances. Each execution session prepares its own instance.
pub trait EffectFloatProcessorFactory: std::fmt::Debug + Send + Sync {
    /// Immutable input admission and complete output negotiation declaration.
    fn alpha_contract(&self) -> crate::EffectAlphaContract;
    /// Maximum returned feedback logical bytes, zero when this provider has none.
    /// Active storage covers provider-owned and returned bounded feedback;
    /// retained storage covers only metadata that the instance still owns.
    fn feedback_budget_bytes(&self) -> usize;
    /// Checked processor-owned active and retained storage for this request.
    /// Active peak is admitted before prepare and every reused invocation;
    /// only retained backing consumes optional between-call residency.
    fn storage(&self, request: EffectFloatProcessorSizing) -> Result<EffectFloatProcessorStorage>;
    /// Create one instance for this occurrence and exact frame extent.
    fn prepare(
        &self,
        request: EffectFloatProcessorPreparation,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<Box<dyn EffectFloatProcessorInstance>>;
}

/// Query immutable recipe sizing before any raster/instance allocation.
pub(crate) fn frame_processor_charge(
    op: &crate::EffectRenderOp,
    width: u32,
    height: u32,
) -> std::result::Result<usize, crate::EffectExecutionError> {
    let crate::EffectRenderOp::Custom { key, processor: Some(binding), .. } = op else {
        return Ok(0);
    };
    let Some(factory) = binding.float32_factory() else {
        return Ok(0);
    };
    let Some(effect_id) = binding.occurrence_id() else {
        return Err(crate::EffectExecutionError::CustomProcessorUnavailable { key: key.clone() });
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        factory.storage(EffectFloatProcessorSizing {
            effect_id,
            width,
            height,
            alpha_formats: binding.alpha_contract().inputs(),
        })
    }))
    .map_err(release_known_processor_panic);
    let reason = match result {
        Ok(Ok(storage)) => return Ok(storage.active_peak_bytes()),
        Ok(Err(error)) => error.to_string(),
        Err(_) => "custom processor sizing panicked".to_owned(),
    };
    binding.record_runtime_failure(reason.clone());
    Err(crate::EffectExecutionError::CustomProcessorFailed { key: key.clone(), reason })
}

/// Distinguish resource-policy denial from processor execution faults.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FloatProcessorError {
    #[error("processor feedback occurrence capacity {limit} is exhausted")]
    FeedbackCapacity { limit: usize },
    #[error("processor is blocked by a persistent user error")]
    UserBlocked(std::sync::Arc<crate::EffectProcessorFeedbackReceipt>),
    #[error("processor requires {required_bytes} bytes; available grant is {budget_bytes}")]
    Budget {
        required_bytes: usize,
        budget_bytes: usize,
    },
    #[error(transparent)]
    HostResourceRefused(EffectProcessorHostResourceRefusalReceipt),
    #[error(transparent)]
    Processor(#[from] MondrianError),
    #[error("processor changed frame extent from {expected:?} to {actual:?}")]
    OutputExtent {
        expected: crate::EffectFrameExtent,
        actual: crate::EffectFrameExtent,
    },
    #[error(transparent)]
    FrameContract(#[from] crate::EffectFloatFrameError),
}

// One completed provider admission for bank-backed and direct CPU providers.
// Extent/format/negotiation are metadata facts; sample storage uses the sole
// controlled validator and preserves the owner's exact sticky stop outcome.
pub(crate) fn validate_completed_processor_frame(
    pixels: &crate::EffectFloatFrame,
    extent: crate::EffectFrameExtent,
    format: crate::EffectValueFormat,
    contract: crate::EffectAlphaContract,
    control: &mut EffectProcessorControl<'_>,
) -> std::result::Result<(), FloatProcessorError> {
    control.check()?;
    if pixels.extent() != extent {
        return Err(FloatProcessorError::OutputExtent {
            expected: extent,
            actual: pixels.extent(),
        });
    }
    if pixels.format().domain() != format.domain()
        || pixels.format().precision() != format.precision()
    {
        return Err(crate::EffectFloatFrameError::FormatMismatch {
            expected: crate::EffectValueFormat::new(
                format.precision(),
                format.domain(),
                pixels.alpha(),
            ),
            actual: pixels.format(),
        }
        .into());
    }
    if !contract.admits_output(format.alpha(), pixels.alpha()) {
        return Err(crate::EffectFloatFrameError::ProcessorOutputContract {
            contract,
            input: format.alpha(),
            output: pixels.alpha(),
        }
        .into());
    }
    pixels
        .view()
        .validate_controlled(&mut || control.check().map_err(FloatProcessorError::from))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResourceKey {
    request: EffectFloatProcessorPreparation,
    revision: u64,
}

// Matches the product's opaque-payload abandonment policy: known string
// payloads have host-owned destructors; arbitrary plugin payloads do not. Never
// run an unknown destructor while unwinding or retiring this exclusive owner.
/// Release host-owned string panic payloads and abandon opaque plugin payloads.
/// Call this before any checkpoint or branch can drop the caught payload.
pub(crate) fn release_known_processor_panic(payload: Box<dyn std::any::Any + Send>) {
    if payload.is::<String>() || payload.is::<&'static str>() {
        drop(payload);
    } else {
        std::mem::forget(payload);
    }
}

#[derive(Debug, Default)]
pub(crate) struct FloatProcessorResidency {
    entries: VecDeque<(ResourceKey, usize, Box<dyn EffectFloatProcessorInstance>)>,
    bytes: usize,
}

impl FloatProcessorResidency {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub(crate) fn retire_occurrence(&mut self, effect_id: EffectId) {
        let mut index = 0;
        while index < self.entries.len() {
            if self.entries[index].0.request.effect_id == effect_id {
                if let Some((_, bytes, _)) = self.entries.remove(index) {
                    self.bytes -= bytes;
                }
            } else {
                index += 1;
            }
        }
    }

    pub(crate) fn reconcile_gui(&mut self, available_working_bytes: usize) -> usize {
        let mut retired = 0;
        let count = self.entries.len();
        for _ in 0..count {
            let Some((key, bytes, mut instance)) = self.entries.pop_front() else {
                break;
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let metadata_bytes = instance.gui_action_storage_bytes();
                if self
                    .bytes
                    .checked_add(metadata_bytes)
                    .is_none_or(|required| required > available_working_bytes)
                {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "effect_gui_resource_admission".to_owned(),
                        reason: "idle GUI action exceeds current processor working grant"
                            .to_owned(),
                    });
                }
                let action = instance.reconcile_gui()?;
                if let Some(action) = &action {
                    let actual = action
                        .feedback
                        .snapshot
                        .retained_bytes()
                        .saturating_add(
                            std::mem::size_of::<crate::EffectProcessorGuiActionFeedback>(),
                        )
                        .saturating_add(
                            std::mem::size_of::<crate::EffectProcessorGuiActionReceipt>(),
                        );
                    if action.action_serial == 0
                        || action.feedback.owner.0.is_nil()
                        || actual > metadata_bytes
                    {
                        return Err(MondrianError::WorkflowStepFailed {
                            step_id: "effect_gui_metadata_contract".to_owned(),
                            reason: "idle GUI action violated its admitted metadata contract"
                                .to_owned(),
                        });
                    }
                }
                Ok(action)
            }));
            let (healthy, action) = match result {
                Ok(Ok(action)) => (true, action),
                Ok(Err(_)) => (false, None),
                Err(payload) => {
                    release_known_processor_panic(payload);
                    (false, None)
                }
            };
            if healthy {
                self.entries.push_back((key, bytes, instance));
                if let Some(action) = action
                    && action.action_serial != 0
                    && let Some(presence) = action.presence.upgrade()
                {
                    let receipt = crate::EffectProcessorFeedbackReceipt {
                        effect_id: key.request.effect_id,
                        preparation: key.request,
                        provider_revision: key.revision,
                        retention: EffectProcessorRetention::Retained,
                        feedback: action.feedback,
                    };
                    // The instance is already retained by this same bank. No
                    // product callback decides native lifetime or DSP replay.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        presence.observe_action(crate::EffectProcessorGuiActionReceipt {
                            action_serial: action.action_serial,
                            receipt,
                        })
                    }));
                    if let Err(payload) = result {
                        release_known_processor_panic(payload);
                    }
                }
            } else {
                self.bytes -= bytes;
                retired += 1;
                if let Err(payload) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(instance)))
                {
                    release_known_processor_panic(payload);
                }
            }
        }
        retired
    }

    pub(crate) fn trim(&mut self, max_instances: usize, max_bytes: usize) {
        while self.entries.len() > max_instances || self.bytes > max_bytes {
            self.evict_oldest();
        }
    }

    pub(crate) fn trim_working(&mut self, available_bytes: usize) {
        while self.bytes > available_bytes {
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) {
        if let Some((_, bytes, _)) = self.entries.pop_front() {
            self.bytes -= bytes;
        }
    }

    fn retain_completed(
        &mut self,
        key: ResourceKey,
        retained: usize,
        instance: Box<dyn EffectFloatProcessorInstance>,
        retain: bool,
    ) -> EffectProcessorRetention {
        if retain {
            self.bytes += retained;
            self.entries.push_back((key, retained, instance));
            EffectProcessorRetention::Retained
        } else {
            drop(instance);
            EffectProcessorRetention::Evicted
        }
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.entries.len(), self.bytes)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process(
        &mut self,
        factory: &Arc<dyn EffectFloatProcessorFactory>,
        request: EffectFloatProcessorPreparation,
        revision: u64,
        max_instances: usize,
        max_bytes: usize,
        available_working_bytes: usize,
        pixels: &mut crate::EffectFloatFrame,
        params: &serde_json::Value,
        frame_seed: i64,
        control: &mut EffectProcessorControl<'_>,
    ) -> std::result::Result<usize, FloatProcessorError> {
        control.check()?;
        let count = (request.width as usize).checked_mul(request.height as usize);
        if request.width == 0
            || request.height == 0
            || count != Some(pixels.len())
            || request.format != pixels.format()
        {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_frame".to_owned(),
                reason: "processor frame extent does not match its pixels".to_owned(),
            }
            .into());
        }
        self.trim(max_instances, max_bytes);
        let key = ResourceKey { request, revision };
        // Dimension/revision changes retire this occurrence's obsolete worker.
        let mut index = 0;
        while index < self.entries.len() {
            if self.entries[index].0.request.effect_id == request.effect_id
                && self.entries[index].0 != key
            {
                if let Some((_, bytes, _)) = self.entries.remove(index) {
                    self.bytes -= bytes;
                }
            } else {
                index += 1;
            }
        }
        // Reuse never substitutes retained backing for this invocation's
        // active peak. Query the same immutable contract on every admission.
        let storage = factory.storage(EffectFloatProcessorSizing {
            effect_id: request.effect_id,
            width: request.width,
            height: request.height,
            alpha_formats: factory.alpha_contract().inputs(),
        })?;
        control.check()?;
        let active = storage.active_peak_bytes();
        let retained = storage.retained_bytes();
        if active > available_working_bytes {
            // No prepare/DSP ran. An existing valid idle owner may remain only
            // under both its optional ceiling and the current working grant.
            self.trim_working(available_working_bytes);
            return Err(FloatProcessorError::Budget {
                required_bytes: active,
                budget_bytes: available_working_bytes,
            });
        }
        let cached = self
            .entries
            .iter()
            .position(|entry| entry.0 == key)
            .and_then(|index| self.entries.remove(index));
        let cached_instance = if let Some((_, charge, instance)) = cached {
            self.bytes -= charge;
            if charge != retained {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "effect_processor_storage".to_owned(),
                    reason: "immutable provider changed its retained storage declaration"
                        .to_owned(),
                }
                .into());
            }
            Some(instance)
        } else {
            None
        };
        // Optional residency cannot reject an otherwise admitted invocation.
        let retain = max_instances > 0 && retained <= max_bytes;
        while !self.entries.is_empty()
            && (self.bytes > available_working_bytes - active
                || (retain
                    && (self.entries.len() >= max_instances || self.bytes > max_bytes - retained)))
        {
            self.evict_oldest();
        }
        let feedback_bound = factory.feedback_budget_bytes();
        if feedback_bound > active {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_feedback".to_owned(),
                reason: "feedback bound exceeds admitted processor charge".to_owned(),
            }
            .into());
        }
        if feedback_bound != 0 && control.feedback.len() == crate::EFFECT_PROCESSOR_FEEDBACK_SLOTS {
            return Err(FloatProcessorError::FeedbackCapacity {
                limit: crate::EFFECT_PROCESSOR_FEEDBACK_SLOTS,
            });
        }
        // Zero-feedback providers retain their original small-grant path.
        // Native/feedback providers acquire every observer slot before prepare
        // or DSP; resource refusal uses this same bank's real retention policy.
        let feedback_growth = if feedback_bound != 0 {
            let prior_bytes = control.feedback.capacity()
                * std::mem::size_of::<crate::EffectProcessorFeedbackReceipt>();
            control.check()?;
            let reserved = control.reserve_feedback();
            control.check()?; // Scheduler stop wins over a resource refusal.
            if let Err(refusal) = reserved {
                let retention = match cached_instance {
                    Some(instance) => self.retain_completed(key, retained, instance, retain),
                    None => EffectProcessorRetention::Evicted,
                };
                return Err(FloatProcessorError::HostResourceRefused(
                    EffectProcessorHostResourceRefusalReceipt {
                        preparation: request,
                        provider_revision: revision,
                        retention,
                        refusal,
                    },
                ));
            }
            control.feedback.capacity()
                * std::mem::size_of::<crate::EffectProcessorFeedbackReceipt>()
                - prior_bytes
        } else {
            0
        };
        let peak_bytes = self
            .bytes
            .checked_add(active)
            .and_then(|bytes| bytes.checked_add(feedback_growth))
            .ok_or(FloatProcessorError::Budget {
                required_bytes: usize::MAX,
                budget_bytes: available_working_bytes,
            })?;
        if peak_bytes > available_working_bytes {
            return Err(FloatProcessorError::Budget {
                required_bytes: peak_bytes,
                budget_bytes: available_working_bytes,
            });
        }
        // Eviction precedes native allocation. Resident siblings and this active
        // instance share the caller's exact remaining working grant.
        let mut instance = match cached_instance {
            Some(instance) => instance,
            None => factory.prepare(request, control)?,
        };
        let extent = pixels.extent();
        let format = pixels.format();
        control.check()?;
        control.current_occurrence = Some((request, revision));
        // Cleanup is local; the original panic is resumed to the existing
        // outer processor-isolation authority without another classification,
        // replay or retained owner. No stale borrowed occurrence survives it.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            instance.process(pixels, params, frame_seed, control)
        }));
        control.current_occurrence = None;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        // This same resource bank is the sole authority for every recoverable
        // disposition. No author/UI/native adapter keeps another instance.
        control.check()?;
        let bind = |feedback: crate::EffectProcessorFeedback, retention| {
            crate::EffectProcessorFeedbackReceipt {
                effect_id: request.effect_id,
                preparation: request,
                provider_revision: revision,
                retention,
                feedback,
            }
        };
        let feedback = match outcome {
            Ok(feedback) => feedback,
            Err(EffectProcessorInvocationError::Failed(error)) => return Err(error.into()),
            Err(EffectProcessorInvocationError::HostResourceRefused(refusal)) => {
                let retention = self.retain_completed(key, retained, instance, retain);
                return Err(FloatProcessorError::HostResourceRefused(
                    EffectProcessorHostResourceRefusalReceipt {
                        preparation: request,
                        provider_revision: revision,
                        retention,
                        refusal,
                    },
                ));
            }
            Err(EffectProcessorInvocationError::UserBlocked(feedback)) => {
                if !feedback.snapshot.is_blocked()
                    || feedback.snapshot.retained_bytes() > feedback_bound
                {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: "effect_processor_feedback".to_owned(),
                        reason: "processor reported an invalid blocked disposition".to_owned(),
                    }
                    .into());
                }
                let retention = self.retain_completed(key, retained, instance, retain);
                return Err(FloatProcessorError::UserBlocked(std::sync::Arc::new(bind(
                    *feedback, retention,
                ))));
            }
        };
        if feedback.as_ref().is_some_and(|value| {
            value.snapshot.is_blocked() || value.snapshot.retained_bytes() > feedback_bound
        }) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_feedback".to_owned(),
                reason: "processor output feedback violates its admitted disposition or byte bound"
                    .to_owned(),
            }
            .into());
        }
        validate_completed_processor_frame(
            pixels,
            extent,
            format,
            factory.alpha_contract(),
            control,
        )?;
        let retention = self.retain_completed(key, retained, instance, retain);
        if let Some(feedback) = feedback {
            control.feedback.push(bind(feedback, retention));
        }
        Ok(peak_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use mondrian_core::{automation::PropertyBag, TimelineTime, WorkingColorSpace};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn coverage_frame(width: u32, height: u32, pixels: Vec<[f32; 4]>) -> crate::EffectFloatFrame {
        crate::EffectFloatFrame::from_pixels(
            crate::EffectFrameExtent::new(width, height),
            crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            pixels,
        )
        .expect("complete normalized fixture")
    }

    #[derive(Debug, Default)]
    struct ProbeFactory {
        created: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        reached: Arc<AtomicBool>,
        cancellation: Option<mondrian_core::ExecutionCancellationToken>,
        resident_charge: Option<usize>,
    }
    #[derive(Debug)]
    struct ProbeInstance {
        dropped: Arc<AtomicUsize>,
        reached: Arc<AtomicBool>,
        cancellation: Option<mondrian_core::ExecutionCancellationToken>,
    }
    impl Drop for ProbeInstance {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl EffectFloatProcessorFactory for ProbeFactory {
        fn feedback_budget_bytes(&self) -> usize {
            0
        }
        fn alpha_contract(&self) -> crate::EffectAlphaContract {
            crate::EffectAlphaContract::COVERAGE_OPERATION
        }
        fn storage(
            &self,
            request: EffectFloatProcessorSizing,
        ) -> Result<EffectFloatProcessorStorage> {
            if request.width == 0 || request.height == 0 {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "fixture_sizing".to_owned(),
                    reason: "native instance requires nonempty frames".to_owned(),
                });
            }
            let bytes = self.resident_charge.unwrap_or(64);
            EffectFloatProcessorStorage::new(bytes, bytes)
        }
        fn prepare(
            &self,
            _: EffectFloatProcessorPreparation,
            control: &mut EffectProcessorControl<'_>,
        ) -> Result<Box<dyn EffectFloatProcessorInstance>> {
            control.check()?;
            self.created.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ProbeInstance {
                dropped: Arc::clone(&self.dropped),
                reached: Arc::clone(&self.reached),
                cancellation: self.cancellation.clone(),
            }))
        }
    }
    impl EffectFloatProcessorInstance for ProbeInstance {
        fn process(
            &mut self,
            pixels: &mut crate::EffectFloatFrame,
            params: &serde_json::Value,
            _: i64,
            control: &mut EffectProcessorControl<'_>,
        ) -> std::result::Result<
            Option<crate::EffectProcessorFeedback>,
            crate::EffectProcessorInvocationError,
        > {
            control.check()?;
            if params["panic"].as_bool() == Some(true) {
                panic!("intentional processor panic");
            }
            if params["fail"].as_bool() == Some(true) {
                pixels[0][0] = 999.0;
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "fixture".to_owned(),
                    reason: "intentional failure".to_owned(),
                }
                .into());
            }
            if params["cancel_without_owner"].as_bool() == Some(true) {
                pixels[0][0] = 999.0;
                return Err(MondrianError::Cancelled.into());
            }
            if params["resize"].as_bool() == Some(true) {
                *pixels = crate::EffectFloatFrame::from_pixels(
                    crate::EffectFrameExtent::new(0, 0),
                    pixels.format(),
                    Vec::new(),
                )
                .expect("owned malformed extent fixture");
                return Ok(None);
            }
            for pixel in pixels.pixels_mut() {
                pixel[0] *= 2.0;
            }
            self.reached.store(true, Ordering::SeqCst);
            if let Some(cancellation) = &self.cancellation {
                cancellation.cancel();
            }
            control.check()?;
            Ok(None)
        }
    }

    fn graph(factory: Arc<ProbeFactory>) -> Arc<CompiledEffectGraph> {
        graph_with_tail(factory, false)
    }

    fn register_probe_factory(factory: Arc<ProbeFactory>) -> String {
        let key = format!("test.resident.{}", EffectId::new());
        let definition = EffectDefinition::new(
            &key,
            "Reusable probe",
            PropertyBag::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Nondeterministic,
            roi_propagation: crate::EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_extension_contract(
            EffectExtensionContract::new("1.0.0").with_runtime_failure_policy(
                EffectExtensionRuntimeFailurePolicy::DisableDefinition,
            ),
        )
        .with_custom_float_processor_factory(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            EffectCachePolicy::Uncacheable,
            factory,
        );
        register_effect_definition(definition).expect("register factory");
        key
    }

    fn graph_with_tail(factory: Arc<ProbeFactory>, gpu_tail: bool) -> Arc<CompiledEffectGraph> {
        let key = register_probe_factory(factory);
        let first =
            instantiate_effect_node(EffectType::External(key.clone())).expect("first occurrence");
        let second = instantiate_effect_node(EffectType::External(key)).expect("second occurrence");
        let mut effects = vec![first, second];
        if gpu_tail {
            let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
            correction
                .set_static_value_by_parameter(
                    &EffectType::BasicCorrection
                        .parameter_id("exposure")
                        .expect("exposure parameter"),
                    mondrian_core::automation::PropertyValue::Float(0.5),
                )
                .expect("set GPU tail");
            effects.push(correction);
        }
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709)
            .expect("prepare factory graph")
            .evaluate(TimelineTime::ZERO)
            .expect("compile factory graph")
    }

    #[test]
    fn empty_scalar_frames_never_query_or_prepare_native_recipes() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(0));
        for (width, height) in [(0, 0), (0, 2), (3, 0)] {
            assert_eq!(
                session
                    .apply_compiled_rgba_f32(
                        crate::EffectFloatFrameView::new(
                            crate::EffectFrameExtent::new(width, height),
                            crate::EffectValueFormat::new(
                                crate::EffectWorkingPrecision::Float32,
                                crate::EffectColorDomain::SceneLinearRgb,
                                mondrian_core::ColorFrameAlpha::StraightCoverage
                            ),
                            &[]
                        ),
                        &graph,
                        0,
                        &mut crate::EffectProcessorControl::uncancelled()
                    )
                    .expect("no pixels require no native instance")
                    .pixels(),
                Vec::<[f32; 4]>::new()
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 0);
        assert_eq!(session.diagnostics().processor_instances, 0);
        for node in &graph.graph().nodes {
            if let EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Custom { key, .. }, ..
            } = &node.kind
            {
                let status = effect_extension_runtime_status(key).expect("status");
                assert!(!status.disabled);
                assert_eq!(status.last_error, None);
            }
        }
    }

    #[test]
    fn compiled_factory_instances_are_owner_exclusive_and_reuse_across_frame_generations() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut preview = EffectExecutionSession::default();
        let mut export = EffectExecutionSession::default();
        let input = [[0.25, 0.5, 0.75, 1.0]];
        for _ in 0..2 {
            assert_eq!(
                preview
                    .apply_compiled_rgba_f32(
                        crate::EffectFloatFrameView::new(
                            crate::EffectFrameExtent::new(1, 1),
                            crate::EffectValueFormat::new(
                                crate::EffectWorkingPrecision::Float32,
                                crate::EffectColorDomain::SceneLinearRgb,
                                mondrian_core::ColorFrameAlpha::StraightCoverage
                            ),
                            &input
                        ),
                        &graph,
                        0,
                        &mut crate::EffectProcessorControl::uncancelled()
                    )
                    .expect("preview")
                    .pixels(),
                [[1.0, 0.5, 0.75, 1.0]]
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        assert_eq!(preview.diagnostics().processor_instances, 2);
        assert_eq!(preview.diagnostics().processor_bytes, 128);
        export
            .apply_compiled_rgba_f32(
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    &input,
                ),
                &graph,
                0,
                &mut crate::EffectProcessorControl::uncancelled(),
            )
            .expect("export");
        assert_eq!(factory.created.load(Ordering::SeqCst), 4);
        for (generation, seed, source) in [(1, 7, 0.5), (2, -3, -0.25), (3, 99, 1.5)] {
            preview.bind_generation(generation);
            let identity = identity_compiled_effect_graph().expect("identity graph");
            assert_eq!(
                preview
                    .apply_compiled_rgba_f32(
                        crate::EffectFloatFrameView::new(
                            crate::EffectFrameExtent::new(1, 1),
                            crate::EffectValueFormat::new(
                                crate::EffectWorkingPrecision::Float32,
                                crate::EffectColorDomain::SceneLinearRgb,
                                mondrian_core::ColorFrameAlpha::StraightCoverage
                            ),
                            &[[source, 0.5, 0.75, 1.0]]
                        ),
                        &identity,
                        seed,
                        &mut EffectProcessorControl::uncancelled()
                    )
                    .expect("other clip's identity frame")
                    .pixels(),
                [[source, 0.5, 0.75, 1.0]]
            );
            assert_eq!(
                preview
                    .apply_compiled_rgba_f32(
                        crate::EffectFloatFrameView::new(
                            crate::EffectFrameExtent::new(1, 1),
                            crate::EffectValueFormat::new(
                                crate::EffectWorkingPrecision::Float32,
                                crate::EffectColorDomain::SceneLinearRgb,
                                mondrian_core::ColorFrameAlpha::StraightCoverage
                            ),
                            &[[source, 0.5, 0.75, 1.0]]
                        ),
                        &graph,
                        seed,
                        &mut EffectProcessorControl::uncancelled()
                    )
                    .expect("changed frame")
                    .pixels(),
                [[source * 4.0, 0.5, 0.75, 1.0]]
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 4);
        assert_eq!(preview.diagnostics().processor_instances, 2);
        assert_eq!(export.diagnostics().processor_instances, 2);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 0);
        preview.retire_processor_resources();
        assert_eq!(preview.diagnostics().processor_instances, 0);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 2);
        drop(export);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn factory_errors_panics_and_invalid_output_preserve_pixels_and_retire_instances() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut op = graph
            .graph()
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect {
                    op: op @ EffectRenderOp::Custom { .. }, ..
                } => Some(op.clone()),
                _ => None,
            })
            .expect("compiled factory operation");
        let mut session = EffectExecutionSession::default();
        let original = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
        for key in ["fail", "resize", "panic", "cancel_without_owner"] {
            if let EffectRenderOp::Custom { params, .. } = &mut op {
                *params = serde_json::json!({key: true});
            }
            let mut working = original.clone();
            assert!(crate::adjustment::apply_custom_render_op_f32(
                &mut working,
                &op,
                0,
                Some(&mut session),
                16,
                &mut EffectProcessorControl::uncancelled()
            )
            .is_err());
            assert_eq!(working, original);
            assert_eq!(session.diagnostics().processor_instances, 0);
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 4);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 4);
        let EffectRenderOp::Custom { key, .. } = &op else {
            unreachable!()
        };
        assert!(effect_extension_runtime_status(key).expect("plugin status").disabled);
    }

    #[test]
    fn stopped_processors_preserve_pixels_retire_instances_and_do_not_quarantine() {
        for reason in [
            EffectProcessorStopReason::Canceled,
            EffectProcessorStopReason::DeadlineExpired,
        ] {
            // Stop at preparation, during processing, and immediately before publication.
            for stop_at in [3, 6, 8] {
                let factory = Arc::new(ProbeFactory::default());
                let graph = graph(Arc::clone(&factory));
                let op = graph
                    .graph()
                    .nodes
                    .iter()
                    .find_map(|node| match &node.kind {
                        EffectGraphNodeKind::UnaryEffect {
                            op: op @ EffectRenderOp::Custom { .. },
                            ..
                        } => Some(op),
                        _ => None,
                    })
                    .expect("compiled operation");
                let EffectRenderOp::Custom { key, .. } = op else {
                    unreachable!()
                };
                let original = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
                let mut working = original.clone();
                let mut session = EffectExecutionSession::default();
                let mut polls = 0;
                let mut checkpoint = || {
                    polls += 1;
                    (polls == stop_at).then_some(reason)
                };
                let mut control = EffectProcessorControl::new(&mut checkpoint);
                let result = crate::adjustment::apply_custom_render_op_f32(
                    &mut working,
                    op,
                    0,
                    Some(&mut session),
                    16,
                    &mut control,
                );
                assert_eq!(
                    result,
                    Err(EffectExecutionError::CustomProcessorStopped { reason })
                );
                assert_eq!(working, original);
                assert_eq!(session.diagnostics().processor_instances, 0);
                assert_eq!(
                    factory.created.load(Ordering::SeqCst),
                    factory.dropped.load(Ordering::SeqCst)
                );
                // A later checkpoint cannot resume a stopped invocation.
                assert!(control.check().is_err());
                assert_eq!(control.stop_reason(), Some(reason));
                let status = effect_extension_runtime_status(key).expect("plugin status");
                assert!(!status.disabled);
                assert_eq!(status.last_error, None);
                crate::adjustment::apply_custom_render_op_f32(
                    &mut working,
                    op,
                    0,
                    Some(&mut session),
                    16,
                    &mut EffectProcessorControl::uncancelled(),
                )
                .expect("fresh invocation after cancel");
                assert_eq!(working.pixels(), [[0.5, 0.5, 0.75, 1.0]]);
                assert_eq!(session.diagnostics().processor_instances, 1);
            }
        }
    }

    fn mixed_work(graph: Arc<CompiledEffectGraph>) -> PreparedHeterogeneousEffectWork {
        let cpu = EffectExecutionLaneId::new(0);
        let gpu = EffectExecutionLaneId::new(1);
        let environment = EffectExecutionEnvironment::new(
            Arc::from([
                EffectExecutionLane::new(
                    cpu,
                    EffectProcessingBackend::Cpu,
                    EffectWorkingPrecisions::FLOAT32,
                    10,
                ),
                EffectExecutionLane::new(
                    gpu,
                    EffectProcessingBackend::Gpu,
                    EffectWorkingPrecisions::FLOAT32,
                    1,
                ),
            ]),
            Arc::from([EffectExecutionTransfer::new(
                cpu,
                EffectWorkingPrecision::Float32,
                gpu,
                EffectWorkingPrecision::Float32,
                2,
            )]),
        )
        .expect("execution lanes");
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        );
        PreparedHeterogeneousEffectWork::prepare(
            graph,
            &environment,
            EffectGraphExecutionRequest::new(
                EffectFrameExtent::new(1, 1),
                EffectValueResidency::new(cpu, format),
                EffectValueResidency::new(gpu, format),
                EffectGraphExecutionBudget::new(1024 * 1024, 1024 * 1024, 1024 * 1024, 128, 256),
            ),
        )
        .expect("prepare mixed route")
    }

    #[test]
    fn heterogeneous_cpu_prefix_reuses_session_owned_factory_instances() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph_with_tail(Arc::clone(&factory), true);
        let work = mixed_work(graph);
        assert_eq!(
            work.cpu_required_working_bytes(),
            96 + 2 * crate::EffectProcessorFeedbackBatch::storage_bytes(),
            "batch admission includes the active recipe"
        );
        assert_eq!(work.cpu_nodes().len(), 2);
        assert_eq!(work.gpu_suffix().node_ids().len(), 1);
        let mut session = EffectExecutionSession::default();
        session.bind_generation(1);
        for _ in 0..2 {
            let completion = work
                .execute_cpu_prefix_uncancelled(
                    &mut session,
                    1,
                    crate::EffectFloatFrameView::new(
                        crate::EffectFrameExtent::new(1, 1),
                        crate::EffectValueFormat::new(
                            crate::EffectWorkingPrecision::Float32,
                            crate::EffectColorDomain::SceneLinearRgb,
                            mondrian_core::ColorFrameAlpha::StraightCoverage,
                        ),
                        &[[0.25, 0.5, 0.75, 1.0]],
                    ),
                    0,
                    WorkingColorSpace::LinearRec709,
                )
                .expect("execute factory CPU prefix");
            assert_eq!(
                completion.boundary_values()[0].pixels(),
                &[[1.0, 0.5, 0.75, 1.0]]
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        assert_eq!(session.diagnostics().processor_instances, 2);
        for reason in [
            HeterogeneousCpuExecutionStopReason::Canceled,
            HeterogeneousCpuExecutionStopReason::DeadlineExpired,
        ] {
            factory.reached.store(false, Ordering::SeqCst);
            let mut stopped_session = EffectExecutionSession::default();
            stopped_session.bind_generation(42);
            let input = [[0.25, 0.5, 0.75, 1.0]];
            let result = work.execute_cpu_prefix_with_checkpoint(
                &mut stopped_session,
                42,
                crate::EffectFloatFrameView::new(
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectValueFormat::new(
                        crate::EffectWorkingPrecision::Float32,
                        crate::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    &input,
                ),
                0,
                WorkingColorSpace::LinearRec709,
                || factory.reached.load(Ordering::SeqCst).then_some(reason),
            );
            assert!(matches!(result,
                Err(PreparedHeterogeneousEffectWorkError::ExecutionStopped { generation: 42, reason: observed }) if observed == reason));
            assert_eq!(input, [[0.25, 0.5, 0.75, 1.0]]);
            assert_eq!(stopped_session.diagnostics().processor_instances, 0);
        }
        // Canceled prefixes leave the definition available for a fresh generation.
        work.execute_cpu_prefix_uncancelled(
            &mut session,
            1,
            crate::EffectFloatFrameView::new(
                crate::EffectFrameExtent::new(1, 1),
                crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                &[[0.25, 0.5, 0.75, 1.0]],
            ),
            0,
            WorkingColorSpace::LinearRec709,
        )
        .expect("fresh prefix after stops");
    }

    #[test]
    fn temporal_processor_cancellation_preserves_owner_result_and_plugin_availability() {
        let cancellation = mondrian_core::ExecutionCancellationToken::new();
        let factory = Arc::new(ProbeFactory {
            cancellation: Some(cancellation.clone()),
            ..ProbeFactory::default()
        });
        let graph = graph(Arc::clone(&factory));
        let mut session = EffectExecutionSession::default();
        let request = EffectTemporalExecutionRequest::new(
            42,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ZERO,
            EffectFrameExtent::new(1, 1),
            EffectPixelRoi::new(0, 0, 1, 1),
            cancellation,
        );
        assert!(matches!(
            session.execute_temporal_f32(&graph, &request, &mut ConstantProvider),
            Err(EffectTemporalExecutionError::Canceled)
        ));
        assert!(factory.reached.load(Ordering::SeqCst));
        assert_eq!(session.diagnostics().processor_instances, 0);
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 1);
        for node in &graph.graph().nodes {
            if let EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Custom { key, .. }, ..
            } = &node.kind
            {
                let status = effect_extension_runtime_status(key).expect("plugin status");
                assert!(!status.disabled);
                assert_eq!(status.last_error, None);
            }
        }
    }

    struct ConstantProvider;

    impl EffectTemporalFrameProvider for ConstantProvider {
        fn source_identity(&self) -> EffectTemporalSourceIdentity {
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([19; 32])
        }

        fn retained_coverage_bytes(&self) -> usize {
            0
        }

        fn copy_frame(
            &mut self,
            request: EffectTemporalFrameRequest,
            cancellation: &mondrian_core::ExecutionCancellationToken,
            destination: &mut Vec<[f32; 4]>,
        ) -> std::result::Result<(), EffectTemporalFrameProviderError> {
            if cancellation.is_canceled() {
                return Err(EffectTemporalFrameProviderError::Canceled);
            }
            let roi = request.input_roi().region();
            *destination = vec![[0.25, 0.5, 0.75, 1.0]; (roi.width() * roi.height()) as usize];
            Ok(())
        }
    }

    #[test]
    fn temporal_execution_reuses_independent_instances_across_frame_generations() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut session = EffectExecutionSession::default();
        let extent = EffectFrameExtent::new(2, 3);
        for generation in [1, 1, 2] {
            let request = EffectTemporalExecutionRequest::new(
                generation,
                EffectExecutionContinuity::Continuous,
                TimelineTime::ZERO,
                extent,
                EffectPixelRoi::new(0, 0, 2, 3),
                mondrian_core::ExecutionCancellationToken::new(),
            );
            let output = session
                .execute_temporal_f32(&graph, &request, &mut ConstantProvider)
                .expect("execute reusable temporal processor");
            assert_eq!(output.tile().pixels(), &[[1.0, 0.5, 0.75, 1.0]; 6]);
            assert_eq!(session.diagnostics().processor_instances, 2);
            assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        }
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 0);
        drop(session);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn scalar_temporal_and_mixed_routes_share_pixel_and_processor_admission() {
        for budget in [95, 96, 160] {
            for route in ["scalar", "temporal", "mixed"] {
                let factory = Arc::new(ProbeFactory::default());
                let graph = graph_with_tail(Arc::clone(&factory), route == "mixed");
                let working_budget = if route == "mixed" {
                    budget + 2 * crate::EffectProcessorFeedbackBatch::storage_bytes()
                } else {
                    budget
                };
                let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
                    max_working_bytes: working_budget,
                    ..EffectExecutionSessionConfig::default()
                });
                session.bind_generation(1);
                let input = [[0.25, 0.5, 0.75, 1.0]];
                let result = match route {
                    "scalar" => session
                        .apply_compiled_rgba_f32(
                            crate::EffectFloatFrameView::new(
                                crate::EffectFrameExtent::new(1, 1),
                                crate::EffectValueFormat::new(
                                    crate::EffectWorkingPrecision::Float32,
                                    crate::EffectColorDomain::SceneLinearRgb,
                                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                                ),
                                &input,
                            ),
                            &graph,
                            0,
                            &mut crate::EffectProcessorControl::uncancelled(),
                        )
                        .map(|frame| frame.into_parts().2)
                        .map_err(|error| format!("{error:?}")),
                    "temporal" => {
                        let request = EffectTemporalExecutionRequest::new(
                            1,
                            EffectExecutionContinuity::Continuous,
                            TimelineTime::ZERO,
                            EffectFrameExtent::new(1, 1),
                            EffectPixelRoi::new(0, 0, 1, 1),
                            mondrian_core::ExecutionCancellationToken::new(),
                        );
                        session
                            .execute_temporal_f32(&graph, &request, &mut ConstantProvider)
                            .map(|output| {
                                assert_eq!(output.peak_working_bytes(), budget);
                                output.tile().pixels().to_vec()
                            })
                            .map_err(|error| format!("{error:?}"))
                    }
                    "mixed" => mixed_work(Arc::clone(&graph))
                        .execute_cpu_prefix_uncancelled(
                            &mut session,
                            1,
                            crate::EffectFloatFrameView::new(
                                crate::EffectFrameExtent::new(1, 1),
                                crate::EffectValueFormat::new(
                                    crate::EffectWorkingPrecision::Float32,
                                    crate::EffectColorDomain::SceneLinearRgb,
                                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                                ),
                                &input,
                            ),
                            0,
                            WorkingColorSpace::LinearRec709,
                        )
                        .map(|output| output.boundary_values()[0].pixels().to_vec())
                        .map_err(|error| format!("{error:?}")),
                    _ => unreachable!(),
                };
                assert_eq!(input, [[0.25, 0.5, 0.75, 1.0]]);
                if budget == 95 {
                    assert!(result.is_err(), "{route}: {result:?}");
                    assert_eq!(factory.created.load(Ordering::SeqCst), 0);
                    for node in &graph.graph().nodes {
                        if let EffectGraphNodeKind::UnaryEffect {
                            op: EffectRenderOp::Custom { key, .. },
                            ..
                        } = &node.kind
                        {
                            assert!(
                                !effect_extension_runtime_status(key).expect("status").disabled
                            );
                        }
                    }
                } else {
                    assert_eq!(result.expect(route), [[1.0, 0.5, 0.75, 1.0]]);
                    let retained = if budget == 96 { 1 } else { 2 };
                    assert_eq!(session.diagnostics().processor_instances, retained);
                    assert_eq!(session.diagnostics().processor_bytes, retained * 64);
                    assert_eq!(factory.created.load(Ordering::SeqCst), 2);
                    assert_eq!(factory.dropped.load(Ordering::SeqCst), 2 - retained);
                }
            }
        }
    }

    #[test]
    fn later_recipe_budget_denial_precedes_all_instances_and_media_fetches() {
        struct CountingProvider(usize);
        impl EffectTemporalFrameProvider for CountingProvider {
            fn source_identity(&self) -> EffectTemporalSourceIdentity {
                ConstantProvider.source_identity()
            }
            fn retained_coverage_bytes(&self) -> usize {
                0
            }
            fn copy_frame(
                &mut self,
                request: EffectTemporalFrameRequest,
                cancellation: &mondrian_core::ExecutionCancellationToken,
                destination: &mut Vec<[f32; 4]>,
            ) -> std::result::Result<(), EffectTemporalFrameProviderError> {
                self.0 += 1;
                ConstantProvider.copy_frame(request, cancellation, destination)
            }
        }
        for route in ["scalar", "temporal", "mixed"] {
            let first = Arc::new(ProbeFactory::default());
            let second = Arc::new(ProbeFactory {
                resident_charge: Some(128),
                ..ProbeFactory::default()
            });
            let keys = [
                register_probe_factory(Arc::clone(&first)),
                register_probe_factory(Arc::clone(&second)),
            ];
            let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
            correction
                .set_static_value_by_parameter(
                    &EffectType::BasicCorrection.parameter_id("exposure").expect("exposure"),
                    mondrian_core::automation::PropertyValue::Float(0.5),
                )
                .expect("set GPU tail");
            let effects = [
                instantiate_effect_node(EffectType::External(keys[0].clone())).expect("first"),
                instantiate_effect_node(EffectType::External(keys[1].clone())).expect("second"),
                correction,
            ];
            let graph =
                PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709)
                    .expect("prepare")
                    .evaluate(TimelineTime::ZERO)
                    .expect("compile");
            let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
                max_working_bytes: 159,
                ..EffectExecutionSessionConfig::default()
            });
            session.bind_generation(1);
            let input = [[0.25, 0.5, 0.75, 1.0]];
            let mut provider = CountingProvider(0);
            let denied = match route {
                "scalar" => session
                    .apply_compiled_rgba_f32(
                        crate::EffectFloatFrameView::new(
                            crate::EffectFrameExtent::new(1, 1),
                            crate::EffectValueFormat::new(
                                crate::EffectWorkingPrecision::Float32,
                                crate::EffectColorDomain::SceneLinearRgb,
                                mondrian_core::ColorFrameAlpha::StraightCoverage,
                            ),
                            &input,
                        ),
                        &graph,
                        0,
                        &mut crate::EffectProcessorControl::uncancelled(),
                    )
                    .is_err(),
                "temporal" => {
                    let request = EffectTemporalExecutionRequest::new(
                        1,
                        EffectExecutionContinuity::Continuous,
                        TimelineTime::ZERO,
                        EffectFrameExtent::new(1, 1),
                        EffectPixelRoi::new(0, 0, 1, 1),
                        mondrian_core::ExecutionCancellationToken::new(),
                    );
                    session.execute_temporal_f32(&graph, &request, &mut provider).is_err()
                }
                "mixed" => mixed_work(Arc::clone(&graph))
                    .execute_cpu_prefix_uncancelled(
                        &mut session,
                        1,
                        crate::EffectFloatFrameView::new(
                            crate::EffectFrameExtent::new(1, 1),
                            crate::EffectValueFormat::new(
                                crate::EffectWorkingPrecision::Float32,
                                crate::EffectColorDomain::SceneLinearRgb,
                                mondrian_core::ColorFrameAlpha::StraightCoverage,
                            ),
                            &input,
                        ),
                        0,
                        WorkingColorSpace::LinearRec709,
                    )
                    .is_err(),
                _ => unreachable!(),
            };
            assert!(denied, "{route}");
            assert_eq!(
                first.created.load(Ordering::SeqCst),
                0,
                "{route}: earlier recipe must not start"
            );
            assert_eq!(second.created.load(Ordering::SeqCst), 0, "{route}");
            assert_eq!(provider.0, 0, "{route}: media must not be fetched");
            assert_eq!(session.diagnostics().processor_instances, 0);
            for key in keys {
                let status = effect_extension_runtime_status(&key).expect("status");
                assert!(!status.disabled);
                assert_eq!(status.last_error, None);
            }
        }
    }

    #[test]
    fn working_budget_denial_preserves_definition_and_allows_reconfigured_retry() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let op = graph
            .graph()
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect {
                    op: op @ EffectRenderOp::Custom { .. }, ..
                } => Some(op),
                _ => None,
            })
            .expect("operation");
        let EffectRenderOp::Custom { key, .. } = op else {
            unreachable!()
        };
        let mut config = EffectExecutionSessionConfig {
            max_working_bytes: 63,
            ..EffectExecutionSessionConfig::default()
        };
        let mut session = EffectExecutionSession::new(config);
        let original = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
        let mut pixels = original.clone();
        assert_eq!(
            crate::adjustment::apply_custom_render_op_f32(
                &mut pixels,
                op,
                0,
                Some(&mut session),
                16,
                &mut EffectProcessorControl::uncancelled()
            ),
            Err(EffectExecutionError::CustomProcessorBudgetExceeded {
                key: key.clone(),
                required_bytes: 64,
                budget_bytes: 31
            })
        );
        assert_eq!(pixels, original);
        assert_eq!(factory.created.load(Ordering::SeqCst), 0);
        let status = effect_extension_runtime_status(key).expect("runtime status");
        assert!(!status.disabled);
        assert_eq!(status.last_error, None);
        config.max_working_bytes = 128;
        session.reconfigure(config);
        crate::adjustment::apply_custom_render_op_f32(
            &mut pixels,
            op,
            0,
            Some(&mut session),
            16,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("retry with sufficient grant");
        assert_eq!(pixels.pixels(), [[0.5, 0.5, 0.75, 1.0]]);
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn oversized_optional_residency_bypasses_cache_and_active_charge_retires_siblings() {
        let concrete = Arc::new(ProbeFactory::default());
        let factory: Arc<dyn EffectFloatProcessorFactory> = concrete.clone();
        let mut bank = FloatProcessorResidency::default();
        let run = |bank: &mut FloatProcessorResidency, request, retention, working| {
            bank.process(
                &factory,
                request,
                1,
                8,
                retention,
                working,
                &mut coverage_frame(
                    request.width,
                    request.height,
                    vec![[0.25; 4]; request.width as usize * request.height as usize],
                ),
                &serde_json::json!({}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("admitted process");
        };
        let first = EffectFloatProcessorPreparation {
            format: crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            effect_id: EffectId::new(),
            width: 1,
            height: 1,
        };
        run(&mut bank, first, 63, 128);
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(concrete.created.load(Ordering::SeqCst), 1);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 1);
        run(&mut bank, first, 256, 128);
        let second = EffectFloatProcessorPreparation { effect_id: EffectId::new(), ..first };
        run(&mut bank, second, 256, 128);
        assert_eq!(bank.counts(), (2, 128));
        let third = EffectFloatProcessorPreparation { effect_id: EffectId::new(), ..first };
        run(&mut bank, third, 256, 128);
        assert_eq!(bank.counts(), (2, 128));
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 2);
        assert!(!bank.entries.iter().any(|entry| entry.0.request == first));
        // Reuse under a smaller active grant must evict the other resident
        // before entering the cached instance, just like fresh preparation.
        run(&mut bank, third, 256, 64);
        assert_eq!(bank.counts(), (1, 64));
        assert_eq!(concrete.created.load(Ordering::SeqCst), 4);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn residency_evicts_lru_revisions_and_uncached_instances() {
        let concrete = Arc::new(ProbeFactory::default());
        let factory: Arc<dyn EffectFloatProcessorFactory> = concrete.clone();
        let mut bank = FloatProcessorResidency::default();
        let first = EffectFloatProcessorPreparation {
            format: crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            effect_id: EffectId::new(),
            width: 1,
            height: 1,
        };
        let second = EffectFloatProcessorPreparation { effect_id: EffectId::new(), ..first };
        let third = EffectFloatProcessorPreparation { effect_id: EffectId::new(), ..first };
        let run = |bank: &mut FloatProcessorResidency, request, revision, count, bytes| {
            bank.process(
                &factory,
                request,
                revision,
                count,
                bytes,
                1024,
                &mut coverage_frame(
                    request.width,
                    request.height,
                    vec![[0.25; 4]; request.width as usize * request.height as usize],
                ),
                &serde_json::json!({}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("bounded processor");
        };
        run(&mut bank, first, 1, 2, 128);
        run(&mut bank, second, 1, 2, 128);
        run(&mut bank, first, 1, 2, 128);
        run(&mut bank, third, 1, 2, 128);
        assert_eq!(concrete.created.load(Ordering::SeqCst), 3);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 1);
        assert!(bank.entries.iter().all(|entry| entry.0.request != second));
        run(&mut bank, first, 2, 2, 128);
        assert_eq!(concrete.created.load(Ordering::SeqCst), 4);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 2);
        assert!(bank
            .entries
            .iter()
            .all(|entry| entry.0.request != first || entry.0.revision == 2));
        bank.trim(1, 64);
        assert_eq!(bank.counts(), (1, 64));
        bank.trim(0, 0);
        run(&mut bank, first, 2, 0, 0);
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(concrete.created.load(Ordering::SeqCst), 5);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn residency_rejects_ungranted_allocation_and_retires_errors_and_extent_changes() {
        let concrete = Arc::new(ProbeFactory::default());
        let factory: Arc<dyn EffectFloatProcessorFactory> = concrete.clone();
        let mut bank = FloatProcessorResidency::default();
        let request = EffectFloatProcessorPreparation {
            format: crate::EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            effect_id: EffectId::new(),
            width: 1,
            height: 1,
        };
        let mut pixels = coverage_frame(1, 1, vec![[1.0; 4]]);
        assert!(bank
            .process(
                &factory,
                request,
                1,
                1,
                128,
                63,
                &mut pixels,
                &serde_json::json!({}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .is_err());
        assert_eq!(concrete.created.load(Ordering::SeqCst), 0);
        bank.process(
            &factory,
            request,
            1,
            1,
            64,
            1024,
            &mut pixels,
            &serde_json::json!({}),
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("admit");
        assert!(bank
            .process(
                &factory,
                request,
                1,
                1,
                64,
                1024,
                &mut pixels,
                &serde_json::json!({"fail": true}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .is_err());
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 1);
        pixels = coverage_frame(1, 1, vec![[1.0; 4]]);
        assert!(bank
            .process(
                &factory,
                request,
                1,
                1,
                64,
                1024,
                &mut pixels,
                &serde_json::json!({"resize": true}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .is_err());
        assert_eq!(bank.counts(), (0, 0));
        pixels = coverage_frame(1, 1, vec![[1.0; 4]]);
        bank.process(
            &factory,
            request,
            1,
            1,
            64,
            1024,
            &mut pixels,
            &serde_json::json!({}),
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("recreate");
        pixels = coverage_frame(2, 1, vec![[1.0; 4]; 2]);
        let changed = EffectFloatProcessorPreparation { width: 2, ..request };
        bank.process(
            &factory,
            changed,
            1,
            1,
            64,
            1024,
            &mut pixels,
            &serde_json::json!({}),
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("changed extent");
        assert_eq!(bank.counts(), (1, 64));
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 3);
        bank.trim(0, 0);
        assert_eq!(bank.counts(), (0, 0));
    }
    #[derive(Debug, Default)]
    struct AssociationProbeFactory {
        calls: Arc<AtomicUsize>,
        created: Arc<AtomicUsize>,
        emission: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct AssociationProbeInstance {
        calls: Arc<AtomicUsize>,
        emission: Arc<AtomicBool>,
    }

    impl EffectFloatProcessorFactory for AssociationProbeFactory {
        fn feedback_budget_bytes(&self) -> usize {
            0
        }
        fn alpha_contract(&self) -> EffectAlphaContract {
            EffectAlphaContract::NATIVE_FLOAT
        }
        fn storage(&self, _: EffectFloatProcessorSizing) -> Result<EffectFloatProcessorStorage> {
            EffectFloatProcessorStorage::new(64, 64)
        }
        fn prepare(
            &self,
            _: EffectFloatProcessorPreparation,
            control: &mut EffectProcessorControl<'_>,
        ) -> Result<Box<dyn EffectFloatProcessorInstance>> {
            control.check()?;
            self.created.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(AssociationProbeInstance {
                calls: Arc::clone(&self.calls),
                emission: Arc::clone(&self.emission),
            }))
        }
    }

    impl EffectFloatProcessorInstance for AssociationProbeInstance {
        fn process(
            &mut self,
            frame: &mut EffectFloatFrame,
            _: &serde_json::Value,
            _: i64,
            control: &mut EffectProcessorControl<'_>,
        ) -> std::result::Result<
            Option<crate::EffectProcessorFeedback>,
            crate::EffectProcessorInvocationError,
        > {
            control.check()?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            let sample = if self.emission.load(Ordering::SeqCst) {
                [7.0, -3.0, 11.0, 0.0]
            } else {
                [0.25, 0.5, 0.75, 0.5]
            };
            frame.pixels_mut().fill(sample);
            frame.set_format_after_write(
                frame.format().with_alpha(mondrian_core::ColorFrameAlpha::PremultipliedFloat),
            );
            control.check()?;
            Ok(None)
        }
    }

    #[test]
    fn finite_coverage_frontier_refuses_emission_once_and_retains_valid_processor_state() {
        let factory = Arc::new(AssociationProbeFactory::default());
        factory.emission.store(true, Ordering::SeqCst);
        let key = format!("test.association-frontier.{}", EffectId::new());
        register_effect_definition(
            EffectDefinition::new(
                &key,
                "Association frontier",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Nondeterministic,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_extension_contract(
                EffectExtensionContract::new("1.0.0").with_runtime_failure_policy(
                    EffectExtensionRuntimeFailurePolicy::DisableDefinition,
                ),
            )
            .with_custom_float_processor_factory(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                EffectCachePolicy::Uncacheable,
                factory.clone(),
            ),
        )
        .expect("register typed probe");
        let native =
            instantiate_effect_node(EffectType::External(key.clone())).expect("native occurrence");
        let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
        correction
            .set_static_value_by_parameter(
                &EffectType::BasicCorrection
                    .parameter_id("exposure")
                    .expect("exposure parameter"),
                mondrian_core::automation::PropertyValue::Float(0.5),
            )
            .expect("nonidentity GPU tail");
        let graph = PreparedEffectProgram::prepare(
            &[native, correction],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare providers")
        .evaluate(TimelineTime::ZERO)
        .expect("canonical compile");
        let work = mixed_work(graph);
        assert_eq!(work.cpu_nodes().len(), 1, "one actual native dispatch");
        assert_eq!(
            work.gpu_suffix().node_ids().len(),
            1,
            "one nonidentity GPU tail"
        );
        let input = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
        let mut session = EffectExecutionSession::default();
        session.bind_generation(17);
        assert!(matches!(
            work.execute_cpu_prefix_uncancelled(
                &mut session,
                17,
                input.view(),
                0,
                WorkingColorSpace::LinearRec709
            ),
            Err(PreparedHeterogeneousEffectWorkError::FrameContract(
                EffectFloatFrameError::AlphaConversion {
                    reason: EffectAlphaConversionFailure::ZeroAlphaEmission,
                    ..
                }
            ))
        ));
        assert_eq!(
            factory.calls.load(Ordering::SeqCst),
            1,
            "the failed route cannot replay a stateful provider"
        );
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
        assert_eq!(
            session.diagnostics().processor_instances,
            1,
            "valid raw native state is not a plugin failure"
        );
        let status = effect_extension_runtime_status(&key).expect("provider status");
        assert!(!status.disabled);
        assert_eq!(status.last_error, None);
        factory.emission.store(false, Ordering::SeqCst);
        let admitted = work
            .execute_cpu_prefix_uncancelled(
                &mut session,
                17,
                input.view(),
                1,
                WorkingColorSpace::LinearRec709,
            )
            .expect("ordinary native coverage materializes to the existing GPU route");
        assert_eq!(factory.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            factory.created.load(Ordering::SeqCst),
            1,
            "same prepared native state survives route refusal"
        );
        assert_eq!(
            admitted.boundary_values()[0].pixels(),
            &[[0.5, 1.0, 1.5, 0.5]]
        );
        assert_eq!(
            admitted.boundary_values()[0].frame().format().alpha(),
            mondrian_core::ColorFrameAlpha::StraightCoverage
        );
        assert_eq!(
            admitted.evidence().transfers()[0].alpha_admission(),
            EffectAlphaMaterializationAdmission::FiniteStraightCoverage
        );
    }

    #[derive(Debug, Default)]
    struct ResourceProbeFactory {
        created: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        dsp_calls: Arc<AtomicUsize>,
        observed_instance: Arc<AtomicUsize>,
        refuse: Arc<AtomicBool>,
        refusal_observed: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct ResourceProbeInstance {
        id: usize,
        dropped: Arc<AtomicUsize>,
        dsp_calls: Arc<AtomicUsize>,
        observed_instance: Arc<AtomicUsize>,
        refuse: Arc<AtomicBool>,
        refusal_observed: Arc<AtomicBool>,
    }

    impl Drop for ResourceProbeInstance {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl EffectFloatProcessorFactory for ResourceProbeFactory {
        fn feedback_budget_bytes(&self) -> usize {
            0
        }
        fn alpha_contract(&self) -> EffectAlphaContract {
            EffectAlphaContract::STRAIGHT_COVERAGE
        }
        fn storage(&self, _: EffectFloatProcessorSizing) -> Result<EffectFloatProcessorStorage> {
            EffectFloatProcessorStorage::new(64, 64)
        }
        fn prepare(
            &self,
            _: EffectFloatProcessorPreparation,
            control: &mut EffectProcessorControl<'_>,
        ) -> Result<Box<dyn EffectFloatProcessorInstance>> {
            control.check()?;
            Ok(Box::new(ResourceProbeInstance {
                id: self.created.fetch_add(1, Ordering::SeqCst) + 1,
                dropped: self.dropped.clone(),
                dsp_calls: self.dsp_calls.clone(),
                observed_instance: self.observed_instance.clone(),
                refuse: self.refuse.clone(),
                refusal_observed: self.refusal_observed.clone(),
            }))
        }
    }
    impl EffectFloatProcessorInstance for ResourceProbeInstance {
        fn process(
            &mut self,
            pixels: &mut EffectFloatFrame,
            _: &serde_json::Value,
            _: i64,
            control: &mut EffectProcessorControl<'_>,
        ) -> std::result::Result<
            Option<crate::EffectProcessorFeedback>,
            crate::EffectProcessorInvocationError,
        > {
            control.check()?;
            self.observed_instance.store(self.id, Ordering::SeqCst);
            if self.refuse.load(Ordering::SeqCst) {
                self.refusal_observed.store(true, Ordering::SeqCst);
                return Err(EffectProcessorInvocationError::HostResourceRefused(
                    EffectProcessorHostResourceRefusal::OutputAllocation {
                        required_bytes: pixels.len() * 16,
                    },
                ));
            }
            self.dsp_calls.fetch_add(1, Ordering::SeqCst);
            pixels.pixels_mut()[0][0] *= 2.0;
            Ok(None)
        }
    }
    fn resource_request(frame: &EffectFloatFrame) -> EffectFloatProcessorPreparation {
        EffectFloatProcessorPreparation {
            effect_id: EffectId::new(),
            width: frame.width(),
            height: frame.height(),
            format: frame.format(),
        }
    }

    #[test]
    fn host_resource_refusal_retains_exact_bank_owner_without_pixels_or_dsp_advance() {
        let provider = Arc::new(ResourceProbeFactory::default());
        provider.refuse.store(true, Ordering::SeqCst);
        let factory: Arc<dyn EffectFloatProcessorFactory> = provider.clone();
        let original = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
        let request = resource_request(&original);
        let mut frame = original.clone();
        let mut bank = FloatProcessorResidency::default();
        let result = bank.process(
            &factory,
            request,
            77,
            1,
            64,
            64,
            &mut frame,
            &serde_json::json!({}),
            0,
            &mut EffectProcessorControl::uncancelled(),
        );
        let Err(FloatProcessorError::HostResourceRefused(receipt)) = result else {
            panic!("typed pre-execution refusal required");
        };
        assert_eq!(receipt.preparation, request);
        assert_eq!(receipt.provider_revision, 77);
        assert_eq!(receipt.retention, EffectProcessorRetention::Retained);
        assert_eq!(
            receipt.refusal,
            EffectProcessorHostResourceRefusal::OutputAllocation { required_bytes: 16 }
        );
        assert_eq!(frame, original);
        assert_eq!(bank.counts(), (1, 64));
        assert_eq!(provider.dsp_calls.load(Ordering::SeqCst), 0);
        assert_eq!(provider.dropped.load(Ordering::SeqCst), 0);
        provider.refuse.store(false, Ordering::SeqCst);
        assert_eq!(
            bank.process(
                &factory,
                request,
                77,
                1,
                64,
                64,
                &mut frame,
                &serde_json::json!({"author_changed": true}),
                1,
                &mut EffectProcessorControl::uncancelled()
            )
            .expect("same owner after allocator recovery"),
            64
        );
        assert_eq!(provider.created.load(Ordering::SeqCst), 1);
        assert_eq!(provider.observed_instance.load(Ordering::SeqCst), 1);
        assert_eq!(provider.dsp_calls.load(Ordering::SeqCst), 1);
        assert_eq!(frame.pixels(), [[0.5, 0.5, 0.75, 1.0]]);
        // A changed immutable provider revision legitimately replaces the
        // instance; an old Retained receipt must not imply eternal identity.
        frame = original;
        bank.process(
            &factory,
            request,
            78,
            1,
            64,
            64,
            &mut frame,
            &serde_json::json!({}),
            2,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("new recipe revision");
        assert_eq!(provider.created.load(Ordering::SeqCst), 2);
        assert_eq!(provider.observed_instance.load(Ordering::SeqCst), 2);
        assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
        bank.clear();
        assert_eq!(provider.dropped.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn host_resource_refusal_uses_unchanged_optional_count_and_byte_eviction_policy() {
        for (count_grant, bytes_grant) in [(0, 64), (1, 63)] {
            let provider = Arc::new(ResourceProbeFactory::default());
            provider.refuse.store(true, Ordering::SeqCst);
            let factory: Arc<dyn EffectFloatProcessorFactory> = provider.clone();
            let mut frame = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
            let original = frame.clone();
            let request = resource_request(&frame);
            let mut bank = FloatProcessorResidency::default();
            let result = bank.process(
                &factory,
                request,
                9,
                count_grant,
                bytes_grant,
                64,
                &mut frame,
                &serde_json::json!({}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            );
            assert!(matches!(
                result,
                Err(FloatProcessorError::HostResourceRefused(
                    EffectProcessorHostResourceRefusalReceipt {
                        retention: EffectProcessorRetention::Evicted,
                        ..
                    }
                ))
            ));
            assert_eq!(bank.counts(), (0, 0));
            assert_eq!(frame, original);
            assert_eq!(
                provider.dropped.load(Ordering::SeqCst),
                1,
                "eviction must already be real when reported"
            );
            assert_eq!(provider.dsp_calls.load(Ordering::SeqCst), 0);
            provider.refuse.store(false, Ordering::SeqCst);
            bank.process(
                &factory,
                request,
                9,
                count_grant,
                bytes_grant,
                64,
                &mut frame,
                &serde_json::json!({}),
                1,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("new owner under active grant");
            assert_eq!(provider.created.load(Ordering::SeqCst), 2);
            assert_eq!(provider.observed_instance.load(Ordering::SeqCst), 2);
            assert_eq!(provider.dsp_calls.load(Ordering::SeqCst), 1);
            assert_eq!(provider.dropped.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn host_resource_refusal_does_not_keep_an_instance_after_scheduler_stop() {
        for reason in [
            EffectProcessorStopReason::Canceled,
            EffectProcessorStopReason::DeadlineExpired,
        ] {
            let provider = Arc::new(ResourceProbeFactory::default());
            provider.refuse.store(true, Ordering::SeqCst);
            let observed = provider.refusal_observed.clone();
            let factory: Arc<dyn EffectFloatProcessorFactory> = provider.clone();
            let mut frame = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
            let original = frame.clone();
            let request = resource_request(&frame);
            let mut bank = FloatProcessorResidency::default();
            let mut checkpoint = || observed.load(Ordering::SeqCst).then_some(reason);
            let mut control = EffectProcessorControl::new(&mut checkpoint);
            let result = bank.process(
                &factory,
                request,
                7,
                1,
                64,
                64,
                &mut frame,
                &serde_json::json!({}),
                0,
                &mut control,
            );
            assert!(matches!(
                result,
                Err(FloatProcessorError::Processor(MondrianError::Cancelled))
            ));
            assert_eq!(control.stop_reason(), Some(reason));
            assert_eq!(frame, original);
            assert_eq!(bank.counts(), (0, 0));
            assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
            assert_eq!(provider.dsp_calls.load(Ordering::SeqCst), 0);
        }
    }

    fn resource_graph(
        provider: Arc<ResourceProbeFactory>,
        gpu_tail: bool,
    ) -> (Arc<CompiledEffectGraph>, String) {
        let key = format!("test.host-resource.{}", EffectId::new());
        register_effect_definition(
            EffectDefinition::new(
                &key,
                "Resource admission",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Nondeterministic,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_extension_contract(
                EffectExtensionContract::new("1.0.0").with_runtime_failure_policy(
                    EffectExtensionRuntimeFailurePolicy::DisableDefinition,
                ),
            )
            .with_custom_float_processor_factory(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                EffectCachePolicy::Uncacheable,
                provider,
            ),
        )
        .expect("registered bounded provider");
        let mut nodes =
            vec![instantiate_effect_node(EffectType::External(key.clone()))
                .expect("native occurrence")];
        if gpu_tail {
            let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
            correction
                .set_static_value_by_parameter(
                    &EffectType::BasicCorrection.parameter_id("exposure").expect("exposure"),
                    mondrian_core::automation::PropertyValue::Float(0.5),
                )
                .expect("actual GPU tail");
            nodes.push(correction);
        }
        let graph = PreparedEffectProgram::prepare(&nodes, &[], WorkingColorSpace::LinearRec709)
            .expect("prepare")
            .evaluate(TimelineTime::ZERO)
            .expect("compile");
        (graph, key)
    }

    #[test]
    fn public_scalar_and_heterogeneous_graph_preserve_resource_refusal_without_quarantine_or_replay(
    ) {
        for mixed in [false, true] {
            let provider = Arc::new(ResourceProbeFactory::default());
            provider.refuse.store(true, Ordering::SeqCst);
            let (graph, key) = resource_graph(provider.clone(), mixed);
            let original = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
            let mut session = EffectExecutionSession::default();
            session.bind_generation(77);
            let receipt = if mixed {
                let result = mixed_work(graph.clone()).execute_cpu_prefix_uncancelled(
                    &mut session,
                    77,
                    original.view(),
                    0,
                    WorkingColorSpace::LinearRec709,
                );
                let Err(PreparedHeterogeneousEffectWorkError::CustomProcessor(
                    EffectExecutionError::ProcessorHostResourceRefused { key: observed, receipt },
                )) = result
                else {
                    panic!("typed heterogeneous refusal required");
                };
                assert_eq!(observed, key);
                receipt
            } else {
                let result = session.apply_compiled_rgba_f32(
                    original.view(),
                    &graph,
                    0,
                    &mut EffectProcessorControl::uncancelled(),
                );
                let Err(EffectFloatExecutionError::CustomProcessor(
                    EffectExecutionError::ProcessorHostResourceRefused { key: observed, receipt },
                )) = result
                else {
                    panic!("typed scalar refusal required");
                };
                assert_eq!(observed, key);
                receipt
            };
            assert_eq!(receipt.retention, EffectProcessorRetention::Retained);
            assert_eq!(session.diagnostics().processor_instances, 1);
            assert_eq!(provider.created.load(Ordering::SeqCst), 1);
            assert_eq!(provider.dsp_calls.load(Ordering::SeqCst), 0);
            let status = effect_extension_runtime_status(&key).expect("availability");
            assert!(!status.disabled);
            assert_eq!(status.last_error, None);
            provider.refuse.store(false, Ordering::SeqCst);
            if mixed {
                let output = mixed_work(graph)
                    .execute_cpu_prefix_uncancelled(
                        &mut session,
                        77,
                        original.view(),
                        1,
                        WorkingColorSpace::LinearRec709,
                    )
                    .expect("same owned processor resumes a new invocation");
                assert_eq!(
                    output.boundary_values()[0].pixels(),
                    [[0.5, 0.5, 0.75, 1.0]]
                );
            } else {
                let output = session
                    .apply_compiled_rgba_f32(
                        original.view(),
                        &graph,
                        1,
                        &mut EffectProcessorControl::uncancelled(),
                    )
                    .expect("same owned processor resumes a new invocation");
                assert_eq!(output.pixels(), [[0.5, 0.5, 0.75, 1.0]]);
            }
            assert_eq!(provider.created.load(Ordering::SeqCst), 1);
            assert_eq!(
                provider.dsp_calls.load(Ordering::SeqCst),
                1,
                "one fresh invocation, no automatic replay"
            );
            assert_eq!(provider.observed_instance.load(Ordering::SeqCst), 1);
            assert_eq!(original.pixels(), [[0.25, 0.5, 0.75, 1.0]]);
        }
    }
    #[derive(Debug)]
    struct StorageProbeFactory {
        active: usize,
        retained: usize,
        calls: Arc<AtomicUsize>,
        lifecycle: Arc<ResourceProbeFactory>,
    }

    impl EffectFloatProcessorFactory for StorageProbeFactory {
        fn alpha_contract(&self) -> EffectAlphaContract {
            EffectAlphaContract::IDENTITY
        }
        fn feedback_budget_bytes(&self) -> usize {
            0
        }
        fn storage(&self, _: EffectFloatProcessorSizing) -> Result<EffectFloatProcessorStorage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            EffectFloatProcessorStorage::new(self.active, self.retained)
        }
        fn prepare(
            &self,
            request: EffectFloatProcessorPreparation,
            control: &mut EffectProcessorControl<'_>,
        ) -> Result<Box<dyn EffectFloatProcessorInstance>> {
            self.lifecycle.prepare(request, control)
        }
    }

    fn storage_provider(
        active: usize,
        retained: usize,
    ) -> (
        Arc<dyn EffectFloatProcessorFactory>,
        Arc<StorageProbeFactory>,
    ) {
        let concrete = Arc::new(StorageProbeFactory {
            active,
            retained,
            calls: Arc::new(AtomicUsize::new(0)),
            lifecycle: Arc::new(ResourceProbeFactory::default()),
        });
        (concrete.clone(), concrete)
    }

    #[allow(clippy::too_many_arguments)]
    fn storage_call(
        bank: &mut FloatProcessorResidency,
        factory: &Arc<dyn EffectFloatProcessorFactory>,
        request: EffectFloatProcessorPreparation,
        revision: u64,
        retained_ceiling: usize,
        working: usize,
        pixels: &mut EffectFloatFrame,
        control: &mut EffectProcessorControl<'_>,
    ) -> std::result::Result<usize, FloatProcessorError> {
        bank.process(
            factory,
            request,
            revision,
            4,
            retained_ceiling,
            working,
            pixels,
            &serde_json::json!({}),
            0,
            control,
        )
    }

    #[test]
    fn processor_storage_contract_rejects_retained_above_active_without_clamp() {
        assert!(EffectFloatProcessorStorage::new(31, 32).is_err());
        let equal =
            EffectFloatProcessorStorage::new(usize::MAX, usize::MAX).expect("checked bounds");
        assert_eq!(
            (equal.active_peak_bytes(), equal.retained_bytes()),
            (usize::MAX, usize::MAX)
        );
        let empty = EffectFloatProcessorStorage::new(0, 0).expect("zero declared owners");
        assert_eq!((empty.active_peak_bytes(), empty.retained_bytes()), (0, 0));
    }

    #[test]
    fn processor_storage_checked_peak_and_retained_sums_do_not_wrap_at_usize_limit() {
        let (large, large_probe) = storage_provider(usize::MAX, usize::MAX - 1);
        let (small, small_probe) = storage_provider(1, 1);
        let mut bank = FloatProcessorResidency::default();
        let mut pixels = coverage_frame(1, 1, vec![[0.125, 0.5, 0.75, 1.0]]);
        let first = resource_request(&pixels);
        let second = resource_request(&pixels);
        assert_eq!(
            storage_call(
                &mut bank,
                &large,
                first,
                0,
                usize::MAX,
                usize::MAX,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            )
            .expect("largest conservative declaration"),
            usize::MAX
        );
        assert_eq!(bank.counts(), (1, usize::MAX - 1));
        assert_eq!(
            storage_call(
                &mut bank,
                &small,
                second,
                0,
                usize::MAX,
                usize::MAX,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            )
            .expect("exact remaining sibling headroom"),
            usize::MAX
        );
        assert_eq!(bank.counts(), (2, usize::MAX));
        bank.trim(1, usize::MAX);
        assert_eq!(bank.counts(), (1, 1));
        assert_eq!(large_probe.lifecycle.dropped.load(Ordering::SeqCst), 1);
        bank.clear();
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(small_probe.lifecycle.dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn processor_storage_uses_active_admission_and_retained_bank_on_cold_and_reused_calls() {
        let (provider, probe) = storage_provider(80, 32);
        let mut bank = FloatProcessorResidency::default();
        let mut pixels = coverage_frame(1, 1, vec![[0.25, 0.5, 0.75, 1.0]]);
        let request = resource_request(&pixels);
        assert!(matches!(
            storage_call(
                &mut bank,
                &provider,
                request,
                0,
                32,
                79,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            ),
            Err(FloatProcessorError::Budget { required_bytes: 80, budget_bytes: 79 })
        ));
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 0);
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 0);
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(
            storage_call(
                &mut bank,
                &provider,
                request,
                0,
                32,
                80,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            )
            .expect("active admitted"),
            80
        );
        assert_eq!(bank.counts(), (1, 32));
        let before = pixels.pixels().to_vec();
        assert!(matches!(
            storage_call(
                &mut bank,
                &provider,
                request,
                0,
                32,
                79,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            ),
            Err(FloatProcessorError::Budget { required_bytes: 80, budget_bytes: 79 })
        ));
        assert_eq!(
            pixels.pixels(),
            before.as_slice(),
            "pre-DSP refusal is atomic"
        );
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            bank.counts(),
            (1, 32),
            "valid idle owner fits both remaining ceilings"
        );
        storage_call(
            &mut bank,
            &provider,
            request,
            0,
            32,
            80,
            &mut pixels,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("retry next authorized call");
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 1);
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            4,
            "reuse cannot bypass active sizing"
        );
        assert!(matches!(
            storage_call(
                &mut bank,
                &provider,
                request,
                0,
                32,
                31,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            ),
            Err(FloatProcessorError::Budget { .. })
        ));
        assert_eq!(
            bank.counts(),
            (0, 0),
            "retained backing cannot exceed current working grant"
        );
        assert_eq!(probe.lifecycle.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn processor_storage_counts_only_retained_siblings_in_exact_active_peak() {
        let (provider, probe) = storage_provider(80, 32);
        let mut bank = FloatProcessorResidency::default();
        let mut pixels = coverage_frame(1, 1, vec![[0.125, 0.5, 0.75, 1.0]]);
        let first = resource_request(&pixels);
        let second = resource_request(&pixels);
        assert_eq!(
            storage_call(
                &mut bank,
                &provider,
                first,
                0,
                64,
                112,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            )
            .expect("first"),
            80
        );
        assert_eq!(
            storage_call(
                &mut bank,
                &provider,
                second,
                0,
                64,
                112,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            )
            .expect("second"),
            112
        );
        assert_eq!(bank.counts(), (2, 64));
        assert_eq!(
            storage_call(
                &mut bank,
                &provider,
                first,
                0,
                64,
                100,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled()
            )
            .expect("evict sibling before active work"),
            80
        );
        assert_eq!(bank.counts(), (1, 32));
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 2);
        assert_eq!(probe.lifecycle.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 3);
        bank.trim(4, 31);
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(probe.lifecycle.dropped.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn processor_storage_optional_undergrant_never_rejects_admitted_execution() {
        let (provider, probe) = storage_provider(80, 32);
        let mut bank = FloatProcessorResidency::default();
        let mut pixels = coverage_frame(1, 1, vec![[0.125, 0.5, 0.75, 1.0]]);
        let request = resource_request(&pixels);
        for ceiling in [31, 0] {
            assert_eq!(
                storage_call(
                    &mut bank,
                    &provider,
                    request,
                    0,
                    ceiling,
                    80,
                    &mut pixels,
                    &mut EffectProcessorControl::uncancelled()
                )
                .expect("cold active call"),
                80
            );
            assert_eq!(bank.counts(), (0, 0));
        }
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 2);
        assert_eq!(probe.lifecycle.dropped.load(Ordering::SeqCst), 2);
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn processor_storage_pre_dsp_host_refusal_uses_the_same_retained_owner_authority() {
        let (provider, probe) = storage_provider(80, 32);
        probe.lifecycle.refuse.store(true, Ordering::SeqCst);
        let mut bank = FloatProcessorResidency::default();
        let mut pixels = coverage_frame(1, 1, vec![[0.125, 0.5, 0.75, 1.0]]);
        let request = resource_request(&pixels);
        for (ceiling, expected) in [
            (32, EffectProcessorRetention::Retained),
            (31, EffectProcessorRetention::Evicted),
        ] {
            let before = pixels.pixels().to_vec();
            let receipt = match storage_call(
                &mut bank,
                &provider,
                request,
                7,
                ceiling,
                80,
                &mut pixels,
                &mut EffectProcessorControl::uncancelled(),
            ) {
                Err(FloatProcessorError::HostResourceRefused(receipt)) => receipt,
                other => panic!("expected exact host-refusal receipt: {other:?}"),
            };
            assert_eq!(receipt.preparation, request);
            assert_eq!(receipt.provider_revision, 7);
            assert_eq!(receipt.retention, expected);
            assert_eq!(pixels.pixels(), before.as_slice());
            assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 0);
        }
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 2);
        assert_eq!(probe.lifecycle.dropped.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn processor_storage_stop_wins_after_refusal_and_never_retains_an_advanced_owner() {
        let (provider, probe) = storage_provider(80, 32);
        probe.lifecycle.refuse.store(true, Ordering::SeqCst);
        let mut bank = FloatProcessorResidency::default();
        let mut pixels = coverage_frame(1, 1, vec![[0.125, 0.5, 0.75, 1.0]]);
        let request = resource_request(&pixels);
        for reason in [
            EffectProcessorStopReason::Canceled,
            EffectProcessorStopReason::DeadlineExpired,
        ] {
            probe.lifecycle.refusal_observed.store(false, Ordering::SeqCst);
            let mut checkpoint =
                || probe.lifecycle.refusal_observed.load(Ordering::SeqCst).then_some(reason);
            let mut control = EffectProcessorControl::new(&mut checkpoint);
            assert!(matches!(
                storage_call(
                    &mut bank,
                    &provider,
                    request,
                    0,
                    32,
                    80,
                    &mut pixels,
                    &mut control
                ),
                Err(FloatProcessorError::Processor(MondrianError::Cancelled))
            ));
            assert_eq!(control.stop_reason(), Some(reason));
            assert_eq!(bank.counts(), (0, 0));
        }
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 0);
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 2);
        assert_eq!(probe.lifecycle.dropped.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn processor_storage_retires_obsolete_extent_format_and_provider_revision() {
        let (provider, probe) = storage_provider(80, 32);
        let mut bank = FloatProcessorResidency::default();
        let mut pixels = coverage_frame(1, 1, vec![[0.125, 0.5, 0.75, 1.0]]);
        let mut request = resource_request(&pixels);
        storage_call(
            &mut bank,
            &provider,
            request,
            0,
            32,
            80,
            &mut pixels,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("original");
        storage_call(
            &mut bank,
            &provider,
            request,
            1,
            32,
            80,
            &mut pixels,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("new provider revision");
        pixels = coverage_frame(2, 1, vec![[0.125, 0.5, 0.75, 1.0]; 2]);
        request.width = 2;
        storage_call(
            &mut bank,
            &provider,
            request,
            1,
            32,
            80,
            &mut pixels,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("new extent");
        pixels = EffectFloatFrame::from_pixels(
            pixels.extent(),
            pixels.format().with_alpha(mondrian_core::ColorFrameAlpha::StraightFloat),
            pixels.pixels().to_vec(),
        )
        .expect("new admitted association");
        request.format = pixels.format();
        storage_call(
            &mut bank,
            &provider,
            request,
            1,
            32,
            80,
            &mut pixels,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("new exact format");
        assert_eq!(bank.counts(), (1, 32));
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 4);
        assert_eq!(probe.lifecycle.dropped.load(Ordering::SeqCst), 3);
        bank.clear();
        assert_eq!(probe.lifecycle.dropped.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn processor_storage_whole_graph_admits_active_not_retained_before_any_prepare() {
        let (provider, probe) = storage_provider(80, 32);
        let key = format!("test.storage-admission.{}", EffectId::new());
        register_effect_definition(
            EffectDefinition::new(
                &key,
                "Storage admission",
                PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Nondeterministic,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_custom_float_processor_factory(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                EffectCachePolicy::Uncacheable,
                provider,
            ),
        )
        .expect("register exact recipe");
        let effect = instantiate_effect_node(EffectType::External(key)).expect("author occurrence");
        let graph = PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709)
            .expect("prepare immutable program")
            .evaluate(TimelineTime::ZERO)
            .expect("compile");
        let mut config = EffectExecutionSessionConfig {
            max_processor_instances: 1,
            max_processor_bytes: 32,
            ..EffectExecutionSessionConfig::uncached(2 * 16 + 80 - 1)
        };
        let mut execution = EffectExecutionSession::new(config);
        let frame = coverage_frame(1, 1, vec![[0.125, 0.5, 0.75, 1.0]]);
        assert!(execution
            .apply_compiled_rgba_f32(
                frame.view(),
                &graph,
                0,
                &mut EffectProcessorControl::uncancelled()
            )
            .is_err());
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 0);
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 0);
        config.max_working_bytes += 1;
        execution.reconfigure(config);
        let output = execution
            .apply_compiled_rgba_f32(
                frame.view(),
                &graph,
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("exact active plus Effects reservations");
        assert_eq!(output.pixels(), &[[0.25, 0.5, 0.75, 1.0]]);
        assert_eq!(execution.diagnostics().processor_bytes, 32);
        assert_eq!(probe.lifecycle.created.load(Ordering::SeqCst), 1);
        assert_eq!(probe.lifecycle.dsp_calls.load(Ordering::SeqCst), 1);
    }
}
#[cfg(test)]
mod feedback_contract_tests {
    use super::*;
    use crate::*;
    use mondrian_core::{ColorFrameAlpha, EffectProcessorOwnerId, TimelineTime, WorkingColorSpace};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct FeedbackFactory {
        created: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        cancel: Option<Arc<AtomicBool>>,
    }
    #[derive(Debug)]
    struct FeedbackInstance {
        factory: Arc<FeedbackFactory>,
        owner: EffectProcessorOwnerId,
        invocation: u64,
    }
    impl Drop for FeedbackInstance {
        fn drop(&mut self) {
            self.factory.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl EffectFloatProcessorFactory for Arc<FeedbackFactory> {
        fn alpha_contract(&self) -> EffectAlphaContract {
            EffectAlphaContract::COVERAGE_OPERATION
        }
        fn feedback_budget_bytes(&self) -> usize {
            EffectProcessorFeedbackSnapshot::maximum_retained_bytes()
        }
        fn storage(&self, _: EffectFloatProcessorSizing) -> Result<EffectFloatProcessorStorage> {
            let retained = self.feedback_budget_bytes() + 64;
            EffectFloatProcessorStorage::new(retained + 32, retained)
        }
        fn prepare(
            &self,
            _: EffectFloatProcessorPreparation,
            control: &mut EffectProcessorControl<'_>,
        ) -> Result<Box<dyn EffectFloatProcessorInstance>> {
            control.check()?;
            self.created.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FeedbackInstance {
                factory: Arc::clone(self),
                owner: EffectProcessorOwnerId::new(),
                invocation: 0,
            }))
        }
    }
    impl EffectFloatProcessorInstance for FeedbackInstance {
        fn process(
            &mut self,
            pixels: &mut EffectFloatFrame,
            params: &serde_json::Value,
            _: i64,
            control: &mut EffectProcessorControl<'_>,
        ) -> std::result::Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError>
        {
            control.check()?;
            self.invocation += 1;
            self.factory.calls.fetch_add(1, Ordering::SeqCst);
            if params["live"].as_bool().unwrap_or(false) {
                control.observe_progress(crate::EffectProcessorLiveProgress {
                    owner: self.owner,
                    origin: EffectProcessorExecutionOrigin::InProcess,
                    native_owner: 1,
                    invocation: self.invocation,
                    revision: 1,
                    progress: crate::EffectProcessorProgress::new(
                        1,
                        true,
                        0.375,
                        EffectProcessorNotice::new(
                            EffectProcessorNoticeKind::Information,
                            "Graph native progress".to_owned(),
                            "live-graph".to_owned(),
                        )
                        .expect("bounded fixture label"),
                    )
                    .expect("real executing progress"),
                })?;
            }
            if params["panic_after_live"].as_bool().unwrap_or(false) {
                panic!("independent processor panic after its actual live observation");
            }
            if let Some(cancel) = &self.factory.cancel {
                cancel.store(true, Ordering::SeqCst);
            }
            let blocked = params["blocked"].as_bool().unwrap_or(false);
            let persistent = blocked.then(|| {
                EffectProcessorNotice::new(
                    EffectProcessorNoticeKind::Error,
                    "Repair the authored parameter".to_owned(),
                    "repair".to_owned(),
                )
                .expect("bounded fixture error")
            });
            let feedback = EffectProcessorFeedback {
                owner: self.owner,
                origin: EffectProcessorExecutionOrigin::InProcess,
                snapshot: EffectProcessorFeedbackSnapshot::new(
                    1,
                    self.invocation,
                    self.invocation,
                    persistent,
                    None,
                    vec![],
                )
                .expect("actual bounded fixture state"),
            };
            if blocked {
                return Err(EffectProcessorInvocationError::UserBlocked(Box::new(
                    feedback,
                )));
            }
            for pixel in pixels.pixels_mut() {
                pixel[0] *= 2.0;
            }
            Ok(Some(feedback))
        }
    }
    fn frame() -> EffectFloatFrame {
        EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(1, 1),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            vec![[0.25, -0.5, 1.5, 0.25]],
        )
        .expect("finite coverage fixture")
    }
    fn request(frame: &EffectFloatFrame) -> EffectFloatProcessorPreparation {
        EffectFloatProcessorPreparation {
            effect_id: EffectId::new(),
            width: 1,
            height: 1,
            format: frame.format(),
        }
    }
    fn factory(value: &Arc<FeedbackFactory>) -> Arc<dyn EffectFloatProcessorFactory> {
        Arc::new(Arc::clone(value))
    }

    #[test]
    fn canonical_bank_retains_or_evicts_actual_user_blocked_owner() {
        for optional_count in [1, 0] {
            let value = Arc::new(FeedbackFactory::default());
            let factory = factory(&value);
            let mut frame = frame();
            let request = request(&frame);
            let mut bank = FloatProcessorResidency::default();
            let Err(FloatProcessorError::UserBlocked(blocked)) = bank.process(
                &factory,
                request,
                1,
                optional_count,
                8 * 1024 * 1024,
                16 * 1024 * 1024,
                &mut frame,
                &serde_json::json!({"blocked":true}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            ) else {
                panic!("actual typed blocked disposition");
            };
            assert_eq!(frame.pixels(), [[0.25, -0.5, 1.5, 0.25]]);
            assert_eq!(bank.counts().0, optional_count);
            assert_eq!(
                bank.counts().1,
                optional_count * (factory.feedback_budget_bytes() + 64)
            );
            assert_eq!(
                bank.counts().1,
                optional_count * (factory.feedback_budget_bytes() + 64)
            );
            assert_eq!(
                blocked.retention,
                if optional_count == 1 {
                    EffectProcessorRetention::Retained
                } else {
                    EffectProcessorRetention::Evicted
                }
            );
            let mut control = EffectProcessorControl::uncancelled();
            bank.process(
                &factory,
                request,
                1,
                optional_count,
                8 * 1024 * 1024,
                16 * 1024 * 1024,
                &mut frame,
                &serde_json::json!({"blocked":false}),
                1,
                &mut control,
            )
            .expect("actual repair invokes valid owner once");
            let receipts = control.take_feedback().expect("pre-admitted slots");
            let recovered = receipts.iter().next().expect("actual recovered feedback");
            assert!(!recovered.feedback.snapshot.is_blocked());
            assert_eq!(
                bank.counts().1,
                optional_count * (factory.feedback_budget_bytes() + 64)
            );
            assert_eq!(
                bank.counts().1,
                optional_count * (factory.feedback_budget_bytes() + 64)
            );
            if optional_count == 1 {
                assert_eq!(recovered.feedback.owner, blocked.feedback.owner);
                assert_eq!(recovered.feedback.snapshot.invocation(), 2);
                assert_eq!(value.created.load(Ordering::SeqCst), 1);
            } else {
                assert_ne!(recovered.feedback.owner, blocked.feedback.owner);
                assert_eq!(recovered.feedback.snapshot.invocation(), 1);
                assert_eq!(value.created.load(Ordering::SeqCst), 2);
            }
            assert_eq!(value.calls.load(Ordering::SeqCst), 2);
            assert_eq!(frame.pixels(), [[0.5, -0.5, 1.5, 0.25]]);
            bank.clear();
            assert_eq!(
                value.created.load(Ordering::SeqCst),
                value.dropped.load(Ordering::SeqCst)
            );
        }
    }

    #[test]
    fn feedback_storage_refusal_precedes_dsp_and_preserves_actual_retained_owner() {
        let value = Arc::new(FeedbackFactory::default());
        let factory = factory(&value);
        let mut frame = frame();
        let request = request(&frame);
        let mut bank = FloatProcessorResidency::default();
        let mut first = EffectProcessorControl::uncancelled();
        bank.process(
            &factory,
            request,
            1,
            1,
            8 * 1024 * 1024,
            16 * 1024 * 1024,
            &mut frame,
            &serde_json::json!({}),
            0,
            &mut first,
        )
        .expect("prepare one valid owner");
        let first = first.take_feedback().expect("bounded receipt");
        let owner = first.iter().next().expect("actual owner").feedback.owner;
        let calls = value.calls.load(Ordering::SeqCst);
        let original = frame.clone();
        let mut oversized = EffectProcessorControl::uncancelled();
        oversized.feedback.reserve_exact(EFFECT_PROCESSOR_FEEDBACK_SLOTS + 1);
        let Err(FloatProcessorError::HostResourceRefused(receipt)) = bank.process(
            &factory,
            request,
            1,
            1,
            8 * 1024 * 1024,
            16 * 1024 * 1024,
            &mut frame,
            &serde_json::json!({}),
            1,
            &mut oversized,
        ) else {
            panic!("observed capacity must refuse before DSP");
        };
        assert!(matches!(
            receipt.refusal,
            EffectProcessorHostResourceRefusal::FeedbackCapacity { .. }
        ));
        assert_eq!(receipt.retention, EffectProcessorRetention::Retained);
        assert_eq!(frame, original);
        assert_eq!(value.calls.load(Ordering::SeqCst), calls);
        let mut recovered = EffectProcessorControl::uncancelled();
        bank.process(
            &factory,
            request,
            1,
            1,
            8 * 1024 * 1024,
            16 * 1024 * 1024,
            &mut frame,
            &serde_json::json!({}),
            2,
            &mut recovered,
        )
        .expect("same bank owner remains valid");
        let recovered = recovered.take_feedback().expect("bounded feedback");
        assert_eq!(
            recovered.iter().next().expect("actual receipt").feedback.owner,
            owner
        );
        assert_eq!(value.created.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn scheduler_stop_wins_over_blocked_feedback_and_retires_owner() {
        let canceled = Arc::new(AtomicBool::new(false));
        let value = Arc::new(FeedbackFactory {
            cancel: Some(Arc::clone(&canceled)),
            ..Default::default()
        });
        let factory = factory(&value);
        let mut frame = frame();
        let request = request(&frame);
        let mut bank = FloatProcessorResidency::default();
        let mut checkpoint =
            || canceled.load(Ordering::SeqCst).then_some(EffectProcessorStopReason::Canceled);
        let mut control = EffectProcessorControl::new(&mut checkpoint);
        assert!(matches!(
            bank.process(
                &factory,
                request,
                1,
                1,
                8 * 1024 * 1024,
                16 * 1024 * 1024,
                &mut frame,
                &serde_json::json!({"blocked":true}),
                0,
                &mut control
            ),
            Err(FloatProcessorError::Processor(MondrianError::Cancelled))
        ));
        assert_eq!(
            control.stop_reason(),
            Some(EffectProcessorStopReason::Canceled)
        );
        assert_eq!(bank.counts().0, 0);
        assert_eq!(value.created.load(Ordering::SeqCst), 1);
        assert_eq!(value.dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn whole_scalar_graph_feedback_capacity_refuses_before_any_prepare_or_dsp() {
        let value = Arc::new(FeedbackFactory::default());
        let key = format!("test.feedback.{}", EffectId::new());
        let definition = EffectDefinition::new(
            &key,
            "Bounded feedback",
            mondrian_core::automation::PropertyBag::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Nondeterministic,
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_extension_contract(
            EffectExtensionContract::new("1.0.0").with_runtime_failure_policy(
                EffectExtensionRuntimeFailurePolicy::DisableDefinition,
            ),
        )
        .with_custom_float_processor_factory(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            EffectCachePolicy::Uncacheable,
            factory(&value),
        );
        register_effect_definition(definition).expect("one immutable feedback provider");
        let effects: Vec<_> = (0..EFFECT_PROCESSOR_FEEDBACK_SLOTS + 1)
            .map(|_| {
                instantiate_effect_node(EffectType::External(key.clone()))
                    .expect("distinct author occurrence")
            })
            .collect();
        let graph = PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709)
            .expect("canonical author program")
            .evaluate(TimelineTime::ZERO)
            .expect("compiled route");
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(128 * 1024 * 1024));
        assert!(matches!(
            session.apply_compiled_rgba_f32(
                frame().view(),
                &graph,
                0,
                &mut EffectProcessorControl::uncancelled()
            ),
            Err(EffectFloatExecutionError::CustomProcessor(
                EffectExecutionError::ProcessorFeedbackCapacityExceeded {
                    limit: EFFECT_PROCESSOR_FEEDBACK_SLOTS
                }
            ))
        ));
        assert_eq!(value.created.load(Ordering::SeqCst), 0);
        assert_eq!(value.calls.load(Ordering::SeqCst), 0);
        let status = effect_extension_runtime_status(&key).expect("actual definition status");
        assert!(!status.disabled);
        assert!(status.last_error.is_none());
    }
    #[derive(Debug)]
    struct CollectProgress {
        owner_thread: std::thread::ThreadId,
        receipts: std::sync::Mutex<Vec<crate::EffectProcessorProgressReceipt>>,
        capacity_bytes: usize,
        cancel: Option<Arc<AtomicBool>>,
    }
    impl CollectProgress {
        fn new(capacity_bytes: usize, cancel: Option<Arc<AtomicBool>>) -> Self {
            Self {
                owner_thread: std::thread::current().id(),
                receipts: std::sync::Mutex::new(Vec::with_capacity(2)),
                capacity_bytes,
                cancel,
            }
        }
    }
    impl crate::EffectProcessorObserver for CollectProgress {
        fn question_service(&self) -> Option<&dyn crate::EffectProcessorQuestionService> {
            None
        }
        fn maximum_storage_bytes(&self) -> usize {
            self.capacity_bytes
        }
        fn observe(&self, receipt: &crate::EffectProcessorProgressReceipt) {
            assert_eq!(std::thread::current().id(), self.owner_thread);
            let mut observed = self.receipts.lock().expect("owned observation");
            assert!(observed.len() < 2, "test owner is bounded");
            observed.push(receipt.clone());
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    fn live_graph(value: &Arc<FeedbackFactory>) -> (String, Arc<crate::CompiledEffectGraph>) {
        let key = format!("test.live-feedback.{}", EffectId::new());
        register_effect_definition(
            EffectDefinition::new(
                &key,
                "Bounded live feedback",
                mondrian_core::automation::PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Nondeterministic,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_extension_contract(
                EffectExtensionContract::new("1.0.0").with_runtime_failure_policy(
                    EffectExtensionRuntimeFailurePolicy::DisableDefinition,
                ),
            )
            .with_custom_float_processor_factory(
                Arc::new(|_, _| Ok(Some(serde_json::json!({"live":true})))),
                None,
                EffectCachePolicy::Uncacheable,
                factory(value),
            ),
        )
        .expect("immutable actual provider");
        let node =
            instantiate_effect_node(EffectType::External(key.clone())).expect("actual occurrence");
        let graph = PreparedEffectProgram::prepare(&[node], &[], WorkingColorSpace::LinearRec709)
            .expect("canonical prepared program")
            .evaluate(TimelineTime::ZERO)
            .expect("canonical binding");
        (key, graph)
    }

    #[test]
    fn actual_graph_live_progress_is_bound_to_same_occurrence_provider_and_terminal_owner() {
        let value = Arc::new(FeedbackFactory::default());
        let (_, graph) = live_graph(&value);
        let sink = Arc::new(CollectProgress::new(
            2 * crate::EffectProcessorProgressReceipt::maximum_retained_bytes(),
            None,
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut control = EffectProcessorControl::uncancelled();
        control.bind_observation(crate::EffectProcessorObservation {
            sink: Some(sink.clone()),
            deadline: Some(deadline),
        });
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_processor_instances: 1,
            max_processor_bytes: factory(&value).feedback_budget_bytes() + 64,
            ..EffectExecutionSessionConfig::uncached(16 * 1024 * 1024)
        });
        session
            .apply_compiled_rgba_f32(frame().view(), &graph, 0, &mut control)
            .expect("one actual native callback");
        assert!(
            control.current_occurrence.is_none(),
            "no stale borrowed invocation binding"
        );
        let receipts = control.take_feedback().expect("canonical terminal facts");
        let terminal = receipts.iter().next().expect("actual completed owner");
        let observed = sink.receipts.lock().expect("same owned consumer");
        assert_eq!(observed.len(), 1);
        let live = &observed[0];
        assert_eq!(live.preparation, terminal.preparation);
        assert_eq!(live.provider_revision, terminal.provider_revision);
        assert_eq!(live.progress.owner, terminal.feedback.owner);
        assert_eq!(
            live.progress.invocation,
            terminal.feedback.snapshot.invocation()
        );
        assert_eq!(live.progress.progress.fraction(), 0.375);
        assert_eq!(value.created.load(Ordering::SeqCst), 1);
        assert_eq!(value.calls.load(Ordering::SeqCst), 1);
        assert_eq!(session.diagnostics().processor_instances, 1);
        let stale = live.progress.clone();
        drop(observed);
        assert!(
            control.observe_progress(stale).is_err(),
            "completed actions cannot publish executing progress"
        );
        assert_eq!(sink.receipts.lock().expect("owner").len(), 1);
    }

    #[test]
    fn whole_graph_observer_capacity_refuses_before_prepare_or_dsp() {
        let value = Arc::new(FeedbackFactory::default());
        let (key, graph) = live_graph(&value);
        let sink = Arc::new(CollectProgress::new(32 * 1024 * 1024, None));
        let mut control = EffectProcessorControl::uncancelled();
        control.bind_observation(crate::EffectProcessorObservation {
            sink: Some(sink.clone()),
            deadline: None,
        });
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(16 * 1024 * 1024));
        let input = frame();
        assert!(session.apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control).is_err());
        assert_eq!(value.created.load(Ordering::SeqCst), 0);
        assert_eq!(value.calls.load(Ordering::SeqCst), 0);
        assert!(sink.receipts.lock().expect("consumer").is_empty());
        assert_eq!(session.diagnostics().processor_instances, 0);
        let status = effect_extension_runtime_status(&key).expect("provider");
        assert!(!status.disabled);
        assert!(status.last_error.is_none());
    }

    #[test]
    fn stop_observed_during_live_publish_retires_once_without_replay_or_quarantine() {
        let value = Arc::new(FeedbackFactory::default());
        let (key, graph) = live_graph(&value);
        let canceled = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(CollectProgress::new(
            2 * crate::EffectProcessorProgressReceipt::maximum_retained_bytes(),
            Some(canceled.clone()),
        ));
        let mut checkpoint =
            || canceled.load(Ordering::SeqCst).then_some(EffectProcessorStopReason::Canceled);
        let mut control = EffectProcessorControl::new(&mut checkpoint);
        control.bind_observation(crate::EffectProcessorObservation {
            sink: Some(sink.clone()),
            deadline: None,
        });
        let input = frame();
        let original = input.clone();
        let mut session =
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(16 * 1024 * 1024));
        assert!(session.apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control).is_err());
        assert_eq!(
            control.stop_reason(),
            Some(EffectProcessorStopReason::Canceled)
        );
        assert_eq!(input, original);
        assert_eq!(value.created.load(Ordering::SeqCst), 1);
        assert_eq!(value.calls.load(Ordering::SeqCst), 1);
        assert_eq!(value.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(session.diagnostics().processor_instances, 0);
        assert_eq!(sink.receipts.lock().expect("consumer").len(), 1);
        let status = effect_extension_runtime_status(&key).expect("provider");
        assert!(!status.disabled);
        assert!(status.last_error.is_none());
    }

    #[test]
    fn bound_owner_deadline_is_sticky_and_never_waits_for_a_native_callback() {
        let mut control = EffectProcessorControl::uncancelled();
        control.bind_observation(crate::EffectProcessorObservation {
            sink: None,
            deadline: Some(std::time::Instant::now()),
        });
        assert_eq!(
            control.checkpoint(),
            Err(EffectProcessorStopReason::DeadlineExpired)
        );
        control.bind_observation(crate::EffectProcessorObservation::default());
        assert_eq!(
            control.checkpoint(),
            Err(EffectProcessorStopReason::DeadlineExpired)
        );
    }

    #[test]
    fn process_unwind_clears_actual_occurrence_before_existing_isolation_receives_panic() {
        let value = Arc::new(FeedbackFactory::default());
        let factory = factory(&value);
        let mut pixels = frame();
        let request = request(&pixels);
        let mut bank = FloatProcessorResidency::default();
        let sink = Arc::new(CollectProgress::new(
            2 * crate::EffectProcessorProgressReceipt::maximum_retained_bytes(),
            None,
        ));
        let mut control = EffectProcessorControl::uncancelled();
        control.bind_observation(crate::EffectProcessorObservation {
            sink: Some(sink.clone()),
            deadline: None,
        });
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bank.process(
                &factory,
                request,
                1,
                1,
                8 * 1024 * 1024,
                16 * 1024 * 1024,
                &mut pixels,
                &serde_json::json!({"live":true,"panic_after_live":true}),
                0,
                &mut control,
            )
        }));
        assert!(
            panic.is_err(),
            "existing isolation still receives the original panic"
        );
        assert!(
            control.current_occurrence.is_none(),
            "no finished invocation binding survives unwind"
        );
        assert_eq!(value.created.load(Ordering::SeqCst), 1);
        assert_eq!(
            value.calls.load(Ordering::SeqCst),
            1,
            "no advanced DSP replay"
        );
        assert_eq!(value.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(bank.counts().0, 0);
        let observed = sink.receipts.lock().expect("original live receipt");
        assert_eq!(observed.len(), 1);
        let stale = observed[0].progress.clone();
        drop(observed);
        assert!(
            control.observe_progress(stale).is_err(),
            "ended owner cannot publish later"
        );
        assert_eq!(sink.receipts.lock().expect("bounded sink").len(), 1);
    }
}

#[cfg(test)]
mod completion_boundary_tests {
    use super::*;
    use crate::*;
    use mondrian_core::ColorFrameAlpha;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Debug)]
    struct OutputProbe {
        complete: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        mode: u8,
    }
    #[derive(Debug)]
    struct OutputProbeInstance {
        probe: Arc<OutputProbe>,
    }
    impl Drop for OutputProbeInstance {
        fn drop(&mut self) {
            self.probe.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl EffectFloatProcessorFactory for Arc<OutputProbe> {
        fn alpha_contract(&self) -> EffectAlphaContract {
            EffectAlphaContract::IDENTITY
        }
        fn feedback_budget_bytes(&self) -> usize {
            0
        }
        fn storage(&self, _: EffectFloatProcessorSizing) -> Result<EffectFloatProcessorStorage> {
            EffectFloatProcessorStorage::new(64, 32)
        }
        fn prepare(
            &self,
            _: EffectFloatProcessorPreparation,
            control: &mut EffectProcessorControl<'_>,
        ) -> Result<Box<dyn EffectFloatProcessorInstance>> {
            control.check()?;
            Ok(Box::new(OutputProbeInstance { probe: Arc::clone(self) }))
        }
    }
    impl EffectFloatProcessorInstance for OutputProbeInstance {
        fn process(
            &mut self,
            pixels: &mut EffectFloatFrame,
            _: &serde_json::Value,
            _: i64,
            control: &mut EffectProcessorControl<'_>,
        ) -> std::result::Result<Option<EffectProcessorFeedback>, EffectProcessorInvocationError>
        {
            control.check()?;
            self.probe.calls.fetch_add(1, Ordering::SeqCst);
            match self.probe.mode {
                1 => {
                    let last = pixels.len() - 1;
                    pixels.pixels_mut()[last][0] = f32::NAN;
                }
                2 => {
                    *pixels = EffectFloatFrame::from_pixels(
                        EffectFrameExtent::new(2, 1),
                        pixels.format(),
                        vec![[0.25, -0.5, 1.5, 0.25]; 2],
                    )
                    .expect("deliberate valid raster with forbidden changed extent");
                }
                3 => pixels
                    .set_format_after_write(pixels.format().with_domain(EffectColorDomain::Data)),
                4 => pixels.set_format_after_write(
                    pixels.format().with_alpha(ColorFrameAlpha::StraightFloat),
                ),
                _ => {
                    for sample in pixels.pixels_mut() {
                        sample[0] *= 2.0;
                    }
                }
            }
            // This marker is set only after the sole DSP call has completed.
            // The owner polls are deterministic; no wall-clock/sleep race.
            self.probe.complete.store(true, Ordering::SeqCst);
            Ok(None)
        }
    }
    fn fixture(
        mode: u8,
        width: u32,
        height: u32,
    ) -> (
        Arc<dyn EffectFloatProcessorFactory>,
        Arc<OutputProbe>,
        EffectFloatFrame,
        EffectFloatProcessorPreparation,
    ) {
        let probe = Arc::new(OutputProbe {
            complete: Arc::new(AtomicBool::new(false)),
            calls: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
            mode,
        });
        let factory: Arc<dyn EffectFloatProcessorFactory> = Arc::new(Arc::clone(&probe));
        let frame = EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(width, height),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            vec![[0.25, -0.5, 1.5, 0.25]; width as usize * height as usize],
        )
        .expect("complete finite coverage input");
        let request = EffectFloatProcessorPreparation {
            effect_id: EffectId::new(),
            width,
            height,
            format: frame.format(),
        };
        (factory, probe, frame, request)
    }
    fn stopped_scan(reason: EffectProcessorStopReason, mode: u8) {
        let (factory, probe, mut frame, request) = fixture(mode, 256, 257);
        let mut polls = 0;
        let mut checkpoint = || {
            if !probe.complete.load(Ordering::SeqCst) {
                return None;
            }
            polls += 1;
            (polls == 4).then_some(reason)
        };
        let mut control = EffectProcessorControl::new(&mut checkpoint);
        let mut bank = FloatProcessorResidency::default();
        let result = bank.process(
            &factory,
            request,
            1,
            1,
            32,
            64,
            &mut frame,
            &serde_json::json!({}),
            0,
            &mut control,
        );
        assert!(
            matches!(
                result,
                Err(FloatProcessorError::Processor(MondrianError::Cancelled))
            ),
            "the current owner stop must win during completed-output scanning: {result:?}"
        );
        assert_eq!(control.stop_reason(), Some(reason));
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            1,
            "an advanced DSP cannot replay"
        );
        assert_eq!(probe.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(
            bank.counts(),
            (0, 0),
            "stopped completed owner cannot be retained"
        );
    }
    #[test]
    fn bank_completed_output_scanning_observes_cancel_before_publication() {
        stopped_scan(EffectProcessorStopReason::Canceled, 0);
    }
    #[test]
    fn bank_completed_output_scanning_observes_deadline_before_invalid_tail() {
        stopped_scan(EffectProcessorStopReason::DeadlineExpired, 1);
    }
    #[test]
    fn completed_processor_diagnostics_name_the_actual_contract() {
        for (mode, contract) in [(2, "extent"), (3, "format"), (4, "alpha")] {
            let (factory, probe, mut frame, request) = fixture(mode, 1, 1);
            let mut bank = FloatProcessorResidency::default();
            let error = bank
                .process(
                    &factory,
                    request,
                    1,
                    1,
                    32,
                    64,
                    &mut frame,
                    &serde_json::json!({}),
                    0,
                    &mut EffectProcessorControl::uncancelled(),
                )
                .expect_err("independently invalid actual provider contract");
            assert!(
                error.to_string().contains(contract),
                "wrong contract diagnostic: {error}"
            );
            assert!(
                !error.to_string().contains("pixel count"),
                "unrelated count explanation: {error}"
            );
            assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
            assert_eq!(probe.dropped.load(Ordering::SeqCst), 1);
            assert_eq!(bank.counts(), (0, 0));
        }
    }
    #[test]
    fn completed_output_positive_control_retains_one_true_preserving_instance() {
        let (factory, probe, mut frame, request) = fixture(0, 256, 257);
        let mut bank = FloatProcessorResidency::default();
        assert_eq!(
            bank.process(
                &factory,
                request,
                1,
                1,
                32,
                64,
                &mut frame,
                &serde_json::json!({}),
                0,
                &mut EffectProcessorControl::uncancelled()
            )
            .expect("valid complete output"),
            64
        );
        assert!(frame.pixels().iter().all(|p| *p == [0.5, -0.5, 1.5, 0.25]));
        assert_eq!(frame.alpha(), ColorFrameAlpha::StraightCoverage);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        assert_eq!(bank.counts(), (1, 32));
        bank.clear();
        assert_eq!(probe.dropped.load(Ordering::SeqCst), 1);
    }
    fn public_graph(probe: &Arc<OutputProbe>) -> (String, Arc<CompiledEffectGraph>) {
        let key = format!("test.completed-output-owner.{}", EffectId::new());
        let factory: Arc<dyn EffectFloatProcessorFactory> = Arc::new(Arc::clone(probe));
        register_effect_definition(
            EffectDefinition::new(
                &key,
                "Completed output owner",
                mondrian_core::automation::PropertyBag::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Nondeterministic,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..EffectExecutionContract::IDENTITY
            })
            .with_extension_contract(
                EffectExtensionContract::new("1.0.0").with_runtime_failure_policy(
                    EffectExtensionRuntimeFailurePolicy::DisableDefinition,
                ),
            )
            .with_custom_float_processor_factory(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                EffectCachePolicy::Uncacheable,
                factory,
            ),
        )
        .expect("actual immutable provider");
        let node =
            instantiate_effect_node(EffectType::External(key.clone())).expect("author occurrence");
        let graph = PreparedEffectProgram::prepare(
            &[node],
            &[],
            mondrian_core::WorkingColorSpace::LinearRec709,
        )
        .expect("canonical prepared graph")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("compiled occurrence");
        (key, graph)
    }
    #[test]
    fn whole_graph_completed_scan_stop_never_publishes_replays_or_quarantines() {
        for reason in [
            EffectProcessorStopReason::Canceled,
            EffectProcessorStopReason::DeadlineExpired,
        ] {
            // Real bank completion polls1..5; outer staged admission polls6..9.
            // Especially6..9 stop after the bank temporarily retained the owner,
            // and must retire that same owner through the existing adjustment.
            for stop_at in 1..=9 {
                let (_, probe, input, _) = fixture(0, 256, 257);
                let original = input.clone();
                let (key, graph) = public_graph(&probe);
                let mut polls = 0;
                let mut checkpoint = || {
                    if !probe.complete.load(Ordering::SeqCst) {
                        return None;
                    }
                    polls += 1;
                    if polls >= 6 {
                        assert_eq!(probe.dropped.load(Ordering::SeqCst), 0,
                            "the same completed bank owner must remain retained before the outer stop");
                    }
                    (polls == stop_at).then_some(reason)
                };
                let mut control = EffectProcessorControl::new(&mut checkpoint);
                let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
                    max_processor_instances: 1,
                    max_processor_bytes: 32,
                    ..EffectExecutionSessionConfig::uncached(16 * 1024 * 1024)
                });
                let result = session.apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control);
                assert!(
                    matches!(result, Err(EffectFloatExecutionError::Stopped {
                    reason: observed }) if observed == reason),
                    "current stopped outcome at completed poll {stop_at}: {result:?}"
                );
                assert_eq!(
                    input, original,
                    "staged output cannot become authored input or published success"
                );
                assert_eq!(control.stop_reason(), Some(reason));
                assert_eq!(
                    probe.calls.load(Ordering::SeqCst),
                    1,
                    "no advanced DSP replay"
                );
                assert_eq!(
                    probe.dropped.load(Ordering::SeqCst),
                    1,
                    "one actual owner retires once"
                );
                assert_eq!(session.diagnostics().processor_instances, 0);
                let status =
                    effect_extension_runtime_status(&key).expect("registered provider status");
                assert!(
                    !status.disabled,
                    "owner stop is not an invalid provider fault"
                );
                assert!(status.last_error.is_none());
            }
        }
    }
    #[test]
    fn whole_graph_completed_output_keeps_original_precision_and_sole_resident() {
        let (_, probe, input, _) = fixture(0, 256, 257);
        let (key, graph) = public_graph(&probe);
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
            max_processor_instances: 1,
            max_processor_bytes: 32,
            ..EffectExecutionSessionConfig::uncached(16 * 1024 * 1024)
        });
        for frame_seed in 0..2 {
            let output = session
                .apply_compiled_rgba_f32(
                    input.view(),
                    &graph,
                    frame_seed,
                    &mut EffectProcessorControl::uncancelled(),
                )
                .expect("same valid public graph owner");
            assert_eq!(output.extent(), input.extent());
            assert_eq!(output.format(), input.format());
            assert!(output.pixels().iter().all(|p| *p == [0.5, -0.5, 1.5, 0.25]));
            assert_eq!(session.diagnostics().processor_instances, 1);
        }
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            2,
            "uncacheable invokes exactly once per authorized frame"
        );
        assert_eq!(probe.dropped.load(Ordering::SeqCst), 0);
        let status = effect_extension_runtime_status(&key).expect("registered provider");
        assert!(!status.disabled);
        assert!(status.last_error.is_none());
        drop(session);
        assert_eq!(probe.dropped.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod gui_reconciliation_contract_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Debug, Clone, Copy)]
    enum Behavior {
        Healthy,
        Failure,
        OpaquePanic,
    }
    #[derive(Debug)]
    struct Instance {
        behavior: Behavior,
        reconciled: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        payload_dropped: Arc<AtomicUsize>,
        processed: Arc<AtomicUsize>,
    }
    #[derive(Debug)]
    struct OpaquePayload(Arc<AtomicUsize>);
    impl Drop for OpaquePayload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("foreign panic-payload destructor must never execute");
        }
    }
    impl Drop for Instance {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl EffectFloatProcessorInstance for Instance {
        fn reconcile_gui(
            &mut self,
        ) -> mondrian_core::Result<Option<crate::EffectProcessorGuiActionFeedback>> {
            self.reconciled.fetch_add(1, Ordering::SeqCst);
            match self.behavior {
                Behavior::Healthy => Ok(None),
                Behavior::Failure => Err(MondrianError::WorkflowStepFailed {
                    step_id: "owned_gui_qualification".to_owned(),
                    reason: "paired action refused".to_owned(),
                }),
                Behavior::OpaquePanic => {
                    std::panic::panic_any(OpaquePayload(self.payload_dropped.clone()))
                }
            }
        }
        fn process(
            &mut self,
            _: &mut crate::EffectFloatFrame,
            _: &serde_json::Value,
            _: i64,
            _: &mut EffectProcessorControl<'_>,
        ) -> std::result::Result<
            Option<crate::EffectProcessorFeedback>,
            EffectProcessorInvocationError,
        > {
            self.processed.fetch_add(1, Ordering::SeqCst);
            panic!("idle GUI reconciliation cannot render or replay native DSP");
        }
    }

    #[test]
    fn idle_gui_reconciliation_retains_same_healthy_owner_and_retires_failed_owner_once() {
        let reconciled = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let payload_dropped = Arc::new(AtomicUsize::new(0));
        let processed = Arc::new(AtomicUsize::new(0));
        let mut residency = FloatProcessorResidency::default();
        let mut healthy_key = None;
        for behavior in [Behavior::Healthy, Behavior::Failure, Behavior::OpaquePanic] {
            let request = EffectFloatProcessorPreparation {
                effect_id: mondrian_core::EffectId::new(),
                width: 1,
                height: 1,
                format: crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightFloat,
                ),
            };
            let key = ResourceKey { request, revision: 7 };
            if matches!(behavior, Behavior::Healthy) {
                healthy_key = Some(key);
            }
            residency.entries.push_back((
                key,
                64,
                Box::new(Instance {
                    behavior,
                    reconciled: reconciled.clone(),
                    dropped: dropped.clone(),
                    payload_dropped: payload_dropped.clone(),
                    processed: processed.clone(),
                }),
            ));
            residency.bytes += 64;
        }
        assert_eq!(residency.reconcile_gui(1024), 2);
        assert_eq!(residency.counts(), (1, 64));
        assert_eq!(
            residency
                .entries
                .front()
                .map(|entry| (entry.0.request.effect_id, entry.0.revision)),
            healthy_key.map(|key| (key.request.effect_id, key.revision))
        );
        assert_eq!(reconciled.load(Ordering::SeqCst), 3);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        assert_eq!(payload_dropped.load(Ordering::SeqCst), 0);
        assert_eq!(processed.load(Ordering::SeqCst), 0);
        assert_eq!(residency.reconcile_gui(1024), 0);
        assert_eq!(residency.counts(), (1, 64));
        assert_eq!(reconciled.load(Ordering::SeqCst), 4);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        drop(residency);
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
        assert_eq!(payload_dropped.load(Ordering::SeqCst), 0);
    }
}
