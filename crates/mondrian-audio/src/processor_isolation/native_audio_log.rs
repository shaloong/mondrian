//! Bounded native diagnostic observations, never playback/authoring authority.

use crate::native_audio_observations::{
    NativeAudioLogBatch, NativeAudioLogRecord, LOG_INCOMPLETE_UTF8_SUFFIX, LOG_INVALID_UTF8,
    LOG_NULL, LOG_TRUNCATED, LOG_UNKNOWN_SEVERITY, LOSS_CONTENTION, LOSS_FULL,
    LOSS_SEQUENCE_EXHAUSTED, NATIVE_AUDIO_LOG_MESSAGE_BYTES as HOST_LOG_MESSAGE_CAPACITY,
    NATIVE_AUDIO_LOG_RECORDS as NATIVE_LOG_CAPACITY,
};
use clack_extensions::log::HostLogMessage;
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

const MAX_PRODUCER_ATTEMPTS: usize = 16;
static NEXT_INGRESS_INCARNATION: AtomicU64 = AtomicU64::new(1);

// Private queue incarnation, never Core installation or author permission.
// Construction alone accesses the global counter; callback producers do not.
#[derive(Clone, Copy, PartialEq, Eq)]
struct IngressIncarnation(u64);

impl NativeAudioLogRecord {
    fn from_message(message: HostLogMessage<'_>) -> Self {
        let mut bytes = [0; HOST_LOG_MESSAGE_CAPACITY];
        let prefix = message.prefix();
        bytes[..prefix.len()].copy_from_slice(prefix);
        let mut flags = 0;
        if message.null_message() {
            flags |= LOG_NULL;
        }
        if !message.terminated() && !message.null_message() {
            flags |= LOG_TRUNCATED;
        }
        if message.severity().is_none() {
            flags |= LOG_UNKNOWN_SEVERITY;
        }
        let utf8_prefix_bytes = match message.text() {
            Ok(_) => prefix.len(),
            Err(error) => {
                if !message.terminated() && error.error_len().is_none() {
                    flags |= LOG_INCOMPLETE_UTF8_SUFFIX;
                } else {
                    flags |= LOG_INVALID_UTF8;
                }
                error.valid_up_to()
            }
        };
        // Prefix capacity is 512; these conversions are exact by construction.
        Self {
            sequence: 0,
            raw_severity: message.raw_severity(),
            length: prefix.len() as u16,
            utf8_prefix_bytes: utf8_prefix_bytes as u16,
            flags,
            bytes,
        }
    }
}

struct LogSlot {
    sequence: AtomicUsize,
    record: UnsafeCell<MaybeUninit<NativeAudioLogRecord>>,
}

// SAFETY: An enqueue ticket claims one slot exclusively. Its Release publish
// happens after initialization; the unique receiver reads only that sequence
// after Acquire, then Release retires it before any next producer can overwrite.
unsafe impl Sync for LogSlot {}

/// Fixed MPSC storage with a uniquely claimed, mutable single receiver.
///
/// Producers make at most 16 reservation attempts. Full/contention/exhaustion
/// records a sticky loss reason without blocking, growing or doing I/O.
pub(crate) struct NativeAudioLogIngress {
    incarnation: IngressIncarnation,
    slots: [LogSlot; NATIVE_LOG_CAPACITY],
    tail: AtomicUsize,
    receiver_claimed: AtomicBool,
    loss_reasons: AtomicU32,
}

impl NativeAudioLogIngress {
    pub(crate) fn new() -> Result<Self, crate::AudioProcessorHostError> {
        let incarnation = NEXT_INGRESS_INCARNATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| {
                crate::AudioProcessorHostError::InvalidContract(
                    "native log ingress incarnation exhausted".into(),
                )
            })?;
        Ok(Self {
            incarnation: IngressIncarnation(incarnation),
            slots: std::array::from_fn(|sequence| LogSlot {
                sequence: AtomicUsize::new(sequence),
                record: UnsafeCell::new(MaybeUninit::uninit()),
            }),
            tail: AtomicUsize::new(0),
            receiver_claimed: AtomicBool::new(false),
            loss_reasons: AtomicU32::new(0),
        })
    }

    pub(crate) fn claim_receiver(&self) -> Option<NativeAudioLogReceiver> {
        self.receiver_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(NativeAudioLogReceiver {
            incarnation: self.incarnation,
            head: 0,
            unique_receiver: PhantomData,
        })
    }

    pub(crate) fn observe(&self, message: HostLogMessage<'_>) {
        let record = NativeAudioLogRecord::from_message(message);
        self.push(record);
    }

    fn push(&self, mut record: NativeAudioLogRecord) {
        for _ in 0..MAX_PRODUCER_ATTEMPTS {
            let position = self.tail.load(Ordering::Relaxed);
            // Refuse before either the ticket or its retirement sequence could
            // wrap. No stale slot generation can be mistaken for a fresh one.
            if position >= usize::MAX - NATIVE_LOG_CAPACITY {
                self.loss_reasons.fetch_or(LOSS_SEQUENCE_EXHAUSTED, Ordering::Release);
                return;
            }
            let slot = &self.slots[position % NATIVE_LOG_CAPACITY];
            let sequence = slot.sequence.load(Ordering::Acquire);
            if sequence < position {
                self.loss_reasons.fetch_or(LOSS_FULL, Ordering::Release);
                return;
            }
            if sequence != position {
                continue;
            }
            if self
                .tail
                .compare_exchange_weak(position, position + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
            {
                continue;
            }
            record.sequence = position as u64;
            // SAFETY: Successful tail CAS reserves this slot exactly once. The
            // observed matching free sequence proves the receiver released it.
            unsafe {
                (*slot.record.get()).write(record);
            }
            slot.sequence.store(position + 1, Ordering::Release);
            return;
        }
        self.loss_reasons.fetch_or(LOSS_CONTENTION, Ordering::Release);
    }
}

/// One non-Clone receiver, owned by the sole child main/control worker.
///
/// It is neither Send nor Sync; &mut self admission prevents concurrent drains.
/// It never resets the claim or silently constructs another head on Drop.
/// A non-wrapping private ingress incarnation rejects stale receivers even if
/// a later SDK host allocation reuses the same address. The actual Clack
/// instance retains the inline ingress; poll borrows that live instance and
/// checks incarnation before touching slots. No raw pointer is dereferenced.
pub(crate) struct NativeAudioLogReceiver {
    incarnation: IngressIncarnation,
    head: usize,
    unique_receiver: PhantomData<Rc<()>>,
}

impl NativeAudioLogReceiver {
    /// Copy at most one full fixed queue into caller-owned observation storage.
    ///
    /// An earlier producer that has reserved but not published stops this
    /// bounded poll; it is never spun on, skipped or read before initialization.
    pub(crate) fn drain(
        &mut self,
        ingress: &NativeAudioLogIngress,
        records: &mut [NativeAudioLogRecord],
    ) -> Result<usize, crate::AudioProcessorHostError> {
        if self.incarnation != ingress.incarnation {
            return Err(crate::AudioProcessorHostError::InvalidContract(
                "native log receiver belongs to another host owner".into(),
            ));
        }
        let maximum = records.len().min(NATIVE_LOG_CAPACITY);
        let mut count = 0;
        while count < maximum {
            let slot = &ingress.slots[self.head % NATIVE_LOG_CAPACITY];
            if slot.sequence.load(Ordering::Acquire) != self.head + 1 {
                break;
            }
            // SAFETY: Only this uniquely claimed mutable receiver can reach
            // head. Acquire observes the initialized Copy record. Retirement
            // occurs only after the entire value has been copied out.
            records[count] = unsafe { (*slot.record.get()).assume_init_read() };
            slot.sequence.store(self.head + NATIVE_LOG_CAPACITY, Ordering::Release);
            self.head += 1;
            count += 1;
        }
        Ok(count)
    }

    pub(crate) fn observations(
        &mut self,
        ingress: &NativeAudioLogIngress,
        consume: crate::NativeAudioObservationConsumption,
    ) -> Result<NativeAudioLogBatch, crate::AudioProcessorHostError> {
        let mut batch = NativeAudioLogBatch::default();
        if consume.drains_logs() {
            batch.count = self.drain(ingress, &mut batch.records)?;
        } else if self.incarnation != ingress.incarnation {
            return Err(crate::AudioProcessorHostError::InvalidContract(
                "native log receiver belongs to another host owner".into(),
            ));
        }
        // The matching incarnation was checked on both consumption branches.
        // No separate unchecked loss getter can observe a different ingress.
        batch.loss_reasons = ingress.loss_reasons.load(Ordering::Acquire);
        Ok(batch)
    }
}

#[cfg(test)]
mod native_log_ingress_contracts {
    use super::*;

    fn record(value: u64) -> NativeAudioLogRecord {
        let mut record = NativeAudioLogRecord::EMPTY;
        record.length = 8;
        record.utf8_prefix_bytes = 0;
        record.flags = LOG_INVALID_UTF8;
        record.bytes[..8].copy_from_slice(&value.to_le_bytes());
        record
    }

    #[test]
    fn native_log_ingress_has_one_receiver_and_preserves_order_after_repeated_wraps() {
        let ingress = NativeAudioLogIngress::new().expect("nonwrapping incarnation");
        let mut receiver = ingress.claim_receiver().expect("sole receiver");
        assert!(ingress.claim_receiver().is_none());
        for round in 0..8 {
            for value in 0..NATIVE_LOG_CAPACITY {
                ingress.push(record((round * NATIVE_LOG_CAPACITY + value) as u64));
            }
            let kept = receiver
                .observations(&ingress, crate::NativeAudioObservationConsumption::Preserve)
                .expect("preserve");
            assert!(kept.records().is_empty() && !kept.incomplete());
            let batch = receiver
                .observations(&ingress, crate::NativeAudioObservationConsumption::Drain)
                .expect("drain");
            assert_eq!(batch.records().len(), NATIVE_LOG_CAPACITY);
            for (index, record) in batch.records().iter().enumerate() {
                let sequence = (round * NATIVE_LOG_CAPACITY + index) as u64;
                assert_eq!(record.sequence(), sequence);
                assert_eq!(record.bytes(), &sequence.to_le_bytes());
            }
        }
    }

    #[test]
    fn native_log_ingress_full_is_sticky_explicit_and_never_overwrites_published_records() {
        let ingress = NativeAudioLogIngress::new().expect("incarnation");
        let mut receiver = ingress.claim_receiver().expect("receiver");
        for value in 0..NATIVE_LOG_CAPACITY + 8 {
            ingress.push(record(value as u64));
        }
        let batch = receiver
            .observations(&ingress, crate::NativeAudioObservationConsumption::Drain)
            .expect("bounded drain");
        assert!(batch.capacity_loss());
        assert_eq!(batch.records().len(), NATIVE_LOG_CAPACITY);
        for (index, record) in batch.records().iter().enumerate() {
            assert_eq!(record.bytes(), &(index as u64).to_le_bytes());
        }
        ingress.push(record(1000));
        let next = receiver
            .observations(&ingress, crate::NativeAudioObservationConsumption::Drain)
            .expect("reusable after explicit loss");
        assert!(next.capacity_loss());
        assert_eq!(next.records().len(), 1);
        assert_eq!(next.records()[0].bytes(), &1000_u64.to_le_bytes());
    }

    #[test]
    fn native_log_ingress_unpublished_earliest_reservation_stops_bounded_poll() {
        let ingress = NativeAudioLogIngress::new().expect("incarnation");
        let mut receiver = ingress.claim_receiver().expect("receiver");
        // Independently model a preempted producer after its successful CAS.
        assert_eq!(
            ingress.tail.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed),
            Ok(0)
        );
        ingress.push(record(2));
        assert!(receiver
            .observations(&ingress, crate::NativeAudioObservationConsumption::Drain)
            .expect("bounded no-spin")
            .records()
            .is_empty());
        let mut first = record(1);
        first.sequence = 0;
        // SAFETY: This test owns the claimed first slot; publish only after write.
        unsafe {
            (*ingress.slots[0].record.get()).write(first);
        }
        ingress.slots[0].sequence.store(1, Ordering::Release);
        let batch = receiver
            .observations(&ingress, crate::NativeAudioObservationConsumption::Drain)
            .expect("same receiver");
        assert_eq!(batch.records().len(), 2);
        assert_eq!(batch.records()[0].bytes(), &1_u64.to_le_bytes());
        assert_eq!(batch.records()[1].bytes(), &2_u64.to_le_bytes());
    }

    #[test]
    fn native_log_ingress_stale_incarnation_cannot_drain_or_read_new_owner_loss() {
        let mut stale = {
            let old = Box::new(NativeAudioLogIngress::new().expect("old"));
            old.claim_receiver().expect("old receiver")
        };
        // The old allocation is gone. Address reuse cannot be the identity proof.
        let new = Box::new(NativeAudioLogIngress::new().expect("new"));
        new.push(record(17));
        for mode in [
            crate::NativeAudioObservationConsumption::Preserve,
            crate::NativeAudioObservationConsumption::Drain,
        ] {
            assert!(stale.observations(&new, mode).is_err());
        }
        let mut current = new.claim_receiver().expect("new receiver remains unclaimed");
        let batch = current
            .observations(&new, crate::NativeAudioObservationConsumption::Drain)
            .expect("current can consume");
        assert_eq!(batch.records().len(), 1);
        assert_eq!(batch.records()[0].bytes(), &17_u64.to_le_bytes());
    }

    #[test]
    fn native_log_ingress_sequence_exhaustion_refuses_before_wrap() {
        let ingress = NativeAudioLogIngress::new().expect("incarnation");
        let mut receiver = ingress.claim_receiver().expect("receiver");
        ingress.tail.store(usize::MAX - NATIVE_LOG_CAPACITY, Ordering::Relaxed);
        ingress.push(record(3));
        let batch = receiver
            .observations(&ingress, crate::NativeAudioObservationConsumption::Preserve)
            .expect("explicit exhausted fact");
        assert!(batch.sequence_exhausted());
        assert!(batch.records().is_empty());
        assert_eq!(
            ingress.tail.load(Ordering::Relaxed),
            usize::MAX - NATIVE_LOG_CAPACITY
        );
    }

    #[test]
    fn native_log_ingress_concurrent_producers_publish_only_whole_unique_records() {
        let ingress = NativeAudioLogIngress::new().expect("incarnation");
        let mut receiver = ingress.claim_receiver().expect("receiver");
        std::thread::scope(|scope| {
            for id in 0..8 {
                let ingress = &ingress;
                scope.spawn(move || {
                    for index in 0..64 {
                        ingress.push(record((id * 64 + index) as u64));
                    }
                });
            }
        });
        let batch = receiver
            .observations(&ingress, crate::NativeAudioObservationConsumption::Drain)
            .expect("joined writers");
        assert!(
            batch.incomplete(),
            "512 attempts exceed the fixed32 capacity"
        );
        assert!(batch.records().len() <= NATIVE_LOG_CAPACITY);
        let mut values = std::collections::BTreeSet::new();
        for (index, record) in batch.records().iter().enumerate() {
            assert_eq!(record.sequence(), index as u64);
            let value = u64::from_le_bytes(record.bytes().try_into().expect("whole8bytes"));
            assert!(
                value < 512 && values.insert(value),
                "no torn/duplicated publisher payload"
            );
        }
    }
}
