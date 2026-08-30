use std::collections::VecDeque;
use std::fmt;

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

/// Low-frequency physical output event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceOutputAdapterEvent {
    /// One scheduled bundle reached the device completion callback.
    FrameCompleted {
        /// Exact frame coordinate.
        frame_index: u64,
        /// Provider hardware time, absent for simulated output.
        hardware_time: Option<u64>,
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

/// Open provider Session after exact capability admission.
pub trait ReferenceOutputAdapterSession: Send {
    /// Immutable provider/runtime evidence for this Session.
    fn evidence(&self) -> &ReferenceOutputProviderEvidence;
    /// Exact open contract read back from the provider.
    fn request(&self) -> &ReferenceOutputOpenRequest;
    /// Provider device generation captured at open.
    fn device_generation(&self) -> u64;
    /// Schedule one atomic video/audio bundle.
    fn schedule(
        &mut self,
        bundle: ReferenceOutputBundle,
    ) -> Result<(), ReferenceOutputAdapterError>;
    /// Start scheduled playback after complete preroll.
    fn start(&mut self) -> Result<(), ReferenceOutputAdapterError>;
    /// Poll one bounded callback/status event.
    fn poll(&mut self) -> Result<Option<ReferenceOutputAdapterEvent>, ReferenceOutputAdapterError>;
    /// Stop playback and release the provider's device ownership.
    fn stop(&mut self) -> Result<(), ReferenceOutputAdapterError>;
}

/// Provider Adapter Interface. Discovery and open are never performed on a
/// realtime callback thread.
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
/// The bridge owns vendor handles, callback threading, device configuration
/// restoration, and SDK ABI details. Rust product code retains only typed
/// signal, lifecycle, and evidence semantics.
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
        Ok(self
            .scheduled
            .pop_front()
            .map(|bundle| ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: bundle.frame_index(),
                hardware_time: None,
            }))
    }

    fn stop(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        self.running = false;
        self.stopped = true;
        self.scheduled.clear();
        Ok(())
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
