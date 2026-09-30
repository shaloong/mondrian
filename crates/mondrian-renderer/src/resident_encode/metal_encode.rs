//! Shared RGB-to-YUV compute, followed by native Metal blits to codec IOSurfaces.

use super::yuv_conversion::conversion_shader;
use super::{
    validate_resident_source_contract, ResidentEncodeAdapterContract,
    ResidentEncodeAdapterCreateError, ResidentEncodeSubmissionError,
};
use crate::GpuResidentEncoderInputLease;
use mondrian_media::{
    MetalResidentEncodeDeviceRoot, MetalResidentEncodeInputFrame, MetalResidentEncodeReadyFrame,
    ResidentEncodeBitDepth,
};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount,
    CVPixelBufferGetWidth,
};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder,
    MTLCommandQueue, MTLDevice, MTLOrigin, MTLPixelFormat, MTLResourceOptions, MTLSize,
};
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

const COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);
/// Actual native conversion/copy facts; this adapter never maps pixel buffers.
#[derive(Clone, Copy, Debug, Default)]
pub struct MetalResidentEncodeAdapterDiagnostics {
    /// Shared color kernel submissions.
    pub color_conversion_submissions: u64,
    /// Metal GPU plane blits.
    pub device_to_device_copies: u64,
    /// Exact compute completion waits.
    pub compute_completion_waits: u64,
    /// Exact native blit completion waits.
    pub metal_completion_waits: u64,
    /// Pixel readbacks.
    pub cpu_pixel_readbacks: u64,
    /// Rawvideo transport bytes.
    pub rawvideo_pipe_bytes: u64,
    /// CPU pixel uploads.
    pub cpu_pixel_uploads: u64,
    /// One GPU bridge allocation, counted once on unified memory.
    pub bridge_allocation_bytes: u64,
}
/// Exact-device resident surface adapter under the existing renderer owner.
pub struct MetalResidentEncodeAdapter {
    contract: ResidentEncodeAdapterContract,
    device: wgpu::Device,
    queue: wgpu::Queue,
    buffer: wgpu::Buffer,
    raw_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    native_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    cache: CFRetained<CVMetalTextureCache>,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    row_pitch: u32,
    chroma_offset: u32,
    root: MetalResidentEncodeDeviceRoot,
    poisoned: bool,
    diagnostics: MetalResidentEncodeAdapterDiagnostics,
}
impl MetalResidentEncodeAdapter {
    /// Bind the actual wgpu Metal device and allocate one native GPU bridge.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        contract: ResidentEncodeAdapterContract,
    ) -> Result<Self, ResidentEncodeAdapterCreateError> {
        contract.validate_common()?;
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }.ok_or(
            ResidentEncodeAdapterCreateError::WrongPlatformBackend {
                required: "Metal",
                object: "device",
            },
        )?;
        let raw = hal.raw_device();
        if !raw.hasUnifiedMemory() {
            return Err(create_error(
                "device identity",
                "resident VideoToolbox requires the active unified-memory Metal device",
            ));
        }
        let root =
            unsafe { MetalResidentEncodeDeviceRoot::from_unified_metal_device(raw.registryID()) }
                .map_err(|e| create_error("device identity", e))?;
        let bytes = if contract.bit_depth == ResidentEncodeBitDepth::Ten {
            2u64
        } else {
            1
        };
        let pitch = (u64::from(contract.width) * bytes).div_ceil(256) * 256;
        let size = contract.compute_transport_byte_len()?;
        let offset = pitch * u64::from(contract.height);
        let row_pitch = u32::try_from(pitch).map_err(|e| create_error("allocation layout", e))?;
        let chroma_offset =
            u32::try_from(offset).map_err(|e| create_error("allocation layout", e))?;
        if size > device.limits().max_storage_buffer_binding_size
            || size > device.limits().max_buffer_size
        {
            return Err(create_error(
                "allocation limits",
                "bridge exceeds actual device limits",
            ));
        }
        let raw_buffer = raw
            .newBufferWithLength_options(
                usize::try_from(size).map_err(|e| create_error("allocation extent", e))?,
                MTLResourceOptions::StorageModePrivate,
            )
            .ok_or_else(|| create_error("allocation", "Metal returned no buffer"))?;
        let native_queue = raw
            .newCommandQueue()
            .ok_or_else(|| create_error("command queue", "Metal returned no queue"))?;
        let mut cache_ptr = std::ptr::null_mut();
        let result = unsafe {
            CVMetalTextureCache::create(None, None, raw, None, NonNull::from(&mut cache_ptr))
        };
        if result != 0 {
            return Err(create_error("texture cache", result));
        }
        let cache = unsafe {
            CFRetained::from_raw(
                NonNull::new(cache_ptr)
                    .ok_or_else(|| create_error("texture cache", "CoreVideo returned null"))?,
            )
        };
        drop(hal);
        // SAFETY: same actual Metal device, private storage, nonzero exact size;
        // this adapter retains a native reference through every GPU blit.
        let native = unsafe { wgpu::hal::metal::Device::buffer_from_raw(raw_buffer.clone(), size) };
        let buffer = unsafe {
            device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                native,
                &wgpu::BufferDescriptor {
                    label: Some("mondrian.resident-metal-yuv"),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                },
            )
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.resident-yuv-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.resident-yuv-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.resident-yuv"),
            source: wgpu::ShaderSource::Wgsl(
                conversion_shader(contract, row_pitch, chroma_offset).into(),
            ),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mondrian.resident-yuv"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(Self {
            contract,
            device: device.clone(),
            queue: queue.clone(),
            buffer,
            raw_buffer,
            native_queue,
            cache,
            pipeline,
            layout,
            row_pitch,
            chroma_offset,
            root,
            poisoned: false,
            diagnostics: MetalResidentEncodeAdapterDiagnostics {
                bridge_allocation_bytes: size,
                ..Default::default()
            },
        })
    }
    /// Actual unified device root for codec admission.
    pub fn encoder_device_root(&self) -> MetalResidentEncodeDeviceRoot {
        self.root.clone()
    }
    /// Convert an exact renderer output and complete both native plane writes.
    pub fn process(
        &mut self,
        source: GpuResidentEncoderInputLease,
        destination: MetalResidentEncodeInputFrame,
    ) -> Result<MetalResidentEncodeReadyFrame, ResidentEncodeSubmissionError> {
        if self.poisoned {
            return Err(submit_error(
                "lifecycle",
                "previous GPU completion remains unproven",
            ));
        }
        validate_resident_source_contract(self.contract, source.contract(), true)?;
        let surface = destination.surface();
        if (
            surface.width,
            surface.height,
            surface.bit_depth,
            surface.registry_id,
        ) != (
            self.contract.width,
            self.contract.height,
            self.contract.bit_depth,
            self.root.registry_id(),
        ) {
            return Err(ResidentEncodeSubmissionError::DestinationContract);
        }
        // SAFETY: the move-only destination owns this CoreVideo reference for
        // the whole borrow; no CPU address is requested or touched.
        let pixel = unsafe { &*surface.pixel_buffer.cast::<CVPixelBuffer>() };
        let expected = match (self.contract.bit_depth, self.contract.full_range) {
            (ResidentEncodeBitDepth::Eight, false) => u32::from_be_bytes(*b"420v"),
            (ResidentEncodeBitDepth::Eight, true) => u32::from_be_bytes(*b"420f"),
            (ResidentEncodeBitDepth::Ten, false) => u32::from_be_bytes(*b"x420"),
            (ResidentEncodeBitDepth::Ten, true) => u32::from_be_bytes(*b"xf20"),
        };
        if CVPixelBufferGetWidth(pixel) != self.contract.width as usize
            || CVPixelBufferGetHeight(pixel) != self.contract.height as usize
            || CVPixelBufferGetPlaneCount(pixel) != 2
            || CVPixelBufferGetPixelFormatType(pixel) != expected
        {
            return Err(ResidentEncodeSubmissionError::DestinationContract);
        }
        let formats = if self.contract.bit_depth == ResidentEncodeBitDepth::Ten {
            [MTLPixelFormat::R16Unorm, MTLPixelFormat::RG16Unorm]
        } else {
            [MTLPixelFormat::R8Unorm, MTLPixelFormat::RG8Unorm]
        };
        let y = self.plane(pixel, 0, formats[0])?;
        let uv = self.plane(pixel, 1, formats[1])?;
        let view = source.texture().create_view(&Default::default());
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.resident-yuv"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mondrian.resident-yuv"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            let pixels = if self.contract.bit_depth == ResidentEncodeBitDepth::Eight {
                4
            } else {
                2
            };
            pass.dispatch_workgroups(
                self.contract.width.div_ceil(pixels).div_ceil(8),
                (self.contract.height / 2).div_ceil(8),
                1,
            );
        }
        let submission = self.queue.submit([encoder.finish()]);
        self.diagnostics.color_conversion_submissions += 1;
        self.diagnostics.compute_completion_waits += 1;
        if let Err(error) = self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(COMPLETION_TIMEOUT),
        }) {
            self.quarantine();
            std::mem::forget(source);
            std::mem::forget(destination);
            return Err(submit_error("compute completion", error));
        }
        let command = self
            .native_queue
            .commandBuffer()
            .ok_or_else(|| submit_error("blit command", "Metal returned no command"))?;
        let blit = command
            .blitCommandEncoder()
            .ok_or_else(|| submit_error("blit encoder", "Metal returned no encoder"))?;
        for (plane, texture) in [y.as_ref(), uv.as_ref()].into_iter().enumerate() {
            let metal = CVMetalTextureGetTexture(texture)
                .ok_or_else(|| submit_error("plane texture", "missing native texture"))?;
            let divisor = if plane == 0 { 1 } else { 2 };
            let width = (self.contract.width / divisor) as usize;
            let height = (self.contract.height / divisor) as usize;
            // SAFETY: source allocation includes both aligned planes; destination
            // CV textures retain their exact parent and extent through completion.
            unsafe {
                blit.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                &self.raw_buffer,if plane==0{0}else{self.chroma_offset as usize},self.row_pitch as usize,self.row_pitch as usize*height,
                MTLSize{width,height,depth:1},&metal,0,0,MTLOrigin{x:0,y:0,z:0});
            }
        }
        blit.endEncoding();
        let (completed, completion) = mpsc::sync_channel(1);
        let handler = block2::RcBlock::new(
            move |buffer: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                // SAFETY: Metal invokes the handler with its live completed command.
                let status = unsafe { buffer.as_ref() }.status();
                let _ = completed.send(status);
            },
        );
        // SAFETY: Metal copies this valid block and owns it through completion;
        // its only capture is a thread-safe sender, never a borrowed surface.
        unsafe { command.addCompletedHandler(block2::RcBlock::as_ptr(&handler)) };
        command.commit();
        self.diagnostics.metal_completion_waits += 1;
        match completion.recv_timeout(COMPLETION_TIMEOUT) {
            Ok(MTLCommandBufferStatus::Completed) => {}
            Ok(MTLCommandBufferStatus::Error) => {
                return Err(submit_error(
                    "blit completion",
                    format!("{:?}", command.error()),
                ));
            }
            outcome => {
                self.quarantine();
                std::mem::forget(source);
                std::mem::forget(destination);
                std::mem::forget(y);
                std::mem::forget(uv);
                std::mem::forget(command);
                return Err(submit_error(
                    "blit completion",
                    format!("{outcome:?}; live parents quarantined"),
                ));
            }
        }
        self.diagnostics.device_to_device_copies += 2;
        drop(source);
        // SAFETY: exact compute and both plane blits have completed on the bound device.
        Ok(unsafe { destination.assume_producer_copy_complete() })
    }
    fn plane(
        &self,
        pixel: &CVPixelBuffer,
        plane: usize,
        format: MTLPixelFormat,
    ) -> Result<CFRetained<CVMetalTexture>, ResidentEncodeSubmissionError> {
        let d = if plane == 0 { 1 } else { 2 };
        let mut ptr = std::ptr::null_mut();
        let result = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.cache,
                pixel,
                None,
                format,
                (self.contract.width / d) as usize,
                (self.contract.height / d) as usize,
                plane,
                NonNull::from(&mut ptr),
            )
        };
        if result != 0 {
            return Err(submit_error("CoreVideo plane", result));
        }
        Ok(unsafe {
            CFRetained::from_raw(
                NonNull::new(ptr).ok_or_else(|| submit_error("CoreVideo plane", "no texture"))?,
            )
        })
    }
    fn quarantine(&mut self) {
        self.poisoned = true;
        std::mem::forget(self.device.clone());
        std::mem::forget(self.queue.clone());
        std::mem::forget(self.buffer.clone());
        std::mem::forget(self.raw_buffer.clone());
        std::mem::forget(self.native_queue.clone());
        std::mem::forget(self.cache.clone());
    }
    /// No-host-boundary evidence, including the one unified bridge allocation.
    pub fn diagnostics(&self) -> MetalResidentEncodeAdapterDiagnostics {
        self.diagnostics
    }
}
fn create_error(stage: &'static str, reason: impl ToString) -> ResidentEncodeAdapterCreateError {
    ResidentEncodeAdapterCreateError::Native {
        backend: "Metal/VideoToolbox",
        stage,
        reason: reason.to_string(),
    }
}
fn submit_error(stage: &'static str, reason: impl ToString) -> ResidentEncodeSubmissionError {
    ResidentEncodeSubmissionError::Native {
        backend: "Metal/VideoToolbox",
        stage,
        reason: reason.to_string(),
    }
}
