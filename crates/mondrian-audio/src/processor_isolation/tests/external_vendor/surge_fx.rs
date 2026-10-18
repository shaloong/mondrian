//! Finite PCM/lifecycle observations for the pinned external Effects case.
//! Neither native-bypass precision nor fresh-output equality is a DSP oracle.

use super::vst3_runtime_pcm::{observe_runtime_pcm, RuntimeObservationConfiguration};
use super::*;

#[test]
#[ignore = "pinned external Effects case, current Product and frozen outer native Job required"]
fn external_vst3_effect_auxiliary_restore_pcm_observations() {
    let path = PathBuf::from(std::env::var_os(CASE_ENV).expect("external case path"));
    let mut case_lease = Lease::open(
        &Binding {
            bytes: fs::metadata(&path).expect("case metadata").len(),
            path,
            sha256: std::env::var(CASE_HASH_ENV).expect("case SHA256"),
        },
        MAX_JSON,
    );
    let bytes = case_lease.read();
    let case: VendorCase =
        serde_json::from_value(strict_json(&bytes)).expect("strict existing case schema");
    assert_eq!(case.schema, 1);
    assert_eq!(case.format, Format::Vst3);
    assert_eq!(case.expected_identity, "ABCDEF019182FAEB566D624153465854");
    assert_eq!(case.expected_parameter_count, 14);
    assert_eq!(case.bypass_native_id, 1_652_125_811);
    assert_eq!(case.plugin.bytes, 9_753_088);
    assert_eq!(
        case.plugin.sha256,
        "3b9f6c9b3363491a37721ed2cb9f080eb0d4abe1ce545a942e4fbb30b3e93050"
    );
    assert_eq!(case.opaque_state.bytes, 1_150);
    assert_eq!(
        case.opaque_state.sha256,
        "e24081fd2018d5a85f6de16ac65ea8f865cafa7bf32fde35b6c7eb0f78977f02"
    );
    // Bind the exact accepted capture bytes, not a generated/default blob.
    assert_eq!(
        case.captured_probe_response.sha256,
        "d9dfe2708eb0a659f2a27898341ec849aaa6e9519c07ea04ff459708ae3d3926"
    );
    assert_eq!(
        case.captured_list_response.sha256,
        "6f899880fded2bdefdef2a4f511015d9a38b34de03cdfd12b6fdc37d3445db08"
    );
    no_reparse(&case.evidence_directory);
    assert_eq!(
        fs::read_dir(&case.evidence_directory)
            .expect("fresh evidence directory")
            .count(),
        0
    );
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let mut state_lease = Lease::open(&case.opaque_state, 128 * 1024);
    let mut list_lease = Lease::open(&case.captured_list_response, MAX_JSON);
    let mut probe_lease = Lease::open(&case.captured_probe_response, MAX_JSON);
    let mut input_lease = Lease::open(&case.pcm, (FRAMES * 2 * 4) as u64);
    let state = state_lease.read();
    let list = strict_json(&list_lease.read());
    let captured = strict_json(&probe_lease.read());
    let input = deterministic_pcm();
    assert_eq!(
        input_lease.read(),
        pcm_bytes(&input),
        "existing asymmetric stereo input"
    );
    write_new(&case.evidence_directory, "case.json", &bytes);
    write_new(
        &case.evidence_directory,
        "restored-opaque-state.bin",
        &state,
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };
    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(
        helper.path.clone(),
        [plugin.path.clone()],
    )
    .expect("actual production VST3 discovery");
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(json!(catalog.descriptors()), list["descriptors"]);
    assert_eq!(captured["kind"], "probe");
    assert_eq!(captured["class_id"], case.expected_identity);
    assert_eq!(captured["latency_frames"], 0);
    assert_eq!(captured["tail_frames"], 96_000);
    assert_eq!(captured["captured_state"], json!(state));
    assert_eq!(
        captured["auxiliary_buses"],
        json!([{
            "bus_key":"vst3.audio-input.1", "channel_layout":AudioChannelLayout::Stereo
        }])
    );
    let parameters: Vec<Vst3ParameterDescriptor> =
        serde_json::from_value(captured["parameters"].clone()).expect("captured descriptors");
    let current =
        serde_json::from_value(captured["current_values"].clone()).expect("captured values");
    let bypass = parameters
        .iter()
        .find(|p| p.id == case.bypass_native_id)
        .expect("native bypass descriptor");
    assert_eq!(
        bypass.flags & 65_536,
        65_536,
        "native kIsBypass identity only"
    );
    assert_eq!(bypass.step_count, 1);
    let facts = Facts::Vst3(parameters, current);
    let catalog = Catalog::Vst3(catalog);
    let author = catalog.instance(&case.expected_identity, render, &state);
    facts.check_author(&author);
    assert_eq!(author.parameters.len(), 14);
    assert!(
        !author.bypassed,
        "host bypass must never short-circuit native processing"
    );
    assert_eq!(
        author.opaque_state.as_ref().map(AsRef::as_ref),
        Some(state.as_slice())
    );
    let serialized = serde_json::to_vec(&author).expect("canonical author serialization");
    let restored: AudioProcessorInstance =
        serde_json::from_slice(&serialized).expect("canonical author reload");
    assert_eq!(restored, author);
    let first = observe_pcm(
        &catalog,
        &restored,
        render,
        &input,
        false,
        "restore-a",
        &case.evidence_directory,
    );
    let second = observe_pcm(
        &catalog,
        &restored,
        render,
        &input,
        false,
        "restore-b",
        &case.evidence_directory,
    );
    let bypass = observe_pcm(
        &catalog,
        &restored,
        render,
        &input,
        true,
        "native-bypass",
        &case.evidence_directory,
    );
    let differences =
        |a: &[f32], b: &[f32]| a.iter().zip(b).filter(|(x, y)| x.to_bits() != y.to_bits()).count();
    write_new(&case.evidence_directory, "observations.json", &serde_json::to_vec_pretty(&json!({
        "case_sha256":case_lease.sha256,"plugin_sha256":plugin.sha256,"state_sha256":state_lease.sha256,
        "frames_per_run":FRAMES,"native_bypass_differing_samples":differences(&input,&bypass),
        "fresh_restores_differing_samples":differences(&first,&second),
        "restore_a_differing_from_input":differences(&first,&input),
        "difference_counts_are_observations_only":true,
        "same_version_effects_bypass_precision_basis_available":false,
        "independent_dsp_oracle_available":false,"nondefault_state_qualified":false,
        "dsp_algorithm_qualified":false,"outer_ownership_qualified_by_test":false,
        "app_playback_export_qualified":false,"realtime_or_performance_qualified":false
    })).expect("finite observation record"));
    // All leases survive admissions, actual calls and native destruction.
    drop((
        case_lease,
        helper,
        plugin,
        state_lease,
        list_lease,
        probe_lease,
        input_lease,
    ));
}

fn observe_pcm(
    catalog: &Catalog,
    source: &AudioProcessorInstance,
    render: AudioRenderContract,
    input: &[f32],
    native_bypass: bool,
    label: &str,
    directory: &Path,
) -> Vec<f32> {
    let mut author = source.clone();
    let bypass_id = ParameterId::new("vst3.param.1652125811").expect("observed native bypass ID");
    if native_bypass {
        author
            .set_parameter_automation(
                ExactAutomationCurve::new(bypass_id.clone(), 1.0).expect("native bypass value"),
            )
            .expect("canonical author change");
    }
    assert!(!author.bypassed);
    write_new(
        directory,
        &format!("{label}-author.json"),
        &serde_json::to_vec(&author).expect("actual author input"),
    );
    let spec = catalog.resolve(AudioProcessorPrepareRequest::new(
        AudioProcessorOccurrence {
            instance_id: author.id,
            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
            insertion: AudioProcessorInsertionPoint::PreFader,
        },
        &author.definition,
        &author.parameters,
        author.opaque_state.as_ref().map(AsRef::as_ref),
        render,
    ));
    let auxiliary = spec.auxiliary_inputs.clone();
    assert_eq!(auxiliary.buses.len(), 1);
    assert_eq!(auxiliary.buses[0].bus_key, "vst3.audio-input.1");
    assert_eq!(
        auxiliary.buses[0].channel_layout,
        AudioChannelLayout::Stereo
    );
    // Keep the original validation.shutdown hard gate and all spec deadlines.
    let factory = IsolatedAudioProcessorFactory::prepare(spec)
        .expect("native prepare and validation shutdown");
    let mut processor = factory.create(None).expect("fresh actual native process");
    let before = processor
        .native_audio_object_observation()
        .expect("authenticated observation")
        .expect("native object");
    let pid = before.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: PID is authenticated by the retained child supervisor; query and
    // synchronize rights only. This handle never terminates/mutates the child.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null(), "exact child observation handle");
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
        258
    );
    processor.enter_state(73).expect("actual native state entry");
    let entered = processor
        .native_audio_object_observation()
        .expect("entered observation")
        .expect("entered object");
    assert_eq!(entered.process_id(), before.process_id());
    let ids: Vec<_> = author.parameters.keys().cloned().collect();
    let mut output = Vec::with_capacity(input.len());
    let mut calls = 0;
    let mut failure = None;
    for offset in (0..FRAMES).step_by(64) {
        let frames = 64.min(FRAMES - offset);
        let start = 73 + offset as i64;
        let mut io = NativeSidechainAudioIo {
            main: input[offset * 2..(offset + frames) * 2].to_vec(),
            inputs: vec![(auxiliary.buses[0].bus_key.clone(), vec![0.0; frames * 2])],
        };
        let events = if native_bypass && offset == 0 {
            vec![AudioParameterEvent { sample_offset: 0, value: 1.0 }]
        } else {
            Vec::new()
        };
        let ranges: Vec<_> = ids
            .iter()
            .map(|id| {
                if *id == bypass_id && !events.is_empty() {
                    0..1
                } else {
                    0..0
                }
            })
            .collect();
        let context = AudioProcessorProcessContext::new(
            AudioRenderRequest { start_sample: start, frames },
            AudioSampleRate::new(48_000).expect("actual sample grid"),
            AudioChannelLayout::Stereo,
            AudioProcessingMode::Offline,
            AudioKernelBackend::ScalarReference,
        );
        if let Err(error) = processor.process(
            context,
            &mut io,
            AudioParameterEventBatch::new(start, frames, &ids, &ranges, &events),
        ) {
            failure = Some(error.to_string());
            break;
        }
        calls += 1;
        assert_eq!(io.main.len(), frames * 2, "exact stereo output extent");
        output.extend(io.main);
    }
    write_new(
        directory,
        &format!("{label}-output.f32le"),
        &pcm_bytes(&output),
    );
    let after = processor.native_audio_object_observation();
    let same_object = matches!(&after, Ok(Some(object)) if *object == entered);
    let alive_before_drop = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    drop(processor);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit_code = 0_u32;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
    let finite = output.iter().all(|v| v.is_finite());
    write_new(directory, &format!("{label}-receipt.json"), &serde_json::to_vec_pretty(&json!({
        "native_pid":pid,"object_serial_before":before.object_serial().get(),"object_serial_entered":entered.object_serial().get(),
        "same_authenticated_object_after_calls":same_object,"after_observation":format!("{after:?}"),
        "native_bypass_event":native_bypass,"host_bypassed":false,"all_other_parameter_events_empty":true,
        "sample_rate":48000,"channels":2,"auxiliary_key":auxiliary.buses[0].bus_key,
        "auxiliary_silence":true,"first_sample":73,"requested_frames":FRAMES,
        "completed_frames":output.len()/2,"successful_worker_process_calls":calls,"block_frames":64,
        "process_failure":failure,"finite":finite,"alive_before_drop_wait_status":alive_before_drop,
        "post_drop_wait_status":settled,"exit_query_succeeded":queried != 0,"exit_code":exit_code,
        "input_sha256":format!("{:x}",Sha256::digest(pcm_bytes(input))),
        "output_sha256":format!("{:x}",Sha256::digest(pcm_bytes(&output))),
        "native_plugin_callback_counter_instrumented":false,"graceful_ack_qualified":false
    })).expect("actual PCM and lifecycle evidence"));
    assert!(failure.is_none(), "actual process failure: {failure:?}");
    assert_eq!(calls, FRAMES / 64);
    assert_eq!(output.len(), input.len());
    assert!(finite);
    assert!(same_object);
    assert_eq!(alive_before_drop, 258);
    assert_eq!(settled, 0, "existing Drop must settle the actual child");
    assert_ne!(queried, 0);
    assert_eq!(exit_code, 0, "forced/error exit cannot qualify lifecycle");
    output
}

use mondrian_core::{MusicalTimeReference, TimelineTime};

#[test]
#[ignore = "pinned external Effects case, current Product and frozen outer native Job required"]
fn external_vst3_effect_runtime_restore_pcm_observations() {
    runtime_restore_pcm_observations(None);
}

#[test]
#[ignore = "pinned Surge Effects case and fresh normal Product; canonical Some musical pure audio, not a tempo-algorithm or shutdown fix"]
fn external_vst3_effect_musical_reference_runtime_restore_pcm_observations() {
    runtime_restore_pcm_observations(Some(
        MusicalTimeReference::new(
            TimelineTime::new(-19, 7).unwrap(),
            TimelineTime::new(3, 4).unwrap(),
        )
        .unwrap(),
    ));
}

fn runtime_restore_pcm_observations(musical_time_reference: Option<MusicalTimeReference>) {
    let path = PathBuf::from(std::env::var_os(CASE_ENV).expect("external case path"));
    let mut case_lease = Lease::open(
        &Binding {
            bytes: fs::metadata(&path).expect("case metadata").len(),
            path,
            sha256: std::env::var(CASE_HASH_ENV).expect("case SHA256"),
        },
        MAX_JSON,
    );
    let bytes = case_lease.read();
    let case: VendorCase =
        serde_json::from_value(strict_json(&bytes)).expect("strict existing case schema");
    assert_eq!(case.schema, 1);
    assert_eq!(case.format, Format::Vst3);
    assert_eq!(case.expected_identity, "ABCDEF019182FAEB566D624153465854");
    assert_eq!(case.expected_parameter_count, 14);
    assert_eq!(case.bypass_native_id, 1_652_125_811);
    assert_eq!(case.plugin.bytes, 9_753_088);
    assert_eq!(
        case.plugin.sha256,
        "3b9f6c9b3363491a37721ed2cb9f080eb0d4abe1ce545a942e4fbb30b3e93050"
    );
    assert_eq!(case.opaque_state.bytes, 1_150);
    assert_eq!(
        case.opaque_state.sha256,
        "e24081fd2018d5a85f6de16ac65ea8f865cafa7bf32fde35b6c7eb0f78977f02"
    );
    // Bind the exact accepted capture bytes, not a generated/default blob.
    assert_eq!(
        case.captured_probe_response.sha256,
        "d9dfe2708eb0a659f2a27898341ec849aaa6e9519c07ea04ff459708ae3d3926"
    );
    assert_eq!(
        case.captured_list_response.sha256,
        "6f899880fded2bdefdef2a4f511015d9a38b34de03cdfd12b6fdc37d3445db08"
    );
    no_reparse(&case.evidence_directory);
    assert_eq!(
        fs::read_dir(&case.evidence_directory)
            .expect("fresh evidence directory")
            .count(),
        0
    );
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let mut state_lease = Lease::open(&case.opaque_state, 128 * 1024);
    let mut list_lease = Lease::open(&case.captured_list_response, MAX_JSON);
    let mut probe_lease = Lease::open(&case.captured_probe_response, MAX_JSON);
    let mut input_lease = Lease::open(&case.pcm, (FRAMES * 2 * 4) as u64);
    let state = state_lease.read();
    let list = strict_json(&list_lease.read());
    let captured = strict_json(&probe_lease.read());
    let input = deterministic_pcm();
    assert_eq!(
        input_lease.read(),
        pcm_bytes(&input),
        "existing asymmetric stereo input"
    );
    write_new(&case.evidence_directory, "case.json", &bytes);
    write_new(
        &case.evidence_directory,
        "original-opaque-state.bin",
        &state,
    );
    assert_eq!(captured["kind"], "probe");
    assert_eq!(captured["class_id"], case.expected_identity);
    assert_eq!(captured["captured_state"], json!(state));
    assert_eq!(captured["latency_frames"], 0);
    assert_eq!(captured["tail_frames"], 96_000);
    assert_eq!(
        captured["auxiliary_buses"],
        json!([{"bus_key":"vst3.audio-input.1", "channel_layout":AudioChannelLayout::Stereo}])
    );
    let parameters: Vec<Vst3ParameterDescriptor> =
        serde_json::from_value(captured["parameters"].clone()).expect("captured VST3 schemas");
    let values = serde_json::from_value(captured["current_values"].clone())
        .expect("captured normalized VST3 values");
    assert_eq!(parameters.len(), 14);
    let bypass = parameters.iter().find(|p| p.id == case.bypass_native_id).unwrap();
    assert_eq!(bypass.flags & 65_536, 65_536);
    assert_eq!(bypass.step_count, 1);
    let facts = Facts::Vst3(parameters, values);
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(
            helper.path.clone(),
            [plugin.path.clone()],
        )
        .expect("actual production VST3 discovery"),
    );
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(json!(catalog.descriptors()), list["descriptors"]);
    let reference: Vec<f32> = input.iter().map(|&raw| 0.0_f32 + raw).collect();
    assert_eq!(
        input
            .iter()
            .zip(&reference)
            .filter(|(raw, normalized)| raw.to_bits() != normalized.to_bits())
            .count(),
        8,
        "the public unity Graph normalizes the bound eight negative zeros"
    );
    assert!(input.iter().zip(&reference).all(|(raw, normalized)| {
        normalized.to_bits()
            == if raw.to_bits() == 0x8000_0000 {
                0
            } else {
                raw.to_bits()
            }
    }));
    write_new(
        &case.evidence_directory,
        "unity-graph-reference.f32le",
        &pcm_bytes(&reference),
    );
    let period: Arc<[f32]> = input.clone().into();
    let first = observe_runtime_pcm(
        catalog.clone(),
        &state,
        period.clone(),
        &facts,
        RuntimeObservationConfiguration {
            case: &case,
            constant_parameter_values: &[],
            sequence_name: "Surge original-state PCM through canonical Runtime",
            auxiliary_input_key: Some("vst3.audio-input.1"),
            musical_time_reference,
            native_bypass: false,
        },
        "restore-a",
        &case.evidence_directory,
    );
    let second = observe_runtime_pcm(
        catalog.clone(),
        &state,
        period.clone(),
        &facts,
        RuntimeObservationConfiguration {
            case: &case,
            constant_parameter_values: &[],
            sequence_name: "Surge original-state PCM through canonical Runtime",
            auxiliary_input_key: Some("vst3.audio-input.1"),
            musical_time_reference,
            native_bypass: false,
        },
        "restore-b",
        &case.evidence_directory,
    );
    let bypass = observe_runtime_pcm(
        catalog,
        &state,
        period,
        &facts,
        RuntimeObservationConfiguration {
            case: &case,
            constant_parameter_values: &[],
            sequence_name: "Surge original-state PCM through canonical Runtime",
            auxiliary_input_key: Some("vst3.audio-input.1"),
            musical_time_reference,
            native_bypass: true,
        },
        "native-bypass",
        &case.evidence_directory,
    );
    let differences =
        |a: &[f32], b: &[f32]| a.iter().zip(b).filter(|(x, y)| x.to_bits() != y.to_bits()).count();
    write_json(
        &case.evidence_directory,
        "observations.json",
        json!({
            "schema":1, "canonical_runtime_and_output_rack":true, "runs":3,
            "case_sha256":case_lease.sha256, "helper_sha256":helper.sha256,
            "plugin_sha256":plugin.sha256, "original_state_sha256":state_lease.sha256,
            "original_state_bytes":state.len(), "source_period_sha256":input_lease.sha256,
            "frames_per_run":FRAMES, "fresh_restores_differing_samples":differences(&first,&second),
            "native_bypass_vs_unity_graph_differing_samples":differences(&bypass,&reference),
            "restore_a_vs_unity_graph_differing_samples":differences(&first,&reference),
            "unity_graph_reference_sha256":format!("{:x}",Sha256::digest(pcm_bytes(&reference))),
            "difference_counts_are_observations_only":true, "independent_dsp_oracle_available":false,
            "bypass_precision_or_algorithm_qualified":false, "nondefault_state_discrimination":false,
            "live_state_capture_or_author_ack_performed":false, "private_dsp_state_qualified":false,
            "app_playback_export_qualified":false, "realtime_or_performance_qualified":false,
            "outer_ownership_qualified_by_test":false
        }),
    );
    drop((
        case_lease,
        helper,
        plugin,
        state_lease,
        list_lease,
        probe_lease,
        input_lease,
    ));
}
