// Owned observations from one deactivated parameter-list acquisition.
// This Windows-only test consumer does not publish schema or processing state.

use super::*;
use clack_extensions::params::{ParamInfoScalars, ParamQueryReturn};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

const COPIED_NAME_CAP: usize = 256;
const CASE_ENV: &str = "MONDRIAN_CLAP_DEACTIVATED_PARAMETER_CASE";
const CASE_HASH_ENV: &str = "MONDRIAN_CLAP_DEACTIVATED_PARAMETER_CASE_SHA256";
const MAX_CASE_BYTES: u64 = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const OUTPUT_MARKER: &str = "MONDRIAN_CLAP_DEACTIVATED_SAME_CALL_V1 ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CopiedParameterObservation {
    id: u32,
    boolean_flags: u8,
    min_bits: u64,
    max_bits: u64,
    default_bits: u64,
    current_bits: u64,
    name_prefix: [u8; COPIED_NAME_CAP],
    name_prefix_bytes: usize,
    name_total_bytes: usize,
}

impl ParameterQueryCapture {
    // Called only after the original push has completed. No query, allocation,
    // borrowed native data, formatting or native callback occurs here.
    pub(super) fn record_copied(&mut self, index: u32, descriptor: &ClapParameterDescriptor) {
        let Some(row) = self.rows.last_mut().filter(|row| row.index == index) else {
            self.allocation_or_capacity_loss = true;
            return;
        };
        let mut name_prefix = [0; COPIED_NAME_CAP];
        let name = descriptor.name.as_bytes();
        let length = name.len().min(COPIED_NAME_CAP);
        name_prefix[..length].copy_from_slice(&name[..length]);
        row.copied = Some(CopiedParameterObservation {
            id: descriptor.id,
            boolean_flags: u8::from(descriptor.automatable)
                | (u8::from(descriptor.stepped) << 1)
                | (u8::from(descriptor.enumeration) << 2)
                | (u8::from(descriptor.read_only) << 3)
                | (u8::from(descriptor.hidden) << 4),
            min_bits: descriptor.min_value.to_bits(),
            max_bits: descriptor.max_value.to_bits(),
            default_bits: descriptor.default_value.to_bits(),
            current_bits: descriptor.current_value.to_bits(),
            name_prefix,
            name_prefix_bytes: length,
            name_total_bytes: name.len(),
        });
    }
}

struct DeactivatedParameterList {
    parameters: Result<Vec<ClapParameterDescriptor>, AudioProcessorHostError>,
    capture: Option<ParameterQueryCapture>,
    before_metadata: MainSourceObservation,
    after_metadata: MainSourceObservation,
    after_drop: MainSourceObservation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MainSourceObservation {
    source: super::super::clap_parameter_source::ClapParameterSourceSnapshot,
    callback_pending: bool,
}

fn observe_main_source(shared: &ClapHostShared) -> MainSourceObservation {
    MainSourceObservation {
        source: shared.parameter_source.snapshot(),
        callback_pending: shared.has_unserviced_callbacks(),
    }
}

// Both setup and native destruction execute on this caller's owning thread.
// A native hang/crash has no returned carrier; the actual outer Job settles it.
fn deactivated_parameters(
    entry: &PluginEntry,
    plugin_id: &str,
    state: Option<&[u8]>,
    observed: bool,
) -> Result<DeactivatedParameterList, AudioProcessorHostError> {
    if state.is_some_and(|bytes| bytes.len() > MAX_CAPTURED_STATE_BYTES) {
        return Err(invalid("diagnostic CLAP state exceeds its bound"));
    }
    let plugin_c_id =
        CString::new(plugin_id).map_err(|_| invalid("CLAP probe plugin ID contains a NUL byte"))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    let descriptors = factory.plugin_descriptors();
    if descriptors.len() > MAX_DESCRIPTORS {
        return Err(invalid("CLAP library exceeds descriptor capacity"));
    }
    if descriptors
        .filter(|descriptor| descriptor.id() == Some(plugin_c_id.as_c_str()))
        .take(2)
        .count()
        != 1
    {
        return Err(unavailable("CLAP probe plugin ID is missing or duplicated"));
    }
    let info = HostInfo::new(
        "Mondrian",
        "Mondrian",
        "https://github.com/shaloong/mondrian",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| invalid(format!("invalid CLAP host identity: {error}")))?;
    let shared = ClapHostShared::new();
    let native_shared = shared.clone().with_native_logs()?;
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| native_shared,
        |shared| super::super::clap_main_services::ClapHostMainThread::new(shared),
        entry,
        plugin_c_id.as_c_str(),
        &info,
    )
    .map_err(|error| unavailable(format!("CLAP probe instance failed: {error}")))?;
    shared.service_callbacks(&mut instance)?;
    if let Some(state) = state {
        let handle = instance.plugin_handle();
        let extension = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| unavailable("CLAP plugin does not support state restoration"))?;
        let mut reader = state;
        extension
            .load(&handle, &mut reader)
            .map_err(|error| unavailable(format!("CLAP probe state restore failed: {error}")))?;
        shared.service_callbacks(&mut instance)?;
    }
    configure_render_mode(&instance.plugin_handle(), AudioProcessingMode::Offline)?;
    shared.service_callbacks(&mut instance)?;
    let mut capture = observed.then(ParameterQueryCapture::default);
    let before_metadata = observe_main_source(&shared);
    // Keep the Result owned: an original failure cannot discard the same-call
    // capture or cause a diagnostic catch-up query.
    let parameters = list_parameters_observed(&instance.plugin_handle(), capture.as_mut());
    let after_metadata = observe_main_source(&shared);
    drop(instance);
    let after_drop = observe_main_source(&shared);
    Ok(DeactivatedParameterList {
        parameters,
        capture,
        before_metadata,
        after_metadata,
        after_drop,
    })
}

fn bits(bits: u64) -> String {
    format!("{bits:016X}")
}

fn bounded_error(error: &AudioProcessorHostError) -> serde_json::Value {
    let mut text = DiscoveryErrorText {
        text: String::with_capacity(MAX_DISCOVERY_ERROR_TEXT_BYTES),
        truncated: false,
    };
    if std::fmt::write(&mut text, format_args!("{error:?}")).is_err() {
        text.truncated = true;
    }
    serde_json::json!({"debug": text.text, "truncated": text.truncated})
}

fn info_json(info: Option<ParamInfoScalars>) -> serde_json::Value {
    match info {
        Some(info) => serde_json::json!({
            "native_id": info.id,
            "flags": info.flags,
            "min_bits": bits(info.min_bits),
            "max_bits": bits(info.max_bits),
            "default_bits": bits(info.default_bits),
        }),
        None => serde_json::Value::Null,
    }
}

fn native_return(returned: ParamQueryReturn) -> &'static str {
    match returned {
        ParamQueryReturn::MissingCallback => "missing_callback",
        ParamQueryReturn::NativeFalse => "native_false",
        ParamQueryReturn::NativeTrue => "native_true",
    }
}

fn copied_json(copied: Option<CopiedParameterObservation>) -> serde_json::Value {
    match copied {
        Some(copied) => {
            let mut name_hex = String::with_capacity(copied.name_prefix_bytes * 2);
            use std::fmt::Write as _;
            for byte in &copied.name_prefix[..copied.name_prefix_bytes] {
                // Writing to String cannot return a formatting error.
                let _ = write!(name_hex, "{byte:02X}");
            }
            serde_json::json!({
                "native_id": copied.id,
                "boolean_flags": copied.boolean_flags,
                "min_bits": bits(copied.min_bits),
                "max_bits": bits(copied.max_bits),
                "default_bits": bits(copied.default_bits),
                "current_bits": bits(copied.current_bits),
                "name_prefix_hex": name_hex,
                "name_total_bytes": copied.name_total_bytes,
                "name_truncated": copied.name_prefix_bytes != copied.name_total_bytes,
            })
        }
        None => serde_json::Value::Null,
    }
}

fn capture_json(capture: &ParameterQueryCapture) -> serde_json::Value {
    let rows: Vec<_> = capture
        .rows
        .iter()
        .map(|row| {
            let info = row.info.map(|info| {
                serde_json::json!({
                    "native_return": native_return(info.native_return),
                    "raw_after": info_json(info.raw_after),
                    "converted": info_json(info.converted),
                })
            });
            let value = row.value.map(|value| {
                serde_json::json!({
                    "native_return": native_return(value.native_return),
                    "initialized_before_bits": bits(value.initialized_before_bits),
                    "raw_after_bits": value.raw_after_bits.map(bits),
                    "converted_bits": value.converted_bits.map(bits),
                })
            });
            serde_json::json!({
                "index": row.index,
                "info": info,
                "value": value,
                "copied": copied_json(row.copied),
            })
        })
        .collect();
    serde_json::json!({
        "reported_count": capture.reported_count,
        "recorded_rows": rows.len(),
        "allocation_or_capacity_loss": capture.allocation_or_capacity_loss,
        "query_reached_reported_count": capture.reported_count.map(|count| rows.len() == count as usize),
        "fresh_info_buffer_constructor": "zeroed",
        "constructor_is_native_write_coverage": false,
        "rows": rows,
    })
}

fn phase_json(
    name: &'static str,
    phase: &Result<DeactivatedParameterList, AudioProcessorHostError>,
) -> serde_json::Value {
    match phase {
        Ok(phase) => serde_json::json!({
            "phase": name,
            "setup": "succeeded",
            "metadata_result": if phase.parameters.is_ok() { "succeeded" } else { "refused" },
            "metadata_error": phase.parameters.as_ref().err().map(bounded_error),
            "native_drop_returned": true,
            "main_source_before_metadata": source_json(phase.before_metadata),
            "main_source_after_metadata": source_json(phase.after_metadata),
            "main_source_after_drop": source_json(phase.after_drop),
            "capture": phase.capture.as_ref().map(capture_json),
        }),
        Err(error) => serde_json::json!({
            "phase": name,
            "setup": "refused_before_owned_list_return",
            "setup_error": bounded_error(error),
            "native_drop_returned": null,
            "capture": null,
        }),
    }
}

fn source_json(observation: MainSourceObservation) -> serde_json::Value {
    let source = observation.source;
    serde_json::json!({
        "revision_hex": bits(source.revision),
        "pending_output": source.pending_output,
        "pending_author_output": source.pending_author_output,
        "pending_controls": source.pending_controls,
        "open_parameter_gestures": source.open_parameter_gestures,
        "incomplete_bits": source.incomplete,
        "publishing_output": source.publishing_output,
        "unserviced_callback_pending": observation.callback_pending,
        "callback_pending_is_part_of_coherent_source_snapshot": false,
        "raw_rescan_flags": "unavailable_not_observed",
        "snapshot_proves_complete_destroy_notifications": false,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    path: PathBuf,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticCase {
    schema_version: u32,
    plugin: Binding,
    opaque_state: Binding,
    plugin_id: String,
    expected_reported_parameter_count: u32,
}

fn require_actual_job() {
    let mut inside = 0;
    // SAFETY: valid current-process pseudo handle and live BOOL output; null
    // job asks membership only. The external receipt binds the actual Job.
    let ok = unsafe {
        windows_sys::Win32::System::JobObjects::IsProcessInJob(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            std::ptr::null_mut(),
            &mut inside,
        )
    };
    assert_ne!(ok, 0, "actual Job membership query failed");
    assert_ne!(inside, 0, "frozen outer native Job required");
}

fn no_reparse(path: &Path) {
    assert!(path.is_absolute(), "absolute diagnostic input required");
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(ancestor).expect("existing diagnostic ancestor");
        assert_eq!(
            metadata.file_attributes() & 0x400,
            0,
            "reparse input ancestor"
        );
    }
}

fn lease_binding(binding: &Binding, cap: u64) -> fs::File {
    no_reparse(&binding.path);
    assert!(
        binding.bytes > 0 && binding.bytes <= cap,
        "bounded exact input size"
    );
    assert!(
        binding.sha256.len() == 64
            && binding
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "lowercase SHA256"
    );
    let mut file = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&binding.path)
        .expect("read-only no-write/delete input lease");
    let metadata = file.metadata().expect("leased input metadata");
    assert!(metadata.is_file());
    assert_eq!(metadata.len(), binding.bytes);
    let mut hash = Sha256::new();
    let mut block = [0; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let length = file.read(&mut block).expect("hash exact leased bytes");
        if length == 0 {
            break;
        }
        bytes += length as u64;
        assert!(bytes <= cap, "leased input grew beyond cap");
        hash.update(&block[..length]);
    }
    assert_eq!(bytes, binding.bytes);
    assert_eq!(format!("{:x}", hash.finalize()), binding.sha256);
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0)).expect("rewind held input lease");
    file
}

fn evidence_available(phase: &Result<DeactivatedParameterList, AudioProcessorHostError>) -> bool {
    let Ok(phase) = phase else {
        return false;
    };
    let Some(capture) = &phase.capture else {
        return false;
    };
    capture.reported_count.is_some_and(|count| count <= MAX_PARAMETERS)
        && !capture.allocation_or_capacity_loss
        && capture.rows.iter().all(|row| row.info.is_some())
        && capture.rows.iter().all(|row| {
            row.copied
                .is_none_or(|copied| copied.name_prefix_bytes == copied.name_total_bytes)
        })
}

#[test]
#[ignore = "explicit hash-bound deactivated metadata case and frozen native Job required"]
fn native_clap_deactivated_same_call_metadata_observations() {
    require_actual_job();
    let case_path = PathBuf::from(std::env::var_os(CASE_ENV).expect("explicit diagnostic case"));
    let expected_case_hash = std::env::var(CASE_HASH_ENV).expect("explicit diagnostic case hash");
    no_reparse(&case_path);
    let case_metadata = fs::metadata(&case_path).expect("ordinary diagnostic case");
    let case_binding = Binding {
        path: case_path,
        bytes: case_metadata.len(),
        sha256: expected_case_hash,
    };
    let mut case_lease = lease_binding(&case_binding, MAX_CASE_BYTES);
    let mut bytes = Vec::with_capacity(case_binding.bytes as usize);
    Read::by_ref(&mut case_lease)
        .take(MAX_CASE_BYTES + 1)
        .read_to_end(&mut bytes)
        .expect("read bounded case");
    assert_eq!(bytes.len() as u64, case_binding.bytes);
    let case: DiagnosticCase =
        serde_json::from_slice(&bytes).expect("complete strict diagnostic case");
    assert_eq!(case.schema_version, 1);
    assert!(case.expected_reported_parameter_count <= MAX_PARAMETERS);
    assert!(
        !case.plugin_id.is_empty() && case.plugin_id.len() <= 256 && !case.plugin_id.contains('\0')
    );
    let plugin_lease = lease_binding(&case.plugin, MAX_CLAP_BINARY_BYTES);
    let mut state_lease = lease_binding(&case.opaque_state, MAX_CAPTURED_STATE_BYTES as u64);
    let mut seed = Vec::with_capacity(case.opaque_state.bytes as usize);
    Read::by_ref(&mut state_lease)
        .take(MAX_CAPTURED_STATE_BYTES as u64 + 1)
        .read_to_end(&mut seed)
        .expect("read original bounded opaque state");
    assert_eq!(seed.len() as u64, case.opaque_state.bytes);
    // SAFETY: Root's frozen DLL lease and actual Job cover this native loader
    // call; PluginEntry owns the library and outlives both complete instances.
    let entry = unsafe { PluginEntry::load(case.plugin.path.as_os_str()) }
        .expect("load actual leased CLAP entry");
    // Two predeclared phases. A semantic refusal never selects or suppresses B.
    let phase_a = deactivated_parameters(&entry, &case.plugin_id, None, true);
    let phase_b = deactivated_parameters(&entry, &case.plugin_id, Some(&seed), true);
    drop(entry);
    let output = serde_json::json!({
        "schema_version": 1,
        "consumer": "windows_audio_test_pe_deactivated_metadata",
        "compatibility_qualified": false,
        "real_plugin_native_exact_counts_qualified": false,
        "plugin_sha256": case.plugin.sha256,
        "opaque_state_sha256": case.opaque_state.sha256,
        "plugin_id": case.plugin_id,
        "same_loaded_library_two_fresh_instances": true,
        "host_parameter_rescan_flags": "unavailable_not_observed",
        "phases": [phase_json("fresh_unseeded", &phase_a), phase_json("fresh_original_seed", &phase_b)],
    });
    let output = serde_json::to_vec(&output).expect("serialize owned exact-bit observations");
    assert!(
        output.len() <= MAX_OUTPUT_BYTES,
        "bounded diagnostic output"
    );
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(OUTPUT_MARKER.as_bytes())
        .expect("write fixed observation marker");
    stdout.write_all(&output).expect("write owned observations");
    stdout.write_all(b"\n").expect("terminate observation record");
    stdout.flush().expect("flush before original-result assertions");
    drop(stdout);
    // Root retains raw output regardless of the original result's eventual 101.
    assert!(
        evidence_available(&phase_a) && evidence_available(&phase_b),
        "diagnostic evidence unavailable/truncated"
    );
    for phase in [&phase_a, &phase_b] {
        let phase = phase.as_ref().expect("original setup result");
        assert_eq!(
            phase.capture.as_ref().unwrap().reported_count,
            Some(case.expected_reported_parameter_count)
        );
        phase.parameters.as_ref().expect("original deactivated parameter-list result");
    }
    drop(state_lease);
    drop(plugin_lease);
    drop(case_lease);
}

mod owned_abi_controls {
    use super::*;
    use clack_extensions::params::{
        ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
        PluginMainThreadParams,
    };
    use clack_extensions::render::{PluginRender, PluginRenderImpl, RenderMode};
    use clack_plugin::events::io::{InputEvents, OutputEvents};
    use clack_plugin::prelude::{
        Audio, DefaultPluginFactory, Events, HostAudioProcessorHandle, HostMainThreadHandle,
        HostSharedHandle, Plugin, PluginAudioConfiguration as PluginConfig, PluginDescriptor,
        PluginError, PluginExtensions, PluginMainThread, PluginShared, Process, ProcessStatus,
        SinglePluginEntry,
    };
    use std::cell::RefCell;

    #[derive(Clone, Copy)]
    enum Mode {
        Success,
        NumericRefusal,
        InfoFalse,
        Utf8Refusal,
        ValueFalse,
    }

    struct Audit {
        mode: Mode,
        count_calls: AtomicU32,
        info_calls: AtomicU32,
        value_calls: AtomicU32,
        order: AtomicU32,
        render_calls: AtomicU32,
        activate_calls: AtomicU32,
        process_calls: AtomicU32,
        deactivate_calls: AtomicU32,
        destroy_calls: AtomicU32,
    }

    impl Audit {
        fn new(mode: Mode) -> Self {
            Self {
                mode,
                count_calls: AtomicU32::new(0),
                info_calls: AtomicU32::new(0),
                value_calls: AtomicU32::new(0),
                order: AtomicU32::new(0),
                render_calls: AtomicU32::new(0),
                activate_calls: AtomicU32::new(0),
                process_calls: AtomicU32::new(0),
                deactivate_calls: AtomicU32::new(0),
                destroy_calls: AtomicU32::new(0),
            }
        }
        fn record(&self, code: u32) {
            let _ = self.order.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                Some((old << 4) | code)
            });
        }
        fn queries(&self) -> [u32; 4] {
            [
                self.count_calls.load(Ordering::SeqCst),
                self.info_calls.load(Ordering::SeqCst),
                self.value_calls.load(Ordering::SeqCst),
                self.order.load(Ordering::SeqCst),
            ]
        }
    }

    thread_local! {
        static NEXT_AUDIT: RefCell<Option<Arc<Audit>>> = const { RefCell::new(None) };
    }
    struct Fixture;
    struct Shared(Arc<Audit>);
    struct Main<'a>(&'a Shared);
    struct Processor<'a>(&'a Shared);

    impl PluginShared<'_> for Shared {}
    impl<'a> PluginMainThread<'a, Shared> for Main<'a> {}
    impl Plugin for Fixture {
        type Shared<'a> = Shared;
        type MainThread<'a> = Main<'a>;
        type AudioProcessor<'a> = Processor<'a>;
        fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
            builder.register::<PluginParams>().register::<PluginRender>();
        }
    }
    impl DefaultPluginFactory for Fixture {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new(
                "org.mondrian.test.deactivated-parameter-capture",
                "Owned Query Capture",
            )
        }
        fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
            NEXT_AUDIT
                .with(|next| next.borrow_mut().take())
                .map(Shared)
                .ok_or(PluginError::Message("owned audit missing"))
        }
        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            shared: &'a Shared,
        ) -> Result<Main<'a>, PluginError> {
            Ok(Main(shared))
        }
    }
    impl PluginMainThreadParams for Main<'_> {
        fn count(&self) -> u32 {
            self.0 .0.count_calls.fetch_add(1, Ordering::SeqCst);
            self.0 .0.record(0xC);
            3
        }
        fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
            self.0 .0.info_calls.fetch_add(1, Ordering::SeqCst);
            self.0 .0.record(match index {
                0 => 1,
                1 => 3,
                2 => 5,
                _ => 0xD,
            });
            if index >= 3 || (index == 1 && matches!(self.0 .0.mode, Mode::InfoFalse)) {
                return;
            }
            let name: &[u8] = if index == 1 && matches!(self.0 .0.mode, Mode::Utf8Refusal) {
                b"\xFF"
            } else {
                [
                    b"First".as_slice(),
                    b"Middle".as_slice(),
                    b"Last".as_slice(),
                ][index as usize]
            };
            let default = if index == 1 && matches!(self.0 .0.mode, Mode::NumericRefusal) {
                f64::from_bits(0xC79D_08E1_0000_0000)
            } else {
                0.5
            };
            writer.set(&ParamInfo {
                id: ClapId::new(7 + index),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Default::default(),
                name,
                module: b"Owned",
                min_value: -0.0,
                max_value: 1.0,
                default_value: default,
            });
        }
        fn get_value(&self, id: ClapId) -> Option<f64> {
            self.0 .0.value_calls.fetch_add(1, Ordering::SeqCst);
            self.0 .0.record(match id.get() {
                7 => 2,
                8 => 4,
                9 => 6,
                _ => 0xE,
            });
            if !(7..=9).contains(&id.get()) {
                return None;
            }
            if id.get() == 8 && matches!(self.0 .0.mode, Mode::ValueFalse) {
                None
            } else {
                Some(if id.get() == 8 { -0.0 } else { 0.5 })
            }
        }
        fn value_to_text(
            &self,
            _id: ClapId,
            _value: f64,
            _writer: &mut ParamDisplayWriter,
        ) -> core::fmt::Result {
            Err(core::fmt::Error)
        }
        fn text_to_value(&self, _id: ClapId, _text: &std::ffi::CStr) -> Option<f64> {
            None
        }
        fn flush(&self, _input: &InputEvents, _output: &mut OutputEvents) {}
    }
    impl PluginRenderImpl for Main<'_> {
        fn has_hard_realtime_requirement(&self) -> bool {
            false
        }
        fn set(&self, _mode: RenderMode) -> Result<(), PluginError> {
            self.0 .0.render_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    impl PluginAudioProcessorParams for Processor<'_> {
        fn flush(&mut self, _input: &InputEvents, _output: &mut OutputEvents) {}
    }
    impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, Shared, Main<'a>> for Processor<'a> {
        fn activate(
            _host: HostAudioProcessorHandle<'a>,
            _main: &Main<'a>,
            shared: &'a Shared,
            _config: PluginConfig,
        ) -> Result<Self, PluginError> {
            shared.0.activate_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Self(shared))
        }
        fn process(
            &mut self,
            _process: Process,
            _audio: Audio,
            _events: Events,
        ) -> Result<ProcessStatus, PluginError> {
            self.0 .0.process_calls.fetch_add(1, Ordering::SeqCst);
            Err(PluginError::Message(
                "deactivated diagnostic must not process",
            ))
        }
        fn deactivate(self, _main: &Main<'a>) {
            self.0 .0.deactivate_calls.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Drop for Main<'_> {
        fn drop(&mut self) {
            self.0 .0.destroy_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn cycle(mode: Mode, observed: bool) -> (DeactivatedParameterList, Arc<Audit>) {
        let audit = Arc::new(Audit::new(mode));
        NEXT_AUDIT.with(|next| {
            assert!(next.borrow().is_none(), "previous owned setup unused");
            *next.borrow_mut() = Some(Arc::clone(&audit));
        });
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<Fixture>>(
            c"/test/deactivated-capture",
        )
        .expect("owned native ABI entry");
        let result = deactivated_parameters(
            &entry,
            "org.mondrian.test.deactivated-parameter-capture",
            None,
            observed,
        )
        .expect("owned setup reaches original list");
        NEXT_AUDIT.with(|next| assert!(next.borrow().is_none(), "owned setup consumed"));
        assert_eq!(audit.render_calls.load(Ordering::SeqCst), 1);
        assert_eq!(audit.activate_calls.load(Ordering::SeqCst), 0);
        assert_eq!(audit.process_calls.load(Ordering::SeqCst), 0);
        assert_eq!(audit.deactivate_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            audit.destroy_calls.load(Ordering::SeqCst),
            1,
            "capture outlives actual native destruction"
        );
        assert_eq!(result.before_metadata, result.after_metadata);
        assert_eq!(result.after_metadata, result.after_drop);
        assert_eq!(result.after_drop.source.revision, 0);
        assert!(!result.after_drop.callback_pending);
        drop(entry);
        (result, audit)
    }

    fn compare(mode: Mode, expected: [u32; 4]) -> DeactivatedParameterList {
        let (plain, plain_audit) = cycle(mode, false);
        let (observed, observed_audit) = cycle(mode, true);
        assert_eq!(plain_audit.queries(), expected);
        assert_eq!(
            observed_audit.queries(),
            expected,
            "observations cannot duplicate or finish queries"
        );
        match (&plain.parameters, &observed.parameters) {
            (Ok(plain), Ok(observed)) => assert_eq!(plain, observed),
            (
                Err(AudioProcessorHostError::InvalidContract(plain)),
                Err(AudioProcessorHostError::InvalidContract(observed)),
            ) => {
                assert!(
                    observed.starts_with(&format!("{plain}; same_call ")),
                    "original refusal and existing suffix order"
                );
            }
            _ => panic!("original list result changed"),
        }
        observed
    }

    #[test]
    fn deactivated_capture_success_survives_native_drop_with_once_only_queries() {
        let observed = compare(Mode::Success, [1, 3, 3, 0xC12_3456]);
        assert!(observed.parameters.is_ok());
        let capture = observed.capture.unwrap();
        assert_eq!(capture.reported_count, Some(3));
        assert_eq!(capture.rows.len(), 3);
        assert!(!capture.allocation_or_capacity_loss);
        let middle = capture.rows[1];
        let copied = middle.copied.unwrap();
        assert_eq!(copied.id, 8);
        assert_eq!(copied.boolean_flags, 1);
        assert_eq!(copied.min_bits, 0x8000_0000_0000_0000);
        assert_eq!(copied.default_bits, 0x3FE0_0000_0000_0000);
        assert_eq!(copied.current_bits, 0x8000_0000_0000_0000);
        assert_eq!(&copied.name_prefix[..copied.name_prefix_bytes], b"Middle");
        let info = middle.info.unwrap();
        assert_eq!(info.raw_after, info.converted);
        assert_eq!(info.raw_after.unwrap().default_bits, copied.default_bits);
        assert_eq!(
            middle.value.unwrap().converted_bits,
            Some(copied.current_bits)
        );
    }

    #[test]
    fn deactivated_capture_numeric_refusal_retains_all_actual_copied_rows() {
        let observed = compare(Mode::NumericRefusal, [1, 3, 3, 0xC12_3456]);
        assert!(
            matches!(&observed.parameters, Err(AudioProcessorHostError::InvalidContract(error))
            if error.contains("index=1 native_id=8 reason=default_out_of_range"))
        );
        let capture = observed.capture.unwrap();
        assert_eq!(
            capture.rows.len(),
            3,
            "original validation runs after the full acquisition"
        );
        assert!(capture.rows.iter().all(|row| row.copied.is_some()));
        let middle = capture.rows[1];
        let info = middle.info.unwrap();
        assert_eq!(info.raw_after, info.converted);
        assert_eq!(info.raw_after.unwrap().default_bits, 0xC79D_08E1_0000_0000);
        assert_eq!(middle.copied.unwrap().default_bits, 0xC79D_08E1_0000_0000);
        let encoded = serde_json::to_vec(&capture_json(&capture)).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            decoded["rows"][1]["copied"]["default_bits"],
            "C79D08E100000000"
        );
        assert_eq!(
            decoded["rows"][1]["copied"]["name_prefix_hex"],
            "4D6964646C65"
        );
        assert!(encoded.len() < MAX_OUTPUT_BYTES);
    }

    #[test]
    fn deactivated_capture_info_false_preserves_original_early_stop() {
        let observed = compare(Mode::InfoFalse, [1, 2, 1, 0xC123]);
        assert!(
            matches!(&observed.parameters, Err(AudioProcessorHostError::InvalidContract(error))
            if error.starts_with("CLAP parameter metadata is missing; same_call "))
        );
        let capture = observed.capture.unwrap();
        assert_eq!(capture.rows.len(), 2);
        assert!(capture.rows[0].copied.is_some());
        let row = capture.rows[1];
        let info = row.info.unwrap();
        assert_eq!(info.native_return, ParamQueryReturn::NativeFalse);
        assert_eq!(info.raw_after, None);
        assert_eq!(info.converted, None);
        assert_eq!(row.value, None);
        assert_eq!(row.copied, None);
    }

    #[test]
    fn deactivated_capture_utf8_refusal_does_not_invent_name_or_value() {
        let observed = compare(Mode::Utf8Refusal, [1, 2, 1, 0xC123]);
        assert!(
            matches!(&observed.parameters, Err(AudioProcessorHostError::InvalidContract(error))
            if error.starts_with("CLAP parameter name is not UTF-8; same_call "))
        );
        let capture = observed.capture.unwrap();
        assert_eq!(capture.rows.len(), 2);
        let row = capture.rows[1];
        assert_eq!(
            row.info.unwrap().native_return,
            ParamQueryReturn::NativeTrue
        );
        assert!(row.info.unwrap().converted.is_some());
        assert_eq!(row.value, None);
        assert_eq!(row.copied, None);
        assert_eq!(
            capture_json(&capture)["rows"][1]["copied"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn deactivated_capture_value_false_keeps_failed_push_unavailable() {
        let observed = compare(Mode::ValueFalse, [1, 2, 2, 0xC1234]);
        assert!(
            matches!(&observed.parameters, Err(AudioProcessorHostError::InvalidContract(error))
            if error.starts_with("CLAP parameter current value is unavailable; same_call "))
        );
        let capture = observed.capture.unwrap();
        assert_eq!(capture.rows.len(), 2);
        let row = capture.rows[1];
        assert!(row.info.unwrap().converted.is_some());
        let value = row.value.unwrap();
        assert_eq!(value.native_return, ParamQueryReturn::NativeFalse);
        assert_eq!(value.raw_after_bits, None);
        assert_eq!(value.converted_bits, None);
        assert_eq!(row.copied, None, "original push never completed");
    }

    #[test]
    fn deactivated_capture_copied_prefix_is_bounded_and_missing_row_is_loss() {
        let (observed, _) = cycle(Mode::Success, true);
        let mut capture = observed.capture.unwrap();
        let mut descriptor = observed.parameters.unwrap().remove(2);
        descriptor.name = "x".repeat(COPIED_NAME_CAP + 1);
        descriptor.default_value = f64::from_bits(0x7FF8_0000_0000_0001);
        capture.record_copied(2, &descriptor);
        let copied = capture.rows[2].copied.unwrap();
        assert_eq!(copied.name_prefix_bytes, COPIED_NAME_CAP);
        assert_eq!(copied.name_total_bytes, COPIED_NAME_CAP + 1);
        let json = copied_json(Some(copied));
        assert_eq!(json["default_bits"], "7FF8000000000001");
        assert_eq!(
            json["name_prefix_hex"].as_str().unwrap().len(),
            COPIED_NAME_CAP * 2
        );
        assert_eq!(json["name_truncated"], true);
        let before = capture.rows.len();
        capture.record_copied(99, &descriptor);
        assert!(capture.allocation_or_capacity_loss);
        assert_eq!(
            capture.rows.len(),
            before,
            "cannot manufacture missing rows"
        );
        assert_eq!(capture.rows[2].copied, Some(copied));
    }
}
