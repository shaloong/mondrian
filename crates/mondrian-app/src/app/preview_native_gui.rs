//! One real product GUI lifetime and bounded metadata on existing work channels.
//!
//! This module owns no native instance, graph, worker, cancellation authority or
//! author transaction. The current bank supplies processor/provider identity;
//! the current Preview route supplies scope. GUI disappearance fails only the
//! Question service. Existing work cancellation remains independently owned.

use super::preview_processor_feedback::{PreviewProcessorFeedbackScope, PreviewProcessorWorkKey};
use mondrian_editor_state::NativeVisualAuthorView;
use mondrian_effects::{
    EffectFloatProcessorPreparation, EffectProcessorGuiState, EffectProcessorQuestionReceipt,
    EffectProcessorQuestionReply, EffectProcessorQuestionService, EffectProcessorQuestionState,
    EffectProcessorQuestionView,
};
use std::sync::{Arc, Mutex, Weak};

const MAX_AUTHOR_VIEW_BYTES: usize = 16 * 1024;

type Publication = dyn Fn(NativeVisualQuestionPresentation) -> bool + Send + Sync;
type ActionPublication = dyn Fn(NativeVisualGuiActionDelivery) -> bool + Send + Sync;

#[derive(Clone)]
struct ActionBinding {
    view: NativeVisualAuthorView,
    preparation: EffectFloatProcessorPreparation,
    revision: u64,
    owner: mondrian_core::EffectProcessorOwnerId,
    publication: Arc<ActionPublication>,
}
impl std::fmt::Debug for ActionBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionBinding")
            .field("preparation", &self.preparation)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// Existing worker-result delivery with actual idle-action provenance. It has
/// no FrameWork, presentation ticket, replacement processor or author edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeVisualGuiActionDelivery {
    pub(crate) view: NativeVisualAuthorView,
    pub(crate) action: mondrian_effects::EffectProcessorGuiActionReceipt,
}

/// Fixed product drain storage. Overflow drops observational delivery only;
/// it never blocks native teardown, retires an owner or fabricates a count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeVisualGuiActionBatch {
    entries:
        [Option<NativeVisualGuiActionDelivery>; mondrian_effects::EFFECT_PROCESSOR_FEEDBACK_SLOTS],
    len: usize,
}
impl Default for NativeVisualGuiActionBatch {
    fn default() -> Self {
        Self {
            entries: [const { None }; mondrian_effects::EFFECT_PROCESSOR_FEEDBACK_SLOTS],
            len: 0,
        }
    }
}
impl NativeVisualGuiActionBatch {
    pub(crate) fn push(&mut self, value: NativeVisualGuiActionDelivery) -> bool {
        if self.len == self.entries.len() {
            return false;
        }
        self.entries[self.len] = Some(value);
        self.len += 1;
        true
    }
    pub(crate) fn append(&mut self, other: Self) {
        for value in other {
            let _ = self.push(value);
        }
    }
}
impl IntoIterator for NativeVisualGuiActionBatch {
    type Item = NativeVisualGuiActionDelivery;
    type IntoIter = std::iter::Flatten<
        std::array::IntoIter<
            Option<Self::Item>,
            { mondrian_effects::EFFECT_PROCESSOR_FEEDBACK_SLOTS },
        >,
    >;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter().flatten()
    }
}

/// A purpose explicitly issued by the current GUI scheduler, never inferred
/// from FrameWorkClass::Interactive, a plugin boolean or decoded question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeVisualGuiWorkPurpose {
    /// Ordinary playback/scrub may observe a real GUI but cannot wait for it.
    Observe,
    /// The real editor explicitly requested a nonrealtime refresh.
    EditorRefresh,
}
impl NativeVisualGuiWorkPurpose {
    fn state(self) -> EffectProcessorGuiState {
        match self {
            Self::Observe => EffectProcessorGuiState::Open,
            Self::EditorRefresh => EffectProcessorGuiState::QuestionWork,
        }
    }
}

#[derive(Debug)]
struct GuiState {
    view: NativeVisualAuthorView,
    open: bool,
    active_panel: bool,
    registered_sections: usize,
    current_work: Option<PreviewProcessorFeedbackScope>,
    question: Weak<QuestionCell>,
    action_binding: Option<Arc<ActionBinding>>,
}
#[derive(Debug)]
struct GuiLifetime {
    state: Mutex<GuiState>,
    revision: Arc<GuiRevision>,
}

#[derive(Debug)]
struct GuiRevision(std::sync::atomic::AtomicU64);
impl Default for GuiRevision {
    fn default() -> Self {
        Self(std::sync::atomic::AtomicU64::new(1))
    }
}
impl mondrian_effects::EffectProcessorGuiPresence for GuiLifetime {
    fn is_open(&self) -> bool {
        self.state.lock().is_ok_and(|state| gui_present(&state))
    }
    fn observe_action(&self, action: mondrian_effects::EffectProcessorGuiActionReceipt) -> bool {
        let binding = {
            let Ok(state) = self.state.lock() else {
                return false;
            };
            let Some(binding) = state.action_binding.as_ref() else {
                return false;
            };
            if state.view != binding.view
                || action.action_serial == 0
                || action.receipt.preparation != binding.preparation
                || action.receipt.provider_revision != binding.revision
                || action.receipt.feedback.owner != binding.owner
            {
                return false;
            }
            Arc::clone(binding)
        };
        // Never call task transport while holding the GUI gate. The channel is
        // the same original worker transport, even after that work completed.
        (binding.publication)(NativeVisualGuiActionDelivery { view: binding.view.clone(), action })
    }
}
impl GuiRevision {
    fn advance(&self) -> bool {
        self.0
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |value| value.checked_add(1),
            )
            .is_ok()
    }
    fn current(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Main-thread-owned real GUI claim. Drop closes its metadata lifetime only;
/// EndInstanceEdit/instance retention remain operations on the existing bank.
#[derive(Debug)]
pub(crate) struct PreviewNativeGuiController {
    lifetime: Arc<GuiLifetime>,
}
impl PreviewNativeGuiController {
    /// Call only after the real corresponding Inspector is active and the
    /// current AuthoringSession issued the read-only view.
    fn open(view: NativeVisualAuthorView, revision: Arc<GuiRevision>) -> Option<Self> {
        if view.retained_bytes() > MAX_AUTHOR_VIEW_BYTES {
            return None;
        }
        Some(Self {
            lifetime: Arc::new(GuiLifetime {
                revision,
                state: Mutex::new(GuiState {
                    view,
                    open: true,
                    active_panel: false,
                    registered_sections: 0,
                    current_work: None,
                    question: Weak::new(),
                    action_binding: None,
                }),
            }),
        })
    }

    /// Refresh a same real GUI after a canonical author change. A stale
    /// Question is dismissed, but this is not a replacement native instance.
    pub(crate) fn update_view(&self, next: NativeVisualAuthorView) -> bool {
        let Ok(mut state) = self.lifetime.state.lock() else {
            return false;
        };
        let same_address = state.view.session_id() == next.session_id()
            && state.view.sequence_id() == next.sequence_id()
            && state.view.clip_id() == next.clip_id()
            && state.view.effect_id() == next.effect_id()
            && state.view.definition() == next.definition();
        if !state.open || !same_address || next.retained_bytes() > MAX_AUTHOR_VIEW_BYTES {
            state.open = false;
            if let Some(question) = state.question.upgrade() {
                question.dismiss();
            }
            return false;
        }
        if state.view != next {
            if !self.lifetime.revision.advance() {
                state.open = false;
                return false;
            }
            if let Some(question) = state.question.upgrade() {
                question.dismiss();
            }
            state.current_work = None;
            state.question = Weak::new();
            state.view = next;
        }
        true
    }

    /// Bind the exact current route before DSP. Prefetch/replacement/shadow
    /// work must not receive this claim. Existing absolute deadline is unchanged.
    pub(crate) fn bind_work(
        &self,
        planning: NativeVisualGuiPlanningIdentity,
        scope: PreviewProcessorFeedbackScope,
        purpose: NativeVisualGuiWorkPurpose,
    ) -> Option<PreviewNativeGuiWork> {
        let Ok(mut state) = self.lifetime.state.lock() else {
            return None;
        };
        if !gui_present(&state)
            || scope.key.is_none()
            || state.view.session_id() != planning.session
            || state.view.author_generation().get() != planning.author_generation
            || state.view.sequence_id() != planning.sequence
            || !scope_addresses(&state.view, scope)
        {
            return None;
        }
        if state.current_work != Some(scope) {
            if let Some(question) = state.question.upgrade() {
                question.dismiss();
            }
            state.question = Weak::new();
            state.current_work = Some(scope);
        }
        Some(PreviewNativeGuiWork {
            lifetime: Arc::downgrade(&self.lifetime),
            view: state.view.clone(),
            scope,
            purpose,
        })
    }

    pub(crate) fn close(&self) {
        if let Ok(mut state) = self.lifetime.state.lock() {
            if state.open && !self.lifetime.revision.advance() {
                state.open = false;
            }
            state.open = false;
            state.current_work = None;
            if let Some(question) = state.question.upgrade() {
                question.dismiss();
            }
            state.question = Weak::new();
        }
    }
}
impl PreviewNativeGuiController {
    /// This private guard is acquired by the concrete Effect PropertySection
    /// control Widget, not by a model, selection metadata or a script action.
    pub(crate) fn section_presence(self: &Arc<Self>) -> Option<NativeVisualGuiSectionPresence> {
        let mut state = self.lifetime.state.lock().ok()?;
        if !state.open {
            return None;
        }
        if state.registered_sections == 0 && !self.lifetime.revision.advance() {
            state.open = false;
            return None;
        }
        state.registered_sections = state.registered_sections.checked_add(1)?;
        Some(NativeVisualGuiSectionPresence { lifetime: Arc::downgrade(&self.lifetime) })
    }

    /// Root proves the actual Dock active-tab state; construction alone is not
    /// presence. Hiding a tab promptly dismisses its original Question.
    pub(crate) fn set_panel_active(&self, active: bool) {
        if let Ok(mut state) = self.lifetime.state.lock() {
            if state.active_panel != active && !self.lifetime.revision.advance() {
                state.open = false;
            }
            state.active_panel = active;
            if !active {
                state.current_work = None;
                if let Some(question) = state.question.upgrade() {
                    question.dismiss();
                }
                state.question = Weak::new();
            }
        }
    }

    fn current_view(&self) -> Option<NativeVisualAuthorView> {
        self.lifetime
            .state
            .lock()
            .ok()
            .filter(|state| state.open)
            .map(|state| state.view.clone())
    }
}

fn gui_present(state: &GuiState) -> bool {
    state.open && state.active_panel && state.registered_sections != 0
}

/// Owned only by a real generic effect section. Rebuild constructs new Widgets
/// before dropping the old dock, so a reused view never falsely closes midway.
pub(crate) struct NativeVisualGuiSectionPresence {
    lifetime: Weak<GuiLifetime>,
}
impl Drop for NativeVisualGuiSectionPresence {
    fn drop(&mut self) {
        if let Some(lifetime) = self.lifetime.upgrade()
            && let Ok(mut state) = lifetime.state.lock()
        {
            match state.registered_sections.checked_sub(1) {
                Some(count) => state.registered_sections = count,
                None => {
                    state.open = false;
                }
            }
            if state.registered_sections == 0 {
                if !lifetime.revision.advance() {
                    state.open = false;
                }
                state.current_work = None;
                if let Some(question) = state.question.upgrade() {
                    question.dismiss();
                }
                state.question = Weak::new();
            }
        }
    }
}

/// These values are sampled from the real immutable Preview authoring snapshot
/// and current Sequence when issuing work; a Visual key hash is no Sequence proof.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeVisualGuiPlanningIdentity {
    pub(crate) session: mondrian_editor_state::AuthoringSessionId,
    pub(crate) author_generation: u64,
    pub(crate) sequence: mondrian_core::SequenceId,
}

impl Drop for PreviewNativeGuiController {
    fn drop(&mut self) {
        self.close();
    }
}

fn scope_addresses(view: &NativeVisualAuthorView, scope: PreviewProcessorFeedbackScope) -> bool {
    match scope.key {
        Some(PreviewProcessorWorkKey::Scalar { sequence_id, .. }) => {
            sequence_id == view.sequence_id()
        }
        // The actual visual key is issued by this Runtime's current Sequence
        // plan. It cannot be reconstructed from a Question or GUI display id.
        Some(PreviewProcessorWorkKey::Visual(_)) => true,
        None => false,
    }
}

/// Immutable GUI-address facts on the existing sole work payload. Weak storage
/// cannot keep a closed editor alive. No private processor is transferred here.
#[derive(Debug, Clone)]
pub(crate) struct PreviewNativeGuiWork {
    lifetime: Weak<GuiLifetime>,
    view: NativeVisualAuthorView,
    scope: PreviewProcessorFeedbackScope,
    purpose: NativeVisualGuiWorkPurpose,
}
impl PreviewNativeGuiWork {
    fn is_current(&self) -> bool {
        self.lifetime.upgrade().is_some_and(|lifetime| {
            lifetime.state.lock().is_ok_and(|state| {
                gui_present(&state)
                    && state.view == self.view
                    && state.current_work == Some(self.scope)
            })
        })
    }
    fn admits(&self, preparation: EffectFloatProcessorPreparation) -> bool {
        self.is_current() && preparation.effect_id == self.view.effect_id()
    }
    /// Single existing task result channel callback must return false on full
    /// or disconnected. A service that could not post returns Failed immediately.
    pub(crate) fn service(
        self,
        result_slots: usize,
        publication: impl Fn(NativeVisualQuestionPresentation) -> bool + Send + Sync + 'static,
        action_publication: impl Fn(NativeVisualGuiActionDelivery) -> bool + Send + Sync + 'static,
    ) -> Option<Arc<dyn EffectProcessorQuestionService>> {
        let capacity_bytes = service_storage_bytes(
            result_slots,
            std::mem::size_of_val(&publication),
            std::mem::size_of_val(&action_publication),
        )?;
        Some(Arc::new(PreviewNativeQuestionService {
            work: self,
            publication: Box::new(publication),
            action_publication: Arc::new(action_publication),
            cell: Mutex::new(None),
            capacity_bytes,
        }))
    }
}

// Declared conservative logical storage for this service's bounded delivery
// frontier. The actual App drain owns 32 action slots; author views and complete
// feedback in those slots are separately owned clones, not just Arc headers.
// Question cells can survive in the channel and real modal after the service
// advances, so their complete work/receipt capacity is charged per live slot.
// Existing UI registry, channel administration, Arc/allocator/OS storage and
// serializer transients remain explicit separate frontiers, not whole RSS.
fn service_storage_bytes(
    result_slots: usize,
    publication: usize,
    action_publication: usize,
) -> Option<usize> {
    let question_cells = result_slots.checked_add(3)?;
    let actions = result_slots
        .checked_add(3)?
        .checked_add(mondrian_effects::EFFECT_PROCESSOR_FEEDBACK_SLOTS)?;
    let question_bytes = std::mem::size_of::<QuestionCell>()
        .checked_add(MAX_AUTHOR_VIEW_BYTES)?
        .checked_add(EffectProcessorQuestionReceipt::maximum_retained_bytes())?;
    let action_bytes = std::mem::size_of::<NativeVisualGuiActionDelivery>()
        .checked_add(MAX_AUTHOR_VIEW_BYTES)?
        .checked_add(mondrian_effects::EffectProcessorFeedbackSnapshot::maximum_retained_bytes())?;
    std::mem::size_of::<PreviewNativeQuestionService>()
        .checked_add(publication)?
        .checked_add(action_publication)?
        // Current service view and potentially overlapping old/new action binding.
        .checked_add(MAX_AUTHOR_VIEW_BYTES.checked_mul(3)?)?
        .checked_add(std::mem::size_of::<ActionBinding>().checked_mul(2)?)?
        .checked_add(question_cells.checked_mul(question_bytes)?)?
        .checked_add(actions.checked_mul(action_bytes)?)?
        .checked_add(
            question_cells.checked_mul(std::mem::size_of::<NativeVisualQuestionPresentation>())?,
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellState {
    Pending,
    Presented,
    Answered(EffectProcessorQuestionReply),
    Finished(Option<EffectProcessorQuestionReply>),
}
#[derive(Debug)]
struct QuestionCell {
    work: PreviewNativeGuiWork,
    receipt: EffectProcessorQuestionReceipt,
    state: Mutex<CellState>,
}
impl QuestionCell {
    fn dismiss(&self) {
        if let Ok(mut state) = self.state.lock()
            && matches!(*state, CellState::Pending | CellState::Presented)
        {
            *state = CellState::Answered(EffectProcessorQuestionReply::Dismissed);
        }
    }
    fn sample(&self) -> EffectProcessorQuestionState {
        // This is a service failure, not a synthetic execution stop or Error.
        if !self.work.is_current() {
            self.dismiss();
        }
        self.state.lock().map_or(
            EffectProcessorQuestionState::Unavailable,
            |state| match *state {
                CellState::Pending => EffectProcessorQuestionState::Pending,
                CellState::Presented => EffectProcessorQuestionState::Presented,
                CellState::Answered(reply) => EffectProcessorQuestionState::Answered(reply),
                CellState::Finished(_) => EffectProcessorQuestionState::Unavailable,
            },
        )
    }
}

/// Opaque exact real-GUI delivery, carried on the task's existing result channel.
/// A script/serialized Action cannot manufacture or answer this cell.
#[derive(Debug, Clone)]
pub(crate) struct NativeVisualQuestionPresentation {
    cell: Arc<QuestionCell>,
}
impl NativeVisualQuestionPresentation {
    #[cfg(feature = "validation")]
    pub(crate) fn validation_receipt(&self) -> &EffectProcessorQuestionReceipt {
        &self.cell.receipt
    }
    #[cfg(feature = "validation")]
    pub(crate) fn validation_state(&self) -> EffectProcessorQuestionState {
        self.cell
            .state
            .lock()
            .map_or(
                EffectProcessorQuestionState::Unavailable,
                |state| match *state {
                    CellState::Pending => EffectProcessorQuestionState::Pending,
                    CellState::Presented => EffectProcessorQuestionState::Presented,
                    CellState::Answered(reply) | CellState::Finished(Some(reply)) => {
                        EffectProcessorQuestionState::Answered(reply)
                    }
                    CellState::Finished(None) => EffectProcessorQuestionState::Unavailable,
                },
            )
    }
    pub(crate) fn scope(&self) -> PreviewProcessorFeedbackScope {
        self.cell.work.scope
    }
    pub(crate) fn author_view(&self) -> &NativeVisualAuthorView {
        &self.cell.work.view
    }
    pub(crate) fn text(&self) -> &str {
        self.cell.receipt.question.text.text()
    }
    pub(crate) fn is_current(&self) -> bool {
        self.cell.work.is_current()
            && matches!(
                self.cell.sample(),
                EffectProcessorQuestionState::Pending | EffectProcessorQuestionState::Presented
            )
    }
    /// Only the real Window surface-presentation path calls this after current App/GUI admission.
    pub(crate) fn presented(&self) -> bool {
        if !self.cell.work.is_current() {
            self.cell.dismiss();
            return false;
        }
        self.cell.state.lock().is_ok_and(|mut state| match *state {
            CellState::Pending => {
                *state = CellState::Presented;
                true
            }
            CellState::Presented => true,
            _ => false,
        })
    }
    /// Actual local Widget button dispatch, never generic author Action handling.
    /// Yes/No is refused before actual surface presentation; dismissal is always Failed.
    pub(crate) fn answer(&self, reply: EffectProcessorQuestionReply) -> bool {
        if reply == EffectProcessorQuestionReply::Dismissed {
            self.cell.dismiss();
            return true;
        }
        if !self.cell.work.is_current() {
            self.cell.dismiss();
            return false;
        }
        self.cell.state.lock().is_ok_and(|mut state| {
            if *state != CellState::Presented {
                return false;
            }
            *state = CellState::Answered(reply);
            true
        })
    }
    pub(crate) fn dismiss(&self) {
        self.cell.dismiss();
    }
}

struct PreviewNativeQuestionService {
    work: PreviewNativeGuiWork,
    publication: Box<Publication>,
    action_publication: Arc<ActionPublication>,
    cell: Mutex<Option<Arc<QuestionCell>>>,
    capacity_bytes: usize,
}
impl std::fmt::Debug for PreviewNativeQuestionService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreviewNativeQuestionService")
            .field("scope", &self.work.scope)
            .finish_non_exhaustive()
    }
}
impl EffectProcessorQuestionService for PreviewNativeQuestionService {
    fn maximum_storage_bytes(&self) -> usize {
        self.capacity_bytes
    }
    fn gui_state(
        &self,
        preparation: EffectFloatProcessorPreparation,
        _revision: u64,
    ) -> EffectProcessorGuiState {
        if self.work.admits(preparation) {
            self.work.purpose.state()
        } else {
            EffectProcessorGuiState::Closed
        }
    }
    fn gui_presence(
        &self,
        preparation: EffectFloatProcessorPreparation,
        revision: u64,
        owner: mondrian_core::EffectProcessorOwnerId,
    ) -> Option<Weak<dyn mondrian_effects::EffectProcessorGuiPresence>> {
        if !self.work.admits(preparation) {
            return None;
        }
        let lifetime = self.work.lifetime.upgrade()?;
        {
            let mut state = lifetime.state.lock().ok()?;
            if !gui_present(&state)
                || state.view != self.work.view
                || state.current_work != Some(self.work.scope)
            {
                return None;
            }
            if owner.0.is_nil() {
                return None;
            }
            if state.action_binding.as_ref().is_none_or(|binding| {
                binding.view != self.work.view
                    || binding.preparation != preparation
                    || binding.revision != revision
                    || binding.owner != owner
            }) {
                // The exact same native owner retains its original task
                // transport. A replacement bank/child must install a new one.
                state.action_binding = Some(Arc::new(ActionBinding {
                    view: self.work.view.clone(),
                    preparation,
                    revision,
                    owner,
                    publication: Arc::clone(&self.action_publication),
                }));
            }
        }
        let presence: Arc<dyn mondrian_effects::EffectProcessorGuiPresence> = lifetime;
        Some(Arc::downgrade(&presence))
    }
    fn question(&self, view: EffectProcessorQuestionView<'_>) -> EffectProcessorQuestionState {
        if !self.work.admits(view.preparation)
            || self.work.purpose == NativeVisualGuiWorkPurpose::Observe
        {
            return EffectProcessorQuestionState::Unavailable;
        }
        let Ok(mut slot) = self.cell.lock() else {
            return EffectProcessorQuestionState::Unavailable;
        };
        if let Some(cell) = slot.as_ref() {
            return if view.matches(&cell.receipt) {
                cell.sample()
            } else {
                EffectProcessorQuestionState::Unavailable
            };
        }
        let Some(lifetime) = self.work.lifetime.upgrade() else {
            return EffectProcessorQuestionState::Unavailable;
        };
        // One first publication acquires complete bounded text. Repeated owner
        // polls borrow it; no per-poll text clone or parallel result queue exists.
        let cell = Arc::new(QuestionCell {
            work: self.work.clone(),
            receipt: view.to_owned(),
            state: Mutex::new(CellState::Pending),
        });
        {
            let Ok(mut state) = lifetime.state.lock() else {
                return EffectProcessorQuestionState::Unavailable;
            };
            if !gui_present(&state)
                || state.view != self.work.view
                || state.current_work != Some(self.work.scope)
            {
                return EffectProcessorQuestionState::Unavailable;
            }
            if state.question.upgrade().is_some() {
                return EffectProcessorQuestionState::Unavailable;
            }
            state.question = Arc::downgrade(&cell);
        }
        *slot = Some(cell.clone());
        drop(slot);
        let presentation = NativeVisualQuestionPresentation { cell: cell.clone() };
        if !(self.publication)(presentation) {
            cell.dismiss();
            return EffectProcessorQuestionState::Unavailable;
        }
        cell.sample()
    }
    fn finish_question(&self, view: EffectProcessorQuestionView<'_>) {
        if let Ok(mut slot) = self.cell.lock()
            && slot.as_ref().is_some_and(|cell| view.matches(&cell.receipt))
            && let Some(cell) = slot.take()
        {
            if let Ok(mut state) = cell.state.lock() {
                // Keep only the actual terminal reply in this same canonical cell.
                // Service sampling remains Unavailable after completion; the
                // validation observer cannot reactivate or answer the service.
                let reply = match *state {
                    CellState::Answered(reply) | CellState::Finished(Some(reply)) => Some(reply),
                    _ => None,
                };
                *state = CellState::Finished(reply);
            }
            if let Some(lifetime) = self.work.lifetime.upgrade()
                && let Ok(mut state) = lifetime.state.lock()
                && state.question.upgrade().is_some_and(|original| Arc::ptr_eq(&original, &cell))
            {
                state.question = Weak::new();
            }
        }
    }
}
impl Drop for PreviewNativeQuestionService {
    fn drop(&mut self) {
        if let Ok(slot) = self.cell.get_mut()
            && let Some(cell) = slot.take()
        {
            cell.dismiss();
            if let Some(lifetime) = self.work.lifetime.upgrade()
                && let Ok(mut state) = lifetime.state.lock()
                && state.question.upgrade().is_some_and(|original| Arc::ptr_eq(&original, &cell))
            {
                state.question = Weak::new();
            }
        }
    }
}

/// Root's section-backed metadata projection. It owns no native instance,
/// pixels, author transaction, worker queue or deadline authority.
#[derive(Debug, Default)]
pub(crate) struct PreviewNativeGuiRegistry {
    controllers: Mutex<Vec<Arc<PreviewNativeGuiController>>>,
    revision: Arc<GuiRevision>,
}
impl PreviewNativeGuiRegistry {
    /// Read current section lifetime facts without creating or binding GUI work.
    #[cfg(feature = "validation")]
    pub(crate) fn validation_presence(&self) -> Option<(usize, usize, usize, u64)> {
        let controllers = self.controllers.lock().ok()?;
        let mut present = 0;
        let mut sections = 0;
        for controller in controllers.iter() {
            let state = controller.lifetime.state.lock().ok()?;
            present += usize::from(gui_present(&state));
            sections += state.registered_sections;
        }
        Some((controllers.len(), present, sections, self.revision()))
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.current()
    }
    pub(crate) fn reconcile_views(&self, views: &[NativeVisualAuthorView]) {
        let Ok(mut controllers) = self.controllers.lock() else {
            return;
        };
        let mut next = Vec::new();
        if next.try_reserve_exact(views.len()).is_err() {
            for controller in controllers.iter() {
                controller.close();
            }
            controllers.clear();
            return;
        }
        for view in views {
            if let Some(existing) = controllers.iter().find(|controller| {
                controller.current_view().is_some_and(|old| same_address(&old, view))
            }) {
                if existing.update_view(view.clone()) {
                    next.push(Arc::clone(existing));
                }
            } else if let Some(controller) =
                PreviewNativeGuiController::open(view.clone(), Arc::clone(&self.revision))
            {
                next.push(Arc::new(controller));
            }
        }
        for old in controllers.iter() {
            if !next.iter().any(|current| Arc::ptr_eq(current, old)) {
                old.close();
            }
        }
        *controllers = next;
    }
    pub(crate) fn controller(
        &self,
        view: &NativeVisualAuthorView,
    ) -> Option<Arc<PreviewNativeGuiController>> {
        self.controllers
            .lock()
            .ok()?
            .iter()
            .find(|controller| controller.current_view().as_ref() == Some(view))
            .map(Arc::clone)
    }
    pub(crate) fn set_panel_active(&self, active: bool) {
        if let Ok(controllers) = self.controllers.lock() {
            for controller in controllers.iter() {
                controller.set_panel_active(active);
            }
        }
    }
    pub(crate) fn bind_work(
        &self,
        planning: NativeVisualGuiPlanningIdentity,
        scope: PreviewProcessorFeedbackScope,
        purpose: NativeVisualGuiWorkPurpose,
    ) -> Option<PreviewNativeGuiWorkBatch> {
        let controllers = self.controllers.lock().ok()?;
        let mut works = Vec::new();
        works.try_reserve_exact(controllers.len()).ok()?;
        for controller in controllers.iter() {
            if let Some(work) = controller.bind_work(planning, scope, purpose) {
                works.push(work);
            }
        }
        (!works.is_empty()).then_some(PreviewNativeGuiWorkBatch { works })
    }
}

fn same_address(left: &NativeVisualAuthorView, right: &NativeVisualAuthorView) -> bool {
    left.session_id() == right.session_id()
        && left.sequence_id() == right.sequence_id()
        && left.clip_id() == right.clip_id()
        && left.effect_id() == right.effect_id()
        && left.definition() == right.definition()
}

#[derive(Debug, Clone)]
pub(crate) struct PreviewNativeGuiWorkBatch {
    works: Vec<PreviewNativeGuiWork>,
}
impl PreviewNativeGuiWorkBatch {
    pub(crate) fn service(
        self,
        result_slots: usize,
        publication: impl Fn(NativeVisualQuestionPresentation) -> bool + Send + Sync + 'static,
        action_publication: impl Fn(NativeVisualGuiActionDelivery) -> bool + Send + Sync + 'static,
    ) -> Option<Arc<dyn EffectProcessorQuestionService>> {
        let mut services = Vec::new();
        services.try_reserve_exact(self.works.len()).ok()?;
        let publication_bytes = std::mem::size_of_val(&publication);
        let action_publication_bytes = std::mem::size_of_val(&action_publication);
        let publication: Arc<Publication> = Arc::new(publication);
        let action_publication: Arc<ActionPublication> = Arc::new(action_publication);
        let mut bytes =
            std::mem::size_of::<BatchQuestionService>()
                .checked_add(publication_bytes)?
                .checked_add(action_publication_bytes)?
                .checked_add(services.capacity().checked_mul(std::mem::size_of::<
                    Arc<dyn EffectProcessorQuestionService>,
                >())?)?;
        for work in self.works {
            let callback = Arc::clone(&publication);
            let actions = Arc::clone(&action_publication);
            let service = work.service(
                result_slots,
                move |presentation| callback(presentation),
                move |action| actions(action),
            )?;
            bytes = bytes.checked_add(service.maximum_storage_bytes())?;
            services.push(service);
        }
        Some(Arc::new(BatchQuestionService { services, bytes }))
    }
}

#[derive(Debug)]
struct BatchQuestionService {
    services: Vec<Arc<dyn EffectProcessorQuestionService>>,
    bytes: usize,
}
impl EffectProcessorQuestionService for BatchQuestionService {
    fn maximum_storage_bytes(&self) -> usize {
        self.bytes
    }
    fn gui_state(
        &self,
        preparation: EffectFloatProcessorPreparation,
        revision: u64,
    ) -> EffectProcessorGuiState {
        self.services
            .iter()
            .map(|service| service.gui_state(preparation, revision))
            .find(|state| *state != EffectProcessorGuiState::Closed)
            .unwrap_or(EffectProcessorGuiState::Closed)
    }
    fn gui_presence(
        &self,
        preparation: EffectFloatProcessorPreparation,
        revision: u64,
        owner: mondrian_core::EffectProcessorOwnerId,
    ) -> Option<Weak<dyn mondrian_effects::EffectProcessorGuiPresence>> {
        self.services
            .iter()
            .find_map(|service| service.gui_presence(preparation, revision, owner))
    }
    fn question(&self, view: EffectProcessorQuestionView<'_>) -> EffectProcessorQuestionState {
        self.services
            .iter()
            .find(|service| {
                service.gui_state(view.preparation, view.provider_revision)
                    != EffectProcessorGuiState::Closed
            })
            .map_or(EffectProcessorQuestionState::Unavailable, |service| {
                service.question(view)
            })
    }
    fn finish_question(&self, view: EffectProcessorQuestionView<'_>) {
        for service in &self.services {
            service.finish_question(view);
        }
    }
}

#[cfg(test)]
pub(crate) mod question_contract_tests {
    use super::*;
    use mondrian_core::effect_data::{EffectNode, EffectType};
    use mondrian_core::{AssetId, TimelineTime};

    pub(crate) struct Fixture {
        _directory: tempfile::TempDir,
        pub(crate) session: mondrian_editor_state::AuthoringSession,
        pub(crate) controller: Arc<PreviewNativeGuiController>,
        pub(crate) presence: Option<NativeVisualGuiSectionPresence>,
        pub(crate) preparation: EffectFloatProcessorPreparation,
        pub(crate) receipt: mondrian_effects::EffectProcessorLiveQuestion,
        pub(crate) service: Arc<dyn EffectProcessorQuestionService>,
        pub(crate) presentation: NativeVisualQuestionPresentation,
        delivery: Arc<Mutex<Option<NativeVisualQuestionPresentation>>>,
    }
    impl Fixture {
        pub(crate) fn sample(&self) -> EffectProcessorQuestionState {
            self.service.question(EffectProcessorQuestionView {
                preparation: self.preparation,
                provider_revision: 7,
                question: &self.receipt,
            })
        }
    }

    pub(crate) fn fixture() -> Fixture {
        let directory = tempfile::tempdir().expect("canonical author fixture");
        let library = mondrian_assets::AssetLibrary::open(directory.path().join("library"))
            .expect("actual library");
        let mut sequence = mondrian_timeline::Sequence::new("Question author GUI");
        let mut clip = mondrian_timeline::Clip::new_solid_color(
            AssetId::new(),
            mondrian_core::Color::WHITE,
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .expect("visual Clip");
        let effect = EffectNode::new(EffectType::Plugin("owned.question.fixture".to_owned()));
        let effect_id = effect.id;
        clip.effects.push(effect);
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("actual visual occurrence");
        let sequence_id = sequence.id;
        let transport =
            mondrian_playback::PlaybackEngine::new(sequence.time_base(), Default::default())
                .expect("canonical transport owner");
        let epoch = transport.snapshot().epoch;
        let document = mondrian_project::ProjectDocument::new(
            "Question project",
            mondrian_timeline::SequenceCollection::new(sequence),
            mondrian_core::ProjectColorEnvironment::default(),
            mondrian_timeline::SequenceSettings::default(),
            mondrian_core::ProjectSettings::default(),
        );
        let session = mondrian_editor_state::AuthoringSession::new_unsaved(
            document,
            directory.path().join("project.mdp"),
            directory.path().to_owned(),
            library,
        )
        .expect("actual Session");
        let view = session
            .native_visual_author_view(sequence_id, clip_id, effect_id)
            .expect("issued view");
        let controller = Arc::new(
            PreviewNativeGuiController::open(view.clone(), Arc::default()).expect("GUI projection"),
        );
        controller.set_panel_active(true);
        let presence = controller.section_presence().expect("actual section guard");
        let scope = PreviewProcessorFeedbackScope { key: Some(PreviewProcessorWorkKey::Visual(
            super::super::preview_visual_execution_task::VisualExecutionTaskKey::from_complete_semantic_fingerprint([31; 32]))),
            generation: 11, epoch };
        let work = controller
            .bind_work(
                NativeVisualGuiPlanningIdentity {
                    session: session.session_id(),
                    author_generation: session.author_generation().get(),
                    sequence: sequence_id,
                },
                scope,
                NativeVisualGuiWorkPurpose::EditorRefresh,
            )
            .expect("actual current work");
        let delivery = Arc::new(Mutex::new(None));
        let publication = delivery.clone();
        let service = work
            .service(
                2,
                move |presentation| {
                    *publication.lock().expect("delivery") = Some(presentation);
                    true
                },
                |_| false,
            )
            .expect("bounded current GUI service");
        let preparation = EffectFloatProcessorPreparation {
            effect_id,
            width: 1,
            height: 1,
            format: mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
        };
        let receipt = mondrian_effects::EffectProcessorLiveQuestion {
            owner: mondrian_core::EffectProcessorOwnerId::new(),
            origin: mondrian_effects::EffectProcessorExecutionOrigin::InProcess,
            id: mondrian_effects::EffectProcessorQuestionId::new(3, 5, 7).expect("nonzero service"),
            text: mondrian_effects::EffectProcessorNotice::new(
                mondrian_effects::EffectProcessorNoticeKind::Information,
                "原始插件问题 / Original plugin Question?".to_owned(),
                "owned.question".to_owned(),
            )
            .expect("full text"),
        };
        assert_eq!(
            service.question(EffectProcessorQuestionView {
                preparation,
                provider_revision: 7,
                question: &receipt
            }),
            EffectProcessorQuestionState::Pending
        );
        let presentation = delivery.lock().expect("delivery").take().expect("one publication");
        Fixture {
            _directory: directory,
            session,
            controller,
            presence: Some(presence),
            preparation,
            receipt,
            service,
            presentation,
            delivery,
        }
    }

    #[test]
    fn product_delivery_storage_includes_action_drain_owned_views_and_both_callbacks() {
        let minimal = service_storage_bytes(0, 0, 0).expect("bounded fixed frontier");
        let action = std::mem::size_of::<NativeVisualGuiActionDelivery>()
            + MAX_AUTHOR_VIEW_BYTES
            + mondrian_effects::EffectProcessorFeedbackSnapshot::maximum_retained_bytes();
        assert!(
            minimal >= mondrian_effects::EFFECT_PROCESSOR_FEEDBACK_SLOTS * action,
            "the actual 32-slot product drain stores complete view/feedback clones"
        );
        assert_eq!(
            service_storage_bytes(0, 17, 31).expect("both real captures"),
            minimal + 48
        );
        assert!(
            service_storage_bytes(usize::MAX, 0, 0).is_none(),
            "unrepresentable service remains unavailable rather than fake a small grant"
        );
    }

    #[test]
    fn idle_action_uses_original_bounded_channel_after_real_section_closed_and_rejects_author_replacement(
    ) {
        let mut fixture = fixture();
        let planning = NativeVisualGuiPlanningIdentity {
            session: fixture.session.session_id(),
            author_generation: fixture.session.author_generation().get(),
            sequence: fixture.presentation.author_view().sequence_id(),
        };
        let work = fixture
            .controller
            .bind_work(
                planning,
                fixture.presentation.scope(),
                NativeVisualGuiWorkPurpose::Observe,
            )
            .expect("same actual generic GUI");
        let delivered = Arc::new(Mutex::new(None));
        let publication = Arc::clone(&delivered);
        let service = work
            .service(
                2,
                |_| false,
                move |action| {
                    *publication.lock().expect("channel") = Some(action);
                    true
                },
            )
            .expect("bounded current GUI service");
        let presence = service
            .gui_presence(fixture.preparation, 7, fixture.receipt.owner)
            .expect("actual section lifetime");
        drop(service);
        fixture.controller.set_panel_active(false);
        let receipt = mondrian_effects::EffectProcessorFeedbackReceipt {
            effect_id: fixture.preparation.effect_id,
            preparation: fixture.preparation,
            provider_revision: 7,
            retention: mondrian_effects::EffectProcessorRetention::Retained,
            feedback: mondrian_effects::EffectProcessorFeedback {
                owner: fixture.receipt.owner,
                origin: mondrian_effects::EffectProcessorExecutionOrigin::InProcess,
                snapshot: mondrian_effects::EffectProcessorFeedbackSnapshot::new(
                    3,
                    5,
                    0,
                    None,
                    None,
                    vec![],
                )
                .expect("actual End snapshot"),
            },
        };
        let action =
            mondrian_effects::EffectProcessorGuiActionReceipt { action_serial: 9, receipt };
        assert!(!presence.upgrade().expect("metadata lifetime").is_open());
        assert!(presence.upgrade().expect("metadata lifetime").observe_action(action.clone()));
        let delivery = delivered.lock().expect("same original channel").take().expect("one action");
        assert_eq!(delivery.action, action);
        assert!(fixture.session.native_visual_author_view_is_current(&delivery.view));
        let mut foreign = action.clone();
        foreign.receipt.preparation.effect_id = mondrian_core::EffectId::new();
        assert!(!presence.upgrade().expect("lifetime").observe_action(foreign));
        assert!(!fixture.session.history().can_undo());
        let clip_id = delivery.view.clip_id();
        let effect_id = delivery.view.effect_id();
        let generation = fixture.session.author_generation();
        let (_, commit) = fixture
            .session
            .edit_active_sequence("Change current author occurrence", |sequence| {
                let effect = sequence
                    .find_clip_mut(clip_id)
                    .expect("same authored visual Clip")
                    .effects
                    .iter_mut()
                    .find(|effect| effect.id == effect_id)
                    .expect("same occurrence");
                effect.is_enabled = false;
                Ok(())
            })
            .expect("sole canonical author transaction");
        assert!(commit.is_some());
        assert_ne!(fixture.session.author_generation(), generation);
        assert!(
            !fixture.session.native_visual_author_view_is_current(&delivery.view),
            "actual current author consumer refuses a queued prior-generation End"
        );
        let next = fixture
            .session
            .native_visual_author_view(delivery.view.sequence_id(), clip_id, effect_id)
            .expect("new current address from the same Session");
        assert!(fixture.controller.update_view(next));
        assert!(
            !presence.upgrade().expect("same GUI metadata lifetime").observe_action(action),
            "prior binding cannot publish after actual author replacement"
        );
        fixture.controller.close();
    }

    #[test]
    fn question_yes_or_no_requires_presented_original_and_only_one_answer() {
        let fixture = fixture();
        assert!(!fixture.presentation.answer(EffectProcessorQuestionReply::Yes));
        assert_eq!(fixture.sample(), EffectProcessorQuestionState::Pending);
        assert!(fixture.presentation.presented());
        assert!(fixture.presentation.answer(EffectProcessorQuestionReply::No));
        assert!(!fixture.presentation.answer(EffectProcessorQuestionReply::Yes));
        assert_eq!(
            fixture.sample(),
            EffectProcessorQuestionState::Answered(EffectProcessorQuestionReply::No)
        );
        assert!(
            !fixture.session.history().can_undo(),
            "read-only UI replies do not author edits"
        );
    }

    #[test]
    fn gui_address_without_actual_section_cannot_admit_begin_or_question() {
        let mut fixture = fixture();
        assert_eq!(
            fixture.service.gui_state(fixture.preparation, 7),
            EffectProcessorGuiState::QuestionWork
        );
        drop(fixture.presence.take());
        assert_eq!(
            fixture.service.gui_state(fixture.preparation, 7),
            EffectProcessorGuiState::Closed
        );
        assert_eq!(fixture.sample(), EffectProcessorQuestionState::Unavailable);
        assert!(!fixture.presentation.answer(EffectProcessorQuestionReply::Yes));
    }

    #[test]
    fn actual_closed_panel_invalidates_question_without_native_cancellation_or_author_write() {
        let fixture = fixture();
        let generation = fixture.session.author_generation();
        fixture.controller.set_panel_active(false);
        assert_eq!(fixture.sample(), EffectProcessorQuestionState::Unavailable);
        assert_eq!(fixture.session.author_generation(), generation);
        assert!(!fixture.presentation.presented());
    }

    #[test]
    fn stale_original_completion_and_button_cannot_clear_or_answer_next_service() {
        let fixture = fixture();
        let original = EffectProcessorQuestionView {
            preparation: fixture.preparation,
            provider_revision: 7,
            question: &fixture.receipt,
        };
        fixture.service.finish_question(original);
        let mut replacement = fixture.receipt.clone();
        replacement.id = mondrian_effects::EffectProcessorQuestionId::new(3, 6, 8)
            .expect("next actual work and service coordinates");
        let current = EffectProcessorQuestionView {
            preparation: fixture.preparation,
            provider_revision: 7,
            question: &replacement,
        };
        assert_eq!(
            fixture.service.question(current),
            EffectProcessorQuestionState::Pending
        );
        let presentation = fixture
            .delivery
            .lock()
            .expect("one bounded publication")
            .take()
            .expect("next original presentation");
        fixture.service.finish_question(original);
        fixture.presentation.dismiss();
        assert!(!fixture.presentation.answer(EffectProcessorQuestionReply::Yes));
        assert_eq!(
            fixture.service.question(current),
            EffectProcessorQuestionState::Pending
        );
        assert!(presentation.presented());
        assert!(presentation.answer(EffectProcessorQuestionReply::Yes));
        assert_eq!(
            fixture.service.question(current),
            EffectProcessorQuestionState::Answered(EffectProcessorQuestionReply::Yes)
        );
        assert!(!fixture.session.history().can_undo());
    }

    #[test]
    fn unavailable_result_channel_returns_failed_without_execution_stop_or_author_edit() {
        let fixture = fixture();
        let planning = NativeVisualGuiPlanningIdentity {
            session: fixture.session.session_id(),
            author_generation: fixture.session.author_generation().get(),
            sequence: fixture.presentation.author_view().sequence_id(),
        };
        // A real replacement work supersedes the first service before it can
        // publish. No serialized Action, native owner or author edit is created.
        let mut scope = fixture.presentation.scope();
        scope.generation += 1;
        let work = fixture
            .controller
            .bind_work(planning, scope, NativeVisualGuiWorkPurpose::EditorRefresh)
            .expect("current real GUI work");
        let service = work.service(0, |_| false, |_| false).expect("bounded no-channel service");
        let view = EffectProcessorQuestionView {
            preparation: fixture.preparation,
            provider_revision: 7,
            question: &fixture.receipt,
        };
        assert_eq!(
            service.question(view),
            EffectProcessorQuestionState::Unavailable
        );
        service.finish_question(view);
        assert!(!fixture.presentation.presented());
        assert!(!fixture.session.history().can_undo());
        assert_eq!(
            service.gui_state(fixture.preparation, 7),
            EffectProcessorGuiState::QuestionWork,
            "failed publication did not invent lost GUI or native cancellation"
        );
    }
}
