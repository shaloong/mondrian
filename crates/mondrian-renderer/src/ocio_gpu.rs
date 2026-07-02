use lru::LruCache;
use mondrian_core::{
    extract_ocio_display_gpu_shader_bundle, extract_ocio_gpu_shader_bundle, ColorSpace,
    GpuLanguage, OcioGpuShaderBundle, OcioGpuTextureChannel, OcioGpuTextureDimensions,
    OcioGpuTextureInterpolation,
};
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Arc;

/// Shader stage used when translating OCIO GPU shader text for wgpu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuShaderStage {
    /// Fragment shader stage.
    Fragment,
}

impl OcioGpuShaderStage {
    fn to_naga(self) -> naga::ShaderStage {
        match self {
            Self::Fragment => naga::ShaderStage::Fragment,
        }
    }
}

/// Target shader language for native wgpu execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuShaderTargetLanguage {
    /// Naga IR consumed by `wgpu::ShaderSource::Naga`.
    NagaIr,
}

/// Renderer-facing OCIO GPU request.
///
/// This is intentionally independent from wgpu resource objects. OCIO emits
/// backend shader source plus texture/uniform payload metadata; the renderer
/// can cache this plan before a backend-specific compiler/upload stage turns it
/// into pipelines and bind groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuShaderRequest {
    /// Convert between two Mondrian color spaces.
    ColorSpace {
        src: ColorSpace,
        dst: ColorSpace,
        language: GpuLanguage,
    },
    /// Convert a source color space through an OCIO display/view transform.
    DisplayView {
        src: ColorSpace,
        display: String,
        view: String,
        language: GpuLanguage,
    },
}

impl Hash for OcioGpuShaderRequest {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::ColorSpace { src, dst, language } => {
                0u8.hash(state);
                src.hash(state);
                dst.hash(state);
                (*language as i32).hash(state);
            }
            Self::DisplayView { src, display, view, language } => {
                1u8.hash(state);
                src.hash(state);
                display.hash(state);
                view.hash(state);
                (*language as i32).hash(state);
            }
        }
    }
}

impl OcioGpuShaderRequest {
    /// Return the shader language requested from OCIO.
    pub fn language(&self) -> GpuLanguage {
        match self {
            Self::ColorSpace { language, .. } | Self::DisplayView { language, .. } => *language,
        }
    }

    /// Human-readable source context for diagnostics.
    pub fn source_label(&self) -> String {
        match self {
            Self::ColorSpace { src, dst, .. } => format!("{src:?}->{dst:?}"),
            Self::DisplayView { src, display, view, .. } => {
                format!("{src:?}->{display}/{view}")
            }
        }
    }
}

/// Cached OCIO GPU shader plan.
#[derive(Debug, Clone)]
pub struct OcioGpuShaderPlan {
    /// Original request used to produce this plan.
    pub request: OcioGpuShaderRequest,
    /// Stable cache key derived from the request and OCIO processor cache id.
    pub cache_key: u64,
    /// OCIO processor cache id, when exposed by OCIO.
    pub processor_cache_id: Option<String>,
    /// OCIO-generated shader source length in bytes.
    pub shader_len: usize,
    /// Stable hash of the OCIO-generated shader source.
    pub shader_hash: u64,
    /// Number of 1D/2D textures referenced by the shader.
    pub texture_2d_count: u32,
    /// Number of 3D textures referenced by the shader.
    pub texture_3d_count: u32,
    /// Number of uniforms referenced by the shader.
    pub uniform_count: u32,
    bundle: Arc<OcioGpuShaderBundle>,
}

impl OcioGpuShaderPlan {
    /// Borrow the full OCIO shader bundle.
    pub fn bundle(&self) -> &OcioGpuShaderBundle {
        &self.bundle
    }
}

/// Request used to translate an OCIO-generated shader into a native wgpu target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuShaderTranslationRequest {
    /// Source shader language emitted by OCIO.
    pub source_language: GpuLanguage,
    /// Target shader language consumed by the renderer.
    pub target_language: OcioGpuShaderTargetLanguage,
    /// Shader stage used for frontend parsing/validation.
    pub stage: OcioGpuShaderStage,
    /// Hash of the OCIO source shader text.
    pub source_shader_hash: u64,
    /// Hash of the OCIO binding contract used with this shader.
    pub binding_contract_hash: u64,
}

impl Hash for OcioGpuShaderTranslationRequest {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.source_language as i32).hash(state);
        self.target_language.hash(state);
        self.stage.hash(state);
        self.source_shader_hash.hash(state);
        self.binding_contract_hash.hash(state);
    }
}

/// A shader translated into a native wgpu artifact.
#[derive(Debug, Clone)]
pub struct OcioGpuTranslatedShader {
    /// Translation request.
    pub request: OcioGpuShaderTranslationRequest,
    /// Canonical Naga module used by `wgpu::ShaderSource::Naga`.
    pub naga_module: naga::Module,
    /// Canonical Naga validation info for the module.
    pub module_info: naga::valid::ModuleInfo,
    /// Optional WGSL debug output. This is not the execution artifact.
    pub debug_wgsl: Option<String>,
    /// Stable hash of the optional WGSL debug output.
    pub debug_wgsl_hash: Option<u64>,
    /// Required OCIO resource binding contract.
    pub required_bindings: OcioGpuBindingContract,
    /// Non-fatal diagnostics observed while building debug artifacts.
    pub diagnostics: Vec<OcioGpuShaderDiagnostic>,
    /// Number of entry points visible after translation.
    pub entry_point_count: usize,
}

/// Non-fatal shader translation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuShaderDiagnostic {
    /// Human-readable diagnostic message.
    pub message: String,
}

/// Resource binding contract paired with an OCIO shader.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuBindingContract {
    /// Descriptor set index reported by OCIO.
    pub descriptor_set_index: u32,
    /// Uniform buffer binding slot reserved by Mondrian's OCIO descriptor policy.
    pub uniform_buffer_binding: u32,
    /// First OCIO texture binding slot.
    pub texture_binding_start: u32,
    /// Packed uniform buffer size in bytes.
    pub uniform_buffer_size: usize,
    /// Number of uniform symbols reported by OCIO.
    pub uniform_count: u32,
    /// OCIO 1D/2D texture resources.
    pub textures_2d: Vec<OcioGpuTexture2DBindingContract>,
    /// OCIO 3D texture resources.
    pub textures_3d: Vec<OcioGpuTexture3DBindingContract>,
}

impl OcioGpuBindingContract {
    /// Stable hash of the contract.
    pub fn stable_hash(&self) -> u64 {
        hash_value(self)
    }
}

/// OCIO 1D/2D texture binding contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuTexture2DBindingContract {
    /// Texture index in the OCIO descriptor.
    pub index: u32,
    /// OCIO-generated texture symbol name.
    pub texture_name: String,
    /// OCIO-generated sampler symbol name.
    pub sampler_name: String,
    /// OCIO-reported binding slot.
    pub binding_index: u32,
    /// Channel packing used by the texture values.
    pub channel: OcioGpuTextureChannel,
    /// Logical dimensionality of this LUT resource.
    pub dimensions: OcioGpuTextureDimensions,
    /// Interpolation policy expected by OCIO.
    pub interpolation: OcioGpuTextureInterpolation,
    /// Logical texture width.
    pub width: u32,
    /// Logical texture height.
    pub height: u32,
    /// Logical texel payload length in f32 values.
    pub value_count: usize,
    /// Stable hash of the copied LUT values.
    pub values_hash: u64,
}

/// OCIO 3D texture binding contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuTexture3DBindingContract {
    /// Texture index in the OCIO descriptor.
    pub index: u32,
    /// OCIO-generated texture symbol name.
    pub texture_name: String,
    /// OCIO-generated sampler symbol name.
    pub sampler_name: String,
    /// OCIO-reported binding slot.
    pub binding_index: u32,
    /// Interpolation policy expected by OCIO.
    pub interpolation: OcioGpuTextureInterpolation,
    /// Cube edge length.
    pub edge_len: u32,
    /// Logical texel payload length in f32 values.
    pub value_count: usize,
    /// Stable hash of the copied LUT values.
    pub values_hash: u64,
}

/// Error returned when shader translation cannot produce a native wgpu shader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuShaderTranslationError {
    /// Translation request that failed.
    pub request: OcioGpuShaderTranslationRequest,
    /// Diagnostic reason.
    pub reason: OcioGpuShaderTranslationFailure,
}

impl std::fmt::Display for OcioGpuShaderTranslationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OCIO GPU shader translation failed: {:?}", self.reason)
    }
}

impl std::error::Error for OcioGpuShaderTranslationError {}

/// Fine-grained shader translation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuShaderTranslationFailure {
    /// The OCIO source language is not supported by the current translator.
    UnsupportedSourceLanguage { language: GpuLanguage },
    /// The requested target shader language is not supported.
    UnsupportedTargetLanguage {
        language: OcioGpuShaderTargetLanguage,
    },
    /// Naga could not parse the source shader.
    ParseFailed { message: String },
    /// Naga validation failed after parsing.
    ValidationFailed { message: String },
}

/// Stateless OCIO shader translator.
pub struct OcioGpuShaderTranslator {
    target_language: OcioGpuShaderTargetLanguage,
    stage: OcioGpuShaderStage,
}

impl OcioGpuShaderTranslator {
    /// Create a translator for wgpu-compatible Naga IR fragment shaders.
    pub fn naga_ir_fragment() -> Self {
        Self {
            target_language: OcioGpuShaderTargetLanguage::NagaIr,
            stage: OcioGpuShaderStage::Fragment,
        }
    }

    /// Translate an extracted OCIO shader plan into the configured target.
    pub fn translate_plan(
        &self,
        plan: &OcioGpuShaderPlan,
    ) -> Result<OcioGpuTranslatedShader, OcioGpuShaderTranslationError> {
        let request = OcioGpuShaderTranslationRequest {
            source_language: plan.request.language(),
            target_language: self.target_language,
            stage: self.stage,
            source_shader_hash: plan.shader_hash,
            binding_contract_hash: binding_contract_for_plan(plan).stable_hash(),
        };

        translate_shader_text(
            request,
            &plan.bundle().shader_text,
            binding_contract_for_plan(plan),
        )
    }
}

impl Default for OcioGpuShaderTranslator {
    fn default() -> Self {
        Self::naga_ir_fragment()
    }
}

/// Bounded cache for OCIO shader translation results.
pub struct OcioGpuShaderTranslationCache {
    translator: OcioGpuShaderTranslator,
    entries: LruCache<u64, Arc<OcioGpuTranslatedShader>>,
    hits: u64,
    misses: u64,
    failures: u64,
}

impl OcioGpuShaderTranslationCache {
    /// Create a translation cache with a fixed non-zero capacity.
    pub fn new(capacity: NonZeroUsize, translator: OcioGpuShaderTranslator) -> Self {
        Self {
            translator,
            entries: LruCache::new(capacity),
            hits: 0,
            misses: 0,
            failures: 0,
        }
    }

    /// Translate a shader plan, using the cache when possible.
    pub fn translate(
        &mut self,
        plan: &OcioGpuShaderPlan,
    ) -> Result<Arc<OcioGpuTranslatedShader>, OcioGpuShaderTranslationError> {
        let request = OcioGpuShaderTranslationRequest {
            source_language: plan.request.language(),
            target_language: self.translator.target_language,
            stage: self.translator.stage,
            source_shader_hash: plan.shader_hash,
            binding_contract_hash: binding_contract_for_plan(plan).stable_hash(),
        };
        let key = hash_value(&request);
        if let Some(hit) = self.entries.get(&key) {
            self.hits = self.hits.saturating_add(1);
            return Ok(Arc::clone(hit));
        }

        self.misses = self.misses.saturating_add(1);
        match self.translator.translate_plan(plan) {
            Ok(translated) => {
                let translated = Arc::new(translated);
                self.entries.put(key, Arc::clone(&translated));
                Ok(translated)
            }
            Err(err) => {
                self.failures = self.failures.saturating_add(1);
                Err(err)
            }
        }
    }

    /// Return translation cache diagnostics.
    pub fn diagnostics(&self) -> OcioGpuShaderTranslationCacheDiagnostics {
        OcioGpuShaderTranslationCacheDiagnostics {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
            failures: self.failures,
        }
    }
}

impl Default for OcioGpuShaderTranslationCache {
    fn default() -> Self {
        Self::new(
            NonZeroUsize::new(64).expect("default cache capacity is non-zero"),
            OcioGpuShaderTranslator::default(),
        )
    }
}

/// Point-in-time OCIO shader translation cache diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuShaderTranslationCacheDiagnostics {
    /// Cached translated shaders.
    pub entries: usize,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Translation failures.
    pub failures: u64,
}

/// Renderer-side preparation result for native wgpu OCIO execution.
#[derive(Debug, Clone)]
pub struct OcioGpuWgpuExecutionPlan {
    /// Cached OCIO shader plan this execution preparation is based on.
    pub shader_plan: Arc<OcioGpuShaderPlan>,
    /// Renderer resource contract required before native wgpu execution.
    pub resources: OcioGpuWgpuResourcePlan,
    /// Native wgpu blockers that must be cleared before this plan can execute.
    pub blockers: Vec<OcioGpuWgpuBlocker>,
}

impl OcioGpuWgpuExecutionPlan {
    /// Whether this plan can be executed by the current native wgpu backend.
    pub fn can_execute(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Fullscreen wrapper contract paired with an OCIO-generated shader program.
///
/// OCIO emits the color transform program and its own LUT/uniform resources.
/// Mondrian still owns the fullscreen fragment wrapper that samples the input
/// frame, calls the OCIO function, and writes the output color.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuFullscreenWrapperContract {
    /// Bind group reserved for Mondrian wrapper resources.
    pub bind_group: u32,
    /// Input frame texture binding in the wrapper bind group.
    pub input_texture_binding: u32,
    /// Input frame sampler binding in the wrapper bind group.
    pub input_sampler_binding: u32,
    /// Fragment output location.
    pub output_location: u32,
}

/// Renderer-side resource contract for a native wgpu OCIO color pass.
///
/// This describes the resources that must be materialized by the render graph.
/// It does not claim those resources have already been created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuWgpuResourcePlan {
    /// Stable key for caching pipeline/resource layout preparation.
    pub resource_key: u64,
    /// Shader hash this resource contract belongs to.
    pub shader_hash: u64,
    /// Stable hash of the OCIO binding contract paired with this shader.
    pub binding_contract_hash: u64,
    /// OCIO resource binding contract reported by the shader descriptor.
    pub binding_contract: OcioGpuBindingContract,
    /// Mondrian fullscreen wrapper contract.
    pub wrapper_contract: OcioGpuFullscreenWrapperContract,
    /// Input frame texture bindings.
    pub input_textures: u32,
    /// Output frame render targets or storage textures.
    pub output_textures: u32,
    /// OCIO 1D/2D LUT texture bindings.
    pub ocio_texture_2d_bindings: u32,
    /// OCIO 3D LUT texture bindings.
    pub ocio_texture_3d_bindings: u32,
    /// Uniform buffers required for OCIO dynamic properties.
    pub uniform_buffers: u32,
    /// Samplers required for input/LUT texture sampling.
    pub samplers: u32,
    /// Bind group entries implied by this resource contract.
    pub bind_group_entries: u32,
    /// Bind groups implied by this resource contract.
    pub bind_groups: u32,
    /// Stable signature for a future wgpu pipeline layout.
    pub pipeline_layout_hash: u64,
}

impl OcioGpuWgpuResourcePlan {
    /// Build a renderer resource contract from a cached OCIO shader plan.
    pub fn for_shader_plan(shader_plan: &OcioGpuShaderPlan) -> Self {
        let binding_contract = binding_contract_for_plan(shader_plan);
        let binding_contract_hash = binding_contract.stable_hash();
        let wrapper_contract = fullscreen_wrapper_contract_for(&binding_contract);
        let wrapper_contract_hash = hash_value(&wrapper_contract);
        let input_textures = 1;
        let output_textures = 1;
        let uniform_buffers =
            u32::from(shader_plan.uniform_count > 0 || binding_contract.uniform_buffer_size > 0);
        let sampled_textures =
            input_textures + shader_plan.texture_2d_count + shader_plan.texture_3d_count;
        let samplers = u32::from(sampled_textures > 0);
        let bind_group_entries =
            shader_plan.texture_2d_count + shader_plan.texture_3d_count + uniform_buffers;
        let bind_groups = binding_contract
            .descriptor_set_index
            .max(wrapper_contract.bind_group)
            .saturating_add(1);
        let pipeline_layout_hash = hash_resource_layout(ResourceLayoutSignature {
            language: shader_plan.request.language(),
            binding_contract_hash,
            wrapper_contract_hash,
            input_textures,
            output_textures,
            texture_2d_count: shader_plan.texture_2d_count,
            texture_3d_count: shader_plan.texture_3d_count,
            uniform_buffers,
            samplers,
            bind_group_entries,
            bind_groups,
        });
        let resource_key = hash_resource_key(
            shader_plan.cache_key,
            shader_plan.shader_hash,
            binding_contract_hash,
            pipeline_layout_hash,
        );
        Self {
            resource_key,
            shader_hash: shader_plan.shader_hash,
            binding_contract_hash,
            binding_contract,
            wrapper_contract,
            input_textures,
            output_textures,
            ocio_texture_2d_bindings: shader_plan.texture_2d_count,
            ocio_texture_3d_bindings: shader_plan.texture_3d_count,
            uniform_buffers,
            samplers,
            bind_group_entries,
            bind_groups,
            pipeline_layout_hash,
        }
    }

    /// Build the bind-group layout contract implied by this resource plan.
    pub fn binding_layout_plan(&self) -> OcioGpuWgpuBindingLayoutPlan {
        OcioGpuWgpuBindingLayoutPlan::for_resource_plan(self)
    }

    /// Total texture resources referenced by this plan.
    pub fn total_textures(&self) -> u32 {
        self.input_textures
            .saturating_add(self.output_textures)
            .saturating_add(self.ocio_texture_2d_bindings)
            .saturating_add(self.ocio_texture_3d_bindings)
    }
}

/// Bind-group layout contract for an OCIO GPU color pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuWgpuBindingLayoutPlan {
    /// Stable resource key this layout belongs to.
    pub resource_key: u64,
    /// Bind group index used by the future OCIO color pass.
    pub bind_group: u32,
    /// Ordered binding entries.
    pub entries: Vec<OcioGpuWgpuBindingPlan>,
    /// Stable hash of the ordered entries.
    pub layout_hash: u64,
}

impl OcioGpuWgpuBindingLayoutPlan {
    fn for_resource_plan(plan: &OcioGpuWgpuResourcePlan) -> Self {
        let mut entries = Vec::with_capacity(plan.bind_group_entries as usize);

        if plan.uniform_buffers > 0 {
            entries.push(OcioGpuWgpuBindingPlan {
                binding: plan.binding_contract.uniform_buffer_binding,
                resource: OcioGpuWgpuBindingResource::OcioUniformBuffer { index: 0 },
            });
        }

        for texture in &plan.binding_contract.textures_2d {
            entries.push(OcioGpuWgpuBindingPlan {
                binding: texture.binding_index,
                resource: OcioGpuWgpuBindingResource::OcioLutTexture2d { index: texture.index },
            });
        }

        for texture in &plan.binding_contract.textures_3d {
            entries.push(OcioGpuWgpuBindingPlan {
                binding: texture.binding_index,
                resource: OcioGpuWgpuBindingResource::OcioLutTexture3d { index: texture.index },
            });
        }

        entries.sort_by_key(|entry| entry.binding);
        let layout_hash = hash_binding_layout(&entries);
        Self {
            resource_key: plan.resource_key,
            bind_group: plan.binding_contract.descriptor_set_index,
            entries,
            layout_hash,
        }
    }
}

/// One binding entry in the future wgpu OCIO bind group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuBindingPlan {
    /// Binding index in the bind group.
    pub binding: u32,
    /// Resource bound at this index.
    pub resource: OcioGpuWgpuBindingResource,
}

/// Resource class for an OCIO GPU color binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuBindingResource {
    /// Source/intermediate frame texture sampled by the color pass.
    InputFrameTexture {
        /// Input texture index.
        index: u32,
    },
    /// Shared filtering sampler for input and LUT sampling.
    FilteringSampler,
    /// OCIO 1D/2D LUT texture.
    OcioLutTexture2d {
        /// LUT texture index from the OCIO shader descriptor.
        index: u32,
    },
    /// OCIO 3D LUT texture.
    OcioLutTexture3d {
        /// LUT texture index from the OCIO shader descriptor.
        index: u32,
    },
    /// OCIO dynamic-property uniform buffer.
    OcioUniformBuffer {
        /// Uniform buffer index from the OCIO shader descriptor.
        index: u32,
    },
}

/// Cached backend-preparation result for an OCIO GPU color pass.
#[derive(Debug, Clone)]
pub struct OcioGpuWgpuPreparedResources {
    /// Resource contract this preparation was derived from.
    pub resources: OcioGpuWgpuResourcePlan,
    /// Bind-group layout contract for backend object creation.
    pub binding_layout: OcioGpuWgpuBindingLayoutPlan,
}

/// Complete LUT payload plan for future wgpu texture uploads.
#[derive(Debug, Clone, PartialEq)]
pub struct OcioGpuWgpuLutUploadPlan {
    /// Stable resource key this upload plan belongs to.
    pub resource_key: u64,
    /// OCIO 1D/2D LUT texture payloads copied as-is.
    pub textures_2d: Vec<OcioGpuWgpuTexture2DUpload>,
    /// OCIO 3D LUT texture payloads copied as-is.
    pub textures_3d: Vec<OcioGpuWgpuTexture3DUpload>,
}

/// Upload payload for an OCIO 1D/2D LUT texture.
#[derive(Debug, Clone, PartialEq)]
pub struct OcioGpuWgpuTexture2DUpload {
    /// Texture index in the OCIO descriptor.
    pub index: u32,
    /// OCIO-generated texture symbol name.
    pub texture_name: String,
    /// OCIO-generated sampler symbol name.
    pub sampler_name: String,
    /// OCIO-reported binding slot.
    pub binding_index: u32,
    /// Channel packing used by the texture values.
    pub channel: OcioGpuTextureChannel,
    /// Logical dimensionality of this LUT resource.
    pub dimensions: OcioGpuTextureDimensions,
    /// Interpolation policy expected by OCIO.
    pub interpolation: OcioGpuTextureInterpolation,
    /// Logical texture width.
    pub width: u32,
    /// Logical texture height.
    pub height: u32,
    /// Stable hash of the copied LUT values.
    pub values_hash: u64,
    /// Flattened texel payload copied from OCIO as-is.
    pub values: Vec<f32>,
}

/// Upload payload for an OCIO 3D LUT texture.
#[derive(Debug, Clone, PartialEq)]
pub struct OcioGpuWgpuTexture3DUpload {
    /// Texture index in the OCIO descriptor.
    pub index: u32,
    /// OCIO-generated texture symbol name.
    pub texture_name: String,
    /// OCIO-generated sampler symbol name.
    pub sampler_name: String,
    /// OCIO-reported binding slot.
    pub binding_index: u32,
    /// Interpolation policy expected by OCIO.
    pub interpolation: OcioGpuTextureInterpolation,
    /// Cube edge length.
    pub edge_len: u32,
    /// Stable hash of the copied LUT values.
    pub values_hash: u64,
    /// Flattened texel payload copied from OCIO as-is.
    pub values: Vec<f32>,
}

impl OcioGpuWgpuLutUploadPlan {
    /// Build a LUT upload plan from the exact OCIO shader bundle payloads.
    pub fn for_shader_plan(
        shader_plan: &OcioGpuShaderPlan,
        resources: &OcioGpuWgpuResourcePlan,
    ) -> Self {
        let textures_2d = shader_plan
            .bundle()
            .textures_2d
            .iter()
            .map(|texture| OcioGpuWgpuTexture2DUpload {
                index: texture.index,
                texture_name: texture.texture_name.clone(),
                sampler_name: texture.sampler_name.clone(),
                binding_index: texture.binding_index,
                channel: texture.channel,
                dimensions: texture.dimensions,
                interpolation: texture.interpolation,
                width: texture.width,
                height: texture.height,
                values_hash: hash_f32_values(&texture.values),
                values: texture.values.clone(),
            })
            .collect();
        let textures_3d = shader_plan
            .bundle()
            .textures_3d
            .iter()
            .map(|texture| OcioGpuWgpuTexture3DUpload {
                index: texture.index,
                texture_name: texture.texture_name.clone(),
                sampler_name: texture.sampler_name.clone(),
                binding_index: texture.binding_index,
                interpolation: texture.interpolation,
                edge_len: texture.edge_len,
                values_hash: hash_f32_values(&texture.values),
                values: texture.values.clone(),
            })
            .collect();
        Self {
            resource_key: resources.resource_key,
            textures_2d,
            textures_3d,
        }
    }
}

/// Bounded cache for renderer backend resource-layout preparation.
pub struct OcioGpuWgpuResourceCache {
    entries: LruCache<u64, Arc<OcioGpuWgpuPreparedResources>>,
    hits: u64,
    misses: u64,
}

impl OcioGpuWgpuResourceCache {
    /// Create a cache with a fixed non-zero capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: LruCache::new(capacity),
            hits: 0,
            misses: 0,
        }
    }

    /// Prepare backend resource-layout contracts from an OCIO resource plan.
    pub fn prepare(
        &mut self,
        resources: OcioGpuWgpuResourcePlan,
    ) -> Arc<OcioGpuWgpuPreparedResources> {
        if let Some(hit) = self.entries.get(&resources.resource_key) {
            self.hits = self.hits.saturating_add(1);
            return Arc::clone(hit);
        }

        self.misses = self.misses.saturating_add(1);
        let prepared = Arc::new(OcioGpuWgpuPreparedResources {
            binding_layout: resources.binding_layout_plan(),
            resources,
        });
        self.entries.put(prepared.resources.resource_key, Arc::clone(&prepared));
        prepared
    }

    /// Return backend resource preparation cache diagnostics.
    pub fn diagnostics(&self) -> OcioGpuWgpuResourceCacheDiagnostics {
        OcioGpuWgpuResourceCacheDiagnostics {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
        }
    }
}

impl Default for OcioGpuWgpuResourceCache {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(64).expect("default cache capacity is non-zero"))
    }
}

/// Point-in-time OCIO GPU backend resource cache diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuWgpuResourceCacheDiagnostics {
    /// Cached backend resource-layout entries.
    pub entries: usize,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
}

/// Cached wgpu shader module produced from a validated Naga OCIO shader.
pub struct OcioGpuWgpuShaderModule {
    /// Stable cache key for this backend shader module.
    pub cache_key: u64,
    /// Source shader hash this module was created from.
    pub shader_hash: u64,
    /// OCIO binding contract hash paired with this module.
    pub binding_contract_hash: u64,
    /// Backend shader module created with `wgpu::ShaderSource::Naga`.
    pub shader_module: wgpu::ShaderModule,
}

/// Error returned before creating an OCIO wgpu shader module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuShaderModuleError {
    /// The translated shader and resource plan do not describe the same source.
    ShaderHashMismatch {
        /// Hash from the translated shader request.
        translated_shader_hash: u64,
        /// Hash from the resource plan.
        resource_shader_hash: u64,
    },
    /// The translated shader and resource plan do not share a binding contract.
    BindingContractMismatch {
        /// Hash from the translated shader request.
        translated_binding_hash: u64,
        /// Hash from the resource plan.
        resource_binding_hash: u64,
    },
    /// The expanded binding contracts differ even though their hashes matched.
    BindingContractPayloadMismatch,
}

/// Point-in-time OCIO wgpu shader-module cache diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuWgpuShaderModuleCacheDiagnostics {
    /// Cached backend shader modules.
    pub entries: usize,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Contract validation failures before module creation.
    pub validation_failures: u64,
}

/// Bounded cache for creating backend shader modules from Naga IR.
pub struct OcioGpuWgpuShaderModuleCache {
    entries: LruCache<u64, Arc<OcioGpuWgpuShaderModule>>,
    hits: u64,
    misses: u64,
    validation_failures: u64,
}

impl OcioGpuWgpuShaderModuleCache {
    /// Create a cache with a fixed non-zero capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: LruCache::new(capacity),
            hits: 0,
            misses: 0,
            validation_failures: 0,
        }
    }

    /// Prepare a wgpu shader module from a validated Naga OCIO shader.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        translated: &OcioGpuTranslatedShader,
        resources: &OcioGpuWgpuResourcePlan,
    ) -> Result<Arc<OcioGpuWgpuShaderModule>, OcioGpuWgpuShaderModuleError> {
        let cache_key = match backend_shader_module_cache_key(translated, resources) {
            Ok(cache_key) => cache_key,
            Err(err) => {
                self.validation_failures = self.validation_failures.saturating_add(1);
                return Err(err);
            }
        };
        if let Some(hit) = self.entries.get(&cache_key) {
            self.hits = self.hits.saturating_add(1);
            return Ok(Arc::clone(hit));
        }

        self.misses = self.misses.saturating_add(1);
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ocio_naga_shader_module"),
            source: wgpu::ShaderSource::Naga(Cow::Owned(translated.naga_module.clone())),
        });
        let module = Arc::new(OcioGpuWgpuShaderModule {
            cache_key,
            shader_hash: resources.shader_hash,
            binding_contract_hash: resources.binding_contract_hash,
            shader_module,
        });
        self.entries.put(cache_key, Arc::clone(&module));
        Ok(module)
    }

    /// Return backend shader-module cache diagnostics.
    pub fn diagnostics(&self) -> OcioGpuWgpuShaderModuleCacheDiagnostics {
        OcioGpuWgpuShaderModuleCacheDiagnostics {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
            validation_failures: self.validation_failures,
        }
    }
}

impl Default for OcioGpuWgpuShaderModuleCache {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(64).expect("default cache capacity is non-zero"))
    }
}

/// Missing pieces before an OCIO GPU shader plan can run in wgpu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuBlocker {
    /// OCIO emitted a language that is not directly consumable by wgpu.
    ShaderLanguageRequiresTranslation { language: GpuLanguage },
    /// OCIO referenced LUT textures that have not yet been uploaded/bound.
    TextureUploadNotImplemented {
        texture_2d_count: u32,
        texture_3d_count: u32,
    },
    /// OCIO referenced uniforms that have not yet been packed into a wgpu buffer.
    UniformUploadNotImplemented { uniform_count: u32 },
}

/// Bounded cache for OCIO GPU shader extraction results.
pub struct OcioGpuShaderCache {
    entries: LruCache<u64, Arc<OcioGpuShaderPlan>>,
    hits: u64,
    misses: u64,
    extraction_failures: u64,
}

impl OcioGpuShaderCache {
    /// Create a cache with a fixed non-zero capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: LruCache::new(capacity),
            hits: 0,
            misses: 0,
            extraction_failures: 0,
        }
    }

    /// Resolve a request to an OCIO GPU shader plan, using the cache when possible.
    pub fn get_or_extract(
        &mut self,
        request: OcioGpuShaderRequest,
    ) -> Result<Arc<OcioGpuShaderPlan>, OcioGpuShaderError> {
        let request_key = request_hash(&request);
        if let Some(hit) = self.entries.get(&request_key) {
            self.hits += 1;
            return Ok(Arc::clone(hit));
        }

        self.misses += 1;
        let bundle = extract_bundle(&request).map_err(|reason| {
            self.extraction_failures += 1;
            OcioGpuShaderError { request: request.clone(), reason }
        })?;
        let plan = Arc::new(plan_from_bundle(request, Arc::new(bundle)));
        self.entries.put(request_key, Arc::clone(&plan));
        Ok(plan)
    }

    /// Prepare an OCIO GPU shader plan for native wgpu execution.
    ///
    /// This currently exposes the exact blockers that keep Mondrian on the CPU
    /// correctness path for OCIO color transforms. The render graph should only
    /// schedule a native pass once `can_execute()` is true.
    pub fn prepare_wgpu_execution(
        &mut self,
        request: OcioGpuShaderRequest,
    ) -> Result<OcioGpuWgpuExecutionPlan, OcioGpuShaderError> {
        let shader_plan = self.get_or_extract(request)?;
        let mut blockers = Vec::new();

        blockers.push(OcioGpuWgpuBlocker::ShaderLanguageRequiresTranslation {
            language: shader_plan.request.language(),
        });

        if shader_plan.texture_2d_count > 0 || shader_plan.texture_3d_count > 0 {
            blockers.push(OcioGpuWgpuBlocker::TextureUploadNotImplemented {
                texture_2d_count: shader_plan.texture_2d_count,
                texture_3d_count: shader_plan.texture_3d_count,
            });
        }
        if shader_plan.uniform_count > 0 {
            blockers.push(OcioGpuWgpuBlocker::UniformUploadNotImplemented {
                uniform_count: shader_plan.uniform_count,
            });
        }

        let resources = OcioGpuWgpuResourcePlan::for_shader_plan(&shader_plan);
        Ok(OcioGpuWgpuExecutionPlan { shader_plan, resources, blockers })
    }

    /// Return cache health counters.
    pub fn diagnostics(&self) -> OcioGpuShaderCacheDiagnostics {
        OcioGpuShaderCacheDiagnostics {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
            extraction_failures: self.extraction_failures,
        }
    }
}

impl Default for OcioGpuShaderCache {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(64).expect("default cache capacity is non-zero"))
    }
}

/// Point-in-time OCIO GPU shader cache diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuShaderCacheDiagnostics {
    /// Cached shader plan entries.
    pub entries: usize,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// OCIO extraction failures.
    pub extraction_failures: u64,
}

/// Error returned when OCIO cannot produce a GPU shader plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuShaderError {
    /// Request that failed.
    pub request: OcioGpuShaderRequest,
    /// Diagnostic reason from the core OCIO layer.
    pub reason: String,
}

impl std::fmt::Display for OcioGpuShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "OCIO GPU shader extraction failed for {}: {}",
            self.request.source_label(),
            self.reason
        )
    }
}

impl std::error::Error for OcioGpuShaderError {}

fn extract_bundle(request: &OcioGpuShaderRequest) -> Result<OcioGpuShaderBundle, String> {
    match request {
        OcioGpuShaderRequest::ColorSpace { src, dst, language } => {
            extract_ocio_gpu_shader_bundle(*src, *dst, *language)
        }
        OcioGpuShaderRequest::DisplayView { src, display, view, language } => {
            extract_ocio_display_gpu_shader_bundle(*src, display, view, *language)
        }
    }
}

fn translate_shader_text(
    request: OcioGpuShaderTranslationRequest,
    shader_text: &str,
    required_bindings: OcioGpuBindingContract,
) -> Result<OcioGpuTranslatedShader, OcioGpuShaderTranslationError> {
    if !is_glsl_language(request.source_language) {
        return Err(translation_error(
            request,
            OcioGpuShaderTranslationFailure::UnsupportedSourceLanguage {
                language: request.source_language,
            },
        ));
    }
    if request.target_language != OcioGpuShaderTargetLanguage::NagaIr {
        return Err(translation_error(
            request,
            OcioGpuShaderTranslationFailure::UnsupportedTargetLanguage {
                language: request.target_language,
            },
        ));
    }

    let mut frontend = naga::front::glsl::Frontend::default();
    let options = naga::front::glsl::Options::from(request.stage.to_naga());
    let module = frontend.parse(&options, shader_text).map_err(|err| {
        translation_error(
            request,
            OcioGpuShaderTranslationFailure::ParseFailed { message: err.to_string() },
        )
    })?;

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    );
    let info = validator.validate(&module).map_err(|err| {
        translation_error(
            request,
            OcioGpuShaderTranslationFailure::ValidationFailed { message: err.to_string() },
        )
    })?;

    let mut diagnostics = Vec::new();
    let debug_wgsl = match naga::back::wgsl::write_string(
        &module,
        &info,
        naga::back::wgsl::WriterFlags::empty(),
    ) {
        Ok(wgsl) => Some(wgsl),
        Err(err) => {
            diagnostics.push(OcioGpuShaderDiagnostic {
                message: format!("WGSL debug output failed: {err}"),
            });
            None
        }
    };
    let debug_wgsl_hash = debug_wgsl.as_ref().map(hash_value);
    let entry_point_count = module.entry_points.len();
    Ok(OcioGpuTranslatedShader {
        request,
        naga_module: module,
        module_info: info,
        debug_wgsl,
        debug_wgsl_hash,
        required_bindings,
        diagnostics,
        entry_point_count,
    })
}

fn translation_error(
    request: OcioGpuShaderTranslationRequest,
    reason: OcioGpuShaderTranslationFailure,
) -> OcioGpuShaderTranslationError {
    OcioGpuShaderTranslationError { request, reason }
}

fn is_glsl_language(language: GpuLanguage) -> bool {
    matches!(
        language,
        GpuLanguage::Glsl1_2
            | GpuLanguage::Glsl1_3
            | GpuLanguage::Glsl4_0
            | GpuLanguage::GlslVk4_6
            | GpuLanguage::GlslEs1_0
            | GpuLanguage::GlslEs3_0
    )
}

fn binding_contract_for_plan(plan: &OcioGpuShaderPlan) -> OcioGpuBindingContract {
    let bundle = plan.bundle();
    OcioGpuBindingContract {
        descriptor_set_index: bundle.descriptor_set_index,
        uniform_buffer_binding: bundle.uniform_buffer_binding,
        texture_binding_start: bundle.texture_binding_start,
        uniform_buffer_size: bundle.uniform_buffer_size,
        uniform_count: bundle.uniform_count,
        textures_2d: bundle
            .textures_2d
            .iter()
            .map(|texture| OcioGpuTexture2DBindingContract {
                index: texture.index,
                texture_name: texture.texture_name.clone(),
                sampler_name: texture.sampler_name.clone(),
                binding_index: texture.binding_index,
                channel: texture.channel,
                dimensions: texture.dimensions,
                interpolation: texture.interpolation,
                width: texture.width,
                height: texture.height,
                value_count: texture.value_count,
                values_hash: hash_f32_values(&texture.values),
            })
            .collect(),
        textures_3d: bundle
            .textures_3d
            .iter()
            .map(|texture| OcioGpuTexture3DBindingContract {
                index: texture.index,
                texture_name: texture.texture_name.clone(),
                sampler_name: texture.sampler_name.clone(),
                binding_index: texture.binding_index,
                interpolation: texture.interpolation,
                edge_len: texture.edge_len,
                value_count: texture.value_count,
                values_hash: hash_f32_values(&texture.values),
            })
            .collect(),
    }
}

fn plan_from_bundle(
    request: OcioGpuShaderRequest,
    bundle: Arc<OcioGpuShaderBundle>,
) -> OcioGpuShaderPlan {
    let shader_hash = hash_value(&bundle.shader_text);
    let cache_key = hash_request_and_processor(&request, bundle.cache_id.as_deref());
    OcioGpuShaderPlan {
        request,
        cache_key,
        processor_cache_id: bundle.cache_id.clone(),
        shader_len: bundle.shader_text.len(),
        shader_hash,
        texture_2d_count: bundle.texture_2d_count,
        texture_3d_count: bundle.texture_3d_count,
        uniform_count: bundle.uniform_count,
        bundle,
    }
}

fn request_hash(request: &OcioGpuShaderRequest) -> u64 {
    hash_value(request)
}

fn hash_request_and_processor(
    request: &OcioGpuShaderRequest,
    processor_cache_id: Option<&str>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    request.hash(&mut hasher);
    processor_cache_id.hash(&mut hasher);
    hasher.finish()
}

fn hash_resource_key(
    shader_cache_key: u64,
    shader_hash: u64,
    binding_contract_hash: u64,
    pipeline_layout_hash: u64,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    shader_cache_key.hash(&mut hasher);
    shader_hash.hash(&mut hasher);
    binding_contract_hash.hash(&mut hasher);
    pipeline_layout_hash.hash(&mut hasher);
    hasher.finish()
}

fn hash_binding_layout(entries: &[OcioGpuWgpuBindingPlan]) -> u64 {
    hash_value(&entries)
}

fn backend_shader_module_cache_key(
    translated: &OcioGpuTranslatedShader,
    resources: &OcioGpuWgpuResourcePlan,
) -> Result<u64, OcioGpuWgpuShaderModuleError> {
    if translated.request.source_shader_hash != resources.shader_hash {
        return Err(OcioGpuWgpuShaderModuleError::ShaderHashMismatch {
            translated_shader_hash: translated.request.source_shader_hash,
            resource_shader_hash: resources.shader_hash,
        });
    }
    if translated.request.binding_contract_hash != resources.binding_contract_hash {
        return Err(OcioGpuWgpuShaderModuleError::BindingContractMismatch {
            translated_binding_hash: translated.request.binding_contract_hash,
            resource_binding_hash: resources.binding_contract_hash,
        });
    }
    if translated.required_bindings != resources.binding_contract {
        return Err(OcioGpuWgpuShaderModuleError::BindingContractPayloadMismatch);
    }

    let mut hasher = DefaultHasher::new();
    translated.request.hash(&mut hasher);
    resources.resource_key.hash(&mut hasher);
    resources.pipeline_layout_hash.hash(&mut hasher);
    Ok(hasher.finish())
}

struct ResourceLayoutSignature {
    language: GpuLanguage,
    binding_contract_hash: u64,
    wrapper_contract_hash: u64,
    input_textures: u32,
    output_textures: u32,
    texture_2d_count: u32,
    texture_3d_count: u32,
    uniform_buffers: u32,
    samplers: u32,
    bind_group_entries: u32,
    bind_groups: u32,
}

fn hash_resource_layout(signature: ResourceLayoutSignature) -> u64 {
    let mut hasher = DefaultHasher::new();
    (signature.language as i32).hash(&mut hasher);
    signature.binding_contract_hash.hash(&mut hasher);
    signature.wrapper_contract_hash.hash(&mut hasher);
    signature.input_textures.hash(&mut hasher);
    signature.output_textures.hash(&mut hasher);
    signature.texture_2d_count.hash(&mut hasher);
    signature.texture_3d_count.hash(&mut hasher);
    signature.uniform_buffers.hash(&mut hasher);
    signature.samplers.hash(&mut hasher);
    signature.bind_group_entries.hash(&mut hasher);
    signature.bind_groups.hash(&mut hasher);
    hasher.finish()
}

fn fullscreen_wrapper_contract_for(
    binding_contract: &OcioGpuBindingContract,
) -> OcioGpuFullscreenWrapperContract {
    OcioGpuFullscreenWrapperContract {
        bind_group: binding_contract.descriptor_set_index.saturating_add(1),
        input_texture_binding: 0,
        input_sampler_binding: 1,
        output_location: 0,
    }
}

fn hash_value<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn hash_f32_values(values: &[f32]) -> u64 {
    let mut hasher = DefaultHasher::new();
    values.len().hash(&mut hasher);
    for value in values {
        value.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ensure_mondrian_default_ocio_loaded, ocio_default_display_view};

    #[test]
    fn cache_extracts_and_reuses_color_space_shader_plan() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let mut cache = OcioGpuShaderCache::default();
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::SLog3,
            dst: ColorSpace::Rec709,
            language: GpuLanguage::Glsl4_0,
        };

        let first = cache.get_or_extract(request.clone()).expect("extract color shader");
        let second = cache.get_or_extract(request).expect("cache color shader");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.shader_len > 0);
        assert_eq!(first.bundle().src_color_space, "S-Log3 S-Gamut3.Cine");
        assert_eq!(first.bundle().dst_color_space, "Camera Rec.709");
        assert!(first.processor_cache_id.as_deref().is_some_and(|id| !id.is_empty()));
        let resources = OcioGpuWgpuResourcePlan::for_shader_plan(&first);
        assert_eq!(resources.input_textures, 1);
        assert_eq!(resources.output_textures, 1);
        assert_eq!(resources.ocio_texture_2d_bindings, first.texture_2d_count);
        assert_eq!(resources.ocio_texture_3d_bindings, first.texture_3d_count);
        assert_eq!(
            resources.uniform_buffers,
            u32::from(first.uniform_count > 0)
        );
        assert_eq!(
            resources.bind_group_entries,
            first.texture_2d_count + first.texture_3d_count + u32::from(first.uniform_count > 0)
        );
        assert_eq!(
            resources.binding_contract_hash,
            resources.binding_contract.stable_hash()
        );
        assert_eq!(
            resources.binding_contract.descriptor_set_index,
            first.bundle().descriptor_set_index
        );
        assert_eq!(
            resources.binding_contract.texture_binding_start,
            first.bundle().texture_binding_start
        );
        assert_eq!(
            resources.wrapper_contract.bind_group,
            resources.binding_contract.descriptor_set_index.saturating_add(1)
        );
        assert_eq!(
            resources.total_textures(),
            2 + first.texture_2d_count + first.texture_3d_count
        );
        assert_ne!(resources.resource_key, 0);
        assert_ne!(resources.pipeline_layout_hash, 0);
        let binding_layout = resources.binding_layout_plan();
        assert_eq!(
            binding_layout.entries.len(),
            resources.bind_group_entries as usize
        );
        assert_eq!(
            binding_layout.bind_group,
            resources.binding_contract.descriptor_set_index
        );
        for texture in &first.bundle().textures_2d {
            assert!(binding_layout.entries.iter().any(|entry| {
                entry.binding == texture.binding_index
                    && entry.resource
                        == OcioGpuWgpuBindingResource::OcioLutTexture2d { index: texture.index }
            }));
            let contract = resources
                .binding_contract
                .textures_2d
                .iter()
                .find(|contract| contract.index == texture.index)
                .expect("2D LUT binding contract");
            assert_eq!(contract.channel, texture.channel);
            assert_eq!(contract.dimensions, texture.dimensions);
            assert_eq!(contract.interpolation, texture.interpolation);
            assert_eq!(contract.value_count, texture.values.len());
            assert_eq!(contract.values_hash, hash_f32_values(&texture.values));
        }
        for texture in &first.bundle().textures_3d {
            assert!(binding_layout.entries.iter().any(|entry| {
                entry.binding == texture.binding_index
                    && entry.resource
                        == OcioGpuWgpuBindingResource::OcioLutTexture3d { index: texture.index }
            }));
            let contract = resources
                .binding_contract
                .textures_3d
                .iter()
                .find(|contract| contract.index == texture.index)
                .expect("3D LUT binding contract");
            assert_eq!(contract.interpolation, texture.interpolation);
            assert_eq!(contract.value_count, texture.values.len());
            assert_eq!(contract.values_hash, hash_f32_values(&texture.values));
        }
        assert_ne!(binding_layout.layout_hash, 0);
        let upload_plan = OcioGpuWgpuLutUploadPlan::for_shader_plan(&first, &resources);
        assert_eq!(upload_plan.resource_key, resources.resource_key);
        assert_eq!(upload_plan.textures_2d.len() as u32, first.texture_2d_count);
        assert_eq!(upload_plan.textures_3d.len() as u32, first.texture_3d_count);
        for upload in &upload_plan.textures_2d {
            assert_eq!(upload.values_hash, hash_f32_values(&upload.values));
            assert_eq!(
                upload.values_hash,
                resources
                    .binding_contract
                    .textures_2d
                    .iter()
                    .find(|contract| contract.index == upload.index)
                    .expect("2D upload contract")
                    .values_hash
            );
        }
        for upload in &upload_plan.textures_3d {
            assert_eq!(upload.values_hash, hash_f32_values(&upload.values));
            assert_eq!(
                upload.values_hash,
                resources
                    .binding_contract
                    .textures_3d
                    .iter()
                    .find(|contract| contract.index == upload.index)
                    .expect("3D upload contract")
                    .values_hash
            );
        }

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.extraction_failures, 0);
    }

    #[test]
    fn cache_extracts_display_view_shader_plan() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let (display, view) = ocio_default_display_view().expect("default display/view");
        let mut cache = OcioGpuShaderCache::default();

        let plan = cache
            .get_or_extract(OcioGpuShaderRequest::DisplayView {
                src: ColorSpace::Rec709,
                display: display.clone(),
                view: view.clone(),
                language: GpuLanguage::Glsl4_0,
            })
            .expect("extract display shader");

        assert!(plan.shader_len > 0);
        assert_eq!(plan.bundle().src_color_space, "Camera Rec.709");
        assert_eq!(plan.bundle().dst_color_space, format!("{display}/{view}"));
        assert_eq!(plan.bundle().language, GpuLanguage::Glsl4_0);
        assert!(plan.shader_hash != 0);
    }

    #[test]
    fn wgpu_execution_preparation_reports_native_blockers_without_fallback() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let mut cache = OcioGpuShaderCache::default();

        let prepared = cache
            .prepare_wgpu_execution(OcioGpuShaderRequest::ColorSpace {
                src: ColorSpace::AppleLog,
                dst: ColorSpace::Rec709,
                language: GpuLanguage::Glsl4_0,
            })
            .expect("prepare wgpu execution");

        assert!(!prepared.can_execute());
        assert_eq!(prepared.resources.input_textures, 1);
        assert_eq!(prepared.resources.output_textures, 1);
        assert_eq!(
            prepared.resources.ocio_texture_2d_bindings,
            prepared.shader_plan.texture_2d_count
        );
        assert_eq!(
            prepared.resources.ocio_texture_3d_bindings,
            prepared.shader_plan.texture_3d_count
        );
        assert_eq!(
            prepared.resources.uniform_buffers,
            u32::from(prepared.shader_plan.uniform_count > 0)
        );
        assert!(prepared.blockers.iter().any(|blocker| matches!(
            blocker,
            OcioGpuWgpuBlocker::ShaderLanguageRequiresTranslation {
                language: GpuLanguage::Glsl4_0
            }
        )));

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.extraction_failures, 0);
    }

    #[test]
    fn wgpu_resource_cache_reuses_prepared_binding_layouts() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let mut shader_cache = OcioGpuShaderCache::default();
        let prepared = shader_cache
            .prepare_wgpu_execution(OcioGpuShaderRequest::ColorSpace {
                src: ColorSpace::SLog3,
                dst: ColorSpace::Rec709,
                language: GpuLanguage::Glsl4_0,
            })
            .expect("prepare shader execution");

        let mut resource_cache = OcioGpuWgpuResourceCache::default();
        let first = resource_cache.prepare(prepared.resources.clone());
        let second = resource_cache.prepare(prepared.resources.clone());

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            first.resources.resource_key,
            prepared.resources.resource_key
        );
        assert_eq!(
            first.binding_layout.entries.len(),
            first.resources.bind_group_entries as usize
        );
        assert_eq!(
            first.binding_layout.layout_hash,
            prepared.resources.binding_layout_plan().layout_hash
        );

        let diagnostics = resource_cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.misses, 1);
    }

    #[test]
    fn shader_translation_cache_reuses_valid_glsl_to_naga_ir_translation() {
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::Rec709,
            dst: ColorSpace::Srgb,
            language: GpuLanguage::Glsl4_0,
        };
        let bundle = Arc::new(OcioGpuShaderBundle {
            src_color_space: "test-src".to_owned(),
            dst_color_space: "test-dst".to_owned(),
            language: GpuLanguage::Glsl4_0,
            shader_text: r#"
                #version 450 core
                layout(location = 0) out vec4 frag_color;

                void main() {
                    frag_color = vec4(1.0, 0.5, 0.25, 1.0);
                }
            "#
            .to_owned(),
            descriptor_set_index: 0,
            texture_binding_start: 1,
            uniform_buffer_binding: 0,
            uniform_buffer_size: 0,
            texture_2d_count: 0,
            texture_3d_count: 0,
            uniform_count: 0,
            textures_2d: Vec::new(),
            textures_3d: Vec::new(),
            cache_id: Some("test-cache".to_owned()),
        });
        let plan = plan_from_bundle(request, bundle);
        let mut cache = OcioGpuShaderTranslationCache::default();

        let first = cache.translate(&plan).expect("translate GLSL to Naga IR");
        let second = cache.translate(&plan).expect("reuse translated Naga IR");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.naga_module.entry_points.len(), 1);
        assert!(first.debug_wgsl.as_deref().is_some_and(|wgsl| wgsl.contains("@fragment")));
        assert_eq!(first.entry_point_count, 1);
        assert_eq!(first.request.source_language, GpuLanguage::Glsl4_0);
        assert_eq!(first.required_bindings.descriptor_set_index, 0);
        assert_eq!(first.required_bindings.uniform_buffer_binding, 0);
        assert_eq!(first.required_bindings.texture_binding_start, 1);
        assert_eq!(
            first.request.binding_contract_hash,
            first.required_bindings.stable_hash()
        );
        assert_ne!(first.debug_wgsl_hash.unwrap_or_default(), 0);

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.failures, 0);
    }

    #[test]
    fn backend_shader_module_cache_key_rejects_contract_mismatch() {
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::Rec709,
            dst: ColorSpace::Srgb,
            language: GpuLanguage::Glsl4_0,
        };
        let bundle = Arc::new(OcioGpuShaderBundle {
            src_color_space: "test-src".to_owned(),
            dst_color_space: "test-dst".to_owned(),
            language: GpuLanguage::Glsl4_0,
            shader_text: r#"
                #version 450 core
                layout(location = 0) out vec4 frag_color;

                void main() {
                    frag_color = vec4(1.0, 0.5, 0.25, 1.0);
                }
            "#
            .to_owned(),
            descriptor_set_index: 0,
            texture_binding_start: 1,
            uniform_buffer_binding: 0,
            uniform_buffer_size: 0,
            texture_2d_count: 0,
            texture_3d_count: 0,
            uniform_count: 0,
            textures_2d: Vec::new(),
            textures_3d: Vec::new(),
            cache_id: Some("test-cache".to_owned()),
        });
        let plan = plan_from_bundle(request, bundle);
        let translated = OcioGpuShaderTranslator::default()
            .translate_plan(&plan)
            .expect("translate GLSL to Naga IR");
        let resources = OcioGpuWgpuResourcePlan::for_shader_plan(&plan);
        assert_ne!(
            backend_shader_module_cache_key(&translated, &resources).expect("matching contract"),
            0
        );

        let mut mismatched_resources = resources.clone();
        mismatched_resources.binding_contract.texture_binding_start =
            mismatched_resources.binding_contract.texture_binding_start.saturating_add(1);
        assert!(matches!(
            backend_shader_module_cache_key(&translated, &mismatched_resources),
            Err(OcioGpuWgpuShaderModuleError::BindingContractPayloadMismatch)
        ));

        let mut mismatched_hash = resources;
        mismatched_hash.binding_contract_hash =
            mismatched_hash.binding_contract_hash.wrapping_add(1);
        assert!(matches!(
            backend_shader_module_cache_key(&translated, &mismatched_hash),
            Err(OcioGpuWgpuShaderModuleError::BindingContractMismatch { .. })
        ));
    }

    #[test]
    fn shader_translation_rejects_non_glsl_source_without_panic() {
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::Rec709,
            dst: ColorSpace::Srgb,
            language: GpuLanguage::HlslSm5_0,
        };
        let bundle = Arc::new(OcioGpuShaderBundle {
            src_color_space: "test-src".to_owned(),
            dst_color_space: "test-dst".to_owned(),
            language: GpuLanguage::HlslSm5_0,
            shader_text: "float4 main() : SV_Target { return float4(1, 1, 1, 1); }".to_owned(),
            descriptor_set_index: 0,
            texture_binding_start: 1,
            uniform_buffer_binding: 0,
            uniform_buffer_size: 0,
            texture_2d_count: 0,
            texture_3d_count: 0,
            uniform_count: 0,
            textures_2d: Vec::new(),
            textures_3d: Vec::new(),
            cache_id: Some("test-hlsl-cache".to_owned()),
        });
        let plan = plan_from_bundle(request, bundle);
        let mut cache = OcioGpuShaderTranslationCache::default();

        let err = cache.translate(&plan).expect_err("HLSL is not translated by this path");
        assert!(matches!(
            err.reason,
            OcioGpuShaderTranslationFailure::UnsupportedSourceLanguage {
                language: GpuLanguage::HlslSm5_0
            }
        ));

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.failures, 1);
    }

    #[test]
    fn actual_ocio_shader_translation_is_structured_success_or_failure() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let mut shader_cache = OcioGpuShaderCache::default();
        let plan = shader_cache
            .get_or_extract(OcioGpuShaderRequest::ColorSpace {
                src: ColorSpace::SLog3,
                dst: ColorSpace::Rec709,
                language: GpuLanguage::Glsl4_0,
            })
            .expect("extract OCIO shader");
        let mut translation_cache = OcioGpuShaderTranslationCache::default();

        match translation_cache.translate(&plan) {
            Ok(translated) => {
                assert_eq!(translated.request.source_shader_hash, plan.shader_hash);
                assert_eq!(
                    translated.request.binding_contract_hash,
                    translated.required_bindings.stable_hash()
                );
                assert_eq!(
                    translated.required_bindings.descriptor_set_index,
                    plan.bundle().descriptor_set_index
                );
                assert_eq!(
                    translated.required_bindings.texture_binding_start,
                    plan.bundle().texture_binding_start
                );
                assert_eq!(
                    translated.naga_module.entry_points.len(),
                    translated.entry_point_count
                );
                let diagnostics = translation_cache.diagnostics();
                assert_eq!(diagnostics.entries, 1);
                assert_eq!(diagnostics.failures, 0);
            }
            Err(err) => {
                assert_eq!(err.request.source_shader_hash, plan.shader_hash);
                assert!(matches!(
                    err.reason,
                    OcioGpuShaderTranslationFailure::ParseFailed { .. }
                        | OcioGpuShaderTranslationFailure::ValidationFailed { .. }
                ));
                let diagnostics = translation_cache.diagnostics();
                assert_eq!(diagnostics.entries, 0);
                assert_eq!(diagnostics.failures, 1);
            }
        }
    }

    #[test]
    fn actual_ocio_glsl_vk_shader_translation_is_structured_success_or_failure() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let mut shader_cache = OcioGpuShaderCache::default();
        let plan = shader_cache
            .get_or_extract(OcioGpuShaderRequest::ColorSpace {
                src: ColorSpace::SLog3,
                dst: ColorSpace::Rec709,
                language: GpuLanguage::GlslVk4_6,
            })
            .expect("extract OCIO GLSL VK shader");
        let mut translation_cache = OcioGpuShaderTranslationCache::default();

        match translation_cache.translate(&plan) {
            Ok(translated) => {
                assert_eq!(translated.request.source_language, GpuLanguage::GlslVk4_6);
                assert_eq!(
                    translated.request.binding_contract_hash,
                    translated.required_bindings.stable_hash()
                );
                assert_eq!(
                    translated.naga_module.entry_points.len(),
                    translated.entry_point_count
                );
            }
            Err(err) => {
                assert_eq!(err.request.source_language, GpuLanguage::GlslVk4_6);
                assert_eq!(err.request.source_shader_hash, plan.shader_hash);
                assert!(matches!(
                    err.reason,
                    OcioGpuShaderTranslationFailure::ParseFailed { .. }
                        | OcioGpuShaderTranslationFailure::ValidationFailed { .. }
                ));
                let diagnostics = translation_cache.diagnostics();
                assert_eq!(diagnostics.entries, 0);
                assert_eq!(diagnostics.failures, 1);
            }
        }
    }
}
