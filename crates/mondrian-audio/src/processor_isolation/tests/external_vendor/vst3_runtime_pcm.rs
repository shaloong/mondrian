//! Shared finite canonical Runtime consumer for the two real VST3 cases.

use super::*;

use crate::{AudioContinuityEpoch, AudioRuntimeResourceGrant, AudioStateEntry};
use mondrian_core::{
    AudioOwnerInstallationSource, AutomationSegmentInterpolation, ExactAutomationKeyframe,
    MusicalTimeReference, TimelineTime,
};
use mondrian_timeline::{Clip, Sequence};

// Immutable inputs for this one canonical Runtime observation.
pub(super) struct RuntimeObservationConfiguration<'a> {
    pub(super) case: &'a VendorCase,
    pub(super) constant_parameter_values: &'a [(u32, f64)],
    pub(super) sequence_name: &'a str,
    // Diagnostic presentation of an auxiliary input already checked by outer.
    pub(super) auxiliary_input_key: Option<&'a str>,
    pub(super) musical_time_reference: Option<MusicalTimeReference>,
    pub(super) native_bypass: bool,
}

pub(super) fn observe_runtime_pcm(
    catalog: Arc<DiscoveredVst3AudioProcessorSpecResolver>,
    state: &[u8],
    period: Arc<[f32]>,
    facts: &Facts,
    configuration: RuntimeObservationConfiguration<'_>,
    label: &str,
    directory: &Path,
) -> Vec<f32> {
    let RuntimeObservationConfiguration {
        case,
        constant_parameter_values,
        sequence_name,
        auxiliary_input_key,
        musical_time_reference,
        native_bypass,
    } = configuration;
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        ..test_render_contract()
    };
    let mut author = catalog
        .create_instance(&case.expected_identity, render, Some(state.to_vec()))
        .expect("fresh canonical author from the original VST3 state");
    facts.check_author(&author);
    assert_eq!(author.parameters.len(), case.expected_parameter_count);
    assert!(
        !author.bypassed,
        "host bypass must not replace native processing"
    );
    assert_eq!(
        author.opaque_state.as_deref().map(Vec::as_slice),
        Some(state)
    );
    assert!(constant_parameter_values.len() <= case.expected_parameter_count);
    for (index, &(native_id, value)) in constant_parameter_values.iter().enumerate() {
        assert!(value.is_finite() && (0.0..=1.0).contains(&value));
        assert_ne!(
            native_id, case.bypass_native_id,
            "native bypass has one intent"
        );
        assert!(constant_parameter_values[..index].iter().all(|(id, _)| *id != native_id));
        let id = ParameterId::new(format!("vst3.param.{native_id}")).unwrap();
        let mut curve = ExactAutomationCurve::new(id, value).unwrap();
        let mut key = ExactAutomationKeyframe::linear(TimelineTime::ZERO, value);
        key.interpolation_to_next = AutomationSegmentInterpolation::Hold;
        curve.set_keyframe(key).unwrap();
        author
            .set_parameter_automation(curve)
            .expect("explicit constant native parameter intent");
    }
    if native_bypass {
        let id = ParameterId::new(format!("vst3.param.{}", case.bypass_native_id)).unwrap();
        let mut curve = ExactAutomationCurve::new(id, 1.0).unwrap();
        let mut key = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 1.0);
        key.interpolation_to_next = AutomationSegmentInterpolation::Hold;
        curve.set_keyframe(key).unwrap();
        author
            .set_parameter_automation(curve)
            .expect("actual canonical native bypass intent");
    }
    // This serializes canonical bytes; it never captures the live native object.
    let serialized = serde_json::to_vec(&author).expect("canonical author save");
    assert!(serialized.len() as u64 <= MAX_JSON);
    write_new(directory, &format!("{label}-author.json"), &serialized);
    let restored: AudioProcessorInstance =
        serde_json::from_slice(&serialized).expect("canonical author reopen");
    assert_eq!(restored, author);
    let instance_id = restored.id;
    let mut sequence = Sequence::new(sequence_name);
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.audio_program.musical_time_reference = musical_time_reference;
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            AudioSourceComponentId::primary(),
        )
        .expect("public source clip");
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(restored);
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    let reads = Arc::new(AtomicUsize::new(0));
    let source = PcmMedia { period, reads: reads.clone() };
    // A finite test admission limit, not a reservation/allocation or new policy.
    let grant = AudioRuntimeResourceGrant::new(1, 32 * 1024 * 1024, 16 * 1024 * 1024);
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &source,
        &resolver,
        render,
        None,
        grant,
    )
    .expect("canonical compiler/Output rack/native Session obey the real root resource grant");
    assert_eq!(
        runtime
            .find_resource_session(runtime.resource_root_identity())
            .expect("the actual prepared root Session exists")
            .resource_plan()
            .program()
            .musical_time_reference(),
        musical_time_reference,
        "the prepared canonical program keeps the exact authored reference"
    );
    let footprint = runtime.resource_footprint();
    assert_eq!(footprint.runtime_occurrences, 1);
    assert!(footprint.fixed_resident_bytes <= grant.max_fixed_resident_bytes);
    assert!(footprint.prepared_logical_bytes <= grant.max_prepared_logical_bytes);
    assert!(footprint.processor_session_bytes > 0);
    assert!(runtime.native_observations_supported());
    let installation = AudioOwnerInstallationSource::new();
    let (registration, verifier) = installation.register().unwrap();
    runtime
        .bind_native_owner_installation(verifier)
        .expect("actual Runtime registration");
    let claim = installation
        .reserve_install(&registration, None)
        .and_then(|reservation| installation.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .expect("real same-object installed Output-rack Bind");
    let cold = owner(&runtime);
    assert_eq!(cold.editor_target().instance, instance_id);
    assert!(cold.editor_target().continuity_epoch.is_none());
    let cold_object = runtime
        .observe_native_owner_object(cold)
        .expect("authenticated actual cold object from the existing owner query");
    let mut observations = Observations::default();
    observe(&mut runtime, cold, &mut observations);
    runtime
        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
        .expect("cold author Source closed; initial input delivery is not claimed");
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .expect("actual public Runtime first signal entry");
    let lease = owner(&runtime);
    assert_eq!(lease.editor_target().instance, instance_id);
    assert_eq!(lease.editor_target().continuity_epoch, Some(1));
    let entered_object = runtime
        .observe_native_owner_object(lease)
        .expect("authenticated actual entered object from the existing owner query");
    assert_eq!(
        entered_object, cold_object,
        "first Enter must anchor the same initialized object"
    );
    let pid = entered_object.process_id().get();
    let serial = entered_object.object_serial().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: this is the current installed Runtime's authenticated observation,
    // not a PID inferred from parameter presentation, logs or process discovery.
    // The retained HANDLE grants query/synchronize rights only.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(
        !raw.is_null(),
        "authenticated actual child observation HANDLE"
    );
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        258
    );
    observe(&mut runtime, lease, &mut observations);
    let mut output = Vec::with_capacity(FRAMES * 2);
    let mut calls = 0;
    for start in (0..FRAMES).step_by(64) {
        let frames = 64.min(FRAMES - start);
        let mut pcm = vec![f32::NAN; frames * 2];
        runtime
            .render_into(
                AudioRenderRequest { start_sample: start as i64, frames },
                &mut pcm,
            )
            .expect("actual contiguous Runtime PCM; native typed failures remain failures");
        assert_eq!(pcm.len(), frames * 2);
        assert!(pcm.iter().all(|value| value.is_finite()));
        output.extend(pcm);
        calls += 1;
        observe(&mut runtime, lease, &mut observations);
    }
    assert_eq!(output.len(), FRAMES * 2);
    assert_eq!(calls, FRAMES / 64);
    assert!(
        reads.load(Ordering::Relaxed) > 0,
        "actual bound media source read"
    );
    assert_eq!(
        owner(&runtime),
        lease,
        "one real owner/epoch across all PCM"
    );
    assert_eq!(
        runtime
            .observe_native_owner_object(lease)
            .expect("current same-object native owner after PCM"),
        entered_object
    );
    runtime
        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
        .expect("actual author Source closed after the real PCM calls");
    write_new(
        directory,
        &format!("{label}-output.f32le"),
        &pcm_bytes(&output),
    );
    let mut receipt = json!({
        "schema":1, "canonical_runtime_and_output_rack":true, "host_bypassed":false,
        "native_bypass":native_bypass, "bypass_key_is_sequence_zero_hold":native_bypass,
        "original_state_bytes":state.len(), "original_state_sha256":format!("{:x}",Sha256::digest(state)),
        "sample_rate":48000, "channels":2, "mode":"Offline", "max_block_frames":64,
        "requested_frames":FRAMES, "completed_frames":output.len()/2, "runtime_render_calls":calls,
        "source_reads":reads.load(Ordering::Relaxed), "finite":true,
        "same_installed_owner_across_pcm":true, "continuity_epoch":1,
        "auxiliary_key":auxiliary_input_key,
        "auxiliary_layout":auxiliary_input_key.map(|_| "Stereo"),
        "auxiliary_has_no_authored_route":sequence.audio_program.sidechain_routes.is_empty(),
        "auxiliary_silence_basis":auxiliary_input_key.map(|_| "existing Graph zero-fill for an unconnected declared input"),
        "parameter_polls":observations.polls, "parameter_batches":observations.parameter_batches,
        "native_diagnostic_records":observations.diagnostic_records,
        "output_sha256":format!("{:x}",Sha256::digest(pcm_bytes(&output))),
        "root_resource_grant":{
            "runtime_occurrences":grant.max_runtime_occurrences,
            "fixed_resident_bytes":grant.max_fixed_resident_bytes,
            "prepared_logical_bytes":grant.max_prepared_logical_bytes
        },
        "root_resource_footprint":{
            "runtime_occurrences":footprint.runtime_occurrences,
            "fixed_resident_bytes":footprint.fixed_resident_bytes,
            "prepared_logical_bytes":footprint.prepared_logical_bytes,
            "render_scratch_bytes":footprint.render_scratch_bytes,
            "processor_session_bytes":footprint.processor_session_bytes,
            "parameter_event_bytes":footprint.parameter_event_bytes,
            "compensation_delay_bytes":footprint.compensation_delay_bytes,
            "media_window_bytes":footprint.media_window_bytes,
            "nested_window_bytes":footprint.nested_window_bytes
        }
    });
    receipt.as_object_mut().unwrap().extend(
        json!({
            "cold_source_closed_is_input_delivery_receipt":false,
            "live_state_capture_or_author_ack_performed":false,
            "native_plugin_callback_counter_instrumented":false,
            "dsp_algorithm_or_bypass_precision_qualified":false,
            "private_dsp_state_qualified":false, "nondefault_state_discrimination":false,
            "app_playback_export_qualified":false, "realtime_or_performance_qualified":false,
            "complete_physical_heap_qualified":false, "outer_job_qualified_by_test":false
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    drop(runtime);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
    receipt.as_object_mut().unwrap().extend(json!({
        "native_pid":pid, "native_object_serial":serial,
        "same_authenticated_object_across_pcm":true, "native_identity_observation_origin":"authenticated_owner_object_query",
        "parameter_facts_are_identity_requirement":false, "alive_before_drop_wait_status":alive,
        "post_drop_wait_status":settled, "exit_query_succeeded":queried != 0, "exit_code":exit_code,
        "graceful_ack_qualified":false, "exact_child_handle_exit_qualified":alive == 258 && settled == 0 && queried != 0 && exit_code == 0
    }).as_object().unwrap().clone());
    write_json(directory, &format!("{label}-receipt.json"), receipt);
    assert_eq!(alive, 258);
    assert_eq!(
        settled, 0,
        "existing Runtime Drop settles the exact actual child"
    );
    assert_ne!(queried, 0);
    assert_eq!(
        exit_code, 0,
        "same child exits with status 0; graceful ACK and exit budget remain unqualified"
    );
    output
}
