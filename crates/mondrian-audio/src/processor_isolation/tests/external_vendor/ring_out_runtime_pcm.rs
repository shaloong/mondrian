//! One finite real-plugin Runtime/Graph steady native-bypass PCM qualification.
//! The fixed transition is rendered, not skipped or replaced with a sleep.

use super::*;
use crate::{
    AudioContinuityEpoch, AudioDecodedSource, AudioMediaResolver, AudioProgramRuntime,
    AudioStateEntry, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioOwnerInstallationSource, AudioSourceComponentId, ExecutionCancellationToken,
    TimelineTime,
};
use mondrian_timeline::{Clip, Sequence};
use std::sync::atomic::{AtomicUsize, Ordering};

const RUNTIME_CASE_ENV: &str = "MONDRIAN_RING_OUT_RUNTIME_PCM_CASE";
const RUNTIME_CASE_HASH_ENV: &str = "MONDRIAN_RING_OUT_RUNTIME_PCM_CASE_SHA256";
const TRANSITION_FRAMES: usize = 6144;
const OBSERVED_FRAMES: usize = TRANSITION_FRAMES + FRAMES;

fn write_json(directory: &Path, name: &str, value: Value) {
    let bytes = serde_json::to_vec_pretty(&value).expect("complete bounded PCM observation");
    assert!(bytes.len() as u64 <= MAX_JSON, "bounded JSON evidence");
    write_new(directory, name, &bytes);
}

// A bound source in the normal media resolver. It has no plugin/output access.
// Repeat the independently pinned 4096-frame input at exact sample positions.
struct PcmMedia {
    period: Arc<[f32]>,
    reads: Arc<AtomicUsize>,
}
impl AudioMediaResolver for PcmMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        if rate != 48_000 {
            return Err("bound PCM source rate".to_owned());
        }
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Stereo,
            Arc::new(PcmMedia {
                period: self.period.clone(),
                reads: self.reads.clone(),
            }),
        ))
    }
}
impl AudioDecodedSource for PcmMedia {
    fn read_interleaved(
        &self,
        start: i64,
        frames: usize,
        out: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        if cancellation.is_canceled() || out.len() != frames * 2 {
            return Err("bound PCM source cancellation/extent".to_owned());
        }
        self.reads.fetch_add(1, Ordering::Relaxed);
        for (offset, pair) in out.chunks_exact_mut(2).enumerate() {
            let sample = start.checked_add(offset as i64).ok_or("source time overflow")?;
            if sample < 0 {
                pair.fill(0.0);
            } else {
                let i = (sample as usize % FRAMES) * 2;
                pair.copy_from_slice(&self.period[i..i + 2]);
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Cursors {
    sessions: Vec<crate::AudioSessionCursorEvidence>,
    processors: Vec<crate::AudioProcessorCursorEvidence>,
}
impl crate::AudioContinuityEvidenceVisitor for Cursors {
    fn session(
        &mut self,
        evidence: crate::AudioSessionCursorEvidence,
    ) -> Result<(), crate::AudioExecutionError> {
        self.sessions.push(evidence);
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: crate::AudioProcessorCursorEvidence,
    ) -> Result<(), crate::AudioExecutionError> {
        self.processors.push(evidence);
        Ok(())
    }
}
fn owner(runtime: &AudioProgramRuntime) -> crate::AudioNativeProcessorOwnerLease {
    let mut cursors = Cursors::default();
    runtime.visit_continuity_evidence(&mut cursors).expect("actual Runtime cursors");
    assert_eq!((cursors.sessions.len(), cursors.processors.len()), (1, 1));
    let processor = cursors.processors[0];
    let crate::AudioProcessorCursorState::Active { epoch, .. } = processor.state else {
        panic!("actual native occurrence must have entered its epoch");
    };
    runtime
        .native_owner_lease(crate::AudioNativeProcessorAddress {
            root: cursors.sessions[0].identity,
            local: processor.identity,
            occurrence: processor.occurrence,
            epoch: Some(epoch),
        })
        .expect("actual installed Runtime-issued native lease")
}

#[derive(Default)]
struct Observations {
    polls: usize,
    parameter_batches: usize,
    diagnostic_records: usize,
    native_object: Option<(u32, u64)>,
    first_parameters: Option<String>,
    last_parameters: Option<String>,
}
fn observe(
    runtime: &mut AudioProgramRuntime,
    lease: crate::AudioNativeProcessorOwnerLease,
    evidence: &mut Observations,
) {
    evidence.polls += 1;
    let batch = runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("actual typed native control poll; no restart/error is cleared");
    let Some(batch) = batch else {
        return;
    };
    // Metadata and RO meter facts use the existing observation projector. A
    // real author transaction needs its canonical handoff, never a fake Ack.
    assert!(
        batch.author.is_none(),
        "unexpected real author work retained: {:?}",
        batch.author
    );
    if let Some(logs) = batch.diagnostics {
        assert!(!logs.loss().incomplete(), "native diagnostic loss");
        evidence.diagnostic_records += logs.records().len();
    }
    if let Some(parameters) = batch.parameters {
        assert_eq!(parameters.target(), lease.editor_target());
        let object = (
            parameters.process_id().get(),
            parameters.object_serial().get(),
        );
        assert_ne!(object.0, std::process::id());
        if let Some(expected) = evidence.native_object {
            assert_eq!(object, expected);
        }
        evidence.native_object = Some(object);
        evidence.parameter_batches += 1;
        let snapshot = format!("{:?}", parameters.facts());
        if evidence.first_parameters.is_none() {
            evidence.first_parameters = Some(snapshot.clone());
        }
        evidence.last_parameters = Some(snapshot);
    }
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
        author
            .set_parameter_automation(
                ExactAutomationCurve::new(
                    ParameterId::new(format!("clap.param.{id}"))
                        .expect("actual native parameter ID"),
                    value,
                )
                .expect("finite canonical constant"),
            )
            .expect("schema-validated native parameter authoring");
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
    let mut observations = Observations::default();
    observe(&mut runtime, lease, &mut observations);
    let (pid, serial) = observations
        .native_object
        .expect("authenticated actual child parameter snapshot");
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
                    AudioRenderRequest { start_sample: start as i64, frames },
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
            "source_reads": reads.load(Ordering::Relaxed), "finite": true,
    });
    // Keep this bounded test receipt below serde_json's macro recursion limit.
    // The two pieces still form the same flat evidence object.
    receipt.as_object_mut().unwrap().extend(
        json!({
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
