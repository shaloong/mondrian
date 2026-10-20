//! Ordinary canonical/provenance controls. Core targets here are shape-only
//! metadata, never native object, Source installation or acknowledgement proof.

use super::super::attachment_continuity_contracts::{opened_author, pending_open_with_parameter};
use super::*;
use mondrian_core::{
    AudioComponentEditId, AudioPreparedPlanId, AudioProcessingScopeId, DomainTime,
    ExactAutomationKeyframe, NativeAudioAuthorBoundaryFacts, NativeAudioAuthorCallback as Callback,
    NativeAudioAuthorCompletion, NativeAudioParameterCallbackPhase as CallbackPhase,
    NativeAudioParameterRescanKind, TimelineTime,
};
use mondrian_timeline::{
    apply_audio_processor_rack_edit, audio_processor_rack, AudioProcessorRackEdit,
    AudioProcessorRackEditRequest, AudioProcessorRackPlacement, NativeAudioParameterOperation,
    NativeAudioParameterWriteMode,
};

fn direct_target(proposal: &NativeAudioAuthorProposal) -> NativeAudioEditorTarget {
    let mut target = proposal.target();
    target.local_plan = target.root_plan;
    target.local_session = target.root_session;
    target
}

fn terminal(
    target: NativeAudioEditorTarget,
    sequence: mondrian_core::SequenceId,
    callbacks: Vec<Callback>,
) -> NativeAudioAuthorProposal {
    NativeAudioAuthorProposal::project(
        target,
        sequence,
        callbacks.len() as u64,
        NativeAudioAuthorBoundaryFacts {
            completion: NativeAudioAuthorCompletion::CoherentConfiguration,
            open_parameter_edits: 0,
            group_depth: 0,
        },
        callbacks,
        Some(Vec::new()),
    )
    .unwrap()
}

fn apply_member(
    mut sequence: mondrian_timeline::Sequence,
    member: NativeAudioTerminalAuthorMember,
) -> mondrian_timeline::Sequence {
    apply_audio_processor_rack_edit(
        &mut sequence,
        &AudioProcessorRackEditRequest {
            address: member.owner.rack_address(),
            edit: AudioProcessorRackEdit::SetNativeAuthorState {
                processor_id: member.processor_id,
                expected_definition: member.expected_definition,
                update: member.update,
            },
        },
    )
    .unwrap();
    sequence
}

#[test]
fn terminal_member_maps_all_direct_root_racks_and_preserves_ordered_projection() {
    let (_directory, mut state, sample) = pending_open_with_parameter(true);
    let mut sequence = state.active_sequence().unwrap().clone();
    let template = sequence.audio_program.outputs[0].strip.pre_fader.processors[0].clone();
    let output = sequence.audio_program.outputs[0].id;
    let track = mondrian_timeline::Track::new_audio("Canonical terminal Track");
    let track_id = track.id;
    sequence.audio_tracks.push(track);
    sequence.audio_program.track_channels.insert(
        track_id,
        mondrian_timeline::audio::AudioTrackMixerChannel::default(),
    );
    let bus_id = mondrian_core::MixBusId::new();
    sequence.audio_program.buses.push(mondrian_timeline::audio::AudioMixBus {
        id: bus_id,
        name: "Canonical terminal Bus".into(),
        strip: Default::default(),
    });
    let mut cases = Vec::new();
    for (channel, occurrence) in [
        (
            AudioChannelStripOwner::Track { track_id },
            Owner::Track(track_id),
        ),
        (AudioChannelStripOwner::Bus { bus_id }, Owner::Bus(bus_id)),
        (
            AudioChannelStripOwner::ProgramOutput { output_id: output },
            Owner::Output(output),
        ),
    ] {
        for (rack, insertion) in [
            (AudioChannelStripRack::PreFader, Insertion::PreFader),
            (AudioChannelStripRack::PostFader, Insertion::PostFader),
        ] {
            let mut processor = template.clone();
            processor.id = mondrian_core::AudioProcessorInstanceId::new();
            let mut target = direct_target(&sample);
            target.owner = occurrence;
            target.insertion = insertion;
            target.instance = processor.id;
            let owner = NativeAudioAuthorOwner::ChannelStrip { owner: channel, rack };
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
            cases.push((owner, target));
        }
    }
    state.test_set_sequence(Some(sequence));
    let author = state.authoring.as_ref().unwrap();
    let before = serde_json::to_value(author.document()).unwrap();
    let snapshot = author.execution_snapshot();
    let id = ParameterId::new("clap.param.7").unwrap();
    for (owner, target) in cases {
        let proposal = terminal(
            target,
            sample.sequence(),
            vec![
                Callback::ParameterReferencesCleared { parameter: id.clone(), flags: 1 },
                Callback::ParameterValuesRescanned {
                    kind: NativeAudioParameterRescanKind::Values,
                    values: vec![(id.clone(), 0.75_f64.to_bits())],
                },
                Callback::Parameter {
                    parameter: id.clone(),
                    phase: CallbackPhase::Value,
                    value_bits: Some(0.5_f64.to_bits()),
                },
            ],
        );
        let projected = member(author, &proposal).unwrap();
        assert_eq!(projected.owner, owner);
        assert_eq!(projected.processor_id, target.instance);
        assert_eq!(projected.expected_definition, template.definition);
        assert_eq!(
            projected.completion,
            NativeAudioAuthorCompletion::CoherentConfiguration
        );
        assert_eq!(
            projected.update.opaque_state.as_ref().map(|s| s.as_slice()),
            Some([].as_slice())
        );
        assert_eq!(
            projected.update.parameters.len(),
            2,
            "Clear prevents a rescan revival"
        );
        assert!(
            matches!(&projected.update.parameters[0], NativeAudioParameterOperation::ClearAutomation { parameter_id } if parameter_id == &id)
        );
        let candidate = apply_member(
            author.sequence(sample.sequence()).unwrap().clone(),
            projected,
        );
        let processor = audio_processor_rack(&candidate, &owner.rack_address())
            .unwrap()
            .processors
            .iter()
            .find(|p| p.id == target.instance)
            .unwrap();
        let curve = processor.parameters[&id].automation.as_ref().unwrap();
        assert_eq!(curve.default_value, 0.5);
        assert!(curve.keyframes.is_empty());
        assert_eq!(
            processor.opaque_state.as_ref().map(|s| s.as_slice()),
            Some([].as_slice())
        );
    }
    assert_eq!(serde_json::to_value(author.document()).unwrap(), before);
    assert!(
        snapshot.enter_commit().is_ok(),
        "projection is not publication"
    );
}

#[test]
fn terminal_member_rejects_scope_nested_unpinned_and_unknown_without_publication() {
    let (_directory, mut state, sample) = pending_open_with_parameter(true);
    let target = direct_target(&sample);
    let author = state.authoring.as_ref().unwrap();
    let before = serde_json::to_value(author.document()).unwrap();
    for rejected in [
        NativeAudioEditorTarget { insertion: Insertion::Scope, ..target },
        NativeAudioEditorTarget {
            owner: Owner::Contribution {
                edit_id: AudioComponentEditId::new(),
                scope_id: AudioProcessingScopeId::new(),
            },
            ..target
        },
        NativeAudioEditorTarget { local_plan: AudioPreparedPlanId::new(), ..target },
        NativeAudioEditorTarget {
            instance: mondrian_core::AudioProcessorInstanceId::new(),
            ..target
        },
    ] {
        assert!(member(
            author,
            &terminal(rejected, sample.sequence(), vec![Callback::GroupFinished])
        )
        .is_err());
    }
    let nonterminal = NativeAudioAuthorProposal::project(
        target,
        sample.sequence(),
        1,
        NativeAudioAuthorBoundaryFacts {
            completion: NativeAudioAuthorCompletion::Nonterminal,
            open_parameter_edits: 0,
            group_depth: 1,
        },
        vec![Callback::GroupStarted],
        None,
    )
    .unwrap();
    assert!(member(author, &nonterminal).is_err());
    let unknown = ParameterId::new("clap.param.999").unwrap();
    assert!(member(
        author,
        &terminal(
            target,
            sample.sequence(),
            vec![Callback::Parameter {
                parameter: unknown.clone(),
                phase: CallbackPhase::Value,
                value_bits: Some(0.5_f64.to_bits()),
            }]
        )
    )
    .is_err());
    // Unknown Clear remains the existing legal no-op; no writable schema or
    // parameter is created for a readonly/presentation-only ID.
    let clear = member(
        author,
        &terminal(
            target,
            sample.sequence(),
            vec![Callback::ParameterReferencesCleared { parameter: unknown.clone(), flags: 1 }],
        ),
    )
    .unwrap();
    let candidate = apply_member(author.sequence(sample.sequence()).unwrap().clone(), clear);
    assert!(
        !candidate.audio_program.outputs[0].strip.pre_fader.processors[0]
            .parameters
            .contains_key(&unknown)
    );
    assert_eq!(serde_json::to_value(author.document()).unwrap(), before);
    for hash in [None, Some([0; 32])] {
        let mut sequence = state.active_sequence().unwrap().clone();
        let processor = &mut sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
        let mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap { binary_sha256, .. } =
            &mut processor.definition
        else {
            panic!("native fixture")
        };
        *binary_sha256 = hash;
        state.test_set_sequence(Some(sequence));
        let author = state.authoring.as_ref().unwrap();
        let before = serde_json::to_value(author.document()).unwrap();
        assert!(member(
            author,
            &terminal(target, sample.sequence(), vec![Callback::GroupFinished])
        )
        .is_err());
        assert_eq!(serde_json::to_value(author.document()).unwrap(), before);
    }
}

#[test]
fn terminal_member_has_no_gui_time_and_uses_only_actual_execution_time() {
    let (_directory, mut state, sample) = pending_open_with_parameter(true);
    let id = ParameterId::new("clap.param.7").unwrap();
    let mut sequence = state.active_sequence().unwrap().clone();
    sequence.audio_program.outputs[0].strip.pre_fader.processors[0]
        .parameters
        .get_mut(&id)
        .unwrap()
        .automation
        .as_mut()
        .unwrap()
        .keyframes
        .push(ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.25));
    state.test_set_sequence(Some(sequence));
    let author = state.authoring.as_ref().unwrap();
    let before = serde_json::to_value(author.document()).unwrap();
    let target = direct_target(&sample);
    let plain = terminal(
        target,
        sample.sequence(),
        vec![Callback::Parameter {
            parameter: id.clone(),
            phase: CallbackPhase::Value,
            value_bits: Some(0.5_f64.to_bits()),
        }],
    );
    assert!(member(author, &plain)
        .unwrap_err()
        .to_string()
        .contains("explicit GUI author time"));
    let time = DomainTime::new(
        AuthoringTimeDomain::Sequence(sample.sequence()),
        TimelineTime::new(1, 3).unwrap(),
    );
    let at_execution = terminal(
        target,
        sample.sequence(),
        vec![Callback::ParameterAtExecution {
            parameter: id.clone(),
            phase: CallbackPhase::Value,
            value_bits: Some(0.5_f64.to_bits()),
            position: time,
        }],
    );
    let projected = member(author, &at_execution).unwrap();
    let NativeAudioParameterOperation::Value { write } = &projected.update.parameters[0] else {
        panic!("value")
    };
    assert_eq!(
        write.mode,
        NativeAudioParameterWriteMode::AtOwnerTime { position: time }
    );
    let candidate = apply_member(
        author.sequence(sample.sequence()).unwrap().clone(),
        projected,
    );
    assert_eq!(
        candidate.audio_program.outputs[0].strip.pre_fader.processors[0].parameters[&id]
            .automation
            .as_ref()
            .unwrap()
            .keyframes[1]
            .time,
        time.time
    );
    assert_eq!(serde_json::to_value(author.document()).unwrap(), before);
}

#[test]
fn terminal_delivery_requires_actual_media_source_before_session_group_or_publication() {
    let (_directory, mut state, sample, renderer) = opened_author();
    state.native_audio_editor = None;
    let target = direct_target(&sample);
    let proposal = terminal(target, sample.sequence(), vec![Callback::GroupFinished]);
    assert!(state.native_audio_author_work.is_none());
    assert!(
        !state.project_audio_source_is_current(),
        "the real builtin Timeline is not installed in Media"
    );
    let author = state.authoring.as_ref().unwrap();
    let session = author.session_id();
    let generation = author.author_generation();
    let history = author.history().diagnostics();
    let before = serde_json::to_value(author.document()).unwrap();
    let snapshot = author.execution_snapshot();
    let error = state
        .observe_native_audio_author_delivery(
            AudioPcmRendererMaterializationId::new(),
            state.audio_playback_snapshot().generation,
            proposal,
        )
        .expect_err("an actual captured renderer Arc is insufficient without the Media Source");
    assert!(
        error.to_string().contains("not the Media Source"),
        "{error:?}"
    );
    assert!(state.native_audio_author_work.is_none());
    assert!(state.native_audio_editor.is_none());
    assert!(state
        .native_audio_renderer
        .as_ref()
        .is_some_and(|weak| Weak::ptr_eq(weak, &Arc::downgrade(&renderer))));
    let author = state.authoring.as_mut().unwrap();
    assert_eq!(author.session_id(), session);
    assert_eq!(author.author_generation(), generation);
    assert_eq!(author.history().diagnostics(), history);
    assert_eq!(serde_json::to_value(author.document()).unwrap(), before);
    assert!(snapshot.enter_commit().is_ok());
    // BeginGroup has no Document delta. A legitimate generic no-op is the
    // independent check that no NativeStatePending barrier was installed.
    author
        .edit_sequence(sample.sequence(), "No author barrier was begun", |_| Ok(()))
        .unwrap();
    assert_eq!(author.author_generation(), generation);
    assert_eq!(author.history().diagnostics(), history);
    assert_eq!(serde_json::to_value(author.document()).unwrap(), before);
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
}
