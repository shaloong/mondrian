//! Exclusive parent-Session note ledger, owned by its retained Hosted instance.
//!
//! One exclusive parent owns this value. Its origin is copied from the existing
//! retained native operation/binding, never trusted from a callback payload.
//! Root must reserve every occurrence's whole window before dispatch; this
//! helper neither stages windows nor runs SDK calls, sorts events or rolls back
//! DSP. Commit methods belong at admitted delivery start, and Ends are applied
//! only after that actual native call returns. Initial use is cold score-origin
//! entry followed by sequential blocks; Holding calls none of these methods.

use crate::{
    AudioNativeProcessorAddress, AudioNoteCompletionPolicy, AudioNoteEvent, AudioNoteEventKind,
    NativeAudioObjectObservation,
};
use mondrian_core::{AudioNoteId, AudioProcessorBindingId};

const NATIVE_ID_LIMIT: u64 = i32::MAX as u64 + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LedgerOrigin {
    pub(crate) address: AudioNativeProcessorAddress,
    pub(crate) binding: AudioProcessorBindingId,
    pub(crate) object: NativeAudioObjectObservation,
}

/// Already admitted actual input-port metadata, not an authored key lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputPortBinding {
    pub(crate) stable_id: u32,
    pub(crate) index: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NoteOn {
    pub(crate) author_id: AudioNoteId,
    pub(crate) port: InputPortBinding,
    pub(crate) channel: i16,
    pub(crate) key: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeNoteTuple {
    pub(crate) port_index: i16,
    pub(crate) channel: i16,
    pub(crate) key: i16,
    pub(crate) native_id: i32,
}

/// Validated scalar End address. SDK header/time/velocity checks stay outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NoteEnd {
    pub(crate) port_index: i16,
    pub(crate) channel: i16,
    pub(crate) key: i16,
    pub(crate) native_id: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotePhase {
    Pressed,
    ReleasePending,
    EndedUntilOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LedgerError {
    UnenteredOrigin,
    UnboundOrigin,
    StaleOrigin,
    InvalidOnAddress,
    InvalidEndAddress,
    WildcardEndIdUnsupported,
    RecordCapacity,
    VoiceCapacity,
    NativeIdExhausted,
    DuplicateAuthorNote,
    UnknownAuthorNote,
    OffPortMismatch,
    OffAlreadyDelivered,
    StorageExtentOverflow,
    StorageAllocation,
    DifferentProducer,
    DifferentCursorOrOccurrence,
    UnchangedEpoch,
    WrongCompletionPolicy,
    OffTupleMismatch,
}

/// Facts only; capacity credit has already been applied in this sole ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndObservation {
    EndedBeforeOff,
    RetiredAfterOff,
    Duplicate,
    StaleId,
    UnknownId,
    StaleOrigin,
    AddressMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct LedgerUsage {
    pub(crate) retained_records: usize,
    pub(crate) active_voices: usize,
    /// Exclusive next ID; NATIVE_ID_LIMIT is the exhausted sentinel.
    pub(crate) next_native_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Record {
    origin: LedgerOrigin,
    author_id: AudioNoteId,
    port: InputPortBinding,
    tuple: NativeNoteTuple,
    phase: NotePhase,
}

/// Fixed storage and both capacity counters in one non-Clone owner.
pub(crate) struct NoteLedger {
    completion: AudioNoteCompletionPolicy,
    origin: Option<LedgerOrigin>,
    // Public score cutoff of this confirmed cold epoch, independent of PDC.
    // Ordinary author publication/rebinding preserves it with the origin.
    public_cutoff: i64,
    records: Box<[Option<Record>]>,
    voice_capacity: usize,
    retained_records: usize,
    active_voices: usize,
    next_native_id: u64,
}

impl NoteLedger {
    pub(super) fn visit_record_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        visitor(crate::AudioProcessorFactoryStorageBorrow::complete(
            &self.records,
        )?)
    }
    pub(crate) const fn origin(&self) -> Option<LedgerOrigin> {
        self.origin
    }

    /// Existing native Bind has acknowledged the same producer. Plan/Session
    /// publication may replace addresses/binding while preserving DSP voices.
    pub(crate) fn preflight_continuing_rebind(
        &self,
        prior: LedgerOrigin,
        next: LedgerOrigin,
    ) -> Result<(), LedgerError> {
        self.require_origin(prior)?;
        if next.object != prior.object {
            return Err(LedgerError::DifferentProducer);
        }
        if next.address.occurrence != prior.address.occurrence {
            return Err(LedgerError::DifferentCursorOrOccurrence);
        }
        if next.address.epoch.is_none() || next.address.epoch != prior.address.epoch {
            return Err(LedgerError::UnenteredOrigin);
        }
        Ok(())
    }

    /// Only after the existing whole-tree validation/final publication guard.
    /// No SDK call, allocation, ID reset or voice/Off retirement happens here.
    pub(crate) fn commit_prevalidated_continuing_rebind(&mut self, next: LedgerOrigin) {
        self.origin = Some(next);
        for record in self.records.iter_mut().flatten() {
            record.origin = next;
        }
    }
    /// Payload plus this owner, for the existing Root resource charge to use.
    /// This is not an admission grant or a bound on allocator overhead.
    pub(crate) fn layout_bytes(record_capacity: usize) -> Result<usize, LedgerError> {
        record_capacity
            .checked_mul(std::mem::size_of::<Option<Record>>())
            .and_then(|payload| payload.checked_add(std::mem::size_of::<Self>()))
            .ok_or(LedgerError::StorageExtentOverflow)
    }

    /// Allocate unbound fixed storage during Session resource preparation,
    /// before actual successful EnterState/binding. Binding and processing do
    /// not allocate. This single owner must remain in sole Session custody.
    pub(crate) fn prepare(
        record_capacity: usize,
        voice_capacity: usize,
        completion: AudioNoteCompletionPolicy,
    ) -> Result<Self, LedgerError> {
        Self::layout_bytes(record_capacity)?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(record_capacity)
            .map_err(|_| LedgerError::StorageAllocation)?;
        records.resize(record_capacity, None);
        Ok(Self {
            completion,
            origin: None,
            public_cutoff: 0,
            records: records.into_boxed_slice(),
            voice_capacity,
            retained_records: 0,
            active_voices: 0,
            next_native_id: 0,
        })
    }

    #[cfg(test)]
    pub(crate) fn usage(&self) -> Result<LedgerUsage, LedgerError> {
        self.bound_origin()?;
        Ok(LedgerUsage {
            retained_records: self.retained_records,
            active_voices: self.active_voices,
            next_native_id: self.next_native_id,
        })
    }

    pub(crate) fn public_cutoff(&self) -> Result<i64, LedgerError> {
        self.bound_origin()?;
        Ok(self.public_cutoff)
    }

    /// Entry knows only the cold batch at S, never a future consumer's window.
    /// Old voices/records will be retired only by confirmed cold commit; their
    /// counts grant no capacity credit here, while the next ID stays monotonic.
    pub(crate) fn preflight_cold_on_count(&self, count: usize) -> Result<(), LedgerError> {
        if count > self.records.len() {
            return Err(LedgerError::RecordCapacity);
        }
        if self.completion == AudioNoteCompletionPolicy::NativeEnd && count > self.voice_capacity {
            return Err(LedgerError::VoiceCapacity);
        }
        if u64::try_from(count).map_err(|_| LedgerError::NativeIdExhausted)?
            > NATIVE_ID_LIMIT - self.next_native_id
        {
            return Err(LedgerError::NativeIdExhausted);
        }
        Ok(())
    }

    /// No mutation, allocation, future Off/End credit, or reservation token.
    /// Root calls this for every new On in the whole prospective window before
    /// any occurrence dispatch. Call order later supplies stable prepared order.
    pub(crate) fn preflight_ons(
        &self,
        origin: LedgerOrigin,
        ons: &[NoteOn],
    ) -> Result<(), LedgerError> {
        self.require_origin(origin)?;
        if ons.len() > self.records.len() - self.retained_records {
            return Err(LedgerError::RecordCapacity);
        }
        if self.completion == AudioNoteCompletionPolicy::NativeEnd
            && ons.len() > self.voice_capacity - self.active_voices
        {
            return Err(LedgerError::VoiceCapacity);
        }
        let count = u64::try_from(ons.len()).map_err(|_| LedgerError::NativeIdExhausted)?;
        if count > NATIVE_ID_LIMIT - self.next_native_id {
            return Err(LedgerError::NativeIdExhausted);
        }
        for (index, on) in ons.iter().enumerate() {
            if on.port.index < 0 || !(0..=15).contains(&on.channel) || !(0..=127).contains(&on.key)
            {
                return Err(LedgerError::InvalidOnAddress);
            }
            if self.find_author(on.author_id).is_some()
                || ons[..index].iter().any(|prior| prior.author_id == on.author_id)
            {
                return Err(LedgerError::DuplicateAuthorNote);
            }
        }
        Ok(())
    }

    /// Commit at admitted native On delivery start, never during preflight.
    /// A subsequent adapter/DSP failure poisons the existing epoch; no rollback
    /// or retry of this consumed prefix is offered by this helper.
    pub(crate) fn commit_on(
        &mut self,
        origin: LedgerOrigin,
        on: NoteOn,
    ) -> Result<NativeNoteTuple, LedgerError> {
        self.preflight_ons(origin, std::slice::from_ref(&on))?;
        let slot = self
            .records
            .iter()
            .position(Option::is_none)
            .ok_or(LedgerError::RecordCapacity)?;
        let native_id =
            i32::try_from(self.next_native_id).map_err(|_| LedgerError::NativeIdExhausted)?;
        let tuple = NativeNoteTuple {
            port_index: on.port.index,
            channel: on.channel,
            key: on.key,
            native_id,
        };
        self.records[slot] = Some(Record {
            origin,
            author_id: on.author_id,
            port: on.port,
            tuple,
            phase: NotePhase::Pressed,
        });
        self.retained_records += 1;
        if self.completion == AudioNoteCompletionPolicy::NativeEnd {
            self.active_voices += 1;
        }
        self.next_native_id += 1;
        Ok(tuple)
    }

    /// Original tuple only, including an early-ended pairing record.
    pub(crate) fn preflight_off(
        &self,
        origin: LedgerOrigin,
        author_id: AudioNoteId,
        port: InputPortBinding,
    ) -> Result<NativeNoteTuple, LedgerError> {
        Ok(self.off_record(origin, author_id, port)?.1.tuple)
    }

    /// Off does not free a live/releasing voice. Early End has already freed
    /// that voice, so delivery of its original Off now retires only its record.
    pub(crate) fn commit_off(
        &mut self,
        origin: LedgerOrigin,
        author_id: AudioNoteId,
        port: InputPortBinding,
    ) -> Result<NativeNoteTuple, LedgerError> {
        let (index, record) = self.off_record(origin, author_id, port)?;
        match record.phase {
            NotePhase::Pressed => {
                self.records[index].as_mut().expect("located record").phase =
                    NotePhase::ReleasePending
            }
            NotePhase::EndedUntilOff => {
                self.records[index] = None;
                self.retained_records -= 1;
            }
            NotePhase::ReleasePending => unreachable!("off_record rejects duplicate Off"),
        }
        Ok(record.tuple)
    }

    /// Apply copied End facts only after the authenticated native call returns.
    /// The event carries no authority to choose this origin. Its timestamp and
    /// velocity are deliberately absent and cannot affect parameter time state.
    pub(crate) fn observe_end_after_native_return(
        &mut self,
        origin: LedgerOrigin,
        end: NoteEnd,
    ) -> Result<EndObservation, LedgerError> {
        if self.completion != AudioNoteCompletionPolicy::NativeEnd {
            return Err(LedgerError::WrongCompletionPolicy);
        }
        if origin != self.bound_origin()? {
            return Ok(EndObservation::StaleOrigin);
        }
        if end.native_id == -1 {
            return Err(LedgerError::WildcardEndIdUnsupported);
        }
        if end.native_id < 0
            || end.port_index < -1
            || !(-1..=15).contains(&end.channel)
            || !(-1..=127).contains(&end.key)
        {
            return Err(LedgerError::InvalidEndAddress);
        }
        let Some(index) = self
            .records
            .iter()
            .position(|slot| slot.is_some_and(|record| record.tuple.native_id == end.native_id))
        else {
            return Ok(if (end.native_id as u64) < self.next_native_id {
                EndObservation::StaleId
            } else {
                EndObservation::UnknownId
            });
        };
        let record = self.records[index].expect("located record");
        if record.origin != origin {
            return Ok(EndObservation::StaleOrigin);
        }
        if (end.port_index != -1 && end.port_index != record.tuple.port_index)
            || (end.channel != -1 && end.channel != record.tuple.channel)
            || (end.key != -1 && end.key != record.tuple.key)
        {
            return Ok(EndObservation::AddressMismatch);
        }
        match record.phase {
            NotePhase::Pressed => {
                self.records[index].as_mut().expect("located record").phase =
                    NotePhase::EndedUntilOff;
                self.active_voices -= 1;
                Ok(EndObservation::EndedBeforeOff)
            }
            NotePhase::ReleasePending => {
                self.records[index] = None;
                self.active_voices -= 1;
                self.retained_records -= 1;
                Ok(EndObservation::RetiredAfterOff)
            }
            NotePhase::EndedUntilOff => Ok(EndObservation::Duplicate),
        }
    }

    pub(crate) const fn completion_policy(&self) -> AudioNoteCompletionPolicy {
        self.completion
    }

    pub(crate) const fn retained_record_count(&self) -> usize {
        self.retained_records
    }

    /// Pure whole-window proof for SuccessfulOffBlock. Peak includes every On
    /// before granting any Off credit from that same native call.
    pub(crate) fn preflight_off_block_peak(
        &self,
        origin: LedgerOrigin,
        peak_records: usize,
        total_ons: usize,
    ) -> Result<(), LedgerError> {
        self.require_origin(origin)?;
        if self.completion != AudioNoteCompletionPolicy::SuccessfulOffBlock {
            return Err(LedgerError::WrongCompletionPolicy);
        }
        if peak_records > self.records.len() {
            return Err(LedgerError::RecordCapacity);
        }
        let count = u64::try_from(total_ons).map_err(|_| LedgerError::NativeIdExhausted)?;
        if count > NATIVE_ID_LIMIT - self.next_native_id {
            return Err(LedgerError::NativeIdExhausted);
        }
        Ok(())
    }

    /// Address/identity proof without applying the current capacity to a later
    /// simulated block. The sole ledger remains unchanged.
    pub(crate) fn preflight_on_identity(
        &self,
        origin: LedgerOrigin,
        on: NoteOn,
    ) -> Result<(), LedgerError> {
        self.require_origin(origin)?;
        if on.port.index < 0 || !(0..=15).contains(&on.channel) || !(0..=127).contains(&on.key) {
            return Err(LedgerError::InvalidOnAddress);
        }
        if self.find_author(on.author_id).is_some() {
            return Err(LedgerError::DuplicateAuthorNote);
        }
        Ok(())
    }

    /// Retire only Offs in this actual successful operation. Validate the whole
    /// batch before changing a slot; failed/Deferred operations never call this.
    /// This grants pairing storage, not evidence that a native voice has ended.
    pub(crate) fn complete_successful_off_block(
        &mut self,
        origin: LedgerOrigin,
        inputs: &[AudioNoteEvent],
    ) -> Result<(), LedgerError> {
        self.require_origin(origin)?;
        if self.completion != AudioNoteCompletionPolicy::SuccessfulOffBlock {
            return Err(LedgerError::WrongCompletionPolicy);
        }
        for (index, event) in inputs.iter().enumerate() {
            if event.kind != AudioNoteEventKind::Off {
                continue;
            }
            if inputs[..index].iter().any(|prior| {
                prior.kind == AudioNoteEventKind::Off
                    && prior.native_note_id == event.native_note_id
            }) {
                return Err(LedgerError::OffAlreadyDelivered);
            }
            let record = self
                .records
                .iter()
                .flatten()
                .find(|record| record.tuple.native_id == event.native_note_id)
                .ok_or(LedgerError::UnknownAuthorNote)?;
            if record.origin != origin {
                return Err(LedgerError::StaleOrigin);
            }
            if record.phase != NotePhase::ReleasePending
                || record.tuple.port_index != event.port_index as i16
                || record.tuple.channel != i16::from(event.channel)
                || record.tuple.key != i16::from(event.key)
            {
                return Err(LedgerError::OffTupleMismatch);
            }
        }
        for event in inputs.iter().filter(|event| event.kind == AudioNoteEventKind::Off) {
            let index = self
                .records
                .iter()
                .position(|slot| {
                    slot.is_some_and(|record| record.tuple.native_id == event.native_note_id)
                })
                .expect("all actual Off tuples were validated before retirement");
            self.records[index] = None;
            self.retained_records -= 1;
        }
        Ok(())
    }

    /// Bind the already prepared storage after the existing successful actual
    /// cold-origin EnterState/binding acknowledgement. On its first binding,
    /// caller must prove this is a genuinely fresh native namespace. On later
    /// entry, caller must prove the old producer can no longer emit Ends. A
    /// failed or pending operation must not call this method; it grants no
    /// native proof itself. This method does not allocate. Continuing producer
    /// IDs stay monotonic across epochs. A different actual object is refused,
    /// never interpreted as permission to reset this owner's namespace.
    pub(crate) fn commit_confirmed_cold_entry(
        &mut self,
        next: LedgerOrigin,
        public_cutoff: i64,
    ) -> Result<(), LedgerError> {
        if next.address.epoch.is_none() {
            return Err(LedgerError::UnenteredOrigin);
        }
        let Some(current) = self.origin else {
            self.origin = Some(next);
            self.public_cutoff = public_cutoff;
            return Ok(());
        };
        if next.object != current.object {
            return Err(LedgerError::DifferentProducer);
        }
        if next.address.root != current.address.root
            || next.address.local != current.address.local
            || next.address.occurrence != current.address.occurrence
        {
            return Err(LedgerError::DifferentCursorOrOccurrence);
        }
        if next.address.epoch == current.address.epoch {
            return Err(LedgerError::UnchangedEpoch);
        }
        self.records.fill(None);
        self.retained_records = 0;
        self.active_voices = 0;
        self.origin = Some(next);
        self.public_cutoff = public_cutoff;
        Ok(())
    }

    /// Only the enclosing entry transaction can construct this consumed proof.
    /// Ordinary Bind/continuing rebind must retain the DifferentProducer gate.
    pub(super) fn commit_confirmed_replacement(
        &mut self,
        proof: super::notes::ConfirmedNoteProducerReplacement,
        public_cutoff: i64,
    ) -> Result<(), LedgerError> {
        let (prior, next) = proof.into_origins();
        self.require_origin(prior)?;
        if next.address.root != prior.address.root
            || next.address.local != prior.address.local
            || next.address.occurrence != prior.address.occurrence
        {
            return Err(LedgerError::DifferentCursorOrOccurrence);
        }
        if next.address.epoch.is_none() {
            return Err(LedgerError::UnenteredOrigin);
        }
        if next.address.epoch == prior.address.epoch {
            return Err(LedgerError::UnchangedEpoch);
        }
        self.records.fill(None);
        self.retained_records = 0;
        self.active_voices = 0;
        self.origin = Some(next);
        self.public_cutoff = public_cutoff;
        // Never recycle IDs, even though the old producer was retired.
        Ok(())
    }

    fn bound_origin(&self) -> Result<LedgerOrigin, LedgerError> {
        self.origin.ok_or(LedgerError::UnboundOrigin)
    }

    fn require_origin(&self, origin: LedgerOrigin) -> Result<(), LedgerError> {
        if origin == self.bound_origin()? {
            Ok(())
        } else {
            Err(LedgerError::StaleOrigin)
        }
    }

    fn find_author(&self, author_id: AudioNoteId) -> Option<usize> {
        self.records
            .iter()
            .position(|slot| slot.is_some_and(|record| record.author_id == author_id))
    }

    fn off_record(
        &self,
        origin: LedgerOrigin,
        author_id: AudioNoteId,
        port: InputPortBinding,
    ) -> Result<(usize, Record), LedgerError> {
        self.require_origin(origin)?;
        let index = self.find_author(author_id).ok_or(LedgerError::UnknownAuthorNote)?;
        let record = self.records[index].expect("located record");
        if record.origin != origin {
            return Err(LedgerError::StaleOrigin);
        }
        if record.port != port {
            return Err(LedgerError::OffPortMismatch);
        }
        if record.phase == NotePhase::ReleasePending {
            return Err(LedgerError::OffAlreadyDelivered);
        }
        Ok((index, record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioContinuityEpoch, AudioProcessorInsertionPoint, AudioProcessorOccurrence,
        AudioProcessorOccurrenceOwner, AudioSessionCursorIdentity,
    };
    use mondrian_core::{
        AudioPreparedPlanId, AudioProcessorInstanceId, AudioRenderSessionId, TrackId,
    };
    use std::num::{NonZeroU32, NonZeroU64};

    fn origin() -> LedgerOrigin {
        let cursor = AudioSessionCursorIdentity {
            prepared_plan_id: AudioPreparedPlanId::new(),
            session_id: AudioRenderSessionId::new(),
        };
        LedgerOrigin {
            address: AudioNativeProcessorAddress {
                root: cursor,
                local: cursor,
                occurrence: AudioProcessorOccurrence {
                    instance_id: AudioProcessorInstanceId::new(),
                    owner: AudioProcessorOccurrenceOwner::Track(TrackId::new()),
                    insertion: AudioProcessorInsertionPoint::PreFader,
                },
                epoch: Some(AudioContinuityEpoch::new(1)),
            },
            binding: AudioProcessorBindingId::new(),
            object: NativeAudioObjectObservation::new(
                NonZeroU32::new(7).unwrap(),
                NonZeroU64::new(11).unwrap(),
            ),
        }
    }

    fn on() -> NoteOn {
        NoteOn {
            author_id: AudioNoteId::new(),
            port: InputPortBinding { stable_id: 27, index: 2 },
            channel: 4,
            key: 60,
        }
    }

    fn bound_ledger(origin: LedgerOrigin, records: usize, voices: usize) -> NoteLedger {
        let mut ledger =
            NoteLedger::prepare(records, voices, AudioNoteCompletionPolicy::NativeEnd).unwrap();
        // Pure fixture simulates the parent's already confirmed native entry;
        // it neither performs nor proves that operation.
        ledger.commit_confirmed_cold_entry(origin, 0).unwrap();
        ledger
    }

    fn end(tuple: NativeNoteTuple) -> NoteEnd {
        NoteEnd {
            port_index: tuple.port_index,
            channel: tuple.channel,
            key: tuple.key,
            native_id: tuple.native_id,
        }
    }

    fn phase(ledger: &NoteLedger, author: AudioNoteId) -> Option<NotePhase> {
        ledger.find_author(author).map(|index| ledger.records[index].unwrap().phase)
    }

    #[test]
    fn off_keeps_voice_until_end_and_retirement_never_reuses_id() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 1, 1);
        let first = on();
        let tuple = ledger.commit_on(origin, first).unwrap();
        assert_eq!(
            tuple,
            NativeNoteTuple { port_index: 2, channel: 4, key: 60, native_id: 0 }
        );
        assert_eq!(
            ledger.commit_off(origin, first.author_id, first.port),
            Ok(tuple)
        );
        assert_eq!(
            phase(&ledger, first.author_id),
            Some(NotePhase::ReleasePending)
        );
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 1,
                active_voices: 1,
                next_native_id: 1
            }
        );
        assert_eq!(
            ledger.commit_off(origin, first.author_id, first.port),
            Err(LedgerError::OffAlreadyDelivered)
        );
        assert_eq!(
            ledger.observe_end_after_native_return(origin, end(tuple)),
            Ok(EndObservation::RetiredAfterOff)
        );
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 0,
                active_voices: 0,
                next_native_id: 1
            }
        );
        assert_eq!(
            ledger.observe_end_after_native_return(origin, end(tuple)),
            Ok(EndObservation::StaleId)
        );
        let second = ledger.commit_on(origin, on()).unwrap();
        assert_eq!(second.native_id, 1);
        let before = ledger.usage().unwrap();
        assert_eq!(
            ledger.observe_end_after_native_return(origin, end(tuple)),
            Ok(EndObservation::StaleId)
        );
        assert_eq!(ledger.usage().unwrap(), before);
    }

    #[test]
    fn early_end_keeps_original_off_tuple_and_duplicate_gives_no_credit() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 1, 1);
        let note = on();
        let tuple = ledger.commit_on(origin, note).unwrap();
        assert_eq!(
            ledger.observe_end_after_native_return(origin, end(tuple)),
            Ok(EndObservation::EndedBeforeOff)
        );
        assert_eq!(
            phase(&ledger, note.author_id),
            Some(NotePhase::EndedUntilOff)
        );
        let before = LedgerUsage {
            retained_records: 1,
            active_voices: 0,
            next_native_id: 1,
        };
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            ledger.observe_end_after_native_return(origin, end(tuple)),
            Ok(EndObservation::Duplicate)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            ledger.preflight_off(origin, note.author_id, note.port),
            Ok(tuple)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            ledger.commit_off(origin, note.author_id, note.port),
            Ok(tuple)
        );
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 0,
                active_voices: 0,
                next_native_id: 1
            }
        );
        assert_eq!(
            ledger.preflight_off(origin, note.author_id, note.port),
            Err(LedgerError::UnknownAuthorNote)
        );
    }

    #[test]
    fn early_ends_cannot_evade_record_capacity() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 2, 1);
        for _ in 0..2 {
            let tuple = ledger.commit_on(origin, on()).unwrap();
            assert_eq!(
                ledger.observe_end_after_native_return(origin, end(tuple)),
                Ok(EndObservation::EndedBeforeOff)
            );
        }
        let before = LedgerUsage {
            retained_records: 2,
            active_voices: 0,
            next_native_id: 2,
        };
        assert_eq!(
            ledger.preflight_ons(origin, &[on()]),
            Err(LedgerError::RecordCapacity)
        );
        assert_eq!(
            ledger.commit_on(origin, on()),
            Err(LedgerError::RecordCapacity)
        );
        assert_eq!(ledger.usage().unwrap(), before);
    }

    #[test]
    fn preflight_is_read_only_and_reserves_all_ons_without_future_credit() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 3, 2);
        let first = on();
        ledger.commit_on(origin, first).unwrap();
        ledger.commit_off(origin, first.author_id, first.port).unwrap();
        let before = ledger.usage().unwrap();
        let second = on();
        assert_eq!(ledger.preflight_ons(origin, &[second]), Ok(()));
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            ledger.preflight_ons(origin, &[second, on()]),
            Err(LedgerError::VoiceCapacity)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(ledger.commit_on(origin, second).unwrap().native_id, 1);
        assert_eq!(
            ledger.preflight_ons(origin, &[on()]),
            Err(LedgerError::VoiceCapacity)
        );
    }

    #[test]
    fn zero_and_exact_capacities_are_distinct_and_checked() {
        let origin = origin();
        let mut no_records = bound_ledger(origin, 0, 1);
        assert_eq!(no_records.preflight_ons(origin, &[]), Ok(()));
        assert_eq!(
            no_records.commit_on(origin, on()),
            Err(LedgerError::RecordCapacity)
        );
        let mut no_voices = bound_ledger(origin, 1, 0);
        assert_eq!(
            no_voices.commit_on(origin, on()),
            Err(LedgerError::VoiceCapacity)
        );
        let ledger = bound_ledger(origin, 2, 2);
        assert_eq!(ledger.preflight_ons(origin, &[on(), on()]), Ok(()));
        assert_eq!(
            ledger.preflight_ons(origin, &[on(), on(), on()]),
            Err(LedgerError::RecordCapacity)
        );
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 0,
                active_voices: 0,
                next_native_id: 0
            }
        );
        assert_eq!(
            NoteLedger::layout_bytes(usize::MAX),
            Err(LedgerError::StorageExtentOverflow)
        );
    }

    #[test]
    fn maximum_id_is_accepted_once_then_exhaustion_consumes_nothing() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 2, 2);
        ledger.next_native_id = i32::MAX as u64;
        let before = ledger.usage().unwrap();
        assert_eq!(
            ledger.preflight_ons(origin, &[on(), on()]),
            Err(LedgerError::NativeIdExhausted)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        let note = on();
        let tuple = ledger.commit_on(origin, note).unwrap();
        assert_eq!(tuple.native_id, i32::MAX);
        ledger.commit_off(origin, note.author_id, note.port).unwrap();
        ledger.observe_end_after_native_return(origin, end(tuple)).unwrap();
        let before = LedgerUsage {
            retained_records: 0,
            active_voices: 0,
            next_native_id: NATIVE_ID_LIMIT,
        };
        assert_eq!(ledger.preflight_ons(origin, &[]), Ok(()));
        assert_eq!(
            ledger.commit_on(origin, on()),
            Err(LedgerError::NativeIdExhausted)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        let mut next = origin;
        next.address.epoch = Some(AudioContinuityEpoch::new(2));
        next.binding = AudioProcessorBindingId::new();
        ledger.commit_confirmed_cold_entry(next, 0).unwrap();
        assert_eq!(
            ledger.commit_on(next, on()),
            Err(LedgerError::NativeIdExhausted)
        );
        assert_eq!(ledger.usage().unwrap(), before);
    }

    #[test]
    fn concrete_id_accepts_each_pck_wildcard_combination() {
        for mask in 0..8 {
            let origin = origin();
            let mut ledger = bound_ledger(origin, 1, 1);
            let note = on();
            let tuple = ledger.commit_on(origin, note).unwrap();
            let mut observation = end(tuple);
            if mask & 1 != 0 {
                observation.port_index = -1;
            }
            if mask & 2 != 0 {
                observation.channel = -1;
            }
            if mask & 4 != 0 {
                observation.key = -1;
            }
            assert_eq!(
                ledger.observe_end_after_native_return(origin, observation),
                Ok(EndObservation::EndedBeforeOff),
                "wildcard mask {mask}"
            );
            assert_eq!(
                ledger.preflight_off(origin, note.author_id, note.port),
                Ok(tuple)
            );
            assert_eq!(
                ledger.usage().unwrap(),
                LedgerUsage {
                    retained_records: 1,
                    active_voices: 0,
                    next_native_id: 1
                }
            );
        }
    }

    #[test]
    fn invalid_wildcard_unknown_and_mismatched_end_addresses_grant_no_credit() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 1, 1);
        let tuple = ledger.commit_on(origin, on()).unwrap();
        let before = ledger.usage().unwrap();
        let cases = [
            (
                NoteEnd { native_id: -1, ..end(tuple) },
                Err(LedgerError::WildcardEndIdUnsupported),
            ),
            (
                NoteEnd { native_id: -2, ..end(tuple) },
                Err(LedgerError::InvalidEndAddress),
            ),
            (
                NoteEnd { port_index: -2, ..end(tuple) },
                Err(LedgerError::InvalidEndAddress),
            ),
            (
                NoteEnd { channel: 16, ..end(tuple) },
                Err(LedgerError::InvalidEndAddress),
            ),
            (
                NoteEnd { channel: -2, ..end(tuple) },
                Err(LedgerError::InvalidEndAddress),
            ),
            (
                NoteEnd { key: 128, ..end(tuple) },
                Err(LedgerError::InvalidEndAddress),
            ),
            (
                NoteEnd { key: -2, ..end(tuple) },
                Err(LedgerError::InvalidEndAddress),
            ),
            (
                NoteEnd { port_index: 3, ..end(tuple) },
                Ok(EndObservation::AddressMismatch),
            ),
            (
                NoteEnd { channel: 5, ..end(tuple) },
                Ok(EndObservation::AddressMismatch),
            ),
            (
                NoteEnd { key: 61, ..end(tuple) },
                Ok(EndObservation::AddressMismatch),
            ),
            (
                NoteEnd { native_id: 1, ..end(tuple) },
                Ok(EndObservation::UnknownId),
            ),
        ];
        for (observation, expected) in cases {
            assert_eq!(
                ledger.observe_end_after_native_return(origin, observation),
                expected
            );
            assert_eq!(ledger.usage().unwrap(), before);
        }
    }

    #[test]
    fn author_duplicates_invalid_ons_and_off_port_rebinding_do_not_consume() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 3, 3);
        let note = on();
        let before = ledger.usage().unwrap();
        assert_eq!(
            ledger.preflight_ons(origin, &[note, note]),
            Err(LedgerError::DuplicateAuthorNote)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            ledger.commit_on(origin, NoteOn { channel: -1, ..note }),
            Err(LedgerError::InvalidOnAddress)
        );
        assert_eq!(
            ledger.commit_on(origin, NoteOn { key: 128, ..note }),
            Err(LedgerError::InvalidOnAddress)
        );
        assert_eq!(
            ledger.commit_on(
                origin,
                NoteOn {
                    port: InputPortBinding { index: -1, ..note.port },
                    ..note
                }
            ),
            Err(LedgerError::InvalidOnAddress)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        let tuple = ledger.commit_on(origin, note).unwrap();
        let before = ledger.usage().unwrap();
        assert_eq!(
            ledger.commit_on(origin, note),
            Err(LedgerError::DuplicateAuthorNote)
        );
        assert_eq!(
            ledger.commit_off(
                origin,
                note.author_id,
                InputPortBinding { stable_id: 28, ..note.port }
            ),
            Err(LedgerError::OffPortMismatch)
        );
        assert_eq!(
            ledger.commit_off(
                origin,
                note.author_id,
                InputPortBinding { index: 3, ..note.port }
            ),
            Err(LedgerError::OffPortMismatch)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            ledger.preflight_off(origin, note.author_id, note.port),
            Ok(tuple)
        );
    }

    #[test]
    fn binding_object_epoch_session_and_occurrence_mismatches_are_stale() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 1, 1);
        let note = on();
        let tuple = ledger.commit_on(origin, note).unwrap();
        let before = ledger.usage().unwrap();
        let mut origins = [origin; 7];
        origins[0].binding = AudioProcessorBindingId::new();
        origins[1].object = NativeAudioObjectObservation::new(
            NonZeroU32::new(7).unwrap(),
            NonZeroU64::new(12).unwrap(),
        );
        origins[2].address.epoch = Some(AudioContinuityEpoch::new(2));
        origins[3].address.root.session_id = AudioRenderSessionId::new();
        origins[4].address.local.session_id = AudioRenderSessionId::new();
        origins[5].address.occurrence.instance_id = AudioProcessorInstanceId::new();
        origins[6].address.root.prepared_plan_id = AudioPreparedPlanId::new();
        for stale in origins {
            assert_eq!(
                ledger.preflight_ons(stale, &[]),
                Err(LedgerError::StaleOrigin)
            );
            assert_eq!(
                ledger.commit_off(stale, note.author_id, note.port),
                Err(LedgerError::StaleOrigin)
            );
            assert_eq!(
                ledger.observe_end_after_native_return(stale, end(tuple)),
                Ok(EndObservation::StaleOrigin)
            );
            assert_eq!(ledger.usage().unwrap(), before);
        }
    }

    #[test]
    fn confirmed_continuing_producer_entry_keeps_ids_and_rejected_entry_keeps_records() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 2, 2);
        let note = on();
        let tuple = ledger.commit_on(origin, note).unwrap();
        let before = ledger.usage().unwrap();
        let mut next = origin;
        next.binding = AudioProcessorBindingId::new();
        assert_eq!(
            ledger.commit_confirmed_cold_entry(next, 0),
            Err(LedgerError::UnchangedEpoch)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        next.address.epoch = Some(AudioContinuityEpoch::new(2));
        let mut replaced = next;
        replaced.object = NativeAudioObjectObservation::new(
            NonZeroU32::new(7).unwrap(),
            NonZeroU64::new(12).unwrap(),
        );
        assert_eq!(
            ledger.commit_confirmed_cold_entry(replaced, 0),
            Err(LedgerError::DifferentProducer)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        let mut different_session = next;
        different_session.address.local.session_id = AudioRenderSessionId::new();
        assert_eq!(
            ledger.commit_confirmed_cold_entry(different_session, 0),
            Err(LedgerError::DifferentCursorOrOccurrence)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        for root in [true, false] {
            let mut different_plan = next;
            if root {
                different_plan.address.root.prepared_plan_id = AudioPreparedPlanId::new();
            } else {
                different_plan.address.local.prepared_plan_id = AudioPreparedPlanId::new();
            }
            assert_eq!(
                ledger.commit_confirmed_cold_entry(different_plan, 0),
                Err(LedgerError::DifferentCursorOrOccurrence)
            );
            assert_eq!(ledger.usage().unwrap(), before);
            assert_eq!(ledger.origin, Some(origin));
            assert_eq!(
                ledger.preflight_off(origin, note.author_id, note.port),
                Ok(tuple)
            );
        }
        assert_eq!(
            ledger.preflight_off(origin, note.author_id, note.port),
            Ok(tuple)
        );
        ledger.commit_confirmed_cold_entry(next, 0).unwrap();
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 0,
                active_voices: 0,
                next_native_id: 1
            }
        );
        let new_tuple = ledger.commit_on(next, on()).unwrap();
        assert_eq!(new_tuple.native_id, 1);
        let before = ledger.usage().unwrap();
        assert_eq!(
            ledger.observe_end_after_native_return(origin, end(tuple)),
            Ok(EndObservation::StaleOrigin)
        );
        assert_eq!(
            ledger.observe_end_after_native_return(next, end(tuple)),
            Ok(EndObservation::StaleId)
        );
        assert_eq!(ledger.usage().unwrap(), before);
    }

    #[test]
    fn fresh_actual_object_namespace_does_not_accept_old_object_end_for_id_zero() {
        let old = origin();
        let mut old_ledger = bound_ledger(old, 1, 1);
        let old_tuple = old_ledger.commit_on(old, on()).unwrap();
        // Fixture assumption: caller has genuinely retired the old object and
        // custody; this helper cannot itself establish native retirement.
        drop(old_ledger);
        let mut fresh = old;
        fresh.binding = AudioProcessorBindingId::new();
        fresh.object = NativeAudioObjectObservation::new(
            NonZeroU32::new(7).unwrap(),
            NonZeroU64::new(12).unwrap(),
        );
        let mut ledger = bound_ledger(fresh, 1, 1);
        let tuple = ledger.commit_on(fresh, on()).unwrap();
        assert_eq!(old_tuple.native_id, 0);
        assert_eq!(tuple.native_id, 0);
        let before = ledger.usage().unwrap();
        assert_eq!(
            ledger.observe_end_after_native_return(old, end(old_tuple)),
            Ok(EndObservation::StaleOrigin)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            ledger.observe_end_after_native_return(fresh, end(tuple)),
            Ok(EndObservation::EndedBeforeOff)
        );
    }

    #[test]
    fn same_pitch_records_use_dispatch_order_and_concrete_id_does_not_end_neighbor() {
        let origin = origin();
        let mut ledger = bound_ledger(origin, 2, 2);
        let first = on();
        let second = on();
        assert_eq!(ledger.preflight_ons(origin, &[first, second]), Ok(()));
        let tuple0 = ledger.commit_on(origin, first).unwrap();
        let tuple1 = ledger.commit_on(origin, second).unwrap();
        assert_eq!(tuple0.native_id, 0);
        assert_eq!(tuple1.native_id, 1);
        let wildcard = NoteEnd { port_index: -1, channel: -1, key: -1, native_id: 0 };
        assert_eq!(
            ledger.observe_end_after_native_return(origin, wildcard),
            Ok(EndObservation::EndedBeforeOff)
        );
        assert_eq!(
            phase(&ledger, first.author_id),
            Some(NotePhase::EndedUntilOff)
        );
        assert_eq!(phase(&ledger, second.author_id), Some(NotePhase::Pressed));
        let before = LedgerUsage {
            retained_records: 2,
            active_voices: 1,
            next_native_id: 2,
        };
        assert_eq!(
            ledger.observe_end_after_native_return(origin, wildcard),
            Ok(EndObservation::Duplicate)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            ledger.commit_off(origin, first.author_id, first.port),
            Ok(tuple0)
        );
        assert_eq!(
            ledger.preflight_off(origin, second.author_id, second.port),
            Ok(tuple1)
        );
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 1,
                active_voices: 1,
                next_native_id: 2
            }
        );
    }

    #[test]
    fn missing_entry_refuses_without_accepting_epoch_or_clearing_pairing() {
        let origin = origin();
        let mut missing = origin;
        missing.address.epoch = None;
        let mut unbound = NoteLedger::prepare(1, 1, AudioNoteCompletionPolicy::NativeEnd).unwrap();
        assert!(matches!(
            unbound.commit_confirmed_cold_entry(missing, 0),
            Err(LedgerError::UnenteredOrigin)
        ));
        let mut ledger = bound_ledger(origin, 1, 1);
        let note = on();
        let tuple = ledger.commit_on(origin, note).unwrap();
        let before = ledger.usage().unwrap();
        assert_eq!(
            ledger.commit_confirmed_cold_entry(missing, 0),
            Err(LedgerError::UnenteredOrigin)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(ledger.origin, Some(origin));
        assert_eq!(
            ledger.preflight_off(origin, note.author_id, note.port),
            Ok(tuple)
        );
    }

    #[test]
    fn successful_off_block_is_atomic_and_never_counts_native_voices() {
        let origin = origin();
        let mut ledger =
            NoteLedger::prepare(2, 0, AudioNoteCompletionPolicy::SuccessfulOffBlock).unwrap();
        ledger.commit_confirmed_cold_entry(origin, 0).unwrap();
        let first = on();
        let second = on();
        let a = ledger.commit_on(origin, first).unwrap();
        let b = ledger.commit_on(origin, second).unwrap();
        assert_eq!(ledger.usage().unwrap().active_voices, 0);
        ledger.commit_off(origin, first.author_id, first.port).unwrap();
        ledger.commit_off(origin, second.author_id, second.port).unwrap();
        let off = |tuple: NativeNoteTuple| AudioNoteEvent {
            sample_offset: 0,
            kind: AudioNoteEventKind::Off,
            port_index: tuple.port_index as u16,
            channel: tuple.channel as u8,
            key: tuple.key as u8,
            native_note_id: tuple.native_id,
            velocity: 0.5,
        };
        let before = ledger.usage().unwrap();
        let invalid = AudioNoteEvent { key: 61, ..off(b) };
        assert_eq!(
            ledger.complete_successful_off_block(origin, &[off(a), invalid]),
            Err(LedgerError::OffTupleMismatch)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(
            phase(&ledger, first.author_id),
            Some(NotePhase::ReleasePending)
        );
        assert_eq!(
            phase(&ledger, second.author_id),
            Some(NotePhase::ReleasePending)
        );
        assert_eq!(
            ledger.complete_successful_off_block(origin, &[off(a), off(a)]),
            Err(LedgerError::OffAlreadyDelivered)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        let mut stale = origin;
        stale.binding = AudioProcessorBindingId::new();
        assert_eq!(
            ledger.complete_successful_off_block(stale, &[off(a), off(b)]),
            Err(LedgerError::StaleOrigin)
        );
        assert_eq!(
            ledger.observe_end_after_native_return(origin, end(a)),
            Err(LedgerError::WrongCompletionPolicy)
        );
        assert_eq!(ledger.usage().unwrap(), before);
        ledger.complete_successful_off_block(origin, &[off(a), off(b)]).unwrap();
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 0,
                active_voices: 0,
                next_native_id: 2
            }
        );
        assert_eq!(ledger.commit_on(origin, on()).unwrap().native_id, 2);
    }

    #[test]
    fn successful_off_block_reuses_records_without_reusing_ids_or_same_block_credit() {
        let origin = origin();
        let mut ledger =
            NoteLedger::prepare(1, 0, AudioNoteCompletionPolicy::SuccessfulOffBlock).unwrap();
        ledger.commit_confirmed_cold_entry(origin, 0).unwrap();
        let storage = ledger.records.as_ptr();
        for id in 0..1100 {
            let note = on();
            let tuple = ledger.commit_on(origin, note).unwrap();
            assert_eq!(tuple.native_id, id);
            ledger.commit_off(origin, note.author_id, note.port).unwrap();
            assert_eq!(
                ledger.commit_on(origin, on()),
                Err(LedgerError::RecordCapacity)
            );
            ledger
                .complete_successful_off_block(
                    origin,
                    &[AudioNoteEvent {
                        sample_offset: 0,
                        kind: AudioNoteEventKind::Off,
                        port_index: tuple.port_index as u16,
                        channel: tuple.channel as u8,
                        key: tuple.key as u8,
                        native_note_id: tuple.native_id,
                        velocity: 0.5,
                    }],
                )
                .unwrap();
            assert_eq!(ledger.usage().unwrap().active_voices, 0);
            assert_eq!(ledger.records.as_ptr(), storage);
        }
        ledger.next_native_id = NATIVE_ID_LIMIT - 1;
        assert_eq!(ledger.preflight_off_block_peak(origin, 1, 1), Ok(()));
        assert_eq!(
            ledger.preflight_off_block_peak(origin, 1, 2),
            Err(LedgerError::NativeIdExhausted)
        );
        let note = on();
        assert_eq!(ledger.commit_on(origin, note).unwrap().native_id, i32::MAX);
        assert_eq!(
            ledger.preflight_off_block_peak(origin, 1, 1),
            Err(LedgerError::NativeIdExhausted)
        );
    }

    #[test]
    fn unbound_storage_refuses_reads_and_dispatch_then_binds_and_reenters_in_place() {
        let origin = origin();
        let note = on();
        let mut ledger = NoteLedger::prepare(2, 2, AudioNoteCompletionPolicy::NativeEnd).unwrap();
        let storage = ledger.records.as_ptr();
        assert_eq!(ledger.records.len(), 2);
        assert_eq!(ledger.origin, None);
        assert_eq!(ledger.usage(), Err(LedgerError::UnboundOrigin));
        assert_eq!(
            ledger.preflight_ons(origin, &[]),
            Err(LedgerError::UnboundOrigin)
        );
        assert_eq!(
            ledger.preflight_ons(origin, &[note]),
            Err(LedgerError::UnboundOrigin)
        );
        assert_eq!(
            ledger.commit_on(origin, note),
            Err(LedgerError::UnboundOrigin)
        );
        assert_eq!(
            ledger.preflight_off(origin, note.author_id, note.port),
            Err(LedgerError::UnboundOrigin)
        );
        assert_eq!(
            ledger.commit_off(origin, note.author_id, note.port),
            Err(LedgerError::UnboundOrigin)
        );
        let end0 = NoteEnd { port_index: 2, channel: 4, key: 60, native_id: 0 };
        assert_eq!(
            ledger.observe_end_after_native_return(origin, end0),
            Err(LedgerError::UnboundOrigin)
        );
        let mut missing = origin;
        missing.address.epoch = None;
        assert_eq!(
            ledger.commit_confirmed_cold_entry(missing, 0),
            Err(LedgerError::UnenteredOrigin)
        );
        assert_eq!(ledger.origin, None);
        assert_eq!(ledger.retained_records, 0);
        assert_eq!(ledger.active_voices, 0);
        assert_eq!(ledger.next_native_id, 0);
        assert!(ledger.records.iter().all(Option::is_none));
        assert_eq!(ledger.records.as_ptr(), storage);

        // The parent would call only after its real successful cold-origin
        // entry/bind. These assertions check helper state and storage identity,
        // not a measured allocator trace or native acknowledgement.
        ledger.commit_confirmed_cold_entry(origin, 0).unwrap();
        assert_eq!(ledger.origin, Some(origin));
        assert_eq!(ledger.records.as_ptr(), storage);
        assert_eq!(ledger.records.len(), 2);
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 0,
                active_voices: 0,
                next_native_id: 0
            }
        );
        let tuple = ledger.commit_on(origin, note).unwrap();
        assert_eq!(tuple.native_id, 0);
        let mut next = origin;
        next.binding = AudioProcessorBindingId::new();
        next.address.epoch = Some(AudioContinuityEpoch::new(2));
        ledger.commit_confirmed_cold_entry(next, 0).unwrap();
        assert_eq!(ledger.records.as_ptr(), storage);
        assert_eq!(ledger.records.len(), 2);
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 0,
                active_voices: 0,
                next_native_id: 1
            }
        );
        assert_eq!(ledger.commit_on(next, on()).unwrap().native_id, 1);
        let before = ledger.usage().unwrap();
        let mut replaced = next;
        replaced.object = NativeAudioObjectObservation::new(
            NonZeroU32::new(7).unwrap(),
            NonZeroU64::new(12).unwrap(),
        );
        replaced.address.epoch = Some(AudioContinuityEpoch::new(3));
        assert_eq!(
            ledger.commit_confirmed_cold_entry(replaced, 0),
            Err(LedgerError::DifferentProducer)
        );
        assert_eq!(ledger.origin, Some(next));
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(ledger.records.as_ptr(), storage);
    }
}
// Scratch-only candidate: append as a child module of processor_host/note_ledger.rs.
// Pure state/API oracles only; no native acknowledgement or allocation trace claim.
#[cfg(test)]
mod continuing_rebind_contracts {
    use super::*;
    use crate::{
        AudioContinuityEpoch, AudioProcessorInsertionPoint, AudioProcessorOccurrence,
        AudioProcessorOccurrenceOwner, AudioSessionCursorIdentity,
    };
    use mondrian_core::{
        AudioPreparedPlanId, AudioProcessorInstanceId, AudioRenderSessionId, TrackId,
    };
    use std::num::{NonZeroU32, NonZeroU64};

    fn origin() -> LedgerOrigin {
        let cursor = AudioSessionCursorIdentity {
            prepared_plan_id: AudioPreparedPlanId::new(),
            session_id: AudioRenderSessionId::new(),
        };
        LedgerOrigin {
            address: AudioNativeProcessorAddress {
                root: cursor,
                local: cursor,
                occurrence: AudioProcessorOccurrence {
                    instance_id: AudioProcessorInstanceId::new(),
                    owner: AudioProcessorOccurrenceOwner::Track(TrackId::new()),
                    insertion: AudioProcessorInsertionPoint::PreFader,
                },
                epoch: Some(AudioContinuityEpoch::new(1)),
            },
            binding: AudioProcessorBindingId::new(),
            object: NativeAudioObjectObservation::new(
                NonZeroU32::new(7).unwrap(),
                NonZeroU64::new(11).unwrap(),
            ),
        }
    }
    fn next(prior: LedgerOrigin) -> LedgerOrigin {
        let mut next = prior;
        next.address.root = AudioSessionCursorIdentity {
            prepared_plan_id: AudioPreparedPlanId::new(),
            session_id: AudioRenderSessionId::new(),
        };
        next.address.local = AudioSessionCursorIdentity {
            prepared_plan_id: AudioPreparedPlanId::new(),
            session_id: AudioRenderSessionId::new(),
        };
        next.binding = AudioProcessorBindingId::new();
        next
    }
    fn on() -> NoteOn {
        NoteOn {
            author_id: AudioNoteId::new(),
            port: InputPortBinding { stable_id: 27, index: 2 },
            channel: 4,
            key: 60,
        }
    }
    fn end(tuple: NativeNoteTuple) -> NoteEnd {
        NoteEnd {
            native_id: tuple.native_id,
            port_index: tuple.port_index,
            channel: tuple.channel,
            key: tuple.key,
        }
    }
    fn record(ledger: &NoteLedger, author: AudioNoteId) -> Record {
        ledger.records[ledger.find_author(author).unwrap()].unwrap()
    }

    #[test]
    fn continuing_rebind_preserves_three_live_phases_original_tuples_and_storage() {
        let prior = origin();
        let next = next(prior);
        let mut ledger = NoteLedger::prepare(4, 3, AudioNoteCompletionPolicy::NativeEnd).unwrap();
        // Fixture assumption only: the parent completed its genuine cold entry.
        ledger.commit_confirmed_cold_entry(prior, 0).unwrap();
        let pressed = on();
        let releasing = on();
        let early = on();
        let p = ledger.commit_on(prior, pressed).unwrap();
        let r = ledger.commit_on(prior, releasing).unwrap();
        let e = ledger.commit_on(prior, early).unwrap();
        ledger.commit_off(prior, releasing.author_id, releasing.port).unwrap();
        ledger.observe_end_after_native_return(prior, end(e)).unwrap();
        let usage = ledger.usage().unwrap();
        let storage = ledger.records.as_ptr();
        assert_eq!(
            usage,
            LedgerUsage {
                retained_records: 3,
                active_voices: 2,
                next_native_id: 3
            }
        );
        assert_eq!(ledger.preflight_continuing_rebind(prior, next), Ok(()));
        assert_eq!(ledger.origin(), Some(prior));
        assert_eq!(ledger.usage().unwrap(), usage);
        // Fixture assumption only: the existing parent Bind/publication guard passed.
        ledger.commit_prevalidated_continuing_rebind(next);
        assert_eq!(ledger.origin(), Some(next));
        assert_eq!(ledger.usage().unwrap(), usage);
        assert_eq!(ledger.records.as_ptr(), storage);
        assert_eq!(ledger.records.len(), 4);
        for (note, tuple, phase) in [
            (pressed, p, NotePhase::Pressed),
            (releasing, r, NotePhase::ReleasePending),
            (early, e, NotePhase::EndedUntilOff),
        ] {
            let value = record(&ledger, note.author_id);
            assert_eq!(value.origin, next);
            assert_eq!(value.port, note.port);
            assert_eq!(value.author_id, note.author_id);
            assert_eq!(value.tuple, tuple);
            assert_eq!(value.phase, phase);
            assert_eq!(
                ledger.observe_end_after_native_return(prior, end(tuple)),
                Ok(EndObservation::StaleOrigin)
            );
            assert_eq!(ledger.usage().unwrap(), usage);
        }
        assert_eq!(
            ledger.preflight_off(next, pressed.author_id, pressed.port),
            Ok(p)
        );
        assert_eq!(
            ledger.preflight_off(next, releasing.author_id, releasing.port),
            Err(LedgerError::OffAlreadyDelivered)
        );
        assert_eq!(
            ledger.preflight_off(next, early.author_id, early.port),
            Ok(e)
        );
        assert_eq!(
            ledger.preflight_continuing_rebind(prior, next),
            Err(LedgerError::StaleOrigin)
        );
        assert_eq!(ledger.usage().unwrap(), usage); // an Ack retry must not use stale prior migration
        assert_eq!(
            ledger.observe_end_after_native_return(next, end(r)),
            Ok(EndObservation::RetiredAfterOff)
        );
        assert_eq!(ledger.commit_off(next, early.author_id, early.port), Ok(e));
        assert_eq!(
            ledger.observe_end_after_native_return(next, end(p)),
            Ok(EndObservation::EndedBeforeOff)
        );
        assert_eq!(
            ledger.commit_off(next, pressed.author_id, pressed.port),
            Ok(p)
        );
        assert_eq!(
            ledger.usage().unwrap(),
            LedgerUsage {
                retained_records: 0,
                active_voices: 0,
                next_native_id: 3
            }
        );
        assert_eq!(ledger.commit_on(next, on()).unwrap().native_id, 3);
        assert_eq!(ledger.records.as_ptr(), storage);
    }

    #[test]
    fn continuing_rebind_refusals_and_id_exhaustion_do_not_reset_namespace() {
        let prior = origin();
        let next = next(prior);
        let mut ledger = NoteLedger::prepare(1, 1, AudioNoteCompletionPolicy::NativeEnd).unwrap();
        ledger.commit_confirmed_cold_entry(prior, 0).unwrap();
        // Arithmetic boundary fixture; not a claim that all preceding IDs were executed.
        ledger.next_native_id = i32::MAX as u64;
        let note = on();
        let tuple = ledger.commit_on(prior, note).unwrap();
        ledger.commit_off(prior, note.author_id, note.port).unwrap();
        ledger.observe_end_after_native_return(prior, end(tuple)).unwrap();
        let usage = ledger.usage().unwrap();
        let storage = ledger.records.as_ptr();
        let mut wrong_object = next;
        wrong_object.object = NativeAudioObjectObservation::new(
            NonZeroU32::new(7).unwrap(),
            NonZeroU64::new(12).unwrap(),
        );
        let mut wrong_epoch = next;
        wrong_epoch.address.epoch = Some(AudioContinuityEpoch::new(2));
        let mut missing_epoch = next;
        missing_epoch.address.epoch = None;
        let mut wrong_occurrence = next;
        wrong_occurrence.address.occurrence.instance_id = AudioProcessorInstanceId::new();
        for invalid in [wrong_object, wrong_epoch, missing_epoch, wrong_occurrence] {
            assert!(ledger.preflight_continuing_rebind(prior, invalid).is_err());
            assert_eq!(ledger.origin(), Some(prior));
            assert_eq!(ledger.usage().unwrap(), usage);
            assert_eq!(ledger.records.as_ptr(), storage);
        }
        ledger.preflight_continuing_rebind(prior, next).unwrap();
        ledger.commit_prevalidated_continuing_rebind(next);
        assert_eq!(ledger.usage().unwrap(), usage);
        assert_eq!(usage.next_native_id, NATIVE_ID_LIMIT);
        assert_eq!(ledger.preflight_ons(next, &[]), Ok(()));
        assert_eq!(
            ledger.commit_on(next, on()),
            Err(LedgerError::NativeIdExhausted)
        );
        assert_eq!(ledger.usage().unwrap(), usage);
        assert_eq!(ledger.records.as_ptr(), storage);
    }
    #[test]
    fn cold_capacity_uses_empty_voices_but_preserves_ids_and_only_confirmed_cutoff() {
        let first = origin();
        let mut ledger = bound_ledger(first, 2, 1);
        ledger.commit_on(first, on()).unwrap();
        let before = ledger.usage().unwrap();
        assert_eq!(
            ledger.preflight_cold_on_count(3),
            Err(LedgerError::RecordCapacity)
        );
        assert_eq!(
            ledger.preflight_cold_on_count(2),
            Err(LedgerError::VoiceCapacity)
        );
        assert_eq!(
            ledger.preflight_cold_on_count(1),
            Ok(()),
            "old voice is not a new cold voice"
        );
        assert_eq!(ledger.usage().unwrap(), before);
        assert_eq!(ledger.public_cutoff().unwrap(), 0);
        ledger.next_native_id = NATIVE_ID_LIMIT;
        assert_eq!(
            ledger.preflight_cold_on_count(1),
            Err(LedgerError::NativeIdExhausted)
        );
        assert_eq!(ledger.public_cutoff().unwrap(), 0);
        ledger.next_native_id = before.next_native_id;
        let next = LedgerOrigin {
            address: AudioNativeProcessorAddress {
                epoch: Some(AudioContinuityEpoch::new(2)),
                ..first.address
            },
            binding: AudioProcessorBindingId::new(),
            ..first
        };
        ledger.commit_confirmed_cold_entry(next, -5).unwrap();
        assert_eq!(ledger.public_cutoff().unwrap(), -5);
        assert_eq!(
            ledger.usage().unwrap().next_native_id,
            before.next_native_id
        );
        let rebound = LedgerOrigin { binding: AudioProcessorBindingId::new(), ..next };
        ledger.preflight_continuing_rebind(next, rebound).unwrap();
        ledger.commit_prevalidated_continuing_rebind(rebound);
        assert_eq!(
            ledger.public_cutoff().unwrap(),
            -5,
            "ordinary author rebind preserves epoch cutoff"
        );
    }
}
