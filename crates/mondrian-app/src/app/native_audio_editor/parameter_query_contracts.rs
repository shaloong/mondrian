//! App presentation and ticket controls over a real builtin Media Source.
//! Opened/native receipts below are DTO models, not native plugin qualification.
use super::*;
use crate::app::audio_rendering::TimelineAudioPcmRenderer;
use crate::app::parameter_gesture::{
    AudioScalarAuthorLease, AudioScalarGestureTarget, AudioScalarOwner,
};
use crate::app::playback::AppAudioPlayback;
use crate::app::product_action::AudioNativeParameterQueryPayload;
use mondrian_core::{
    AudioPcmRendererMaterializationId, NativeAudioParameterObservation, NativeAudioParameterQuery,
    NativeAudioParameterQueryOutcome,
};
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::Arc;

pub(super) fn fixture() -> (
    tempfile::TempDir,
    AppState,
    AudioNativeParameterQueryPayload,
    Arc<TimelineAudioPcmRenderer>,
) {
    let (directory, mut state, proposal) =
        super::attachment_continuity_contracts::pending_open_with_parameter(true);
    let old_payload = state.native_audio_editor.as_ref().unwrap().payload.clone();
    let mut sequence = state.active_sequence().unwrap().clone();
    let processor = &mut sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
    processor.definition = mondrian_timeline::AudioProcessorDefinitionRef::Vst3 {
        class_id: "ABCDEF019182FAEB566D624153675854".into(),
        vendor: Some("Surge Synth Team".into()),
        schema_version: 1,
        binary_sha256: Some([7; 32]),
    };
    let id = ParameterId::new("vst3.param.0").unwrap();
    let mut parameter = processor.parameters.values().next().unwrap().clone();
    parameter.schema.parameter_id = id.clone();
    parameter.automation = None;
    let schema = parameter.schema.clone();
    processor.parameters = BTreeMap::from([(id, parameter)]).into();
    let definition = processor.definition.clone();
    let processor_id = processor.id;
    state.test_set_sequence(Some(sequence));
    let mut payload = old_payload;
    payload.session_id = state.authoring_session_id().unwrap();
    payload.definition = definition.clone();
    let mut context = state.prepare_native_audio_editor_context(payload).unwrap();
    let target = NativeAudioEditorTarget {
        instance: processor_id,
        owner: context.locator.owner,
        insertion: context.locator.insertion,
        ..proposal.target()
    };
    context.target = Some(target);
    context.attachment_target = Some(target);
    context.surface_renderer_id = Some(AudioPcmRendererMaterializationId::new());
    let mut builtin = state.active_sequence().unwrap().clone();
    builtin.audio_program.outputs[0].strip.pre_fader.processors.clear();
    let mut renderer = TimelineAudioPcmRenderer::new(
        builtin.clone(),
        vec![builtin],
        state.asset_library_handle().unwrap(),
        Arc::clone(&state.audio_source_cache),
        state.execution_resources.decision().audio.runtime_grant,
        mondrian_audio::AudioAuditionOverlay::default(),
        state.audio_sample_rate,
        mondrian_core::AudioChannelLayout::Stereo,
    )
    .unwrap();
    renderer.bind_author_snapshot(state.authoring.as_ref().unwrap().execution_snapshot());
    let renderer = Arc::new(renderer);
    context.surface_renderer = Some(Arc::downgrade(&renderer));
    state.native_audio_renderer = Some(Arc::downgrade(&renderer));
    state.native_audio_editor = Some(context);
    let anchor = mondrian_core::AudioSamplePosition::new(
        0,
        mondrian_core::AudioSampleRate::new(state.audio_sample_rate).unwrap(),
    );
    let AppAudioPlayback::Available(playback) = &mut state.audio_playback else {
        panic!("actual Media execution required")
    };
    playback.prepare(anchor, renderer.clone(), renderer.continuity_model()).unwrap();
    let scalar = AudioScalarGestureTarget {
        lease: AudioScalarAuthorLease::from_state(&state, state.active_sequence_id().unwrap())
            .unwrap(),
        owner: AudioScalarOwner::Parameter {
            owner: mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip {
                owner: AudioChannelStripOwner::ProgramOutput {
                    output_id: state.active_sequence().unwrap().audio_program.outputs[0].id,
                },
                rack: AudioChannelStripRack::PreFader,
            },
            processor_id,
            definition,
            parameter_schema: Box::new(schema),
        },
    };
    let payload = AudioNativeParameterQueryPayload {
        target: Box::new(scalar),
        expected_author_generation: state.project_author_generation(),
    };
    (directory, state, payload, renderer)
}

fn pending(
    state: &mut AppState,
    payload: AudioNativeParameterQueryPayload,
    serial: u64,
) -> (
    NonZeroU64,
    AudioPcmRendererMaterializationId,
    u64,
    NativeAudioParameterQuery,
) {
    let query = state
        .native_parameter_query_context(&payload)
        .expect("eligible noFact None row");
    let generation = state.audio_playback_snapshot().generation;
    let context = state.native_audio_editor.as_mut().unwrap();
    let ticket = NonZeroU64::new(serial).unwrap();
    let renderer_id = context.surface_renderer_id.unwrap();
    context.parameter_query = Some(NativeParameterQueryPending {
        ticket,
        generation,
        renderer_id,
        renderer: context.surface_renderer.clone().unwrap(),
        query: query.clone(),
        payload,
    });
    (ticket, renderer_id, generation, query)
}

fn observed(
    query: &NativeAudioParameterQuery,
) -> mondrian_media::AudioNativeParameterQueryCompletion {
    let fact = mondrian_core::NativeAudioParameterFact {
        revision: 3,
        parameter: query.parameter.clone(),
        event_flags: 0,
        value_bits: Some(0.75_f64.to_bits()),
        author_value_bits: Some(0.75_f64.to_bits()),
        origin: mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { rescan_flags: 0 },
        hidden: Some(false),
        periodic: None,
        name: Some("Current caption".into()),
        module: None,
        info_flags: Some(1),
        display: Some("0.75".into()),
    };
    mondrian_media::AudioNativeParameterQueryCompletion::Completed(
        NativeAudioParameterQueryOutcome::Observed(Box::new(
            NativeAudioParameterObservation::project(
                query.target,
                NonZeroU32::new(std::process::id()).unwrap(),
                NonZeroU64::new(7).unwrap(),
                3,
                vec![fact],
            )
            .unwrap(),
        )),
    )
}

fn finish(mut state: AppState, renderer: Arc<TimelineAudioPcmRenderer>) {
    drop(renderer);
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
}

#[test]
fn explicit_query_app_stationary_no_device_bootstraps_only_presentation_without_authoring() {
    let (_directory, mut state, payload, renderer) = fixture();
    assert_eq!(
        state.audio_playback_snapshot().state,
        mondrian_media::AudioPlaybackState::DeviceUnavailable
    );
    assert!(state.displayed_native_parameter_query((*payload.target).clone()).is_some());
    let author = state.authoring.as_ref().unwrap();
    let document = serde_json::to_value(author.document()).unwrap();
    let generation = author.author_generation();
    let history = author.history().diagnostics();
    let args = pending(&mut state, payload.clone(), 1);
    state.observe_native_audio_parameter_query(
        args.0,
        args.1,
        args.2,
        args.3.clone(),
        observed(&args.3),
    );
    let context = state.native_audio_editor.as_ref().unwrap();
    assert_eq!(
        context.parameter_fact(&args.3.parameter).unwrap().name.as_deref(),
        Some("Current caption")
    );
    assert!(context.parameter_query.is_none());
    assert!(
        state.displayed_native_parameter_query((*payload.target).clone()).is_none(),
        "existing Fact follows the original Adopt path"
    );
    let author = state.authoring.as_ref().unwrap();
    assert_eq!(serde_json::to_value(author.document()).unwrap(), document);
    assert_eq!(author.author_generation(), generation);
    assert_eq!(author.history().diagnostics(), history);
    assert!(state.active_sequence().unwrap().audio_program.outputs[0]
        .strip
        .pre_fader
        .processors[0]
        .parameters
        .values()
        .all(|p| p.automation.is_none()));
    finish(state, renderer);
}

#[test]
fn explicit_query_app_old_ticket_cannot_clear_new_pending_and_cancel_does_not_retire_source() {
    let (_directory, mut state, payload, renderer) = fixture();
    let args = pending(&mut state, payload.clone(), 2);
    state.observe_native_audio_parameter_query(
        NonZeroU64::new(1).unwrap(),
        args.1,
        args.2,
        args.3.clone(),
        observed(&args.3),
    );
    assert_eq!(
        state
            .native_audio_editor
            .as_ref()
            .unwrap()
            .parameter_query
            .as_ref()
            .unwrap()
            .ticket,
        args.0
    );
    assert!(state.native_audio_editor.as_ref().unwrap().parameter_observation.is_none());
    state.observe_native_audio_parameter_query(
        args.0,
        args.1,
        args.2,
        args.3,
        mondrian_media::AudioNativeParameterQueryCompletion::Cancelled,
    );
    assert!(state.native_audio_editor.as_ref().unwrap().parameter_query.is_none());
    assert!(state.native_audio_editor.as_ref().unwrap().parameter_observation.is_none());
    let concrete = Arc::downgrade(&renderer);
    let weak: std::sync::Weak<dyn mondrian_media::AudioPcmRenderer> = concrete;
    assert!(state.audio_playback.matches_source_weak(&weak));
    assert!(state.native_parameter_query_context(&payload).is_some());
    finish(state, renderer);
}

#[test]
fn explicit_query_app_completion_rejects_changed_lease_schema_owner_renderer_generation_or_source()
{
    for boundary in 0..8 {
        let (_directory, mut state, payload, renderer) = fixture();
        let (ticket, mut renderer_id, mut generation, mut query) = pending(&mut state, payload, 3);
        match boundary {
            0 => renderer_id = AudioPcmRendererMaterializationId::new(),
            1 => generation += 1,
            2 => query.target.binding = mondrian_core::AudioProcessorBindingId::new(),
            3 => {
                state
                    .native_audio_editor
                    .as_mut()
                    .unwrap()
                    .parameter_query
                    .as_mut()
                    .unwrap()
                    .payload
                    .expected_author_generation += 1;
            }
            4 => {
                state
                    .native_audio_editor
                    .as_mut()
                    .unwrap()
                    .parameter_query
                    .as_mut()
                    .unwrap()
                    .payload
                    .target
                    .lease
                    .session_id =
                    serde_json::from_str("\"00000000-0000-0000-0000-000000000007\"").unwrap();
            }
            5 => {
                state.native_audio_editor.as_mut().unwrap().schemas.clear();
            }
            6 => {
                state.native_audio_editor.as_mut().unwrap().attachment_target = None;
            }
            7 => {
                let anchor = mondrian_core::AudioSamplePosition::new(
                    0,
                    mondrian_core::AudioSampleRate::new(state.audio_sample_rate).unwrap(),
                );
                let AppAudioPlayback::Available(playback) = &mut state.audio_playback else {
                    unreachable!()
                };
                playback.clear_source(anchor).unwrap();
            }
            _ => unreachable!(),
        }
        let completion = observed(&query);
        state.observe_native_audio_parameter_query(
            ticket,
            renderer_id,
            generation,
            query,
            completion,
        );
        assert!(
            state.native_audio_editor.as_ref().unwrap().parameter_query.is_none(),
            "own ticket releases at boundary {boundary}"
        );
        assert!(
            state.native_audio_editor.as_ref().unwrap().parameter_observation.is_none(),
            "stale fact rejected at boundary {boundary}"
        );
        finish(state, renderer);
    }
}

#[test]
fn explicit_query_app_transport_contract_refuses_every_nonstationary_state() {
    use mondrian_playback::TransportState::*;
    for state in [Stopped, Paused, Ended] {
        assert!(native_parameter_query_transport_is_stationary(state));
    }
    for state in [
        Priming,
        Playing,
        Recovering,
        HoldingForNativeAudioControl,
        Blocked,
    ] {
        assert!(!native_parameter_query_transport_is_stationary(state));
    }
}

#[test]
fn explicit_query_app_some_and_closed_surface_keep_original_authority() {
    for boundary in 0..3 {
        let (_directory, mut state, mut payload, renderer) = fixture();
        match boundary {
            0 => state.native_audio_editor.as_mut().unwrap().surface_renderer_id = None,
            1 => state.native_audio_editor.as_mut().unwrap().attachment_target = None,
            2 => {
                let id = ParameterId::new("vst3.param.0").unwrap();
                let mut sequence = state.active_sequence().unwrap().clone();
                sequence.audio_program.outputs[0].strip.pre_fader.processors[0]
                    .parameters
                    .get_mut(&id)
                    .unwrap()
                    .set_automation(mondrian_core::ExactAutomationCurve::new(id, 0.25).unwrap())
                    .unwrap();
                state.test_set_sequence(Some(sequence));
                payload.expected_author_generation = state.project_author_generation();
                payload.target.lease.session_id = state.authoring_session_id().unwrap();
            }
            _ => unreachable!(),
        }
        assert!(state.native_parameter_query_context(&payload).is_none());
        finish(state, renderer);
    }
}

#[test]
fn explicit_query_app_checked_serial_survives_refusal_close_and_context_replacement() {
    let (_directory, mut state, payload, renderer) = fixture();
    state.native_parameter_query_serial = 41;
    // This fixture models App Opened, but has no native Media attachment;
    // the actual Media queue therefore refuses without querying any native SDK.
    state.query_native_audio_parameter_product(payload.clone()).unwrap();
    assert_eq!(state.native_parameter_query_serial, 42);
    assert!(state.native_audio_editor.as_ref().unwrap().parameter_query.is_none());
    let mut context = state.native_audio_editor.take().unwrap();
    context.attachment_target = None;
    state.native_audio_editor = Some(context);
    state.query_native_audio_parameter_product(payload.clone()).unwrap();
    assert_eq!(
        state.native_parameter_query_serial, 42,
        "closed surface is pre-refused"
    );
    let context = state.native_audio_editor.as_mut().unwrap();
    context.attachment_target = context.target;
    state.query_native_audio_parameter_product(payload.clone()).unwrap();
    assert_eq!(
        state.native_parameter_query_serial, 43,
        "a new surface never resets the App serial"
    );
    state.native_parameter_query_serial = u64::MAX;
    state.query_native_audio_parameter_product(payload).unwrap();
    assert_eq!(
        state.native_parameter_query_serial,
        u64::MAX,
        "serial exhaustion refuses, never wraps"
    );
    assert!(state.native_audio_editor.as_ref().unwrap().parameter_query.is_none());
    finish(state, renderer);
}
