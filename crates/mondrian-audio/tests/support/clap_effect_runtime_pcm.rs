//! Finite third-party CLAP PCM through the original canonical Runtime/DAG.
//! No native-bypass precision, algorithm, tail, Note inventory or RT oracle.

use super::audio::{
    probe_clap_plugin_registration, AudioContinuityEpoch, AudioProcessingMode,
    AudioProcessorCursorState, AudioProcessorFactory, AudioProcessorHostError,
    AudioProcessorPrepareRequest, AudioProcessorResolver, AudioProcessorTail, AudioProgramRuntime,
    AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant, AudioSessionCursorState,
    AudioStateEntry, ClapMainAudioInput, ClapParameterDescriptor,
    DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use super::external_vendor_support::*;
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationSource, AudioSourceComponentId,
    ExecutionCancellationToken, TimelineTime,
};
use mondrian_timeline::{Clip, Sequence};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, WaitForSingleObject};

const RUNTIME_CASE_ENV: &str = "MONDRIAN_EXTERNAL_CLAP_RUNTIME_PCM_CASE";
const RUNTIME_CASE_HASH_ENV: &str = "MONDRIAN_EXTERNAL_CLAP_RUNTIME_PCM_CASE_SHA256";
const BLOCK: usize = 64;
// Finite engineering admission caps, not measured physical-heap bounds.
const GRANT: AudioRuntimeResourceGrant =
    AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 16 * 1024 * 1024);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectCase {
    schema: u32,
    helper: Binding,
    plugin: Binding,
    opaque_state: Binding,
    captured_probe_response: Binding,
    pcm: Binding,
    expected_identity: String,
    expected_parameter_count: usize,
    evidence_directory: PathBuf,
}

fn parse_case(bytes: &[u8]) -> EffectCase {
    let case: EffectCase =
        serde_json::from_value(strict_json(bytes)).expect("strict Runtime effect case");
    assert_eq!(case.schema, 1);
    assert!(
        !case.expected_identity.is_empty()
            && case.expected_identity.len() <= 256
            && !case.expected_identity.contains('\0')
    );
    assert_eq!(
        case.expected_parameter_count, 14,
        "declared finite fourteen-parameter case"
    );
    assert!(case.opaque_state.bytes > 0 && case.opaque_state.bytes <= 64 * 1024);
    case
}

// Only envelope/case comparison here. Actual metadata admission still runs the
// original native producer and parent decoder on every normal catalog probe.
fn processing<'a>(value: &'a Value, case: &EffectCase) -> &'a Value {
    exact_keys(value, &["descriptors", "probe"]);
    assert_eq!(value["descriptors"], json!([]));
    let normal = &value["probe"];
    exact_keys(normal, &["schema_version", "processing", "input_names"]);
    assert_eq!(normal["schema_version"], 4);
    assert_eq!(normal["input_names"], json!([]));
    let p = &normal["processing"];
    exact_keys(
        p,
        &[
            "schema_version",
            "main_audio_input",
            "main_audio_output_id",
            "auxiliary_outputs",
            "plugin_id",
            "latency_frames",
            "offline",
            "tail",
            "parameters",
            "auxiliary_buses",
            "captured_state",
        ],
    );
    assert_eq!(p["schema_version"], 3);
    assert_eq!(p["plugin_id"], case.expected_identity);
    assert_eq!(p["offline"], true);
    assert_eq!(p["main_audio_input"], "present");
    assert_eq!(p["main_audio_output_id"], 1);
    assert_eq!(p["auxiliary_outputs"], json!([]));
    assert_eq!(p["auxiliary_buses"], json!([]));
    assert_eq!(p["latency_frames"], 0);
    assert!(
        p["captured_state"].is_null(),
        "fresh restore response, not a default capture"
    );
    assert_eq!(
        p["parameters"].as_array().expect("complete original parameters").len(),
        case.expected_parameter_count
    );
    p
}

// Observe the same Arc that the original resolver returns to the real builder.
// No pre-read resolve/probe, extra Factory, contract override or create occurs.
struct RecordingResolver {
    inner: IsolatedAudioProcessorResolver,
    calls: AtomicUsize,
    scratch: AtomicUsize,
}
impl AudioProcessorResolver for RecordingResolver {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        self.inner.native_child_settlement_requirement()
    }
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        assert!(
            !request.note_input_requested(),
            "this finite Runtime requests no Notes; no plugin Note census is inferred"
        );
        let factory = self.inner.prepare(request)?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.scratch.store(
            factory.execution_contract().session_scratch_bytes(),
            Ordering::Relaxed,
        );
        Ok(factory)
    }
}

fn check_pcm(output: &[f32], frames: usize) -> Result<(), &'static str> {
    if output.len() != frames.checked_mul(2).ok_or("PCM extent overflow")? {
        return Err("PCM extent");
    }
    if !output.iter().all(|sample| sample.is_finite()) {
        return Err("nonfinite or uncovered NaN sentinel");
    }
    if !output.iter().any(|sample| *sample != 0.0) {
        return Err("complete output is silent");
    }
    Ok(())
}

fn check_cursors(runtime: &AudioProgramRuntime, end: usize) {
    let mut actual = Cursors::default();
    runtime.visit_continuity_evidence(&mut actual).unwrap();
    assert_eq!((actual.sessions.len(), actual.processors.len()), (1, 1));
    assert_eq!(actual.sessions[0].public_output_lookahead_frames, 0);
    assert!(matches!(actual.sessions[0].state,
        AudioSessionCursorState::Active {epoch, next_public_sample, next_execution_sample,
            alignment_primed: true} if epoch == AudioContinuityEpoch::new(1)
            && next_public_sample.sample() == end as i64
            && next_execution_sample.sample() == end as i64));
    assert!(matches!(actual.processors[0].state,
        AudioProcessorCursorState::Active {epoch, next_input_signal_sample}
            if epoch == AudioContinuityEpoch::new(1)
                && next_input_signal_sample.sample() == end as i64));
}

#[test]
#[ignore = "hash-bound third-party CLAP case, current normal Product and original outer 180s/2s Job"]
fn external_clap_effect_runtime_renders_finite_nonzero_pcm() {
    let mut in_job = 0;
    // SAFETY: query the current process pseudo-HANDLE and a live BOOL output.
    // Membership is necessary; the original caller authenticates its owner Job.
    assert_ne!(
        unsafe {
            windows_sys::Win32::System::JobObjects::IsProcessInJob(
                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                std::ptr::null_mut(),
                &mut in_job,
            )
        },
        0
    );
    assert_ne!(
        in_job, 0,
        "original outer owner Job required before any native admission"
    );
    let path = PathBuf::from(std::env::var_os(RUNTIME_CASE_ENV).expect("Runtime case path"));
    let mut case_lease = Lease::open(
        &Binding {
            bytes: fs::metadata(&path).expect("case metadata").len(),
            path,
            sha256: std::env::var(RUNTIME_CASE_HASH_ENV).expect("case SHA256"),
        },
        MAX_JSON,
    );
    let case_bytes = case_lease.read();
    let case = parse_case(&case_bytes);
    no_reparse(&case.evidence_directory);
    assert!(fs::metadata(&case.evidence_directory).unwrap().is_dir());
    assert!(fs::read_dir(&case.evidence_directory).unwrap().next().is_none());
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let mut seed = Lease::open(&case.opaque_state, 64 * 1024);
    let mut reference = Lease::open(&case.captured_probe_response, MAX_JSON);
    let mut pcm = Lease::open(&case.pcm, (FRAMES * 2 * 4) as u64);
    let state = seed.read();
    let captured = strict_json(&reference.read());
    let expected = processing(&captured, &case);
    let parameters: Vec<ClapParameterDescriptor> =
        serde_json::from_value(expected["parameters"].clone())
            .expect("original parameter snapshot");
    assert!(
        parameters.iter().all(|p| !p.read_only),
        "this case has fourteen author lanes"
    );
    let period: Arc<[f32]> = deterministic_pcm().into();
    assert_eq!(period.len(), FRAMES * 2);
    assert_eq!(
        pcm.read(),
        pcm_bytes(&period),
        "entire independently bound input period"
    );
    check_pcm(&period, FRAMES).expect("finite nonzero input, not an output reference");

    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: BLOCK,
        processing_mode: AudioProcessingMode::Offline,
        // This is an upper limit, not a fee or reservation. The original
        // schedule sums actual Factory scratch, then Root admits that actual
        // Session footprint before allocate_storage/populate creates a child.
        processor_session_scratch_budget_bytes: GRANT.max_fixed_resident_bytes,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let actual = probe_clap_plugin_registration(
        &helper.path,
        &plugin.path,
        &case.expected_identity,
        render,
        Some(&state),
    )
    .expect("original normal producer/parent validators on the same real restored state");
    assert_eq!(
        actual.binary_sha256.as_slice(),
        decode_hash(&plugin.sha256).as_slice()
    );
    let facts = Facts::Clap(parameters);
    facts.check_native_current(&Facts::Clap(actual.parameters));
    assert_eq!(actual.main_audio_input, ClapMainAudioInput::Present);
    assert_eq!(actual.main_audio_output_id, 1);
    assert!(actual.auxiliary_inputs.buses.is_empty() && actual.auxiliary_outputs.buses.is_empty());
    assert_eq!(actual.execution_contract.algorithmic_latency_frames(), 0);
    let tail = match actual.execution_contract.tail() {
        AudioProcessorTail::None => json!("None"),
        AudioProcessorTail::Finite(n) => json!({"Finite":n}),
        AudioProcessorTail::Infinite => json!("Infinite"),
    };
    assert_eq!(tail, expected["tail"], "query only; no DSP-tail oracle");
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            helper.path.clone(),
            [plugin.path.clone()],
        )
        .expect("normal catalog discovery"),
    );
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(catalog.descriptors()[0].plugin_id, case.expected_identity);
    let author = catalog
        .create_instance(&case.expected_identity, render, Some(state.clone()))
        .expect("original catalog restores and captures canonical current author values");
    assert!(!author.bypassed);
    assert_eq!(
        author.opaque_state.as_deref().map(Vec::as_slice),
        Some(state.as_slice())
    );
    facts.check_native_owned_author(&author);
    let instance_id = author.id;
    let mut sequence = Sequence::new("finite external CLAP effect Runtime PCM");
    sequence.settings.audio_sample_rate = render.sample_rate;
    sequence.settings.audio_channel_layout = render.channel_layout;
    sequence
        .add_media_audio_clip(
            sequence.audio_tracks[0].id,
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            AudioSourceComponentId::primary(),
        )
        .expect("original media clip authoring");
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(author);
    let author_before = serde_json::to_vec(&sequence).unwrap();
    let resolver = RecordingResolver {
        inner: IsolatedAudioProcessorResolver::new(catalog),
        calls: AtomicUsize::new(0),
        scratch: AtomicUsize::new(0),
    };
    let reads = Arc::new(AtomicUsize::new(0));
    // Exactly one complete original 4096-frame period; no short-buffer modulo assumption.
    let source = PcmMedia { period, reads: reads.clone() };
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &source,
        &resolver,
        render,
        None,
        GRANT,
    )
    .expect("original canonical DAG/Plan/Session under the finite Root grant");
    assert_eq!(resolver.calls.load(Ordering::Relaxed), 1);
    let scratch = resolver.scratch.load(Ordering::Relaxed);
    assert!(
        scratch > 0,
        "actual Hosted scratch for this isolated-worker case"
    );
    let footprint = runtime.resource_footprint();
    assert_eq!(footprint.processor_session_bytes, scratch);
    assert_eq!(footprint.runtime_occurrences, 1);
    assert!(footprint.fixed_resident_bytes <= GRANT.max_fixed_resident_bytes);
    assert!(footprint.prepared_logical_bytes <= GRANT.max_prepared_logical_bytes);
    assert_eq!(runtime.resource_grant(), GRANT);
    assert!(runtime.native_observations_supported());
    let installation = AudioOwnerInstallationSource::new();
    let (registration, verifier) = installation.register().unwrap();
    runtime.bind_native_owner_installation(verifier.clone()).unwrap();
    let claim = installation
        .reserve_install(&registration, None)
        .and_then(|reservation| installation.begin_commit(reservation))
        .unwrap()
        .commit()
        .unwrap();
    let cancel = ExecutionCancellationToken::new();
    runtime
        .install_native_owner_bindings(&claim, &cancel)
        .expect("real same-child Source Bind");
    let cold = owner(&runtime);
    assert!(cold.editor_target().continuity_epoch.is_none());
    let object = runtime
        .observe_native_owner_object(cold)
        .expect("actual authenticated cold child");
    let pid = object.process_id().get();
    assert_ne!(pid, std::process::id());
    // SAFETY: the PID comes from the authenticated installed native object.
    // This independent HANDLE has query/synchronize rights only and closes once.
    let raw = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    assert!(!raw.is_null(), "retain actual child observation HANDLE");
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
            258
        );
        // Ready admits the real DSP role, not completion of its first idle
        // flush. Service exactly one original installed-owner observation; it
        // submits/joins the existing exclusive DSP flush, without PCM or Ack.
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        let batch = runtime
            .poll_native_observations(&cancel)
            .expect("one original cold owner Poll; actual typed errors remain failures");
        let closed = runtime.validate_native_author_source_closed(&cancel);
        let author = batch.as_ref().and_then(|batch| batch.author.as_ref());
        let parameters = batch.as_ref().and_then(|batch| batch.parameters.as_ref());
        let diagnostics = batch.as_ref().and_then(|batch| batch.diagnostics.as_ref());
        // Keep the real DTO owned until after every check. Only bounded facts
        // leave this consumer; opaque state bytes are never printed or copied.
        let summary = json!({
            "batch_present":batch.is_some(),
            "author":author.map(|proposal| json!({
                "target":format!("{:?}",proposal.target()),
                "sequence":format!("{:?}",proposal.sequence()),
                "revision":proposal.revision(), "callback_count":proposal.callbacks().len(),
                "state_bytes":proposal.state().map(|state| state.len()),
                "completion":format!("{:?}",proposal.completion()),
                "open_parameter_edits":proposal.open_parameter_edits(),
                "group_depth":proposal.group_depth()
            })),
            "parameters":parameters.map(|parameters| json!({
                "target":format!("{:?}",parameters.target()),
                "process_id":parameters.process_id().get(),
                "object_serial":parameters.object_serial().get(),
                "fact_count":parameters.facts().len()
            })),
            "diagnostic_records":diagnostics.map(|logs| logs.records().len()),
            "diagnostic_loss":diagnostics.map(|logs| format!("{:?}",logs.loss())),
            "attachment":batch.as_ref().and_then(|batch| batch.attachment.as_ref())
                .map(|attachment| format!("{attachment:?}")),
            "closed_result":format!("{closed:?}")
        });
        write_json(&case.evidence_directory, "cold-poll.json", summary.clone());
        assert_eq!(owner(&runtime), cold);
        assert_eq!(cold.installed_root_serial(), claim.generation());
        let current = runtime.native_owner_installation_verifier().unwrap();
        assert!(current.same_source(&verifier) && current.same_registration(&verifier));
        current.validate(&claim).unwrap();
        assert_eq!(
            installation.current_claim().unwrap().unwrap().generation(),
            claim.generation()
        );
        let mut cursors = Cursors::default();
        runtime.visit_continuity_evidence(&mut cursors).unwrap();
        assert_eq!((cursors.sessions.len(), cursors.processors.len()), (1, 1));
        assert_eq!(
            cursors.sessions[0].state,
            AudioSessionCursorState::Unentered
        );
        assert_eq!(
            cursors.processors[0].state,
            AudioProcessorCursorState::Unentered
        );
        assert_eq!(
            reads.load(Ordering::Relaxed),
            0,
            "cold Poll cannot read/render PCM"
        );
        if let Some(parameters) = parameters {
            assert_eq!(parameters.target(), cold.editor_target());
            assert_eq!(parameters.process_id(), object.process_id());
            assert_eq!(parameters.object_serial(), object.object_serial());
        }
        if let Some(logs) = diagnostics {
            assert!(
                !logs.loss().incomplete(),
                "actual cold diagnostics loss: {summary}"
            );
        }
        if let Some(proposal) = author {
            assert_eq!(proposal.target(), cold.editor_target());
            assert_eq!(proposal.sequence(), sequence.id);
            panic!("COLD_AUTHOR_TRANSACTION_NOT_COVERED: actual proposal retained without Ack/install; {summary}");
        }
        // NoBatch is never a substitute for the independent current proof.
        closed.unwrap_or_else(|error| panic!(
            "real cold Source closed after exactly one owner Poll; actual typed cause: {error:?}; {summary}"));
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(1),
                start_sample: 0,
            })
            .expect("original explicit Fresh entry");
        let lease = owner(&runtime);
        assert_eq!(lease.editor_target().instance, instance_id);
        assert_eq!(lease.editor_target().continuity_epoch, Some(1));
        assert_eq!(lease.installed_root_serial(), claim.generation());
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
        let mut output = Vec::with_capacity(FRAMES * 2);
        for start in (0..FRAMES).step_by(BLOCK) {
            let mut block = [f32::NAN; BLOCK * 2];
            runtime.render_into(AudioRenderRequest { transport_activity: None, start_sample: start as i64, frames: BLOCK },
                &mut block).unwrap_or_else(|error| panic!(
                    "actual CLAP Runtime PCM failure at start={start}; original typed failure retained: {error:?}"));
            assert!(
                block.iter().all(|v| v.is_finite()),
                "complete NaN sentinel coverage at {start}"
            );
            output.extend_from_slice(&block);
            check_cursors(&runtime, start + BLOCK);
            assert_eq!(owner(&runtime), lease);
            let current = runtime.native_owner_installation_verifier().unwrap();
            assert!(current.same_source(&verifier) && current.same_registration(&verifier));
            current.validate(&claim).unwrap();
            assert_eq!(
                installation.current_claim().unwrap().unwrap().generation(),
                claim.generation()
            );
        }
        check_pcm(&output, FRAMES).expect("complete finite nonzero Runtime output");
        assert!(
            reads.load(Ordering::Relaxed) > 0,
            "real media source was read"
        );
        runtime.validate_native_owner(lease).unwrap();
        assert_eq!(runtime.observe_native_owner_object(lease).unwrap(), object);
        runtime
            .validate_native_author_source_closed(&cancel)
            .expect("real post-PCM Source closed; native requests/errors remain failures");
        assert_eq!(runtime.resource_footprint(), footprint);
        assert_eq!(serde_json::to_vec(&sequence).unwrap(), author_before);
        output
    }));
    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    drop(runtime);
    let settled = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
    let output = match result {
        Ok(output) => output,
        Err(panic) => {
            eprintln!("CLAP Runtime target failure; not cleanup qualification: before_drop_wait={alive}; after_drop_wait={settled}; exit_query_result={queried}; raw_exit_code={exit_code}");
            std::panic::resume_unwind(panic)
        }
    };
    assert_eq!(alive, 258);
    assert_eq!(
        settled, 0,
        "original Runtime Drop must settle the exact child"
    );
    assert_ne!(queried, 0);
    assert_eq!(exit_code, 0, "forced/error exit cannot qualify lifecycle");
    write_new(&case.evidence_directory, "case.json", &case_bytes);
    write_new(
        &case.evidence_directory,
        "output.f32le",
        &pcm_bytes(&output),
    );
    write_json(
        &case.evidence_directory,
        "result.json",
        json!({
            "schema":1, "canonical_runtime_and_output_rack":true, "host_bypassed":false,
            "plugin_id":case.expected_identity, "case_sha256":case_lease.sha256,
            "plugin_sha256":plugin.sha256, "state_sha256":seed.sha256, "state_bytes":state.len(),
            "reference_probe_sha256":reference.sha256, "input_sha256":pcm.sha256,
            "sample_rate":48000, "channels":2, "mode":"Offline", "max_block_frames":BLOCK,
            "completed_frames":FRAMES, "runtime_render_calls":FRAMES/BLOCK,
            "source_reads":reads.load(Ordering::Relaxed), "finite_nonzero_output":true,
            "same_source_claim_lease_epoch_and_object":true, "native_pid":pid,
            "native_object_serial":object.object_serial().get(), "continuity_epoch":1,
            "actual_processor_session_bytes":scratch,
            "fixed_resident_bytes":footprint.fixed_resident_bytes,
            "prepared_logical_bytes":footprint.prepared_logical_bytes,
            "fixed_cap":GRANT.max_fixed_resident_bytes, "logical_cap":GRANT.max_prepared_logical_bytes,
            "alive_before_drop_wait":alive, "post_drop_wait":settled,
            "exit_query_succeeded":queried != 0, "exit_code":exit_code,
            "graceful_ack_qualified":false, "full_physical_heap_qualified":false,
            "native_note_inventory_qualified":false, "bitexact_bypass_qualified":false,
            "algorithm_partition_tail_or_realtime_qualified":false,
            "canonical_project_state_or_ui_qualified":false
        }),
    );
    // Leases outlive every original probe, Runtime and actual child Drop.
    drop((case_lease, helper, plugin, seed, reference, pcm));
}

#[test]
fn effect_runtime_case_and_probe_require_current_strict_schemas() {
    let binding = json!({"path":"C:/case.bin", "bytes":1, "sha256":"0".repeat(64)});
    let value = json!({"schema":1,"helper":binding,"plugin":binding,"opaque_state":binding,
        "captured_probe_response":binding,"pcm":binding,"expected_identity":"test.effect",
        "expected_parameter_count":14,"evidence_directory":"C:/observations"});
    let case = parse_case(&serde_json::to_vec(&value).unwrap());
    let normal = json!({"descriptors":[],"probe":{"schema_version":4,"input_names":[],
        "processing":{"schema_version":3,"main_audio_input":"present","main_audio_output_id":1,
            "auxiliary_outputs":[],"plugin_id":"test.effect","latency_frames":0,"offline":true,
            "tail":"None","parameters":vec![json!({});14],"auxiliary_buses":[],"captured_state":null}}});
    processing(&normal, &case);
    let mut bad = value.clone();
    bad["unexpected"] = true.into();
    assert!(std::panic::catch_unwind(|| parse_case(&serde_json::to_vec(&bad).unwrap())).is_err());
    let mut bad = value.clone();
    bad["expected_parameter_count"] = 13.into();
    assert!(std::panic::catch_unwind(|| parse_case(&serde_json::to_vec(&bad).unwrap())).is_err());
    let mut bad = normal.clone();
    bad["probe"]["schema_version"] = 3.into();
    assert!(std::panic::catch_unwind(|| processing(&bad, &case)).is_err());
    let mut bad = normal.clone();
    bad["probe"]["processing"]["schema_version"] = 2.into();
    assert!(std::panic::catch_unwind(|| processing(&bad, &case)).is_err());
    let mut bad = normal.clone();
    bad["probe"]["processing"]["captured_state"] = json!([]);
    assert!(std::panic::catch_unwind(|| processing(&bad, &case)).is_err());
    let mut bad = normal.clone();
    bad["probe"]["processing"]["auxiliary_outputs"] = json!([{}]);
    assert!(std::panic::catch_unwind(|| processing(&bad, &case)).is_err());
}

#[test]
fn effect_runtime_pcm_oracle_rejects_uncovered_silent_nonfinite_and_wrong_extent() {
    assert_eq!(check_pcm(&[0.25, -0.5, 0.0, 0.125], 2), Ok(()));
    assert_eq!(
        check_pcm(&[0.25, f32::NAN, 0.0, 0.125], 2),
        Err("nonfinite or uncovered NaN sentinel")
    );
    assert_eq!(check_pcm(&[0.0, -0.0], 1), Err("complete output is silent"));
    assert_eq!(
        check_pcm(&[0.25, f32::INFINITY], 1),
        Err("nonfinite or uncovered NaN sentinel")
    );
    assert_eq!(check_pcm(&[0.25], 1), Err("PCM extent"));
    assert_eq!(check_pcm(&[], usize::MAX), Err("PCM extent overflow"));
}
