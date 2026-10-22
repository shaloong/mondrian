//! Prepared finite storage and the same Hosted instance's exclusive Note ledger.

use super::note_ledger::{
    InputPortBinding, LedgerError, LedgerOrigin, NoteEnd, NoteLedger, NoteOn,
};
use super::*;
use crate::schedule::{PreparedNotePhase, PreparedNoteScore};
use crate::{
    AudioNoteEndEvent, AudioNoteEvent, AudioNoteEventKind, AudioProcessorNoteInputContract,
};

pub(super) struct HostedNoteState {
    pub(super) ledger: NoteLedger,
    pub(super) inputs: Vec<AudioNoteEvent>,
    pub(super) ends: Vec<AudioNoteEndEvent>,
    ons: Vec<NoteOn>,
    input_limit: usize,
    pub(super) end_limit: usize,
    pub(super) ports: usize,
    pub(super) output_refused: bool,
    // One entry transaction only; included in storage_bytes through size_of<Self>().
    pending_entry: Option<PendingNoteEntry>,
}

// Not Clone/Copy: capture only from a currently Installed owner, before revoke.
pub(super) struct PendingNoteEntry {
    lease: crate::AudioNativeProcessorOwnerLease,
    object: crate::NativeAudioObjectObservation,
    entry: AudioStateEntry,
}

pub(super) struct ConfirmedNoteProducerReplacement {
    prior: LedgerOrigin,
    next: LedgerOrigin,
}

impl ConfirmedNoteProducerReplacement {
    pub(super) fn into_origins(self) -> (LedgerOrigin, LedgerOrigin) {
        (self.prior, self.next)
    }
}

impl PendingNoteEntry {
    fn capture(
        instance: &HostedProcessorInstance,
        entry: AudioStateEntry,
        context: &crate::native_runtime_owner::NativeStateEntryBindingContext,
        local: crate::AudioSessionCursorIdentity,
    ) -> Result<Self, AudioProcessorHostError> {
        let actual = origin(instance)?;
        let crate::native_runtime_owner::NativeHostedBinding::Installed { lease, .. } =
            instance.native_binding
        else {
            unreachable!("origin checked installed owner")
        };
        let captured = Self { lease, object: actual.object, entry };
        captured.validate(
            entry,
            &instance.notes.as_ref().expect("Note origin").ledger,
            context,
            local,
        )?;
        Ok(captured)
    }

    fn prior(&self) -> LedgerOrigin {
        LedgerOrigin {
            address: self.lease.address(),
            binding: self.lease.binding(),
            object: self.object,
        }
    }

    fn validate(
        &self,
        entry: AudioStateEntry,
        ledger: &NoteLedger,
        context: &crate::native_runtime_owner::NativeStateEntryBindingContext,
        local: crate::AudioSessionCursorIdentity,
    ) -> Result<(), AudioProcessorHostError> {
        if self.entry != entry
            || ledger.origin() != Some(self.prior())
            || self.lease.address().root != context.root
            || self.lease.address().local != local
            || self.lease.installed_root_serial() != context.claim.generation()
        {
            return Err(refusal(
                "Note entry transaction no longer matches its installed owner",
            ));
        }
        Ok(())
    }

    fn confirm(
        self,
        next: LedgerOrigin,
        next_lease: crate::AudioNativeProcessorOwnerLease,
    ) -> Result<ConfirmedNoteProducerReplacement, AudioProcessorHostError> {
        let prior = self.prior();
        if next.address.root != prior.address.root
            || next.address.local != prior.address.local
            || next.address.occurrence != prior.address.occurrence
            || next.address.epoch != Some(self.entry.epoch)
            || next.address.epoch == prior.address.epoch
            || next.binding == prior.binding
            || next_lease.installed_root_serial() != self.lease.installed_root_serial()
            || next.object.process_id() != prior.object.process_id()
            || next.object.object_serial() <= prior.object.object_serial()
        {
            return Err(refusal(
                "Note entry did not bind the confirmed fresh producer",
            ));
        }
        Ok(ConfirmedNoteProducerReplacement { prior, next })
    }
}

pub(super) fn refusal(reason: &'static str) -> AudioProcessorHostError {
    AudioProcessorHostError::NoteExecutionRefused(reason)
}

pub(super) fn ledger_refusal(error: LedgerError) -> AudioProcessorHostError {
    refusal(match error {
        LedgerError::RecordCapacity => "retained Note record capacity",
        LedgerError::VoiceCapacity => "live/releasing Note voice capacity",
        LedgerError::NativeIdExhausted => "native Note ID namespace exhausted",
        LedgerError::UnboundOrigin | LedgerError::UnenteredOrigin => {
            "Note origin is not entered/bound"
        }
        LedgerError::StaleOrigin => "stale Note operation origin",
        LedgerError::InvalidOnAddress | LedgerError::InvalidEndAddress => "invalid Note address",
        LedgerError::WildcardEndIdUnsupported => "wildcard End ID is unsupported",
        LedgerError::DuplicateAuthorNote => "duplicate authored Note On",
        LedgerError::UnknownAuthorNote => "Off has no retained original On",
        LedgerError::OffPortMismatch => "Off changed its original input port",
        LedgerError::OffAlreadyDelivered => "Off was already delivered",
        LedgerError::StorageExtentOverflow | LedgerError::StorageAllocation => {
            "Note storage unavailable"
        }
        LedgerError::DifferentProducer => "Note producer object changed",
        LedgerError::DifferentCursorOrOccurrence => "Note occurrence/cursor changed",
        LedgerError::UnchangedEpoch => "cold Note entry did not change epoch",
        LedgerError::WrongCompletionPolicy => "unexpected Note completion policy",
        LedgerError::OffTupleMismatch => "successful Off changed its original tuple",
    })
}

fn reserve<T>(count: usize) -> Result<Vec<T>, AudioProcessorHostError> {
    let mut storage = Vec::new();
    storage
        .try_reserve_exact(count)
        .map_err(|_| refusal("Note storage unavailable"))?;
    if storage.capacity() != count {
        return Err(refusal("Note allocation exceeds admitted capacity"));
    }
    Ok(storage)
}

impl HostedNoteState {
    pub(super) fn visit_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        use crate::AudioProcessorFactoryStorageBorrow as Storage;
        self.ledger.visit_record_storage(visitor)?;
        visitor(Storage::complete(&self.inputs)?)?;
        visitor(Storage::complete(&self.ends)?)?;
        visitor(Storage::complete(&self.ons)?)
    }
    pub(super) fn storage_bytes(
        contract: &AudioProcessorNoteInputContract,
    ) -> Result<usize, crate::AudioResourceFootprintError> {
        use crate::AudioResourceFootprintError::ExtentOverflow;
        if contract.ports.is_empty() {
            return Ok(0);
        }
        let limits = contract.limits;
        let records =
            NoteLedger::layout_bytes(limits.retained_records).map_err(|_| ExtentOverflow)?;
        let payload = std::alloc::Layout::array::<AudioNoteEvent>(limits.input_events)
            .map_err(|_| ExtentOverflow)?
            .size()
            .checked_add(
                std::alloc::Layout::array::<AudioNoteEndEvent>(limits.output_ends)
                    .map_err(|_| ExtentOverflow)?
                    .size(),
            )
            .and_then(|bytes| {
                bytes.checked_add(
                    std::alloc::Layout::array::<NoteOn>(limits.retained_records).ok()?.size(),
                )
            })
            .ok_or(ExtentOverflow)?;
        // Ledger header is included in HostedNoteState; count its payload once.
        records
            .checked_sub(size_of::<NoteLedger>())
            .and_then(|bytes| bytes.checked_add(size_of::<Self>()))
            .and_then(|bytes| bytes.checked_add(payload))
            .ok_or(ExtentOverflow)
    }

    pub(super) fn prepare(
        contract: &AudioProcessorNoteInputContract,
    ) -> Result<Option<Box<Self>>, AudioProcessorHostError> {
        if contract.ports.is_empty() {
            return Ok(None);
        }
        let limits = contract.limits;
        Ok(Some(Box::new(Self {
            ledger: NoteLedger::prepare(
                limits.retained_records,
                limits.live_voices,
                contract.completion,
            )
            .map_err(ledger_refusal)?,
            inputs: reserve(limits.input_events)?,
            ends: reserve(limits.output_ends)?,
            ons: reserve(limits.retained_records)?,
            input_limit: limits.input_events,
            end_limit: limits.output_ends,
            ports: contract.ports.len(),
            output_refused: false,
            pending_entry: None,
        })))
    }

    fn preflight(
        &mut self,
        score: &PreparedNoteScore,
        processor_slot: usize,
        origin: LedgerOrigin,
        start: i64,
        frames: usize,
        priming_frames: usize,
        block_frames: usize,
    ) -> Result<(), AudioProcessorHostError> {
        let end = start
            .checked_add(i64::try_from(frames).map_err(|_| refusal("Note window overflow"))?)
            .ok_or_else(|| refusal("Note window overflow"))?;
        let events = score.events();
        let begin = events.partition_point(|event| event.sample < start);
        let finish = events.partition_point(|event| event.sample < end);
        for event in &events[begin..finish] {
            let target = &score.targets()[event.target_slot];
            if target.processor_slot == processor_slot
                && (event.channel >= target.channel_count
                    || target.completion != self.ledger.completion_policy())
            {
                return Err(refusal(
                    "Note channel or completion differs from admitted contract",
                ));
            }
        }
        if self.ledger.completion_policy() == crate::AudioNoteCompletionPolicy::SuccessfulOffBlock {
            return self.preflight_off_blocks(
                score,
                processor_slot,
                origin,
                AudioRenderRequest { start_sample: start, frames },
                &events[begin..finish],
                priming_frames,
                block_frames,
            );
        }
        self.ons.clear();
        // No future Off/End credit. All new Ons in the entire prospective root
        // window must fit before any processor/source can consume its prefix.
        for event in &events[begin..finish] {
            let target = &score.targets()[event.target_slot];
            if target.processor_slot != processor_slot || event.phase != PreparedNotePhase::On {
                continue;
            }
            if self.ons.len() == self.ons.capacity() {
                return Err(refusal("retained Note record capacity"));
            }
            self.ons.push(NoteOn {
                author_id: event.note_id,
                port: InputPortBinding {
                    stable_id: target.native_port_id,
                    index: target.native_port_index as i16,
                },
                channel: i16::from(event.channel),
                key: i16::from(event.key),
            });
        }
        self.ledger.preflight_ons(origin, &self.ons).map_err(ledger_refusal)?;
        for event in &events[begin..finish] {
            let target = &score.targets()[event.target_slot];
            if target.processor_slot != processor_slot || event.phase != PreparedNotePhase::Off {
                continue;
            }
            let port = InputPortBinding {
                stable_id: target.native_port_id,
                index: target.native_port_index as i16,
            };
            // A same-window On/Off pair is proved from immutable positive-duration
            // score and staged On facts; no ID or record is committed here.
            if !self.ons.iter().any(|on| on.author_id == event.note_id && on.port == port) {
                self.ledger.preflight_off(origin, event.note_id, port).map_err(ledger_refusal)?;
            }
        }
        if block_frames == 0 || priming_frames > frames {
            return Err(refusal("invalid prospective Note block partition"));
        }
        // Priming has its own possibly partial final block. The public request
        // begins a new native call, rather than continuing that partial block.
        // Iterate only actual score events, never empty lookahead chunks.
        let main_call = priming_frames.div_ceil(block_frames);
        let mut prior_call = None;
        let mut count = 0usize;
        for event in &events[begin..finish] {
            if score.targets()[event.target_slot].processor_slot != processor_slot {
                continue;
            }
            let relative = usize::try_from(event.sample - start)
                .map_err(|_| refusal("Note window overflow"))?;
            let call = if relative < priming_frames {
                relative / block_frames
            } else {
                main_call
            };
            if prior_call != Some(call) {
                prior_call = Some(call);
                count = 0;
            }
            count += 1;
            if count > self.input_limit {
                return Err(refusal("Note input event capacity"));
            }
        }
        Ok(())
    }

    fn preflight_off_blocks(
        &self,
        score: &PreparedNoteScore,
        processor_slot: usize,
        origin: LedgerOrigin,
        request: AudioRenderRequest,
        events: &[crate::schedule::PreparedNoteEvent],
        priming_frames: usize,
        block_frames: usize,
    ) -> Result<(), AudioProcessorHostError> {
        if block_frames == 0
            || priming_frames > request.frames
            || request.frames - priming_frames > block_frames
        {
            return Err(refusal("invalid prospective Note block partition"));
        }
        let public_call = priming_frames.div_ceil(block_frames);
        let mut records = self.ledger.retained_record_count();
        let mut peak = records;
        let mut total_ons = 0usize;
        let mut call = None;
        let (mut ons, mut offs, mut inputs) = (0usize, 0usize, 0usize);
        // Cached IR is not an author-identity proof. Preserve the prior-event
        // checks without another index/ledger; worst-case work is quadratic in
        // this window's events. This foundation grants no RT/performance claim.
        for (index, event) in events.iter().enumerate() {
            let target = &score.targets()[event.target_slot];
            if target.processor_slot != processor_slot {
                continue;
            }
            let relative = usize::try_from(event.sample - request.start_sample)
                .map_err(|_| refusal("Note window overflow"))?;
            let next_call = if relative < priming_frames {
                relative / block_frames
            } else {
                public_call
            };
            if call != Some(next_call) {
                let pending =
                    records.checked_add(ons).ok_or_else(|| refusal("Note window overflow"))?;
                peak = peak.max(pending);
                records = pending
                    .checked_sub(offs)
                    .ok_or_else(|| refusal("Off has no retained original On"))?;
                call = Some(next_call);
                (ons, offs, inputs) = (0, 0, 0);
            }
            inputs += 1;
            if inputs > self.input_limit {
                return Err(refusal("Note input event capacity"));
            }
            let port = InputPortBinding {
                stable_id: target.native_port_id,
                index: target.native_port_index as i16,
            };
            let prior = &events[..index];
            match event.phase {
                PreparedNotePhase::On => {
                    self.ledger
                        .preflight_on_identity(
                            origin,
                            NoteOn {
                                author_id: event.note_id,
                                port,
                                channel: i16::from(event.channel),
                                key: i16::from(event.key),
                            },
                        )
                        .map_err(ledger_refusal)?;
                    if prior.iter().any(|old| {
                        old.note_id == event.note_id
                            && old.phase == PreparedNotePhase::On
                            && score.targets()[old.target_slot].processor_slot == processor_slot
                    }) {
                        return Err(refusal("duplicate authored Note On"));
                    }
                    ons = ons.checked_add(1).ok_or_else(|| refusal("Note window overflow"))?;
                    total_ons =
                        total_ons.checked_add(1).ok_or_else(|| refusal("Note window overflow"))?;
                }
                PreparedNotePhase::Off => {
                    if prior.iter().any(|old| {
                        old.note_id == event.note_id
                            && old.phase == PreparedNotePhase::Off
                            && score.targets()[old.target_slot].processor_slot == processor_slot
                    }) {
                        return Err(refusal("Off was already delivered"));
                    }
                    if let Some(on) = prior.iter().find(|old| {
                        old.note_id == event.note_id
                            && old.phase == PreparedNotePhase::On
                            && score.targets()[old.target_slot].processor_slot == processor_slot
                    }) {
                        let original = &score.targets()[on.target_slot];
                        if on.sample >= event.sample
                            || on.channel != event.channel
                            || on.key != event.key
                            || original.native_port_id != port.stable_id
                            || original.native_port_index as i16 != port.index
                        {
                            return Err(refusal("Off changed its original input port"));
                        }
                    } else {
                        let original = self
                            .ledger
                            .preflight_off(origin, event.note_id, port)
                            .map_err(ledger_refusal)?;
                        if original.channel != i16::from(event.channel)
                            || original.key != i16::from(event.key)
                        {
                            return Err(refusal("successful Off changed its original tuple"));
                        }
                    }
                    offs = offs.checked_add(1).ok_or_else(|| refusal("Note window overflow"))?;
                }
            }
        }
        peak = peak.max(records.checked_add(ons).ok_or_else(|| refusal("Note window overflow"))?);
        self.ledger
            .preflight_off_block_peak(origin, peak, total_ons)
            .map_err(ledger_refusal)
    }

    pub(super) fn prepare_admitted_block(
        &mut self,
        score: &PreparedNoteScore,
        processor_slot: usize,
        origin: LedgerOrigin,
        request: AudioRenderRequest,
    ) -> Result<(), AudioProcessorHostError> {
        self.inputs.clear();
        self.ends.clear();
        self.output_refused = false;
        let end = request
            .start_sample
            .checked_add(request.frames as i64)
            .ok_or_else(|| refusal("Note window overflow"))?;
        let events = score.events();
        let a = events.partition_point(|event| event.sample < request.start_sample);
        let b = events.partition_point(|event| event.sample < end);
        for event in &events[a..b] {
            let target = &score.targets()[event.target_slot];
            if target.processor_slot != processor_slot {
                continue;
            }
            if event.channel >= target.channel_count
                || target.completion != self.ledger.completion_policy()
            {
                return Err(refusal(
                    "Note channel or completion differs from admitted contract",
                ));
            }
            if self.inputs.len() == self.input_limit {
                return Err(refusal("Note input event capacity"));
            }
            let port = InputPortBinding {
                stable_id: target.native_port_id,
                index: target.native_port_index as i16,
            };
            let tuple = match event.phase {
                PreparedNotePhase::On => self.ledger.commit_on(
                    origin,
                    NoteOn {
                        author_id: event.note_id,
                        port,
                        channel: i16::from(event.channel),
                        key: i16::from(event.key),
                    },
                ),
                PreparedNotePhase::Off => self.ledger.commit_off(origin, event.note_id, port),
            }
            .map_err(ledger_refusal)?;
            self.inputs.push(AudioNoteEvent {
                sample_offset: (event.sample - request.start_sample) as u32,
                kind: match event.phase {
                    PreparedNotePhase::Off => AudioNoteEventKind::Off,
                    PreparedNotePhase::On => AudioNoteEventKind::On,
                },
                port_index: tuple.port_index as u16,
                channel: tuple.channel as u8,
                key: tuple.key as u8,
                native_note_id: tuple.native_id,
                velocity: event.velocity,
            });
        }
        Ok(())
    }

    pub(super) fn accept_native_return(
        &mut self,
        origin: LedgerOrigin,
    ) -> Result<(), AudioProcessorHostError> {
        if self.output_refused {
            return Err(refusal("Note End retention refused"));
        }
        if self.ledger.completion_policy() == crate::AudioNoteCompletionPolicy::SuccessfulOffBlock {
            if !self.ends.is_empty() {
                return Err(refusal(
                    "unexpected native End for successful Off completion",
                ));
            }
            return self
                .ledger
                .complete_successful_off_block(origin, &self.inputs)
                .map_err(ledger_refusal);
        }
        for end in &self.ends {
            if !end.valid(self.ports) {
                return Err(refusal("invalid retained Note End"));
            }
            let observation = self
                .ledger
                .observe_end_after_native_return(
                    origin,
                    NoteEnd {
                        native_id: end.native_note_id,
                        port_index: end.port_index,
                        channel: end.channel,
                        key: end.key,
                    },
                )
                .map_err(ledger_refusal)?;
            if matches!(
                observation,
                super::note_ledger::EndObservation::AddressMismatch
                    | super::note_ledger::EndObservation::StaleOrigin
                    | super::note_ledger::EndObservation::UnknownId
            ) {
                return Err(refusal(
                    "Note End does not identify a delivered original On",
                ));
            }
        }
        Ok(())
    }
}

fn validate_note_reentry_arm(
    instance: &HostedProcessorInstance,
    entry: AudioStateEntry,
    context: &crate::native_runtime_owner::NativeStateEntryBindingContext,
    local: crate::AudioSessionCursorIdentity,
) -> Result<(), AudioProcessorHostError> {
    let notes = instance.notes.as_ref().expect("selected Note reentry");
    if entry.start_sample != 0
        || !matches!(
            instance.occurrence.owner,
            AudioProcessorOccurrenceOwner::Track(_)
        )
        || !matches!(instance.continuity, HostedProcessorContinuity::Active { epoch, .. } if epoch != entry.epoch)
        || instance.native_editor_attachment.is_some()
        || instance.native_editor_detached.is_some()
    {
        return Err(refusal(
            "Note replacement requires a closed eager Track owner and fresh origin epoch",
        ));
    }
    if let Some(pending) = &notes.pending_entry {
        pending.validate(entry, &notes.ledger, context, local)?;
        if !matches!(
            instance.native_binding,
            crate::native_runtime_owner::NativeHostedBinding::Unbound
        ) {
            return Err(refusal(
                "Note entry transaction cannot be recaptured or rebound",
            ));
        }
    } else {
        PendingNoteEntry::capture(instance, entry, context, local)?;
    }
    Ok(())
}

pub(super) fn clear_pending_entry(instance: &mut HostedProcessorInstance) {
    if let Some(notes) = &mut instance.notes {
        notes.pending_entry = None;
    }
}

pub(super) fn origin(
    instance: &HostedProcessorInstance,
) -> Result<LedgerOrigin, AudioProcessorHostError> {
    if let crate::native_runtime_owner::NativeHostedBinding::Installed { lease, object } =
        instance.native_binding
        && lease.address().occurrence == instance.occurrence
        && matches!(instance.continuity, HostedProcessorContinuity::Active { epoch, .. } if lease.address().epoch == Some(epoch))
    {
        return Ok(LedgerOrigin {
            address: lease.address(),
            binding: lease.binding(),
            object,
        });
    }
    Err(refusal(
        "Note execution needs an entered installed native owner",
    ))
}

impl PreparedProcessorHost {
    pub(crate) fn note_storage_bytes(
        schedule: &PreparedAudioSchedule,
    ) -> Result<usize, crate::AudioResourceFootprintError> {
        if schedule.notes.is_empty() {
            return Ok(0);
        }
        let slots =
            std::alloc::Layout::array::<Option<Box<HostedNoteState>>>(schedule.processors.len())
                .map_err(|_| crate::AudioResourceFootprintError::ExtentOverflow)?
                .size();
        schedule
            .processors
            .iter()
            .enumerate()
            .try_fold(slots, |bytes, (slot, processor)| {
                if !schedule.notes.targets().iter().any(|target| target.processor_slot == slot) {
                    return Ok(bytes);
                }
                bytes
                    .checked_add(HostedNoteState::storage_bytes(
                        processor.factory.note_inputs(),
                    )?)
                    .ok_or(crate::AudioResourceFootprintError::ExtentOverflow)
            })
    }

    /// Read only the actual selected occurrence and existing Hosted/ledger
    /// state. Completion policy and raw event-bus presence grant no entry.
    pub(crate) fn validate_vst3_note_entry(
        &self,
        program: &crate::CompiledAudioProgram,
        entry: AudioStateEntry,
    ) -> Result<(), AudioExecutionError> {
        let mut deferred_refusal = None;
        for instance in &self.instances {
            let Some(notes) = &instance.notes else {
                continue;
            };
            let processor = crate::schedule::processor(program, instance.occurrence)
                .map_err(|_| AudioExecutionError::InvalidPreparedSchedule)?;
            if !matches!(
                &processor.definition,
                mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
            ) {
                continue;
            }
            // A failed first entry can still have no origin; it is poisoned,
            // never mistaken for a fresh producer or recoverable repeat.
            if matches!(instance.continuity, HostedProcessorContinuity::Failed) {
                return Err(AudioProcessorHostError::PoisonedInstance.into());
            }
            if notes.ledger.origin().is_some() {
                if !matches!(
                    instance.continuity,
                    HostedProcessorContinuity::Active { .. }
                ) {
                    return Err(AudioProcessorHostError::PoisonedInstance.into());
                }
                if !matches!(
                    instance.occurrence.owner,
                    AudioProcessorOccurrenceOwner::Track(_)
                ) {
                    deferred_refusal
                        .get_or_insert("Note producer replacement requires an eager Track owner");
                }
                if entry.start_sample != 0 {
                    deferred_refusal.get_or_insert(
                        "Note score cold entry requires public origin zero; seek replay is unsupported",
                    );
                }
            } else if !matches!(instance.continuity, HostedProcessorContinuity::Unentered) {
                return Err(AudioProcessorHostError::PoisonedInstance.into());
            } else if entry.start_sample != 0 {
                deferred_refusal.get_or_insert(
                    "Note score cold entry requires public origin zero; seek replay is unsupported",
                );
            }
        }
        // Any failed target dominates a repeat/seek refusal, even when a
        // preceding target already entered before a later target failed.
        match deferred_refusal {
            Some(reason) => Err(refusal(reason).into()),
            None => Ok(()),
        }
    }

    pub(crate) fn validate_note_entry(
        &self,
        entry: AudioStateEntry,
    ) -> Result<(), AudioExecutionError> {
        self.validate_note_entry_context(entry, self.entry_binding.is_some())
    }

    pub(crate) fn validate_note_entry_context(
        &self,
        entry: AudioStateEntry,
        installed_root: bool,
    ) -> Result<(), AudioExecutionError> {
        if self.instances.iter().any(|instance| instance.notes.is_some()) {
            if self.instances.iter().any(|instance| {
                instance.notes.is_some()
                    && !matches!(
                        instance.occurrence.owner,
                        AudioProcessorOccurrenceOwner::Track(_)
                    )
            }) {
                return Err(refusal("Note entry requires an eager Track owner").into());
            }
            if entry.start_sample != 0 {
                return Err(refusal(
                    "Note score cold entry requires public origin zero; seek replay is unsupported",
                )
                .into());
            }
            if !installed_root {
                return Err(
                    refusal("Note score execution requires the installed root owner").into(),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn clear_note_entry_permits(&mut self) {
        for instance in &mut self.instances {
            if let Some(notes) = &mut instance.notes {
                notes.pending_entry = None;
            }
        }
    }

    pub(crate) fn ensure_note_entry_permits_consumed(&self) -> Result<(), AudioExecutionError> {
        if self.instances.iter().any(|instance| {
            instance.notes.as_ref().is_some_and(|notes| notes.pending_entry.is_some())
        }) {
            return Err(refusal("Note entry transaction was not consumed by eager entry").into());
        }
        Ok(())
    }

    pub(crate) fn arm_current_note_reentry(
        &mut self,
        program: &crate::CompiledAudioProgram,
        entry: AudioStateEntry,
        cancellation: Option<&mondrian_core::ExecutionCancellationToken>,
    ) -> Result<(), AudioExecutionError> {
        let context = self.entry_binding.clone();
        self.arm_note_reentry(program, entry, context.as_ref(), cancellation)
    }

    pub(crate) fn arm_note_reentry(
        &mut self,
        program: &crate::CompiledAudioProgram,
        entry: AudioStateEntry,
        context: Option<&(
            crate::native_runtime_owner::NativeStateEntryBindingContext,
            crate::AudioSessionCursorIdentity,
        )>,
        cancellation: Option<&mondrian_core::ExecutionCancellationToken>,
    ) -> Result<(), AudioExecutionError> {
        // Validate the complete finite owner set before writing any slot.
        for instance in &self.instances {
            let Some(notes) = &instance.notes else {
                continue;
            };
            let processor = crate::schedule::processor(program, instance.occurrence)
                .map_err(|_| AudioExecutionError::InvalidPreparedSchedule)?;
            if !matches!(
                &processor.definition,
                mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
            ) || notes.ledger.origin().is_none()
            {
                continue;
            }
            let (context, local) = context
                .ok_or_else(|| refusal("Note replacement needs the installed root owner"))?;
            context.validate(cancellation)?;
            validate_note_reentry_arm(instance, entry, context, *local)?;
        }
        for instance in &mut self.instances {
            let Some(notes) = &mut instance.notes else {
                continue;
            };
            if notes.pending_entry.is_some() || notes.ledger.origin().is_none() {
                continue;
            }
            let processor = crate::schedule::processor(program, instance.occurrence)
                .map_err(|_| AudioExecutionError::InvalidPreparedSchedule)?;
            if matches!(
                &processor.definition,
                mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
            ) {
                let crate::native_runtime_owner::NativeHostedBinding::Installed { lease, object } =
                    instance.native_binding
                else {
                    unreachable!("validated installed Note owner")
                };
                notes.pending_entry = Some(PendingNoteEntry { lease, object, entry });
            }
        }
        Ok(())
    }

    pub(super) fn take_note_entry(
        instance: &mut HostedProcessorInstance,
        entry: AudioStateEntry,
        context: Option<&(
            crate::native_runtime_owner::NativeStateEntryBindingContext,
            crate::AudioSessionCursorIdentity,
        )>,
        cancellation: Option<&mondrian_core::ExecutionCancellationToken>,
        replacement_required: bool,
    ) -> Result<Option<PendingNoteEntry>, AudioExecutionError> {
        let Some(notes) = &mut instance.notes else {
            return Ok(None);
        };
        let Some(pending) = notes.pending_entry.take() else {
            return if replacement_required {
                Err(refusal("Note replacement has no captured installed producer").into())
            } else {
                Ok(None)
            };
        };
        let (context, local) =
            context.ok_or_else(|| refusal("Note entry lost its installed context"))?;
        context.validate(cancellation)?;
        pending.validate(entry, &notes.ledger, context, *local)?;
        Ok(Some(pending))
    }

    pub(super) fn refuse_pending_note_entry_pcm(
        instance: &mut HostedProcessorInstance,
    ) -> Result<(), AudioExecutionError> {
        if instance
            .notes
            .as_mut()
            .is_some_and(|notes| notes.pending_entry.take().is_some())
        {
            instance.continuity = HostedProcessorContinuity::Failed;
            return Err(refusal("Note entry transaction cannot be consumed by PCM").into());
        }
        Ok(())
    }

    pub(super) fn bind_note_cold_entry(
        instance: &mut HostedProcessorInstance,
        pending: Option<PendingNoteEntry>,
    ) -> Result<(), AudioExecutionError> {
        if instance.notes.is_none() {
            return Ok(());
        }
        let next = origin(instance)?;
        let replacement = if let Some(pending) = pending {
            let crate::native_runtime_owner::NativeHostedBinding::Installed { lease, .. } =
                instance.native_binding
            else {
                unreachable!("origin checked installed owner")
            };
            Some(pending.confirm(next, lease)?)
        } else {
            None
        };
        let ledger = &mut instance.notes.as_mut().expect("observed Note owner").ledger;
        if let Some(proof) = replacement {
            ledger.commit_confirmed_replacement(proof)
        } else {
            ledger.commit_confirmed_cold_entry(next)
        }
        .map_err(ledger_refusal)?;
        Ok(())
    }

    pub(crate) fn preflight_notes(
        &mut self,
        schedule: &PreparedAudioSchedule,
        execution_start: i64,
        frames: usize,
        priming_frames: usize,
    ) -> Result<(), AudioExecutionError> {
        if schedule.notes.sample_rate() != self.render_contract.sample_rate {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        if !schedule.notes.matches_factory_contracts(&schedule.processors) {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        for (slot, (processor, instance)) in
            schedule.processors.iter().zip(&mut self.instances).enumerate()
        {
            if instance.notes.is_none() {
                continue;
            }
            let origin = origin(instance)?;
            let start =
                subtract_signal_delay(execution_start, processor.input_signal_delay_frames)?;
            instance.notes.as_mut().expect("observed Note owner").preflight(
                &schedule.notes,
                slot,
                origin,
                start,
                frames,
                priming_frames,
                self.render_contract.max_block_frames,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{PreparedNoteEvent, PreparedNoteTarget};
    use crate::{
        AudioContinuityEpoch, AudioNativeProcessorAddress, AudioSessionCursorIdentity,
        NativeAudioObjectObservation,
    };
    use mondrian_core::{
        AudioNoteId, AudioNoteLaneId, AudioPreparedPlanId, AudioProcessorBindingId,
        AudioProcessorInstanceId, AudioRenderSessionId, TrackId,
    };
    use std::num::{NonZeroU32, NonZeroU64};

    // Owned scalar/IR controls: this models an already confirmed operation; it
    // does not manufacture or qualify an installed product/native authority.
    struct NoPcm;
    impl AudioProcessor for NoPcm {
        fn process(
            &mut self,
            _: AudioProcessorProcessContext,
            _: &mut dyn AudioProcessorAudioIo,
            _: crate::AudioParameterEventBatch<'_>,
        ) -> Result<(), AudioProcessorHostError> {
            panic!("transaction controls must not execute PCM")
        }
    }

    fn retained_note_owner() -> (
        mondrian_core::AudioOwnerInstallationSource,
        crate::native_runtime_owner::NativeStateEntryBindingContext,
        HostedProcessorInstance,
    ) {
        let origin = operation();
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().unwrap();
        let claim = source
            .begin_commit(source.reserve_install(&registration, None).unwrap())
            .unwrap()
            .commit()
            .unwrap();
        let context = crate::native_runtime_owner::NativeStateEntryBindingContext {
            root: origin.address.root,
            verifier,
            claim,
        };
        let lease = crate::AudioNativeProcessorOwnerLease::issue(
            origin.address,
            origin.binding,
            context.claim.generation(),
        );
        let instance = HostedProcessorInstance {
            occurrence: origin.address.occurrence,
            processor: Box::new(NoPcm),
            continuity: HostedProcessorContinuity::Active {
                epoch: origin.address.epoch.unwrap(),
                next_sample: 1,
            },
            notes: Some(state(origin, 2)),
            native_binding: crate::native_runtime_owner::NativeHostedBinding::Installed {
                lease,
                object: origin.object,
            },
            native_author_outstanding: None,
            native_reprepare_seed_bytes: 0,
            native_editor_detached: None,
            native_editor_attachment: None,
        };
        (source, context, instance)
    }

    fn next_entry() -> AudioStateEntry {
        AudioStateEntry {
            epoch: AudioContinuityEpoch::new(2),
            start_sample: 0,
        }
    }

    fn stage_entry(
        instance: &mut HostedProcessorInstance,
        context: &crate::native_runtime_owner::NativeStateEntryBindingContext,
    ) {
        let pending =
            PendingNoteEntry::capture(instance, next_entry(), context, context.root).unwrap();
        instance.notes.as_mut().unwrap().pending_entry = Some(pending);
        instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
    }

    #[test]
    fn note_reentry_permit_requires_installed_origin_and_survives_only_matching_revoke() {
        let (_source, context, mut instance) = retained_note_owner();
        let entry = next_entry();
        validate_note_reentry_arm(&instance, entry, &context, context.root).unwrap();
        stage_entry(&mut instance, &context);
        let prior = instance.notes.as_ref().unwrap().pending_entry.as_ref().unwrap().prior();
        // Models the two existing local revocations. Neither can re-mint from
        // the ledger: only the captured transaction permits the second arm.
        for _ in 0..2 {
            instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
            validate_note_reentry_arm(&instance, entry, &context, context.root).unwrap();
            assert_eq!(
                instance.notes.as_ref().unwrap().pending_entry.as_ref().unwrap().prior(),
                prior
            );
            assert!(PendingNoteEntry::capture(&instance, entry, &context, context.root).is_err());
        }
        let wrong = AudioStateEntry { epoch: AudioContinuityEpoch::new(3), ..entry };
        assert!(validate_note_reentry_arm(&instance, wrong, &context, context.root).is_err());
        // This exact helper is called before ordinary observation/publication
        // Bind. Losing the slot cannot turn Unbound into cold proof.
        assert!(!PreparedProcessorHost::prepare_native_binding(
            &mut instance,
            context.root,
            context.root
        )
        .unwrap());
        assert!(instance.notes.as_ref().unwrap().pending_entry.is_none());
        assert!(validate_note_reentry_arm(&instance, entry, &context, context.root).is_err());
        assert!(PreparedProcessorHost::take_note_entry(
            &mut instance,
            entry,
            Some(&(context.clone(), context.root)),
            None,
            true
        )
        .is_err());
    }

    #[test]
    fn note_reentry_capture_rejects_wrong_claim_object_occurrence_and_open_gui() {
        for control in 0..4 {
            let (_source, context, mut instance) = retained_note_owner();
            let crate::native_runtime_owner::NativeHostedBinding::Installed { lease, object } =
                instance.native_binding
            else {
                unreachable!()
            };
            match control {
                0 => {
                    instance.native_binding =
                        crate::native_runtime_owner::NativeHostedBinding::Installed {
                            lease: crate::AudioNativeProcessorOwnerLease::issue(
                                lease.address(),
                                lease.binding(),
                                NonZeroU64::new(context.claim.generation().get() + 1).unwrap(),
                            ),
                            object,
                        }
                }
                1 => {
                    instance.native_binding =
                        crate::native_runtime_owner::NativeHostedBinding::Installed {
                            lease,
                            object: NativeAudioObjectObservation::new(
                                object.process_id(),
                                NonZeroU64::new(object.object_serial().get() + 1).unwrap(),
                            ),
                        }
                }
                2 => {
                    instance.occurrence.owner = AudioProcessorOccurrenceOwner::Track(TrackId::new())
                }
                3 => instance.native_editor_attachment = Some((lease, object)),
                _ => unreachable!(),
            }
            let before = instance.notes.as_ref().unwrap().ledger.usage().unwrap();
            assert!(
                validate_note_reentry_arm(&instance, next_entry(), &context, context.root).is_err()
            );
            assert!(instance.notes.as_ref().unwrap().pending_entry.is_none());
            assert_eq!(
                instance.notes.as_ref().unwrap().ledger.usage().unwrap(),
                before
            );
        }
    }

    #[test]
    fn note_reentry_permit_is_consumed_once_and_replacement_keeps_native_id_counter() {
        let (_source, context, mut instance) = retained_note_owner();
        let prior = origin(&instance).unwrap();
        let score = score(prior, &[(0, 3)]);
        instance
            .notes
            .as_mut()
            .unwrap()
            .prepare_admitted_block(
                &score,
                0,
                prior,
                AudioRenderRequest { start_sample: 0, frames: 1 },
            )
            .unwrap();
        assert_eq!(
            instance.notes.as_ref().unwrap().ledger.usage().unwrap().next_native_id,
            1
        );
        stage_entry(&mut instance, &context);
        let pending = PreparedProcessorHost::take_note_entry(
            &mut instance,
            next_entry(),
            Some(&(context.clone(), context.root)),
            None,
            true,
        )
        .unwrap();
        assert!(instance.notes.as_ref().unwrap().pending_entry.is_none());
        assert!(PreparedProcessorHost::take_note_entry(
            &mut instance,
            next_entry(),
            Some(&(context.clone(), context.root)),
            None,
            true
        )
        .is_err());
        // Scalar proof-shape control only. The ignored native gate supplies the
        // real Enter/retirement/Bind witnesses; these fields are not native ACKs.
        let next = LedgerOrigin {
            address: crate::AudioNativeProcessorAddress {
                epoch: Some(next_entry().epoch),
                ..prior.address
            },
            binding: AudioProcessorBindingId::new(),
            object: NativeAudioObjectObservation::new(
                prior.object.process_id(),
                NonZeroU64::new(prior.object.object_serial().get() + 1).unwrap(),
            ),
        };
        instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Installed {
            lease: crate::AudioNativeProcessorOwnerLease::issue(
                next.address,
                next.binding,
                context.claim.generation(),
            ),
            object: next.object,
        };
        instance.continuity =
            HostedProcessorContinuity::Active { epoch: next_entry().epoch, next_sample: 0 };
        PreparedProcessorHost::bind_note_cold_entry(&mut instance, pending).unwrap();
        let usage = instance.notes.as_ref().unwrap().ledger.usage().unwrap();
        assert_eq!(
            (
                usage.retained_records,
                usage.active_voices,
                usage.next_native_id
            ),
            (0, 0, 1)
        );
        instance
            .notes
            .as_mut()
            .unwrap()
            .prepare_admitted_block(
                &score,
                0,
                next,
                AudioRenderRequest { start_sample: 0, frames: 1 },
            )
            .unwrap();
        assert_eq!(instance.notes.as_ref().unwrap().inputs[0].native_note_id, 1);
    }

    #[test]
    fn note_reentry_wrong_request_and_pcm_cannot_reuse_captured_permit() {
        for pcm in [false, true] {
            let (_source, context, mut instance) = retained_note_owner();
            stage_entry(&mut instance, &context);
            let before = instance.notes.as_ref().unwrap().ledger.usage().unwrap();
            if pcm {
                assert!(
                    PreparedProcessorHost::refuse_pending_note_entry_pcm(&mut instance).is_err()
                );
                assert!(matches!(
                    instance.continuity,
                    HostedProcessorContinuity::Failed
                ));
            } else {
                let wrong = AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(3),
                    ..next_entry()
                };
                assert!(PreparedProcessorHost::take_note_entry(
                    &mut instance,
                    wrong,
                    Some(&(context.clone(), context.root)),
                    None,
                    true
                )
                .is_err());
            }
            assert!(instance.notes.as_ref().unwrap().pending_entry.is_none());
            assert_eq!(
                instance.notes.as_ref().unwrap().ledger.usage().unwrap(),
                before
            );
            assert!(PreparedProcessorHost::take_note_entry(
                &mut instance,
                next_entry(),
                Some(&(context.clone(), context.root)),
                None,
                true
            )
            .is_err());
        }
    }

    fn operation() -> LedgerOrigin {
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
                    insertion: crate::AudioProcessorInsertionPoint::PreFader,
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
    fn contract(input_events: usize) -> AudioProcessorNoteInputContract {
        AudioProcessorNoteInputContract {
            completion: crate::AudioNoteCompletionPolicy::NativeEnd,
            ports: vec![crate::AudioProcessorNoteInputPort {
                channel_count: 16,
                port_key: "clap.note-input.27".to_owned(),
                native_id: 27,
                native_index: 0,
            }],
            limits: crate::AudioNoteExecutionLimits {
                input_events,
                output_ends: 2,
                retained_records: 4,
                live_voices: 4,
            },
        }
    }
    fn state(origin: LedgerOrigin, input_events: usize) -> Box<HostedNoteState> {
        let mut state = HostedNoteState::prepare(&contract(input_events)).unwrap().unwrap();
        state.ledger.commit_confirmed_cold_entry(origin).unwrap();
        state
    }
    fn score(origin: LedgerOrigin, pairs: &[(i64, i64)]) -> PreparedNoteScore {
        let lane_id = AudioNoteLaneId::new();
        let mut events = Vec::new();
        for &(start, end) in pairs {
            assert!(end > start);
            let note_id = AudioNoteId::new();
            for (sample, phase, velocity) in [
                (start, PreparedNotePhase::On, -0.0),
                (end, PreparedNotePhase::Off, 0.5),
            ] {
                events.push(PreparedNoteEvent {
                    sample,
                    phase,
                    lane_id,
                    note_id,
                    target_slot: 0,
                    channel: 3,
                    key: 60,
                    velocity,
                });
            }
        }
        PreparedNoteScore::from_test_parts(
            vec![PreparedNoteTarget {
                lane_id,
                processor_slot: 0,
                occurrence: origin.address.occurrence,
                input_port_key: "clap.note-input.27".to_owned(),
                native_port_id: 27,
                native_port_index: 0,
                channel_count: 16,
                completion: crate::AudioNoteCompletionPolicy::NativeEnd,
                limits: contract(4).limits,
            }],
            events,
        )
    }

    #[test]
    fn partial_priming_public_call_capacity_refuses_without_assigning_ids() {
        let origin = operation();
        let mut state = state(origin, 1);
        let score = score(origin, &[(3, 20), (6, 21)]);
        let before = state.ledger.usage().unwrap();
        assert!(matches!(
            state.preflight(&score, 0, origin, 0, 7, 3, 4),
            Err(AudioProcessorHostError::NoteExecutionRefused(
                "Note input event capacity"
            ))
        ));
        assert_eq!(state.ledger.usage().unwrap(), before);
        assert!(state.inputs.is_empty());
    }

    #[test]
    fn partial_priming_and_public_window_are_distinct_native_calls() {
        let origin = operation();
        let mut state = state(origin, 1);
        let score = score(origin, &[(2, 20), (3, 21)]);
        let before = state.ledger.usage().unwrap();
        assert_eq!(state.preflight(&score, 0, origin, 0, 7, 3, 4), Ok(()));
        assert_eq!(state.ledger.usage().unwrap(), before);
        assert_eq!(
            state.preflight(&score, 0, origin, 0, 7, 0, 7),
            Err(refusal("Note input event capacity"))
        );
    }

    #[test]
    fn half_open_off_is_paired_next_block_and_zero_on_bits_survive() {
        let origin = operation();
        let mut state = state(origin, 2);
        let score = score(origin, &[(0, 1)]);
        let before = state.ledger.usage().unwrap();
        assert_eq!(state.preflight(&score, 0, origin, 0, 1, 0, 4), Ok(()));
        assert_eq!(state.ledger.usage().unwrap(), before);
        state
            .prepare_admitted_block(
                &score,
                0,
                origin,
                AudioRenderRequest { start_sample: 0, frames: 1 },
            )
            .unwrap();
        let on = state.inputs[0];
        assert_eq!(on.kind, AudioNoteEventKind::On);
        assert_eq!(on.velocity.to_bits(), (-0.0f64).to_bits());
        assert_eq!(on.sample_offset, 0);
        assert_eq!(state.ledger.usage().unwrap().next_native_id, 1);
        assert_eq!(state.preflight(&score, 0, origin, 1, 1, 0, 4), Ok(()));
        state
            .prepare_admitted_block(
                &score,
                0,
                origin,
                AudioRenderRequest { start_sample: 1, frames: 1 },
            )
            .unwrap();
        let off = state.inputs[0];
        assert_eq!(off.kind, AudioNoteEventKind::Off);
        assert_eq!(
            (off.native_note_id, off.port_index, off.channel, off.key),
            (on.native_note_id, on.port_index, on.channel, on.key)
        );
        assert_eq!(state.ledger.usage().unwrap().active_voices, 1);
    }

    #[test]
    fn same_window_on_off_preflight_uses_staged_facts_without_committing() {
        let origin = operation();
        let mut state = state(origin, 2);
        let score = score(origin, &[(0, 1)]);
        let before = state.ledger.usage().unwrap();
        assert_eq!(state.preflight(&score, 0, origin, 0, 2, 0, 4), Ok(()));
        assert_eq!(state.ledger.usage().unwrap(), before);
        state
            .prepare_admitted_block(
                &score,
                0,
                origin,
                AudioRenderRequest { start_sample: 0, frames: 2 },
            )
            .unwrap();
        assert_eq!(state.inputs.len(), 2);
        assert_eq!(
            state.inputs[0].native_note_id,
            state.inputs[1].native_note_id
        );
        assert_eq!(state.ledger.usage().unwrap().active_voices, 1);
    }

    #[test]
    fn successful_off_preflight_uses_block_peaks_including_partial_priming() {
        let origin = operation();
        let make = |pairs: &[(i64, i64)]| {
            let mut contract = contract(2);
            contract.completion = crate::AudioNoteCompletionPolicy::SuccessfulOffBlock;
            contract.limits.output_ends = 0;
            contract.limits.live_voices = 0;
            contract.limits.retained_records = 1;
            let mut state = HostedNoteState::prepare(&contract).unwrap().unwrap();
            state.ledger.commit_confirmed_cold_entry(origin).unwrap();
            let old = score(origin, pairs);
            let mut targets = old.targets().to_vec();
            targets[0].completion = contract.completion;
            targets[0].limits = contract.limits;
            (
                state,
                PreparedNoteScore::from_test_parts(targets, old.events().to_vec()),
            )
        };
        let pairs = (0..1100).map(|n| (n * 2, n * 2 + 1)).collect::<Vec<_>>();
        let (mut state, score) = make(&pairs);
        let before = state.ledger.usage().unwrap();
        assert_eq!(state.preflight(&score, 0, origin, 0, 2200, 2199, 2), Ok(()));
        assert_eq!(state.ledger.usage().unwrap(), before);
        assert!(state.inputs.is_empty());
        assert!(state.ons.is_empty());
        // Partial priming [0,3) ends its own call before public [3,7).
        let (mut state, score) = make(&[(0, 2), (3, 6)]);
        assert_eq!(state.preflight(&score, 0, origin, 0, 7, 3, 4), Ok(()));
        assert_eq!(
            state.preflight(&score, 0, origin, 0, 7, 0, 7),
            Err(refusal("Note input event capacity"))
        );
        let (mut state, score) = make(&[(0, 1), (1, 2)]);
        state.preflight(&score, 0, origin, 0, 1, 0, 2).unwrap();
        state
            .prepare_admitted_block(
                &score,
                0,
                origin,
                AudioRenderRequest { start_sample: 0, frames: 1 },
            )
            .unwrap();
        state.accept_native_return(origin).unwrap();
        let before = state.ledger.usage().unwrap();
        assert_eq!(
            state.preflight(&score, 0, origin, 1, 1, 0, 2),
            Err(refusal("retained Note record capacity"))
        );
        assert_eq!(state.ledger.usage().unwrap(), before);
    }

    #[test]
    fn successful_off_preflight_refuses_adversarial_cached_identity_and_channel() {
        let origin = operation();
        let mut contract = contract(4);
        contract.completion = crate::AudioNoteCompletionPolicy::SuccessfulOffBlock;
        contract.limits.output_ends = 0;
        contract.limits.live_voices = 0;
        let mut state = HostedNoteState::prepare(&contract).unwrap().unwrap();
        state.ledger.commit_confirmed_cold_entry(origin).unwrap();
        let source = score(origin, &[(0, 1), (2, 3)]);
        let mut targets = source.targets().to_vec();
        targets[0].completion = contract.completion;
        targets[0].limits = contract.limits;
        let mut events = source.events().to_vec();
        let first_id = events[0].note_id;
        events[2].note_id = first_id;
        events[3].note_id = first_id;
        let duplicate = PreparedNoteScore::from_test_parts(targets.clone(), events);
        let before = state.ledger.usage().unwrap();
        assert_eq!(
            state.preflight(&duplicate, 0, origin, 0, 4, 3, 1),
            Err(refusal("duplicate authored Note On"))
        );
        let mut events = source.events().to_vec();
        events[3].key = 61;
        let changed = PreparedNoteScore::from_test_parts(targets.clone(), events);
        assert_eq!(
            state.preflight(&changed, 0, origin, 0, 4, 3, 1),
            Err(refusal("Off changed its original input port"))
        );
        targets[0].channel_count = 3;
        let narrowed = PreparedNoteScore::from_test_parts(targets, source.events().to_vec());
        assert_eq!(
            state.preflight(&narrowed, 0, origin, 0, 4, 3, 1),
            Err(refusal(
                "Note channel or completion differs from admitted contract"
            ))
        );
        assert_eq!(state.ledger.usage().unwrap(), before);
        assert!(state.inputs.is_empty());
    }

    #[test]
    fn actual_state_storage_matches_charge_and_end_sink_latches_unknown_flags() {
        use crate::AudioProcessorFactoryStorageBorrow as Storage;
        let origin = operation();
        let mut state = state(origin, 2);
        let mut bytes = Storage::complete(&state).unwrap().layout().size();
        state
            .visit_storage(&mut |owner| {
                bytes += owner.layout().size();
                Ok(())
            })
            .unwrap();
        assert_eq!(bytes, HostedNoteState::storage_bytes(&contract(2)).unwrap());
        let end_ptr = state.ends.as_ptr();
        let mut pcm = [1.0];
        let end = AudioNoteEndEvent {
            native_note_id: 0,
            port_index: -1,
            channel: -1,
            key: -1,
            flags: 4,
        };
        {
            let mut io = MainBusAudioIo {
                output_buses: &[],
                output_pcm: &mut [],
                channel_layout: AudioChannelLayout::Mono,
                frames: 1,
                pcm: &mut pcm,
                auxiliary_buses: &[],
                auxiliary_pcm: &[],
                notes: Some(NoteBlockIo {
                    inputs: &state.inputs,
                    ends: &mut state.ends,
                    end_limit: state.end_limit,
                    ports: state.ports,
                    refused: &mut state.output_refused,
                }),
            };
            assert!(!io.try_push_note_end(end));
            assert!(!io.try_push_note_end(AudioNoteEndEvent { flags: 0, ..end }));
        }
        assert!(state.ends.is_empty());
        assert_eq!(state.ends.as_ptr(), end_ptr);
        assert_eq!(
            state.accept_native_return(origin),
            Err(refusal("Note End retention refused"))
        );
        assert_eq!(pcm, [1.0]);
    }
}
