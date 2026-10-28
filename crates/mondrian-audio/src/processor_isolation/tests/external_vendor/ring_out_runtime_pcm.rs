//! One finite real-plugin Runtime/Graph steady native-bypass PCM qualification.
//! The fixed transition is rendered, not skipped or replaced with a sleep.

use super::*;
use crate::{
    AudioContinuityEpoch, AudioDecodedSource, AudioMediaResolver, AudioProgramRuntime,
    AudioStateEntry, NativeAudioAuthorProgram, NativeAudioParameterHandoffContinuity,
    ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioOwnerInstallationSource, AudioSourceComponentId, AutomationSegmentInterpolation,
    ExactAutomationKeyframe, ExecutionCancellationToken, NativeAudioAuthorCallback, TimelineTime,
};
use mondrian_timeline::audio::AudioChannelStripOwner;
use mondrian_timeline::{
    apply_audio_processor_rack_edit, AudioChannelStripRack, AudioProcessorRackAddress,
    AudioProcessorRackEdit, AudioProcessorRackEditRequest, Clip, NativeAudioAuthorUpdate,
    NativeAudioParameterWrite, NativeAudioParameterWriteMode, Sequence,
};
use std::sync::atomic::{AtomicUsize, Ordering};

const RUNTIME_CASE_ENV: &str = "MONDRIAN_RING_OUT_RUNTIME_PCM_CASE";
const RUNTIME_CASE_HASH_ENV: &str = "MONDRIAN_RING_OUT_RUNTIME_PCM_CASE_SHA256";
const TRANSITION_FRAMES: usize = 6144;
const OBSERVED_FRAMES: usize = TRANSITION_FRAMES + FRAMES;

// Explicit source layout/rate, independent of project and plugin defaults.
#[derive(Clone)]
struct ConfigPcm {
    layout: AudioChannelLayout,
    rate: u32,
    period: Arc<[f32]>,
}
impl AudioMediaResolver for ConfigPcm {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        assert_eq!(rate, self.rate);
        Ok(ResolvedAudioSource::new(
            self.layout,
            Arc::new(self.clone()),
        ))
    }
}
impl AudioDecodedSource for ConfigPcm {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        out: &mut [f32],
        cancel: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        let channels = self.layout.channel_count();
        assert!(!cancel.is_canceled());
        assert_eq!(out.len(), frames * channels);
        for (offset, frame) in out.chunks_exact_mut(channels).enumerate() {
            let sample = start.checked_add(offset as i64).unwrap();
            if sample < 0 {
                frame.fill(0.0);
            } else {
                let at = sample as usize % FRAMES * channels;
                frame.copy_from_slice(&self.period[at..at + channels]);
            }
        }
        Ok(())
    }
}

fn run_config_runtime(
    catalog: Arc<DiscoveredClapAudioProcessorSpecResolver>,
    state: &[u8],
    render: AudioRenderContract,
    gain_db: f64,
    partitions: &[usize],
    directory: &Path,
    label: &str,
) -> Vec<f32> {
    let channels = render.channel_count();
    let author = catalog
        .create_instance("com.duskaudio.ring-out", render, Some(state.to_vec()))
        .expect("actual selected-layout state restore");
    assert!(!author.bypassed);
    assert!(author.parameters.values().all(|p| p.automation.is_none()));
    assert_eq!(
        author.opaque_state.as_deref().map(Vec::as_slice),
        Some(state)
    );
    let instance_id = author.id;
    let saved = serde_json::to_vec(&author).unwrap();
    let reopened: AudioProcessorInstance = serde_json::from_slice(&saved).unwrap();
    assert_eq!(author, reopened);
    let mut sequence = Sequence::new("Real CLAP configuration and native state");
    sequence.settings.audio_channel_layout = render.channel_layout;
    sequence.settings.audio_sample_rate = render.sample_rate;
    let unity = mondrian_timeline::audio::AudioChannelStrip {
        input_trim_db: 0.0,
        pre_fader: Default::default(),
        fader_db: 0.0,
        fader_automation: None,
        post_fader: Default::default(),
    };
    for channel in sequence.audio_program.track_channels.values_mut() {
        channel.strip = unity.clone();
    }
    for output in sequence.audio_program.outputs.iter_mut() {
        output.strip = unity.clone();
    }
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            AudioSourceComponentId::primary(),
        )
        .unwrap();
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(reopened);
    let period: Arc<[f32]> = (0..FRAMES * channels)
        .map(|i| (((i / channels * 29 + i % channels * 11) % 113) as f32 - 56.0) / 256.0)
        .collect::<Vec<_>>()
        .into();
    let source = ConfigPcm {
        layout: render.channel_layout,
        rate: render.sample_rate,
        period: period.clone(),
    };
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &source,
        &resolver,
        render,
        None,
    )
    .unwrap();
    assert_eq!(runtime.public_output_lookahead_frames(), 0);
    let installation = AudioOwnerInstallationSource::new();
    let (registration, verifier) = installation.register().unwrap();
    runtime.bind_native_owner_installation(verifier).unwrap();
    let claim = installation
        .reserve_install(&registration, None)
        .and_then(|r| installation.begin_commit(r))
        .unwrap()
        .commit()
        .unwrap();
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .unwrap();
    let (_, object) = accept_cold_startup(
        &mut runtime,
        &mut sequence,
        &installation,
        directory,
        label,
        Some(gain_db),
    );
    let (pid, serial) = object.expect("actual pre-DSP worker parameter readback");
    assert_ne!(pid, std::process::id());
    assert_ne!(serial, 0);
    let current = &sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
    assert_eq!(current.id, instance_id);
    assert!(current.parameters.values().all(|p| p.automation.is_none()));
    assert_eq!(
        current.opaque_state.as_deref().map(Vec::as_slice),
        Some(state)
    );
    let canonical = serde_json::to_value(&sequence).unwrap();
    // SAFETY: the PID comes from this installed Runtime's authenticated native
    // object observation. This HANDLE only observes that exact process object.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null());
    let process = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();
    let lease = owner(&runtime);
    assert_eq!(lease.editor_target().instance, instance_id);
    let mut output = Vec::with_capacity(FRAMES * channels);
    let mut block = 0;
    while output.len() < FRAMES * channels {
        let start = output.len() / channels;
        let frames = partitions[block % partitions.len()].min(FRAMES - start);
        assert!(frames > 0 && frames <= render.max_block_frames);
        let mut pcm = vec![f32::NAN; frames * channels];
        runtime
            .render_into(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: start as i64,
                    frames,
                },
                &mut pcm,
            )
            .unwrap();
        output.extend(pcm);
        let mut observations = Observations {
            native_object: Some((pid, serial)),
            ..Default::default()
        };
        observe(&mut runtime, lease, &mut observations);
        block += 1;
    }
    runtime.validate_native_owner(lease).unwrap();
    runtime
        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
        .unwrap();
    assert_eq!(serde_json::to_value(&sequence).unwrap(), canonical);
    let alive = unsafe { WaitForSingleObject(process.as_raw_handle().cast(), 0) };
    drop(runtime);
    let settled = unsafe { WaitForSingleObject(process.as_raw_handle().cast(), 0) };
    let mut exit = 0;
    let queried = unsafe { GetExitCodeProcess(process.as_raw_handle().cast(), &mut exit) };
    assert_eq!((alive, settled), (258, 0));
    assert_ne!(queried, 0);
    assert_eq!(exit, 0);
    // Empty-filter state, native-owned gain, no host curves/bypass. Check the
    // entire output from sample0, including startup, against independent math.
    let gain = 10.0_f64.powf(gain_db / 20.0) as f32;
    for (index, (&actual, &input)) in output.iter().zip(period.iter()).enumerate() {
        let expected = 0.0_f32 + input * gain;
        assert!(
            actual.is_finite() && (actual - expected).abs() <= 2.0 * f32::EPSILON,
            "{label}: sample {index}, actual={actual:?}, expected={expected:?}"
        );
    }
    output
}

#[test]
#[ignore = "real Ring Out config/state inputs, current Product and original outer Job required"]
fn external_clap_ring_out_mono_config_preserves_native_state_and_runtime_pcm() {
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").unwrap());
    let plugin = PathBuf::from(std::env::var_os("MONDRIAN_RING_OUT_TEST_PLUGIN").unwrap());
    let directory = PathBuf::from(std::env::var_os("MONDRIAN_RING_OUT_CONFIG_DIR").unwrap());
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
            .unwrap(),
    );
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(catalog.descriptors()[0].plugin_id, "com.duskaudio.ring-out");
    for (state_env, gain_db) in [
        ("MONDRIAN_RING_OUT_FLAT_STATE", 0.0_f64),
        ("MONDRIAN_RING_OUT_GAIN_STATE", -6.0_f64),
    ] {
        let path = PathBuf::from(std::env::var_os(state_env).unwrap());
        assert!(fs::metadata(&path).unwrap().len() <= 64 * 1024);
        let state = fs::read(&path).unwrap();
        assert!(!state.is_empty());
        for (layout, rate) in [
            (AudioChannelLayout::Mono, 44_100),
            (AudioChannelLayout::Mono, 48_000),
            (AudioChannelLayout::Stereo, 48_000),
        ] {
            for processing_mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
                let render = AudioRenderContract {
                    sample_rate: rate,
                    channel_layout: layout,
                    max_block_frames: 64,
                    processing_mode,
                    processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
                    public_output_lookahead_budget_frames: 4096,
                    compensation_delay_scratch_budget_bytes: 1024 * 1024,
                };
                let registration = crate::probe_clap_plugin_registration(
                    &helper,
                    &plugin,
                    "com.duskaudio.ring-out",
                    render,
                    Some(&state),
                )
                .unwrap();
                assert_eq!(
                    registration.main_audio_input,
                    crate::ClapMainAudioInput::Present
                );
                assert!(registration.auxiliary_inputs.buses.is_empty());
                assert!(registration.auxiliary_outputs.buses.is_empty());
                for (id, value) in [(7, gain_db), (8, 0.0)] {
                    assert_eq!(
                        registration
                            .parameters
                            .iter()
                            .find(|p| p.id == id)
                            .unwrap()
                            .current_value
                            .to_bits(),
                        value.to_bits()
                    );
                }
                let mut reference = None;
                for (index, partitions) in [[64, 64], [17, 47]].iter().enumerate() {
                    let label = format!("{layout:?}-{rate}-{processing_mode:?}-{gain_db}-p{index}");
                    let output = run_config_runtime(
                        catalog.clone(),
                        &state,
                        render,
                        gain_db,
                        partitions,
                        &directory,
                        &label,
                    );
                    if let Some(expected) = &reference {
                        assert_eq!(&output, expected, "fresh partition invariance");
                    } else {
                        reference = Some(output);
                    }
                }
            }
        }
    }
}

// Consume the real cold installed source before public Enter can erase or
// overwrite it. This is the same canonical transaction/plan/Source/Ack path as
// the App, bounded to four actual prefixes for this finite startup scenario.
fn accept_cold_startup(
    runtime: &mut AudioProgramRuntime,
    sequence: &mut Sequence,
    installation: &AudioOwnerInstallationSource,
    directory: &Path,
    label: &str,
    expected_native_gain: Option<f64>,
) -> (usize, Option<(u32, u64)>) {
    let cancel = ExecutionCancellationToken::new();
    let mut handoffs = 0;
    let mut native_object = None;
    for round in 0..4 {
        let lease = owner(runtime);
        assert!(lease.editor_target().continuity_epoch.is_none());
        let batch = runtime
            .poll_native_observations(&cancel)
            .expect("cold installed native poll retains typed source failures");
        write_json(
            directory,
            &format!("{label}-cold-{round}-poll.json"),
            json!({
                "schema": 1, "target_debug": format!("{:?}", lease.editor_target()),
                "raw_public_batch_debug": format!("{batch:?}"),
                "continuity_epoch_is_none": true, "author_handoffs_completed": handoffs,
                "flags_and_values_are_actual_observations": true,
            }),
        );
        if let Some(batch) = &batch {
            if let Some(logs) = &batch.diagnostics {
                assert!(
                    !logs.loss().incomplete(),
                    "cold native diagnostics incomplete"
                );
            }
            if let Some(parameters) = &batch.parameters {
                assert_eq!(parameters.target(), lease.editor_target());
                if let Some(gain) = expected_native_gain {
                    for (id, value) in [(7, gain), (8, 0.0)] {
                        let id = ParameterId::new(format!("clap.param.{id}")).unwrap();
                        let fact = parameters
                            .facts()
                            .iter()
                            .find(|fact| fact.parameter == id)
                            .expect("actual restored gain/bypass observation");
                        assert_eq!(fact.value_bits, Some(value.to_bits()));
                        assert_eq!(fact.author_value_bits, Some(value.to_bits()));
                    }
                }
                let actual = (
                    parameters.process_id().get(),
                    parameters.object_serial().get(),
                );
                if let Some(prior) = native_object {
                    assert_eq!(actual, prior);
                }
                native_object = Some(actual);
            }
        }
        let Some(proposal) = batch.and_then(|batch| batch.author) else {
            runtime
                .validate_native_author_source_closed(&cancel)
                .expect("cold source must actually close, not merely lack a public proposal");
            runtime.validate_native_owner(owner(runtime)).expect("closed exact cold owner");
            return (handoffs, native_object);
        };
        assert_eq!(proposal.target(), lease.editor_target());
        assert_eq!(proposal.sequence(), sequence.id);
        assert!(proposal.callbacks().len() <= mondrian_core::NATIVE_AUDIO_AUTHOR_CALLBACKS);
        if let Some(state) = proposal.state() {
            let bytes: Vec<u8> = state.iter().copied().collect();
            assert!(bytes.len() <= 64 * 1024, "bounded actual cold state");
            write_new(
                directory,
                &format!("{label}-cold-{round}-state.bin"),
                &bytes,
            );
        }
        let current = &sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
        assert_eq!(current.id, proposal.target().instance);
        let definition = current.definition.clone();
        let mut writes = Vec::new();
        for callback in proposal.callbacks() {
            match callback {
                NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values } => {
                    assert!(kind.refreshes_values() || values.is_empty());
                    // App native_audio_editor.rs: rescan VALUES refreshes only
                    // existing unkeyed references, never records automation.
                    if kind.refreshes_values() {
                        for (id, bits) in values {
                            let parameter =
                                current.parameters.get(id).expect("actual rescan schema");
                            if parameter
                                .automation
                                .as_ref()
                                .is_none_or(|curve| !curve.keyframes.is_empty())
                            {
                                continue;
                            }
                            writes.push(
                                NativeAudioParameterWrite {
                                    schema: Box::new(parameter.schema.clone()),
                                    value: f64::from_bits(*bits),
                                    mode: NativeAudioParameterWriteMode::Unkeyed,
                                }
                                .into(),
                            );
                        }
                    }
                }
                NativeAudioAuthorCallback::DirtyChanged(_) => {}
                other => panic!("cold startup callback needs a chosen canonical policy: {other:?}"),
            }
        }
        let request = AudioProcessorRackEditRequest {
            address: AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::ProgramOutput {
                    output_id: sequence.audio_program.outputs[0].id,
                },
                rack: AudioChannelStripRack::PreFader,
            },
            edit: AudioProcessorRackEdit::SetNativeAuthorState {
                processor_id: current.id,
                expected_definition: definition,
                update: NativeAudioAuthorUpdate {
                    parameters: writes,
                    opaque_state: proposal.state().cloned(),
                },
            },
        };
        let transaction = apply_audio_processor_rack_edit(sequence, &request)
            .expect("real atomic SetNativeAuthorState accepts the actual cold proposal");
        let canonical = serde_json::to_vec(sequence).expect("accepted canonical cold author");
        assert!(canonical.len() as u64 <= MAX_JSON);
        write_new(
            directory,
            &format!("{label}-cold-{round}-canonical-sequence.json"),
            &canonical,
        );
        let programs = [NativeAudioAuthorProgram {
            sequence: sequence.id,
            program: Arc::new(
                crate::compile_audio_program(
                    sequence,
                    crate::AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
                )
                .expect("compile only the accepted canonical cold transaction"),
            ),
        }];
        let (registration, destination) =
            installation.register().expect("same private Source registration");
        let current = installation.current_claim().unwrap().expect("actual cold installed claim");
        let reservation = installation.reserve_install(&registration, Some(&current)).unwrap();
        let mut authorization = installation.authorize_runtime_installation(reservation).unwrap();
        let mut prepared = runtime
            .prepare_native_author_parameter_handoff(&programs, &proposal, destination, &cancel)
            .expect("cold same-object handoff obeys original resource grant");
        let installed = runtime
            .install_native_author_parameter_handoff(&mut prepared, &mut authorization, &cancel)
            .expect("actual cold plan/Source CAS installation");
        assert_eq!(
            installed.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        assert!(installed.target.continuity_epoch.is_none());
        drop(prepared);
        let acknowledged = runtime
            .acknowledge_native_author_parameter_handoff(&proposal, &cancel)
            .expect("exact actual Ack only after canonical transaction and Source install");
        assert_eq!(
            acknowledged.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        assert_eq!(owner(runtime).editor_target(), acknowledged.target);
        assert!(acknowledged.target.continuity_epoch.is_none());
        write_json(
            directory,
            &format!("{label}-cold-{round}-handoff.json"),
            json!({
                "schema": 1, "proposal_revision": proposal.revision(),
                "original_callbacks_debug": format!("{:?}", proposal.callbacks()),
                "canonical_transaction_changed": transaction.changed,
                "installed_target_debug": format!("{:?}", installed.target),
                "acknowledged_target_debug": format!("{:?}", acknowledged.target),
                "continuity_preserved": true, "continuity_epoch_is_none": true,
                "current_object_observed": native_object,
            }),
        );
        handoffs += 1;
    }
    panic!("cold startup did not close within four original proposal polls; source remains unqualified");
}
fn run_runtime(
    catalog: Arc<DiscoveredClapAudioProcessorSpecResolver>,
    state: &[u8],
    period: Arc<[f32]>,
    native_bypass: bool,
    partitions: &[usize],
    label: &str,
    directory: &Path,
) {
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        ..test_render_contract()
    };
    let mut author = catalog
        .create_instance("com.duskaudio.ring-out", render, Some(state.to_vec()))
        .expect("fresh native original-state author capture");
    assert!(
        !author.bypassed,
        "host bypass must never replace native processing"
    );
    assert_eq!(
        author.opaque_state.as_deref().map(Vec::as_slice),
        Some(state)
    );
    assert_eq!(author.parameters.len(), 9, "RO meters are not author lanes");
    for (id, value) in [(7, -6.0), (8, if native_bypass { 1.0 } else { 0.0 })] {
        let mut curve = ExactAutomationCurve::new(
            ParameterId::new(format!("clap.param.{id}")).expect("actual native parameter ID"),
            value,
        )
        .expect("finite canonical curve");
        let mut key = ExactAutomationKeyframe::linear(TimelineTime::ZERO, value);
        key.interpolation_to_next = AutomationSegmentInterpolation::Hold;
        curve.set_keyframe(key).expect("public exact Sequence-time zero key");
        author
            .set_parameter_automation(curve)
            .expect("schema-validated keyed native intent");
    }
    let author_bytes = serde_json::to_vec(&author).expect("canonical author save");
    assert!(author_bytes.len() as u64 <= MAX_JSON);
    write_new(directory, &format!("{label}-author.json"), &author_bytes);
    let reopened: AudioProcessorInstance =
        serde_json::from_slice(&author_bytes).expect("canonical author reopen");
    assert_eq!(reopened, author);
    let instance_id = reopened.id;
    let mut sequence = Sequence::new("Ring Out steady native bypass through public Runtime");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE)
                .expect("public source clip"),
            AudioSourceComponentId::primary(),
        )
        .expect("public clip authoring");
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(reopened);
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    let reads = Arc::new(AtomicUsize::new(0));
    let source = PcmMedia { period: period.clone(), reads: reads.clone() };
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &source,
        &resolver,
        render,
        None,
    )
    .expect("canonical compiler/Audio DAG/Prepared Plan/native Session");
    assert!(runtime.native_observations_supported());
    let installation = AudioOwnerInstallationSource::new();
    let (registration, verifier) = installation.register().expect("real root registration");
    runtime
        .bind_native_owner_installation(verifier)
        .expect("same Runtime authority");
    let claim = installation
        .reserve_install(&registration, None)
        .and_then(|reservation| installation.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .expect("real child acknowledges exact installed Output occurrence");
    let (cold_handoffs, cold_object) = accept_cold_startup(
        &mut runtime,
        &mut sequence,
        &installation,
        directory,
        label,
        None,
    );
    for id in [7, 8] {
        let id = ParameterId::new(format!("clap.param.{id}")).unwrap();
        let actual =
            &sequence.audio_program.outputs[0].strip.pre_fader.processors[0].parameters[&id];
        assert_eq!(
            actual.automation, author.parameters[&id].automation,
            "startup rescan may not replace exact keyed user intent"
        );
        let curve = actual.automation.as_ref().unwrap();
        assert_eq!(curve.keyframes.len(), 1);
        assert_eq!(curve.keyframes[0].time, TimelineTime::ZERO);
    }
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .expect("actual installed native state entry");
    let lease = owner(&runtime);
    assert_eq!(lease.editor_target().instance, instance_id);
    runtime
        .validate_native_owner(lease)
        .expect("real same-object control admission");
    // Presentation polls are incremental. The real cold batch already supplied
    // an authenticated object; an unchanged revision need not publish it again.
    let mut observations = Observations {
        native_object: cold_object,
        ..Observations::default()
    };
    observe(&mut runtime, lease, &mut observations);
    let (pid, serial) = observations
        .native_object
        .expect("authenticated actual cold or post-Enter child parameter observation");
    if let Some(cold_object) = cold_object {
        assert_eq!((pid, serial), cold_object);
    }
    // SAFETY: PID is authenticated by the installed Runtime's retained native
    // child. This extra observation HANDLE has only query/synchronize rights.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null(), "retain exact live child observation handle");
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        258
    );
    let mut output = Vec::with_capacity(OBSERVED_FRAMES * 2);
    let mut calls = 0_usize;
    let mut start = 0_usize;
    for end in [TRANSITION_FRAMES, OBSERVED_FRAMES] {
        let mut block = 0;
        while start < end {
            let frames = partitions[block % partitions.len()].min(end - start);
            assert!(frames > 0 && frames <= 64);
            let mut pcm = vec![f32::NAN; frames * 2];
            runtime
                .render_into(
                    AudioRenderRequest {
                        transport_activity: None,
                        start_sample: start as i64,
                        frames,
                    },
                    &mut pcm,
                )
                .expect(
                    "actual contiguous public Runtime PCM; native requests remain typed failures",
                );
            assert_eq!(pcm.len(), frames * 2);
            assert!(
                pcm.iter().all(|value| value.is_finite()),
                "transition and steady PCM finite"
            );
            output.extend(pcm);
            calls += 1;
            start += frames;
            block += 1;
            observe(&mut runtime, lease, &mut observations);
        }
    }
    assert_eq!(output.len(), OBSERVED_FRAMES * 2);
    assert!(
        reads.load(Ordering::Relaxed) > 0,
        "real media resolver read"
    );
    assert_eq!(
        owner(&runtime),
        lease,
        "one native epoch/owner across the transition"
    );
    runtime
        .validate_native_owner(lease)
        .expect("same actual native object after all PCM");
    write_new(
        directory,
        &format!("{label}-output.f32le"),
        &pcm_bytes(&output),
    );
    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    drop(runtime);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
    // The public Graph sums into +0-initialized buffers. Its independent unity
    // reference keeps every raw finite value and the IEEE addition's zero sign.
    let reference: Vec<f32> = period.iter().map(|&raw| 0.0_f32 + raw).collect();
    let zero_normalizations = period
        .iter()
        .zip(&reference)
        .filter(|(raw, expected)| raw.to_bits() != expected.to_bits())
        .count();
    assert_eq!(
        zero_normalizations, 8,
        "bound input has eight negative zeros"
    );
    let mut differing = 0_usize;
    let mut maximum_difference = 0.0_f32;
    for (frame, pair) in output.chunks_exact(2).enumerate().skip(TRANSITION_FRAMES) {
        let i = (frame % FRAMES) * 2;
        for channel in 0..2 {
            let expected = reference[i + channel];
            if pair[channel].to_bits() != expected.to_bits() {
                differing += 1;
            }
            maximum_difference = maximum_difference.max((pair[channel] - expected).abs());
        }
    }
    let mut receipt = json!({
            "schema": 1, "host_bypassed": false, "native_bypass": native_bypass, "gain_out_db": -6,
            "sample_rate": 48000, "channels": 2, "mode": "Offline", "max_block_frames": 64,
            "partitions": partitions, "transition_frames_actually_rendered": TRANSITION_FRAMES,
            "steady_frames": FRAMES, "completed_frames": start, "runtime_render_calls": calls,
            "native_pid": pid, "native_object_serial": serial, "same_installed_owner_across_pcm": true,
            "parameter_polls": observations.polls, "parameter_batches": observations.parameter_batches,
            "first_parameter_facts_observation": observations.first_parameters,
            "last_parameter_facts_observation": observations.last_parameters,
            "native_diagnostic_records": observations.diagnostic_records,
            "cold_real_proposal_handoffs": cold_handoffs,
            "keyed_automation_intent_preserved": true,
            "source_reads": reads.load(Ordering::Relaxed), "finite": true,
    });
    // Keep this bounded test receipt below serde_json's macro recursion limit.
    // The two pieces still form the same flat evidence object.
    receipt.as_object_mut().unwrap().extend(
        json!({
            "native_identity_observation_origin": if cold_object.is_some() {
                "cold_authenticated_parameter_observation"
            } else {
                "post_enter_authenticated_parameter_observation"
            },
            "cold_native_object_observation": cold_object,
            "post_enter_parameter_batches": observations.parameter_batches,
            "cold_parameter_values_are_native_target_delivery_receipts": false,
            "raw_source_period_sha256": format!("{:x}", Sha256::digest(pcm_bytes(&period))),
            "unity_graph_reference_period_sha256": format!("{:x}", Sha256::digest(pcm_bytes(&reference))),
            "negative_zero_normalizations_per_period": zero_normalizations,
            "steady_reference_differing_samples": differing,
            "steady_reference_max_abs_difference": maximum_difference,
            "raw_source_bit_preservation_qualified": false,
            "output_sha256": format!("{:x}", Sha256::digest(pcm_bytes(&output))),
            "alive_before_drop_wait_status": alive, "post_drop_wait_status": settled,
            "exit_query_succeeded": queried != 0, "exit_code": exit_code,
            "native_plugin_callback_counter_instrumented": false,
            "transition_curve_oracle": false, "nonbypass_algorithm_oracle": false,
            "private_dsp_state_qualified": false, "realtime_qualified": false,
            "performance_qualified": false, "gui_qualified": false,
            "graceful_ack_qualified": false, "outer_job_qualified_by_test": false,
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    write_json(directory, &format!("{label}-receipt.json"), receipt);
    assert_eq!(alive, 258);
    assert_eq!(
        settled, 0,
        "existing Runtime Drop must settle the exact real child"
    );
    assert_ne!(queried, 0);
    assert_eq!(exit_code, 0, "forced/error exit cannot qualify lifecycle");
    if native_bypass {
        assert_eq!(
            differing, 0,
            "steady Runtime PCM must match the unity Graph reference bit-for-bit"
        );
    } else {
        assert!(
            differing > 0 && maximum_difference > 0.01,
            "fresh -6dB active control must distinguish unity Graph passthrough from native bypass"
        );
    }
}

#[test]
#[ignore = "pinned real Ring Out case, current Product and frozen outer native Job required"]
fn external_clap_ring_out_runtime_steady_native_bypass_pcm() {
    let path =
        PathBuf::from(std::env::var_os(RUNTIME_CASE_ENV).expect("Ring Out Runtime case path"));
    let mut case_lease = Lease::open(
        &Binding {
            bytes: fs::metadata(&path).expect("case metadata").len(),
            path,
            sha256: std::env::var(RUNTIME_CASE_HASH_ENV).expect("case SHA256"),
        },
        MAX_JSON,
    );
    let bytes = case_lease.read();
    let case: VendorCase =
        serde_json::from_value(strict_json(&bytes)).expect("strict existing vendor case schema");
    assert_eq!(case.schema, 1);
    assert_eq!(case.format, Format::Clap);
    assert_eq!(case.expected_identity, "com.duskaudio.ring-out");
    assert_eq!(case.expected_parameter_count, 11);
    assert_eq!(case.bypass_native_id, 8);
    assert_eq!(case.plugin.bytes, 1_658_880);
    assert_eq!(
        case.plugin.sha256,
        "df86e10f2b0aada4a3eb5f4f0bda315a941960b587b4b9513cd7ba8a9f8bd99b"
    );
    assert_eq!(case.opaque_state.bytes, 184);
    assert_eq!(
        case.opaque_state.sha256,
        "1cc8e19194558d2b72358549fdf9516fb41631a58b1d784eb95e70036095b7c1"
    );
    assert_eq!(
        case.captured_list_response.sha256,
        "630f68f65df920769cf235d3dac0795be64e72784f6e9ee248565721a03a629a"
    );
    assert_eq!(
        case.captured_probe_response.sha256,
        "ff825debe4a0f32249c4aff68e2e3e8f20e490accc236698b14ab15f798981c9"
    );
    assert_eq!(
        case.pcm.sha256,
        "0c54bf849a05bea1f80494ba50adddd3828c31736da3c179a5cc735cb2450bb2"
    );
    no_reparse(&case.evidence_directory);
    assert!(fs::metadata(&case.evidence_directory).unwrap().is_dir());
    assert!(fs::read_dir(&case.evidence_directory).unwrap().next().is_none());
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let mut opaque = Lease::open(&case.opaque_state, 64 * 1024);
    let mut list_lease = Lease::open(&case.captured_list_response, MAX_JSON);
    let mut probe_lease = Lease::open(&case.captured_probe_response, MAX_JSON);
    let mut pcm_lease = Lease::open(&case.pcm, (FRAMES * 2 * 4) as u64);
    let state = opaque.read();
    let list = strict_json(&list_lease.read());
    let probe = strict_json(&probe_lease.read());
    assert_eq!(list["probe"], Value::Null);
    assert_eq!(list["descriptors"].as_array().unwrap().len(), 1);
    assert_eq!(list["descriptors"][0]["plugin_id"], case.expected_identity);
    assert_eq!(probe["descriptors"], json!([]));
    assert_eq!(probe["probe"]["plugin_id"], case.expected_identity);
    assert_eq!(probe["probe"]["captured_state"], json!(state));
    let captured: Vec<ClapParameterDescriptor> =
        serde_json::from_value(probe["probe"]["parameters"].clone())
            .expect("bound original actual native metadata");
    assert_eq!(captured.len(), 11);
    assert_eq!(
        captured.iter().filter(|p| p.read_only).map(|p| p.id).collect::<BTreeSet<_>>(),
        BTreeSet::from([9, 10])
    );
    assert_eq!(probe["probe"]["auxiliary_buses"], json!([]));
    let period: Arc<[f32]> = deterministic_pcm().into();
    assert_eq!(
        pcm_lease.read(),
        pcm_bytes(&period),
        "independent hash-bound stereo source"
    );
    // This is a Graph oracle, not a change to the bound source or native input.
    // render::NodeBuffers::clear + dsp::add normalize -0 at unity. Do not skip
    // zeros or accept a tolerance: compare all 8192 expected output bit patterns.
    let reference: Vec<f32> = period.iter().map(|&raw| 0.0_f32 + raw).collect();
    let zero_normalizations = period
        .iter()
        .zip(&reference)
        .filter(|(raw, expected)| raw.to_bits() != expected.to_bits())
        .count();
    assert_eq!(zero_normalizations, 8);
    assert!(period.iter().zip(&reference).all(|(raw, expected)| {
        expected.to_bits()
            == if raw.to_bits() == 0x8000_0000 {
                0
            } else {
                raw.to_bits()
            }
    }));
    write_new(
        &case.evidence_directory,
        "unity-graph-reference-period.f32le",
        &pcm_bytes(&reference),
    );
    write_new(&case.evidence_directory, "case.json", &bytes);
    write_new(
        &case.evidence_directory,
        "original-default-state.bin",
        &state,
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };
    let actual = probe_clap_plugin_registration(
        &helper.path,
        &plugin.path,
        &case.expected_identity,
        render,
        Some(&state),
    )
    .expect("fresh actual restored metadata with unchanged production validator");
    assert_eq!(
        actual.binary_sha256.as_slice(),
        decode_hash(&plugin.sha256).as_slice()
    );
    assert_eq!(
        actual.parameters.iter().map(|p| p.id).collect::<BTreeSet<_>>(),
        (0..11).collect()
    );
    assert_eq!(
        actual
            .parameters
            .iter()
            .filter(|p| p.read_only)
            .map(|p| p.id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([9, 10])
    );
    assert!(actual.auxiliary_inputs.buses.is_empty());
    assert_eq!(actual.execution_contract.algorithmic_latency_frames(), 0);
    assert_eq!(actual.execution_contract.tail(), AudioProcessorTail::None);
    write_json(
        &case.evidence_directory,
        "fresh-restored-metadata.json",
        json!({
            "parameters": actual.parameters, "execution_contract": format!("{:?}", actual.execution_contract),
            "names_and_current_values_are_observations_only": true, "auxiliary_inputs": [],
        }),
    );
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            helper.path.clone(),
            [plugin.path.clone()],
        )
        .expect("real canonical catalog discovery"),
    );
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(catalog.descriptors()[0].plugin_id, case.expected_identity);
    // tau=10ms at fs/32, coefficient grid32, explicit clamp<1e-4 then
    // memcpy dry in the pinned source. Render 6144 frames (128ms) before the
    // 4096-frame oracle; the original first-block gate remains untouched.
    for (label, partition) in [("block64", &[64_usize][..]), ("mixed31-33", &[31, 33][..])] {
        for bypass in [true, false] {
            run_runtime(
                catalog.clone(),
                &state,
                period.clone(),
                bypass,
                partition,
                &format!(
                    "{label}-{}",
                    if bypass {
                        "native-bypass"
                    } else {
                        "active-control"
                    }
                ),
                &case.evidence_directory,
            );
        }
    }
    write_json(
        &case.evidence_directory,
        "result.json",
        json!({
            "schema": 1, "actual_identity": case.expected_identity,
            "case_sha256": case_lease.sha256, "helper_sha256": helper.sha256, "plugin_sha256": plugin.sha256,
            "state_sha256": opaque.sha256, "source_pcm_period_sha256": pcm_lease.sha256,
            "original_state_bytes": state.len(), "runs": 4, "canonical_runtime_and_output_rack": true,
            "native_bypass_steady_pcm_matches_unity_graph_bits": true,
            "raw_source_bit_preservation_qualified": false,
            "raw_source_period_sha256": pcm_lease.sha256,
            "unity_graph_reference_period_sha256": format!("{:x}", Sha256::digest(pcm_bytes(&reference))),
            "negative_zero_normalizations_per_period": zero_normalizations,
            "active_control_distinguishes_passthrough": true,
            "transition_frames_per_run": TRANSITION_FRAMES, "steady_frames_per_run": FRAMES,
            "transition_is_actually_processed": true, "sleep_warmup": false,
            "transition_algorithm_qualified": false, "nonbypass_algorithm_qualified": false,
            "nondefault_state_discrimination": false, "private_dsp_state_qualified": false,
            "native_gui_qualified": false, "realtime_qualified": false, "performance_qualified": false,
            "full_clap_qualified": false, "other_platforms_qualified": false,
        }),
    );
    drop((
        case_lease,
        helper,
        plugin,
        opaque,
        list_lease,
        probe_lease,
        pcm_lease,
    ));
}
