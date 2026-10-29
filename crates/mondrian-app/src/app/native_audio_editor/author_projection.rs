//! Pure ordered callback projection shared by product author consumers.
//! Target/Session/native-source admission and publication stay with the caller.

use super::failure;
use mondrian_core::{
    automation::ParameterSchema, DomainTime, NativeAudioAuthorCallback as Callback,
    NativeAudioAuthorProposal, NativeAudioParameterCallbackPhase as Phase, ParameterId, Result,
};
use mondrian_editor_state::NativeAudioAuthorOwner;
use mondrian_timeline::{
    audio::AudioProcessorInstance, AudioAutomationTarget, NativeAudioAuthorUpdate,
    NativeAudioParameterWrite, NativeAudioParameterWriteMode, Sequence,
};
use std::collections::BTreeMap;

/// Project against the exact canonical processor already inspected by the caller.
/// GUI callers retain their original schema pins and actual author time. Without
/// GUI time, only unkeyed values, rescans and actual AtExecution times apply.
/// This does not mutate author state or authenticate a native prefix/target.
pub(super) fn project_update(
    sequence: &Sequence,
    owner: NativeAudioAuthorOwner,
    processor: &AudioProcessorInstance,
    pinned_schemas: Option<&BTreeMap<ParameterId, ParameterSchema>>,
    proposal: &NativeAudioAuthorProposal,
    gui_sequence_time: Option<DomainTime>,
) -> Result<NativeAudioAuthorUpdate> {
    let address = owner.rack_address();
    let occurrence = match owner {
        NativeAudioAuthorOwner::ProcessingScope { occurrence, .. } => Some(occurrence),
        NativeAudioAuthorOwner::ChannelStrip { .. } => None,
    };
    let schema_for = |id: &ParameterId| match pinned_schemas {
        Some(schemas) => schemas.get(id),
        None => processor.parameters.get(id).map(|parameter| &parameter.schema),
    };
    let mut parameters = Vec::with_capacity(proposal.callbacks().len());
    let mut references = processor
        .parameters
        .iter()
        .map(|(id, parameter)| {
            (
                id.clone(),
                parameter.automation.as_ref().map(|curve| !curve.keyframes.is_empty()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for callback in proposal.callbacks() {
        match callback {
            Callback::ParameterReferencesCleared { parameter, flags } => {
                if flags & !7 != 0 {
                    return Err(failure("native clear flags changed"));
                }
                // Modulation-only clear is an explicit NoChange: this
                // current canonical model has no host modulation binding.
                if flags & 3 != 0 {
                    references.insert(parameter.clone(), None);
                    parameters.push(
                        mondrian_timeline::NativeAudioParameterOperation::ClearAutomation {
                            parameter_id: parameter.clone(),
                        },
                    );
                }
            }
            Callback::ParameterValuesRescanned { kind, values } => {
                // Actual get_value facts refresh only an existing unkeyed
                // reference. No key is created, keyed playback remains the
                // sole curve, and a preceding Clear remains absent.
                if kind.refreshes_values() {
                    for (id, bits) in values {
                        let schema =
                            schema_for(id).ok_or_else(|| failure("rescan schema changed"))?;
                        let parameter = processor
                            .parameters
                            .get(id)
                            .filter(|actual| actual.schema == *schema)
                            .ok_or_else(|| failure("rescan parameter changed"))?;
                        if references.get(id) != Some(&Some(false)) {
                            continue;
                        }
                        let value = f64::from_bits(*bits);
                        if !value.is_finite() {
                            return Err(failure("rescan value is not finite"));
                        }
                        parameters.push(mondrian_timeline::NativeAudioParameterOperation::from(
                            NativeAudioParameterWrite {
                                schema: Box::new(parameter.schema.clone()),
                                value,
                                mode: NativeAudioParameterWriteMode::Unkeyed,
                            },
                        ));
                    }
                }
            }
            Callback::Parameter { parameter: id, phase, value_bits }
            | Callback::ParameterAtExecution { parameter: id, phase, value_bits, .. } => {
                let schema = schema_for(id)
                    .ok_or_else(|| failure("native callback lacks its pinned parameter schema"))?;
                let parameter = processor
                    .parameters
                    .get(id)
                    .filter(|actual| actual.schema == *schema)
                    .ok_or_else(|| failure("native parameter schema changed"))?;
                if *phase != Phase::Value {
                    continue;
                }
                let value =
                    value_bits.map(f64::from_bits).filter(|value| value.is_finite()).ok_or_else(
                        || failure("native value is not a finite definition-domain scalar"),
                    )?;
                let mode = if let Callback::ParameterAtExecution { position, .. } = callback {
                    let target = AudioAutomationTarget::ProcessorParameter {
                        rack: address,
                        processor_id: processor.id,
                        parameter_id: id.clone(),
                    };
                    NativeAudioParameterWriteMode::AtOwnerTime {
                        position: mondrian_timeline::audio_automation_execution_time(
                            sequence, &target, *position, occurrence,
                        )
                        .map_err(failure)?,
                    }
                } else if references.get(id).is_none_or(|state| state != &Some(true)) {
                    NativeAudioParameterWriteMode::Unkeyed
                } else {
                    let target = AudioAutomationTarget::ProcessorParameter {
                        rack: address,
                        processor_id: processor.id,
                        parameter_id: id.clone(),
                    };
                    NativeAudioParameterWriteMode::AtOwnerTime {
                        position: mondrian_timeline::audio_automation_author_time(
                            sequence,
                            &target,
                            gui_sequence_time.ok_or_else(|| {
                                failure("keyed native value requires an explicit GUI author time")
                            })?,
                            occurrence,
                        )
                        .map_err(failure)?,
                    }
                };
                references.insert(
                    id.clone(),
                    Some(matches!(
                        mode,
                        NativeAudioParameterWriteMode::AtOwnerTime { .. }
                    )),
                );
                parameters.push(mondrian_timeline::NativeAudioParameterOperation::from(
                    NativeAudioParameterWrite {
                        schema: Box::new(parameter.schema.clone()),
                        value,
                        mode,
                    },
                ));
            }
            Callback::DirtyChanged(_) | Callback::GroupStarted | Callback::GroupFinished => {}
        }
    }
    Ok(NativeAudioAuthorUpdate {
        parameters,
        opaque_state: proposal.state().cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{
        ParameterInvalidValuePolicy, ParameterNumericContract, ParameterUnit,
    };
    use mondrian_core::{
        AuthoringTimeDomain, ExactAutomationKeyframe, NativeAudioAuthorBoundaryFacts,
        NativeAudioAuthorCompletion, NativeAudioEditorInsertion, NativeAudioEditorOccurrenceOwner,
        NativeAudioEditorTarget, NativeAudioParameterRescanKind, PropertyValue, SequenceId,
        TimelineTime,
    };
    use mondrian_timeline::{
        apply_audio_processor_rack_edit, audio_processor_rack, AudioChannelStripOwner,
        AudioChannelStripRack, AudioProcessorRackEdit, AudioProcessorRackEditRequest,
        AudioProcessorRackPlacement, NativeAudioParameterOperation as Operation,
    };
    use std::num::NonZeroU64;

    struct Fixture {
        sequence: Sequence,
        owner: NativeAudioAuthorOwner,
        parameter: ParameterId,
        target: NativeAudioEditorTarget,
        schemas: BTreeMap<ParameterId, ParameterSchema>,
    }

    impl Fixture {
        fn new(keyed: bool, animatable: bool) -> Self {
            let mut sequence = Sequence::new("Pure native author projection");
            let output = sequence.audio_program.outputs[0].id;
            let owner = NativeAudioAuthorOwner::ChannelStrip {
                owner: AudioChannelStripOwner::ProgramOutput { output_id: output },
                rack: AudioChannelStripRack::PreFader,
            };
            let parameter = ParameterId::new("clap.param.7").unwrap();
            let mut schema = ParameterSchema::v1(parameter.clone(), PropertyValue::Double(0.25))
                .with_numeric_contract(
                    ParameterUnit::Unitless,
                    ParameterNumericContract::closed(
                        0.0,
                        1.0,
                        None,
                        ParameterInvalidValuePolicy::Reject,
                    )
                    .unwrap(),
                );
            schema.is_animatable = animatable;
            let mut value =
                mondrian_timeline::audio::AudioProcessorParameter::from_schema(schema).unwrap();
            if keyed {
                value
                    .automation
                    .as_mut()
                    .unwrap()
                    .keyframes
                    .push(ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.25));
            }
            let schemas = BTreeMap::from([(parameter.clone(), value.schema.clone())]);
            let instance = mondrian_core::AudioProcessorInstanceId::new();
            let processor = AudioProcessorInstance {
                id: instance,
                definition: mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
                    plugin_id: "owned.projection.metadata".into(),
                    schema_version: 1,
                    binary_sha256: Some([9; 32]),
                },
                bypassed: false,
                parameters: BTreeMap::from([(parameter.clone(), value)]).into(),
                opaque_state: Some(vec![1].into()),
            };
            apply_audio_processor_rack_edit(
                &mut sequence,
                &AudioProcessorRackEditRequest {
                    address: owner.rack_address(),
                    edit: AudioProcessorRackEdit::Insert {
                        processor,
                        placement: AudioProcessorRackPlacement::End,
                    },
                },
            )
            .unwrap();
            // Real Core DTO shape over canonical metadata only, never a native
            // binding, Source, Session gesture, runtime install or Ack grant.
            let target = NativeAudioEditorTarget {
                root_plan: mondrian_core::AudioPreparedPlanId::new(),
                root_session: mondrian_core::AudioRenderSessionId::new(),
                local_plan: mondrian_core::AudioPreparedPlanId::new(),
                local_session: mondrian_core::AudioRenderSessionId::new(),
                instance,
                owner: NativeAudioEditorOccurrenceOwner::Output(output),
                insertion: NativeAudioEditorInsertion::PreFader,
                continuity_epoch: None,
                binding: mondrian_core::AudioProcessorBindingId::new(),
                installed_generation: NonZeroU64::new(1).unwrap(),
            };
            Self { sequence, owner, parameter, target, schemas }
        }

        fn processor_in<'a>(&self, sequence: &'a Sequence) -> &'a AudioProcessorInstance {
            &audio_processor_rack(sequence, &self.owner.rack_address()).unwrap().processors[0]
        }

        fn proposal(
            &self,
            callbacks: Vec<Callback>,
            state: Option<Vec<u8>>,
        ) -> NativeAudioAuthorProposal {
            self.proposal_in(self.sequence.id, callbacks, state)
        }

        fn proposal_in(
            &self,
            sequence: SequenceId,
            callbacks: Vec<Callback>,
            state: Option<Vec<u8>>,
        ) -> NativeAudioAuthorProposal {
            NativeAudioAuthorProposal::project(
                self.target,
                sequence,
                callbacks.len() as u64,
                NativeAudioAuthorBoundaryFacts {
                    completion: if state.is_some() {
                        NativeAudioAuthorCompletion::CoherentConfiguration
                    } else {
                        NativeAudioAuthorCompletion::Nonterminal
                    },
                    open_parameter_edits: 0,
                    group_depth: 0,
                },
                callbacks,
                state,
            )
            .unwrap()
        }

        fn project(
            &self,
            proposal: &NativeAudioAuthorProposal,
            pinned: bool,
            time: Option<DomainTime>,
        ) -> Result<NativeAudioAuthorUpdate> {
            project_update(
                &self.sequence,
                self.owner,
                self.processor_in(&self.sequence),
                pinned.then_some(&self.schemas),
                proposal,
                time,
            )
        }

        fn apply(&self, update: NativeAudioAuthorUpdate) -> Sequence {
            let mut candidate = self.sequence.clone();
            apply_audio_processor_rack_edit(
                &mut candidate,
                &AudioProcessorRackEditRequest {
                    address: self.owner.rack_address(),
                    edit: AudioProcessorRackEdit::SetNativeAuthorState {
                        processor_id: self.target.instance,
                        expected_definition: self.processor_in(&self.sequence).definition.clone(),
                        update,
                    },
                },
            )
            .unwrap();
            candidate
        }

        fn value(&self) -> Callback {
            Callback::Parameter {
                parameter: self.parameter.clone(),
                phase: Phase::Value,
                value_bits: Some(0.75_f64.to_bits()),
            }
        }

        fn rescan(&self) -> Callback {
            Callback::ParameterValuesRescanned {
                kind: NativeAudioParameterRescanKind::Values,
                values: vec![(self.parameter.clone(), 0.75_f64.to_bits())],
            }
        }

        fn clear(&self) -> Callback {
            Callback::ParameterReferencesCleared { parameter: self.parameter.clone(), flags: 2 }
        }
    }

    #[test]
    fn native_author_projection_preserves_explicit_time_and_refuses_headless_keyed_value() {
        let f = Fixture::new(true, true);
        let original = f.sequence.clone();
        let plain = f.proposal(vec![f.value()], None);
        let error = f.project(&plain, false, None).unwrap_err();
        assert!(error.to_string().contains("explicit GUI author time"));
        let time = DomainTime::new(
            AuthoringTimeDomain::Sequence(f.sequence.id),
            TimelineTime::new(1, 3).unwrap(),
        );
        let gui = f.project(&plain, true, Some(time)).unwrap();
        let Operation::Value { write } = &gui.parameters[0] else {
            panic!("value")
        };
        assert_eq!(
            write.mode,
            NativeAudioParameterWriteMode::AtOwnerTime { position: time }
        );
        let changed = f.apply(gui);
        assert_eq!(
            f.processor_in(&changed).parameters[&f.parameter]
                .automation
                .as_ref()
                .unwrap()
                .keyframes[1]
                .time,
            time.time
        );
        let at_execution = |position| Callback::ParameterAtExecution {
            parameter: f.parameter.clone(),
            phase: Phase::Value,
            value_bits: Some(0.75_f64.to_bits()),
            position,
        };
        let actual = f.proposal(vec![at_execution(time)], None);
        let update = f.project(&actual, false, None).unwrap();
        let Operation::Value { write } = &update.parameters[0] else {
            panic!("value")
        };
        assert_eq!(
            write.mode,
            NativeAudioParameterWriteMode::AtOwnerTime { position: time }
        );
        let foreign = SequenceId::new();
        let wrong = f.proposal_in(
            foreign,
            vec![at_execution(DomainTime::new(
                AuthoringTimeDomain::Sequence(foreign),
                time.time,
            ))],
            None,
        );
        assert!(f.project(&wrong, false, None).is_err());
        assert!(f
            .project(
                &plain,
                true,
                Some(DomainTime::new(
                    AuthoringTimeDomain::Sequence(foreign),
                    time.time
                ))
            )
            .is_err());
        assert_eq!(f.sequence, original);
    }

    #[test]
    fn native_author_projection_checks_pinned_schema_before_ignoring_boundaries() {
        let mut f = Fixture::new(true, true);
        // Readonly presentation IDs have no canonical writable schema; this
        // helper must not manufacture one or infer writability from animatable.
        let unknown = ParameterId::new("clap.param.99").unwrap();
        for phase in [Phase::Begin, Phase::Value, Phase::End] {
            let proposal = f.proposal(
                vec![Callback::Parameter {
                    parameter: unknown.clone(),
                    phase,
                    value_bits: (phase == Phase::Value).then_some(0.75_f64.to_bits()),
                }],
                None,
            );
            assert!(f
                .project(&proposal, true, None)
                .unwrap_err()
                .to_string()
                .contains("lacks its pinned parameter schema"));
        }
        let unknown_rescan = f.proposal(
            vec![Callback::ParameterValuesRescanned {
                kind: NativeAudioParameterRescanKind::Values,
                values: vec![(unknown.clone(), 0.75_f64.to_bits())],
            }],
            None,
        );
        assert!(f
            .project(&unknown_rescan, true, None)
            .unwrap_err()
            .to_string()
            .contains("rescan schema changed"));
        let clear = f.proposal(
            vec![Callback::ParameterReferencesCleared { parameter: unknown.clone(), flags: 1 }],
            None,
        );
        let candidate = f.apply(f.project(&clear, false, None).unwrap());
        assert!(!f.processor_in(&candidate).parameters.contains_key(&unknown));
        let modulation_only = f.proposal(
            vec![Callback::ParameterReferencesCleared { parameter: unknown, flags: 4 }],
            None,
        );
        assert!(f.project(&modulation_only, true, None).unwrap().parameters.is_empty());

        // A touched stale schema must still fail on Begin/End and on a keyed
        // rescan that would otherwise be skipped without emitting a write.
        f.sequence.audio_program.outputs[0].strip.pre_fader.processors[0]
            .parameters
            .get_mut(&f.parameter)
            .unwrap()
            .schema
            .schema_version += 1;
        let original = f.sequence.clone();
        for phase in [Phase::Begin, Phase::Value, Phase::End] {
            let proposal = f.proposal(
                vec![Callback::Parameter {
                    parameter: f.parameter.clone(),
                    phase,
                    value_bits: (phase == Phase::Value).then_some(0.75_f64.to_bits()),
                }],
                None,
            );
            assert!(f
                .project(&proposal, true, None)
                .unwrap_err()
                .to_string()
                .contains("native parameter schema changed"));
        }
        assert!(f
            .project(&f.proposal(vec![f.rescan()], None), true, None)
            .unwrap_err()
            .to_string()
            .contains("rescan parameter changed"));
        assert_eq!(f.sequence, original);
    }

    #[test]
    fn native_author_projection_preserves_clear_rescan_and_value_source_order() {
        let f = Fixture::new(false, true);
        let original = f.sequence.clone();
        for (callbacks, count, expected) in [
            (vec![f.clear(), f.rescan()], 1, None),
            (vec![f.rescan(), f.clear()], 2, None),
            (vec![f.clear(), f.value()], 2, Some(0.75_f64)),
            (vec![f.value(), f.clear()], 2, None),
        ] {
            let update = f.project(&f.proposal(callbacks, None), false, None).unwrap();
            assert_eq!(update.parameters.len(), count);
            let changed = f.apply(update);
            assert_eq!(
                f.processor_in(&changed).parameters[&f.parameter]
                    .automation
                    .as_ref()
                    .map(|curve| curve.default_value.to_bits()),
                expected.map(f64::to_bits)
            );
        }
        let keyed = Fixture::new(true, true);
        assert!(keyed
            .project(&keyed.proposal(vec![keyed.rescan()], None), false, None)
            .unwrap()
            .parameters
            .is_empty());
        assert_eq!(f.sequence, original);
    }

    #[test]
    fn native_author_projection_keeps_empty_state_and_nonanimatable_static_values_distinct() {
        let f = Fixture::new(false, false);
        let original = f.sequence.clone();
        for state in [None, Some(Vec::new())] {
            let proposal = f.proposal(vec![Callback::DirtyChanged(false)], state.clone());
            let update = f.project(&proposal, false, None).unwrap();
            assert!(update.parameters.is_empty());
            assert_eq!(
                update.opaque_state.as_ref().map(|bytes| bytes.as_slice()),
                state.as_deref()
            );
            let changed = f.apply(update);
            assert_eq!(
                f.processor_in(&changed).opaque_state.as_ref().unwrap().as_slice(),
                state.as_deref().unwrap_or(&[1])
            );
        }
        let update = f.project(&f.proposal(vec![f.value()], None), false, None).unwrap();
        let Operation::Value { write } = &update.parameters[0] else {
            panic!("value")
        };
        assert!(!write.schema.is_animatable);
        assert_eq!(write.mode, NativeAudioParameterWriteMode::Unkeyed);
        assert_eq!(
            f.processor_in(&f.apply(update)).parameters[&f.parameter]
                .automation
                .as_ref()
                .unwrap()
                .default_value
                .to_bits(),
            0.75_f64.to_bits()
        );
        assert_eq!(f.sequence, original);
    }
}
