//! Bounded callback evidence for the same retained native CLAP object.
//! Source order and publication facts never authorize an author or graph commit.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

const ORDER_ATTEMPTS: usize = 16;
pub(super) const CLAP_PARAMETER_SOURCE_EVENTS: u32 = 4096;
pub(super) const CLAP_PARAMETER_CONTROLS: usize = 128;

/// Sticky source loss from a void callback, not a successful capability claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClapParameterSourceFailure {
    /// A main-only or !audio callback was made from the wrong actual role.
    WrongThread,
    /// A void control exceeded its already reserved inline slots.
    ControlCapacity,
    /// Source sequence or exclusive output publication epoch cannot advance.
    SequenceExhausted,
    /// A void callback could not reserve order within bounded contention work.
    OrderContention,
    /// A control retained flags outside the currently official known mask.
    UnknownFlags,
    /// RESCAN_ALL was requested before the actual deactivate boundary.
    ActiveRescanAll,
}

/// Snapshot of actual producer owners. It does not prove application acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapParameterSourceSnapshot {
    /// Actual latest reserved callback sequence, never reset by Ack.
    pub revision: u64,
    /// Accepted output callbacks not yet retired by the sole same-object Ack.
    pub pending_output: u32,
    /// Accepted recordable output callbacks awaiting the same delivery path.
    pub pending_author_output: u32,
    /// Main controls retained until their exact delivered prefix is accepted.
    pub pending_controls: u32,
    /// Real unmatched native starts, retained across prefix retirement.
    pub open_parameter_gestures: u32,
    /// Sticky incomplete-source bits indexed by ClapParameterSourceFailure.
    pub incomplete: u16,
    /// A native output batch or main control publication prevented a coherent snapshot.
    pub publishing_output: bool,
}

/// One shared sequence/publication owner for main controls and native output.
///
/// Construction occurs before native initialization. The production host and
/// its sole OutputEvents owner share this exact allocation. The Arc payload is
/// known storage; standard Arc metadata is a separately qualified frontier.
pub struct ClapParameterSource {
    revision: AtomicU64,
    output_epoch: AtomicU64,
    main_epoch: AtomicU64,
    output_claimed: AtomicBool,
    pending_slots: AtomicU32,
    pending_output: AtomicU32,
    pending_author_output: AtomicU32,
    pending_controls: AtomicU32,
    open_gestures: AtomicU32,
    failure: AtomicU16,
}

impl Default for ClapParameterSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ClapParameterSource {
    /// An empty native source; reuse of this owner across objects is prohibited.
    pub const fn new() -> Self {
        Self {
            revision: AtomicU64::new(0),
            output_epoch: AtomicU64::new(0),
            main_epoch: AtomicU64::new(0),
            output_claimed: AtomicBool::new(false),
            pending_slots: AtomicU32::new(0),
            pending_output: AtomicU32::new(0),
            pending_author_output: AtomicU32::new(0),
            pending_controls: AtomicU32::new(0),
            open_gestures: AtomicU32::new(0),
            failure: AtomicU16::new(0),
        }
    }

    /// Bounded scalar source observation. A batch publication may not be
    /// mistaken for ClosedEmpty during its reserve/initialize/publish interval.
    pub fn snapshot(&self) -> ClapParameterSourceSnapshot {
        let mut snapshot = ClapParameterSourceSnapshot {
            revision: 0,
            pending_output: 0,
            pending_author_output: 0,
            pending_controls: 0,
            open_parameter_gestures: 0,
            incomplete: 0,
            publishing_output: true,
        };
        for _ in 0..ORDER_ATTEMPTS {
            let before = self.output_epoch.load(Ordering::Acquire);
            let main_before = self.main_epoch.load(Ordering::Acquire);
            snapshot.revision = self.revision.load(Ordering::Acquire);
            snapshot.pending_output = self.pending_output.load(Ordering::Acquire);
            snapshot.pending_author_output = self.pending_author_output.load(Ordering::Acquire);
            snapshot.pending_controls = self.pending_controls.load(Ordering::Acquire);
            snapshot.open_parameter_gestures = self.open_gestures.load(Ordering::Acquire);
            snapshot.incomplete = self.failure.load(Ordering::Acquire);
            let after = self.output_epoch.load(Ordering::Acquire);
            let main_after = self.main_epoch.load(Ordering::Acquire);
            if before == after
                && before & 1 == 0
                && main_before == main_after
                && main_before & 1 == 0
            {
                snapshot.publishing_output = false;
                break;
            }
        }
        snapshot
    }

    pub(super) fn reserve_slot(&self) -> Result<ClapParameterSlot<'_>, ClapParameterSourceFailure> {
        let mut count = self.pending_slots.load(Ordering::Acquire);
        for _ in 0..ORDER_ATTEMPTS {
            if count >= CLAP_PARAMETER_SOURCE_EVENTS {
                return Err(ClapParameterSourceFailure::ControlCapacity);
            }
            match self.pending_slots.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(ClapParameterSlot {
                        source: self,
                        committed: false,
                        owner_thread: PhantomData,
                    })
                }
                Err(current) => count = current,
            }
        }
        Err(ClapParameterSourceFailure::OrderContention)
    }

    pub(super) fn retire_slots(&self, count: u32) {
        // Only exact retained local prefixes can reach this private method.
        self.pending_slots.fetch_sub(count, Ordering::AcqRel);
    }

    pub(super) fn next(&self) -> Result<u64, ClapParameterSourceFailure> {
        let mut previous = self.revision.load(Ordering::Acquire);
        for _ in 0..ORDER_ATTEMPTS {
            let Some(next) = previous.checked_add(1) else {
                self.refuse(ClapParameterSourceFailure::SequenceExhausted);
                return Err(ClapParameterSourceFailure::SequenceExhausted);
            };
            match self.revision.compare_exchange_weak(
                previous,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(next),
                Err(observed) => previous = observed,
            }
        }
        // A false-returning output can retry. Its caller retains the refusal
        // fact, but only a void callback turns contention into sticky source loss.
        Err(ClapParameterSourceFailure::OrderContention)
    }

    pub(super) fn refuse(&self, failure: ClapParameterSourceFailure) {
        self.failure.fetch_or(1 << failure as u8, Ordering::Release);
    }

    pub(super) fn claim_output_owner(&self) -> bool {
        // Exactly one retained output Vec can publish counts into this source.
        // Never release/reassign the claim within a native object lifetime.
        self.output_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn begin_output(
        &self,
    ) -> Result<ClapParameterPublication<'_>, ClapOutputSourceBusy> {
        self.begin_publication(&self.output_epoch)
    }

    pub(super) fn begin_main(&self) -> Result<ClapParameterPublication<'_>, ClapOutputSourceBusy> {
        self.begin_publication(&self.main_epoch)
    }

    fn begin_publication<'a>(
        &self,
        epoch: &'a AtomicU64,
    ) -> Result<ClapParameterPublication<'a>, ClapOutputSourceBusy> {
        let before = epoch.load(Ordering::Acquire);
        let Some(after) = before.checked_add(2) else {
            self.refuse(ClapParameterSourceFailure::SequenceExhausted);
            return Err(ClapOutputSourceBusy);
        };
        if before & 1 != 0
            || epoch
                .compare_exchange(before, before + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(ClapOutputSourceBusy);
        }
        Ok(ClapParameterPublication { epoch, after, owner_thread: PhantomData })
    }

    pub(super) fn publish_output_count(&self, all: u32, author: u32, open: u32) {
        // Only the one exclusive output owner writes these counts. Main reads
        // them through the publication epoch, not by borrowing DSP's Vec.
        self.pending_output.store(all, Ordering::Release);
        self.pending_author_output.store(author, Ordering::Release);
        self.open_gestures.store(open, Ordering::Release);
    }
}

/// No allocation is needed to refuse a concurrent/expired native source batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapOutputSourceBusy;
impl std::fmt::Display for ClapOutputSourceBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CLAP output source has no exclusive publication epoch")
    }
}
impl std::error::Error for ClapOutputSourceBusy {}

pub(super) struct ClapParameterSlot<'a> {
    source: &'a ClapParameterSource,
    committed: bool,
    owner_thread: PhantomData<Rc<()>>,
}
impl ClapParameterSlot<'_> {
    pub(super) fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for ClapParameterSlot<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.source.retire_slots(1);
        }
    }
}

pub(super) struct ClapParameterPublication<'a> {
    epoch: &'a AtomicU64,
    after: u64,
    owner_thread: PhantomData<Rc<()>>,
}
impl Drop for ClapParameterPublication<'_> {
    fn drop(&mut self) {
        self.epoch.store(self.after, Ordering::Release);
    }
}

/// Exact main-thread control and raw flag evidence from the native ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClapParameterControlKind {
    Rescan { flags: u32 },
    Clear { parameter: u32, flags: u32 },
    StateDirty,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClapParameterControl {
    pub(super) sequence: u64,
    pub(super) kind: ClapParameterControlKind,
}

/// Inline main control storage. Cell prevents concurrent safe main access.
/// It creates no second dispatcher, IPC reader, controller or author authority.
pub(super) struct ClapParameterControlSource<'a> {
    source: &'a ClapParameterSource,
    records: Cell<[Option<ClapParameterControl>; CLAP_PARAMETER_CONTROLS]>,
    count: Cell<usize>,
}
impl<'a> ClapParameterControlSource<'a> {
    pub(super) const fn new(source: &'a ClapParameterSource) -> Self {
        Self {
            source,
            records: Cell::new([None; CLAP_PARAMETER_CONTROLS]),
            count: Cell::new(0),
        }
    }
    pub(super) fn record(&self, kind: ClapParameterControlKind) -> bool {
        let source = self.source;
        let Ok(_publication) = source.begin_main() else {
            source.refuse(ClapParameterSourceFailure::OrderContention);
            return false;
        };
        let count = self.count.get();
        if count >= CLAP_PARAMETER_CONTROLS {
            source.refuse(ClapParameterSourceFailure::ControlCapacity);
            return false;
        }
        let slot = match source.reserve_slot() {
            Ok(slot) => slot,
            Err(failure) => {
                source.refuse(failure);
                return false;
            }
        };
        let sequence = match source.next() {
            Ok(sequence) => sequence,
            Err(failure) => {
                source.refuse(failure);
                return false;
            }
        };
        let mut records = self.records.get();
        records[count] = Some(ClapParameterControl { sequence, kind });
        self.records.set(records);
        self.count.set(count + 1);
        source.pending_controls.store((count + 1) as u32, Ordering::Release);
        slot.commit();
        true
    }
    pub(super) fn snapshot(
        &self,
    ) -> (
        [Option<ClapParameterControl>; CLAP_PARAMETER_CONTROLS],
        usize,
    ) {
        (self.records.get(), self.count.get())
    }
    /// Check an exact retained local prefix before either source owner retires.
    /// Zero callbacks for this lane is valid when the other lane owns the prefix.
    pub(super) fn prefix_matches(&self, through: Option<u64>, expected_count: usize) -> bool {
        if expected_count == 0 {
            return through.is_none();
        }
        let count = self.count.get();
        if expected_count > count {
            return false;
        }
        let records = self.records.get();
        records[expected_count - 1].map(|record| record.sequence) == through
            && records[..expected_count].iter().all(Option::is_some)
    }

    pub(super) fn belongs_to(&self, source: &ClapParameterSource) -> bool {
        std::ptr::eq(self.source, source)
    }

    /// Retire only after the same exclusive-main operation validated BOTH
    /// local prefixes. No foreign callback, allocation or fallible operation
    /// may intervene; this is evidence retirement, never a Session commit.
    pub(super) fn retire_prevalidated(&self, expected_count: usize) {
        let count = self.count.get();
        let mut records = self.records.get();
        records.copy_within(expected_count..count, 0);
        records[count - expected_count..count].fill(None);
        self.records.set(records);
        self.count.set(count - expected_count);
        self.source
            .pending_controls
            .store((count - expected_count) as u32, Ordering::Release);
        self.source.retire_slots(expected_count as u32);
    }

    #[cfg(test)]
    fn acknowledge(&self, through: u64, expected_count: usize) -> bool {
        let Ok(_publication) = self.source.begin_main() else {
            return false;
        };
        if expected_count == 0 || !self.prefix_matches(Some(through), expected_count) {
            return false;
        }
        self.retire_prevalidated(expected_count);
        true
    }
}

#[cfg(test)]
mod qualification {
    use super::*;
    #[test]
    fn interleaved_order_and_exact_ack_leave_newer_controls() {
        let order = ClapParameterSource::new();
        let source = ClapParameterControlSource::new(&order);
        assert!(source.record(ClapParameterControlKind::Rescan { flags: 1 }));
        assert_eq!(order.next(), Ok(2));
        assert!(source.record(ClapParameterControlKind::Clear { parameter: 17, flags: 2 }));
        assert!(!source.acknowledge(2, 2));
        assert!(source.record(ClapParameterControlKind::StateDirty));
        assert!(source.acknowledge(3, 2));
        let (records, count) = source.snapshot();
        assert_eq!(count, 1);
        assert_eq!(
            records[0],
            Some(ClapParameterControl {
                sequence: 4,
                kind: ClapParameterControlKind::StateDirty
            })
        );
        assert_eq!(order.snapshot().revision, 4);
        assert_eq!(order.snapshot().pending_controls, 1);
    }
    #[test]
    fn void_overflow_remains_incomplete_after_exact_ack() {
        let order = ClapParameterSource::new();
        let source = ClapParameterControlSource::new(&order);
        for parameter in 0..CLAP_PARAMETER_CONTROLS {
            assert!(source
                .record(ClapParameterControlKind::Clear { parameter: parameter as u32, flags: 1 }));
        }
        assert!(!source.record(ClapParameterControlKind::Rescan { flags: 8 }));
        assert!(source.acknowledge(CLAP_PARAMETER_CONTROLS as u64, CLAP_PARAMETER_CONTROLS));
        assert_eq!(order.snapshot().pending_controls, 0);
        assert_ne!(order.snapshot().incomplete, 0);
    }
    #[test]
    fn reserve_publication_cannot_report_closed_even_before_first_event() {
        let source = ClapParameterSource::new();
        let publication = source.begin_output().expect("exclusive source");
        assert!(source.snapshot().publishing_output);
        assert_eq!(source.next(), Ok(1));
        assert!(
            source.snapshot().publishing_output,
            "reserved but not initialized must be busy"
        );
        source.publish_output_count(1, 1, 0);
        drop(publication);
        let facts = source.snapshot();
        assert!(!facts.publishing_output);
        assert_eq!(facts.pending_output, 1);
    }
    #[test]
    fn revision_and_batch_epochs_never_wrap() {
        let source = ClapParameterSource::new();
        source.revision.store(u64::MAX, Ordering::Release);
        assert_eq!(
            source.next(),
            Err(ClapParameterSourceFailure::SequenceExhausted)
        );
        source.output_epoch.store(u64::MAX - 1, Ordering::Release);
        assert!(source.begin_output().is_err());
        assert_ne!(source.snapshot().incomplete, 0);
    }
}
