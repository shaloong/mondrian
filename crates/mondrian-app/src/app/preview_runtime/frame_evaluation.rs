//! Terminal frame-evaluation contract shared by every Preview consumer.
//!
//! This Module defines the boundary that keeps
//!
//! ```text
//! "what does this frame look like"
//! ```
//!
//! separate from
//!
//! ```text
//! "why are we computing it now, what is its deadline, is it a successor"
//! ```
//!
//! A [`FrameEvaluationKey`] contains only factors that change the resolved
//! picture. Scheduling state (epoch, playing, seek source, deadlines,
//! successor/current role) lives in [`PreviewExecutionIntent`] and never
//! enters the key, so the same evaluation can be reused across a
//! successor-to-current promotion or a play/pause transition. Media
//! asynchrony is carried as typed [`EvaluationDependency`]s and drives a
//! [`EvaluationState`] machine instead of a boolean memo.
//!
//! The long-term rule enforced here is:
//!
//! > One semantic frame evaluation has exactly one authoritative producer;
//! > GPU production, CPU production, presentation arbitration, and headless
//! > consumers only consume [`ResolvedFrameEvaluation`].
//!
//! P6 commit 1 declares the interface types with behavior unchanged; the
//! coordinator that produces them is wired in by later commits. Remove this
//! module-level expect once every type below is constructed by the runtime.
#![expect(
    dead_code,
    reason = "P6 commit 1: interface types; wired in by later commits"
)]

use std::sync::Arc;

use mondrian_core::display_contract::DisplayOutputIdentity;
use mondrian_core::types::{AssetId, ColorSpace, SequenceId};
use mondrian_core::SequenceRevision;
use mondrian_playback::PreviewResolutionScale;
use mondrian_timeline::sequence::ProgramColorContext;

use crate::app::preview_access_mode::MediaPreviewRequestPriority;
use crate::app::preview_execution::PreviewOutputKey;
use crate::app::preview_runtime::PreviewAuthoringSnapshot;
use crate::app::preview_viewer_plan::ResolvedPreviewElement;

/// What changes the resolved picture for one timeline frame.
///
/// Everything in this key must be a factor of [`ResolvedFrameEvaluation`]
/// content. Scheduling state is intentionally absent: `playing`, seek
/// source, epochs, deadlines, and request priority never appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameEvaluationKey {
    pub(crate) sequence_id: SequenceId,
    pub(crate) sequence_revision: SequenceRevision,
    /// Project author generation; an authoring mutation changes the picture.
    pub(crate) author_generation: u64,
    /// Exact timeline frame being evaluated.
    pub(crate) frame: i64,
    /// Output extent in logical pixels.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Runtime-only spatial quality selected by recovery/resource policy.
    pub(crate) runtime_scale: PreviewResolutionScale,
    /// Display color space that shapes the monitor adaptation boundary.
    pub(crate) display_color_space: ColorSpace,
    /// Display output contract identity, when one is attached.
    pub(crate) display_contract_identity: Option<DisplayOutputIdentity>,
}

/// Immutable input for one frame evaluation.
///
/// `authoring` is currently a borrowed snapshot; a future refactor may
/// promote it to `Arc<AuthoringSnapshot>` so one evaluation observes one
/// world state without borrowing mutable authoring during resolve.
pub(crate) struct FrameEvaluationRequest<'a> {
    pub(crate) key: FrameEvaluationKey,
    pub(crate) authoring: &'a PreviewAuthoringSnapshot<'a>,
}

/// Execution intent attached to one frame request.
///
/// This is scheduling and consumer identity, never frame content. Two
/// requests with the same [`FrameEvaluationKey`] but different intents
/// share one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewExecutionIntent {
    pub(crate) mode: PreviewMode,
    pub(crate) role: PreviewRole,
    pub(crate) priority: MediaPreviewRequestPriority,
}

impl PreviewExecutionIntent {
    pub(crate) const fn current(mode: PreviewMode) -> Self {
        Self {
            mode,
            role: PreviewRole::Current,
            priority: MediaPreviewRequestPriority::Current,
        }
    }

    pub(crate) const fn successor(mode: PreviewMode) -> Self {
        Self {
            mode,
            role: PreviewRole::Successor,
            priority: MediaPreviewRequestPriority::Prefetch,
        }
    }
}

/// Consumer class that requested the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewMode {
    Playback,
    Scrub,
    Still,
    Headless,
}

/// Whether the request is the visible current frame or speculative
/// immediate-successor preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewRole {
    Current,
    Successor,
}

/// One complete authoritative evaluation request.
pub(crate) struct PreviewFrameRequest<'a> {
    pub(crate) evaluation: FrameEvaluationRequest<'a>,
    pub(crate) intent: PreviewExecutionIntent,
}

/// Outcome of one frame evaluation lookup/resolve attempt.
///
/// [`EvaluationState`] is the coordinator-shaped view used once dependency
/// tracking lands; [`FrameResolutionOutcome`] is the resolver-shaped view
/// that preserves every branch of today's single resolve call so consumers
/// keep their branch-local side effects while sharing the evaluation
/// construction.
pub(crate) enum FrameResolutionOutcome {
    /// A fully resolved, immutable evaluation is available.
    Ready(Arc<ResolvedFrameEvaluation>),
    /// The frame resolves to the transparent canvas.
    Empty,
    /// Resolution is blocked on one concrete dependency.
    Pending(crate::app::preview_timeline_execution::PreviewTimelinePendingDependency),
    /// Resolution failed closed with a typed reason.
    Unavailable(crate::app::preview_unavailability::PreviewUnavailability),
}

/// Outcome of one evaluation lookup/resolve attempt.
pub(crate) enum EvaluationState {
    /// A fully resolved, immutable evaluation is available.
    Ready(Arc<ResolvedFrameEvaluation>),
    /// Resolution is blocked on concrete dependencies. Re-resolve is
    /// deferred until one of them is invalidated; repeated acquires with
    /// unchanged dependencies must not re-resolve.
    Waiting(Arc<[EvaluationDependency]>),
    /// Resolution failed closed with a typed reason.
    Unavailable(EvaluationUnavailableReason),
}

/// Typed dependency of one frame evaluation.
///
/// Initial granularity is per-asset; future variants may name exact source
/// PTS, asset revisions, generator resources, or remote media arrivals so
/// invalidation never cascades to unrelated evaluations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationDependency {
    MediaFrame(AssetId),
}

/// Typed reason why evaluation could not produce a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationUnavailableReason {
    NoActiveSequence,
    TimelineUnavailable,
}

/// Immutable, generation-independent authoritative frame plan.
///
/// `output_key` is constructed exactly once here; consumers must read it,
/// never re-hash the plan. `elements` are held as `Arc` handles; the
/// evaluation working set must stay tiny so these handles never become an
/// invisible decoded-frame cache.
pub(crate) struct ResolvedFrameEvaluation {
    pub(crate) key: FrameEvaluationKey,
    pub(crate) output_key: PreviewOutputKey,
    pub(crate) elements: Arc<[ResolvedPreviewElement]>,
    pub(crate) color_context: ProgramColorContext,
    pub(crate) resolved_quality: ResolvedFrameQuality,
    pub(crate) reuse_policy: EvaluationReusePolicy,
    pub(crate) dependencies: Arc<[EvaluationDependency]>,
}

/// Spatial quality of the resolved evaluation itself.
///
/// This is distinct from presentation freshness (Current/Stale/Fallback),
/// which is an arbitration result and never part of the evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedFrameQuality {
    Proxy,
    Half,
    Full,
}

/// Whether the resolved plan may be cached across consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationReusePolicy {
    /// The plan is deterministic for its key and may be retained.
    Reusable,
    /// The plan carries transient identity and must be re-resolved.
    Transient,
}

/// One evaluation bound to its scheduling generation and execution intent.
///
/// The evaluation itself may outlive a generation: a successor evaluated
/// under generation 51 becomes the current frame under generation 52
/// without re-resolution, while its produced candidates never gain
/// presentation authority outside their own generation.
pub(crate) struct FrameEvaluationLease {
    pub(crate) generation: u64,
    pub(crate) intent: PreviewExecutionIntent,
    pub(crate) evaluation: Arc<ResolvedFrameEvaluation>,
}

/// Tiny role-aware bounded working set of resolved evaluations.
///
/// Deliberately not a general LRU: every entry pins the decoded frames its
/// elements reference, so the set must stay small (2-4 entries) and evict
/// the least recently used evaluation. Successor-to-current promotion
/// reuses the same entry because the key never contains the role.
pub(crate) struct EvaluationWorkingSet {
    entries: Vec<EvaluationWorkingSetEntry>,
}

struct EvaluationWorkingSetEntry {
    key: FrameEvaluationKey,
    evaluation: Arc<ResolvedFrameEvaluation>,
    last_used: u64,
}

impl EvaluationWorkingSet {
    pub(crate) const fn capacity() -> usize {
        4
    }

    pub(crate) fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Drop every retained evaluation; used by coarse dependency invalidation.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Return the retained evaluation for an exact key, if resident.
    pub(crate) fn get(
        &mut self,
        key: FrameEvaluationKey,
        clock: u64,
    ) -> Option<Arc<ResolvedFrameEvaluation>> {
        let entry = self.entries.iter_mut().find(|entry| entry.key == key)?;
        entry.last_used = clock;
        Some(Arc::clone(&entry.evaluation))
    }

    /// Retain one evaluation for its key, evicting the least recently used
    /// entry when the set is full.
    pub(crate) fn insert(
        &mut self,
        key: FrameEvaluationKey,
        evaluation: Arc<ResolvedFrameEvaluation>,
        clock: u64,
    ) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            entry.evaluation = evaluation;
            entry.last_used = clock;
            return;
        }
        if self.entries.len() >= Self::capacity() {
            let least = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| index)
                .expect("non-empty working set");
            self.entries.remove(least);
        }
        self.entries
            .push(EvaluationWorkingSetEntry { key, evaluation, last_used: clock });
    }
}
