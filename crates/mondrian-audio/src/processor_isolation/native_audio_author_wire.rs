//! Field codec for ordered author observations on the one current Poll9 reply.
//! This is not an additional transport, decoder, instance or permission source.

use crate::native_audio_author_observations::*;
use crate::{NativeAudioObjectBindRequest, NativeAudioObjectObservation};
use mondrian_core::NativeAudioParameterRescanKind;
use std::io::{self, Read, Write};

fn rescan_kind_tag(kind: NativeAudioParameterRescanKind) -> u32 {
    match kind {
        NativeAudioParameterRescanKind::NoValueRefresh => 1,
        NativeAudioParameterRescanKind::Values => 2,
        NativeAudioParameterRescanKind::ParameterSetAndValues => 3,
    }
}
fn rescan_kind(tag: u32) -> io::Result<NativeAudioParameterRescanKind> {
    match tag {
        1 => Ok(NativeAudioParameterRescanKind::NoValueRefresh),
        2 => Ok(NativeAudioParameterRescanKind::Values),
        3 => Ok(NativeAudioParameterRescanKind::ParameterSetAndValues),
        _ => Err(invalid("native rescan kind")),
    }
}

fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}
fn u32_read(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
fn u64_read(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn source_failure_tag(value: Option<NativeAudioAuthorSourceFailure>) -> u32 {
    match value {
        None => 0,
        Some(NativeAudioAuthorSourceFailure::WrongThread) => 1,
        Some(NativeAudioAuthorSourceFailure::Overflow) => 2,
        Some(NativeAudioAuthorSourceFailure::InvalidValue) => 3,
        Some(NativeAudioAuthorSourceFailure::InvalidBoundary) => 4,
        Some(NativeAudioAuthorSourceFailure::SequenceExhausted) => 5,
        Some(NativeAudioAuthorSourceFailure::Contention) => 6,
        Some(NativeAudioAuthorSourceFailure::InvalidSource) => 7,
    }
}
fn source_failure(value: u32) -> io::Result<Option<NativeAudioAuthorSourceFailure>> {
    Ok(match value {
        0 => None,
        1 => Some(NativeAudioAuthorSourceFailure::WrongThread),
        2 => Some(NativeAudioAuthorSourceFailure::Overflow),
        3 => Some(NativeAudioAuthorSourceFailure::InvalidValue),
        4 => Some(NativeAudioAuthorSourceFailure::InvalidBoundary),
        5 => Some(NativeAudioAuthorSourceFailure::SequenceExhausted),
        6 => Some(NativeAudioAuthorSourceFailure::Contention),
        7 => Some(NativeAudioAuthorSourceFailure::InvalidSource),
        _ => return Err(invalid("native author sticky failure tag")),
    })
}
fn refusal_tag(value: NativeAudioAuthorRefusal) -> u32 {
    match value {
        NativeAudioAuthorRefusal::UnsupportedAdapter => 1,
        NativeAudioAuthorRefusal::OwnerChanged => 2,
        NativeAudioAuthorRefusal::AwaitingAcceptance => 3,
        NativeAudioAuthorRefusal::InvalidSource => 4,
        NativeAudioAuthorRefusal::UnknownParameter => 5,
        NativeAudioAuthorRefusal::ReadOnlyParameter => 6,
        NativeAudioAuthorRefusal::InvalidValue => 7,
        NativeAudioAuthorRefusal::SourceChanged => 8,
        NativeAudioAuthorRefusal::DspFeedbackPending => 9,
        NativeAudioAuthorRefusal::ConfigurationUnavailable => 10,
        NativeAudioAuthorRefusal::ResourceLimit => 11,
        NativeAudioAuthorRefusal::StaleDelivery => 12,
    }
}
fn refusal(value: u32) -> io::Result<NativeAudioAuthorRefusal> {
    Ok(match value {
        1 => NativeAudioAuthorRefusal::UnsupportedAdapter,
        2 => NativeAudioAuthorRefusal::OwnerChanged,
        3 => NativeAudioAuthorRefusal::AwaitingAcceptance,
        4 => NativeAudioAuthorRefusal::InvalidSource,
        5 => NativeAudioAuthorRefusal::UnknownParameter,
        6 => NativeAudioAuthorRefusal::ReadOnlyParameter,
        7 => NativeAudioAuthorRefusal::InvalidValue,
        8 => NativeAudioAuthorRefusal::SourceChanged,
        9 => NativeAudioAuthorRefusal::DspFeedbackPending,
        10 => NativeAudioAuthorRefusal::ConfigurationUnavailable,
        11 => NativeAudioAuthorRefusal::ResourceLimit,
        12 => NativeAudioAuthorRefusal::StaleDelivery,
        _ => return Err(invalid("native author refusal tag")),
    })
}

fn source_boolean(reader: &mut impl Read) -> io::Result<bool> {
    let mut byte = [0_u8; 1];
    reader.read_exact(&mut byte)?;
    match byte[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid("native author scheduling/publication boolean")),
    }
}

fn validate_binding(
    binding: NativeAudioObjectBindRequest,
    object: Option<NativeAudioObjectObservation>,
) -> io::Result<()> {
    if object != Some(binding.expected_object) {
        return Err(invalid("native author delivery/object mismatch"));
    }
    Ok(())
}

fn write_parameter(
    writer: &mut impl Write,
    parameter: &NativeAudioAuthorParameterEvent,
) -> io::Result<()> {
    let phase: u32 = match parameter.phase {
        NativeAudioAuthorParameterPhase::Begin => 0,
        NativeAudioAuthorParameterPhase::Value => 1,
        NativeAudioAuthorParameterPhase::End => 2,
    };
    writer.write_all(&phase.to_le_bytes())?;
    let id = parameter.parameter.as_str().as_bytes();
    writer.write_all(
        &u32::try_from(id.len())
            .map_err(|_| invalid("native author identity extent"))?
            .to_le_bytes(),
    )?;
    writer.write_all(id)?;
    if let Some(value) = parameter.value {
        writer.write_all(&value.bits().to_le_bytes())?;
    }
    Ok(())
}
fn read_parameter(
    reader: &mut impl Read,
    storage: &mut usize,
) -> io::Result<NativeAudioAuthorParameterEvent> {
    let phase = match u32_read(reader)? {
        0 => NativeAudioAuthorParameterPhase::Begin,
        1 => NativeAudioAuthorParameterPhase::Value,
        2 => NativeAudioAuthorParameterPhase::End,
        _ => return Err(invalid("native author parameter phase")),
    };
    let length =
        usize::try_from(u32_read(reader)?).map_err(|_| invalid("native author identity extent"))?;
    if length == 0 || length > 255 {
        return Err(invalid("native author identity extent"));
    }
    *storage = (*storage)
        .checked_add(length)
        .ok_or_else(|| invalid("native author storage overflow"))?;
    if *storage > super::protocol::MAX_CONTROL_FRAME_BYTES {
        return Err(invalid("native author storage exceeds grant"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let id = mondrian_core::ParameterId::new(
        String::from_utf8(bytes).map_err(|_| invalid("native author identity UTF-8"))?,
    )
    .map_err(|_| invalid("native author schema identity"))?;
    let value = if phase == NativeAudioAuthorParameterPhase::Value {
        Some(
            NativeAudioAuthorScalar::from_bits(u64_read(reader)?)
                .map_err(|_| invalid("native author non-finite value"))?,
        )
    } else {
        None
    };
    NativeAudioAuthorParameterEvent::admit(id, phase, value)
        .map_err(|_| invalid("native author parameter payload"))
}
fn parameter_storage(
    total: usize,
    parameter: &NativeAudioAuthorParameterEvent,
) -> io::Result<usize> {
    if parameter.parameter.storage_owner().capacity() > 255
        || (parameter.phase == NativeAudioAuthorParameterPhase::Value) != parameter.value.is_some()
        || parameter.value.is_some_and(|value| !value.value().is_finite())
    {
        return Err(invalid("native author parameter payload"));
    }
    total
        .checked_add(parameter.parameter.storage_owner().capacity())
        .ok_or_else(|| invalid("native author storage overflow"))
}

fn write_prefix_cohort(
    writer: &mut impl Write,
    cohort: Option<crate::NativeRequestCohort>,
) -> io::Result<()> {
    super::native_configuration_wire::write_observed_request(
        writer,
        cohort.map(crate::NativeObservedRequest::Cohort),
    )
}

fn read_prefix_cohort(reader: &mut impl Read) -> io::Result<Option<crate::NativeRequestCohort>> {
    match super::native_configuration_wire::read_observed_request(reader)? {
        None => Ok(None),
        Some(crate::NativeObservedRequest::Cohort(cohort)) => Ok(Some(cohort)),
        Some(crate::NativeObservedRequest::RequestOnly(_)) => {
            Err(invalid("author prefix requires original cohort identity"))
        }
    }
}

fn validate_prefix_cohort(cohort: Option<crate::NativeRequestCohort>) -> io::Result<()> {
    if let Some(cohort) = cohort {
        crate::NativeRequestCohort::new(cohort.serial, cohort.request)
            .map_err(|_| invalid("invalid author prefix cohort"))?;
    }
    Ok(())
}

fn validate(
    observation: &NativeAudioAuthorObservation,
    object: Option<NativeAudioObjectObservation>,
) -> io::Result<()> {
    observation
        .source
        .validate()
        .map_err(|_| invalid("native author source extent"))?;
    if object.is_none() {
        return Err(invalid("native author source has no actual object"));
    }
    match &observation.delivery {
        NativeAudioAuthorDeliveryState::Delivered(delivery) => {
            validate_binding(delivery.binding, object)?;
            delivery
                .validate(observation.source)
                .map_err(|_| invalid("native author delivery extent"))?;
            let storage = delivery
                .events
                .len()
                .checked_mul(size_of::<NativeAudioAuthorEvent>())
                .and_then(|v| {
                    v.checked_add(
                        delivery
                            .configuration
                            .as_ref()
                            .map_or(0, NativeAudioAuthorConfiguration::retained_capacity),
                    )
                })
                .ok_or_else(|| invalid("native author storage overflow"))?;
            let total = delivery.events.iter().try_fold(storage, |total, event| match event {
                NativeAudioAuthorEvent::ParameterReferencesCleared { parameter, flags } => {
                    if flags & !7 != 0 || parameter.storage_owner().capacity() > 255 {
                        return Err(invalid("native reference-clear payload"));
                    }
                    total
                        .checked_add(parameter.as_str().len())
                        .ok_or_else(|| invalid("native author storage overflow"))
                }
                NativeAudioAuthorEvent::Parameter(parameter) => parameter_storage(total, parameter),
                NativeAudioAuthorEvent::ParameterAtExecution { parameter, sample_rate, .. } => {
                    if *sample_rate == 0 {
                        return Err(invalid("native execution sample rate"));
                    }
                    parameter_storage(total, parameter)
                }
                NativeAudioAuthorEvent::ParameterValuesRescanned { kind, values } => {
                    if values.capacity() > mondrian_core::NATIVE_AUDIO_AUTHOR_RESCAN_VALUES
                        || (!kind.refreshes_values() && !values.is_empty())
                    {
                        return Err(invalid("native rescan shape"));
                    }
                    let storage = total
                        .checked_add(
                            values
                                .capacity()
                                .checked_mul(size_of::<NativeAudioAuthorParameterEvent>())
                                .ok_or_else(|| invalid("native rescan storage"))?,
                        )
                        .ok_or_else(|| invalid("native rescan storage"))?;
                    values.iter().try_fold(storage, |total, parameter| {
                        if parameter.phase != NativeAudioAuthorParameterPhase::Value {
                            return Err(invalid("native rescan non-value"));
                        }
                        parameter_storage(total, parameter)
                    })
                }
                _ => Ok(total),
            })?;
            if total > super::protocol::MAX_CONTROL_FRAME_BYTES {
                return Err(invalid("native author complete payload exceeds grant"));
            }
        }
        NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
            binding,
            revision,
            request_cohort,
        } => {
            validate_binding(*binding, object)?;
            validate_prefix_cohort(*request_cohort)?;
            if *revision == 0 || *revision > observation.source.revision {
                return Err(invalid("native author outstanding revision"));
            }
        }
        NativeAudioAuthorDeliveryState::Acknowledged { acknowledgement: ack, request_cohort } => {
            validate_prefix_cohort(*request_cohort)?;
            validate_binding(ack.source_binding, object)?;
            validate_binding(ack.installed_binding, object)?;
            if ack.revision == 0 || ack.revision > observation.source.revision {
                return Err(invalid("native author acknowledgement revision"));
            }
        }
        NativeAudioAuthorDeliveryState::ReenteredAcknowledged(ack) => {
            super::native_audio_author_state_entry_wire::validate_receipt(ack.preparation)?;
            validate_binding(ack.installed_binding, object)?;
            if ack.preparation.object() != ack.installed_binding.expected_object
                || !matches!(
                    ack.preparation.source(),
                    crate::NativeAudioAuthorStateEntrySource::SelectedTerminal { .. }
                )
            {
                return Err(invalid("reentered author Ack private transition"));
            }
        }
        NativeAudioAuthorDeliveryState::Preserved | NativeAudioAuthorDeliveryState::Refused(_) => {}
    }
    Ok(())
}

pub(super) fn write_author(
    writer: &mut impl Write,
    observation: Option<&NativeAudioAuthorObservation>,
    object: Option<NativeAudioObjectObservation>,
) -> io::Result<()> {
    writer.write_all(&[u8::from(observation.is_some())])?;
    let Some(observation) = observation else {
        return Ok(());
    };
    validate(observation, object)?;
    let source = observation.source;
    writer.write_all(&source.revision.to_le_bytes())?;
    for value in [
        source.pending_events,
        source.pending_parameter_observations,
        source.pending_dsp_feedback_values,
        source.open_parameter_edits,
        source.group_depth,
        source_failure_tag(source.failure),
    ] {
        writer.write_all(&value.to_le_bytes())?;
    }
    writer.write_all(&[
        u8::from(source.parameter_flush_pending),
        u8::from(source.publishing),
    ])?;
    let tag: u32 = match &observation.delivery {
        NativeAudioAuthorDeliveryState::Preserved => 0,
        NativeAudioAuthorDeliveryState::Delivered(_) => 1,
        NativeAudioAuthorDeliveryState::AwaitingAcknowledgement { .. } => 2,
        NativeAudioAuthorDeliveryState::Refused(_) => 3,
        NativeAudioAuthorDeliveryState::Acknowledged { .. } => 4,
        NativeAudioAuthorDeliveryState::ReenteredAcknowledged(_) => 5,
    };
    writer.write_all(&tag.to_le_bytes())?;
    match &observation.delivery {
        NativeAudioAuthorDeliveryState::Delivered(delivery) => {
            super::native_audio_object_wire::write_bind_request(writer, delivery.binding)?;
            writer.write_all(&delivery.revision.to_le_bytes())?;
            write_prefix_cohort(writer, delivery.request_cohort)?;
            writer.write_all(
                &u32::try_from(delivery.events.len())
                    .map_err(|_| invalid("native author event count"))?
                    .to_le_bytes(),
            )?;
            for event in &delivery.events {
                let tag: u32 = match event {
                    NativeAudioAuthorEvent::Parameter(_) => 1,
                    NativeAudioAuthorEvent::ParameterAtExecution { .. } => 6,
                    NativeAudioAuthorEvent::ParameterValuesRescanned { .. } => 7,
                    NativeAudioAuthorEvent::DirtyChanged(_) => 2,
                    NativeAudioAuthorEvent::GroupStarted => 3,
                    NativeAudioAuthorEvent::GroupFinished => 4,
                    NativeAudioAuthorEvent::ParameterReferencesCleared { .. } => 5,
                };
                writer.write_all(&tag.to_le_bytes())?;
                match event {
                    NativeAudioAuthorEvent::Parameter(parameter) => {
                        write_parameter(writer, parameter)?
                    }
                    NativeAudioAuthorEvent::ParameterAtExecution {
                        parameter,
                        sample_position,
                        sample_rate,
                    } => {
                        writer.write_all(&sample_position.to_le_bytes())?;
                        writer.write_all(&sample_rate.to_le_bytes())?;
                        write_parameter(writer, parameter)?;
                    }
                    NativeAudioAuthorEvent::ParameterValuesRescanned { kind, values } => {
                        writer.write_all(&rescan_kind_tag(*kind).to_le_bytes())?;
                        writer.write_all(
                            &u32::try_from(values.len())
                                .map_err(|_| invalid("native rescan value count"))?
                                .to_le_bytes(),
                        )?;
                        for parameter in values {
                            write_parameter(writer, parameter)?;
                        }
                    }
                    NativeAudioAuthorEvent::ParameterReferencesCleared { parameter, flags } => {
                        writer.write_all(&flags.to_le_bytes())?;
                        let id = parameter.as_str().as_bytes();
                        writer.write_all(
                            &u32::try_from(id.len())
                                .map_err(|_| invalid("native clear identity extent"))?
                                .to_le_bytes(),
                        )?;
                        writer.write_all(id)?;
                    }
                    NativeAudioAuthorEvent::DirtyChanged(value) => {
                        writer.write_all(&[u8::from(*value)])?
                    }
                    _ => {}
                }
            }
            writer.write_all(&[match delivery.completion {
                mondrian_core::NativeAudioAuthorCompletion::Nonterminal => 0,
                mondrian_core::NativeAudioAuthorCompletion::ParametersOnly => 1,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration => 2,
            }])?;
            writer.write_all(&[u8::from(delivery.configuration.is_some())])?;
            if let Some(state) = &delivery.configuration {
                writer.write_all(
                    &u32::try_from(state.bytes().len())
                        .map_err(|_| invalid("native author state extent"))?
                        .to_le_bytes(),
                )?;
                writer.write_all(state.bytes())?;
            }
        }
        NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
            binding,
            revision,
            request_cohort,
        } => {
            super::native_audio_object_wire::write_bind_request(writer, *binding)?;
            writer.write_all(&revision.to_le_bytes())?;
            write_prefix_cohort(writer, *request_cohort)?;
        }
        NativeAudioAuthorDeliveryState::Acknowledged { acknowledgement: ack, request_cohort } => {
            super::native_audio_object_wire::write_bind_request(writer, ack.source_binding)?;
            super::native_audio_object_wire::write_bind_request(writer, ack.installed_binding)?;
            writer.write_all(&ack.revision.to_le_bytes())?;
            write_prefix_cohort(writer, *request_cohort)?;
        }
        NativeAudioAuthorDeliveryState::Refused(reason) => {
            writer.write_all(&refusal_tag(*reason).to_le_bytes())?
        }
        NativeAudioAuthorDeliveryState::ReenteredAcknowledged(ack) => {
            super::native_audio_author_state_entry_wire::write_receipt(writer, ack.preparation)?;
            super::native_audio_object_wire::write_bind_request(writer, ack.installed_binding)?;
        }
        NativeAudioAuthorDeliveryState::Preserved => {}
    }
    Ok(())
}

pub(super) fn read_author(
    reader: &mut impl Read,
    object: Option<NativeAudioObjectObservation>,
) -> io::Result<Option<NativeAudioAuthorObservation>> {
    let mut flag = [0];
    reader.read_exact(&mut flag)?;
    match flag[0] {
        0 => return Ok(None),
        1 => {}
        _ => return Err(invalid("native author source presence")),
    }
    let source = NativeAudioAuthorSourceStatus {
        revision: u64_read(reader)?,
        pending_events: u32_read(reader)?,
        pending_parameter_observations: u32_read(reader)?,
        pending_dsp_feedback_values: u32_read(reader)?,
        open_parameter_edits: u32_read(reader)?,
        group_depth: u32_read(reader)?,
        failure: source_failure(u32_read(reader)?)?,
        parameter_flush_pending: source_boolean(reader)?,
        publishing: source_boolean(reader)?,
    };
    source.validate().map_err(|_| invalid("native author source extent"))?;
    let delivery = match u32_read(reader)? {
        0 => NativeAudioAuthorDeliveryState::Preserved,
        1 => {
            let binding = super::native_audio_object_wire::read_bind_request(reader)?;
            validate_binding(binding, object)?;
            let revision = u64_read(reader)?;
            let request_cohort = read_prefix_cohort(reader)?;
            let count = usize::try_from(u32_read(reader)?)
                .map_err(|_| invalid("native author count extent"))?;
            if count == 0
                || count > NATIVE_AUDIO_AUTHOR_EVENTS
                || revision == 0
                || revision > source.revision
            {
                return Err(invalid("native author complete event extent"));
            }
            let mut storage = count
                .checked_mul(size_of::<NativeAudioAuthorEvent>())
                .ok_or_else(|| invalid("native author storage overflow"))?;
            if storage > super::protocol::MAX_CONTROL_FRAME_BYTES {
                return Err(invalid("native author storage exceeds grant"));
            }
            let mut events = Vec::with_capacity(count);
            for _ in 0..count {
                events.push(match u32_read(reader)? {
                    1 => NativeAudioAuthorEvent::Parameter(read_parameter(reader, &mut storage)?),
                    6 => {
                        let mut sample = [0; 8];
                        reader.read_exact(&mut sample)?;
                        let sample_position = i64::from_le_bytes(sample);
                        let sample_rate = u32_read(reader)?;
                        if sample_rate == 0 {
                            return Err(invalid("native execution sample rate"));
                        }
                        NativeAudioAuthorEvent::ParameterAtExecution {
                            sample_position,
                            sample_rate,
                            parameter: read_parameter(reader, &mut storage)?,
                        }
                    }
                    7 => {
                        let kind = rescan_kind(u32_read(reader)?)?;
                        let count = usize::try_from(u32_read(reader)?)
                            .map_err(|_| invalid("native rescan count"))?;
                        if count > mondrian_core::NATIVE_AUDIO_AUTHOR_RESCAN_VALUES
                            || (!kind.refreshes_values() && count != 0)
                        {
                            return Err(invalid("native rescan shape"));
                        }
                        let mut values = Vec::new();
                        values
                            .try_reserve_exact(count)
                            .map_err(|_| invalid("native rescan allocation"))?;
                        if values.capacity() > mondrian_core::NATIVE_AUDIO_AUTHOR_RESCAN_VALUES {
                            return Err(invalid("native rescan capacity"));
                        }
                        storage = storage
                            .checked_add(
                                values
                                    .capacity()
                                    .checked_mul(size_of::<NativeAudioAuthorParameterEvent>())
                                    .ok_or_else(|| invalid("native rescan storage"))?,
                            )
                            .ok_or_else(|| invalid("native rescan storage"))?;
                        if storage > super::protocol::MAX_CONTROL_FRAME_BYTES {
                            return Err(invalid("native rescan exceeds grant"));
                        }
                        for _ in 0..count {
                            let parameter = read_parameter(reader, &mut storage)?;
                            if parameter.phase != NativeAudioAuthorParameterPhase::Value {
                                return Err(invalid("native rescan non-value"));
                            }
                            values.push(parameter);
                        }
                        NativeAudioAuthorEvent::ParameterValuesRescanned { kind, values }
                    }
                    2 => {
                        let mut flag = [0];
                        reader.read_exact(&mut flag)?;
                        NativeAudioAuthorEvent::DirtyChanged(match flag[0] {
                            0 => false,
                            1 => true,
                            _ => return Err(invalid("native author dirty flag")),
                        })
                    }
                    3 => NativeAudioAuthorEvent::GroupStarted,
                    4 => NativeAudioAuthorEvent::GroupFinished,
                    5 => {
                        let flags = u32_read(reader)?;
                        if flags & !7 != 0 {
                            return Err(invalid("native clear flags"));
                        }
                        let length = usize::try_from(u32_read(reader)?)
                            .map_err(|_| invalid("native clear identity extent"))?;
                        if length == 0 || length > 255 {
                            return Err(invalid("native clear identity extent"));
                        }
                        storage = storage
                            .checked_add(length)
                            .ok_or_else(|| invalid("native author storage overflow"))?;
                        if storage > super::protocol::MAX_CONTROL_FRAME_BYTES {
                            return Err(invalid("native author storage exceeds grant"));
                        }
                        let mut bytes = vec![0; length];
                        reader.read_exact(&mut bytes)?;
                        let parameter = mondrian_core::ParameterId::new(
                            String::from_utf8(bytes)
                                .map_err(|_| invalid("native clear identity UTF-8"))?,
                        )
                        .map_err(|_| invalid("native clear identity"))?;
                        NativeAudioAuthorEvent::ParameterReferencesCleared { parameter, flags }
                    }
                    _ => return Err(invalid("native author event tag")),
                });
            }
            let mut flag = [0];
            reader.read_exact(&mut flag)?;
            let completion = match flag[0] {
                0 => mondrian_core::NativeAudioAuthorCompletion::Nonterminal,
                1 => mondrian_core::NativeAudioAuthorCompletion::ParametersOnly,
                2 => mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
                _ => return Err(invalid("native author completion kind")),
            };
            reader.read_exact(&mut flag)?;
            let configuration = match flag[0] {
                0 => None,
                1 => {
                    let length = usize::try_from(u32_read(reader)?)
                        .map_err(|_| invalid("native author configuration extent"))?;
                    if length == 0 || length > NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES {
                        return Err(invalid("native author configuration extent"));
                    }
                    storage = storage
                        .checked_add(length)
                        .ok_or_else(|| invalid("native author storage overflow"))?;
                    if storage > super::protocol::MAX_CONTROL_FRAME_BYTES {
                        return Err(invalid("native author storage exceeds grant"));
                    }
                    let mut state = vec![0; length];
                    reader.read_exact(&mut state)?;
                    Some(
                        NativeAudioAuthorConfiguration::admit(state)
                            .map_err(|_| invalid("native author configuration grant"))?,
                    )
                }
                _ => return Err(invalid("native author configuration presence")),
            };
            NativeAudioAuthorDeliveryState::Delivered(NativeAudioAuthorDelivery {
                binding,
                revision,
                request_cohort,
                events,
                configuration,
                completion,
            })
        }
        2 => NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
            binding: super::native_audio_object_wire::read_bind_request(reader)?,
            revision: u64_read(reader)?,
            request_cohort: read_prefix_cohort(reader)?,
        },
        3 => NativeAudioAuthorDeliveryState::Refused(refusal(u32_read(reader)?)?),
        4 => NativeAudioAuthorDeliveryState::Acknowledged {
            acknowledgement: NativeAudioAuthorAcknowledgement {
                source_binding: super::native_audio_object_wire::read_bind_request(reader)?,
                installed_binding: super::native_audio_object_wire::read_bind_request(reader)?,
                revision: u64_read(reader)?,
            },
            request_cohort: read_prefix_cohort(reader)?,
        },
        5 => NativeAudioAuthorDeliveryState::ReenteredAcknowledged(
            crate::NativeAudioReenteredAuthorAcknowledgement {
                preparation: super::native_audio_author_state_entry_wire::read_receipt(reader)?,
                installed_binding: super::native_audio_object_wire::read_bind_request(reader)?,
            },
        ),
        _ => return Err(invalid("native author delivery tag")),
    };
    let observation = NativeAudioAuthorObservation { source, delivery };
    validate(&observation, object)?;
    Ok(Some(observation))
}
#[cfg(test)]
mod native_author_wire_contracts {
    use super::*;
    use std::io::Cursor;
    use std::num::{NonZeroU32, NonZeroU64};
    fn object() -> NativeAudioObjectObservation {
        NativeAudioObjectObservation::new(
            NonZeroU32::new(97).expect("PID"),
            NonZeroU64::new(3).expect("actual load"),
        )
    }
    fn binding() -> NativeAudioObjectBindRequest {
        NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: object(),
        }
    }
    fn closed_source() -> NativeAudioAuthorSourceStatus {
        NativeAudioAuthorSourceStatus {
            parameter_flush_pending: false,
            publishing: false,
            revision: 7,
            pending_events: 0,
            pending_dsp_feedback_values: 0,
            open_parameter_edits: 0,
            group_depth: 0,
            failure: None,

            pending_parameter_observations: 0,
        }
    }
    fn complete() -> NativeAudioAuthorObservation {
        let parameter = mondrian_core::ParameterId::new("vst3.0").expect("machine parameter");
        NativeAudioAuthorObservation {
            source: closed_source(),
            delivery: NativeAudioAuthorDeliveryState::Delivered(NativeAudioAuthorDelivery {
                binding: binding(),
                revision: 7,
                request_cohort: None,
                events: vec![
                    NativeAudioAuthorEvent::GroupStarted,
                    NativeAudioAuthorEvent::Parameter(
                        NativeAudioAuthorParameterEvent::admit(
                            parameter.clone(),
                            NativeAudioAuthorParameterPhase::Begin,
                            None,
                        )
                        .expect("actual Begin"),
                    ),
                    NativeAudioAuthorEvent::Parameter(
                        NativeAudioAuthorParameterEvent::admit(
                            parameter.clone(),
                            NativeAudioAuthorParameterPhase::Value,
                            Some(NativeAudioAuthorScalar::admit(0.75).expect("actual scalar")),
                        )
                        .expect("Value"),
                    ),
                    NativeAudioAuthorEvent::Parameter(
                        NativeAudioAuthorParameterEvent::admit(
                            parameter,
                            NativeAudioAuthorParameterPhase::End,
                            None,
                        )
                        .expect("actual End"),
                    ),
                    NativeAudioAuthorEvent::DirtyChanged(true),
                    NativeAudioAuthorEvent::GroupFinished,
                    NativeAudioAuthorEvent::DirtyChanged(false),
                ],
                completion: mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
                configuration: Some(
                    NativeAudioAuthorConfiguration::admit(vec![5, 7, 11])
                        .expect("bounded actual state"),
                ),
            }),
        }
    }
    fn roundtrip(value: &NativeAudioAuthorObservation) -> NativeAudioAuthorObservation {
        let mut bytes = Vec::new();
        write_author(&mut bytes, Some(value), Some(object())).expect("one V7 author field");
        let mut reader = Cursor::new(bytes);
        let actual = read_author(&mut reader, Some(object()))
            .expect("same V7 decoder")
            .expect("normative source");
        assert_eq!(
            reader.position(),
            reader.get_ref().len() as u64,
            "field decoder consumes the exact declared extent"
        );
        actual
    }

    #[test]
    fn native_rescan_wire_uses_independent_kind_tags_and_rejects_unknown_before_values() {
        for (kind, tag) in [
            (NativeAudioParameterRescanKind::NoValueRefresh, 1_u32),
            (NativeAudioParameterRescanKind::Values, 2),
            (NativeAudioParameterRescanKind::ParameterSetAndValues, 3),
        ] {
            let mut observation = complete();
            let NativeAudioAuthorDeliveryState::Delivered(delivery) = &mut observation.delivery
            else {
                panic!("fixture");
            };
            delivery.events =
                vec![NativeAudioAuthorEvent::ParameterValuesRescanned { kind, values: Vec::new() }];
            let mut bytes = Vec::new();
            write_author(&mut bytes, Some(&observation), Some(object())).unwrap();
            // One empty rescan record, coherent completion=2, presence=1,
            // state length=3 and literal state [5,7,11] occupy the final 21 bytes.
            let offset = bytes.len() - 21;
            let mut expected = vec![7, 0, 0, 0];
            expected.extend_from_slice(&tag.to_le_bytes());
            expected.extend_from_slice(&[0, 0, 0, 0, 2, 1, 3, 0, 0, 0, 5, 7, 11]);
            assert_eq!(&bytes[offset..], expected);
            assert_eq!(roundtrip(&observation), observation);
            for unknown in [0_u32, 4, 16, u32::MAX] {
                let mut bad = bytes.clone();
                bad[offset + 4..offset + 8].copy_from_slice(&unknown.to_le_bytes());
                bad[offset + 8..offset + 12].copy_from_slice(&u32::MAX.to_le_bytes());
                let mut reader = Cursor::new(bad);
                let error = read_author(&mut reader, Some(object())).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                assert_eq!(error.to_string(), "native rescan kind");
                assert_eq!(
                    reader.position(),
                    (offset + 8) as u64,
                    "unknown kind refuses before reading the value count or reserving values"
                );
            }
            if kind.refreshes_values() {
                for oversized in [4097_u32, u32::MAX] {
                    let mut bad = bytes.clone();
                    bad[offset + 8..offset + 12].copy_from_slice(&oversized.to_le_bytes());
                    let mut reader = Cursor::new(bad);
                    assert_eq!(
                        read_author(&mut reader, Some(object())).unwrap_err().to_string(),
                        "native rescan shape"
                    );
                    assert_eq!(
                        reader.position(),
                        (offset + 12) as u64,
                        "oversized value count refuses before values or state"
                    );
                }
            }
            if kind == NativeAudioParameterRescanKind::NoValueRefresh {
                bytes[offset + 8..offset + 12].copy_from_slice(&1_u32.to_le_bytes());
                let mut reader = Cursor::new(bytes);
                assert_eq!(
                    read_author(&mut reader, Some(object())).unwrap_err().to_string(),
                    "native rescan shape"
                );
                assert_eq!(reader.position(), (offset + 12) as u64);
            }
        }
    }

    #[test]
    fn native_rescan_wire_refuses_no_refresh_values_nonvalue_and_oversized_capacity() {
        let parameter = NativeAudioAuthorParameterEvent::admit(
            mondrian_core::ParameterId::new("test.value").unwrap(),
            NativeAudioAuthorParameterPhase::Value,
            Some(NativeAudioAuthorScalar::admit(0.625).unwrap()),
        )
        .unwrap();
        let mut oversized =
            Vec::with_capacity(mondrian_core::NATIVE_AUDIO_AUTHOR_RESCAN_VALUES + 1);
        oversized.push(parameter.clone());
        for (kind, values) in [
            (
                NativeAudioParameterRescanKind::NoValueRefresh,
                vec![parameter],
            ),
            (NativeAudioParameterRescanKind::Values, oversized),
            (
                NativeAudioParameterRescanKind::Values,
                vec![NativeAudioAuthorParameterEvent::admit(
                    mondrian_core::ParameterId::new("test.value").unwrap(),
                    NativeAudioAuthorParameterPhase::Begin,
                    None,
                )
                .unwrap()],
            ),
        ] {
            let mut observation = complete();
            let NativeAudioAuthorDeliveryState::Delivered(delivery) = &mut observation.delivery
            else {
                panic!("fixture");
            };
            delivery.events =
                vec![NativeAudioAuthorEvent::ParameterValuesRescanned { kind, values }];
            assert!(write_author(&mut Vec::new(), Some(&observation), Some(object())).is_err());
        }
    }

    #[test]
    fn native_rescan_wire_preserves_complete_4096_value_payload() {
        let mut observation = complete();
        let NativeAudioAuthorDeliveryState::Delivered(delivery) = &mut observation.delivery else {
            panic!("fixture")
        };
        let values = (0..4096)
            .map(|id| {
                NativeAudioAuthorParameterEvent::admit(
                    mondrian_core::ParameterId::new(format!("vst3.full{id}")).unwrap(),
                    NativeAudioAuthorParameterPhase::Value,
                    Some(NativeAudioAuthorScalar::admit(0.625).unwrap()),
                )
                .unwrap()
            })
            .collect();
        delivery.events = vec![NativeAudioAuthorEvent::ParameterValuesRescanned {
            kind: NativeAudioParameterRescanKind::Values,
            values,
        }];
        assert_eq!(roundtrip(&observation), observation);
    }
    #[test]
    fn native_author_parameter_completion_wire_has_independent_kind_and_no_state() {
        use mondrian_core::NativeAudioAuthorCompletion as Completion;
        let observation = NativeAudioAuthorObservation {
            source: NativeAudioAuthorSourceStatus { revision: 1, ..closed_source() },
            delivery: NativeAudioAuthorDeliveryState::Delivered(NativeAudioAuthorDelivery {
                binding: binding(),
                revision: 1,
                request_cohort: None,
                events: vec![NativeAudioAuthorEvent::Parameter(
                    NativeAudioAuthorParameterEvent::admit(
                        mondrian_core::ParameterId::new("clap.17").expect("stable ID"),
                        NativeAudioAuthorParameterPhase::Value,
                        Some(NativeAudioAuthorScalar::admit(0.75).expect("actual scalar")),
                    )
                    .expect("value"),
                )],
                configuration: None,
                completion: Completion::ParametersOnly,
            }),
        };
        let NativeAudioAuthorDeliveryState::Delivered(ref delivery) = observation.delivery else {
            panic!("fixture");
        };
        let mut expected = vec![1];
        expected.extend_from_slice(&1_u64.to_le_bytes());
        for value in [0_u32; 6] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&[0, 0]);
        expected.extend_from_slice(&1_u32.to_le_bytes());
        expected.extend_from_slice(delivery.binding.binding.0.as_bytes());
        expected.extend_from_slice(&97_u32.to_le_bytes());
        expected.extend_from_slice(&3_u64.to_le_bytes());
        expected.extend_from_slice(&1_u64.to_le_bytes());
        expected.extend_from_slice(&[0; 16]); // no prefix request cohort, independently encoded
        expected.extend_from_slice(&1_u32.to_le_bytes());
        expected.extend_from_slice(&1_u32.to_le_bytes());
        expected.extend_from_slice(&1_u32.to_le_bytes());
        expected.extend_from_slice(&7_u32.to_le_bytes());
        expected.extend_from_slice(b"clap.17");
        expected.extend_from_slice(&0.75_f64.to_bits().to_le_bytes());
        expected.extend_from_slice(&[1, 0]);
        let mut actual = Vec::new();
        write_author(&mut actual, Some(&observation), Some(object())).expect("one current schema");
        assert_eq!(
            actual, expected,
            "independent field oracle, no encoder reuse"
        );
        assert_eq!(roundtrip(&observation), observation);
        let completion_offset = actual.len() - 2;
        for invalid_kind in [2, 3, 255] {
            let mut malformed = actual.clone();
            malformed[completion_offset] = invalid_kind;
            assert!(
                read_author(&mut Cursor::new(malformed), Some(object())).is_err(),
                "opaque-without-state and unknown completion are refused"
            );
        }
        let mut incomplete = observation;
        incomplete.source.open_parameter_edits = 1;
        assert!(write_author(&mut Vec::new(), Some(&incomplete), Some(object())).is_err());
    }

    #[test]
    fn native_author_wire_complete_group_and_coherent_bytes_preserve_exact_scalar_and_order() {
        let expected = complete();
        assert_eq!(roundtrip(&expected), expected);
        let mut observations = crate::NativeAudioObservations {
            owner_process_id: 97,
            object: Some(object()),
            editor_cleanup: Some(crate::NativeAudioEditorCloseStage::ClearHostFrame),
            pending_reprepare: Some(crate::NativeObservedRequest::RequestOnly(
                mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 0x8000_0010 },
            )),
            author: Some(expected),
            ..Default::default()
        };
        let mut bytes = Vec::new();
        super::super::native_audio_object_wire::write_reply(
            &mut bytes,
            super::super::native_audio_object_wire::ObjectReply::Observation(&observations),
        )
        .expect("sole current reply");
        let actual =
            super::super::native_audio_object_wire::read_observation_reply(&mut Cursor::new(bytes))
                .expect("one native decoder");
        assert_eq!(
            actual, observations,
            "pending/cleanup/author facts use the same native object reply"
        );
        observations.object = None;
        assert!(super::super::native_audio_object_wire::write_reply(
            &mut Vec::new(),
            super::super::native_audio_object_wire::ObjectReply::Observation(&observations)
        )
        .is_err());
    }

    #[test]
    fn native_author_wire_preserve_awaiting_refused_and_ack_are_distinct_from_empty_source() {
        let binding = binding();
        for delivery in [
            NativeAudioAuthorDeliveryState::Preserved,
            NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
                binding,
                revision: 7,
                request_cohort: None,
            },
            NativeAudioAuthorDeliveryState::Refused(NativeAudioAuthorRefusal::InvalidSource),
            NativeAudioAuthorDeliveryState::Acknowledged {
                acknowledgement: NativeAudioAuthorAcknowledgement {
                    source_binding: binding,
                    installed_binding: binding,
                    revision: 7,
                },
                request_cohort: None,
            },
        ] {
            let expected = NativeAudioAuthorObservation { source: closed_source(), delivery };
            assert_eq!(roundtrip(&expected), expected);
        }
        let mut missing = Vec::new();
        write_author(&mut missing, None, None).expect("unsupported source");
        assert_eq!(missing, [0]);
        assert_eq!(
            read_author(&mut Cursor::new(missing), None).expect("explicit unavailable"),
            None
        );
    }

    #[test]
    fn native_author_wire_partial_timestamp_group_may_close_with_parameter_gestures_open() {
        let mut expected = complete();
        expected.source.open_parameter_edits = 2;
        let NativeAudioAuthorDeliveryState::Delivered(ref mut delivery) = expected.delivery else {
            panic!("delivery")
        };
        delivery.configuration = None;
        delivery.completion = mondrian_core::NativeAudioAuthorCompletion::Nonterminal;
        assert_eq!(
            roundtrip(&expected),
            expected,
            "official multi-fader pattern must not require GroupFinish to close parameter edits"
        );
        let NativeAudioAuthorDeliveryState::Delivered(ref mut delivery) = expected.delivery else {
            panic!("delivery")
        };
        delivery.configuration =
            Some(NativeAudioAuthorConfiguration::admit(vec![1]).expect("small fixture state"));
        assert!(
            write_author(&mut Vec::new(), Some(&expected), Some(object())).is_err(),
            "opaque state cannot commit while individual gestures remain open"
        );
    }

    #[test]
    fn native_author_wire_request_drain_and_ack_bind_exact_actual_load_and_revision() {
        let binding = binding();
        for requested in [
            crate::NativeAudioObservationConsumption::Preserve,
            crate::NativeAudioObservationConsumption::Drain,
            crate::NativeAudioObservationConsumption::DrainAuthor(binding),
            crate::NativeAudioObservationConsumption::AcknowledgeAuthor(
                NativeAudioAuthorAcknowledgement {
                    source_binding: binding,
                    installed_binding: binding,
                    revision: 7,
                },
            ),
        ] {
            let mut bytes = Vec::new();
            super::super::native_audio_object_wire::write_observation_request(
                &mut bytes, requested,
            )
            .expect("bounded actual request");
            assert_eq!(
                super::super::native_audio_object_wire::read_observation_request(&mut Cursor::new(
                    bytes
                ))
                .expect("sole Poll9 request decoder"),
                requested
            );
        }
        let bad = crate::NativeAudioObservationConsumption::AcknowledgeAuthor(
            NativeAudioAuthorAcknowledgement {
                source_binding: binding,
                installed_binding: binding,
                revision: 0,
            },
        );
        assert!(
            super::super::native_audio_object_wire::write_observation_request(&mut Vec::new(), bad)
                .is_err()
        );
    }

    #[test]
    fn native_author_wire_rejects_wrong_load_nan_and_oversized_state_before_author_publication() {
        let expected = complete();
        let mut bytes = Vec::new();
        write_author(&mut bytes, Some(&expected), Some(object())).expect("positive actual frame");
        let replacement = NativeAudioObjectObservation::new(
            NonZeroU32::new(97).expect("same PID"),
            NonZeroU64::new(4).expect("different actual load"),
        );
        assert!(
            read_author(&mut Cursor::new(bytes), Some(replacement)).is_err(),
            "same PID does not authorize a different native object"
        );
        let mut bytes = Vec::new();
        write_author(&mut bytes, Some(&expected), Some(object())).expect("finite baseline");
        let needle = 0.75_f64.to_bits().to_le_bytes();
        let positions: Vec<_> = bytes
            .windows(8)
            .enumerate()
            .filter_map(|(i, value)| (value == needle).then_some(i))
            .collect();
        assert_eq!(positions.len(), 1, "independent malformed scalar fixture");
        bytes[positions[0]..positions[0] + 8].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
        assert!(read_author(&mut Cursor::new(bytes), Some(object())).is_err());
        assert!(NativeAudioAuthorConfiguration::admit(vec![
            0;
            NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES
                + 1
        ])
        .is_err());
        assert!(
            read_author(&mut Cursor::new([2_u8]), Some(object())).is_err(),
            "unknown presence cannot become an empty source"
        );
    }

    fn prefix_receipt_cases(
        request_cohort: Option<crate::NativeRequestCohort>,
    ) -> [(NativeAudioAuthorObservation, usize); 3] {
        let mut delivered = complete();
        let NativeAudioAuthorDeliveryState::Delivered(delivery) = &mut delivered.delivery else {
            panic!("complete delivery fixture");
        };
        delivery.request_cohort = request_cohort;
        [
            (delivered, 75),
            (
                NativeAudioAuthorObservation {
                    source: closed_source(),
                    delivery: NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
                        binding: binding(),
                        revision: 7,
                        request_cohort,
                    },
                },
                75,
            ),
            (
                NativeAudioAuthorObservation {
                    source: closed_source(),
                    delivery: NativeAudioAuthorDeliveryState::Acknowledged {
                        acknowledgement: NativeAudioAuthorAcknowledgement {
                            source_binding: binding(),
                            installed_binding: binding(),
                            revision: 7,
                        },
                        request_cohort,
                    },
                },
                103,
            ),
        ]
    }

    #[test]
    fn native_author_prefix_cohort_stays_with_delivery_while_live_pending_is_newer() {
        let a = crate::NativeRequestCohort::new(
            41,
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
        )
        .unwrap();
        let b = crate::NativeRequestCohort::new(42, a.request).unwrap();
        for prefix in [None, Some(a)] {
            for (author, offset) in prefix_receipt_cases(prefix) {
                let mut bytes = Vec::new();
                write_author(&mut bytes, Some(&author), Some(object())).unwrap();
                let expected = if prefix.is_some() {
                    [2, 0, 0, 0, 41, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0]
                } else {
                    [0; 16]
                };
                assert_eq!(
                    &bytes[offset..offset + 16],
                    &expected,
                    "prefix identity has an independent fixed byte oracle"
                );
                assert_eq!(roundtrip(&author), author);
                let observations = crate::NativeAudioObservations {
                    owner_process_id: 97,
                    object: Some(object()),
                    pending_reprepare: Some(crate::NativeObservedRequest::Cohort(b)),
                    author: Some(author),
                    ..Default::default()
                };
                let mut bytes = Vec::new();
                super::super::native_audio_object_wire::write_reply(
                    &mut bytes,
                    super::super::native_audio_object_wire::ObjectReply::Observation(&observations),
                )
                .unwrap();
                let actual = super::super::native_audio_object_wire::read_observation_reply(
                    &mut Cursor::new(bytes),
                )
                .unwrap();
                assert_eq!(
                    actual, observations,
                    "live B never supplies or replaces an original prefix A/None"
                );
            }
        }
    }

    #[test]
    fn native_author_prefix_wire_rejects_raw_only_and_noncanonical_cohorts() {
        let a = crate::NativeRequestCohort::new(
            u32::MAX,
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 0x8000_0004 },
        )
        .unwrap();
        for (author, offset) in prefix_receipt_cases(Some(a)) {
            assert_eq!(
                roundtrip(&author),
                author,
                "unknown bits and exhausted serial remain observations, not permission"
            );
            let mut bytes = Vec::new();
            write_author(&mut bytes, Some(&author), Some(object())).unwrap();
            for words in [
                [1_u32, 0, 1, 4], // RequestOnly is not prefix identity.
                [0, 0, 0, 4],
                [0, 1, 0, 0],
                [2, 0, 1, 4],
                [2, 41, 1, 0],
                [2, 41, 2, 4],
                [3, 41, 1, 4],
            ] {
                let mut invalid = bytes.clone();
                for (index, word) in words.into_iter().enumerate() {
                    invalid[offset + index * 4..offset + index * 4 + 4]
                        .copy_from_slice(&word.to_le_bytes());
                }
                assert!(read_author(&mut Cursor::new(invalid), Some(object())).is_err());
            }
            for cut in 0..16 {
                assert!(
                    read_author(&mut Cursor::new(&bytes[..offset + cut]), Some(object())).is_err()
                );
            }
        }
        for (author, _) in prefix_receipt_cases(Some(crate::NativeRequestCohort {
            serial: 0,
            request: a.request,
        })) {
            assert!(write_author(&mut Vec::new(), Some(&author), Some(object())).is_err());
        }
    }

    #[test]
    fn native_author_wire_incomplete_frame_never_reports_a_complete_prefix() {
        let expected = complete();
        let mut bytes = Vec::new();
        write_author(&mut bytes, Some(&expected), Some(object())).expect("finite positive frame");
        for length in [1, 8, bytes.len() - 1] {
            assert!(
                read_author(&mut Cursor::new(&bytes[..length]), Some(object())).is_err(),
                "truncated response must not supply a successful partial author batch"
            );
        }
    }
}

#[cfg(test)]
mod observation_status_wire_contracts {
    use super::*;
    use std::num::{NonZeroU32, NonZeroU64};
    #[test]
    fn native_author_observation_pending_wire_has_independent_count_and_bound() {
        let object = NativeAudioObjectObservation::new(
            NonZeroU32::new(97).expect("PID"),
            NonZeroU64::new(3).expect("load"),
        );
        let status = NativeAudioAuthorSourceStatus {
            revision: 7,
            pending_events: 0,
            pending_parameter_observations: 2,
            pending_dsp_feedback_values: 0,
            open_parameter_edits: 0,
            group_depth: 0,
            failure: None,
            parameter_flush_pending: false,
            publishing: false,
        };
        let observation = NativeAudioAuthorObservation {
            source: status,
            delivery: NativeAudioAuthorDeliveryState::Preserved,
        };
        let mut expected = vec![1];
        expected.extend_from_slice(&7_u64.to_le_bytes());
        for field in [0_u32, 2, 0, 0, 0, 0] {
            expected.extend_from_slice(&field.to_le_bytes());
        }
        expected.extend_from_slice(&[0, 0]);
        expected.extend_from_slice(&0_u32.to_le_bytes());
        let mut actual = Vec::new();
        write_author(&mut actual, Some(&observation), Some(object))
            .expect("current sole author status");
        assert_eq!(actual, expected, "source presence/revision, author/observation/DSP/open/group/failure, flags, preserved tag");
        assert_eq!(actual.len(), 39);
        let decoded = read_author(&mut std::io::Cursor::new(&actual), Some(object))
            .expect("current decoder")
            .expect("source");
        assert_eq!(decoded.source.pending_events(), 0);
        assert_eq!(decoded.source.pending_parameter_observations(), 2);
        assert!(!decoded.source.closed_empty());
        actual[13..17].copy_from_slice(&4097_u32.to_le_bytes());
        assert!(read_author(&mut std::io::Cursor::new(actual), Some(object)).is_err());
    }
}
