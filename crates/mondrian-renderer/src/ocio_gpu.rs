use lru::LruCache;
use mondrian_core::{
    extract_ocio_display_gpu_shader_bundle, extract_ocio_gpu_shader_bundle, ColorSpace,
    GpuLanguage, OcioGpuShaderBundle, OcioGpuTextureChannel, OcioGpuTextureDimensions,
    OcioGpuTextureInterpolation, OcioGpuUniformType, OcioGpuUniformValue,
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
    /// OCIO uniform resources.
    pub uniforms: Vec<OcioGpuUniformBindingContract>,
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

/// OCIO uniform binding contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuUniformBindingContract {
    /// Uniform index in the OCIO descriptor.
    pub index: u32,
    /// Uniform symbol name used in emitted shader code.
    pub name: String,
    /// OCIO-reported uniform type.
    pub uniform_type: OcioGpuUniformType,
    /// Byte offset into OCIO's packed uniform buffer layout.
    pub buffer_offset: usize,
    /// Logical scalar count for this payload.
    pub value_count: usize,
    /// Stable hash of the copied uniform payload.
    pub value_hash: u64,
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
        let lut_texture_count = shader_plan.texture_2d_count + shader_plan.texture_3d_count;
        let samplers = input_textures + lut_texture_count;
        let bind_group_entries = uniform_buffers + lut_texture_count.saturating_mul(2);
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
    pub fn binding_layout_plan(
        &self,
    ) -> Result<OcioGpuWgpuBindingLayoutPlan, OcioGpuWgpuBindingLayoutPlanError> {
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
    /// Policy used to derive separated LUT sampler bindings.
    pub sampler_policy: OcioGpuWgpuSamplerBindingPolicy,
    /// Stable hash of the ordered entries.
    pub layout_hash: u64,
}

impl OcioGpuWgpuBindingLayoutPlan {
    fn for_resource_plan(
        plan: &OcioGpuWgpuResourcePlan,
    ) -> Result<Self, OcioGpuWgpuBindingLayoutPlanError> {
        let mut entries = Vec::with_capacity(plan.bind_group_entries as usize);
        let sampler_policy = OcioGpuWgpuSamplerBindingPolicy::for_contract(&plan.binding_contract)?;

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
            entries.push(OcioGpuWgpuBindingPlan {
                binding: sampler_policy.sampler_binding_for_texture(
                    OcioGpuWgpuLutTextureDimension::D2,
                    texture.index,
                )?,
                resource: OcioGpuWgpuBindingResource::OcioLutSampler2d { index: texture.index },
            });
        }

        for texture in &plan.binding_contract.textures_3d {
            entries.push(OcioGpuWgpuBindingPlan {
                binding: texture.binding_index,
                resource: OcioGpuWgpuBindingResource::OcioLutTexture3d { index: texture.index },
            });
            entries.push(OcioGpuWgpuBindingPlan {
                binding: sampler_policy.sampler_binding_for_texture(
                    OcioGpuWgpuLutTextureDimension::D3,
                    texture.index,
                )?,
                resource: OcioGpuWgpuBindingResource::OcioLutSampler3d { index: texture.index },
            });
        }

        entries.sort_by_key(|entry| entry.binding);
        validate_unique_bindings(&entries)?;
        let layout_hash = hash_binding_layout(&entries);
        Ok(Self {
            resource_key: plan.resource_key,
            bind_group: plan.binding_contract.descriptor_set_index,
            entries,
            sampler_policy,
            layout_hash,
        })
    }
}

/// Error returned when a wgpu bind-group layout plan cannot be built safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuBindingLayoutPlanError {
    /// A binding index overflowed while deriving separated sampler bindings.
    BindingIndexOverflow {
        /// First binding used for the derived range.
        start: u32,
        /// Number of bindings requested from the range.
        count: usize,
    },
    /// A derived binding collides with another resource binding.
    BindingCollision { binding: u32 },
    /// No sampler binding was found for a required LUT texture.
    MissingSamplerBinding {
        /// Texture dimension.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Texture index from the OCIO shader descriptor.
        index: u32,
    },
}

/// Deterministic policy for separating OCIO texture/sampler symbols in wgpu.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuSamplerBindingPolicy {
    /// First binding reserved for derived LUT samplers.
    pub sampler_binding_start: u32,
    /// Per-texture sampler bindings.
    pub mappings: Vec<OcioGpuWgpuSamplerBinding>,
}

impl OcioGpuWgpuSamplerBindingPolicy {
    /// Derive sampler bindings from the OCIO descriptor contract.
    pub fn for_contract(
        contract: &OcioGpuBindingContract,
    ) -> Result<Self, OcioGpuWgpuBindingLayoutPlanError> {
        let texture_count = contract.textures_2d.len().saturating_add(contract.textures_3d.len());
        let sampler_binding_start = first_free_binding_after_ocio_resources(contract)?;
        let _exclusive_end =
            checked_binding_offset(sampler_binding_start, texture_count, texture_count)?;

        let mut mappings = Vec::with_capacity(texture_count);
        for (offset, texture) in contract.textures_2d.iter().enumerate() {
            let sampler_binding =
                checked_binding_offset(sampler_binding_start, offset, texture_count)?;
            mappings.push(OcioGpuWgpuSamplerBinding {
                texture_index: texture.index,
                texture_dimension: OcioGpuWgpuLutTextureDimension::D2,
                texture_binding: texture.binding_index,
                sampler_binding,
                sampler_name: texture.sampler_name.clone(),
                interpolation: texture.interpolation,
            });
        }
        let texture_2d_count = contract.textures_2d.len();
        for (offset, texture) in contract.textures_3d.iter().enumerate() {
            let sampler_binding = checked_binding_offset(
                sampler_binding_start,
                texture_2d_count.saturating_add(offset),
                texture_count,
            )?;
            mappings.push(OcioGpuWgpuSamplerBinding {
                texture_index: texture.index,
                texture_dimension: OcioGpuWgpuLutTextureDimension::D3,
                texture_binding: texture.binding_index,
                sampler_binding,
                sampler_name: texture.sampler_name.clone(),
                interpolation: texture.interpolation,
            });
        }

        Ok(Self { sampler_binding_start, mappings })
    }

    fn sampler_binding_for_texture(
        &self,
        dimension: OcioGpuWgpuLutTextureDimension,
        index: u32,
    ) -> Result<u32, OcioGpuWgpuBindingLayoutPlanError> {
        self.mappings
            .iter()
            .find(|mapping| {
                mapping.texture_dimension == dimension && mapping.texture_index == index
            })
            .map(|mapping| mapping.sampler_binding)
            .ok_or(OcioGpuWgpuBindingLayoutPlanError::MissingSamplerBinding { dimension, index })
    }
}

/// One derived sampler binding for an OCIO LUT texture.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuSamplerBinding {
    /// Texture index from the OCIO shader descriptor.
    pub texture_index: u32,
    /// Texture dimensionality.
    pub texture_dimension: OcioGpuWgpuLutTextureDimension,
    /// OCIO-reported texture binding.
    pub texture_binding: u32,
    /// Derived wgpu sampler binding.
    pub sampler_binding: u32,
    /// OCIO-generated sampler symbol name.
    pub sampler_name: String,
    /// OCIO interpolation policy that the wrapper shader must honor.
    pub interpolation: OcioGpuTextureInterpolation,
}

/// Backend bind-group layout descriptor plan for wgpu object creation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuBindGroupLayoutDescriptorPlan {
    /// Bind group index in the future pipeline layout.
    pub bind_group: u32,
    /// Human-readable layout label.
    pub label: String,
    /// Ordered layout entries.
    pub entries: Vec<OcioGpuWgpuBindGroupLayoutEntryPlan>,
    /// Stable hash of the descriptor plan.
    pub layout_hash: u64,
}

impl OcioGpuWgpuBindGroupLayoutDescriptorPlan {
    /// Build the OCIO resource bind-group layout descriptor.
    pub fn for_ocio_resources(layout: &OcioGpuWgpuBindingLayoutPlan) -> Self {
        let entries = layout
            .entries
            .iter()
            .map(|entry| OcioGpuWgpuBindGroupLayoutEntryPlan {
                binding: entry.binding,
                visibility: OcioGpuWgpuShaderVisibility::Fragment,
                resource: OcioGpuWgpuLayoutBindingResource::from_ocio_binding_resource(
                    entry.resource,
                ),
            })
            .collect::<Vec<_>>();
        let layout_hash = hash_value(&entries);
        Self {
            bind_group: layout.bind_group,
            label: "ocio_resource_bind_group_layout".to_owned(),
            entries,
            layout_hash,
        }
    }

    /// Build the Mondrian fullscreen wrapper bind-group layout descriptor.
    pub fn for_wrapper_input(layout: &OcioGpuWgpuWrapperBindingPlan) -> Self {
        let entries = layout
            .entries
            .iter()
            .map(|entry| OcioGpuWgpuBindGroupLayoutEntryPlan {
                binding: entry.binding,
                visibility: OcioGpuWgpuShaderVisibility::Fragment,
                resource: OcioGpuWgpuLayoutBindingResource::from_wrapper_binding_resource(
                    entry.resource,
                ),
            })
            .collect::<Vec<_>>();
        let layout_hash = hash_value(&entries);
        Self {
            bind_group: layout.bind_group,
            label: "ocio_wrapper_input_bind_group_layout".to_owned(),
            entries,
            layout_hash,
        }
    }

    /// Create a concrete wgpu bind-group layout from this validated plan.
    pub fn create_bind_group_layout(&self, device: &wgpu::Device) -> wgpu::BindGroupLayout {
        let entries = self.entries.iter().map(|entry| entry.to_wgpu()).collect::<Vec<_>>();
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&self.label),
            entries: &entries,
        })
    }
}

/// One backend bind-group layout entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuBindGroupLayoutEntryPlan {
    /// Binding index.
    pub binding: u32,
    /// Shader stages allowed to access this resource.
    pub visibility: OcioGpuWgpuShaderVisibility,
    /// Binding resource type.
    pub resource: OcioGpuWgpuLayoutBindingResource,
}

impl OcioGpuWgpuBindGroupLayoutEntryPlan {
    fn to_wgpu(self) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding: self.binding,
            visibility: self.visibility.to_wgpu(),
            ty: self.resource.to_wgpu(),
            count: None,
        }
    }
}

/// Shader visibility for an OCIO backend layout entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuShaderVisibility {
    /// Fragment shader visibility.
    Fragment,
}

impl OcioGpuWgpuShaderVisibility {
    fn to_wgpu(self) -> wgpu::ShaderStages {
        match self {
            Self::Fragment => wgpu::ShaderStages::FRAGMENT,
        }
    }
}

/// Resource binding type for a backend layout entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuLayoutBindingResource {
    /// Uniform buffer binding.
    UniformBuffer { min_binding_size: Option<u64> },
    /// Sampled texture binding.
    SampledTexture {
        dimension: OcioGpuWgpuLutTextureDimension,
        sample_type: OcioGpuWgpuTextureSampleType,
    },
    /// Sampler binding.
    Sampler {
        filtering: OcioGpuWgpuSamplerFiltering,
    },
}

impl OcioGpuWgpuLayoutBindingResource {
    fn from_ocio_binding_resource(resource: OcioGpuWgpuBindingResource) -> Self {
        match resource {
            OcioGpuWgpuBindingResource::OcioUniformBuffer { .. } => {
                Self::UniformBuffer { min_binding_size: None }
            }
            OcioGpuWgpuBindingResource::OcioLutTexture2d { .. } => Self::SampledTexture {
                dimension: OcioGpuWgpuLutTextureDimension::D2,
                sample_type: OcioGpuWgpuTextureSampleType::Float32,
            },
            OcioGpuWgpuBindingResource::OcioLutTexture3d { .. } => Self::SampledTexture {
                dimension: OcioGpuWgpuLutTextureDimension::D3,
                sample_type: OcioGpuWgpuTextureSampleType::Float32,
            },
            OcioGpuWgpuBindingResource::OcioLutSampler2d { .. }
            | OcioGpuWgpuBindingResource::OcioLutSampler3d { .. } => Self::Sampler {
                filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
            },
            OcioGpuWgpuBindingResource::InputFrameTexture { .. } => Self::SampledTexture {
                dimension: OcioGpuWgpuLutTextureDimension::D2,
                sample_type: OcioGpuWgpuTextureSampleType::Float32,
            },
            OcioGpuWgpuBindingResource::FilteringSampler => {
                Self::Sampler { filtering: OcioGpuWgpuSamplerFiltering::Filtering }
            }
        }
    }

    fn from_wrapper_binding_resource(resource: OcioGpuWgpuWrapperBindingResource) -> Self {
        match resource {
            OcioGpuWgpuWrapperBindingResource::InputFrameTexture => Self::SampledTexture {
                dimension: OcioGpuWgpuLutTextureDimension::D2,
                sample_type: OcioGpuWgpuTextureSampleType::Float32,
            },
            OcioGpuWgpuWrapperBindingResource::InputFrameSampler => {
                Self::Sampler { filtering: OcioGpuWgpuSamplerFiltering::Filtering }
            }
        }
    }

    fn to_wgpu(self) -> wgpu::BindingType {
        match self {
            Self::UniformBuffer { min_binding_size } => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: min_binding_size.and_then(wgpu::BufferSize::new),
            },
            Self::SampledTexture { dimension, sample_type } => wgpu::BindingType::Texture {
                sample_type: sample_type.to_wgpu(),
                view_dimension: dimension.view_dimension(),
                multisampled: false,
            },
            Self::Sampler { filtering } => wgpu::BindingType::Sampler(filtering.to_wgpu()),
        }
    }
}

/// Texture sample type used in an OCIO backend layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuTextureSampleType {
    /// 32-bit float sampled texture. This is non-filterable without device features.
    Float32,
}

impl OcioGpuWgpuTextureSampleType {
    fn to_wgpu(self) -> wgpu::TextureSampleType {
        match self {
            Self::Float32 => wgpu::TextureSampleType::Float { filterable: false },
        }
    }
}

/// Sampler filtering mode used in an OCIO backend layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuSamplerFiltering {
    /// Filtering sampler.
    Filtering,
    /// Non-filtering sampler. Required for portable 32-bit float LUT textures.
    NonFiltering,
}

impl OcioGpuWgpuSamplerFiltering {
    fn to_wgpu(self) -> wgpu::SamplerBindingType {
        match self {
            Self::Filtering => wgpu::SamplerBindingType::Filtering,
            Self::NonFiltering => wgpu::SamplerBindingType::NonFiltering,
        }
    }
}

/// Validated resource plan for the future OCIO bind group.
///
/// This is the bridge between packed/uploaded OCIO resources and concrete
/// `wgpu::BindGroup` creation. It deliberately stays data-only because OCIO's
/// GLSL contract reports combined texture/sampler symbols while wgpu requires
/// separated texture-view and sampler bindings. The renderer must keep that
/// separation explicit instead of guessing sampler slots from generated shader
/// text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuWgpuBindResourcePlan {
    /// Stable resource key this bind-resource plan belongs to.
    pub resource_key: u64,
    /// OCIO descriptor set used by LUT/uniform resources.
    pub ocio_bind_group: u32,
    /// Mondrian wrapper bind group used by input frame resources.
    pub wrapper_bind_group: u32,
    /// Hash of the OCIO bind-group layout contract.
    pub ocio_layout_hash: u64,
    /// Wrapper input binding contract.
    pub wrapper_layout: OcioGpuWgpuWrapperBindingPlan,
    /// Ordered OCIO resource entries validated against packed payloads.
    pub ocio_entries: Vec<OcioGpuWgpuBindResourceEntry>,
    /// Stable hash of all bind-resource entries and wrapper bindings.
    pub plan_hash: u64,
}

impl OcioGpuWgpuBindResourcePlan {
    /// Validate packed LUT/uniform payloads against a renderer resource plan.
    pub fn from_packed_resources(
        resources: &OcioGpuWgpuResourcePlan,
        packed_luts: &OcioGpuWgpuPackedLutUploadPlan,
        packed_uniform: Option<&OcioGpuWgpuPackedUniformBuffer>,
    ) -> Result<Self, OcioGpuWgpuBindResourcePlanError> {
        if resources.resource_key != packed_luts.resource_key {
            return Err(OcioGpuWgpuBindResourcePlanError::LutResourceKeyMismatch {
                expected: resources.resource_key,
                actual: packed_luts.resource_key,
            });
        }
        let binding_layout = resources
            .binding_layout_plan()
            .map_err(OcioGpuWgpuBindResourcePlanError::BindingLayout)?;
        let sampler_policy = &binding_layout.sampler_policy;

        let mut ocio_entries = Vec::new();
        if resources.uniform_buffers > 0 {
            let uniform =
                packed_uniform.ok_or(OcioGpuWgpuBindResourcePlanError::MissingUniformBuffer {
                    binding: resources.binding_contract.uniform_buffer_binding,
                })?;
            validate_uniform_bind_resource(resources, uniform)?;
            ocio_entries.push(OcioGpuWgpuBindResourceEntry {
                binding: uniform.binding,
                resource: OcioGpuWgpuBindResource::UniformBuffer {
                    byte_len: uniform.byte_len,
                    bytes_hash: uniform.bytes_hash,
                },
            });
        } else if let Some(uniform) = packed_uniform {
            return Err(OcioGpuWgpuBindResourcePlanError::UnexpectedUniformBuffer {
                binding: uniform.binding,
            });
        }

        if packed_luts.textures_2d.len() != resources.binding_contract.textures_2d.len() {
            return Err(OcioGpuWgpuBindResourcePlanError::TextureCountMismatch {
                dimension: OcioGpuWgpuLutTextureDimension::D2,
                expected: resources.binding_contract.textures_2d.len(),
                actual: packed_luts.textures_2d.len(),
            });
        }
        if packed_luts.textures_3d.len() != resources.binding_contract.textures_3d.len() {
            return Err(OcioGpuWgpuBindResourcePlanError::TextureCountMismatch {
                dimension: OcioGpuWgpuLutTextureDimension::D3,
                expected: resources.binding_contract.textures_3d.len(),
                actual: packed_luts.textures_3d.len(),
            });
        }

        for contract in &resources.binding_contract.textures_2d {
            let texture = packed_luts
                .textures_2d
                .iter()
                .find(|texture| texture.index == contract.index)
                .ok_or(OcioGpuWgpuBindResourcePlanError::MissingTexture2D {
                    index: contract.index,
                })?;
            validate_texture_2d_bind_resource(contract, texture)?;
            let sampler_binding = sampler_policy
                .sampler_binding_for_texture(OcioGpuWgpuLutTextureDimension::D2, texture.index)?;
            ocio_entries.push(OcioGpuWgpuBindResourceEntry {
                binding: texture.binding_index,
                resource: OcioGpuWgpuBindResource::LutTexture {
                    index: texture.index,
                    texture_name: texture.texture_name.clone(),
                    sampler_name: texture.sampler_name.clone(),
                    dimension: texture.dimension,
                    format: texture.format,
                    extent: texture.extent,
                    source_values_hash: texture.source_values_hash,
                    packed_bytes_hash: texture.packed_bytes_hash,
                },
            });
            ocio_entries.push(OcioGpuWgpuBindResourceEntry {
                binding: sampler_binding,
                resource: OcioGpuWgpuBindResource::LutSampler {
                    index: texture.index,
                    sampler_name: texture.sampler_name.clone(),
                    interpolation: texture.interpolation,
                    filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
                },
            });
        }

        for contract in &resources.binding_contract.textures_3d {
            let texture = packed_luts
                .textures_3d
                .iter()
                .find(|texture| texture.index == contract.index)
                .ok_or(OcioGpuWgpuBindResourcePlanError::MissingTexture3D {
                    index: contract.index,
                })?;
            validate_texture_3d_bind_resource(contract, texture)?;
            let sampler_binding = sampler_policy
                .sampler_binding_for_texture(OcioGpuWgpuLutTextureDimension::D3, texture.index)?;
            ocio_entries.push(OcioGpuWgpuBindResourceEntry {
                binding: texture.binding_index,
                resource: OcioGpuWgpuBindResource::LutTexture {
                    index: texture.index,
                    texture_name: texture.texture_name.clone(),
                    sampler_name: texture.sampler_name.clone(),
                    dimension: texture.dimension,
                    format: texture.format,
                    extent: texture.extent,
                    source_values_hash: texture.source_values_hash,
                    packed_bytes_hash: texture.packed_bytes_hash,
                },
            });
            ocio_entries.push(OcioGpuWgpuBindResourceEntry {
                binding: sampler_binding,
                resource: OcioGpuWgpuBindResource::LutSampler {
                    index: texture.index,
                    sampler_name: texture.sampler_name.clone(),
                    interpolation: texture.interpolation,
                    filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
                },
            });
        }

        ocio_entries.sort_by_key(|entry| entry.binding);
        let wrapper_layout =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&resources.wrapper_contract);
        let ocio_layout_hash = binding_layout.layout_hash;
        let plan_hash = hash_bind_resource_plan(
            resources.resource_key,
            resources.binding_contract.descriptor_set_index,
            wrapper_layout.layout_hash,
            ocio_layout_hash,
            &ocio_entries,
        );
        Ok(Self {
            resource_key: resources.resource_key,
            ocio_bind_group: resources.binding_contract.descriptor_set_index,
            wrapper_bind_group: resources.wrapper_contract.bind_group,
            ocio_layout_hash,
            wrapper_layout,
            ocio_entries,
            plan_hash,
        })
    }
}

/// Wrapper bind-group contract for the fullscreen OCIO pass.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuWrapperBindingPlan {
    /// Mondrian wrapper bind group index.
    pub bind_group: u32,
    /// Ordered wrapper resource bindings.
    pub entries: Vec<OcioGpuWgpuWrapperBindingEntry>,
    /// Stable hash of the wrapper bindings.
    pub layout_hash: u64,
}

impl OcioGpuWgpuWrapperBindingPlan {
    /// Build the wrapper binding plan from the fullscreen wrapper contract.
    pub fn for_contract(contract: &OcioGpuFullscreenWrapperContract) -> Self {
        let mut entries = vec![
            OcioGpuWgpuWrapperBindingEntry {
                binding: contract.input_texture_binding,
                resource: OcioGpuWgpuWrapperBindingResource::InputFrameTexture,
            },
            OcioGpuWgpuWrapperBindingEntry {
                binding: contract.input_sampler_binding,
                resource: OcioGpuWgpuWrapperBindingResource::InputFrameSampler,
            },
        ];
        entries.sort_by_key(|entry| entry.binding);
        let layout_hash = hash_value(&entries);
        Self {
            bind_group: contract.bind_group,
            entries,
            layout_hash,
        }
    }
}

/// One wrapper bind-group entry for the fullscreen OCIO pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuWrapperBindingEntry {
    /// Binding index in the wrapper bind group.
    pub binding: u32,
    /// Resource bound at this index.
    pub resource: OcioGpuWgpuWrapperBindingResource,
}

/// Resource class for a fullscreen wrapper binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuWrapperBindingResource {
    /// GPU-resident input frame texture.
    InputFrameTexture,
    /// Sampler used to sample the input frame texture.
    InputFrameSampler,
}

/// One validated OCIO bind-resource entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuBindResourceEntry {
    /// OCIO binding index.
    pub binding: u32,
    /// Resource bound at this index.
    pub resource: OcioGpuWgpuBindResource,
}

/// Validated resource payload used by an OCIO bind-resource entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuBindResource {
    /// Packed OCIO uniform buffer.
    UniformBuffer {
        /// Buffer length in bytes.
        byte_len: usize,
        /// Stable hash of packed bytes.
        bytes_hash: u64,
    },
    /// Packed OCIO LUT texture payload.
    LutTexture {
        /// Texture index from the OCIO shader descriptor.
        index: u32,
        /// OCIO-generated texture symbol name.
        texture_name: String,
        /// OCIO-generated sampler symbol name.
        sampler_name: String,
        /// Texture dimension selected for wgpu upload.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Texture format selected for wgpu upload.
        format: OcioGpuWgpuLutTextureFormat,
        /// Texture extent.
        extent: OcioGpuWgpuLutTextureExtent,
        /// Hash of source OCIO values.
        source_values_hash: u64,
        /// Hash of packed texture bytes.
        packed_bytes_hash: u64,
    },
    /// Sampler paired with an OCIO LUT texture.
    LutSampler {
        /// Texture index from the OCIO shader descriptor.
        index: u32,
        /// OCIO-generated sampler symbol name.
        sampler_name: String,
        /// OCIO interpolation policy that the wrapper shader must honor.
        interpolation: OcioGpuTextureInterpolation,
        /// Backend sampler filtering mode.
        filtering: OcioGpuWgpuSamplerFiltering,
    },
}

/// Error returned when packed OCIO resources do not match a resource contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuBindResourcePlanError {
    /// The binding layout plan could not derive a safe wgpu layout.
    BindingLayout(OcioGpuWgpuBindingLayoutPlanError),
    /// Packed LUT resources belong to a different shader/resource plan.
    LutResourceKeyMismatch { expected: u64, actual: u64 },
    /// Packed uniform resources belong to a different shader/resource plan.
    UniformResourceKeyMismatch { expected: u64, actual: u64 },
    /// The resource contract requires a uniform buffer but none was supplied.
    MissingUniformBuffer { binding: u32 },
    /// A uniform buffer was supplied for a resource contract that has none.
    UnexpectedUniformBuffer { binding: u32 },
    /// Packed uniform buffer binding does not match OCIO's contract.
    UniformBindingMismatch { expected: u32, actual: u32 },
    /// Packed uniform buffer byte length does not match OCIO's contract.
    UniformByteLengthMismatch { expected: usize, actual: usize },
    /// Packed LUT count does not match OCIO's contract.
    TextureCountMismatch {
        /// Texture dimension being validated.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Expected texture count.
        expected: usize,
        /// Actual texture count.
        actual: usize,
    },
    /// A required 1D/2D LUT texture was not supplied.
    MissingTexture2D { index: u32 },
    /// A required 3D LUT texture was not supplied.
    MissingTexture3D { index: u32 },
    /// Packed texture metadata does not match OCIO's contract.
    TextureContractMismatch {
        /// Texture dimension being validated.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Texture index from the OCIO shader descriptor.
        index: u32,
        /// Human-readable mismatch reason.
        reason: OcioGpuWgpuTextureContractMismatch,
    },
}

impl From<OcioGpuWgpuBindingLayoutPlanError> for OcioGpuWgpuBindResourcePlanError {
    fn from(reason: OcioGpuWgpuBindingLayoutPlanError) -> Self {
        Self::BindingLayout(reason)
    }
}

/// Field-level texture contract mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuTextureContractMismatch {
    /// Binding index mismatch.
    BindingIndex { expected: u32, actual: u32 },
    /// Texture symbol mismatch.
    TextureName { expected: String, actual: String },
    /// Sampler symbol mismatch.
    SamplerName { expected: String, actual: String },
    /// Texture extent mismatch.
    Extent {
        expected: OcioGpuWgpuLutTextureExtent,
        actual: OcioGpuWgpuLutTextureExtent,
    },
    /// Source OCIO values hash mismatch.
    SourceValuesHash { expected: u64, actual: u64 },
    /// Packed texture dimension mismatch.
    PackedDimension {
        expected: OcioGpuWgpuLutTextureDimension,
        actual: OcioGpuWgpuLutTextureDimension,
    },
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
    /// OCIO 1D/2D LUT sampler.
    OcioLutSampler2d {
        /// LUT texture index from the OCIO shader descriptor.
        index: u32,
    },
    /// OCIO 3D LUT texture.
    OcioLutTexture3d {
        /// LUT texture index from the OCIO shader descriptor.
        index: u32,
    },
    /// OCIO 3D LUT sampler.
    OcioLutSampler3d {
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

    /// Validate and pack OCIO LUT payloads into wgpu texture upload bytes.
    pub fn pack_textures(
        &self,
    ) -> Result<OcioGpuWgpuPackedLutUploadPlan, OcioGpuWgpuLutUploadError> {
        let textures_2d = self
            .textures_2d
            .iter()
            .map(OcioGpuWgpuPackedLutTexture::from_2d_upload)
            .collect::<Result<Vec<_>, _>>()?;
        let textures_3d = self
            .textures_3d
            .iter()
            .map(OcioGpuWgpuPackedLutTexture::from_3d_upload)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(OcioGpuWgpuPackedLutUploadPlan {
            resource_key: self.resource_key,
            textures_2d,
            textures_3d,
        })
    }
}

/// Packed OCIO LUT payloads ready for wgpu texture creation and queue upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuWgpuPackedLutUploadPlan {
    /// Stable resource key this packed upload plan belongs to.
    pub resource_key: u64,
    /// Packed 1D/2D LUT textures.
    pub textures_2d: Vec<OcioGpuWgpuPackedLutTexture>,
    /// Packed 3D LUT textures.
    pub textures_3d: Vec<OcioGpuWgpuPackedLutTexture>,
}

/// Texture format selected for an OCIO LUT upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuLutTextureFormat {
    /// Single-channel 32-bit float texture.
    R32Float,
    /// Four-channel 32-bit float texture.
    Rgba32Float,
}

impl OcioGpuWgpuLutTextureFormat {
    fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::R32Float => wgpu::TextureFormat::R32Float,
            Self::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
        }
    }

    fn bytes_per_texel(self) -> u32 {
        match self {
            Self::R32Float => 4,
            Self::Rgba32Float => 16,
        }
    }
}

/// Texture dimensionality selected for an OCIO LUT upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuLutTextureDimension {
    /// 2D wgpu texture, including OCIO logical 1D LUTs stored as 2D resources.
    D2,
    /// 3D wgpu texture.
    D3,
}

impl OcioGpuWgpuLutTextureDimension {
    fn to_wgpu(self) -> wgpu::TextureDimension {
        match self {
            Self::D2 => wgpu::TextureDimension::D2,
            Self::D3 => wgpu::TextureDimension::D3,
        }
    }

    fn view_dimension(self) -> wgpu::TextureViewDimension {
        match self {
            Self::D2 => wgpu::TextureViewDimension::D2,
            Self::D3 => wgpu::TextureViewDimension::D3,
        }
    }
}

/// Extent for an OCIO LUT texture upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuLutTextureExtent {
    /// Texture width in texels.
    pub width: u32,
    /// Texture height in texels.
    pub height: u32,
    /// Texture depth or array layer count.
    pub depth_or_array_layers: u32,
}

impl OcioGpuWgpuLutTextureExtent {
    fn to_wgpu(self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: self.width,
            height: self.height,
            depth_or_array_layers: self.depth_or_array_layers,
        }
    }
}

/// Packed bytes for one OCIO LUT texture upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuWgpuPackedLutTexture {
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
    /// Texture format selected for wgpu upload.
    pub format: OcioGpuWgpuLutTextureFormat,
    /// Texture dimension selected for wgpu upload.
    pub dimension: OcioGpuWgpuLutTextureDimension,
    /// Texture extent.
    pub extent: OcioGpuWgpuLutTextureExtent,
    /// Bytes per row for `Queue::write_texture`.
    pub bytes_per_row: u32,
    /// Rows per image for `Queue::write_texture`.
    pub rows_per_image: u32,
    /// Hash of the OCIO source `f32` values.
    pub source_values_hash: u64,
    /// Hash of the packed upload bytes.
    pub packed_bytes_hash: u64,
    /// Packed texture bytes.
    pub bytes: Vec<u8>,
}

impl OcioGpuWgpuPackedLutTexture {
    fn from_2d_upload(
        upload: &OcioGpuWgpuTexture2DUpload,
    ) -> Result<Self, OcioGpuWgpuLutUploadError> {
        if upload.width == 0 || upload.height == 0 {
            return Err(OcioGpuWgpuLutUploadError::EmptyTexture2D {
                index: upload.index,
                width: upload.width,
                height: upload.height,
            });
        }

        let texel_count = checked_texel_count_2d(upload.index, upload.width, upload.height)?;
        let (format, bytes) = match upload.channel {
            OcioGpuTextureChannel::Red => {
                validate_value_count(
                    OcioGpuWgpuLutUploadResource::Texture2D { index: upload.index },
                    upload.values.len(),
                    texel_count,
                )?;
                (
                    OcioGpuWgpuLutTextureFormat::R32Float,
                    f32_values_to_bytes(&upload.values),
                )
            }
            OcioGpuTextureChannel::Rgb => {
                let expected = checked_rgb_value_count(
                    OcioGpuWgpuLutUploadResource::Texture2D { index: upload.index },
                    texel_count,
                )?;
                validate_value_count(
                    OcioGpuWgpuLutUploadResource::Texture2D { index: upload.index },
                    upload.values.len(),
                    expected,
                )?;
                (
                    OcioGpuWgpuLutTextureFormat::Rgba32Float,
                    pack_rgb_values_as_rgba32(&upload.values),
                )
            }
        };
        let extent = OcioGpuWgpuLutTextureExtent {
            width: upload.width,
            height: upload.height,
            depth_or_array_layers: 1,
        };
        Ok(Self::new(
            upload.index,
            upload.texture_name.clone(),
            upload.sampler_name.clone(),
            upload.binding_index,
            upload.interpolation,
            format,
            OcioGpuWgpuLutTextureDimension::D2,
            extent,
            upload.values_hash,
            bytes,
        ))
    }

    fn from_3d_upload(
        upload: &OcioGpuWgpuTexture3DUpload,
    ) -> Result<Self, OcioGpuWgpuLutUploadError> {
        if upload.edge_len == 0 {
            return Err(OcioGpuWgpuLutUploadError::EmptyTexture3D {
                index: upload.index,
                edge_len: upload.edge_len,
            });
        }

        let texel_count = checked_texel_count_3d(upload.index, upload.edge_len)?;
        let expected = checked_rgb_value_count(
            OcioGpuWgpuLutUploadResource::Texture3D { index: upload.index },
            texel_count,
        )?;
        validate_value_count(
            OcioGpuWgpuLutUploadResource::Texture3D { index: upload.index },
            upload.values.len(),
            expected,
        )?;
        let extent = OcioGpuWgpuLutTextureExtent {
            width: upload.edge_len,
            height: upload.edge_len,
            depth_or_array_layers: upload.edge_len,
        };
        Ok(Self::new(
            upload.index,
            upload.texture_name.clone(),
            upload.sampler_name.clone(),
            upload.binding_index,
            upload.interpolation,
            OcioGpuWgpuLutTextureFormat::Rgba32Float,
            OcioGpuWgpuLutTextureDimension::D3,
            extent,
            upload.values_hash,
            pack_rgb_values_as_rgba32(&upload.values),
        ))
    }

    fn new(
        index: u32,
        texture_name: String,
        sampler_name: String,
        binding_index: u32,
        interpolation: OcioGpuTextureInterpolation,
        format: OcioGpuWgpuLutTextureFormat,
        dimension: OcioGpuWgpuLutTextureDimension,
        extent: OcioGpuWgpuLutTextureExtent,
        source_values_hash: u64,
        bytes: Vec<u8>,
    ) -> Self {
        let bytes_per_row = extent.width.saturating_mul(format.bytes_per_texel());
        let rows_per_image = extent.height;
        let packed_bytes_hash = hash_bytes(&bytes);
        Self {
            index,
            texture_name,
            sampler_name,
            binding_index,
            interpolation,
            format,
            dimension,
            extent,
            bytes_per_row,
            rows_per_image,
            source_values_hash,
            packed_bytes_hash,
            bytes,
        }
    }
}

/// Resource identifier for an OCIO LUT upload validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcioGpuWgpuLutUploadResource {
    /// A 1D/2D LUT texture.
    Texture2D { index: u32 },
    /// A 3D LUT texture.
    Texture3D { index: u32 },
}

/// Error returned when an OCIO LUT payload cannot be packed or uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuLutUploadError {
    /// OCIO reported a 1D/2D texture with an empty extent.
    EmptyTexture2D { index: u32, width: u32, height: u32 },
    /// OCIO reported a 3D texture with an empty edge length.
    EmptyTexture3D { index: u32, edge_len: u32 },
    /// Texture extent overflowed when computing texel count.
    TexelCountOverflow {
        /// Resource that overflowed.
        resource: OcioGpuWgpuLutUploadResource,
    },
    /// The copied value payload does not match the texture metadata.
    ValueCountMismatch {
        /// Resource with an invalid payload length.
        resource: OcioGpuWgpuLutUploadResource,
        /// Actual number of `f32` values.
        actual: usize,
        /// Expected number of `f32` values.
        expected: usize,
    },
}

/// Uploaded OCIO LUT texture resources owned by wgpu.
pub struct OcioGpuWgpuUploadedLutTexture {
    /// Texture index in the OCIO descriptor.
    pub index: u32,
    /// OCIO-generated texture symbol name.
    pub texture_name: String,
    /// OCIO-generated sampler symbol name.
    pub sampler_name: String,
    /// OCIO-reported binding slot.
    pub binding_index: u32,
    /// Texture format selected for upload.
    pub format: OcioGpuWgpuLutTextureFormat,
    /// Texture dimension selected for upload.
    pub dimension: OcioGpuWgpuLutTextureDimension,
    /// Texture extent.
    pub extent: OcioGpuWgpuLutTextureExtent,
    /// Hash of the source OCIO values.
    pub source_values_hash: u64,
    /// Hash of the packed bytes uploaded to wgpu.
    pub packed_bytes_hash: u64,
    /// Uploaded texture.
    pub texture: wgpu::Texture,
    /// Default texture view for shader binding.
    pub view: wgpu::TextureView,
    /// Sampler matching the OCIO interpolation policy.
    pub sampler: wgpu::Sampler,
}

/// Uploaded OCIO LUT resources for a shader plan.
pub struct OcioGpuWgpuUploadedLuts {
    /// Stable resource key this upload belongs to.
    pub resource_key: u64,
    /// Uploaded 1D/2D LUT textures.
    pub textures_2d: Vec<OcioGpuWgpuUploadedLutTexture>,
    /// Uploaded 3D LUT textures.
    pub textures_3d: Vec<OcioGpuWgpuUploadedLutTexture>,
}

/// Uniform buffer payload plan for an OCIO shader.
#[derive(Debug, Clone, PartialEq)]
pub struct OcioGpuWgpuUniformUploadPlan {
    /// Stable resource key this upload plan belongs to.
    pub resource_key: u64,
    /// Binding slot reserved for OCIO uniform data.
    pub binding: u32,
    /// OCIO-reported uniform buffer size in bytes.
    pub buffer_size: usize,
    /// Uniform metadata and values copied from OCIO.
    pub uniforms: Vec<OcioGpuWgpuUniformUpload>,
}

/// Upload payload for one OCIO uniform.
#[derive(Debug, Clone, PartialEq)]
pub struct OcioGpuWgpuUniformUpload {
    /// Uniform index in the OCIO descriptor.
    pub index: u32,
    /// Uniform symbol name.
    pub name: String,
    /// OCIO-reported uniform type.
    pub uniform_type: OcioGpuUniformType,
    /// Byte offset in OCIO's packed uniform buffer.
    pub buffer_offset: usize,
    /// Logical scalar count.
    pub value_count: usize,
    /// Stable hash of the copied uniform payload.
    pub value_hash: u64,
    /// Typed uniform payload.
    pub value: OcioGpuUniformValue,
}

impl OcioGpuWgpuUniformUploadPlan {
    /// Build a uniform upload plan from OCIO shader bundle metadata.
    pub fn for_shader_plan(
        shader_plan: &OcioGpuShaderPlan,
        resources: &OcioGpuWgpuResourcePlan,
    ) -> Self {
        let uniforms = shader_plan
            .bundle()
            .uniforms
            .iter()
            .map(|uniform| OcioGpuWgpuUniformUpload {
                index: uniform.index,
                name: uniform.name.clone(),
                uniform_type: uniform.uniform_type,
                buffer_offset: uniform.buffer_offset,
                value_count: uniform.value_count,
                value_hash: hash_uniform_value(&uniform.value),
                value: uniform.value.clone(),
            })
            .collect();
        Self {
            resource_key: resources.resource_key,
            binding: resources.binding_contract.uniform_buffer_binding,
            buffer_size: resources.binding_contract.uniform_buffer_size,
            uniforms,
        }
    }

    /// Validate and pack OCIO uniforms into a single uniform-buffer payload.
    pub fn pack_buffer(
        &self,
    ) -> Result<OcioGpuWgpuPackedUniformBuffer, OcioGpuWgpuUniformUploadError> {
        if self.buffer_size == 0 && !self.uniforms.is_empty() {
            return Err(OcioGpuWgpuUniformUploadError::MissingUniformBuffer {
                uniform_count: self.uniforms.len(),
            });
        }
        let mut bytes = vec![0u8; self.buffer_size];
        for uniform in &self.uniforms {
            let uniform_bytes = uniform_value_to_bytes(
                OcioGpuWgpuUniformUploadResource {
                    index: uniform.index,
                    name: uniform.name.clone(),
                },
                &uniform.value,
            )?;
            if uniform.value_count != uniform_value_scalar_count(&uniform.value) {
                return Err(OcioGpuWgpuUniformUploadError::ValueCountMismatch {
                    resource: OcioGpuWgpuUniformUploadResource {
                        index: uniform.index,
                        name: uniform.name.clone(),
                    },
                    actual: uniform_value_scalar_count(&uniform.value),
                    expected: uniform.value_count,
                });
            }
            let end = uniform.buffer_offset.checked_add(uniform_bytes.len()).ok_or_else(|| {
                OcioGpuWgpuUniformUploadError::UniformRangeOverflow {
                    resource: OcioGpuWgpuUniformUploadResource {
                        index: uniform.index,
                        name: uniform.name.clone(),
                    },
                }
            })?;
            if end > bytes.len() {
                return Err(OcioGpuWgpuUniformUploadError::UniformOutOfBounds {
                    resource: OcioGpuWgpuUniformUploadResource {
                        index: uniform.index,
                        name: uniform.name.clone(),
                    },
                    offset: uniform.buffer_offset,
                    byte_len: uniform_bytes.len(),
                    buffer_size: bytes.len(),
                });
            }
            bytes[uniform.buffer_offset..end].copy_from_slice(&uniform_bytes);
        }
        Ok(OcioGpuWgpuPackedUniformBuffer {
            resource_key: self.resource_key,
            binding: self.binding,
            byte_len: bytes.len(),
            bytes_hash: hash_bytes(&bytes),
            bytes,
        })
    }
}

/// Packed OCIO uniform buffer bytes ready for wgpu upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuWgpuPackedUniformBuffer {
    /// Stable resource key this buffer belongs to.
    pub resource_key: u64,
    /// Binding slot reserved for OCIO uniform data.
    pub binding: u32,
    /// Buffer length in bytes.
    pub byte_len: usize,
    /// Stable hash of packed bytes.
    pub bytes_hash: u64,
    /// Packed buffer bytes.
    pub bytes: Vec<u8>,
}

/// Uploaded OCIO uniform buffer.
pub struct OcioGpuWgpuUploadedUniformBuffer {
    /// Stable resource key this buffer belongs to.
    pub resource_key: u64,
    /// Binding slot reserved for OCIO uniform data.
    pub binding: u32,
    /// Buffer length in bytes.
    pub byte_len: usize,
    /// Stable hash of packed bytes.
    pub bytes_hash: u64,
    /// Uploaded wgpu buffer.
    pub buffer: wgpu::Buffer,
}

/// Resource identifier for uniform upload errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuWgpuUniformUploadResource {
    /// Uniform index in the OCIO descriptor.
    pub index: u32,
    /// Uniform symbol name.
    pub name: String,
}

/// Error returned when OCIO uniforms cannot be packed or uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuUniformUploadError {
    /// OCIO reported uniforms but no backing uniform buffer size.
    MissingUniformBuffer { uniform_count: usize },
    /// OCIO reported a uniform value type the current uploader cannot encode.
    UnsupportedUniformValue {
        /// Uniform resource that failed.
        resource: OcioGpuWgpuUniformUploadResource,
    },
    /// Uniform scalar count does not match the copied payload.
    ValueCountMismatch {
        /// Uniform resource that failed.
        resource: OcioGpuWgpuUniformUploadResource,
        /// Actual scalar count.
        actual: usize,
        /// Expected scalar count.
        expected: usize,
    },
    /// Offset + payload size overflowed.
    UniformRangeOverflow {
        /// Uniform resource that failed.
        resource: OcioGpuWgpuUniformUploadResource,
    },
    /// Uniform payload does not fit inside OCIO's packed buffer.
    UniformOutOfBounds {
        /// Uniform resource that failed.
        resource: OcioGpuWgpuUniformUploadResource,
        /// Byte offset in the packed buffer.
        offset: usize,
        /// Payload length in bytes.
        byte_len: usize,
        /// Uniform buffer length in bytes.
        buffer_size: usize,
    },
}

/// Stateless uploader for OCIO uniform buffers.
pub struct OcioGpuWgpuUniformUploader;

impl OcioGpuWgpuUniformUploader {
    /// Upload a validated OCIO uniform plan into a wgpu uniform buffer.
    pub fn upload(
        device: &wgpu::Device,
        plan: &OcioGpuWgpuUniformUploadPlan,
    ) -> Result<Option<OcioGpuWgpuUploadedUniformBuffer>, OcioGpuWgpuUniformUploadError> {
        let packed = plan.pack_buffer()?;
        Ok(Self::upload_packed(device, &packed))
    }

    /// Upload an already packed OCIO uniform buffer.
    pub fn upload_packed(
        device: &wgpu::Device,
        packed: &OcioGpuWgpuPackedUniformBuffer,
    ) -> Option<OcioGpuWgpuUploadedUniformBuffer> {
        if packed.bytes.is_empty() {
            return None;
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ocio_uniform_buffer"),
            size: packed.bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        buffer.slice(..).get_mapped_range_mut().copy_from_slice(&packed.bytes);
        buffer.unmap();
        Some(OcioGpuWgpuUploadedUniformBuffer {
            resource_key: packed.resource_key,
            binding: packed.binding,
            byte_len: packed.byte_len,
            bytes_hash: packed.bytes_hash,
            buffer,
        })
    }
}

/// Stateless uploader for OCIO LUT texture payloads.
pub struct OcioGpuWgpuLutUploader;

impl OcioGpuWgpuLutUploader {
    /// Upload a validated OCIO LUT plan into wgpu textures.
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &OcioGpuWgpuLutUploadPlan,
    ) -> Result<OcioGpuWgpuUploadedLuts, OcioGpuWgpuLutUploadError> {
        let packed = plan.pack_textures()?;
        Self::upload_packed(device, queue, &packed)
    }

    /// Upload an already packed OCIO LUT plan into wgpu textures.
    pub fn upload_packed(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packed: &OcioGpuWgpuPackedLutUploadPlan,
    ) -> Result<OcioGpuWgpuUploadedLuts, OcioGpuWgpuLutUploadError> {
        let textures_2d = packed
            .textures_2d
            .iter()
            .map(|texture| upload_lut_texture(device, queue, texture))
            .collect();
        let textures_3d = packed
            .textures_3d
            .iter()
            .map(|texture| upload_lut_texture(device, queue, texture))
            .collect();
        Ok(OcioGpuWgpuUploadedLuts {
            resource_key: packed.resource_key,
            textures_2d,
            textures_3d,
        })
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
    ) -> Result<Arc<OcioGpuWgpuPreparedResources>, OcioGpuWgpuBindingLayoutPlanError> {
        if let Some(hit) = self.entries.get(&resources.resource_key) {
            self.hits = self.hits.saturating_add(1);
            return Ok(Arc::clone(hit));
        }

        self.misses = self.misses.saturating_add(1);
        let prepared = Arc::new(OcioGpuWgpuPreparedResources {
            binding_layout: resources.binding_layout_plan()?,
            resources,
        });
        self.entries.put(prepared.resources.resource_key, Arc::clone(&prepared));
        Ok(prepared)
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
    /// A backend shader module has not been prepared from the translated shader.
    ShaderModuleNotPrepared { language: GpuLanguage },
    /// OCIO resource bind entries have not been connected to a concrete wgpu bind group.
    OcioResourceBindGroupNotPrepared {
        /// OCIO 1D/2D LUT texture count.
        texture_2d_count: u32,
        /// OCIO 3D LUT texture count.
        texture_3d_count: u32,
        /// OCIO uniform buffer count.
        uniform_buffers: u32,
    },
    /// Mondrian has not generated the fullscreen wrapper that calls the OCIO program.
    FullscreenWrapperNotPrepared,
    /// The final render pipeline/render-pass node is not implemented yet.
    RenderPipelineNotPrepared,
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

        blockers.push(OcioGpuWgpuBlocker::ShaderModuleNotPrepared {
            language: shader_plan.request.language(),
        });

        let resources = OcioGpuWgpuResourcePlan::for_shader_plan(&shader_plan);
        if shader_plan.texture_2d_count > 0
            || shader_plan.texture_3d_count > 0
            || resources.uniform_buffers > 0
        {
            blockers.push(OcioGpuWgpuBlocker::OcioResourceBindGroupNotPrepared {
                texture_2d_count: shader_plan.texture_2d_count,
                texture_3d_count: shader_plan.texture_3d_count,
                uniform_buffers: resources.uniform_buffers,
            });
        }
        blockers.push(OcioGpuWgpuBlocker::FullscreenWrapperNotPrepared);
        blockers.push(OcioGpuWgpuBlocker::RenderPipelineNotPrepared);

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
        uniforms: bundle
            .uniforms
            .iter()
            .map(|uniform| OcioGpuUniformBindingContract {
                index: uniform.index,
                name: uniform.name.clone(),
                uniform_type: uniform.uniform_type,
                buffer_offset: uniform.buffer_offset,
                value_count: uniform.value_count,
                value_hash: hash_uniform_value(&uniform.value),
            })
            .collect(),
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

fn first_free_binding_after_ocio_resources(
    contract: &OcioGpuBindingContract,
) -> Result<u32, OcioGpuWgpuBindingLayoutPlanError> {
    let mut max_binding = if contract.uniform_buffer_size > 0 || contract.uniform_count > 0 {
        Some(contract.uniform_buffer_binding)
    } else {
        None
    };
    for texture in &contract.textures_2d {
        max_binding =
            Some(max_binding.map_or(texture.binding_index, |max| max.max(texture.binding_index)));
    }
    for texture in &contract.textures_3d {
        max_binding =
            Some(max_binding.map_or(texture.binding_index, |max| max.max(texture.binding_index)));
    }
    match max_binding {
        Some(binding) => {
            binding
                .checked_add(1)
                .ok_or(OcioGpuWgpuBindingLayoutPlanError::BindingIndexOverflow {
                    start: binding,
                    count: 1,
                })
        }
        None => Ok(contract.texture_binding_start),
    }
}

fn checked_binding_offset(
    start: u32,
    offset: usize,
    count: usize,
) -> Result<u32, OcioGpuWgpuBindingLayoutPlanError> {
    let offset = u32::try_from(offset)
        .map_err(|_| OcioGpuWgpuBindingLayoutPlanError::BindingIndexOverflow { start, count })?;
    start
        .checked_add(offset)
        .ok_or(OcioGpuWgpuBindingLayoutPlanError::BindingIndexOverflow { start, count })
}

fn validate_unique_bindings(
    entries: &[OcioGpuWgpuBindingPlan],
) -> Result<(), OcioGpuWgpuBindingLayoutPlanError> {
    for pair in entries.windows(2) {
        if pair[0].binding == pair[1].binding {
            return Err(OcioGpuWgpuBindingLayoutPlanError::BindingCollision {
                binding: pair[0].binding,
            });
        }
    }
    Ok(())
}

fn hash_bind_resource_plan(
    resource_key: u64,
    ocio_bind_group: u32,
    wrapper_layout_hash: u64,
    ocio_layout_hash: u64,
    entries: &[OcioGpuWgpuBindResourceEntry],
) -> u64 {
    let mut hasher = DefaultHasher::new();
    resource_key.hash(&mut hasher);
    ocio_bind_group.hash(&mut hasher);
    wrapper_layout_hash.hash(&mut hasher);
    ocio_layout_hash.hash(&mut hasher);
    entries.hash(&mut hasher);
    hasher.finish()
}

fn validate_uniform_bind_resource(
    resources: &OcioGpuWgpuResourcePlan,
    uniform: &OcioGpuWgpuPackedUniformBuffer,
) -> Result<(), OcioGpuWgpuBindResourcePlanError> {
    if resources.resource_key != uniform.resource_key {
        return Err(
            OcioGpuWgpuBindResourcePlanError::UniformResourceKeyMismatch {
                expected: resources.resource_key,
                actual: uniform.resource_key,
            },
        );
    }
    if resources.binding_contract.uniform_buffer_binding != uniform.binding {
        return Err(OcioGpuWgpuBindResourcePlanError::UniformBindingMismatch {
            expected: resources.binding_contract.uniform_buffer_binding,
            actual: uniform.binding,
        });
    }
    if resources.binding_contract.uniform_buffer_size != uniform.byte_len {
        return Err(
            OcioGpuWgpuBindResourcePlanError::UniformByteLengthMismatch {
                expected: resources.binding_contract.uniform_buffer_size,
                actual: uniform.byte_len,
            },
        );
    }
    Ok(())
}

fn validate_texture_2d_bind_resource(
    contract: &OcioGpuTexture2DBindingContract,
    texture: &OcioGpuWgpuPackedLutTexture,
) -> Result<(), OcioGpuWgpuBindResourcePlanError> {
    let expected_extent = OcioGpuWgpuLutTextureExtent {
        width: contract.width,
        height: contract.height,
        depth_or_array_layers: 1,
    };
    validate_texture_common(
        OcioGpuWgpuLutTextureDimension::D2,
        contract.index,
        contract.binding_index,
        &contract.texture_name,
        &contract.sampler_name,
        expected_extent,
        contract.values_hash,
        texture,
    )
}

fn validate_texture_3d_bind_resource(
    contract: &OcioGpuTexture3DBindingContract,
    texture: &OcioGpuWgpuPackedLutTexture,
) -> Result<(), OcioGpuWgpuBindResourcePlanError> {
    let expected_extent = OcioGpuWgpuLutTextureExtent {
        width: contract.edge_len,
        height: contract.edge_len,
        depth_or_array_layers: contract.edge_len,
    };
    validate_texture_common(
        OcioGpuWgpuLutTextureDimension::D3,
        contract.index,
        contract.binding_index,
        &contract.texture_name,
        &contract.sampler_name,
        expected_extent,
        contract.values_hash,
        texture,
    )
}

fn validate_texture_common(
    dimension: OcioGpuWgpuLutTextureDimension,
    index: u32,
    binding_index: u32,
    texture_name: &str,
    sampler_name: &str,
    expected_extent: OcioGpuWgpuLutTextureExtent,
    values_hash: u64,
    texture: &OcioGpuWgpuPackedLutTexture,
) -> Result<(), OcioGpuWgpuBindResourcePlanError> {
    if texture.binding_index != binding_index {
        return Err(texture_contract_mismatch(
            dimension,
            index,
            OcioGpuWgpuTextureContractMismatch::BindingIndex {
                expected: binding_index,
                actual: texture.binding_index,
            },
        ));
    }
    if texture.texture_name != texture_name {
        return Err(texture_contract_mismatch(
            dimension,
            index,
            OcioGpuWgpuTextureContractMismatch::TextureName {
                expected: texture_name.to_owned(),
                actual: texture.texture_name.clone(),
            },
        ));
    }
    if texture.sampler_name != sampler_name {
        return Err(texture_contract_mismatch(
            dimension,
            index,
            OcioGpuWgpuTextureContractMismatch::SamplerName {
                expected: sampler_name.to_owned(),
                actual: texture.sampler_name.clone(),
            },
        ));
    }
    if texture.dimension != dimension {
        return Err(texture_contract_mismatch(
            dimension,
            index,
            OcioGpuWgpuTextureContractMismatch::PackedDimension {
                expected: dimension,
                actual: texture.dimension,
            },
        ));
    }
    if texture.extent != expected_extent {
        return Err(texture_contract_mismatch(
            dimension,
            index,
            OcioGpuWgpuTextureContractMismatch::Extent {
                expected: expected_extent,
                actual: texture.extent,
            },
        ));
    }
    if texture.source_values_hash != values_hash {
        return Err(texture_contract_mismatch(
            dimension,
            index,
            OcioGpuWgpuTextureContractMismatch::SourceValuesHash {
                expected: values_hash,
                actual: texture.source_values_hash,
            },
        ));
    }
    Ok(())
}

fn texture_contract_mismatch(
    dimension: OcioGpuWgpuLutTextureDimension,
    index: u32,
    reason: OcioGpuWgpuTextureContractMismatch,
) -> OcioGpuWgpuBindResourcePlanError {
    OcioGpuWgpuBindResourcePlanError::TextureContractMismatch { dimension, index, reason }
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

fn upload_lut_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    packed: &OcioGpuWgpuPackedLutTexture,
) -> OcioGpuWgpuUploadedLutTexture {
    let label = format!("ocio_lut_{}", packed.texture_name);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&label),
        size: packed.extent.to_wgpu(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: packed.dimension.to_wgpu(),
        format: packed.format.to_wgpu(),
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &packed.bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(packed.bytes_per_row),
            rows_per_image: Some(packed.rows_per_image),
        },
        packed.extent.to_wgpu(),
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("ocio_lut_texture_view"),
        format: Some(packed.format.to_wgpu()),
        dimension: Some(packed.dimension.view_dimension()),
        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
        aspect: wgpu::TextureAspect::All,
        base_mip_level: 0,
        mip_level_count: Some(1),
        base_array_layer: 0,
        array_layer_count: None,
    });
    let sampler = device.create_sampler(&sampler_descriptor_for_interpolation(
        packed.interpolation,
        &packed.sampler_name,
    ));
    OcioGpuWgpuUploadedLutTexture {
        index: packed.index,
        texture_name: packed.texture_name.clone(),
        sampler_name: packed.sampler_name.clone(),
        binding_index: packed.binding_index,
        format: packed.format,
        dimension: packed.dimension,
        extent: packed.extent,
        source_values_hash: packed.source_values_hash,
        packed_bytes_hash: packed.packed_bytes_hash,
        texture,
        view,
        sampler,
    }
}

fn sampler_descriptor_for_interpolation(
    _interpolation: OcioGpuTextureInterpolation,
    label: &str,
) -> wgpu::SamplerDescriptor<'_> {
    wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    }
}

fn checked_texel_count_2d(
    index: u32,
    width: u32,
    height: u32,
) -> Result<usize, OcioGpuWgpuLutUploadError> {
    (width as usize).checked_mul(height as usize).ok_or(
        OcioGpuWgpuLutUploadError::TexelCountOverflow {
            resource: OcioGpuWgpuLutUploadResource::Texture2D { index },
        },
    )
}

fn checked_texel_count_3d(index: u32, edge_len: u32) -> Result<usize, OcioGpuWgpuLutUploadError> {
    let edge = edge_len as usize;
    edge.checked_mul(edge).and_then(|area| area.checked_mul(edge)).ok_or(
        OcioGpuWgpuLutUploadError::TexelCountOverflow {
            resource: OcioGpuWgpuLutUploadResource::Texture3D { index },
        },
    )
}

fn validate_value_count(
    resource: OcioGpuWgpuLutUploadResource,
    actual: usize,
    expected: usize,
) -> Result<(), OcioGpuWgpuLutUploadError> {
    if actual != expected {
        return Err(OcioGpuWgpuLutUploadError::ValueCountMismatch { resource, actual, expected });
    }
    Ok(())
}

fn checked_rgb_value_count(
    resource: OcioGpuWgpuLutUploadResource,
    texel_count: usize,
) -> Result<usize, OcioGpuWgpuLutUploadError> {
    texel_count
        .checked_mul(3)
        .ok_or(OcioGpuWgpuLutUploadError::TexelCountOverflow { resource })
}

fn uniform_value_to_bytes(
    resource: OcioGpuWgpuUniformUploadResource,
    value: &OcioGpuUniformValue,
) -> Result<Vec<u8>, OcioGpuWgpuUniformUploadError> {
    match value {
        OcioGpuUniformValue::F32(values) => Ok(f32_values_to_bytes(values)),
        OcioGpuUniformValue::I32(values) => Ok(bytemuck::cast_slice(values).to_vec()),
        OcioGpuUniformValue::Unsupported => {
            Err(OcioGpuWgpuUniformUploadError::UnsupportedUniformValue { resource })
        }
    }
}

fn uniform_value_scalar_count(value: &OcioGpuUniformValue) -> usize {
    match value {
        OcioGpuUniformValue::F32(values) => values.len(),
        OcioGpuUniformValue::I32(values) => values.len(),
        OcioGpuUniformValue::Unsupported => 0,
    }
}

fn f32_values_to_bytes(values: &[f32]) -> Vec<u8> {
    bytemuck::cast_slice(values).to_vec()
}

fn pack_rgb_values_as_rgba32(values: &[f32]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(values.len() / 3 * 4);
    for rgb in values.chunks_exact(3) {
        rgba.push(rgb[0]);
        rgba.push(rgb[1]);
        rgba.push(rgb[2]);
        rgba.push(1.0);
    }
    f32_values_to_bytes(&rgba)
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

fn hash_i32_values(values: &[i32]) -> u64 {
    let mut hasher = DefaultHasher::new();
    values.len().hash(&mut hasher);
    for value in values {
        value.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_uniform_value(value: &OcioGpuUniformValue) -> u64 {
    let mut hasher = DefaultHasher::new();
    match value {
        OcioGpuUniformValue::F32(values) => {
            0u8.hash(&mut hasher);
            hash_f32_values(values).hash(&mut hasher);
        }
        OcioGpuUniformValue::I32(values) => {
            1u8.hash(&mut hasher);
            hash_i32_values(values).hash(&mut hasher);
        }
        OcioGpuUniformValue::Unsupported => {
            2u8.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ensure_mondrian_default_ocio_loaded, ocio_default_display_view};

    fn f32s_from_bytes(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }

    fn texture_2d_upload(
        channel: OcioGpuTextureChannel,
        width: u32,
        height: u32,
        values: Vec<f32>,
    ) -> OcioGpuWgpuTexture2DUpload {
        OcioGpuWgpuTexture2DUpload {
            index: 7,
            texture_name: "lut2d".to_owned(),
            sampler_name: "lut2d_sampler".to_owned(),
            binding_index: 3,
            channel,
            dimensions: OcioGpuTextureDimensions::Texture2D,
            interpolation: OcioGpuTextureInterpolation::Linear,
            width,
            height,
            values_hash: hash_f32_values(&values),
            values,
        }
    }

    fn texture_3d_upload(edge_len: u32, values: Vec<f32>) -> OcioGpuWgpuTexture3DUpload {
        OcioGpuWgpuTexture3DUpload {
            index: 9,
            texture_name: "lut3d".to_owned(),
            sampler_name: "lut3d_sampler".to_owned(),
            binding_index: 4,
            interpolation: OcioGpuTextureInterpolation::Tetrahedral,
            edge_len,
            values_hash: hash_f32_values(&values),
            values,
        }
    }

    fn f32_uniform(index: u32, offset: usize, values: Vec<f32>) -> OcioGpuWgpuUniformUpload {
        OcioGpuWgpuUniformUpload {
            index,
            name: format!("uniform_f32_{index}"),
            uniform_type: OcioGpuUniformType::VectorFloat,
            buffer_offset: offset,
            value_count: values.len(),
            value_hash: hash_uniform_value(&OcioGpuUniformValue::F32(values.clone())),
            value: OcioGpuUniformValue::F32(values),
        }
    }

    fn i32_uniform(index: u32, offset: usize, values: Vec<i32>) -> OcioGpuWgpuUniformUpload {
        OcioGpuWgpuUniformUpload {
            index,
            name: format!("uniform_i32_{index}"),
            uniform_type: OcioGpuUniformType::VectorInt,
            buffer_offset: offset,
            value_count: values.len(),
            value_hash: hash_uniform_value(&OcioGpuUniformValue::I32(values.clone())),
            value: OcioGpuUniformValue::I32(values),
        }
    }

    fn bind_resource_test_plan(resource_key: u64) -> OcioGpuWgpuResourcePlan {
        let uniform_value = OcioGpuUniformValue::F32(vec![0.5, 1.0]);
        let binding_contract = OcioGpuBindingContract {
            descriptor_set_index: 0,
            uniform_buffer_binding: 0,
            texture_binding_start: 1,
            uniform_buffer_size: 16,
            uniform_count: 1,
            uniforms: vec![OcioGpuUniformBindingContract {
                index: 1,
                name: "uniform_f32_1".to_owned(),
                uniform_type: OcioGpuUniformType::VectorFloat,
                buffer_offset: 0,
                value_count: 2,
                value_hash: hash_uniform_value(&uniform_value),
            }],
            textures_2d: vec![OcioGpuTexture2DBindingContract {
                index: 7,
                texture_name: "lut2d".to_owned(),
                sampler_name: "lut2d_sampler".to_owned(),
                binding_index: 3,
                channel: OcioGpuTextureChannel::Rgb,
                dimensions: OcioGpuTextureDimensions::Texture2D,
                interpolation: OcioGpuTextureInterpolation::Linear,
                width: 2,
                height: 1,
                value_count: 6,
                values_hash: hash_f32_values(&[1.0, 0.0, 0.25, 0.0, 1.0, 0.5]),
            }],
            textures_3d: vec![OcioGpuTexture3DBindingContract {
                index: 9,
                texture_name: "lut3d".to_owned(),
                sampler_name: "lut3d_sampler".to_owned(),
                binding_index: 4,
                interpolation: OcioGpuTextureInterpolation::Tetrahedral,
                edge_len: 2,
                value_count: 24,
                values_hash: hash_f32_values(
                    &(0..24).map(|value| value as f32 / 23.0).collect::<Vec<_>>(),
                ),
            }],
        };
        let binding_contract_hash = binding_contract.stable_hash();
        let wrapper_contract = fullscreen_wrapper_contract_for(&binding_contract);
        resource_plan_from_contract(
            resource_key,
            101,
            binding_contract_hash,
            binding_contract,
            wrapper_contract,
        )
    }

    fn resource_plan_from_contract(
        resource_key: u64,
        shader_hash: u64,
        binding_contract_hash: u64,
        binding_contract: OcioGpuBindingContract,
        wrapper_contract: OcioGpuFullscreenWrapperContract,
    ) -> OcioGpuWgpuResourcePlan {
        OcioGpuWgpuResourcePlan {
            resource_key,
            shader_hash,
            binding_contract_hash,
            binding_contract,
            wrapper_contract,
            input_textures: 1,
            output_textures: 1,
            ocio_texture_2d_bindings: 1,
            ocio_texture_3d_bindings: 1,
            uniform_buffers: 1,
            samplers: 3,
            bind_group_entries: 5,
            bind_groups: 2,
            pipeline_layout_hash: 202,
        }
    }

    fn packed_luts_for_bind_resource(resource_key: u64) -> OcioGpuWgpuPackedLutUploadPlan {
        let texture_2d = texture_2d_upload(
            OcioGpuTextureChannel::Rgb,
            2,
            1,
            vec![1.0, 0.0, 0.25, 0.0, 1.0, 0.5],
        );
        let texture_3d = texture_3d_upload(
            2,
            (0..24).map(|value| value as f32 / 23.0).collect::<Vec<_>>(),
        );
        OcioGpuWgpuLutUploadPlan {
            resource_key,
            textures_2d: vec![texture_2d],
            textures_3d: vec![texture_3d],
        }
        .pack_textures()
        .expect("pack bind-resource LUTs")
    }

    fn packed_uniform_for_bind_resource(resource_key: u64) -> OcioGpuWgpuPackedUniformBuffer {
        OcioGpuWgpuUniformUploadPlan {
            resource_key,
            binding: 0,
            buffer_size: 16,
            uniforms: vec![f32_uniform(1, 0, vec![0.5, 1.0])],
        }
        .pack_buffer()
        .expect("pack bind-resource uniform")
    }

    #[test]
    fn lut_upload_pack_preserves_red_2d_values_as_r32float() {
        let upload = texture_2d_upload(OcioGpuTextureChannel::Red, 2, 1, vec![0.25, 0.75]);
        let plan = OcioGpuWgpuLutUploadPlan {
            resource_key: 11,
            textures_2d: vec![upload.clone()],
            textures_3d: Vec::new(),
        };

        let packed = plan.pack_textures().expect("pack red 2D LUT");

        assert_eq!(packed.resource_key, 11);
        assert_eq!(packed.textures_2d.len(), 1);
        let texture = &packed.textures_2d[0];
        assert_eq!(texture.format, OcioGpuWgpuLutTextureFormat::R32Float);
        assert_eq!(texture.dimension, OcioGpuWgpuLutTextureDimension::D2);
        assert_eq!(
            texture.extent,
            OcioGpuWgpuLutTextureExtent { width: 2, height: 1, depth_or_array_layers: 1 }
        );
        assert_eq!(texture.bytes_per_row, 8);
        assert_eq!(texture.rows_per_image, 1);
        assert_eq!(texture.source_values_hash, upload.values_hash);
        assert_eq!(f32s_from_bytes(&texture.bytes), upload.values);
    }

    #[test]
    fn lut_upload_pack_expands_rgb_2d_values_to_rgba32float() {
        let upload = texture_2d_upload(
            OcioGpuTextureChannel::Rgb,
            2,
            1,
            vec![1.0, 0.0, 0.25, 0.0, 1.0, 0.5],
        );
        let plan = OcioGpuWgpuLutUploadPlan {
            resource_key: 12,
            textures_2d: vec![upload],
            textures_3d: Vec::new(),
        };

        let packed = plan.pack_textures().expect("pack RGB 2D LUT");
        let texture = &packed.textures_2d[0];

        assert_eq!(texture.format, OcioGpuWgpuLutTextureFormat::Rgba32Float);
        assert_eq!(texture.bytes_per_row, 32);
        assert_eq!(
            f32s_from_bytes(&texture.bytes),
            vec![1.0, 0.0, 0.25, 1.0, 0.0, 1.0, 0.5, 1.0]
        );
        assert_ne!(texture.packed_bytes_hash, 0);
    }

    #[test]
    fn lut_upload_pack_expands_rgb_3d_values_to_rgba32float() {
        let values = (0..24).map(|value| value as f32 / 23.0).collect::<Vec<_>>();
        let upload = texture_3d_upload(2, values);
        let plan = OcioGpuWgpuLutUploadPlan {
            resource_key: 13,
            textures_2d: Vec::new(),
            textures_3d: vec![upload],
        };

        let packed = plan.pack_textures().expect("pack RGB 3D LUT");
        let texture = &packed.textures_3d[0];
        let unpacked = f32s_from_bytes(&texture.bytes);

        assert_eq!(texture.format, OcioGpuWgpuLutTextureFormat::Rgba32Float);
        assert_eq!(texture.dimension, OcioGpuWgpuLutTextureDimension::D3);
        assert_eq!(
            texture.extent,
            OcioGpuWgpuLutTextureExtent { width: 2, height: 2, depth_or_array_layers: 2 }
        );
        assert_eq!(texture.bytes_per_row, 32);
        assert_eq!(texture.rows_per_image, 2);
        assert_eq!(unpacked.len(), 32);
        assert!(unpacked.chunks_exact(4).all(|rgba| rgba[3] == 1.0));
    }

    #[test]
    fn lut_upload_pack_rejects_mismatched_rgb_value_count() {
        let upload = texture_2d_upload(OcioGpuTextureChannel::Rgb, 1, 1, vec![0.0, 1.0]);
        let plan = OcioGpuWgpuLutUploadPlan {
            resource_key: 14,
            textures_2d: vec![upload],
            textures_3d: Vec::new(),
        };

        let err = plan.pack_textures().expect_err("RGB 2D LUT needs 3 values per texel");

        assert_eq!(
            err,
            OcioGpuWgpuLutUploadError::ValueCountMismatch {
                resource: OcioGpuWgpuLutUploadResource::Texture2D { index: 7 },
                actual: 2,
                expected: 3
            }
        );
    }

    #[test]
    fn uniform_upload_pack_writes_f32_and_i32_values_at_offsets() {
        let plan = OcioGpuWgpuUniformUploadPlan {
            resource_key: 21,
            binding: 0,
            buffer_size: 24,
            uniforms: vec![
                f32_uniform(0, 0, vec![0.25, 0.5]),
                i32_uniform(1, 16, vec![7, -3]),
            ],
        };

        let packed = plan.pack_buffer().expect("pack uniform buffer");

        assert_eq!(packed.resource_key, 21);
        assert_eq!(packed.binding, 0);
        assert_eq!(packed.byte_len, 24);
        assert_ne!(packed.bytes_hash, 0);
        assert_eq!(f32s_from_bytes(&packed.bytes[0..8]), vec![0.25, 0.5]);
        assert_eq!(&packed.bytes[8..16], &[0u8; 8]);
        let first_i32 = i32::from_ne_bytes([
            packed.bytes[16],
            packed.bytes[17],
            packed.bytes[18],
            packed.bytes[19],
        ]);
        let second_i32 = i32::from_ne_bytes([
            packed.bytes[20],
            packed.bytes[21],
            packed.bytes[22],
            packed.bytes[23],
        ]);
        assert_eq!((first_i32, second_i32), (7, -3));
    }

    #[test]
    fn uniform_upload_pack_rejects_out_of_bounds_uniform() {
        let plan = OcioGpuWgpuUniformUploadPlan {
            resource_key: 22,
            binding: 0,
            buffer_size: 4,
            uniforms: vec![f32_uniform(2, 2, vec![1.0])],
        };

        let err = plan.pack_buffer().expect_err("uniform payload must fit inside OCIO buffer");

        assert_eq!(
            err,
            OcioGpuWgpuUniformUploadError::UniformOutOfBounds {
                resource: OcioGpuWgpuUniformUploadResource {
                    index: 2,
                    name: "uniform_f32_2".to_owned()
                },
                offset: 2,
                byte_len: 4,
                buffer_size: 4
            }
        );
    }

    #[test]
    fn uniform_upload_pack_rejects_unsupported_uniform_value() {
        let plan = OcioGpuWgpuUniformUploadPlan {
            resource_key: 23,
            binding: 0,
            buffer_size: 4,
            uniforms: vec![OcioGpuWgpuUniformUpload {
                index: 3,
                name: "unsupported".to_owned(),
                uniform_type: OcioGpuUniformType::Unknown,
                buffer_offset: 0,
                value_count: 1,
                value_hash: hash_uniform_value(&OcioGpuUniformValue::Unsupported),
                value: OcioGpuUniformValue::Unsupported,
            }],
        };

        let err = plan.pack_buffer().expect_err("unsupported uniform value must fail closed");

        assert_eq!(
            err,
            OcioGpuWgpuUniformUploadError::UnsupportedUniformValue {
                resource: OcioGpuWgpuUniformUploadResource {
                    index: 3,
                    name: "unsupported".to_owned()
                }
            }
        );
    }

    #[test]
    fn bind_resource_plan_validates_matching_packed_luts_and_uniforms() {
        let resources = bind_resource_test_plan(31);
        let packed_luts = packed_luts_for_bind_resource(31);
        let packed_uniform = packed_uniform_for_bind_resource(31);

        let bind_plan = OcioGpuWgpuBindResourcePlan::from_packed_resources(
            &resources,
            &packed_luts,
            Some(&packed_uniform),
        )
        .expect("bind resource plan");

        assert_eq!(bind_plan.resource_key, resources.resource_key);
        assert_eq!(bind_plan.ocio_bind_group, 0);
        assert_eq!(bind_plan.wrapper_bind_group, 1);
        assert_eq!(bind_plan.ocio_entries.len(), 5);
        assert_eq!(bind_plan.wrapper_layout.bind_group, 1);
        assert_eq!(bind_plan.wrapper_layout.entries.len(), 2);
        assert!(bind_plan.wrapper_layout.entries.iter().any(|entry| {
            entry.binding == 0
                && entry.resource == OcioGpuWgpuWrapperBindingResource::InputFrameTexture
        }));
        assert!(bind_plan.wrapper_layout.entries.iter().any(|entry| {
            entry.binding == 1
                && entry.resource == OcioGpuWgpuWrapperBindingResource::InputFrameSampler
        }));
        assert!(bind_plan.ocio_entries.iter().any(|entry| {
            entry.binding == 0
                && matches!(
                    entry.resource,
                    OcioGpuWgpuBindResource::UniformBuffer { byte_len: 16, .. }
                )
        }));
        assert!(bind_plan.ocio_entries.iter().any(|entry| {
            entry.binding == 3
                && matches!(
                    entry.resource,
                    OcioGpuWgpuBindResource::LutTexture {
                        index: 7,
                        dimension: OcioGpuWgpuLutTextureDimension::D2,
                        ..
                    }
                )
        }));
        assert!(bind_plan.ocio_entries.iter().any(|entry| {
            entry.binding == 4
                && matches!(
                    entry.resource,
                    OcioGpuWgpuBindResource::LutTexture {
                        index: 9,
                        dimension: OcioGpuWgpuLutTextureDimension::D3,
                        ..
                    }
                )
        }));
        assert!(bind_plan.ocio_entries.iter().any(|entry| {
            entry.binding == 5
                && matches!(
                    entry.resource,
                    OcioGpuWgpuBindResource::LutSampler {
                        index: 7,
                        interpolation: OcioGpuTextureInterpolation::Linear,
                        filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
                        ..
                    }
                )
        }));
        assert!(bind_plan.ocio_entries.iter().any(|entry| {
            entry.binding == 6
                && matches!(
                    entry.resource,
                    OcioGpuWgpuBindResource::LutSampler {
                        index: 9,
                        interpolation: OcioGpuTextureInterpolation::Tetrahedral,
                        filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
                        ..
                    }
                )
        }));
        assert_ne!(bind_plan.ocio_layout_hash, 0);
        assert_ne!(bind_plan.wrapper_layout.layout_hash, 0);
        assert_ne!(bind_plan.plan_hash, 0);
    }

    #[test]
    fn binding_layout_descriptor_separates_lut_textures_and_samplers() {
        let resources = bind_resource_test_plan(41);

        let binding_layout =
            resources.binding_layout_plan().expect("binding layout with separated samplers");
        let ocio_descriptor =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_ocio_resources(&binding_layout);
        let wrapper_descriptor = OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_wrapper_input(
            &OcioGpuWgpuWrapperBindingPlan::for_contract(&resources.wrapper_contract),
        );

        assert_eq!(binding_layout.sampler_policy.sampler_binding_start, 5);
        assert_eq!(binding_layout.sampler_policy.mappings.len(), 2);
        assert_eq!(binding_layout.entries.len(), 5);
        assert!(binding_layout.entries.iter().any(|entry| {
            entry.binding == 5
                && entry.resource == OcioGpuWgpuBindingResource::OcioLutSampler2d { index: 7 }
        }));
        assert!(binding_layout.entries.iter().any(|entry| {
            entry.binding == 6
                && entry.resource == OcioGpuWgpuBindingResource::OcioLutSampler3d { index: 9 }
        }));
        assert!(ocio_descriptor.entries.iter().any(|entry| {
            entry.binding == 3
                && entry.resource
                    == OcioGpuWgpuLayoutBindingResource::SampledTexture {
                        dimension: OcioGpuWgpuLutTextureDimension::D2,
                        sample_type: OcioGpuWgpuTextureSampleType::Float32,
                    }
        }));
        assert!(ocio_descriptor.entries.iter().any(|entry| {
            entry.binding == 5
                && entry.resource
                    == OcioGpuWgpuLayoutBindingResource::Sampler {
                        filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
                    }
        }));
        assert_eq!(wrapper_descriptor.bind_group, 1);
        assert!(wrapper_descriptor.entries.iter().any(|entry| {
            entry.binding == 0
                && entry.resource
                    == OcioGpuWgpuLayoutBindingResource::SampledTexture {
                        dimension: OcioGpuWgpuLutTextureDimension::D2,
                        sample_type: OcioGpuWgpuTextureSampleType::Float32,
                    }
        }));
        assert!(wrapper_descriptor.entries.iter().any(|entry| {
            entry.binding == 1
                && entry.resource
                    == OcioGpuWgpuLayoutBindingResource::Sampler {
                        filtering: OcioGpuWgpuSamplerFiltering::Filtering,
                    }
        }));
        assert_ne!(ocio_descriptor.layout_hash, wrapper_descriptor.layout_hash);
    }

    #[test]
    fn bind_resource_plan_rejects_missing_lut_texture() {
        let resources = bind_resource_test_plan(32);
        let mut packed_luts = packed_luts_for_bind_resource(32);
        packed_luts.textures_3d.clear();
        let packed_uniform = packed_uniform_for_bind_resource(32);

        let err = OcioGpuWgpuBindResourcePlan::from_packed_resources(
            &resources,
            &packed_luts,
            Some(&packed_uniform),
        )
        .expect_err("missing 3D LUT should fail");

        assert!(matches!(
            err,
            OcioGpuWgpuBindResourcePlanError::TextureCountMismatch {
                dimension: OcioGpuWgpuLutTextureDimension::D3,
                expected: 1,
                actual: 0
            }
        ));
    }

    #[test]
    fn bind_resource_plan_rejects_lut_source_hash_mismatch() {
        let resources = bind_resource_test_plan(33);
        let mut packed_luts = packed_luts_for_bind_resource(33);
        packed_luts.textures_2d[0].source_values_hash =
            packed_luts.textures_2d[0].source_values_hash.wrapping_add(1);
        let packed_uniform = packed_uniform_for_bind_resource(33);

        let err = OcioGpuWgpuBindResourcePlan::from_packed_resources(
            &resources,
            &packed_luts,
            Some(&packed_uniform),
        )
        .expect_err("source hash mismatch should fail");

        assert!(matches!(
            err,
            OcioGpuWgpuBindResourcePlanError::TextureContractMismatch {
                dimension: OcioGpuWgpuLutTextureDimension::D2,
                index: 7,
                reason: OcioGpuWgpuTextureContractMismatch::SourceValuesHash { .. }
            }
        ));
    }

    #[test]
    fn bind_resource_plan_requires_uniform_buffer_when_contract_needs_it() {
        let resources = bind_resource_test_plan(34);
        let packed_luts = packed_luts_for_bind_resource(34);

        let err =
            OcioGpuWgpuBindResourcePlan::from_packed_resources(&resources, &packed_luts, None)
                .expect_err("missing uniform buffer should fail");

        assert!(matches!(
            err,
            OcioGpuWgpuBindResourcePlanError::MissingUniformBuffer { binding: 0 }
        ));
    }

    #[test]
    fn bind_resource_plan_rejects_uniform_resource_key_mismatch() {
        let resources = bind_resource_test_plan(35);
        let packed_luts = packed_luts_for_bind_resource(35);
        let packed_uniform = packed_uniform_for_bind_resource(36);

        let err = OcioGpuWgpuBindResourcePlan::from_packed_resources(
            &resources,
            &packed_luts,
            Some(&packed_uniform),
        )
        .expect_err("uniform resource key mismatch should fail");

        assert!(matches!(
            err,
            OcioGpuWgpuBindResourcePlanError::UniformResourceKeyMismatch {
                expected: 35,
                actual: 36
            }
        ));
    }

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
            (first.texture_2d_count + first.texture_3d_count) * 2
                + u32::from(first.uniform_count > 0)
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
        let binding_layout =
            resources.binding_layout_plan().expect("binding layout with separated samplers");
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
            assert!(binding_layout.entries.iter().any(|entry| {
                entry.resource
                    == OcioGpuWgpuBindingResource::OcioLutSampler2d { index: texture.index }
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
            assert!(binding_layout.entries.iter().any(|entry| {
                entry.resource
                    == OcioGpuWgpuBindingResource::OcioLutSampler3d { index: texture.index }
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
        let ocio_descriptor =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_ocio_resources(&binding_layout);
        assert_eq!(ocio_descriptor.bind_group, binding_layout.bind_group);
        assert_eq!(ocio_descriptor.entries.len(), binding_layout.entries.len());
        if first.texture_2d_count > 0 || first.texture_3d_count > 0 {
            assert!(ocio_descriptor.entries.iter().any(|entry| {
                matches!(
                    entry.resource,
                    OcioGpuWgpuLayoutBindingResource::Sampler {
                        filtering: OcioGpuWgpuSamplerFiltering::NonFiltering
                    }
                )
            }));
        }
        assert_ne!(ocio_descriptor.layout_hash, 0);
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
            OcioGpuWgpuBlocker::ShaderModuleNotPrepared { language: GpuLanguage::Glsl4_0 }
        )));
        assert!(prepared.blockers.iter().any(|blocker| matches!(
            blocker,
            OcioGpuWgpuBlocker::OcioResourceBindGroupNotPrepared { .. }
        )));
        assert!(prepared.blockers.contains(&OcioGpuWgpuBlocker::FullscreenWrapperNotPrepared));
        assert!(prepared.blockers.contains(&OcioGpuWgpuBlocker::RenderPipelineNotPrepared));

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
        let first = resource_cache
            .prepare(prepared.resources.clone())
            .expect("prepare binding layout");
        let second = resource_cache
            .prepare(prepared.resources.clone())
            .expect("reuse binding layout");

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
            prepared.resources.binding_layout_plan().expect("binding layout").layout_hash
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
            uniforms: Vec::new(),
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
            uniforms: Vec::new(),
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
            uniforms: Vec::new(),
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
