//! # mondrian-core
//!
//! Domain foundations shared by authoring and execution crates: exact value
//! types, strongly typed identities, automation and processor contracts,
//! execution intent/evidence, and a bounded observation-only notification
//! seam. Canonical Project persistence and Timeline ownership live in their
//! dedicated crates rather than in a second Core interpretation.

pub mod audio_layout;
pub mod audio_mix;
pub mod audio_owner_installation;
pub mod audio_time;
pub mod authoring;
pub mod automation;
pub mod camera_raw;
pub mod color;
pub mod color_models;
pub mod color_science;
pub mod display_calibration;
pub mod display_contract;
pub mod display_labels;
pub mod display_probe;
pub mod display_timecode;
pub mod dynamic_hdr;
pub mod effect_data;
mod effect_pass_blend;
pub use effect_pass_blend::EffectPassBlendPolicy;
pub mod error;
pub mod events;
pub mod execution_cancellation;
pub mod execution_work;
pub mod gallery;
pub mod grade_graph;
pub mod hdr_metadata;
pub mod icc;
pub mod image_alpha;
pub use image_alpha::{ColorFrameAlpha, ColorFrameAlphaFormats};
pub mod image_sampling;
pub use image_sampling::{ImageSampling, ImageSamplingError};
pub mod mask_data;
pub mod media_contract;
pub mod musical_time;
pub mod native_audio;
pub mod native_audio_author;
pub mod native_audio_editor;
pub mod native_audio_parameter_observation;
pub mod native_child_settlement;
pub mod native_path;
mod numeric_bezier;
pub mod ocio;
pub mod parameter;
pub mod picture;
pub mod project;
mod resolved_visual_identity;
pub mod signal_compliance;
pub mod timeline_data;
pub mod timeline_time;
pub mod title;
pub mod types;

pub use audio_layout::*;
pub use audio_mix::*;
pub use audio_owner_installation::*;
pub use audio_time::*;
pub use authoring::{
    AuthoringAllocationId, AuthoringFootprint, AuthoringFootprintClaim,
    AuthoringFootprintClaimStatus, AuthoringFootprintCollector, AuthoringFootprintError,
    AuthoringFootprintManifest, AuthoringFootprintVersion, AuthoringList, AuthoringMap,
    AuthoringSet, AuthoringSnapshot, AUTHORING_FOOTPRINT_VERSION,
};
pub use automation::*;
pub use camera_raw::*;
pub use color::*;
pub use color_models::*;
pub use color_science::*;
pub use display_labels::*;
pub use display_timecode::*;
pub use dynamic_hdr::*;
pub use error::{MondrianError, Result};
pub use events::{AppEvent, EventBus};
pub use execution_cancellation::ExecutionCancellationToken;
pub use execution_work::*;
pub use gallery::*;
pub use grade_graph::*;
pub use hdr_metadata::*;
pub use media_contract::*;
pub use musical_time::{
    MusicalTimeReference, MusicalTimeReferenceError, MusicalTimeSignature, TimelineTransportActivity,
};
pub use native_audio::*;
pub use native_audio_author::*;
pub use native_audio_editor::*;
pub use native_audio_parameter_observation::*;
pub use native_child_settlement::*;
pub use native_path::*;
pub use ocio::*;
pub use parameter::*;
pub use picture::*;
pub use project::*;
pub use resolved_visual_identity::*;
pub use signal_compliance::*;
pub use timeline_time::*;
pub use title::*;
pub use types::*;

mod native_audio_diagnostics;
pub use native_audio_diagnostics::{
    NativeAudioDiagnosticBatch, NativeAudioDiagnosticExtentError, NativeAudioDiagnosticInsertion,
    NativeAudioDiagnosticLocation, NativeAudioDiagnosticLoss, NativeAudioDiagnosticMessageFacts,
    NativeAudioDiagnosticRecord, NativeAudioDiagnosticSeverity, NativeAudioDiagnosticSource,
    NATIVE_AUDIO_DIAGNOSTIC_MESSAGE_BYTES, NATIVE_AUDIO_DIAGNOSTIC_RECORDS,
};
