//! Demand-driven Program Output video scopes recorded entirely on the GPU.
//!
//! The runtime consumes display-encoded Program Output before monitor
//! adaptation. Atomic buffers preserve exact per-frame counts; a second compute
//! pass turns those counts into sampleable display textures without a normal
//! CPU readback path.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use mondrian_core::{ColorSpace, ProgramColorScopeError, ProgramSignalColorimetry, WaveformMode};

const VECTOR_GRID: u32 = 64;
const HISTOGRAM_HEIGHT: u32 = 128;
const VECTOR_TEXTURE_SIZE: u32 = 256;
const WORKGROUP_SIZE: u32 = 16;
const DISPLAY_WORKGROUP_SIZE: u32 = 8;

/// Bounded configuration for one GPU Program Output scope request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuProgramScopesRequest {
    signal_color_space: ColorSpace,
    waveform_mode: WaveformMode,
    bins: u32,
    waveform_width: u32,
}

impl GpuProgramScopesRequest {
    /// Construct a request, bounding density dimensions to predictable GPU
    /// memory and dispatch costs.
    pub fn new(
        signal_color_space: ColorSpace,
        waveform_mode: WaveformMode,
        bins: u32,
        waveform_width: u32,
    ) -> Result<Self, ProgramColorScopeError> {
        ProgramSignalColorimetry::for_color_space(signal_color_space)?;
        Ok(Self {
            signal_color_space,
            waveform_mode,
            bins: bins.clamp(16, 1024),
            waveform_width: waveform_width.clamp(64, 1024),
        })
    }

    /// Encoded Program Output identity measured by this request.
    pub const fn signal_color_space(self) -> ColorSpace {
        self.signal_color_space
    }

    /// Requested waveform component layout.
    pub const fn waveform_mode(self) -> WaveformMode {
        self.waveform_mode
    }

    /// Number of signal bins.
    pub const fn bins(self) -> u32 {
        self.bins
    }

    /// Number of horizontally aggregated waveform columns.
    pub const fn waveform_width(self) -> u32 {
        self.waveform_width
    }

    const fn waveform_channels(self) -> u32 {
        match self.waveform_mode {
            WaveformMode::Luma => 1,
            WaveformMode::RgbParade => 3,
        }
    }
}

impl Default for GpuProgramScopesRequest {
    fn default() -> Self {
        Self {
            signal_color_space: ColorSpace::Rec709,
            waveform_mode: WaveformMode::Luma,
            bins: 256,
            waveform_width: 512,
        }
    }
}

/// Stable element offsets within the exact atomic-count buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuProgramScopesBufferLayout {
    /// First histogram counter; planes are R, G, B, then luma.
    pub histogram_offset: u32,
    /// First waveform counter.
    pub waveform_offset: u32,
    /// First 64x64 vectorscope counter.
    pub vectorscope_offset: u32,
    /// Total u32 counter elements in the buffer.
    pub counter_count: u32,
}

impl GpuProgramScopesBufferLayout {
    const EXCURSION_COUNTERS: u32 = 8;

    fn for_request(request: GpuProgramScopesRequest) -> Self {
        let histogram_offset = Self::EXCURSION_COUNTERS;
        let waveform_offset = histogram_offset + request.bins * 4;
        let vectorscope_offset =
            waveform_offset + request.waveform_width * request.bins * request.waveform_channels();
        Self {
            histogram_offset,
            waveform_offset,
            vectorscope_offset,
            counter_count: vectorscope_offset + VECTOR_GRID * VECTOR_GRID,
        }
    }

    const fn byte_size(self) -> u64 {
        self.counter_count as u64 * std::mem::size_of::<u32>() as u64
    }
}

/// GPU textures and exact count-buffer layout produced for one frame.
pub struct GpuProgramScopesRecord {
    /// Request that defined the aggregation geometry and signal coefficients.
    pub request: GpuProgramScopesRequest,
    /// Exact atomic count-buffer layout retained by the runtime.
    pub buffer_layout: GpuProgramScopesBufferLayout,
    /// RGB histogram visualization (`bins x 128`, RGBA8 linear carrier).
    pub histogram_view: wgpu::TextureView,
    /// Waveform visualization (`waveform_width x bins`, RGBA8 linear carrier).
    pub waveform_view: wgpu::TextureView,
    /// Vectorscope visualization (`256 x 256`, RGBA8 linear carrier).
    pub vectorscope_view: wgpu::TextureView,
    _counts: Arc<wgpu::Buffer>,
}

impl GpuProgramScopesRecord {
    #[cfg(test)]
    fn counts(&self) -> &wgpu::Buffer {
        self._counts.as_ref()
    }
}

/// Allocation and recording evidence for the retained scopes runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuProgramScopesRuntimeDiagnostics {
    /// Pipeline bundles created for this device.
    pub pipeline_creations: u64,
    /// Exact-count buffers allocated after request-shape changes.
    pub buffer_allocations: u64,
    /// Display textures allocated after request-shape changes.
    pub texture_allocations: u64,
    /// Frames for which aggregation and visualization were recorded.
    pub frames_recorded: u64,
}

/// Retained, device-local Program Output scopes executor.
#[derive(Default)]
pub struct GpuProgramScopesRuntime {
    pipelines: Option<ScopesPipelines>,
    resources: Option<ScopesResources>,
    diagnostics: GpuProgramScopesRuntimeDiagnostics,
}

impl GpuProgramScopesRuntime {
    /// Current evidence for pipeline/resource reuse and demand-driven work.
    pub const fn diagnostics(&self) -> GpuProgramScopesRuntimeDiagnostics {
        self.diagnostics
    }

    /// Release device resources after a device transition.
    pub fn clear(&mut self) {
        self.pipelines = None;
        self.resources = None;
    }

    /// Record exact aggregation and GPU visualization for one Program Output.
    ///
    /// Callers represent hidden-panel behavior by not calling this method; the
    /// runtime performs no allocation, clear, upload, or dispatch on its own.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input_view: &wgpu::TextureView,
        input_width: u32,
        input_height: u32,
        request: GpuProgramScopesRequest,
    ) -> Result<GpuProgramScopesRecord, GpuProgramScopesError> {
        if input_width == 0 || input_height == 0 {
            return Err(GpuProgramScopesError::InvalidDimensions {
                width: input_width,
                height: input_height,
            });
        }
        let colorimetry = ProgramSignalColorimetry::for_color_space(request.signal_color_space)
            .map_err(GpuProgramScopesError::UnsupportedSignal)?;
        if self.pipelines.is_none() {
            self.pipelines = Some(ScopesPipelines::new(device));
            self.diagnostics.pipeline_creations =
                self.diagnostics.pipeline_creations.saturating_add(1);
        }
        let key = ScopesResourceKey { request };
        if self.resources.as_ref().is_none_or(|resources| resources.key != key) {
            let display_layout = &self
                .pipelines
                .as_ref()
                .ok_or(GpuProgramScopesError::InternalState)?
                .display_bind_group_layout;
            let resources = ScopesResources::new(device, key, display_layout);
            self.resources = Some(resources);
            self.diagnostics.buffer_allocations =
                self.diagnostics.buffer_allocations.saturating_add(1);
            self.diagnostics.texture_allocations =
                self.diagnostics.texture_allocations.saturating_add(3);
        }
        let pipelines = self.pipelines.as_ref().ok_or(GpuProgramScopesError::InternalState)?;
        let resources = self.resources.as_ref().ok_or(GpuProgramScopesError::InternalState)?;
        let max_column_samples =
            input_height.saturating_mul(input_width.div_ceil(request.waveform_width)).max(1);
        let uniforms = ScopesUniforms {
            input_width,
            input_height,
            bins: request.bins,
            waveform_width: request.waveform_width,
            waveform_channels: request.waveform_channels(),
            histogram_offset: resources.layout.histogram_offset,
            waveform_offset: resources.layout.waveform_offset,
            vectorscope_offset: resources.layout.vectorscope_offset,
            kr: colorimetry.kr(),
            kb: colorimetry.kb(),
            sample_count: input_width.saturating_mul(input_height) as f32,
            max_column_samples: max_column_samples as f32,
        };
        queue.write_buffer(&resources.uniforms, 0, bytemuck::bytes_of(&uniforms));
        encoder.clear_buffer(resources.counts.as_ref(), 0, None);

        let aggregate_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("program-scopes-aggregate-bind-group"),
            layout: &pipelines.aggregate_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: resources.counts.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: resources.uniforms.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("program-scopes-aggregate-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipelines.aggregate_pipeline);
            pass.set_bind_group(0, &aggregate_bind_group, &[]);
            pass.dispatch_workgroups(
                input_width.div_ceil(WORKGROUP_SIZE),
                input_height.div_ceil(WORKGROUP_SIZE),
                1,
            );
        }

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("program-scopes-display-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipelines.display_pipeline);
            pass.set_bind_group(0, &resources.display_bind_group, &[]);
            pass.dispatch_workgroups(
                request.waveform_width.max(VECTOR_TEXTURE_SIZE).div_ceil(DISPLAY_WORKGROUP_SIZE),
                request.bins.max(VECTOR_TEXTURE_SIZE).div_ceil(DISPLAY_WORKGROUP_SIZE),
                3,
            );
        }
        self.diagnostics.frames_recorded = self.diagnostics.frames_recorded.saturating_add(1);
        Ok(GpuProgramScopesRecord {
            request,
            buffer_layout: resources.layout,
            histogram_view: resources.histogram_view.clone(),
            waveform_view: resources.waveform_view.clone(),
            vectorscope_view: resources.vectorscope_view.clone(),
            _counts: Arc::clone(&resources.counts),
        })
    }
}

/// Failures at the renderer-owned GPU scopes boundary.
#[derive(Debug, thiserror::Error)]
pub enum GpuProgramScopesError {
    /// Source dimensions must describe a real Program Output texture.
    #[error("GPU Program Output scopes require non-zero dimensions, got {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },
    /// The requested identity is not an encoded signal color space.
    #[error("unsupported GPU Program Output scope signal: {0}")]
    UnsupportedSignal(ProgramColorScopeError),
    /// Retained runtime initialization failed unexpectedly.
    #[error("GPU Program Output scopes runtime entered an invalid internal state")]
    InternalState,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct ScopesUniforms {
    input_width: u32,
    input_height: u32,
    bins: u32,
    waveform_width: u32,
    waveform_channels: u32,
    histogram_offset: u32,
    waveform_offset: u32,
    vectorscope_offset: u32,
    kr: f32,
    kb: f32,
    sample_count: f32,
    max_column_samples: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScopesResourceKey {
    request: GpuProgramScopesRequest,
}

struct ScopesResources {
    key: ScopesResourceKey,
    layout: GpuProgramScopesBufferLayout,
    counts: Arc<wgpu::Buffer>,
    uniforms: wgpu::Buffer,
    _histogram_texture: wgpu::Texture,
    histogram_view: wgpu::TextureView,
    _waveform_texture: wgpu::Texture,
    waveform_view: wgpu::TextureView,
    _vectorscope_texture: wgpu::Texture,
    vectorscope_view: wgpu::TextureView,
    display_bind_group: wgpu::BindGroup,
}

impl ScopesResources {
    fn new(
        device: &wgpu::Device,
        key: ScopesResourceKey,
        display_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let layout = GpuProgramScopesBufferLayout::for_request(key.request);
        let counts = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("program-scopes-exact-counts"),
            size: layout.byte_size(),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("program-scopes-uniforms"),
            size: std::mem::size_of::<ScopesUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let (_histogram_texture, histogram_view) = create_display_texture(
            device,
            "program-scopes-histogram",
            key.request.bins,
            HISTOGRAM_HEIGHT,
        );
        let (_waveform_texture, waveform_view) = create_display_texture(
            device,
            "program-scopes-waveform",
            key.request.waveform_width,
            key.request.bins,
        );
        let (_vectorscope_texture, vectorscope_view) = create_display_texture(
            device,
            "program-scopes-vectorscope",
            VECTOR_TEXTURE_SIZE,
            VECTOR_TEXTURE_SIZE,
        );
        let display_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("program-scopes-display-bind-group"),
            layout: display_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: counts.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: uniforms.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&histogram_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&waveform_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&vectorscope_view),
                },
            ],
        });
        Self {
            key,
            layout,
            counts,
            uniforms,
            _histogram_texture,
            histogram_view,
            _waveform_texture,
            waveform_view,
            _vectorscope_texture,
            vectorscope_view,
            display_bind_group,
        }
    }
}

fn create_display_texture(
    device: &wgpu::Device,
    label: &'static str,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

struct ScopesPipelines {
    aggregate_bind_group_layout: wgpu::BindGroupLayout,
    display_bind_group_layout: wgpu::BindGroupLayout,
    aggregate_pipeline: wgpu::ComputePipeline,
    display_pipeline: wgpu::ComputePipeline,
}

impl ScopesPipelines {
    fn new(device: &wgpu::Device) -> Self {
        let aggregate_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("program-scopes-aggregate-layout"),
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
                    storage_buffer_layout_entry(1),
                    uniform_buffer_layout_entry(2),
                ],
            });
        let display_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("program-scopes-display-layout"),
                entries: &[
                    storage_buffer_layout_entry(0),
                    uniform_buffer_layout_entry(1),
                    storage_texture_layout_entry(2),
                    storage_texture_layout_entry(3),
                    storage_texture_layout_entry(4),
                ],
            });
        let aggregate_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("program-scopes-aggregate-shader"),
            source: wgpu::ShaderSource::Wgsl(AGGREGATE_SHADER.into()),
        });
        let display_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("program-scopes-display-shader"),
            source: wgpu::ShaderSource::Wgsl(DISPLAY_SHADER.into()),
        });
        let aggregate_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("program-scopes-aggregate-pipeline-layout"),
                bind_group_layouts: &[Some(&aggregate_bind_group_layout)],
                immediate_size: 0,
            });
        let display_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("program-scopes-display-pipeline-layout"),
                bind_group_layouts: &[Some(&display_bind_group_layout)],
                immediate_size: 0,
            });
        let aggregate_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("program-scopes-aggregate-pipeline"),
            layout: Some(&aggregate_pipeline_layout),
            module: &aggregate_module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let display_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("program-scopes-display-pipeline"),
            layout: Some(&display_pipeline_layout),
            module: &display_module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            aggregate_bind_group_layout,
            display_bind_group_layout,
            aggregate_pipeline,
            display_pipeline,
        }
    }
}

fn storage_buffer_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_buffer_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba8Unorm,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

const AGGREGATE_SHADER: &str = r#"
struct Uniforms {
    input_width: u32,
    input_height: u32,
    bins: u32,
    waveform_width: u32,
    waveform_channels: u32,
    histogram_offset: u32,
    waveform_offset: u32,
    vectorscope_offset: u32,
    kr: f32,
    kb: f32,
    sample_count: f32,
    max_column_samples: f32,
};

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> counts: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

fn signal_bin(value: f32) -> u32 {
    return min(u32(clamp(value, 0.0, 1.0) * f32(uniforms.bins)), uniforms.bins - 1u);
}

fn observe_excursion(value: f32, low_offset: u32, high_offset: u32) {
    if value < 0.0 {
        atomicAdd(&counts[low_offset], 1u);
    } else if value > 1.0 {
        atomicAdd(&counts[high_offset], 1u);
    }
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= uniforms.input_width || gid.y >= uniforms.input_height {
        return;
    }
    let rgb = textureLoad(source, vec2<i32>(gid.xy), 0).rgb;
    if any(rgb != rgb) {
        return;
    }
    let kg = 1.0 - uniforms.kr - uniforms.kb;
    let y = uniforms.kr * rgb.r + kg * rgb.g + uniforms.kb * rgb.b;
    observe_excursion(rgb.r, 0u, 1u);
    observe_excursion(rgb.g, 2u, 3u);
    observe_excursion(rgb.b, 4u, 5u);
    observe_excursion(y, 6u, 7u);

    let rb = signal_bin(rgb.r);
    let gb = signal_bin(rgb.g);
    let bb = signal_bin(rgb.b);
    let yb = signal_bin(y);
    atomicAdd(&counts[uniforms.histogram_offset + rb], 1u);
    atomicAdd(&counts[uniforms.histogram_offset + uniforms.bins + gb], 1u);
    atomicAdd(&counts[uniforms.histogram_offset + 2u * uniforms.bins + bb], 1u);
    atomicAdd(&counts[uniforms.histogram_offset + 3u * uniforms.bins + yb], 1u);

    let wx = min(gid.x * uniforms.waveform_width / uniforms.input_width, uniforms.waveform_width - 1u);
    let plane = uniforms.waveform_width * uniforms.bins;
    if uniforms.waveform_channels == 1u {
        atomicAdd(&counts[uniforms.waveform_offset + wx * uniforms.bins + yb], 1u);
    } else {
        atomicAdd(&counts[uniforms.waveform_offset + wx * uniforms.bins + rb], 1u);
        atomicAdd(&counts[uniforms.waveform_offset + plane + wx * uniforms.bins + gb], 1u);
        atomicAdd(&counts[uniforms.waveform_offset + 2u * plane + wx * uniforms.bins + bb], 1u);
    }

    let u = (rgb.b - y) / (2.0 * (1.0 - uniforms.kb));
    let v = (rgb.r - y) / (2.0 * (1.0 - uniforms.kr));
    let ux = min(u32(clamp(u + 0.5, 0.0, 0.999999) * 64.0), 63u);
    let vy = min(u32(clamp(v + 0.5, 0.0, 0.999999) * 64.0), 63u);
    atomicAdd(&counts[uniforms.vectorscope_offset + vy * 64u + ux], 1u);
}
"#;

const DISPLAY_SHADER: &str = r#"
struct Uniforms {
    input_width: u32,
    input_height: u32,
    bins: u32,
    waveform_width: u32,
    waveform_channels: u32,
    histogram_offset: u32,
    waveform_offset: u32,
    vectorscope_offset: u32,
    kr: f32,
    kb: f32,
    sample_count: f32,
    max_column_samples: f32,
};

@group(0) @binding(0) var<storage, read_write> counts: array<atomic<u32>>;
@group(0) @binding(1) var<uniform> uniforms: Uniforms;
@group(0) @binding(2) var histogram_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(3) var waveform_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(4) var vectorscope_out: texture_storage_2d<rgba8unorm, write>;

fn log_density(count: u32, maximum: f32) -> f32 {
    return clamp(log2(1.0 + f32(count)) / log2(2.0 + maximum), 0.0, 1.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.z == 0u {
        if gid.x >= uniforms.bins || gid.y >= 128u { return; }
        let r = log_density(atomicLoad(&counts[uniforms.histogram_offset + gid.x]), uniforms.sample_count);
        let g = log_density(atomicLoad(&counts[uniforms.histogram_offset + uniforms.bins + gid.x]), uniforms.sample_count);
        let b = log_density(atomicLoad(&counts[uniforms.histogram_offset + 2u * uniforms.bins + gid.x]), uniforms.sample_count);
        let threshold = 1.0 - (f32(gid.y) + 0.5) / 128.0;
        textureStore(histogram_out, vec2<i32>(gid.xy), vec4<f32>(select(0.0, 1.0, r >= threshold), select(0.0, 1.0, g >= threshold), select(0.0, 1.0, b >= threshold), 1.0));
        return;
    }
    if gid.z == 1u {
        if gid.x >= uniforms.waveform_width || gid.y >= uniforms.bins { return; }
        let bin = uniforms.bins - 1u - gid.y;
        let base = uniforms.waveform_offset + gid.x * uniforms.bins + bin;
        if uniforms.waveform_channels == 1u {
            let d = log_density(atomicLoad(&counts[base]), uniforms.max_column_samples);
            textureStore(waveform_out, vec2<i32>(gid.xy), vec4<f32>(d, d, d, 1.0));
        } else {
            let plane = uniforms.waveform_width * uniforms.bins;
            let r = log_density(atomicLoad(&counts[base]), uniforms.max_column_samples);
            let g = log_density(atomicLoad(&counts[base + plane]), uniforms.max_column_samples);
            let b = log_density(atomicLoad(&counts[base + 2u * plane]), uniforms.max_column_samples);
            textureStore(waveform_out, vec2<i32>(gid.xy), vec4<f32>(r, g, b, 1.0));
        }
        return;
    }
    if gid.x >= 256u || gid.y >= 256u { return; }
    let cell = (gid.y / 4u) * 64u + gid.x / 4u;
    let d = log_density(atomicLoad(&counts[uniforms.vectorscope_offset + cell]), uniforms.sample_count);
    let center = gid.x == 127u || gid.x == 128u || gid.y == 127u || gid.y == 128u;
    let guide = select(0.0, 0.12, center);
    textureStore(vectorscope_out, vec2<i32>(gid.xy), vec4<f32>(guide + d * 0.25, guide + d, guide + d * 0.55, 1.0));
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GpuContext;
    use mondrian_core::compute_program_color_scopes_rgba8;

    #[test]
    fn buffer_layout_is_compact_and_non_overlapping() {
        let request =
            GpuProgramScopesRequest::new(ColorSpace::Rec709, WaveformMode::RgbParade, 256, 512)
                .expect("scope request");
        let layout = GpuProgramScopesBufferLayout::for_request(request);

        assert_eq!(layout.histogram_offset, 8);
        assert_eq!(layout.waveform_offset, 8 + 4 * 256);
        assert_eq!(layout.vectorscope_offset, 8 + 4 * 256 + 3 * 512 * 256);
        assert_eq!(layout.counter_count, layout.vectorscope_offset + 64 * 64);
        assert!(layout.byte_size() < 2 * 1024 * 1024);
    }

    #[test]
    fn request_rejects_working_spaces_and_bounds_density() {
        assert!(GpuProgramScopesRequest::new(
            ColorSpace::LinearRec709,
            WaveformMode::Luma,
            256,
            512,
        )
        .is_err());
        let bounded =
            GpuProgramScopesRequest::new(ColorSpace::Rec2100Pq, WaveformMode::Luma, 1, u32::MAX)
                .expect("PQ is an encoded signal");
        assert_eq!(bounded.bins(), 16);
        assert_eq!(bounded.waveform_width(), 1024);
    }

    #[tokio::test]
    async fn gpu_atomic_histograms_match_cpu_reference_without_frame_readback() {
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping GPU scopes test: no adapter available");
            return;
        };
        let pixels = vec![
            0, 0, 0, 255, 255, 255, 255, 255, 255, 0, 0, 255, 128, 128, 128, 255,
        ];
        let input = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("program-scopes-test-input"),
            size: wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: Some(2),
            },
            wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
        );
        let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());
        let request = GpuProgramScopesRequest::new(ColorSpace::Rec709, WaveformMode::Luma, 16, 64)
            .expect("scope request");
        let mut runtime = GpuProgramScopesRuntime::default();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("program-scopes-test-encoder"),
        });
        let record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                &input_view,
                2,
                2,
                request,
            )
            .expect("GPU scopes record");
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("program-scopes-test-readback"),
            size: record.buffer_layout.byte_size(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(
            record.counts(),
            0,
            &readback,
            0,
            record.buffer_layout.byte_size(),
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let bytes = map_test_readback(&context.device, &readback);
        let actual = bytemuck::cast_slice::<u8, u32>(&bytes);
        let cpu = compute_program_color_scopes_rgba8(
            &pixels,
            2,
            2,
            ColorSpace::Rec709,
            WaveformMode::Luma,
            16,
        )
        .expect("CPU scopes reference");
        let histogram = record.buffer_layout.histogram_offset as usize;
        assert_eq!(
            &actual[histogram..histogram + 16],
            cpu.histogram.red.as_slice()
        );
        assert_eq!(
            &actual[histogram + 16..histogram + 32],
            cpu.histogram.green.as_slice()
        );
        assert_eq!(
            &actual[histogram + 32..histogram + 48],
            cpu.histogram.blue.as_slice()
        );
        assert_eq!(
            &actual[histogram + 48..histogram + 64],
            cpu.histogram.luma.as_slice()
        );
        let waveform = record.buffer_layout.waveform_offset as usize;
        let vectorscope = record.buffer_layout.vectorscope_offset as usize;
        assert_eq!(
            actual[waveform..vectorscope].iter().copied().sum::<u32>(),
            4
        );
        assert_eq!(actual[vectorscope..].iter().copied().sum::<u32>(), 4);
        assert_eq!(runtime.diagnostics().pipeline_creations, 1);
        assert_eq!(runtime.diagnostics().buffer_allocations, 1);
        assert_eq!(runtime.diagnostics().texture_allocations, 3);
        assert_eq!(runtime.diagnostics().frames_recorded, 1);
    }

    fn map_test_readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        receiver.recv().expect("readback callback").expect("readback map");
        slice.get_mapped_range().expect("mapped readback").to_vec()
    }
}
