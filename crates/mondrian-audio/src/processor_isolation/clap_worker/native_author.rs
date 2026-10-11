//! Same-object CLAP author delivery over the one existing Poll9 response.
//! Main callbacks and actual output events keep their original two owners until
//! one ordered prefix has passed canonical Session/Plan acceptance and exact Ack.

use super::super::clap_output_events::{ClapOutputEvents, ClapOutputParameterKind};
use super::super::clap_parameter_prefix::{ClapParameterRecord, PreparedClapParameterPrefix};
use super::super::clap_parameter_source::{ClapParameterControlKind, ClapParameterSourceSnapshot};
use super::*;
use crate::{
    NativeAudioAuthorAcknowledgement, NativeAudioAuthorConfiguration, NativeAudioAuthorDelivery,
    NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorEvent as Event,
    NativeAudioAuthorObservation, NativeAudioAuthorParameterEvent,
    NativeAudioAuthorParameterPhase as Phase, NativeAudioAuthorRefusal as Refusal,
    NativeAudioAuthorScalar, NativeAudioAuthorSourceFailure, NativeAudioAuthorSourceStatus,
};

pub(super) struct PendingDelivery {
    binding: crate::NativeAudioObjectBindRequest,
    prefix: PreparedClapParameterPrefix,
    configuration: Option<NativeAudioAuthorConfiguration>,
}

impl PendingDelivery {
    pub(super) fn projection(&self) -> Delivery {
        Delivery::AwaitingAcknowledgement {
            binding: self.binding,
            revision: self.prefix.key.revision,
        }
    }
    pub(super) fn retained_bytes(&self) -> Result<usize, Refusal> {
        self.prefix
            .allocated_bytes()?
            .checked_add(
                self.configuration
                    .as_ref()
                    .map_or(0, NativeAudioAuthorConfiguration::retained_capacity),
            )
            .ok_or(Refusal::ResourceLimit)
    }
}

pub(super) fn source_status(
    actual: ClapParameterSourceSnapshot,
    flush_requested: bool,
) -> NativeAudioAuthorSourceStatus {
    use super::super::clap_parameter_source::ClapParameterSourceFailure as Failure;
    // All void source loss remains available in the raw parameter snapshot.
    // The generic author view reports an explicit conservative refusal; it does
    // not turn unknown flags, invalid lifecycle or contention into ClosedEmpty.
    let (pending_observations, invalid_counts) =
        match actual.pending_output.checked_sub(actual.pending_author_output) {
            Some(count) => (count, false),
            None => (0, true),
        };
    let failure = if actual.incomplete == 0 && !invalid_counts {
        None
    } else if actual.incomplete & (1 << Failure::WrongThread as u8) != 0 {
        Some(NativeAudioAuthorSourceFailure::WrongThread)
    } else if actual.incomplete & (1 << Failure::SequenceExhausted as u8) != 0 {
        Some(NativeAudioAuthorSourceFailure::SequenceExhausted)
    } else if actual.incomplete & (1 << Failure::ControlCapacity as u8) != 0 {
        Some(NativeAudioAuthorSourceFailure::Overflow)
    } else if actual.incomplete & (1 << Failure::OrderContention as u8) != 0 {
        Some(NativeAudioAuthorSourceFailure::Contention)
    } else {
        Some(NativeAudioAuthorSourceFailure::InvalidSource)
    };
    NativeAudioAuthorSourceStatus {
        revision: actual.revision,
        pending_events: actual.pending_author_output + actual.pending_controls,
        pending_parameter_observations: pending_observations,
        pending_dsp_feedback_values: 0,
        open_parameter_edits: actual.open_parameter_gestures,
        group_depth: 0,
        failure,
        parameter_flush_pending: flush_requested,
        publishing: actual.publishing_output,
    }
}

fn project(
    prefix: &PreparedClapParameterPrefix,
    parameters: &[super::super::ClapParameterDescriptor],
    facts: &[mondrian_core::NativeAudioParameterFact],
) -> Result<Vec<Event>, Refusal> {
    use clack_extensions::params::ParamClearFlags;
    let mut events = Vec::new();
    events
        .try_reserve_exact(prefix.records.len())
        .map_err(|_| Refusal::ResourceLimit)?;
    if events.capacity() > super::super::clap_parameter_prefix::CLAP_PARAMETER_PREFIX_EVENTS {
        return Err(Refusal::ResourceLimit);
    }
    for record in &prefix.records {
        let event = match *record {
            ClapParameterRecord::Output(output) if ClapOutputEvents::recordable(&output) => {
                let descriptor = parameters
                    .iter()
                    .find(|parameter| parameter.id == output.parameter)
                    .ok_or(Refusal::UnknownParameter)?;
                if descriptor.read_only {
                    return Err(Refusal::ReadOnlyParameter);
                }
                let id = descriptor.parameter_id().map_err(|_| Refusal::UnknownParameter)?;
                let (phase, value) = match output.kind {
                    ClapOutputParameterKind::Value(value) => {
                        (Phase::Value, Some(NativeAudioAuthorScalar::admit(value)?))
                    }
                    ClapOutputParameterKind::GestureBegin => (Phase::Begin, None),
                    ClapOutputParameterKind::GestureEnd => (Phase::End, None),
                };
                let parameter = NativeAudioAuthorParameterEvent::admit(id, phase, value)?;
                match output.origin {
                    super::super::clap_output_events::ClapOutputOrigin::Process {
                        start_sample,
                        sample_rate,
                        ..
                    } => Event::ParameterAtExecution {
                        parameter,
                        sample_position: start_sample
                            .checked_add(i64::from(output.sample_offset))
                            .ok_or(Refusal::InvalidValue)?,
                        sample_rate,
                    },
                    _ => Event::Parameter(parameter),
                }
            }
            ClapParameterRecord::Output(_) => continue,
            ClapParameterRecord::Control(control) => match control.kind {
                ClapParameterControlKind::StateDirty => Event::DirtyChanged(true),
                ClapParameterControlKind::Clear { parameter, flags } => {
                    if flags & !ParamClearFlags::all().bits() != 0 {
                        return Err(Refusal::InvalidSource);
                    }
                    // CLEAR is expressly legal for an ID already removed from
                    // the plugin. Translate the original ID, never an ordinal
                    // or another descriptor which happens to reuse that slot.
                    let parameter =
                        mondrian_core::ParameterId::new(format!("clap.param.{parameter}"))
                            .map_err(|_| Refusal::UnknownParameter)?;
                    Event::ParameterReferencesCleared { parameter, flags }
                }
                ClapParameterControlKind::Rescan { flags } => {
                    if flags & !15 != 0 {
                        return Err(Refusal::InvalidSource);
                    }
                    let mut values = Vec::new();
                    if flags & 9 != 0 {
                        values
                            .try_reserve_exact(parameters.len())
                            .map_err(|_| Refusal::ResourceLimit)?;
                        if values.capacity() > 1024 {
                            return Err(Refusal::ResourceLimit);
                        }
                        for parameter in parameters.iter().filter(|parameter| !parameter.read_only)
                        {
                            let id =
                                parameter.parameter_id().map_err(|_| Refusal::UnknownParameter)?;
                            let value = facts.iter().rev().find(|fact| fact.parameter == id
                                && matches!(fact.origin, mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { .. }))
                                .and_then(|fact| fact.value_bits).ok_or(Refusal::InvalidValue)?;
                            values.push(NativeAudioAuthorParameterEvent::admit(
                                id,
                                Phase::Value,
                                Some(NativeAudioAuthorScalar::from_bits(value)?),
                            )?);
                        }
                    }
                    Event::ParameterValuesRescanned { flags, values }
                }
            },
        };
        events.push(event);
    }
    Ok(events)
}

impl ClapWorker {
    fn author_source_status(&self) -> NativeAudioAuthorSourceStatus {
        source_status(
            self.shared.parameter_source.snapshot(),
            self.instance.access_shared_handler(|host| {
                host.services.flush_requested.load(Ordering::Acquire)
            }),
        )
    }

    fn author_binding_current(&self, binding: crate::NativeAudioObjectBindRequest) -> bool {
        self.native_binding.is_some_and(|receipt| {
            receipt.binding() == binding.binding
                && receipt.object() == binding.expected_object
                && binding.expected_object == self.native_object
        })
    }

    fn capture_clap_author_configuration(
        &mut self,
    ) -> Result<Option<NativeAudioAuthorConfiguration>, Refusal> {
        use super::super::native_configuration_control::FixedConfigurationWriter;
        let handle = self.instance.plugin_handle();
        let Some(extension) = handle.get_extension::<PluginState>() else {
            return Ok(None);
        };
        let limit = crate::NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES;
        let mut writer =
            FixedConfigurationWriter::new(limit, limit).map_err(|_| Refusal::ResourceLimit)?;
        // The full owner is on the actual init/main thread after DSP join. Save
        // does not Stop/deactivate, reload, reset or pretend to checkpoint DSP.
        extension
            .save(&handle, &mut writer)
            .map_err(|_| Refusal::ConfigurationUnavailable)?;
        NativeAudioAuthorConfiguration::admit(writer.finish().map_err(|_| Refusal::ResourceLimit)?)
            .map(Some)
    }

    pub(super) fn observe_native_author(
        &mut self,
        consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<
        (
            NativeAudioAuthorObservation,
            u64,
            Vec<mondrian_core::NativeAudioParameterFact>,
        ),
        AudioProcessorHostError,
    > {
        let source = self.author_source_status();
        let mut parameters = Vec::new();
        let mut parameter_revision = source.revision();
        let observation = (|| -> Result<NativeAudioAuthorObservation, AudioProcessorHostError> {
            if matches!(
                consumption,
                crate::NativeAudioObservationConsumption::AcknowledgeReenteredAuthor(_)
            ) {
                return Ok(NativeAudioAuthorObservation {
                    source,
                    delivery: Delivery::Refused(Refusal::StaleDelivery),
                });
            }
            let Some(binding) = consumption.author_binding() else {
                return Ok(NativeAudioAuthorObservation {
                    source,
                    delivery: self
                        .native_author_delivery
                        .as_ref()
                        .map_or(Delivery::Preserved, PendingDelivery::projection),
                });
            };
            if !self.author_binding_current(binding) {
                return Ok(NativeAudioAuthorObservation {
                    source,
                    delivery: Delivery::Refused(Refusal::OwnerChanged),
                });
            }
            if let crate::NativeAudioObservationConsumption::AcknowledgeAuthor(ack) = consumption {
                return Ok(self.acknowledge_clap_author(ack));
            }
            if let Some(pending) = &self.native_author_delivery {
                return Ok(NativeAudioAuthorObservation {
                    source,
                    delivery: if pending.binding == binding {
                        pending.projection()
                    } else {
                        Delivery::Refused(Refusal::OwnerChanged)
                    },
                });
            }
            if source.failure().is_some() || source.publishing() {
                return Ok(NativeAudioAuthorObservation {
                    source,
                    delivery: Delivery::Refused(Refusal::InvalidSource),
                });
            }
            if source.parameter_flush_pending() {
                // A scheduled flush is not a DSP-feedback value or a delivery.
                // Preserve every callback until the actual exclusive audio slot
                // flushes; the independent status bit keeps ClosedEmpty false.
                return Ok(NativeAudioAuthorObservation { source, delivery: Delivery::Preserved });
            }
            let prepared = self.instance.access_handler(|main| {
                main.prepare_parameter_prefix(
                    Arc::clone(&self.shared.parameter_source),
                    &self.output_events,
                )
            });
            let prefix = match prepared {
                Ok(Some(prefix)) => prefix,
                Ok(None) => {
                    parameters = match self.prepare_parameter_observations(None) {
                        Ok(parameters) => parameters,
                        Err(_) => {
                            return Ok(NativeAudioAuthorObservation {
                                source,
                                delivery: Delivery::Refused(Refusal::SourceChanged),
                            })
                        }
                    };
                    return Ok(NativeAudioAuthorObservation {
                        source,
                        delivery: Delivery::Preserved,
                    });
                }
                Err(reason) => {
                    return Ok(NativeAudioAuthorObservation {
                        source,
                        delivery: Delivery::Refused(reason),
                    })
                }
            };
            // The same raw prefix also owns readonly/DONT_RECORD/process-origin
            // and metadata records. The sole parameter observation projector must
            // admit these before this prefix can be retired or delivered.
            parameters = match self.prepare_parameter_observations(Some(&prefix)) {
                Ok(parameters) => parameters,
                Err(_) => {
                    return Ok(NativeAudioAuthorObservation {
                        source,
                        delivery: Delivery::Refused(Refusal::SourceChanged),
                    })
                }
            };
            parameter_revision = self.author_source_status().revision();
            let events = match project(&prefix, &self.expected_parameters, &parameters) {
                Ok(events) => events,
                Err(reason) => {
                    return Ok(NativeAudioAuthorObservation {
                        source,
                        delivery: Delivery::Refused(reason),
                    })
                }
            };
            if events.is_empty() {
                let retired = self.instance.access_handler(|main| {
                    main.acknowledge_prepared_parameter_prefix(prefix, &mut self.output_events)
                });
                return Ok(NativeAudioAuthorObservation {
                    source: self.author_source_status(),
                    delivery: if retired.is_ok() {
                        Delivery::Preserved
                    } else {
                        Delivery::Refused(Refusal::SourceChanged)
                    },
                });
            }
            let mut configuration = None;
            let mut completion = mondrian_core::NativeAudioAuthorCompletion::Nonterminal;
            if self.instance.plugin_handle().get_extension::<PluginState>().is_none() {
                // Clear affects host references, and nonstructural Rescan supplies
                // actual metadata/current values. Neither requires plugin State.
                // Private dirty state cannot be persisted by parameter curves.
                if events.iter().any(|event| matches!(event, Event::DirtyChanged(_))) {
                    return Ok(NativeAudioAuthorObservation {
                        source,
                        delivery: Delivery::Refused(Refusal::ConfigurationUnavailable),
                    });
                }
                if events.iter().any(|event| matches!(event, Event::ParameterValuesRescanned { flags, .. } if flags & 8 != 0)) {
                    // ALL is a schema/state-entry transaction, independent of the
                    // optional State capability. This parameter-only path retains
                    // it for the actual dynamic candidate consumer.
                    return Ok(NativeAudioAuthorObservation { source, delivery: Delivery::Refused(Refusal::SourceChanged) });
                }
            }
            if prefix.covers_current_source && source.boundaries_closed() {
                configuration = match self.capture_clap_author_configuration() {
                    Ok(configuration) => configuration,
                    Err(reason) => {
                        return Ok(NativeAudioAuthorObservation {
                            source,
                            delivery: Delivery::Refused(reason),
                        })
                    }
                };
                completion = if configuration.is_some() {
                    mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
                } else {
                    mondrian_core::NativeAudioAuthorCompletion::ParametersOnly
                };
                if self.author_source_status() != source {
                    return Ok(NativeAudioAuthorObservation {
                        source: self.author_source_status(),
                        delivery: Delivery::Refused(Refusal::SourceChanged),
                    });
                }
            }
            let returned_configuration = configuration
                .as_ref()
                .map(|state| NativeAudioAuthorConfiguration::admit(state.bytes().to_vec()))
                .transpose();
            let returned_configuration = match returned_configuration {
                Ok(configuration) => configuration,
                Err(reason) => {
                    return Ok(NativeAudioAuthorObservation {
                        source,
                        delivery: Delivery::Refused(reason),
                    })
                }
            };
            let delivery = NativeAudioAuthorDelivery {
                binding,
                revision: prefix.key.revision,
                events,
                configuration: returned_configuration,
                completion,
            };
            if let Err(reason) = delivery.validate(source) {
                return Ok(NativeAudioAuthorObservation {
                    source,
                    delivery: Delivery::Refused(reason),
                });
            }
            let pending = PendingDelivery { binding, prefix, configuration };
            if pending.retained_bytes().map_or(true, |bytes| {
                bytes > super::super::protocol::MAX_CONTROL_FRAME_BYTES
            }) {
                return Ok(NativeAudioAuthorObservation {
                    source,
                    delivery: Delivery::Refused(Refusal::ResourceLimit),
                });
            }
            self.native_author_delivery = Some(pending);
            Ok(NativeAudioAuthorObservation { source, delivery: Delivery::Delivered(delivery) })
        })()?;
        Ok((observation, parameter_revision, parameters))
    }

    fn acknowledge_clap_author(
        &mut self,
        ack: NativeAudioAuthorAcknowledgement,
    ) -> NativeAudioAuthorObservation {
        let source = self.author_source_status();
        if ack.source_binding.expected_object != ack.installed_binding.expected_object
            || !self.author_binding_current(ack.installed_binding)
            || !self.native_author_delivery.as_ref().is_some_and(|pending| {
                pending.binding == ack.source_binding && pending.prefix.key.revision == ack.revision
            })
        {
            return NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Refused(Refusal::StaleDelivery),
            };
        }
        let Some(pending) = self.native_author_delivery.take() else {
            return NativeAudioAuthorObservation {
                source,
                delivery: Delivery::Refused(Refusal::StaleDelivery),
            };
        };
        let PendingDelivery { binding, prefix, configuration } = pending;
        match self.instance.access_handler(|main| {
            main.acknowledge_prepared_parameter_prefix(prefix, &mut self.output_events)
        }) {
            Ok(()) => {
                if let Some(configuration) = configuration {
                    // Exactly accepted authored configuration for future fresh
                    // creation/Enter. No state load or private DSP replay here.
                    self.configuration_seed = Some(configuration.into_bytes());
                }
                NativeAudioAuthorObservation {
                    source: self.author_source_status(),
                    delivery: Delivery::Acknowledged(ack),
                }
            }
            Err(prefix) => {
                self.native_author_delivery =
                    Some(PendingDelivery { binding, prefix, configuration });
                NativeAudioAuthorObservation {
                    source: self.author_source_status(),
                    delivery: Delivery::Refused(Refusal::StaleDelivery),
                }
            }
        }
    }
}

#[cfg(test)]
mod observation_closure_contracts {
    use super::*;
    use clack_host::events::event_types::ParamValueEvent;
    use clack_host::events::EventFlags;
    use clack_host::prelude::{ClapId, Event, Pckn};

    pub(super) fn descriptor(read_only: bool) -> super::super::super::ClapParameterDescriptor {
        super::super::super::ClapParameterDescriptor {
            id: 17,
            name: "gain".into(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 0.25,
            current_value: 0.25,
            automatable: !read_only,
            stepped: false,
            enumeration: false,
            read_only,
            hidden: false,
        }
    }
    pub(super) fn push(output: &mut ClapOutputEvents, flags: EventFlags, value: f64) {
        output
            .writer(super::super::super::clap_output_events::ClapOutputOrigin::ActiveFlush)
            .expect("actual retained native output owner")
            .as_output()
            .try_push(
                ParamValueEvent::new(0, ClapId::new(17), Pckn::match_all(), value)
                    .with_flags(flags),
            )
            .expect("actual standard output callback accepts");
    }
    #[test]
    fn clap_raw_transient_output_is_not_a_closed_lifecycle_source() {
        for read_only in [false, true] {
            let order =
                Arc::new(super::super::super::clap_parameter_source::ClapParameterSource::new());
            let mut output = ClapOutputEvents::new(&[descriptor(read_only)], Arc::clone(&order))
                .expect("actual owner");
            let flags = if read_only {
                EventFlags::empty()
            } else {
                EventFlags::DONT_RECORD
            };
            push(&mut output, flags, 0.75);
            let raw = order.snapshot();
            assert_eq!(
                raw.pending_output, 1,
                "original callback really remains owned"
            );
            assert_eq!(
                raw.pending_author_output, 0,
                "readonly and DONT_RECORD are not author writes"
            );
            let generic = source_status(raw, false);
            assert_eq!(
                generic.pending_events(),
                0,
                "never manufacture an author callback count"
            );
            assert!(
                !generic.closed_empty(),
                "unacknowledged raw observations cannot grant lifecycle or reseed closure"
            );
            assert_eq!(
                output.events().len(),
                1,
                "nonconsuming closure query does not drop accepted output"
            );
        }
    }
}

#[cfg(test)]
mod observation_ack_contracts {
    use super::super::super::clap_parameter_source::ClapParameterControlSource;
    use super::observation_closure_contracts::{descriptor, push};
    use super::*;
    use clack_host::events::EventFlags;
    #[test]
    fn clap_transient_exact_prefix_ack_preserves_new_suffix_and_observation_count() {
        let order =
            Arc::new(super::super::super::clap_parameter_source::ClapParameterSource::new());
        let controls = ClapParameterControlSource::new(&order);
        let other_order =
            Arc::new(super::super::super::clap_parameter_source::ClapParameterSource::new());
        let other_controls = ClapParameterControlSource::new(&other_order);
        let mut output =
            ClapOutputEvents::new(&[descriptor(false)], Arc::clone(&order)).expect("actual owner");
        push(&mut output, EventFlags::DONT_RECORD, 0.75);
        let prefix = PreparedClapParameterPrefix::prepare(Arc::clone(&order), &output, &controls)
            .expect("source")
            .expect("prefix");
        let status = source_status(order.snapshot(), false);
        assert_eq!(
            (
                status.pending_events(),
                status.pending_parameter_observations()
            ),
            (0, 1)
        );
        assert_eq!(
            crate::native_audio_author_runtime::author_pcm_source(
                Some(&NativeAudioAuthorObservation {
                    source: status,
                    delivery: Delivery::Preserved,
                }),
                false
            ),
            Ok(()),
            "unrecorded meters do not manufacture a PCM author hold"
        );
        let prefix = match prefix.acknowledge(&mut output, &other_controls) {
            Err(prefix) => prefix,
            Ok(()) => panic!("different owner cannot retire accepted observation"),
        };
        push(&mut output, EventFlags::DONT_RECORD, 0.125);
        assert!(prefix.acknowledge(&mut output, &controls).is_ok());
        assert_eq!(output.events().len(), 1);
        assert_eq!(output.events()[0].sequence, 2);
        assert_eq!(
            source_status(order.snapshot(), false).pending_parameter_observations(),
            1
        );
        assert!(!source_status(order.snapshot(), false).closed_empty());
        let suffix = PreparedClapParameterPrefix::prepare(Arc::clone(&order), &output, &controls)
            .expect("source")
            .expect("suffix");
        assert!(suffix.acknowledge(&mut output, &controls).is_ok());
        let final_status = source_status(order.snapshot(), false);
        assert_eq!(final_status.revision(), 2);
        assert!(
            final_status.closed_empty(),
            "only actual same-owner prefix retirement clears observational source"
        );
    }
}
