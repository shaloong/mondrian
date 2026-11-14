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
    /// The renderer cannot record this step shape yet.
    UnsupportedStep {
        /// Stable operation name.
        operation: &'static str,
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
            Self::UnsupportedStep { operation } => {
                write!(
                    formatter,
                    "the renderer cannot record the {operation} temporal step"
                )
            }
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
    let mut values: HashMap<TemporalGpuValue, GpuColorFrameHandle> = HashMap::new();
    let mut peak_resident = 0usize;
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
            if leaf_extent != extent {
                return Err(TemporalGpuExecutionError::LeafExtentMismatch {
                    value,
                    expected: extent,
                    actual: leaf_extent,
                });
            }
            if pixels.len() != extent_pixels {
                return Err(TemporalGpuExecutionError::LeafCoverageMismatch {
                    value,
                    expected: extent_pixels,
                    actual: pixels.len(),
                });
            }
            let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
                sampling,
                alpha,
                width: extent.width(),
                height: extent.height(),
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
            peak_resident = peak_resident.max(values.len());
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("temporal-gpu-schedule"),
        });
        for step in schedule.steps() {
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
                TemporalGpuStepOperation::Spatial { .. } => {
                    return Err(TemporalGpuExecutionError::UnsupportedStep {
                        operation: "spatial",
                    });
                }
            };
            values.insert(value, output);
            dispatches = dispatches.saturating_add(1);
            peak_resident = peak_resident.max(values.len());
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
        Ok((tile, peak_resident))
    })();

    for handle in values.into_values() {
        let _ = table.take(&handle);
    }
    let (tile, peak_resident) = executed?;
    let peak_working_bytes = extent_pixels.saturating_mul(16).saturating_mul(peak_resident.max(1));
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

    fn echo_over_set_matte() -> PreparedEffectProgram {
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
        PreparedEffectProgram::prepare(&[matte, echo], &[], WorkingColorSpace::LinearRec709)
            .expect("echo over set matte is a valid program")
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
        let program = echo_over_set_matte();
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
        assert!(
            difference <= PARITY_TOLERANCE,
            "device schedule diverged from the scalar executor by {difference}"
        );
        assert!(
            cpu_pixels.iter().any(|pixel| pixel[3] > 0.02 && pixel[3] < 0.98),
            "the fixture must produce a visible second-input mix"
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
        let program = echo_over_set_matte();
        let execution = prepare_temporal_frame_execution(&program, schedule.request(), |_| Ok(1))
            .expect("finite temporal execution");
        let again = execution.plan_gpu_execution().expect("device schedule");
        assert_eq!(again.cache_identity(), schedule.cache_identity());
        execution
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
}
