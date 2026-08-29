//! Hardware-qualification Adapter for renderer-owned color internals.
//!
//! This is not a product execution Interface. It exists so ignored hardware
//! gates can measure exact cache and resource behavior without making the
//! generic stage IR part of Preview or Export integration.

pub use crate::color_stage::{
    RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeDiagnostics,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
};
