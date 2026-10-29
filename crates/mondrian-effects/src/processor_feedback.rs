//! Bounded processor feedback; execution facts are never authoring authority.

use mondrian_core::{EffectId, EffectProcessorOwnerId};
use serde::{Deserialize, Serialize};

/// Explicit product policy; these are not bounds imposed by any plugin standard.
pub const EFFECT_PROCESSOR_NOTICE_SLOTS: usize = 32;
/// Maximum complete UTF-8 text/label bytes admitted per notice.
pub const EFFECT_PROCESSOR_NOTICE_TEXT_BYTES: usize = 4096;
/// Maximum optional plugin-local notice identifier bytes.
pub const EFFECT_PROCESSOR_NOTICE_ID_BYTES: usize = 256;
/// Maximum feedback-bearing processor occurrences per execution attempt.
pub const EFFECT_PROCESSOR_FEEDBACK_SLOTS: usize = 32;

/// Bounded observational requirements from the sole compiled execution graph.
/// No graph, instance, pixels, or authoring interpretation is duplicated here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectProcessorFeedbackRequirements {
    occurrences: usize,
    owned_bytes: usize,
}
impl EffectProcessorFeedbackRequirements {
    /// Count scheduled providers and their maximum separately owned metadata.
    /// This conservative upper bound also covers cache misses and first prepares.
    pub fn for_compiled_graph(compiled: &crate::CompiledEffectGraph) -> Self {
        let mut requirements = Self::default();
        for id in &compiled.schedule().ordered_nodes {
            let Some(node) = compiled.graph().node(*id) else {
                continue;
            };
            if let crate::EffectGraphNodeKind::UnaryEffect {
                op: crate::EffectRenderOp::Custom { processor: Some(binding), .. },
                ..
            }
            | crate::EffectGraphNodeKind::CustomSource {
                op: crate::EffectRenderOp::Custom { processor: Some(binding), .. },
                ..
            }
            | crate::EffectGraphNodeKind::DomainEffect {
                op: crate::EffectRenderOp::Custom { processor: Some(binding), .. },
                ..
            } = &node.kind
                && let Some(factory) = binding.float32_factory()
            {
                let bound = factory.feedback_budget_bytes();
                if bound != 0 {
                    requirements.occurrences = requirements.occurrences.saturating_add(1);
                    requirements.owned_bytes = requirements.owned_bytes.saturating_add(
                        bound
                            .saturating_sub(std::mem::size_of::<EffectProcessorFeedbackSnapshot>()),
                    );
                }
            }
        }
        requirements
    }
    /// Combine already compiled routes before any stateful occurrence executes.
    pub fn merge(&mut self, other: Self) {
        self.occurrences = self.occurrences.saturating_add(other.occurrences);
        self.owned_bytes = self.owned_bytes.saturating_add(other.owned_bytes);
    }
    /// Maximum feedback-bearing occurrences, not the number already executed.
    pub const fn occurrences(self) -> usize {
        self.occurrences
    }
    /// Maximum separately owned text/notice storage, excluding inline receipts.
    pub const fn maximum_owned_bytes(self) -> usize {
        self.owned_bytes
    }
}

/// Meaning assigned by the plugin's standard, not an invented user reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectProcessorNoticeKind {
    /// Informational user-visible fact.
    Information,
    /// A warning permits the current invocation to proceed.
    Warning,
    /// Persistent error requires repair/clear before proceeding.
    Error,
    /// Diagnostic-only text, not a popup request.
    Log,
    /// Nonrecoverable failure; it does not become UserBlocked.
    Fatal,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoticeWire {
    kind: EffectProcessorNoticeKind,
    text: String,
    id: String,
}

/// Complete bounded plugin-authored UTF-8 data, without truncation or translation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "NoticeWire")]
pub struct EffectProcessorNotice {
    kind: EffectProcessorNoticeKind,
    text: Box<str>,
    id: Box<str>,
}

impl EffectProcessorNotice {
    /// Admit complete text atomically under the declared host policy.
    pub fn new(
        kind: EffectProcessorNoticeKind,
        text: String,
        id: String,
    ) -> Result<Self, EffectProcessorFeedbackError> {
        if text.len() > EFFECT_PROCESSOR_NOTICE_TEXT_BYTES
            || id.len() > EFFECT_PROCESSOR_NOTICE_ID_BYTES
            || text.contains('\0')
            || id.contains('\0')
        {
            return Err(EffectProcessorFeedbackError::NoticeBounds);
        }
        Ok(Self {
            kind,
            text: text.into_boxed_str(),
            id: id.into_boxed_str(),
        })
    }
    /// Actual standard notice kind.
    pub const fn kind(&self) -> EffectProcessorNoticeKind {
        self.kind
    }
    /// Unmodified plugin-authored UTF-8 text.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Optional plugin-local ID; the empty value has no supplied ID.
    pub fn id(&self) -> &str {
        &self.id
    }
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.text.len() + self.id.len()
    }
    fn owned_text_bytes(&self) -> usize {
        self.text.len() + self.id.len()
    }
}
impl TryFrom<NoticeWire> for EffectProcessorNotice {
    type Error = EffectProcessorFeedbackError;
    fn try_from(value: NoticeWire) -> Result<Self, Self::Error> {
        Self::new(value.kind, value.text, value.id)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressWire {
    generation: u64,
    active: bool,
    fraction: f64,
    label: EffectProcessorNotice,
}

/// One actual begin/update/end progress generation, sampled at terminal delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ProgressWire")]
pub struct EffectProcessorProgress {
    generation: u64,
    active: bool,
    fraction: f64,
    label: EffectProcessorNotice,
}
impl Eq for EffectProcessorProgress {}
impl EffectProcessorProgress {
    /// Validate real progress facts without fabricating cancellation.
    pub fn new(
        generation: u64,
        active: bool,
        fraction: f64,
        label: EffectProcessorNotice,
    ) -> Result<Self, EffectProcessorFeedbackError> {
        if generation == 0
            || !fraction.is_finite()
            || !(0.0..=1.0).contains(&fraction)
            || label.kind() != EffectProcessorNoticeKind::Information
        {
            return Err(EffectProcessorFeedbackError::Progress);
        }
        Ok(Self { generation, active, fraction, label })
    }
    /// Monotonic begin/end generation within the exact processor owner.
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    /// Whether the plugin has an actual open progress display.
    pub const fn active(&self) -> bool {
        self.active
    }
    /// Latest admitted finite `[0,1]` fraction.
    pub const fn fraction(&self) -> f64 {
        self.fraction
    }
    /// Complete label and optional plugin-local identifier.
    pub const fn label(&self) -> &EffectProcessorNotice {
        &self.label
    }
}
impl TryFrom<ProgressWire> for EffectProcessorProgress {
    type Error = EffectProcessorFeedbackError;
    fn try_from(value: ProgressWire) -> Result<Self, Self::Error> {
        Self::new(value.generation, value.active, value.fraction, value.label)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWire {
    native_owner: u64,
    invocation: u64,
    persistent_revision: u64,
    persistent: Option<EffectProcessorNotice>,
    progress: Option<EffectProcessorProgress>,
    #[serde(deserialize_with = "deserialize_bounded_notices")]
    notices: Vec<EffectProcessorNotice>,
}

fn deserialize_bounded_notices<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Vec<EffectProcessorNotice>, D::Error> {
    struct Notices;
    impl<'de> serde::de::Visitor<'de> for Notices {
        type Value = Vec<EffectProcessorNotice>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("at most 32 complete bounded processor notices")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut notices = Vec::with_capacity(
                sequence.size_hint().unwrap_or(0).min(EFFECT_PROCESSOR_NOTICE_SLOTS),
            );
            while notices.len() < EFFECT_PROCESSOR_NOTICE_SLOTS {
                let Some(notice) = sequence.next_element::<EffectProcessorNotice>()? else {
                    return Ok(notices);
                };
                notices.push(notice);
            }
            // Refuse before constructing a 33rd owned notice. IgnoredAny skips
            // its shape without allocating another text/notice representation.
            if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom(
                    "processor notice capacity exceeded",
                ));
            }
            Ok(notices)
        }
    }
    decoder.deserialize_seq(Notices)
}

/// Native-owned bounded state. It has no EffectId or playback/authoring identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SnapshotWire")]
pub struct EffectProcessorFeedbackSnapshot {
    native_owner: u64,
    invocation: u64,
    persistent_revision: u64,
    persistent: Option<EffectProcessorNotice>,
    progress: Option<EffectProcessorProgress>,
    notices: Box<[EffectProcessorNotice]>,
}
impl EffectProcessorFeedbackSnapshot {
    /// Validate a complete native report before it enters an execution consumer.
    pub fn new(
        native_owner: u64,
        invocation: u64,
        persistent_revision: u64,
        persistent: Option<EffectProcessorNotice>,
        progress: Option<EffectProcessorProgress>,
        notices: Vec<EffectProcessorNotice>,
    ) -> Result<Self, EffectProcessorFeedbackError> {
        if native_owner == 0
            || invocation == 0
            || notices.len() > EFFECT_PROCESSOR_NOTICE_SLOTS
            || persistent.as_ref().is_some_and(|notice| {
                persistent_revision == 0
                    || !matches!(
                        notice.kind(),
                        EffectProcessorNoticeKind::Information
                            | EffectProcessorNoticeKind::Warning
                            | EffectProcessorNoticeKind::Error
                    )
            })
        {
            return Err(EffectProcessorFeedbackError::Snapshot);
        }
        Ok(Self {
            native_owner,
            invocation,
            persistent_revision,
            persistent,
            progress,
            notices: notices.into_boxed_slice(),
        })
    }
    /// Native callback owner identity, meaningful only inside its supervised child.
    pub const fn native_owner(&self) -> u64 {
        self.native_owner
    }
    /// Actual accepted native invocation serial.
    pub const fn invocation(&self) -> u64 {
        self.invocation
    }
    /// Revision advances on both replace and clear, including an idempotent clear.
    pub const fn persistent_revision(&self) -> u64 {
        self.persistent_revision
    }
    /// Current persistent state; absence with a newer revision proves a clear.
    pub fn persistent(&self) -> Option<&EffectProcessorNotice> {
        self.persistent.as_ref()
    }
    /// Actual progress state, not a claim of live UI streaming.
    pub fn progress(&self) -> Option<&EffectProcessorProgress> {
        self.progress.as_ref()
    }
    /// Bounded transient facts consumed by this native report.
    pub fn notices(&self) -> &[EffectProcessorNotice] {
        &self.notices
    }
    /// Whether the standards-defined persistent error prevents proceeding.
    pub fn is_blocked(&self) -> bool {
        self.persistent()
            .is_some_and(|notice| notice.kind() == EffectProcessorNoticeKind::Error)
    }
    /// Exact logical owned metadata/text storage, excluding allocator bookkeeping.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .persistent
                .iter()
                .map(EffectProcessorNotice::owned_text_bytes)
                .sum::<usize>()
            + self.progress.iter().map(|value| value.label.owned_text_bytes()).sum::<usize>()
            + self.notices.iter().map(EffectProcessorNotice::retained_bytes).sum::<usize>()
    }
    /// Conservative complete snapshot bound used before provider loading.
    pub const fn maximum_retained_bytes() -> usize {
        std::mem::size_of::<Self>()
            + EFFECT_PROCESSOR_NOTICE_SLOTS * std::mem::size_of::<EffectProcessorNotice>()
            + (EFFECT_PROCESSOR_NOTICE_SLOTS + 2)
                * (EFFECT_PROCESSOR_NOTICE_TEXT_BYTES + EFFECT_PROCESSOR_NOTICE_ID_BYTES)
    }
}
impl TryFrom<SnapshotWire> for EffectProcessorFeedbackSnapshot {
    type Error = EffectProcessorFeedbackError;
    fn try_from(value: SnapshotWire) -> Result<Self, Self::Error> {
        Self::new(
            value.native_owner,
            value.invocation,
            value.persistent_revision,
            value.persistent,
            value.progress,
            value.notices,
        )
    }
}

/// Parent-issued identity and validated native feedback; pixels are separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProcessorFeedback {
    /// Trusted supervisor-owned identity; child-local counters cannot replace it.
    pub owner: EffectProcessorOwnerId,
    /// Actual execution location issued by the supervisor, never native text.
    pub origin: EffectProcessorExecutionOrigin,
    /// Actual bounded native facts.
    pub snapshot: EffectProcessorFeedbackSnapshot,
}

/// Actual processor execution location for diagnostics and lifecycle proofs.
/// It is not an authoring identity or a claim that evicted private state survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectProcessorExecutionOrigin {
    /// A provider executes within its sole domain-owned executor.
    InProcess,
    /// One supervised native child, issued from the owned process handle.
    SupervisedChild {
        /// Actual nonzero OS process coordinate, not supplied in native notices.
        process_id: std::num::NonZeroU32,
    },
}

/// Trusted compiler/resource binding attached after exactly one processor call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProcessorFeedbackReceipt {
    /// Authored occurrence from the compiler, never supplied by the plugin.
    pub effect_id: EffectId,
    /// Exact sizing/format identity of the sole residency key.
    pub preparation: crate::EffectFloatProcessorPreparation,
    /// Immutable provider registration revision of that key.
    pub provider_revision: u64,
    /// Actual retained/evicted decision; it does not change the feedback meaning.
    pub retention: crate::EffectProcessorRetention,
    /// Supervisor identity and actual facts.
    pub feedback: EffectProcessorFeedback,
}
impl EffectProcessorFeedbackReceipt {
    /// Logical retained receipt storage, with no raster allocation.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.feedback.snapshot.retained_bytes()
            - std::mem::size_of::<EffectProcessorFeedbackSnapshot>()
    }
}
impl std::fmt::Display for EffectProcessorFeedbackReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = self.feedback.snapshot.persistent().map_or("", EffectProcessorNotice::text);
        formatter.write_str(text)
    }
}

/// Invalid/unbounded metadata is a protocol failure, not user blocking.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectProcessorFeedbackError {
    /// The immutable route's explicit occurrence policy was exhausted.
    #[error("processor feedback occurrence capacity is exhausted")]
    Capacity,
    /// Complete UTF-8 data exceeds the host policy or contains embedded NUL.
    #[error("processor notice exceeds admitted text/identifier bounds")]
    NoticeBounds,
    /// Invalid lifecycle generation, fraction or label.
    #[error("processor progress facts violate their admitted lifecycle")]
    Progress,
    /// Invalid native owner/invocation, persistent kind or queue count.
    #[error("processor feedback violates its bounded snapshot contract")]
    Snapshot,
}

/// One fixed-capacity metadata owner. Receiving/moving actual receipts never
/// grows a Vec after a stateful DSP callback. Text remains in its bounded owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProcessorFeedbackBatch {
    entries: [Option<EffectProcessorFeedbackReceipt>; EFFECT_PROCESSOR_FEEDBACK_SLOTS],
    len: usize,
}
impl Default for EffectProcessorFeedbackBatch {
    fn default() -> Self {
        Self::new()
    }
}
impl EffectProcessorFeedbackBatch {
    /// Empty fixed storage, without heap allocation.
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; EFFECT_PROCESSOR_FEEDBACK_SLOTS],
            len: 0,
        }
    }
    /// Actual receipt count; unused storage owns no text or processor.
    pub const fn len(&self) -> usize {
        self.len
    }
    /// Whether any actual processor facts were returned.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Fixed inline bytes used by pre-execution working/retention admission.
    pub const fn storage_bytes() -> usize {
        std::mem::size_of::<Self>()
    }
    /// Actual bounded receipts in execution order.
    pub fn iter(&self) -> impl Iterator<Item = &EffectProcessorFeedbackReceipt> {
        self.entries[..self.len].iter().filter_map(Option::as_ref)
    }
    /// Replace only an already observed exact actual bank binding. Idle GUI
    /// actions cannot add a synthetic frame occurrence or replace a new owner.
    pub fn replace_current(&mut self, receipt: EffectProcessorFeedbackReceipt) -> bool {
        for old in self.entries[..self.len].iter_mut().flatten() {
            if old.preparation == receipt.preparation
                && old.provider_revision == receipt.provider_revision
                && old.feedback.owner == receipt.feedback.owner
                && old.feedback.origin == receipt.feedback.origin
            {
                *old = receipt;
                return true;
            }
        }
        false
    }
    /// All inline storage plus the actual separately owned text/slice bytes.
    /// Allocator usable-size and runtime/OS bookkeeping are separate frontiers.
    pub fn retained_bytes(&self) -> usize {
        Self::storage_bytes()
            + self
                .iter()
                .map(|receipt| {
                    receipt.retained_bytes() - std::mem::size_of::<EffectProcessorFeedbackReceipt>()
                })
                .sum::<usize>()
    }
    /// Admit a receipt into an already reserved slot without allocation.
    /// Callers check capacity before starting the next stateful occurrence.
    pub fn push(
        &mut self,
        receipt: EffectProcessorFeedbackReceipt,
    ) -> Result<(), EffectProcessorFeedbackError> {
        if self.len == EFFECT_PROCESSOR_FEEDBACK_SLOTS {
            return Err(EffectProcessorFeedbackError::Capacity);
        }
        self.entries[self.len] = Some(receipt);
        self.len += 1;
        Ok(())
    }
    /// Transfer a pre-admitted child batch atomically without duplicating text.
    pub fn append(&mut self, other: Self) -> Result<(), EffectProcessorFeedbackError> {
        if other.len > EFFECT_PROCESSOR_FEEDBACK_SLOTS - self.len {
            return Err(EffectProcessorFeedbackError::Capacity);
        }
        for receipt in other {
            self.push(receipt)?;
        }
        Ok(())
    }
}
impl IntoIterator for EffectProcessorFeedbackBatch {
    type Item = EffectProcessorFeedbackReceipt;
    type IntoIter = std::iter::Flatten<
        std::array::IntoIter<
            Option<EffectProcessorFeedbackReceipt>,
            EFFECT_PROCESSOR_FEEDBACK_SLOTS,
        >,
    >;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(kind: EffectProcessorNoticeKind) -> EffectProcessorNotice {
        EffectProcessorNotice::new(kind, "插件 α 中文".to_owned(), "native-id".to_owned())
            .expect("complete UTF-8 notice")
    }
    fn receipt(owner: EffectProcessorOwnerId, invocation: u64) -> EffectProcessorFeedbackReceipt {
        let effect_id = EffectId::new();
        EffectProcessorFeedbackReceipt {
            effect_id,
            preparation: crate::EffectFloatProcessorPreparation {
                project_geometry: None,
                input_role: crate::EffectFloatProcessorInputRole::SingleImage,
                effect_id,
                width: 1,
                height: 1,
                format: crate::EffectValueFormat::new(
                    crate::EffectWorkingPrecision::Float32,
                    crate::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
            },
            provider_revision: 1,
            retention: crate::EffectProcessorRetention::Retained,
            feedback: EffectProcessorFeedback {
                owner,
                origin: EffectProcessorExecutionOrigin::InProcess,
                snapshot: EffectProcessorFeedbackSnapshot::new(
                    1,
                    invocation,
                    0,
                    None,
                    None,
                    vec![],
                )
                .expect("actual invocation"),
            },
        }
    }

    #[test]
    fn complete_notice_bounds_count_utf8_bytes_and_never_truncate() {
        let text = "中".repeat(EFFECT_PROCESSOR_NOTICE_TEXT_BYTES / 3);
        let admitted = EffectProcessorNotice::new(
            EffectProcessorNoticeKind::Information,
            text.clone(),
            String::new(),
        )
        .expect("complete UTF-8 fits");
        assert_eq!(admitted.text(), text);
        assert!(EffectProcessorNotice::new(
            EffectProcessorNoticeKind::Warning,
            "中".repeat(EFFECT_PROCESSOR_NOTICE_TEXT_BYTES / 3 + 1),
            String::new()
        )
        .is_err());
        assert!(EffectProcessorNotice::new(
            EffectProcessorNoticeKind::Information,
            "a\0b".to_owned(),
            String::new()
        )
        .is_err());
        assert!(EffectProcessorNotice::new(
            EffectProcessorNoticeKind::Information,
            String::new(),
            "\0".to_owned()
        )
        .is_err());
        assert!(EffectProcessorNotice::new(
            EffectProcessorNoticeKind::Error,
            String::new(),
            "x".repeat(EFFECT_PROCESSOR_NOTICE_ID_BYTES + 1)
        )
        .is_err());
    }

    #[test]
    fn progress_and_snapshot_validate_real_owner_lifecycle() {
        for fraction in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            assert!(EffectProcessorProgress::new(
                1,
                true,
                fraction,
                notice(EffectProcessorNoticeKind::Information)
            )
            .is_err());
        }
        assert!(EffectProcessorProgress::new(
            0,
            true,
            0.0,
            notice(EffectProcessorNoticeKind::Information)
        )
        .is_err());
        assert!(EffectProcessorProgress::new(
            1,
            true,
            0.0,
            notice(EffectProcessorNoticeKind::Warning)
        )
        .is_err());
        for (owner, invocation) in [(0, 1), (1, 0)] {
            assert!(
                EffectProcessorFeedbackSnapshot::new(owner, invocation, 0, None, None, vec![])
                    .is_err()
            );
        }
        assert!(EffectProcessorFeedbackSnapshot::new(
            1,
            1,
            0,
            Some(notice(EffectProcessorNoticeKind::Error)),
            None,
            vec![]
        )
        .is_err());
        assert!(EffectProcessorFeedbackSnapshot::new(
            1,
            1,
            1,
            Some(notice(EffectProcessorNoticeKind::Log)),
            None,
            vec![]
        )
        .is_err());
        let blocked = EffectProcessorFeedbackSnapshot::new(
            1,
            1,
            1,
            Some(notice(EffectProcessorNoticeKind::Error)),
            None,
            vec![],
        )
        .expect("actual persistent error");
        assert!(blocked.is_blocked());
        let cleared = EffectProcessorFeedbackSnapshot::new(1, 2, 2, None, None, vec![])
            .expect("actual clear revision");
        assert!(!cleared.is_blocked());
        assert_eq!(cleared.persistent_revision(), 2);
    }

    #[test]
    fn serde_snapshot_refuses_the_thirty_third_notice() {
        let snapshot = EffectProcessorFeedbackSnapshot::new(
            1,
            1,
            0,
            None,
            None,
            vec![notice(EffectProcessorNoticeKind::Information); EFFECT_PROCESSOR_NOTICE_SLOTS],
        )
        .expect("full admitted notice queue");
        let mut wire = serde_json::to_value(&snapshot).expect("bounded wire");
        let notices = wire["notices"].as_array_mut().expect("notice array");
        notices.push(
            serde_json::to_value(notice(EffectProcessorNoticeKind::Information))
                .expect("one excess notice"),
        );
        assert!(serde_json::from_value::<EffectProcessorFeedbackSnapshot>(wire).is_err());
        assert_eq!(
            serde_json::from_str::<EffectProcessorFeedbackSnapshot>(
                &serde_json::to_string(&snapshot).expect("wire")
            )
            .expect("round trip"),
            snapshot
        );
    }

    #[test]
    fn exact_logical_metadata_charge_counts_inline_fields_once() {
        let full = || {
            EffectProcessorNotice::new(
                EffectProcessorNoticeKind::Information,
                "x".repeat(EFFECT_PROCESSOR_NOTICE_TEXT_BYTES),
                "y".repeat(EFFECT_PROCESSOR_NOTICE_ID_BYTES),
            )
            .expect("full notice")
        };
        let snapshot = EffectProcessorFeedbackSnapshot::new(
            1,
            1,
            1,
            Some(full()),
            Some(EffectProcessorProgress::new(1, false, 1.0, full()).expect("closed progress")),
            (0..EFFECT_PROCESSOR_NOTICE_SLOTS).map(|_| full()).collect(),
        )
        .expect("maximum snapshot");
        assert_eq!(
            snapshot.retained_bytes(),
            EffectProcessorFeedbackSnapshot::maximum_retained_bytes()
        );
        let mut receipt = receipt(EffectProcessorOwnerId::new(), 1);
        receipt.feedback.snapshot = snapshot;
        assert_eq!(
            receipt.retained_bytes(),
            std::mem::size_of::<EffectProcessorFeedbackReceipt>()
                + EffectProcessorFeedbackSnapshot::maximum_retained_bytes()
                - std::mem::size_of::<EffectProcessorFeedbackSnapshot>()
        );
    }

    #[test]
    fn batch_append_refuses_atomically_without_cloning_receipts() {
        let owner = EffectProcessorOwnerId::new();
        let mut target = EffectProcessorFeedbackBatch::new();
        for invocation in 1..EFFECT_PROCESSOR_FEEDBACK_SLOTS as u64 {
            target.push(receipt(owner, invocation)).expect("admitted slot");
        }
        let before = target.clone();
        let mut excess = EffectProcessorFeedbackBatch::new();
        excess.push(receipt(owner, 99)).expect("first child receipt");
        excess.push(receipt(owner, 100)).expect("second child receipt");
        assert_eq!(
            target.append(excess),
            Err(EffectProcessorFeedbackError::Capacity)
        );
        assert_eq!(target, before, "no partial append");
        let mut final_slot = EffectProcessorFeedbackBatch::new();
        final_slot.push(receipt(owner, 32)).expect("one receipt");
        target.append(final_slot).expect("complete capacity");
        assert_eq!(target.len(), EFFECT_PROCESSOR_FEEDBACK_SLOTS);
        assert_eq!(
            target.iter().last().expect("last receipt").feedback.snapshot.invocation(),
            32
        );
        assert_eq!(
            target.retained_bytes(),
            EffectProcessorFeedbackBatch::storage_bytes()
        );
        assert_eq!(target.into_iter().count(), EFFECT_PROCESSOR_FEEDBACK_SLOTS);
    }
}

/// An actual executing native work observation, without retention or pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProcessorLiveProgress {
    /// Parent-issued actual instance namespace.
    pub owner: EffectProcessorOwnerId,
    /// Actual execution origin, never an authoring authority.
    pub origin: EffectProcessorExecutionOrigin,
    /// Native auxiliary owner, stable inside this actual parent namespace.
    pub native_owner: u64,
    /// Exact accepted processor request sequence.
    pub invocation: u64,
    /// Strictly increasing progress observation serial in that request.
    pub revision: u64,
    /// Validated bounded current progress.
    pub progress: EffectProcessorProgress,
}

/// Compiler/bank-bound executing observation. Only the actual residency bank
/// binds these occurrence/provider facts; a plugin cannot invent them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProcessorProgressReceipt {
    /// Exact prepared occurrence, extent and input format.
    pub preparation: crate::EffectFloatProcessorPreparation,
    /// Immutable provider revision used by that actual invocation.
    pub provider_revision: u64,
    /// Executing facts, with no Retained/Evicted or frame-publication claim.
    pub progress: EffectProcessorLiveProgress,
}

/// Bounded observational consumer on the owning execution thread.
/// Implementations may coalesce/drop progress under backpressure. They cannot
/// change author state, cancel a different work or perform native processing.
pub trait EffectProcessorObserver: std::fmt::Debug + Send + Sync {
    /// Complete declared logical capacity of this shared observational owner.
    /// Execution charges it once per current route, separately from processors.
    fn maximum_storage_bytes(&self) -> usize;
    /// Observe borrowed bounded metadata without retaining native pointers.
    fn observe(&self, receipt: &EffectProcessorProgressReceipt);

    /// Optional actual-GUI service on this same bounded execution observation.
    /// Absence makes a native Question return Failed; it cannot stop legal DSP.
    fn question_service(&self) -> Option<&dyn crate::EffectProcessorQuestionService>;
}

/// Current route's observational sink and exact scheduler deadline.
/// This carries no plan or instance and never outlives its task binding.
#[derive(Debug, Clone, Default)]
pub struct EffectProcessorObservation {
    /// Optional explicitly bounded product observer.
    pub sink: Option<std::sync::Arc<dyn EffectProcessorObserver>>,
    /// Exact absolute scheduler deadline, when the route has one.
    pub deadline: Option<std::time::Instant>,
}
impl EffectProcessorObservation {
    /// Conservative route-owned logical observation capacity.
    pub fn maximum_storage_bytes(&self) -> usize {
        self.sink.as_ref().map_or(0, |sink| sink.maximum_storage_bytes())
    }
}

impl EffectProcessorProgressReceipt {
    /// Bounded logical receipt/text capacity; no pixels or native pointer.
    pub const fn maximum_retained_bytes() -> usize {
        std::mem::size_of::<Self>()
            + EFFECT_PROCESSOR_NOTICE_TEXT_BYTES
            + EFFECT_PROCESSOR_NOTICE_ID_BYTES
    }
}
