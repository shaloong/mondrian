//! Shared execution for native decoders that can expose shader-readable Y/UV
//! textures directly to the active wgpu device.
//!
//! Platform Adapters own native handle validation, synchronization, and plane
//! wrapping. This Module owns the single YUV-to-encoded-RGB and source-to-
//! working color execution path, so Metal and Vulkan cannot fork color math.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use mondrian_media::PreviewNativeDecodedFrame;

use crate::{
    ColorFrameResidency, GpuColorFrameResource, GpuColorFrameWgpuResource,
    GpuColorFrameWgpuResourcePool, GpuNativeDecodedFrameImportBackend,
    GpuNativeDecodedFrameImportError, GpuNativeDecodedFrameImportPlan,
    GpuNativeDecodedFrameImportSupport, GpuNativeVideoExtent, GpuNativeYuvDecodePlan,
    GpuNativeYuvDecoder, GpuNativeYuvPlaneViews, NativeVideoImportCpuTimings,
    RenderColorTransformGpuOptions, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
};

/// Shader-readable planes plus any platform ownership retained by their wgpu
/// objects. The textures themselves keep the native drop callbacks alive.
pub(crate) struct DirectNativeYuvTextures {
    pub(crate) luma: wgpu::Texture,
    pub(crate) chroma: wgpu::Texture,
}

/// Platform Adapter for one native decoded-surface family.
pub(crate) trait DirectNativeYuvPlaneAdapter {
    /// Exact support exposed by this concrete renderer/device pair.
    fn support(&self) -> &GpuNativeDecodedFrameImportSupport;

    /// Validate, synchronize, and wrap the two native planes.
    fn import_textures(
        &mut self,
        device: &wgpu::Device,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<DirectNativeYuvTextures, GpuNativeDecodedFrameImportError>;
}

/// Failure to create a direct native-video backend.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(crate) enum DirectNativeVideoImportBackendCreateError {
    /// The shared source-to-working color runtime could not be created.
    #[error("could not create native-video color runtime: {reason}")]
    ColorRuntime {
        /// Concrete renderer error.
        reason: String,
    },
}

/// Shared direct-plane import and color execution backend.
pub(crate) struct DirectNativeVideoImportBackend<A> {
    adapter: A,
    device: wgpu::Device,
    queue: wgpu::Queue,
    yuv_decoder: GpuNativeYuvDecoder,
    color_runtime: RenderGpuOutputBoundaryRuntime,
    frame_cpu_timings: NativeVideoImportCpuTimings,
    retained_sources: Arc<AtomicUsize>,
}

impl<A> DirectNativeVideoImportBackend<A>
where
    A: DirectNativeYuvPlaneAdapter,
{
    pub(crate) fn new(
        adapter: A,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, DirectNativeVideoImportBackendCreateError> {
        let color_runtime = RenderGpuOutputBoundaryRuntime::with_resource_pool(resource_pool)
            .map_err(
                |error| DirectNativeVideoImportBackendCreateError::ColorRuntime {
                    reason: error.to_string(),
                },
            )?;
        Ok(Self {
            adapter,
            device: device.clone(),
            queue: queue.clone(),
            yuv_decoder: GpuNativeYuvDecoder::new(device),
            color_runtime,
            frame_cpu_timings: NativeVideoImportCpuTimings::default(),
            retained_sources: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub(crate) fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        self.adapter.support()
    }

    pub(crate) fn frame_cpu_timings(&self) -> NativeVideoImportCpuTimings {
        self.frame_cpu_timings
    }

    pub(crate) fn prepare_import_plan(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        self.color_runtime
            .prepare_wgpu_input_stage_gpu_frame_backend_objects(
                &plan.input_transform,
                &plan.encoded_source_frame,
                &plan.working_frame,
                RenderColorTransformGpuOptions {
                    output_residency: ColorFrameResidency::Gpu,
                    ..RenderColorTransformGpuOptions::default()
                },
                &self.device,
                &self.queue,
            )
            .map_err(|error| {
                backend_rejected(format!(
                    "source-to-working color backend preparation failed: {error:?}"
                ))
            })
    }

    pub(crate) fn retained_source_count(&self) -> usize {
        self.retained_sources.load(Ordering::Acquire)
    }

    fn import_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuNativeDecodedFrameImportError>
    {
        let total_started = Instant::now();
        let source_validation_started = Instant::now();
        let textures = self.adapter.import_textures(&self.device, plan, native_frame)?;
        let source_validation_us = elapsed_us(source_validation_started);

        let pipeline_prepare_started = Instant::now();
        let yuv_plan = GpuNativeYuvDecodePlan::from_import_plan(
            plan,
            GpuNativeVideoExtent {
                width: native_frame.width,
                height: native_frame.height,
            },
        )
        .map_err(|error| backend_rejected(error.to_string()))?;
        let luma_view = textures.luma.create_view(&wgpu::TextureViewDescriptor::default());
        let chroma_view = textures.chroma.create_view(&wgpu::TextureViewDescriptor::default());
        let prepared_yuv = self.yuv_decoder.prepare_pass(
            &self.device,
            &yuv_plan,
            GpuNativeYuvPlaneViews {
                luma: &luma_view,
                chroma: &chroma_view,
                chroma_v: &chroma_view,
            },
        );
        let (_, encoded_payload) =
            GpuNativeYuvDecoder::allocate_output(&self.device, &yuv_plan).into_parts();
        let encoded_resource =
            GpuColorFrameResource::new(plan.encoded_source_frame.clone(), encoded_payload);
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.native-video.direct-import"),
        });
        let pipeline_prepare_us = elapsed_us(pipeline_prepare_started);

        let yuv_record_started = Instant::now();
        self.yuv_decoder
            .record(&mut encoder, &yuv_plan, &prepared_yuv, &encoded_resource)
            .map_err(|error| backend_rejected(error.to_string()))?;
        let yuv_record_us = elapsed_us(yuv_record_started);

        let color_stage_started = Instant::now();
        if let Some(previous) =
            self.color_runtime.frame_table_mut().insert(encoded_resource).map_err(|error| {
                backend_rejected(format!("encoded source insertion failed: {error:?}"))
            })?
        {
            let _new_entry =
                self.color_runtime.frame_table_mut().remove(plan.encoded_source_frame.id());
            let restore_result = self.color_runtime.frame_table_mut().insert(previous);
            debug_assert!(
                restore_result.is_ok(),
                "failed to restore collided GPU resource"
            );
            return Err(backend_rejected(
                "encoded source id replaced a live native-video resource".to_owned(),
            ));
        }
        if let Err(error) = self.color_runtime.record_wgpu_input_stage_gpu_frame_owned_backend(
            &plan.input_transform,
            &plan.encoded_source_frame,
            &plan.working_frame,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Gpu,
                ..RenderColorTransformGpuOptions::default()
            },
            RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                device: &self.device,
                queue: &self.queue,
                encoder: &mut encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            },
        ) {
            self.color_runtime.frame_table_mut().remove(plan.encoded_source_frame.id());
            self.color_runtime.frame_table_mut().remove(plan.working_frame.id());
            return Err(backend_rejected(format!(
                "source-to-working color stage failed: {error:?}"
            )));
        }
        let color_stage_us = elapsed_us(color_stage_started);

        let resource_extract_started = Instant::now();
        let Some(working) = self.color_runtime.frame_table_mut().remove(plan.working_frame.id())
        else {
            self.color_runtime.frame_table_mut().remove(plan.encoded_source_frame.id());
            return Err(backend_rejected(
                "color stage did not retain its working output".to_owned(),
            ));
        };
        if self
            .color_runtime
            .frame_table_mut()
            .remove(plan.encoded_source_frame.id())
            .is_none()
        {
            return Err(backend_rejected(
                "color stage lost its encoded input".to_owned(),
            ));
        }
        let resource_extract_us = elapsed_us(resource_extract_started);

        let submit_started = Instant::now();
        let retained_source = native_frame.handle.clone();
        self.retained_sources
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| {
                backend_rejected("direct native-source residency counter exhausted".to_owned())
            })?;
        self.queue.submit(std::iter::once(encoder.finish()));
        let retained_sources = Arc::clone(&self.retained_sources);
        self.queue.on_submitted_work_done(move || {
            drop(retained_source);
            let previous = retained_sources.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "direct native-source residency underflow");
        });
        let submit_us = elapsed_us(submit_started);
        self.frame_cpu_timings = NativeVideoImportCpuTimings {
            source_validation_us,
            bridge_acquire_us: 0,
            pipeline_prepare_us,
            yuv_record_us,
            color_stage_us,
            resource_extract_us,
            submit_us,
            total_us: elapsed_us(total_started),
        };
        Ok(working)
    }
}

impl<A> GpuNativeDecodedFrameImportBackend for DirectNativeVideoImportBackend<A>
where
    A: DirectNativeYuvPlaneAdapter,
{
    type NativeFrame = PreviewNativeDecodedFrame;
    type Resource = GpuColorFrameWgpuResource;

    fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        self.support()
    }

    fn import_native_decoded_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &Self::NativeFrame,
    ) -> Result<GpuColorFrameResource<Self::Resource>, GpuNativeDecodedFrameImportError> {
        self.import_frame(plan, native_frame)
    }
}

fn backend_rejected(reason: String) -> GpuNativeDecodedFrameImportError {
    GpuNativeDecodedFrameImportError::BackendRejected { reason }
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
