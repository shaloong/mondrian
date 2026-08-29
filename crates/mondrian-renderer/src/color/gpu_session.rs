//! Shared owner-scoped GPU color execution Session.

use std::sync::Arc;

use crate::{
    color_stage::{
        RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeDiagnostics,
        RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
        RenderGpuOutputBoundaryRuntimeRecordError, RenderGpuOutputStageRecord,
    },
    CpuColorFrame, GpuColorFrameHandle, GpuColorFrameIdAllocationError, GpuColorFrameReadback,
    GpuColorFrameReadbackPlan, GpuColorFrameResourceTableError, GpuColorFrameTextureFormat,
    GpuColorFrameWgpuResourcePool, GpuResidentEncoderInputLease, RenderColorTransformGpuOptions,
    RenderGpuOutputExecutionResourceGrant,
};

use super::program_output::ProgramOutputBoundary;

/// Backend Adapter borrowed for one GPU color submission.
pub struct GpuColorBackendContext<'a> {
    /// Device owning every resource in the Session.
    pub device: &'a wgpu::Device,
    /// Queue used for uploads and later submission.
    pub queue: &'a wgpu::Queue,
    /// Encoder receiving the color pass.
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// Load operation for the output target.
    pub load_op: wgpu::LoadOp<wgpu::Color>,
}

/// Working input accepted by a Program Output GPU boundary.
#[derive(Clone, Copy)]
pub enum GpuProgramInput<'a> {
    /// CPU working frame requiring one upload.
    Cpu(&'a CpuColorFrame),
    /// Already GPU-resident working frame.
    Gpu(&'a GpuColorFrameHandle),
}

/// Recorded Program Output with backend materialization details hidden.
pub struct GpuProgramOutputRecord {
    output: GpuColorFrameHandle,
    readback_buffer: Option<wgpu::Buffer>,
    stage_diagnostics: crate::color_stage::RenderColorStageDiagnostics,
}

impl GpuProgramOutputRecord {
    /// GPU-resident Program Output handle.
    pub fn output(&self) -> &GpuColorFrameHandle {
        &self.output
    }

    /// Take the optional readback buffer requested by the output residency policy.
    pub fn take_readback_buffer(&mut self) -> Option<wgpu::Buffer> {
        self.readback_buffer.take()
    }

    /// Aggregate stage diagnostics for this Program Output boundary.
    pub const fn stage_diagnostics(&self) -> crate::color_stage::RenderColorStageDiagnostics {
        self.stage_diagnostics
    }
}

/// Stable Program Output GPU failure categories.
#[derive(Debug, thiserror::Error)]
pub enum GpuProgramOutputError {
    /// The admitted active working set is insufficient.
    #[error("GPU Program Output active working set was rejected")]
    ActiveWorkingSet,
    /// Planning, preparation, or command recording failed.
    #[error("GPU Program Output recording failed: {reason}")]
    Record {
        /// Stable diagnostic string retained without exposing backend IR.
        reason: String,
    },
}

/// Failure to create the shared GPU color execution Session.
#[derive(Debug, thiserror::Error)]
pub enum GpuColorExecutionSessionError {
    /// Initial frame identity allocation failed.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
}

/// Failure to read one exact frame retained by a GPU color Session.
#[derive(Debug, thiserror::Error)]
pub enum GpuColorSessionReadbackError {
    /// The requested frame does not belong to this Session.
    #[error(transparent)]
    ResourceTable(#[from] GpuColorFrameResourceTableError),
    /// The retained resource did not satisfy the readback contract.
    #[error("GPU color readback contract failed: {reason}")]
    Copy {
        /// Stable diagnostic without exposing resource-table internals.
        reason: String,
    },
}

/// One owner-scoped GPU color Session shared by source, working, Program Output,
/// and monitor Modules.
pub struct GpuColorExecutionSession {
    runtime: RenderGpuOutputBoundaryRuntime,
}

impl GpuColorExecutionSession {
    /// Create a Session with its own bounded texture pool.
    pub fn new() -> Result<Self, GpuColorExecutionSessionError> {
        Ok(Self { runtime: RenderGpuOutputBoundaryRuntime::new()? })
    }

    /// Create a Session sharing a device-owned texture pool with adjacent GPU Modules.
    pub fn with_resource_pool(
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, GpuColorExecutionSessionError> {
        Ok(Self {
            runtime: RenderGpuOutputBoundaryRuntime::with_resource_pool(resource_pool)?,
        })
    }

    /// Return immutable cache, table, and pool diagnostics.
    pub fn diagnostics(&self) -> RenderGpuOutputBoundaryRuntimeDiagnostics {
        self.runtime.diagnostics()
    }

    /// Release frame-local resources while preserving reusable caches and the pool.
    pub fn clear_frame_resources(&mut self) {
        self.runtime.clear_frame_resources();
    }

    /// Number of frame resources currently retained for this owner.
    pub fn retained_frame_count(&self) -> usize {
        self.runtime.frame_table().len()
    }

    /// Record a readback copy for one exact retained frame without exposing the resource table.
    pub fn record_readback(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        plan: &GpuColorFrameReadbackPlan,
    ) -> Result<wgpu::Buffer, GpuColorSessionReadbackError> {
        let resource = self.runtime.frame_table().get(&plan.handle)?;
        GpuColorFrameReadback::record_copy(device, encoder, plan, resource)
            .map_err(|error| GpuColorSessionReadbackError::Copy { reason: format!("{error:?}") })
    }

    /// Record one CPU- or GPU-origin Program Output boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn record_program_output(
        &mut self,
        boundary: &ProgramOutputBoundary,
        input: GpuProgramInput<'_>,
        output_texture_format: GpuColorFrameTextureFormat,
        options: RenderColorTransformGpuOptions,
        active_grant: RenderGpuOutputExecutionResourceGrant,
        backend: GpuColorBackendContext<'_>,
    ) -> Result<GpuProgramOutputRecord, GpuProgramOutputError> {
        let backend = RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
            device: backend.device,
            queue: backend.queue,
            encoder: backend.encoder,
            load_op: backend.load_op,
        };
        let record = match input {
            GpuProgramInput::Cpu(frame) => {
                self.runtime.record_wgpu_output_boundary_owned_backend_with_grant(
                    boundary,
                    frame,
                    output_texture_format,
                    options,
                    active_grant,
                    backend,
                )
            }
            GpuProgramInput::Gpu(frame) => {
                self.runtime.record_wgpu_output_boundary_gpu_frame_owned_backend_with_grant(
                    boundary,
                    frame,
                    output_texture_format,
                    options,
                    active_grant,
                    backend,
                )
            }
        }
        .map_err(classify_program_output_error)?;
        Ok(program_output_record(record))
    }

    /// Transfer one exact resident Program Output to an encoder Adapter.
    pub fn take_resident_encoder_input(
        &mut self,
        handle: &GpuColorFrameHandle,
    ) -> Result<GpuResidentEncoderInputLease, GpuColorFrameResourceTableError> {
        self.runtime.take_resident_encoder_input(handle)
    }

    pub(crate) fn runtime_mut(&mut self) -> &mut RenderGpuOutputBoundaryRuntime {
        &mut self.runtime
    }
}

fn classify_program_output_error(
    error: RenderGpuOutputBoundaryRuntimeRecordError,
) -> GpuProgramOutputError {
    match error {
        RenderGpuOutputBoundaryRuntimeRecordError::ActiveWorkingSet(_) => {
            GpuProgramOutputError::ActiveWorkingSet
        }
        error => GpuProgramOutputError::Record { reason: format!("{error:?}") },
    }
}

fn program_output_record(record: RenderGpuOutputStageRecord) -> GpuProgramOutputRecord {
    GpuProgramOutputRecord {
        output: record.materialized.output,
        readback_buffer: record.readback_buffer,
        stage_diagnostics: record.stage_diagnostics,
    }
}
