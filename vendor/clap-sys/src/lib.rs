//! One ABI generated from the complete official CLAP 1.2.10 include tree.
//!
//! `raw` retains the actual C declarations and carriers. Header modules only
//! organize reexports; they do not create alternate FFI structs or aliases.
//! Header-local version/color values are independently checked Rust constants.
//! ABI coverage does not imply that a host implements every optional service.
#![no_std]

/// Unedited bindgen output from the verified official headers.
#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
#[rustfmt::skip]
pub mod raw;
mod support;
pub use raw::*;
/// Official `audio_buffer` declarations.
pub mod audio_buffer;
/// Official `color` declarations.
pub mod color;
/// Official `entry` declarations.
pub mod entry;
/// Official `events` declarations.
pub mod events;
/// Official `ext` declarations.
pub mod ext;
/// Official `factory` declarations.
pub mod factory;
/// Official `fixedpoint` declarations.
pub mod fixedpoint;
/// Official `host` declarations.
pub mod host;
/// Official `id` declarations.
pub mod id;
/// Official `plugin` declarations.
pub mod plugin;
/// Official `plugin_features` declarations.
pub mod plugin_features;
/// Official `process` declarations.
pub mod process;
/// Official `stream` declarations.
pub mod stream;
/// Official `string_sizes` declarations.
pub mod string_sizes;
/// Official `timestamp` declarations.
pub mod timestamp;
/// Official `universal_plugin_id` declarations.
pub mod universal_plugin_id;
/// Official `version` declarations.
pub mod version;
