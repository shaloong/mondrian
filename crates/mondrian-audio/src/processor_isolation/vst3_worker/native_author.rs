//! Same-instance VST author observations. This module owns neither a controller
//! nor an author Session. It preserves one outstanding native delivery until
//! the installed Runtime confirms canonical author plus live-plan acceptance.

use super::*;
use crate::{
    NativeAudioAuthorAcknowledgement, NativeAudioAuthorConfiguration, NativeAudioAuthorDelivery,
    NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorEvent as Event,
    NativeAudioAuthorObservation, NativeAudioAuthorParameterEvent,
    NativeAudioAuthorParameterPhase as Phase, NativeAudioAuthorRefusal as Refusal,
    NativeAudioAuthorScalar, NativeAudioAuthorSourceFailure, NativeAudioAuthorSourceStatus,
};

pub(super) enum PendingDelivery {
    Delivered {
        binding: crate::NativeAudioObjectBindRequest,
        revision: u64,
        configuration: Option<NativeAudioAuthorConfiguration>,
    },
    Rejected {
        binding: crate::NativeAudioObjectBindRequest,
        revision: u64,
        reason: Refusal,
        // Retain the actual rejected batch until an explicit exact-owner
        // resynchronization. No successful prefix or synthetic End is emitted.
        events: Vec<vst3_host::plugin::NativeEditorEvent>,
    },
}

impl PendingDelivery {
    fn binding_revision(&self) -> (crate::NativeAudioObjectBindRequest, u64) {
        match self {
            Self::Delivered { binding, revision, .. }
            | Self::Rejected { binding, revision, .. } => (*binding, *revision),
        }
    }
    fn retained_bytes(&self) -> Option<usize> {
        match self {
            Self::Delivered { configuration, .. } => Some(
                configuration
                    .as_ref()
                    .map_or(0, NativeAudioAuthorConfiguration::retained_capacity),
            ),
            Self::Rejected { events, .. } => {
                events.capacity().checked_mul(size_of::<vst3_host::plugin::NativeEditorEvent>())
            }
        }
    }
}

pub(super) fn pending_projection(pending: Option<&PendingDelivery>) -> Delivery {
    match pending {
        Some(PendingDelivery::Delivered { binding, revision, .. }) => {
            Delivery::AwaitingAcknowledgement { binding: *binding, revision: *revision }
        }
        Some(PendingDelivery::Rejected { reason, .. }) => Delivery::Refused(*reason),
        None => Delivery::Preserved,
    }
}

pub(super) fn status(
    source: vst3_host::plugin::NativeEditorEventSourceStatus,
) -> Result<NativeAudioAuthorSourceStatus, AudioProcessorHostError> {
    use vst3_host::plugin::NativeEditorEventFailure as Failure;
    let count = |value: usize| {
        u32::try_from(value).map_err(|_| invalid("VST3 native author source extent"))
    };
    let result = NativeAudioAuthorSourceStatus {
        parameter_flush_pending: false,
        publishing: false,
        revision: source.revision(),
        pending_events: count(source.pending_events())?,
        pending_parameter_observations: 0,
        pending_dsp_feedback_values: count(source.pending_dsp_feedback_values())?,
        open_parameter_edits: count(source.open_parameter_edits())?,
        group_depth: source.group_depth(),
        failure: source.failure().map(|failure| match failure {
            Failure::WrongThread => NativeAudioAuthorSourceFailure::WrongThread,
            Failure::Overflow => NativeAudioAuthorSourceFailure::Overflow,
            Failure::InvalidValue => NativeAudioAuthorSourceFailure::InvalidValue,
            Failure::InvalidBoundary => NativeAudioAuthorSourceFailure::InvalidBoundary,
            Failure::SequenceExhausted => NativeAudioAuthorSourceFailure::SequenceExhausted,
        }),
    };
    result
        .validate()
        .map_err(|_| invalid("VST3 actual author source cannot fit its fixed extent"))?;
    Ok(result)
}

fn project(
    events: &[vst3_host::plugin::NativeEditorEvent],
    parameters: &[Vst3ParameterDescriptor],
) -> Result<Vec<Event>, Refusal> {
    use vst3_host::plugin::{NativeEditorEvent, ParameterEditKind};
    let mut projected = Vec::new();
    projected.try_reserve_exact(events.len()).map_err(|_| Refusal::ResourceLimit)?;
    for event in events {
        projected.push(match event {
            NativeEditorEvent::Parameter(edit) => {
                let descriptor = parameters
                    .iter()
                    .find(|parameter| parameter.id == edit.id)
                    .ok_or(Refusal::UnknownParameter)?;
                if descriptor.read_only {
                    return Err(Refusal::ReadOnlyParameter);
                }
                let id = descriptor.parameter_id().map_err(|_| Refusal::UnknownParameter)?;
                let phase = match edit.kind {
                    ParameterEditKind::BeginGesture => Phase::Begin,
                    ParameterEditKind::ValueChange => Phase::Value,
                    ParameterEditKind::EndGesture => Phase::End,
                };
                let value = edit
                    .value
                    .map(|value| {
                        descriptor
                            .author_scalar_from_normalized(value)
                            .map_err(|_| Refusal::InvalidValue)
                            .and_then(NativeAudioAuthorScalar::admit)
                    })
                    .transpose()?;
                Event::Parameter(NativeAudioAuthorParameterEvent::admit(id, phase, value)?)
            }
            // Initial service does not authorize an active rescan consumer.
            NativeEditorEvent::ParameterRescanRequested { .. } => {
                return Err(Refusal::UnsupportedAdapter);
            }
            NativeEditorEvent::DirtyChanged(value) => Event::DirtyChanged(*value),
            NativeEditorEvent::GroupEditStarted => Event::GroupStarted,
            NativeEditorEvent::GroupEditFinished => Event::GroupFinished,
        });
    }
    Ok(projected)
}

impl Vst3Worker {
    pub(super) fn author_binding_current(
        &self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<bool, AudioProcessorHostError> {
        Ok(self.native_binding.as_ref().is_some_and(|receipt| {
            receipt.binding() == binding.binding && receipt.object() == binding.expected_object
        }) && self.native_audio_object_observation()? == Some(binding.expected_object))
    }

    pub(super) fn observe_native_author(
        &mut self,
        consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<Option<NativeAudioAuthorObservation>, AudioProcessorHostError> {
        let Some(plugin) = self.plugin.as_ref() else {
            return Ok(None);
        };
        let mut source = status(plugin.editor_event_source_status().map_err(|error| {
            unavailable(format!(
                "VST3 native author source observation failed: {error}"
            ))
        })?)?;
        if let crate::NativeAudioObservationConsumption::AcknowledgeReenteredAuthor(ack) =
            consumption
        {
            return self.acknowledge_reentered_author(ack, source).map(Some);
        }
        if self.prepared_author_entry.as_ref().is_some_and(|entry| entry.awaiting_ack()) {
            return Ok(Some(NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Refused(Refusal::AwaitingAcceptance),
            }));
        }
        let Some(binding) = consumption.author_binding() else {
            let delivery = pending_projection(self.native_author_delivery.as_ref());
            return Ok(Some(NativeAudioAuthorObservation { source, delivery }));
        };
        if !self.author_binding_current(binding)? {
            return Ok(Some(NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Refused(Refusal::OwnerChanged),
            }));
        }
        if let crate::NativeAudioObservationConsumption::AcknowledgeAuthor(ack) = consumption {
            return self.acknowledge_native_author(ack, source).map(Some);
        }
        if let Some(pending) = &self.native_author_delivery {
            if pending.binding_revision().0 != binding {
                return Ok(Some(NativeAudioAuthorObservation {
                    source,
                    delivery: Delivery::Refused(Refusal::OwnerChanged),
                }));
            }
            if pending
                .retained_bytes()
                .is_none_or(|bytes| bytes > super::super::protocol::MAX_CONTROL_FRAME_BYTES)
            {
                return Ok(Some(NativeAudioAuthorObservation {
                    source,
                    delivery: Delivery::Refused(Refusal::ResourceLimit),
                }));
            }
            let delivery = match pending {
                PendingDelivery::Delivered { binding, revision, .. } => {
                    Delivery::AwaitingAcknowledgement { binding: *binding, revision: *revision }
                }
                PendingDelivery::Rejected { reason, .. } => Delivery::Refused(*reason),
            };
            return Ok(Some(NativeAudioAuthorObservation { source, delivery }));
        }
        if source.failure.is_some() {
            return Ok(Some(NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Refused(Refusal::InvalidSource),
            }));
        }
        if source.pending_dsp_feedback_values != 0 {
            return Ok(Some(NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Refused(Refusal::DspFeedbackPending),
            }));
        }
        if source.pending_events == 0 {
            return Ok(Some(NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Preserved,
            }));
        }
        let configuration = if source.boundaries_closed() {
            // The same DSP role has completed/joined. IComponent/getState is
            // allowed on main in Processing; this does not Stop/Deactivate or
            // create a private DSP checkpoint. Source revision must stay exact.
            let limit = crate::NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES;
            let grant = limit
                .checked_mul(3)
                .ok_or_else(|| invalid("VST3 author capture extent overflow"))?;
            let plugin = self
                .plugin
                .as_mut()
                .ok_or_else(|| unavailable("VST3 author native object closed"))?;
            match plugin.capture_author_state_bounded(limit, grant) {
                Ok(capture) => match NativeAudioAuthorConfiguration::admit(capture.into_bytes()) {
                    Ok(configuration) => Some(configuration),
                    Err(reason) => {
                        return Ok(Some(NativeAudioAuthorObservation {
                            source,
                            delivery: Delivery::Refused(reason),
                        }))
                    }
                },
                Err(error) => {
                    use vst3_host::configuration_capture::ConfigurationCaptureError as Error;
                    let reason = match error {
                        Error::InvalidLimit { .. }
                        | Error::BufferGrantExceeded { .. }
                        | Error::StateLimitExceeded { .. }
                        | Error::AllocationFailed
                        | Error::AllocationExtent { .. }
                        | Error::ExtentOverflow => Refusal::ResourceLimit,
                        _ => Refusal::ConfigurationUnavailable,
                    };
                    return Ok(Some(NativeAudioAuthorObservation {
                        source,
                        delivery: Delivery::Refused(reason),
                    }));
                }
            }
        } else {
            None
        };
        let plugin = self
            .plugin
            .as_mut()
            .ok_or_else(|| unavailable("VST3 author native object closed"))?;
        if configuration.is_some() {
            let coherent =
                plugin.editor_capture_source_status_since(source.revision).map_err(|error| {
                    unavailable(format!("VST3 author capture coherence failed: {error}"))
                })?;
            let Some(coherent) = coherent else {
                let changed = status(plugin.editor_event_source_status().map_err(|error| {
                    unavailable(format!(
                        "VST3 changed capture source observation failed: {error}"
                    ))
                })?)?;
                return Ok(Some(NativeAudioAuthorObservation {
                    source: changed,
                    delivery: Delivery::Refused(Refusal::SourceChanged),
                }));
            };
            // A legitimate getState dirty=false callback is part of the actual
            // appended source, not a reason to retry forever or erase notices.
            source = status(coherent)?;
        } else {
            let observed = status(plugin.editor_event_source_status().map_err(|error| {
                unavailable(format!("VST3 partial author source check failed: {error}"))
            })?)?;
            if observed != source {
                return Ok(Some(NativeAudioAuthorObservation {
                    source: observed,
                    delivery: Delivery::Refused(Refusal::SourceChanged),
                }));
            }
        }
        let events = match plugin
            .take_editor_events()
            .map_err(|error| unavailable(format!("VST3 author source drain failed: {error}")))?
        {
            vst3_host::plugin::NativeEditorEventDrain::Complete { events } => events,
            vst3_host::plugin::NativeEditorEventDrain::Incomplete { .. } => {
                return Ok(Some(NativeAudioAuthorObservation {
                    source: status(plugin.editor_event_source_status().map_err(|error| {
                        unavailable(format!("VST3 source refusal observation failed: {error}"))
                    })?)?,
                    delivery: Delivery::Refused(Refusal::InvalidSource),
                }))
            }
        };
        let after = status(plugin.editor_event_source_status().map_err(|error| {
            unavailable(format!(
                "VST3 drained author source observation failed: {error}"
            ))
        })?)?;
        let projected = match project(&events, &self.payload.parameters) {
            Ok(projected) => projected,
            Err(reason) => {
                self.native_author_delivery = Some(PendingDelivery::Rejected {
                    binding,
                    revision: source.revision,
                    reason,
                    events,
                });
                return Ok(Some(NativeAudioAuthorObservation {
                    source: after,
                    delivery: Delivery::Refused(reason),
                }));
            }
        };
        if after.revision != source.revision
            || after.failure.is_some()
            || after.pending_dsp_feedback_values != 0
        {
            let reason = Refusal::SourceChanged;
            self.native_author_delivery = Some(PendingDelivery::Rejected {
                binding,
                revision: source.revision,
                reason,
                events,
            });
            return Ok(Some(NativeAudioAuthorObservation {
                source: after,
                delivery: Delivery::Refused(reason),
            }));
        }
        let delivery = NativeAudioAuthorDelivery {
            binding,
            revision: source.revision,
            events: projected,
            completion: if configuration.is_some() {
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
            } else {
                mondrian_core::NativeAudioAuthorCompletion::Nonterminal
            },
            configuration: configuration.clone(),
        };
        if let Err(reason) = delivery.validate(after) {
            self.native_author_delivery = Some(PendingDelivery::Rejected {
                binding,
                revision: source.revision,
                reason,
                events,
            });
            return Ok(Some(NativeAudioAuthorObservation {
                source: after,
                delivery: Delivery::Refused(reason),
            }));
        }
        self.native_author_delivery =
            Some(PendingDelivery::Delivered { binding, revision: source.revision, configuration });
        Ok(Some(NativeAudioAuthorObservation {
            source: after,
            delivery: Delivery::Delivered(delivery),
        }))
    }

    fn acknowledge_native_author(
        &mut self,
        ack: NativeAudioAuthorAcknowledgement,
        source: NativeAudioAuthorSourceStatus,
    ) -> Result<NativeAudioAuthorObservation, AudioProcessorHostError> {
        if ack.source_binding.expected_object != ack.installed_binding.expected_object
            || !self.author_binding_current(ack.installed_binding)?
            || !matches!(&self.native_author_delivery,Some(PendingDelivery::Delivered{binding,revision,..}) if *binding==ack.source_binding && *revision==ack.revision)
        {
            return Ok(NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Refused(Refusal::StaleDelivery),
            });
        }
        let Some(PendingDelivery::Delivered { configuration, .. }) =
            self.native_author_delivery.take()
        else {
            return Err(invalid(
                "VST3 native author acknowledgement lost its checked receipt",
            ));
        };
        if let Some(configuration) = configuration {
            // Exactly accepted actual bytes seed future native Enter; no SDK
            // call, state recapture or author change occurs at this receipt.
            self.payload.state = Some(configuration.into_bytes());
        }
        Ok(NativeAudioAuthorObservation { source, delivery: Delivery::Acknowledged(ack) })
    }
}
