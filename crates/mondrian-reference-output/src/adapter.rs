use std::collections::VecDeque;
use std::fmt;
#[cfg(test)]
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{ReferenceOutputBundle, ReferenceOutputMode, ReferenceOutputOpenRequest};

/// Professional I/O provider family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReferenceOutputProvider {
    /// Blackmagic Design Desktop Video / DeckLink API.
    DeckLink,
    /// AJA NTV2 SDK.
    AjaNtv2,
    /// Deterministic software-only qualification provider.
    Simulated,
}

/// Machine-local device routing preference. Activating hardware remains an
/// explicit runtime action and is never implied by loading this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputRoutingPreferences {
    /// Selected provider family, absent until the user chooses one.
    pub provider: Option<ReferenceOutputProvider>,
    /// Stable provider-owned device identity.
    pub device_id: Option<ReferenceOutputDeviceId>,
    /// Preferred physical precision/carrier; exact mode admission still wins.
    pub pixel_format: crate::ReferenceOutputPixelFormat,
    /// External reference policy.
    pub reference_policy: crate::ReferenceOutputReferencePolicy,
}

impl Default for ReferenceOutputRoutingPreferences {
    fn default() -> Self {
        Self {
            provider: None,
            device_id: None,
            pixel_format: crate::ReferenceOutputPixelFormat::Yuv422TenV210,
            reference_policy: crate::ReferenceOutputReferencePolicy::FreeRunAllowed,
        }
    }
}

/// Stable provider/device identity, never a discovery-list index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReferenceOutputDeviceId(String);

impl ReferenceOutputDeviceId {
    /// Construct a bounded provider-owned identity.
    pub fn new(value: impl Into<String>) -> Result<Self, ReferenceOutputAdapterError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(ReferenceOutputAdapterError::InvalidDeviceId);
        }
        Ok(Self(value))
    }

    /// Borrow the stable opaque identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReferenceOutputDeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for ReferenceOutputDeviceId {
    type Error = ReferenceOutputAdapterError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ReferenceOutputDeviceId> for String {
    fn from(value: ReferenceOutputDeviceId) -> Self {
        value.0
    }
}

/// Runtime discovery result that cannot invent hardware support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceOutputRuntimeAvailability {
    /// Runtime loaded and device discovery completed.
    Available,
    /// Vendor driver/runtime is not installed.
    DriverMissing { detail: String },
    /// Runtime loaded but no devices were found.
    NoDevices,
    /// Bridge, SDK, driver, or firmware version contract is incompatible.
    VersionMismatch { detail: String },
}

/// Immutable provider/runtime evidence attached to discovery and Sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputProviderEvidence {
    /// Provider family.
    pub provider: ReferenceOutputProvider,
    /// Mondrian Adapter implementation version.
    pub adapter_version: String,
    /// Vendor SDK/API version read from the runtime, when present.
    pub sdk_version: Option<String>,
    /// Installed driver version, when present.
    pub driver_version: Option<String>,
    /// Whether this evidence came from physical hardware.
    pub hardware_backed: bool,
    /// Runtime availability at discovery time.
    pub availability: ReferenceOutputRuntimeAvailability,
}

/// One device and its exact mode inventory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputDeviceDescriptor {
    /// Stable provider-owned identity.
    pub id: ReferenceOutputDeviceId,
    /// Provider family.
    pub provider: ReferenceOutputProvider,
    /// User-facing device/profile name.
    pub display_name: String,
    /// Profile/device generation; changes invalidate stale Sessions.
    pub generation: u64,
    /// Exact modes admitted without implicit conversion.
    pub modes: Vec<ReferenceOutputMode>,
}

impl ReferenceOutputDeviceDescriptor {
    /// Find one mode that admits the exact request.
    pub fn admit(
        &self,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<&ReferenceOutputMode, ReferenceOutputAdapterError> {
        request.validate()?;
        self.modes
            .iter()
            .find(|mode| mode.admits(request).is_ok())
            .ok_or(ReferenceOutputAdapterError::ModeUnsupported)
    }
}

/// Provider hardware-clock timestamp with an explicit tick rate.
///
/// Raw ticks without a rate cannot prove callback cadence or drift and are not
/// eligible long-duration evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputHardwareTime {
    /// Monotonic provider hardware-clock tick.
    pub ticks: u64,
    /// Exact hardware-clock ticks per second.
    pub ticks_per_second: u64,
}

/// Low-frequency physical output event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceOutputAdapterEvent {
    /// One scheduled bundle reached the device completion callback.
    FrameCompleted {
        /// Exact frame coordinate.
        frame_index: u64,
        /// Provider hardware time, absent for simulated output.
        hardware_time: Option<ReferenceOutputHardwareTime>,
        /// Digest of the actual packet inventory read back by the provider.
        ancillary_readback_sha256: Option<[u8; 32]>,
    },
    /// Device reported a late frame.
    FrameLate { frame_index: u64 },
    /// Device dropped a scheduled frame.
    FrameDropped { frame_index: u64 },
    /// Device flushed a frame without presentation.
    FrameFlushed { frame_index: u64 },
    /// Continuous external-reference status changed.
    ReferenceLockChanged { locked: bool },
    /// Physical device disappeared.
    DeviceLost,
    /// Another application or profile change invalidated the mode inventory.
    ProfileChanged,
}

/// Stable provider failure captured while consuming a Reference Output Session.
///
/// Shutdown cannot return the Session owner to the caller, so provider failure
/// is retained as evidence instead of being returned as an error that could
/// discard the only resource-lifetime record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputProviderShutdownFailure {
    /// Provider operation that failed.
    pub operation: String,
    /// Stable provider-supplied failure detail.
    pub detail: String,
}

/// Lifecycle facts for the coordinator used by a deadline-bounded Session shutdown.
///
/// Direct, unbounded shutdown does not require a coordinator. Once
/// `required` is true, complete release requires positive proof that the
/// coordinator was spawned and joined without panic, timeout, or detachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputShutdownCoordinatorFacts {
    /// Whether the selected shutdown path required a coordinator.
    pub required: bool,
    /// Whether the coordinator thread was created successfully.
    pub spawned: bool,
    /// Whether the caller observed and joined coordinator completion.
    pub joined: bool,
    /// Whether coordinator execution panicked.
    pub panicked: bool,
    /// Whether the absolute deadline elapsed before completion was observed.
    pub timed_out: bool,
    /// Whether a still-running coordinator had to be detached at the deadline.
    pub detached: bool,
    /// Whether the provider owner had to be intentionally abandoned to keep
    /// its potentially blocking destructor off the deadline caller.
    pub owner_abandoned: bool,
}

impl ReferenceOutputShutdownCoordinatorFacts {
    /// Facts for direct shutdown or a Module that never opened a Session.
    pub const fn not_required() -> Self {
        Self {
            required: false,
            spawned: false,
            joined: false,
            panicked: false,
            timed_out: false,
            detached: false,
            owner_abandoned: false,
        }
    }

    /// Whether coordinator ownership is positively closed.
    pub const fn lifecycle_closed(&self) -> bool {
        !self.panicked
            && !self.timed_out
            && !self.detached
            && !self.owner_abandoned
            && (!self.required || self.spawned)
            && self.spawned == self.joined
    }
}

impl ReferenceOutputProviderShutdownFailure {
    /// Construct one provider shutdown failure record.
    pub fn new(operation: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { operation: operation.into(), detail: detail.into() }
    }
}

/// Consuming provider Session shutdown evidence.
///
/// A provider must populate every lifetime fact after consuming its Session.
/// A successful playback-stop request alone is deliberately insufficient:
/// callback execution and device ownership require separate positive proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputSessionShutdownReceipt {
    /// Receipt schema version.
    pub schema_version: u32,
    /// Whether a live Session owner existed at the shutdown seam.
    pub session_present: bool,
    /// Whether the provider accepted the non-blocking shutdown request.
    pub shutdown_request_completed: bool,
    /// Whether scheduled playback is proven stopped.
    pub playback_stopped: bool,
    /// Whether every provider callback execution context is proven terminated.
    pub callback_execution_terminated: bool,
    /// Whether provider device/profile ownership is proven released.
    pub device_released: bool,
    /// Provider frames still unresolved after shutdown.
    pub outstanding_frames: u64,
    /// Non-frame provider resources still unresolved after shutdown.
    pub outstanding_resources: u64,
    /// Provider failure observed during consuming shutdown.
    pub provider_failure: Option<ReferenceOutputProviderShutdownFailure>,
    /// Deadline-coordinator lifecycle facts, when bounded shutdown was used.
    pub coordinator: ReferenceOutputShutdownCoordinatorFacts,
}

impl ReferenceOutputSessionShutdownReceipt {
    /// Produce clean evidence for a Module that never owned a Session.
    pub const fn never_opened() -> Self {
        Self {
            schema_version: 2,
            session_present: false,
            shutdown_request_completed: true,
            playback_stopped: true,
            callback_execution_terminated: true,
            device_released: true,
            outstanding_frames: 0,
            outstanding_resources: 0,
            provider_failure: None,
            coordinator: ReferenceOutputShutdownCoordinatorFacts::not_required(),
        }
    }

    /// Whether the receipt positively proves complete Session resource release.
    pub const fn all_resources_released(&self) -> bool {
        self.schema_version == 2
            && (!self.session_present || self.shutdown_request_completed)
            && self.playback_stopped
            && self.callback_execution_terminated
            && self.device_released
            && self.outstanding_frames == 0
            && self.outstanding_resources == 0
            && self.provider_failure.is_none()
            && self.coordinator.lifecycle_closed()
    }
}

/// Provider Session owning every resource transferred by exact open admission.
///
/// Every device handle, callback thread, configuration-restoration guard, and
/// other blocking teardown obligation acquired for one open lifetime belongs
/// to this object and must be covered by its consuming shutdown receipt.
pub trait ReferenceOutputAdapterSession: Send {
    /// Immutable provider/runtime evidence for this Session.
    fn evidence(&self) -> &ReferenceOutputProviderEvidence;
    /// Exact open contract read back from the provider.
    fn request(&self) -> &ReferenceOutputOpenRequest;
    /// Provider device generation captured at open.
    fn device_generation(&self) -> u64;
    /// Schedule one atomic video/audio/ancillary bundle.
    fn schedule(
        &mut self,
        bundle: ReferenceOutputBundle,
    ) -> Result<(), ReferenceOutputAdapterError>;
    /// Start scheduled playback after complete preroll.
    fn start(&mut self) -> Result<(), ReferenceOutputAdapterError>;
    /// Poll one bounded callback/status event.
    fn poll(&mut self) -> Result<Option<ReferenceOutputAdapterEvent>, ReferenceOutputAdapterError>;
    /// Request shutdown without waiting for callback or device termination.
    ///
    /// Implementations may stop new scheduling, signal their provider callback
    /// loop, and request playback stop, but this method **must not wait** for a
    /// callback thread, device/profile release, or any other terminal owner.
    /// Those waits belong exclusively to consuming [`Self::shutdown`], which a
    /// caller may move onto a deadline coordinator.
    fn begin_shutdown(&mut self) -> Result<(), ReferenceOutputAdapterError>;
    /// Request playback stop and scheduled-queue flush.
    ///
    /// This mutable operation is not proof that callback execution terminated
    /// or device ownership was released. Only [`Self::shutdown`] can provide
    /// those consuming lifetime facts.
    fn stop(&mut self) -> Result<(), ReferenceOutputAdapterError>;
    /// Consume the Session and return provider-owned lifetime evidence.
    ///
    /// Implementations must explicitly terminate callbacks, release device
    /// ownership, and report unresolved resources or provider failure. There is
    /// intentionally no default implementation that upgrades [`Self::stop`]
    /// into release evidence.
    fn shutdown(self: Box<Self>) -> ReferenceOutputSessionShutdownReceipt;
}

/// Provider Adapter Interface. Discovery and open are never performed on a
/// realtime callback thread.
///
/// Implementations may retain immutable runtime evidence and discovery data,
/// but must not retain a provider resource whose release can block after
/// [`Self::open`] returns. All such ownership transfers into the returned
/// [`ReferenceOutputAdapterSession`], and Adapter Drop must be non-blocking.
/// Product teardown nevertheless moves the complete Module, including this
/// Adapter, onto its coordinator; a defective vendor bridge therefore cannot
/// turn UI-thread Drop into an unbounded wait.
pub trait ReferenceOutputAdapter: Send {
    /// Provider/runtime evidence from the most recent discovery.
    fn evidence(&self) -> &ReferenceOutputProviderEvidence;
    /// Enumerate stable devices and exact modes.
    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError>;
    /// Acquire, configure, read back, and return one unopened scheduled Session.
    fn open(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError>;
}

impl<T> ReferenceOutputAdapter for Box<T>
where
    T: ReferenceOutputAdapter + ?Sized,
{
    fn evidence(&self) -> &ReferenceOutputProviderEvidence {
        (**self).evidence()
    }

    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
        (**self).discover()
    }

    fn open(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
        (**self).open(device, request)
    }
}

/// Narrow bridge implemented by DeckLink COM or AJA NTV2 C++ integration.
///
/// The bridge encapsulates SDK ABI details and creates the Session that owns
/// vendor handles, callback threading, and device-configuration restoration.
/// After `open` returns, the bridge must retain no blocking provider lifetime
/// owner; its Drop is non-blocking. Whole-Module teardown still destroys the
/// bridge on a coordinator as a defensive ownership boundary. Rust product
/// code retains only typed signal, lifecycle, and evidence semantics.
pub trait VendorReferenceOutputBridge: Send {
    /// Immutable runtime evidence.
    fn evidence(&self) -> &ReferenceOutputProviderEvidence;
    /// Discover physical devices.
    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError>;
    /// Open an already-admitted physical Session.
    fn open(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError>;
}

/// Delayed-runtime bridge used when a physical provider was selected but its
/// licensed SDK/driver runtime is unavailable on this machine.
///
/// This bridge exposes stable diagnostic evidence and never fabricates a
/// device or Session. A composition root can therefore register DeckLink/AJA
/// product controls without making hardware claims before the vendor runtime
/// is actually installed and qualified.
pub struct UnavailableVendorReferenceOutputBridge {
    evidence: ReferenceOutputProviderEvidence,
}

impl UnavailableVendorReferenceOutputBridge {
    /// Report that a physical provider's driver/runtime could not be loaded.
    pub fn driver_missing(
        provider: ReferenceOutputProvider,
        detail: impl Into<String>,
    ) -> Result<Self, ReferenceOutputAdapterError> {
        Self::new(
            provider,
            ReferenceOutputRuntimeAvailability::DriverMissing { detail: detail.into() },
            None,
            None,
        )
    }

    /// Report that a physical provider runtime loaded but exposed no devices.
    pub fn no_devices(
        provider: ReferenceOutputProvider,
        sdk_version: Option<String>,
        driver_version: Option<String>,
    ) -> Result<Self, ReferenceOutputAdapterError> {
        Self::new(
            provider,
            ReferenceOutputRuntimeAvailability::NoDevices,
            sdk_version,
            driver_version,
        )
    }

    /// Report an incompatible bridge, SDK, driver, or firmware contract.
    pub fn version_mismatch(
        provider: ReferenceOutputProvider,
        detail: impl Into<String>,
        sdk_version: Option<String>,
        driver_version: Option<String>,
    ) -> Result<Self, ReferenceOutputAdapterError> {
        Self::new(
            provider,
            ReferenceOutputRuntimeAvailability::VersionMismatch { detail: detail.into() },
            sdk_version,
            driver_version,
        )
    }

    fn new(
        provider: ReferenceOutputProvider,
        availability: ReferenceOutputRuntimeAvailability,
        sdk_version: Option<String>,
        driver_version: Option<String>,
    ) -> Result<Self, ReferenceOutputAdapterError> {
        if provider == ReferenceOutputProvider::Simulated {
            return Err(ReferenceOutputAdapterError::ProviderMismatch);
        }
        Ok(Self {
            evidence: ReferenceOutputProviderEvidence {
                provider,
                adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
                sdk_version,
                driver_version,
                hardware_backed: false,
                availability,
            },
        })
    }
}

impl VendorReferenceOutputBridge for UnavailableVendorReferenceOutputBridge {
    fn evidence(&self) -> &ReferenceOutputProviderEvidence {
        &self.evidence
    }

    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
        ensure_runtime_available(&self.evidence)?;
        Err(ReferenceOutputAdapterError::NoDevices)
    }

    fn open(
        &mut self,
        _device: &ReferenceOutputDeviceDescriptor,
        _request: &ReferenceOutputOpenRequest,
    ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
        ensure_runtime_available(&self.evidence)?;
        Err(ReferenceOutputAdapterError::NoDevices)
    }
}

/// Concrete DeckLink/AJA Adapter over one delayed vendor bridge.
pub struct VendorReferenceOutputAdapter<B> {
    provider: ReferenceOutputProvider,
    bridge: B,
}

impl<B> VendorReferenceOutputAdapter<B>
where
    B: VendorReferenceOutputBridge,
{
    /// Bind a bridge to exactly one physical provider family.
    pub fn new(
        provider: ReferenceOutputProvider,
        bridge: B,
    ) -> Result<Self, ReferenceOutputAdapterError> {
        if provider == ReferenceOutputProvider::Simulated || bridge.evidence().provider != provider
        {
            return Err(ReferenceOutputAdapterError::ProviderMismatch);
        }
        Ok(Self { provider, bridge })
    }
}

impl<B> ReferenceOutputAdapter for VendorReferenceOutputAdapter<B>
where
    B: VendorReferenceOutputBridge,
{
    fn evidence(&self) -> &ReferenceOutputProviderEvidence {
        self.bridge.evidence()
    }

    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
        ensure_runtime_available(self.bridge.evidence())?;
        let devices = self.bridge.discover()?;
        if devices.iter().any(|device| device.provider != self.provider) {
            return Err(ReferenceOutputAdapterError::ProviderMismatch);
        }
        Ok(devices)
    }

    fn open(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
        ensure_runtime_available(self.bridge.evidence())?;
        if device.provider != self.provider {
            return Err(ReferenceOutputAdapterError::ProviderMismatch);
        }
        device.admit(request)?;
        let session = self.bridge.open(device, request)?;
        if session.request() != request
            || session.device_generation() != device.generation
            || session.evidence().provider != self.provider
            || !session.evidence().hardware_backed
        {
            return Err(ReferenceOutputAdapterError::ReadbackMismatch);
        }
        Ok(session)
    }
}

/// Deterministic, bounded software Adapter for contract qualification.
///
/// Its evidence explicitly remains non-hardware-backed and therefore can never
/// satisfy DeckLink/AJA or wire-level qualification.
pub struct SimulatedReferenceOutputAdapter {
    evidence: ReferenceOutputProviderEvidence,
    devices: Vec<ReferenceOutputDeviceDescriptor>,
    scripted_events: VecDeque<ReferenceOutputAdapterEvent>,
    #[cfg(test)]
    fail_stop: bool,
    #[cfg(test)]
    panic_begin_shutdown: bool,
    #[cfg(test)]
    shutdown_behavior: SimulatedShutdownBehavior,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
enum SimulatedShutdownBehavior {
    Normal,
    Panic,
    Delay(Duration),
    StaleSchema,
}

impl SimulatedReferenceOutputAdapter {
    /// Create a simulator exposing caller-supplied exact modes.
    pub fn new(modes: Vec<ReferenceOutputMode>) -> Result<Self, ReferenceOutputAdapterError> {
        let id = ReferenceOutputDeviceId::new("simulated:reference-output:v1")?;
        Ok(Self {
            evidence: ReferenceOutputProviderEvidence {
                provider: ReferenceOutputProvider::Simulated,
                adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
                sdk_version: None,
                driver_version: None,
                hardware_backed: false,
                availability: ReferenceOutputRuntimeAvailability::Available,
            },
            devices: vec![ReferenceOutputDeviceDescriptor {
                id,
                provider: ReferenceOutputProvider::Simulated,
                display_name: "Deterministic Reference Output".to_owned(),
                generation: 1,
                modes,
            }],
            scripted_events: VecDeque::new(),
            #[cfg(test)]
            fail_stop: false,
            #[cfg(test)]
            panic_begin_shutdown: false,
            #[cfg(test)]
            shutdown_behavior: SimulatedShutdownBehavior::Normal,
        })
    }

    /// Inject deterministic callback/failure events before normal completions.
    pub fn with_scripted_events(
        mut self,
        events: impl IntoIterator<Item = ReferenceOutputAdapterEvent>,
    ) -> Self {
        self.scripted_events.extend(events);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_stop_failure(mut self) -> Self {
        self.fail_stop = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_begin_shutdown_panic(mut self) -> Self {
        self.panic_begin_shutdown = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_shutdown_panic(mut self) -> Self {
        self.shutdown_behavior = SimulatedShutdownBehavior::Panic;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_shutdown_delay(mut self, delay: Duration) -> Self {
        self.shutdown_behavior = SimulatedShutdownBehavior::Delay(delay);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_stale_shutdown_schema(mut self) -> Self {
        self.shutdown_behavior = SimulatedShutdownBehavior::StaleSchema;
        self
    }
}

impl ReferenceOutputAdapter for SimulatedReferenceOutputAdapter {
    fn evidence(&self) -> &ReferenceOutputProviderEvidence {
        &self.evidence
    }

    fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
        Ok(self.devices.clone())
    }

    fn open(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: &ReferenceOutputOpenRequest,
    ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
        let actual = self
            .devices
            .iter()
            .find(|candidate| {
                candidate.id == device.id && candidate.generation == device.generation
            })
            .ok_or(ReferenceOutputAdapterError::DeviceUnavailable)?;
        actual.admit(request)?;
        Ok(Box::new(SimulatedSession {
            evidence: self.evidence.clone(),
            request: request.clone(),
            generation: actual.generation,
            scheduled: VecDeque::new(),
            scripted_events: std::mem::take(&mut self.scripted_events),
            running: false,
            stopped: false,
            #[cfg(test)]
            fail_stop: self.fail_stop,
            #[cfg(test)]
            panic_begin_shutdown: self.panic_begin_shutdown,
            #[cfg(test)]
            shutdown_behavior: self.shutdown_behavior,
        }))
    }
}

struct SimulatedSession {
    evidence: ReferenceOutputProviderEvidence,
    request: ReferenceOutputOpenRequest,
    generation: u64,
    scheduled: VecDeque<ReferenceOutputBundle>,
    scripted_events: VecDeque<ReferenceOutputAdapterEvent>,
    running: bool,
    stopped: bool,
    #[cfg(test)]
    fail_stop: bool,
    #[cfg(test)]
    panic_begin_shutdown: bool,
    #[cfg(test)]
    shutdown_behavior: SimulatedShutdownBehavior,
}

impl ReferenceOutputAdapterSession for SimulatedSession {
    fn evidence(&self) -> &ReferenceOutputProviderEvidence {
        &self.evidence
    }

    fn request(&self) -> &ReferenceOutputOpenRequest {
        &self.request
    }

    fn device_generation(&self) -> u64 {
        self.generation
    }

    fn schedule(
        &mut self,
        bundle: ReferenceOutputBundle,
    ) -> Result<(), ReferenceOutputAdapterError> {
        if self.stopped {
            return Err(ReferenceOutputAdapterError::SessionStopped);
        }
        if self.scheduled.len() >= self.request.max_scheduled_frames as usize {
            return Err(ReferenceOutputAdapterError::Backpressure);
        }
        bundle.validate()?;
        if bundle.video.signal() != &self.request.signal {
            return Err(ReferenceOutputAdapterError::PayloadSignalMismatch);
        }
        if !self.request.ancillary_policy.is_required() && !bundle.ancillary.packets().is_empty() {
            return Err(ReferenceOutputAdapterError::AncillaryNotEnabled);
        }
        self.scheduled.push_back(bundle);
        Ok(())
    }

    fn start(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        if self.stopped {
            return Err(ReferenceOutputAdapterError::SessionStopped);
        }
        if self.scheduled.len() < self.request.preroll_frames as usize {
            return Err(ReferenceOutputAdapterError::PrerollIncomplete {
                required: self.request.preroll_frames,
                scheduled: self.scheduled.len() as u32,
            });
        }
        self.running = true;
        Ok(())
    }

    fn poll(&mut self) -> Result<Option<ReferenceOutputAdapterEvent>, ReferenceOutputAdapterError> {
        if let Some(event) = self.scripted_events.pop_front() {
            return Ok(Some(event));
        }
        if !self.running || self.stopped {
            return Ok(None);
        }
        Ok(self.scheduled.pop_front().map(|bundle| {
            let ancillary_readback_sha256 = self
                .request
                .ancillary_policy
                .requires_readback()
                .then(|| bundle.ancillary.sha256());
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: bundle.frame_index(),
                hardware_time: None,
                ancillary_readback_sha256,
            }
        }))
    }

    fn stop(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        #[cfg(test)]
        if self.fail_stop {
            return Err(ReferenceOutputAdapterError::Vendor {
                operation: "stop",
                detail: "synthetic stop failure".to_owned(),
            });
        }
        self.running = false;
        self.stopped = true;
        self.scheduled.clear();
        Ok(())
    }

    fn begin_shutdown(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        #[cfg(test)]
        if self.panic_begin_shutdown {
            panic!("synthetic provider begin-shutdown panic");
        }
        // The simulator has no callback thread or physical device. Mutating
        // these in-memory flags therefore satisfies the non-blocking contract.
        self.stop()
    }

    fn shutdown(mut self: Box<Self>) -> ReferenceOutputSessionShutdownReceipt {
        #[cfg(test)]
        match self.shutdown_behavior {
            SimulatedShutdownBehavior::Normal | SimulatedShutdownBehavior::StaleSchema => {}
            SimulatedShutdownBehavior::Panic => {
                panic!("synthetic provider shutdown panic");
            }
            SimulatedShutdownBehavior::Delay(delay) => std::thread::sleep(delay),
        }
        let outstanding_frames = u64::try_from(self.scheduled.len()).unwrap_or(u64::MAX);
        let receipt = match self.stop() {
            Ok(()) => ReferenceOutputSessionShutdownReceipt {
                schema_version: 2,
                session_present: true,
                shutdown_request_completed: true,
                playback_stopped: true,
                callback_execution_terminated: true,
                device_released: true,
                outstanding_frames: 0,
                outstanding_resources: 0,
                provider_failure: None,
                coordinator: ReferenceOutputShutdownCoordinatorFacts::not_required(),
            },
            Err(error) => ReferenceOutputSessionShutdownReceipt {
                schema_version: 2,
                session_present: true,
                shutdown_request_completed: false,
                playback_stopped: false,
                callback_execution_terminated: false,
                device_released: false,
                outstanding_frames,
                // A failed consuming provider call leaves at least the Session
                // or device ownership unproven even when no frames were queued.
                outstanding_resources: 1,
                provider_failure: Some(ReferenceOutputProviderShutdownFailure::new(
                    "shutdown",
                    error.to_string(),
                )),
                coordinator: ReferenceOutputShutdownCoordinatorFacts::not_required(),
            },
        };
        #[cfg(test)]
        let receipt = if matches!(
            self.shutdown_behavior,
            SimulatedShutdownBehavior::StaleSchema
        ) {
            ReferenceOutputSessionShutdownReceipt { schema_version: 1, ..receipt }
        } else {
            receipt
        };
        receipt
    }
}

fn ensure_runtime_available(
    evidence: &ReferenceOutputProviderEvidence,
) -> Result<(), ReferenceOutputAdapterError> {
    match &evidence.availability {
        ReferenceOutputRuntimeAvailability::Available => Ok(()),
        ReferenceOutputRuntimeAvailability::DriverMissing { detail } => {
            Err(ReferenceOutputAdapterError::DriverMissing { detail: detail.clone() })
        }
        ReferenceOutputRuntimeAvailability::NoDevices => {
            Err(ReferenceOutputAdapterError::NoDevices)
        }
        ReferenceOutputRuntimeAvailability::VersionMismatch { detail } => {
            Err(ReferenceOutputAdapterError::VersionMismatch { detail: detail.clone() })
        }
    }
}

/// Stable provider/Session failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceOutputAdapterError {
    /// Stable device identity is malformed.
    #[error("reference output device identity is empty, too long, or contains control characters")]
    InvalidDeviceId,
    /// Adapter and device/provider evidence disagree.
    #[error("reference output provider identity mismatch")]
    ProviderMismatch,
    /// Vendor runtime is not installed.
    #[error("reference output vendor driver/runtime is missing: {detail}")]
    DriverMissing { detail: String },
    /// Runtime loaded but exposed no devices.
    #[error("reference output vendor runtime found no devices")]
    NoDevices,
    /// SDK/driver/bridge versions are incompatible.
    #[error("reference output vendor runtime version mismatch: {detail}")]
    VersionMismatch { detail: String },
    /// Stable device or profile generation disappeared.
    #[error("reference output device is unavailable")]
    DeviceUnavailable,
    /// No advertised mode exactly matches the request.
    #[error("reference output exact mode is unsupported")]
    ModeUnsupported,
    /// Provider configuration readback differs from the admitted request.
    #[error("reference output provider configuration readback mismatch")]
    ReadbackMismatch,
    /// Scheduled queue is full.
    #[error("reference output scheduled queue is backpressured")]
    Backpressure,
    /// Start was requested before complete preroll.
    #[error("reference output preroll has {scheduled} frames, requires {required}")]
    PrerollIncomplete { required: u32, scheduled: u32 },
    /// Session already stopped and released ownership.
    #[error("reference output session is stopped")]
    SessionStopped,
    /// Bundle was packed against another signal.
    #[error("reference output payload signal differs from the open Session")]
    PayloadSignalMismatch,
    /// Non-empty ANC was submitted to a Session opened without ANC.
    #[error("reference output ancillary packets were not enabled for this Session")]
    AncillaryNotEnabled,
    /// Exact mode request is invalid.
    #[error(transparent)]
    Mode(#[from] crate::ReferenceOutputModeError),
    /// Payload is invalid.
    #[error(transparent)]
    Payload(#[from] crate::ReferenceOutputPayloadError),
    /// Vendor bridge returned a stable operation failure.
    #[error("reference output vendor {operation} failed: {detail}")]
    Vendor {
        operation: &'static str,
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ReferenceOutputPixelFormat, ReferenceOutputRange, ReferenceOutputReferencePolicy,
        ReferenceOutputScan, ReferenceOutputSignal,
    };
    use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};

    #[test]
    fn coordinator_closure_requires_exact_spawn_join_and_no_faults() {
        for required in [false, true] {
            for spawned in [false, true] {
                for joined in [false, true] {
                    let facts = ReferenceOutputShutdownCoordinatorFacts {
                        required,
                        spawned,
                        joined,
                        ..ReferenceOutputShutdownCoordinatorFacts::not_required()
                    };
                    assert_eq!(
                        facts.lifecycle_closed(),
                        (!required || spawned) && spawned == joined
                    );
                    for faulty in [
                        ReferenceOutputShutdownCoordinatorFacts { panicked: true, ..facts },
                        ReferenceOutputShutdownCoordinatorFacts { timed_out: true, ..facts },
                        ReferenceOutputShutdownCoordinatorFacts { detached: true, ..facts },
                        ReferenceOutputShutdownCoordinatorFacts { owner_abandoned: true, ..facts },
                    ] {
                        assert!(!faulty.lifecycle_closed());
                    }
                }
            }
        }
    }

    fn request() -> ReferenceOutputOpenRequest {
        ReferenceOutputOpenRequest {
            signal: ReferenceOutputSignal {
                width: 1920,
                height: 1080,
                frame_rate: Rational::FPS_25,
                scan: ReferenceOutputScan::Progressive,
                pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
                color_space: ColorSpace::Rec709,
                range: ReferenceOutputRange::Legal,
                hdr: None,
                audio_layout: AudioChannelLayout::Stereo,
            },
            reference_policy: ReferenceOutputReferencePolicy::FreeRunAllowed,
            ancillary_policy: crate::ReferenceOutputAncillaryPolicy::Disabled,
            preroll_frames: 3,
            max_scheduled_frames: 5,
        }
    }

    #[test]
    fn simulated_evidence_can_never_claim_physical_hardware() {
        let request = request();
        let mode = ReferenceOutputMode {
            signal: request.signal,
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode]).expect("adapter");
        assert_eq!(
            adapter.evidence().provider,
            ReferenceOutputProvider::Simulated
        );
        assert!(!adapter.evidence().hardware_backed);
    }

    #[test]
    fn malformed_device_identity_fails_closed() {
        assert_eq!(
            ReferenceOutputDeviceId::new(""),
            Err(ReferenceOutputAdapterError::InvalidDeviceId)
        );
    }

    #[test]
    fn missing_decklink_runtime_never_invents_a_device() {
        let bridge = UnavailableVendorReferenceOutputBridge::driver_missing(
            ReferenceOutputProvider::DeckLink,
            "Desktop Video is not installed",
        )
        .expect("physical provider");
        let mut adapter =
            VendorReferenceOutputAdapter::new(ReferenceOutputProvider::DeckLink, bridge)
                .expect("matching provider");

        assert_eq!(
            adapter.discover(),
            Err(ReferenceOutputAdapterError::DriverMissing {
                detail: "Desktop Video is not installed".to_owned(),
            })
        );
        assert!(!adapter.evidence().hardware_backed);
    }

    #[test]
    fn loaded_aja_runtime_without_hardware_fails_closed() {
        let bridge = UnavailableVendorReferenceOutputBridge::no_devices(
            ReferenceOutputProvider::AjaNtv2,
            Some("17.5".to_owned()),
            Some("17.5.0".to_owned()),
        )
        .expect("physical provider");
        let mut adapter =
            VendorReferenceOutputAdapter::new(ReferenceOutputProvider::AjaNtv2, bridge)
                .expect("matching provider");

        assert_eq!(
            adapter.discover(),
            Err(ReferenceOutputAdapterError::NoDevices)
        );
        assert_eq!(adapter.evidence().sdk_version.as_deref(), Some("17.5"));
    }
}
