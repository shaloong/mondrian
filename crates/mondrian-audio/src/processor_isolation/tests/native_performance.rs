//! Opt-in acceptance of real native fixture workers, including their IPC path.
//! Run the release test serially with a current machine/build receipt.

use crate::{
    AudioKernelBackend, AudioParameterEventBatch, AudioProcessingMode, AudioProcessor,
    AudioProcessorAudioIo, AudioProcessorFactory, AudioProcessorHostError, AudioProcessorInputBus,
    AudioProcessorInsertionPoint, AudioProcessorMainAndInputBuses, AudioProcessorOccurrence,
    AudioProcessorOccurrenceOwner, AudioProcessorPrepareRequest, AudioProcessorProcessContext,
    AudioProcessorTail, AudioRenderContract, AudioRenderRequest,
    DiscoveredClapAudioProcessorSpecResolver, DiscoveredVst3AudioProcessorSpecResolver,
    IsolatedAudioProcessorFactory, IsolatedAudioProcessorSpecResolver,
    IsolatedAudioProcessorWorkerSpec,
};
use mondrian_core::{AudioChannelLayout, AudioSampleRate, ProgramOutputId};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const WARMUP_CALLBACKS: usize = 32;
// Nearest-rank p99 differs from max for this observation count.
const MEASURED_CALLBACKS: usize = 512;
const NS_PER_SECOND: u128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Serialize)]
enum NativeFormat {
    Clap,
    Vst3,
}

#[derive(Clone, Copy, Debug, Serialize)]
enum PartitionPattern {
    Fixed,
    Uneven,
}

impl PartitionPattern {
    fn frames(self, maximum: usize, index: usize) -> usize {
        match self {
            Self::Fixed => maximum,
            Self::Uneven => [maximum / 2, maximum / 4, maximum / 8, maximum / 8][index % 4],
        }
    }
}

#[derive(Serialize)]
struct CallbackObservation {
    index: usize,
    start_sample: i64,
    frames: usize,
    elapsed_ns: u128,
    audio_deadline_floor_ns: u128,
    deadline_missed: bool,
    pcm_mismatches: usize,
    error: Option<String>,
}

#[derive(Serialize)]
struct CallbackDistribution {
    frames: usize,
    count: usize,
    p50_ns: u128,
    p95_ns: u128,
    p99_ns: u128,
    max_ns: u128,
    deadline_floor_ns: u128,
    deadline_misses: usize,
    largest_overrun_ns: u128,
}

#[derive(Serialize)]
struct CaseReport {
    kind: &'static str,
    format: NativeFormat,
    mode: &'static str,
    max_block_frames: usize,
    partition: PartitionPattern,
    sample_rate: u32,
    channels: usize,
    algorithmic_latency_frames: usize,
    finite_tail_frames: usize,
    worker_fault_timeout_ns: u128,
    preparation_payload_sha256: String,
    warmup_requested: usize,
    warmup_completed: usize,
    measured_requested: usize,
    correctness_corner_cases: &'static str,
    all_observations: Vec<CallbackObservation>,
    distributions: Vec<CallbackDistribution>,
    total_pcm_mismatches: usize,
    operation_errors: Vec<String>,
    total_callback_ns: u128,
    total_frames: usize,
    throughput_relative_to_realtime: Option<f64>,
    acceptance: &'static str,
}

struct PerfIo {
    frames: usize,
    main: Vec<f32>,
    inputs: Vec<(String, Vec<f32>)>,
}

impl PerfIo {
    fn new(maximum: usize, keys: &[String]) -> Self {
        Self {
            frames: 0,
            main: vec![0.0; maximum * CHANNELS],
            inputs: keys.iter().map(|key| (key.clone(), vec![0.0; maximum * CHANNELS])).collect(),
        }
    }

    fn fill(&mut self, signal_offset: usize, frames: usize) {
        self.frames = frames;
        for frame in 0..frames {
            let absolute = signal_offset + frame;
            let main = source_numerator(absolute, 0) as f32 / 1024.0;
            self.main[frame * 2..frame * 2 + 2].copy_from_slice(&[main, -main * 0.5]);
            for (index, (_, samples)) in self.inputs.iter_mut().enumerate() {
                let source = source_numerator(absolute, index + 1) as f32 / 1024.0;
                samples[frame * 2..frame * 2 + 2].copy_from_slice(&[source, -source * 0.5]);
            }
        }
    }

    fn mismatches(&self, signal_offset: usize, delay: usize, gain_numerator: i32) -> usize {
        let mut mismatches = 0;
        for frame in 0..self.frames {
            let expected = expected_left(signal_offset + frame, delay, gain_numerator);
            let pair = &self.main[frame * 2..frame * 2 + 2];
            // Sources, integer affine mix, and the fixture gain are binary
            // rationals with bounded numerators. Their exact f32 result is the
            // independent integer oracle, so no fitted PCM tolerance is used.
            mismatches += usize::from(pair[0] != expected);
            mismatches += usize::from(pair[1] != -expected * 0.5);
        }
        mismatches
    }
}

impl AudioProcessorAudioIo for PerfIo {
    fn main_layout(&self) -> AudioChannelLayout {
        AudioChannelLayout::Stereo
    }
    fn frames(&self) -> usize {
        self.frames
    }
    fn main_interleaved(&mut self) -> &mut [f32] {
        &mut self.main[..self.frames * CHANNELS]
    }
    fn auxiliary_input(&self, bus_key: &str) -> Option<AudioProcessorInputBus<'_>> {
        let (key, samples) = self.inputs.iter().find(|(key, _)| key == bus_key)?;
        Some(AudioProcessorInputBus {
            bus_key: key,
            channel_layout: AudioChannelLayout::Stereo,
            frames: self.frames,
            interleaved: &samples[..self.frames * CHANNELS],
        })
    }
    fn main_and_auxiliary_input(
        &mut self,
        bus_key: &str,
    ) -> Option<AudioProcessorMainAndInputBuses<'_>> {
        let (key, samples) = self.inputs.iter().find(|(key, _)| key == bus_key)?;
        Some(AudioProcessorMainAndInputBuses {
            main_layout: AudioChannelLayout::Stereo,
            frames: self.frames,
            main_interleaved: &mut self.main[..self.frames * CHANNELS],
            auxiliary: AudioProcessorInputBus {
                bus_key: key,
                channel_layout: AudioChannelLayout::Stereo,
                frames: self.frames,
                interleaved: &samples[..self.frames * CHANNELS],
            },
        })
    }
}

fn source_numerator(sample: usize, bus: usize) -> i32 {
    let phase = [0, 17, 83][bus];
    ((sample + phase) % 257) as i32 - 128
}

fn expected_left(sample: usize, delay: usize, gain_numerator: i32) -> f32 {
    let Some(source) = sample.checked_sub(delay) else {
        return 0.0;
    };
    let affine = 2 * source_numerator(source, 0)
        + 3 * source_numerator(source, 1)
        + 5 * source_numerator(source, 2);
    // gain numerator is 2 (one half) or 3 (three quarters).
    (affine * gain_numerator) as f32 / 4096.0
}

#[test]
#[ignore = "current release product, owned native fixtures, serial reference-machine acceptance"]
fn native_plugin_isolated_worker_callback_acceptance() {
    if cfg!(debug_assertions) {
        panic!("native performance acceptance requires --release");
    }
    let context_path = required_path("MONDRIAN_NATIVE_PERF_CONTEXT");
    let context: Value =
        serde_json::from_slice(&fs::read(&context_path).expect("read performance receipt"))
            .expect("machine/build receipt JSON");
    validate_context(&context);
    let test_binary = std::env::current_exe().expect("performance harness executable");
    verify_artifact(&context, "audio_test_executable", &test_binary);
    let machine_identity_complete = context["machine"]["cpu_identity_and_ram_available"]
        .as_bool()
        .expect("explicit hardware-receipt completeness");
    let clap_worker = required_path("MONDRIAN_CLAP_TEST_HELPER");
    let vst3_worker = required_path("MONDRIAN_VST3_TEST_HELPER");
    let clap_plugin = required_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN");
    let vst3_plugin = required_path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN");
    verify_artifact(&context, "clap_worker", &clap_worker);
    verify_artifact(&context, "vst3_worker", &vst3_worker);
    verify_artifact(&context, "clap_fixture", &clap_plugin);
    verify_artifact(&context, "vst3_fixture", &vst3_plugin);
    let output = required_path("MONDRIAN_NATIVE_PERF_OUTPUT");
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).expect("report directory");
    }
    let mut report = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .expect("fresh performance report; previous evidence must not be overwritten");
    emit(
        &mut report,
        &json!({"kind":"context", "schema_version":1, "machine_build":context,
        "harness_binary": {"path":test_binary,"sha256":sha256_file(&test_binary)},
        "harness_package_version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,
        "arch":std::env::consts::ARCH,"debug_assertions":cfg!(debug_assertions),
        "measurement_scope":"synchronous public process call including parent IPC, child DSP and PCM copy",
        "excluded":"discovery/startup/reset, warmup, input generation, PCM oracle, JSON/report I/O",
        "workload":"serial, back-to-back callbacks, one isolated native occurrence; no device underrun claim",
        "percentile":"nearest rank: sorted[ceil(count*percent/100)-1]",
        "realtime_acceptance":"every measured callback succeeds, exact PCM, elapsed_ns * sample_rate <= frames * 1e9",
        "offline_acceptance":"all callbacks succeed with exact PCM; throughput reported without realtime threshold"}),
    );

    let clap_catalog =
        DiscoveredClapAudioProcessorSpecResolver::discover(clap_worker, [clap_plugin])
            .expect("discover current CLAP fixture in production worker");
    assert_eq!(clap_catalog.descriptors().len(), 1);
    assert_eq!(
        clap_catalog.descriptors()[0].plugin_id,
        "org.mondrian.fixture.sidechain"
    );
    let vst3_catalog =
        DiscoveredVst3AudioProcessorSpecResolver::discover(vst3_worker, [vst3_plugin])
            .expect("discover current VST3 fixture in production worker");
    assert_eq!(vst3_catalog.descriptors().len(), 1);
    assert_eq!(
        vst3_catalog.descriptors()[0].name,
        "Mondrian Sidechain Fixture"
    );
    let mut rejected = Vec::new();
    for format in [NativeFormat::Clap, NativeFormat::Vst3] {
        for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
            for maximum in [64, 128, 256, 512, 1024] {
                let render = AudioRenderContract {
                    sample_rate: SAMPLE_RATE,
                    channel_layout: AudioChannelLayout::Stereo,
                    max_block_frames: maximum,
                    processing_mode: mode,
                    processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
                    public_output_lookahead_budget_frames: 4096,
                    compensation_delay_scratch_budget_bytes: 1024 * 1024,
                };
                let mut instance = match format {
                    NativeFormat::Clap => {
                        let mut state = vec![3];
                        state.extend_from_slice(&0.5_f32.to_le_bytes());
                        clap_catalog
                            .create_instance("org.mondrian.fixture.sidechain", render, Some(state))
                            .expect("capture mode-aware CLAP fixture state")
                    }
                    NativeFormat::Vst3 => vst3_catalog
                        .create_instance(
                            &vst3_catalog.descriptors()[0].class_id,
                            render,
                            Some(vst3_snapshot()),
                        )
                        .expect("capture VST3 fixture state"),
                };
                // State is this oracle's sole control. Automation is qualified
                // by the separate sample/event native correctness gates.
                instance.parameters.clear();
                let request = AudioProcessorPrepareRequest::new(
                    AudioProcessorOccurrence {
                        instance_id: instance.id,
                        owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                        insertion: AudioProcessorInsertionPoint::PreFader,
                    },
                    &instance.definition,
                    &instance.parameters,
                    instance.opaque_state.as_ref().map(AsRef::as_ref),
                    render,
                );
                let spec = match format {
                    NativeFormat::Clap => clap_catalog.resolve(request),
                    NativeFormat::Vst3 => vst3_catalog.resolve(request),
                }
                .expect("resolve production native worker contract");
                for pattern in [PartitionPattern::Fixed, PartitionPattern::Uneven] {
                    let case = run_case(format, mode, maximum, pattern, &spec);
                    if !matches!(
                        case.acceptance,
                        "accepted_realtime_fixture_callbacks" | "accepted_offline_correctness"
                    ) {
                        rejected.push(format!(
                            "{format:?}/{mode:?}/{maximum}/{pattern:?}: {}",
                            case.acceptance
                        ));
                    }
                    emit(&mut report, &case);
                }
            }
        }
    }
    emit(
        &mut report,
        &json!({"kind":"summary","accepted":rejected.is_empty()&&machine_identity_complete,
        "callback_conditions_accepted":rejected.is_empty(),"machine_identity_receipt_complete":machine_identity_complete,
        "rejected_cases":rejected}),
    );
    report.sync_all().expect("durably flush native performance evidence");
    assert!(
        rejected.is_empty() && machine_identity_complete,
        "native callback acceptance or machine receipt incomplete; full evidence: {}",
        output.display()
    );
}

fn run_case(
    format: NativeFormat,
    mode: AudioProcessingMode,
    maximum: usize,
    pattern: PartitionPattern,
    spec: &IsolatedAudioProcessorWorkerSpec,
) -> CaseReport {
    let quality = matches!(format, NativeFormat::Clap) && mode == AudioProcessingMode::Offline;
    let delay = if quality { 5 } else { 3 };
    let gain_numerator = if quality { 3 } else { 2 };
    let mut result = CaseReport {
        kind: "case",
        format,
        mode: if mode == AudioProcessingMode::Realtime {
            "realtime"
        } else {
            "offline"
        },
        max_block_frames: maximum,
        partition: pattern,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        algorithmic_latency_frames: delay,
        finite_tail_frames: delay,
        worker_fault_timeout_ns: spec.operation_timeout.as_nanos(),
        warmup_requested: WARMUP_CALLBACKS,
        warmup_completed: 0,
        preparation_payload_sha256: format!("{:x}", Sha256::digest(&spec.preparation_payload)),
        measured_requested: MEASURED_CALLBACKS,
        correctness_corner_cases:
            "one-frame and uneven partitions; negative start; reset and backward seek",
        all_observations: Vec::with_capacity(MEASURED_CALLBACKS),
        distributions: Vec::new(),
        total_pcm_mismatches: 0,
        operation_errors: Vec::new(),
        total_callback_ns: 0,
        total_frames: 0,
        throughput_relative_to_realtime: None,
        acceptance: "rejected_incomplete",
    };
    let expected_keys: Vec<_> = match format {
        NativeFormat::Clap => [11, 7].map(|id| format!("clap.audio-input.{id}")),
        NativeFormat::Vst3 => [1, 2].map(|id| format!("vst3.audio-input.{id}")),
    }
    .into_iter()
    .collect();
    if spec
        .auxiliary_inputs
        .buses
        .iter()
        .map(|bus| bus.bus_key.as_str())
        .collect::<Vec<_>>()
        != expected_keys.iter().map(String::as_str).collect::<Vec<_>>()
        || spec.plugin_execution_contract.algorithmic_latency_frames() != delay
        || spec.plugin_execution_contract.tail() != AudioProcessorTail::Finite(delay)
    {
        result
            .operation_errors
            .push("native fixture scheduling/port facts differ from independent reference".into());
        return result;
    }
    let factory = match IsolatedAudioProcessorFactory::prepare(spec.clone()) {
        Ok(factory) => factory,
        Err(error) => {
            result.operation_errors.push(error.to_string());
            return result;
        }
    };
    let mut processor = match factory.create(None) {
        Ok(processor) => processor,
        Err(error) => {
            result.operation_errors.push(error.to_string());
            return result;
        }
    };
    let mut io = PerfIo::new(maximum, &expected_keys);
    // Correctness includes CLAP's legal minimum extent and both directions of
    // state entry, outside the steady callback measurement window.
    for origin in [-17, 101, -29] {
        if let Err(error) = processor.enter_state(origin) {
            result.operation_errors.push(error.to_string());
            return result;
        }
        let mut offset = 0;
        for frames in [1, 3, maximum - 4, 2, maximum - 2] {
            io.fill(offset, frames);
            if let Err(error) = callback(
                processor.as_mut(),
                mode,
                origin + offset as i64,
                frames,
                &mut io,
            ) {
                result.operation_errors.push(error.to_string());
                return result;
            }
            result.total_pcm_mismatches += io.mismatches(offset, delay, gain_numerator);
            offset += frames;
        }
    }
    if let Err(error) = processor.enter_state(-29) {
        result.operation_errors.push(error.to_string());
        return result;
    }
    let mut offset = 0;
    for index in 0..WARMUP_CALLBACKS {
        let frames = pattern.frames(maximum, index);
        io.fill(offset, frames);
        if let Err(error) = callback(
            processor.as_mut(),
            mode,
            -29 + offset as i64,
            frames,
            &mut io,
        ) {
            result.operation_errors.push(error.to_string());
            return result;
        }
        result.total_pcm_mismatches += io.mismatches(offset, delay, gain_numerator);
        result.warmup_completed += 1;
        offset += frames;
    }
    // Continue the warmed instance and exact delay history. VST3 state entry
    // recreates its native processing context, so resetting after warmup would
    // silently put cold activation/start work back into the measurement window.
    for index in 0..MEASURED_CALLBACKS {
        let frames = pattern.frames(maximum, index);
        let start_sample = -29 + offset as i64;
        io.fill(offset, frames);
        let context = process_context(mode, start_sample, frames);
        let parameters = AudioParameterEventBatch::new(start_sample, frames, &[], &[], &[]);
        let started = Instant::now();
        let outcome = processor.process(context, &mut io, parameters);
        let elapsed_ns = started.elapsed().as_nanos();
        let pcm_mismatches = if outcome.is_ok() {
            io.mismatches(offset, delay, gain_numerator)
        } else {
            0
        };
        result.total_pcm_mismatches += pcm_mismatches;
        result.total_callback_ns += elapsed_ns;
        result.total_frames += frames;
        let failed = outcome.is_err();
        let error = outcome.err().map(|error| error.to_string());
        if let Some(error) = &error {
            result.operation_errors.push(error.clone());
        }
        result.all_observations.push(CallbackObservation {
            index,
            start_sample,
            frames,
            elapsed_ns,
            audio_deadline_floor_ns: deadline_ns(frames),
            deadline_missed: elapsed_ns * u128::from(SAMPLE_RATE) > frames as u128 * NS_PER_SECOND,
            pcm_mismatches,
            error,
        });
        if failed {
            break;
        }
        offset += frames;
    }
    let mut extents: Vec<_> = result.all_observations.iter().map(|sample| sample.frames).collect();
    extents.sort_unstable();
    extents.dedup();
    for frames in extents {
        let mut durations: Vec<_> = result
            .all_observations
            .iter()
            .filter(|sample| sample.frames == frames)
            .map(|sample| sample.elapsed_ns)
            .collect();
        durations.sort_unstable();
        let misses = result
            .all_observations
            .iter()
            .filter(|sample| sample.frames == frames && sample.deadline_missed)
            .count();
        let max_ns = durations.last().copied().unwrap_or_default();
        result.distributions.push(CallbackDistribution {
            frames,
            count: durations.len(),
            p50_ns: percentile(&durations, 50),
            p95_ns: percentile(&durations, 95),
            p99_ns: percentile(&durations, 99),
            max_ns,
            deadline_floor_ns: deadline_ns(frames),
            deadline_misses: misses,
            largest_overrun_ns: max_ns.saturating_sub(deadline_ns(frames)),
        });
    }
    if result.total_callback_ns > 0
        && result.operation_errors.is_empty()
        && result.total_pcm_mismatches == 0
    {
        result.throughput_relative_to_realtime = Some(
            result.total_frames as f64 * 1e9
                / f64::from(SAMPLE_RATE)
                / result.total_callback_ns as f64,
        );
    }
    let complete = result.all_observations.len() == MEASURED_CALLBACKS;
    result.acceptance = if !complete || !result.operation_errors.is_empty() {
        "rejected_operation_or_incomplete"
    } else if result.total_pcm_mismatches != 0 {
        "rejected_pcm"
    } else if mode == AudioProcessingMode::Offline {
        "accepted_offline_correctness"
    } else if result.all_observations.iter().any(|sample| sample.deadline_missed) {
        "rejected_realtime_deadline"
    } else {
        "accepted_realtime_fixture_callbacks"
    };
    result
}

fn callback(
    processor: &mut dyn AudioProcessor,
    mode: AudioProcessingMode,
    start_sample: i64,
    frames: usize,
    io: &mut PerfIo,
) -> Result<(), AudioProcessorHostError> {
    processor.process(
        process_context(mode, start_sample, frames),
        io,
        AudioParameterEventBatch::new(start_sample, frames, &[], &[], &[]),
    )
}

fn process_context(
    mode: AudioProcessingMode,
    start_sample: i64,
    frames: usize,
) -> AudioProcessorProcessContext {
    AudioProcessorProcessContext::new(
        AudioRenderRequest { transport_activity: None, start_sample, frames },
        AudioSampleRate::new(SAMPLE_RATE).expect("fixed valid sample rate"),
        AudioChannelLayout::Stereo,
        mode,
        AudioKernelBackend::ScalarReference,
    )
}

fn deadline_ns(frames: usize) -> u128 {
    frames as u128 * NS_PER_SECOND / u128::from(SAMPLE_RATE)
}

fn percentile(sorted: &[u128], percent: usize) -> u128 {
    sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)]
}

fn vst3_snapshot() -> Vec<u8> {
    let mut component = Vec::new();
    component.extend_from_slice(b"MSS1");
    component.extend_from_slice(&1_u32.to_le_bytes());
    component.extend_from_slice(&0.5_f64.to_le_bytes());
    component.extend_from_slice(&0_u32.to_le_bytes());
    let mut snapshot = Vec::new();
    snapshot.extend_from_slice(b"VST3HOST_STATE\0\0");
    snapshot.extend_from_slice(&1_u32.to_le_bytes());
    snapshot.extend_from_slice(&(component.len() as u32).to_le_bytes());
    snapshot.extend_from_slice(&(component.len() as u32).to_le_bytes());
    snapshot.extend_from_slice(&component);
    snapshot.extend_from_slice(&component);
    snapshot
}

fn required_path(name: &str) -> PathBuf {
    let path = std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {name}"));
    assert!(path.is_absolute(), "{name} must be absolute");
    path
}

fn sha256_file(path: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(fs::read(path).expect("hash exact evidence file"))
    )
}

fn verify_artifact(context: &Value, role: &str, path: &Path) {
    let entry = context["artifacts"]
        .as_array()
        .expect("artifact receipts")
        .iter()
        .find(|entry| entry["role"] == role)
        .unwrap_or_else(|| panic!("missing {role} receipt"));
    let recorded = PathBuf::from(entry["path"].as_str().expect("artifact path"));
    assert_eq!(
        fs::canonicalize(path).expect("selected artifact"),
        fs::canonicalize(recorded).expect("recorded artifact")
    );
    assert_eq!(
        entry["sha256"].as_str().expect("artifact SHA256"),
        sha256_file(path),
        "stale {role} receipt"
    );
}

fn validate_context(context: &Value) {
    assert_eq!(context["schema_version"], 1);
    assert!(context["machine"]["os_version"].as_str().is_some_and(|value| !value.is_empty()));
    let has_cpu = context["machine"]["cpu"].as_array().is_some_and(|cpus| {
        cpus.iter().any(|cpu| {
            ["name", "architecture_identifier", "proc_cpuinfo"]
                .iter()
                .any(|key| cpu[*key].as_str().is_some_and(|value| !value.is_empty()))
        })
    });
    let has_ram = context["machine"]["total_physical_memory_bytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0);
    let has_limitations = context["machine"]["limitations"]
        .as_array()
        .is_some_and(|items| !items.is_empty());
    assert!(
        (has_cpu && has_ram) || has_limitations,
        "Unavailable hardware facts require explicit limitations"
    );
    assert_eq!(
        context["machine"]["cpu_identity_and_ram_available"].as_bool(),
        Some(has_cpu && has_ram)
    );
    assert_eq!(context["build"]["profile"], "release");
    assert!(context["build"]["rustc_verbose"]
        .as_str()
        .is_some_and(|value| value.contains("release:")));
    assert!(context["build"]["commands"]
        .as_array()
        .is_some_and(|commands| !commands.is_empty()));
    let transcript = PathBuf::from(
        context["build"]["transcript"]["path"]
            .as_str()
            .expect("actual build transcript path"),
    );
    assert_eq!(
        context["build"]["transcript"]["sha256"]
            .as_str()
            .expect("build transcript SHA256"),
        sha256_file(&transcript)
    );
    assert!(context["build"]["git_commit"].as_str().is_some_and(|commit| commit.len() == 40));
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    assert_eq!(
        context["build"]["cargo_lock_sha256"].as_str().expect("lockfile receipt"),
        sha256_file(&root.join("Cargo.lock"))
    );
    let sources = context["source_receipts"].as_array().expect("source receipts");
    assert!(!sources.is_empty());
    for source in sources {
        let path = Path::new(source["path"].as_str().expect("relative source path"));
        assert!(
            !path.is_absolute()
                && !path.components().any(|part| part == std::path::Component::ParentDir)
        );
        assert_eq!(
            source["sha256"].as_str().expect("source SHA256"),
            sha256_file(&root.join(path)),
            "source changed after build receipt"
        );
    }
    assert!(context["build"]["sdk_locked_packages"]
        .as_array()
        .is_some_and(|packages| !packages.is_empty()));
}

fn emit(file: &mut File, value: &impl Serialize) {
    let line = serde_json::to_string(value).expect("serialize native acceptance evidence");
    let record: Value = serde_json::from_str(&line).expect("compact progress fields");
    println!(
        "MONDRIAN_NATIVE_PERF={}",
        json!({"kind":record["kind"],"format":record["format"],
        "mode":record["mode"],"max_block_frames":record["max_block_frames"],
        "partition":record["partition"],"acceptance":record["acceptance"],"accepted":record["accepted"]})
    );
    writeln!(file, "{line}").expect("write native acceptance evidence");
    file.flush().expect("flush each completed case before acceptance assertions");
}
