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
        request_cohort: Option<crate::NativeRequestCohort>,
        rescan_witness: Option<vst3_host::plugin::RestartSourceWitness>,
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
        Some(PendingDelivery::Delivered { binding, revision, request_cohort, .. }) => {
            Delivery::AwaitingAcknowledgement {
                binding: *binding,
                revision: *revision,
                request_cohort: *request_cohort,
            }
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
                PendingDelivery::Delivered { binding, revision, request_cohort, .. } => {
                    Delivery::AwaitingAcknowledgement {
                        binding: *binding,
                        revision: *revision,
                        request_cohort: *request_cohort,
                    }
                }
                PendingDelivery::Rejected { reason, .. } => Delivery::Refused(*reason),
            };
            return Ok(Some(NativeAudioAuthorObservation { source, delivery }));
        }
        // Existing Source obligations refuse before any deferred native setter.
        // The same gates below are repeated after real controller service.
        if source.failure.is_some() {
            return Ok(Some(refused(source, Refusal::InvalidSource)));
        }
        if source.pending_dsp_feedback_values != 0 {
            return Ok(Some(refused(source, Refusal::DspFeedbackPending)));
        }
        // Only the actual joined/full owner services its deferred controller.
        // Preserve/awaiting-Ack paths above never manufacture a second delivery.
        let plugin = self.plugin.as_mut().ok_or_else(|| unavailable("VST3 object closed"))?;
        if plugin.pending_processor_parameter_input().map_err(|error| {
            unavailable(format!("VST3 author input observation failed: {error}"))
        })? {
            return Ok(Some(refused(source, Refusal::DspFeedbackPending)));
        }
        plugin
            .borrow_editor_control()
            .map_err(|error| unavailable(format!("VST3 author controller borrow failed: {error}")))?
            .service_controller()
            .map_err(|error| {
                unavailable(format!("VST3 author controller service failed: {error}"))
            })?;
        source = current_source(plugin)?;
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
        // This short-lived peek classifies retained markers even if a public
        // raw consumer already retired their cohort. It is gone before query or
        // large state capture; it is not another persistent Source owner.
        let plugin = self.plugin.as_ref().ok_or_else(|| unavailable("VST3 object closed"))?;
        let cohort = plugin.pending_restart_cohort();
        let count = source.pending_events as usize;
        if count > crate::NATIVE_AUDIO_AUTHOR_EVENTS {
            return Ok(Some(refused(source, Refusal::ResourceLimit)));
        }
        let mut peek = Vec::new();
        if peek.try_reserve_exact(count).is_err()
            || peek.capacity() > crate::NATIVE_AUDIO_AUTHOR_EVENTS
        {
            return Ok(Some(refused(source, Refusal::ResourceLimit)));
        }
        let mut feedback = Vec::new();
        let observed = plugin.observe_editor_source_bounded(count, &mut peek, &mut feedback);
        if !matches!(observed, Ok(actual) if status(actual)? == source)
            || plugin.pending_restart_cohort() != cohort
        {
            return Ok(Some(refused(
                current_source(plugin)?,
                Refusal::SourceChanged,
            )));
        }
        let rescan = classify_rescan(&peek, source, cohort);
        drop(peek);
        drop(feedback);
        let rescan = match rescan {
            Ok(rescan) => rescan,
            Err(reason) => return Ok(Some(refused(source, reason))),
        };
        if let Some(cohort) = rescan {
            if self.initial_signal_phase != InitialSignalPhase::Entered
                || self.prepared_author_entry.is_some()
                || !matches!(
                    self.configuration_phase,
                    super::super::native_configuration_control::NativeConfigurationPhase::Admitted
                )
            {
                return Ok(Some(refused(source, Refusal::UnsupportedAdapter)));
            }
            return self.collect_single_rescan(binding, source, cohort).map(Some);
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
            request_cohort: None,
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
        self.native_author_delivery = Some(PendingDelivery::Delivered {
            binding,
            revision: source.revision,
            configuration,
            request_cohort: None,
            rescan_witness: None,
        });
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
        if let Some(PendingDelivery::Delivered {
            revision,
            configuration,
            request_cohort,
            rescan_witness: Some(witness),
            ..
        }) = self.native_author_delivery.as_mut()
        {
            let Some(plugin) = self.plugin.as_mut() else {
                return Ok(refused(source, Refusal::StaleDelivery));
            };
            let cohort = witness.cohort();
            let prefix = Some(crate::NativeRequestCohort {
                serial: cohort.serial(),
                request: mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
            });
            if configuration.is_none()
                || *revision != witness.source().revision()
                || *request_cohort != prefix
                || cohort.flags().bits() != 4
                || cohort.serial() == 0
                || cohort.exhausted()
                || plugin.native_editor_attachment_owner() != Some(witness.owner())
            {
                return Ok(refused(source, Refusal::StaleDelivery));
            }
            // The held final witness identifies prefix A, never a later live B.
            // SDK settlement neither services native code nor changes Source or
            // feedback; new open bounds/values remain later obligations.
            match plugin.settle_restart_source_prefix(*witness) {
                Ok(
                    vst3_host::plugin::RestartSourceSettlement::Retired
                    | vst3_host::plugin::RestartSourceSettlement::NewerParameterSuffixPending {
                        ..
                    },
                ) => {}
                Err(error) => return Ok(refused(source, rescan_settlement_refusal(error))),
            }
            // Every fallible check precedes settlement. Move the already admitted
            // configuration and retire only this pending receipt, without native
            // calls, recapture or another validation after the SDK commit.
            self.payload.state =
                configuration.take().map(NativeAudioAuthorConfiguration::into_bytes);
            self.native_author_delivery = None;
            return Ok(NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Acknowledged { acknowledgement: ack, request_cohort: prefix },
            });
        }
        let Some(PendingDelivery::Delivered { configuration, request_cohort, .. }) =
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
        Ok(NativeAudioAuthorObservation {
            source,
            delivery: Delivery::Acknowledged { acknowledgement: ack, request_cohort },
        })
    }
}

fn refused(source: NativeAudioAuthorSourceStatus, reason: Refusal) -> NativeAudioAuthorObservation {
    NativeAudioAuthorObservation { source, delivery: Delivery::Refused(reason) }
}

fn rescan_settlement_refusal(error: vst3_host::plugin::RestartSourceError) -> Refusal {
    use vst3_host::plugin::RestartSourceError as Error;
    match error {
        Error::Unsupported | Error::WrongThread => Refusal::UnsupportedAdapter,
        Error::InvalidSource | Error::PoisonedOwner => Refusal::InvalidSource,
        Error::Busy => Refusal::SourceChanged,
        Error::Conflict | Error::Exhausted => Refusal::StaleDelivery,
    }
}

fn current_source(
    plugin: &Plugin,
) -> Result<NativeAudioAuthorSourceStatus, AudioProcessorHostError> {
    status(
        plugin.editor_event_source_status().map_err(|error| {
            unavailable(format!("VST3 author source observation failed: {error}"))
        })?,
    )
}

fn classify_rescan(
    events: &[vst3_host::plugin::NativeEditorEvent],
    source: NativeAudioAuthorSourceStatus,
    current: Option<vst3_host::plugin::RestartCohort>,
) -> Result<Option<vst3_host::plugin::RestartCohort>, Refusal> {
    use vst3_host::plugin::NativeEditorEvent;
    let has_marker = events
        .iter()
        .any(|event| matches!(event, NativeEditorEvent::ParameterRescanRequested { .. }));
    let parameter_request = current.is_some_and(|cohort| cohort.flags().bits() & 20 != 0);
    if !has_marker && !parameter_request {
        return Ok(None);
    }
    let [NativeEditorEvent::ParameterRescanRequested { flags: 4, cohort }] = events else {
        return Err(Refusal::UnsupportedAdapter);
    };
    if !source.boundaries_closed()
        || source.failure.is_some()
        || source.pending_dsp_feedback_values != 0
        || source.pending_events != 1
        || cohort.flags().bits() != 4
        || cohort.serial() == 0
        || cohort.exhausted()
        || current != Some(*cohort)
    {
        return Err(Refusal::UnsupportedAdapter);
    }
    Ok(Some(*cohort))
}

impl Vst3Worker {
    fn collect_single_rescan(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
        baseline: NativeAudioAuthorSourceStatus,
        cohort: vst3_host::plugin::RestartCohort,
    ) -> Result<NativeAudioAuthorObservation, AudioProcessorHostError> {
        use vst3_host::plugin::NativeEditorEventDrain;
        let plugin = self.plugin.as_mut().ok_or_else(|| unavailable("VST3 object closed"))?;
        let parameters = &self.payload.parameters;
        let writable = parameters.iter().filter(|parameter| !parameter.read_only).count();
        if parameters.len() > MAX_PARAMETERS {
            return Ok(refused(baseline, Refusal::ResourceLimit));
        }
        let mut values = Vec::new();
        if values.try_reserve_exact(writable).is_err() || values.capacity() > MAX_PARAMETERS {
            return Ok(refused(baseline, Refusal::ResourceLimit));
        }
        let mut frame = vst3_host::controller_parameters::ControllerParameterQueryFrame::default();
        let mut projection_refusal = None;
        let query = super::initial_parameter_values::visit_admitted_controller_values(
            plugin,
            parameters,
            &mut frame,
            &mut |index, normalized| {
                let descriptor = &parameters[index];
                if descriptor.read_only {
                    return Ok(());
                }
                let projected = (|| {
                    let id = descriptor.parameter_id().map_err(|_| Refusal::UnknownParameter)?;
                    let value = descriptor
                        .author_scalar_from_normalized(normalized)
                        .map_err(|_| Refusal::InvalidValue)?;
                    NativeAudioAuthorParameterEvent::admit(
                        id,
                        Phase::Value,
                        Some(NativeAudioAuthorScalar::admit(value)?),
                    )
                })();
                match projected {
                    Ok(value) => {
                        values.push(value);
                        Ok(())
                    }
                    Err(reason) => {
                        projection_refusal = Some(reason);
                        Err(vst3_host::Error::InvalidParameter(
                            "VST3 author value refused".to_owned(),
                        ))
                    }
                }
            },
        );
        let queried = current_source(plugin)?;
        if query.is_err() || queried != baseline || plugin.pending_restart_cohort() != Some(cohort)
        {
            return Ok(refused(
                queried,
                projection_refusal.unwrap_or(Refusal::SourceChanged),
            ));
        }
        let limit = crate::NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES;
        let grant = limit.checked_mul(3).ok_or_else(|| invalid("VST3 capture extent overflow"))?;
        let configuration = match plugin.capture_author_state_bounded(limit, grant) {
            Ok(capture) => match NativeAudioAuthorConfiguration::admit(capture.into_bytes()) {
                Ok(configuration) => configuration,
                Err(reason) => return Ok(refused(current_source(plugin)?, reason)),
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
                return Ok(refused(current_source(plugin)?, reason));
            }
        };
        let coherent =
            plugin.editor_capture_source_status_since(baseline.revision).map_err(|error| {
                unavailable(format!("VST3 rescan capture coherence failed: {error}"))
            })?;
        let Some(coherent) = coherent else {
            return Ok(refused(current_source(plugin)?, Refusal::SourceChanged));
        };
        // Only this fresh post-getState witness may label the delivered prefix.
        // Legitimate Dirty(false) callbacks remain in its exact revision/order.
        let witness = match plugin.capture_restart_source_witness() {
            Ok(witness) if witness.source() == coherent && witness.cohort() == cohort => witness,
            _ => return Ok(refused(current_source(plugin)?, Refusal::SourceChanged)),
        };
        let source = status(coherent)?;
        let count = source.pending_events as usize;
        let Some(dirty_count) = source.revision.checked_sub(baseline.revision) else {
            return Ok(refused(source, Refusal::SourceChanged));
        };
        if count > crate::NATIVE_AUDIO_AUTHOR_EVENTS
            || usize::try_from(dirty_count).ok().and_then(|n| n.checked_add(1)) != Some(count)
        {
            return Ok(refused(source, Refusal::SourceChanged));
        }
        let mut projected = Vec::new();
        if projected.try_reserve_exact(count).is_err()
            || projected.capacity() > crate::NATIVE_AUDIO_AUTHOR_EVENTS
        {
            return Ok(refused(source, Refusal::ResourceLimit));
        }
        projected.push(Event::ParameterValuesRescanned {
            kind: mondrian_core::NativeAudioParameterRescanKind::Values,
            values,
        });
        projected.extend((1..count).map(|_| Event::DirtyChanged(false)));
        let request_cohort = Some(crate::NativeRequestCohort {
            serial: witness.cohort().serial(),
            request: mondrian_core::NativeAudioReprepareRequest::Vst3Restart {
                flags: witness.cohort().flags().bits() as u32,
            },
        });
        let delivery = NativeAudioAuthorDelivery {
            binding,
            revision: source.revision,
            request_cohort,
            events: projected,
            configuration: Some(configuration.clone()),
            completion: mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
        };
        if let Err(reason) = delivery.validate(source) {
            return Ok(refused(source, reason));
        }
        // No native call follows the final witness before this direct drain.
        // Plugin::take_editor_events would implicitly service caches/setters.
        let drained = plugin
            .borrow_editor_control()
            .map_err(|error| unavailable(format!("VST3 rescan drain borrow failed: {error}")))?
            .take_editor_events()
            .map_err(|error| unavailable(format!("VST3 rescan direct drain failed: {error}")))?;
        let events = match drained {
            NativeEditorEventDrain::Complete { events } => events,
            NativeEditorEventDrain::Incomplete { .. } => {
                return Ok(refused(current_source(plugin)?, Refusal::InvalidSource));
            }
        };
        let after = current_source(plugin);
        let exact = captured_rescan_prefix(&events, count, cohort)
            && plugin.pending_restart_cohort() == Some(cohort);
        let reason = match &after {
            Ok(after)
                if exact
                    && after.revision == source.revision
                    && after.closed_empty()
                    && after.pending_dsp_feedback_values == 0 =>
            {
                None
            }
            _ => Some(Refusal::SourceChanged),
        };
        if let Some(reason) = reason {
            // Unexpected SDK/racing loss after drain retains the original batch.
            self.native_author_delivery = Some(PendingDelivery::Rejected {
                binding,
                revision: source.revision,
                reason,
                events,
            });
            return Ok(refused(after?, reason));
        }
        self.native_author_delivery = Some(PendingDelivery::Delivered {
            binding,
            revision: source.revision,
            configuration: Some(configuration),
            request_cohort,
            rescan_witness: Some(witness),
        });
        Ok(NativeAudioAuthorObservation {
            source: after?,
            delivery: Delivery::Delivered(delivery),
        })
    }
}

fn captured_rescan_prefix(
    events: &[vst3_host::plugin::NativeEditorEvent],
    count: usize,
    cohort: vst3_host::plugin::RestartCohort,
) -> bool {
    use vst3_host::plugin::NativeEditorEvent;
    count != 0 && events.len() == count && events.iter().enumerate().all(|(index, event)| {
        if index == 0 {
            matches!(event, NativeEditorEvent::ParameterRescanRequested { flags: 4, cohort: actual }
                if *actual == cohort)
        } else {
            matches!(event, NativeEditorEvent::DirtyChanged(false))
        }
    })
}

#[cfg(test)]
mod rescan_tests {
    use super::*;
    use vst3_host::plugin::{NativeEditorEvent as Raw, RestartCohort};

    fn cohort(serial: u32, flags: u32) -> RestartCohort {
        serde_json::from_value(serde_json::json!(
            (u64::from(serial) << 32) | u64::from(flags)
        ))
        .unwrap()
    }
    fn source(count: usize) -> NativeAudioAuthorSourceStatus {
        NativeAudioAuthorSourceStatus {
            parameter_flush_pending: false,
            publishing: false,
            revision: 7,
            pending_events: count as u32,
            pending_parameter_observations: 0,
            pending_dsp_feedback_values: 0,
            open_parameter_edits: 0,
            group_depth: 0,
            failure: None,
        }
    }
    fn marker(cohort: RestartCohort) -> Raw {
        Raw::ParameterRescanRequested { flags: cohort.flags().bits(), cohort }
    }

    #[test]
    fn single_rescan_requires_one_current_exact4_marker_before_drain() {
        let exact = cohort(3, 4);
        assert_eq!(
            classify_rescan(&[marker(exact)], source(1), Some(exact)),
            Ok(Some(exact))
        );
        for (events, live) in [
            (
                vec![marker(exact), marker(cohort(4, 4))],
                Some(cohort(4, 4)),
            ),
            (vec![marker(exact), Raw::GroupEditStarted], Some(exact)),
            (vec![marker(exact), Raw::DirtyChanged(false)], Some(exact)),
            (vec![marker(cohort(3, 16))], Some(cohort(3, 16))),
            (vec![marker(cohort(3, 20))], Some(cohort(3, 20))),
            (vec![marker(cohort(3, 36))], Some(cohort(3, 36))),
            (vec![marker(exact)], Some(cohort(3, 0))),
            (vec![marker(exact)], None),
            (vec![marker(exact)], Some(cohort(4, 4))),
            (vec![marker(cohort(u32::MAX, 4))], Some(cohort(u32::MAX, 4))),
            (vec![marker(cohort(0, 4))], Some(cohort(0, 4))),
            (vec![], Some(exact)),
        ] {
            assert_eq!(
                classify_rescan(&events, source(events.len()), live),
                Err(Refusal::UnsupportedAdapter)
            );
        }
        let mut open = source(1);
        open.open_parameter_edits = 1;
        assert!(classify_rescan(&[marker(exact)], open, Some(exact)).is_err());
        open = source(1);
        open.pending_dsp_feedback_values = 1;
        assert!(classify_rescan(&[marker(exact)], open, Some(exact)).is_err());
    }

    #[test]
    fn single_rescan_classification_preserves_the_nonrescan_ui_route() {
        let events = [
            Raw::GroupEditStarted,
            Raw::GroupEditFinished,
            Raw::DirtyChanged(true),
        ];
        for live in [None, Some(cohort(4, 0)), Some(cohort(5, 8))] {
            assert_eq!(
                classify_rescan(&events, source(events.len()), live),
                Ok(None)
            );
        }
    }

    #[test]
    fn captured_rescan_prefix_accepts_only_the_original_marker_and_dirty_false_suffix() {
        let exact = cohort(3, 4);
        let complete = [
            marker(exact),
            Raw::DirtyChanged(false),
            Raw::DirtyChanged(false),
        ];
        assert!(captured_rescan_prefix(&complete, 3, exact));
        assert!(!captured_rescan_prefix(&complete, 2, exact));
        assert!(!captured_rescan_prefix(&complete, 3, cohort(4, 4)));
        assert!(!captured_rescan_prefix(&[], 0, exact));
        for suffix in [
            Raw::DirtyChanged(true),
            marker(cohort(4, 4)),
            Raw::GroupEditStarted,
        ] {
            assert!(!captured_rescan_prefix(&[marker(exact), suffix], 2, exact));
        }
    }
}
