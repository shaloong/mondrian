//! Working-space conversion Module.

use crate::{
    color::gpu_session::{GpuColorBackendContext, GpuColorExecutionSession},
    product_gpu_working_texture_format, ColorFrameDomain, ColorFrameEncoding, CpuColorFrame,
    GpuColorFrameHandle, RenderColorStageDiagnostics, RenderColorTransformDiagnostics,
    RenderColorTransformGpuOptions, RenderCpuColorExecutionSession,
    RenderIntermediateColorTransform,
};
use mondrian_core::{types::ColorEngine, OcioColorSpaceIdentity, WorkingColorSpace};

/// Working-space conversion Interface.
pub struct WorkingColorModule;

/// CPU working-space conversion result with generic stage IR hidden.
pub struct CpuWorkingColorRecord {
    frame: CpuColorFrame,
    transform_diagnostics: RenderColorTransformDiagnostics,
    stage_diagnostics: RenderColorStageDiagnostics,
}

impl CpuWorkingColorRecord {
    /// Converted working-space frame.
    pub fn frame(&self) -> &CpuColorFrame {
        &self.frame
    }

    /// Color-transform diagnostics for the conversion.
    pub const fn transform_diagnostics(&self) -> RenderColorTransformDiagnostics {
        self.transform_diagnostics
    }

    /// Stage diagnostics for the conversion.
    pub const fn stage_diagnostics(&self) -> RenderColorStageDiagnostics {
        self.stage_diagnostics
    }

    /// Consume the record and retain the converted frame.
    pub fn into_frame(self) -> CpuColorFrame {
        self.frame
    }
}

/// GPU-resident result of a working-identity conversion.
pub struct GpuWorkingColorRecord {
    output: GpuColorFrameHandle,
    stage_diagnostics: RenderColorStageDiagnostics,
}

impl GpuWorkingColorRecord {
    /// Converted GPU working frame.
    pub fn output(&self) -> &GpuColorFrameHandle {
        &self.output
    }

    /// Diagnostics for the single working conversion pass.
    pub const fn stage_diagnostics(&self) -> RenderColorStageDiagnostics {
        self.stage_diagnostics
    }
}

impl WorkingColorModule {
    /// Convert one CPU linear working frame into another working identity.
    pub fn execute_cpu(
        frame: &CpuColorFrame,
        destination: WorkingColorSpace,
        engine: ColorEngine,
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<CpuWorkingColorRecord, crate::RenderColorTransformError> {
        let execution = crate::color_stage::execute_cpu_working_transform_with_session(
            frame,
            destination,
            engine,
            session,
        )?;
        Ok(CpuWorkingColorRecord {
            frame: execution.result.frame,
            transform_diagnostics: execution.result.diagnostics,
            stage_diagnostics: execution.stage_diagnostics,
        })
    }

    /// Record one GPU-resident working-identity conversion in the shared Session.
    pub fn record_gpu(
        session: &mut GpuColorExecutionSession,
        frame: &GpuColorFrameHandle,
        destination: WorkingColorSpace,
        engine: ColorEngine,
        options: RenderColorTransformGpuOptions,
        backend: GpuColorBackendContext<'_>,
    ) -> Result<GpuWorkingColorRecord, crate::RenderGpuColorTransformRuntimeRecordError> {
        let record = session.runtime_mut().record_wgpu_intermediate_color_transform_owned_backend(
            &RenderIntermediateColorTransform {
                output_identity: OcioColorSpaceIdentity::Working(destination),
                output_domain: ColorFrameDomain::Working,
                output_encoding: ColorFrameEncoding::LinearFloat,
                engine,
            },
            frame,
            product_gpu_working_texture_format(),
            "working-color-module-output",
            options,
            crate::color_stage::RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                device: backend.device,
                queue: backend.queue,
                encoder: backend.encoder,
                load_op: backend.load_op,
            },
        )?;
        Ok(GpuWorkingColorRecord {
            output: record.materialized.output,
            stage_diagnostics: record.stage_diagnostics,
        })
    }
}
