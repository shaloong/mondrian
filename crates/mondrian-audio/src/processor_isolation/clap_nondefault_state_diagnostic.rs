//! Finite inactive-main parameter mutation and same-instance state save.
//! This adapter control grants no canonical author, project, or DSP qualification.
use super::clap_deactivated_parameter_diagnostic::{
    deactivated_instance, lease_binding, no_reparse, require_actual_job, Binding,
};
use super::*;
use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::{InputEvents, OutputEvents};
use clack_host::events::Pckn;

const CASE_ENV: &str = "MONDRIAN_CLAP_NONDEFAULT_STATE_CASE";
const CASE_HASH_ENV: &str = "MONDRIAN_CLAP_NONDEFAULT_STATE_CASE_SHA256";
const MAX_CASE_BYTES: u64 = 64 * 1024;
const MAX_SIDECAR_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateCase {
    schema_version: u32,
    plugin: Binding,
    plugin_id: String,
    native_parameter_id: u32,
    expected_fresh_value: f64,
    target_plain_value: f64,
    seed_path: PathBuf,
    sidecar_path: PathBuf,
}

struct CapturedState {
    parameter_count: usize,
    before: ClapParameterDescriptor,
    after_value: f64,
    bytes: Vec<u8>,
}

fn capture_nondefault_state(
    entry: &PluginEntry,
    plugin_id: &str,
    parameter_id: u32,
    expected_fresh_value: f64,
    target: f64,
) -> Result<CapturedState, AudioProcessorHostError> {
    let (shared, mut instance) = deactivated_instance(entry, plugin_id, None)?;
    let result = (|| {
        let parameters = list_parameters_observed(&instance.plugin_handle(), None)?;
        let parameter_count = parameters.len();
        let before = parameters
            .into_iter()
            .find(|p| p.id == parameter_id)
            .ok_or_else(|| invalid("diagnostic parameter is absent"))?;
        if before.read_only
            || before.current_value.to_bits() != expected_fresh_value.to_bits()
            || before.current_value == target
        {
            return Err(invalid(
                "diagnostic requires the declared writable nondefault transition",
            ));
        }
        // Apply the original validator to the proposed plain value, including
        // finite/range/stepped rules; do not normalize, clamp or rewrite defaults.
        let mut proposed = before.clone();
        proposed.current_value = target;
        validate_parameters(std::slice::from_ref(&proposed))?;
        let id = ClapId::new(parameter_id); // The original list admitted this ID.
        let events = [ParamValueEvent::new(0, id, Pckn::match_all(), target)];
        {
            let mut handle = instance
                .inactive_plugin_handle()
                .ok_or_else(|| invalid("diagnostic requires inactive main-thread flush"))?;
            let params = handle
                .get_extension::<PluginParams>()
                .ok_or_else(|| unavailable("diagnostic parameters extension disappeared"))?;
            // The typed handle enforces inactive/main ownership. No audio role,
            // activation, process call or native author prefix is manufactured.
            params.flush(
                &mut handle,
                &InputEvents::from_buffer(&events),
                &mut OutputEvents::void(),
            );
        }
        shared.service_callbacks(&mut instance)?;
        let handle = instance.plugin_handle();
        let params = handle
            .get_extension::<PluginParams>()
            .ok_or_else(|| unavailable("diagnostic parameters extension disappeared"))?;
        let after_value = params
            .get_value(&handle, id)
            .ok_or_else(|| invalid("diagnostic target readback is unavailable"))?;
        if after_value.to_bits() != target.to_bits() {
            return Err(invalid(
                "diagnostic target readback differs after inactive flush",
            ));
        }
        let state = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| unavailable("diagnostic plugin has no State extension"))?;
        let mut writer = BoundedStateWriter::default();
        let saved = state.save(&handle, &mut writer);
        if writer.exceeded || writer.bytes.len() > MAX_CAPTURED_STATE_BYTES {
            return Err(invalid("CLAP state exceeds the capture limit"));
        }
        saved.map_err(|error| unavailable(format!("CLAP state capture failed: {error}")))?;
        // Some(empty) is a successful real save, not absence of State support.
        Ok(CapturedState {
            parameter_count,
            before,
            after_value,
            bytes: writer.bytes,
        })
    })();
    drop(instance);
    result
}

fn validate_output(path: &Path) {
    assert!(
        path.is_absolute() && path.file_name().is_some(),
        "absolute output file required"
    );
    let parent = path.parent().expect("output parent");
    no_reparse(parent);
    assert!(fs::metadata(parent).expect("output parent metadata").is_dir());
    assert!(
        !path.try_exists().expect("output existence check"),
        "output must not exist"
    );
}

fn write_new(path: &Path, bytes: &[u8]) {
    // A failed/partial publication is never a qualified seed without the full
    // sidecar and original Job result; no old output is overwritten or reused.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("create new diagnostic output");
    file.write_all(bytes).expect("write complete diagnostic output");
    file.sync_all().expect("sync diagnostic output");
}

#[test]
#[ignore = "explicit hash-bound CLAP nondefault state case and frozen native Job required"]
fn native_clap_inactive_nondefault_parameter_state_capture() {
    require_actual_job();
    let path = PathBuf::from(std::env::var_os(CASE_ENV).expect("explicit state case"));
    let binding = Binding {
        bytes: fs::metadata(&path).expect("state case metadata").len(),
        path,
        sha256: std::env::var(CASE_HASH_ENV).expect("explicit state case SHA256"),
    };
    let mut case_lease = lease_binding(&binding, MAX_CASE_BYTES);
    let mut bytes = Vec::with_capacity(binding.bytes as usize);
    Read::by_ref(&mut case_lease)
        .take(MAX_CASE_BYTES + 1)
        .read_to_end(&mut bytes)
        .expect("read bounded state case");
    assert_eq!(bytes.len() as u64, binding.bytes);
    let case: StateCase = serde_json::from_slice(&bytes).expect("strict state case");
    assert_eq!(case.schema_version, 1);
    assert!(
        !case.plugin_id.is_empty() && case.plugin_id.len() <= 256 && !case.plugin_id.contains('\0')
    );
    assert!(case.expected_fresh_value.is_finite() && case.target_plain_value.is_finite());
    assert_ne!(case.seed_path, case.sidecar_path);
    validate_output(&case.seed_path);
    validate_output(&case.sidecar_path);
    let plugin_lease = lease_binding(&case.plugin, MAX_CLAP_BINARY_BYTES);
    // SAFETY: the held read-only DLL lease and actual outer Job bind this load.
    // The entry outlives the single instance on this same owning main thread.
    let entry = unsafe { PluginEntry::load(case.plugin.path.as_os_str()) }
        .expect("load actual leased CLAP entry");
    let captured = capture_nondefault_state(
        &entry,
        &case.plugin_id,
        case.native_parameter_id,
        case.expected_fresh_value,
        case.target_plain_value,
    );
    drop(entry);
    let captured = captured.expect("actual inactive mutation and same-instance state save");
    assert!(captured.bytes.len() <= MAX_CAPTURED_STATE_BYTES);
    let seed_hash = format!("{:x}", Sha256::digest(&captured.bytes));
    let sidecar = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1,
        "consumer": "windows_clap_inactive_nondefault_state_capture",
        "case_sha256": &binding.sha256,
        "plugin_sha256": &case.plugin.sha256,
        "plugin_id": &case.plugin_id,
        "native_parameter_id": case.native_parameter_id,
        "parameter_count": captured.parameter_count,
        "before_parameter": &captured.before,
        "expected_fresh_value_bits": format!("{:016X}", case.expected_fresh_value.to_bits()),
        "target_plain_value_bits": format!("{:016X}", case.target_plain_value.to_bits()),
        "after_value_bits": format!("{:016X}", captured.after_value.to_bits()),
        "seed": { "path": &case.seed_path, "bytes": captured.bytes.len(), "sha256": seed_hash },
        "state_saved": true, "state_loaded": false, "offline": true,
        "native_instance_drop_returned": true, "library_drop_returned": true,
        "activated": false, "processed_pcm": false,
        "native_output_events_qualified": false,
        "canonical_author_qualified": false, "project_persistence_qualified": false,
        "dsp_state_qualified": false, "historical_seed_qualified": false
    }))
    .expect("serialize owned state observations");
    assert!(sidecar.len() <= MAX_SIDECAR_BYTES, "bounded state sidecar");
    validate_output(&case.seed_path);
    validate_output(&case.sidecar_path);
    write_new(&case.seed_path, &captured.bytes);
    write_new(&case.sidecar_path, &sidecar);
    drop((plugin_lease, case_lease));
}

mod owned_abi_controls {
    use super::*;
    use clack_extensions::params::{
        ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
        PluginMainThreadParams,
    };
    use clack_extensions::render::{PluginRender, PluginRenderImpl, RenderMode};
    use clack_extensions::state::PluginStateImpl;
    use clack_plugin::prelude::{
        Audio, DefaultPluginFactory, Events, HostAudioProcessorHandle, HostMainThreadHandle,
        HostSharedHandle, Plugin, PluginAudioConfiguration, PluginDescriptor, PluginError,
        PluginExtensions, PluginMainThread, PluginShared, Process, ProcessStatus,
        SinglePluginEntry,
    };
    use clack_plugin::stream::{InputStream, OutputStream};
    use std::cell::RefCell;
    use std::sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    };

    const ID: u32 = 17;
    const PLUGIN_ID: &str = "org.mondrian.test.inactive-state-capture";
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Mode {
        Success,
        EmptyState,
        IgnoreFlush,
        MissingState,
        SaveError,
        Overflow,
        ReadOnly,
    }
    struct Audit {
        mode: Mode,
        owner: thread::ThreadId,
        value: AtomicU64,
        queries: AtomicUsize,
        flushes: AtomicUsize,
        saves: AtomicUsize,
        loads: AtomicUsize,
        offline: AtomicUsize,
        activated: AtomicUsize,
        processed: AtomicUsize,
        destroyed: AtomicUsize,
    }
    impl Audit {
        fn new(mode: Mode) -> Self {
            Self {
                mode,
                owner: thread::current().id(),
                value: AtomicU64::new(0.5_f64.to_bits()),
                queries: AtomicUsize::new(0),
                flushes: AtomicUsize::new(0),
                saves: AtomicUsize::new(0),
                loads: AtomicUsize::new(0),
                offline: AtomicUsize::new(0),
                activated: AtomicUsize::new(0),
                processed: AtomicUsize::new(0),
                destroyed: AtomicUsize::new(0),
            }
        }
        fn on_main(&self) {
            assert_eq!(thread::current().id(), self.owner);
        }
    }
    thread_local! { static NEXT: RefCell<Option<Arc<Audit>>> = const { RefCell::new(None) }; }
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
        fn declare_extensions(builder: &mut PluginExtensions<Self>, shared: Option<&Shared>) {
            builder.register::<PluginParams>().register::<PluginRender>();
            if shared.is_none_or(|s| s.0.mode != Mode::MissingState) {
                builder.register::<PluginState>();
            }
        }
    }
    impl DefaultPluginFactory for Fixture {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new(PLUGIN_ID, "Owned State Capture")
        }
        fn new_shared(_: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
            NEXT.with(|slot| slot.borrow_mut().take())
                .map(Shared)
                .ok_or(PluginError::Message("owned audit missing"))
        }
        fn new_main_thread<'a>(
            _: HostMainThreadHandle<'a>,
            shared: &'a Shared,
        ) -> Result<Main<'a>, PluginError> {
            shared.0.on_main();
            Ok(Main(shared))
        }
    }
    impl PluginMainThreadParams for Main<'_> {
        fn count(&self) -> u32 {
            1
        }
        fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
            self.0 .0.on_main();
            if index != 0 {
                return;
            }
            let flags = if self.0 .0.mode == Mode::ReadOnly {
                ParamInfoFlags::IS_AUTOMATABLE | ParamInfoFlags::IS_READONLY
            } else {
                ParamInfoFlags::IS_AUTOMATABLE
            };
            writer.set(&ParamInfo {
                id: ClapId::new(ID),
                flags,
                cookie: Default::default(),
                name: b"Owned Value",
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.5,
            });
        }
        fn get_value(&self, id: ClapId) -> Option<f64> {
            self.0 .0.on_main();
            self.0 .0.queries.fetch_add(1, Ordering::SeqCst);
            (id == ClapId::new(ID)).then(|| f64::from_bits(self.0 .0.value.load(Ordering::SeqCst)))
        }
        fn value_to_text(
            &self,
            _: ClapId,
            _: f64,
            _: &mut ParamDisplayWriter,
        ) -> core::fmt::Result {
            Err(core::fmt::Error)
        }
        fn text_to_value(&self, _: ClapId, _: &std::ffi::CStr) -> Option<f64> {
            None
        }
        fn flush(&self, input: &InputEvents, _: &mut OutputEvents) {
            self.0 .0.on_main();
            self.0 .0.flushes.fetch_add(1, Ordering::SeqCst);
            assert_eq!(input.len(), 1);
            let event = input.get(0).unwrap();
            assert_eq!(event.header().time(), 0);
            let value = event.as_event::<ParamValueEvent>().expect("actual typed parameter event");
            assert_eq!(value.param_id(), Some(ClapId::new(ID)));
            assert_eq!(value.value().to_bits(), 0.25_f64.to_bits());
            if self.0 .0.mode != Mode::IgnoreFlush {
                self.0 .0.value.store(value.value().to_bits(), Ordering::SeqCst);
            }
        }
    }
    impl PluginRenderImpl for Main<'_> {
        fn has_hard_realtime_requirement(&self) -> bool {
            false
        }
        fn set(&self, mode: RenderMode) -> Result<(), PluginError> {
            self.0 .0.on_main();
            assert_eq!(mode, RenderMode::Offline);
            self.0 .0.offline.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    impl PluginStateImpl for Main<'_> {
        fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
            self.0 .0.on_main();
            self.0 .0.saves.fetch_add(1, Ordering::SeqCst);
            assert_eq!(self.0 .0.value.load(Ordering::SeqCst), 0.25_f64.to_bits());
            match self.0 .0.mode {
                Mode::EmptyState => Ok(()),
                Mode::Overflow => {
                    // Deliberately swallow the writer error: its sticky limit
                    // must still reject the partial state after native success.
                    for _ in 0..17 {
                        let _ = output.write_all(&[7; 4096]);
                    }
                    Ok(())
                }
                Mode::SaveError => {
                    output.write_all(b"partial")?;
                    Err(PluginError::Message("owned save failure"))
                }
                _ => {
                    output.write_all(&self.0 .0.value.load(Ordering::SeqCst).to_le_bytes())?;
                    Ok(())
                }
            }
        }
        fn load(&self, _: &mut InputStream) -> Result<(), PluginError> {
            self.0 .0.loads.fetch_add(1, Ordering::SeqCst);
            Err(PluginError::Message("capture must not load state"))
        }
    }
    impl PluginAudioProcessorParams for Processor<'_> {
        fn flush(&mut self, _: &InputEvents, _: &mut OutputEvents) {}
    }
    impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, Shared, Main<'a>> for Processor<'a> {
        fn activate(
            _: HostAudioProcessorHandle<'a>,
            _: &Main<'a>,
            shared: &'a Shared,
            _: PluginAudioConfiguration,
        ) -> Result<Self, PluginError> {
            shared.0.activated.fetch_add(1, Ordering::SeqCst);
            Err(PluginError::Message("capture must not activate"))
        }
        fn process(
            &mut self,
            _: Process,
            _: Audio,
            _: Events,
        ) -> Result<ProcessStatus, PluginError> {
            self.0 .0.processed.fetch_add(1, Ordering::SeqCst);
            Err(PluginError::Message("capture must not process"))
        }
        fn deactivate(self, _: &Main<'a>) {
            panic!("capture must not deactivate");
        }
    }
    impl Drop for Main<'_> {
        fn drop(&mut self) {
            self.0 .0.on_main();
            self.0 .0.destroyed.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn cycle(
        mode: Mode,
        expected: f64,
        target: f64,
    ) -> (Result<CapturedState, AudioProcessorHostError>, Arc<Audit>) {
        let audit = Arc::new(Audit::new(mode));
        NEXT.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(audit.clone());
        });
        let entry =
            PluginEntry::load_from_clack::<SinglePluginEntry<Fixture>>(c"/test/inactive-state")
                .expect("owned actual ABI entry");
        let result = capture_nondefault_state(&entry, PLUGIN_ID, ID, expected, target);
        assert_eq!(
            audit.destroyed.load(Ordering::SeqCst),
            1,
            "instance Drop before owned result"
        );
        drop(entry);
        assert_eq!(audit.offline.load(Ordering::SeqCst), 1);
        assert_eq!(audit.activated.load(Ordering::SeqCst), 0);
        assert_eq!(audit.processed.load(Ordering::SeqCst), 0);
        assert_eq!(audit.loads.load(Ordering::SeqCst), 0);
        NEXT.with(|slot| assert!(slot.borrow().is_none()));
        (result, audit)
    }
    #[test]
    fn inactive_nondefault_capture_saves_actual_changed_value_and_distinguishes_empty_state() {
        for mode in [Mode::Success, Mode::EmptyState] {
            let (result, audit) = cycle(mode, 0.5, 0.25);
            let captured = result.expect("real inactive set/read/save");
            assert_eq!(captured.parameter_count, 1);
            assert_eq!(captured.before.id, ID);
            assert_eq!(captured.before.current_value.to_bits(), 0.5_f64.to_bits());
            assert_eq!(captured.after_value.to_bits(), 0.25_f64.to_bits());
            if mode == Mode::EmptyState {
                assert!(captured.bytes.is_empty());
            } else {
                assert_eq!(captured.bytes, 0.25_f64.to_le_bytes());
            }
            assert_eq!(audit.flushes.load(Ordering::SeqCst), 1);
            assert_eq!(audit.saves.load(Ordering::SeqCst), 1);
            assert_eq!(audit.queries.load(Ordering::SeqCst), 2);
        }
    }
    #[test]
    fn inactive_nondefault_capture_refuses_unapplied_missing_failed_and_oversized_state_after_drop()
    {
        for (mode, detail, saves) in [
            (Mode::IgnoreFlush, "target readback differs", 0),
            (Mode::MissingState, "has no State extension", 0),
            (Mode::SaveError, "state capture failed", 1),
            (Mode::Overflow, "exceeds the capture limit", 1),
        ] {
            let (result, audit) = cycle(mode, 0.5, 0.25);
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("partial/absent state cannot qualify"),
            };
            assert!(error.to_string().contains(detail), "{mode:?}: {error:?}");
            assert_eq!(audit.flushes.load(Ordering::SeqCst), 1);
            assert_eq!(audit.saves.load(Ordering::SeqCst), saves);
        }
    }
    #[test]
    fn inactive_nondefault_capture_rejects_bad_transition_before_native_flush_or_save() {
        for (mode, expected, target) in [
            (Mode::ReadOnly, 0.5, 0.25),
            (Mode::Success, 0.75, 0.25),
            (Mode::Success, 0.5, 0.5),
            (Mode::Success, 0.5, 2.0),
            (Mode::Success, 0.5, f64::NAN),
        ] {
            let (result, audit) = cycle(mode, expected, target);
            assert!(result.is_err());
            assert_eq!(audit.flushes.load(Ordering::SeqCst), 0);
            assert_eq!(audit.saves.load(Ordering::SeqCst), 0);
            assert_eq!(audit.queries.load(Ordering::SeqCst), 1);
        }
    }
}
