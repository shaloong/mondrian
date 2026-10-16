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

struct GuiPlugin;
struct GuiShared<'a> {
    host: HostSharedHandle<'a>,
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
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginGui>()
            .register::<PluginTimer>();
    }
}
impl DefaultPluginFactory for GuiPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("org.mondrian.test.native-gui", "Owned native GUI")
    }
    fn new_shared(host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        let shared = GuiShared { host };
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
        if count < 200 {
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
    let layout =
        super::super::protocol::SharedBlockLayout::new(16, 2, 0, 0).expect("actual shared layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut aligned);
    for bytes in shared[..8 * 2 * 4].chunks_exact_mut(4) {
        bytes.copy_from_slice(&0.125_f32.to_le_bytes());
    }
    let aux = AudioProcessorAuxiliaryInputContract::default();
    let context = super::super::worker::WorkerProcessContext {
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
    let layout = super::super::protocol::SharedBlockLayout::new(16, 2, 0, 0).expect("layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut aligned);
    let auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
    let context = super::super::worker::WorkerProcessContext {
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
    let layout =
        super::super::protocol::SharedBlockLayout::new(16, 2, 0, 0).expect("real mapped layout");
    let mut aligned = vec![0_u64; layout.total_bytes / 8];
    let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut aligned);
    for bytes in shared[..8 * 2 * 4].chunks_exact_mut(4) {
        bytes.copy_from_slice(&0.125_f32.to_le_bytes());
    }
    let aux = AudioProcessorAuxiliaryInputContract::default();
    let context = super::super::worker::WorkerProcessContext {
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
