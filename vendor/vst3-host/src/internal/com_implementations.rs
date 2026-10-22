//! Internal COM interface implementations for VST3

use crate::midi::{PluginEvent, PluginEventData, MAX_EVENT_PAYLOAD_BYTES, MAX_EVENT_TEXT_UNITS};
use crate::plugin::StateContext;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};

/// Only these homogeneous restart kinds have a retained-prefix consumer.
/// Information witnesses identify source ownership, not metadata admission.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RestartSourceParameterKind {
    Values,
    UnchangedInfo,
}
impl RestartSourceParameterKind {
    fn from_flags(flags: i32) -> Option<Self> {
        match flags {
            4 => Some(Self::Values),
            16 => Some(Self::UnchangedInfo),
            _ => None,
        }
    }
    fn flags(self) -> i32 {
        match self {
            Self::Values => 4,
            Self::UnchangedInfo => 16,
        }
    }
}

pub(super) fn visit_event_payload(
    event: &PluginEvent,
    visitor: &mut dyn crate::host_storage::HostStorageVisitor,
) -> Result<(), crate::host_storage::HostStorageError> {
    use crate::host_storage::HostStorageRef;
    match &event.data {
        PluginEventData::Data { bytes, .. } => visitor.visit(HostStorageRef::vector(bytes)?)?,
        PluginEventData::NoteExpressionText { text, .. }
        | PluginEventData::Chord { text, .. }
        | PluginEventData::Scale { text, .. } => visitor.visit(HostStorageRef::vector(text)?)?,
        // The remaining variants contain no heap owners. Any new heap-bearing
        // variant must be added here before it can participate in admission.
        PluginEventData::NoteOn { .. }
        | PluginEventData::NoteOff { .. }
        | PluginEventData::PolyPressure { .. }
        | PluginEventData::NoteExpressionValue { .. }
        | PluginEventData::NoteExpressionIntValue { .. }
        | PluginEventData::LegacyMidiCcOut { .. } => {}
    }
    Ok(())
}

fn visit_notification_payload(
    notification: &crate::plugin::HostNotification,
    visitor: &mut dyn crate::host_storage::HostStorageVisitor,
) -> Result<(), crate::host_storage::HostStorageError> {
    use crate::host_storage::HostStorageRef;
    use crate::plugin::HostNotification;
    match notification {
        HostNotification::OpenEditorRequested { name: Some(name) }
        | HostNotification::ProgressStarted {
            description: Some(name),
            ..
        } => {
            visitor.visit(HostStorageRef::string(name)?)?;
        }
        HostNotification::ContextMenuRequested { items, .. } => {
            visitor.visit(HostStorageRef::vector(items)?)?;
            for item in items {
                visitor.visit(HostStorageRef::string(&item.name)?)?;
            }
        }
        HostNotification::OpenEditorRequested { name: None }
        | HostNotification::ProgressStarted {
            description: None, ..
        }
        | HostNotification::UnitSelectionChanged { .. }
        | HostNotification::ProgramListChanged { .. }
        | HostNotification::UnitByBusChanged
        | HostNotification::ProgressUpdated { .. }
        | HostNotification::ProgressFinished { .. } => {}
    }
    Ok(())
}

// Host Application implementation.
//
// Many plugins (u-he, Waves, ...) query the context passed to `IComponent::initialize`
// for `IHostApplication` and dereference it. Passing a null context makes them crash.
// Providing a real host-application object that at least answers `getName` lets them
// initialize. `createInstance` below also vends the host-created objects they ask for
// (IMessage/IAttributeList), used to pass data between a plugin's component and controller.
struct ProgressState {
    notifications: Vec<crate::plugin::HostNotification>,
    active: Vec<u64>,
    next_id: u64,
}

impl Default for ProgressState {
    fn default() -> Self {
        Self {
            notifications: Vec::with_capacity(MAX_HOST_NOTIFICATIONS),
            active: Vec::with_capacity(MAX_HOST_NOTIFICATIONS),
            next_id: 1,
        }
    }
}

pub struct HostApplication {
    interfaces: crate::PluginInterfaceSupport,
    progress: Mutex<ProgressState>,
    data_exchange: Arc<super::data_exchange::DataExchangeState>,
    storage: super::host_storage::RetainedComStorage,
}

impl Default for HostApplication {
    fn default() -> Self {
        Self {
            interfaces: crate::PluginInterfaceSupport::default(),
            progress: Mutex::new(ProgressState::default()),
            data_exchange: super::data_exchange::DataExchangeState::new(),
            storage: super::host_storage::RetainedComStorage::new(),
        }
    }
}

impl HostApplication {
    pub(crate) fn consumes_plugin_interface(&self, interface: crate::PluginInterface) -> bool {
        self.interfaces.supports(interface)
    }
    pub(crate) fn require_plugin_interface(
        &self,
        interface: crate::PluginInterface,
    ) -> crate::Result<()> {
        if self.consumes_plugin_interface(interface) {
            Ok(())
        } else {
            Err(crate::Error::InterfaceNotConsumed(interface))
        }
    }
    pub(crate) fn visit_progress_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        let state = self
            .progress
            .lock()
            .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
        visitor.visit(crate::host_storage::HostStorageRef::vector(&state.active)?)?;
        visitor.visit(crate::host_storage::HostStorageRef::vector(
            &state.notifications,
        )?)?;
        for value in &state.notifications {
            visit_notification_payload(value, visitor)?;
        }
        Ok(())
    }

    pub(crate) fn visit_data_exchange_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        visitor.visit(crate::host_storage::HostStorageRef::arc(
            &self.data_exchange,
        ))?;
        self.data_exchange.visit_owned_storage(visitor)
    }

    pub(crate) fn register_memory_stream(
        &self,
        stream: &ComWrapper<MemoryStream>,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        self.storage.register_memory_stream(stream)
    }

    pub(crate) fn register_capture_stream(
        &self,
        stream: &ComWrapper<super::bounded_configuration_stream::ConfigurationCaptureStream>,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        self.storage.register_capture_stream(stream)
    }

    pub(crate) fn visit_retained_com_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        self.storage.visit_owned_storage(visitor)
    }
    pub fn take_progress_notifications(&self) -> Vec<crate::plugin::HostNotification> {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.notifications.drain(..).collect()
    }

    pub fn configure_data_exchange(
        &self,
        processor: *mut IAudioProcessor,
        receiver: Option<ComPtr<IDataExchangeReceiver>>,
    ) {
        self.data_exchange.configure(processor, receiver);
    }

    pub fn set_data_exchange_active(&self, active: bool) {
        self.data_exchange.set_active(active);
    }

    pub fn enter_data_exchange_process(&self) {
        self.data_exchange.enter_process();
    }

    pub fn leave_data_exchange_process(&self) {
        self.data_exchange.leave_process();
    }

    pub fn service_main_thread_data_exchange(&self) {
        self.data_exchange.service_main_thread();
    }

    pub fn flush_data_exchange(&self) {
        self.data_exchange.flush();
    }

    pub fn take_data_exchange_blocks(&self) -> Vec<crate::plugin::DataExchangeBlock> {
        self.data_exchange.take_blocks()
    }

    pub fn shutdown_data_exchange(&self) {
        self.data_exchange.shutdown();
    }
}

impl Class for HostApplication {
    // The standard SDK host context implements both IHostApplication and
    // IPlugInterfaceSupport; plugins query the context for either.
    type Interfaces = (
        IHostApplication,
        IPlugInterfaceSupport,
        IProgress,
        IDataExchangeHandler,
    );
}

impl IPlugInterfaceSupportTrait for HostApplication {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        if iid.is_null() {
            return kInvalidArgument;
        }
        // This interface describes plug-in-side interfaces the host knows how to consume.
        // Host callbacks such as IComponentHandler are deliberately absent: advertising
        // those reverses the direction of the contract and makes plug-ins enable features
        // whose corresponding plug-in interface the host may never query.
        let bytes = std::slice::from_raw_parts(iid as *const u8, 16);
        use crate::PluginInterface::*;
        let known = [
            (&IComponent::IID, Component),
            (&IAudioProcessor::IID, AudioProcessor),
            (&IEditController::IID, EditController),
            (&IConnectionPoint::IID, ConnectionPoint),
            (&IMidiMapping::IID, MidiMapping),
            (&IUnitInfo::IID, UnitInfo),
            (&IProgramListData::IID, ProgramListData),
            (&IUnitData::IID, UnitData),
            (&IEditControllerHostEditing::IID, EditControllerHostEditing),
            (&IMidiLearn::IID, MidiLearn),
            (&IAutomationState::IID, AutomationState),
            (&INoteExpressionController::IID, NoteExpressionController),
            (
                &IPlugViewContentScaleSupport::IID,
                PlugViewContentScaleSupport,
            ),
            (&IPlugView::IID, PlugView),
            (
                &IProcessContextRequirements::IID,
                ProcessContextRequirements,
            ),
            (&IPrefetchableSupport::IID, PrefetchableSupport),
            (&IRemapParamID::IID, RemapParamId),
            (&IDataExchangeReceiver::IID, DataExchangeReceiver),
        ];
        if known
            .iter()
            .any(|(iid, interface)| bytes == &iid[..] && self.interfaces.supports(*interface))
        {
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

impl IDataExchangeHandlerTrait for HostApplication {
    unsafe fn openQueue(
        &self,
        processor: *mut IAudioProcessor,
        block_size: u32,
        num_blocks: u32,
        alignment: u32,
        user_context_id: u32,
        out_id: *mut u32,
    ) -> tresult {
        self.data_exchange.open_queue(
            processor,
            block_size,
            num_blocks,
            alignment,
            user_context_id,
            out_id,
        )
    }

    unsafe fn closeQueue(&self, queue_id: u32) -> tresult {
        self.data_exchange.close_queue(queue_id)
    }

    unsafe fn lockBlock(
        &self,
        queue_id: u32,
        block: *mut vst3::Steinberg::Vst::DataExchangeBlock,
    ) -> tresult {
        self.data_exchange.lock_block(queue_id, block)
    }

    unsafe fn freeBlock(&self, queue_id: u32, block_id: u32, send_to_controller: TBool) -> tresult {
        self.data_exchange
            .free_block(queue_id, block_id, send_to_controller != 0)
    }
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kResultFalse;
        }
        let dst = &mut *name;
        let mut i = 0;
        for ch in "vst3-host".encode_utf16() {
            if i + 1 >= dst.len() {
                break;
            }
            dst[i] = ch;
            i += 1;
        }
        dst[i] = 0;
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut std::ffi::c_void,
    ) -> tresult {
        // Vend the host-created objects plugins ask for (the SDK's HostApplication does
        // this): IMessage and IAttributeList, used to pass data between a plugin's
        // component and controller halves. Anything else fails cleanly.
        if obj.is_null() || cid.is_null() || iid.is_null() {
            return kInvalidArgument;
        }
        *obj = ptr::null_mut();

        // IMessage and IAttributeList use their interface UID as their host-created class UID.
        // Honour both inputs: returning an IMessage pointer for an unrelated requested IID is
        // a COM type confusion bug even when the class id itself is valid.
        let cid_bytes = std::slice::from_raw_parts(cid as *const u8, 16);
        let iid_bytes = std::slice::from_raw_parts(iid as *const u8, 16);
        let matches =
            |expected: &[u8; 16]| cid_bytes == &expected[..] && iid_bytes == &expected[..];

        if matches(&IMessage::IID) {
            let message = create_host_message();
            if self.storage.register_message(&message).is_err() {
                return kOutOfMemory;
            }
            if let Some(p) = message.to_com_ptr::<IMessage>() {
                *obj = p.into_raw() as *mut std::ffi::c_void;
                return kResultTrue;
            }
        } else if matches(&IAttributeList::IID) {
            let attributes = create_host_attribute_list();
            if self.storage.register_attributes(&attributes).is_err() {
                return kOutOfMemory;
            }
            if let Some(p) = attributes.to_com_ptr::<IAttributeList>() {
                *obj = p.into_raw() as *mut std::ffi::c_void;
                return kResultTrue;
            }
        }
        kNoInterface
    }
}

impl IProgressTrait for HostApplication {
    unsafe fn start(
        &self,
        r#type: IProgress_::ProgressType,
        optional_description: *const tchar,
        out_id: *mut IProgress_::ID,
    ) -> tresult {
        if out_id.is_null() {
            return kInvalidArgument;
        }
        let description = if optional_description.is_null() {
            None
        } else {
            const MAX_PROGRESS_DESCRIPTION_UNITS: usize = 1024;
            let mut units = Vec::with_capacity(64);
            for index in 0..MAX_PROGRESS_DESCRIPTION_UNITS {
                let unit = *optional_description.add(index);
                if unit == 0 {
                    break;
                }
                units.push(unit);
            }
            if units.len() == MAX_PROGRESS_DESCRIPTION_UNITS {
                return kInvalidArgument;
            }
            Some(String::from_utf16_lossy(&units))
        };
        let kind = match r#type {
            IProgress_::ProgressType_::AsyncStateRestoration => {
                crate::plugin::ProgressKind::AsyncStateRestoration
            }
            IProgress_::ProgressType_::UIBackgroundTask => {
                crate::plugin::ProgressKind::UiBackgroundTask
            }
            other => crate::plugin::ProgressKind::Other(other),
        };

        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.notifications.len() >= MAX_HOST_NOTIFICATIONS
            || state.active.len() >= MAX_HOST_NOTIFICATIONS
        {
            return kResultFalse;
        }
        let id = state.next_id;
        let Some(next_id) = id.checked_add(1) else {
            return kResultFalse;
        };
        state.next_id = next_id;
        // Nonwrapping monotonic ids make append already sorted.
        state.active.push(id);
        state
            .notifications
            .push(crate::plugin::HostNotification::ProgressStarted {
                id,
                kind,
                description,
            });
        *out_id = id;
        kResultOk
    }

    unsafe fn update(&self, id: IProgress_::ID, norm_value: ParamValue) -> tresult {
        let Some(value) = crate::plugin::ProgressValue::new(norm_value) else {
            return kInvalidArgument;
        };
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.active.binary_search(&id).is_err()
            || state.notifications.len() >= MAX_HOST_NOTIFICATIONS
        {
            return kResultFalse;
        }
        state
            .notifications
            .push(crate::plugin::HostNotification::ProgressUpdated { id, value });
        kResultOk
    }

    unsafe fn finish(&self, id: IProgress_::ID) -> tresult {
        let mut state = self
            .progress
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.active.binary_search(&id).is_err()
            || state.notifications.len() >= MAX_HOST_NOTIFICATIONS
        {
            return kResultFalse;
        }
        if let Ok(index) = state.active.binary_search(&id) {
            state.active.remove(index);
        }
        state
            .notifications
            .push(crate::plugin::HostNotification::ProgressFinished { id });
        kResultOk
    }
}

/// Create a host-application context to pass to `IComponent::initialize`.
pub fn create_host_application() -> ComWrapper<HostApplication> {
    create_host_application_with_interfaces(crate::PluginInterfaceSupport::default())
}

/// Retain one immutable declaration before native factory/component initialization.
pub fn create_host_application_with_interfaces(
    interfaces: crate::PluginInterfaceSupport,
) -> ComWrapper<HostApplication> {
    ComWrapper::new(HostApplication {
        interfaces,
        ..HostApplication::default()
    })
}

/// Log the first off-thread drop, then every `DROP_LOG_INTERVAL`-th one. A plugin that
/// notifies from its processor thread does so per block, so logging unconditionally would
/// emit thousands of lines a second.
const DROP_LOG_INTERVAL: u64 = 256;

/// Host-side connection point which prevents processor-thread messages from invoking the
/// controller directly.
///
/// # Which thread is the "UI thread"
///
/// The gate is `ConnectionPair::connect`'s calling thread — in practice the thread that
/// loaded the plugin, because the pair is built during load. That matches this library's
/// documented threading model: `load_plugin`, `open_editor` and every controller call belong
/// on the host's GUI thread (see `docs/explanation/threading.md`). Load on a worker thread
/// and the gate follows that worker, not your GUI thread.
///
/// # Dropped messages
///
/// `notify` from any other thread is refused with `kResultFalse` and the message is
/// **dropped**, matching the SDK reference host's `ConnectionProxy`, which also has nothing
/// to hand a message to off the UI thread. Plugins that push meter/waveform updates from
/// `process()` therefore lose those updates; their editors typically fall back to polling.
/// The drop is counted ([`ConnectionPair::dropped_message_count`]) and logged rate-limited so
/// it is diagnosable instead of silent. Queueing the message onto the UI thread would need an
/// owned message copy plus a pump the host is not required to run, and is not implemented.
pub struct ConnectionProxy {
    destination: Mutex<Option<ComPtr<IConnectionPoint>>>,
    control_thread: ThreadId,
    /// Messages refused because `notify` came from a thread other than `control_thread`.
    dropped: AtomicU64,
    /// Which direction this proxy carries, for the drop log ("component→controller").
    direction: &'static str,
}

impl ConnectionProxy {
    fn new(
        destination: ComPtr<IConnectionPoint>,
        control_thread: ThreadId,
        direction: &'static str,
    ) -> Self {
        Self {
            destination: Mutex::new(Some(destination)),
            control_thread,
            dropped: AtomicU64::new(0),
            direction,
        }
    }

    fn clear(&self) {
        self.destination
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
    }

    /// Count an off-thread `notify` and log the first one plus every
    /// [`DROP_LOG_INTERVAL`]-th one after it.
    fn record_off_thread_drop(&self) {
        let count = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count % DROP_LOG_INTERVAL == 0 {
            log::warn!(
                "ConnectionProxy ({}): dropped an off-thread IConnectionPoint::notify \
                 ({count} so far). VST3 requires component↔controller messages on the UI \
                 thread; the plugin sent this one from another thread (usually its processor \
                 thread), so meter/waveform-style updates will not reach its editor.",
                self.direction
            );
        }
    }

    /// How many `notify` calls this proxy has refused for arriving off the UI thread.
    fn dropped_message_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Class for ConnectionProxy {
    type Interfaces = (IConnectionPoint,);
}

impl IConnectionPointTrait for ConnectionProxy {
    unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
        // The endpoints are fixed by `ConnectionPair`; accepting an arbitrary replacement
        // would let a plugin bypass the thread gate.
        kResultFalse
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultFalse
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        if message.is_null() {
            return kResultFalse;
        }
        if thread::current().id() != self.control_thread {
            self.record_off_thread_drop();
            return kResultFalse;
        }

        // Clone under the lock, then release it before calling plugin code. `notify` is allowed
        // to re-enter the host (including disconnect/teardown).
        let destination = self
            .destination
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        match destination {
            Some(destination) => destination.notify(message),
            None => kResultFalse,
        }
    }
}

/// The two proxy connections between a separate component and edit controller.
///
/// Both directions are gated to the thread that called [`Self::connect`] — see
/// [`ConnectionProxy`] for what that means and what is dropped.
pub struct ConnectionPair {
    component: ComPtr<IConnectionPoint>,
    controller: ComPtr<IConnectionPoint>,
    component_to_controller: ComWrapper<ConnectionProxy>,
    controller_to_component: ComWrapper<ConnectionProxy>,
    component_connected: AtomicBool,
    controller_connected: AtomicBool,
}

impl ConnectionPair {
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        visitor.visit(crate::host_storage::HostStorageRef::com(
            &self.component_to_controller,
        ))?;
        visitor.visit(crate::host_storage::HostStorageRef::com(
            &self.controller_to_component,
        ))
    }

    /// Connect both directions, rolling the first direction back if the second fails.
    pub unsafe fn connect(
        component: ComPtr<IConnectionPoint>,
        controller: ComPtr<IConnectionPoint>,
    ) -> Option<Self> {
        let control_thread = thread::current().id();
        let component_to_controller = ComWrapper::new(ConnectionProxy::new(
            controller.clone(),
            control_thread,
            "component→controller",
        ));
        let controller_to_component = ComWrapper::new(ConnectionProxy::new(
            component.clone(),
            control_thread,
            "controller→component",
        ));
        let component_proxy = component_to_controller.to_com_ptr::<IConnectionPoint>()?;
        let controller_proxy = controller_to_component.to_com_ptr::<IConnectionPoint>()?;

        let component_result = component.connect(component_proxy.as_ptr());
        if component_result != kResultOk && component_result != kResultTrue {
            log::warn!("component refused host connection proxy: {component_result:#x}");
            return None;
        }

        let controller_result = controller.connect(controller_proxy.as_ptr());
        if controller_result != kResultOk && controller_result != kResultTrue {
            component.disconnect(component_proxy.as_ptr());
            log::warn!(
                "controller refused host connection proxy; rolled component connection back: \
                 {controller_result:#x}"
            );
            return None;
        }

        Some(Self {
            component,
            controller,
            component_to_controller,
            controller_to_component,
            component_connected: AtomicBool::new(true),
            controller_connected: AtomicBool::new(true),
        })
    }

    /// Total `notify` calls both directions have refused because they arrived off the UI
    /// thread (the thread [`Self::connect`] ran on).
    ///
    /// Non-zero means the plugin is trying to push component↔controller messages from another
    /// thread and those messages are being dropped, exactly as the SDK reference host drops
    /// them. Useful for answering "why is this plugin's meter frozen?" without a debugger.
    pub fn dropped_message_count(&self) -> u64 {
        self.component_to_controller
            .dropped_message_count()
            .saturating_add(self.controller_to_component.dropped_message_count())
    }

    /// Disconnect both plugin endpoints. Safe to call more than once.
    pub unsafe fn disconnect(&self) {
        if self.component_connected.swap(false, Ordering::AcqRel) {
            if let Some(proxy) = self
                .component_to_controller
                .as_com_ref::<IConnectionPoint>()
            {
                self.component.disconnect(proxy.as_ptr());
            }
        }
        if self.controller_connected.swap(false, Ordering::AcqRel) {
            if let Some(proxy) = self
                .controller_to_component
                .as_com_ref::<IConnectionPoint>()
            {
                self.controller.disconnect(proxy.as_ptr());
            }
        }
        self.component_to_controller.clear();
        self.controller_to_component.clear();
    }
}

impl Drop for ConnectionPair {
    fn drop(&mut self) {
        // A closing summary, so the drops are visible even to a host that never polls the
        // counter. The per-drop log is rate-limited and easy to miss in a long session.
        let dropped = self.dropped_message_count();
        if dropped > 0 {
            log::warn!(
                "ConnectionPair: {dropped} component↔controller message(s) were dropped for \
                 arriving off the UI thread over this plugin's lifetime"
            );
        }
        unsafe {
            self.disconnect();
        }
    }
}

// A host-side IAttributeList: a typed key/value bag plugins use (via the host's
// createInstance) to pass data between their component and controller halves.
#[derive(Debug, Clone, PartialEq)]
enum AttrValue {
    Int(i64),
    Float(f64),
    /// UTF-16 (TChar) string, not null-terminated.
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

/// Host implementation of `IAttributeList`.
#[derive(Default)]
pub struct HostAttributeList {
    attrs: Mutex<Vec<(String, AttrValue)>>,
}

impl HostAttributeList {
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::{HostStorageError, HostStorageRef};
        let attrs = self
            .attrs
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        visitor.visit(HostStorageRef::vector(&attrs)?)?;
        for (key, value) in attrs.iter() {
            visitor.visit(HostStorageRef::string(key)?)?;
            match value {
                AttrValue::Str(value) => visitor.visit(HostStorageRef::vector(value)?)?,
                AttrValue::Bin(value) => visitor.visit(HostStorageRef::vector(value)?)?,
                AttrValue::Int(_) | AttrValue::Float(_) => {}
            }
        }
        Ok(())
    }
    pub fn new() -> Self {
        Self::default()
    }

    // Safe inner API (also the unit-test surface).
    fn put(&self, key: String, value: AttrValue) {
        if let Ok(mut m) = self.attrs.lock() {
            match m.binary_search_by(|(stored, _)| stored.as_str().cmp(key.as_str())) {
                Ok(index) => m[index].1 = value,
                Err(index) => m.insert(index, (key, value)),
            }
        }
    }
    fn get_value(&self, key: &str) -> Option<AttrValue> {
        self.attrs.lock().ok().and_then(|m| {
            m.binary_search_by(|(stored, _)| stored.as_str().cmp(key))
                .ok()
                .map(|index| m[index].1.clone())
        })
    }
}

/// Decode an `AttrID` (a C string) into an owned key.
unsafe fn attr_key(id: *const std::os::raw::c_char) -> String {
    if id.is_null() {
        return String::new();
    }
    CStr::from_ptr(id).to_string_lossy().into_owned()
}

impl Class for HostAttributeList {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for HostAttributeList {
    unsafe fn setInt(&self, id: *const std::os::raw::c_char, value: i64) -> tresult {
        self.put(attr_key(id), AttrValue::Int(value));
        kResultOk
    }
    unsafe fn getInt(&self, id: *const std::os::raw::c_char, value: *mut i64) -> tresult {
        match self.get_value(&attr_key(id)) {
            Some(AttrValue::Int(v)) if !value.is_null() => {
                *value = v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setFloat(&self, id: *const std::os::raw::c_char, value: f64) -> tresult {
        self.put(attr_key(id), AttrValue::Float(value));
        kResultOk
    }
    unsafe fn getFloat(&self, id: *const std::os::raw::c_char, value: *mut f64) -> tresult {
        match self.get_value(&attr_key(id)) {
            Some(AttrValue::Float(v)) if !value.is_null() => {
                *value = v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setString(&self, id: *const std::os::raw::c_char, string: *const u16) -> tresult {
        if string.is_null() {
            return kResultFalse;
        }
        let mut buf = Vec::new();
        let mut p = string;
        while *p != 0 {
            buf.push(*p);
            p = p.add(1);
        }
        self.put(attr_key(id), AttrValue::Str(buf));
        kResultOk
    }
    unsafe fn getString(
        &self,
        id: *const std::os::raw::c_char,
        string: *mut u16,
        size_in_bytes: u32,
    ) -> tresult {
        match self.get_value(&attr_key(id)) {
            Some(AttrValue::Str(v)) if !string.is_null() => {
                // Copy up to capacity-1 chars, then null-terminate.
                let cap_chars = (size_in_bytes as usize / 2).saturating_sub(1);
                let n = v.len().min(cap_chars);
                for (i, &ch) in v.iter().take(n).enumerate() {
                    *string.add(i) = ch;
                }
                *string.add(n) = 0;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setBinary(
        &self,
        id: *const std::os::raw::c_char,
        data: *const std::ffi::c_void,
        size_in_bytes: u32,
    ) -> tresult {
        if data.is_null() {
            return kResultFalse;
        }
        let bytes = std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize).to_vec();
        self.put(attr_key(id), AttrValue::Bin(bytes));
        kResultOk
    }
    unsafe fn getBinary(
        &self,
        id: *const std::os::raw::c_char,
        data: *mut *const std::ffi::c_void,
        size_in_bytes: *mut u32,
    ) -> tresult {
        // Note: returns a pointer into the stored buffer; valid until the entry is
        // replaced. VST3 plugins read it synchronously during init, which is safe here.
        if data.is_null() || size_in_bytes.is_null() {
            return kResultFalse;
        }
        if let Ok(m) = self.attrs.lock() {
            let key = attr_key(id);
            if let Ok(index) = m.binary_search_by(|(stored, _)| stored.as_str().cmp(key.as_str())) {
                let AttrValue::Bin(v) = &m[index].1 else {
                    return kResultFalse;
                };
                *data = v.as_ptr() as *const std::ffi::c_void;
                *size_in_bytes = v.len() as u32;
                return kResultOk;
            }
        }
        kResultFalse
    }
}

/// Create a host attribute list.
pub fn create_host_attribute_list() -> ComWrapper<HostAttributeList> {
    ComWrapper::new(HostAttributeList::new())
}

/// Host implementation of `IMessage` (an id + an attribute list), used for
/// component<->controller communication that plugins allocate via the host.
pub struct HostMessage {
    id: Mutex<Option<std::ffi::CString>>,
    attributes: ComWrapper<HostAttributeList>,
}

impl Default for HostMessage {
    fn default() -> Self {
        Self {
            id: Mutex::new(None),
            attributes: create_host_attribute_list(),
        }
    }
}

impl HostMessage {
    pub(crate) fn host_attributes(&self) -> &ComWrapper<HostAttributeList> {
        &self.attributes
    }

    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::{HostStorageError, HostStorageRef};
        let id = self
            .id
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        if let Some(id) = id.as_ref() {
            visitor.visit(HostStorageRef::c_string(id)?)?;
        }
        visitor.visit(HostStorageRef::com(&self.attributes))?;
        self.attributes.visit_owned_storage(visitor)
    }
    pub fn new() -> Self {
        Self::default()
    }
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        // Pointer to the stored id (valid until replaced); null if unset.
        if let Ok(g) = self.id.lock() {
            if let Some(ref s) = *g {
                return s.as_ptr();
            }
        }
        ptr::null()
    }
    unsafe fn setMessageID(&self, id: FIDString) {
        if id.is_null() {
            return;
        }
        let owned = CStr::from_ptr(id).to_owned();
        if let Ok(mut g) = self.id.lock() {
            *g = Some(owned);
        }
    }
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Borrowed pointer to the message's own attribute list (kept alive by `self`).
        self.attributes
            .to_com_ptr::<IAttributeList>()
            .map(|p| p.as_ptr())
            .unwrap_or(ptr::null_mut())
    }
}

/// Create a host message.
pub fn create_host_message() -> ComWrapper<HostMessage> {
    ComWrapper::new(HostMessage::new())
}

// Host-side in-memory `IBStream`. Plugins serialize their state into a stream the host
// provides (`IComponent::getState`) and restore from one the host fills
// (`IComponent::setState`). This backs both with a growable byte buffer plus a cursor.
struct MemBuf {
    data: Vec<u8>,
    pos: usize,
}

/// Cap on how large a plugin can grow a host-provided state stream (and how far it may seek
/// into one). The cursor and the write length both come from the plugin, and the buffer grows
/// to `cursor + length`: without a bound, a wild seek turns the following write into a
/// multi-gigabyte `Vec::resize` — a capacity-overflow panic inside an `extern "system"` vtable
/// thunk, which aborts the process rather than unwinding. 64 MiB is far above any real plugin
/// state (the largest sample-library presets are a few MiB). Mirrors the `MAX_*` caps on every
/// other host-side buffer; over-cap operations fail with a result code instead of allocating.
pub const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;
const MAX_STREAM_FILENAME_UNITS: usize = 127;

/// The purpose of a host-provided stream, published under the `StateType` key of
/// `IStreamAttributes::getAttributes` so a plugin can tell a project load from a preset load.
///
/// Every variant maps to a string the SDK defines in `Steinberg::Vst::StateType`
/// (`vstpresetkeys.h`) — the `state_type_values_match_the_sdk_constants` test pins each one
/// against `vst3`'s generated constants so a typo cannot ship. `kDefault` is deliberately
/// absent: it means "restored from a preset *marked as default*, or the host wants to store a
/// default state of the plug-in", which is not something this host ever asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStateType {
    /// `StateType::kProject` — state saved with, or restored from, a host project.
    Project,
    /// `StateType::kTrackPreset` — state saved to, or restored from, a standalone preset
    /// (a `.vstpreset` file).
    TrackPreset,
}

impl StreamStateType {
    fn attribute_value(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::TrackPreset => "TrackPreset",
        }
    }
}

impl From<&StateContext> for StreamStateType {
    fn from(context: &StateContext) -> Self {
        match context {
            StateContext::Project => Self::Project,
            StateContext::Preset { .. } => Self::TrackPreset,
        }
    }
}

/// What the host publishes about a stream it hands a plugin: the `IStreamAttributes` entries
/// and the name `IStreamAttributes::getFileName` reports.
///
/// A struct rather than a row of `Option<&str>` parameters — the two string fields mean very
/// different things (a bare file name versus a full path) and would otherwise be trivially
/// swappable at a call site.
#[derive(Debug, Clone, Copy, Default)]
struct StreamMetadata<'a> {
    /// `PresetAttributes::kStateType`. `None` leaves the attribute off entirely, which the
    /// SDK's `Helpers::isProjectState` reports as "the host does not implement this".
    state_type: Option<StreamStateType>,
    /// What `IStreamAttributes::getFileName` returns — "filename (without file extension)".
    file_name: Option<&'a str>,
    /// `PresetAttributes::kFilePathStringType` — "full file path string (if available) where
    /// the preset comes from".
    file_path: Option<&'a str>,
}

impl<'a> StreamMetadata<'a> {
    /// Metadata that says only what kind of state the stream carries.
    fn new(state_type: StreamStateType) -> Self {
        Self {
            state_type: Some(state_type),
            file_name: None,
            file_path: None,
        }
    }

    /// The metadata a `setState` stream should carry for `context`, including the source
    /// file's path and stem when the context names one.
    fn for_state_context(context: &'a StateContext) -> Self {
        let path = context.file_path();
        Self {
            state_type: Some(StreamStateType::from(context)),
            file_name: path.and_then(|p| p.file_stem()).and_then(|s| s.to_str()),
            file_path: path.and_then(|p| p.to_str()),
        }
    }
}

#[cfg(test)]
mod stream_state_type_tests {
    use super::*;

    /// Read one of `vst3`'s `CString` constants (a NUL-terminated `*const c_char`).
    fn sdk_constant(value: *const std::os::raw::c_char) -> &'static str {
        // SAFETY: the argument is a `'static` NUL-terminated literal generated by `vst3`.
        unsafe { CStr::from_ptr(value) }
            .to_str()
            .expect("SDK state-type constants are ASCII")
    }

    #[test]
    fn state_type_values_match_the_sdk_constants() {
        use vst3::Steinberg::Vst::StateType;

        assert_eq!(
            StreamStateType::Project.attribute_value(),
            sdk_constant(StateType::kProject),
        );
        assert_eq!(
            StreamStateType::TrackPreset.attribute_value(),
            sdk_constant(StateType::kTrackPreset),
        );
        // The one defined value this host does not produce; asserted so that adding it later
        // starts from the SDK spelling rather than a guess.
        assert_eq!(sdk_constant(StateType::kDefault), "Default");
    }

    #[test]
    fn the_attribute_is_published_under_the_sdk_key() {
        assert_eq!(sdk_constant(PresetAttributes::kStateType), "StateType");
        assert_eq!(
            sdk_constant(PresetAttributes::kFilePathStringType),
            "FilePathString"
        );
    }

    /// A project restore and a preset load must not look alike to the plugin: `kProject` is
    /// "restored from a project loading", and everything else is what the SDK's
    /// `Helpers::isProjectState` reports as "coming from a preset".
    #[test]
    fn a_state_context_picks_the_matching_state_type() {
        assert_eq!(
            StreamStateType::from(&StateContext::Project),
            StreamStateType::Project
        );
        assert_eq!(
            StreamStateType::from(&StateContext::preset()),
            StreamStateType::TrackPreset
        );
        assert_eq!(
            StreamStateType::from(&StateContext::preset_from_path("/tmp/Lead.vstpreset")),
            StreamStateType::TrackPreset
        );
    }
}

/// Host implementation of `IBStream` over an in-memory buffer.
pub struct MemoryStream {
    inner: Mutex<MemBuf>,
    file_name: Option<Vec<u16>>,
    attributes: ComWrapper<HostAttributeList>,
}

impl MemoryStream {
    pub(crate) fn host_attributes(&self) -> &ComWrapper<HostAttributeList> {
        &self.attributes
    }

    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::{HostStorageError, HostStorageRef};
        let buffer = self
            .inner
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        visitor.visit(HostStorageRef::vector(&buffer.data)?)?;
        if let Some(name) = self.file_name.as_ref() {
            visitor.visit(HostStorageRef::vector(name)?)?;
        }
        visitor.visit(HostStorageRef::com(&self.attributes))?;
        self.attributes.visit_owned_storage(visitor)
    }
    #[cfg(test)]
    fn new(data: Vec<u8>) -> Self {
        Self::with_metadata(data, StreamMetadata::default())
    }

    fn with_metadata(data: Vec<u8>, metadata: StreamMetadata<'_>) -> Self {
        let attributes = create_host_attribute_list();
        if let Some(state_type) = metadata.state_type {
            attributes.put(
                "StateType".to_string(),
                AttrValue::Str(state_type.attribute_value().encode_utf16().collect()),
            );
        }
        if let Some(file_path) = metadata.file_path {
            attributes.put(
                "FilePathString".to_string(),
                AttrValue::Str(file_path.encode_utf16().collect()),
            );
        }
        let file_name = metadata.file_name.map(|name| {
            name.encode_utf16()
                .take(MAX_STREAM_FILENAME_UNITS)
                .collect()
        });
        Self {
            inner: Mutex::new(MemBuf { data, pos: 0 }),
            file_name,
            attributes,
        }
    }

    /// A copy of everything written to the stream (used after `getState`).
    pub fn to_vec(&self) -> Vec<u8> {
        self.inner
            .lock()
            .map(|b| b.data.clone())
            .unwrap_or_default()
    }

    // Safe inner ops — also the unit-test surface for the read/write/seek logic.

    /// Write `src` at the cursor, zero-filling any gap a prior seek left past the end, and
    /// return the number of bytes written. `None` when the write would grow the buffer past
    /// [`MAX_STREAM_BYTES`] (or overflow `usize`), leaving the stream untouched.
    fn write_at_cursor(&self, src: &[u8]) -> Option<usize> {
        let mut b = self.inner.lock().ok()?;
        let end = b.pos.checked_add(src.len())?;
        if end > MAX_STREAM_BYTES {
            return None;
        }
        if end > b.data.len() {
            b.data.resize(end, 0);
        }
        let pos = b.pos;
        b.data[pos..end].copy_from_slice(src);
        b.pos = end;
        Some(src.len())
    }

    fn read_at_cursor(&self, n: usize) -> Vec<u8> {
        if let Ok(mut b) = self.inner.lock() {
            let start = b.pos.min(b.data.len());
            let end = (start + n).min(b.data.len());
            let out = b.data[start..end].to_vec();
            b.pos = end;
            out
        } else {
            Vec::new()
        }
    }

    /// Move the cursor and return its new absolute position. A position before the start is
    /// clamped to 0 (a lenient plugin seeking past the beginning still lands on a valid
    /// stream); one past [`MAX_STREAM_BYTES`] is rejected with `None`, because the cursor is
    /// what the next write grows the buffer to.
    fn seek_to(&self, pos: i64, mode: u32) -> Option<i64> {
        let mut b = self.inner.lock().ok()?;
        let base = match mode {
            SEEK_CUR => b.pos as i64,
            SEEK_END => b.data.len() as i64,
            _ => 0, // SEEK_SET
        };
        let new = base.checked_add(pos)?.max(0);
        if new > MAX_STREAM_BYTES as i64 {
            return None;
        }
        b.pos = new as usize;
        Some(new)
    }

    fn position(&self) -> i64 {
        self.inner.lock().map(|b| b.pos as i64).unwrap_or(0)
    }
}

// IBStream seek modes — fixed by the VST3 ABI. kIBSeekSet (0) is the `_` arm in seek_to.
const SEEK_CUR: u32 = 1; // kIBSeekCur
const SEEK_END: u32 = 2; // kIBSeekEnd

impl Class for MemoryStream {
    type Interfaces = (IBStream, IStreamAttributes);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(
        &self,
        buffer: *mut std::ffi::c_void,
        num_bytes: i32,
        num_bytes_read: *mut i32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kResultFalse;
        }
        let bytes = self.read_at_cursor(num_bytes as usize);
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
        if !num_bytes_read.is_null() {
            *num_bytes_read = bytes.len() as i32;
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut std::ffi::c_void,
        num_bytes: i32,
        num_bytes_written: *mut i32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kResultFalse;
        }
        let src = std::slice::from_raw_parts(buffer as *const u8, num_bytes as usize);
        // A refused write reports zero bytes written and kOutOfMemory: the plugin's own error
        // path is the only correct answer here, since panicking (or aborting on a capacity
        // overflow) would unwind out of a C++ call.
        let Some(written) = self.write_at_cursor(src) else {
            if !num_bytes_written.is_null() {
                *num_bytes_written = 0;
            }
            return kOutOfMemory;
        };
        if !num_bytes_written.is_null() {
            *num_bytes_written = written as i32;
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> tresult {
        let Some(new) = self.seek_to(pos, mode as u32) else {
            return kInvalidArgument;
        };
        if !result.is_null() {
            *result = new;
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        if pos.is_null() {
            return kResultFalse;
        }
        *pos = self.position();
        kResultOk
    }
}

impl IStreamAttributesTrait for MemoryStream {
    unsafe fn getFileName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        let Some(file_name) = self.file_name.as_ref() else {
            (*name).fill(0);
            return kResultFalse;
        };
        (*name).fill(0);
        let count = file_name.len().min((*name).len().saturating_sub(1));
        (&mut *name)[..count].copy_from_slice(&file_name[..count]);
        kResultOk
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map(|attributes| attributes.as_ptr())
            .unwrap_or(ptr::null_mut())
    }
}

/// Create an empty attributed stream for a plugin data/state write.
pub fn create_memory_stream_with_metadata(
    file_name: Option<&str>,
    state_type: StreamStateType,
) -> ComWrapper<MemoryStream> {
    ComWrapper::new(MemoryStream::with_metadata(
        Vec::new(),
        StreamMetadata {
            file_name,
            ..StreamMetadata::new(state_type)
        },
    ))
}

/// Create an attributed stream seeded for a plugin data/state read.
pub fn create_memory_stream_from_with_metadata(
    data: Vec<u8>,
    file_name: Option<&str>,
    state_type: StreamStateType,
) -> ComWrapper<MemoryStream> {
    ComWrapper::new(MemoryStream::with_metadata(
        data,
        StreamMetadata {
            file_name,
            ..StreamMetadata::new(state_type)
        },
    ))
}

/// Create the stream a `setState` call reads from, carrying the attributes that tell the
/// plugin where the bytes came from: the `StateType`, and — when the context names a source
/// file — that file's path and stem.
pub fn create_state_restore_stream(
    data: Vec<u8>,
    context: &StateContext,
) -> ComWrapper<MemoryStream> {
    ComWrapper::new(MemoryStream::with_metadata(
        data,
        StreamMetadata::for_state_context(context),
    ))
}

// --- Linux IRunLoop ------------------------------------------------------
// VSTGUI-based editors (and most non-JUCE plugin UIs) strictly require the
// host frame to also implement `Steinberg::Linux::IRunLoop`: the view
// registers file-descriptor event handlers (its X11 connection) and
// periodic timers with the host, and paints/responds ONLY when the host
// services them. Without this the editor attaches but stays black. The host
// must call `Plugin::service_run_loop()` on its UI thread regularly (every
// frame) while an editor is open.

/// What a plugin's editor registered with the host's run loop, shared
/// between the frame (registration, called by the plugin during attach) and
/// the plugin impl (servicing, driven by the host each UI frame).
#[cfg(target_os = "linux")]
pub struct RunLoopRegistry {
    pub handlers: Vec<(
        vst3::ComPtr<vst3::Steinberg::Linux::IEventHandler>,
        vst3::Steinberg::Linux::FileDescriptor,
    )>,
    pub timers: Vec<RunLoopTimer>,
}

#[cfg(target_os = "linux")]
pub struct RunLoopTimer {
    pub handler: vst3::ComPtr<vst3::Steinberg::Linux::ITimerHandler>,
    pub interval_ms: u64,
    pub due: std::time::Instant,
}

#[cfg(target_os = "linux")]
impl RunLoopRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            timers: Vec::new(),
        }
    }
}

// The registry holds COM pointers into plugin code. They are only ever
// touched from the host's UI thread (registration inside `open_editor`,
// servicing inside `service_run_loop`, both UI-thread calls); the Send
// bound is inherited from `PluginInternal: Send` storage, the same
// pragmatics as the ComPtrs PluginImpl already holds.
#[cfg(target_os = "linux")]
unsafe impl Send for RunLoopRegistry {}

// Host implementation of `IPlugFrame` (all platforms) plus
// `Linux::IRunLoop` (Linux only). A plugin editor calls `resizeView` to ask
// the host to resize the window hosting its view (recorded; the host polls
// take_editor_resize_request), and on Linux registers its event
// handlers/timers via the IRunLoop half (serviced via
// `Plugin::service_run_loop`).
pub struct HostPlugFrame {
    requested: Arc<Mutex<Option<(i32, i32)>>>,
    #[cfg(target_os = "linux")]
    run_loop: Arc<Mutex<RunLoopRegistry>>,
}

impl HostPlugFrame {
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        visitor.visit(crate::host_storage::HostStorageRef::arc(&self.requested))?;
        #[cfg(target_os = "linux")]
        {
            visitor.visit(crate::host_storage::HostStorageRef::arc(&self.run_loop))?;
            let run_loop = self
                .run_loop
                .lock()
                .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
            visitor.visit(crate::host_storage::HostStorageRef::vector(
                &run_loop.handlers,
            )?)?;
            visitor.visit(crate::host_storage::HostStorageRef::vector(
                &run_loop.timers,
            )?)?;
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub fn new(
        requested: Arc<Mutex<Option<(i32, i32)>>>,
        run_loop: Arc<Mutex<RunLoopRegistry>>,
    ) -> Self {
        Self {
            requested,
            run_loop,
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new(requested: Arc<Mutex<Option<(i32, i32)>>>) -> Self {
        Self { requested }
    }
}

#[cfg(target_os = "linux")]
impl Class for HostPlugFrame {
    type Interfaces = (IPlugFrame, vst3::Steinberg::Linux::IRunLoop);
}

#[cfg(not(target_os = "linux"))]
impl Class for HostPlugFrame {
    type Interfaces = (IPlugFrame,);
}

#[cfg(target_os = "linux")]
impl vst3::Steinberg::Linux::IRunLoopTrait for HostPlugFrame {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut vst3::Steinberg::Linux::IEventHandler,
        fd: vst3::Steinberg::Linux::FileDescriptor,
    ) -> tresult {
        let Some(handler) = vst3::ComRef::from_raw(handler) else {
            return kInvalidArgument;
        };
        match self.run_loop.lock() {
            Ok(mut reg) => {
                reg.handlers.push((handler.to_com_ptr(), fd));
                kResultOk
            }
            Err(_) => kInternalError,
        }
    }

    unsafe fn unregisterEventHandler(
        &self,
        handler: *mut vst3::Steinberg::Linux::IEventHandler,
    ) -> tresult {
        match self.run_loop.lock() {
            Ok(mut reg) => {
                reg.handlers.retain(|(h, _)| h.as_ptr() != handler);
                kResultOk
            }
            Err(_) => kInternalError,
        }
    }

    unsafe fn registerTimer(
        &self,
        handler: *mut vst3::Steinberg::Linux::ITimerHandler,
        milliseconds: vst3::Steinberg::Linux::TimerInterval,
    ) -> tresult {
        let Some(handler) = vst3::ComRef::from_raw(handler) else {
            return kInvalidArgument;
        };
        let interval_ms = milliseconds.max(1);
        match self.run_loop.lock() {
            Ok(mut reg) => {
                reg.timers.push(RunLoopTimer {
                    handler: handler.to_com_ptr(),
                    interval_ms,
                    due: std::time::Instant::now() + std::time::Duration::from_millis(interval_ms),
                });
                kResultOk
            }
            Err(_) => kInternalError,
        }
    }

    unsafe fn unregisterTimer(
        &self,
        handler: *mut vst3::Steinberg::Linux::ITimerHandler,
    ) -> tresult {
        match self.run_loop.lock() {
            Ok(mut reg) => {
                reg.timers.retain(|t| t.handler.as_ptr() != handler);
                kResultOk
            }
            Err(_) => kInternalError,
        }
    }
}

impl IPlugFrameTrait for HostPlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        let Some(view) = ComRef::<IPlugView>::from_raw(view) else {
            return kInvalidArgument;
        };
        if new_size.is_null() {
            return kInvalidArgument;
        }
        let r = &*new_size;
        let Some(width) = r.right.checked_sub(r.left).filter(|width| *width > 0) else {
            return kInvalidArgument;
        };
        let Some(height) = r.bottom.checked_sub(r.top).filter(|height| *height > 0) else {
            return kInvalidArgument;
        };

        match self.requested.lock() {
            Ok(mut slot) => *slot = Some((width, height)),
            Err(_) => return kInternalError,
        }
        // Drop the slot lock before invoking plugin code. `onSize` is required in this exact
        // callstack and may re-enter `resizeView`; retaining the mutex here would deadlock and
        // overwriting the slot after the callback would lose the nested request.
        view.onSize(new_size)
    }
}

/// Create a host plug-frame backed by a shared resize-request slot (and, on
/// Linux, a run-loop registry - see `RunLoopRegistry`).
#[cfg(target_os = "linux")]
pub fn create_host_plug_frame(
    requested: Arc<Mutex<Option<(i32, i32)>>>,
    run_loop: Arc<Mutex<RunLoopRegistry>>,
) -> ComWrapper<HostPlugFrame> {
    ComWrapper::new(HostPlugFrame::new(requested, run_loop))
}

/// Create a host plug-frame backed by a shared resize-request slot.
#[cfg(not(target_os = "linux"))]
pub fn create_host_plug_frame(
    requested: Arc<Mutex<Option<(i32, i32)>>>,
) -> ComWrapper<HostPlugFrame> {
    ComWrapper::new(HostPlugFrame::new(requested))
}

/// Cap on native author events and the separate DSP parameter-feedback sink.
/// The ordered author source is pre-reserved and refuses capacity overflow.
/// Either sink becoming full makes the author source sticky Incomplete; no
/// truncated author vector or silently unaccepted DSP value can report success.
pub const MAX_EDITOR_FEEDBACK: usize = 4096;

/// Cap on the queued host-notification stream — the `IComponentHandler2` / `IUnitHandler` /
/// `IProgress` requests a plugin raises, drained by `Plugin::take_host_notifications`.
///
/// Control-only request callbacks return `kResultFalse` at capacity. Group
/// and dirty author callbacks use the separate ordered author source instead.
/// Hosts must drain control requests regularly; progress producers and this
/// queue do not promise a global callback order or author group membership.
pub const MAX_HOST_NOTIFICATIONS: usize = 1024;
const MAX_CONTEXT_MENU_ITEMS: usize = 256;

struct PendingContextMenuItem {
    tag: i32,
    flags: i32,
    target: Option<ComPtr<IContextMenuTarget>>,
}

struct ContextMenuRegistry {
    next_menu_id: AtomicU64,
    pending: Mutex<Vec<(u64, Vec<PendingContextMenuItem>)>>,
    owner_thread: ThreadId,
}

impl ContextMenuRegistry {
    fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        let menus = self
            .pending
            .lock()
            .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
        visitor.visit(crate::host_storage::HostStorageRef::vector(&menus)?)?;
        for (_, items) in menus.iter() {
            visitor.visit(crate::host_storage::HostStorageRef::vector(items)?)?;
        }
        Ok(())
    }

    fn new() -> Self {
        Self {
            next_menu_id: AtomicU64::new(1),
            pending: Mutex::new(Vec::with_capacity(MAX_HOST_NOTIFICATIONS)),
            owner_thread: thread::current().id(),
        }
    }

    fn execute(&self, menu_id: u64, item_id: u32) -> crate::Result<()> {
        if thread::current().id() != self.owner_thread {
            return Err(crate::Error::Other(
                "context-menu targets must be invoked on the plugin control thread".to_string(),
            ));
        }
        let (tag, target) = {
            let mut menus = self
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let menu_index = menus
                .binary_search_by_key(&menu_id, |entry| entry.0)
                .map_err(|_| {
                    crate::Error::Other("context menu is no longer pending".to_string())
                })?;
            let items = &menus[menu_index].1;
            let item = items.get(item_id as usize).ok_or_else(|| {
                crate::Error::Other("context-menu item id is out of range".to_string())
            })?;
            if item.flags & IContextMenuItem_::Flags_::kIsDisabled as i32 != 0
                || item.flags & IContextMenuItem_::Flags_::kIsSeparator as i32 != 0
            {
                return Err(crate::Error::Other(
                    "context-menu item is not executable".to_string(),
                ));
            }
            if item.target.is_none() {
                return Err(crate::Error::Other(
                    "context-menu item has no executable target".to_string(),
                ));
            }
            let tag = item.tag;
            let target = item.target.clone();
            menus.remove(menu_index);
            (tag, target)
        };
        let result = unsafe {
            target
                .as_ref()
                .ok_or_else(|| crate::Error::Other("context-menu target disappeared".to_string()))?
                .executeMenuItem(tag)
        };
        if result == kResultOk || result == kResultTrue {
            Ok(())
        } else {
            Err(crate::Error::Other(format!(
                "plugin rejected context-menu command: {result:#x}"
            )))
        }
    }

    fn dismiss(&self, menu_id: u64) -> crate::Result<()> {
        if thread::current().id() != self.owner_thread {
            return Err(crate::Error::Other(
                "context menus must be dismissed on the plugin control thread".to_string(),
            ));
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Ok(index) = pending.binary_search_by_key(&menu_id, |entry| entry.0) {
            pending.remove(index);
            Ok(())
        } else {
            Err(crate::Error::Other(
                "context menu is no longer pending".to_string(),
            ))
        }
    }
}

struct StoredContextMenuItem {
    item: IContextMenuItem,
    target: Option<ComPtr<IContextMenuTarget>>,
}

pub(crate) struct HostContextMenu {
    parameter_id: Option<u32>,
    items: Mutex<Vec<StoredContextMenuItem>>,
    notifications: Arc<Mutex<Vec<crate::plugin::HostNotification>>>,
    registry: Arc<ContextMenuRegistry>,
}

impl HostContextMenu {
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        // Observe each mutex separately, never inverse popup's notification ->
        // registry order. No foreign target is called or dereferenced here.
        {
            let items = self
                .items
                .lock()
                .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
            visitor.visit(crate::host_storage::HostStorageRef::vector(&items)?)?;
        }
        visitor.visit(crate::host_storage::HostStorageRef::arc(
            &self.notifications,
        ))?;
        {
            let notifications = self
                .notifications
                .lock()
                .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
            visitor.visit(crate::host_storage::HostStorageRef::vector(&notifications)?)?;
            for value in notifications.iter() {
                visit_notification_payload(value, visitor)?;
            }
        }
        visitor.visit(crate::host_storage::HostStorageRef::arc(&self.registry))?;
        self.registry.visit_owned_storage(visitor)
    }
}

impl Class for HostContextMenu {
    type Interfaces = (IContextMenu,);
}

impl IContextMenuTrait for HostContextMenu {
    unsafe fn getItemCount(&self) -> i32 {
        self.items
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
            .min(i32::MAX as usize) as i32
    }

    unsafe fn getItem(
        &self,
        index: i32,
        item: *mut IContextMenuItem,
        target: *mut *mut IContextMenuTarget,
    ) -> tresult {
        if index < 0 || item.is_null() || target.is_null() {
            return kInvalidArgument;
        }
        *target = ptr::null_mut();
        let items = self
            .items
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(stored) = items.get(index as usize) else {
            return kInvalidArgument;
        };
        *item = stored.item;
        if let Some(stored_target) = stored.target.as_ref() {
            let owned_target = stored_target.clone();
            *target = owned_target.as_ptr();
            std::mem::forget(owned_target);
        }
        kResultOk
    }

    unsafe fn addItem(
        &self,
        item: *const IContextMenuItem,
        target: *mut IContextMenuTarget,
    ) -> tresult {
        if item.is_null() {
            return kInvalidArgument;
        }
        let target =
            ComRef::<IContextMenuTarget>::from_raw(target).map(|target| target.to_com_ptr());
        let mut items = self
            .items
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if items.len() >= MAX_CONTEXT_MENU_ITEMS {
            return kResultFalse;
        }
        items.push(StoredContextMenuItem {
            item: *item,
            target,
        });
        kResultOk
    }

    unsafe fn removeItem(
        &self,
        item: *const IContextMenuItem,
        target: *mut IContextMenuTarget,
    ) -> tresult {
        if item.is_null() {
            return kInvalidArgument;
        }
        let requested = &*item;
        let mut items = self
            .items
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(index) = items.iter().position(|stored| {
            stored.item.name == requested.name
                && stored.item.tag == requested.tag
                && stored.item.flags == requested.flags
                && stored
                    .target
                    .as_ref()
                    .map_or(ptr::null_mut(), ComPtr::as_ptr)
                    == target
        }) else {
            return kResultFalse;
        };
        items.remove(index);
        kResultOk
    }

    unsafe fn popup(&self, x: i32, y: i32) -> tresult {
        let items = self
            .items
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let public_items = items
            .iter()
            .enumerate()
            .map(|(index, stored)| crate::plugin::ContextMenuItem {
                item_id: index as u32,
                name: crate::internal::utils::vst_string_to_string(&stored.item.name),
                tag: stored.item.tag,
                flags: stored.item.flags,
            })
            .collect::<Vec<_>>();
        let pending_items = items
            .iter()
            .map(|stored| PendingContextMenuItem {
                tag: stored.item.tag,
                flags: stored.item.flags,
                target: stored.target.clone(),
            })
            .collect::<Vec<_>>();
        drop(items);

        let mut notifications = self
            .notifications
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if notifications.len() >= MAX_HOST_NOTIFICATIONS {
            return kResultFalse;
        }
        let mut pending = self
            .registry
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if pending.len() >= MAX_HOST_NOTIFICATIONS {
            return kResultFalse;
        }
        let Ok(menu_id) =
            self.registry
                .next_menu_id
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
        else {
            return kResultFalse;
        };
        // All actual popup writes own this mutex; keep lookup ordered even if
        // callback arrival differs from serial reservation order.
        let index = match pending.binary_search_by_key(&menu_id, |entry| entry.0) {
            Ok(_) => return kResultFalse,
            Err(index) => index,
        };
        pending.insert(index, (menu_id, pending_items));
        notifications.push(crate::plugin::HostNotification::ContextMenuRequested {
            menu_id,
            parameter_id: self.parameter_id,
            x,
            y,
            items: public_items,
        });
        kResultOk
    }
}

// Component Handler implementation
// Native author callbacks share one ordered, bounded event source. Control-only
// notifications remain separate and cannot imply author group membership.
struct EditorEventBuffer {
    events: Vec<crate::plugin::NativeEditorEvent>,
    failure: Option<crate::plugin::NativeEditorEventFailure>,

    revision: u64,
    // History evidence only: successful nonempty raw retirement, never source
    // drain, claim or a second copy of the raw callback state.
    retirement_lineage: u64,
    open_parameters: [u32; MAX_EDITOR_FEEDBACK],
    open_parameter_count: usize,
    group_depth: u32,
}

impl EditorEventBuffer {
    fn new() -> Self {
        Self {
            events: Vec::with_capacity(MAX_EDITOR_FEEDBACK),
            failure: None,
            revision: 0,
            retirement_lineage: 0,
            open_parameters: [0; MAX_EDITOR_FEEDBACK],
            open_parameter_count: 0,
            group_depth: 0,
        }
    }

    fn refuse(&mut self, failure: crate::plugin::NativeEditorEventFailure) -> bool {
        if self.failure.is_none() {
            self.failure = Some(failure);
        }
        false
    }

    fn push(&mut self, event: crate::plugin::NativeEditorEvent) -> bool {
        if self.failure.is_some() {
            return false;
        }
        if self.events.len() >= MAX_EDITOR_FEEDBACK {
            return self.refuse(crate::plugin::NativeEditorEventFailure::Overflow);
        }

        let Some(next_revision) = self.revision.checked_add(1) else {
            return self.refuse(crate::plugin::NativeEditorEventFailure::SequenceExhausted);
        };
        use crate::plugin::{NativeEditorEvent, NativeEditorEventFailure, ParameterEditKind};
        match &event {
            NativeEditorEvent::Parameter(edit) => match edit.kind {
                ParameterEditKind::BeginGesture => {
                    if self.open_parameter_count == MAX_EDITOR_FEEDBACK {
                        return self.refuse(NativeEditorEventFailure::Overflow);
                    }
                    self.open_parameters[self.open_parameter_count] = edit.id;
                    self.open_parameter_count += 1;
                }
                ParameterEditKind::EndGesture => {
                    let Some(index) = self.open_parameters[..self.open_parameter_count]
                        .iter()
                        .position(|id| *id == edit.id)
                    else {
                        return self.refuse(NativeEditorEventFailure::InvalidBoundary);
                    };
                    self.open_parameter_count -= 1;
                    self.open_parameters[index] = self.open_parameters[self.open_parameter_count];
                }
                ParameterEditKind::ValueChange => {}
            },
            NativeEditorEvent::GroupEditStarted => {
                let Some(depth) = self.group_depth.checked_add(1) else {
                    return self.refuse(NativeEditorEventFailure::Overflow);
                };
                self.group_depth = depth;
            }
            NativeEditorEvent::GroupEditFinished => {
                let Some(depth) = self.group_depth.checked_sub(1) else {
                    return self.refuse(NativeEditorEventFailure::InvalidBoundary);
                };
                self.group_depth = depth;
            }
            NativeEditorEvent::DirtyChanged(_)
            | NativeEditorEvent::ParameterRescanRequested { .. } => {}
        }
        self.revision = next_revision;
        self.events.push(event);
        true
    }

    fn status(&self) -> crate::plugin::NativeEditorEventSourceStatus {
        crate::plugin::NativeEditorEventSourceStatus {
            revision: self.revision,
            pending_events: self.events.len(),
            pending_dsp_feedback_values: 0,
            open_parameter_edits: self.open_parameter_count,
            group_depth: self.group_depth,
            failure: self.failure,
        }
    }

    fn drain(&mut self) -> crate::plugin::NativeEditorEventDrain {
        if let Some(failure) = self.failure {
            self.events.clear();
            return crate::plugin::NativeEditorEventDrain::Incomplete { failure };
        }
        crate::plugin::NativeEditorEventDrain::Complete {
            events: self.events.drain(..).collect(),
        }
    }
}

/// Native VST3 component callbacks with one bounded ordered author source.
pub struct ComponentHandler {
    control_thread: thread::ThreadId,
    // Execution feedback remains a separate bounded DSP sink, not author history.
    pub parameter_changes: Arc<Mutex<Vec<(u32, f64)>>>,
    editor_events: Mutex<EditorEventBuffer>,
    // One callback authority: high 32 bits serial, low 32 bits raw flag union.
    // Repeated flags advance the serial; u32::MAX is sticky exhaustion.
    restart_cohort: AtomicU64,
    notifications: Arc<Mutex<Vec<crate::plugin::HostNotification>>>,
    context_menus: Arc<ContextMenuRegistry>,
    retained_storage: super::host_storage::RetainedComStorage,
}

impl ComponentHandler {
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        {
            let events = self
                .editor_events
                .lock()
                .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
            visitor.visit(crate::host_storage::HostStorageRef::vector(&events.events)?)?;
        }
        visitor.visit(crate::host_storage::HostStorageRef::arc(
            &self.parameter_changes,
        ))?;
        {
            let changes = self
                .parameter_changes
                .lock()
                .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
            visitor.visit(crate::host_storage::HostStorageRef::vector(&changes)?)?;
        }
        visitor.visit(crate::host_storage::HostStorageRef::arc(
            &self.notifications,
        ))?;
        {
            let notifications = self
                .notifications
                .lock()
                .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
            visitor.visit(crate::host_storage::HostStorageRef::vector(&notifications)?)?;
            for value in notifications.iter() {
                visit_notification_payload(value, visitor)?;
            }
        }
        visitor.visit(crate::host_storage::HostStorageRef::arc(
            &self.context_menus,
        ))?;
        self.context_menus.visit_owned_storage(visitor)?;
        self.retained_storage.visit_owned_storage(visitor)
    }

    /// Create a bounded native source sharing the existing DSP feedback sink.
    pub fn new(parameter_changes: Arc<Mutex<Vec<(u32, f64)>>>) -> Self {
        ComponentHandler {
            control_thread: thread::current().id(),
            parameter_changes,
            editor_events: Mutex::new(EditorEventBuffer::new()),
            restart_cohort: AtomicU64::new(0),
            notifications: Arc::new(Mutex::new(Vec::with_capacity(MAX_HOST_NOTIFICATIONS))),
            context_menus: Arc::new(ContextMenuRegistry::new()),
            retained_storage: super::host_storage::RetainedComStorage::new(),
        }
    }

    /// Read pending flags without consuming them or calling plugin/controller code.
    pub fn pending_restart_flags(&self) -> crate::plugin::RestartFlags {
        self.pending_restart_cohort().flags()
    }

    /// Exact recorded callback generation; no native metadata calls.
    pub fn pending_restart_cohort(&self) -> crate::plugin::RestartCohort {
        crate::plugin::RestartCohort::from_word(self.restart_cohort.load(Ordering::Acquire))
    }

    /// Clear only an exact completed service cohort, retaining its serial.
    pub fn retire_restart_cohort(&self, expected: crate::plugin::RestartCohort) -> bool {
        if !expected.pending() || expected.exhausted() {
            return false;
        }
        let Ok(mut source) = self.editor_events.try_lock() else {
            return false;
        };
        self.retire_restart_cohort_locked(&mut source, expected)
            .unwrap_or(false)
    }

    // The caller already owns the original source lock (initial handoff also
    // holds feedback/input). All fallible arithmetic precedes the CAS; after a
    // successful clear the lineage update cannot fail or call foreign code.
    fn retire_restart_cohort_locked(
        &self,
        source: &mut EditorEventBuffer,
        expected: crate::plugin::RestartCohort,
    ) -> Result<bool, crate::plugin::RestartSourceError> {
        use crate::plugin::RestartSourceError as Failure;
        if expected.exhausted() {
            return Err(Failure::Exhausted);
        }
        let next = if expected.flags().is_empty() {
            None
        } else {
            Some(
                source
                    .retirement_lineage
                    .checked_add(1)
                    .ok_or(Failure::Exhausted)?,
            )
        };
        if self
            .restart_cohort
            .compare_exchange(
                expected.word(),
                u64::from(expected.serial()) << 32,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Ok(false);
        }
        if let Some(next) = next {
            source.retirement_lineage = next;
        }
        Ok(true)
    }

    /// Claim a still-current serviced serial; do not clear any pending callback.
    pub fn claim_retired_restart_serial(&self, serial: u32) -> bool {
        if serial == 0 || serial == u32::MAX {
            return false;
        }
        let expected = u64::from(serial) << 32;
        self.restart_cohort
            .compare_exchange(expected, expected, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Observe and attempt one bounded drain; concurrent newer flags remain pending.
    pub fn take_restart_flags(&self) -> crate::plugin::RestartFlags {
        let current = self.pending_restart_cohort();
        self.retire_restart_cohort(current);
        current.flags()
    }

    fn record_restart_flags(&self, flags: u32) -> crate::plugin::RestartCohort {
        if flags == 0 {
            return self.pending_restart_cohort();
        }
        let mut previous = self.restart_cohort.load(Ordering::Acquire);
        loop {
            let serial = (previous >> 32) as u32;
            let next =
                (u64::from(serial.saturating_add(1)) << 32) | u64::from(previous as u32 | flags);
            match self.restart_cohort.compare_exchange_weak(
                previous,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return crate::plugin::RestartCohort::from_word(next),
                Err(observed) => previous = observed,
            }
        }
    }

    pub(crate) fn capture_restart_source_witness(
        &self,
        owner: crate::plugin::NativeEditorAttachmentOwner,
    ) -> Result<crate::plugin::RestartSourceWitness, crate::plugin::RestartSourceError> {
        use crate::plugin::{NativeEditorEvent as Event, RestartSourceError as Failure};
        if thread::current().id() != self.control_thread {
            return Err(Failure::WrongThread);
        }
        let source = self.editor_events.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::Busy,
            std::sync::TryLockError::Poisoned(_) => Failure::PoisonedOwner,
        })?;
        let before = self.pending_restart_cohort();
        let feedback = self
            .parameter_changes
            .try_lock()
            .map_err(|error| match error {
                std::sync::TryLockError::WouldBlock => Failure::Busy,
                std::sync::TryLockError::Poisoned(_) => Failure::PoisonedOwner,
            })?;
        let mut status = source.status();
        status.pending_dsp_feedback_values = feedback.len();
        let cohort = self.pending_restart_cohort();
        if before != cohort {
            return Err(Failure::Conflict);
        }
        if cohort.exhausted() || source.retirement_lineage == u64::MAX {
            return Err(Failure::Exhausted);
        }
        if status.failure().is_some()
            || status.open_parameter_edits() != 0
            || status.group_depth() != 0
            || !feedback.is_empty()
        {
            return Err(Failure::InvalidSource);
        }
        let kind = RestartSourceParameterKind::from_flags(cohort.flags().bits())
            .ok_or(Failure::Conflict)?;
        let mut last = None;
        for event in &source.events {
            if let Event::ParameterRescanRequested {
                flags,
                cohort: marker,
            } = event
            {
                if *flags != kind.flags()
                    || marker.flags().bits() != kind.flags()
                    || marker.serial() == 0
                    || marker.exhausted()
                    || last.is_some_and(|prior: crate::plugin::RestartCohort| {
                        prior.serial() >= marker.serial()
                    })
                {
                    return Err(Failure::Conflict);
                }
                last = Some(*marker);
            }
        }
        if cohort.serial() == 0 || last != Some(cohort) {
            return Err(Failure::Conflict);
        }
        Ok(crate::plugin::RestartSourceWitness {
            owner,
            source: status,
            cohort,
            retirement_lineage: source.retirement_lineage,
        })
    }

    pub(crate) fn settle_restart_source_prefix(
        &self,
        owner: crate::plugin::NativeEditorAttachmentOwner,
        witness: crate::plugin::RestartSourceWitness,
    ) -> Result<crate::plugin::RestartSourceSettlement, crate::plugin::RestartSourceError> {
        use crate::plugin::{
            NativeEditorEvent as Event, RestartSourceError as Failure,
            RestartSourceSettlement as Settlement,
        };
        if thread::current().id() != self.control_thread {
            return Err(Failure::WrongThread);
        }
        if owner != witness.owner {
            return Err(Failure::Conflict);
        }
        let mut source = self.editor_events.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::Busy,
            std::sync::TryLockError::Poisoned(_) => Failure::PoisonedOwner,
        })?;
        let before = self.pending_restart_cohort();
        // Keep the original lock order, but never consume or require an empty,
        // closed new suffix. Its gestures/feedback are later obligations.
        let _feedback = self
            .parameter_changes
            .try_lock()
            .map_err(|error| match error {
                std::sync::TryLockError::WouldBlock => Failure::Busy,
                std::sync::TryLockError::Poisoned(_) => Failure::PoisonedOwner,
            })?;
        let current = self.pending_restart_cohort();
        if before != current || source.retirement_lineage != witness.retirement_lineage {
            return Err(Failure::Conflict);
        }
        if source.failure.is_some() {
            return Err(Failure::InvalidSource);
        }
        if current.exhausted() || source.retirement_lineage == u64::MAX {
            return Err(Failure::Exhausted);
        }
        let delta = source
            .revision
            .checked_sub(witness.source.revision())
            .and_then(|value| usize::try_from(value).ok());
        let kind = RestartSourceParameterKind::from_flags(witness.cohort.flags().bits())
            .ok_or(Failure::Conflict)?;
        if delta != Some(source.events.len()) {
            return Err(Failure::Conflict);
        }
        let mut last = witness.cohort;
        for event in &source.events {
            if let Event::ParameterRescanRequested { flags, cohort } = event {
                if *flags != kind.flags()
                    || cohort.flags().bits() != kind.flags()
                    || cohort.exhausted()
                    || cohort.serial() <= last.serial()
                {
                    return Err(Failure::Conflict);
                }
                last = *cohort;
            }
        }
        if last != current {
            return Err(Failure::Conflict);
        }
        if current == witness.cohort {
            return if self.retire_restart_cohort_locked(&mut source, current)? {
                Ok(Settlement::Retired)
            } else {
                Err(Failure::Conflict)
            };
        }
        if current.flags().bits() != kind.flags() || current.serial() <= witness.cohort.serial() {
            return Err(Failure::Conflict);
        }
        // Lineage equality proves no raw clear occurred after publication.
        // The intact suffix explains the newer same-kind cohort. Mixed 4/16
        // unions and unknown requests refuse; neither kind can settle the other.
        // Settle only the old caller-held prefix; leave all current raw/source
        // state untouched.
        Ok(Settlement::NewerParameterSuffixPending { cohort: current })
    }

    /// Drain one complete ordered author batch or the sticky source failure.
    /// Incomplete never publishes a truncated vector and drain cannot clear it.
    pub fn take_editor_events(&self) -> crate::plugin::NativeEditorEventDrain {
        self.editor_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain()
    }

    /// Read immutable facts without clearing the same source or its open bounds.
    /// Runtime outstanding-delivery admission remains a separate required gate.
    pub fn editor_event_source_status(&self) -> crate::plugin::NativeEditorEventSourceStatus {
        // Same lock order as performEdit: author source then existing DSP sink.
        // The DSP never acquires these two locks in the opposite order.
        let events = self
            .editor_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let feedback = self
            .parameter_changes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut status = events.status();
        status.pending_dsp_feedback_values = feedback.len();
        status
    }

    pub(crate) fn observe_editor_source_bounded(
        &self,
        maximum: usize,
        output_events: &mut Vec<crate::plugin::NativeEditorEvent>,
        output_feedback: &mut Vec<(u32, f64)>,
    ) -> crate::Result<crate::plugin::NativeEditorEventSourceStatus> {
        if thread::current().id() != self.control_thread || maximum > MAX_EDITOR_FEEDBACK {
            return Err(crate::Error::InvalidParameter(
                "native source observation thread or bound is invalid".to_owned(),
            ));
        }
        // Only static errors cross this scope: allocation/formatting and all
        // native calls stay outside the two producer locks. Existing events
        // contain only inline scalars; cloning them must remain allocation-free.
        let observed = (|| -> Result<_, &'static str> {
            let events = self
                .editor_events
                .lock()
                .map_err(|_| "native source lock poisoned")?;
            let feedback = self
                .parameter_changes
                .lock()
                .map_err(|_| "native feedback lock poisoned")?;
            if events.failure.is_some() {
                return Err("native source is incomplete");
            }
            if events.events.len() > maximum || feedback.len() > maximum {
                return Err("native source exceeds observation bound");
            }
            if output_events.capacity() < events.events.len()
                || output_feedback.capacity() < feedback.len()
            {
                return Err("native source observation destinations are too small");
            }
            let mut status = events.status();
            status.pending_dsp_feedback_values = feedback.len();
            // Every fallible check precedes these complete, preallocated copies.
            output_events.clear();
            output_events.extend_from_slice(&events.events);
            output_feedback.clear();
            output_feedback.extend_from_slice(&feedback);
            Ok(status)
        })();
        observed.map_err(|reason| crate::Error::Other(reason.to_owned()))
    }

    /// The exact appended source suffix during bounded main getState.
    /// Clearing the native dirty flag may be part of save-state capture;
    /// any value/author boundary/dirty=true mutation remains a typed retry.
    pub fn editor_capture_source_status_since(
        &self,
        revision: u64,
    ) -> Option<crate::plugin::NativeEditorEventSourceStatus> {
        let events = self
            .editor_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let feedback = self
            .parameter_changes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if events.failure.is_some() || !feedback.is_empty() {
            return None;
        }
        let delta = usize::try_from(events.revision.checked_sub(revision)?).ok()?;
        let start = events.events.len().checked_sub(delta)?;
        if !events.events[start..]
            .iter()
            .all(|event| matches!(event, crate::plugin::NativeEditorEvent::DirtyChanged(false)))
        {
            return None;
        }
        Some(events.status())
    }

    /// Copy no source/history payload: visit finite native Value callbacks in
    /// their original order while retaining every Begin/End/group/dirty item.
    /// The processing role's visitor only appends to its preallocated input
    /// queue. No native callback is invoked while this source lock is held.
    pub(crate) fn visit_unconsumed_editor_values(
        &self,
        mut queue: impl FnMut(u32, f64) -> bool,
    ) -> bool {
        let mut events = self
            .editor_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if events.failure.is_some() {
            return false;
        }
        let mut feedback = self
            .parameter_changes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let value_events = events.events.iter().filter_map(|event| match event {
            crate::plugin::NativeEditorEvent::Parameter(edit)
                if edit.kind == crate::plugin::ParameterEditKind::ValueChange =>
            {
                edit.value.map(|value| (edit.id, value))
            }
            _ => None,
        });
        let value_count = value_events.clone().count();
        // Only consume GUI-to-DSP feedback proved to be the exact unconsumed
        // source suffix. Independent public event drains cannot silently erase
        // an unrelated still-pending native feedback value.
        if feedback.len() > value_count
            || !value_events
                .skip(value_count - feedback.len())
                .zip(feedback.iter())
                .all(|((id, value), (actual_id, actual_value))| {
                    id == *actual_id && value.to_bits() == actual_value.to_bits()
                })
        {
            return false;
        }
        let complete = events.events.iter().all(|event| match event {
            crate::plugin::NativeEditorEvent::Parameter(edit)
                if edit.kind == crate::plugin::ParameterEditKind::ValueChange =>
            {
                edit.value.is_none_or(|value| queue(edit.id, value))
            }
            _ => true,
        });
        if complete {
            feedback.clear();
            true
        } else {
            events.refuse(crate::plugin::NativeEditorEventFailure::Overflow)
        }
    }

    /// Drain control-only host requests. These do not carry author group/dirty
    /// boundaries and have no cross-ordering with the author event source.
    pub fn take_host_notifications(&self) -> Vec<crate::plugin::HostNotification> {
        self.notifications
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain(..)
            .collect()
    }

    /// Execute one retained control-only context-menu target.
    pub fn execute_context_menu_item(&self, menu_id: u64, item_id: u32) -> crate::Result<()> {
        self.context_menus.execute(menu_id, item_id)
    }

    /// Release one retained control-only context-menu popup.
    pub fn dismiss_context_menu(&self, menu_id: u64) -> crate::Result<()> {
        self.context_menus.dismiss(menu_id)
    }

    fn push_editor_event(&self, event: crate::plugin::NativeEditorEvent) -> bool {
        if thread::current().id() != self.control_thread {
            return self
                .editor_events
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .refuse(crate::plugin::NativeEditorEventFailure::WrongThread);
        }
        self.editor_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(event)
    }

    fn push_notification(&self, notification: crate::plugin::HostNotification) -> bool {
        if thread::current().id() != self.control_thread {
            return false;
        }
        let mut notifications = self
            .notifications
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if notifications.len() >= MAX_HOST_NOTIFICATIONS {
            return false;
        }
        notifications.push(notification);
        true
    }
}

impl Class for ComponentHandler {
    type Interfaces = (
        IComponentHandler,
        IComponentHandler2,
        IComponentHandler3,
        IUnitHandler,
        IUnitHandler2,
    );
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: u32) -> i32 {
        if self.push_editor_event(crate::plugin::NativeEditorEvent::Parameter(
            crate::plugin::ParameterEdit {
                id,
                kind: crate::plugin::ParameterEditKind::BeginGesture,
                value: None,
            },
        )) {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn performEdit(&self, id: u32, value_normalized: f64) -> i32 {
        let mut events = self
            .editor_events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if thread::current().id() != self.control_thread {
            events.refuse(crate::plugin::NativeEditorEventFailure::WrongThread);
            return kResultFalse;
        }
        if events.failure.is_some() {
            return kResultFalse;
        }
        if !value_normalized.is_finite() || !(0.0..=1.0).contains(&value_normalized) {
            events.refuse(crate::plugin::NativeEditorEventFailure::InvalidValue);
            return kResultFalse;
        }
        // Hold the event source before the execution sink. DSP readers never
        // acquire the event lock, so there is no inverse lock order.
        let mut changes = self
            .parameter_changes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if changes.len() >= MAX_EDITOR_FEEDBACK {
            events.refuse(crate::plugin::NativeEditorEventFailure::Overflow);
            return kResultFalse;
        }
        if !events.push(crate::plugin::NativeEditorEvent::Parameter(
            crate::plugin::ParameterEdit {
                id,
                kind: crate::plugin::ParameterEditKind::ValueChange,
                value: Some(value_normalized),
            },
        )) {
            return kResultFalse;
        }
        changes.push((id, value_normalized));
        kResultOk
    }

    unsafe fn endEdit(&self, id: u32) -> i32 {
        if self.push_editor_event(crate::plugin::NativeEditorEvent::Parameter(
            crate::plugin::ParameterEdit {
                id,
                kind: crate::plugin::ParameterEditKind::EndGesture,
                value: None,
            },
        )) {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn restartComponent(&self, flags: i32) -> i32 {
        if thread::current().id() != self.control_thread {
            return kResultFalse;
        }
        log::debug!("Host: Restart component requested with flags: {flags:#x}");
        // Parameter rescans join the original ordered source. All nonzero raw
        // callbacks still advance the full cohort, even if marker publication
        // fails. Raw service/retirement never drains author events.
        if flags & (4 | 16) != 0 {
            let mut events = self
                .editor_events
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let cohort = self.record_restart_flags(flags as u32);
            if !events
                .push(crate::plugin::NativeEditorEvent::ParameterRescanRequested { flags, cohort })
            {
                return kResultFalse;
            }
        } else {
            self.record_restart_flags(flags as u32);
        }
        kResultOk
    }
}

impl IComponentHandler3Trait for ComponentHandler {
    unsafe fn createContextMenu(
        &self,
        plug_view: *mut IPlugView,
        parameter_id: *const u32,
    ) -> *mut IContextMenu {
        if thread::current().id() != self.control_thread {
            return ptr::null_mut();
        }
        if plug_view.is_null() {
            return ptr::null_mut();
        }
        let menu = ComWrapper::new(HostContextMenu {
            parameter_id: parameter_id.as_ref().copied(),
            items: Mutex::new(Vec::with_capacity(16)),
            notifications: self.notifications.clone(),
            registry: self.context_menus.clone(),
        });
        if self.retained_storage.register_menu(&menu).is_err() {
            return ptr::null_mut();
        }
        let Some(menu) = menu.to_com_ptr::<IContextMenu>() else {
            return ptr::null_mut();
        };
        let raw = menu.as_ptr();
        std::mem::forget(menu);
        raw
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: u8) -> i32 {
        log::debug!("Host: Plugin marked state as dirty (state: {})", state);
        if self.push_editor_event(crate::plugin::NativeEditorEvent::DirtyChanged(state != 0)) {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn requestOpenEditor(&self, name: *const std::os::raw::c_char) -> i32 {
        if thread::current().id() != self.control_thread {
            return kResultFalse;
        }
        log::debug!("Host: Plugin requested editor open");
        let name = if name.is_null() {
            None
        } else {
            Some(CStr::from_ptr(name).to_string_lossy().into_owned())
        };
        if self.push_notification(crate::plugin::HostNotification::OpenEditorRequested { name }) {
            kResultOk
        } else {
            kResultFalse
        }
    }

    // Group boundaries use the same source order as parameters and dirty state.
    unsafe fn startGroupEdit(&self) -> i32 {
        log::debug!("Host: Plugin started group edit");
        if self.push_editor_event(crate::plugin::NativeEditorEvent::GroupEditStarted) {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn finishGroupEdit(&self) -> i32 {
        log::debug!("Host: Plugin finished group edit");
        if self.push_editor_event(crate::plugin::NativeEditorEvent::GroupEditFinished) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IUnitHandlerTrait for ComponentHandler {
    unsafe fn notifyUnitSelection(&self, unit_id: i32) -> tresult {
        if self.push_notification(crate::plugin::HostNotification::UnitSelectionChanged { unit_id })
        {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn notifyProgramListChange(&self, list_id: i32, program_index: i32) -> tresult {
        if self.push_notification(crate::plugin::HostNotification::ProgramListChanged {
            list_id,
            program_index: (program_index >= 0).then_some(program_index),
        }) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IUnitHandler2Trait for ComponentHandler {
    unsafe fn notifyUnitByBusChange(&self) -> tresult {
        if self.push_notification(crate::plugin::HostNotification::UnitByBusChanged) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

// Event List implementation
/// Cap on queued events. The input list's only drain is `process()`, which returns early while
/// the plugin isn't processing, so a host that sends MIDI to a stopped plugin would otherwise
/// grow this forever (and then dump every stale event into the first block once it starts). Far
/// above any single block's working set; same pre-reserve/drop-when-full policy as
/// `MAX_OUTPUT_MIDI`.
pub const MAX_QUEUED_EVENTS: usize = 4096;
const MAX_QUEUED_EVENT_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

pub struct HostEventList {
    pub events: Mutex<Vec<PluginEvent>>,
    payload_bytes: AtomicUsize,
}

impl HostEventList {
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        let events = self
            .events
            .lock()
            .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
        visitor.visit(crate::host_storage::HostStorageRef::vector(&events)?)?;
        for event in events.iter() {
            visit_event_payload(event, visitor)?;
        }
        Ok(())
    }

    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::with_capacity(MAX_QUEUED_EVENTS)),
            payload_bytes: AtomicUsize::new(0),
        }
    }

    pub fn clear(&self) {
        match self.events.lock() {
            Ok(mut events) => {
                events.clear();
                self.payload_bytes.store(0, Ordering::Relaxed);
                log::trace!("HostEventList: Cleared all events");
            }
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for clear");
            }
        }
    }

    /// Prove that the original ingress and retained chunks contain no logical
    /// event, without consuming either owner or waiting for its mutex. An
    /// anchored pure-audio call must not access this ingress again afterwards:
    /// late events belong to the next invocation, never this SDK input window.
    pub(super) fn require_empty_anchored_ingress(
        &self,
        retained: &[Option<PluginEvent>],
    ) -> Result<(), crate::error::NativeEventError> {
        use crate::error::NativeEventError as Failure;
        let queued = self.events.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
            std::sync::TryLockError::Poisoned(_) => Failure::StoragePoisoned,
        })?;
        if !queued.is_empty() || retained.iter().any(Option::is_some) {
            return Err(Failure::PendingEvents);
        }
        Ok(())
    }

    /// Begin the checked Note operation in the original chunk owner. Rejected
    /// input leaves both owners unchanged; no separate pending producer is made.
    pub(super) fn begin_checked_note_batch(
        &self,
        out: &mut Vec<Option<PluginEvent>>,
        batch: &[PluginEvent],
    ) -> Result<(), crate::error::NativeEventError> {
        use crate::error::NativeEventError as Failure;
        let queued = self.events.lock().map_err(|_| Failure::StoragePoisoned)?;
        if !queued.is_empty() || !out.is_empty() || self.payload_bytes.load(Ordering::Relaxed) != 0
        {
            return Err(Failure::PendingEvents);
        }
        if batch.len() > MAX_QUEUED_EVENTS
            || batch.len() > queued.capacity()
            || batch.len() > out.capacity()
        {
            return Err(Failure::Capacity);
        }
        for (index, event) in batch.iter().enumerate() {
            if !matches!(
                event.data,
                PluginEventData::NoteOn { .. } | PluginEventData::NoteOff { .. }
            ) {
                return Err(Failure::InvalidEvent { index });
            }
        }
        // Only scalar Note variants passed validation. Clone cannot copy a
        // String/Vec payload, and both original capacities have been admitted.
        out.extend(batch.iter().cloned().map(Some));
        Ok(())
    }

    /// Stage exactly one native chunk, preserving equal-offset batch order.
    /// No clamp, silent prefix or backing allocation is permitted here.
    pub(super) fn stage_checked_note_chunk(
        &self,
        queued: &mut [Option<PluginEvent>],
        start: usize,
        frames: usize,
    ) -> Result<(), crate::error::NativeEventError> {
        use crate::error::NativeEventError as Failure;
        let end = start.checked_add(frames).ok_or(Failure::InvalidFrames)?;
        let belongs = |event: &PluginEvent| {
            event.sample_offset >= 0
                && (event.sample_offset as usize) >= start
                && (event.sample_offset as usize) < end
        };
        let mut native = self.events.lock().map_err(|_| Failure::StoragePoisoned)?;
        if !native.is_empty() {
            return Err(Failure::PendingEvents);
        }
        let count = queued
            .iter()
            .flatten()
            .filter(|event| belongs(event))
            .count();
        if count > MAX_QUEUED_EVENTS || count > native.capacity() {
            return Err(Failure::Capacity);
        }
        for slot in queued {
            if slot.as_ref().is_some_and(&belongs) {
                let mut event = slot.take().expect("matched occupied slot");
                event.sample_offset -= start as i32;
                native.push(event);
            }
        }
        Ok(())
    }

    /// Only after this checked operation began consumption. Even a poisoned
    /// owner must not replay accepted input on a later caller invocation.
    pub(super) fn clear_consumed_note_events(&self) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        events.clear();
        self.payload_bytes.store(0, Ordering::Relaxed);
    }

    pub(super) fn require_collected_output(&self) -> Result<(), crate::error::NativeOutputError> {
        use crate::error::NativeOutputError as Failure;
        let events = self.events.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
            std::sync::TryLockError::Poisoned(_) => Failure::StoragePoisoned,
        })?;
        if !events.is_empty() || self.payload_bytes.load(Ordering::Relaxed) != 0 {
            return Err(Failure::Pending);
        }
        Ok(())
    }

    pub(super) fn collect_process_output(
        &self,
        output: &crossbeam_queue::ArrayQueue<PluginEvent>,
    ) -> Result<(), crate::error::NativeOutputError> {
        use crate::error::NativeOutputError as Failure;
        let mut events = self.events.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
            std::sync::TryLockError::Poisoned(_) => Failure::StoragePoisoned,
        })?;
        for event in events.drain(..) {
            output.force_push(event);
        }
        self.payload_bytes.store(0, Ordering::Relaxed);
        Ok(())
    }

    /// Move queued events into reusable optional slots for allocation-free chunk routing.
    pub fn take_into_slots(&self, out: &mut Vec<Option<PluginEvent>>) {
        out.clear();
        if let Ok(mut events) = self.events.lock() {
            out.extend(events.drain(..).map(Some));
            self.payload_bytes.store(0, Ordering::Relaxed);
        }
    }

    /// Replace the queued events with `events`, reusing the existing allocation. Capped like
    /// every other path into the list; the excess is dropped with a warning.
    pub fn reset_with(&self, events: impl IntoIterator<Item = PluginEvent>) {
        let Ok(mut queued) = self.events.lock() else {
            log::error!("HostEventList: Failed to lock events for reset_with");
            return;
        };
        queued.clear();
        let mut payload_bytes = 0usize;
        for event in events {
            if queued.len() >= MAX_QUEUED_EVENTS {
                log::warn!("HostEventList: dropping event, queue full at {MAX_QUEUED_EVENTS}");
                break;
            }
            let next_payload_bytes = payload_bytes.saturating_add(event.payload_bytes());
            if next_payload_bytes > MAX_QUEUED_EVENT_PAYLOAD_BYTES {
                log::warn!(
                    "HostEventList: dropping event, payload budget exceeds \
                     {MAX_QUEUED_EVENT_PAYLOAD_BYTES} bytes"
                );
                continue;
            }
            payload_bytes = next_payload_bytes;
            queued.push(event);
        }
        self.payload_bytes.store(payload_bytes, Ordering::Relaxed);
    }

    /// True if the list currently holds no events.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.events
            .lock()
            .map(|events| events.is_empty())
            .unwrap_or(true)
    }

    /// Move each queued event into `f`, leaving the list empty while retaining its backing
    /// allocation. Pointer-backed payloads therefore cross into the output queue without a
    /// second allocation or byte copy on the audio thread.
    #[cfg(test)]
    pub fn drain_each(&self, mut f: impl FnMut(PluginEvent)) {
        if let Ok(mut events) = self.events.lock() {
            for event in events.drain(..) {
                f(event);
            }
            self.payload_bytes.store(0, Ordering::Relaxed);
        }
    }

    pub fn add_event(&self, event: PluginEvent) {
        match self.events.lock() {
            Ok(mut events) => {
                if events.len() >= MAX_QUEUED_EVENTS {
                    log::warn!(
                        "HostEventList: dropping event, queue full at {MAX_QUEUED_EVENTS} \
                         (is the plugin processing?)"
                    );
                    return;
                }
                let queued_payload_bytes = self.payload_bytes.load(Ordering::Relaxed);
                if queued_payload_bytes.saturating_add(event.payload_bytes())
                    > MAX_QUEUED_EVENT_PAYLOAD_BYTES
                {
                    log::warn!(
                        "HostEventList: dropping event, payload budget exceeds \
                         {MAX_QUEUED_EVENT_PAYLOAD_BYTES} bytes"
                    );
                    return;
                }
                self.payload_bytes.store(
                    queued_payload_bytes + event.payload_bytes(),
                    Ordering::Relaxed,
                );
                events.push(event);
                log::trace!(
                    "HostEventList: Added event via add_event, total count: {}",
                    events.len()
                );
            }
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for add_event");
            }
        }
    }

    /// Deep-copy a raw SDK event into the owned list.
    pub fn add_raw_event(&self, event: &Event) -> bool {
        let Ok(event) = (unsafe { raw_event_to_plugin_event(event) }) else {
            return false;
        };
        self.add_event(event);
        true
    }
}

/// A persistent, genuinely ref-counted read-only view of one checked call.
/// Its only event backing is transferred from the original chunk owner; no
/// guard crosses native code and no operation waits for this mutex.
pub(super) struct ActiveNoteEventList {
    storage: Mutex<ActiveNoteEvents>,
    exposed: AtomicBool,
    first_fault: AtomicU32,
}
struct ActiveNoteEvents {
    events: Vec<Option<PluginEvent>>,
    first: usize,
    end: usize,
    chunk_start: i32,
}
impl ActiveNoteEventList {
    pub(super) fn new() -> Self {
        Self {
            storage: Mutex::new(ActiveNoteEvents {
                events: Vec::new(),
                first: 0,
                end: 0,
                chunk_start: 0,
            }),
            exposed: AtomicBool::new(false),
            first_fault: AtomicU32::new(0),
        }
    }
    fn latch(&self, fault: crate::error::NativeEventError) {
        let code = if fault == crate::error::NativeEventError::StorageBusy {
            1
        } else {
            2
        };
        let _ = self
            .first_fault
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
    }
    pub(super) fn require_quiescent(&self) -> Result<(), crate::error::NativeEventError> {
        use crate::error::NativeEventError as Failure;
        match self.first_fault.load(Ordering::Acquire) {
            1 => Err(Failure::StorageBusy),
            2 => Err(Failure::StoragePoisoned),
            _ if self.exposed.load(Ordering::Acquire) => Err(Failure::PendingEvents),
            _ => Ok(()),
        }
    }
    pub(super) fn require_empty_for_parameter_flush(
        &self,
    ) -> Result<(), crate::error::NativeEventError> {
        use crate::error::NativeEventError as Failure;
        self.require_quiescent()?;
        let storage = self.storage.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
            std::sync::TryLockError::Poisoned(_) => Failure::StoragePoisoned,
        })?;
        if !storage.events.is_empty() || storage.events.capacity() != 0 {
            return Err(Failure::PendingEvents);
        }
        Ok(())
    }

    pub(super) fn begin(
        &self,
        retained: &mut Vec<Option<PluginEvent>>,
        batch: &[PluginEvent],
        reference: crate::audio::ProcessMusicalReference,
        call_start: i64,
        rate: u32,
    ) -> Result<(), crate::error::NativeEventError> {
        use crate::error::NativeEventError as Failure;
        self.require_quiescent()?;
        // A busy admission before consumption is recoverable; it does not latch.
        let mut storage = self.storage.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
            std::sync::TryLockError::Poisoned(_) => {
                self.latch(Failure::StoragePoisoned);
                Failure::StoragePoisoned
            }
        })?;
        if !storage.events.is_empty() || storage.events.capacity() != 0 {
            return Err(Failure::PendingEvents);
        }
        if batch.len() > MAX_QUEUED_EVENTS || batch.len() > retained.capacity() {
            return Err(Failure::Capacity);
        }
        // Whole-window/scalar validation precedes this method. Check every
        // absolute projection before mutating either original event owner.
        for (index, event) in batch.iter().enumerate() {
            let sample = call_start
                .checked_add(i64::from(event.sample_offset))
                .ok_or(Failure::InvalidEvent { index })?;
            reference
                .quarter_notes_at(sample, rate)
                .map_err(|_| Failure::InvalidEvent { index })?;
        }
        retained.clear(); // Only empty slots passed ingress admission.
        for event in batch {
            let mut projected = event.clone(); // Admitted scalar Note only.
            projected.ppq_position = reference
                .quarter_notes_at(call_start + i64::from(event.sample_offset), rate)
                .expect("the same immutable absolute event projection passed preflight");
            retained.push(Some(projected));
        }
        std::mem::swap(retained, &mut storage.events);
        Ok(())
    }
    pub(super) fn expose_chunk(
        &self,
        start: usize,
        frames: usize,
    ) -> Result<(), crate::error::NativeEventError> {
        use crate::error::NativeEventError as Failure;
        self.require_quiescent()?;
        let end = start.checked_add(frames).ok_or(Failure::InvalidFrames)?;
        let mut storage = self.storage.try_lock().map_err(|error| {
            let fault = match error {
                std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
                std::sync::TryLockError::Poisoned(_) => Failure::StoragePoisoned,
            };
            self.latch(fault);
            fault
        })?;
        storage.first = storage.events.partition_point(|event| {
            event
                .as_ref()
                .is_some_and(|event| (event.sample_offset as usize) < start)
        });
        storage.end = storage.events.partition_point(|event| {
            event
                .as_ref()
                .is_some_and(|event| (event.sample_offset as usize) < end)
        });
        storage.chunk_start = i32::try_from(start).map_err(|_| Failure::InvalidFrames)?;
        Ok(())
    }
    pub(super) fn open(&self) {
        self.exposed.store(true, Ordering::Release);
    }
    pub(super) fn close(&self) {
        self.exposed.store(false, Ordering::Release);
    }
    pub(super) fn finish(
        &self,
        retained: &mut Vec<Option<PluginEvent>>,
    ) -> Result<(), crate::error::NativeEventError> {
        use crate::error::NativeEventError as Failure;
        self.close();
        let (mut storage, poison) = match self.storage.try_lock() {
            Ok(storage) => (storage, false),
            Err(std::sync::TryLockError::Poisoned(error)) => {
                self.latch(Failure::StoragePoisoned);
                (error.into_inner(), true)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                // The closed COM owner retains the real backing. All future
                // processing, including None, fails closed without replay or
                // allocating another chunk buffer around the retained owner.
                self.latch(Failure::StorageBusy);
                return Err(Failure::StorageBusy);
            }
        };
        storage.events.clear();
        storage.first = 0;
        storage.end = 0;
        std::mem::swap(retained, &mut storage.events);
        if poison {
            Err(Failure::StoragePoisoned)
        } else {
            self.require_quiescent()
        }
    }
    #[cfg(test)]
    pub(super) fn test_finish_while_busy(
        &self,
        retained: &mut Vec<Option<PluginEvent>>,
    ) -> Result<(), crate::error::NativeEventError> {
        let _guard = self.storage.try_lock().unwrap();
        self.finish(retained)
    }
    #[cfg(test)]
    pub(super) fn test_busy_query(&self) {
        let _guard = self.storage.try_lock().unwrap();
        assert_eq!(unsafe { self.getEventCount() }, 0);
    }
    pub(super) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::{HostStorageError, HostStorageRef};
        let storage = self.storage.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => HostStorageError::NonExclusiveSharedOwner,
            std::sync::TryLockError::Poisoned(_) => HostStorageError::PoisonedOwner,
        })?;
        visitor.visit(HostStorageRef::vector(&storage.events)?)
    }
}
impl Class for ActiveNoteEventList {
    type Interfaces = (IEventList,);
}
impl IEventListTrait for ActiveNoteEventList {
    unsafe fn getEventCount(&self) -> i32 {
        if !self.exposed.load(Ordering::Acquire) || self.first_fault.load(Ordering::Acquire) != 0 {
            return 0;
        }
        match self.storage.try_lock() {
            Ok(storage) if self.exposed.load(Ordering::Acquire) => {
                (storage.end - storage.first) as i32
            }
            Ok(_) => 0,
            Err(error) => {
                self.latch(match error {
                    std::sync::TryLockError::WouldBlock => {
                        crate::error::NativeEventError::StorageBusy
                    }
                    std::sync::TryLockError::Poisoned(_) => {
                        crate::error::NativeEventError::StoragePoisoned
                    }
                });
                0
            }
        }
    }
    unsafe fn getEvent(&self, index: i32, event: *mut Event) -> i32 {
        if event.is_null()
            || index < 0
            || !self.exposed.load(Ordering::Acquire)
            || self.first_fault.load(Ordering::Acquire) != 0
        {
            return kResultFalse;
        }
        let storage = match self.storage.try_lock() {
            Ok(storage) => storage,
            Err(error) => {
                self.latch(match error {
                    std::sync::TryLockError::WouldBlock => {
                        crate::error::NativeEventError::StorageBusy
                    }
                    std::sync::TryLockError::Poisoned(_) => {
                        crate::error::NativeEventError::StoragePoisoned
                    }
                });
                return kResultFalse;
            }
        };
        if !self.exposed.load(Ordering::Acquire) || index as usize >= storage.end - storage.first {
            return kResultFalse;
        }
        let Some(source) = storage.events[storage.first + index as usize].as_ref() else {
            return kResultFalse;
        };
        let Ok(mut raw) = plugin_event_to_raw(source) else {
            return kResultFalse;
        };
        raw.sampleOffset -= storage.chunk_start;
        // SAFETY: the SDK caller supplied a non-null writable Event destination.
        unsafe {
            *event = raw;
        }
        kResultOk
    }
    unsafe fn addEvent(&self, _: *mut Event) -> i32 {
        kResultFalse
    }
}

impl Default for HostEventList {
    fn default() -> Self {
        Self::new()
    }
}

impl Class for HostEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for HostEventList {
    unsafe fn getEventCount(&self) -> i32 {
        match self.events.lock() {
            Ok(events) => events.len() as i32,
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for getEventCount");
                0
            }
        }
    }

    unsafe fn getEvent(&self, index: i32, event: *mut Event) -> i32 {
        if event.is_null() {
            log::warn!("HostEventList: getEvent called with null event pointer");
            return kResultFalse;
        }

        if index < 0 {
            log::warn!(
                "HostEventList: getEvent called with negative index: {}",
                index
            );
            return kResultFalse;
        }

        match self.events.lock() {
            Ok(events) => {
                if let Some(e) = events.get(index as usize) {
                    match plugin_event_to_raw(e) {
                        Ok(raw) => {
                            *event = raw;
                            kResultOk
                        }
                        Err(()) => kResultFalse,
                    }
                } else {
                    log::warn!(
                        "HostEventList: getEvent index {} out of bounds (count: {})",
                        index,
                        events.len()
                    );
                    kResultFalse
                }
            }
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for getEvent");
                kResultFalse
            }
        }
    }

    unsafe fn addEvent(&self, event: *mut Event) -> i32 {
        if event.is_null() {
            log::warn!("HostEventList: addEvent called with null event pointer");
            return kResultFalse;
        }

        match self.events.lock() {
            Ok(mut events) => {
                // Bound what a plugin can emit into the output list in a single block, so a
                // misbehaving plugin can't drive unbounded growth from inside `process()`.
                if events.len() >= MAX_QUEUED_EVENTS {
                    log::warn!(
                        "HostEventList: dropping plugin event, queue full at {MAX_QUEUED_EVENTS}"
                    );
                    return kResultFalse;
                }
                let owned = match raw_event_to_plugin_event(&*event) {
                    Ok(event) => event,
                    Err(()) => return kResultFalse,
                };
                let queued_payload_bytes = self.payload_bytes.load(Ordering::Relaxed);
                if queued_payload_bytes.saturating_add(owned.payload_bytes())
                    > MAX_QUEUED_EVENT_PAYLOAD_BYTES
                {
                    log::warn!(
                        "HostEventList: dropping plugin event, payload budget exceeds \
                         {MAX_QUEUED_EVENT_PAYLOAD_BYTES} bytes"
                    );
                    return kResultFalse;
                }
                self.payload_bytes.store(
                    queued_payload_bytes + owned.payload_bytes(),
                    Ordering::Relaxed,
                );
                events.push(owned);
                log::trace!("HostEventList: Added event, total count: {}", events.len());
                kResultOk
            }
            Err(_) => {
                log::error!("HostEventList: Failed to lock events for addEvent");
                kResultFalse
            }
        }
    }
}

#[allow(non_upper_case_globals, clippy::unnecessary_cast)]
fn plugin_event_to_raw(event: &PluginEvent) -> std::result::Result<Event, ()> {
    use Event_::EventTypes_::*;

    let mut raw: Event = unsafe { std::mem::zeroed() };
    raw.busIndex = event.bus_index;
    raw.sampleOffset = event.sample_offset;
    raw.ppqPosition = event.ppq_position;
    raw.flags = event.flags;
    match &event.data {
        PluginEventData::NoteOn {
            channel,
            pitch,
            tuning,
            velocity,
            length,
            note_id,
        } => {
            raw.r#type = kNoteOnEvent as u16;
            raw.__field0.noteOn = NoteOnEvent {
                channel: *channel,
                pitch: *pitch,
                tuning: *tuning,
                velocity: *velocity,
                length: *length,
                noteId: *note_id,
            };
        }
        PluginEventData::NoteOff {
            channel,
            pitch,
            velocity,
            note_id,
            tuning,
        } => {
            raw.r#type = kNoteOffEvent as u16;
            raw.__field0.noteOff = NoteOffEvent {
                channel: *channel,
                pitch: *pitch,
                velocity: *velocity,
                noteId: *note_id,
                tuning: *tuning,
            };
        }
        PluginEventData::Data { data_type, bytes } => {
            let size = u32::try_from(bytes.len()).map_err(|_| ())?;
            raw.r#type = kDataEvent as u16;
            raw.__field0.data = DataEvent {
                size,
                r#type: *data_type,
                bytes: bytes.as_ptr(),
            };
        }
        PluginEventData::PolyPressure {
            channel,
            pitch,
            pressure,
            note_id,
        } => {
            raw.r#type = kPolyPressureEvent as u16;
            raw.__field0.polyPressure = PolyPressureEvent {
                channel: *channel,
                pitch: *pitch,
                pressure: *pressure,
                noteId: *note_id,
            };
        }
        PluginEventData::NoteExpressionValue {
            type_id,
            note_id,
            value,
        } => {
            raw.r#type = kNoteExpressionValueEvent as u16;
            raw.__field0.noteExpressionValue = NoteExpressionValueEvent {
                typeId: *type_id,
                noteId: *note_id,
                value: *value,
            };
        }
        PluginEventData::NoteExpressionText {
            type_id,
            note_id,
            text,
        } => {
            raw.r#type = kNoteExpressionTextEvent as u16;
            raw.__field0.noteExpressionText = NoteExpressionTextEvent {
                typeId: *type_id,
                noteId: *note_id,
                textLen: u32::try_from(text.len()).map_err(|_| ())?,
                text: text.as_ptr(),
            };
        }
        PluginEventData::NoteExpressionIntValue {
            type_id,
            note_id,
            value,
        } => {
            raw.r#type = kNoteExpressionIntValueEvent as u16;
            raw.__field0.noteExpressionIntValue = NoteExpressionIntValueEvent {
                typeId: *type_id,
                noteId: *note_id,
                value: *value,
            };
        }
        PluginEventData::Chord {
            root,
            bass_note,
            mask,
            text,
        } => {
            raw.r#type = kChordEvent as u16;
            raw.__field0.chord = ChordEvent {
                root: *root,
                bassNote: *bass_note,
                mask: *mask,
                textLen: u16::try_from(text.len()).map_err(|_| ())?,
                text: text.as_ptr(),
            };
        }
        PluginEventData::Scale { root, mask, text } => {
            raw.r#type = kScaleEvent as u16;
            raw.__field0.scale = ScaleEvent {
                root: *root,
                mask: *mask,
                textLen: u16::try_from(text.len()).map_err(|_| ())?,
                text: text.as_ptr(),
            };
        }
        PluginEventData::LegacyMidiCcOut {
            control_number,
            channel,
            value,
            value2,
        } => {
            raw.r#type = kLegacyMIDICCOutEvent as u16;
            // VST3's `int8` is `c_char`, which is signed on macOS/x86 and unsigned on ARM
            // Linux, so these fields must be cast rather than assigned.
            raw.__field0.midiCCOut = LegacyMIDICCOutEvent {
                controlNumber: *control_number,
                channel: *channel as c_char,
                value: *value as c_char,
                value2: *value2 as c_char,
            };
        }
    }
    Ok(raw)
}

#[allow(non_upper_case_globals, clippy::unnecessary_cast)]
unsafe fn raw_event_to_plugin_event(raw: &Event) -> std::result::Result<PluginEvent, ()> {
    use Event_::EventTypes_::*;

    unsafe fn copy_bytes(ptr: *const u8, len: usize) -> std::result::Result<Vec<u8>, ()> {
        if len > MAX_EVENT_PAYLOAD_BYTES || (len != 0 && ptr.is_null()) {
            return Err(());
        }
        Ok(if len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
        })
    }

    unsafe fn copy_text(ptr: *const u16, len: usize) -> std::result::Result<Vec<u16>, ()> {
        if len > MAX_EVENT_TEXT_UNITS || (len != 0 && ptr.is_null()) {
            return Err(());
        }
        Ok(if len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
        })
    }

    let data = unsafe {
        match raw.r#type as u32 {
            t if t == kNoteOnEvent as u32 => {
                let value = raw.__field0.noteOn;
                PluginEventData::NoteOn {
                    channel: value.channel,
                    pitch: value.pitch,
                    tuning: value.tuning,
                    velocity: value.velocity,
                    length: value.length,
                    note_id: value.noteId,
                }
            }
            t if t == kNoteOffEvent as u32 => {
                let value = raw.__field0.noteOff;
                PluginEventData::NoteOff {
                    channel: value.channel,
                    pitch: value.pitch,
                    velocity: value.velocity,
                    note_id: value.noteId,
                    tuning: value.tuning,
                }
            }
            t if t == kDataEvent as u32 => {
                let value = raw.__field0.data;
                PluginEventData::Data {
                    data_type: value.r#type,
                    bytes: copy_bytes(value.bytes, usize::try_from(value.size).map_err(|_| ())?)?,
                }
            }
            t if t == kPolyPressureEvent as u32 => {
                let value = raw.__field0.polyPressure;
                PluginEventData::PolyPressure {
                    channel: value.channel,
                    pitch: value.pitch,
                    pressure: value.pressure,
                    note_id: value.noteId,
                }
            }
            t if t == kNoteExpressionValueEvent as u32 => {
                let value = raw.__field0.noteExpressionValue;
                PluginEventData::NoteExpressionValue {
                    type_id: value.typeId,
                    note_id: value.noteId,
                    value: value.value,
                }
            }
            t if t == kNoteExpressionTextEvent as u32 => {
                let value = raw.__field0.noteExpressionText;
                PluginEventData::NoteExpressionText {
                    type_id: value.typeId,
                    note_id: value.noteId,
                    text: copy_text(value.text, usize::try_from(value.textLen).map_err(|_| ())?)?,
                }
            }
            t if t == kNoteExpressionIntValueEvent as u32 => {
                let value = raw.__field0.noteExpressionIntValue;
                PluginEventData::NoteExpressionIntValue {
                    type_id: value.typeId,
                    note_id: value.noteId,
                    value: value.value,
                }
            }
            t if t == kChordEvent as u32 => {
                let value = raw.__field0.chord;
                PluginEventData::Chord {
                    root: value.root,
                    bass_note: value.bassNote,
                    mask: value.mask,
                    text: copy_text(value.text, usize::from(value.textLen))?,
                }
            }
            t if t == kScaleEvent as u32 => {
                let value = raw.__field0.scale;
                PluginEventData::Scale {
                    root: value.root,
                    mask: value.mask,
                    text: copy_text(value.text, usize::from(value.textLen))?,
                }
            }
            t if t == kLegacyMIDICCOutEvent as u32 => {
                let value = raw.__field0.midiCCOut;
                PluginEventData::LegacyMidiCcOut {
                    control_number: value.controlNumber,
                    channel: value.channel as i8,
                    value: value.value as u8,
                    value2: value.value2 as u8,
                }
            }
            _ => return Err(()),
        }
    };
    Ok(PluginEvent {
        bus_index: raw.busIndex,
        sample_offset: raw.sampleOffset,
        ppq_position: raw.ppqPosition,
        flags: raw.flags,
        data,
    })
}

pub fn create_event_list() -> ComWrapper<HostEventList> {
    ComWrapper::new(HostEventList::new())
}

// Parameter Changes implementation
pub struct ParameterChanges {
    /// A pool of queue objects. Entries `[0, used)` are active this block; entries `[used, len)`
    /// are recycled — kept allocated and reused across blocks rather than dropped, so the
    /// steady-state audio path never allocates a `ComWrapper`. The pool only ever grows, bounded
    /// by the number of distinct parameters changed within a single block.
    pub queues: Mutex<Vec<ComWrapper<ParameterValueQueue>>>,
    /// Number of active queues this block (`<= queues.len()`). Mutated only under the `queues`
    /// lock, so the pair stays consistent.
    used: AtomicUsize,
}

impl Default for ParameterChanges {
    fn default() -> Self {
        Self {
            queues: Mutex::new(Vec::new()),
            used: AtomicUsize::new(0),
        }
    }
}

/// Unpublished native view of one original pending Vec. This is temporary
/// cold projection storage, not a second pending-input authority or RT guarantee.
/// ComWrapper::new retains its standard OOM-abort behavior; Vec reservations
/// alone have typed recoverable failure.
pub(super) struct PendingParameterProjection {
    queues: Vec<ComWrapper<ParameterValueQueue>>,
    used: usize,
}
impl PendingParameterProjection {
    pub(super) fn prepare(
        values: &[crate::parameters::ParameterChange],
        limit: usize,
    ) -> Result<Self, crate::plugin::PendingParameterFlushRefusal> {
        use crate::plugin::PendingParameterFlushRefusal as Failure;
        let mut queues = Vec::new();
        queues
            .try_reserve_exact(values.len())
            .map_err(|_| Failure::StorageCapacity)?;
        if queues.capacity() > limit {
            return Err(Failure::StorageCapacity);
        }
        for (index, value) in values.iter().enumerate() {
            if values[..index].iter().any(|prior| prior.id == value.id) {
                continue;
            }
            // Preserve first-ID queue order and the old enqueue operation's
            // last-value override for duplicate points at the same offset zero.
            let value = values[index..]
                .iter()
                .rev()
                .find(|last| last.id == value.id)
                .unwrap();
            let mut points = Vec::new();
            points
                .try_reserve_exact(1)
                .map_err(|_| Failure::StorageCapacity)?;
            if points.capacity() > 1 {
                return Err(Failure::StorageCapacity);
            }
            points.push((0, value.value));
            queues.push(ComWrapper::new(ParameterValueQueue {
                param_id: AtomicU32::new(value.id),
                points: Mutex::new(points),
                origin: AtomicU8::new(ParameterQueueOrigin::Ordinary as u8),
            }));
        }
        let used = queues.len();
        Ok(Self { queues, used })
    }
}

fn pending_flush_lock_refusal<T>(
    error: std::sync::TryLockError<T>,
) -> crate::plugin::PendingParameterFlushRefusal {
    match error {
        std::sync::TryLockError::WouldBlock => {
            crate::plugin::PendingParameterFlushRefusal::StorageBusy
        }
        std::sync::TryLockError::Poisoned(_) => {
            crate::plugin::PendingParameterFlushRefusal::StoragePoisoned
        }
    }
}

impl ParameterChanges {
    /// Health-only admission also applies to an empty host Vec. Legitimate
    /// unconsumed Source values/feedback still belong to the old replay path;
    /// an already active COM producer never does, even with zero point count.
    pub(super) fn require_empty_pending_flush_input(
        &self,
        handler: Option<&ComponentHandler>,
    ) -> Result<(), crate::plugin::PendingParameterFlushRefusal> {
        use crate::plugin::PendingParameterFlushRefusal as Failure;
        let source = handler
            .map(|handler| handler.editor_events.try_lock())
            .transpose()
            .map_err(pending_flush_lock_refusal)?;
        let cohort = handler.map(ComponentHandler::pending_restart_cohort);
        let _feedback = handler
            .map(|handler| handler.parameter_changes.try_lock())
            .transpose()
            .map_err(pending_flush_lock_refusal)?;
        let _queues = self.queues.try_lock().map_err(pending_flush_lock_refusal)?;
        if source
            .as_ref()
            .is_some_and(|source| source.failure.is_some())
        {
            return Err(Failure::InvalidSource);
        }
        if self.used.load(Ordering::Relaxed) != 0 {
            return Err(Failure::ExistingInput);
        }
        if handler.map(ComponentHandler::pending_restart_cohort) != cohort {
            return Err(Failure::SourceChanged);
        }
        if cohort
            .is_some_and(|cohort| cohort.exhausted() || !matches!(cohort.flags().bits(), 0 | 4))
        {
            return Err(Failure::RestartRequired);
        }
        Ok(())
    }

    /// An empty host Vec can also have an actual initial handoff waiting in
    /// these original COM queues. No other active COM producer gains this right.
    /// With no active queues, only health is checked so old Source replay remains
    /// responsible for legitimate Value callbacks and feedback.
    pub(super) fn initial_pending_flush_input(
        &self,
        handler: Option<&ComponentHandler>,
    ) -> Result<bool, crate::plugin::PendingParameterFlushRefusal> {
        use crate::plugin::{
            NativeEditorEvent, ParameterEditKind, PendingParameterFlushRefusal as Failure,
        };
        let source = handler
            .map(|handler| handler.editor_events.try_lock())
            .transpose()
            .map_err(pending_flush_lock_refusal)?;
        let cohort = handler.map(ComponentHandler::pending_restart_cohort);
        let feedback = handler
            .map(|handler| handler.parameter_changes.try_lock())
            .transpose()
            .map_err(pending_flush_lock_refusal)?;
        let queues = self.queues.try_lock().map_err(pending_flush_lock_refusal)?;
        if source
            .as_ref()
            .is_some_and(|source| source.failure.is_some())
        {
            return Err(Failure::InvalidSource);
        }
        let used = self.used.load(Ordering::Relaxed);
        if used > queues.len() || used > crate::initial_parameter_input::MAX_INITIAL_PARAMETERS {
            return Err(Failure::ExistingInput);
        }
        if used != 0 {
            for (index, queue) in queues[..used].iter().enumerate() {
                let points = queue
                    .points
                    .try_lock()
                    .map_err(pending_flush_lock_refusal)?;
                if !queue.has_initial_origin()
                    || points.len() != 1
                    || points[0].0 != 0
                    || !points[0].1.is_finite()
                    || !(0.0..=1.0).contains(&points[0].1)
                    || queues[..index]
                        .iter()
                        .any(|prior| prior.param_id() == queue.param_id())
                {
                    return Err(Failure::ExistingInput);
                }
            }
            let source = source.as_ref().ok_or(Failure::InvalidSource)?;
            if source.events.iter().any(|event| {
                matches!(event,
                NativeEditorEvent::Parameter(edit) if edit.kind == ParameterEditKind::ValueChange)
            }) {
                return Err(Failure::SourceValuesPending);
            }
            if feedback
                .as_ref()
                .is_some_and(|feedback| !feedback.is_empty())
            {
                return Err(Failure::DspFeedbackPending);
            }
            // A retained addPoint invalidates before acquiring its point lock.
            // This is not an atomicity claim for writes after final admission.
            if queues[..used]
                .iter()
                .any(|queue| !queue.has_initial_origin())
            {
                return Err(Failure::ExistingInput);
            }
        }
        if handler.map(ComponentHandler::pending_restart_cohort) != cohort {
            return Err(Failure::SourceChanged);
        }
        if cohort
            .is_some_and(|cohort| cohort.exhausted() || !matches!(cohort.flags().bits(), 0 | 4))
        {
            return Err(Failure::RestartRequired);
        }
        Ok(used != 0)
    }

    /// With None, inspect only. With Some, all fallible checks finish before
    /// the pool swap and active-count publication. The caller retains the
    /// displaced inactive pool until every original lock below has ended.
    pub(super) fn pending_flush_input(
        &self,
        handler: Option<&ComponentHandler>,
        candidate: Option<&mut PendingParameterProjection>,
    ) -> Result<(), crate::plugin::PendingParameterFlushRefusal> {
        use crate::plugin::{
            NativeEditorEvent, ParameterEditKind, PendingParameterFlushRefusal as Failure,
        };
        let source = handler
            .map(|handler| handler.editor_events.try_lock())
            .transpose()
            .map_err(pending_flush_lock_refusal)?;
        let cohort = handler.map(ComponentHandler::pending_restart_cohort);
        let feedback = handler
            .map(|handler| handler.parameter_changes.try_lock())
            .transpose()
            .map_err(pending_flush_lock_refusal)?;
        let mut queues = self.queues.try_lock().map_err(pending_flush_lock_refusal)?;
        if let Some(source) = &source {
            if source.failure.is_some() {
                return Err(Failure::InvalidSource);
            }
            if source.events.iter().any(|event| {
                matches!(event,
                NativeEditorEvent::Parameter(edit) if edit.kind == ParameterEditKind::ValueChange)
            }) {
                return Err(Failure::SourceValuesPending);
            }
        }
        if feedback
            .as_ref()
            .is_some_and(|feedback| !feedback.is_empty())
        {
            return Err(Failure::DspFeedbackPending);
        }
        // Even a used queue with zero points belongs to another producer.
        if self.used.load(Ordering::Relaxed) != 0 {
            return Err(Failure::ExistingInput);
        }
        if handler.map(ComponentHandler::pending_restart_cohort) != cohort {
            return Err(Failure::SourceChanged);
        }
        if cohort
            .is_some_and(|cohort| cohort.exhausted() || !matches!(cohort.flags().bits(), 0 | 4))
        {
            return Err(Failure::RestartRequired);
        }
        if let Some(candidate) = candidate {
            std::mem::swap(&mut *queues, &mut candidate.queues);
            self.used.store(candidate.used, Ordering::Relaxed);
        }
        Ok(())
    }

    pub(super) fn require_collected_output(&self) -> Result<(), crate::error::NativeOutputError> {
        use crate::error::NativeOutputError as Failure;
        let queues = self.queues.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
            std::sync::TryLockError::Poisoned(_) => Failure::StoragePoisoned,
        })?;
        let used = self.used.load(Ordering::Relaxed);
        if used > queues.len() {
            return Err(Failure::InvalidOwner);
        }
        if used != 0 {
            return Err(Failure::Pending);
        }
        Ok(())
    }

    pub(super) fn collect_process_output(
        &self,
        output: &crossbeam_queue::ArrayQueue<(u32, f64)>,
    ) -> Result<(), crate::error::NativeOutputError> {
        use crate::error::NativeOutputError as Failure;
        let queues = self.queues.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
            std::sync::TryLockError::Poisoned(_) => Failure::StoragePoisoned,
        })?;
        let used = self.used.load(Ordering::Relaxed);
        if used > queues.len() {
            return Err(Failure::InvalidOwner);
        }
        for queue in &queues[..used] {
            let points = queue.points.try_lock().map_err(|error| match error {
                std::sync::TryLockError::WouldBlock => Failure::StorageBusy,
                std::sync::TryLockError::Poisoned(_) => Failure::StoragePoisoned,
            })?;
            for &(_offset, value) in points.iter() {
                output.force_push((queue.param_id(), value));
            }
        }
        // Failure above keeps the original active output owner visible. Every
        // later processing entrance refuses it before it can be overwritten.
        self.used.store(0, Ordering::Relaxed);
        Ok(())
    }
    pub(crate) fn has_active_points(&self) -> crate::Result<bool> {
        let queues = self.queues.lock().map_err(|_| {
            crate::Error::HostStorage(crate::host_storage::HostStorageError::PoisonedOwner)
        })?;
        let used = self.used.load(Ordering::Relaxed);
        if used > queues.len() {
            return Err(crate::Error::HostStorage(
                crate::host_storage::HostStorageError::PoisonedOwner,
            ));
        }
        for queue in &queues[..used] {
            if !queue
                .points
                .lock()
                .map_err(|_| {
                    crate::Error::HostStorage(crate::host_storage::HostStorageError::PoisonedOwner)
                })?
                .is_empty()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        let queues = self
            .queues
            .lock()
            .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
        visitor.visit(crate::host_storage::HostStorageRef::vector(&queues)?)?;
        // Recycled inactive queues still own their Arc and point capacity.
        for queue in queues.iter() {
            visitor.visit(crate::host_storage::HostStorageRef::com(queue))?;
            queue.visit_owned_storage(visitor)?;
        }
        Ok(())
    }

    /// Host-side: queue a parameter change point for the next process block. The processor
    /// reads these from `inputParameterChanges` during `process()`. Points for the same id
    /// share one queue and are kept ordered by sample offset. Reuses a pooled queue object
    /// rather than allocating, so this is allocation-free in steady state once the pool has
    /// grown to the per-block working set.
    pub fn enqueue(&self, id: u32, sample_offset: i32, value: f64) {
        if let Ok(mut queues) = self.queues.lock() {
            let used = self.used.load(Ordering::Relaxed);
            // Merge into an already-active queue for this id (e.g. several offsets in one block).
            if let Some(q) = queues[..used].iter().find(|q| q.param_id() == id) {
                q.insert_point(sample_offset, value);
                return;
            }
            // Otherwise activate a slot: recycle a pooled queue if one exists, else grow once.
            if used < queues.len() {
                queues[used].reset(id);
                queues[used].insert_point(sample_offset, value);
            } else {
                let q = ComWrapper::new(ParameterValueQueue::new(id));
                q.insert_point(sample_offset, value);
                queues.push(q);
            }
            self.used.store(used + 1, Ordering::Relaxed);
        }
    }

    /// Forget all queued changes. Call after each `process()` block so values don't re-stick.
    /// Only resets the active count — inactive queues confer no initial-input
    /// right. Every reactivation invalidates their historical origin first.
    /// Queue objects/points remain retained, so this allocates and drops nothing.
    pub fn clear_all(&self) {
        self.used.store(0, Ordering::Relaxed);
    }

    /// Inspect the original active queues for storage and per-block reset tests.
    #[cfg(test)]
    pub fn for_each_active_point(&self, mut f: impl FnMut(u32, i32, f64)) {
        let queues = self.queues.lock().unwrap_or_else(|p| p.into_inner());
        let used = self.used.load(Ordering::Relaxed).min(queues.len());
        for queue in &queues[..used] {
            let id = queue.param_id();
            let points = queue.points.lock().unwrap_or_else(|p| p.into_inner());
            for &(offset, value) in points.iter() {
                f(id, offset, value);
            }
        }
    }
}

impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        match self.queues.lock() {
            Ok(_queues) => {
                // Only the active slots, not the recycled pool capacity.
                let count = self.used.load(Ordering::Relaxed) as i32;
                log::trace!(
                    "Internal ParameterChanges: getParameterCount returning {}",
                    count
                );
                count
            }
            Err(_) => {
                log::error!(
                    "Internal ParameterChanges: Failed to lock queues for getParameterCount"
                );
                0
            }
        }
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 {
            log::warn!(
                "Internal ParameterChanges: getParameterData called with negative index: {}",
                index
            );
            return ptr::null_mut();
        }

        match self.queues.lock() {
            Ok(queues) => {
                let used = self.used.load(Ordering::Relaxed);
                if (index as usize) < used {
                    let queue = &queues[index as usize];
                    match queue.as_com_ref::<IParamValueQueue>() {
                        Some(ptr) => {
                            log::trace!("Internal ParameterChanges: getParameterData returning queue for index {}", index);
                            ptr.as_ptr()
                        }
                        None => {
                            log::error!("Internal ParameterChanges: Failed to convert queue to COM pointer for index {}", index);
                            ptr::null_mut()
                        }
                    }
                } else {
                    log::warn!("Internal ParameterChanges: getParameterData index {} out of bounds (count: {})", index, used);
                    ptr::null_mut()
                }
            }
            Err(_) => {
                log::error!(
                    "Internal ParameterChanges: Failed to lock queues for getParameterData"
                );
                ptr::null_mut()
            }
        }
    }

    unsafe fn addParameterData(&self, id: *const u32, index: *mut i32) -> *mut IParamValueQueue {
        if id.is_null() {
            log::warn!("Internal ParameterChanges: addParameterData called with null id pointer");
            return ptr::null_mut();
        }

        let param_id = *id;

        match self.queues.lock() {
            Ok(mut queues) => {
                let used = self.used.load(Ordering::Relaxed);
                // Reuse an already-active queue for this parameter if present.
                for (i, queue) in queues[..used].iter().enumerate() {
                    if queue.param_id() == param_id {
                        queue.invalidate_initial_origin();
                        if !index.is_null() {
                            *index = i as i32;
                        }
                        log::trace!(
                            "Internal ParameterChanges: Found existing queue for parameter {}",
                            param_id
                        );
                        return queue
                            .as_com_ref::<IParamValueQueue>()
                            .map(|ptr| ptr.as_ptr())
                            .unwrap_or_else(|| {
                                log::error!("Internal ParameterChanges: Failed to convert existing queue to COM pointer");
                                ptr::null_mut()
                            });
                    }
                }

                // Activate a slot: recycle a pooled queue if one exists, else grow once.
                if used == queues.len() {
                    queues.push(ComWrapper::new(ParameterValueQueue::new(param_id)));
                } else {
                    queues[used].reset(param_id);
                }
                let queue_ptr = queues[used]
                    .as_com_ref::<IParamValueQueue>()
                    .map(|ptr| ptr.as_ptr())
                    .unwrap_or_else(|| {
                        log::error!(
                            "Internal ParameterChanges: Failed to convert new queue to COM pointer"
                        );
                        ptr::null_mut()
                    });

                if !index.is_null() {
                    *index = used as i32;
                }

                self.used.store(used + 1, Ordering::Relaxed);
                log::trace!(
                    "Internal ParameterChanges: Activated queue for parameter {}, active count: {}",
                    param_id,
                    used + 1
                );
                queue_ptr
            }
            Err(_) => {
                log::error!(
                    "Internal ParameterChanges: Failed to lock queues for addParameterData"
                );
                ptr::null_mut()
            }
        }
    }
}

// One queue owns its provenance with its actual point projection. No parent
// batch flag, Source copy or retained back-pointer is needed.
#[repr(u8)]
enum ParameterQueueOrigin {
    Ordinary,
    InitialHandoff,
}

// Parameter Value Queue implementation
pub struct ParameterValueQueue {
    // Atomic so a pooled queue can be re-targeted to a different parameter (see `reset`) without
    // dropping and reallocating the ComWrapper each block.
    pub param_id: AtomicU32,
    pub points: Mutex<Vec<(i32, f64)>>, // sample offset, value
    origin: AtomicU8,
}

impl ParameterValueQueue {
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        let points = self
            .points
            .lock()
            .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
        visitor.visit(crate::host_storage::HostStorageRef::vector(&points)?)
    }

    pub fn new(param_id: u32) -> Self {
        Self {
            param_id: AtomicU32::new(param_id),
            points: Mutex::new(Vec::new()),
            origin: AtomicU8::new(ParameterQueueOrigin::Ordinary as u8),
        }
    }

    fn has_initial_origin(&self) -> bool {
        self.origin.load(Ordering::Acquire) == ParameterQueueOrigin::InitialHandoff as u8
    }

    fn invalidate_initial_origin(&self) {
        self.origin
            .store(ParameterQueueOrigin::Ordinary as u8, Ordering::Release);
    }

    /// Only the final successful initial publication may call this, while the
    /// original input pool is locked and before its active count is published.
    fn publish_initial_origin(&self) {
        self.origin.store(
            ParameterQueueOrigin::InitialHandoff as u8,
            Ordering::Release,
        );
    }

    /// This queue's current parameter id.
    pub fn param_id(&self) -> u32 {
        self.param_id.load(Ordering::Relaxed)
    }

    /// Re-target a pooled queue to a new parameter, clearing its points in place (keeping the
    /// `points` Vec's capacity). Used when recycling a queue object for a different parameter so
    /// the steady-state path allocates nothing.
    fn reset(&self, param_id: u32) {
        self.invalidate_initial_origin();
        self.param_id.store(param_id, Ordering::Relaxed);
        if let Ok(mut points) = self.points.lock() {
            points.clear();
        }
    }

    /// Insert a point keeping sample-offset order (safe host-side population, mirroring the
    /// COM `addPoint`).
    fn insert_point(&self, sample_offset: i32, value: f64) {
        self.invalidate_initial_origin();
        if let Ok(mut points) = self.points.lock() {
            // Later host input overrides an earlier initialization baseline.
            // The separate plugin COM addPoint method is unchanged.
            if let Some(point) = points.iter_mut().find(|(off, _)| *off == sample_offset) {
                point.1 = value;
            } else {
                let pos = points
                    .iter()
                    .position(|(off, _)| *off > sample_offset)
                    .unwrap_or(points.len());
                points.insert(pos, (sample_offset, value));
            }
        }
    }
}

impl Class for ParameterValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParameterValueQueue {
    unsafe fn getParameterId(&self) -> u32 {
        self.param_id.load(Ordering::Relaxed)
    }

    unsafe fn getPointCount(&self) -> i32 {
        // These run as COM FFI callbacks; a panic (e.g. from `.unwrap()` on a poisoned
        // lock) would unwind across the C++ boundary — UB. Recover the lock instead.
        self.points.lock().unwrap_or_else(|p| p.into_inner()).len() as i32
    }

    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut f64) -> i32 {
        if let Some((offset, val)) = self
            .points
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(index as usize)
        {
            if !sample_offset.is_null() {
                *sample_offset = *offset;
            }
            if !value.is_null() {
                *value = *val;
            }
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn addPoint(&self, sample_offset: i32, value: f64, index: *mut i32) -> i32 {
        self.invalidate_initial_origin();
        let mut points = self.points.lock().unwrap_or_else(|p| p.into_inner());

        // Find insertion point
        let insert_pos = points
            .iter()
            .position(|(offset, _)| *offset > sample_offset)
            .unwrap_or(points.len());

        points.insert(insert_pos, (sample_offset, value));

        if !index.is_null() {
            *index = insert_pos as i32;
        }

        kResultOk
    }
}

#[cfg(test)]
mod host_attr_tests {
    use super::*;

    #[test]
    fn attribute_list_round_trips_each_type() {
        let list = HostAttributeList::new();
        list.put("i".into(), AttrValue::Int(42));
        list.put("f".into(), AttrValue::Float(1.5));
        list.put("s".into(), AttrValue::Str(vec![72, 105])); // "Hi"
        list.put("b".into(), AttrValue::Bin(vec![1, 2, 3]));

        assert_eq!(list.get_value("i"), Some(AttrValue::Int(42)));
        assert_eq!(list.get_value("f"), Some(AttrValue::Float(1.5)));
        assert_eq!(list.get_value("s"), Some(AttrValue::Str(vec![72, 105])));
        assert_eq!(list.get_value("b"), Some(AttrValue::Bin(vec![1, 2, 3])));
        assert_eq!(list.get_value("missing"), None);
    }
}

#[cfg(test)]
mod component_handler_tests {
    use super::*;
    use crate::plugin::{
        ContextMenuItem, HostNotification, NativeEditorEvent, NativeEditorEventDrain,
        NativeEditorEventFailure, ParameterEdit, ParameterEditKind,
    };

    pub(super) fn complete_author_events(handler: &ComponentHandler) -> Vec<NativeEditorEvent> {
        let NativeEditorEventDrain::Complete { events } = handler.take_editor_events() else {
            panic!("fixture requires a complete native author source")
        };
        events
    }

    struct TestContextMenuTarget {
        calls: Arc<Mutex<Vec<i32>>>,
    }

    impl Class for TestContextMenuTarget {
        type Interfaces = (IContextMenuTarget,);
    }

    impl IContextMenuTargetTrait for TestContextMenuTarget {
        unsafe fn executeMenuItem(&self, tag: i32) -> tresult {
            self.calls
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(tag);
            kResultOk
        }
    }

    fn context_menu_item(name: &str, tag: i32, flags: i32) -> IContextMenuItem {
        let mut item = IContextMenuItem {
            name: [0; 128],
            tag,
            flags,
        };
        for (destination, source) in item.name.iter_mut().zip(name.encode_utf16()) {
            *destination = source;
        }
        item
    }

    #[test]
    fn captures_begin_perform_end_in_order_and_drains() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));

        // Drive a full gesture: mouse-down, two drag values, mouse-up.
        unsafe {
            handler.beginEdit(5);
            handler.performEdit(5, 0.25);
            handler.performEdit(5, 0.5);
            handler.endEdit(5);
        }

        let events = complete_author_events(&handler);
        assert_eq!(
            events,
            vec![
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 5,
                    kind: ParameterEditKind::BeginGesture,
                    value: None,
                }),
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 5,
                    kind: ParameterEditKind::ValueChange,
                    value: Some(0.25),
                }),
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 5,
                    kind: ParameterEditKind::ValueChange,
                    value: Some(0.5),
                }),
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 5,
                    kind: ParameterEditKind::EndGesture,
                    value: None,
                }),
            ]
        );

        // The drain empties the buffer; the value-change sink still mirrors the performEdits.
        assert!(complete_author_events(&handler).is_empty());
        assert_eq!(
            *handler.parameter_changes.lock().unwrap(),
            vec![(5, 0.25), (5, 0.5)]
        );
    }

    /// Both editor-feedback buffers are drained only by an optional host poll, so a host that
    /// never polls must not be able to grow them without bound — dragging a knob emits one
    /// `performEdit` (and one gesture event) per UI frame, for as long as the editor is open.
    #[test]
    fn editor_feedback_is_capped_when_the_host_never_polls() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));

        // Simulate a very long drag: far more edits than the cap, never polled.
        unsafe {
            for i in 0..(MAX_EDITOR_FEEDBACK * 2) {
                handler.performEdit(7, (i % 100) as f64 / 100.0);
            }
        }

        assert_eq!(
            handler.parameter_changes.lock().unwrap().len(),
            MAX_EDITOR_FEEDBACK,
            "the value-change sink must stop at the cap, not grow with the drag"
        );
        assert_eq!(
            handler.take_editor_events(),
            NativeEditorEventDrain::Incomplete {
                failure: NativeEditorEventFailure::Overflow
            }
        );
        assert_eq!(
            handler.take_editor_events(),
            NativeEditorEventDrain::Incomplete {
                failure: NativeEditorEventFailure::Overflow
            },
            "drain cannot pretend a dropped gesture became complete"
        );
        let source = handler.editor_events.lock().unwrap();
        assert!(source.events.is_empty());
        assert!(source.events.capacity() >= MAX_EDITOR_FEEDBACK);
    }

    #[test]
    fn handler2_requests_are_ordered_and_report_backpressure() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.setDirty(1), kResultOk);
            assert_eq!(handler.requestOpenEditor(c"editor".as_ptr()), kResultOk);
            assert_eq!(handler.startGroupEdit(), kResultOk);
            assert_eq!(handler.finishGroupEdit(), kResultOk);
        }
        assert_eq!(
            handler.take_host_notifications(),
            vec![HostNotification::OpenEditorRequested {
                name: Some("editor".to_string())
            }]
        );
        assert_eq!(
            complete_author_events(&handler),
            vec![
                NativeEditorEvent::DirtyChanged(true),
                NativeEditorEvent::GroupEditStarted,
                NativeEditorEvent::GroupEditFinished
            ]
        );
        unsafe {
            for _ in 0..MAX_HOST_NOTIFICATIONS {
                assert_eq!(handler.requestOpenEditor(ptr::null()), kResultOk);
            }
            assert_eq!(handler.requestOpenEditor(ptr::null()), kResultFalse);
        }
        assert_eq!(
            handler.take_host_notifications().len(),
            MAX_HOST_NOTIFICATIONS
        );
        assert!(
            complete_author_events(&handler).is_empty(),
            "control backpressure does not fabricate a native author edit"
        );
    }

    #[test]
    fn handler3_context_menu_preserves_items_and_executes_plugin_target() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        let handler_wrapper = ComWrapper::new(handler);
        assert!(
            handler_wrapper.as_com_ref::<IComponentHandler3>().is_some(),
            "controllers must be able to query IComponentHandler3"
        );

        let calls = Arc::new(Mutex::new(Vec::new()));
        let target = ComWrapper::new(TestContextMenuTarget {
            calls: calls.clone(),
        });
        let target = target.as_com_ref::<IContextMenuTarget>().unwrap();
        let parameter_id = 42;
        let menu = unsafe {
            handler_wrapper.createContextMenu(
                std::ptr::NonNull::<IPlugView>::dangling().as_ptr(),
                &parameter_id,
            )
        };
        let menu = unsafe { ComPtr::<IContextMenu>::from_raw(menu) }.unwrap();
        let reset = context_menu_item("Reset", 17, 0);
        let separator = context_menu_item("", 0, IContextMenuItem_::Flags_::kIsSeparator as i32);
        unsafe {
            assert_eq!(menu.addItem(&reset, target.as_ptr()), kResultOk);
            assert_eq!(menu.addItem(&separator, ptr::null_mut()), kResultOk);
            assert_eq!(menu.getItemCount(), 2);
            let mut returned = context_menu_item("", 0, 0);
            let mut returned_target = ptr::null_mut();
            assert_eq!(
                menu.getItem(0, &mut returned, &mut returned_target),
                kResultOk
            );
            assert_eq!(
                crate::internal::utils::vst_string_to_string(&returned.name),
                "Reset"
            );
            assert_eq!(returned.tag, 17);
            assert!(!returned_target.is_null());
            drop(ComPtr::<IContextMenuTarget>::from_raw(returned_target));
            assert_eq!(menu.popup(11, 23), kResultOk);
        }
        drop(menu);

        let notifications = handler_wrapper.take_host_notifications();
        let (menu_id, items) = match notifications.as_slice() {
            [HostNotification::ContextMenuRequested {
                menu_id,
                parameter_id: Some(42),
                x: 11,
                y: 23,
                items,
            }] => (*menu_id, items),
            other => panic!("unexpected context-menu notification: {other:?}"),
        };
        assert_eq!(
            items,
            &[
                ContextMenuItem {
                    item_id: 0,
                    name: "Reset".to_string(),
                    tag: 17,
                    flags: 0,
                },
                ContextMenuItem {
                    item_id: 1,
                    name: String::new(),
                    tag: 0,
                    flags: IContextMenuItem_::Flags_::kIsSeparator as i32,
                },
            ]
        );
        assert!(items[1].is_separator());

        handler_wrapper
            .execute_context_menu_item(menu_id, 0)
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![17]);
        assert!(
            handler_wrapper
                .execute_context_menu_item(menu_id, 0)
                .is_err(),
            "a popup can only be completed once"
        );
    }

    #[test]
    fn handler3_rejects_invalid_views_and_releases_dismissed_targets() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        assert!(unsafe { handler.createContextMenu(ptr::null_mut(), ptr::null()) }.is_null());

        let menu = unsafe {
            handler.createContextMenu(
                std::ptr::NonNull::<IPlugView>::dangling().as_ptr(),
                ptr::null(),
            )
        };
        let menu = unsafe { ComPtr::<IContextMenu>::from_raw(menu) }.unwrap();
        let separator = context_menu_item("", 0, IContextMenuItem_::Flags_::kIsSeparator as i32);
        unsafe {
            assert_eq!(menu.addItem(&separator, ptr::null_mut()), kResultOk);
            assert_eq!(menu.popup(0, 0), kResultOk);
        }
        let notification = handler.take_host_notifications().pop().unwrap();
        let HostNotification::ContextMenuRequested { menu_id, .. } = notification else {
            panic!("expected context-menu notification");
        };
        handler.dismiss_context_menu(menu_id).unwrap();
        assert!(handler.dismiss_context_menu(menu_id).is_err());
    }

    #[test]
    fn unit_handler_requests_are_ordered_and_preserve_whole_list_changes() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.notifyUnitSelection(7), kResultOk);
            assert_eq!(handler.notifyProgramListChange(11, 3), kResultOk);
            assert_eq!(handler.notifyProgramListChange(11, -1), kResultOk);
            assert_eq!(handler.notifyUnitByBusChange(), kResultOk);
        }
        let notifications = handler.take_host_notifications();
        assert_eq!(
            notifications,
            vec![
                HostNotification::UnitSelectionChanged { unit_id: 7 },
                HostNotification::ProgramListChanged {
                    list_id: 11,
                    program_index: Some(3)
                },
                HostNotification::ProgramListChanged {
                    list_id: 11,
                    program_index: None
                },
                HostNotification::UnitByBusChanged,
            ]
        );
        assert!(!notifications[0].invalidates_unit_cache());
        assert!(notifications[1..]
            .iter()
            .all(HostNotification::invalidates_unit_cache));
        assert!(handler.take_host_notifications().is_empty());
    }

    /// `restartComponent` is how a plugin tells the host something about it changed. Every flag
    /// used to be acknowledged and dropped; they must survive until the host drains them.
    #[test]
    fn restart_flags_accumulate_until_drained() {
        use vst3::Steinberg::Vst::RestartFlags_ as Flags;
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        assert!(handler.take_restart_flags().is_empty());

        // Two separate restarts, e.g. a preset load followed by a mode switch.
        unsafe {
            handler.restartComponent(Flags::kParamValuesChanged | Flags::kParamTitlesChanged);
            handler.restartComponent(Flags::kLatencyChanged);
        }

        let flags = handler.take_restart_flags();
        assert!(flags.param_values_changed());
        assert!(flags.param_titles_changed());
        assert!(flags.latency_changed());
        assert!(!flags.io_changed());

        // Draining clears them, so the host doesn't act on the same request twice.
        assert!(handler.take_restart_flags().is_empty());

        // A plugin that spams restarts while nothing polls costs one word, not a queue.
        unsafe {
            for _ in 0..10_000 {
                handler.restartComponent(Flags::kIoChanged);
            }
        }
        let flags = handler.take_restart_flags();
        assert!(flags.io_changed());
        assert_eq!(flags.bits(), Flags::kIoChanged);
    }
}

#[cfg(test)]
mod host_application_tests {
    use super::component_handler_tests::complete_author_events;
    use super::*;
    use crate::plugin::{
        HostNotification, NativeEditorEvent, NativeEditorEventDrain, NativeEditorEventFailure,
        ParameterEdit, ParameterEditKind,
    };

    fn audio_effect_interfaces() -> crate::PluginInterfaceSupport {
        use crate::{PluginInterface as Interface, PluginInterfaceSupport};
        let interfaces = PluginInterfaceSupport::none()
            .with(Interface::Component)
            .with(Interface::AudioProcessor)
            .with(Interface::EditController)
            .with(Interface::ConnectionPoint)
            .with(Interface::ProcessContextRequirements);
        #[cfg(windows)]
        let interfaces = interfaces
            .with(Interface::PlugView)
            .with(Interface::PlugViewContentScaleSupport)
            .with(Interface::DataExchangeReceiver);
        interfaces
    }

    #[test]
    fn interface_support_audio_effect_consumers() {
        let host = create_host_application_with_interfaces(audio_effect_interfaces())
            .to_com_ptr::<IPlugInterfaceSupport>()
            .unwrap();
        let expectations = [
            (IComponent::IID, true),
            (IAudioProcessor::IID, true),
            (IEditController::IID, true),
            (IConnectionPoint::IID, true),
            (IProcessContextRequirements::IID, true),
            (IPlugViewContentScaleSupport::IID, cfg!(windows)),
            (IMidiMapping::IID, false),
            (IUnitInfo::IID, false),
            (IProgramListData::IID, false),
            (IUnitData::IID, false),
            (IEditControllerHostEditing::IID, false),
            (IMidiLearn::IID, false),
            (IAutomationState::IID, false),
            (INoteExpressionController::IID, false),
            (IPrefetchableSupport::IID, false),
            (IRemapParamID::IID, false),
            (IDataExchangeReceiver::IID, cfg!(windows)),
            (IComponentHandler::IID, false),
            ([0x5a; 16], false),
        ];
        for (index, (iid, supported)) in expectations.iter().enumerate() {
            let result = unsafe { host.isPlugInterfaceSupported(iid as *const _ as *const TUID) };
            assert_eq!(
                result,
                if *supported {
                    kResultTrue
                } else {
                    kResultFalse
                },
                "IID case {index}"
            );
        }
        assert_eq!(
            unsafe { host.isPlugInterfaceSupported(ptr::null()) },
            kInvalidArgument
        );
        assert_eq!(
            unsafe { host.isPlugInterfaceSupported(&IPlugView::IID as *const _ as *const TUID) },
            if cfg!(windows) {
                kResultTrue
            } else {
                kResultFalse
            },
        );
    }

    #[test]
    fn interface_support_contexts_are_independent_immutable_values() {
        use crate::{PluginInterface, PluginInterfaceSupport};
        let original = PluginInterfaceSupport::none().with(PluginInterface::Component);
        let first = create_host_application_with_interfaces(original)
            .to_com_ptr::<IPlugInterfaceSupport>()
            .unwrap();
        let second = create_host_application_with_interfaces(
            PluginInterfaceSupport::none().with(PluginInterface::MidiMapping),
        )
        .to_com_ptr::<IPlugInterfaceSupport>()
        .unwrap();
        // Building a new policy must not mutate either retained host context.
        let third_policy = original.with(PluginInterface::MidiMapping);
        assert!(third_policy.supports(PluginInterface::MidiMapping));
        let component = &IComponent::IID as *const _ as *const TUID;
        let midi = &IMidiMapping::IID as *const _ as *const TUID;
        unsafe {
            assert_eq!(first.isPlugInterfaceSupported(component), kResultTrue);
            assert_eq!(first.isPlugInterfaceSupported(midi), kResultFalse);
            assert_eq!(second.isPlugInterfaceSupported(component), kResultFalse);
            assert_eq!(second.isPlugInterfaceSupported(midi), kResultTrue);
            assert_eq!(first.isPlugInterfaceSupported(component), kResultTrue);
        }
    }

    #[test]
    fn interface_support_standalone_default_preserves_original_declarations() {
        let host = create_host_application()
            .to_com_ptr::<IPlugInterfaceSupport>()
            .unwrap();
        let supported = [
            IConnectionPoint::IID,
            IMidiMapping::IID,
            IUnitInfo::IID,
            IProgramListData::IID,
            IUnitData::IID,
            IEditControllerHostEditing::IID,
            IMidiLearn::IID,
            IAutomationState::IID,
            INoteExpressionController::IID,
            IPlugViewContentScaleSupport::IID,
            IProcessContextRequirements::IID,
            IPrefetchableSupport::IID,
            IRemapParamID::IID,
            IDataExchangeReceiver::IID,
        ];
        for (index, iid) in supported.iter().enumerate() {
            assert_eq!(
                unsafe { host.isPlugInterfaceSupported(iid as *const _ as *const TUID) },
                kResultTrue,
                "original IID {index}"
            );
        }
        for iid in [
            IComponent::IID,
            IAudioProcessor::IID,
            IEditController::IID,
            IPlugView::IID,
            IComponentHandler::IID,
            [0x5a; 16],
        ] {
            assert_eq!(
                unsafe { host.isPlugInterfaceSupported(&iid as *const _ as *const TUID) },
                kResultFalse
            );
        }
    }

    #[test]
    fn interface_support_reports_plugin_side_interfaces_only() {
        let host = create_host_application()
            .to_com_ptr::<IPlugInterfaceSupport>()
            .unwrap();
        unsafe {
            let mut process_context = IProcessContextRequirements::IID;
            assert_eq!(
                host.isPlugInterfaceSupported(&mut process_context as *mut _ as *const TUID,),
                kResultTrue
            );
            let mut remap_param_id = IRemapParamID::IID;
            assert_eq!(
                host.isPlugInterfaceSupported(&mut remap_param_id as *mut _ as *const TUID),
                kResultTrue
            );

            let mut component_handler = IComponentHandler::IID;
            assert_eq!(
                host.isPlugInterfaceSupported(&mut component_handler as *mut _ as *const TUID,),
                kResultFalse
            );
            assert_eq!(
                host.isPlugInterfaceSupported(ptr::null_mut()),
                kInvalidArgument
            );
        }
    }

    #[test]
    fn create_instance_requires_matching_class_and_interface_ids() {
        let host = create_host_application()
            .to_com_ptr::<IHostApplication>()
            .unwrap();
        unsafe {
            let mut message_cid = IMessage::IID;
            let mut message_iid = IMessage::IID;
            let mut raw = ptr::null_mut();
            assert_eq!(
                host.createInstance(
                    &mut message_cid as *mut _ as *mut TUID,
                    &mut message_iid as *mut _ as *mut TUID,
                    &mut raw,
                ),
                kResultTrue
            );
            assert!(!raw.is_null());
            drop(ComPtr::<IMessage>::from_raw(raw.cast::<IMessage>()));

            let mut attributes_iid = IAttributeList::IID;
            raw = std::ptr::NonNull::<std::ffi::c_void>::dangling().as_ptr();
            assert_eq!(
                host.createInstance(
                    &mut message_cid as *mut _ as *mut TUID,
                    &mut attributes_iid as *mut _ as *mut TUID,
                    &mut raw,
                ),
                kNoInterface
            );
            assert!(raw.is_null());

            assert_eq!(
                host.createInstance(
                    ptr::null_mut(),
                    &mut message_iid as *mut _ as *mut TUID,
                    &mut raw,
                ),
                kInvalidArgument
            );
        }
    }

    #[test]
    fn progress_callbacks_are_bounded_ordered_and_polled() {
        use crate::plugin::{HostNotification, ProgressKind};

        let host = create_host_application();
        let progress = host.to_com_ptr::<IProgress>().unwrap();
        let description: Vec<u16> = "Loading samples\0".encode_utf16().collect();
        let mut id = 0;
        unsafe {
            assert_eq!(
                progress.start(
                    IProgress_::ProgressType_::AsyncStateRestoration,
                    description.as_ptr(),
                    &mut id,
                ),
                kResultOk
            );
            assert_ne!(id, 0);
            assert_eq!(progress.update(id, 0.5), kResultOk);
            assert_eq!(progress.finish(id), kResultOk);
            assert_eq!(progress.update(id, 0.75), kResultFalse);
            assert_eq!(progress.update(u64::MAX, 0.5), kResultFalse);
            assert_eq!(progress.update(id, f64::NAN), kInvalidArgument);
        }
        assert_eq!(
            host.take_progress_notifications(),
            vec![
                HostNotification::ProgressStarted {
                    id,
                    kind: ProgressKind::AsyncStateRestoration,
                    description: Some("Loading samples".to_string()),
                },
                HostNotification::ProgressUpdated {
                    id,
                    value: crate::plugin::ProgressValue::new(0.5).unwrap(),
                },
                HostNotification::ProgressFinished { id },
            ]
        );
    }

    #[test]
    fn progress_reports_backpressure_instead_of_false_success() {
        let host = create_host_application();
        let progress = host.to_com_ptr::<IProgress>().unwrap();
        let mut id = 0;
        unsafe {
            assert_eq!(
                progress.start(
                    IProgress_::ProgressType_::UIBackgroundTask,
                    ptr::null(),
                    &mut id,
                ),
                kResultOk
            );
            for _ in 1..MAX_HOST_NOTIFICATIONS {
                assert_eq!(progress.update(id, 0.25), kResultOk);
            }
            assert_eq!(progress.update(id, 0.5), kResultFalse);
            assert_eq!(progress.finish(id), kResultFalse);
        }
        assert_eq!(
            host.take_progress_notifications().len(),
            MAX_HOST_NOTIFICATIONS
        );
        unsafe {
            assert_eq!(progress.finish(id), kResultOk);
        }
    }

    #[test]
    fn native_editor_author_event_saturation_refuses_callback_instead_of_silent_drop() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        let wrapper = ComWrapper::new(handler);
        let interface = wrapper
            .to_com_ptr::<IComponentHandler>()
            .expect("real IComponentHandler COM interface");
        unsafe {
            for _ in 0..MAX_EDITOR_FEEDBACK {
                assert_eq!(
                    interface.beginEdit(5),
                    kResultOk,
                    "fixture must fill the complete event source"
                );
            }
            assert_eq!(interface.endEdit(5), kResultFalse, "a full native author event source must explicitly refuse the callback rather than silently report success");
        }
    }

    #[test]
    fn native_editor_dsp_feedback_saturation_refuses_value_after_independent_event_drains() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            for _ in 0..MAX_EDITOR_FEEDBACK {
                assert_eq!(
                    handler.performEdit(5, 0.5),
                    kResultOk,
                    "fixture must fill the existing bounded DSP feedback sink"
                );
                assert_eq!(
                    complete_author_events(&handler).len(),
                    1,
                    "the event log stays available while the DSP sink fills"
                );
            }
            assert_eq!(handler.performEdit(5, 0.75), kResultFalse, "an unaccepted DSP feedback value cannot be reported as a complete native author edit");
        }
    }

    #[test]
    fn native_editor_author_events_preserve_group_parameter_and_dirty_order() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        let wrapper = ComWrapper::new(handler);
        let parameter = wrapper
            .to_com_ptr::<IComponentHandler>()
            .expect("native parameter interface");
        let group = wrapper
            .to_com_ptr::<IComponentHandler2>()
            .expect("native group/dirty interface");
        unsafe {
            assert_eq!(group.startGroupEdit(), kResultOk);
            assert_eq!(parameter.beginEdit(5), kResultOk);
            assert_eq!(parameter.performEdit(5, 0.25), kResultOk);
            assert_eq!(group.setDirty(1), kResultOk);
            assert_eq!(parameter.endEdit(5), kResultOk);
            assert_eq!(group.requestOpenEditor(c"editor".as_ptr()), kResultOk);
            assert_eq!(parameter.beginEdit(9), kResultOk);
            assert_eq!(parameter.performEdit(9, 0.75), kResultOk);
            assert_eq!(parameter.endEdit(9), kResultOk);
            assert_eq!(group.finishGroupEdit(), kResultOk);
        }
        assert_eq!(
            complete_author_events(&wrapper),
            vec![
                NativeEditorEvent::GroupEditStarted,
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 5,
                    kind: ParameterEditKind::BeginGesture,
                    value: None
                }),
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 5,
                    kind: ParameterEditKind::ValueChange,
                    value: Some(0.25)
                }),
                NativeEditorEvent::DirtyChanged(true),
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 5,
                    kind: ParameterEditKind::EndGesture,
                    value: None
                }),
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 9,
                    kind: ParameterEditKind::BeginGesture,
                    value: None
                }),
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 9,
                    kind: ParameterEditKind::ValueChange,
                    value: Some(0.75)
                }),
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 9,
                    kind: ParameterEditKind::EndGesture,
                    value: None
                }),
                NativeEditorEvent::GroupEditFinished,
            ]
        );
        assert_eq!(
            wrapper.take_host_notifications(),
            vec![HostNotification::OpenEditorRequested {
                name: Some("editor".to_owned())
            }],
            "control-only requests cannot acquire author gesture membership"
        );
        assert!(complete_author_events(&wrapper).is_empty());
        assert_eq!(
            wrapper.parameter_changes.lock().unwrap().as_slice(),
            &[(5, 0.25), (9, 0.75)]
        );
    }

    #[test]
    fn native_editor_source_overflow_remains_incomplete_after_drains_and_dsp_releases() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            for _ in 0..MAX_EDITOR_FEEDBACK {
                assert_eq!(handler.performEdit(5, 0.25), kResultOk);
                assert_eq!(complete_author_events(&handler).len(), 1);
            }
            assert_eq!(handler.performEdit(5, 0.5), kResultFalse);
        }
        let refusal = NativeEditorEventDrain::Incomplete {
            failure: NativeEditorEventFailure::Overflow,
        };
        assert_eq!(handler.take_editor_events(), refusal);
        handler.parameter_changes.lock().unwrap().clear();
        unsafe {
            assert_eq!(handler.beginEdit(5), kResultFalse);
            assert_eq!(handler.performEdit(5, 0.75), kResultFalse);
            assert_eq!(handler.endEdit(5), kResultFalse);
            assert_eq!(handler.startGroupEdit(), kResultFalse);
            assert_eq!(handler.finishGroupEdit(), kResultFalse);
            assert_eq!(handler.setDirty(1), kResultFalse);
        }
        assert_eq!(
            handler.take_editor_events(),
            refusal,
            "capacity release and polling cannot complete a lost native gesture"
        );
        assert!(
            handler.parameter_changes.lock().unwrap().is_empty(),
            "a refused native author value cannot re-enter the DSP feedback sink"
        );
        let fresh = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(fresh.beginEdit(5), kResultOk);
        }
        assert_eq!(
            complete_author_events(&fresh).len(),
            1,
            "only a fresh native source establishes a new complete lifetime"
        );
    }

    #[test]
    fn native_editor_invalid_normalized_value_refuses_entire_source_without_publication() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.1] {
            let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
            unsafe {
                assert_eq!(handler.beginEdit(5), kResultOk);
                assert_eq!(handler.performEdit(5, value), kResultFalse);
                assert_eq!(handler.endEdit(5), kResultFalse);
            }
            assert_eq!(
                handler.take_editor_events(),
                NativeEditorEventDrain::Incomplete {
                    failure: NativeEditorEventFailure::InvalidValue
                }
            );
            assert!(handler.parameter_changes.lock().unwrap().is_empty());
            assert_eq!(
                handler.take_editor_events(),
                NativeEditorEventDrain::Incomplete {
                    failure: NativeEditorEventFailure::InvalidValue
                }
            );
        }
    }
}

#[cfg(test)]
mod connection_proxy_tests {
    use super::*;

    #[derive(Default)]
    struct RecordingConnectionPoint {
        notifications: AtomicUsize,
    }

    impl Class for RecordingConnectionPoint {
        type Interfaces = (IConnectionPoint,);
    }

    impl IConnectionPointTrait for RecordingConnectionPoint {
        unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
            kResultOk
        }

        unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
            kResultOk
        }

        unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
            self.notifications.fetch_add(1, Ordering::Relaxed);
            kResultOk
        }
    }

    /// A proxy gated to the calling thread, plus the endpoint it forwards to.
    fn proxy_with_destination() -> (ConnectionProxy, ComWrapper<RecordingConnectionPoint>) {
        let destination = ComWrapper::new(RecordingConnectionPoint::default());
        let destination_ptr = destination
            .to_com_ptr::<IConnectionPoint>()
            .expect("RecordingConnectionPoint declares IConnectionPoint");
        let proxy = ConnectionProxy::new(destination_ptr, thread::current().id(), "test");
        (proxy, destination)
    }

    #[test]
    fn connection_proxy_forwards_only_on_its_control_thread() {
        let destination = ComWrapper::new(RecordingConnectionPoint::default());
        let destination_ptr = destination.to_com_ptr::<IConnectionPoint>().unwrap();
        let proxy = ComWrapper::new(ConnectionProxy::new(
            destination_ptr,
            thread::current().id(),
            "test",
        ));
        let proxy_ptr = proxy.to_com_ptr::<IConnectionPoint>().unwrap();
        let message = create_host_message().to_com_ptr::<IMessage>().unwrap();

        unsafe {
            assert_eq!(proxy_ptr.notify(message.as_ptr()), kResultOk);
        }
        assert_eq!(destination.notifications.load(Ordering::Relaxed), 1);

        let message_ptr = message.as_ptr() as usize;
        let result =
            std::thread::spawn(move || unsafe { proxy_ptr.notify(message_ptr as *mut IMessage) })
                .join()
                .unwrap();
        assert_eq!(result, kResultFalse);
        assert_eq!(destination.notifications.load(Ordering::Relaxed), 1);
    }

    /// The drop is the SDK's behaviour, but it must be countable — otherwise a plugin whose
    /// meters never update looks identical to a plugin that sends nothing.
    #[test]
    fn off_thread_notifies_are_counted_so_the_drop_is_diagnosable() {
        let (proxy, destination) = proxy_with_destination();
        let message = create_host_message().to_com_ptr::<IMessage>().unwrap();
        let message_ptr = message.as_ptr() as usize;

        let results = thread::scope(|scope| {
            scope
                .spawn(|| {
                    (0..3)
                        .map(|_| unsafe { proxy.notify(message_ptr as *mut IMessage) })
                        .collect::<Vec<_>>()
                })
                .join()
                .expect("notify never panics")
        });

        assert_eq!(results, vec![kResultFalse; 3]);
        assert_eq!(destination.notifications.load(Ordering::Relaxed), 0);
        assert_eq!(proxy.dropped_message_count(), 3);
    }

    /// Only the thread gate increments the counter: a forwarded message and a malformed one
    /// must not inflate the "your plugin is messaging off-thread" signal.
    #[test]
    fn forwarded_and_null_notifies_do_not_count_as_off_thread_drops() {
        let (proxy, destination) = proxy_with_destination();
        let message = create_host_message().to_com_ptr::<IMessage>().unwrap();

        assert_eq!(unsafe { proxy.notify(message.as_ptr()) }, kResultOk);
        assert_eq!(unsafe { proxy.notify(ptr::null_mut()) }, kResultFalse);

        assert_eq!(destination.notifications.load(Ordering::Relaxed), 1);
        assert_eq!(proxy.dropped_message_count(), 0);
    }
}

#[cfg(test)]
mod host_event_list_tests {
    use super::*;

    /// `process()` is the input list's only drain and it returns early while the plugin isn't
    /// processing, so queueing MIDI at a stopped plugin must not grow the list forever.
    #[test]
    fn queued_events_are_capped_so_a_stopped_plugin_cannot_grow_them() {
        let list = HostEventList::new();
        let event: Event = unsafe { std::mem::zeroed() };

        for _ in 0..(MAX_QUEUED_EVENTS + 500) {
            list.add_raw_event(&event);
        }
        assert_eq!(unsafe { list.getEventCount() }, MAX_QUEUED_EVENTS as i32);

        // The plugin-facing COM path is capped too, and reports the refusal.
        let mut event = event;
        assert_eq!(
            unsafe { list.addEvent(&mut event as *mut Event) },
            kResultFalse
        );

        // Clearing (as each block does) makes room again without dropping capacity.
        list.clear();
        assert_eq!(unsafe { list.getEventCount() }, 0);
        assert!(list.events.lock().unwrap().capacity() >= MAX_QUEUED_EVENTS);
        list.add_raw_event(&event);
        assert_eq!(unsafe { list.getEventCount() }, 1);
    }

    #[test]
    fn sysex_is_deep_copied_before_plugin_memory_expires() {
        let list = HostEventList::new();
        let source = vec![0xf0, 0x7d, 1, 2, 3, 0xf7];
        let source_ptr = source.as_ptr();
        let mut raw: Event = unsafe { std::mem::zeroed() };
        raw.r#type = Event_::EventTypes_::kDataEvent as u16;
        raw.__field0.data = DataEvent {
            size: source.len() as u32,
            r#type: u32::try_from(DataEvent_::DataTypes_::kMidiSysEx)
                .expect("SDK SysEx type is nonnegative"),
            bytes: source_ptr,
        };

        assert_eq!(unsafe { list.addEvent(&mut raw) }, kResultOk);
        drop(source);

        let stored = list.events.lock().unwrap();
        let PluginEventData::Data { bytes, .. } = &stored[0].data else {
            panic!("expected data event");
        };
        assert_eq!(bytes, &[0xf0, 0x7d, 1, 2, 3, 0xf7]);
        assert_ne!(bytes.as_ptr(), source_ptr);
        drop(stored);

        let mut returned: Event = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { list.getEvent(0, &mut returned) }, kResultOk);
        let returned_data = unsafe { returned.__field0.data };
        assert_eq!(
            unsafe { std::slice::from_raw_parts(returned_data.bytes, returned_data.size as usize) },
            &[0xf0, 0x7d, 1, 2, 3, 0xf7]
        );
    }

    #[test]
    fn malformed_or_oversized_pointer_payloads_are_rejected() {
        let list = HostEventList::new();
        let mut raw: Event = unsafe { std::mem::zeroed() };
        raw.r#type = Event_::EventTypes_::kDataEvent as u16;
        raw.__field0.data = DataEvent {
            size: 1,
            r#type: u32::try_from(DataEvent_::DataTypes_::kMidiSysEx)
                .expect("SDK SysEx type is nonnegative"),
            bytes: ptr::null(),
        };
        assert_eq!(unsafe { list.addEvent(&mut raw) }, kResultFalse);

        raw.__field0.data.size = (MAX_EVENT_PAYLOAD_BYTES + 1) as u32;
        raw.__field0.data.bytes = std::ptr::dangling();
        assert_eq!(unsafe { list.addEvent(&mut raw) }, kResultFalse);
        assert_eq!(unsafe { list.getEventCount() }, 0);
    }
}

#[cfg(test)]
mod plug_frame_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};

    struct FakePlugView {
        on_size_calls: Arc<AtomicUsize>,
        on_size_result: Arc<AtomicI32>,
        observed_unlocked_slot: Arc<AtomicBool>,
        slot: Arc<Mutex<Option<(i32, i32)>>>,
        reenter_once: AtomicBool,
        frame: AtomicPtr<IPlugFrame>,
        view: Arc<AtomicPtr<IPlugView>>,
    }

    impl Class for FakePlugView {
        type Interfaces = (IPlugView,);
    }

    impl IPlugViewTrait for FakePlugView {
        unsafe fn isPlatformTypeSupported(&self, _type: FIDString) -> tresult {
            kResultOk
        }

        unsafe fn attached(&self, _parent: *mut std::ffi::c_void, _type: FIDString) -> tresult {
            kResultOk
        }

        unsafe fn removed(&self) -> tresult {
            kResultOk
        }

        unsafe fn onWheel(&self, _distance: f32) -> tresult {
            kResultOk
        }

        unsafe fn onKeyDown(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
            kResultOk
        }

        unsafe fn onKeyUp(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
            kResultOk
        }

        unsafe fn getSize(&self, _size: *mut ViewRect) -> tresult {
            kResultOk
        }

        unsafe fn onSize(&self, _new_size: *mut ViewRect) -> tresult {
            self.on_size_calls.fetch_add(1, Ordering::SeqCst);
            self.observed_unlocked_slot
                .store(self.slot.try_lock().is_ok(), Ordering::SeqCst);

            if self.reenter_once.swap(false, Ordering::SeqCst) {
                let frame = ComRef::<IPlugFrame>::from_raw(self.frame.load(Ordering::SeqCst))
                    .expect("frame pointer");
                let mut nested = ViewRect {
                    left: 0,
                    top: 0,
                    right: 321,
                    bottom: 123,
                };
                assert_eq!(
                    frame.resizeView(self.view.load(Ordering::SeqCst), &mut nested),
                    kResultOk
                );
            }
            self.on_size_result.load(Ordering::SeqCst)
        }

        unsafe fn onFocus(&self, _state: TBool) -> tresult {
            kResultOk
        }

        unsafe fn setFrame(&self, _frame: *mut IPlugFrame) -> tresult {
            kResultOk
        }

        unsafe fn canResize(&self) -> tresult {
            kResultTrue
        }

        unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
            kResultOk
        }
    }

    #[cfg(target_os = "linux")]
    fn make_frame(slot: Arc<Mutex<Option<(i32, i32)>>>) -> HostPlugFrame {
        HostPlugFrame::new(slot, Arc::new(Mutex::new(RunLoopRegistry::new())))
    }

    #[cfg(not(target_os = "linux"))]
    fn make_frame(slot: Arc<Mutex<Option<(i32, i32)>>>) -> HostPlugFrame {
        HostPlugFrame::new(slot)
    }

    #[test]
    fn records_requested_size_and_calls_on_size_synchronously() {
        let slot = Arc::new(Mutex::new(None));
        let frame = make_frame(slot.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let unlocked = Arc::new(AtomicBool::new(false));
        let view = ComWrapper::new(FakePlugView {
            on_size_calls: calls.clone(),
            on_size_result: Arc::new(AtomicI32::new(kResultOk)),
            observed_unlocked_slot: unlocked.clone(),
            slot: slot.clone(),
            reenter_once: AtomicBool::new(false),
            frame: AtomicPtr::new(std::ptr::null_mut()),
            view: Arc::new(AtomicPtr::new(std::ptr::null_mut())),
        });
        let view = view.to_com_ptr::<IPlugView>().unwrap();
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        };
        let r = unsafe { frame.resizeView(view.as_ptr(), &mut rect) };
        assert_eq!(r, kResultOk);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(unlocked.load(Ordering::SeqCst));
        assert_eq!(*slot.lock().unwrap(), Some((640, 480)));
    }

    #[test]
    fn rejects_nulls_and_invalid_dimensions_without_recording() {
        let slot = Arc::new(Mutex::new(None));
        let frame = make_frame(slot.clone());
        let mut rect = ViewRect {
            left: 5,
            top: 5,
            right: 5,
            bottom: 10,
        };
        assert_eq!(
            unsafe { frame.resizeView(std::ptr::null_mut(), &mut rect) },
            kInvalidArgument
        );
        assert_eq!(
            unsafe { frame.resizeView(std::ptr::null_mut(), std::ptr::null_mut()) },
            kInvalidArgument
        );
        assert_eq!(*slot.lock().unwrap(), None);
    }

    #[test]
    fn propagates_on_size_failure_and_allows_reentrant_resize() {
        let slot = Arc::new(Mutex::new(None));
        let frame = make_frame(slot.clone());
        let frame_wrapper = {
            #[cfg(target_os = "linux")]
            {
                ComWrapper::new(HostPlugFrame::new(
                    slot.clone(),
                    Arc::new(Mutex::new(RunLoopRegistry::new())),
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                ComWrapper::new(HostPlugFrame::new(slot.clone()))
            }
        };
        let frame_ptr = frame_wrapper.to_com_ptr::<IPlugFrame>().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let on_size_result = Arc::new(AtomicI32::new(kResultOk));
        let view_ptr = Arc::new(AtomicPtr::new(std::ptr::null_mut()));
        let view_wrapper = ComWrapper::new(FakePlugView {
            on_size_calls: calls.clone(),
            on_size_result: on_size_result.clone(),
            observed_unlocked_slot: Arc::new(AtomicBool::new(false)),
            slot: slot.clone(),
            reenter_once: AtomicBool::new(true),
            frame: AtomicPtr::new(frame_ptr.as_ptr()),
            view: view_ptr.clone(),
        });
        let view = view_wrapper.to_com_ptr::<IPlugView>().unwrap();
        view_ptr.store(view.as_ptr(), Ordering::SeqCst);

        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        };
        assert_eq!(
            unsafe { frame_ptr.resizeView(view.as_ptr(), &mut rect) },
            kResultOk
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(*slot.lock().unwrap(), Some((321, 123)));

        on_size_result.store(kResultFalse, Ordering::SeqCst);
        assert_eq!(
            unsafe { frame.resizeView(view.as_ptr(), &mut rect) },
            kResultFalse
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn run_loop_rejects_null_registrations_and_starts_empty() {
        use vst3::Steinberg::Linux::IRunLoopTrait;
        let reg = Arc::new(Mutex::new(RunLoopRegistry::new()));
        let frame = HostPlugFrame::new(Arc::new(Mutex::new(None)), reg.clone());
        // A registry with no editor attached is empty.
        assert!(reg.lock().unwrap().handlers.is_empty());
        assert!(reg.lock().unwrap().timers.is_empty());
        // Null handlers are rejected without touching the registry.
        unsafe {
            assert_eq!(
                frame.registerEventHandler(std::ptr::null_mut(), 3),
                kInvalidArgument
            );
            assert_eq!(
                frame.registerTimer(std::ptr::null_mut(), 16),
                kInvalidArgument
            );
        }
        assert!(reg.lock().unwrap().handlers.is_empty());
        assert!(reg.lock().unwrap().timers.is_empty());
    }
}

#[cfg(test)]
mod parameter_changes_tests {
    use super::*;

    #[test]
    fn parameter_queue_com_roundtrip_preserves_all_sparse_points() {
        let owner = ComWrapper::new(ParameterChanges::default());
        // Enqueue out of order, then read only through the generated COM
        // vtables. No vector inspection or parameter interpolator is involved.
        owner.enqueue(1, 7, 0.75);
        owner.enqueue(1, 0, 0.25);
        owner.enqueue(1, 4, 0.75);
        let changes = owner.to_com_ptr::<IParameterChanges>().unwrap();
        assert_eq!(unsafe { changes.getParameterCount() }, 1);
        assert!(unsafe { changes.getParameterData(-1) }.is_null());
        assert!(unsafe { changes.getParameterData(1) }.is_null());
        let queue = unsafe { ComRef::<IParamValueQueue>::from_raw(changes.getParameterData(0)) }
            .expect("the only active parameter queue");
        assert_eq!(unsafe { queue.getParameterId() }, 1);
        assert_eq!(unsafe { queue.getPointCount() }, 3);
        for (index, expected_offset, expected_value) in
            [(0, 0, 0.25_f64), (1, 4, 0.75_f64), (2, 7, 0.75_f64)]
        {
            let (mut offset, mut value) = (-99, -123.5_f64);
            assert_eq!(
                unsafe { queue.getPoint(index, &mut offset, &mut value) },
                kResultOk,
                "COM point index {index}"
            );
            assert_eq!(offset, expected_offset, "COM point index {index}");
            assert_eq!(
                value.to_bits(),
                expected_value.to_bits(),
                "COM point index {index}"
            );
        }
        for index in [-1, 3] {
            let (mut offset, mut value) = (-99, -123.5_f64);
            assert_eq!(
                unsafe { queue.getPoint(index, &mut offset, &mut value) },
                kResultFalse,
                "out-of-range COM point index {index}"
            );
            assert_eq!(offset, -99);
            assert_eq!(value.to_bits(), (-123.5_f64).to_bits());
        }
        assert_eq!(unsafe { changes.getParameterCount() }, 1);
        assert_eq!(unsafe { queue.getPointCount() }, 3);
        owner.clear_all();
        assert_eq!(unsafe { changes.getParameterCount() }, 0);
        assert!(unsafe { changes.getParameterData(0) }.is_null());
    }

    #[test]
    fn enqueue_groups_by_id_orders_by_offset_and_clears() {
        let pc = ParameterChanges::default();
        pc.enqueue(7, 64, 0.9);
        pc.enqueue(7, 0, 0.5); // earlier offset, same id → must sort before the 64 point
        pc.enqueue(3, 0, 0.1);

        // Two distinct parameter ids → two queues; the processor reads this count.
        assert_eq!(unsafe { pc.getParameterCount() }, 2);
        {
            let queues = pc.queues.lock().unwrap();
            let q7 = queues.iter().find(|q| q.param_id() == 7).unwrap();
            assert_eq!(*q7.points.lock().unwrap(), vec![(0, 0.5), (64, 0.9)]);
            let q3 = queues.iter().find(|q| q.param_id() == 3).unwrap();
            assert_eq!(*q3.points.lock().unwrap(), vec![(0, 0.1)]);
        }

        // After a block the host clears it so values don't re-stick.
        pc.clear_all();
        assert_eq!(unsafe { pc.getParameterCount() }, 0);
    }

    #[test]
    fn active_points_can_be_drained_into_bounded_feedback_before_clear() {
        let pc = ParameterChanges::default();
        pc.enqueue(9, 31, 0.75);
        pc.enqueue(9, 2, 0.25);
        pc.enqueue(4, 0, 1.0);
        let mut seen = Vec::new();
        pc.for_each_active_point(|id, offset, value| seen.push((id, offset, value)));
        assert_eq!(seen, vec![(9, 2, 0.25), (9, 31, 0.75), (4, 0, 1.0)]);
        pc.clear_all();
        let mut after_clear = Vec::new();
        pc.for_each_active_point(|id, offset, value| after_clear.push((id, offset, value)));
        assert!(after_clear.is_empty());
    }

    /// Regression test for the plugin-facing `addParameterData`/`addPoint` path used for
    /// `ProcessData::outputParameterChanges`. Per the VST3 docs, `outputParameterChanges`
    /// describes changes for the *current* processing block only, mirroring the reference
    /// `ParameterChanges` host helper's `clearQueue()`. Without calling `clear_all()` before
    /// each block (as `PluginImpl::process` now does), a plugin emitting output parameter
    /// points for the same id every block would keep finding its own queue "already active"
    /// (since `used` never resets) and keep appending points to it forever.
    #[test]
    fn output_queue_is_isolated_per_block_and_does_not_grow_when_cleared() {
        let pc = ParameterChanges::default();

        // Simulate a plugin emitting one output point per block via the same `addParameterData`
        // activation path the real COM `IParameterChanges::addParameterData` call uses, then
        // insert the point directly into the returned slot's queue (equivalent to what the
        // plugin's `IParamValueQueue::addPoint` COM call would do on that same object).
        let emit_one_point = |pc: &ParameterChanges, id: u32, offset: i32, value: f64| {
            let mut index: i32 = -1;
            let queue_ptr =
                unsafe { pc.addParameterData(&id as *const u32, &mut index as *mut i32) };
            assert!(!queue_ptr.is_null());
            assert!(index >= 0);
            let queues = pc.queues.lock().unwrap();
            queues[index as usize].insert_point(offset, value);
        };

        // Block 1: plugin writes one point for parameter 42.
        emit_one_point(&pc, 42, 0, 0.1);
        assert_eq!(unsafe { pc.getParameterCount() }, 1);
        {
            let queues = pc.queues.lock().unwrap();
            let q = queues.iter().find(|q| q.param_id() == 42).unwrap();
            assert_eq!(*q.points.lock().unwrap(), vec![(0, 0.1)]);
        }
        let pool_len_after_block_1 = pc.queues.lock().unwrap().len();

        // Host resets the queue for the next block, as `PluginImpl::process` now does
        // immediately before invoking the processor.
        pc.clear_all();
        assert_eq!(unsafe { pc.getParameterCount() }, 0);

        // Block 2: plugin writes a different point for the same parameter id.
        emit_one_point(&pc, 42, 5, 0.9);
        assert_eq!(
            unsafe { pc.getParameterCount() },
            1,
            "clearing between blocks must not leave stale points visible or double-counted"
        );
        {
            let queues = pc.queues.lock().unwrap();
            let q = queues.iter().find(|q| q.param_id() == 42).unwrap();
            assert_eq!(
                *q.points.lock().unwrap(),
                vec![(5, 0.9)],
                "block 2 must only expose its own point, not block 1's stale value"
            );
        }

        // The pool is reused (recycled), not grown, across blocks.
        assert_eq!(
            pc.queues.lock().unwrap().len(),
            pool_len_after_block_1,
            "clearing between blocks must reuse pooled queues rather than growing the pool"
        );
    }

    #[test]
    fn enqueue_with_nan_value_orders_by_offset_without_panicking() {
        // The queue is ordered by `sample_offset` (an i32 — total order), never by the f64
        // value, so a NaN value can never reach a comparator and can never panic or corrupt
        // ordering. This pins that property: the points sort by offset and the NaN survives
        // verbatim in its offset slot.
        let pc = ParameterChanges::default();
        pc.enqueue(1, 128, f64::NAN);
        pc.enqueue(1, 0, 0.25);
        pc.enqueue(1, 64, f64::NAN);

        let queues = pc.queues.lock().unwrap();
        let q = queues.iter().find(|q| q.param_id() == 1).unwrap();
        let points = q.points.lock().unwrap();
        let offsets: Vec<i32> = points.iter().map(|(off, _)| *off).collect();
        assert_eq!(offsets, vec![0, 64, 128]);
        // The finite point is intact and the two NaN-valued points are still NaN.
        assert_eq!(points[0].1, 0.25);
        assert!(points[1].1.is_nan());
        assert!(points[2].1.is_nan());
    }
}

#[cfg(test)]
mod memory_stream_tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips_from_start() {
        let s = MemoryStream::new(Vec::new());
        assert_eq!(s.write_at_cursor(&[1, 2, 3, 4]), Some(4));
        assert_eq!(s.position(), 4);
        assert_eq!(s.to_vec(), vec![1, 2, 3, 4]);

        // Rewind (mode 0 = SEEK_SET) and read it all back.
        assert_eq!(s.seek_to(0, 0), Some(0));
        assert_eq!(s.read_at_cursor(4), vec![1, 2, 3, 4]);
    }

    #[test]
    fn read_past_end_is_clamped() {
        let s = MemoryStream::new(vec![9, 8]);
        assert_eq!(s.read_at_cursor(10), vec![9, 8]);
        // Cursor now at end; further reads yield nothing.
        assert_eq!(s.read_at_cursor(10), Vec::<u8>::new());
    }

    #[test]
    fn seek_modes_and_overwrite() {
        let s = MemoryStream::new(vec![0, 0, 0, 0]);
        // SEEK_END then write appends.
        assert_eq!(s.seek_to(0, SEEK_END), Some(4));
        s.write_at_cursor(&[5]);
        assert_eq!(s.to_vec(), vec![0, 0, 0, 0, 5]);
        // SEEK_SET to 1 then overwrite in place.
        assert_eq!(s.seek_to(1, 0), Some(1));
        s.write_at_cursor(&[7, 7]);
        assert_eq!(s.to_vec(), vec![0, 7, 7, 0, 5]);
        // SEEK_CUR is relative, and a seek before the start clamps to it.
        assert_eq!(s.seek_to(-3, SEEK_CUR), Some(0));
        assert_eq!(s.seek_to(-100, SEEK_CUR), Some(0));
    }

    /// The cursor and the write length both come from the plugin, and the buffer grows to
    /// `cursor + length`. A wild seek followed by any write must not turn into a multi-gigabyte
    /// allocation — that panics on capacity overflow inside a vtable thunk, which aborts the
    /// process instead of unwinding.
    #[test]
    fn huge_seek_then_write_is_refused_instead_of_allocating() {
        let s = MemoryStream::new(Vec::new());

        // Past the cap: the seek itself is refused, so the cursor never gets there.
        assert_eq!(s.seek_to(i64::MAX / 2, 0), None);
        assert_eq!(s.position(), 0);
        assert_eq!(s.seek_to(MAX_STREAM_BYTES as i64 + 1, 0), None);
        assert_eq!(s.position(), 0);

        // At the cap the seek is allowed, but the write that would grow past it is not, and
        // the stream is left untouched.
        assert_eq!(
            s.seek_to(MAX_STREAM_BYTES as i64, 0),
            Some(MAX_STREAM_BYTES as i64)
        );
        assert_eq!(s.write_at_cursor(&[1]), None);
        assert!(s.to_vec().is_empty());
    }

    /// The same refusal through the COM vtable, which is where a real plugin arrives: a result
    /// code and a zero byte count, never a panic.
    #[test]
    fn com_write_past_the_cap_reports_an_error_code() {
        let s = MemoryStream::new(Vec::new());
        let mut byte = 0u8;
        let mut written: i32 = -1;

        unsafe {
            // A seek beyond the cap is rejected outright...
            let mut pos: i64 = -1;
            assert_eq!(
                s.seek(MAX_STREAM_BYTES as i64 * 4, 0, &mut pos),
                kInvalidArgument
            );

            // ...and a write from a legal cursor that would cross it fails cleanly.
            assert_eq!(s.seek(MAX_STREAM_BYTES as i64, 0, &mut pos), kResultOk);
            assert_eq!(pos, MAX_STREAM_BYTES as i64);
            let result = s.write(
                &mut byte as *mut u8 as *mut std::ffi::c_void,
                1,
                &mut written,
            );
            assert_eq!(result, kOutOfMemory);
            assert_eq!(written, 0);
        }
        assert!(s.to_vec().is_empty());
    }

    /// Growth is bounded like every other host-side buffer: a plugin that keeps writing hits
    /// the cap and is told so, rather than growing the host's memory without limit.
    #[test]
    fn total_size_is_capped() {
        let s = MemoryStream::new(Vec::new());
        // Land one byte short of the cap, then write two.
        assert_eq!(
            s.seek_to(MAX_STREAM_BYTES as i64 - 1, 0),
            Some(MAX_STREAM_BYTES as i64 - 1)
        );
        assert_eq!(s.write_at_cursor(&[1]), Some(1));
        assert_eq!(s.to_vec().len(), MAX_STREAM_BYTES);
        assert_eq!(s.write_at_cursor(&[2]), None);
        assert_eq!(s.to_vec().len(), MAX_STREAM_BYTES);
    }

    /// Read a UTF-16 attribute back the way a plugin would, or `None` when the stream does
    /// not carry it.
    fn read_attribute(stream: &MemoryStream, key: &CStr) -> Option<String> {
        unsafe {
            let attributes = ComRef::<IAttributeList>::from_raw(stream.getAttributes())
                .expect("MemoryStream must vend its owned attribute list");
            let mut buf = [0u16; 512];
            if attributes.getString(
                key.as_ptr(),
                buf.as_mut_ptr(),
                std::mem::size_of_val(&buf) as u32,
            ) != kResultOk
            {
                return None;
            }
            let end = buf.iter().position(|unit| *unit == 0).unwrap_or(buf.len());
            Some(String::from_utf16_lossy(&buf[..end]))
        }
    }

    #[test]
    fn stream_attributes_expose_bounded_filename_and_state_type() {
        let long_name = "x".repeat(300);
        let stream = MemoryStream::with_metadata(
            vec![1, 2, 3],
            StreamMetadata {
                file_name: Some(&long_name),
                ..StreamMetadata::new(StreamStateType::Project)
            },
        );
        unsafe {
            let mut name: String128 = [0; 128];
            assert_eq!(stream.getFileName(&mut name), kResultOk);
            assert_eq!(name[..MAX_STREAM_FILENAME_UNITS], [u16::from(b'x'); 127]);
            assert_eq!(name[MAX_STREAM_FILENAME_UNITS], 0);
        }
        assert_eq!(
            read_attribute(&stream, c"StateType").as_deref(),
            Some("Project")
        );
        // No source file was named, so the path attribute must be absent rather than empty.
        assert_eq!(read_attribute(&stream, c"FilePathString"), None);
    }

    /// A preset restore tells the plugin both that the bytes came from a preset and which
    /// file they came from; a project restore tells it neither.
    #[test]
    fn a_restore_stream_publishes_the_context_it_was_built_from() {
        let preset = create_state_restore_stream(
            vec![7],
            &StateContext::preset_from_path("/Users/me/Presets/Big Lead.vstpreset"),
        );
        assert_eq!(
            read_attribute(&preset, c"StateType").as_deref(),
            Some("TrackPreset")
        );
        assert_eq!(
            read_attribute(&preset, c"FilePathString").as_deref(),
            Some("/Users/me/Presets/Big Lead.vstpreset")
        );
        unsafe {
            let mut name: String128 = [0; 128];
            assert_eq!(preset.getFileName(&mut name), kResultOk);
            let end = name
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(name.len());
            assert_eq!(String::from_utf16_lossy(&name[..end]), "Big Lead");
        }

        let pathless = create_state_restore_stream(vec![7], &StateContext::preset());
        assert_eq!(
            read_attribute(&pathless, c"StateType").as_deref(),
            Some("TrackPreset")
        );
        assert_eq!(read_attribute(&pathless, c"FilePathString"), None);

        let project = create_state_restore_stream(vec![7], &StateContext::Project);
        assert_eq!(
            read_attribute(&project, c"StateType").as_deref(),
            Some("Project")
        );
        assert_eq!(read_attribute(&project, c"FilePathString"), None);
    }
}

#[cfg(test)]
mod native_callback_thread_tests {
    use super::*;

    #[test]
    fn native_callback_thread_perform_rejects_other_thread_without_dsp_value() {
        let wrapper = ComWrapper::new(ComponentHandler::new(Arc::new(Mutex::new(Vec::new()))));
        let callbacks = wrapper
            .to_com_ptr::<IComponentHandler>()
            .expect("actual native parameter ABI");
        unsafe {
            assert_eq!(callbacks.beginEdit(5), kResultOk);
        }
        let result = thread::spawn(move || unsafe { callbacks.performEdit(5, 0.75) })
            .join()
            .expect("callback thread");
        assert_eq!(
            result, kResultFalse,
            "native author values must come from the initializing control thread"
        );
        assert!(
            wrapper
                .parameter_changes
                .lock()
                .expect("DSP feedback")
                .is_empty(),
            "wrong-thread callbacks cannot enter the execution sink"
        );
    }

    #[test]
    fn native_callback_thread_boundaries_reject_other_thread() {
        for kind in 0..5 {
            let wrapper = ComWrapper::new(ComponentHandler::new(Arc::new(Mutex::new(Vec::new()))));
            let parameter = wrapper
                .to_com_ptr::<IComponentHandler>()
                .expect("actual native parameter ABI");
            let group = wrapper
                .to_com_ptr::<IComponentHandler2>()
                .expect("actual native group ABI");
            let result = thread::spawn(move || unsafe {
                match kind {
                    0 => parameter.beginEdit(5),
                    1 => parameter.endEdit(5),
                    2 => group.startGroupEdit(),
                    3 => group.finishGroupEdit(),
                    _ => group.setDirty(1),
                }
            })
            .join()
            .expect("callback thread");
            assert_eq!(
                result, kResultFalse,
                "native author boundary {kind} must remain on its initializing control thread"
            );
        }
    }

    #[test]
    fn native_callback_thread_control_requests_reject_other_thread_without_requests() {
        let wrapper = ComWrapper::new(ComponentHandler::new(Arc::new(Mutex::new(Vec::new()))));
        let parameter = wrapper
            .to_com_ptr::<IComponentHandler>()
            .expect("actual native parameter ABI");
        let group = wrapper
            .to_com_ptr::<IComponentHandler2>()
            .expect("actual native group ABI");
        let (restart, open) = thread::spawn(move || unsafe {
            (
                parameter.restartComponent(1),
                group.requestOpenEditor(c"editor".as_ptr()),
            )
        })
        .join()
        .expect("callback thread");
        assert_eq!(
            restart, kResultFalse,
            "restartComponent is a control-thread callback"
        );
        assert_eq!(
            open, kResultFalse,
            "requestOpenEditor is a control-thread callback"
        );
        assert!(wrapper.take_restart_flags().is_empty());
        assert!(wrapper.take_host_notifications().is_empty());
        unsafe {
            assert_eq!(wrapper.restartComponent(1), kResultOk);
            assert_eq!(wrapper.requestOpenEditor(c"editor".as_ptr()), kResultOk);
        }
        assert_eq!(wrapper.take_restart_flags().bits(), 1);
        assert_eq!(wrapper.take_host_notifications().len(), 1);
    }
}

#[cfg(test)]
mod native_callback_thread_contract_tests {
    use super::*;
    use crate::plugin::{NativeEditorEventDrain, NativeEditorEventFailure};

    #[test]
    fn native_wrong_thread_author_source_is_sticky_without_fake_end() {
        let wrapper = ComWrapper::new(ComponentHandler::new(Arc::new(Mutex::new(Vec::new()))));
        unsafe {
            assert_eq!(wrapper.beginEdit(5), kResultOk);
        }
        let callbacks = wrapper
            .to_com_ptr::<IComponentHandler>()
            .expect("actual native parameter ABI");
        assert_eq!(
            thread::spawn(move || unsafe { callbacks.performEdit(5, 0.5) })
                .join()
                .expect("callback thread"),
            kResultFalse
        );
        let incomplete = NativeEditorEventDrain::Incomplete {
            failure: NativeEditorEventFailure::WrongThread,
        };
        assert_eq!(wrapper.take_editor_events(), incomplete);
        unsafe {
            assert_eq!(wrapper.endEdit(5), kResultFalse);
            assert_eq!(wrapper.startGroupEdit(), kResultFalse);
            assert_eq!(wrapper.setDirty(1), kResultFalse);
        }
        assert_eq!(
            wrapper.take_editor_events(),
            incomplete,
            "main-thread polling cannot manufacture a complete source after an invalid producer"
        );
        assert!(wrapper
            .parameter_changes
            .lock()
            .expect("DSP feedback")
            .is_empty());
        let fresh = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(fresh.beginEdit(5), kResultOk);
        }
        assert!(
            matches!(fresh.take_editor_events(), NativeEditorEventDrain::Complete { events } if events.len() == 1)
        );
    }

    #[test]
    fn native_wrong_thread_control_refusal_does_not_corrupt_author_source() {
        let wrapper = ComWrapper::new(ComponentHandler::new(Arc::new(Mutex::new(Vec::new()))));
        let callbacks = wrapper
            .to_com_ptr::<IComponentHandler>()
            .expect("actual native parameter ABI");
        assert_eq!(
            thread::spawn(move || unsafe { callbacks.restartComponent(1) })
                .join()
                .expect("callback thread"),
            kResultFalse
        );
        assert_eq!(
            wrapper.take_editor_events(),
            NativeEditorEventDrain::Complete { events: Vec::new() }
        );
        unsafe {
            assert_eq!(wrapper.beginEdit(5), kResultOk);
            assert_eq!(wrapper.performEdit(5, 0.5), kResultOk);
            assert_eq!(wrapper.endEdit(5), kResultOk);
        }
        assert!(
            matches!(wrapper.take_editor_events(), NativeEditorEventDrain::Complete { events } if events.len() == 3)
        );
    }
}

#[cfg(test)]
mod restart_cohort_tests {
    use super::*;

    #[test]
    fn native_vst3_restart_cohort_exact_retirement_preserves_same_bit_callback() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            handler.restartComponent(RestartFlags_::kLatencyChanged);
        }
        let serviced = handler.pending_restart_cohort();
        unsafe {
            handler.restartComponent(RestartFlags_::kLatencyChanged);
        }
        assert!(!handler.retire_restart_cohort(serviced));
        let current = handler.pending_restart_cohort();
        assert_eq!(current.serial(), 2);
        assert_eq!(current.flags().bits(), RestartFlags_::kLatencyChanged);
        assert!(handler.retire_restart_cohort(current));
        assert_eq!(handler.pending_restart_cohort().serial(), 2);
        assert!(handler.pending_restart_flags().is_empty());
        unsafe {
            handler.restartComponent(i32::MIN);
        }
        assert_eq!(handler.pending_restart_cohort().serial(), 3);
        assert_eq!(handler.take_restart_flags().bits(), i32::MIN);
        assert!(!handler.retire_restart_cohort(current));
    }

    #[test]
    fn parameter_restart_marker_survives_raw_retirement_and_independent_author_drain() {
        use crate::plugin::{NativeEditorEvent as Event, NativeEditorEventDrain};
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.restartComponent(8), kResultOk);
        }
        let latency = handler.pending_restart_cohort();
        assert!(handler.retire_restart_cohort(latency));
        assert!(handler.editor_event_source_status().closed_empty());
        unsafe {
            assert_eq!(handler.beginEdit(7), kResultOk);
            assert_eq!(handler.restartComponent(4), kResultOk);
            assert_eq!(handler.endEdit(7), kResultOk);
        }
        let cohort = handler.pending_restart_cohort();
        let source = handler.editor_event_source_status();
        assert_eq!(source.pending_events(), 3);
        assert_eq!(cohort.serial(), 2);
        assert_eq!(handler.take_restart_flags().bits(), 4);
        assert_eq!(handler.editor_event_source_status(), source);
        assert!(handler.claim_retired_restart_serial(cohort.serial()));
        assert_eq!(handler.editor_event_source_status(), source);
        let NativeEditorEventDrain::Complete { events } = handler.take_editor_events() else {
            panic!("complete independently drained source")
        };
        assert!(matches!(&events[0], Event::Parameter(edit) if edit.id == 7));
        assert_eq!(
            events[1],
            Event::ParameterRescanRequested { flags: 4, cohort }
        );
        assert!(matches!(&events[2], Event::Parameter(edit) if edit.id == 7));
        assert!(handler.editor_event_source_status().closed_empty());
        assert_eq!(
            handler.editor_event_source_status().revision(),
            source.revision()
        );
        assert!(!handler.pending_restart_cohort().pending());
    }

    #[test]
    fn parameter_restart_marker_preserves_mixed_unknown_bits_and_raw_exhaustion() {
        use crate::plugin::{NativeEditorEvent as Event, NativeEditorEventDrain};
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            for flags in [0, 8, 2, i32::MIN] {
                assert_eq!(handler.restartComponent(flags), kResultOk);
            }
        }
        assert!(handler.editor_event_source_status().closed_empty());
        let flags = 4 | i32::MIN;
        unsafe {
            assert_eq!(handler.restartComponent(flags), kResultOk);
        }
        let cohort = handler.pending_restart_cohort();
        assert_eq!(cohort.serial(), 4);
        assert_eq!(cohort.flags().bits(), 8 | 2 | flags);
        assert_eq!(
            handler.take_editor_events(),
            NativeEditorEventDrain::Complete {
                events: vec![Event::ParameterRescanRequested { flags, cohort }],
            }
        );
        handler
            .restart_cohort
            .store(u64::from(u32::MAX - 1) << 32, Ordering::Release);
        unsafe {
            assert_eq!(handler.restartComponent(20), kResultOk);
        }
        let exhausted = handler.pending_restart_cohort();
        assert!(exhausted.exhausted());
        let source = handler.editor_event_source_status();
        assert!(!handler.retire_restart_cohort(exhausted));
        assert_eq!(handler.take_restart_flags().bits(), 20);
        assert_eq!(handler.editor_event_source_status(), source);
    }

    #[test]
    fn parameter_restart_marker_overflow_keeps_raw_obligation_and_sticky_loss() {
        use crate::plugin::NativeEditorEventFailure;
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            for _ in 0..MAX_EDITOR_FEEDBACK {
                assert_eq!(handler.setDirty(0), kResultOk);
            }
            assert_eq!(handler.restartComponent(4), kResultFalse);
        }
        let source = handler.editor_event_source_status();
        assert_eq!(source.failure(), Some(NativeEditorEventFailure::Overflow));
        assert_eq!(source.pending_events(), MAX_EDITOR_FEEDBACK);
        let cohort = handler.pending_restart_cohort();
        assert_eq!(cohort.serial(), 1);
        assert_eq!(cohort.flags().bits(), 4);
        let _ = handler.take_editor_events();
        assert_eq!(
            handler.editor_event_source_status().failure(),
            source.failure()
        );
        assert_eq!(handler.pending_restart_cohort(), cohort);
    }

    #[test]
    fn native_vst3_restart_cohort_exhaustion_never_wraps_or_drains() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        handler
            .restart_cohort
            .store(u64::from(u32::MAX - 1) << 32, Ordering::Release);
        unsafe {
            handler.restartComponent(RestartFlags_::kLatencyChanged);
        }
        let current = handler.pending_restart_cohort();
        assert!(current.exhausted());
        assert!(!handler.retire_restart_cohort(current));
        assert!(handler.take_restart_flags().latency_changed());
        unsafe {
            handler.restartComponent(i32::MIN);
        }
        assert_eq!(handler.pending_restart_cohort().serial(), u32::MAX);
        assert_eq!(
            handler.pending_restart_flags().bits(),
            RestartFlags_::kLatencyChanged | i32::MIN
        );
    }
}

#[cfg(test)]
mod native_author_boundary_old_api_tests {
    use super::*;

    #[test]
    fn native_author_unmatched_parameter_end_explicitly_refuses_callback() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(
                handler.endEdit(5),
                kResultFalse,
                "an unmatched native EndEdit cannot silently create a complete author boundary"
            );
        }
    }

    #[test]
    fn native_author_unmatched_group_end_explicitly_refuses_callback() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(
                handler.finishGroupEdit(),
                kResultFalse,
                "an unmatched native group End cannot silently report a complete author source"
            );
        }
    }
}

#[cfg(test)]
mod native_source_observation_contracts {
    use super::*;
    use crate::plugin::{NativeEditorEvent, ParameterEdit, ParameterEditKind};

    #[test]
    fn source_observation_copies_complete_order_and_float_bits_without_consumption() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.startGroupEdit(), kResultOk);
            assert_eq!(handler.beginEdit(3), kResultOk);
            assert_eq!(handler.performEdit(3, -0.0), kResultOk);
            assert_eq!(handler.setDirty(1), kResultOk);
            assert_eq!(handler.performEdit(7, 0.75), kResultOk);
            assert_eq!(handler.endEdit(3), kResultOk);
            assert_eq!(handler.finishGroupEdit(), kResultOk);
            assert_eq!(handler.restartComponent(20), kResultOk);
        }
        let before = handler.editor_event_source_status();
        let cohort = handler.pending_restart_cohort();
        let mut events = Vec::with_capacity(MAX_EDITOR_FEEDBACK);
        let mut feedback = Vec::with_capacity(MAX_EDITOR_FEEDBACK);
        for _ in 0..2 {
            assert_eq!(
                handler
                    .observe_editor_source_bounded(MAX_EDITOR_FEEDBACK, &mut events, &mut feedback)
                    .unwrap(),
                before
            );
            assert_eq!(events.len(), 8);
            assert_eq!(feedback.len(), 2);
            assert_eq!(feedback[0].0, 3);
            assert_eq!(feedback[0].1.to_bits(), (-0.0_f64).to_bits());
            assert_eq!(feedback[1], (7, 0.75));
            assert!(matches!(&events[0], NativeEditorEvent::GroupEditStarted));
            assert!(matches!(
                &events[1],
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 3,
                    kind: ParameterEditKind::BeginGesture,
                    value: None
                })
            ));
            assert!(
                matches!(&events[2], NativeEditorEvent::Parameter(ParameterEdit { id: 3, kind: ParameterEditKind::ValueChange, value: Some(value) }) if value.to_bits() == (-0.0_f64).to_bits())
            );
            assert!(matches!(&events[3], NativeEditorEvent::DirtyChanged(true)));
            assert!(matches!(
                &events[4],
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 7,
                    kind: ParameterEditKind::ValueChange,
                    value: Some(0.75)
                })
            ));
            assert!(matches!(
                &events[5],
                NativeEditorEvent::Parameter(ParameterEdit {
                    id: 3,
                    kind: ParameterEditKind::EndGesture,
                    value: None
                })
            ));
            assert!(matches!(&events[6], NativeEditorEvent::GroupEditFinished));
            assert_eq!(
                events[7],
                NativeEditorEvent::ParameterRescanRequested { flags: 20, cohort }
            );
            assert_eq!(handler.editor_event_source_status(), before);
            assert_eq!(handler.pending_restart_cohort(), cohort);
        }
    }

    #[test]
    fn source_observation_refuses_small_destinations_and_bounds_without_partial_output() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.performEdit(3, 0.25), kResultOk);
            assert_eq!(handler.performEdit(7, 0.75), kResultOk);
        }
        let before = handler.editor_event_source_status();
        for (maximum, event_capacity, feedback_capacity) in [
            (MAX_EDITOR_FEEDBACK + 1, 2, 2),
            (1, 2, 2),
            (2, 1, 2),
            (2, 2, 1),
        ] {
            let mut events = Vec::with_capacity(event_capacity);
            events.push(NativeEditorEvent::DirtyChanged(false));
            let mut feedback = Vec::with_capacity(feedback_capacity);
            feedback.push((99, -0.0));
            assert!(handler
                .observe_editor_source_bounded(maximum, &mut events, &mut feedback)
                .is_err());
            assert_eq!(events, [NativeEditorEvent::DirtyChanged(false)]);
            assert_eq!(feedback.len(), 1);
            assert_eq!(feedback[0].0, 99);
            assert_eq!(feedback[0].1.to_bits(), (-0.0_f64).to_bits());
            assert_eq!(handler.editor_event_source_status(), before);
        }
    }

    #[test]
    fn source_observation_refuses_sticky_overflow_without_consuming_retained_prefix() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        for _ in 0..MAX_EDITOR_FEEDBACK {
            assert_eq!(unsafe { handler.performEdit(3, 0.25) }, kResultOk);
        }
        assert_eq!(unsafe { handler.performEdit(3, 0.5) }, kResultFalse);
        let before = handler.editor_event_source_status();
        assert_eq!(
            before.failure(),
            Some(crate::plugin::NativeEditorEventFailure::Overflow)
        );
        let mut events = Vec::with_capacity(MAX_EDITOR_FEEDBACK);
        events.push(NativeEditorEvent::DirtyChanged(false));
        let mut feedback = Vec::with_capacity(MAX_EDITOR_FEEDBACK);
        feedback.push((99, -0.0));
        assert!(handler
            .observe_editor_source_bounded(MAX_EDITOR_FEEDBACK, &mut events, &mut feedback)
            .is_err());
        assert_eq!(events, [NativeEditorEvent::DirtyChanged(false)]);
        assert_eq!(feedback.len(), 1);
        assert_eq!(feedback[0].0, 99);
        assert_eq!(feedback[0].1.to_bits(), (-0.0_f64).to_bits());
        assert_eq!(handler.editor_event_source_status(), before);
    }
}

#[cfg(test)]
mod native_author_source_status_contracts {
    use super::*;
    use crate::plugin::{NativeEditorEventDrain, NativeEditorEventFailure};

    fn complete_count(handler: &ComponentHandler) -> usize {
        match handler.take_editor_events() {
            NativeEditorEventDrain::Complete { events } => events.len(),
            NativeEditorEventDrain::Incomplete { failure } => {
                panic!("expected complete source: {failure:?}")
            }
        }
    }

    #[test]
    fn native_author_status_tracks_exact_interleaved_parameter_owners_across_drains() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        assert!(handler.editor_event_source_status().closed_empty());
        unsafe {
            assert_eq!(handler.beginEdit(5), kResultOk);
            assert_eq!(handler.beginEdit(9), kResultOk);
            assert_eq!(handler.performEdit(5, 0.25), kResultOk);
        }
        let before = handler.editor_event_source_status();
        assert_eq!(before.revision(), 3);
        assert_eq!(before.pending_events(), 3);
        assert_eq!(before.open_parameter_edits(), 2);
        assert_eq!(complete_count(&handler), 3);
        let drained = handler.editor_event_source_status();
        assert_eq!(drained.pending_events(), 0);
        assert_eq!(
            drained.open_parameter_edits(),
            2,
            "Drain cannot manufacture matching native Ends"
        );
        assert_eq!(drained.revision(), before.revision());
        assert!(
            !drained.closed_empty(),
            "empty native outbox still owns two open parameter gestures"
        );
        unsafe {
            assert_eq!(handler.endEdit(9), kResultOk);
        }
        assert_eq!(complete_count(&handler), 1);
        assert_eq!(
            handler.editor_event_source_status().open_parameter_edits(),
            1
        );
        unsafe {
            assert_eq!(handler.endEdit(5), kResultOk);
        }
        assert_eq!(complete_count(&handler), 1);
        assert!(handler.editor_event_source_status().closed_empty());
        assert_eq!(handler.editor_event_source_status().revision(), 5);
    }

    #[test]
    fn native_author_status_preserve_does_not_drain_grouped_dirty_source() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.startGroupEdit(), kResultOk);
            assert_eq!(handler.beginEdit(5), kResultOk);
            assert_eq!(handler.setDirty(1), kResultOk);
            assert_eq!(handler.endEdit(5), kResultOk);
        }
        let first = handler.editor_event_source_status();
        for _ in 0..8 {
            assert_eq!(handler.editor_event_source_status(), first);
        }
        assert_eq!(first.pending_events(), 4);
        assert_eq!(first.open_parameter_edits(), 0);
        assert_eq!(first.group_depth(), 1);
        assert_eq!(complete_count(&handler), 4);
        assert!(
            !handler.editor_event_source_status().closed_empty(),
            "group still open after a parameter End"
        );
        unsafe {
            assert_eq!(handler.finishGroupEdit(), kResultOk);
        }
        assert_eq!(complete_count(&handler), 1);
        assert!(handler.editor_event_source_status().closed_empty());
    }

    #[test]
    fn native_author_status_accepts_official_multi_fader_timestamp_group_pattern() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.startGroupEdit(), kResultOk);
            assert_eq!(handler.beginEdit(1), kResultOk);
            assert_eq!(handler.beginEdit(3), kResultOk);
            assert_eq!(handler.finishGroupEdit(), kResultOk);
        }
        assert_eq!(complete_count(&handler), 4);
        let down = handler.editor_event_source_status();
        assert_eq!(down.open_parameter_edits(), 2);
        assert_eq!(down.group_depth(), 0);
        assert!(
            !down.closed_empty(),
            "timestamp group finish cannot synthesize parameter Ends"
        );
        for value in [0.25, 0.5, 0.75] {
            unsafe {
                assert_eq!(handler.startGroupEdit(), kResultOk);
                assert_eq!(handler.performEdit(1, value), kResultOk);
                assert_eq!(handler.performEdit(3, value), kResultOk);
                assert_eq!(handler.finishGroupEdit(), kResultOk);
            }
            assert_eq!(complete_count(&handler), 4);
            assert_eq!(
                handler.editor_event_source_status().open_parameter_edits(),
                2
            );
            assert!(!handler.editor_event_source_status().closed_empty());
        }
        unsafe {
            assert_eq!(handler.startGroupEdit(), kResultOk);
            assert_eq!(handler.endEdit(1), kResultOk);
            assert_eq!(handler.endEdit(3), kResultOk);
            assert_eq!(handler.finishGroupEdit(), kResultOk);
        }
        assert_eq!(complete_count(&handler), 4);
        assert!(handler.editor_event_source_status().closed_empty());
    }

    #[test]
    fn native_author_status_accounts_repeated_begin_same_id_without_extra_prohibition() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.beginEdit(5), kResultOk);
            assert_eq!(handler.beginEdit(5), kResultOk);
        }
        assert_eq!(complete_count(&handler), 2);
        assert_eq!(
            handler.editor_event_source_status().open_parameter_edits(),
            2
        );
        unsafe {
            assert_eq!(handler.endEdit(5), kResultOk);
        }
        assert_eq!(complete_count(&handler), 1);
        assert_eq!(
            handler.editor_event_source_status().open_parameter_edits(),
            1
        );
        assert!(!handler.editor_event_source_status().closed_empty());
        unsafe {
            assert_eq!(handler.endEdit(5), kResultOk);
        }
        assert_eq!(complete_count(&handler), 1);
        assert!(handler.editor_event_source_status().closed_empty());
    }

    #[test]
    fn native_author_invalid_boundary_remains_sticky_after_drain() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.endEdit(5), kResultFalse);
        }
        assert_eq!(
            handler.editor_event_source_status().failure(),
            Some(NativeEditorEventFailure::InvalidBoundary)
        );
        assert!(matches!(
            handler.take_editor_events(),
            NativeEditorEventDrain::Incomplete {
                failure: NativeEditorEventFailure::InvalidBoundary
            }
        ));
        unsafe {
            assert_eq!(handler.beginEdit(5), kResultFalse);
        }
        assert!(!handler.editor_event_source_status().closed_empty());
    }

    #[test]
    fn native_author_revision_exhaustion_refuses_without_wrapping_or_fake_end() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        handler.editor_events.lock().expect("source").revision = u64::MAX;
        unsafe {
            assert_eq!(handler.beginEdit(5), kResultFalse);
        }
        let status = handler.editor_event_source_status();
        assert_eq!(status.revision(), u64::MAX);
        assert_eq!(status.open_parameter_edits(), 0);
        assert_eq!(status.pending_events(), 0);
        assert_eq!(
            status.failure(),
            Some(NativeEditorEventFailure::SequenceExhausted)
        );
        assert!(!status.closed_empty());
    }
}

#[cfg(test)]
mod native_author_capture_catchup_contracts {
    use super::*;
    use crate::plugin::{NativeEditorEventDrain, NativeEditorEventFailure};

    #[test]
    fn native_author_catchup_visits_exact_value_order_without_draining_boundaries() {
        let feedback = Arc::new(Mutex::new(Vec::new()));
        let handler = ComponentHandler::new(feedback.clone());
        unsafe {
            assert_eq!(handler.beginEdit(3), kResultOk);
            assert_eq!(handler.beginEdit(9), kResultOk);
            assert_eq!(handler.performEdit(9, 0.25), kResultOk);
            assert_eq!(handler.performEdit(3, 0.5), kResultOk);
        }
        let before = handler.editor_event_source_status();
        let mut delivered = Vec::new();
        assert!(handler.visit_unconsumed_editor_values(|id, value| {
            delivered.push((id, value));
            true
        }));
        assert_eq!(
            delivered,
            [(9, 0.25), (3, 0.5)],
            "same processor catchup retains native callback order"
        );
        assert!(feedback.lock().expect("feedback").is_empty());
        let after = handler.editor_event_source_status();
        assert_eq!(after.revision(), before.revision());
        assert_eq!(
            after.pending_events(),
            4,
            "DSP delivery never consumes author Begin/Value/End history"
        );
        assert_eq!(after.open_parameter_edits(), 2);
        assert!(!after.closed_empty());
    }

    #[test]
    fn native_author_catchup_refuses_feedback_orphaned_by_an_independent_event_drain() {
        let feedback = Arc::new(Mutex::new(Vec::new()));
        let handler = ComponentHandler::new(feedback.clone());
        unsafe {
            assert_eq!(handler.beginEdit(3), kResultOk);
            assert_eq!(handler.performEdit(3, 0.25), kResultOk);
        }
        assert!(matches!(
            handler.take_editor_events(),
            NativeEditorEventDrain::Complete { .. }
        ));
        let mut calls = 0;
        assert!(!handler.visit_unconsumed_editor_values(|_, _| {
            calls += 1;
            true
        }));
        assert_eq!(
            calls, 0,
            "no foreign process receives an incomplete history interpretation"
        );
        assert_eq!(
            *feedback.lock().expect("feedback"),
            [(3, 0.25)],
            "orphaned real native feedback cannot be silently cleared"
        );
    }

    #[test]
    fn native_author_catchup_capacity_failure_preserves_feedback_and_marks_whole_source_incomplete()
    {
        let feedback = Arc::new(Mutex::new(Vec::new()));
        let handler = ComponentHandler::new(feedback.clone());
        unsafe {
            assert_eq!(handler.beginEdit(3), kResultOk);
            assert_eq!(handler.performEdit(3, 0.25), kResultOk);
            assert_eq!(handler.performEdit(3, 0.5), kResultOk);
        }
        let mut capacity = 1;
        assert!(!handler.visit_unconsumed_editor_values(|_, _| {
            if capacity == 0 {
                false
            } else {
                capacity -= 1;
                true
            }
        }));
        assert_eq!(*feedback.lock().expect("feedback"), [(3, 0.25), (3, 0.5)]);
        assert_eq!(
            handler.editor_event_source_status().failure(),
            Some(NativeEditorEventFailure::Overflow)
        );
        assert!(matches!(
            handler.take_editor_events(),
            NativeEditorEventDrain::Incomplete {
                failure: NativeEditorEventFailure::Overflow
            }
        ));
        assert!(!handler.visit_unconsumed_editor_values(|_, _| panic!(
            "a failed source cannot replay a prefix"
        )));
    }

    #[test]
    fn native_author_capture_allows_real_dirty_false_suffix_without_fabricating_a_saved_project() {
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        unsafe {
            assert_eq!(handler.setDirty(1), kResultOk);
        }
        let before = handler.editor_event_source_status();
        unsafe {
            assert_eq!(handler.setDirty(0), kResultOk);
            assert_eq!(handler.setDirty(0), kResultOk);
        }
        let after = handler
            .editor_capture_source_status_since(before.revision())
            .expect("getState may clear native dirty");
        assert_eq!(
            after.pending_events(),
            3,
            "actual dirty false callbacks stay in the complete author delivery"
        );
        assert_eq!(after.revision(), before.revision() + 2);
        assert!(
            matches!(handler.take_editor_events(),NativeEditorEventDrain::Complete{events} if events==[
            crate::plugin::NativeEditorEvent::DirtyChanged(true),crate::plugin::NativeEditorEvent::DirtyChanged(false),crate::plugin::NativeEditorEvent::DirtyChanged(false)])
        );
    }

    #[test]
    fn native_author_capture_retries_on_new_value_boundary_or_dirty_true_without_consuming_it() {
        for kind in 0..3 {
            let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
            let revision = handler.editor_event_source_status().revision();
            unsafe {
                match kind {
                    0 => {
                        assert_eq!(handler.setDirty(1), kResultOk);
                    }
                    1 => {
                        assert_eq!(handler.beginEdit(3), kResultOk);
                    }
                    _ => {
                        assert_eq!(handler.beginEdit(3), kResultOk);
                        assert_eq!(handler.performEdit(3, 0.5), kResultOk);
                    }
                }
            }
            let before = handler.editor_event_source_status();
            assert!(handler
                .editor_capture_source_status_since(revision)
                .is_none());
            assert_eq!(
                handler.editor_event_source_status(),
                before,
                "failed coherent capture preserves every actual callback"
            );
        }
    }
}

#[path = "initial_parameter_input.rs"]
pub(crate) mod initial_parameter_input;

#[cfg(test)]
mod checked_note_input_tests {
    use super::*;
    use crate::error::NativeEventError as Failure;

    fn on(offset: i32, id: i32) -> PluginEvent {
        PluginEvent {
            bus_index: 0,
            sample_offset: offset,
            ppq_position: 0.0,
            flags: 0,
            data: PluginEventData::NoteOn {
                channel: 0,
                pitch: 60,
                tuning: 0.0,
                velocity: 0.0,
                length: 0,
                note_id: id,
            },
        }
    }

    #[test]
    fn checked_note_batch_rejects_legacy_input_or_chunks_without_discarding_them() {
        let list = HostEventList::new();
        list.add_event(on(0, 77));
        let mut chunks = Vec::with_capacity(4);
        assert_eq!(
            list.begin_checked_note_batch(&mut chunks, &[on(1, 1)]),
            Err(Failure::PendingEvents)
        );
        assert!(chunks.is_empty());
        assert_eq!(list.events.lock().unwrap()[0], on(0, 77));
        list.clear();
        chunks.push(Some(on(0, 88)));
        assert_eq!(
            list.begin_checked_note_batch(&mut chunks, &[on(1, 1)]),
            Err(Failure::PendingEvents)
        );
        assert_eq!(chunks, [Some(on(0, 88))]);
        assert!(list.events.lock().unwrap().is_empty());
    }

    #[test]
    fn checked_note_batch_checks_actual_capacities_before_mutation() {
        let list = HostEventList::new();
        let mut chunks = Vec::new();
        assert_eq!(
            list.begin_checked_note_batch(&mut chunks, &[on(0, 1)]),
            Err(Failure::Capacity)
        );
        assert!(chunks.is_empty());
        let list = HostEventList {
            events: Mutex::new(Vec::new()),
            payload_bytes: AtomicUsize::new(0),
        };
        let mut chunks = Vec::with_capacity(1);
        assert_eq!(
            list.begin_checked_note_batch(&mut chunks, &[on(0, 1)]),
            Err(Failure::Capacity)
        );
        assert!(chunks.is_empty());
        assert!(list.events.lock().unwrap().is_empty());
    }

    #[test]
    fn checked_note_batch_poison_refuses_before_consumption() {
        let list = HostEventList::new();
        let _ = std::panic::catch_unwind(|| {
            let _locked = list.events.lock().unwrap();
            panic!("owned poison control");
        });
        let mut chunks = Vec::with_capacity(1);
        assert_eq!(
            list.begin_checked_note_batch(&mut chunks, &[on(0, 1)]),
            Err(Failure::StoragePoisoned)
        );
        assert!(chunks.is_empty());
        assert!(list.events.lock().unwrap_err().into_inner().is_empty());
    }

    #[test]
    fn checked_note_chunks_keep_equal_offsets_and_rebase_without_replaying() {
        let list = HostEventList::new();
        let mut chunks = Vec::with_capacity(4);
        let events = [on(1, 3), on(1, 4), on(4, 5), on(7, 6)];
        list.begin_checked_note_batch(&mut chunks, &events).unwrap();
        list.stage_checked_note_chunk(&mut chunks, 0, 4).unwrap();
        assert_eq!(*list.events.lock().unwrap(), events[..2]);
        list.clear_consumed_note_events();
        list.stage_checked_note_chunk(&mut chunks, 4, 4).unwrap();
        let mut expected = events[2..].to_vec();
        expected[0].sample_offset = 0;
        expected[1].sample_offset = 3;
        assert_eq!(*list.events.lock().unwrap(), expected);
        assert!(chunks.iter().all(Option::is_none));
        list.clear_consumed_note_events();
        list.stage_checked_note_chunk(&mut chunks, 4, 4).unwrap();
        assert!(list.events.lock().unwrap().is_empty());
    }

    #[test]
    fn checked_note_batch_exact_capacity_and_one_past_limit_are_distinct() {
        let list = HostEventList::new();
        let events = vec![on(0, 1); 4096];
        let mut chunks = Vec::with_capacity(4096);
        list.begin_checked_note_batch(&mut chunks, &events).unwrap();
        assert_eq!(chunks.len(), 4096);
        chunks.clear();
        let events = vec![on(0, 1); 4097];
        assert_eq!(
            list.begin_checked_note_batch(&mut chunks, &events),
            Err(Failure::Capacity)
        );
        assert!(chunks.is_empty());
        assert!(list.events.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod active_note_owner_tests {
    use super::*;
    use crate::audio::ProcessMusicalReference;
    use crate::error::NativeEventError as Failure;

    fn note(offset: i32) -> PluginEvent {
        PluginEvent {
            bus_index: 0,
            sample_offset: offset,
            ppq_position: -3.5,
            flags: 0,
            data: PluginEventData::NoteOn {
                channel: 0,
                pitch: 60,
                tuning: 0.0,
                velocity: 0.5,
                length: 0,
                note_id: offset,
            },
        }
    }
    fn reference() -> ProcessMusicalReference {
        ProcessMusicalReference::new(-19, 7, 3, 4).unwrap()
    }

    #[test]
    fn active_checked_owner_transfers_one_backing_and_retained_com_is_empty_after_close() {
        use crate::host_storage::{HostStorageError, HostStorageRef, HostStorageVisitor};
        struct Actual(Vec<(usize, std::alloc::Layout)>);
        impl HostStorageVisitor for Actual {
            fn visit(&mut self, storage: HostStorageRef<'_>) -> Result<(), HostStorageError> {
                self.0.push((storage.address(), storage.layout()));
                Ok(())
            }
        }
        let owner = ComWrapper::new(ActiveNoteEventList::new());
        let retained_com = owner.to_com_ptr::<IEventList>().unwrap();
        let payload = HostStorageRef::com(&owner);
        assert_eq!(
            payload.kind(),
            crate::host_storage::HostStorageKind::ArcPayload
        );
        assert_eq!(payload.layout(), owner.payload_storage().layout());
        let mut chunks = Vec::with_capacity(4096);
        let original = (chunks.as_ptr() as usize, chunks.capacity());
        owner
            .begin(&mut chunks, &[note(0), note(4)], reference(), -17, 48_000)
            .unwrap();
        assert_eq!(chunks.capacity(), 0);
        let mut actual = Actual(Vec::new());
        owner.visit_owned_storage(&mut actual).unwrap();
        assert_eq!(
            actual.0,
            [(
                original.0,
                std::alloc::Layout::array::<Option<PluginEvent>>(original.1).unwrap()
            )]
        );
        owner.expose_chunk(4, 1).unwrap();
        owner.open();
        assert_eq!(unsafe { retained_com.getEventCount() }, 1);
        let mut raw: Event = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { retained_com.getEvent(0, &mut raw) }, kResultOk);
        assert_eq!(raw.sampleOffset, 0);
        assert_eq!(raw.ppqPosition.to_bits(), 0x400cf311e970828d);
        owner.close();
        assert_eq!(unsafe { retained_com.getEventCount() }, 0);
        assert_eq!(unsafe { retained_com.getEvent(0, &mut raw) }, kResultFalse);
        owner.finish(&mut chunks).unwrap();
        assert_eq!((chunks.as_ptr() as usize, chunks.capacity()), original);
        assert!(chunks.is_empty());
        drop(owner);
        assert_eq!(
            unsafe { retained_com.getEventCount() },
            0,
            "a real retained COM reference outlives the original wrapper"
        );
    }

    #[test]
    fn active_checked_owner_preflight_busy_recovers_but_cleanup_busy_retains_closed_backing() {
        let owner = ActiveNoteEventList::new();
        let mut chunks = Vec::with_capacity(4096);
        let original = (chunks.as_ptr(), chunks.capacity());
        let guard = owner.storage.try_lock().unwrap();
        assert_eq!(
            owner.begin(&mut chunks, &[note(0)], reference(), -17, 48_000),
            Err(Failure::StorageBusy)
        );
        assert_eq!(owner.require_quiescent(), Ok(()));
        assert_eq!((chunks.as_ptr(), chunks.capacity()), original);
        drop(guard);
        owner
            .begin(&mut chunks, &[note(0)], reference(), -17, 48_000)
            .unwrap();
        owner.expose_chunk(0, 1).unwrap();
        owner.open();
        assert_eq!(
            owner.test_finish_while_busy(&mut chunks),
            Err(Failure::StorageBusy)
        );
        assert_eq!(chunks.capacity(), 0);
        assert_eq!(owner.require_quiescent(), Err(Failure::StorageBusy));
        assert_eq!(unsafe { owner.getEventCount() }, 0);
        let storage = owner.storage.try_lock().unwrap();
        assert_eq!(
            (storage.events.as_ptr(), storage.events.capacity()),
            original
        );
        assert_eq!(storage.events.len(), 1);
    }

    #[test]
    fn active_checked_owner_poison_cleanup_returns_same_backing_and_stays_failed() {
        let owner = ActiveNoteEventList::new();
        let mut chunks = Vec::with_capacity(4096);
        let original = (chunks.as_ptr(), chunks.capacity());
        owner
            .begin(&mut chunks, &[note(0)], reference(), -17, 48_000)
            .unwrap();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.storage.try_lock().unwrap();
            panic!("owned mutex poison control");
        }));
        assert!(poisoned.is_err());
        assert_eq!(owner.finish(&mut chunks), Err(Failure::StoragePoisoned));
        assert_eq!((chunks.as_ptr(), chunks.capacity()), original);
        assert!(chunks.is_empty());
        assert_eq!(owner.require_quiescent(), Err(Failure::StoragePoisoned));
        assert_eq!(unsafe { owner.getEventCount() }, 0);
    }
    #[test]
    fn parameter_flush_requires_empty_healthy_checked_storage_without_latching_pure_busy() {
        let owner = ActiveNoteEventList::new();
        {
            let _guard = owner.storage.lock().unwrap();
            assert_eq!(
                owner.require_empty_for_parameter_flush(),
                Err(crate::error::NativeEventError::StorageBusy)
            );
        }
        assert_eq!(owner.require_quiescent(), Ok(()));
        owner.open();
        assert_eq!(
            owner.require_empty_for_parameter_flush(),
            Err(crate::error::NativeEventError::PendingEvents)
        );
        owner.close();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.storage.lock().unwrap();
            panic!("controlled checked owner poison");
        }))
        .is_err());
        assert_eq!(
            owner.require_empty_for_parameter_flush(),
            Err(crate::error::NativeEventError::StoragePoisoned)
        );
    }

    #[test]
    fn pending_flush_projection_rechecks_original_source_feedback_and_input_before_swap() {
        use crate::plugin::PendingParameterFlushRefusal as Failure;
        let changes = ParameterChanges::default();
        changes.enqueue(99, 2, 0.125);
        changes.clear_all();
        let old = changes.queues.lock().unwrap()[0]
            .to_com_ptr::<IParamValueQueue>()
            .unwrap();
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        let values = [crate::parameters::ParameterChange {
            id: 7,
            sample_offset: 0,
            value: 0.75,
        }];
        assert_eq!(changes.pending_flush_input(Some(&handler), None), Ok(()));
        let mut candidate = PendingParameterProjection::prepare(&values, 4096).unwrap();
        {
            let _guard = handler.editor_events.lock().unwrap();
            assert_eq!(
                changes.pending_flush_input(Some(&handler), Some(&mut candidate)),
                Err(Failure::StorageBusy)
            );
        }
        {
            let _guard = handler.parameter_changes.lock().unwrap();
            assert_eq!(
                changes.pending_flush_input(Some(&handler), Some(&mut candidate)),
                Err(Failure::StorageBusy)
            );
        }
        // A real callback between initial check and publication must be seen.
        assert_eq!(unsafe { handler.performEdit(7, 0.625) }, kResultOk);
        let status = handler.editor_event_source_status();
        assert_eq!(
            changes.pending_flush_input(Some(&handler), Some(&mut candidate)),
            Err(Failure::SourceValuesPending)
        );
        assert_eq!(handler.editor_event_source_status(), status);
        assert_eq!(changes.used.load(Ordering::Relaxed), 0);
        assert_eq!(
            changes.queues.lock().unwrap()[0]
                .as_com_ref::<IParamValueQueue>()
                .unwrap()
                .as_ptr(),
            old.as_ptr()
        );
        let (mut offset, mut value) = (0, 0.0);
        assert_eq!(
            unsafe { old.getPoint(0, &mut offset, &mut value) },
            kResultOk
        );
        assert_eq!((offset, value), (2, 0.125));
    }
    #[test]
    fn pending_flush_restart_exhaustion_keeps_source_and_original_input_owner() {
        use crate::plugin::PendingParameterFlushRefusal as Failure;
        let input = ParameterChanges::default();
        let handler = ComponentHandler::new(Arc::new(Mutex::new(Vec::new())));
        handler
            .restart_cohort
            .store((u64::from(u32::MAX) << 32) | 4, Ordering::Release);
        let before = handler.editor_event_source_status();
        let raw = handler.pending_restart_cohort();
        let values = [crate::parameters::ParameterChange {
            id: 7,
            value: 0.5,
            sample_offset: 0,
        }];
        let mut candidate = PendingParameterProjection::prepare(&values, 4096).unwrap();
        assert_eq!(
            input.require_empty_pending_flush_input(Some(&handler)),
            Err(Failure::RestartRequired)
        );
        assert_eq!(
            input.pending_flush_input(Some(&handler), Some(&mut candidate)),
            Err(Failure::RestartRequired)
        );
        assert_eq!(handler.editor_event_source_status(), before);
        assert_eq!(handler.pending_restart_cohort(), raw);
        assert!(input.queues.lock().unwrap().is_empty());
        assert_eq!(input.used.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod restart_retirement_lineage_tests {
    use super::*;
    use crate::plugin::{
        NativeEditorAttachmentOwner as Owner, NativeEditorEvent as Event,
        RestartSourceError as Failure, RestartSourceSettlement as Settlement,
    };

    fn handler() -> ComponentHandler {
        ComponentHandler::new(Arc::new(Mutex::new(Vec::with_capacity(
            MAX_EDITOR_FEEDBACK,
        ))))
    }
    fn snapshot(
        handler: &ComponentHandler,
    ) -> (
        crate::plugin::NativeEditorEventSourceStatus,
        Vec<Event>,
        Vec<(u32, u64)>,
        crate::plugin::RestartCohort,
        u64,
    ) {
        let events = handler.editor_events.lock().unwrap();
        let feedback = handler.parameter_changes.lock().unwrap();
        let mut status = events.status();
        status.pending_dsp_feedback_values = feedback.len();
        (
            status,
            events.events.clone(),
            feedback
                .iter()
                .map(|&(id, value)| (id, value.to_bits()))
                .collect(),
            handler.pending_restart_cohort(),
            events.retirement_lineage,
        )
    }
    fn capture_and_drain(
        handler: &ComponentHandler,
        owner: Owner,
    ) -> crate::plugin::RestartSourceWitness {
        unsafe {
            assert_eq!(handler.restartComponent(4), kResultOk);
        }
        let witness = handler.capture_restart_source_witness(owner).unwrap();
        assert!(
            matches!(handler.take_editor_events(), crate::plugin::NativeEditorEventDrain::Complete { events } if events.len() == 1)
        );
        witness
    }

    #[test]
    fn raw_retirement_lineage_changes_once_only_for_successful_nonempty_clear() {
        let handler = handler();
        unsafe {
            assert_eq!(handler.restartComponent(8), kResultOk);
        }
        let stale = handler.pending_restart_cohort();
        unsafe {
            assert_eq!(handler.restartComponent(8), kResultOk);
        }
        assert!(!handler.retire_restart_cohort(stale));
        assert_eq!(snapshot(&handler).4, 0);
        let current = handler.pending_restart_cohort();
        assert!(handler.retire_restart_cohort(current));
        assert_eq!(snapshot(&handler).4, 1);
        assert!(handler.claim_retired_restart_serial(current.serial()));
        assert!(!handler.retire_restart_cohort(handler.pending_restart_cohort()));
        assert!(handler.take_restart_flags().is_empty());
        assert_eq!(snapshot(&handler).4, 1);
        let source = handler.editor_event_source_status();
        unsafe {
            assert_eq!(handler.restartComponent(4), kResultOk);
        }
        let published = handler.editor_event_source_status();
        assert_eq!(handler.take_restart_flags().bits(), 4);
        assert_eq!(handler.editor_event_source_status(), published);
        assert_eq!(snapshot(&handler).4, 2);
        let _ = handler.take_editor_events();
        assert_eq!(snapshot(&handler).4, 2);
        assert_eq!(
            handler.editor_event_source_status().revision(),
            source.revision() + 1
        );
    }

    #[test]
    fn parameter_info_prefix_settles_only_its_same_kind_suffix() {
        for later in [false, true] {
            let handler = handler();
            let owner = Owner::issue().unwrap();
            unsafe {
                assert_eq!(handler.restartComponent(16), kResultOk);
                assert_eq!(handler.setDirty(0), kResultOk);
            }
            let witness = handler.capture_restart_source_witness(owner).unwrap();
            assert_eq!(witness.cohort().flags().bits(), 16);
            assert_eq!(witness.source().pending_events(), 2);
            let _ = handler.take_editor_events();
            if later {
                unsafe {
                    assert_eq!(handler.restartComponent(16), kResultOk);
                    assert_eq!(handler.beginEdit(7), kResultOk);
                    assert_eq!(handler.performEdit(7, 0.625), kResultOk);
                }
            }
            let before = snapshot(&handler);
            let expected = if later {
                Ok(Settlement::NewerParameterSuffixPending { cohort: before.3 })
            } else {
                Ok(Settlement::Retired)
            };
            assert_eq!(
                handler.settle_restart_source_prefix(owner, witness),
                expected
            );
            let after = snapshot(&handler);
            if later {
                assert_eq!(
                    after, before,
                    "new Info prefix was not validated or retired"
                );
            } else {
                assert_eq!((after.0, after.1, after.2), (before.0, before.1, before.2));
                assert_eq!(after.3.serial(), before.3.serial());
                assert!(!after.3.pending());
                assert_eq!(after.4, before.4 + 1);
            }
        }
    }

    #[test]
    fn parameter_restart_prefix_refuses_both_cross_kind_unions() {
        for (prefix, suffix) in [(4, 16), (16, 4)] {
            let handler = handler();
            let owner = Owner::issue().unwrap();
            unsafe {
                assert_eq!(handler.restartComponent(prefix), kResultOk);
            }
            let witness = handler.capture_restart_source_witness(owner).unwrap();
            let _ = handler.take_editor_events();
            unsafe {
                assert_eq!(handler.restartComponent(suffix), kResultOk);
            }
            let before = snapshot(&handler);
            assert_eq!(before.3.flags().bits(), 20);
            assert_eq!(
                handler.capture_restart_source_witness(owner),
                Err(Failure::Conflict)
            );
            assert_eq!(
                handler.settle_restart_source_prefix(owner, witness),
                Err(Failure::Conflict)
            );
            assert_eq!(snapshot(&handler), before);
        }
    }

    #[test]
    fn parameter_info_prefix_requires_original_owner_drain_and_retirement_lineage() {
        for case in [
            "missing_drain",
            "other_owner",
            "external_retire",
            "unknown_suffix",
        ] {
            let handler = handler();
            let owner = Owner::issue().unwrap();
            unsafe {
                assert_eq!(handler.restartComponent(16), kResultOk);
            }
            let witness = handler.capture_restart_source_witness(owner).unwrap();
            if case != "missing_drain" {
                let _ = handler.take_editor_events();
            }
            if case == "external_retire" {
                assert_eq!(handler.take_restart_flags().bits(), 16);
                unsafe {
                    assert_eq!(handler.restartComponent(16), kResultOk);
                }
            }
            if case == "unknown_suffix" {
                unsafe {
                    assert_eq!(handler.restartComponent(1 << 29), kResultOk);
                }
            }
            let target = if case == "other_owner" {
                Owner::issue().unwrap()
            } else {
                owner
            };
            let before = snapshot(&handler);
            assert_eq!(
                handler.settle_restart_source_prefix(target, witness),
                Err(Failure::Conflict)
            );
            assert_eq!(snapshot(&handler), before, "case={case}");
        }
    }

    #[test]
    fn restart_prefix_exact_settlement_preserves_open_source_and_feedback_suffix() {
        let handler = handler();
        let owner = Owner::issue().unwrap();
        unsafe {
            assert_eq!(handler.restartComponent(4), kResultOk);
            assert_eq!(handler.setDirty(0), kResultOk);
        }
        let witness = handler.capture_restart_source_witness(owner).unwrap();
        assert_eq!(witness.source().revision(), 2);
        assert_eq!(witness.source().pending_events(), 2);
        let _ = handler.take_editor_events();
        unsafe {
            assert_eq!(handler.startGroupEdit(), kResultOk);
            assert_eq!(handler.beginEdit(7), kResultOk);
            assert_eq!(handler.performEdit(7, 0.375), kResultOk);
        }
        let before = snapshot(&handler);
        assert_eq!(
            handler.settle_restart_source_prefix(owner, witness),
            Ok(Settlement::Retired)
        );
        let after = snapshot(&handler);
        assert_eq!(after.0, before.0);
        assert_eq!(after.1, before.1);
        assert_eq!(after.2, before.2);
        assert_eq!(after.3.serial(), before.3.serial());
        assert!(after.3.flags().is_empty());
        assert_eq!(after.4, before.4 + 1);
    }

    #[test]
    fn restart_prefix_lineage_distinguishes_late_same_bit_from_external_retirement() {
        for external in [false, true] {
            let handler = handler();
            let owner = Owner::issue().unwrap();
            let witness = capture_and_drain(&handler, owner);
            if external {
                assert_eq!(handler.take_restart_flags().bits(), 4);
            }
            unsafe {
                assert_eq!(handler.restartComponent(4), kResultOk);
                assert_eq!(handler.beginEdit(7), kResultOk);
                assert_eq!(handler.performEdit(7, 0.625), kResultOk);
            }
            let before = snapshot(&handler);
            assert_eq!(before.3.serial(), witness.cohort().serial() + 1);
            assert_eq!(before.3.flags().bits(), 4);
            let expected = if external {
                Err(Failure::Conflict)
            } else {
                Ok(Settlement::NewerParameterSuffixPending { cohort: before.3 })
            };
            assert_eq!(
                handler.settle_restart_source_prefix(owner, witness),
                expected
            );
            assert_eq!(snapshot(&handler), before);
        }
    }

    #[test]
    fn restart_prefix_refuses_missing_drain_missing_suffix_mixed_and_other_owner() {
        for case in 0..4 {
            let handler = handler();
            let owner = Owner::issue().unwrap();
            unsafe {
                assert_eq!(handler.restartComponent(4), kResultOk);
            }
            let witness = handler.capture_restart_source_witness(owner).unwrap();
            let target = if case == 3 {
                Owner::issue().unwrap()
            } else {
                owner
            };
            if case != 0 {
                let _ = handler.take_editor_events();
            }
            match case {
                1 => {
                    unsafe {
                        assert_eq!(handler.restartComponent(4), kResultOk);
                    }
                    let _ = handler.take_editor_events();
                }
                2 => unsafe {
                    assert_eq!(handler.restartComponent(8), kResultOk);
                },
                _ => {}
            }
            let before = snapshot(&handler);
            assert_eq!(
                handler.settle_restart_source_prefix(target, witness),
                Err(Failure::Conflict)
            );
            assert_eq!(snapshot(&handler), before);
        }
    }

    #[test]
    fn raw_retirement_busy_poison_and_exhaustion_keep_flags_and_source() {
        let handler = handler();
        let owner = Owner::issue().unwrap();
        unsafe {
            assert_eq!(handler.restartComponent(4), kResultOk);
        }
        let cohort = handler.pending_restart_cohort();
        {
            let source = handler.editor_events.lock().unwrap();
            assert!(!handler.retire_restart_cohort(cohort));
            assert_eq!(handler.take_restart_flags().bits(), 4);
            assert_eq!(
                handler.capture_restart_source_witness(owner),
                Err(Failure::Busy)
            );
            assert_eq!(source.retirement_lineage, 0);
        }
        handler.editor_events.lock().unwrap().retirement_lineage = u64::MAX;
        let before = snapshot(&handler);
        assert!(!handler.retire_restart_cohort(cohort));
        assert_eq!(handler.take_restart_flags().bits(), 4);
        assert_eq!(
            handler.capture_restart_source_witness(owner),
            Err(Failure::Exhausted)
        );
        assert_eq!(snapshot(&handler), before);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _source = handler.editor_events.lock().unwrap();
            panic!("source poison control");
        }));
        assert!(!handler.retire_restart_cohort(cohort));
        assert_eq!(handler.take_restart_flags().bits(), 4);
        assert_eq!(
            handler.capture_restart_source_witness(owner),
            Err(Failure::PoisonedOwner)
        );
        assert_eq!(handler.pending_restart_cohort(), cohort);
    }

    #[test]
    fn nonparameter_restart_remains_atomic_while_source_owned_and_invalidates_old_proof() {
        let handler = handler();
        let owner = Owner::issue().unwrap();
        let witness = capture_and_drain(&handler, owner);
        {
            let source = handler.editor_events.lock().unwrap();
            unsafe {
                assert_eq!(handler.restartComponent(8), kResultOk);
            }
            assert!(source.events.is_empty());
            assert_eq!(source.retirement_lineage, witness.retirement_lineage());
        }
        let before = snapshot(&handler);
        assert_eq!(
            handler.settle_restart_source_prefix(owner, witness),
            Err(Failure::Conflict)
        );
        assert_eq!(snapshot(&handler), before);
    }
    #[test]
    fn restart_source_busy_feedback_and_wrong_thread_preserve_every_obligation() {
        let handler = handler();
        let owner = Owner::issue().unwrap();
        let witness = capture_and_drain(&handler, owner);
        let before = snapshot(&handler);
        {
            let _feedback = handler.parameter_changes.lock().unwrap();
            assert_eq!(
                handler.capture_restart_source_witness(owner),
                Err(Failure::Busy)
            );
            assert_eq!(
                handler.settle_restart_source_prefix(owner, witness),
                Err(Failure::Busy)
            );
        }
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    assert_eq!(
                        handler.capture_restart_source_witness(owner),
                        Err(Failure::WrongThread)
                    );
                    assert_eq!(
                        handler.settle_restart_source_prefix(owner, witness),
                        Err(Failure::WrongThread)
                    );
                })
                .join()
                .unwrap();
        });
        assert_eq!(snapshot(&handler), before);
    }
}
