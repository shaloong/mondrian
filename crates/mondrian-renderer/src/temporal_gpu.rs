//! Device-side execution of one frozen temporal value schedule.
//!
//! [`PreparedEffectTemporalGpuExecution`] states which host rasters a
//! time-expanded graph reads and which ordered passes combine them. This module
//! is the only place that turns that schedule into real work: it uploads one
//! working texture per leaf and records every step into the owner's command
//! encoder. The standalone executor reads the recorded root back once.
//!
//! Nothing here invents a pass for a shape the renderer cannot record. An
//! unsupported step fails the whole execution, so the caller keeps the scalar
//! CPU executor instead of publishing a silently different image.

use crate::gpu_visual_frame::GpuVisualFrameExecutionResourceGrant;
use crate::{
    CpuColorFrame, GpuColorFrameHandle, GpuColorFrameIdAllocator, GpuColorFrameReadback,
    GpuColorFrameReadbackPlan, GpuColorFrameResource, GpuColorFrameResourceTable,
    GpuColorFrameTextureFormat, GpuColorFrameUploadPlan, GpuColorFrameUploader,
    GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool, GpuCompositeError,
    GpuFrameCompositor,
};
use mondrian_core::{
    ColorFrameAlpha, ExecutionCancellationToken, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_effects::{
    EffectFrameExtent, EffectFrameTileF32, EffectPassBlendPolicy, EffectTemporalAuxiliaryProvider,
    EffectTemporalExecutionOutput, EffectTemporalFrameProvider, PreparedEffectTemporalGpuExecution,
    PreparedTemporalAuxiliarySet, PreparedTemporalFrameSet, TemporalGpuLeafRequest,
    TemporalGpuStepOperation, TemporalGpuValue,
};
use std::{
    collections::HashMap,
    fmt,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

/// How long one poll waits before the loop re-checks cancellation and deadline.
const TEMPORAL_GPU_POLL_SLICE: Duration = Duration::from_millis(10);

/// Caller-owned device state one temporal schedule is recorded with.
///
/// The renderer's continuation runtime builds this from its own resources, so a
/// schedule uses the same allocator, table and pool as every other device pass.
pub struct TemporalGpuDevice<'a> {
    /// Physical device.
    pub device: &'a wgpu::Device,
    /// Queue uploads are submitted on.
    pub queue: &'a wgpu::Queue,
    /// Compositor that records the individual passes.
    pub compositor: &'a GpuFrameCompositor,
    /// Frame id allocator shared with the owning runtime.
    pub ids: &'a mut GpuColorFrameIdAllocator,
    /// Resource table every frame in this schedule lives in.
    pub table: &'a mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
    /// Pool the working textures are allocated from.
    pub resource_pool: &'a Arc<GpuColorFrameWgpuResourcePool>,
}

/// One exact auxiliary leaf already owned by the visual frame's resource table.
pub struct TemporalGpuResidentAuxiliary {
    /// Consumer's exact Clip time.
    pub context_time: mondrian_core::TimelineTime,
    /// Compiled auxiliary slot in that context.
    pub slot: u32,
    /// Working Float32 texture handle from this same visual frame.
    pub frame: GpuColorFrameHandle,
    /// Actual source-grid sampling.
    pub sampling: mondrian_core::ImageSampling,
}

/// A frozen source-local temporal picture for the Viewer GPU compositor.
#[derive(Debug)]
pub struct ViewerTemporalPicture {
    schedule: PreparedEffectTemporalGpuExecution,
    frames: PreparedTemporalFrameSet,
    auxiliary: PreparedTemporalAuxiliarySet,
    history: HashMap<TemporalGpuValue, Arc<Self>>,
    demand: crate::ViewerGpuActiveTextureDemand,
}

/// A source-local historical picture addressed by the exact consumer context.
#[derive(Debug)]
pub struct ViewerTemporalHistory {
    /// Consumer Clip time.
    pub context_time: mondrian_core::TimelineTime,
    /// Consumer auxiliary slot.
    pub slot: u32,
    /// Prepared device schedule for this historical picture.
    pub picture: Arc<ViewerTemporalPicture>,
}

impl ViewerTemporalPicture {
    /// Freeze the raster providers and account for all device work in the closure.
    pub fn prepare(
        schedule: PreparedEffectTemporalGpuExecution,
        frames: PreparedTemporalFrameSet,
        auxiliary: Option<PreparedTemporalAuxiliarySet>,
        history: Vec<ViewerTemporalHistory>,
    ) -> Result<Self, TemporalGpuExecutionError> {
        if schedule.is_empty() {
            return Err(TemporalGpuExecutionError::EmptySchedule);
        }
        let auxiliary = match auxiliary {
            Some(auxiliary) => auxiliary,
            None => PreparedTemporalAuxiliarySet::prepare(
                frames.source_identity(),
                schedule.request().generation(),
                [],
            )
            .map_err(|error| TemporalGpuExecutionError::ResourceTable(error.to_string()))?,
        };
        let mut history_by_value = HashMap::new();
        let mut demand = crate::ViewerGpuActiveTextureDemand::default();
        for ViewerTemporalHistory { context_time, slot, picture } in history {
            let value = TemporalGpuValue::Auxiliary { context_time, slot };
            if !schedule.leaves().iter().any(|leaf| leaf.value() == value) {
                return Err(TemporalGpuExecutionError::MissingAuxiliaryRaster {
                    time: context_time,
                    slot,
                });
            }
            demand.bytes = demand.bytes.saturating_add(picture.demand.bytes);
            demand.textures = demand.textures.saturating_add(picture.demand.textures);
            if history_by_value.insert(value, picture).is_some() {
                return Err(TemporalGpuExecutionError::DuplicateLeaf(value));
            }
        }
        let spatial_chains = schedule
            .steps()
            .iter()
            .map(|step| temporal_spatial_chain(schedule.frame_extent(), step.operation()))
            .collect::<Result<Vec<_>, _>>()?;
        let own = estimate_temporal_demand(&schedule, &auxiliary, &spatial_chains, |value| {
            history_by_value.contains_key(&value)
        })?;
        demand.bytes = demand.bytes.saturating_add(own.bytes);
        demand.textures = demand.textures.saturating_add(own.textures);
        Ok(Self {
            schedule,
            frames,
            auxiliary,
            history: history_by_value,
            demand,
        })
    }

    pub(crate) const fn demand(&self) -> crate::ViewerGpuActiveTextureDemand {
        self.demand
    }

    pub(crate) fn record(
        &self,
        gpu: &mut TemporalGpuDevice<'_>,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        working_color_space: WorkingColorSpace,
    ) -> Result<(GpuColorFrameHandle, u32), TemporalGpuExecutionError> {
        let cancellation = self.schedule.request().cancellation();
        let deadline = cancellation
            .deadline()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
        if cancellation.is_canceled() {
            return Err(TemporalGpuExecutionError::Cancelled);
        }
        let mut schedules = 1u32;
        let mut resident = Vec::with_capacity(self.history.len());
        for leaf in self.schedule.leaves() {
            let TemporalGpuValue::Auxiliary { context_time, slot } = leaf.value() else {
                continue;
            };
            if let Some(picture) = self.history.get(&leaf.value()) {
                let (frame, count) = picture.record(gpu, encoder, working_color_space)?;
                schedules = schedules.saturating_add(count);
                resident.push(TemporalGpuResidentAuxiliary {
                    context_time,
                    slot,
                    frame,
                    sampling: picture.schedule.sampling(),
                });
            }
        }
        let recorded = record_temporal_schedule(
            gpu,
            encoder,
            &self.schedule,
            &mut self.frames.clone(),
            &mut self.auxiliary.clone(),
            working_color_space,
            deadline,
            cancellation,
            None,
            &resident,
        )?;
        let root = retain_recorded_temporal_root(gpu, recorded)?;
        Ok((root, schedules))
    }
}

/// Evidence of one completed device schedule.
pub struct TemporalGpuExecution {
    output: EffectTemporalExecutionOutput,
    uploads: usize,
    dispatches: usize,
    readbacks: usize,
}

impl TemporalGpuExecution {
    /// Scalar-executor-compatible output carrying the device tile.
    pub const fn output(&self) -> &EffectTemporalExecutionOutput {
        &self.output
    }

    /// Consume the evidence and keep only the scalar-compatible output.
    pub fn into_output(self) -> EffectTemporalExecutionOutput {
        self.output
    }

    /// Host rasters uploaded for the leaves.
    pub const fn uploads(&self) -> usize {
        self.uploads
    }

    /// Device passes recorded in schedule order.
    pub const fn dispatches(&self) -> usize {
        self.dispatches
    }

    /// Root readbacks. This is always one for a completed schedule.
    pub const fn readbacks(&self) -> usize {
        self.readbacks
    }
}

/// Why one temporal schedule could not be executed on the device.
///
/// Every variant is a refusal: the caller must fall back to the scalar CPU
/// executor. The renderer never publishes a partially executed schedule.
#[derive(Debug)]
pub enum TemporalGpuExecutionError {
    /// The schedule has no device pass, so it is not a device schedule.
    EmptySchedule,
    /// Two leaves address the same value.
    DuplicateLeaf(TemporalGpuValue),
    /// A leaf raster does not cover the schedule's frame extent.
    LeafExtentMismatch {
        /// Leaf value.
        value: TemporalGpuValue,
        /// Expected frame extent.
        expected: EffectFrameExtent,
        /// Extent the provider delivered.
        actual: EffectFrameExtent,
    },
    /// A leaf raster carried the wrong number of pixels.
    LeafCoverageMismatch {
        /// Leaf value.
        value: TemporalGpuValue,
        /// Pixels the extent requires.
        expected: usize,
        /// Pixels the provider delivered.
        actual: usize,
    },
    /// No auxiliary raster exists for an exact context and slot.
    MissingAuxiliaryRaster {
        /// Exact author time of the context.
        time: mondrian_core::TimelineTime,
        /// Declared external slot.
        slot: u32,
    },
    /// A sample picture has different geometry from its annotation grid.
    InvalidSamplePicture,
    /// A step reads a value no earlier step or leaf produced.
    MissingStepInput(TemporalGpuValue),
    /// A step has no frozen frame seed for its producing context.
    MissingFrameSeed(TemporalGpuValue),
    /// The spatial steps would exceed the shared scratch allocation budget.
    SpatialScratchBudgetExceeded {
        /// Peak pooled bytes plus all step-local scratch in this submission.
        required_bytes: u64,
        /// Shared spatial admission limit.
        budget_bytes: u64,
    },
    /// Existing visual resources plus the temporal schedule exceed the owner's grant.
    VisualWorkingSetExceeded {
        /// Conservative bytes retained during this recording.
        bytes: u64,
        /// Conservative texture count during this recording.
        textures: u64,
        /// Grant shared with the rest of the visual closure.
        grant: GpuVisualFrameExecutionResourceGrant,
    },
    /// The root value was never materialized.
    RootNotMaterialized(TemporalGpuValue),
    /// Host raster provider refused or could not deliver a leaf.
    FrameProvider(mondrian_effects::EffectTemporalFrameProviderError),
    /// One recorded pass failed.
    Composite(GpuCompositeError),
    /// Resource table bookkeeping failed.
    ResourceTable(String),
    /// Upload plan or upload failed.
    Upload(String),
    /// Readback plan, copy, mapping or unpacking failed.
    Readback(String),
    /// The readback did not cover the schedule's frame extent.
    ReadbackCoverage {
        /// Pixels the extent requires.
        expected: usize,
        /// Pixels the device delivered.
        actual: usize,
    },
    /// The owning runtime is poisoned and rejects every later frame.
    RuntimePoisoned,
    /// The caller cancelled the schedule.
    Cancelled,
    /// The caller's deadline elapsed.
    DeadlineExceeded,
}

impl fmt::Display for TemporalGpuExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySchedule => {
                write!(formatter, "temporal schedule has no device pass")
            }
            Self::DuplicateLeaf(value) => {
                write!(formatter, "temporal schedule repeats leaf {value:?}")
            }
            Self::LeafExtentMismatch { value, expected, actual } => write!(
                formatter,
                "temporal leaf {value:?} covers {}x{} but the schedule works in {}x{}",
                actual.width(),
                actual.height(),
                expected.width(),
                expected.height()
            ),
            Self::LeafCoverageMismatch { value, expected, actual } => write!(
                formatter,
                "temporal leaf {value:?} delivered {actual} pixels instead of {expected}"
            ),
            Self::MissingAuxiliaryRaster { time, slot } => {
                write!(formatter, "no auxiliary raster for slot {slot} at {time}")
            }
            Self::InvalidSamplePicture => formatter.write_str("grain sample picture does not match the frozen calibration grid"),
            Self::MissingStepInput(value) => {
                write!(formatter, "temporal step reads {value:?} before it exists")
            }
            Self::MissingFrameSeed(value) => {
                write!(
                    formatter,
                    "temporal step {value:?} has no frozen frame seed"
                )
            }
            Self::SpatialScratchBudgetExceeded { required_bytes, budget_bytes } => write!(
                formatter,
                "temporal spatial steps need {required_bytes} scratch bytes, budget is {budget_bytes}",
            ),
            Self::VisualWorkingSetExceeded { bytes, textures, grant } => write!(
                formatter,
                "temporal recording requires {bytes} bytes and {textures} textures, granted {} bytes and {} textures",
                grant.max_active_bytes(), grant.max_active_textures(),
            ),
            Self::RootNotMaterialized(value) => {
                write!(formatter, "temporal root {value:?} was never materialized")
            }
            Self::FrameProvider(error) => write!(formatter, "{error}"),
            Self::Composite(error) => write!(formatter, "{error}"),
            Self::ResourceTable(reason) => write!(formatter, "resource table: {reason}"),
            Self::Upload(reason) => write!(formatter, "temporal leaf upload: {reason}"),
            Self::Readback(reason) => write!(formatter, "temporal root readback: {reason}"),
            Self::ReadbackCoverage { expected, actual } => write!(
                formatter,
                "temporal readback delivered {actual} pixels instead of {expected}"
            ),
            Self::RuntimePoisoned => {
                write!(formatter, "the temporal execution runtime is poisoned")
            }
            Self::Cancelled => write!(formatter, "temporal device execution was cancelled"),
            Self::DeadlineExceeded => {
                write!(formatter, "temporal device execution passed its deadline")
            }
        }
    }
}

impl std::error::Error for TemporalGpuExecutionError {}

/// Keeps one read mapping alive until every mapped view is dropped.
struct ReadbackMapLease<'a> {
    buffer: &'a wgpu::Buffer,
}

impl<'a> ReadbackMapLease<'a> {
    fn new(buffer: &'a wgpu::Buffer) -> Self {
        Self { buffer }
    }
}

impl Drop for ReadbackMapLease<'_> {
    fn drop(&mut self) {
        self.buffer.unmap();
    }
}

struct RecordedTemporalGpuSchedule {
    root: GpuColorFrameHandle,
    frames: HashMap<TemporalGpuValue, GpuColorFrameHandle>,
    // The pooled matte intermediates must live through submission and readback.
    matte_scratch: Vec<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
    uploads: usize,
    dispatches: usize,
    peak_working_bytes: usize,
}

/// Execute one frozen schedule and read its root back exactly once.
///
/// The schedule's leaves are uploaded as working Float32 textures, every step is
/// recorded into one encoder in schedule order, and the root is the only value
/// read back. Intermediate values never leave the device.
pub fn execute_temporal_schedule(
    mut gpu: TemporalGpuDevice<'_>,
    schedule: &PreparedEffectTemporalGpuExecution,
    frames: &mut dyn EffectTemporalFrameProvider,
    auxiliary: &mut dyn EffectTemporalAuxiliaryProvider,
    working_color_space: WorkingColorSpace,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<TemporalGpuExecution, TemporalGpuExecutionError> {
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("temporal-gpu-schedule"),
    });
    let recorded = record_temporal_schedule(
        &mut gpu,
        &mut encoder,
        schedule,
        frames,
        auxiliary,
        working_color_space,
        deadline,
        cancellation,
        None,
        &[],
    )?;
    let result = read_temporal_root(
        &gpu,
        encoder,
        &recorded.root,
        schedule,
        deadline,
        cancellation,
    );
    for handle in recorded.frames.into_values() {
        let _ = gpu.table.take(&handle);
    }
    drop(recorded.matte_scratch);
    Ok(TemporalGpuExecution {
        output: EffectTemporalExecutionOutput::from_device_tile(
            result?,
            schedule.cache_identity(),
            recorded.uploads,
            recorded.peak_working_bytes,
        ),
        uploads: recorded.uploads,
        dispatches: recorded.dispatches,
        readbacks: 1,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_resident_temporal_root(
    mut gpu: TemporalGpuDevice<'_>,
    encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
    schedule: &PreparedEffectTemporalGpuExecution,
    frames: &mut dyn EffectTemporalFrameProvider,
    auxiliary: &mut dyn EffectTemporalAuxiliaryProvider,
    working_color_space: WorkingColorSpace,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
    grant: GpuVisualFrameExecutionResourceGrant,
    resident_auxiliary: &[TemporalGpuResidentAuxiliary],
) -> Result<(GpuColorFrameHandle, usize), TemporalGpuExecutionError> {
    let recorded = record_temporal_schedule(
        &mut gpu,
        encoder,
        schedule,
        frames,
        auxiliary,
        working_color_space,
        deadline,
        cancellation,
        Some(grant),
        resident_auxiliary,
    )?;
    let dispatches = recorded.dispatches;
    Ok((
        retain_recorded_temporal_root(&mut gpu, recorded)?,
        dispatches,
    ))
}

fn retain_recorded_temporal_root(
    gpu: &mut TemporalGpuDevice<'_>,
    recorded: RecordedTemporalGpuSchedule,
) -> Result<GpuColorFrameHandle, TemporalGpuExecutionError> {
    // Keep pooled textures with the visual owner until the closure retires.
    for resource in recorded.matte_scratch {
        gpu.table
            .insert(resource)
            .map_err(|error| TemporalGpuExecutionError::ResourceTable(format!("{error:?}")))?;
    }
    Ok(recorded.root.with_bilinear_composite_projection())
}

fn temporal_spatial_chain(
    extent: EffectFrameExtent,
    operation: &TemporalGpuStepOperation,
) -> Result<Option<crate::GpuSpatialChainPlan>, TemporalGpuExecutionError> {
    let TemporalGpuStepOperation::Spatial { plan, .. } = operation else {
        return Ok(None);
    };
    crate::spatial_driver::gpu_spatial_chain_plan(plan, extent.width(), extent.height())
        .map(Some)
        .map_err(|blocker| {
            TemporalGpuExecutionError::Composite(GpuCompositeError::SpatialChainFailed {
                pass: crate::gpu_compositor::spatial_chain_blocker_pass(&blocker),
                detail: blocker.to_string(),
            })
        })
}

#[allow(clippy::too_many_arguments)]
fn record_temporal_schedule(
    gpu: &mut TemporalGpuDevice<'_>,
    encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
    schedule: &PreparedEffectTemporalGpuExecution,
    frames: &mut dyn EffectTemporalFrameProvider,
    auxiliary: &mut dyn EffectTemporalAuxiliaryProvider,
    working_color_space: WorkingColorSpace,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
    visual_grant: Option<GpuVisualFrameExecutionResourceGrant>,
    resident_auxiliary: &[TemporalGpuResidentAuxiliary],
) -> Result<RecordedTemporalGpuSchedule, TemporalGpuExecutionError> {
    if schedule.is_empty() {
        return Err(TemporalGpuExecutionError::EmptySchedule);
    }
    let TemporalGpuDevice {
        device,
        queue,
        compositor,
        ids,
        table,
        resource_pool,
    } = gpu;
    let ids = &mut **ids;
    let table = &mut **table;
    let from_resident = |value| match value {
        TemporalGpuValue::Auxiliary { context_time, slot } => resident_auxiliary
            .iter()
            .find(|frame| frame.context_time == context_time && frame.slot == slot),
        _ => None,
    };
    let extent = schedule.frame_extent();
    let extent_pixels = extent.width().saturating_mul(extent.height()) as usize;
    if extent_pixels == 0 {
        return Err(TemporalGpuExecutionError::ReadbackCoverage { expected: 0, actual: 0 });
    }
    let mut pooled_spatial_scratch = 0u64;
    let mut per_stage_spatial_scratch = 0u64;
    let mut spatial_chains = Vec::with_capacity(schedule.steps().len());
    for step in schedule.steps() {
        if cancellation.is_canceled() {
            return Err(TemporalGpuExecutionError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(TemporalGpuExecutionError::DeadlineExceeded);
        }
        let chain = temporal_spatial_chain(extent, step.operation())?;
        if let Some(chain) = &chain {
            for layout in chain.particle_systems.values() {
                layout.check_limits(&device.limits()).map_err(|error| {
                    TemporalGpuExecutionError::Composite(GpuCompositeError::SpatialChainFailed {
                        pass: "particle_system",
                        detail: error.to_string(),
                    })
                })?;
            }
            for layout in chain.mask_path_strokes.values() {
                crate::mask_path_stroke_gpu::check_limits(layout, &device.limits()).map_err(
                    |error| {
                        TemporalGpuExecutionError::Composite(
                            GpuCompositeError::SpatialChainFailed {
                                pass: "mask_path_stroke",
                                detail: error.to_string(),
                            },
                        )
                    },
                )?;
            }
            let pooled_bytes =
                extent_pixels as u64 * 16 * u64::from(chain.scratch_texture_request());
            pooled_spatial_scratch = pooled_spatial_scratch.max(pooled_bytes);
            per_stage_spatial_scratch = per_stage_spatial_scratch
                .saturating_add(chain.scratch_bytes.saturating_sub(pooled_bytes));
        } else {
            let private_textures = match step.operation() {
                TemporalGpuStepOperation::Qualifier { qualifier, .. } => {
                    u64::from(qualifier.gpu_pass_count().saturating_sub(1))
                }
                TemporalGpuStepOperation::MatteFeather { .. } => 1,
                _ => 0,
            };
            per_stage_spatial_scratch = per_stage_spatial_scratch.saturating_add(
                (extent_pixels as u64).saturating_mul(16).saturating_mul(private_textures),
            );
        }
        let required_bytes = pooled_spatial_scratch.saturating_add(per_stage_spatial_scratch);
        let budget_bytes = crate::spatial_driver::GPU_SPATIAL_SCRATCH_BUDGET_BYTES;
        if required_bytes > budget_bytes {
            return Err(TemporalGpuExecutionError::SpatialScratchBudgetExceeded {
                required_bytes,
                budget_bytes,
            });
        }
        spatial_chains.push(chain);
    }
    if let Some(grant) = visual_grant {
        let additional = estimate_temporal_demand(schedule, auxiliary, &spatial_chains, |value| {
            from_resident(value).is_some()
        })?;
        let bytes = table
            .logical_texture_bytes()
            .unwrap_or(u64::MAX)
            .saturating_add(additional.bytes);
        let textures = (table.len() as u64).saturating_add(additional.textures);
        if bytes > grant.max_active_bytes() || textures > grant.max_active_textures() {
            return Err(TemporalGpuExecutionError::VisualWorkingSetExceeded {
                bytes,
                textures,
                grant,
            });
        }
    }
    let mut values: HashMap<TemporalGpuValue, GpuColorFrameHandle> = HashMap::new();
    let mut resident_bytes = 0usize;
    let mut uploads = 0usize;
    let mut dispatches = 0usize;

    let mut matte_scratch = Vec::new();
    let recorded = (|| -> Result<GpuColorFrameHandle, TemporalGpuExecutionError> {
        for leaf in schedule.leaves() {
            let value = leaf.value();
            if values.contains_key(&value) {
                return Err(TemporalGpuExecutionError::DuplicateLeaf(value));
            }
            if let Some(resident) = from_resident(value) {
                table.get(&resident.frame).map_err(|error| {
                    TemporalGpuExecutionError::ResourceTable(format!("{error:?}"))
                })?;
                let descriptor = resident.frame.descriptor();
                if descriptor.color_space.working() != Some(working_color_space)
                    || !descriptor.alpha.is_straight_compatible()
                    || resident.frame.texture_format() != GpuColorFrameTextureFormat::Rgba32Float
                {
                    return Err(TemporalGpuExecutionError::ResourceTable(format!(
                        "resident auxiliary {value:?} must be straight working Float32"
                    )));
                }
                if let Some((extent, sampling, _)) =
                    auxiliary.auxiliary_image(resident.context_time, resident.slot)
                    && (extent != EffectFrameExtent::new(descriptor.width, descriptor.height)
                        || sampling != resident.sampling)
                {
                    return Err(TemporalGpuExecutionError::ResourceTable(format!(
                        "resident auxiliary {value:?} differs from its frozen raster geometry"
                    )));
                }
                values.insert(value, resident.frame.clone());
                continue;
            }
            let (leaf_extent, sampling, alpha, pixels) = match leaf.request() {
                TemporalGpuLeafRequest::Source(request) => {
                    let mut pixels = Vec::new();
                    frames
                        .copy_frame(request, cancellation, &mut pixels)
                        .map_err(TemporalGpuExecutionError::FrameProvider)?;
                    (
                        request.frame_extent(),
                        request.sampling(),
                        ColorFrameAlpha::StraightCoverage,
                        pixels,
                    )
                }
                TemporalGpuLeafRequest::Auxiliary(demand) => {
                    let (leaf_extent, sampling, format) = auxiliary
                        .auxiliary_image(demand.context_time(), demand.slot())
                        .ok_or(TemporalGpuExecutionError::MissingAuxiliaryRaster {
                            time: demand.context_time(),
                            slot: demand.slot(),
                        })?;
                    let request =
                        demand.request(schedule.request().generation(), leaf_extent, sampling);
                    let mut pixels = Vec::new();
                    auxiliary
                        .copy_auxiliary(request, cancellation, &mut pixels)
                        .map_err(TemporalGpuExecutionError::FrameProvider)?;
                    (leaf_extent, sampling, format.alpha(), pixels)
                }
            };
            if matches!(leaf.request(), TemporalGpuLeafRequest::Source(_)) && leaf_extent != extent
            {
                return Err(TemporalGpuExecutionError::LeafExtentMismatch {
                    value,
                    expected: extent,
                    actual: leaf_extent,
                });
            }
            let leaf_pixels =
                (leaf_extent.width() as usize).saturating_mul(leaf_extent.height() as usize);
            if pixels.len() != leaf_pixels {
                return Err(TemporalGpuExecutionError::LeafCoverageMismatch {
                    value,
                    expected: leaf_pixels,
                    actual: pixels.len(),
                });
            }
            let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
                sampling,
                alpha,
                width: leaf_extent.width(),
                height: leaf_extent.height(),
                color_space: working_color_space,
                data: pixels,
            });
            let upload_plan = GpuColorFrameUploadPlan::from_cpu_color_frame(
                ids.allocate().map_err(|error| {
                    TemporalGpuExecutionError::ResourceTable(format!("{error:?}"))
                })?,
                &frame,
                GpuColorFrameTextureFormat::Rgba32Float,
                "temporal-gpu-leaf",
            )
            .map_err(|error| TemporalGpuExecutionError::Upload(format!("{error:?}")))?;
            let uploaded =
                GpuColorFrameUploader::upload_with_pool(device, queue, &upload_plan, resource_pool);
            let handle = uploaded.handle().clone();
            table
                .insert(uploaded)
                .map_err(|error| TemporalGpuExecutionError::ResourceTable(format!("{error:?}")))?;
            values.insert(value, handle);
            uploads = uploads.saturating_add(1);
            resident_bytes = resident_bytes.saturating_add(leaf_pixels.saturating_mul(16));
        }

        for (step, spatial_chain) in schedule.steps().iter().zip(&spatial_chains) {
            let value = step.value();
            let seed = schedule
                .frame_seed(value)
                .ok_or(TemporalGpuExecutionError::MissingFrameSeed(value))?;
            let input_handle = |input: TemporalGpuValue| {
                values
                    .get(&input)
                    .cloned()
                    .ok_or(TemporalGpuExecutionError::MissingStepInput(input))
            };
            let sample_geometry = |context_time, slot| {
                let value = TemporalGpuValue::Auxiliary { context_time, slot };
                if let Some(source) = from_resident(value) {
                    let descriptor = source.frame.descriptor();
                    return Some((
                        EffectFrameExtent::new(descriptor.width, descriptor.height),
                        source.sampling,
                    ));
                }
                auxiliary
                    .auxiliary_image(context_time, slot)
                    .map(|(extent, sampling, _)| (extent, sampling))
            };
            let output = match step.operation() {
                TemporalGpuStepOperation::Point { input, plan } => {
                    let input = input_handle(*input)?;
                    compositor
                        .record_point_effect_pass(
                            device,
                            queue,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input,
                            plan,
                            seed,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                        .output
                }
                TemporalGpuStepOperation::TemporalHold { input } => compositor
                    .record_temporal_hold(
                        device,
                        encoder,
                        ids,
                        table,
                        resource_pool,
                        &input_handle(*input)?,
                    )
                    .map_err(TemporalGpuExecutionError::Composite)?,
                TemporalGpuStepOperation::TemporalBlend { input, sample, mix } => {
                    let input = input_handle(*input)?;
                    let sample = input_handle(*sample)?;
                    compositor
                        .record_temporal_blend_pass(
                            device,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input,
                            &sample,
                            *mix,
                            working_color_space,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                }
                TemporalGpuStepOperation::TemporalGrain { input, neighbors, grade } => {
                    let input = input_handle(*input)?;
                    let [previous, following] =
                        neighbors.map(|sample| sample.map(input_handle).transpose());
                    compositor
                        .record_temporal_grain_pass(
                            device,
                            encoder,
                            ids,
                            table,
                            resource_pool.as_ref(),
                            &input,
                            [previous?.as_ref(), following?.as_ref()],
                            *grade,
                            working_color_space,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                }
                TemporalGpuStepOperation::Qualifier { input, qualifier } => {
                    let record = compositor
                        .record_qualifier_pass(
                            device,
                            queue,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input_handle(*input)?,
                            qualifier,
                            working_color_space,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?;
                    matte_scratch.extend(record.scratch);
                    record.output
                }
                TemporalGpuStepOperation::MatteFeather { input, kernel } => {
                    let record = compositor
                        .record_matte_feather_pass(
                            device,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input_handle(*input)?,
                            *kernel,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?;
                    matte_scratch.extend(record.scratch);
                    record.output
                }
                TemporalGpuStepOperation::MattePreview { input, invert } => {
                    compositor
                        .record_matte_preview_pass(
                            device,
                            queue,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input_handle(*input)?,
                            *invert,
                            working_color_space,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                        .output
                }
                TemporalGpuStepOperation::MaskCombine { left, right, mask_op } => {
                    compositor
                        .record_alpha_mask_combine_with_mode(
                            device,
                            queue,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input_handle(*left)?,
                            &input_handle(*right)?,
                            *mask_op,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                        .output
                }
                TemporalGpuStepOperation::MatteMix { base, graded, matte, amount } => {
                    compositor
                        .record_weighted_matte_mix_pass(
                            device,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input_handle(*base)?,
                            &input_handle(*graded)?,
                            &input_handle(*matte)?,
                            *amount,
                            working_color_space,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                        .output
                }
                TemporalGpuStepOperation::PreviewWindow { original, processed, window } => {
                    compositor
                        .record_preview_window_pass(
                            device,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input_handle(*original)?,
                            &input_handle(*processed)?,
                            *window,
                            schedule.sampling(),
                            working_color_space,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                        .output
                }
                TemporalGpuStepOperation::GrainSampleBoxes { input, boxes } => {
                    if let TemporalGpuValue::Auxiliary { context_time, slot } = *input
                        && !sample_geometry(context_time, slot).is_some_and(|(extent, sampling)| {
                            extent == boxes.extent() && sampling == schedule.sampling()
                        })
                    {
                        return Err(TemporalGpuExecutionError::InvalidSamplePicture);
                    }
                    compositor
                        .record_grain_sample_boxes_pass(
                            device,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input_handle(*input)?,
                            boxes,
                            working_color_space,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                        .output
                }
                TemporalGpuStepOperation::Blend { base, overlay, opacity, blend_mode } => {
                    let base = input_handle(*base)?;
                    let overlay = input_handle(*overlay)?;
                    compositor
                        .record_adjustment_blend_pass(
                            device,
                            queue,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &base,
                            &overlay,
                            *opacity,
                            EffectPassBlendPolicy::Blend(*blend_mode),
                            seed,
                            working_color_space,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                        .output
                }
                TemporalGpuStepOperation::Auxiliary { input, auxiliary: auxiliary_value, pass } => {
                    let input = input_handle(*input)?;
                    let auxiliary_frame = input_handle(*auxiliary_value)?;
                    compositor
                        .record_graph_auxiliary_pass(
                            device,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input,
                            &auxiliary_frame,
                            pass,
                            working_color_space,
                            schedule.sampling(),
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                }
                TemporalGpuStepOperation::VideoCollage { input, pass, auxiliary: bindings } => {
                    let mut pictures = Vec::with_capacity(bindings.len());
                    for &(tile_slot, value) in bindings.iter() {
                        let TemporalGpuValue::Auxiliary { context_time, slot } = value else {
                            return Err(TemporalGpuExecutionError::MissingStepInput(value));
                        };
                        let (_, sampling) = sample_geometry(context_time, slot).ok_or(
                            TemporalGpuExecutionError::MissingAuxiliaryRaster {
                                time: context_time,
                                slot,
                            },
                        )?;
                        pictures.push((tile_slot, input_handle(value)?, sampling));
                    }
                    let rasters = pictures
                        .iter()
                        .map(|(slot, frame, sampling)| {
                            crate::gpu_compositor::GpuSpatialAuxiliaryRaster {
                                slot: *slot,
                                frame,
                                sampling: *sampling,
                                transform: mondrian_effects::IDENTITY_AFFINE,
                                policy: mondrian_effects::AuxiliaryPolicy::new(
                                    mondrian_effects::AuxiliaryResolution::Authored,
                                ),
                            }
                        })
                        .collect::<Vec<_>>();
                    compositor
                        .record_temporal_video_collage(
                            device,
                            encoder,
                            ids,
                            table,
                            resource_pool.as_ref(),
                            &input_handle(*input)?,
                            pass,
                            &rasters,
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                }
                TemporalGpuStepOperation::Spatial { input, .. } => {
                    let input = input_handle(*input)?;
                    compositor
                        .record_prepared_spatial_effect_input(
                            device,
                            queue,
                            encoder,
                            ids,
                            table,
                            Some(resource_pool.as_ref()),
                            &input,
                            spatial_chain.as_ref().expect("admitted spatial step"),
                            seed,
                            &[],
                        )
                        .map_err(TemporalGpuExecutionError::Composite)?
                        .output
                }
            };
            values.insert(value, output);
            dispatches = dispatches.saturating_add(1);
            resident_bytes = resident_bytes.saturating_add(extent_pixels.saturating_mul(16));
        }

        let root = schedule.root();
        values
            .get(&root)
            .cloned()
            .ok_or(TemporalGpuExecutionError::RootNotMaterialized(root))
    })();
    let root = match recorded {
        Ok(root) => root,
        Err(error) => {
            for handle in values.into_values() {
                if !resident_auxiliary.iter().any(|source| source.frame == handle) {
                    let _ = table.take(&handle);
                }
            }
            return Err(error);
        }
    };
    let peak_working_bytes = resident_bytes.saturating_add(
        usize::try_from(pooled_spatial_scratch.saturating_add(per_stage_spatial_scratch))
            .unwrap_or(usize::MAX),
    );
    Ok(RecordedTemporalGpuSchedule {
        root,
        frames: values,
        matte_scratch,
        uploads,
        dispatches,
        peak_working_bytes,
    })
}

pub(crate) fn estimate_temporal_demand(
    schedule: &PreparedEffectTemporalGpuExecution,
    auxiliary: &dyn EffectTemporalAuxiliaryProvider,
    spatial_chains: &[Option<crate::GpuSpatialChainPlan>],
    is_resident: impl Fn(TemporalGpuValue) -> bool,
) -> Result<crate::GpuVisualFrameActiveTextureDemand, TemporalGpuExecutionError> {
    let extent = schedule.frame_extent();
    let mut bytes = 0u64;
    let mut textures = 0u64;
    let texture_bytes = |extent: EffectFrameExtent| {
        (u64::from(extent.width()) * u64::from(extent.height())).saturating_mul(16)
    };
    for leaf in schedule.leaves() {
        if is_resident(leaf.value()) {
            continue; // An earlier node's retained demand already includes this texture.
        }
        let extent = match leaf.request() {
            TemporalGpuLeafRequest::Source(request) => request.frame_extent(),
            TemporalGpuLeafRequest::Auxiliary(demand) => auxiliary
                .auxiliary_image(demand.context_time(), demand.slot())
                .map(|(extent, _, _)| extent)
                .ok_or(TemporalGpuExecutionError::MissingAuxiliaryRaster {
                    time: demand.context_time(),
                    slot: demand.slot(),
                })?,
        };
        bytes = bytes.saturating_add(texture_bytes(extent));
        textures = textures.saturating_add(1);
    }
    for (step, chain) in schedule.steps().iter().zip(spatial_chains) {
        let (output, extra_bytes, extra_textures) = if let Some(chain) = chain {
            (
                chain.output_bounds.extent(),
                chain.scratch_bytes,
                (chain.scratch_textures as u64)
                    .saturating_mul(chain.scratch_extents as u64)
                    .saturating_add(chain.stages.len().saturating_sub(1) as u64),
            )
        } else {
            let scratch = match step.operation() {
                TemporalGpuStepOperation::Qualifier { qualifier, .. } => {
                    u64::from(qualifier.gpu_pass_count().saturating_sub(1))
                }
                TemporalGpuStepOperation::MatteFeather { .. } => 1,
                _ => 0,
            };
            (
                extent,
                texture_bytes(extent).saturating_mul(scratch),
                scratch,
            )
        };
        bytes = bytes.saturating_add(texture_bytes(output)).saturating_add(extra_bytes);
        textures = textures.saturating_add(1).saturating_add(extra_textures);
    }
    Ok(crate::GpuVisualFrameActiveTextureDemand { bytes, textures })
}

fn read_temporal_root(
    gpu: &TemporalGpuDevice<'_>,
    mut encoder: wgpu::CommandEncoder,
    root_handle: &GpuColorFrameHandle,
    schedule: &PreparedEffectTemporalGpuExecution,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<EffectFrameTileF32, TemporalGpuExecutionError> {
    let TemporalGpuDevice { device, queue, table, .. } = gpu;
    let extent = schedule.frame_extent();
    let extent_pixels = extent.width().saturating_mul(extent.height()) as usize;
    let readback_plan = GpuColorFrameReadbackPlan::encoded_rgba32float(root_handle.clone())
        .map_err(|error| TemporalGpuExecutionError::Readback(format!("{error:?}")))?;
    let readback = {
        let root_resource = table
            .get(root_handle)
            .map_err(|error| TemporalGpuExecutionError::ResourceTable(format!("{error:?}")))?;
        GpuColorFrameReadback::record_copy(device, &mut encoder, &readback_plan, root_resource)
            .map_err(|error| TemporalGpuExecutionError::Readback(format!("{error:?}")))?
    };
    if cancellation.is_canceled() {
        return Err(TemporalGpuExecutionError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(TemporalGpuExecutionError::DeadlineExceeded);
    }
    let submission = queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    let map_lease = ReadbackMapLease::new(&readback);
    let map_result = loop {
        match receiver.try_recv() {
            Ok(result) => {
                break result
                    .map_err(|error| TemporalGpuExecutionError::Readback(format!("{error:?}")));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                break Err(TemporalGpuExecutionError::Readback(
                    "readback map callback was dropped".to_owned(),
                ));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if cancellation.is_canceled() {
            break Err(TemporalGpuExecutionError::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            break Err(TemporalGpuExecutionError::DeadlineExceeded);
        }
        let timeout = deadline.saturating_duration_since(now).min(TEMPORAL_GPU_POLL_SLICE);
        match device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission.clone()),
            timeout: Some(timeout),
        }) {
            Ok(_) | Err(wgpu::PollError::Timeout) => {}
            Err(error) => {
                break Err(TemporalGpuExecutionError::Readback(format!("{error:?}")));
            }
        }
    };
    if let Err(error) = map_result {
        drop(map_lease);
        return Err(error);
    }
    let mapped = slice
        .get_mapped_range()
        .map_err(|error| TemporalGpuExecutionError::Readback(format!("{error:?}")))?;
    let components = readback_plan
        .unpack_mapped_rgba32float(&mapped)
        .map_err(|error| TemporalGpuExecutionError::Readback(format!("{error:?}")))?;
    drop(mapped);
    drop(map_lease);

    let mut pixels = Vec::with_capacity(components.len() / 4);
    for chunk in components.chunks_exact(4) {
        pixels.push([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    if pixels.len() != extent_pixels {
        return Err(TemporalGpuExecutionError::ReadbackCoverage {
            expected: extent_pixels,
            actual: pixels.len(),
        });
    }
    EffectFrameTileF32::new(
        schedule.request().output_time(),
        extent,
        schedule.sampling(),
        extent.full_frame_roi(),
        schedule.request().output_frame_seed(),
        pixels,
    )
    .map_err(TemporalGpuExecutionError::FrameProvider)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GpuColorFrameWgpuResourcePoolOptions, HeterogeneousGpuContinuationRuntime};
    use mondrian_core::{
        AssetId, ColorFrameAlpha, ImageSampling, PropertyValue, TimelineTime, VisualSourceReference,
    };
    use mondrian_effects::{
        prepare_temporal_frame_execution, EffectColorDomain, EffectExecutionContinuity,
        EffectExecutionSession, EffectFrameExtent, EffectProcessorControl,
        EffectTemporalAuxiliaryRequest, EffectTemporalExecutionError,
        EffectTemporalExecutionRequest, EffectTemporalFrameProviderError,
        EffectTemporalFrameRequest, EffectTemporalSourceIdentity, EffectType, EffectValueFormat,
        EffectWorkingPrecision, PreparedEffectProgram,
    };
    use std::time::{Duration, Instant};

    const PARITY_TOLERANCE: f32 = 1.0e-4;

    fn echo_over_set_matte(spatial_prefix: bool) -> PreparedEffectProgram {
        let mut matte = mondrian_effects::instantiate_effect_node(EffectType::SetMatte)
            .expect("set matte definition");
        matte
            .set_static_value_by_parameter(
                &EffectType::SetMatte
                    .parameter_id("matte_source")
                    .expect("matte source parameter"),
                PropertyValue::VisualSource(VisualSourceReference::Asset {
                    asset_id: AssetId::new(),
                    retime: Default::default(),
                }),
            )
            .expect("matte source is admissible");
        let mut echo =
            mondrian_effects::instantiate_effect_node(EffectType::Echo).expect("echo definition");
        for (key, value) in [
            ("echo_time", PropertyValue::Float(-0.25)),
            ("operator", PropertyValue::Enum("normal".to_owned())),
            ("starting_intensity", PropertyValue::Float(0.5)),
        ] {
            echo.set_static_value_by_parameter(
                &EffectType::Echo.parameter_id(key).expect("echo parameter"),
                value,
            )
            .expect("echo value is admissible");
        }
        let mut effects = vec![matte];
        if spatial_prefix {
            let mut blur =
                mondrian_effects::instantiate_effect_node(EffectType::SmartBlur).unwrap();
            for (key, value) in [
                ("radius", PropertyValue::Float(3.0)),
                ("threshold", PropertyValue::Float(100.0)),
            ] {
                blur.set_static_value_by_parameter(
                    &EffectType::SmartBlur.parameter_id(key).unwrap(),
                    value,
                )
                .unwrap();
            }
            effects.push(blur);
        }
        effects.push(echo);
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709)
            .expect("echo prefix is a valid program")
    }

    fn current_source_pixel(x: usize, y: usize, width: usize, height: usize) -> [f32; 4] {
        let x = x as f32 / (width.max(2) - 1) as f32;
        let y = y as f32 / (height.max(2) - 1) as f32;
        [0.9 - 0.5 * y, 0.2 + 0.6 * x, 0.6 - 0.4 * x * y, 1.0]
    }

    fn historical_source_pixel(x: usize, y: usize, width: usize, height: usize) -> [f32; 4] {
        let x = x as f32 / (width.max(2) - 1) as f32;
        let y = y as f32 / (height.max(2) - 1) as f32;
        [0.15 + 0.7 * x, 0.85 - 0.6 * y, 0.05, 1.0]
    }

    /// Two frozen author instants with deliberately different pictures.
    struct GradientFrames {
        extent: EffectFrameExtent,
        historical: TimelineTime,
        copies: usize,
    }

    impl EffectTemporalFrameProvider for GradientFrames {
        fn source_identity(&self) -> EffectTemporalSourceIdentity {
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([0x31; 32])
        }

        fn retained_coverage_bytes(&self) -> usize {
            self.extent.width() as usize * self.extent.height() as usize * 16 * 2
        }

        fn copy_frame(
            &mut self,
            request: EffectTemporalFrameRequest,
            cancellation: &ExecutionCancellationToken,
            destination: &mut Vec<[f32; 4]>,
        ) -> Result<(), EffectTemporalFrameProviderError> {
            if cancellation.is_canceled() {
                return Err(EffectTemporalFrameProviderError::Canceled);
            }
            let extent = request.frame_extent();
            let (width, height) = (extent.width() as usize, extent.height() as usize);
            destination.clear();
            let past = request.time() == self.historical;
            for y in 0..height {
                for x in 0..width {
                    destination.push(if past {
                        historical_source_pixel(x, y, width, height)
                    } else {
                        current_source_pixel(x, y, width, height)
                    });
                }
            }
            self.copies = self.copies.saturating_add(1);
            Ok(())
        }
    }

    /// One matte slot whose coverage differs between the two instants.
    struct GradientMatte {
        extent: EffectFrameExtent,
        historical: TimelineTime,
        copies: usize,
    }

    impl EffectTemporalAuxiliaryProvider for GradientMatte {
        fn source_identity(&self) -> EffectTemporalSourceIdentity {
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([0x52; 32])
        }

        fn retained_coverage_bytes(&self) -> usize {
            self.extent.width() as usize * self.extent.height() as usize * 16 * 2
        }

        fn auxiliary_image(
            &self,
            _context_time: TimelineTime,
            slot: u32,
        ) -> Option<(EffectFrameExtent, ImageSampling, EffectValueFormat)> {
            (slot == 0).then_some((
                self.extent,
                ImageSampling::UNIT_SQUARE,
                EffectValueFormat::new(
                    EffectWorkingPrecision::Float32,
                    EffectColorDomain::SceneLinearRgb,
                    ColorFrameAlpha::StraightCoverage,
                ),
            ))
        }

        fn copy_auxiliary(
            &mut self,
            request: EffectTemporalAuxiliaryRequest,
            cancellation: &ExecutionCancellationToken,
            destination: &mut Vec<[f32; 4]>,
        ) -> Result<(), EffectTemporalFrameProviderError> {
            if cancellation.is_canceled() {
                return Err(EffectTemporalFrameProviderError::Canceled);
            }
            let extent = request.extent();
            let (width, height) = (extent.width() as usize, extent.height() as usize);
            destination.clear();
            let past = request.context_time() == self.historical;
            for y in 0..height {
                for x in 0..width {
                    let xn = x as f32 / (width.max(2) - 1) as f32;
                    let yn = y as f32 / (height.max(2) - 1) as f32;
                    let coverage = if past {
                        0.10 + 0.60 * yn
                    } else {
                        0.90 - 0.50 * xn
                    };
                    destination.push([1.0, 1.0, 1.0, coverage]);
                }
            }
            self.copies = self.copies.saturating_add(1);
            Ok(())
        }
    }

    fn frozen_schedule(
        extent: EffectFrameExtent,
        now: TimelineTime,
    ) -> mondrian_effects::PreparedEffectTemporalGpuExecution {
        let program = echo_over_set_matte(true);
        let request = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Discontinuous,
            now,
            extent,
            ImageSampling::UNIT_SQUARE,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        let execution = prepare_temporal_frame_execution(&program, &request, |_| Ok(1))
            .expect("finite temporal execution");
        execution
            .plan_gpu_execution()
            .expect("echo over set matte lowers to a device schedule")
    }

    fn max_channel_difference(left: &[[f32; 4]], right: &[[f32; 4]]) -> f32 {
        left.iter()
            .zip(right)
            .flat_map(|(left, right)| left.iter().zip(right).map(|(l, r)| (l - r).abs()))
            .fold(0.0_f32, f32::max)
    }

    #[test]
    fn device_grain_sample_boxes_draw_only_the_fixed_picture() {
        use mondrian_core::{
            MediaFileFingerprint, Rational, SampleAspectRatio, SourceSampleTarget,
        };
        use mondrian_effects::{
            grain_calibration::{
                calibrate_grain, GrainCalibrationObservation, GrainCalibrationRequest,
                GrainCalibrationSettings, GrainSamplePicture,
            },
            register_effect_definition, EffectColorDomainContract, EffectDefinition,
            EffectExecutionContract, EffectExecutionModes, EffectFloatFrame, EffectFloatFrameView,
            EffectGraphTopology, EffectNode, EffectRoiPropagation, PreparedTemporalAuxiliarySet,
            PreparedTemporalFrameSet,
        };

        let Ok(context) = pollster::block_on(crate::GpuContext::new()) else {
            eprintln!("skipping grain sample boxes: no GPU adapter available");
            return;
        };
        eprintln!(
            "Grain sample boxes adapter: {:?}",
            context.adapter.get_info()
        );
        let mut gpu = HeterogeneousGpuContinuationRuntime::new(
            context,
            GpuColorFrameWgpuResourcePoolOptions::default(),
        )
        .unwrap();
        let extent = EffectFrameExtent::new(53, 37);
        let sampling = ImageSampling::new(
            SampleAspectRatio::new(4, 3).unwrap(),
            [Rational::new(1, 1); 2],
        )
        .unwrap();
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        );
        let pixel_count = (extent.width() * extent.height()) as usize;
        let mut random = 19_u32;
        let pixels: Vec<_> = (0..pixel_count)
            .map(|_| {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (random >> 16) as f32 * (0.02 / 65_535.0);
                [0.4 + noise, -0.03 + noise, 1.2 + noise, 0.75]
            })
            .collect();
        let cancellation = ExecutionCancellationToken::new();
        let fixed = EffectFloatFrameView::new(extent, format, sampling, &pixels);
        let automatic = calibrate_grain(
            fixed,
            GrainCalibrationSettings::new(16, 4).unwrap(),
            &cancellation,
        )
        .unwrap()
        .expect("automatic fixed-source windows");
        let manual = calibrate_grain(
            fixed,
            GrainCalibrationSettings::manual(16, &[[20, 18], [21, 18]]).unwrap(),
            &cancellation,
        )
        .unwrap()
        .expect("manual fixed-source windows");
        assert_eq!(manual.windows.len(), 2);
        let color = [0.3, 0.8, 0.2, 0.5];
        let now = TimelineTime::new(12, 25).unwrap();
        let picture = GrainSamplePicture {
            target: SourceSampleTarget::covering(TimelineTime::new(4, 25).unwrap()),
            fingerprint: MediaFileFingerprint::capture(std::path::Path::new(file!())),
            video_stream_index: 1,
            extent,
            sampling,
        };
        for (index, profile) in [Some(Arc::new(automatic)), Some(Arc::new(manual)), None]
            .into_iter()
            .enumerate()
        {
            let settings = if index == 1 {
                GrainCalibrationSettings::manual(16, &[[20, 18], [21, 18]]).unwrap()
            } else {
                GrainCalibrationSettings::new(16, 4).unwrap()
            };
            let authored_request = GrainCalibrationRequest::new(4, settings).unwrap();
            let windows = profile.as_deref().map_or(&[][..], |profile| profile.windows.as_ref());
            let kind = EffectType::External(format!("test.fixed-grain-boxes.{index}"));
            register_effect_definition(
                EffectDefinition::new(
                    kind.key(),
                    "Fixed grain sample boxes",
                    Default::default(),
                    EffectColorDomainContract::SCENE_LINEAR,
                )
                .with_execution_contract(EffectExecutionContract {
                    execution_modes: EffectExecutionModes::CPU_F32
                        .union(EffectExecutionModes::GPU_F32),
                    roi_propagation: EffectRoiPropagation::FullFrame,
                    topology: EffectGraphTopology::GeneralDag,
                    ..EffectExecutionContract::IDENTITY
                })
                .with_branching_graph_builder(Arc::new(move |_, _, graph| {
                    let output = graph.add_fixed_grain_sample_display(authored_request, color)?;
                    graph.set_current_output(output);
                    Ok(())
                })),
            )
            .unwrap();
            let working_color_space = WorkingColorSpace::LinearRec709;
            let program =
                PreparedEffectProgram::prepare(&[EffectNode::new(kind)], &[], working_color_space)
                    .unwrap();
            let request = EffectTemporalExecutionRequest::new(
                15,
                EffectExecutionContinuity::Discontinuous,
                now,
                extent,
                sampling,
                extent.full_frame_roi(),
                cancellation.clone(),
            );
            let mut effects = EffectExecutionSession::default();
            let execution = effects
                .prepare_source_frame_program_with_binding(
                    &program,
                    &request,
                    |_| Ok(0),
                    |_, graph| {
                        assert!(graph
                            .bind_grain_calibration_from_media(|_| Ok(None), |_| Ok(true))
                            .unwrap_err()
                            .contains("requires admitted source frame 4"));
                        graph.bind_grain_calibration_from_media(
                            |request| {
                                assert_eq!(request, authored_request);
                                Ok(Some(GrainCalibrationObservation {
                                    picture,
                                    profile: profile.clone(),
                                }))
                            },
                            |_| Ok(true),
                        )
                    },
                )
                .unwrap()
                .bind_image_domain(&request)
                .unwrap();
            assert!(
                execution.demands().is_empty(),
                "the playing picture cannot enter the sample display"
            );
            let [demand] = execution.auxiliary_demands().demands() else {
                panic!("one fixed picture");
            };
            assert_eq!(
                execution.graph().grain_sample_picture(demand.slot()),
                Some((0, picture))
            );
            let identity =
                EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([17; 32]);
            let mut frames =
                PreparedTemporalFrameSet::prepare(identity, execution.demands().clone(), [])
                    .unwrap();
            let image =
                EffectFloatFrame::from_pixels(extent, format, sampling, pixels.clone()).unwrap();
            let mut auxiliary = PreparedTemporalAuxiliarySet::prepare(
                identity,
                15,
                [(demand.request(15, extent, sampling), image)],
            )
            .unwrap();
            let mut checkpoint = || None;
            let mut control = EffectProcessorControl::new(&mut checkpoint);
            let cpu = effects
                .execute_prepared_temporal_f32_with_auxiliary(
                    &execution,
                    &mut frames,
                    &mut auxiliary,
                    &mut control,
                )
                .unwrap();
            let schedule = execution.plan_gpu_execution().unwrap();
            let actual = gpu
                .execute_temporal_schedule(
                    &schedule,
                    &mut frames,
                    &mut auxiliary,
                    working_color_space,
                    Instant::now() + Duration::from_secs(120),
                    &cancellation,
                )
                .unwrap();
            let samples = actual.output().tile().pixels();
            assert_eq!(
                (actual.uploads(), actual.dispatches(), actual.readbacks()),
                (1, 1, 1)
            );
            let error = max_channel_difference(cpu.tile().pixels(), samples);
            eprintln!(
                "Grain sample boxes case {index}: {} windows, maximum Float32 error {error:.9e}",
                windows.len()
            );
            assert!(error <= PARITY_TOLERANCE);
            assert!(samples.iter().all(|pixel| pixel[3] == 0.75));
            if let Some(window) = windows.first() {
                let sample = (window.origin[1] * extent.width() + window.origin[0]) as usize;
                assert_eq!(samples[sample][0], (pixels[sample][0] + color[0]) * 0.5);
                assert_ne!(samples[sample], pixels[sample]);
            } else {
                assert_eq!(samples, pixels);
            }
        }
    }

    #[test]
    fn device_denoise_preview_window_uses_actual_media_sampling() {
        use mondrian_core::{Color, FramePosition, Rational, Resolution, SampleAspectRatio};
        use mondrian_effects::{
            EffectFrameTileF32, PreparedTemporalAuxiliarySet, PreparedTemporalFrameSet,
        };

        let Ok(context) = pollster::block_on(crate::GpuContext::new()) else {
            eprintln!("skipping denoise Preview window test: no GPU adapter available");
            return;
        };
        eprintln!(
            "Denoise Preview window adapter: {:?}",
            context.adapter.get_info()
        );
        let mut runtime = HeterogeneousGpuContinuationRuntime::new(
            context,
            GpuColorFrameWgpuResourcePoolOptions::default(),
        )
        .unwrap();
        let extent = EffectFrameExtent::new(57, 39);
        let canvas = EffectFrameExtent::new(114, 59);
        let sampling = ImageSampling::new(
            SampleAspectRatio::new(4, 3).unwrap(),
            [Rational::new(1, 2), Rational::new(2, 3)],
        )
        .unwrap();
        let now = TimelineTime::new(8, 25).unwrap();
        let identity = EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([0x53; 32]);

        for temporal in [false, true] {
            let mut effect =
                mondrian_effects::instantiate_effect_node(EffectType::Denoise).unwrap();
            for (key, value) in [
                ("view", PropertyValue::Enum("preview".into())),
                ("temporal_enabled", PropertyValue::Bool(temporal)),
                ("detail_amount", PropertyValue::Float(0.5)),
                ("preview_center", PropertyValue::Vec2([55.5, 29.0].into())),
                ("preview_width", PropertyValue::Float(51.0)),
                ("preview_height", PropertyValue::Float(52.0)),
                (
                    "preview_border_color",
                    PropertyValue::Color(Color { r: 1.2, g: 0.1, b: 0.3, a: 0.8 }),
                ),
            ] {
                effect
                    .set_static_value_by_parameter(
                        &EffectType::Denoise.parameter_id(key).unwrap(),
                        value,
                    )
                    .unwrap();
            }
            let mut sequence = mondrian_timeline::Sequence::new("Denoise preview region");
            sequence.settings.frame_rate = Rational::FPS_25;
            sequence.settings.resolution =
                Resolution { width: canvas.width(), height: canvas.height() };
            let mut clip =
                mondrian_timeline::Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE)
                    .unwrap();
            clip.add_effect_node(effect);
            sequence.video_tracks[0].add_clip(clip).unwrap();
            let program = crate::PreparedVisualProgram::prepare(&sequence).unwrap();
            let request = crate::TimelineFrameExecutionRequest::new(
                crate::TimelineEvaluationRequest::export(FramePosition::new(
                    8,
                    sequence.time_base(),
                )),
                15,
                EffectExecutionContinuity::Discontinuous,
                canvas,
                canvas.full_frame_roi(),
                ExecutionCancellationToken::new(),
            );
            let prepared = crate::TimelineCompositeScratch::default()
                .prepare_timeline_frame_execution_with_media_binding(
                    &program,
                    request,
                    |_, _, graph, _| {
                        graph.bind_grain_calibration(|_| Ok(Some([0.025; 3])), |_| Ok(true))
                    },
                )
                .unwrap();
            assert!(prepared.has_unbound_temporal_media());
            let prepared =
                prepared.bind_temporal_media_image_domains(|_| Ok((extent, sampling))).unwrap();
            let [batch] = prepared.batches() else {
                panic!("one authored Preview batch");
            };
            let execution = batch.execution();
            assert_eq!(execution.request().output_time(), now);
            let schedule = execution.plan_gpu_execution().unwrap();
            let expected_leaves = if temporal { 3 } else { 1 };
            assert_eq!(schedule.leaves().len(), expected_leaves);
            assert!(matches!(
                schedule.steps().last().unwrap().operation(),
                TemporalGpuStepOperation::PreviewWindow { .. }
            ));
            let mut frames = PreparedTemporalFrameSet::prepare(
                identity,
                execution.demands().clone(),
                execution.demands().requests().iter().copied().map(|sample| {
                    let pixels = (0..extent.width() * extent.height())
                        .map(|i| {
                            let shift = if sample.time() == now { 0 } else { 1 };
                            let grain = if (i + shift) % 2 == 0 { 0.09 } else { -0.09 };
                            [0.5 + grain, -0.1 + grain, 1.3 - grain, 0.6]
                        })
                        .collect();
                    (
                        sample,
                        EffectFrameTileF32::new(
                            sample.time(),
                            extent,
                            sampling,
                            extent.full_frame_roi(),
                            8,
                            pixels,
                        )
                        .unwrap(),
                    )
                }),
            )
            .unwrap();
            let cpu = EffectExecutionSession::default()
                .execute_prepared_temporal_f32(execution, &mut frames)
                .unwrap();
            let mut auxiliary = PreparedTemporalAuxiliarySet::prepare(identity, 15, []).unwrap();
            let gpu = runtime
                .execute_temporal_schedule(
                    &schedule,
                    &mut frames,
                    &mut auxiliary,
                    WorkingColorSpace::LinearRec709,
                    Instant::now() + Duration::from_secs(120),
                    execution.request().cancellation(),
                )
                .unwrap();
            let samples = gpu.output().tile().pixels();
            let error = max_channel_difference(cpu.tile().pixels(), samples);
            eprintln!("Denoise Preview window temporal={temporal}: {expected_leaves} inputs, maximum Float32 error {error:.9e}");
            assert!(error <= PARITY_TOLERANCE);
            assert_eq!(gpu.readbacks(), 1);
            assert_eq!(gpu.uploads(), expected_leaves);
            assert_eq!(samples[0], [0.5 + 0.09, -0.1 + 0.09, 1.3 - 0.09, 0.6]);
            assert!(samples.iter().all(|pixel| pixel[3] == 0.6));
        }
    }

    #[test]
    fn device_temporal_grain_matches_scalar_with_two_or_missing_neighbors() {
        verify_temporal_grain(false, false);
    }

    #[test]
    fn device_temporal_grain_feeds_calibrated_spatial_grain_in_the_same_graph() {
        verify_temporal_grain(true, false);
    }

    #[test]
    fn device_temporal_grain_runs_unsharp_mask_after_spatial_grain() {
        verify_temporal_grain(true, true);
    }

    fn verify_temporal_grain(spatial_after: bool, sharpen_after: bool) {
        use mondrian_effects::{
            grain_spatial::{GrainSpatialGrade, GrainSpatialShaping},
            grain_temporal::GrainTemporalGrade,
            register_effect_definition, EffectColorDomainContract, EffectDefinition,
            EffectExecutionContract, EffectExecutionModes, EffectFrameTileF32, EffectNode,
            EffectRenderOp, EffectRoiPropagation, PreparedTemporalAuxiliarySet,
            PreparedTemporalFrameSet,
        };

        let Ok(context) = pollster::block_on(crate::GpuContext::new()) else {
            eprintln!("skipping temporal grain test: no GPU adapter available");
            return;
        };
        eprintln!("Temporal grain adapter: {:?}", context.adapter.get_info());
        let mut runtime = HeterogeneousGpuContinuationRuntime::new(
            context,
            GpuColorFrameWgpuResourcePoolOptions::default(),
        )
        .unwrap();
        let extent = EffectFrameExtent::new(33, 19);
        let now = TimelineTime::new(2, 24).unwrap();
        let previous = TimelineTime::new(-1, 24).unwrap();
        let following = TimelineTime::new(1, 24).unwrap();
        let identity = EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([0x35; 32]);
        let mut auxiliary = PreparedTemporalAuxiliarySet::prepare(identity, 17, []).unwrap();
        let mut stationary_probe = [0.0; 4];
        let spatial = GrainSpatialGrade::new(
            [0.025; 3],
            [1.2, 2.0, 0.0],
            GrainSpatialShaping { size_bias: 0.4, ..Default::default() },
        )
        .unwrap();
        for (case, offsets) in [
            [Some(previous), Some(following)],
            [Some(previous), None],
            [None, Some(following)],
            [None, None],
        ]
        .into_iter()
        .enumerate()
        {
            let grade = GrainTemporalGrade::new(offsets, [0.025; 3], 0.85, 0.9).unwrap();
            let kind = EffectType::External(format!(
                "test.temporal-grain.{spatial_after}.{sharpen_after}.{case}"
            ));
            register_effect_definition(
                EffectDefinition::new(
                    kind.key(),
                    "Bound temporal grain",
                    Default::default(),
                    EffectColorDomainContract::SCENE_LINEAR,
                )
                .with_execution_contract(EffectExecutionContract {
                    execution_modes: EffectExecutionModes::CPU_F32
                        .union(EffectExecutionModes::GPU_F32),
                    temporal_input: grade.temporal_extent(),
                    roi_propagation: if spatial_after {
                        EffectRoiPropagation::Expand {
                            horizontal_pixels: spatial.halo(),
                            vertical_pixels: spatial.halo(),
                        }
                    } else {
                        EffectRoiPropagation::PixelLocal
                    },
                    ..EffectExecutionContract::IDENTITY
                })
                .with_graph_builder(Arc::new(move |_, _, graph| {
                    graph.append_unary(EffectRenderOp::TemporalGrain { grade });
                    if spatial_after {
                        graph.append_unary(EffectRenderOp::SpatialGrain { grade: spatial });
                    }
                    Ok(())
                })),
            )
            .unwrap();
            // Put a spatial stage before the grain operation, so sampled values
            // must run the upstream graph at each of the requested instants.
            let mut blur =
                mondrian_effects::instantiate_effect_node(EffectType::SmartBlur).unwrap();
            blur.set_static_value_by_parameter(
                &EffectType::SmartBlur.parameter_id("radius").unwrap(),
                PropertyValue::Float(1.0),
            )
            .unwrap();
            let mut effects = vec![blur, EffectNode::new(kind)];
            if sharpen_after {
                let mut unsharp =
                    mondrian_effects::instantiate_effect_node(EffectType::UnsharpMask).unwrap();
                unsharp
                    .set_static_value_by_parameter(
                        &EffectType::UnsharpMask.parameter_id("radius").unwrap(),
                        PropertyValue::Float(5.5),
                    )
                    .unwrap();
                effects.push(unsharp);
            }
            let program =
                PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709)
                    .unwrap();
            let request = EffectTemporalExecutionRequest::new(
                17,
                EffectExecutionContinuity::Discontinuous,
                now,
                extent,
                ImageSampling::UNIT_SQUARE,
                extent.full_frame_roi(),
                ExecutionCancellationToken::new(),
            );
            let execution =
                prepare_temporal_frame_execution(&program, &request, |_| Ok(1)).unwrap();
            let schedule = execution.plan_gpu_execution().unwrap();
            let expected_leaves = 1 + offsets.iter().flatten().count();
            assert_eq!(schedule.leaves().len(), expected_leaves);
            assert_eq!(
                schedule
                    .steps()
                    .iter()
                    .filter(|step| matches!(
                        step.operation(),
                        TemporalGpuStepOperation::Spatial { .. }
                    ))
                    .count(),
                expected_leaves + usize::from(spatial_after) + usize::from(sharpen_after)
            );
            if sharpen_after {
                assert!(
                    matches!(schedule.steps().last().unwrap().operation(), TemporalGpuStepOperation::Spatial { plan, .. } if matches!(plan.stages(), [mondrian_effects::EffectGpuStage::Spatial { pass: mondrian_effects::EffectGpuSpatialPass::UnsharpMask { .. }, .. }]))
                );
            } else if spatial_after {
                assert!(
                    matches!(schedule.steps().last().unwrap().operation(), TemporalGpuStepOperation::Spatial { plan, .. } if matches!(plan.stages(), [mondrian_effects::EffectGpuStage::Spatial { pass: mondrian_effects::EffectGpuSpatialPass::SpatialGrain { .. }, .. }]))
                );
            }
            let mut frames = PreparedTemporalFrameSet::prepare(
                identity,
                execution.demands().clone(),
                execution.demands().requests().iter().copied().map(|sample| {
                    let pixels = (0..33 * 19)
                        .map(|i| {
                            let x = i % 33;
                            let y = i / 33;
                            let edge = if x >= 17 { 1.6 } else { -0.3 };
                            let delta = if sample.time() == now {
                                0.0
                            } else if sample.time() < now {
                                if y == 2 {
                                    1.35
                                } else {
                                    -0.045
                                }
                            } else if y == 14 {
                                -0.95
                            } else {
                                0.07
                            };
                            let grain = ((i * 41 % 97) as f32 / 97.0 - 0.5) * 0.03;
                            [
                                edge + delta + grain,
                                0.82 + grain,
                                0.15 + grain,
                                if y == 0 { 0.0 } else { 0.6 },
                            ]
                        })
                        .collect();
                    (
                        sample,
                        EffectFrameTileF32::new(
                            sample.time(),
                            extent,
                            sample.sampling(),
                            extent.full_frame_roi(),
                            1,
                            pixels,
                        )
                        .unwrap(),
                    )
                }),
            )
            .unwrap();
            let cpu = EffectExecutionSession::default()
                .execute_prepared_temporal_f32(&execution, &mut frames)
                .unwrap();
            let gpu = runtime
                .execute_temporal_schedule(
                    &schedule,
                    &mut frames,
                    &mut auxiliary,
                    WorkingColorSpace::LinearRec709,
                    Instant::now() + Duration::from_secs(120),
                    request.cancellation(),
                )
                .unwrap();
            let error = max_channel_difference(cpu.tile().pixels(), gpu.output().tile().pixels());
            eprintln!("Temporal grain case {case}: {expected_leaves} leaves, Float32 maximum error {error:.9e}");
            assert!(error <= PARITY_TOLERANCE);
            assert_eq!(gpu.uploads(), expected_leaves);
            assert_eq!(gpu.readbacks(), 1);
            assert_eq!(
                gpu.dispatches(),
                expected_leaves + 1 + usize::from(spatial_after) + usize::from(sharpen_after)
            );
            stationary_probe[case] = gpu.output().tile().pixels()[8 * 33 + 5][0];
        }
        assert!(stationary_probe[1] < stationary_probe[3] - 0.008);
        assert!(stationary_probe[2] > stationary_probe[3] + 0.014);
        assert!(
            stationary_probe[0] > stationary_probe[1] && stationary_probe[0] < stationary_probe[2]
        );
    }

    #[test]
    fn device_temporal_schedule_matches_the_scalar_executor() {
        let Ok(context) = pollster::block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU temporal schedule parity test: no GPU adapter available");
            return;
        };
        let extent = EffectFrameExtent::new(9, 7);
        let now = TimelineTime::new(1, 2).expect("current time");
        let historical = TimelineTime::new(1, 4).expect("historical time");
        let schedule = frozen_schedule(extent, now);
        assert!(
            schedule
                .leaves()
                .iter()
                .any(|leaf| matches!(leaf.request(), TemporalGpuLeafRequest::Auxiliary(_))),
            "the schedule must read a second-input raster"
        );
        assert!(
            schedule.steps().iter().any(|step| matches!(
                step.operation(),
                TemporalGpuStepOperation::TemporalBlend { .. }
            )),
            "the schedule must combine two instants"
        );
        assert_eq!(
            schedule
                .steps()
                .iter()
                .filter(|step| matches!(
                    step.operation(),
                    TemporalGpuStepOperation::Spatial { plan, .. }
                        if matches!(plan.stages(), [mondrian_effects::EffectGpuStage::Spatial {
                            pass: mondrian_effects::EffectGpuSpatialPass::SmartBlur { .. }, ..
                        }])
                ))
                .count(),
            2,
            "both instants require the same multipass spatial prefix",
        );

        let mut session = EffectExecutionSession::default();
        let mut cpu_frames = GradientFrames { extent, historical, copies: 0 };
        let mut cpu_matte = GradientMatte { extent, historical, copies: 0 };
        let cpu = session
            .execute_prepared_temporal_f32_with_auxiliary(
                &schedule_execution(&schedule),
                &mut cpu_frames,
                &mut cpu_matte,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("scalar temporal execution");

        let mut runtime = HeterogeneousGpuContinuationRuntime::new(
            context,
            GpuColorFrameWgpuResourcePoolOptions::default(),
        )
        .expect("device runtime");
        let mut gpu_frames = GradientFrames { extent, historical, copies: 0 };
        let mut gpu_matte = GradientMatte { extent, historical, copies: 0 };
        let device = runtime
            .execute_temporal_schedule(
                &schedule,
                &mut gpu_frames,
                &mut gpu_matte,
                WorkingColorSpace::LinearRec709,
                Instant::now() + Duration::from_secs(120),
                &ExecutionCancellationToken::new(),
            )
            .expect("device temporal execution");

        assert_eq!(device.readbacks(), 1, "the root must cross the seam once");
        assert_eq!(device.dispatches(), schedule.steps().len());
        assert_eq!(device.uploads(), schedule.leaves().len());
        assert_eq!(gpu_matte.copies, 2, "both instants read the second input");
        assert_eq!(gpu_frames.copies, 2, "both instants read the source");

        let cpu_pixels = cpu.tile().pixels();
        let gpu_pixels = device.output().tile().pixels();
        assert_eq!(cpu_pixels.len(), gpu_pixels.len());
        let difference = max_channel_difference(cpu_pixels, gpu_pixels);
        eprintln!("Temporal spatial prefix CPU/GPU maximum Float32 error: {difference:.9e}");
        assert!(
            difference <= PARITY_TOLERANCE,
            "device schedule diverged from the scalar executor by {difference}"
        );
        assert!(
            cpu_pixels.iter().any(|pixel| pixel[3] > 0.02 && pixel[3] < 0.98),
            "the fixture must produce a visible second-input mix"
        );
        let unfiltered = prepare_temporal_frame_execution(
            &echo_over_set_matte(false),
            schedule.request(),
            |_| Ok(1),
        )
        .unwrap();
        let unfiltered = session
            .execute_prepared_temporal_f32_with_auxiliary(
                &unfiltered,
                &mut cpu_frames,
                &mut cpu_matte,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        assert!(
            max_channel_difference(unfiltered.tile().pixels(), cpu_pixels) > 0.01,
            "the spatial prefix must change published pixels",
        );
        assert!(
            device.output().peak_working_bytes()
                > (schedule.steps().len() + schedule.leaves().len()) * 9 * 7 * 16,
            "the reported GPU working set must include spatial scratch",
        );
        assert_ne!(
            cpu_pixels.iter().map(|pixel| pixel[0].to_bits()).collect::<Vec<_>>(),
            cpu_pixels.iter().map(|pixel| pixel[1].to_bits()).collect::<Vec<_>>(),
            "the fixture must not be a flat frame"
        );
    }

    /// Rebuild the frozen execution a schedule was lowered from.
    ///
    /// The schedule and the scalar executor must consume the exact same frozen
    /// graph contexts, so the fixture rebuild is deterministic and asserted
    /// against the schedule identity.
    fn schedule_execution(
        schedule: &mondrian_effects::PreparedEffectTemporalGpuExecution,
    ) -> mondrian_effects::PreparedEffectTemporalExecution {
        let program = echo_over_set_matte(true);
        let execution = prepare_temporal_frame_execution(&program, schedule.request(), |_| Ok(1))
            .expect("finite temporal execution");
        let again = execution.plan_gpu_execution().expect("device schedule");
        assert_eq!(again.cache_identity(), schedule.cache_identity());
        execution
    }

    #[test]
    fn device_temporal_spatial_schedule_rejects_over_budget_prefix_before_upload() {
        let Ok(context) = pollster::block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU temporal scratch test: no GPU adapter available");
            return;
        };
        let extent = EffectFrameExtent::new(8192, 8192);
        let now = TimelineTime::new(1, 2).unwrap();
        let historical = TimelineTime::new(1, 4).unwrap();
        let schedule = frozen_schedule(extent, now);
        let mut runtime = HeterogeneousGpuContinuationRuntime::new(
            context,
            GpuColorFrameWgpuResourcePoolOptions::default(),
        )
        .unwrap();
        let mut frames = GradientFrames { extent, historical, copies: 0 };
        let mut matte = GradientMatte { extent, historical, copies: 0 };
        let result = runtime.execute_temporal_schedule(
            &schedule,
            &mut frames,
            &mut matte,
            WorkingColorSpace::LinearRec709,
            Instant::now() + Duration::from_secs(120),
            &ExecutionCancellationToken::new(),
        );
        let error = result.err().expect("spatial prefix exceeds scratch budget");
        assert!(
            matches!(error, TemporalGpuExecutionError::Composite(
                GpuCompositeError::SpatialChainFailed { ref detail, .. }
            ) if detail.contains("scratch bytes, budget")),
            "{error}",
        );
        assert_eq!(frames.copies, 0);
        assert_eq!(matte.copies, 0);
    }

    #[test]
    fn device_temporal_schedule_reports_a_missing_second_input() {
        let extent = EffectFrameExtent::new(3, 2);
        let now = TimelineTime::new(1, 2).expect("current time");
        let historical = TimelineTime::new(1, 4).expect("historical time");
        let schedule = frozen_schedule(extent, now);
        let Ok(context) = pollster::block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU temporal schedule refusal test: no GPU adapter available");
            return;
        };
        let mut runtime = HeterogeneousGpuContinuationRuntime::new(
            context,
            GpuColorFrameWgpuResourcePoolOptions::default(),
        )
        .expect("device runtime");
        let mut frames = GradientFrames { extent, historical, copies: 0 };
        // A provider that never admits the historical slot must fail closed
        // before any pass is recorded.
        let mut empty = EmptyMatte;
        let error = runtime
            .execute_temporal_schedule(
                &schedule,
                &mut frames,
                &mut empty,
                WorkingColorSpace::LinearRec709,
                Instant::now() + Duration::from_secs(120),
                &ExecutionCancellationToken::new(),
            )
            .err()
            .expect("a missing second input must not publish a frame");
        assert!(
            matches!(
                error,
                TemporalGpuExecutionError::MissingAuxiliaryRaster { slot: 0, .. }
            ),
            "{error}"
        );
    }

    struct EmptyMatte;

    impl EffectTemporalAuxiliaryProvider for EmptyMatte {
        fn source_identity(&self) -> EffectTemporalSourceIdentity {
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([0x53; 32])
        }

        fn retained_coverage_bytes(&self) -> usize {
            0
        }

        fn auxiliary_image(
            &self,
            _context_time: TimelineTime,
            _slot: u32,
        ) -> Option<(EffectFrameExtent, ImageSampling, EffectValueFormat)> {
            None
        }

        fn copy_auxiliary(
            &mut self,
            request: EffectTemporalAuxiliaryRequest,
            _cancellation: &ExecutionCancellationToken,
            _destination: &mut Vec<[f32; 4]>,
        ) -> Result<(), EffectTemporalFrameProviderError> {
            Err(EffectTemporalFrameProviderError::Unavailable {
                reason: format!("no raster for slot {}", request.slot()),
            })
        }
    }

    /// The scalar executor rejects the same missing raster, so both routes
    /// agree on failure as well as on pixels.
    #[test]
    fn scalar_executor_rejects_the_missing_second_input_frame() {
        let extent = EffectFrameExtent::new(3, 2);
        let now = TimelineTime::new(1, 2).expect("current time");
        let historical = TimelineTime::new(1, 4).expect("historical time");
        let schedule = frozen_schedule(extent, now);
        let mut session = EffectExecutionSession::default();
        let mut frames = GradientFrames { extent, historical, copies: 0 };
        let mut empty = EmptyMatte;
        let error = session
            .execute_prepared_temporal_f32_with_auxiliary(
                &schedule_execution(&schedule),
                &mut frames,
                &mut empty,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect_err("the scalar executor also refuses a missing raster");
        // The root context is demanded first, so the scalar executor names the
        // current instant; the historical demand would follow it.
        assert!(
            matches!(
                error,
                EffectTemporalExecutionError::AuxiliaryFrameMissing { time, slot: 0 }
                    if time == now
            ),
            "{error}"
        );
        assert!(historical < now);
    }

    fn posterize_time_program() -> PreparedEffectProgram {
        let mut effect = mondrian_effects::instantiate_effect_node(EffectType::PosterizeTime)
            .expect("posterize time definition");
        effect
            .set_static_value_by_parameter(
                &EffectType::PosterizeTime
                    .parameter_id("frame_rate")
                    .expect("frame rate parameter"),
                PropertyValue::Float(24.0),
            )
            .expect("a 24 fps hold is admissible");
        PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709)
            .expect("posterize time is a valid program")
    }

    fn posterize_time_schedule(
        extent: EffectFrameExtent,
        now: TimelineTime,
    ) -> mondrian_effects::PreparedEffectTemporalGpuExecution {
        let program = posterize_time_program();
        let request = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Discontinuous,
            now,
            extent,
            ImageSampling::UNIT_SQUARE,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        prepare_temporal_frame_execution(&program, &request, |_| Ok(1))
            .expect("finite posterize time execution")
            .plan_gpu_execution()
            .expect("posterize time lowers to a device schedule")
    }

    /// A held frame is a real effect payload: the device must publish the
    /// earlier instant's pixels, not the current ones, and the scalar executor
    /// must agree to the seam tolerance.
    #[test]
    fn device_posterize_time_matches_the_scalar_executor() {
        let Ok(context) = pollster::block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU posterize time parity test: no GPU adapter available");
            return;
        };
        let extent = EffectFrameExtent::new(7, 5);
        let now = TimelineTime::new(51, 100).expect("current time");
        let held = TimelineTime::new(1, 2).expect("held instant");
        let schedule = posterize_time_schedule(extent, now);
        assert_eq!(schedule.step_count(), 1, "one hold is one sampled frame");

        let program = posterize_time_program();
        let execution = prepare_temporal_frame_execution(&program, schedule.request(), |_| Ok(1))
            .expect("finite posterize time execution");
        let mut session = EffectExecutionSession::default();
        let mut cpu_frames = GradientFrames { extent, historical: held, copies: 0 };
        let cpu = session
            .execute_prepared_temporal_f32_with_auxiliary(
                &execution,
                &mut cpu_frames,
                &mut EmptyMatte,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("scalar hold");

        let mut runtime = HeterogeneousGpuContinuationRuntime::new(
            context,
            GpuColorFrameWgpuResourcePoolOptions::default(),
        )
        .expect("device runtime");
        let mut gpu_frames = GradientFrames { extent, historical: held, copies: 0 };
        let device = runtime
            .execute_temporal_schedule(
                &schedule,
                &mut gpu_frames,
                &mut EmptyMatte,
                WorkingColorSpace::LinearRec709,
                Instant::now() + Duration::from_secs(120),
                &ExecutionCancellationToken::new(),
            )
            .expect("device hold");

        assert_eq!(
            device.readbacks(),
            1,
            "the held frame crosses the seam once"
        );
        assert_eq!(device.dispatches(), 1);
        assert_eq!(
            device.uploads(),
            2,
            "the current and held frames are leaves"
        );
        assert_eq!(gpu_frames.copies, 2, "both instants read the source");

        let cpu_pixels = cpu.tile().pixels();
        let gpu_pixels = device.output().tile().pixels();
        let difference = max_channel_difference(cpu_pixels, gpu_pixels);
        assert!(
            difference <= PARITY_TOLERANCE,
            "device hold diverged from the scalar executor by {difference}"
        );
        let width = extent.width() as usize;
        for (index, pixel) in gpu_pixels.iter().enumerate() {
            let expected = historical_source_pixel(
                index % width,
                index / width,
                width,
                extent.height() as usize,
            );
            for (channel, value) in expected.iter().enumerate() {
                assert!(
                    (pixel[channel] - value).abs() <= PARITY_TOLERANCE,
                    "pixel {index} channel {channel}: {} vs {value}",
                    pixel[channel]
                );
            }
        }
        // The two fixture gradients cross at a few pixels, so the whole-frame
        // difference from the current instant is the meaningful refusal.
        let current_frame = (0..gpu_pixels.len())
            .map(|index| {
                current_source_pixel(
                    index % width,
                    index / width,
                    width,
                    extent.height() as usize,
                )
            })
            .collect::<Vec<_>>();
        let drift = max_channel_difference(gpu_pixels, &current_frame);
        assert!(
            drift > 0.1,
            "the hold must not pass the current frame through, drifted by {drift}"
        );
    }

    #[test]
    fn device_timewarp_copies_the_held_frame_and_matches_scalar_coverage_mix() {
        use mondrian_core::{Rational, SampleAspectRatio, TimelineTimeRange};
        use mondrian_effects::{EffectFrameContext, EffectProjectGeometry, LutPreparationCache};

        let Ok(context) = pollster::block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU Timewarp parity test: no GPU adapter available");
            return;
        };
        let mut runtime = HeterogeneousGpuContinuationRuntime::new(
            context,
            GpuColorFrameWgpuResourcePoolOptions::default(),
        )
        .unwrap();
        let extent = EffectFrameExtent::new(7, 5);
        let first_time = TimelineTime::new(1, 25).unwrap();
        let second_time = TimelineTime::new(2, 25).unwrap();
        let now = TimelineTime::new(8, 25).unwrap();
        let frame_context = EffectFrameContext::new(
            Rational::FPS_25,
            EffectProjectGeometry::new(7, 5, SampleAspectRatio::SQUARE).unwrap(),
            TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
        )
        .unwrap();
        for (position, expected_times) in [
            (1.0, &[first_time][..]),
            (1.25, &[first_time, second_time][..]),
        ] {
            let mut effect =
                mondrian_effects::instantiate_effect_node(EffectType::Timewarp).unwrap();
            for (parameter, value) in [
                ("adjust_by", PropertyValue::Enum("source_frame".into())),
                ("method", PropertyValue::Enum("frame_mix".into())),
                ("source_frame", PropertyValue::Float(position)),
            ] {
                effect
                    .set_static_value_by_parameter(
                        &EffectType::Timewarp.parameter_id(parameter).unwrap(),
                        value,
                    )
                    .unwrap();
            }
            let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
                &[effect],
                &[],
                &[],
                &[],
                WorkingColorSpace::LinearRec709,
                frame_context,
                &LutPreparationCache::uncached(),
            )
            .unwrap();
            let request = EffectTemporalExecutionRequest::new(
                1,
                EffectExecutionContinuity::Discontinuous,
                now,
                extent,
                ImageSampling::UNIT_SQUARE,
                extent.full_frame_roi(),
                ExecutionCancellationToken::new(),
            );
            let execution =
                prepare_temporal_frame_execution(&program, &request, |_| Ok(1)).unwrap();
            let schedule = execution.plan_gpu_execution().unwrap();
            let demands = execution.demands();
            assert_eq!(
                demands.requests().iter().map(|request| request.time()).collect::<Vec<_>>(),
                expected_times,
            );
            let resolved = demands.requests().iter().copied().map(|request| {
                let pair = if request.time() == first_time {
                    [[2.0, -1.0, 4.0, 0.5], [3.0, -0.5, 1.0, 0.0]]
                } else {
                    [[10.0, 4.0, 0.0, 0.0], [-1.0, 3.0, 0.5, 0.75]]
                };
                let pixels = (0..35).map(|index| pair[index % 2]).collect();
                let tile = EffectFrameTileF32::new(
                    request.time(),
                    extent,
                    request.sampling(),
                    request.input_roi().region(),
                    1,
                    pixels,
                )
                .unwrap();
                (request, tile)
            });
            let mut frames = PreparedTemporalFrameSet::prepare(
                EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([0x81; 32]),
                demands.clone(),
                resolved,
            )
            .unwrap();
            let cpu = EffectExecutionSession::default()
                .execute_prepared_temporal_f32_with_auxiliary(
                    &execution,
                    &mut frames.clone(),
                    &mut EmptyMatte,
                    &mut EffectProcessorControl::uncancelled(),
                )
                .unwrap();
            let gpu = runtime
                .execute_temporal_schedule(
                    &schedule,
                    &mut frames,
                    &mut EmptyMatte,
                    WorkingColorSpace::LinearRec709,
                    Instant::now() + Duration::from_secs(60),
                    &ExecutionCancellationToken::new(),
                )
                .unwrap();
            assert_eq!(gpu.uploads(), expected_times.len());
            assert_eq!(gpu.dispatches(), 1);
            assert_eq!(gpu.readbacks(), 1);
            let actual = gpu.output().tile().pixels();
            if position == 1.0 {
                assert_eq!(
                    actual[1],
                    [3.0, -0.5, 1.0, 0.0],
                    "hold preserves hidden RGB"
                );
            } else {
                assert_eq!(actual[0], [2.0, -1.0, 4.0, 0.375]);
                assert_eq!(actual[1], [-1.0, 3.0, 0.5, 0.1875]);
            }
            let error = max_channel_difference(cpu.tile().pixels(), actual);
            eprintln!("Timewarp source frame {position}: maximum Float32 error {error:e}");
            assert!(error <= PARITY_TOLERANCE);
        }
    }
}
