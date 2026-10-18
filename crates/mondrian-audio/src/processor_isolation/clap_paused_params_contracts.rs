//! Owned paused CLAP GUI/parameter producer exercising existing worker APIs.
//! GUI-originated parameters use an actual paused, activated CLAP instance.

use super::*;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, HostGui, PluginGui, PluginGuiImpl, Window,
};
use clack_extensions::timer::{HostTimer, PluginTimer, PluginTimerImpl, TimerId};
use clack_plugin::prelude::{
    Audio, ChannelPair, DefaultPluginFactory, Events, HostAudioProcessorHandle,
    HostMainThreadHandle, HostSharedHandle, Plugin, PluginDescriptor, PluginError,
    PluginExtensions, PluginMainThread, PluginShared, Process, ProcessStatus, SinglePluginEntry,
};
use mondrian_core::{AudioChannelLayout, AudioProcessorBindingId, NativeAudioEditorTitle};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering as AuditOrdering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetParent, IsWindow, SetWindowPos, ShowWindow, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_HIDE, SW_SHOW, WS_CHILD,
};

static WITHOUT_STATE: AtomicBool = AtomicBool::new(false);
static REQUEST_HOST_CONTROLS: AtomicUsize = AtomicUsize::new(0);
static HOST_CONTROLS_EMITTED: AtomicUsize = AtomicUsize::new(0);
static SAVE_FAILS: AtomicBool = AtomicBool::new(false);
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
static WRONG_THREAD: AtomicUsize = AtomicUsize::new(0);
static CREATED: AtomicUsize = AtomicUsize::new(0);
static DESTROYED: AtomicUsize = AtomicUsize::new(0);
static HIDDEN: AtomicUsize = AtomicUsize::new(0);
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static TIMERS: AtomicUsize = AtomicUsize::new(0);
static PROCESSES: AtomicUsize = AtomicUsize::new(0);
static CHILD: AtomicUsize = AtomicUsize::new(0);
static PARENT: AtomicUsize = AtomicUsize::new(0);
static SELF_DESTROY_ON_TIMER: AtomicBool = AtomicBool::new(false);
static SELF_DESTROYED: AtomicBool = AtomicBool::new(false);

struct PausedParamsPlugin;
struct PausedParamsShared<'a> {
    host: HostSharedHandle<'a>,
    desired: std::sync::atomic::AtomicU64,
    applied: std::sync::atomic::AtomicU64,
    pending: AtomicBool,
}
struct PausedParamsMain<'a> {
    shared: &'a PausedParamsShared<'a>,
    host: HostMainThreadHandle<'a>,
    timer: Cell<Option<TimerId>>,
    child: Cell<HWND>,
    created: Cell<bool>,
    size: Cell<GuiSize>,
}
struct PausedParamsProcessor<'a>(&'a PausedParamsShared<'a>);

impl<'a> PluginShared<'a> for PausedParamsShared<'a> {}
impl PausedParamsShared<'_> {
    fn check(&self, main: Option<bool>, audio: bool) {
        let Some(check) = self.host.get_extension::<HostThreadCheck>() else {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
            return;
        };
        if main.is_some_and(|main| check.is_main_thread(&self.host) != Some(main))
            || check.is_audio_thread(&self.host) != Some(audio)
        {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
        }
    }
}
impl Plugin for PausedParamsPlugin {
    type Shared<'a> = PausedParamsShared<'a>;
    type MainThread<'a> = PausedParamsMain<'a>;
    type AudioProcessor<'a> = PausedParamsProcessor<'a>;
    fn declare_extensions(
        builder: &mut PluginExtensions<Self>,
        _shared: Option<&Self::Shared<'_>>,
    ) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginGui>()
            .register::<PluginTimer>()
            .register::<PluginParams>();
        if !WITHOUT_STATE.load(AuditOrdering::SeqCst) {
            builder.register::<PluginState>();
        }
    }
}
impl DefaultPluginFactory for PausedParamsPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "org.mondrian.test.paused-params",
            "Owned paused parameter GUI",
        )
    }
    fn new_shared(host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        let shared = PausedParamsShared {
            host,
            desired: std::sync::atomic::AtomicU64::new(0.5_f64.to_bits()),
            applied: std::sync::atomic::AtomicU64::new(0.5_f64.to_bits()),
            pending: AtomicBool::new(false),
        };
        shared.check(Some(true), false);
        host.request_callback();
        Ok(shared)
    }
    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a Self::Shared<'a>,
    ) -> Result<Self::MainThread<'a>, PluginError> {
        Ok(PausedParamsMain {
            shared,
            host,
            timer: Cell::new(None),
            child: Cell::new(std::ptr::null_mut()),
            created: Cell::new(false),
            size: Cell::new(GuiSize { width: 320, height: 200 }),
        })
    }
}
impl<'a> PluginMainThread<'a, PausedParamsShared<'a>> for PausedParamsMain<'a> {
    fn on_main_thread(&self) {
        self.shared.check(Some(true), false);
        if self.timer.get().is_none()
            && let Some(extension) = self.host.get_extension::<HostTimer>()
            && let Ok(id) = extension.register_timer(&self.host, 10)
        {
            self.timer.set(Some(id));
        }
        let count = CALLBACKS.fetch_add(1, AuditOrdering::SeqCst) + 1;
        if count < 200 {
            self.shared.host.request_callback();
        }
    }
}
impl PluginAudioPortsImpl for PausedParamsMain<'_> {
    fn count(&self, _input: bool) -> u32 {
        1
    }
    fn get(&self, index: u32, _input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"main",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}
impl PluginTimerImpl for PausedParamsMain<'_> {
    fn on_timer(&self, id: TimerId) {
        self.shared.check(Some(true), false);
        if self.timer.get() != Some(id) {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
        }
        TIMERS.fetch_add(1, AuditOrdering::SeqCst);
        let controls = REQUEST_HOST_CONTROLS.swap(0, AuditOrdering::SeqCst);
        if controls != 0 {
            let params = self
                .host
                .get_extension::<HostParams>()
                .expect("actual advertised main HostParams");
            if controls & 1 != 0 {
                params.clear(
                    &self.host,
                    ClapId::new(17),
                    clack_extensions::params::ParamClearFlags::AUTOMATIONS,
                );
                HOST_CONTROLS_EMITTED.fetch_add(1, AuditOrdering::SeqCst);
            }
            if controls & 2 != 0 {
                // The plugin changes its real parameter immediately. This is
                // not an output event, automation key, host mirror or reset.
                self.shared.desired.store(0.625_f64.to_bits(), AuditOrdering::Release);
                self.shared.applied.store(0.625_f64.to_bits(), AuditOrdering::Release);
                params.rescan(
                    &self.host,
                    clack_extensions::params::ParamRescanFlags::VALUES,
                );
                HOST_CONTROLS_EMITTED.fetch_add(1, AuditOrdering::SeqCst);
            }
        }
        if !self.child.get().is_null() && REQUEST_GUI_VALUE.swap(false, AuditOrdering::SeqCst) {
            self.shared.desired.store(0.75_f64.to_bits(), AuditOrdering::Release);
            self.shared.pending.store(true, AuditOrdering::Release);
            if let Some(params) = self.shared.host.get_extension::<HostParams>() {
                HOST_PARAMS_PRESENT.store(true, AuditOrdering::SeqCst);
                params.request_flush(&self.shared.host);
            }
        }
        if SELF_DESTROY_ON_TIMER.swap(false, AuditOrdering::SeqCst) && !self.child.get().is_null() {
            // A real plugin-side native GUI destruction, then the standard
            // thread-safe closed(true) observation. Destroy acknowledgement is
            // still required; hide must never target this destroyed GUI.
            let child = self.child.replace(std::ptr::null_mut());
            let destroyed = unsafe { DestroyWindow(child) };
            if destroyed == 0 {
                WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
            }
            if let Some(gui) = self.shared.host.get_extension::<HostGui>() {
                gui.closed(&self.shared.host, true);
            } else {
                WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
            }
            SELF_DESTROYED.store(true, AuditOrdering::SeqCst);
        }
    }
}
impl PluginGuiImpl for PausedParamsMain<'_> {
    fn is_api_supported(&self, config: GuiConfiguration) -> bool {
        self.shared.check(Some(true), false);
        config.api_type == GuiApiType::WIN32 && !config.is_floating
    }
    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        Some(GuiConfiguration { api_type: GuiApiType::WIN32, is_floating: false })
    }
    fn create(&self, config: GuiConfiguration) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        if !self.is_api_supported(config) || self.created.replace(true) {
            return Err(PluginError::Message("unexpected GUI create"));
        }
        CREATED.fetch_add(1, AuditOrdering::SeqCst);
        Ok(())
    }
    fn destroy(&self) {
        self.shared.check(Some(true), false);
        if !self.created.replace(false) {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
        }
        let child = self.child.replace(std::ptr::null_mut());
        if !child.is_null() && unsafe { DestroyWindow(child) } == 0 {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
        }
        if unsafe { IsWindow(PARENT.load(AuditOrdering::SeqCst) as HWND) } == 0 {
            // Independent borrowed-parent lifetime oracle: it must still exist
            // until the plugin's actual destroy acknowledgement returns.
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
        }
        DESTROYED.fetch_add(1, AuditOrdering::SeqCst);
    }
    fn set_scale(&self, scale: f64) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        if !scale.is_finite() || scale <= 0.0 {
            return Err(PluginError::Message("invalid scale"));
        }
        Ok(())
    }
    fn get_size(&self) -> Option<GuiSize> {
        self.shared.check(Some(true), false);
        Some(self.size.get())
    }
    fn can_resize(&self) -> bool {
        true
    }
    fn adjust_size(&self, size: GuiSize) -> Option<GuiSize> {
        Some(size)
    }
    fn set_size(&self, size: GuiSize) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        self.size.set(size);
        if !self.child.get().is_null()
            && unsafe {
                SetWindowPos(
                    self.child.get(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    size.width as i32,
                    size.height as i32,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            } == 0
        {
            return Err(PluginError::Message("native child resize failed"));
        }
        Ok(())
    }
    fn set_parent(&self, parent: Window) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        let parent = parent.as_win32_hwnd().ok_or(PluginError::Message("wrong parent API"))?;
        if unsafe { IsWindow(parent) } == 0 {
            return Err(PluginError::Message("parent is not an actual HWND"));
        }
        let class = [
            b'S' as u16,
            b'T' as u16,
            b'A' as u16,
            b'T' as u16,
            b'I' as u16,
            b'C' as u16,
            0,
        ];
        let title = [0_u16];
        let size = self.size.get();
        // Use the OS predefined STATIC class, with no custom borrowed WndProc
        // or DLL registration that could survive native fixture teardown.
        let child = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_CHILD,
                0,
                0,
                size.width as i32,
                size.height as i32,
                parent,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if child.is_null() {
            return Err(PluginError::Message("native child creation failed"));
        }
        if unsafe { GetParent(child) } != parent {
            unsafe {
                DestroyWindow(child);
            }
            return Err(PluginError::Message("native child attachment failed"));
        }
        self.child.set(child);
        CHILD.store(child as usize, AuditOrdering::SeqCst);
        PARENT.store(parent as usize, AuditOrdering::SeqCst);
        Ok(())
    }
    fn set_transient(&self, _parent: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("embedded fixture"))
    }
    fn show(&self) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        if self.child.get().is_null() {
            return Err(PluginError::Message("GUI child missing"));
        }
        unsafe {
            ShowWindow(self.child.get(), SW_SHOW);
        }
        Ok(())
    }
    fn hide(&self) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        if self.child.get().is_null() {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
            return Err(PluginError::Message("hide after native GUI destruction"));
        }
        unsafe {
            ShowWindow(self.child.get(), SW_HIDE);
        }
        HIDDEN.fetch_add(1, AuditOrdering::SeqCst);
        Ok(())
    }
}
impl Drop for PausedParamsMain<'_> {
    fn drop(&mut self) {
        self.shared.check(Some(true), false);
        if let Some(id) = self.timer.take()
            && let Some(timer) = self.host.get_extension::<HostTimer>()
            && timer.unregister_timer(&self.host, id).is_err()
        {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
        }
        if self.created.get() || !self.child.get().is_null() {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
        }
    }
}
impl<'a>
    clack_plugin::plugin::PluginAudioProcessor<'a, PausedParamsShared<'a>, PausedParamsMain<'a>>
    for PausedParamsProcessor<'a>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &PausedParamsMain<'a>,
        shared: &'a PausedParamsShared<'a>,
        _config: clack_plugin::prelude::PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        shared.check(Some(true), false);
        Ok(Self(shared))
    }
    fn reset(&mut self) {
        self.0.check(None, true);
    }
    fn start_processing(&mut self) -> Result<(), PluginError> {
        self.0.check(None, true);
        Ok(())
    }
    fn stop_processing(&mut self) {
        self.0.check(None, true);
    }
    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        self.0.check(Some(false), true);
        PROCESSES.fetch_add(1, AuditOrdering::SeqCst);
        let mut pair = audio.port_pair(0).ok_or(PluginError::Message("main port missing"))?;
        let mut channels =
            pair.channels()?.into_f32().ok_or(PluginError::Message("f32 expected"))?;
        for channel in channels.iter_mut() {
            match channel {
                ChannelPair::InputOutput(input, output) => {
                    for (&sample, output) in input.iter().zip(output.iter_mut()) {
                        *output = sample
                            * f64::from_bits(self.0.applied.load(AuditOrdering::Acquire)) as f32;
                    }
                }
                _ => return Err(PluginError::Message("separate main buffers expected")),
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

fn reset_audit() {
    WITHOUT_STATE.store(false, AuditOrdering::SeqCst);
    SAVE_FAILS.store(false, AuditOrdering::SeqCst);
    REQUEST_HOST_CONTROLS.store(0, AuditOrdering::SeqCst);
    HOST_CONTROLS_EMITTED.store(0, AuditOrdering::SeqCst);
    for value in [
        &WRONG_THREAD,
        &CREATED,
        &DESTROYED,
        &HIDDEN,
        &CALLBACKS,
        &TIMERS,
        &PROCESSES,
        &CHILD,
        &PARENT,
    ] {
        value.store(0, AuditOrdering::SeqCst);
    }
    SELF_DESTROY_ON_TIMER.store(false, AuditOrdering::SeqCst);
    SELF_DESTROYED.store(false, AuditOrdering::SeqCst);
    REQUEST_GUI_VALUE.store(false, AuditOrdering::SeqCst);
    HOST_PARAMS_PRESENT.store(false, AuditOrdering::SeqCst);
    ACTIVE_FLUSHES.store(0, AuditOrdering::SeqCst);
    INACTIVE_FLUSHES.store(0, AuditOrdering::SeqCst);
    ACCEPTED_EVENTS.store(0, AuditOrdering::SeqCst);
}
fn render_contract() -> crate::AudioRenderContract {
    crate::AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 16,
        processing_mode: crate::AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 64 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}
fn owned_worker() -> Box<dyn IsolatedAudioProcessorWorker> {
    let entry =
        PluginEntry::load_from_clack::<SinglePluginEntry<PausedParamsPlugin>>(c"/test/native-gui")
            .expect("owned actual CLAP ABI entry");
    let contract = crate::AudioProcessorExecutionContract::new(
        0,
        crate::AudioProcessorTail::None,
        true,
        true,
        true,
        0,
    )
    .expect("contract");
    let profile = ClapAudioProcessorWorkerFactory
        .native_worker_lane_resources()
        .expect("actual factory profile");
    let descriptor = super::super::ClapParameterDescriptor {
        id: 17,
        name: "Gain".to_owned(),
        min_value: 0.0,
        max_value: 1.0,
        default_value: 0.5,
        current_value: 0.5,
        automatable: true,
        stepped: false,
        enumeration: false,
        read_only: false,
        hidden: false,
    };
    prepare_loaded(
        IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: profile,
            payload: Vec::new(),
            plugin_execution_contract: contract,
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: vec![descriptor.parameter_id().expect("native stable parameter")],
            render_contract: render_contract(),
        },
        entry,
        c"org.mondrian.test.paused-params",
        None,
        &[descriptor],
        &[17],
    )
    .expect("prepare actual owned plugin")
}
fn bind_and_open(
    worker: &mut dyn IsolatedAudioProcessorWorker,
) -> crate::NativeAudioObjectBindRequest {
    let actual = worker
        .native_audio_object_observation()
        .expect("actual object query")
        .expect("CLAP actual loaded object capability");
    let binding = crate::NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: actual,
    };
    assert!(matches!(
        worker.bind_native_audio_object(binding).expect("same-owner binding"),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    let request = crate::NativeAudioEditorOpenRequest::new(
        binding,
        NativeAudioEditorTitle::new("Owned CLAP GUI".to_owned()).expect("title"),
    );
    assert_eq!(
        worker.open_native_audio_editor(request).expect("actual open"),
        crate::NativeAudioEditorOutcome::Opened { binding, width: 320, height: 200 }
    );
    binding
}

#[test]
fn clap_paused_gui_requests_actual_audio_flush_without_process_old_api() {
    let _serial = SERIAL.lock().expect("actual Windows fixture serialization");
    reset_audit();
    let mut worker = owned_worker();
    worker.enter_state(0).expect("initial actual Enter");
    let binding = bind_and_open(worker.as_mut());
    let parameter = super::super::ClapParameterDescriptor {
        id: 17,
        name: "Gain".to_owned(),
        min_value: 0.0,
        max_value: 1.0,
        default_value: 0.5,
        current_value: 0.5,
        automatable: true,
        stepped: false,
        enumeration: false,
        read_only: false,
        hidden: false,
    }
    .parameter_id()
    .expect("actual stable parameter");
    let layout = super::super::protocol::SharedBlockLayout::new(16, 2, 0, 1)
        .expect("actual one-lane layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut aligned);
    for bytes in shared[..8 * 2 * 4].chunks_exact_mut(4) {
        bytes.copy_from_slice(&0.5_f32.to_le_bytes());
    }
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    let parameter_ids = [parameter];
    let context = super::super::worker::WorkerProcessContext {
        musical_time_reference: None,
        auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
        layout,
        auxiliary_inputs: &auxiliary,
        note_input_contract: &crate::AudioProcessorNoteInputContract::default(),
        parameter_ids: &parameter_ids,
        render_contract: render_contract(),
    };
    let profile = worker.native_worker_lane_resources().expect("actual Windows split profile");
    REQUEST_GUI_VALUE.store(true, AuditOrdering::SeqCst);
    super::super::native_worker_processing::with_processing_scope(
        worker.split_native_processing().expect("same-object borrowed roles"),
        profile,
        shared,
        context,
        |main, dsp| {
            let until = Instant::now() + Duration::from_secs(2);
            while ACTIVE_FLUSHES.load(AuditOrdering::SeqCst) == 0 && Instant::now() < until {
                main.service_native_main_thread()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                HOST_PARAMS_PRESENT.load(AuditOrdering::SeqCst),
                "actual GUI must find a complete HostParams consumer"
            );
            assert_eq!(
                ACTIVE_FLUSHES.load(AuditOrdering::SeqCst),
                1,
                "paused activated parameters exchange on the same DSP, with no PCM job"
            );
            assert_eq!(
                ACCEPTED_EVENTS.load(AuditOrdering::SeqCst),
                3,
                "actual begin/plain value/end callbacks are owned, not discarded"
            );
            assert_eq!(
                PROCESSES.load(AuditOrdering::SeqCst),
                0,
                "paused GUI may not invent Process/time/PCM"
            );
            // After the paused-exchange assertions, independently observe the
            // real private DSP gain. Desired GUI state alone cannot satisfy it.
            dsp.submit(super::super::protocol::WorkerCommand {
                kind: super::super::protocol::CommandKind::Process,
                start_sample: 0,
                frames: 8,
                event_count: 0,
                note_count: 0,
            })?;
            dsp.complete(main)?;
            Ok(())
        },
    )
    .expect("actual idle DSP join/main finish");
    for bytes in shared[..8 * 2 * 4].chunks_exact(4) {
        assert_eq!(
            f32::from_le_bytes(bytes.try_into().expect("actual f32 output")),
            0.375,
            "actual flushed private DSP, not controller mirror"
        );
    }
    assert_eq!(
        PROCESSES.load(AuditOrdering::SeqCst),
        1,
        "only the explicitly requested oracle PCM call executes"
    );
    assert_eq!(
        WRONG_THREAD.load(AuditOrdering::SeqCst),
        0,
        "native flush really owns audio and timer really owns main"
    );
    worker.close_native_audio_editor(binding).expect("joined real GUI destroy");
    drop(worker);
}

use clack_extensions::params::{
    HostParams, ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter,
    PluginAudioProcessorParams, PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::events::{Event, EventFlags};
use clack_plugin::prelude::{InputEvents, OutputEvents, Pckn};
use clack_plugin::stream::{InputStream, OutputStream};
use std::io::{Read as _, Write as _};
static REQUEST_GUI_VALUE: AtomicBool = AtomicBool::new(false);
static HOST_PARAMS_PRESENT: AtomicBool = AtomicBool::new(false);
static ACTIVE_FLUSHES: AtomicUsize = AtomicUsize::new(0);
static INACTIVE_FLUSHES: AtomicUsize = AtomicUsize::new(0);
static ACCEPTED_EVENTS: AtomicUsize = AtomicUsize::new(0);
static PARAMETER_QUERIES: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];
fn parameter_query_counts() -> [usize; 4] {
    std::array::from_fn(|index| PARAMETER_QUERIES[index].load(AuditOrdering::SeqCst))
}

impl PausedParamsShared<'_> {
    fn exchange(&self, output: &mut OutputEvents) {
        if !self.pending.swap(false, AuditOrdering::AcqRel) {
            return;
        }
        let value = f64::from_bits(self.desired.load(AuditOrdering::Acquire));
        // The real DSP private parameter changes only upon actual exchange.
        // A host-side controller mirror or GUI value alone is insufficient.
        self.applied.store(value.to_bits(), AuditOrdering::Release);
        let events = [
            output.try_push(ParamGestureBeginEvent::new(0, ClapId::new(17))),
            output.try_push(
                ParamValueEvent::new(0, ClapId::new(17), Pckn::match_all(), value)
                    .with_flags(EventFlags::IS_LIVE),
            ),
            output.try_push(ParamGestureEndEvent::new(0, ClapId::new(17))),
        ];
        for event in events {
            if event.is_ok() {
                ACCEPTED_EVENTS.fetch_add(1, AuditOrdering::SeqCst);
            }
        }
    }
}
impl PluginMainThreadParams for PausedParamsMain<'_> {
    fn count(&self) -> u32 {
        PARAMETER_QUERIES[0].fetch_add(1, AuditOrdering::SeqCst);
        1
    }
    fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
        PARAMETER_QUERIES[1].fetch_add(1, AuditOrdering::SeqCst);
        if index == 0 {
            writer.set(&ParamInfo {
                id: ClapId::new(17),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Default::default(),
                name: b"Gain",
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.5,
            });
        }
    }
    fn get_value(&self, id: ClapId) -> Option<f64> {
        PARAMETER_QUERIES[2].fetch_add(1, AuditOrdering::SeqCst);
        (id == ClapId::new(17))
            .then(|| f64::from_bits(self.shared.desired.load(AuditOrdering::Acquire)))
    }
    fn value_to_text(
        &self,
        _id: ClapId,
        _value: f64,
        _writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        PARAMETER_QUERIES[3].fetch_add(1, AuditOrdering::SeqCst);
        Err(std::fmt::Error)
    }
    fn text_to_value(&self, _id: ClapId, _text: &CStr) -> Option<f64> {
        None
    }
    fn flush(&self, _input: &InputEvents, output: &mut OutputEvents) {
        self.shared.check(Some(true), false);
        INACTIVE_FLUSHES.fetch_add(1, AuditOrdering::SeqCst);
        self.shared.exchange(output);
    }
}
impl PluginAudioProcessorParams for PausedParamsProcessor<'_> {
    fn flush(&mut self, _input: &InputEvents, output: &mut OutputEvents) {
        self.0.check(Some(false), true);
        self.0.exchange(output);
        // The main-thread oracle waits for completed event delivery. Publishing
        // at entry would let it inspect ACCEPTED_EVENTS before exchange runs.
        ACTIVE_FLUSHES.fetch_add(1, AuditOrdering::SeqCst);
    }
}
impl PluginStateImpl for PausedParamsMain<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        if SAVE_FAILS.load(AuditOrdering::SeqCst) {
            return Err(PluginError::Message("owned State.save refusal"));
        }
        output.write_all(&self.shared.applied.load(AuditOrdering::Acquire).to_le_bytes())?;
        Ok(())
    }
    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        let mut bytes = [0; 8];
        input.read_exact(&mut bytes)?;
        let bits = u64::from_le_bytes(bytes);
        let value = f64::from_bits(bits);
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(PluginError::Message("invalid owned gain state"));
        }
        self.shared.applied.store(bits, AuditOrdering::Release);
        self.shared.desired.store(bits, AuditOrdering::Release);
        Ok(())
    }
}

fn exchange_another_paused_gui_value(worker: &mut dyn IsolatedAudioProcessorWorker) {
    let parameter = super::super::ClapParameterDescriptor {
        id: 17,
        name: "Gain".to_owned(),
        min_value: 0.0,
        max_value: 1.0,
        default_value: 0.5,
        current_value: 0.5,
        automatable: true,
        stepped: false,
        enumeration: false,
        read_only: false,
        hidden: false,
    }
    .parameter_id()
    .expect("actual stable parameter");
    let layout = super::super::protocol::SharedBlockLayout::new(16, 2, 0, 1).expect("layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared = bytemuck::cast_slice_mut(&mut aligned);
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    let ids = [parameter];
    let context = super::super::worker::WorkerProcessContext {
        musical_time_reference: None,
        auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
        layout,
        auxiliary_inputs: &auxiliary,
        note_input_contract: &crate::AudioProcessorNoteInputContract::default(),
        parameter_ids: &ids,
        render_contract: render_contract(),
    };
    let profile = worker.native_worker_lane_resources().expect("same exact lane profile");
    let before = ACTIVE_FLUSHES.load(AuditOrdering::SeqCst);
    REQUEST_GUI_VALUE.store(true, AuditOrdering::SeqCst);
    super::super::native_worker_processing::with_processing_scope(
        worker.split_native_processing().expect("same native object disjoint roles"),
        profile,
        shared,
        context,
        |main, _| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while ACTIVE_FLUSHES.load(AuditOrdering::SeqCst) == before && Instant::now() < deadline
            {
                main.service_native_main_thread()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(ACTIVE_FLUSHES.load(AuditOrdering::SeqCst), before + 1);
            Ok(())
        },
    )
    .expect("real idle flush, joined before main capture");
}

#[test]
fn clap_paused_native_prefix_retry_keeps_new_suffix_and_exact_ack_object() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery,
        NativeAudioObservationConsumption as Consumption,
    };
    let _serial = SERIAL.lock().expect("owned fixture serialization");
    reset_audit();
    let mut worker = owned_worker();
    worker.enter_state(23).expect("actual state entry");
    let binding = bind_and_open(worker.as_mut());
    exchange_another_paused_gui_value(worker.as_mut());
    assert_eq!(
        PROCESSES.load(AuditOrdering::SeqCst),
        0,
        "no dummy Process as flush"
    );
    let first = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("one full observation");
    let Some(crate::NativeAudioAuthorObservation { delivery: Delivery::Delivered(first), .. }) =
        first.author
    else {
        panic!("actual output must reach the retained author prefix");
    };
    assert_eq!(first.revision, 3);
    assert_eq!(first.events.len(), 3);
    assert_eq!(
        first.configuration.as_ref().expect("real state save").bytes(),
        &0.75_f64.to_bits().to_le_bytes()
    );
    let repeated = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("retry");
    assert!(matches!(repeated.author.expect("same source").delivery,
        Delivery::AwaitingAcknowledgement { binding: same, revision: 3, request_cohort: None } if same == binding));
    exchange_another_paused_gui_value(worker.as_mut());
    assert_eq!(ACCEPTED_EVENTS.load(AuditOrdering::SeqCst), 6);
    let installed =
        crate::NativeAudioObjectBindRequest { binding: AudioProcessorBindingId::new(), ..binding };
    assert!(matches!(
        worker
            .bind_native_audio_object(installed)
            .expect("same actual object Plan bind"),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    let ack = crate::NativeAudioAuthorAcknowledgement {
        source_binding: binding,
        installed_binding: installed,
        revision: 3,
    };
    let accepted = worker
        .poll_native_observations(Consumption::AcknowledgeAuthor(ack))
        .expect("prefix Ack");
    let author = accepted.author.expect("source retained after Ack");
    assert_eq!(
        author.source.revision(),
        6,
        "nonwrapping accepted sequence never resets"
    );
    assert_eq!(
        author.source.pending_events(),
        3,
        "only the selected prefix retires"
    );
    assert!(
        matches!(author.delivery, Delivery::Acknowledged { acknowledgement: actual, request_cohort: None } if actual == ack)
    );
    let suffix = worker
        .poll_native_observations(Consumption::DrainAuthor(installed))
        .expect("next independent prefix");
    let Some(crate::NativeAudioAuthorObservation { delivery: Delivery::Delivered(suffix), .. }) =
        suffix.author
    else {
        panic!("new actual callbacks may not be lost during prefix Ack retry");
    };
    assert_eq!(suffix.revision, 6);
    assert_eq!(suffix.events.len(), 3);
    let ack_suffix = crate::NativeAudioAuthorAcknowledgement {
        source_binding: installed,
        installed_binding: installed,
        revision: 6,
    };
    let closed = worker
        .poll_native_observations(Consumption::AcknowledgeAuthor(ack_suffix))
        .expect("suffix Ack");
    assert!(closed.author.expect("actual source").source.closed_empty());
    assert_eq!(
        worker.close_native_audio_editor(installed).expect("same attachment close"),
        crate::NativeAudioEditorOutcome::Closed { binding: installed }
    );
    drop(worker);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
}

#[test]
fn clap_paused_empty_author_poll_queries_once_and_rescans_new_source() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery,
        NativeAudioObservationConsumption as Consumption,
    };
    let _serial = SERIAL.lock().expect("actual native fixture serialization");
    reset_audit();
    let mut worker = owned_worker();
    worker.enter_state(23).expect("actual entry");
    let binding = bind_and_open(worker.as_mut());
    let before = parameter_query_counts();
    for _ in 0..32 {
        worker
            .poll_native_observations(Consumption::Preserve)
            .expect("fixed status only");
        worker.poll_native_observations(Consumption::Drain).expect("logs only");
    }
    assert_eq!(
        parameter_query_counts(),
        before,
        "PCM Preserve and unbound Drain may not query any parameter"
    );
    let first = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("first actual current query");
    assert_eq!(first.parameters.len(), 1);
    assert_eq!(first.parameters[0].value_bits, Some(0.5_f64.to_bits()));
    let once = std::array::from_fn(|index| before[index] + 1);
    assert_eq!(
        parameter_query_counts(),
        once,
        "one count/info/value/text query per actual parameter"
    );
    for _ in 0..32 {
        let unchanged = worker
            .poll_native_observations(Consumption::DrainAuthor(binding))
            .expect("same source revision");
        assert!(
            unchanged.parameters.is_empty(),
            "no new current-value evidence was queried"
        );
    }
    assert_eq!(
        parameter_query_counts(),
        once,
        "empty repeated author ticks do not call plugin queries"
    );
    exchange_another_paused_gui_value(worker.as_mut());
    let changed = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("actual new source");
    assert!(changed
        .parameters
        .iter()
        .any(|fact| fact.value_bits == Some(0.75_f64.to_bits())));
    let Some(crate::NativeAudioAuthorObservation {
        delivery: Delivery::Delivered(delivery), ..
    }) = changed.author
    else {
        panic!("real paused flush must retain an author prefix");
    };
    let twice = std::array::from_fn(|index| once[index] + 1);
    assert_eq!(
        parameter_query_counts(),
        twice,
        "new accepted source causes a real fresh query"
    );
    let ack = crate::NativeAudioAuthorAcknowledgement {
        source_binding: binding,
        installed_binding: binding,
        revision: delivery.revision,
    };
    worker
        .poll_native_observations(Consumption::AcknowledgeAuthor(ack))
        .expect("same native exact prefix Ack");
    let empty = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("already observed source revision");
    assert!(empty.parameters.is_empty());
    assert_eq!(
        parameter_query_counts(),
        twice,
        "Ack is not another current-value query request"
    );
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    worker.close_native_audio_editor(binding).expect("actual owner cleanup");
}

#[test]
fn clap_paused_changed_binding_refreshes_presentation_without_new_source() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioObjectBindOutcome as BindOutcome,
        NativeAudioObservationConsumption as Consumption,
    };
    let _serial = SERIAL.lock().expect("actual native fixture serialization");
    reset_audit();
    let mut worker = owned_worker();
    worker.enter_state(23).expect("actual entry");
    let binding = bind_and_open(worker.as_mut());
    let object = binding.expected_object;
    exchange_another_paused_gui_value(worker.as_mut());

    let before = parameter_query_counts();
    let original = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("actual callback prefix and current values");
    assert_eq!(original.object, Some(object));
    assert!(original
        .parameters
        .iter()
        .any(|fact| fact.value_bits == Some(0.75_f64.to_bits())));
    let original_source = original.author.as_ref().expect("actual Source").source;
    assert_eq!(original_source.revision(), 3);
    let Some(crate::NativeAudioAuthorObservation {
        delivery: Delivery::Delivered(delivery), ..
    }) = original.author
    else {
        panic!("actual paused flush must retain an author prefix");
    };
    assert_eq!(delivery.binding, binding);
    assert_eq!(delivery.revision, original_source.revision());
    assert_eq!(
        delivery.configuration.as_ref().expect("actual coherent state").bytes(),
        &0.75_f64.to_bits().to_le_bytes()
    );
    let once = std::array::from_fn(|index| before[index] + 1);
    assert_eq!(parameter_query_counts(), once);

    let installed =
        crate::NativeAudioObjectBindRequest { binding: AudioProcessorBindingId::new(), ..binding };
    assert_ne!(installed.binding, binding.binding);
    let BindOutcome::Bound(receipt) = worker
        .bind_native_audio_object(installed)
        .expect("same actual object new Plan binding")
    else {
        panic!("same actual object must accept its new binding");
    };
    assert_eq!(receipt.binding(), installed.binding);
    assert_eq!(receipt.object(), object);
    assert_eq!(
        parameter_query_counts(),
        once,
        "Bind does not query native parameters"
    );
    let preserved = worker
        .poll_native_observations(Consumption::Preserve)
        .expect("held prefix status only");
    assert_eq!(preserved.object, Some(object));
    assert!(preserved.parameters.is_empty());
    let preserved = preserved.author.expect("original held Source");
    assert_eq!(preserved.source, original_source);
    assert!(matches!(
        preserved.delivery,
        Delivery::AwaitingAcknowledgement {
            binding: actual, revision: 3, request_cohort: None,
        } if actual == binding
    ));
    assert_eq!(
        parameter_query_counts(),
        once,
        "Preserve does not query after Bind"
    );

    let acknowledgement = crate::NativeAudioAuthorAcknowledgement {
        source_binding: binding,
        installed_binding: installed,
        revision: delivery.revision,
    };
    let accepted = worker
        .poll_native_observations(Consumption::AcknowledgeAuthor(acknowledgement))
        .expect("exact original prefix Ack at the installed binding");
    assert_eq!(accepted.object, Some(object));
    assert!(accepted.parameters.is_empty());
    let accepted = accepted.author.expect("actual retired prefix");
    assert!(matches!(
        accepted.delivery,
        Delivery::Acknowledged { acknowledgement: actual, request_cohort: None }
            if actual == acknowledgement
    ));
    assert!(accepted.source.closed_empty());
    assert_eq!(accepted.source.revision(), original_source.revision());
    assert_eq!(
        parameter_query_counts(),
        once,
        "Ack does not query native parameters"
    );

    let refreshed = worker
        .poll_native_observations(Consumption::DrainAuthor(installed))
        .expect("current facts for the new binding on the same actual object");
    assert_eq!(refreshed.object, Some(object));
    assert_eq!(refreshed.parameter_revision, original_source.revision());
    assert_eq!(
        refreshed.parameters.len(),
        1,
        "changed CLAP binding must refresh presentation at the unchanged Source revision"
    );
    let fact = &refreshed.parameters[0];
    assert_eq!(
        fact.parameter,
        mondrian_core::ParameterId::new("clap.param.17").unwrap()
    );
    assert_eq!(fact.value_bits, Some(0.75_f64.to_bits()));
    assert_eq!(fact.revision, original_source.revision());
    assert_eq!(
        fact.origin,
        mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { rescan_flags: 0 }
    );
    let refreshed = refreshed.author.expect("same closed Source after actual query");
    assert_eq!(refreshed.source, accepted.source);
    assert!(matches!(refreshed.delivery, Delivery::Preserved));
    let twice = std::array::from_fn(|index| once[index] + 1);
    assert_eq!(parameter_query_counts(), twice);

    assert!(matches!(
        worker.bind_native_audio_object(installed).expect("idempotent installed binding"),
        BindOutcome::Bound(actual) if actual == receipt
    ));
    let wrong_object = crate::NativeAudioObjectObservation::new(
        object.process_id(),
        std::num::NonZeroU64::new(
            object.object_serial().get().checked_add(1).expect("test object serial"),
        )
        .unwrap(),
    );
    let refused = crate::NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: wrong_object,
    };
    assert_eq!(
        worker.bind_native_audio_object(refused).expect("wrong object pure refusal"),
        BindOutcome::Refused(crate::NativeAudioObjectBindRefusal::ObjectChanged)
    );
    for consumption in [
        Consumption::Preserve,
        Consumption::Drain,
        Consumption::DrainAuthor(installed),
        Consumption::DrainAuthor(installed),
    ] {
        let unchanged = worker.poll_native_observations(consumption).expect("same bound Source");
        assert_eq!(unchanged.object, Some(object));
        assert!(unchanged.parameters.is_empty());
        assert_eq!(
            unchanged.author.expect("same closed Source").source,
            accepted.source
        );
    }
    assert_eq!(
        parameter_query_counts(),
        twice,
        "same receipt and refused Bind keep the cache"
    );
    assert_eq!(ACCEPTED_EVENTS.load(AuditOrdering::SeqCst), 3);
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
    assert_eq!(
        worker
            .close_native_audio_editor(installed)
            .expect("original same-object view close"),
        crate::NativeAudioEditorOutcome::Closed { binding: installed }
    );
    drop(worker);
    assert_eq!(CREATED.load(AuditOrdering::SeqCst), 1);
    assert_eq!(DESTROYED.load(AuditOrdering::SeqCst), 1);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
}

#[test]
fn clap_paused_without_state_delivers_canonical_parameters_and_keeps_exact_ack() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery,
        NativeAudioObservationConsumption as Consumption,
    };
    let _serial = SERIAL.lock().expect("actual native fixture serialization");
    reset_audit();
    WITHOUT_STATE.store(true, AuditOrdering::SeqCst);
    let mut worker = owned_worker();
    worker.enter_state(23).expect("legal actual first entry without State");
    let binding = bind_and_open(worker.as_mut());
    exchange_another_paused_gui_value(worker.as_mut());
    let first = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("same main owner");
    let author = first.author.expect("actual source");
    let Delivery::Delivered(first) = author.delivery else {
        panic!("legal optional State absence must not reject representable parameter edits");
    };
    assert_eq!(
        first.events.len(),
        3,
        "real begin/value/end, no invented close"
    );
    assert!(
        first.configuration.is_none(),
        "no empty or manufactured opaque state"
    );
    assert_eq!(
        (
            author.source.open_parameter_edits(),
            author.source.group_depth()
        ),
        (0, 0)
    );
    let retry = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("same retained prefix");
    assert!(matches!(retry.author.expect("owner").delivery,
        Delivery::AwaitingAcknowledgement { binding: actual, revision: 3, request_cohort: None } if actual == binding));
    exchange_another_paused_gui_value(worker.as_mut());
    let installed =
        crate::NativeAudioObjectBindRequest { binding: AudioProcessorBindingId::new(), ..binding };
    assert!(matches!(
        worker.bind_native_audio_object(installed).expect("same native object"),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    let ack = crate::NativeAudioAuthorAcknowledgement {
        source_binding: binding,
        installed_binding: installed,
        revision: 3,
    };
    let acknowledged = worker
        .poll_native_observations(Consumption::AcknowledgeAuthor(ack))
        .expect("exact selected Ack");
    assert!(
        matches!(acknowledged.author.expect("owner").delivery, Delivery::Acknowledged { acknowledgement: actual, request_cohort: None } if actual == ack)
    );
    let suffix = worker
        .poll_native_observations(Consumption::DrainAuthor(installed))
        .expect("new suffix");
    let Delivery::Delivered(suffix) = suffix.author.expect("source").delivery else {
        panic!("new callbacks retained");
    };
    assert_eq!(suffix.revision, 6);
    assert!(suffix.configuration.is_none());
    worker
        .poll_native_observations(Consumption::AcknowledgeAuthor(
            crate::NativeAudioAuthorAcknowledgement {
                source_binding: installed,
                installed_binding: installed,
                revision: 6,
            },
        ))
        .expect("suffix exact Ack");
    worker.close_native_audio_editor(installed).expect("same real view close");
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    assert_eq!(
        PROCESSES.load(AuditOrdering::SeqCst),
        0,
        "actual idle flush, no dummy PCM"
    );
}

#[test]
fn clap_paused_stateless_completion_is_explicit_and_save_failure_keeps_source() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery,
        NativeAudioObservationConsumption as Consumption,
    };
    use mondrian_core::NativeAudioAuthorCompletion as Completion;
    let _serial = SERIAL.lock().expect("actual native fixture serialization");
    for without_state in [true, false] {
        reset_audit();
        WITHOUT_STATE.store(without_state, AuditOrdering::SeqCst);
        let mut worker = owned_worker();
        worker.enter_state(23).expect("legal first entry");
        let binding = bind_and_open(worker.as_mut());
        exchange_another_paused_gui_value(worker.as_mut());
        SAVE_FAILS.store(!without_state, AuditOrdering::SeqCst);
        let result = worker
            .poll_native_observations(Consumption::DrainAuthor(binding))
            .expect("real main capture");
        let author = result.author.expect("actual source");
        if without_state {
            let Delivery::Delivered(delivery) = author.delivery else {
                panic!("no-State parameter source");
            };
            assert_eq!(delivery.completion(), Completion::ParametersOnly);
            assert!(delivery.configuration().is_none());
            worker
                .poll_native_observations(Consumption::AcknowledgeAuthor(
                    crate::NativeAudioAuthorAcknowledgement {
                        source_binding: binding,
                        installed_binding: binding,
                        revision: delivery.revision(),
                    },
                ))
                .expect("real exact parameter Ack");
        } else {
            assert!(
                matches!(
                    author.delivery,
                    Delivery::Refused(crate::NativeAudioAuthorRefusal::ConfigurationUnavailable)
                ),
                "actual save failure cannot be reinterpreted as optional extension absence"
            );
            assert_eq!(
                author.source.pending_events(),
                3,
                "unaccepted actual output stays owned"
            );
            assert!(!author.source.closed_empty());
            SAVE_FAILS.store(false, AuditOrdering::SeqCst);
            let retry = worker
                .poll_native_observations(Consumption::DrainAuthor(binding))
                .expect("same retained source retry");
            let Delivery::Delivered(delivery) = retry.author.expect("actual source").delivery
            else {
                panic!("actual successful State.save retry");
            };
            assert_eq!(delivery.completion(), Completion::CoherentConfiguration);
            assert_eq!(
                delivery.configuration(),
                Some(0.75_f64.to_bits().to_le_bytes().as_slice())
            );
            worker
                .poll_native_observations(Consumption::AcknowledgeAuthor(
                    crate::NativeAudioAuthorAcknowledgement {
                        source_binding: binding,
                        installed_binding: binding,
                        revision: delivery.revision(),
                    },
                ))
                .expect("same accepted coherent prefix");
        }
        worker.close_native_audio_editor(binding).expect("same actual view close");
        assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
        assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
        drop(worker);
    }
}

fn emit_paused_host_reference_controls(worker: &mut dyn IsolatedAudioProcessorWorker) {
    let descriptor = super::super::ClapParameterDescriptor {
        id: 17,
        name: "Gain".to_owned(),
        min_value: 0.0,
        max_value: 1.0,
        default_value: 0.5,
        current_value: 0.5,
        automatable: true,
        stepped: false,
        enumeration: false,
        read_only: false,
        hidden: false,
    };
    let ids = [descriptor.parameter_id().expect("same actual schema")];
    let layout = super::super::protocol::SharedBlockLayout::new(16, 2, 0, 1).expect("layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared = bytemuck::cast_slice_mut(&mut aligned);
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    let context = super::super::worker::WorkerProcessContext {
        musical_time_reference: None,
        auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
        layout,
        auxiliary_inputs: &auxiliary,
        note_input_contract: &crate::AudioProcessorNoteInputContract::default(),
        parameter_ids: &ids,
        render_contract: render_contract(),
    };
    REQUEST_HOST_CONTROLS.store(3, AuditOrdering::SeqCst);
    let profile = worker.native_worker_lane_resources().expect("same owning profile");
    super::super::native_worker_processing::with_processing_scope(
        worker.split_native_processing().expect("same real native instance"),
        profile,
        shared,
        context,
        |main, _| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while HOST_CONTROLS_EMITTED.load(AuditOrdering::SeqCst) < 2 && Instant::now() < deadline
            {
                main.service_native_main_thread()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(HOST_CONTROLS_EMITTED.load(AuditOrdering::SeqCst), 2);
            Ok(())
        },
    )
    .expect("real main timer callback, joined native DSP role");
}

#[test]
fn clap_stateless_clear_values_rescan_retains_order_and_exact_ack_old_api() {
    use crate::{
        NativeAudioAuthorDeliveryState as Delivery, NativeAudioAuthorEvent as Event,
        NativeAudioObservationConsumption as Consumption,
    };
    let _serial = SERIAL.lock().expect("actual native fixture serialization");
    reset_audit();
    WITHOUT_STATE.store(true, AuditOrdering::SeqCst);
    let mut worker = owned_worker();
    worker.enter_state(23).expect("legal actual native entry");
    let binding = bind_and_open(worker.as_mut());
    emit_paused_host_reference_controls(worker.as_mut());
    let result = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("actual main controls");
    let author = result.author.expect("same source");
    let Delivery::Delivered(delivery) = author.delivery else {
        panic!("optional State absence must not reject legal host-reference Clear/VALUES");
    };
    assert_eq!(delivery.events.len(), 2);
    assert!(
        matches!(&delivery.events[0], Event::ParameterReferencesCleared { parameter, flags: 2 }
        if parameter.as_str() == "clap.param.17")
    );
    assert!(
        matches!(&delivery.events[1], Event::ParameterValuesRescanned { kind: mondrian_core::NativeAudioParameterRescanKind::Values, values }
        if values.len() == 1 && values[0].value().map(|value| value.bits()) == Some(0.625_f64.to_bits()))
    );
    assert!(
        delivery.configuration.is_none(),
        "no fabricated opaque payload"
    );
    assert_eq!(author.source.pending_events(), 2);
    assert_eq!(
        PROCESSES.load(AuditOrdering::SeqCst),
        0,
        "actual paused main controls need no dummy Process"
    );
    let retry = worker
        .poll_native_observations(Consumption::DrainAuthor(binding))
        .expect("prefix retained");
    assert!(matches!(
        retry.author.expect("source").delivery,
        Delivery::AwaitingAcknowledgement { revision: 2, .. }
    ));
    let installed =
        crate::NativeAudioObjectBindRequest { binding: AudioProcessorBindingId::new(), ..binding };
    assert!(matches!(
        worker
            .bind_native_audio_object(installed)
            .expect("same object next Plan binding"),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    let acknowledged = worker
        .poll_native_observations(Consumption::AcknowledgeAuthor(
            crate::NativeAudioAuthorAcknowledgement {
                source_binding: binding,
                installed_binding: installed,
                revision: 2,
            },
        ))
        .expect("exact accepted prefix Ack");
    assert!(
        matches!(acknowledged.author.expect("same source").delivery, Delivery::Acknowledged { acknowledgement: actual, request_cohort: None } if actual.revision == 2)
    );
    let closed = worker
        .poll_native_observations(Consumption::Preserve)
        .expect("actual remaining source");
    assert!(closed.author.expect("same source").source.closed_empty());
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    worker.close_native_audio_editor(installed).expect("same real attachment close");
}
