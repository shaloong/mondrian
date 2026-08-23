//! # mondrian-core
//!
//! Domain foundations shared by authoring and execution crates: exact value
//! types, strongly typed identities, automation and processor contracts,
//! execution intent/evidence, and a bounded observation-only notification
//! seam. Canonical Project persistence and Timeline ownership live in their
//! dedicated crates rather than in a second Core interpretation.

pub mod audio_layout;
pub mod audio_mix;
pub mod audio_time;
pub mod authoring;
pub mod automation;
pub mod color;
pub mod color_models;
pub mod color_science;
pub mod display_calibration;
pub mod display_contract;
pub mod display_labels;
pub mod display_probe;
pub mod display_timecode;
pub mod effect_data;
pub mod error;
pub mod events;
pub mod execution_cancellation;
pub mod execution_work;
pub mod hdr_metadata;
pub mod icc;
pub mod mask_data;
pub mod media_contract;
pub mod ocio;
pub mod parameter;
pub mod project;
pub mod timeline_data;
pub mod timeline_time;
pub mod title;
pub mod types;

pub use audio_layout::*;
pub use audio_mix::*;
pub use audio_time::*;
pub use authoring::{
    AuthoringAllocationId, AuthoringFootprint, AuthoringFootprintClaim,
    AuthoringFootprintClaimStatus, AuthoringFootprintCollector, AuthoringFootprintError,
    AuthoringFootprintManifest, AuthoringFootprintVersion, AuthoringList, AuthoringMap,
    AuthoringSet, AuthoringSnapshot, AUTHORING_FOOTPRINT_VERSION,
};
pub use automation::*;
pub use color::*;
pub use color_models::*;
pub use color_science::*;
pub use display_labels::*;
pub use display_timecode::*;
pub use error::{MondrianError, Result};
pub use events::{AppEvent, EventBus};
pub use execution_cancellation::ExecutionCancellationToken;
pub use execution_work::*;
pub use hdr_metadata::*;
pub use media_contract::*;
pub use ocio::*;
pub use parameter::*;
pub use project::*;
pub use timeline_time::*;
pub use title::*;
pub use types::*;
