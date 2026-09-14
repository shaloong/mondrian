//! Shared execution for native decoders that can expose shader-readable Y/UV
//! textures directly to the active wgpu device.
//!
//! Platform Adapters own native handle validation, synchronization, and plane
//! wrapping. This Module owns the single YUV-to-encoded-RGB and source-to-
//! working color execution path, so Metal and Vulkan cannot fork color math.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use mondrian_media::PreviewNativeDecodedFrame;

use crate::{
    GpuColorFrameAllocationPlan, GpuColorFrameResource, GpuColorFrameWgpuResource,
    GpuColorFrameWgpuResourcePool, GpuNativeDecodedFrameImportBackend,
    GpuNativeDecodedFrameImportError, GpuNativeDecodedFrameImportPlan,
    GpuNativeDecodedFrameImportSupport, GpuNativeVideoExtent, GpuNativeYuvDecodePlan,
    GpuNativeYuvDecoder, GpuNativeYuvPlaneViews, NativeVideoImportCpuTimings,
    RenderGpuOutputBoundaryRuntime,
};

/// Shader-readable planes plus any platform ownership retained by their wgpu
/// objects. The textures themselves keep the native drop callbacks alive.
pub(crate) struct DirectNativeYuvTextures {
    pub(crate) luma: wgpu::Texture,
    pub(crate) chroma: wgpu::Texture,
}

pub(crate) trait DirectNativeBufferSynchronization {
    fn submit(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        command: wgpu::CommandBuffer,
    ) -> Result<(), GpuNativeDecodedFrameImportError>;
}

/// Buffer and synchronization retain the decoder source through final GPU use;
/// their native owner also accounts for release after the last HAL reference.
pub(crate) struct DirectNativeYuvBuffer {
    pub buffer: wgpu::Buffer,
    pub row_pitch: u32,
    pub chroma_offset: u32,
    pub synchronization: Box<dyn DirectNativeBufferSynchronization>,
}

pub(crate) enum DirectNativeYuvInput {
    Textures(DirectNativeYuvTextures),
    Buffer(DirectNativeYuvBuffer),
}

#[derive(Default)]
struct DirectRetainedSourceState {
    count: AtomicUsize,
    changed: Condvar,
    change_lock: Mutex<()>,
}

impl DirectRetainedSourceState {
    fn increment(&self) -> Result<(), GpuNativeDecodedFrameImportError> {
        self.count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map(|_| ())
            .map_err(|_| {
                backend_rejected("direct native-source residency counter exhausted".to_owned())
            })
    }

    fn decrement(&self) {
        let _change = self.change_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = self.count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "direct native-source residency underflow");
        self.changed.notify_all();
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    fn wait_for_idle_until(
        &self,
        deadline: Instant,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        let mut change = self.change_lock.lock().map_err(|_| {
            backend_rejected("direct native-source release synchronization failed".to_owned())
        })?;
        loop {
            let remaining = self.count();
            if remaining == 0 {
                return Ok(());
            }
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Err(
                    GpuNativeDecodedFrameImportError::NativeReleaseDeadlineExceeded { remaining },
                );
            }
            let (next, wait) = self.changed.wait_timeout(change, timeout).map_err(|_| {
                backend_rejected("direct native-source release synchronization failed".to_owned())
            })?;
            change = next;
            if wait.timed_out() {
                let remaining = self.count();
                if remaining != 0 {
                    return Err(
                        GpuNativeDecodedFrameImportError::NativeReleaseDeadlineExceeded {
                            remaining,
                        },
                    );
                }
            }
        }
    }
}

/// Platform Adapter for one native decoded-surface family.
pub(crate) trait DirectNativeYuvPlaneAdapter {
    /// Exact support exposed by this concrete renderer/device pair.
    fn support(&self) -> &GpuNativeDecodedFrameImportSupport;

    fn supports_buffer_source(&self) -> bool {
        false
    }
    #[cfg(target_os = "linux")]
    fn poll_retirement(&mut self) -> Result<bool, GpuNativeDecodedFrameImportError> {
        Ok(true)
    }
    #[cfg(target_os = "linux")]
    fn wait_for_released_owners_until(
        &self,
        _deadline: Instant,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        Ok(())
    }
    fn retained_owner_count(&self) -> usize {
        0
    }

    fn import_input(
        &mut self,
        device: &wgpu::Device,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<DirectNativeYuvInput, GpuNativeDecodedFrameImportError> {
        self.import_textures(device, plan, native_frame)
            .map(DirectNativeYuvInput::Textures)
    }

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
    retained_sources: Arc<DirectRetainedSourceState>,
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
        let mut yuv_decoder = GpuNativeYuvDecoder::new(device);
        if adapter.supports_buffer_source() {
            yuv_decoder.enable_buffer_source(device);
        }
        Ok(Self {
            adapter,
            device: device.clone(),
            queue: queue.clone(),
            yuv_decoder,
            color_runtime,
            frame_cpu_timings: NativeVideoImportCpuTimings::default(),
            retained_sources: Arc::new(DirectRetainedSourceState::default()),
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
        let source = if self.adapter.supports_buffer_source() {
            self.yuv_decoder.fused_buffer_input().ok_or_else(|| {
                backend_rejected("native buffer sampling pipeline is unavailable".to_owned())
            })?
        } else {
            self.yuv_decoder.fused_texture_input()
        };
        self.color_runtime
            .prepare_fused_yuv_input(
                &plan.input_transform,
                &plan.encoded_source_frame,
                &plan.working_frame,
                source,
                &self.device,
                &self.queue,
            )
            .map(|_| ())
            .map_err(|error| {
                backend_rejected(format!("native fused input preparation failed: {error}"))
            })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn poll_retirement(&mut self) -> Result<bool, GpuNativeDecodedFrameImportError> {
        self.adapter.poll_retirement()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn wait_for_released_sources_until(
        &self,
        deadline: Instant,
    ) -> Result<usize, GpuNativeDecodedFrameImportError> {
        self.adapter.wait_for_released_owners_until(deadline)?;
        self.retained_sources.wait_for_idle_until(deadline)?;
        Ok(self.retained_source_count())
    }

    pub(crate) fn retained_source_count(&self) -> usize {
        self.retained_sources
            .count()
            .saturating_add(self.adapter.retained_owner_count())
    }

    fn import_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuNativeDecodedFrameImportError>
    {
        let total_started = Instant::now();
        let bridge_acquire_started = Instant::now();
        let input = self.adapter.import_input(&self.device, plan, native_frame)?;
        let bridge_acquire_us = elapsed_us(bridge_acquire_started);

        let pipeline_prepare_started = Instant::now();
        let yuv_plan = GpuNativeYuvDecodePlan::from_import_plan(
            plan,
            GpuNativeVideoExtent {
                width: native_frame.width,
                height: native_frame.height,
            },
        )
        .map_err(|error| backend_rejected(error.to_string()))?;
        let prepared_yuv = match &input {
            DirectNativeYuvInput::Textures(textures) => {
                let luma_view = textures.luma.create_view(&wgpu::TextureViewDescriptor::default());
                let chroma_view =
                    textures.chroma.create_view(&wgpu::TextureViewDescriptor::default());
                self.yuv_decoder.prepare_pass(
                    &self.device,
                    &yuv_plan,
                    GpuNativeYuvPlaneViews {
                        luma: &luma_view,
                        chroma: &chroma_view,
                        chroma_v: &chroma_view,
                    },
                )
            }
            DirectNativeYuvInput::Buffer(source) => self
                .yuv_decoder
                .prepare_buffer_pass(
                    &self.device,
                    &yuv_plan,
                    &source.buffer,
                    source.row_pitch,
                    source.chroma_offset,
                )
                .map_err(|error| backend_rejected(error.to_string()))?,
        };
        let source = match &input {
            DirectNativeYuvInput::Textures(_) => self.yuv_decoder.fused_texture_input(),
            DirectNativeYuvInput::Buffer(_) => {
                self.yuv_decoder.fused_buffer_input().ok_or_else(|| {
                    backend_rejected("native buffer sampling pipeline is unavailable".to_owned())
                })?
            }
        };
        let backend = self
            .color_runtime
            .prepare_fused_yuv_input(
                &plan.input_transform,
                &plan.encoded_source_frame,
                &plan.working_frame,
                source,
                &self.device,
                &self.queue,
            )
            .map_err(|error| {
                backend_rejected(format!("native fused input preparation failed: {error}"))
            })?;
        let resource_pool = self.color_runtime.resource_pool();
        let working = resource_pool.acquire(
            &self.device,
            &GpuColorFrameAllocationPlan::for_handle(plan.working_frame.clone()),
        );
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.native-video.direct-import"),
        });
        let pipeline_prepare_us = elapsed_us(pipeline_prepare_started);
        let yuv_record_started = Instant::now();
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mondrian.native-video.fused-working-input"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &working.resource().texture_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&backend.pipeline);
            pass.set_bind_group(0, &backend.objects.ocio_bind_group.bind_group, &[]);
            pass.set_bind_group(1, &prepared_yuv.bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
        // This bracket records the fused YUV + OCIO pass. No separate color
        // pass or frame-table extraction remains to time.
        let yuv_record_us = elapsed_us(yuv_record_started);
        let color_stage_us = 0;
        let resource_extract_us = 0;

        let submit_started = Instant::now();
        // Buffer inputs retain their decoder source in the native allocation
        // owner through final HAL release. Only direct textures need this extra
        // queue-completion retain.
        let retained_source = matches!(&input, DirectNativeYuvInput::Textures(_))
            .then(|| native_frame.handle.clone());
        if retained_source.is_some() {
            self.retained_sources.increment()?;
        }
        let command = encoder.finish();
        let submitted = match &input {
            DirectNativeYuvInput::Textures(_) => {
                self.queue.submit([command]);
                Ok(())
            }
            DirectNativeYuvInput::Buffer(source) => {
                source.synchronization.submit(&self.device, &self.queue, command)
            }
        };
        if let Err(error) = submitted {
            if retained_source.is_some() {
                self.retained_sources.decrement();
            }
            return Err(error);
        }
        if let Some(retained_source) = retained_source {
            let retained_sources = Arc::clone(&self.retained_sources);
            self.queue.on_submitted_work_done(move || {
                drop(retained_source);
                retained_sources.decrement();
            });
        }
        let submit_us = elapsed_us(submit_started);
        self.frame_cpu_timings = NativeVideoImportCpuTimings {
            // The shared executor validated the immutable plan and native
            // source descriptor before entering this backend. The platform
            // Adapter then validates physical handles while adopting them, so
            // that inseparable work belongs to the bridge-acquire stage.
            source_validation_us: 0,
            bridge_acquire_us,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn direct_source_wait_fails_closed_then_observes_callback_release() {
        let state = Arc::new(DirectRetainedSourceState::default());
        state.increment().expect("source admission");
        let error = state
            .wait_for_idle_until(Instant::now() + Duration::from_millis(1))
            .expect_err("live source cannot establish release");
        assert!(matches!(
            error,
            GpuNativeDecodedFrameImportError::NativeReleaseDeadlineExceeded { remaining: 1 }
        ));

        let callback_state = Arc::clone(&state);
        let callback = std::thread::spawn(move || callback_state.decrement());
        state
            .wait_for_idle_until(Instant::now() + Duration::from_secs(2))
            .expect("callback release observed");
        callback.join().expect("callback joined");
        assert_eq!(state.count(), 0);
    }
}
