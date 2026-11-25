//! Device-side execution of one frozen temporal value schedule.
//!
//! [`PreparedEffectTemporalGpuExecution`] states which host rasters a
//! time-expanded graph reads and which ordered passes combine them. This module
//! is the only place that turns that schedule into real work: it uploads one
//! working texture per leaf, records every step in schedule order into a single
//! command encoder, and reads the root back exactly once.
//!
//! Nothing here invents a pass for a shape the renderer cannot record. An
//! unsupported step fails the whole execution, so the caller keeps the scalar
//! CPU executor instead of publishing a silently different image.

use crate::{
    CpuColorFrame, GpuColorFrameHandle, GpuColorFrameIdAllocator, GpuColorFrameReadback,
    GpuColorFrameReadbackPlan, GpuColorFrameResourceTable, GpuColorFrameTextureFormat,
    GpuColorFrameUploadPlan, GpuColorFrameUploader, GpuColorFrameWgpuResource,
    GpuColorFrameWgpuResourcePool, GpuCompositeError, GpuFrameCompositor,
};
use mondrian_core::{
    ColorFrameAlpha, ExecutionCancellationToken, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_effects::{
    EffectFrameExtent, EffectFrameTileF32, EffectPassBlendPolicy, EffectTemporalAuxiliaryProvider,
    EffectTemporalExecutionOutput, EffectTemporalFrameProvider, PreparedEffectTemporalGpuExecution,
    TemporalGpuLeafRequest, TemporalGpuStepOperation, TemporalGpuValue,
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

/// Execute one frozen schedule and read its root back exactly once.
///
/// The schedule's leaves are uploaded as working Float32 textures, every step is
/// recorded into one encoder in schedule order, and the root is the only value
/// read back. Intermediate values never leave the device.
pub fn execute_temporal_schedule(
    device: TemporalGpuDevice<'_>,
    schedule: &PreparedEffectTemporalGpuExecution,
    frames: &mut dyn EffectTemporalFrameProvider,
    auxiliary: &mut dyn EffectTemporalAuxiliaryProvider,
    working_color_space: WorkingColorSpace,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<TemporalGpuExecution, TemporalGpuExecutionError> {
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
    } = device;
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
        let chain = if let TemporalGpuStepOperation::Spatial { plan, .. } = step.operation() {
            let chain = crate::spatial_driver::gpu_spatial_chain_plan(
                plan,
                extent.width(),
                extent.height(),
            )
            .map_err(|blocker| {
                TemporalGpuExecutionError::Composite(GpuCompositeError::SpatialChainFailed {
                    pass: crate::gpu_compositor::spatial_chain_blocker_pass(&blocker),
                    detail: blocker.to_string(),
                })
            })?;
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
            Some(chain)
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
            None
        };
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
    let mut values: HashMap<TemporalGpuValue, GpuColorFrameHandle> = HashMap::new();
    let mut resident_bytes = 0usize;
    let mut uploads = 0usize;
    let mut dispatches = 0usize;

    let executed = (|| -> Result<(EffectFrameTileF32, usize), TemporalGpuExecutionError> {
        for leaf in schedule.leaves() {
            let value = leaf.value();
            if values.contains_key(&value) {
                return Err(TemporalGpuExecutionError::DuplicateLeaf(value));
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

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("temporal-gpu-schedule"),
        });
        let mut matte_scratch = Vec::new();
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
            let output = match step.operation() {
                TemporalGpuStepOperation::Point { input, plan } => {
                    let input = input_handle(*input)?;
                    compositor
                        .record_point_effect_pass(
                            device,
                            queue,
                            &mut encoder,
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
                TemporalGpuStepOperation::TemporalBlend { input, sample, mix } => {
                    let input = input_handle(*input)?;
                    let sample = input_handle(*sample)?;
                    compositor
                        .record_temporal_blend_pass(
                            device,
                            &mut encoder,
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
                            &mut encoder,
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
                            &mut encoder,
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
                            &mut encoder,
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
                            &mut encoder,
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
                            &mut encoder,
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
                            &mut encoder,
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
                TemporalGpuStepOperation::Blend { base, overlay, opacity, blend_mode } => {
                    let base = input_handle(*base)?;
                    let overlay = input_handle(*overlay)?;
                    compositor
                        .record_adjustment_blend_pass(
                            device,
                            queue,
                            &mut encoder,
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
                            &mut encoder,
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
                TemporalGpuStepOperation::Spatial { input, .. } => {
                    let input = input_handle(*input)?;
                    compositor
                        .record_prepared_spatial_effect_input(
                            device,
                            queue,
                            &mut encoder,
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
        let root_handle = values
            .get(&root)
            .cloned()
            .ok_or(TemporalGpuExecutionError::RootNotMaterialized(root))?;
        let readback_plan = GpuColorFrameReadbackPlan::encoded_rgba32float(root_handle.clone())
            .map_err(|error| TemporalGpuExecutionError::Readback(format!("{error:?}")))?;
        let readback = {
            let root_resource = table
                .get(&root_handle)
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
                    break result.map_err(|error| {
                        TemporalGpuExecutionError::Readback(format!("{error:?}"))
                    });
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
        let tile = EffectFrameTileF32::new(
            schedule.request().output_time(),
            extent,
            schedule.sampling(),
            extent.full_frame_roi(),
            schedule.request().output_frame_seed(),
            pixels,
        )
        .map_err(TemporalGpuExecutionError::FrameProvider)?;
        Ok((tile, resident_bytes))
    })();

    for handle in values.into_values() {
        let _ = table.take(&handle);
    }
    let (tile, resident_bytes) = executed?;
    let peak_working_bytes = resident_bytes.saturating_add(
        usize::try_from(pooled_spatial_scratch.saturating_add(per_stage_spatial_scratch))
            .unwrap_or(usize::MAX),
    );
    Ok(TemporalGpuExecution {
        output: EffectTemporalExecutionOutput::from_device_tile(
            tile,
            schedule.cache_identity(),
            uploads,
            peak_working_bytes,
        ),
        uploads,
        dispatches,
        readbacks: 1,
    })
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
}
