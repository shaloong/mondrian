//! Reference-only legacy eframe/egui editor modules.
//!
//! The product entrypoint is the self-hosted winit/wgpu shell. This module keeps
//! the old eframe app compiling while useful migration reference code is lifted
//! into neutral app/self-hosted modules or deleted.

use super::*;

#[allow(dead_code)]
pub(in crate::app) mod bootstrap;
#[allow(dead_code)]
pub(in crate::app) mod chrome;
#[allow(dead_code)]
pub(in crate::app) mod new_project;
#[allow(dead_code)]
pub(in crate::app) mod preferences;
pub(in crate::app) mod preferences_model;
