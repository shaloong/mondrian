//! Scheduled clean-feed output for professional video I/O adapters.
//!
//! This crate owns the platform-neutral Reference Output Module: exact signal
//! admission, clean-feed payload contracts, rational embedded-audio cadence,
//! bounded scheduled playout, lifecycle evidence, and vendor-adapter seams.
//! DeckLink COM and AJA NTV2 C++ details remain behind concrete bridges.

mod adapter;
mod frame;
mod module;
mod signal;

pub use mondrian_broadcast::AncillaryFrame;

pub use adapter::{
    ReferenceOutputAdapter, ReferenceOutputAdapterError, ReferenceOutputAdapterEvent,
    ReferenceOutputAdapterSession, ReferenceOutputDeviceDescriptor, ReferenceOutputDeviceId,
    ReferenceOutputHardwareTime, ReferenceOutputProvider, ReferenceOutputProviderEvidence,
    ReferenceOutputRoutingPreferences, ReferenceOutputRuntimeAvailability,
    SimulatedReferenceOutputAdapter, UnavailableVendorReferenceOutputBridge,
    VendorReferenceOutputAdapter, VendorReferenceOutputBridge,
};
pub use frame::{
    pack_encoded_rgb_to_rgb12, pack_encoded_rgb_to_v210, pack_f32_audio_to_s24,
    ReferenceAudioFrame, ReferenceAudioPackingError, ReferenceOutputBundle,
    ReferenceOutputPayloadError, ReferenceVideoFrame, ReferenceVideoPackingError,
};
pub use module::{
    ReferenceOutputDiagnostics, ReferenceOutputError, ReferenceOutputModule, ReferenceOutputState,
};
pub use signal::{
    ReferenceAudioCadence, ReferenceAudioCadenceError, ReferenceHdrSignal,
    ReferenceOutputAncillaryPolicy, ReferenceOutputMode, ReferenceOutputModeError,
    ReferenceOutputOpenRequest, ReferenceOutputPixelFormat, ReferenceOutputRange,
    ReferenceOutputReferencePolicy, ReferenceOutputScan, ReferenceOutputSignal,
    ReferenceOutputSignalError,
};
