//! Owned native CLAP GUI/timer producer exercising existing worker APIs.
//! The fixture observes native lifecycle, thread roles and rendered samples.

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
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering as AuditOrdering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetClientRect, GetParent, IsWindow, SetWindowPos, ShowWindow,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WS_CHILD,
};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
static WRONG_THREAD: AtomicUsize = AtomicUsize::new(0);
static CREATED: AtomicUsize = AtomicUsize::new(0);
static DESTROYED: AtomicUsize = AtomicUsize::new(0);
static HIDDEN: AtomicUsize = AtomicUsize::new(0);
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static TIMERS: AtomicUsize = AtomicUsize::new(0);
static PROCESSES: AtomicUsize = AtomicUsize::new(0);
static RESETS: AtomicUsize = AtomicUsize::new(0);
static CHILD: AtomicUsize = AtomicUsize::new(0);
static PARENT: AtomicUsize = AtomicUsize::new(0);
static SELF_DESTROY_ON_TIMER: AtomicBool = AtomicBool::new(false);
static SELF_DESTROYED: AtomicBool = AtomicBool::new(false);
static RESIZE_ON_TIMER: AtomicBool = AtomicBool::new(false);
static RESIZES: AtomicUsize = AtomicUsize::new(0);

static STATIC_PARAMETERS_MODE: AtomicUsize = AtomicUsize::new(0);
static STATIC_FLUSHES: AtomicUsize = AtomicUsize::new(0);
static STATIC_STATE_SAVES: AtomicUsize = AtomicUsize::new(0);
static STATIC_STATE_LOADS: AtomicUsize = AtomicUsize::new(0);
static STATIC_METADATA_CHANGED: AtomicBool = AtomicBool::new(false);
static STATIC_POST_FLUSH_REQUESTED: AtomicBool = AtomicBool::new(false);
struct GuiPlugin;
struct GuiShared<'a> {
    host: HostSharedHandle<'a>,
    static_mode: usize,
    static_value: std::sync::atomic::AtomicU64,
}
struct GuiMain<'a> {
    shared: &'a GuiShared<'a>,
    host: HostMainThreadHandle<'a>,
    timer: Cell<Option<TimerId>>,
    child: Cell<HWND>,
    created: Cell<bool>,
    size: Cell<GuiSize>,
}
struct GuiProcessor<'a>(&'a GuiShared<'a>);

impl<'a> PluginShared<'a> for GuiShared<'a> {}
impl GuiShared<'_> {
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
impl Plugin for GuiPlugin {
    type Shared<'a> = GuiShared<'a>;
    type MainThread<'a> = GuiMain<'a>;
    type AudioProcessor<'a> = GuiProcessor<'a>;
    fn declare_extensions(
        builder: &mut PluginExtensions<Self>,
        _shared: Option<&Self::Shared<'_>>,
    ) {
        builder.register::<PluginAudioPorts>().register::<PluginTimer>();
        if STATIC_PARAMETERS_MODE.load(AuditOrdering::SeqCst) != 15 {
            builder.register::<PluginGui>();
        }
        // Only the new accepted-configuration control advertises State. Existing
        // GUI and parameter cases retain their original State capability.
        if STATIC_PARAMETERS_MODE.load(AuditOrdering::SeqCst) == 16 {
            builder.register::<PluginState>();
        }
        if STATIC_PARAMETERS_MODE.load(AuditOrdering::SeqCst) == 9 {
            builder.register::<MissingFlushParams>();
        } else if STATIC_PARAMETERS_MODE.load(AuditOrdering::SeqCst) != 0 {
            builder.register::<PluginParams>();
        }
    }
}
impl DefaultPluginFactory for GuiPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("org.mondrian.test.native-gui", "Owned native GUI")
    }
    fn new_shared(host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        let shared = GuiShared {
            host,
            static_mode: STATIC_PARAMETERS_MODE.load(AuditOrdering::SeqCst),
            static_value: std::sync::atomic::AtomicU64::new(1.0_f64.to_bits()),
        };
        shared.check(Some(true), false);
        host.request_callback();
        Ok(shared)
    }
    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a Self::Shared<'a>,
    ) -> Result<Self::MainThread<'a>, PluginError> {
        Ok(GuiMain {
            shared,
            host,
            timer: Cell::new(None),
            child: Cell::new(std::ptr::null_mut()),
            created: Cell::new(false),
            size: Cell::new(GuiSize { width: 320, height: 200 }),
        })
    }
}
impl<'a> PluginMainThread<'a, GuiShared<'a>> for GuiMain<'a> {
    fn on_main_thread(&self) {
        self.shared.check(Some(true), false);
        if self.timer.get().is_none()
            && let Some(extension) = self.host.get_extension::<HostTimer>()
            && let Ok(id) = extension.register_timer(&self.host, 10)
        {
            self.timer.set(Some(id));
        }
        let count = CALLBACKS.fetch_add(1, AuditOrdering::SeqCst) + 1;
        if count < 200 && self.shared.static_mode == 0 {
            self.shared.host.request_callback();
        }
    }
}
impl PluginAudioPortsImpl for GuiMain<'_> {
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
impl PluginTimerImpl for GuiMain<'_> {
    fn on_timer(&self, id: TimerId) {
        self.shared.check(Some(true), false);
        if self.timer.get() != Some(id) {
            WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
        }
        TIMERS.fetch_add(1, AuditOrdering::SeqCst);
        if RESIZE_ON_TIMER.swap(false, AuditOrdering::SeqCst) {
            let size = GuiSize { width: 480, height: 240 };
            let accepted = self.set_size(size).is_ok()
                && self.shared.host.get_extension::<HostGui>().is_some_and(|gui| {
                    gui.request_resize(&self.shared.host, size.width, size.height).is_ok()
                });
            if accepted {
                RESIZES.fetch_add(1, AuditOrdering::SeqCst);
            } else {
                WRONG_THREAD.fetch_add(1, AuditOrdering::SeqCst);
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
impl PluginGuiImpl for GuiMain<'_> {
    fn is_api_supported(&self, config: GuiConfiguration) -> bool {
        self.shared.check(Some(true), false);
        if self.shared.static_mode == 13
            && let Some(params) =
                self.shared.host.get_extension::<clack_extensions::params::HostParams>()
        {
            params.request_flush(&self.shared.host);
        }
        self.shared.static_mode != 7 && config.api_type == GuiApiType::WIN32 && !config.is_floating
    }
    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        Some(GuiConfiguration { api_type: GuiApiType::WIN32, is_floating: false })
    }
    fn create(&self, config: GuiConfiguration) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        if self.shared.static_mode == 8
            || !self.is_api_supported(config)
            || self.created.replace(true)
        {
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
impl Drop for GuiMain<'_> {
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
impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, GuiShared<'a>, GuiMain<'a>>
    for GuiProcessor<'a>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &GuiMain<'a>,
        shared: &'a GuiShared<'a>,
        _config: clack_plugin::prelude::PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        shared.check(Some(true), false);
        Ok(Self(shared))
    }
    fn reset(&mut self) {
        self.0.check(None, true);
        RESETS.fetch_add(1, AuditOrdering::SeqCst);
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
                        *output = sample * 2.0;
                    }
                }
                _ => return Err(PluginError::Message("separate main buffers expected")),
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

fn reset_audit() {
    STATIC_PARAMETERS_MODE.store(0, AuditOrdering::SeqCst);
    STATIC_FLUSHES.store(0, AuditOrdering::SeqCst);
    STATIC_STATE_SAVES.store(0, AuditOrdering::SeqCst);
    STATIC_STATE_LOADS.store(0, AuditOrdering::SeqCst);
    STATIC_METADATA_CHANGED.store(false, AuditOrdering::SeqCst);
    STATIC_POST_FLUSH_REQUESTED.store(false, AuditOrdering::SeqCst);
    for value in [
        &WRONG_THREAD,
        &CREATED,
        &DESTROYED,
        &HIDDEN,
        &CALLBACKS,
        &TIMERS,
        &PROCESSES,
        &RESETS,
        &CHILD,
        &PARENT,
    ] {
        value.store(0, AuditOrdering::SeqCst);
    }
    SELF_DESTROY_ON_TIMER.store(false, AuditOrdering::SeqCst);
    SELF_DESTROYED.store(false, AuditOrdering::SeqCst);
    RESIZE_ON_TIMER.store(false, AuditOrdering::SeqCst);
    RESIZES.store(0, AuditOrdering::SeqCst);
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
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<GuiPlugin>>(c"/test/native-gui")
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
    prepare_loaded(
        IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: profile,
            payload: Vec::new(),
            plugin_execution_contract: contract,
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract: render_contract(),
        },
        entry,
        c"org.mondrian.test.native-gui",
        None,
        &[],
        &[],
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
fn clap_native_gui_real_parent_destroy_balanced_old_api() {
    let _serial = SERIAL.lock().expect("native fixture serialization");
    reset_audit();
    let mut worker = owned_worker();
    let binding = bind_and_open(worker.as_mut());
    let child = CHILD.load(AuditOrdering::SeqCst) as HWND;
    let parent = PARENT.load(AuditOrdering::SeqCst) as HWND;
    assert_ne!(unsafe { IsWindow(child) }, 0);
    assert_ne!(unsafe { IsWindow(parent) }, 0);
    assert_eq!(unsafe { GetParent(child) }, parent);
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    assert_ne!(unsafe { GetClientRect(parent, &mut rect) }, 0);
    assert_eq!((rect.right - rect.left, rect.bottom - rect.top), (320, 200));
    assert_eq!(
        worker
            .close_native_audio_editor(binding)
            .expect("native destroy acknowledgement"),
        crate::NativeAudioEditorOutcome::Closed { binding }
    );
    assert_eq!(unsafe { IsWindow(child) }, 0);
    assert_eq!(unsafe { IsWindow(parent) }, 0);
    assert_eq!(CREATED.load(AuditOrdering::SeqCst), 1);
    assert_eq!(DESTROYED.load(AuditOrdering::SeqCst), 1);
    drop(worker);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
}

#[test]
fn clap_native_gui_idle_timer_and_callback_after_real_dsp_old_api() {
    let _serial = SERIAL.lock().expect("native fixture serialization");
    reset_audit();
    let mut worker = owned_worker();
    worker.enter_state(0).expect("canonical Enter");
    let binding = bind_and_open(worker.as_mut());
    let before = CALLBACKS.load(AuditOrdering::SeqCst);
    assert!(
        before < 200,
        "preparation must leave a legal continuously requested callback pending"
    );
    let layout = super::super::protocol::SharedBlockLayout::uniform(16, 2, 0, 0)
        .expect("actual shared layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut aligned);
    for bytes in shared[..8 * 2 * 4].chunks_exact_mut(4) {
        bytes.copy_from_slice(&0.125_f32.to_le_bytes());
    }
    let aux = AudioProcessorAuxiliaryInputContract::default();
    let context = super::super::worker::WorkerProcessContext {
        musical_time_reference: None,
        auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
        layout,
        auxiliary_inputs: &aux,
        note_input_contract: &crate::AudioProcessorNoteInputContract::default(),
        parameter_ids: &[],
        render_contract: render_contract(),
    };
    let profile = worker
        .native_worker_lane_resources()
        .expect("actual Windows native lane profile");
    let split = worker.split_native_processing().expect("same-object native roles");
    let result = super::super::native_worker_processing::with_processing_scope(
        split,
        profile,
        shared,
        context,
        |main, dsp| {
            dsp.submit(super::super::protocol::WorkerCommand {
                transport_activity: None,
                kind: super::super::protocol::CommandKind::Process,
                start_sample: 0,
                frames: 8,
                event_count: 0,
                note_count: 0,
            })?;
            dsp.complete(main)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            // No more Process jobs and no incoming IPC commands. Real main service
            // must still deliver timer ticks and callback continuation on main.
            while (TIMERS.load(AuditOrdering::SeqCst) < 3
                || CALLBACKS.load(AuditOrdering::SeqCst) < 200)
                && Instant::now() < deadline
            {
                main.service_native_main_thread()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                TIMERS.load(AuditOrdering::SeqCst) >= 3,
                "real periodic main timers must continue idle"
            );
            assert_eq!(
                CALLBACKS.load(AuditOrdering::SeqCst),
                200,
                "continuous legal requests cannot be lost at hard16"
            );
            Ok(())
        },
    )
    .expect("actual scoped DSP join/main finish");
    assert!(
        result.scheduling_exit.is_none(),
        "offline is never counted as realtime admission"
    );
    for bytes in shared[..8 * 2 * 4].chunks_exact(4) {
        assert_eq!(f32::from_le_bytes(bytes.try_into().expect("f32")), 0.25);
    }
    assert_eq!(
        PROCESSES.load(AuditOrdering::SeqCst),
        1,
        "idle service must not invent PCM"
    );
    assert_eq!(
        worker.close_native_audio_editor(binding).expect("joined native close"),
        crate::NativeAudioEditorOutcome::Closed { binding }
    );
    drop(worker);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
}

#[test]
fn clap_native_gui_closed_true_acknowledges_destroy_without_hide() {
    let _serial = SERIAL.lock().expect("native fixture serialization");
    reset_audit();
    let mut worker = owned_worker();
    worker.enter_state(0).expect("Enter");
    let binding = bind_and_open(worker.as_mut());
    let profile = worker.native_worker_lane_resources().expect("actual Windows profile");
    let layout = super::super::protocol::SharedBlockLayout::uniform(16, 2, 0, 0).expect("layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut aligned);
    let auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
    let context = super::super::worker::WorkerProcessContext {
        musical_time_reference: None,
        auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
        layout,
        auxiliary_inputs: &auxiliary_inputs,
        note_input_contract: &crate::AudioProcessorNoteInputContract::default(),
        parameter_ids: &[],
        render_contract: render_contract(),
    };
    SELF_DESTROY_ON_TIMER.store(true, AuditOrdering::SeqCst);
    super::super::native_worker_processing::with_processing_scope(
        worker.split_native_processing().expect("same-object split"),
        profile,
        shared,
        context,
        |main, _| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !SELF_DESTROYED.load(AuditOrdering::SeqCst) && Instant::now() < deadline {
                main.service_native_main_thread()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                SELF_DESTROYED.load(AuditOrdering::SeqCst),
                "actual timer-driven plugin native close"
            );
            assert!(main.native_editor_close_requested()?);
            Ok(())
        },
    )
    .expect("joined idle DSP scope");
    assert_eq!(
        worker.close_native_audio_editor(binding).expect("destroy acknowledgement"),
        crate::NativeAudioEditorOutcome::Closed { binding }
    );
    assert_eq!(
        HIDDEN.load(AuditOrdering::SeqCst),
        0,
        "destroyed GUI must not receive hide"
    );
    assert_eq!(
        DESTROYED.load(AuditOrdering::SeqCst),
        1,
        "exactly one destroy acknowledgement"
    );
    assert_eq!(
        PROCESSES.load(AuditOrdering::SeqCst),
        0,
        "idle close cannot invent a process call"
    );
    assert_eq!(
        unsafe { IsWindow(PARENT.load(AuditOrdering::SeqCst) as HWND) },
        0
    );
    drop(worker);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
}

#[test]
fn clap_native_timer_main_identity_remains_valid_with_symbolic_audio_role() {
    use clack_extensions::timer::HostTimerImpl;
    let _serial = SERIAL.lock().expect("native fixture serialization");
    reset_audit();
    let control = ClapHostShared::new();
    let entry =
        PluginEntry::load_from_clack::<SinglePluginEntry<GuiPlugin>>(c"/test/dual-role-timer")
            .expect("actual owned native plugin");
    let info = HostInfo::new(
        "Timer role host",
        "Mondrian",
        "https://mondrian.invalid",
        "0.0.0",
    )
    .expect("host identity");
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| control.clone().with_native_logs().expect("actual shared owner"),
        |shared| super::super::clap_main_services::ClapHostMainThread::new(shared),
        &entry,
        c"org.mondrian.test.native-gui",
        &info,
    )
    .expect("actual initialized plugin/host");
    let mut processor: PluginAudioProcessor<ClapHost> = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: 48_000.0,
                min_frames_count: 1,
                max_frames_count: 16,
            },
        )
        .expect("actual exclusive activated audio owner")
        .into();
    assert!(control.is_main_thread());
    assert!(!control.is_audio_thread());
    {
        let _exclusive_audio_handle = processor.plugin_handle();
        let _audio = control.enter_audio_thread();
        assert!(control.is_main_thread());
        assert!(control.is_audio_thread());
        instance.access_handler(|main| {
            let id = main
                .register_timer(10)
                .expect("main-thread timer remains legal with simultaneous symbolic audio role");
            main.unregister_timer(id).expect("same main identity unregister");
        });
    }
    assert!(!control.is_audio_thread());
    drop(processor);
    instance.try_deactivate().expect("actual main deactivation");
    drop(instance);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
}

#[test]
fn native_entry_binding_clap_first_signal_keeps_real_idle_gui_without_reset_old_api() {
    let _serial = SERIAL.lock().expect("actual owned plugin serialization");
    reset_audit();
    let mut worker = owned_worker();
    let prior = bind_and_open(worker.as_mut());
    let child = CHILD.load(AuditOrdering::SeqCst) as HWND;
    let parent = PARENT.load(AuditOrdering::SeqCst) as HWND;
    worker.enter_state(-19).expect("real signed first coordinate");
    assert_eq!(
        RESETS.load(AuditOrdering::SeqCst),
        0,
        "FIRST_CLAP_RESET_PRIMARY: successful activation is the first signal initialization proof"
    );
    assert_eq!(CREATED.load(AuditOrdering::SeqCst), 1);
    assert_eq!(DESTROYED.load(AuditOrdering::SeqCst), 0);
    assert_eq!(unsafe { IsWindow(child) }, 1);
    assert_eq!(unsafe { IsWindow(parent) }, 1);
    assert_eq!(
        worker.native_audio_object_observation().expect("same actual object"),
        Some(prior.expected_object)
    );
    let binding = crate::NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: prior.expected_object,
    };
    assert!(
        matches!(worker.bind_native_audio_object(binding).expect("current same-object binding"),crate::NativeAudioObjectBindOutcome::Bound(receipt) if receipt.binding()==binding.binding)
    );
    let layout = super::super::protocol::SharedBlockLayout::uniform(16, 2, 0, 0)
        .expect("real mapped layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut aligned);
    for bytes in shared[..8 * 2 * 4].chunks_exact_mut(4) {
        bytes.copy_from_slice(&0.125_f32.to_le_bytes());
    }
    let aux = AudioProcessorAuxiliaryInputContract::default();
    let context = super::super::worker::WorkerProcessContext {
        musical_time_reference: None,
        auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
        layout,
        auxiliary_inputs: &aux,
        note_input_contract: &crate::AudioProcessorNoteInputContract::default(),
        parameter_ids: &[],
        render_contract: render_contract(),
    };
    let profile = worker.native_worker_lane_resources().expect("actual admitted roles");
    RESIZE_ON_TIMER.store(true, AuditOrdering::SeqCst);
    let split = worker.split_native_processing().expect("same actual object DSP role");
    let result = super::super::native_worker_processing::with_processing_scope(
        split,
        profile,
        shared,
        context,
        |main, dsp| {
            dsp.submit(super::super::protocol::WorkerCommand {
                transport_activity: None,
                kind: super::super::protocol::CommandKind::Process,
                start_sample: -19,
                frames: 8,
                event_count: 0,
                note_count: 0,
            })?;
            dsp.complete(main)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while RESIZES.load(AuditOrdering::SeqCst) == 0 && Instant::now() < deadline {
                main.service_native_main_thread()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            main.service_native_main_thread()?;
            Ok(())
        },
    )
    .expect("actual native role entry and return");
    assert!(
        result.scheduling_exit.is_none(),
        "Offline never reports realtime admission"
    );
    for bytes in shared[..8 * 2 * 4].chunks_exact(4) {
        assert_eq!(
            f32::from_le_bytes(bytes.try_into().expect("real PCM")),
            0.25
        );
    }
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 1);
    assert_eq!(CHILD.load(AuditOrdering::SeqCst) as HWND, child);
    assert_eq!(PARENT.load(AuditOrdering::SeqCst) as HWND, parent);
    assert_eq!(
        RESIZES.load(AuditOrdering::SeqCst),
        1,
        "same main-thread timer requested resize after first Entry"
    );
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    assert_ne!(unsafe { GetClientRect(parent, &mut rect) }, 0);
    assert_eq!(
        (rect.right - rect.left, rect.bottom - rect.top),
        (480, 240),
        "same actual parent accepts resize under current native object"
    );
    worker.enter_state(77).expect("actual post-DSP seek");
    assert_eq!(
        RESETS.load(AuditOrdering::SeqCst),
        1,
        "later entry still resets actual DSP"
    );
    let current = crate::NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: prior.expected_object,
    };
    assert!(matches!(
        worker.bind_native_audio_object(current).expect("post-reset current binding"),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    assert_eq!(
        worker
            .close_native_audio_editor(current)
            .expect("actual destroy acknowledgement"),
        crate::NativeAudioEditorOutcome::Closed { binding: current }
    );
    assert_eq!(DESTROYED.load(AuditOrdering::SeqCst), 1);
    drop(worker);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
}

// These controls use the actual in-process CLAP ABI and original GUI/worker
// owner. They do not qualify a third-party DLL's process counter or RT cost.
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::PluginStateImpl;
use clack_host::prelude::{Event, InputEvents, OutputEvents};
use clack_host::stream::{InputStream, OutputStream};
use clack_plugin::extensions::prelude::{
    Extension, ExtensionImplementation, PluginExtensionSide, RawExtension,
    RawExtensionImplementation,
};
use std::io::{Read, Write};

// Keep the original metadata callbacks, replacing only the real optional flush
// function pointer. This exercises supports_flush on the actual same instance.
#[derive(Clone, Copy)]
struct MissingFlushParams;
// SAFETY: this test table has exactly the CLAP params ABI and identifier.
unsafe impl Extension for MissingFlushParams {
    const IDENTIFIERS: &[&CStr] = &[clap_sys::ext::params::CLAP_EXT_PARAMS];
    type ExtensionSide = PluginExtensionSide;
    unsafe fn from_raw(_raw: RawExtension<Self::ExtensionSide>) -> Self {
        Self
    }
}
// The ABI table itself owns a real static lifetime; no callback borrows a
// temporary produced while constructing RawExtensionImplementation.
static MISSING_FLUSH_PARAMS: clap_sys::ext::params::clap_plugin_params = {
    let original = <PluginParams as ExtensionImplementation<GuiPlugin>>::IMPLEMENTATION;
    // SAFETY: PluginParams exposes this exact permanently live C record.
    let original =
        unsafe { &*original.as_ptr().as_ptr().cast::<clap_sys::ext::params::clap_plugin_params>() };
    clap_sys::ext::params::clap_plugin_params {
        count: original.count,
        get_info: original.get_info,
        get_value: original.get_value,
        value_to_text: original.value_to_text,
        text_to_value: original.text_to_value,
        flush: None,
    }
};
// SAFETY: the original static implementation belongs to this exact GuiPlugin;
// all copied callbacks and the replacement optional function have matching ABI.
unsafe impl ExtensionImplementation<GuiPlugin> for MissingFlushParams {
    const IMPLEMENTATION: RawExtensionImplementation =
        RawExtensionImplementation::new(&MISSING_FLUSH_PARAMS);
}

fn static_flags(id: u32, mode: usize) -> ParamInfoFlags {
    if id == 900 {
        let mut flags = ParamInfoFlags::IS_HIDDEN; // nonautomatable is still a legal static write
        if mode == 4 {
            flags |= ParamInfoFlags::REQUIRES_PROCESS;
        }
        if mode == 5 || STATIC_METADATA_CHANGED.load(AuditOrdering::SeqCst) {
            flags |= ParamInfoFlags::IS_READONLY;
        }
        flags
    } else if id == 0 {
        ParamInfoFlags::IS_READONLY
    } else {
        ParamInfoFlags::REQUIRES_PROCESS
    }
}
impl PluginMainThreadParams for GuiMain<'_> {
    fn count(&self) -> u32 {
        3
    }
    fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
        self.shared.check(Some(true), false);
        if self.shared.static_mode == 14
            && STATIC_FLUSHES.load(AuditOrdering::SeqCst) != 0
            && !STATIC_POST_FLUSH_REQUESTED.swap(true, AuditOrdering::SeqCst)
            && let Some(params) =
                self.shared.host.get_extension::<clack_extensions::params::HostParams>()
        {
            params.request_flush(&self.shared.host);
        }
        if let Some(id) = [900, 0, 77].get(index as usize) {
            writer.set(&ParamInfo {
                id: ClapId::new(*id),
                flags: static_flags(*id, self.shared.static_mode),
                cookie: Default::default(),
                name: b"Static",
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 1.0,
            });
        }
    }
    fn get_value(&self, id: ClapId) -> Option<f64> {
        [900, 0, 77]
            .contains(&id.get())
            .then(|| f64::from_bits(self.shared.static_value.load(AuditOrdering::Acquire)))
    }
    fn value_to_text(
        &self,
        _id: ClapId,
        _value: f64,
        _writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        Err(std::fmt::Error)
    }
    fn text_to_value(&self, _id: ClapId, _text: &CStr) -> Option<f64> {
        None
    }
    fn flush(&self, _input: &InputEvents, _output: &mut OutputEvents) {
        panic!("an activated static Some must never use inactive/main flush");
    }
}
impl PluginAudioProcessorParams for GuiProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        self.0.check(Some(true), true); // same full main owner temporarily owns audio
        let flush = STATIC_FLUSHES.fetch_add(1, AuditOrdering::SeqCst) + 1;
        let mut received = 0;
        for event in input.iter() {
            let Some(clack_host::events::spaces::CoreEventSpace::ParamValue(value)) =
                event.as_core_event()
            else {
                panic!("static exchange contains only typed global values");
            };
            assert_eq!(value.param_id(), Some(ClapId::new(900)));
            assert_eq!(event.header().time(), 0);
            assert!(value.cookie().as_raw().is_null());
            assert_eq!(value.value().to_bits(), 0.25_f64.to_bits());
            self.0.static_value.store(value.value().to_bits(), AuditOrdering::Release);
            received += 1;
        }
        assert_eq!(received, 1);
        match self.0.static_mode {
            2 => {
                // Invalid output only sets the local refusal owner, not shared
                // source.failure. A plugin ignoring false still cannot Open.
                assert!(output
                    .try_push(ParamValueEvent::new(
                        0,
                        ClapId::new(999),
                        Pckn::match_all(),
                        0.5
                    ))
                    .is_err());
            }
            16 if flush == 1 => {
                // A real recordable value and actual private state, captured
                // by the original full-owner Poll before its exact Ack.
                assert!(output
                    .try_push(
                        ParamValueEvent::new(
                            0,
                            ClapId::new(900),
                            Pckn::match_all(),
                            f64::from_bits(self.0.static_value.load(AuditOrdering::Acquire))
                        )
                        .with_flags(EventFlags::IS_LIVE)
                    )
                    .is_ok());
            }
            3 => self.0.host.request_restart(),
            10 => STATIC_METADATA_CHANGED.store(true, AuditOrdering::SeqCst),
            11 | 14 => {
                assert!(output
                    .try_push(
                        ParamValueEvent::new(0, ClapId::new(900), Pckn::match_all(), 0.375)
                            .with_flags(EventFlags::DONT_RECORD)
                    )
                    .is_ok());
                assert!(output
                    .try_push(
                        ParamValueEvent::new(0, ClapId::new(900), Pckn::match_all(), 0.625)
                            .with_flags(EventFlags::IS_LIVE)
                    )
                    .is_ok());
                self.0.host.request_callback();
            }
            12 => {
                // Fixed main identity can also own the symbolic audio role, so
                // a main-only rescan would be legal here. request_flush alone
                // has the explicit !audio-thread restriction in params.h.
                let params = self
                    .0
                    .host
                    .get_extension::<clack_extensions::params::HostParams>()
                    .expect("the same actual host must advertise HostParams");
                params.request_flush(&self.0.host);
            }
            _ => {}
        }
    }
}

impl PluginStateImpl for GuiMain<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        assert_eq!(self.shared.static_mode, 16);
        STATIC_STATE_SAVES.fetch_add(1, AuditOrdering::SeqCst);
        output.write_all(&self.shared.static_value.load(AuditOrdering::Acquire).to_le_bytes())?;
        Ok(())
    }
    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        self.shared.check(Some(true), false);
        assert_eq!(self.shared.static_mode, 16);
        STATIC_STATE_LOADS.fetch_add(1, AuditOrdering::SeqCst);
        let mut bytes = [0; 8];
        input.read_exact(&mut bytes)?;
        let bits = u64::from_le_bytes(bytes);
        let value = f64::from_bits(bits);
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(PluginError::Message("invalid static control state"));
        }
        self.shared.static_value.store(bits, AuditOrdering::Release);
        Ok(())
    }
}

fn static_parameter_worker(mode: usize) -> Box<ClapWorker> {
    STATIC_PARAMETERS_MODE.store(mode, AuditOrdering::SeqCst);
    let descriptors = [900, 0, 77].map(|id| {
        let flags = static_flags(id, mode);
        super::super::ClapParameterDescriptor {
            id,
            name: "Static".to_owned(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 1.0,
            current_value: 1.0,
            automatable: false,
            stepped: false,
            enumeration: false,
            read_only: flags.contains(ParamInfoFlags::IS_READONLY),
            hidden: flags.contains(ParamInfoFlags::IS_HIDDEN),
        }
    });
    let entry =
        PluginEntry::load_from_clack::<SinglePluginEntry<GuiPlugin>>(c"/test/native-gui").unwrap();
    let native_main = Rc::new(vst3_host::window::WindowsEditorMainThread::initialize().unwrap());
    let mut worker = prepare_loaded_on_main(
        native_main,
        IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: Default::default(),
            note_input_contract: Default::default(),
            native_lane: ClapAudioProcessorWorkerFactory.native_worker_lane_resources().unwrap(),
            payload: Vec::new(),
            plugin_execution_contract: AudioProcessorExecutionContract::new(
                0,
                crate::AudioProcessorTail::None,
                true,
                true,
                true,
                0,
            )
            .unwrap(),
            auxiliary_inputs: Default::default(),
            parameter_ids: Vec::new(),
            render_contract: render_contract(),
        },
        entry,
        c"org.mondrian.test.native-gui",
        None,
        ClapMainAudioInput::Present,
        0,
        &descriptors,
        &[77, 900, 0],
    )
    .unwrap();
    let binding = crate::NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: worker.native_object,
    };
    assert!(matches!(
        worker.bind_native_audio_object(binding).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    worker
}
fn static_open(
    worker: &mut ClapWorker,
    lane: u32,
    value: f64,
) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
    let receipt = worker.native_binding.unwrap();
    let binding = crate::NativeAudioObjectBindRequest {
        binding: receipt.binding(),
        expected_object: receipt.object(),
    };
    worker.open_native_audio_editor(
        crate::NativeAudioEditorOpenRequest::new(
            binding,
            NativeAudioEditorTitle::new("Static CLAP".to_owned()).unwrap(),
        )
        .with_static_parameter_values(vec![(lane, value.to_bits())])
        .unwrap(),
    )
}

#[test]
fn clap_idle_static_values_exchange_uses_actual_active_role_and_preserves_live_open_idempotence() {
    let _serial = SERIAL.lock().unwrap();
    reset_audit();
    let mut worker = static_parameter_worker(1);
    let source = worker.shared.parameter_source.snapshot();
    let object = worker.native_object;
    assert!(matches!(
        static_open(&mut worker, 1, 0.25).unwrap(),
        crate::NativeAudioEditorOutcome::Opened { width: 320, height: 200, .. }
    ));
    assert_eq!(STATIC_FLUSHES.load(AuditOrdering::SeqCst), 1);
    assert_eq!(worker.shared.parameter_source.snapshot(), source);
    assert_eq!(worker.native_object, object);
    assert!(worker.initial_signal_available);
    assert_eq!(worker.last_end, None);
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
    assert_eq!(RESETS.load(AuditOrdering::SeqCst), 0);
    assert!(matches!(
        static_open(&mut worker, 1, 0.25).unwrap(),
        crate::NativeAudioEditorOutcome::Opened { .. }
    ));
    assert_eq!(
        STATIC_FLUSHES.load(AuditOrdering::SeqCst),
        1,
        "attached Open is idempotent"
    );
    let binding = worker.native_binding.unwrap();
    worker
        .close_native_audio_editor(crate::NativeAudioObjectBindRequest {
            binding: binding.binding(),
            expected_object: binding.object(),
        })
        .unwrap();
    drop(worker);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    reset_audit();
}

#[test]
fn clap_idle_static_values_pre_refuse_flags_range_capacity_missing_flush_and_no_gui() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [1, 4, 5, 7, 9, 13, 15] {
        reset_audit();
        let mut worker = static_parameter_worker(mode);
        if mode == 1 {
            worker.input_events = ClapInputEvents::try_with_note_capacity(0, 0).unwrap();
        }
        let before = worker.shared.parameter_source.snapshot();
        let handle = worker.instance.plugin_handle();
        let params = handle.get_extension::<PluginParams>().unwrap();
        assert_eq!(
            params.supports_flush(&handle),
            mode != 9,
            "actual optional native function pointer"
        );
        assert!(matches!(
            static_open(&mut worker, 1, 0.25).unwrap(),
            crate::NativeAudioEditorOutcome::Refused(_)
        ));
        assert_eq!(STATIC_FLUSHES.load(AuditOrdering::SeqCst), 0, "mode {mode}");
        assert_eq!(CREATED.load(AuditOrdering::SeqCst), 0);
        assert_eq!(worker.shared.parameter_source.snapshot(), before);
        assert_eq!(worker.input_events.as_input().len(), 0);
        let handle = worker.instance.plugin_handle();
        let params = handle.get_extension::<PluginParams>().unwrap();
        assert_eq!(
            params.get_value(&handle, ClapId::new(900)).unwrap().to_bits(),
            1.0_f64.to_bits()
        );
        if mode == 13 {
            assert!(worker.author_source_status().parameter_flush_pending());
        }
        if ![7, 15].contains(&mode) {
            let receipt = worker.native_binding.unwrap();
            let binding = crate::NativeAudioObjectBindRequest {
                binding: receipt.binding(),
                expected_object: receipt.object(),
            };
            let empty = crate::NativeAudioEditorOpenRequest::new(
                binding,
                NativeAudioEditorTitle::new("No static values".to_owned()).unwrap(),
            );
            assert!(
                matches!(
                    worker.open_native_audio_editor(empty).unwrap(),
                    crate::NativeAudioEditorOutcome::Opened { .. }
                ),
                "None must not require selected-lane flush support"
            );
            assert_eq!(STATIC_FLUSHES.load(AuditOrdering::SeqCst), 0);
            worker.close_native_audio_editor(binding).unwrap();
        }
        drop(worker);
        assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    }
    reset_audit();
    let mut worker = static_parameter_worker(1);
    for (lane, value) in [(1, -0.1), (1, 1.1), (0, 0.25), (2, 0.25)] {
        assert!(matches!(
            static_open(&mut worker, lane, value).unwrap(),
            crate::NativeAudioEditorOutcome::Refused(_)
        ));
    }
    assert!(static_open(&mut worker, 3, 0.25).is_err());
    assert_eq!(STATIC_FLUSHES.load(AuditOrdering::SeqCst), 0);
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
    drop(worker);
    reset_audit();
}

#[test]
fn clap_idle_static_values_keep_actual_output_flags_prefix_and_callbacks_without_ack() {
    let _serial = SERIAL.lock().unwrap();
    reset_audit();
    let mut worker = static_parameter_worker(11);
    assert!(matches!(
        static_open(&mut worker, 1, 0.25).unwrap(),
        crate::NativeAudioEditorOutcome::Opened { .. }
    ));
    let output = worker.output_events.events();
    assert_eq!(output.len(), 2);
    assert_eq!(output[0].flags, EventFlags::DONT_RECORD.bits());
    assert_eq!(output[1].flags, EventFlags::IS_LIVE.bits());
    assert_eq!((output[0].sequence, output[1].sequence), (1, 2));
    assert!(output.iter().all(|event| event.origin == ClapOutputOrigin::ActiveFlush));
    let source = worker.shared.parameter_source.snapshot();
    assert_eq!(
        (source.pending_output, source.pending_author_output),
        (2, 1)
    );
    assert!(worker.shared.has_unserviced_callbacks());

    assert!(worker.native_author_delivery.is_none());
    let binding = worker.native_binding.unwrap();
    let binding = crate::NativeAudioObjectBindRequest {
        binding: binding.binding(),
        expected_object: binding.object(),
    };
    let observed = worker
        .poll_native_observations(crate::NativeAudioObservationConsumption::DrainAuthor(
            binding,
        ))
        .unwrap();
    let delivery = observed.author.unwrap();
    assert!(
        matches!(delivery.delivery, crate::NativeAudioAuthorDeliveryState::Delivered(ref delivery) if delivery.events.len() == 1)
    );
    assert_eq!(
        worker.output_events.events().len(),
        2,
        "no implicit author Ack or echo drop"
    );
    assert!(observed
        .parameters
        .iter()
        .any(|fact| fact.event_flags == EventFlags::DONT_RECORD.bits()));
    worker.close_native_audio_editor(binding).unwrap();
    drop(worker);
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    reset_audit();
    let mut worker = static_parameter_worker(14);
    assert!(matches!(
        static_open(&mut worker, 1, 0.25).unwrap(),
        crate::NativeAudioEditorOutcome::Opened { .. }
    ));
    assert!(
        worker.author_source_status().parameter_flush_pending(),
        "healthy new request_flush survives the exchange and post-query"
    );
    assert_eq!(worker.output_events.events().len(), 2);
    let receipt = worker.native_binding.unwrap();
    let binding = crate::NativeAudioObjectBindRequest {
        binding: receipt.binding(),
        expected_object: receipt.object(),
    };
    let held = worker
        .poll_native_observations(crate::NativeAudioObservationConsumption::DrainAuthor(
            binding,
        ))
        .unwrap();
    assert!(matches!(
        held.author.unwrap().delivery,
        crate::NativeAudioAuthorDeliveryState::Preserved
    ));
    assert_eq!(
        worker.output_events.events().len(),
        2,
        "scheduled flush cannot implicitly retire a prefix"
    );
    worker.close_native_audio_editor(binding).unwrap();
    drop(worker);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    reset_audit();
}

#[test]
fn clap_idle_static_values_mutation_failures_are_errors_without_fake_opened() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [2, 3, 8, 10, 12] {
        reset_audit();
        let mut worker = static_parameter_worker(mode);
        assert!(static_open(&mut worker, 1, 0.25).is_err(), "mode {mode}");
        assert_eq!(STATIC_FLUSHES.load(AuditOrdering::SeqCst), 1);
        assert_eq!(CREATED.load(AuditOrdering::SeqCst), 0);
        if mode == 2 {
            assert!(worker.output_events.has_refusals());
            assert_eq!(
                worker.shared.parameter_source.snapshot().incomplete,
                0,
                "local rejection need not set shared failure"
            );
            assert!(matches!(
                static_open(&mut worker, 1, 0.25).unwrap(),
                crate::NativeAudioEditorOutcome::Refused(_)
            ));
            assert_eq!(STATIC_FLUSHES.load(AuditOrdering::SeqCst), 1);
        }
        if mode == 12 {
            use super::super::clap_parameter_source::ClapParameterSourceFailure;
            assert_eq!(
                worker.shared.parameter_source.snapshot().incomplete,
                1 << ClapParameterSourceFailure::WrongThread as u8,
                "the real forbidden callback must execute and retain its WrongThread source fact"
            );
            assert!(
                !worker.author_source_status().parameter_flush_pending(),
                "the rejected audio-role callback cannot schedule a flush"
            );
        }
        assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
        drop(worker);
    }
    reset_audit();
}

#[test]
fn clap_idle_static_values_reopen_after_real_configuration_ack_without_signal_entry() {
    use crate::{
        NativeAudioAuthorAcknowledgement, NativeAudioAuthorDeliveryState as Delivery,
        NativeAudioObservationConsumption as Consumption,
    };
    let _serial = SERIAL.lock().unwrap();
    reset_audit();
    let mut worker = static_parameter_worker(16);
    let object = worker.native_object;
    let instance = worker.instance.plugin_handle().as_raw_ptr();
    assert_eq!(object.process_id().get(), std::process::id());
    assert!(worker.configuration_seed.is_none());
    assert_eq!(worker.last_end, None);
    assert!(worker.initial_signal_available);
    assert!(matches!(
        static_open(&mut worker, 1, 0.25).unwrap(),
        crate::NativeAudioEditorOutcome::Opened { .. }
    ));
    let receipt = worker.native_binding.unwrap();
    let source_binding = crate::NativeAudioObjectBindRequest {
        binding: receipt.binding(),
        expected_object: receipt.object(),
    };
    let observed = worker
        .poll_native_observations(Consumption::DrainAuthor(source_binding))
        .unwrap();
    assert_eq!(observed.object, Some(object));
    let Delivery::Delivered(delivery) = observed.author.unwrap().delivery else {
        panic!("the original actual flush prefix must supply a real delivery");
    };
    assert_eq!(delivery.binding, source_binding);
    assert_eq!(delivery.events.len(), 1);
    let expected_seed = 0.25_f64.to_bits().to_le_bytes();
    assert_eq!(
        delivery.configuration.as_ref().unwrap().bytes(),
        &expected_seed
    );
    assert_eq!(STATIC_STATE_SAVES.load(AuditOrdering::SeqCst), 1);
    assert!(worker.native_author_delivery.is_some());
    assert!(
        worker.configuration_seed.is_none(),
        "delivery alone does not accept a seed"
    );
    let installed_binding = crate::NativeAudioObjectBindRequest {
        binding: AudioProcessorBindingId::new(),
        expected_object: object,
    };
    assert!(matches!(
        worker.bind_native_audio_object(installed_binding).unwrap(),
        crate::NativeAudioObjectBindOutcome::Bound(_)
    ));
    let ack = NativeAudioAuthorAcknowledgement {
        source_binding,
        installed_binding,
        revision: delivery.revision,
    };
    let accepted = worker.poll_native_observations(Consumption::AcknowledgeAuthor(ack)).unwrap();
    let author = accepted.author.unwrap();
    assert!(
        matches!(author.delivery, Delivery::Acknowledged { acknowledgement, request_cohort: None } if acknowledgement == ack)
    );
    assert!(author.source.closed_empty());
    assert!(worker.native_author_delivery.is_none());
    assert_eq!(
        worker.configuration_seed.as_deref(),
        Some(expected_seed.as_slice())
    );
    assert_eq!(
        worker.last_end, None,
        "Ack does not manufacture signal entry"
    );
    assert!(worker.initial_signal_available);
    assert!(worker.processor.is_some());
    assert!(worker
        .instance
        .access_shared_handler(|host| host.services.active.load(Ordering::Acquire)));
    assert_eq!(worker.instance.plugin_handle().as_raw_ptr(), instance);
    assert_eq!(worker.native_object, object);
    assert_eq!(
        worker.close_native_audio_editor(installed_binding).unwrap(),
        crate::NativeAudioEditorOutcome::Closed { binding: installed_binding }
    );
    assert!(worker.author_source_status().closed_empty());
    assert_eq!(
        worker.configuration_seed.as_deref(),
        Some(expected_seed.as_slice())
    );
    assert!(
        matches!(static_open(&mut worker, 1, 0.25).unwrap(), crate::NativeAudioEditorOutcome::Opened { binding, .. } if binding == installed_binding)
    );
    assert_eq!(
        STATIC_FLUSHES.load(AuditOrdering::SeqCst),
        2,
        "reopen executes a real active exchange"
    );
    assert_eq!(worker.native_object, object);
    assert_eq!(worker.instance.plugin_handle().as_raw_ptr(), instance);
    assert_eq!(worker.last_end, None);
    assert!(worker.initial_signal_available);
    assert!(worker.author_source_status().closed_empty());
    assert_eq!(
        worker.configuration_seed.as_deref(),
        Some(expected_seed.as_slice()),
        "accepted future-entry seed is retained without rewrite"
    );
    let handle = worker.instance.plugin_handle();
    assert_eq!(
        handle
            .get_extension::<PluginParams>()
            .unwrap()
            .get_value(&handle, ClapId::new(900))
            .unwrap()
            .to_bits(),
        0.25_f64.to_bits()
    );
    assert_eq!(STATIC_STATE_SAVES.load(AuditOrdering::SeqCst), 1);
    assert_eq!(STATIC_STATE_LOADS.load(AuditOrdering::SeqCst), 0);
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
    assert_eq!(RESETS.load(AuditOrdering::SeqCst), 0);
    worker.close_native_audio_editor(installed_binding).unwrap();
    drop(worker);
    assert_eq!(CREATED.load(AuditOrdering::SeqCst), 2);
    assert_eq!(DESTROYED.load(AuditOrdering::SeqCst), 2);
    assert_eq!(WRONG_THREAD.load(AuditOrdering::SeqCst), 0);
    assert_eq!(PROCESSES.load(AuditOrdering::SeqCst), 0);
    assert_eq!(RESETS.load(AuditOrdering::SeqCst), 0);
    reset_audit();
}
