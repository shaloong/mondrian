use lru::LruCache;
#[cfg(test)]
use mondrian_core::ColorSpace;
use mondrian_core::{
    extract_ocio_display_identity_gpu_shader_bundle, extract_ocio_identity_gpu_shader_bundle,
    GpuLanguage, OcioColorSpaceIdentity, OcioGpuShaderBundle, OcioGpuTextureChannel,
    OcioGpuTextureDimensions, OcioGpuTextureInterpolation, OcioGpuUniformType, OcioGpuUniformValue,
    MONDRIAN_OCIO_GPU_FUNCTION_NAME, MONDRIAN_OCIO_GPU_PIXEL_NAME,
    MONDRIAN_OCIO_GPU_RESOURCE_PREFIX,
};
use std::borrow::Cow;
use std::collections::{hash_map::DefaultHasher, BTreeSet};
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Arc;

/// Shader stage used when translating OCIO GPU shader text for wgpu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuShaderStage {
    /// Vertex shader stage.
    Vertex,
    /// Fragment shader stage.
    Fragment,
}

impl OcioGpuShaderStage {
    fn to_naga(self) -> naga::ShaderStage {
        match self {
            Self::Vertex => naga::ShaderStage::Vertex,
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
        src: OcioColorSpaceIdentity,
        dst: OcioColorSpaceIdentity,
        language: GpuLanguage,
    },
    /// Convert a source color space through an OCIO display/view transform.
    DisplayView {
        src: OcioColorSpaceIdentity,
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

/// Shape of an OCIO-generated GPU program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuGeneratedProgramSourceKind {
    /// OCIO emitted a callable color function/program without a fragment entry point.
    CallableFunction,
    /// OCIO emitted a complete fragment shader entry point.
    CompleteFragmentShader,
    /// The generated source does not match a known linkable shape.
    Unknown,
}

/// Callable signature style emitted by OCIO for the generated program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuGeneratedProgramCallStyle {
    /// The OCIO function returns the transformed pixel.
    ReturnsVec4,
    /// The OCIO function mutates its pixel argument in place.
    MutatesInOut,
    /// Mondrian cannot safely infer how to call this function.
    Unknown,
}

/// Diagnostic observed while analyzing an OCIO-generated GPU program.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OcioGpuGeneratedProgramDiagnostic {
    /// The expected OCIO function name is not present in the generated source.
    MissingFunctionName { function_name: String },
    /// The expected OCIO pixel variable name is not present in the generated source.
    MissingPixelName { pixel_name: String },
    /// The generated source already contains a fragment `main` entry point.
    ContainsFragmentMain,
}

/// Renderer-facing contract for the OCIO-generated GPU program.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuGeneratedProgramContract {
    /// Stable hash of the OCIO-generated shader source.
    pub shader_hash: u64,
    /// OCIO function name configured in `mondrian-core`.
    pub function_name: String,
    /// OCIO pixel variable name configured in `mondrian-core`.
    pub pixel_name: String,
    /// OCIO resource symbol prefix configured in `mondrian-core`.
    pub resource_prefix: String,
    /// Detected source shape.
    pub source_kind: OcioGpuGeneratedProgramSourceKind,
    /// Detected callable function signature style.
    pub call_style: OcioGpuGeneratedProgramCallStyle,
    /// Whether the expected function name appears in the source.
    pub function_present: bool,
    /// Whether the expected pixel variable name appears in the source.
    pub pixel_name_present: bool,
    /// Whether the source contains a fragment `main` entry point.
    pub main_function_present: bool,
    /// Diagnostics that affect wrapper-link readiness.
    pub diagnostics: Vec<OcioGpuGeneratedProgramDiagnostic>,
}

impl OcioGpuGeneratedProgramContract {
    /// Analyze the OCIO-generated shader source carried by a shader plan.
    pub fn for_shader_plan(plan: &OcioGpuShaderPlan) -> Self {
        Self::analyze(plan.shader_hash, &plan.bundle().shader_text)
    }

    /// Analyze raw OCIO-generated shader source.
    pub fn analyze(shader_hash: u64, shader_text: &str) -> Self {
        let function_name = MONDRIAN_OCIO_GPU_FUNCTION_NAME.to_owned();
        let pixel_name = MONDRIAN_OCIO_GPU_PIXEL_NAME.to_owned();
        let resource_prefix = MONDRIAN_OCIO_GPU_RESOURCE_PREFIX.to_owned();
        let function_present = shader_text.contains(MONDRIAN_OCIO_GPU_FUNCTION_NAME);
        let pixel_name_present = shader_text.contains(MONDRIAN_OCIO_GPU_PIXEL_NAME);
        let main_function_present = contains_glsl_main(shader_text);
        let call_style = glsl_function_call_style(shader_text, MONDRIAN_OCIO_GPU_FUNCTION_NAME);
        let source_kind = match (function_present, main_function_present) {
            (true, false) => OcioGpuGeneratedProgramSourceKind::CallableFunction,
            (true, true) => OcioGpuGeneratedProgramSourceKind::CompleteFragmentShader,
            (false, true) => OcioGpuGeneratedProgramSourceKind::CompleteFragmentShader,
            (false, false) => OcioGpuGeneratedProgramSourceKind::Unknown,
        };
        let mut diagnostics = Vec::new();
        if !function_present {
            diagnostics.push(OcioGpuGeneratedProgramDiagnostic::MissingFunctionName {
                function_name: function_name.clone(),
            });
        }
        if !pixel_name_present {
            diagnostics.push(OcioGpuGeneratedProgramDiagnostic::MissingPixelName {
                pixel_name: pixel_name.clone(),
            });
        }
        if main_function_present {
            diagnostics.push(OcioGpuGeneratedProgramDiagnostic::ContainsFragmentMain);
        }
        Self {
            shader_hash,
            function_name,
            pixel_name,
            resource_prefix,
            source_kind,
            call_style,
            function_present,
            pixel_name_present,
            main_function_present,
            diagnostics,
        }
    }

    /// Whether Mondrian can link this program into its fullscreen wrapper.
    pub fn is_wrapper_linkable(&self) -> bool {
        self.function_present
            && self.pixel_name_present
            && self.call_style != OcioGpuGeneratedProgramCallStyle::Unknown
            && self.source_kind == OcioGpuGeneratedProgramSourceKind::CallableFunction
    }
}

/// Missing piece before Mondrian can link an OCIO program into its fullscreen wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuWrapperLinkBlocker {
    /// OCIO did not emit the configured callable function name.
    MissingFunctionName { function_name: String },
    /// OCIO did not emit the configured pixel variable name.
    MissingPixelName { pixel_name: String },
    /// OCIO emitted a complete fragment shader, not a callable wrapper program.
    CompleteFragmentShaderRequiresSplit,
    /// The generated program source shape is unknown.
    UnknownProgramShape,
    /// The generated callable function signature is unknown.
    UnknownFunctionCallStyle,
}

/// Pure link plan between the OCIO generated program and Mondrian's fullscreen wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuWrapperLinkPlan {
    /// Stable resource key this link plan belongs to.
    pub resource_key: u64,
    /// Stable hash of the OCIO-generated shader source.
    pub shader_hash: u64,
    /// OCIO generated program contract.
    pub program_contract: OcioGpuGeneratedProgramContract,
    /// Fullscreen wrapper shader contract.
    pub shader_contract: OcioGpuWgpuFullscreenShaderContract,
    /// Blockers that prevent wrapper shader generation.
    pub blockers: Vec<OcioGpuWgpuWrapperLinkBlocker>,
    /// Stable hash of the link plan.
    pub link_hash: u64,
}

impl OcioGpuWgpuWrapperLinkPlan {
    /// Build a wrapper-link plan from a shader plan and renderer resource contract.
    pub fn for_shader_plan(
        shader_plan: &OcioGpuShaderPlan,
        resources: &OcioGpuWgpuResourcePlan,
    ) -> Self {
        let program_contract = OcioGpuGeneratedProgramContract::for_shader_plan(shader_plan);
        let shader_contract =
            OcioGpuWgpuFullscreenShaderContract::for_wrapper_contract(&resources.wrapper_contract);
        let blockers = wrapper_link_blockers(&program_contract);
        let link_hash = hash_wrapper_link_plan(
            resources.resource_key,
            shader_plan.shader_hash,
            &program_contract,
            &shader_contract,
        );
        Self {
            resource_key: resources.resource_key,
            shader_hash: shader_plan.shader_hash,
            program_contract,
            shader_contract,
            blockers,
            link_hash,
        }
    }

    /// Whether this link plan can generate a wrapper shader.
    pub fn can_link(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Error returned when a wrapper shader source artifact cannot be generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuWrapperShaderArtifactError {
    /// The wrapper link plan still has blockers.
    LinkPlanBlocked {
        /// Blockers reported by the link plan.
        blockers: Vec<OcioGpuWgpuWrapperLinkBlocker>,
    },
    /// The shader plan does not match the wrapper link plan's shader hash.
    ShaderHashMismatch { expected: u64, actual: u64 },
    /// The OCIO generated program could not be lowered into wgpu-compatible GLSL.
    SourceLoweringFailed { reason: String },
}

/// GLSL source artifact for the future OCIO fullscreen wrapper shader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcioGpuWgpuWrapperShaderSourceArtifact {
    /// Stable resource key this shader belongs to.
    pub resource_key: u64,
    /// Hash of the wrapper-link plan.
    pub link_hash: u64,
    /// Hash of the OCIO-generated program source.
    pub ocio_shader_hash: u64,
    /// Hash of the stage-split wrapper shader sources.
    pub source_hash: u64,
    /// Hash of the fullscreen vertex shader source.
    pub vertex_source_hash: u64,
    /// Hash of the OCIO fragment wrapper shader source.
    pub fragment_source_hash: u64,
    /// Fullscreen vertex shader source.
    pub vertex_source: String,
    /// Fragment shader source that calls the OCIO-generated program.
    pub fragment_source: String,
    /// Combined source text for diagnostics only. This is not an execution artifact.
    pub debug_combined_source: String,
    /// Vertex entry point expected by the render pipeline.
    pub vertex_entry_point: String,
    /// Fragment entry point expected by the render pipeline.
    pub fragment_entry_point: String,
    /// Output location written by the fragment shader.
    pub output_location: u32,
    /// Non-fatal diagnostics recorded while generating the artifact.
    pub diagnostics: Vec<OcioGpuShaderDiagnostic>,
}

impl OcioGpuWgpuWrapperShaderSourceArtifact {
    /// Generate stage-split fullscreen wrapper GLSL source from a linkable OCIO program.
    pub fn generate(
        shader_plan: &OcioGpuShaderPlan,
        link_plan: &OcioGpuWgpuWrapperLinkPlan,
    ) -> Result<Self, OcioGpuWgpuWrapperShaderArtifactError> {
        if !link_plan.blockers.is_empty() {
            return Err(OcioGpuWgpuWrapperShaderArtifactError::LinkPlanBlocked {
                blockers: link_plan.blockers.clone(),
            });
        }
        if shader_plan.shader_hash != link_plan.shader_hash {
            return Err(OcioGpuWgpuWrapperShaderArtifactError::ShaderHashMismatch {
                expected: link_plan.shader_hash,
                actual: shader_plan.shader_hash,
            });
        }

        let sources = build_wrapper_shader_sources(shader_plan, link_plan)?;
        let vertex_source_hash = hash_value(&sources.vertex_source);
        let fragment_source_hash = hash_value(&sources.fragment_source);
        let source_hash = hash_value(&(vertex_source_hash, fragment_source_hash));
        Ok(Self {
            resource_key: link_plan.resource_key,
            link_hash: link_plan.link_hash,
            ocio_shader_hash: shader_plan.shader_hash,
            source_hash,
            vertex_source_hash,
            fragment_source_hash,
            vertex_source: sources.vertex_source,
            fragment_source: sources.fragment_source,
            debug_combined_source: sources.debug_combined_source,
            vertex_entry_point: link_plan.shader_contract.vertex_entry_point.clone(),
            fragment_entry_point: link_plan.shader_contract.fragment_entry_point.clone(),
            output_location: link_plan.shader_contract.output_location,
            diagnostics: Vec::new(),
        })
    }
}

/// Error returned when wrapper shader sources cannot become validated Naga modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuWrapperShaderModuleArtifactError {
    /// The wrapper source artifact hash does not match its stage source hashes.
    SourceHashMismatch { expected: u64, actual: u64 },
    /// The pipeline layout belongs to a different resource key.
    PipelineLayoutResourceKeyMismatch { expected: u64, actual: u64 },
    /// The render descriptor belongs to a different resource key.
    RenderDescriptorResourceKeyMismatch { expected: u64, actual: u64 },
    /// The render descriptor does not reference the provided pipeline layout.
    PipelineLayoutHashMismatch { expected: u64, actual: u64 },
    /// The wrapper vertex entry point differs from the render descriptor contract.
    VertexEntryPointMismatch { expected: String, actual: String },
    /// The wrapper fragment entry point differs from the render descriptor contract.
    FragmentEntryPointMismatch { expected: String, actual: String },
    /// The wrapper output location differs from the render descriptor contract.
    OutputLocationMismatch { expected: u32, actual: u32 },
    /// Naga could not translate or validate the generated vertex wrapper source.
    VertexTranslationFailed {
        reason: OcioGpuShaderTranslationFailure,
    },
    /// Naga could not translate or validate the generated fragment wrapper source.
    FragmentTranslationFailed {
        reason: OcioGpuShaderTranslationFailure,
    },
}

/// Validated Naga module artifact for Mondrian's OCIO fullscreen wrapper pass.
#[derive(Debug, Clone)]
pub struct OcioGpuWgpuWrapperShaderModuleArtifact {
    /// Stable resource key this wrapper belongs to.
    pub resource_key: u64,
    /// Hash of the wrapper-link plan.
    pub link_hash: u64,
    /// Hash of the stage-split wrapper shader sources.
    pub source_hash: u64,
    /// Hash of the pipeline layout contract.
    pub pipeline_layout_hash: u64,
    /// Hash of the render-pipeline descriptor contract.
    pub render_descriptor_hash: u64,
    /// Output target format bound to this module artifact.
    pub output_format: OcioGpuWgpuColorTargetFormat,
    /// Stable cache key for this wrapper module artifact.
    pub module_key: u64,
    /// Validated fullscreen vertex shader module.
    pub vertex: OcioGpuNagaShaderStageArtifact,
    /// Validated fragment shader module that calls the OCIO-generated function.
    pub fragment: OcioGpuNagaShaderStageArtifact,
}

impl OcioGpuWgpuWrapperShaderModuleArtifact {
    /// Translate a wrapper source artifact into validated stage-split Naga modules.
    pub fn translate(
        source: &OcioGpuWgpuWrapperShaderSourceArtifact,
        pipeline_layout: &OcioGpuWgpuPipelineLayoutPlan,
        render_descriptor: &OcioGpuWgpuRenderPipelineDescriptorPlan,
    ) -> Result<Self, OcioGpuWgpuWrapperShaderModuleArtifactError> {
        let module_key =
            wrapper_shader_module_artifact_key(source, pipeline_layout, render_descriptor)?;
        let vertex = translate_naga_shader_stage(
            GpuLanguage::Glsl4_0,
            OcioGpuShaderTargetLanguage::NagaIr,
            OcioGpuShaderStage::Vertex,
            source.vertex_source_hash,
            &source.vertex_source,
        )
        .map_err(|reason| {
            OcioGpuWgpuWrapperShaderModuleArtifactError::VertexTranslationFailed { reason }
        })?;
        let fragment = translate_naga_shader_stage(
            GpuLanguage::Glsl4_0,
            OcioGpuShaderTargetLanguage::NagaIr,
            OcioGpuShaderStage::Fragment,
            source.fragment_source_hash,
            &source.fragment_source,
        )
        .map_err(|reason| {
            OcioGpuWgpuWrapperShaderModuleArtifactError::FragmentTranslationFailed { reason }
        })?;

        Ok(Self {
            resource_key: source.resource_key,
            link_hash: source.link_hash,
            source_hash: source.source_hash,
            pipeline_layout_hash: pipeline_layout.layout_hash,
            render_descriptor_hash: render_descriptor.descriptor_hash,
            output_format: render_descriptor.output_format,
            module_key,
            vertex,
            fragment,
        })
    }
}

/// Point-in-time wrapper shader module artifact cache diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuWgpuWrapperShaderModuleArtifactCacheDiagnostics {
    /// Cached wrapper module artifacts.
    pub entries: usize,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Contract or translation failures before caching.
    pub failures: u64,
}

/// Bounded cache for stage-split wrapper shader Naga artifacts.
pub struct OcioGpuWgpuWrapperShaderModuleArtifactCache {
    entries: LruCache<u64, Arc<OcioGpuWgpuWrapperShaderModuleArtifact>>,
    hits: u64,
    misses: u64,
    failures: u64,
}

impl OcioGpuWgpuWrapperShaderModuleArtifactCache {
    /// Create a cache with a fixed non-zero capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: LruCache::new(capacity),
            hits: 0,
            misses: 0,
            failures: 0,
        }
    }

    /// Translate and cache a wrapper source artifact for a specific pipeline contract.
    pub fn translate(
        &mut self,
        source: &OcioGpuWgpuWrapperShaderSourceArtifact,
        pipeline_layout: &OcioGpuWgpuPipelineLayoutPlan,
        render_descriptor: &OcioGpuWgpuRenderPipelineDescriptorPlan,
    ) -> Result<
        Arc<OcioGpuWgpuWrapperShaderModuleArtifact>,
        OcioGpuWgpuWrapperShaderModuleArtifactError,
    > {
        let key =
            match wrapper_shader_module_artifact_key(source, pipeline_layout, render_descriptor) {
                Ok(key) => key,
                Err(err) => {
                    self.failures = self.failures.saturating_add(1);
                    return Err(err);
                }
            };
        if let Some(hit) = self.entries.get(&key) {
            self.hits = self.hits.saturating_add(1);
            return Ok(Arc::clone(hit));
        }

        self.misses = self.misses.saturating_add(1);
        match OcioGpuWgpuWrapperShaderModuleArtifact::translate(
            source,
            pipeline_layout,
            render_descriptor,
        ) {
            Ok(module) => {
                let module = Arc::new(module);
                self.entries.put(key, Arc::clone(&module));
                Ok(module)
            }
            Err(err) => {
                self.failures = self.failures.saturating_add(1);
                Err(err)
            }
        }
    }

    /// Return wrapper shader module artifact cache diagnostics.
    pub fn diagnostics(&self) -> OcioGpuWgpuWrapperShaderModuleArtifactCacheDiagnostics {
        OcioGpuWgpuWrapperShaderModuleArtifactCacheDiagnostics {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
            failures: self.failures,
        }
    }
}

impl Default for OcioGpuWgpuWrapperShaderModuleArtifactCache {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(64).expect("default cache capacity is non-zero"))
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

/// One validated Naga shader stage produced from generated GPU source.
#[derive(Debug, Clone)]
pub struct OcioGpuNagaShaderStageArtifact {
    /// Source language parsed by Naga.
    pub source_language: GpuLanguage,
    /// Target language consumed by the renderer.
    pub target_language: OcioGpuShaderTargetLanguage,
    /// Shader stage represented by this module.
    pub stage: OcioGpuShaderStage,
    /// Stable hash of the parsed source text.
    pub source_hash: u64,
    /// Canonical Naga module used by `wgpu::ShaderSource::Naga`.
    pub naga_module: naga::Module,
    /// Canonical Naga validation info for the module.
    pub module_info: naga::valid::ModuleInfo,
    /// Optional WGSL debug output. This is not the execution artifact.
    pub debug_wgsl: Option<String>,
    /// Stable hash of the optional WGSL debug output.
    pub debug_wgsl_hash: Option<u64>,
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

    /// Validate this contract against the shader plan it was extracted from.
    pub fn validate_for_shader_plan(
        &self,
        shader_plan: &OcioGpuShaderPlan,
    ) -> Result<(), OcioGpuBindingContractValidationError> {
        let texture_2d_count = u32::try_from(self.textures_2d.len()).map_err(|_| {
            OcioGpuBindingContractValidationError::Texture2DCountMismatch {
                expected: shader_plan.texture_2d_count,
                actual: usize::MAX,
            }
        })?;
        if shader_plan.texture_2d_count != texture_2d_count {
            return Err(
                OcioGpuBindingContractValidationError::Texture2DCountMismatch {
                    expected: shader_plan.texture_2d_count,
                    actual: self.textures_2d.len(),
                },
            );
        }

        let texture_3d_count = u32::try_from(self.textures_3d.len()).map_err(|_| {
            OcioGpuBindingContractValidationError::Texture3DCountMismatch {
                expected: shader_plan.texture_3d_count,
                actual: usize::MAX,
            }
        })?;
        if shader_plan.texture_3d_count != texture_3d_count {
            return Err(
                OcioGpuBindingContractValidationError::Texture3DCountMismatch {
                    expected: shader_plan.texture_3d_count,
                    actual: self.textures_3d.len(),
                },
            );
        }

        let uniform_count = u32::try_from(self.uniforms.len()).map_err(|_| {
            OcioGpuBindingContractValidationError::UniformCountMismatch {
                expected: shader_plan.uniform_count,
                actual: usize::MAX,
            }
        })?;
        if shader_plan.uniform_count != self.uniform_count || self.uniform_count != uniform_count {
            return Err(
                OcioGpuBindingContractValidationError::UniformCountMismatch {
                    expected: shader_plan.uniform_count,
                    actual: self.uniforms.len(),
                },
            );
        }

        if self.texture_binding_start <= self.uniform_buffer_binding {
            return Err(
                OcioGpuBindingContractValidationError::TextureBindingStartOverlapsUniform {
                    uniform_binding: self.uniform_buffer_binding,
                    texture_binding_start: self.texture_binding_start,
                },
            );
        }

        if self.uniform_count > 0 && self.uniform_buffer_size == 0 {
            return Err(
                OcioGpuBindingContractValidationError::MissingUniformBuffer {
                    uniform_count: self.uniform_count,
                },
            );
        }

        let mut uniform_indices = BTreeSet::new();
        let mut resource_bindings = BTreeSet::new();
        if self.uniform_count > 0 || self.uniform_buffer_size > 0 {
            resource_bindings.insert(self.uniform_buffer_binding);
        }

        for uniform in &self.uniforms {
            if uniform.name.is_empty() {
                return Err(OcioGpuBindingContractValidationError::EmptyUniformName {
                    index: uniform.index,
                });
            }
            if !uniform_indices.insert(uniform.index) {
                return Err(
                    OcioGpuBindingContractValidationError::DuplicateUniformIndex {
                        index: uniform.index,
                    },
                );
            }
        }

        let mut texture_2d_indices = BTreeSet::new();
        for texture in &self.textures_2d {
            validate_texture_contract_names(
                OcioGpuWgpuLutTextureDimension::D2,
                texture.index,
                &texture.texture_name,
                &texture.sampler_name,
            )?;
            validate_texture_binding_index(
                OcioGpuWgpuLutTextureDimension::D2,
                texture.index,
                texture.binding_index,
                self.texture_binding_start,
                &mut resource_bindings,
            )?;
            if !texture_2d_indices.insert(texture.index) {
                return Err(
                    OcioGpuBindingContractValidationError::DuplicateTextureIndex {
                        dimension: OcioGpuWgpuLutTextureDimension::D2,
                        index: texture.index,
                    },
                );
            }
            if texture.width == 0 || texture.height == 0 {
                return Err(
                    OcioGpuBindingContractValidationError::InvalidTextureExtent {
                        dimension: OcioGpuWgpuLutTextureDimension::D2,
                        index: texture.index,
                    },
                );
            }
            if texture.value_count == 0 {
                return Err(OcioGpuBindingContractValidationError::EmptyTexturePayload {
                    dimension: OcioGpuWgpuLutTextureDimension::D2,
                    index: texture.index,
                });
            }
        }

        let mut texture_3d_indices = BTreeSet::new();
        for texture in &self.textures_3d {
            validate_texture_contract_names(
                OcioGpuWgpuLutTextureDimension::D3,
                texture.index,
                &texture.texture_name,
                &texture.sampler_name,
            )?;
            validate_texture_binding_index(
                OcioGpuWgpuLutTextureDimension::D3,
                texture.index,
                texture.binding_index,
                self.texture_binding_start,
                &mut resource_bindings,
            )?;
            if !texture_3d_indices.insert(texture.index) {
                return Err(
                    OcioGpuBindingContractValidationError::DuplicateTextureIndex {
                        dimension: OcioGpuWgpuLutTextureDimension::D3,
                        index: texture.index,
                    },
                );
            }
            if texture.edge_len == 0 {
                return Err(
                    OcioGpuBindingContractValidationError::InvalidTextureExtent {
                        dimension: OcioGpuWgpuLutTextureDimension::D3,
                        index: texture.index,
                    },
                );
            }
            if texture.value_count == 0 {
                return Err(OcioGpuBindingContractValidationError::EmptyTexturePayload {
                    dimension: OcioGpuWgpuLutTextureDimension::D3,
                    index: texture.index,
                });
            }
        }

        Ok(())
    }
}

/// Structural error in an OCIO GPU binding contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuBindingContractValidationError {
    /// OCIO-reported 2D texture count does not match copied texture resources.
    Texture2DCountMismatch {
        /// Count reported by the shader plan.
        expected: u32,
        /// Count present in the binding contract.
        actual: usize,
    },
    /// OCIO-reported 3D texture count does not match copied texture resources.
    Texture3DCountMismatch {
        /// Count reported by the shader plan.
        expected: u32,
        /// Count present in the binding contract.
        actual: usize,
    },
    /// OCIO-reported uniform count does not match copied uniform resources.
    UniformCountMismatch {
        /// Count reported by the shader plan.
        expected: u32,
        /// Count present in the binding contract.
        actual: usize,
    },
    /// OCIO texture bindings overlap the reserved uniform binding range.
    TextureBindingStartOverlapsUniform {
        /// Uniform buffer binding.
        uniform_binding: u32,
        /// First OCIO texture binding.
        texture_binding_start: u32,
    },
    /// OCIO reported uniforms without a backing uniform buffer.
    MissingUniformBuffer {
        /// Uniform count reported by OCIO.
        uniform_count: u32,
    },
    /// A uniform resource has no shader symbol name.
    EmptyUniformName {
        /// Uniform index.
        index: u32,
    },
    /// A uniform index appears more than once.
    DuplicateUniformIndex {
        /// Duplicate uniform index.
        index: u32,
    },
    /// A texture or sampler resource has no shader symbol name.
    EmptyTextureResourceName {
        /// Texture dimensionality.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Texture index.
        index: u32,
    },
    /// A texture binding is lower than OCIO's declared texture binding start.
    TextureBindingBeforeStart {
        /// Texture dimensionality.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Texture index.
        index: u32,
        /// Texture binding.
        binding: u32,
        /// First valid OCIO texture binding.
        texture_binding_start: u32,
    },
    /// Two OCIO resources use the same binding slot.
    DuplicateResourceBinding {
        /// Duplicate binding slot.
        binding: u32,
    },
    /// A texture index appears more than once within the same dimensionality.
    DuplicateTextureIndex {
        /// Texture dimensionality.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Duplicate texture index.
        index: u32,
    },
    /// A texture resource has an invalid zero extent.
    InvalidTextureExtent {
        /// Texture dimensionality.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Texture index.
        index: u32,
    },
    /// A texture resource has no LUT payload.
    EmptyTexturePayload {
        /// Texture dimensionality.
        dimension: OcioGpuWgpuLutTextureDimension,
        /// Texture index.
        index: u32,
    },
}

impl std::fmt::Display for OcioGpuBindingContractValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid OCIO GPU binding contract: {self:?}")
    }
}

impl std::error::Error for OcioGpuBindingContractValidationError {}

fn validate_texture_contract_names(
    dimension: OcioGpuWgpuLutTextureDimension,
    index: u32,
    texture_name: &str,
    sampler_name: &str,
) -> Result<(), OcioGpuBindingContractValidationError> {
    if texture_name.is_empty() || sampler_name.is_empty() {
        return Err(
            OcioGpuBindingContractValidationError::EmptyTextureResourceName { dimension, index },
        );
    }
    Ok(())
}

fn validate_texture_binding_index(
    dimension: OcioGpuWgpuLutTextureDimension,
    index: u32,
    binding: u32,
    texture_binding_start: u32,
    resource_bindings: &mut BTreeSet<u32>,
) -> Result<(), OcioGpuBindingContractValidationError> {
    if binding < texture_binding_start {
        return Err(
            OcioGpuBindingContractValidationError::TextureBindingBeforeStart {
                dimension,
                index,
                binding,
                texture_binding_start,
            },
        );
    }
    if !resource_bindings.insert(binding) {
        return Err(OcioGpuBindingContractValidationError::DuplicateResourceBinding { binding });
    }
    Ok(())
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
    pub fn for_shader_plan(
        shader_plan: &OcioGpuShaderPlan,
    ) -> Result<Self, OcioGpuBindingContractValidationError> {
        let binding_contract = binding_contract_for_plan(shader_plan);
        binding_contract.validate_for_shader_plan(shader_plan)?;
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
        Ok(Self {
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
        })
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

    /// An empty resource plan for blocked GPU execution.
    ///
    /// Used when shader extraction fails and a blocked plan must be returned
    /// with the appropriate blocker recorded instead of propagating an error.
    pub fn empty() -> Self {
        Self {
            resource_key: 0,
            shader_hash: 0,
            binding_contract_hash: 0,
            binding_contract: OcioGpuBindingContract {
                descriptor_set_index: 0,
                uniform_buffer_binding: 0,
                texture_binding_start: 0,
                uniform_buffer_size: 0,
                uniform_count: 0,
                uniforms: Vec::new(),
                textures_2d: Vec::new(),
                textures_3d: Vec::new(),
            },
            wrapper_contract: OcioGpuFullscreenWrapperContract {
                bind_group: 0,
                input_texture_binding: 0,
                input_sampler_binding: 0,
                output_location: 0,
            },
            input_textures: 0,
            output_textures: 0,
            ocio_texture_2d_bindings: 0,
            ocio_texture_3d_bindings: 0,
            uniform_buffers: 0,
            samplers: 0,
            bind_group_entries: 0,
            bind_groups: 0,
            pipeline_layout_hash: 0,
        }
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
                filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
            });
        }

        for texture in &plan.binding_contract.textures_2d {
            let filtering = sampler_filtering_for_interpolation(texture.interpolation);
            entries.push(OcioGpuWgpuBindingPlan {
                binding: texture.binding_index,
                resource: OcioGpuWgpuBindingResource::OcioLutTexture2d { index: texture.index },
                filtering,
            });
            entries.push(OcioGpuWgpuBindingPlan {
                binding: sampler_policy.sampler_binding_for_texture(
                    OcioGpuWgpuLutTextureDimension::D2,
                    texture.index,
                )?,
                resource: OcioGpuWgpuBindingResource::OcioLutSampler2d { index: texture.index },
                filtering,
            });
        }

        for texture in &plan.binding_contract.textures_3d {
            let filtering = sampler_filtering_for_interpolation(texture.interpolation);
            entries.push(OcioGpuWgpuBindingPlan {
                binding: texture.binding_index,
                resource: OcioGpuWgpuBindingResource::OcioLutTexture3d { index: texture.index },
                filtering,
            });
            entries.push(OcioGpuWgpuBindingPlan {
                binding: sampler_policy.sampler_binding_for_texture(
                    OcioGpuWgpuLutTextureDimension::D3,
                    texture.index,
                )?,
                resource: OcioGpuWgpuBindingResource::OcioLutSampler3d { index: texture.index },
                filtering,
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
                    entry.filtering,
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
    fn from_ocio_binding_resource(
        resource: OcioGpuWgpuBindingResource,
        filtering: OcioGpuWgpuSamplerFiltering,
    ) -> Self {
        match resource {
            OcioGpuWgpuBindingResource::OcioUniformBuffer { .. } => {
                Self::UniformBuffer { min_binding_size: None }
            }
            OcioGpuWgpuBindingResource::OcioLutTexture2d { .. } => Self::SampledTexture {
                dimension: OcioGpuWgpuLutTextureDimension::D2,
                sample_type: OcioGpuWgpuTextureSampleType::Float32 {
                    filterable: filtering == OcioGpuWgpuSamplerFiltering::Filtering,
                },
            },
            OcioGpuWgpuBindingResource::OcioLutTexture3d { .. } => Self::SampledTexture {
                dimension: OcioGpuWgpuLutTextureDimension::D3,
                sample_type: OcioGpuWgpuTextureSampleType::Float32 {
                    filterable: filtering == OcioGpuWgpuSamplerFiltering::Filtering,
                },
            },
            OcioGpuWgpuBindingResource::OcioLutSampler2d { .. }
            | OcioGpuWgpuBindingResource::OcioLutSampler3d { .. } => Self::Sampler { filtering },
            OcioGpuWgpuBindingResource::InputFrameTexture { .. } => Self::SampledTexture {
                dimension: OcioGpuWgpuLutTextureDimension::D2,
                sample_type: OcioGpuWgpuTextureSampleType::Float32 { filterable: false },
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
                sample_type: OcioGpuWgpuTextureSampleType::Float32 { filterable: false },
            },
            OcioGpuWgpuWrapperBindingResource::InputFrameSampler => Self::Sampler {
                filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
            },
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
    Float32 {
        /// Whether the bind-group layout permits hardware filtering.
        filterable: bool,
    },
}

impl OcioGpuWgpuTextureSampleType {
    fn to_wgpu(self) -> wgpu::TextureSampleType {
        match self {
            Self::Float32 { filterable } => wgpu::TextureSampleType::Float { filterable },
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
                    filtering: sampler_filtering_for_interpolation(texture.interpolation),
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
                    filtering: sampler_filtering_for_interpolation(texture.interpolation),
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

/// Wrapper input resources borrowed while creating the fullscreen wrapper bind group.
pub struct OcioGpuWgpuWrapperInputResources<'a> {
    /// GPU view for the input frame texture.
    pub input_texture_view: &'a wgpu::TextureView,
    /// Sampler used to read the input frame.
    pub input_sampler: &'a wgpu::Sampler,
}

/// Concrete OCIO resource bind group created from validated uploaded resources.
pub struct OcioGpuWgpuOcioBindGroup {
    /// OCIO resource bind group index.
    pub bind_group_index: u32,
    /// Stable resource key this bind group belongs to.
    pub resource_key: u64,
    /// Hash of the bind-resource plan used to create it.
    pub bind_resource_plan_hash: u64,
    /// Hash of the OCIO bind-group layout.
    pub layout_hash: u64,
    /// Concrete wgpu bind-group layout.
    pub layout: wgpu::BindGroupLayout,
    /// Concrete wgpu bind group.
    pub bind_group: wgpu::BindGroup,
}

/// Concrete wrapper input bind group for the fullscreen OCIO pass.
pub struct OcioGpuWgpuWrapperBindGroup {
    /// Wrapper bind group index.
    pub bind_group_index: u32,
    /// Hash of the wrapper bind-group layout.
    pub layout_hash: u64,
    /// Concrete wgpu bind-group layout.
    pub layout: wgpu::BindGroupLayout,
    /// Concrete wgpu bind group.
    pub bind_group: wgpu::BindGroup,
}

/// Pure pipeline-layout contract for an OCIO fullscreen color pass.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuPipelineLayoutPlan {
    /// Stable resource key this pipeline layout belongs to.
    pub resource_key: u64,
    /// Hash of the OCIO resource bind-group layout.
    pub ocio_layout_hash: u64,
    /// Hash of the wrapper input bind-group layout.
    pub wrapper_layout_hash: u64,
    /// Ordered bind-group slots used by the future render pipeline.
    pub bind_groups: Vec<OcioGpuWgpuPipelineBindGroupSlot>,
    /// Stable hash of this pipeline-layout plan.
    pub layout_hash: u64,
}

impl OcioGpuWgpuPipelineLayoutPlan {
    /// Build a pipeline-layout plan from validated bind-group contracts.
    pub fn for_bind_groups(
        resources: &OcioGpuWgpuResourcePlan,
        ocio_layout: &OcioGpuWgpuBindingLayoutPlan,
        wrapper_layout: &OcioGpuWgpuWrapperBindingPlan,
    ) -> Self {
        let ocio_layout_descriptor =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_ocio_resources(ocio_layout);
        let wrapper_layout_descriptor =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_wrapper_input(wrapper_layout);
        let mut bind_groups = vec![
            OcioGpuWgpuPipelineBindGroupSlot {
                bind_group: ocio_layout.bind_group,
                resource: OcioGpuWgpuPipelineBindGroupResource::OcioResources,
                layout_hash: ocio_layout_descriptor.layout_hash,
            },
            OcioGpuWgpuPipelineBindGroupSlot {
                bind_group: wrapper_layout.bind_group,
                resource: OcioGpuWgpuPipelineBindGroupResource::WrapperInput,
                layout_hash: wrapper_layout_descriptor.layout_hash,
            },
        ];
        bind_groups.sort_by_key(|slot| slot.bind_group);
        let layout_hash = hash_pipeline_layout_plan(
            resources.resource_key,
            ocio_layout_descriptor.layout_hash,
            wrapper_layout_descriptor.layout_hash,
            &bind_groups,
        );
        Self {
            resource_key: resources.resource_key,
            ocio_layout_hash: ocio_layout_descriptor.layout_hash,
            wrapper_layout_hash: wrapper_layout_descriptor.layout_hash,
            bind_groups,
            layout_hash,
        }
    }
}

/// One bind-group slot in the OCIO fullscreen pipeline layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuPipelineBindGroupSlot {
    /// Bind group index.
    pub bind_group: u32,
    /// Resource class bound at this index.
    pub resource: OcioGpuWgpuPipelineBindGroupResource,
    /// Hash of the bind-group layout at this slot.
    pub layout_hash: u64,
}

/// Resource class for a pipeline-layout bind-group slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuPipelineBindGroupResource {
    /// OCIO LUT/uniform resources.
    OcioResources,
    /// Mondrian input frame texture/sampler resources.
    WrapperInput,
}

/// Concrete wgpu pipeline layout for an OCIO fullscreen color pass.
pub struct OcioGpuWgpuPipelineLayout {
    /// Stable resource key this pipeline layout belongs to.
    pub resource_key: u64,
    /// Hash of the pure pipeline-layout plan.
    pub layout_hash: u64,
    /// Concrete wgpu pipeline layout.
    pub pipeline_layout: wgpu::PipelineLayout,
}

/// Error returned when a concrete pipeline layout cannot satisfy the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuPipelineLayoutError {
    /// The OCIO bind group belongs to a different resource plan.
    ResourceKeyMismatch { expected: u64, actual: u64 },
    /// The OCIO bind-group layout hash does not match the plan.
    OcioLayoutHashMismatch { expected: u64, actual: u64 },
    /// The wrapper bind-group layout hash does not match the plan.
    WrapperLayoutHashMismatch { expected: u64, actual: u64 },
    /// A bind group index cannot be represented by the backend layout vector.
    BindGroupIndexOverflow { bind_group: u32 },
}

/// Stateless backend preparer for OCIO pipeline layouts.
pub struct OcioGpuWgpuPipelineLayoutPreparer;

impl OcioGpuWgpuPipelineLayoutPreparer {
    /// Create a concrete wgpu pipeline layout from prepared OCIO/wrapper bind groups.
    pub fn prepare(
        device: &wgpu::Device,
        plan: &OcioGpuWgpuPipelineLayoutPlan,
        ocio_bind_group: &OcioGpuWgpuOcioBindGroup,
        wrapper_bind_group: &OcioGpuWgpuWrapperBindGroup,
    ) -> Result<OcioGpuWgpuPipelineLayout, OcioGpuWgpuPipelineLayoutError> {
        if plan.resource_key != ocio_bind_group.resource_key {
            return Err(OcioGpuWgpuPipelineLayoutError::ResourceKeyMismatch {
                expected: plan.resource_key,
                actual: ocio_bind_group.resource_key,
            });
        }
        if plan.ocio_layout_hash != ocio_bind_group.layout_hash {
            return Err(OcioGpuWgpuPipelineLayoutError::OcioLayoutHashMismatch {
                expected: plan.ocio_layout_hash,
                actual: ocio_bind_group.layout_hash,
            });
        }
        if plan.wrapper_layout_hash != wrapper_bind_group.layout_hash {
            return Err(OcioGpuWgpuPipelineLayoutError::WrapperLayoutHashMismatch {
                expected: plan.wrapper_layout_hash,
                actual: wrapper_bind_group.layout_hash,
            });
        }

        let bind_group_layouts =
            pipeline_layout_bind_group_layouts(plan, ocio_bind_group, wrapper_bind_group)?;
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ocio_fullscreen_pipeline_layout"),
            immediate_size: 0,
            bind_group_layouts: &bind_group_layouts,
        });
        Ok(OcioGpuWgpuPipelineLayout {
            resource_key: plan.resource_key,
            layout_hash: plan.layout_hash,
            pipeline_layout,
        })
    }

    /// Create a concrete wgpu pipeline layout from stable prepared layouts.
    pub fn prepare_with_wrapper_layout(
        device: &wgpu::Device,
        plan: &OcioGpuWgpuPipelineLayoutPlan,
        ocio_bind_group: &OcioGpuWgpuOcioBindGroup,
        wrapper_layout_hash: u64,
        wrapper_layout: &wgpu::BindGroupLayout,
    ) -> Result<OcioGpuWgpuPipelineLayout, OcioGpuWgpuPipelineLayoutError> {
        if plan.resource_key != ocio_bind_group.resource_key {
            return Err(OcioGpuWgpuPipelineLayoutError::ResourceKeyMismatch {
                expected: plan.resource_key,
                actual: ocio_bind_group.resource_key,
            });
        }
        if plan.ocio_layout_hash != ocio_bind_group.layout_hash {
            return Err(OcioGpuWgpuPipelineLayoutError::OcioLayoutHashMismatch {
                expected: plan.ocio_layout_hash,
                actual: ocio_bind_group.layout_hash,
            });
        }
        if plan.wrapper_layout_hash != wrapper_layout_hash {
            return Err(OcioGpuWgpuPipelineLayoutError::WrapperLayoutHashMismatch {
                expected: plan.wrapper_layout_hash,
                actual: wrapper_layout_hash,
            });
        }

        let bind_group_layouts = pipeline_layout_bind_group_layouts_from_wrapper_layout(
            plan,
            ocio_bind_group,
            wrapper_layout,
        )?;
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ocio_fullscreen_pipeline_layout"),
            immediate_size: 0,
            bind_group_layouts: &bind_group_layouts,
        });
        Ok(OcioGpuWgpuPipelineLayout {
            resource_key: plan.resource_key,
            layout_hash: plan.layout_hash,
            pipeline_layout,
        })
    }
}

/// Fullscreen wrapper shader contract for an OCIO render pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuFullscreenShaderContract {
    /// Bind group used by wrapper input resources.
    pub wrapper_bind_group: u32,
    /// Input frame texture binding inside the wrapper bind group.
    pub input_texture_binding: u32,
    /// Input frame sampler binding inside the wrapper bind group.
    pub input_sampler_binding: u32,
    /// Vertex entry point owned by Mondrian.
    pub vertex_entry_point: String,
    /// Fragment entry point owned by Mondrian.
    pub fragment_entry_point: String,
    /// Draw topology used by the fullscreen pass.
    pub topology: OcioGpuWgpuFullscreenTopology,
    /// Fragment output location.
    pub output_location: u32,
    /// Whether the fragment wrapper still needs to link/call the OCIO program.
    pub requires_ocio_program_link: bool,
}

impl OcioGpuWgpuFullscreenShaderContract {
    /// Build the default fullscreen wrapper shader contract.
    pub fn for_wrapper_contract(contract: &OcioGpuFullscreenWrapperContract) -> Self {
        Self {
            wrapper_bind_group: contract.bind_group,
            input_texture_binding: contract.input_texture_binding,
            input_sampler_binding: contract.input_sampler_binding,
            vertex_entry_point: "main".to_owned(),
            fragment_entry_point: "main".to_owned(),
            topology: OcioGpuWgpuFullscreenTopology::TriangleStrip,
            output_location: contract.output_location,
            requires_ocio_program_link: true,
        }
    }
}

/// Fullscreen draw topology for an OCIO wrapper pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuFullscreenTopology {
    /// Four-vertex fullscreen triangle strip.
    TriangleStrip,
}

impl OcioGpuWgpuFullscreenTopology {
    fn to_wgpu(self) -> wgpu::PrimitiveTopology {
        match self {
            Self::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        }
    }
}

/// Color target format for the future OCIO render pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuWgpuColorTargetFormat {
    /// 8-bit normalized RGBA target.
    Rgba8Unorm,
    /// 16-bit float RGBA target.
    Rgba16Float,
    /// 32-bit float RGBA target.
    Rgba32Float,
}

impl OcioGpuWgpuColorTargetFormat {
    fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            Self::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
            Self::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
        }
    }
}

/// Pure render-pipeline descriptor contract for an OCIO fullscreen pass.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuRenderPipelineDescriptorPlan {
    /// Stable resource key this pipeline belongs to.
    pub resource_key: u64,
    /// Hash of the pipeline layout plan.
    pub pipeline_layout_hash: u64,
    /// Fullscreen wrapper shader contract.
    pub shader_contract: OcioGpuWgpuFullscreenShaderContract,
    /// Output color target format.
    pub output_format: OcioGpuWgpuColorTargetFormat,
    /// Stable hash of this render-pipeline descriptor plan.
    pub descriptor_hash: u64,
}

impl OcioGpuWgpuRenderPipelineDescriptorPlan {
    /// Build a render-pipeline descriptor contract from resource/layout state.
    pub fn for_pipeline_layout(
        resources: &OcioGpuWgpuResourcePlan,
        pipeline_layout: &OcioGpuWgpuPipelineLayoutPlan,
        wrapper_link: &OcioGpuWgpuWrapperLinkPlan,
        output_format: OcioGpuWgpuColorTargetFormat,
    ) -> Self {
        let descriptor_hash = hash_render_pipeline_descriptor(
            resources.resource_key,
            pipeline_layout.layout_hash,
            wrapper_link.link_hash,
            &wrapper_link.shader_contract,
            output_format,
        );
        Self {
            resource_key: resources.resource_key,
            pipeline_layout_hash: pipeline_layout.layout_hash,
            shader_contract: wrapper_link.shader_contract.clone(),
            output_format,
            descriptor_hash,
        }
    }

    /// Return the wgpu primitive state implied by this descriptor.
    pub fn primitive_state(&self) -> wgpu::PrimitiveState {
        wgpu::PrimitiveState {
            topology: self.shader_contract.topology.to_wgpu(),
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        }
    }

    /// Return the wgpu color target state implied by this descriptor.
    pub fn color_target_state(&self) -> wgpu::ColorTargetState {
        wgpu::ColorTargetState {
            format: self.output_format.to_wgpu(),
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        }
    }
}

/// Error returned when actual wgpu bind-group creation cannot satisfy the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuBindGroupError {
    /// Uploaded LUT resources belong to a different resource plan.
    LutResourceKeyMismatch { expected: u64, actual: u64 },
    /// Uploaded uniform resources belong to a different resource plan.
    UniformResourceKeyMismatch { expected: u64, actual: u64 },
    /// The uploaded uniform buffer required by the bind-resource plan is missing.
    MissingUploadedUniformBuffer { binding: u32 },
    /// The bind-resource plan did not contain an entry required by the layout.
    MissingBindResourceEntry { binding: u32 },
    /// A 1D/2D LUT texture required by the bind-resource plan is missing.
    MissingUploadedTexture2D { index: u32 },
    /// A 3D LUT texture required by the bind-resource plan is missing.
    MissingUploadedTexture3D { index: u32 },
    /// Uploaded uniform buffer metadata does not match the bind-resource plan.
    UniformMetadataMismatch {
        /// Binding index.
        binding: u32,
        /// Human-readable mismatch reason.
        reason: OcioGpuWgpuUploadedUniformMismatch,
    },
    /// Uploaded LUT texture metadata does not match the bind-resource plan.
    TextureMetadataMismatch {
        /// Texture index.
        index: u32,
        /// Human-readable mismatch reason.
        reason: OcioGpuWgpuUploadedTextureMismatch,
    },
}

/// Field-level uploaded uniform mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuUploadedUniformMismatch {
    /// Byte length mismatch.
    ByteLen { expected: usize, actual: usize },
    /// Packed bytes hash mismatch.
    BytesHash { expected: u64, actual: u64 },
}

/// Field-level uploaded texture mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuUploadedTextureMismatch {
    /// Binding index mismatch.
    BindingIndex { expected: u32, actual: u32 },
    /// Texture symbol mismatch.
    TextureName { expected: String, actual: String },
    /// Sampler symbol mismatch.
    SamplerName { expected: String, actual: String },
    /// Texture format mismatch.
    Format {
        expected: OcioGpuWgpuLutTextureFormat,
        actual: OcioGpuWgpuLutTextureFormat,
    },
    /// Texture dimension mismatch.
    Dimension {
        expected: OcioGpuWgpuLutTextureDimension,
        actual: OcioGpuWgpuLutTextureDimension,
    },
    /// Texture extent mismatch.
    Extent {
        expected: OcioGpuWgpuLutTextureExtent,
        actual: OcioGpuWgpuLutTextureExtent,
    },
    /// Source values hash mismatch.
    SourceValuesHash { expected: u64, actual: u64 },
    /// Packed bytes hash mismatch.
    PackedBytesHash { expected: u64, actual: u64 },
}

/// Stateless backend preparer for OCIO wgpu bind groups.
pub struct OcioGpuWgpuBindGroupPreparer;

impl OcioGpuWgpuBindGroupPreparer {
    /// Create the concrete OCIO resource bind group from uploaded LUT/uniform resources.
    pub fn prepare_ocio_bind_group(
        device: &wgpu::Device,
        layout_plan: &OcioGpuWgpuBindingLayoutPlan,
        bind_resource_plan: &OcioGpuWgpuBindResourcePlan,
        uploaded_luts: &OcioGpuWgpuUploadedLuts,
        uploaded_uniform: Option<&OcioGpuWgpuUploadedUniformBuffer>,
    ) -> Result<OcioGpuWgpuOcioBindGroup, OcioGpuWgpuBindGroupError> {
        if bind_resource_plan.resource_key != uploaded_luts.resource_key {
            return Err(OcioGpuWgpuBindGroupError::LutResourceKeyMismatch {
                expected: bind_resource_plan.resource_key,
                actual: uploaded_luts.resource_key,
            });
        }
        if let Some(uniform) = uploaded_uniform {
            if bind_resource_plan.resource_key != uniform.resource_key {
                return Err(OcioGpuWgpuBindGroupError::UniformResourceKeyMismatch {
                    expected: bind_resource_plan.resource_key,
                    actual: uniform.resource_key,
                });
            }
        }

        let descriptor = OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_ocio_resources(layout_plan);
        let layout = descriptor.create_bind_group_layout(device);
        let entries = layout_plan
            .entries
            .iter()
            .map(|entry| {
                ocio_bind_group_entry(*entry, bind_resource_plan, uploaded_luts, uploaded_uniform)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ocio_resource_bind_group"),
            layout: &layout,
            entries: &entries,
        });
        Ok(OcioGpuWgpuOcioBindGroup {
            bind_group_index: layout_plan.bind_group,
            resource_key: bind_resource_plan.resource_key,
            bind_resource_plan_hash: bind_resource_plan.plan_hash,
            layout_hash: descriptor.layout_hash,
            layout,
            bind_group,
        })
    }

    /// Create the concrete fullscreen wrapper input bind group.
    pub fn prepare_wrapper_bind_group(
        device: &wgpu::Device,
        wrapper_layout: &OcioGpuWgpuWrapperBindingPlan,
        input: OcioGpuWgpuWrapperInputResources<'_>,
    ) -> OcioGpuWgpuWrapperBindGroup {
        let descriptor =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_wrapper_input(wrapper_layout);
        let layout = descriptor.create_bind_group_layout(device);
        Self::prepare_wrapper_bind_group_with_layout(
            device,
            wrapper_layout,
            descriptor.layout_hash,
            &layout,
            input,
        )
    }

    /// Create the concrete fullscreen wrapper input bind group from a stable layout.
    pub fn prepare_wrapper_bind_group_with_layout(
        device: &wgpu::Device,
        wrapper_layout: &OcioGpuWgpuWrapperBindingPlan,
        layout_hash: u64,
        layout: &wgpu::BindGroupLayout,
        input: OcioGpuWgpuWrapperInputResources<'_>,
    ) -> OcioGpuWgpuWrapperBindGroup {
        let entries = wrapper_layout
            .entries
            .iter()
            .map(|entry| match entry.resource {
                OcioGpuWgpuWrapperBindingResource::InputFrameTexture => wgpu::BindGroupEntry {
                    binding: entry.binding,
                    resource: wgpu::BindingResource::TextureView(input.input_texture_view),
                },
                OcioGpuWgpuWrapperBindingResource::InputFrameSampler => wgpu::BindGroupEntry {
                    binding: entry.binding,
                    resource: wgpu::BindingResource::Sampler(input.input_sampler),
                },
            })
            .collect::<Vec<_>>();
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ocio_wrapper_input_bind_group"),
            layout,
            entries: &entries,
        });
        OcioGpuWgpuWrapperBindGroup {
            bind_group_index: wrapper_layout.bind_group,
            layout_hash,
            layout: layout.clone(),
            bind_group,
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
    /// Filtering contract for LUT texture/sampler pairs; ignored for uniforms.
    pub filtering: OcioGpuWgpuSamplerFiltering,
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
        buffer
            .slice(..)
            .get_mapped_range_mut()
            .expect("OCIO LUT upload mapped range")
            .copy_from_slice(&packed.bytes);
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

/// Validated pure-preparation output for an OCIO fullscreen GPU pipeline.
///
/// This contains no concrete wgpu device objects. It is the deterministic
/// contract bundle that backend object creation must consume.
#[derive(Debug, Clone)]
pub struct OcioGpuWgpuPreparedStaticPipeline {
    /// Cached resource/layout preparation.
    pub resources: Arc<OcioGpuWgpuPreparedResources>,
    /// Wrapper input bind-group plan.
    pub wrapper_binding: OcioGpuWgpuWrapperBindingPlan,
    /// Pipeline layout plan joining OCIO resources and wrapper input resources.
    pub pipeline_layout: OcioGpuWgpuPipelineLayoutPlan,
    /// Link plan between the OCIO-generated program and Mondrian wrapper.
    pub wrapper_link: OcioGpuWgpuWrapperLinkPlan,
    /// Generated stage-split wrapper shader source.
    pub wrapper_source: OcioGpuWgpuWrapperShaderSourceArtifact,
    /// Validated stage-split Naga wrapper modules.
    pub wrapper_module_artifact: Arc<OcioGpuWgpuWrapperShaderModuleArtifact>,
    /// Render-pipeline descriptor contract for the fullscreen pass.
    pub render_descriptor: OcioGpuWgpuRenderPipelineDescriptorPlan,
}

/// Error returned while preparing pure OCIO GPU backend contracts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuBackendPrepError {
    /// The OCIO shader descriptor binding contract is invalid.
    BindingContract(OcioGpuBindingContractValidationError),
    /// The OCIO resource layout cannot be built safely.
    ResourceLayout(OcioGpuWgpuBindingLayoutPlanError),
    /// The OCIO generated program cannot be linked into Mondrian's wrapper.
    WrapperSource(OcioGpuWgpuWrapperShaderArtifactError),
    /// The wrapper shader source cannot become validated Naga modules.
    WrapperModule(OcioGpuWgpuWrapperShaderModuleArtifactError),
}

impl std::fmt::Display for OcioGpuWgpuBackendPrepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OCIO GPU backend preparation failed: {self:?}")
    }
}

impl std::error::Error for OcioGpuWgpuBackendPrepError {}

/// Renderer-owned runtime for pure OCIO GPU backend preparation.
///
/// This runtime owns the caches needed to turn a shader plan into validated
/// layout and wrapper Naga artifacts. Concrete wgpu object creation remains in
/// the backend caches that consume this prepared static pipeline.
#[derive(Default)]
pub struct OcioGpuWgpuBackendPrepRuntime {
    resources: OcioGpuWgpuResourceCache,
    wrapper_module_artifacts: OcioGpuWgpuWrapperShaderModuleArtifactCache,
}

impl OcioGpuWgpuBackendPrepRuntime {
    /// Create a runtime with default cache capacities.
    pub fn new() -> Self {
        Self::default()
    }

    /// Prepare pure backend contracts for an OCIO fullscreen color pass.
    pub fn prepare_static_pipeline(
        &mut self,
        shader_plan: &OcioGpuShaderPlan,
        output_format: OcioGpuWgpuColorTargetFormat,
    ) -> Result<OcioGpuWgpuPreparedStaticPipeline, OcioGpuWgpuBackendPrepError> {
        let resources = OcioGpuWgpuResourcePlan::for_shader_plan(shader_plan)
            .map_err(OcioGpuWgpuBackendPrepError::BindingContract)?;
        let resources = self
            .resources
            .prepare(resources)
            .map_err(OcioGpuWgpuBackendPrepError::ResourceLayout)?;
        let wrapper_binding =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&resources.resources.wrapper_contract);
        let pipeline_layout = OcioGpuWgpuPipelineLayoutPlan::for_bind_groups(
            &resources.resources,
            &resources.binding_layout,
            &wrapper_binding,
        );
        let wrapper_link =
            OcioGpuWgpuWrapperLinkPlan::for_shader_plan(shader_plan, &resources.resources);
        let wrapper_source =
            OcioGpuWgpuWrapperShaderSourceArtifact::generate(shader_plan, &wrapper_link)
                .map_err(OcioGpuWgpuBackendPrepError::WrapperSource)?;
        let render_descriptor = OcioGpuWgpuRenderPipelineDescriptorPlan::for_pipeline_layout(
            &resources.resources,
            &pipeline_layout,
            &wrapper_link,
            output_format,
        );
        let wrapper_module_artifact = self
            .wrapper_module_artifacts
            .translate(&wrapper_source, &pipeline_layout, &render_descriptor)
            .map_err(OcioGpuWgpuBackendPrepError::WrapperModule)?;

        Ok(OcioGpuWgpuPreparedStaticPipeline {
            resources,
            wrapper_binding,
            pipeline_layout,
            wrapper_link,
            wrapper_source,
            wrapper_module_artifact,
            render_descriptor,
        })
    }

    /// Return point-in-time cache diagnostics for this runtime.
    pub fn diagnostics(&self) -> OcioGpuWgpuBackendPrepRuntimeDiagnostics {
        OcioGpuWgpuBackendPrepRuntimeDiagnostics {
            resources: self.resources.diagnostics(),
            wrapper_module_artifacts: self.wrapper_module_artifacts.diagnostics(),
        }
    }
}

/// Point-in-time diagnostics for OCIO GPU backend preparation caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuWgpuBackendPrepRuntimeDiagnostics {
    /// Resource-layout cache diagnostics.
    pub resources: OcioGpuWgpuResourceCacheDiagnostics,
    /// Wrapper Naga artifact cache diagnostics.
    pub wrapper_module_artifacts: OcioGpuWgpuWrapperShaderModuleArtifactCacheDiagnostics,
}

/// Prepared stable wrapper input layout for an OCIO fullscreen pass.
pub struct OcioGpuWgpuPreparedWrapperInputLayout {
    /// Wrapper bind group index.
    pub bind_group: u32,
    /// Stable hash of the wrapper layout descriptor.
    pub layout_hash: u64,
    /// Concrete wgpu bind-group layout reused for per-frame wrapper bind groups.
    pub layout: wgpu::BindGroupLayout,
}

impl OcioGpuWgpuPreparedWrapperInputLayout {
    /// Create a per-frame wrapper input bind group using this stable layout.
    pub fn prepare_bind_group(
        &self,
        device: &wgpu::Device,
        wrapper_binding: &OcioGpuWgpuWrapperBindingPlan,
        input: OcioGpuWgpuWrapperInputResources<'_>,
    ) -> OcioGpuWgpuWrapperBindGroup {
        OcioGpuWgpuBindGroupPreparer::prepare_wrapper_bind_group_with_layout(
            device,
            wrapper_binding,
            self.layout_hash,
            &self.layout,
            input,
        )
    }
}

/// Concrete backend objects prepared for a static OCIO fullscreen pipeline.
pub struct OcioGpuWgpuPreparedBackendObjects {
    /// Stable cache key for this object bundle.
    pub cache_key: u64,
    /// Stable resource key shared by all prepared objects.
    pub resource_key: u64,
    /// Packed/upload-validated bind-resource contract.
    pub bind_resource_plan: OcioGpuWgpuBindResourcePlan,
    /// Uploaded LUT textures.
    pub uploaded_luts: OcioGpuWgpuUploadedLuts,
    /// Uploaded uniform buffer, when required by OCIO.
    pub uploaded_uniform: Option<OcioGpuWgpuUploadedUniformBuffer>,
    /// Concrete OCIO LUT/uniform bind group.
    pub ocio_bind_group: OcioGpuWgpuOcioBindGroup,
    /// Stable wrapper input layout reused by per-frame wrapper bind groups.
    pub wrapper_input_layout: OcioGpuWgpuPreparedWrapperInputLayout,
    /// Concrete wrapper shader modules.
    pub wrapper_modules: Arc<OcioGpuWgpuWrapperShaderModules>,
    /// Concrete pipeline layout.
    pub pipeline_layout: OcioGpuWgpuPipelineLayout,
    /// Concrete render pipeline.
    pub render_pipeline: Arc<OcioGpuWgpuRenderPipeline>,
    /// Render-pass node plan for scheduling/recording.
    pub pass_node: OcioGpuWgpuRenderPassNodePlan,
}

/// Error returned while preparing concrete OCIO GPU backend objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuBackendObjectError {
    /// A filtering 32-bit float LUT was planned on a device lacking the
    /// corresponding optional wgpu feature.
    Float32FilteringUnsupported,
    /// LUT payloads could not be packed or uploaded.
    LutUpload(OcioGpuWgpuLutUploadError),
    /// Uniform payloads could not be packed or uploaded.
    UniformUpload(OcioGpuWgpuUniformUploadError),
    /// Packed resources did not match the OCIO binding contract.
    BindResource(OcioGpuWgpuBindResourcePlanError),
    /// Concrete bind-group creation failed contract validation.
    BindGroup(OcioGpuWgpuBindGroupError),
    /// Concrete pipeline layout creation failed contract validation.
    PipelineLayout(OcioGpuWgpuPipelineLayoutError),
    /// Concrete render pipeline creation failed contract validation.
    RenderPipeline(OcioGpuWgpuRenderPipelineError),
    /// Render-pass node creation failed contract validation.
    RenderPass(OcioGpuWgpuRenderPassError),
}

impl std::fmt::Display for OcioGpuWgpuBackendObjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OCIO GPU backend object preparation failed: {self:?}")
    }
}

impl std::error::Error for OcioGpuWgpuBackendObjectError {}

/// Renderer-owned runtime for concrete OCIO GPU backend object preparation.
pub struct OcioGpuWgpuBackendObjectRuntime {
    objects: LruCache<u64, Arc<OcioGpuWgpuPreparedBackendObjects>>,
    wrapper_modules: OcioGpuWgpuWrapperShaderModuleCache,
    render_pipelines: OcioGpuWgpuRenderPipelineCache,
    hits: u64,
    misses: u64,
    failures: u64,
}

impl OcioGpuWgpuBackendObjectRuntime {
    /// Create a runtime with a fixed non-zero object-cache capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            objects: LruCache::new(capacity),
            wrapper_modules: OcioGpuWgpuWrapperShaderModuleCache::default(),
            render_pipelines: OcioGpuWgpuRenderPipelineCache::default(),
            hits: 0,
            misses: 0,
            failures: 0,
        }
    }

    /// Prepare or reuse concrete backend objects for a static OCIO pipeline.
    pub fn prepare_backend_objects(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shader_plan: &OcioGpuShaderPlan,
        static_pipeline: &OcioGpuWgpuPreparedStaticPipeline,
    ) -> Result<Arc<OcioGpuWgpuPreparedBackendObjects>, OcioGpuWgpuBackendObjectError> {
        let needs_float32_filtering =
            static_pipeline.resources.binding_layout.entries.iter().any(|entry| {
                entry.filtering == OcioGpuWgpuSamplerFiltering::Filtering
                    && matches!(
                        entry.resource,
                        OcioGpuWgpuBindingResource::OcioLutTexture2d { .. }
                            | OcioGpuWgpuBindingResource::OcioLutTexture3d { .. }
                    )
            });
        if needs_float32_filtering
            && !device.features().contains(wgpu::Features::FLOAT32_FILTERABLE)
        {
            return Err(OcioGpuWgpuBackendObjectError::Float32FilteringUnsupported);
        }
        let cache_key = backend_object_cache_key(static_pipeline);
        if let Some(hit) = self.objects.get(&cache_key) {
            self.hits = self.hits.saturating_add(1);
            return Ok(Arc::clone(hit));
        }

        self.misses = self.misses.saturating_add(1);
        match self.prepare_backend_objects_uncached(
            device,
            queue,
            shader_plan,
            static_pipeline,
            cache_key,
        ) {
            Ok(objects) => {
                let objects = Arc::new(objects);
                self.objects.put(cache_key, Arc::clone(&objects));
                Ok(objects)
            }
            Err(err) => {
                self.failures = self.failures.saturating_add(1);
                Err(err)
            }
        }
    }

    fn prepare_backend_objects_uncached(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shader_plan: &OcioGpuShaderPlan,
        static_pipeline: &OcioGpuWgpuPreparedStaticPipeline,
        cache_key: u64,
    ) -> Result<OcioGpuWgpuPreparedBackendObjects, OcioGpuWgpuBackendObjectError> {
        let resources = &static_pipeline.resources.resources;
        let lut_upload_plan = OcioGpuWgpuLutUploadPlan::for_shader_plan(shader_plan, resources);
        let packed_luts = lut_upload_plan
            .pack_textures()
            .map_err(OcioGpuWgpuBackendObjectError::LutUpload)?;
        let uploaded_luts = OcioGpuWgpuLutUploader::upload_packed(device, queue, &packed_luts)
            .map_err(OcioGpuWgpuBackendObjectError::LutUpload)?;

        let uniform_upload_plan =
            OcioGpuWgpuUniformUploadPlan::for_shader_plan(shader_plan, resources);
        let packed_uniform = uniform_upload_plan
            .pack_buffer()
            .map_err(OcioGpuWgpuBackendObjectError::UniformUpload)?;
        let uploaded_uniform = OcioGpuWgpuUniformUploader::upload_packed(device, &packed_uniform);

        let bind_resource_plan = OcioGpuWgpuBindResourcePlan::from_packed_resources(
            resources,
            &packed_luts,
            if packed_uniform.bytes.is_empty() {
                None
            } else {
                Some(&packed_uniform)
            },
        )
        .map_err(OcioGpuWgpuBackendObjectError::BindResource)?;
        let ocio_bind_group = OcioGpuWgpuBindGroupPreparer::prepare_ocio_bind_group(
            device,
            &static_pipeline.resources.binding_layout,
            &bind_resource_plan,
            &uploaded_luts,
            uploaded_uniform.as_ref(),
        )
        .map_err(OcioGpuWgpuBackendObjectError::BindGroup)?;

        let wrapper_descriptor = OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_wrapper_input(
            &static_pipeline.wrapper_binding,
        );
        let wrapper_layout = wrapper_descriptor.create_bind_group_layout(device);
        let wrapper_input_layout = OcioGpuWgpuPreparedWrapperInputLayout {
            bind_group: static_pipeline.wrapper_binding.bind_group,
            layout_hash: wrapper_descriptor.layout_hash,
            layout: wrapper_layout,
        };

        let pipeline_layout = OcioGpuWgpuPipelineLayoutPreparer::prepare_with_wrapper_layout(
            device,
            &static_pipeline.pipeline_layout,
            &ocio_bind_group,
            wrapper_input_layout.layout_hash,
            &wrapper_input_layout.layout,
        )
        .map_err(OcioGpuWgpuBackendObjectError::PipelineLayout)?;
        let wrapper_modules =
            self.wrapper_modules.prepare(device, &static_pipeline.wrapper_module_artifact);
        let render_pipeline = self
            .render_pipelines
            .prepare(
                device,
                &static_pipeline.render_descriptor,
                &pipeline_layout,
                &wrapper_modules,
            )
            .map_err(OcioGpuWgpuBackendObjectError::RenderPipeline)?;
        let pass_node = OcioGpuWgpuRenderPassNodePlan::for_pipeline_and_wrapper_layout(
            &render_pipeline,
            &ocio_bind_group,
            wrapper_input_layout.layout_hash,
            static_pipeline.render_descriptor.output_format,
        )
        .map_err(OcioGpuWgpuBackendObjectError::RenderPass)?;

        Ok(OcioGpuWgpuPreparedBackendObjects {
            cache_key,
            resource_key: resources.resource_key,
            bind_resource_plan,
            uploaded_luts,
            uploaded_uniform,
            ocio_bind_group,
            wrapper_input_layout,
            wrapper_modules,
            pipeline_layout,
            render_pipeline,
            pass_node,
        })
    }

    /// Return point-in-time diagnostics for backend object caches.
    pub fn diagnostics(&self) -> OcioGpuWgpuBackendObjectRuntimeDiagnostics {
        OcioGpuWgpuBackendObjectRuntimeDiagnostics {
            entries: self.objects.len(),
            hits: self.hits,
            misses: self.misses,
            failures: self.failures,
            wrapper_modules: self.wrapper_modules.diagnostics(),
            render_pipelines: self.render_pipelines.diagnostics(),
        }
    }
}

impl Default for OcioGpuWgpuBackendObjectRuntime {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(64).expect("default cache capacity is non-zero"))
    }
}

/// Point-in-time diagnostics for concrete OCIO GPU backend object caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuWgpuBackendObjectRuntimeDiagnostics {
    /// Cached prepared backend object bundles.
    pub entries: usize,
    /// Object cache hits.
    pub hits: u64,
    /// Object cache misses.
    pub misses: u64,
    /// Object preparation failures.
    pub failures: u64,
    /// Wrapper shader-module cache diagnostics.
    pub wrapper_modules: OcioGpuWgpuWrapperShaderModuleCacheDiagnostics,
    /// Render-pipeline cache diagnostics.
    pub render_pipelines: OcioGpuWgpuRenderPipelineCacheDiagnostics,
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

/// Concrete wgpu shader modules for a validated OCIO fullscreen wrapper.
pub struct OcioGpuWgpuWrapperShaderModules {
    /// Stable cache key for these backend shader modules.
    pub cache_key: u64,
    /// Stable resource key this wrapper belongs to.
    pub resource_key: u64,
    /// Wrapper shader module artifact key.
    pub module_key: u64,
    /// Hash of the stage-split wrapper shader sources.
    pub source_hash: u64,
    /// Hash of the pipeline layout contract.
    pub pipeline_layout_hash: u64,
    /// Hash of the render-pipeline descriptor contract.
    pub render_descriptor_hash: u64,
    /// Backend fullscreen vertex shader module.
    pub vertex_module: wgpu::ShaderModule,
    /// Backend fragment shader module that calls the OCIO-generated function.
    pub fragment_module: wgpu::ShaderModule,
}

/// Point-in-time wrapper wgpu shader-module cache diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuWgpuWrapperShaderModuleCacheDiagnostics {
    /// Cached wrapper shader module pairs.
    pub entries: usize,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
}

/// Bounded cache for concrete wgpu shader modules built from wrapper Naga artifacts.
pub struct OcioGpuWgpuWrapperShaderModuleCache {
    entries: LruCache<u64, Arc<OcioGpuWgpuWrapperShaderModules>>,
    hits: u64,
    misses: u64,
}

impl OcioGpuWgpuWrapperShaderModuleCache {
    /// Create a cache with a fixed non-zero capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: LruCache::new(capacity),
            hits: 0,
            misses: 0,
        }
    }

    /// Prepare concrete wgpu shader modules from a validated wrapper module artifact.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        artifact: &OcioGpuWgpuWrapperShaderModuleArtifact,
    ) -> Arc<OcioGpuWgpuWrapperShaderModules> {
        let cache_key = wrapper_backend_shader_modules_cache_key(artifact);
        if let Some(hit) = self.entries.get(&cache_key) {
            self.hits = self.hits.saturating_add(1);
            return Arc::clone(hit);
        }

        self.misses = self.misses.saturating_add(1);
        let vertex_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ocio_wrapper_vertex_naga_shader_module"),
            source: wgpu::ShaderSource::Naga(Cow::Owned(artifact.vertex.naga_module.clone())),
        });
        let fragment_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ocio_wrapper_fragment_naga_shader_module"),
            source: wgpu::ShaderSource::Naga(Cow::Owned(artifact.fragment.naga_module.clone())),
        });
        let modules = Arc::new(OcioGpuWgpuWrapperShaderModules {
            cache_key,
            resource_key: artifact.resource_key,
            module_key: artifact.module_key,
            source_hash: artifact.source_hash,
            pipeline_layout_hash: artifact.pipeline_layout_hash,
            render_descriptor_hash: artifact.render_descriptor_hash,
            vertex_module,
            fragment_module,
        });
        self.entries.put(cache_key, Arc::clone(&modules));
        modules
    }

    /// Return wrapper shader-module cache diagnostics.
    pub fn diagnostics(&self) -> OcioGpuWgpuWrapperShaderModuleCacheDiagnostics {
        OcioGpuWgpuWrapperShaderModuleCacheDiagnostics {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
        }
    }
}

impl Default for OcioGpuWgpuWrapperShaderModuleCache {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(64).expect("default cache capacity is non-zero"))
    }
}

/// Concrete wgpu render pipeline for an OCIO fullscreen color pass.
pub struct OcioGpuWgpuRenderPipeline {
    /// Stable cache key for this backend render pipeline.
    pub cache_key: u64,
    /// Stable resource key this render pipeline belongs to.
    pub resource_key: u64,
    /// Hash of the pure render-pipeline descriptor plan.
    pub descriptor_hash: u64,
    /// Hash of the concrete pipeline layout contract.
    pub pipeline_layout_hash: u64,
    /// Wrapper shader module key consumed by this pipeline.
    pub module_key: u64,
    /// Concrete wgpu render pipeline.
    pub render_pipeline: wgpu::RenderPipeline,
}

/// Error returned before creating an OCIO fullscreen render pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuRenderPipelineError {
    /// The pipeline layout belongs to a different resource key.
    PipelineLayoutResourceKeyMismatch { expected: u64, actual: u64 },
    /// The wrapper shader modules belong to a different resource key.
    ShaderModuleResourceKeyMismatch { expected: u64, actual: u64 },
    /// The concrete pipeline layout hash does not match the descriptor plan.
    PipelineLayoutHashMismatch { expected: u64, actual: u64 },
    /// The wrapper shader modules were built for a different pipeline layout.
    ShaderModulePipelineLayoutHashMismatch { expected: u64, actual: u64 },
    /// The wrapper shader modules were built for a different render descriptor.
    ShaderModuleRenderDescriptorHashMismatch { expected: u64, actual: u64 },
}

/// Point-in-time OCIO render pipeline cache diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioGpuWgpuRenderPipelineCacheDiagnostics {
    /// Cached render pipelines.
    pub entries: usize,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Contract validation failures before pipeline creation.
    pub validation_failures: u64,
}

/// Bounded cache for concrete OCIO fullscreen render pipelines.
pub struct OcioGpuWgpuRenderPipelineCache {
    entries: LruCache<u64, Arc<OcioGpuWgpuRenderPipeline>>,
    hits: u64,
    misses: u64,
    validation_failures: u64,
}

impl OcioGpuWgpuRenderPipelineCache {
    /// Create a cache with a fixed non-zero capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: LruCache::new(capacity),
            hits: 0,
            misses: 0,
            validation_failures: 0,
        }
    }

    /// Prepare a concrete render pipeline from validated layout and wrapper shader modules.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        descriptor: &OcioGpuWgpuRenderPipelineDescriptorPlan,
        pipeline_layout: &OcioGpuWgpuPipelineLayout,
        modules: &OcioGpuWgpuWrapperShaderModules,
    ) -> Result<Arc<OcioGpuWgpuRenderPipeline>, OcioGpuWgpuRenderPipelineError> {
        let cache_key = match render_pipeline_cache_key(
            descriptor,
            pipeline_layout.resource_key,
            pipeline_layout.layout_hash,
            OcioGpuWgpuWrapperShaderModuleKeyMetadata {
                resource_key: modules.resource_key,
                pipeline_layout_hash: modules.pipeline_layout_hash,
                render_descriptor_hash: modules.render_descriptor_hash,
                cache_key: modules.cache_key,
                module_key: modules.module_key,
            },
        ) {
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
        let color_target = descriptor.color_target_state();
        let targets = [Some(color_target)];
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            cache: None,
            multiview_mask: None,
            label: Some("ocio_fullscreen_render_pipeline"),
            layout: Some(&pipeline_layout.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &modules.vertex_module,
                entry_point: Some(&descriptor.shader_contract.vertex_entry_point),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &modules.fragment_module,
                entry_point: Some(&descriptor.shader_contract.fragment_entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &targets,
            }),
            primitive: descriptor.primitive_state(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
        });
        let pipeline = Arc::new(OcioGpuWgpuRenderPipeline {
            cache_key,
            resource_key: descriptor.resource_key,
            descriptor_hash: descriptor.descriptor_hash,
            pipeline_layout_hash: pipeline_layout.layout_hash,
            module_key: modules.module_key,
            render_pipeline,
        });
        self.entries.put(cache_key, Arc::clone(&pipeline));
        Ok(pipeline)
    }

    /// Return render pipeline cache diagnostics.
    pub fn diagnostics(&self) -> OcioGpuWgpuRenderPipelineCacheDiagnostics {
        OcioGpuWgpuRenderPipelineCacheDiagnostics {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
            validation_failures: self.validation_failures,
        }
    }
}

impl Default for OcioGpuWgpuRenderPipelineCache {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(64).expect("default cache capacity is non-zero"))
    }
}

/// Render target borrowed while recording an OCIO fullscreen pass.
pub struct OcioGpuWgpuRenderPassTarget<'a> {
    /// Stable resource key this target belongs to.
    pub resource_key: u64,
    /// Output color target format.
    pub output_format: OcioGpuWgpuColorTargetFormat,
    /// Target view written by the fullscreen pass.
    pub view: &'a wgpu::TextureView,
    /// Load operation for the target attachment.
    pub load_op: wgpu::LoadOp<wgpu::Color>,
}

/// Pure render-pass node contract for an OCIO fullscreen color pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OcioGpuWgpuRenderPassNodePlan {
    /// Stable resource key this render pass belongs to.
    pub resource_key: u64,
    /// Hash of the render pipeline contract.
    pub render_pipeline_cache_key: u64,
    /// Hash of the render-pipeline descriptor plan.
    pub render_descriptor_hash: u64,
    /// Hash of the OCIO bind-group layout.
    pub ocio_layout_hash: u64,
    /// Hash of the wrapper bind-group layout.
    pub wrapper_layout_hash: u64,
    /// Output color target format.
    pub output_format: OcioGpuWgpuColorTargetFormat,
    /// Number of vertices drawn by the fullscreen pass.
    pub vertex_count: u32,
    /// Stable hash of this render-pass node.
    pub node_hash: u64,
}

impl OcioGpuWgpuRenderPassNodePlan {
    /// Build a render-pass node plan from validated backend objects.
    pub fn for_pipeline_and_bind_groups(
        pipeline: &OcioGpuWgpuRenderPipeline,
        ocio_bind_group: &OcioGpuWgpuOcioBindGroup,
        wrapper_bind_group: &OcioGpuWgpuWrapperBindGroup,
        output_format: OcioGpuWgpuColorTargetFormat,
    ) -> Result<Self, OcioGpuWgpuRenderPassError> {
        let metadata = OcioGpuWgpuRenderPipelineMetadata {
            resource_key: pipeline.resource_key,
            cache_key: pipeline.cache_key,
            descriptor_hash: pipeline.descriptor_hash,
        };
        Self::for_pipeline_metadata_and_bind_groups(
            metadata,
            ocio_bind_group.resource_key,
            ocio_bind_group.layout_hash,
            wrapper_bind_group.layout_hash,
            output_format,
        )
    }

    /// Build a render-pass node plan from a pipeline, OCIO bind group, and stable wrapper layout.
    pub fn for_pipeline_and_wrapper_layout(
        pipeline: &OcioGpuWgpuRenderPipeline,
        ocio_bind_group: &OcioGpuWgpuOcioBindGroup,
        wrapper_layout_hash: u64,
        output_format: OcioGpuWgpuColorTargetFormat,
    ) -> Result<Self, OcioGpuWgpuRenderPassError> {
        let metadata = OcioGpuWgpuRenderPipelineMetadata {
            resource_key: pipeline.resource_key,
            cache_key: pipeline.cache_key,
            descriptor_hash: pipeline.descriptor_hash,
        };
        Self::for_pipeline_metadata_and_bind_groups(
            metadata,
            ocio_bind_group.resource_key,
            ocio_bind_group.layout_hash,
            wrapper_layout_hash,
            output_format,
        )
    }

    fn for_pipeline_metadata_and_bind_groups(
        pipeline: OcioGpuWgpuRenderPipelineMetadata,
        ocio_bind_group_resource_key: u64,
        ocio_layout_hash: u64,
        wrapper_layout_hash: u64,
        output_format: OcioGpuWgpuColorTargetFormat,
    ) -> Result<Self, OcioGpuWgpuRenderPassError> {
        let node_hash = render_pass_node_hash(
            pipeline,
            ocio_bind_group_resource_key,
            ocio_layout_hash,
            wrapper_layout_hash,
            output_format,
        )?;
        Ok(Self {
            resource_key: pipeline.resource_key,
            render_pipeline_cache_key: pipeline.cache_key,
            render_descriptor_hash: pipeline.descriptor_hash,
            ocio_layout_hash,
            wrapper_layout_hash,
            output_format,
            vertex_count: 4,
            node_hash,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct OcioGpuWgpuRenderPipelineMetadata {
    resource_key: u64,
    cache_key: u64,
    descriptor_hash: u64,
}

/// Error returned before recording an OCIO fullscreen render pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcioGpuWgpuRenderPassError {
    /// The OCIO bind group belongs to a different resource key.
    OcioBindGroupResourceKeyMismatch { expected: u64, actual: u64 },
    /// The render target belongs to a different resource key.
    TargetResourceKeyMismatch { expected: u64, actual: u64 },
    /// The render target format differs from the pass contract.
    TargetFormatMismatch {
        expected: OcioGpuWgpuColorTargetFormat,
        actual: OcioGpuWgpuColorTargetFormat,
    },
    /// The render pipeline differs from the pass contract.
    PipelineCacheKeyMismatch { expected: u64, actual: u64 },
    /// The OCIO bind-group layout differs from the pass contract.
    OcioLayoutHashMismatch { expected: u64, actual: u64 },
    /// The wrapper bind-group layout differs from the pass contract.
    WrapperLayoutHashMismatch { expected: u64, actual: u64 },
}

/// Stateless recorder for an OCIO fullscreen render pass.
pub struct OcioGpuWgpuRenderPassRecorder;

impl OcioGpuWgpuRenderPassRecorder {
    /// Record a fullscreen OCIO pass into an existing command encoder.
    pub fn record(
        encoder: &mut wgpu::CommandEncoder,
        plan: &OcioGpuWgpuRenderPassNodePlan,
        pipeline: &OcioGpuWgpuRenderPipeline,
        ocio_bind_group: &OcioGpuWgpuOcioBindGroup,
        wrapper_bind_group: &OcioGpuWgpuWrapperBindGroup,
        target: OcioGpuWgpuRenderPassTarget<'_>,
    ) -> Result<(), OcioGpuWgpuRenderPassError> {
        validate_render_pass_contract(
            plan,
            pipeline,
            ocio_bind_group,
            wrapper_bind_group,
            &target,
        )?;

        let attachments = [Some(wgpu::RenderPassColorAttachment {
            view: target.view,
            resolve_target: None,
            ops: wgpu::Operations { load: target.load_op, store: wgpu::StoreOp::Store },
            depth_slice: None,
        })];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ocio_fullscreen_render_pass"),
            color_attachments: &attachments,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline.render_pipeline);
        pass.set_bind_group(
            ocio_bind_group.bind_group_index,
            &ocio_bind_group.bind_group,
            &[],
        );
        pass.set_bind_group(
            wrapper_bind_group.bind_group_index,
            &wrapper_bind_group.bind_group,
            &[],
        );
        pass.draw(0..plan.vertex_count, 0..1);
        Ok(())
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
    /// OCIO config is not loaded or unavailable for GPU shader extraction.
    OcioConfigNotLoaded,
    /// OCIO processor could not be created for the requested transform.
    OcioProcessorUnavailable,
    /// OCIO GPU shader extraction failed (transpilation, Naga, or backend error).
    OcioGpuShaderExtractionFailed {
        /// Human-readable extraction failure reason.
        reason: String,
    },
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

    /// Clear all cached entries. Call this when the OCIO config changes to
    /// prevent stale shader plans from being served.
    pub fn clear(&mut self) {
        self.entries.clear();
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
    /// This validates the shader-side wgpu resource contract. Concrete backend
    /// object creation is owned by `OcioGpuWgpuBackendPrepRuntime` and
    /// `OcioGpuWgpuBackendObjectRuntime`, so missing shader modules, bind
    /// groups, wrappers, and pipelines are not blockers at this planning layer.
    ///
    /// When shader extraction fails, a blocked plan is returned with the
    /// appropriate `OcioGpuWgpuBlocker` populated instead of propagating an
    /// error.  This ensures the stage plan is always produced and the blocker
    /// is recorded in diagnostics.
    pub fn prepare_wgpu_execution(
        &mut self,
        request: OcioGpuShaderRequest,
    ) -> Result<OcioGpuWgpuExecutionPlan, OcioGpuShaderError> {
        match self.get_or_extract(request.clone()) {
            Ok(shader_plan) => {
                let resources =
                    OcioGpuWgpuResourcePlan::for_shader_plan(&shader_plan).map_err(|reason| {
                        OcioGpuShaderError {
                            request: shader_plan.request.clone(),
                            reason: reason.to_string(),
                        }
                    })?;
                Ok(OcioGpuWgpuExecutionPlan { shader_plan, resources, blockers: Vec::new() })
            }
            Err(err) => {
                let blocker = classify_ocio_shader_error(&err.reason);
                let dummy_bundle = Arc::new(OcioGpuShaderBundle {
                    src_color_space: String::new(),
                    dst_color_space: String::new(),
                    language: request.language(),
                    shader_text: String::new(),
                    descriptor_set_index: 0,
                    texture_binding_start: 0,
                    uniform_buffer_binding: 0,
                    uniform_buffer_size: 0,
                    texture_2d_count: 0,
                    texture_3d_count: 0,
                    uniform_count: 0,
                    textures_2d: Vec::new(),
                    textures_3d: Vec::new(),
                    uniforms: Vec::new(),
                    cache_id: None,
                });
                let shader_plan = Arc::new(plan_from_bundle(err.request.clone(), dummy_bundle));
                let resources = OcioGpuWgpuResourcePlan::empty();
                Ok(OcioGpuWgpuExecutionPlan { shader_plan, resources, blockers: vec![blocker] })
            }
        }
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

/// Classify an OCIO GPU shader extraction error reason into a typed blocker.
///
/// The core OCIO layer returns flat `String` errors. This function maps known
/// error message prefixes to structured `OcioGpuWgpuBlocker` variants so the
/// stage diagnostics can record the specific failure reason.
pub fn classify_ocio_shader_error(reason: &str) -> OcioGpuWgpuBlocker {
    let lower = reason.to_lowercase();
    if lower.contains("no ocio config loaded") || lower.contains("call ensure_ocio_loaded") {
        OcioGpuWgpuBlocker::OcioConfigNotLoaded
    } else if lower.contains("processor") && !lower.contains("gpu processor") {
        OcioGpuWgpuBlocker::OcioProcessorUnavailable
    } else {
        OcioGpuWgpuBlocker::OcioGpuShaderExtractionFailed { reason: reason.to_owned() }
    }
}

fn extract_bundle(request: &OcioGpuShaderRequest) -> Result<OcioGpuShaderBundle, String> {
    match request {
        OcioGpuShaderRequest::ColorSpace { src, dst, language } => {
            extract_ocio_identity_gpu_shader_bundle(*src, *dst, *language)
        }
        OcioGpuShaderRequest::DisplayView { src, display, view, language } => {
            extract_ocio_display_identity_gpu_shader_bundle(*src, display, view, *language)
        }
    }
}

fn translate_shader_text(
    request: OcioGpuShaderTranslationRequest,
    shader_text: &str,
    required_bindings: OcioGpuBindingContract,
) -> Result<OcioGpuTranslatedShader, OcioGpuShaderTranslationError> {
    let stage = translate_naga_shader_stage(
        request.source_language,
        request.target_language,
        request.stage,
        request.source_shader_hash,
        shader_text,
    )
    .map_err(|reason| translation_error(request, reason))?;
    Ok(OcioGpuTranslatedShader {
        request,
        naga_module: stage.naga_module,
        module_info: stage.module_info,
        debug_wgsl: stage.debug_wgsl,
        debug_wgsl_hash: stage.debug_wgsl_hash,
        required_bindings,
        diagnostics: stage.diagnostics,
        entry_point_count: stage.entry_point_count,
    })
}

fn translate_naga_shader_stage(
    source_language: GpuLanguage,
    target_language: OcioGpuShaderTargetLanguage,
    stage: OcioGpuShaderStage,
    source_hash: u64,
    shader_text: &str,
) -> Result<OcioGpuNagaShaderStageArtifact, OcioGpuShaderTranslationFailure> {
    if !is_glsl_language(source_language) {
        return Err(OcioGpuShaderTranslationFailure::UnsupportedSourceLanguage {
            language: source_language,
        });
    }
    if target_language != OcioGpuShaderTargetLanguage::NagaIr {
        return Err(OcioGpuShaderTranslationFailure::UnsupportedTargetLanguage {
            language: target_language,
        });
    }

    let mut frontend = naga::front::glsl::Frontend::default();
    let options = naga::front::glsl::Options::from(stage.to_naga());
    let module = frontend
        .parse(&options, shader_text)
        .map_err(|err| OcioGpuShaderTranslationFailure::ParseFailed { message: err.to_string() })?;

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    );
    let info = validator.validate(&module).map_err(|err| {
        OcioGpuShaderTranslationFailure::ValidationFailed { message: err.to_string() }
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
    Ok(OcioGpuNagaShaderStageArtifact {
        source_language,
        target_language,
        stage,
        source_hash,
        naga_module: module,
        module_info: info,
        debug_wgsl,
        debug_wgsl_hash,
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

fn contains_glsl_main(shader_text: &str) -> bool {
    shader_text.match_indices("main").any(|(index, _)| {
        let before = shader_text[..index].chars().next_back();
        let after_index = index.saturating_add("main".len());
        let after = shader_text[after_index..].chars().next();
        let ident_before = before.is_some_and(is_glsl_identifier_char);
        let ident_after = after.is_some_and(is_glsl_identifier_char);
        !ident_before
            && !ident_after
            && next_non_whitespace_is_open_paren(&shader_text[after_index..])
    })
}

fn glsl_function_call_style(
    shader_text: &str,
    function_name: &str,
) -> OcioGpuGeneratedProgramCallStyle {
    for (index, _) in shader_text.match_indices(function_name) {
        let before = shader_text[..index].chars().next_back();
        let after_index = index.saturating_add(function_name.len());
        let after = shader_text[after_index..].chars().next();
        let ident_before = before.is_some_and(is_glsl_identifier_char);
        let ident_after = after.is_some_and(is_glsl_identifier_char);
        if ident_before
            || ident_after
            || !next_non_whitespace_is_open_paren(&shader_text[after_index..])
        {
            continue;
        }

        return match previous_glsl_identifier(&shader_text[..index]).as_deref() {
            Some("vec4") => OcioGpuGeneratedProgramCallStyle::ReturnsVec4,
            Some("void") => OcioGpuGeneratedProgramCallStyle::MutatesInOut,
            _ => OcioGpuGeneratedProgramCallStyle::Unknown,
        };
    }

    OcioGpuGeneratedProgramCallStyle::Unknown
}

fn previous_glsl_identifier(prefix: &str) -> Option<String> {
    let mut ident = String::new();
    for ch in prefix.trim_end().chars().rev() {
        if is_glsl_identifier_char(ch) {
            ident.push(ch);
        } else if ident.is_empty() {
            continue;
        } else {
            break;
        }
    }
    if ident.is_empty() {
        None
    } else {
        Some(ident.chars().rev().collect())
    }
}

fn is_glsl_identifier_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn next_non_whitespace_is_open_paren(text: &str) -> bool {
    text.chars().find(|ch| !ch.is_whitespace()) == Some('(')
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
    let mut hasher = DefaultHasher::new();
    request.hash(&mut hasher);
    // Include OCIO config generation so cache entries are invalidated when the
    // config changes. This prevents stale shader plans from being served after
    // a config switch.
    mondrian_core::ocio_config_generation().hash(&mut hasher);
    hasher.finish()
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

fn hash_pipeline_layout_plan(
    resource_key: u64,
    ocio_layout_hash: u64,
    wrapper_layout_hash: u64,
    bind_groups: &[OcioGpuWgpuPipelineBindGroupSlot],
) -> u64 {
    let mut hasher = DefaultHasher::new();
    resource_key.hash(&mut hasher);
    ocio_layout_hash.hash(&mut hasher);
    wrapper_layout_hash.hash(&mut hasher);
    bind_groups.hash(&mut hasher);
    hasher.finish()
}

fn hash_render_pipeline_descriptor(
    resource_key: u64,
    pipeline_layout_hash: u64,
    wrapper_link_hash: u64,
    shader_contract: &OcioGpuWgpuFullscreenShaderContract,
    output_format: OcioGpuWgpuColorTargetFormat,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    resource_key.hash(&mut hasher);
    pipeline_layout_hash.hash(&mut hasher);
    wrapper_link_hash.hash(&mut hasher);
    shader_contract.hash(&mut hasher);
    output_format.hash(&mut hasher);
    hasher.finish()
}

fn hash_wrapper_link_plan(
    resource_key: u64,
    shader_hash: u64,
    program_contract: &OcioGpuGeneratedProgramContract,
    shader_contract: &OcioGpuWgpuFullscreenShaderContract,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    resource_key.hash(&mut hasher);
    shader_hash.hash(&mut hasher);
    program_contract.hash(&mut hasher);
    shader_contract.hash(&mut hasher);
    hasher.finish()
}

fn wrapper_link_blockers(
    contract: &OcioGpuGeneratedProgramContract,
) -> Vec<OcioGpuWgpuWrapperLinkBlocker> {
    let mut blockers = Vec::new();
    if !contract.function_present {
        blockers.push(OcioGpuWgpuWrapperLinkBlocker::MissingFunctionName {
            function_name: contract.function_name.clone(),
        });
    }
    if !contract.pixel_name_present {
        blockers.push(OcioGpuWgpuWrapperLinkBlocker::MissingPixelName {
            pixel_name: contract.pixel_name.clone(),
        });
    }
    match contract.source_kind {
        OcioGpuGeneratedProgramSourceKind::CallableFunction => {}
        OcioGpuGeneratedProgramSourceKind::CompleteFragmentShader => {
            blockers.push(OcioGpuWgpuWrapperLinkBlocker::CompleteFragmentShaderRequiresSplit);
        }
        OcioGpuGeneratedProgramSourceKind::Unknown => {
            blockers.push(OcioGpuWgpuWrapperLinkBlocker::UnknownProgramShape);
        }
    }
    if contract.call_style == OcioGpuGeneratedProgramCallStyle::Unknown {
        blockers.push(OcioGpuWgpuWrapperLinkBlocker::UnknownFunctionCallStyle);
    }
    blockers
}

struct OcioGpuWgpuWrapperShaderSources {
    vertex_source: String,
    fragment_source: String,
    debug_combined_source: String,
}

fn build_wrapper_shader_sources(
    shader_plan: &OcioGpuShaderPlan,
    link_plan: &OcioGpuWgpuWrapperLinkPlan,
) -> Result<OcioGpuWgpuWrapperShaderSources, OcioGpuWgpuWrapperShaderArtifactError> {
    let wrapper = &link_plan.shader_contract;
    let ocio_program = lower_ocio_program_source_for_wgpu(shader_plan)
        .map_err(|reason| OcioGpuWgpuWrapperShaderArtifactError::SourceLoweringFailed { reason })?;
    let ocio_program_call = wrapper_ocio_program_glsl_call(&link_plan.program_contract);
    let vertex_source = format!(
        r#"#version 450 core

layout(location = 0) out vec2 mondrian_wrapper_uv;

void {vertex_entry}() {{
    vec2 positions[4] = vec2[](
        vec2(-1.0, -1.0),
        vec2( 1.0, -1.0),
        vec2(-1.0,  1.0),
        vec2( 1.0,  1.0)
    );
    vec2 uvs[4] = vec2[](
        vec2(0.0, 1.0),
        vec2(1.0, 1.0),
        vec2(0.0, 0.0),
        vec2(1.0, 0.0)
    );
    gl_Position = vec4(positions[gl_VertexIndex], 0.0, 1.0);
    mondrian_wrapper_uv = uvs[gl_VertexIndex];
}}
"#,
        vertex_entry = wrapper.vertex_entry_point,
    );
    let fragment_source = format!(
        r#"#version 450 core

{ocio_program}

layout(location = 0) in vec2 mondrian_fragment_uv;
layout(location = {output_location}) out vec4 mondrian_fragment_color;

layout(set = {wrapper_set}, binding = {input_texture_binding}) uniform texture2D mondrian_wrapper_input_texture;
layout(set = {wrapper_set}, binding = {input_sampler_binding}) uniform sampler mondrian_wrapper_input_sampler;

void {fragment_entry}() {{
    vec4 {pixel_name} = texture(sampler2D(mondrian_wrapper_input_texture, mondrian_wrapper_input_sampler), mondrian_fragment_uv);
    float mondrian_ocio_preserved_alpha = {pixel_name}.a;
    {ocio_program_call}
    {pixel_name}.a = mondrian_ocio_preserved_alpha;
    mondrian_fragment_color = {pixel_name};
}}
"#,
        ocio_program = ocio_program,
        wrapper_set = wrapper.wrapper_bind_group,
        input_texture_binding = wrapper.input_texture_binding,
        input_sampler_binding = wrapper.input_sampler_binding,
        fragment_entry = wrapper.fragment_entry_point,
        output_location = wrapper.output_location,
        pixel_name = &link_plan.program_contract.pixel_name,
        ocio_program_call = ocio_program_call,
    );
    let debug_combined_source =
        format!("{vertex_source}\n/* ---- fragment ---- */\n{fragment_source}");
    Ok(OcioGpuWgpuWrapperShaderSources {
        vertex_source,
        fragment_source,
        debug_combined_source,
    })
}

fn wrapper_ocio_program_glsl_call(contract: &OcioGpuGeneratedProgramContract) -> String {
    match contract.call_style {
        OcioGpuGeneratedProgramCallStyle::ReturnsVec4 => {
            format!(
                "{pixel} = {function}({pixel});",
                pixel = contract.pixel_name,
                function = contract.function_name
            )
        }
        OcioGpuGeneratedProgramCallStyle::MutatesInOut => {
            format!(
                "{function}({pixel});",
                function = contract.function_name,
                pixel = contract.pixel_name
            )
        }
        OcioGpuGeneratedProgramCallStyle::Unknown => String::new(),
    }
}

fn lower_ocio_program_source_for_wgpu(shader_plan: &OcioGpuShaderPlan) -> Result<String, String> {
    let bundle = shader_plan.bundle();
    let binding_contract = binding_contract_for_plan(shader_plan);
    let sampler_policy = OcioGpuWgpuSamplerBindingPolicy::for_contract(&binding_contract)
        .map_err(|err| format!("OCIO sampler binding policy: {err:?}"))?;
    let source = strip_glsl_version_directives(&bundle.shader_text);
    let legacy_sampler_1d_names = legacy_sampler_names_for_kind(
        &source,
        &binding_contract,
        LegacySamplerDeclarationKind::Sampler1D,
    );
    let source = lower_legacy_sampler_declarations(&source, &binding_contract, &sampler_policy)?;
    lower_legacy_sampler_texture_calls(&source, &binding_contract, &legacy_sampler_1d_names)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacySamplerDeclarationKind {
    Sampler1D,
    Sampler2D,
    Sampler3D,
}

fn legacy_sampler_names_for_kind(
    source: &str,
    contract: &OcioGpuBindingContract,
    kind: LegacySamplerDeclarationKind,
) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            contract
                .textures_2d
                .iter()
                .find(|texture| {
                    legacy_sampler_declaration_matches(trimmed, kind, &texture.sampler_name)
                })
                .map(|texture| texture.sampler_name.clone())
        })
        .collect()
}

fn lower_legacy_sampler_declarations(
    source: &str,
    contract: &OcioGpuBindingContract,
    sampler_policy: &OcioGpuWgpuSamplerBindingPolicy,
) -> Result<String, String> {
    let mut lowered = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if legacy_sampler_declaration_match(trimmed, contract).is_some() {
            continue;
        }
        lowered.push(line.to_owned());
    }

    let mut declarations = Vec::new();
    for texture in &contract.textures_2d {
        let sampler_binding = sampler_policy
            .sampler_binding_for_texture(OcioGpuWgpuLutTextureDimension::D2, texture.index)
            .map_err(|err| format!("OCIO 1D/2D sampler binding: {err:?}"))?;
        declarations.push(format!(
            "layout(set = {set}, binding = {texture_binding}) uniform texture2D {texture_name};",
            set = contract.descriptor_set_index,
            texture_binding = texture.binding_index,
            texture_name = texture.texture_name,
        ));
        declarations.push(format!(
            "layout(set = {set}, binding = {sampler_binding}) uniform sampler {sampler_name};",
            set = contract.descriptor_set_index,
            sampler_name = texture.sampler_name,
        ));
    }
    for texture in &contract.textures_3d {
        let sampler_binding = sampler_policy
            .sampler_binding_for_texture(OcioGpuWgpuLutTextureDimension::D3, texture.index)
            .map_err(|err| format!("OCIO 3D sampler binding: {err:?}"))?;
        declarations.push(format!(
            "layout(set = {set}, binding = {texture_binding}) uniform texture3D {texture_name};",
            set = contract.descriptor_set_index,
            texture_binding = texture.binding_index,
            texture_name = texture.texture_name,
        ));
        declarations.push(format!(
            "layout(set = {set}, binding = {sampler_binding}) uniform sampler {sampler_name};",
            set = contract.descriptor_set_index,
            sampler_name = texture.sampler_name,
        ));
    }

    if declarations.is_empty() {
        Ok(lowered.join("\n"))
    } else {
        let mut output = declarations.join("\n");
        output.push('\n');
        output.push_str(&lowered.join("\n"));
        Ok(output)
    }
}

fn legacy_sampler_declaration_match<'a>(
    line: &str,
    contract: &'a OcioGpuBindingContract,
) -> Option<&'a str> {
    contract
        .textures_2d
        .iter()
        .find(|texture| {
            legacy_sampler_declaration_matches(
                line,
                LegacySamplerDeclarationKind::Sampler1D,
                &texture.sampler_name,
            ) || legacy_sampler_declaration_matches(
                line,
                LegacySamplerDeclarationKind::Sampler2D,
                &texture.sampler_name,
            )
        })
        .map(|texture| texture.sampler_name.as_str())
        .or_else(|| {
            contract
                .textures_3d
                .iter()
                .find(|texture| {
                    legacy_sampler_declaration_matches(
                        line,
                        LegacySamplerDeclarationKind::Sampler3D,
                        &texture.sampler_name,
                    )
                })
                .map(|texture| texture.sampler_name.as_str())
        })
}

fn legacy_sampler_declaration_matches(
    line: &str,
    kind: LegacySamplerDeclarationKind,
    sampler_name: &str,
) -> bool {
    let Some(code) = line.split("//").next() else {
        return false;
    };
    let declaration = code.trim();
    if !declaration.ends_with(';') {
        return false;
    }
    let declaration = declaration.trim_end_matches(';').trim();
    let mut tokens = declaration.split_whitespace();
    if !tokens.any(|token| token == "uniform") {
        return false;
    }
    let sampler_type = match kind {
        LegacySamplerDeclarationKind::Sampler1D => "sampler1D",
        LegacySamplerDeclarationKind::Sampler2D => "sampler2D",
        LegacySamplerDeclarationKind::Sampler3D => "sampler3D",
    };
    let tokens_after_uniform: Vec<_> = tokens.collect();
    tokens_after_uniform
        .windows(2)
        .any(|window| window[0] == sampler_type && window[1] == sampler_name)
}

fn lower_legacy_sampler_texture_calls(
    source: &str,
    contract: &OcioGpuBindingContract,
    legacy_sampler_1d_names: &BTreeSet<String>,
) -> Result<String, String> {
    let mut lowered = source.to_owned();
    for texture in &contract.textures_2d {
        let sampler_constructor = "sampler2D";
        let wrap_1d_coordinate = texture.dimensions == OcioGpuTextureDimensions::Texture1D
            || legacy_sampler_1d_names.contains(&texture.sampler_name);
        lowered = lower_texture_calls_for_sampler(
            &lowered,
            &texture.sampler_name,
            &texture.texture_name,
            sampler_constructor,
            wrap_1d_coordinate,
        )?;
    }
    for texture in &contract.textures_3d {
        lowered = lower_texture_calls_for_sampler(
            &lowered,
            &texture.sampler_name,
            &texture.texture_name,
            "sampler3D",
            false,
        )?;
    }
    Ok(lowered)
}

fn lower_texture_calls_for_sampler(
    source: &str,
    sampler_name: &str,
    texture_name: &str,
    sampler_constructor: &str,
    wrap_1d_coordinate: bool,
) -> Result<String, String> {
    let pattern = format!("texture({sampler_name},");
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(relative_start) = source[cursor..].find(&pattern) {
        let start = cursor + relative_start;
        output.push_str(&source[cursor..start]);
        let argument_start = start + pattern.len();
        let call_end = find_matching_texture_call_end(source, start)
            .ok_or_else(|| format!("unclosed texture() call for sampler '{sampler_name}'"))?;
        let coordinate = source[argument_start..call_end].trim();
        if wrap_1d_coordinate {
            output.push_str(&format!(
                "texture({sampler_constructor}({texture_name}, {sampler_name}), vec2({coordinate}, 0.5))"
            ));
        } else {
            output.push_str(&format!(
                "texture({sampler_constructor}({texture_name}, {sampler_name}), {coordinate})"
            ));
        }
        cursor = call_end + 1;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}

fn find_matching_texture_call_end(source: &str, call_start: usize) -> Option<usize> {
    let open = source[call_start..].find('(')? + call_start;
    let mut depth = 0usize;
    for (index, ch) in source[open..].char_indices() {
        match ch {
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open + index);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_glsl_version_directives(shader_text: &str) -> String {
    shader_text
        .lines()
        .filter(|line| !line.trim_start().starts_with("#version"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pipeline_layout_bind_group_layouts<'a>(
    plan: &OcioGpuWgpuPipelineLayoutPlan,
    ocio_bind_group: &'a OcioGpuWgpuOcioBindGroup,
    wrapper_bind_group: &'a OcioGpuWgpuWrapperBindGroup,
) -> Result<Vec<Option<&'a wgpu::BindGroupLayout>>, OcioGpuWgpuPipelineLayoutError> {
    pipeline_layout_bind_group_layouts_from_wrapper_layout(
        plan,
        ocio_bind_group,
        &wrapper_bind_group.layout,
    )
}

fn pipeline_layout_bind_group_layouts_from_wrapper_layout<'a>(
    plan: &OcioGpuWgpuPipelineLayoutPlan,
    ocio_bind_group: &'a OcioGpuWgpuOcioBindGroup,
    wrapper_layout: &'a wgpu::BindGroupLayout,
) -> Result<Vec<Option<&'a wgpu::BindGroupLayout>>, OcioGpuWgpuPipelineLayoutError> {
    let max_bind_group =
        plan.bind_groups.iter().map(|slot| slot.bind_group).max().unwrap_or_default();
    let len = usize::try_from(max_bind_group)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(OcioGpuWgpuPipelineLayoutError::BindGroupIndexOverflow {
            bind_group: max_bind_group,
        })?;
    let mut layouts = vec![None; len];
    for slot in &plan.bind_groups {
        let index = usize::try_from(slot.bind_group).map_err(|_| {
            OcioGpuWgpuPipelineLayoutError::BindGroupIndexOverflow { bind_group: slot.bind_group }
        })?;
        layouts[index] = Some(match slot.resource {
            OcioGpuWgpuPipelineBindGroupResource::OcioResources => &ocio_bind_group.layout,
            OcioGpuWgpuPipelineBindGroupResource::WrapperInput => wrapper_layout,
        });
    }
    Ok(layouts)
}

fn ocio_bind_group_entry<'a>(
    layout_entry: OcioGpuWgpuBindingPlan,
    bind_resource_plan: &OcioGpuWgpuBindResourcePlan,
    uploaded_luts: &'a OcioGpuWgpuUploadedLuts,
    uploaded_uniform: Option<&'a OcioGpuWgpuUploadedUniformBuffer>,
) -> Result<wgpu::BindGroupEntry<'a>, OcioGpuWgpuBindGroupError> {
    let resource_entry = bind_resource_entry_for_binding(bind_resource_plan, layout_entry.binding)?;
    match (layout_entry.resource, &resource_entry.resource) {
        (
            OcioGpuWgpuBindingResource::OcioUniformBuffer { .. },
            OcioGpuWgpuBindResource::UniformBuffer { byte_len, bytes_hash },
        ) => {
            let uniform = uploaded_uniform.ok_or(
                OcioGpuWgpuBindGroupError::MissingUploadedUniformBuffer {
                    binding: layout_entry.binding,
                },
            )?;
            validate_uploaded_uniform(layout_entry.binding, *byte_len, *bytes_hash, uniform)?;
            Ok(wgpu::BindGroupEntry {
                binding: layout_entry.binding,
                resource: uniform.buffer.as_entire_binding(),
            })
        }
        (
            OcioGpuWgpuBindingResource::OcioLutTexture2d { index },
            OcioGpuWgpuBindResource::LutTexture { .. },
        ) => {
            let texture = uploaded_luts
                .textures_2d
                .iter()
                .find(|texture| texture.index == index)
                .ok_or(OcioGpuWgpuBindGroupError::MissingUploadedTexture2D { index })?;
            validate_uploaded_texture(layout_entry.binding, &resource_entry.resource, texture)?;
            Ok(wgpu::BindGroupEntry {
                binding: layout_entry.binding,
                resource: wgpu::BindingResource::TextureView(&texture.view),
            })
        }
        (
            OcioGpuWgpuBindingResource::OcioLutTexture3d { index },
            OcioGpuWgpuBindResource::LutTexture { .. },
        ) => {
            let texture = uploaded_luts
                .textures_3d
                .iter()
                .find(|texture| texture.index == index)
                .ok_or(OcioGpuWgpuBindGroupError::MissingUploadedTexture3D { index })?;
            validate_uploaded_texture(layout_entry.binding, &resource_entry.resource, texture)?;
            Ok(wgpu::BindGroupEntry {
                binding: layout_entry.binding,
                resource: wgpu::BindingResource::TextureView(&texture.view),
            })
        }
        (
            OcioGpuWgpuBindingResource::OcioLutSampler2d { index },
            OcioGpuWgpuBindResource::LutSampler { sampler_name, .. },
        ) => {
            let texture = uploaded_luts
                .textures_2d
                .iter()
                .find(|texture| texture.index == index)
                .ok_or(OcioGpuWgpuBindGroupError::MissingUploadedTexture2D { index })?;
            validate_uploaded_sampler(index, sampler_name, texture)?;
            Ok(wgpu::BindGroupEntry {
                binding: layout_entry.binding,
                resource: wgpu::BindingResource::Sampler(&texture.sampler),
            })
        }
        (
            OcioGpuWgpuBindingResource::OcioLutSampler3d { index },
            OcioGpuWgpuBindResource::LutSampler { sampler_name, .. },
        ) => {
            let texture = uploaded_luts
                .textures_3d
                .iter()
                .find(|texture| texture.index == index)
                .ok_or(OcioGpuWgpuBindGroupError::MissingUploadedTexture3D { index })?;
            validate_uploaded_sampler(index, sampler_name, texture)?;
            Ok(wgpu::BindGroupEntry {
                binding: layout_entry.binding,
                resource: wgpu::BindingResource::Sampler(&texture.sampler),
            })
        }
        _ => Err(OcioGpuWgpuBindGroupError::MissingBindResourceEntry {
            binding: layout_entry.binding,
        }),
    }
}

fn bind_resource_entry_for_binding(
    bind_resource_plan: &OcioGpuWgpuBindResourcePlan,
    binding: u32,
) -> Result<&OcioGpuWgpuBindResourceEntry, OcioGpuWgpuBindGroupError> {
    bind_resource_plan
        .ocio_entries
        .iter()
        .find(|entry| entry.binding == binding)
        .ok_or(OcioGpuWgpuBindGroupError::MissingBindResourceEntry { binding })
}

fn validate_uploaded_uniform(
    binding: u32,
    byte_len: usize,
    bytes_hash: u64,
    uniform: &OcioGpuWgpuUploadedUniformBuffer,
) -> Result<(), OcioGpuWgpuBindGroupError> {
    if uniform.binding != binding {
        return Err(OcioGpuWgpuBindGroupError::MissingUploadedUniformBuffer { binding });
    }
    if uniform.byte_len != byte_len {
        return Err(OcioGpuWgpuBindGroupError::UniformMetadataMismatch {
            binding,
            reason: OcioGpuWgpuUploadedUniformMismatch::ByteLen {
                expected: byte_len,
                actual: uniform.byte_len,
            },
        });
    }
    if uniform.bytes_hash != bytes_hash {
        return Err(OcioGpuWgpuBindGroupError::UniformMetadataMismatch {
            binding,
            reason: OcioGpuWgpuUploadedUniformMismatch::BytesHash {
                expected: bytes_hash,
                actual: uniform.bytes_hash,
            },
        });
    }
    Ok(())
}

fn validate_uploaded_texture(
    binding: u32,
    expected: &OcioGpuWgpuBindResource,
    texture: &OcioGpuWgpuUploadedLutTexture,
) -> Result<(), OcioGpuWgpuBindGroupError> {
    let OcioGpuWgpuBindResource::LutTexture {
        index,
        texture_name,
        sampler_name,
        dimension,
        format,
        extent,
        source_values_hash,
        packed_bytes_hash,
    } = expected
    else {
        return Err(OcioGpuWgpuBindGroupError::MissingBindResourceEntry { binding });
    };
    if texture.binding_index != binding {
        return Err(texture_mismatch(
            *index,
            OcioGpuWgpuUploadedTextureMismatch::BindingIndex {
                expected: binding,
                actual: texture.binding_index,
            },
        ));
    }
    if texture.texture_name != *texture_name {
        return Err(texture_mismatch(
            *index,
            OcioGpuWgpuUploadedTextureMismatch::TextureName {
                expected: texture_name.clone(),
                actual: texture.texture_name.clone(),
            },
        ));
    }
    if texture.sampler_name != *sampler_name {
        return Err(texture_mismatch(
            *index,
            OcioGpuWgpuUploadedTextureMismatch::SamplerName {
                expected: sampler_name.clone(),
                actual: texture.sampler_name.clone(),
            },
        ));
    }
    if texture.format != *format {
        return Err(texture_mismatch(
            *index,
            OcioGpuWgpuUploadedTextureMismatch::Format {
                expected: *format,
                actual: texture.format,
            },
        ));
    }
    if texture.dimension != *dimension {
        return Err(texture_mismatch(
            *index,
            OcioGpuWgpuUploadedTextureMismatch::Dimension {
                expected: *dimension,
                actual: texture.dimension,
            },
        ));
    }
    if texture.extent != *extent {
        return Err(texture_mismatch(
            *index,
            OcioGpuWgpuUploadedTextureMismatch::Extent {
                expected: *extent,
                actual: texture.extent,
            },
        ));
    }
    if texture.source_values_hash != *source_values_hash {
        return Err(texture_mismatch(
            *index,
            OcioGpuWgpuUploadedTextureMismatch::SourceValuesHash {
                expected: *source_values_hash,
                actual: texture.source_values_hash,
            },
        ));
    }
    if texture.packed_bytes_hash != *packed_bytes_hash {
        return Err(texture_mismatch(
            *index,
            OcioGpuWgpuUploadedTextureMismatch::PackedBytesHash {
                expected: *packed_bytes_hash,
                actual: texture.packed_bytes_hash,
            },
        ));
    }
    Ok(())
}

fn validate_uploaded_sampler(
    index: u32,
    sampler_name: &str,
    texture: &OcioGpuWgpuUploadedLutTexture,
) -> Result<(), OcioGpuWgpuBindGroupError> {
    if texture.sampler_name != sampler_name {
        return Err(texture_mismatch(
            index,
            OcioGpuWgpuUploadedTextureMismatch::SamplerName {
                expected: sampler_name.to_owned(),
                actual: texture.sampler_name.clone(),
            },
        ));
    }
    Ok(())
}

fn texture_mismatch(
    index: u32,
    reason: OcioGpuWgpuUploadedTextureMismatch,
) -> OcioGpuWgpuBindGroupError {
    OcioGpuWgpuBindGroupError::TextureMetadataMismatch { index, reason }
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

fn wrapper_shader_module_artifact_key(
    source: &OcioGpuWgpuWrapperShaderSourceArtifact,
    pipeline_layout: &OcioGpuWgpuPipelineLayoutPlan,
    render_descriptor: &OcioGpuWgpuRenderPipelineDescriptorPlan,
) -> Result<u64, OcioGpuWgpuWrapperShaderModuleArtifactError> {
    let actual_source_hash = hash_value(&(source.vertex_source_hash, source.fragment_source_hash));
    if source.source_hash != actual_source_hash {
        return Err(
            OcioGpuWgpuWrapperShaderModuleArtifactError::SourceHashMismatch {
                expected: source.source_hash,
                actual: actual_source_hash,
            },
        );
    }
    if source.resource_key != pipeline_layout.resource_key {
        return Err(
            OcioGpuWgpuWrapperShaderModuleArtifactError::PipelineLayoutResourceKeyMismatch {
                expected: source.resource_key,
                actual: pipeline_layout.resource_key,
            },
        );
    }
    if source.resource_key != render_descriptor.resource_key {
        return Err(
            OcioGpuWgpuWrapperShaderModuleArtifactError::RenderDescriptorResourceKeyMismatch {
                expected: source.resource_key,
                actual: render_descriptor.resource_key,
            },
        );
    }
    if pipeline_layout.layout_hash != render_descriptor.pipeline_layout_hash {
        return Err(
            OcioGpuWgpuWrapperShaderModuleArtifactError::PipelineLayoutHashMismatch {
                expected: pipeline_layout.layout_hash,
                actual: render_descriptor.pipeline_layout_hash,
            },
        );
    }
    if source.vertex_entry_point != render_descriptor.shader_contract.vertex_entry_point {
        return Err(
            OcioGpuWgpuWrapperShaderModuleArtifactError::VertexEntryPointMismatch {
                expected: source.vertex_entry_point.clone(),
                actual: render_descriptor.shader_contract.vertex_entry_point.clone(),
            },
        );
    }
    if source.fragment_entry_point != render_descriptor.shader_contract.fragment_entry_point {
        return Err(
            OcioGpuWgpuWrapperShaderModuleArtifactError::FragmentEntryPointMismatch {
                expected: source.fragment_entry_point.clone(),
                actual: render_descriptor.shader_contract.fragment_entry_point.clone(),
            },
        );
    }
    if source.output_location != render_descriptor.shader_contract.output_location {
        return Err(
            OcioGpuWgpuWrapperShaderModuleArtifactError::OutputLocationMismatch {
                expected: source.output_location,
                actual: render_descriptor.shader_contract.output_location,
            },
        );
    }

    let mut hasher = DefaultHasher::new();
    source.resource_key.hash(&mut hasher);
    source.link_hash.hash(&mut hasher);
    source.source_hash.hash(&mut hasher);
    pipeline_layout.layout_hash.hash(&mut hasher);
    render_descriptor.descriptor_hash.hash(&mut hasher);
    render_descriptor.output_format.hash(&mut hasher);
    Ok(hasher.finish())
}

fn wrapper_backend_shader_modules_cache_key(
    artifact: &OcioGpuWgpuWrapperShaderModuleArtifact,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    artifact.resource_key.hash(&mut hasher);
    artifact.module_key.hash(&mut hasher);
    artifact.source_hash.hash(&mut hasher);
    artifact.pipeline_layout_hash.hash(&mut hasher);
    artifact.render_descriptor_hash.hash(&mut hasher);
    artifact.vertex.source_hash.hash(&mut hasher);
    artifact.fragment.source_hash.hash(&mut hasher);
    hasher.finish()
}

fn render_pipeline_cache_key(
    descriptor: &OcioGpuWgpuRenderPipelineDescriptorPlan,
    pipeline_layout_resource_key: u64,
    pipeline_layout_hash: u64,
    modules: OcioGpuWgpuWrapperShaderModuleKeyMetadata,
) -> Result<u64, OcioGpuWgpuRenderPipelineError> {
    if descriptor.resource_key != pipeline_layout_resource_key {
        return Err(
            OcioGpuWgpuRenderPipelineError::PipelineLayoutResourceKeyMismatch {
                expected: descriptor.resource_key,
                actual: pipeline_layout_resource_key,
            },
        );
    }
    if descriptor.resource_key != modules.resource_key {
        return Err(
            OcioGpuWgpuRenderPipelineError::ShaderModuleResourceKeyMismatch {
                expected: descriptor.resource_key,
                actual: modules.resource_key,
            },
        );
    }
    if descriptor.pipeline_layout_hash != pipeline_layout_hash {
        return Err(OcioGpuWgpuRenderPipelineError::PipelineLayoutHashMismatch {
            expected: descriptor.pipeline_layout_hash,
            actual: pipeline_layout_hash,
        });
    }
    if modules.pipeline_layout_hash != pipeline_layout_hash {
        return Err(
            OcioGpuWgpuRenderPipelineError::ShaderModulePipelineLayoutHashMismatch {
                expected: pipeline_layout_hash,
                actual: modules.pipeline_layout_hash,
            },
        );
    }
    if modules.render_descriptor_hash != descriptor.descriptor_hash {
        return Err(
            OcioGpuWgpuRenderPipelineError::ShaderModuleRenderDescriptorHashMismatch {
                expected: descriptor.descriptor_hash,
                actual: modules.render_descriptor_hash,
            },
        );
    }

    let mut hasher = DefaultHasher::new();
    descriptor.resource_key.hash(&mut hasher);
    descriptor.descriptor_hash.hash(&mut hasher);
    pipeline_layout_hash.hash(&mut hasher);
    modules.cache_key.hash(&mut hasher);
    modules.module_key.hash(&mut hasher);
    Ok(hasher.finish())
}

#[derive(Debug, Clone, Copy)]
struct OcioGpuWgpuWrapperShaderModuleKeyMetadata {
    resource_key: u64,
    pipeline_layout_hash: u64,
    render_descriptor_hash: u64,
    cache_key: u64,
    module_key: u64,
}

fn render_pass_node_hash(
    pipeline: OcioGpuWgpuRenderPipelineMetadata,
    ocio_bind_group_resource_key: u64,
    ocio_layout_hash: u64,
    wrapper_layout_hash: u64,
    output_format: OcioGpuWgpuColorTargetFormat,
) -> Result<u64, OcioGpuWgpuRenderPassError> {
    if pipeline.resource_key != ocio_bind_group_resource_key {
        return Err(
            OcioGpuWgpuRenderPassError::OcioBindGroupResourceKeyMismatch {
                expected: pipeline.resource_key,
                actual: ocio_bind_group_resource_key,
            },
        );
    }

    let mut hasher = DefaultHasher::new();
    pipeline.resource_key.hash(&mut hasher);
    pipeline.cache_key.hash(&mut hasher);
    pipeline.descriptor_hash.hash(&mut hasher);
    ocio_layout_hash.hash(&mut hasher);
    wrapper_layout_hash.hash(&mut hasher);
    output_format.hash(&mut hasher);
    Ok(hasher.finish())
}

fn validate_render_pass_contract(
    plan: &OcioGpuWgpuRenderPassNodePlan,
    pipeline: &OcioGpuWgpuRenderPipeline,
    ocio_bind_group: &OcioGpuWgpuOcioBindGroup,
    wrapper_bind_group: &OcioGpuWgpuWrapperBindGroup,
    target: &OcioGpuWgpuRenderPassTarget<'_>,
) -> Result<(), OcioGpuWgpuRenderPassError> {
    if plan.resource_key != ocio_bind_group.resource_key {
        return Err(
            OcioGpuWgpuRenderPassError::OcioBindGroupResourceKeyMismatch {
                expected: plan.resource_key,
                actual: ocio_bind_group.resource_key,
            },
        );
    }
    if plan.resource_key != target.resource_key {
        return Err(OcioGpuWgpuRenderPassError::TargetResourceKeyMismatch {
            expected: plan.resource_key,
            actual: target.resource_key,
        });
    }
    if plan.output_format != target.output_format {
        return Err(OcioGpuWgpuRenderPassError::TargetFormatMismatch {
            expected: plan.output_format,
            actual: target.output_format,
        });
    }
    if plan.render_pipeline_cache_key != pipeline.cache_key {
        return Err(OcioGpuWgpuRenderPassError::PipelineCacheKeyMismatch {
            expected: plan.render_pipeline_cache_key,
            actual: pipeline.cache_key,
        });
    }
    if plan.ocio_layout_hash != ocio_bind_group.layout_hash {
        return Err(OcioGpuWgpuRenderPassError::OcioLayoutHashMismatch {
            expected: plan.ocio_layout_hash,
            actual: ocio_bind_group.layout_hash,
        });
    }
    if plan.wrapper_layout_hash != wrapper_bind_group.layout_hash {
        return Err(OcioGpuWgpuRenderPassError::WrapperLayoutHashMismatch {
            expected: plan.wrapper_layout_hash,
            actual: wrapper_bind_group.layout_hash,
        });
    }
    Ok(())
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
    interpolation: OcioGpuTextureInterpolation,
    label: &str,
) -> wgpu::SamplerDescriptor<'_> {
    let filter = match sampler_filtering_for_interpolation(interpolation) {
        OcioGpuWgpuSamplerFiltering::Filtering => wgpu::FilterMode::Linear,
        OcioGpuWgpuSamplerFiltering::NonFiltering => wgpu::FilterMode::Nearest,
    };
    wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    }
}

fn sampler_filtering_for_interpolation(
    interpolation: OcioGpuTextureInterpolation,
) -> OcioGpuWgpuSamplerFiltering {
    if interpolation == OcioGpuTextureInterpolation::Nearest {
        OcioGpuWgpuSamplerFiltering::NonFiltering
    } else {
        OcioGpuWgpuSamplerFiltering::Filtering
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

fn backend_object_cache_key(static_pipeline: &OcioGpuWgpuPreparedStaticPipeline) -> u64 {
    let mut hasher = DefaultHasher::new();
    static_pipeline.resources.resources.resource_key.hash(&mut hasher);
    static_pipeline.resources.binding_layout.layout_hash.hash(&mut hasher);
    static_pipeline.wrapper_binding.layout_hash.hash(&mut hasher);
    static_pipeline.pipeline_layout.layout_hash.hash(&mut hasher);
    static_pipeline.wrapper_module_artifact.module_key.hash(&mut hasher);
    static_pipeline.render_descriptor.descriptor_hash.hash(&mut hasher);
    static_pipeline.render_descriptor.output_format.hash(&mut hasher);
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
    use crate::GpuContext;
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

    #[test]
    fn lut_sampler_descriptor_matches_ocio_host_interpolation_contract() {
        for interpolation in [
            OcioGpuTextureInterpolation::Unknown,
            OcioGpuTextureInterpolation::Linear,
            OcioGpuTextureInterpolation::Tetrahedral,
            OcioGpuTextureInterpolation::Cubic,
            OcioGpuTextureInterpolation::Default,
            OcioGpuTextureInterpolation::Best,
        ] {
            let descriptor = sampler_descriptor_for_interpolation(interpolation, "linear-lut");
            assert_eq!(descriptor.mag_filter, wgpu::FilterMode::Linear);
            assert_eq!(descriptor.min_filter, wgpu::FilterMode::Linear);
        }

        let descriptor = sampler_descriptor_for_interpolation(
            OcioGpuTextureInterpolation::Nearest,
            "nearest-lut",
        );
        assert_eq!(descriptor.mag_filter, wgpu::FilterMode::Nearest);
        assert_eq!(descriptor.min_filter, wgpu::FilterMode::Nearest);
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

    fn shader_plan_with_text(shader_text: &str) -> OcioGpuShaderPlan {
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::Rec709.into(),
            dst: ColorSpace::Srgb.into(),
            language: GpuLanguage::Glsl4_0,
        };
        let bundle = Arc::new(OcioGpuShaderBundle {
            src_color_space: "test-src".to_owned(),
            dst_color_space: "test-dst".to_owned(),
            language: GpuLanguage::Glsl4_0,
            shader_text: shader_text.to_owned(),
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
        plan_from_bundle(request, bundle)
    }

    fn shader_plan_with_single_2d_texture_binding(binding_index: u32) -> OcioGpuShaderPlan {
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::Rec709.into(),
            dst: ColorSpace::Srgb.into(),
            language: GpuLanguage::Glsl4_0,
        };
        let values = vec![0.0, 0.5, 1.0];
        let bundle = Arc::new(OcioGpuShaderBundle {
            src_color_space: "test-src".to_owned(),
            dst_color_space: "test-dst".to_owned(),
            language: GpuLanguage::Glsl4_0,
            shader_text: callable_ocio_program_text().to_owned(),
            descriptor_set_index: 0,
            texture_binding_start: 1,
            uniform_buffer_binding: 0,
            uniform_buffer_size: 0,
            texture_2d_count: 1,
            texture_3d_count: 0,
            uniform_count: 0,
            textures_2d: vec![mondrian_core::OcioGpuTexture2DBinding {
                index: 0,
                texture_name: "lut2d".to_owned(),
                sampler_name: "lut2d_sampler".to_owned(),
                binding_index,
                channel: OcioGpuTextureChannel::Rgb,
                dimensions: OcioGpuTextureDimensions::Texture2D,
                interpolation: OcioGpuTextureInterpolation::Linear,
                width: 1,
                height: 1,
                value_count: values.len(),
                values,
            }],
            textures_3d: Vec::new(),
            uniforms: Vec::new(),
            cache_id: Some("test-cache".to_owned()),
        });
        plan_from_bundle(request, bundle)
    }

    fn fragment_shader_plan_with_single_2d_texture_binding(
        shader_text: &str,
        binding_index: u32,
    ) -> OcioGpuShaderPlan {
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::Rec709.into(),
            dst: ColorSpace::Srgb.into(),
            language: GpuLanguage::Glsl4_0,
        };
        let values = vec![0.0, 0.5, 1.0];
        let bundle = Arc::new(OcioGpuShaderBundle {
            src_color_space: "test-src".to_owned(),
            dst_color_space: "test-dst".to_owned(),
            language: GpuLanguage::Glsl4_0,
            shader_text: shader_text.to_owned(),
            descriptor_set_index: 0,
            texture_binding_start: 1,
            uniform_buffer_binding: 0,
            uniform_buffer_size: 0,
            texture_2d_count: 1,
            texture_3d_count: 0,
            uniform_count: 0,
            textures_2d: vec![mondrian_core::OcioGpuTexture2DBinding {
                index: 0,
                texture_name: "lut2d".to_owned(),
                sampler_name: "lut2d_sampler".to_owned(),
                binding_index,
                channel: OcioGpuTextureChannel::Rgb,
                dimensions: OcioGpuTextureDimensions::Texture2D,
                interpolation: OcioGpuTextureInterpolation::Linear,
                width: 1,
                height: 1,
                value_count: values.len(),
                values,
            }],
            textures_3d: Vec::new(),
            uniforms: Vec::new(),
            cache_id: Some("test-cache".to_owned()),
        });
        plan_from_bundle(request, bundle)
    }

    fn shader_plan_with_single_uniform_buffer_size(buffer_size: usize) -> OcioGpuShaderPlan {
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::Rec709.into(),
            dst: ColorSpace::Srgb.into(),
            language: GpuLanguage::Glsl4_0,
        };
        let value = OcioGpuUniformValue::F32(vec![1.0]);
        let bundle = Arc::new(OcioGpuShaderBundle {
            src_color_space: "test-src".to_owned(),
            dst_color_space: "test-dst".to_owned(),
            language: GpuLanguage::Glsl4_0,
            shader_text: callable_ocio_program_text().to_owned(),
            descriptor_set_index: 0,
            texture_binding_start: 1,
            uniform_buffer_binding: 0,
            uniform_buffer_size: buffer_size,
            texture_2d_count: 0,
            texture_3d_count: 0,
            uniform_count: 1,
            textures_2d: Vec::new(),
            textures_3d: Vec::new(),
            uniforms: vec![mondrian_core::OcioGpuUniformBinding {
                index: 0,
                name: "exposure".to_owned(),
                uniform_type: OcioGpuUniformType::VectorFloat,
                buffer_offset: 0,
                value_count: 1,
                value,
            }],
            cache_id: Some("test-cache".to_owned()),
        });
        plan_from_bundle(request, bundle)
    }

    fn callable_ocio_program_text() -> &'static str {
        r#"
            #version 450 core

            void mondrian_ocio_main(inout vec4 mondrian_ocio_pixel) {
                mondrian_ocio_pixel.rgb = clamp(mondrian_ocio_pixel.rgb, vec3(0.0), vec3(1.0));
            }
        "#
    }

    fn returning_ocio_program_text() -> &'static str {
        r#"
            #version 450 core

            vec4 mondrian_ocio_main(vec4 inPixel) {
                vec4 mondrian_ocio_pixel = inPixel;
                mondrian_ocio_pixel.rgb = sqrt(max(mondrian_ocio_pixel.rgb, vec3(0.0)));
                return mondrian_ocio_pixel;
            }
        "#
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
    fn resource_plan_rejects_binding_contract_count_mismatch() {
        let mut plan = shader_plan_with_text(callable_ocio_program_text());
        plan.texture_2d_count = 1;

        let err = OcioGpuWgpuResourcePlan::for_shader_plan(&plan)
            .expect_err("texture count mismatch should fail");

        assert!(matches!(
            err,
            OcioGpuBindingContractValidationError::Texture2DCountMismatch {
                expected: 1,
                actual: 0
            }
        ));
    }

    #[test]
    fn resource_plan_rejects_texture_binding_before_ocio_start() {
        let plan = shader_plan_with_single_2d_texture_binding(0);

        let err = OcioGpuWgpuResourcePlan::for_shader_plan(&plan)
            .expect_err("texture binding before start should fail");

        assert!(matches!(
            err,
            OcioGpuBindingContractValidationError::TextureBindingBeforeStart {
                dimension: OcioGpuWgpuLutTextureDimension::D2,
                index: 0,
                binding: 0,
                texture_binding_start: 1
            }
        ));
    }

    #[test]
    fn resource_plan_rejects_uniform_without_uniform_buffer() {
        let plan = shader_plan_with_single_uniform_buffer_size(0);

        let err = OcioGpuWgpuResourcePlan::for_shader_plan(&plan)
            .expect_err("uniform without buffer should fail");

        assert!(matches!(
            err,
            OcioGpuBindingContractValidationError::MissingUniformBuffer { uniform_count: 1 }
        ));
    }

    #[test]
    fn backend_prep_runtime_prepares_static_pipeline_and_reuses_caches() {
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let mut runtime = OcioGpuWgpuBackendPrepRuntime::default();

        let first = runtime
            .prepare_static_pipeline(&shader_plan, OcioGpuWgpuColorTargetFormat::Rgba16Float)
            .expect("prepare static pipeline");

        assert_eq!(
            first.resources.resources.shader_hash,
            shader_plan.shader_hash
        );
        assert_eq!(
            first.wrapper_binding.bind_group,
            first.resources.resources.wrapper_contract.bind_group
        );
        assert_eq!(
            first.pipeline_layout.resource_key,
            first.resources.resources.resource_key
        );
        assert_eq!(
            first.wrapper_link.resource_key,
            first.resources.resources.resource_key
        );
        assert_eq!(
            first.render_descriptor.output_format,
            OcioGpuWgpuColorTargetFormat::Rgba16Float
        );
        assert_eq!(
            first.wrapper_module_artifact.render_descriptor_hash,
            first.render_descriptor.descriptor_hash
        );
        assert_eq!(
            first.wrapper_module_artifact.pipeline_layout_hash,
            first.pipeline_layout.layout_hash
        );

        let diagnostics = runtime.diagnostics();
        assert_eq!(diagnostics.resources.entries, 1);
        assert_eq!(diagnostics.resources.misses, 1);
        assert_eq!(diagnostics.resources.hits, 0);
        assert_eq!(diagnostics.wrapper_module_artifacts.entries, 1);
        assert_eq!(diagnostics.wrapper_module_artifacts.misses, 1);
        assert_eq!(diagnostics.wrapper_module_artifacts.hits, 0);

        let second = runtime
            .prepare_static_pipeline(&shader_plan, OcioGpuWgpuColorTargetFormat::Rgba16Float)
            .expect("reuse static pipeline");

        assert!(Arc::ptr_eq(&first.resources, &second.resources));
        assert!(Arc::ptr_eq(
            &first.wrapper_module_artifact,
            &second.wrapper_module_artifact
        ));
        let diagnostics = runtime.diagnostics();
        assert_eq!(diagnostics.resources.entries, 1);
        assert_eq!(diagnostics.resources.misses, 1);
        assert_eq!(diagnostics.resources.hits, 1);
        assert_eq!(diagnostics.wrapper_module_artifacts.entries, 1);
        assert_eq!(diagnostics.wrapper_module_artifacts.misses, 1);
        assert_eq!(diagnostics.wrapper_module_artifacts.hits, 1);
    }

    #[test]
    fn backend_prep_runtime_surfaces_wrapper_link_blockers() {
        let shader_plan = shader_plan_with_text("void unrelated(inout vec4 color) {}");
        let mut runtime = OcioGpuWgpuBackendPrepRuntime::default();

        let err = runtime
            .prepare_static_pipeline(&shader_plan, OcioGpuWgpuColorTargetFormat::Rgba16Float)
            .expect_err("unlinked wrapper should fail");

        assert!(matches!(
            err,
            OcioGpuWgpuBackendPrepError::WrapperSource(
                OcioGpuWgpuWrapperShaderArtifactError::LinkPlanBlocked { .. }
            )
        ));
        let diagnostics = runtime.diagnostics();
        assert_eq!(diagnostics.resources.entries, 1);
        assert_eq!(diagnostics.wrapper_module_artifacts.entries, 0);
    }

    #[test]
    fn backend_object_cache_key_is_stable_and_output_format_sensitive() {
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let mut runtime = OcioGpuWgpuBackendPrepRuntime::default();
        let first = runtime
            .prepare_static_pipeline(&shader_plan, OcioGpuWgpuColorTargetFormat::Rgba16Float)
            .expect("prepare first static pipeline");
        let second = runtime
            .prepare_static_pipeline(&shader_plan, OcioGpuWgpuColorTargetFormat::Rgba16Float)
            .expect("prepare second static pipeline");
        let different_format = runtime
            .prepare_static_pipeline(&shader_plan, OcioGpuWgpuColorTargetFormat::Rgba32Float)
            .expect("prepare different static pipeline");

        assert_eq!(
            backend_object_cache_key(&first),
            backend_object_cache_key(&second)
        );
        assert_ne!(
            backend_object_cache_key(&first),
            backend_object_cache_key(&different_format)
        );
    }

    #[test]
    fn backend_object_runtime_diagnostics_start_empty() {
        let runtime = OcioGpuWgpuBackendObjectRuntime::default();

        assert_eq!(
            runtime.diagnostics(),
            OcioGpuWgpuBackendObjectRuntimeDiagnostics {
                entries: 0,
                hits: 0,
                misses: 0,
                failures: 0,
                wrapper_modules: OcioGpuWgpuWrapperShaderModuleCache::default().diagnostics(),
                render_pipelines: OcioGpuWgpuRenderPipelineCache::default().diagnostics(),
            }
        );
    }

    #[tokio::test]
    async fn backend_object_runtime_creates_filtering_lut_pipeline_on_real_wgpu_device() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = GpuContext::new().await else {
            eprintln!("skipping real wgpu OCIO backend object test: no GPU adapter available");
            return;
        };
        if !context.device.features().contains(wgpu::Features::FLOAT32_FILTERABLE) {
            eprintln!("skipping filtering OCIO backend test: float32 filtering unavailable");
            return;
        }
        let mut shader_cache = OcioGpuShaderCache::default();
        let shader_plan = shader_cache
            .get_or_extract(OcioGpuShaderRequest::ColorSpace {
                src: ColorSpace::AppleLogBt2020.into(),
                dst: ColorSpace::Rec709.into(),
                language: GpuLanguage::Glsl4_0,
            })
            .expect("extract default OCIO shader");
        let mut prep_runtime = OcioGpuWgpuBackendPrepRuntime::default();
        let static_pipeline = prep_runtime
            .prepare_static_pipeline(&shader_plan, OcioGpuWgpuColorTargetFormat::Rgba16Float)
            .expect("prepare static OCIO GPU pipeline");
        let mut object_runtime = OcioGpuWgpuBackendObjectRuntime::default();
        assert!(static_pipeline
            .resources
            .binding_layout
            .entries
            .iter()
            .any(|entry| entry.filtering == OcioGpuWgpuSamplerFiltering::Filtering));

        let first = object_runtime
            .prepare_backend_objects(
                &context.device,
                &context.queue,
                &shader_plan,
                &static_pipeline,
            )
            .expect("create wgpu OCIO backend objects");
        let second = object_runtime
            .prepare_backend_objects(
                &context.device,
                &context.queue,
                &shader_plan,
                &static_pipeline,
            )
            .expect("reuse wgpu OCIO backend objects");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            first.resource_key,
            static_pipeline.resources.resources.resource_key
        );
        assert_eq!(
            first.wrapper_modules.module_key,
            static_pipeline.wrapper_module_artifact.module_key
        );
        assert_eq!(
            first.wrapper_modules.pipeline_layout_hash,
            static_pipeline.pipeline_layout.layout_hash
        );
        assert_eq!(
            first.wrapper_modules.render_descriptor_hash,
            static_pipeline.render_descriptor.descriptor_hash
        );
        assert_eq!(
            first.render_pipeline.descriptor_hash,
            static_pipeline.render_descriptor.descriptor_hash
        );
        assert_eq!(
            first.pass_node.wrapper_layout_hash,
            first.wrapper_input_layout.layout_hash
        );
        assert_ne!(first.wrapper_modules.cache_key, 0);
        assert_ne!(first.render_pipeline.cache_key, 0);
        assert_ne!(first.pass_node.node_hash, 0);

        let diagnostics = object_runtime.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.failures, 0);
        assert_eq!(diagnostics.wrapper_modules.entries, 1);
        assert_eq!(diagnostics.wrapper_modules.misses, 1);
        assert_eq!(diagnostics.wrapper_modules.hits, 0);
        assert_eq!(diagnostics.render_pipelines.entries, 1);
        assert_eq!(diagnostics.render_pipelines.misses, 1);
        assert_eq!(diagnostics.render_pipelines.hits, 0);

        let (device_without_filtering, queue_without_filtering) = context
            .adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("request device without optional float32 filtering");
        let mut unsupported_runtime = OcioGpuWgpuBackendObjectRuntime::default();
        let error = match unsupported_runtime.prepare_backend_objects(
            &device_without_filtering,
            &queue_without_filtering,
            &shader_plan,
            &static_pipeline,
        ) {
            Err(error) => error,
            Ok(_) => panic!("filtering LUT must fail before wgpu object creation"),
        };
        assert_eq!(
            error,
            OcioGpuWgpuBackendObjectError::Float32FilteringUnsupported
        );
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
                        filtering: OcioGpuWgpuSamplerFiltering::Filtering,
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
                        filtering: OcioGpuWgpuSamplerFiltering::Filtering,
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
                        sample_type: OcioGpuWgpuTextureSampleType::Float32 { filterable: true },
                    }
        }));
        assert!(ocio_descriptor.entries.iter().any(|entry| {
            entry.binding == 5
                && entry.resource
                    == OcioGpuWgpuLayoutBindingResource::Sampler {
                        filtering: OcioGpuWgpuSamplerFiltering::Filtering,
                    }
        }));
        assert_eq!(wrapper_descriptor.bind_group, 1);
        assert!(wrapper_descriptor.entries.iter().any(|entry| {
            entry.binding == 0
                && entry.resource
                    == OcioGpuWgpuLayoutBindingResource::SampledTexture {
                        dimension: OcioGpuWgpuLutTextureDimension::D2,
                        sample_type: OcioGpuWgpuTextureSampleType::Float32 { filterable: false },
                    }
        }));
        assert!(wrapper_descriptor.entries.iter().any(|entry| {
            entry.binding == 1
                && entry.resource
                    == OcioGpuWgpuLayoutBindingResource::Sampler {
                        filtering: OcioGpuWgpuSamplerFiltering::NonFiltering,
                    }
        }));
        assert_ne!(ocio_descriptor.layout_hash, wrapper_descriptor.layout_hash);
    }

    #[test]
    fn wrapper_link_plan_accepts_callable_ocio_program_shape() {
        let resources = bind_resource_test_plan(42);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());

        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        assert!(link_plan.can_link());
        assert_eq!(
            link_plan.program_contract.source_kind,
            OcioGpuGeneratedProgramSourceKind::CallableFunction
        );
        assert_eq!(
            link_plan.program_contract.call_style,
            OcioGpuGeneratedProgramCallStyle::MutatesInOut
        );
        assert!(link_plan.program_contract.function_present);
        assert!(link_plan.program_contract.pixel_name_present);
        assert!(!link_plan.program_contract.main_function_present);
        assert!(link_plan.blockers.is_empty());
        assert_ne!(link_plan.link_hash, 0);
    }

    #[test]
    fn wrapper_link_plan_accepts_returning_ocio_program_shape() {
        let resources = bind_resource_test_plan(42);
        let shader_plan = shader_plan_with_text(returning_ocio_program_text());

        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        assert!(link_plan.can_link());
        assert_eq!(
            link_plan.program_contract.call_style,
            OcioGpuGeneratedProgramCallStyle::ReturnsVec4
        );
        assert!(link_plan.blockers.is_empty());
    }

    #[test]
    fn wrapper_link_plan_blocks_complete_fragment_shader_shape() {
        let resources = bind_resource_test_plan(43);
        let shader_plan = shader_plan_with_text(
            r#"
                void mondrian_ocio_main(inout vec4 mondrian_ocio_pixel) {
                    mondrian_ocio_pixel = vec4(1.0);
                }
                void main() {}
            "#,
        );

        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        assert!(!link_plan.can_link());
        assert_eq!(
            link_plan.program_contract.source_kind,
            OcioGpuGeneratedProgramSourceKind::CompleteFragmentShader
        );
        assert!(link_plan
            .blockers
            .contains(&OcioGpuWgpuWrapperLinkBlocker::CompleteFragmentShaderRequiresSplit));
    }

    #[test]
    fn wrapper_link_plan_blocks_missing_function_and_pixel_contract() {
        let resources = bind_resource_test_plan(44);
        let shader_plan =
            shader_plan_with_text("vec4 unrelated_color(vec4 color) { return color; }");

        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        assert!(!link_plan.can_link());
        assert_eq!(
            link_plan.program_contract.source_kind,
            OcioGpuGeneratedProgramSourceKind::Unknown
        );
        assert!(link_plan.blockers.iter().any(|blocker| matches!(
            blocker,
            OcioGpuWgpuWrapperLinkBlocker::MissingFunctionName { .. }
        )));
        assert!(link_plan.blockers.iter().any(|blocker| matches!(
            blocker,
            OcioGpuWgpuWrapperLinkBlocker::MissingPixelName { .. }
        )));
        assert!(link_plan.blockers.contains(&OcioGpuWgpuWrapperLinkBlocker::UnknownProgramShape));
    }

    #[test]
    fn wrapper_shader_artifact_generates_stage_split_fullscreen_sources() {
        let resources = bind_resource_test_plan(45);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        let artifact = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&shader_plan, &link_plan)
            .expect("generate wrapper shader source");

        assert_eq!(artifact.resource_key, resources.resource_key);
        assert_eq!(artifact.link_hash, link_plan.link_hash);
        assert_eq!(artifact.ocio_shader_hash, shader_plan.shader_hash);
        assert_eq!(artifact.vertex_entry_point, "main");
        assert_eq!(artifact.fragment_entry_point, "main");
        assert_eq!(
            artifact.output_location,
            resources.wrapper_contract.output_location
        );
        assert_ne!(artifact.source_hash, 0);
        assert_ne!(artifact.vertex_source_hash, 0);
        assert_ne!(artifact.fragment_source_hash, 0);
        assert_ne!(artifact.vertex_source_hash, artifact.fragment_source_hash);
        assert_eq!(artifact.vertex_source.matches("#version").count(), 1);
        assert_eq!(artifact.fragment_source.matches("#version").count(), 1);
        assert_eq!(
            artifact.debug_combined_source.matches("#version").count(),
            2
        );
        assert!(artifact.vertex_source.contains("void main()"));
        assert!(!artifact.vertex_source.contains("mondrian_ocio_main"));
        assert!(artifact
            .fragment_source
            .contains("layout(set = 1, binding = 0) uniform texture2D"));
        assert!(artifact
            .fragment_source
            .contains("layout(set = 1, binding = 1) uniform sampler"));
        assert!(artifact.fragment_source.contains("void main()"));
        assert!(artifact.fragment_source.contains("mondrian_ocio_main(mondrian_ocio_pixel);"));
        assert!(artifact
            .fragment_source
            .contains("float mondrian_ocio_preserved_alpha = mondrian_ocio_pixel.a;"));
        assert!(artifact
            .fragment_source
            .contains("mondrian_ocio_pixel.a = mondrian_ocio_preserved_alpha;"));
    }

    #[test]
    fn wrapper_shader_artifact_assigns_returning_ocio_program_result() {
        let resources = bind_resource_test_plan(45);
        let shader_plan = shader_plan_with_text(returning_ocio_program_text());
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        let artifact = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&shader_plan, &link_plan)
            .expect("generate wrapper shader source");

        assert!(artifact
            .fragment_source
            .contains("mondrian_ocio_pixel = mondrian_ocio_main(mondrian_ocio_pixel);"));
    }

    #[test]
    fn wrapper_shader_artifact_fragment_translation_is_structured_success_or_failure() {
        let resources = bind_resource_test_plan(46);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);
        let artifact = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&shader_plan, &link_plan)
            .expect("generate wrapper shader source");
        let required_bindings = binding_contract_for_plan(&shader_plan);
        let request = OcioGpuShaderTranslationRequest {
            source_language: GpuLanguage::Glsl4_0,
            target_language: OcioGpuShaderTargetLanguage::NagaIr,
            stage: OcioGpuShaderStage::Fragment,
            source_shader_hash: artifact.fragment_source_hash,
            binding_contract_hash: required_bindings.stable_hash(),
        };

        match translate_shader_text(request, &artifact.fragment_source, required_bindings) {
            Ok(translated) => {
                assert_eq!(translated.request, request);
                assert_eq!(translated.entry_point_count, 1);
            }
            Err(err) => match err.reason {
                OcioGpuShaderTranslationFailure::ParseFailed { .. }
                | OcioGpuShaderTranslationFailure::ValidationFailed { .. } => {
                    assert_eq!(err.request, request);
                }
                other => panic!("unexpected wrapper fragment translation failure: {other:?}"),
            },
        }
    }

    #[test]
    fn wrapper_shader_artifact_rejects_blocked_link_plan() {
        let resources = bind_resource_test_plan(47);
        let shader_plan = shader_plan_with_text("void main() {}");
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        let err = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&shader_plan, &link_plan)
            .expect_err("blocked link plan should not generate wrapper source");

        assert!(matches!(
            err,
            OcioGpuWgpuWrapperShaderArtifactError::LinkPlanBlocked { .. }
        ));
    }

    #[test]
    fn wrapper_shader_artifact_rejects_shader_hash_mismatch() {
        let resources = bind_resource_test_plan(48);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let other_shader_plan = shader_plan_with_text(
            "void mondrian_ocio_main(inout vec4 mondrian_ocio_pixel) { mondrian_ocio_pixel *= 0.5; }",
        );
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        let err = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&other_shader_plan, &link_plan)
            .expect_err("shader hash mismatch should fail");

        assert!(matches!(
            err,
            OcioGpuWgpuWrapperShaderArtifactError::ShaderHashMismatch { .. }
        ));
    }

    #[test]
    fn pipeline_layout_and_render_descriptor_preserve_fullscreen_contract() {
        let resources = bind_resource_test_plan(49);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let ocio_layout =
            resources.binding_layout_plan().expect("binding layout with separated samplers");
        let wrapper_layout =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&resources.wrapper_contract);
        let ocio_descriptor =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_ocio_resources(&ocio_layout);
        let wrapper_descriptor =
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_wrapper_input(&wrapper_layout);
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);

        let pipeline_layout = OcioGpuWgpuPipelineLayoutPlan::for_bind_groups(
            &resources,
            &ocio_layout,
            &wrapper_layout,
        );
        assert!(link_plan.can_link());
        let render_descriptor = OcioGpuWgpuRenderPipelineDescriptorPlan::for_pipeline_layout(
            &resources,
            &pipeline_layout,
            &link_plan,
            OcioGpuWgpuColorTargetFormat::Rgba16Float,
        );

        assert_eq!(pipeline_layout.resource_key, resources.resource_key);
        assert_eq!(pipeline_layout.bind_groups.len(), 2);
        assert_eq!(
            pipeline_layout.bind_groups[0],
            OcioGpuWgpuPipelineBindGroupSlot {
                bind_group: 0,
                resource: OcioGpuWgpuPipelineBindGroupResource::OcioResources,
                layout_hash: ocio_descriptor.layout_hash,
            }
        );
        assert_eq!(
            pipeline_layout.bind_groups[1],
            OcioGpuWgpuPipelineBindGroupSlot {
                bind_group: 1,
                resource: OcioGpuWgpuPipelineBindGroupResource::WrapperInput,
                layout_hash: wrapper_descriptor.layout_hash,
            }
        );
        assert_ne!(pipeline_layout.layout_hash, 0);
        assert_eq!(render_descriptor.resource_key, resources.resource_key);
        assert_eq!(
            render_descriptor.pipeline_layout_hash,
            pipeline_layout.layout_hash
        );
        assert_eq!(render_descriptor.shader_contract, link_plan.shader_contract);
        assert_eq!(
            render_descriptor.output_format,
            OcioGpuWgpuColorTargetFormat::Rgba16Float
        );
        assert_ne!(render_descriptor.descriptor_hash, 0);
        assert_eq!(
            render_descriptor.primitive_state().topology,
            wgpu::PrimitiveTopology::TriangleStrip
        );
        assert_eq!(
            render_descriptor.color_target_state().format,
            wgpu::TextureFormat::Rgba16Float
        );
    }

    #[test]
    fn wrapper_shader_module_artifact_cache_reuses_validated_stage_split_naga_modules() {
        let resources = bind_resource_test_plan(50);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let ocio_layout =
            resources.binding_layout_plan().expect("binding layout with separated samplers");
        let wrapper_layout =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&resources.wrapper_contract);
        let pipeline_layout = OcioGpuWgpuPipelineLayoutPlan::for_bind_groups(
            &resources,
            &ocio_layout,
            &wrapper_layout,
        );
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);
        let source = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&shader_plan, &link_plan)
            .expect("generate wrapper shader source");
        let render_descriptor = OcioGpuWgpuRenderPipelineDescriptorPlan::for_pipeline_layout(
            &resources,
            &pipeline_layout,
            &link_plan,
            OcioGpuWgpuColorTargetFormat::Rgba16Float,
        );
        let mut cache = OcioGpuWgpuWrapperShaderModuleArtifactCache::default();

        let first = cache
            .translate(&source, &pipeline_layout, &render_descriptor)
            .expect("translate wrapper sources to Naga modules");
        let second = cache
            .translate(&source, &pipeline_layout, &render_descriptor)
            .expect("reuse wrapper Naga modules");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.resource_key, resources.resource_key);
        assert_eq!(first.link_hash, link_plan.link_hash);
        assert_eq!(first.source_hash, source.source_hash);
        assert_eq!(first.pipeline_layout_hash, pipeline_layout.layout_hash);
        assert_eq!(
            first.render_descriptor_hash,
            render_descriptor.descriptor_hash
        );
        assert_eq!(
            first.output_format,
            OcioGpuWgpuColorTargetFormat::Rgba16Float
        );
        assert_ne!(first.module_key, 0);
        assert_eq!(first.vertex.stage, OcioGpuShaderStage::Vertex);
        assert_eq!(first.fragment.stage, OcioGpuShaderStage::Fragment);
        assert_eq!(first.vertex.source_hash, source.vertex_source_hash);
        assert_eq!(first.fragment.source_hash, source.fragment_source_hash);
        assert_eq!(first.vertex.entry_point_count, 1);
        assert_eq!(first.fragment.entry_point_count, 1);
        assert!(first
            .vertex
            .naga_module
            .entry_points
            .iter()
            .any(|entry| entry.name == source.vertex_entry_point));
        assert!(first
            .fragment
            .naga_module
            .entry_points
            .iter()
            .any(|entry| entry.name == source.fragment_entry_point));

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.failures, 0);
    }

    #[test]
    fn wrapper_shader_module_artifact_rejects_mismatched_pipeline_contracts() {
        let resources = bind_resource_test_plan(51);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let ocio_layout =
            resources.binding_layout_plan().expect("binding layout with separated samplers");
        let wrapper_layout =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&resources.wrapper_contract);
        let pipeline_layout = OcioGpuWgpuPipelineLayoutPlan::for_bind_groups(
            &resources,
            &ocio_layout,
            &wrapper_layout,
        );
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);
        let source = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&shader_plan, &link_plan)
            .expect("generate wrapper shader source");
        let mut render_descriptor = OcioGpuWgpuRenderPipelineDescriptorPlan::for_pipeline_layout(
            &resources,
            &pipeline_layout,
            &link_plan,
            OcioGpuWgpuColorTargetFormat::Rgba16Float,
        );
        render_descriptor.pipeline_layout_hash =
            render_descriptor.pipeline_layout_hash.wrapping_add(1);
        let mut cache = OcioGpuWgpuWrapperShaderModuleArtifactCache::default();

        let err = cache
            .translate(&source, &pipeline_layout, &render_descriptor)
            .expect_err("pipeline layout mismatch must fail before translation");

        assert!(matches!(
            err,
            OcioGpuWgpuWrapperShaderModuleArtifactError::PipelineLayoutHashMismatch { .. }
        ));
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.misses, 0);
        assert_eq!(diagnostics.failures, 1);
    }

    #[test]
    fn render_pipeline_cache_key_binds_descriptor_layout_and_wrapper_modules() {
        let resources = bind_resource_test_plan(52);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let ocio_layout =
            resources.binding_layout_plan().expect("binding layout with separated samplers");
        let wrapper_layout =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&resources.wrapper_contract);
        let pipeline_layout = OcioGpuWgpuPipelineLayoutPlan::for_bind_groups(
            &resources,
            &ocio_layout,
            &wrapper_layout,
        );
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);
        let source = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&shader_plan, &link_plan)
            .expect("generate wrapper shader source");
        let render_descriptor = OcioGpuWgpuRenderPipelineDescriptorPlan::for_pipeline_layout(
            &resources,
            &pipeline_layout,
            &link_plan,
            OcioGpuWgpuColorTargetFormat::Rgba16Float,
        );
        let module_artifact = OcioGpuWgpuWrapperShaderModuleArtifact::translate(
            &source,
            &pipeline_layout,
            &render_descriptor,
        )
        .expect("translate wrapper modules");
        let module_cache_key = wrapper_backend_shader_modules_cache_key(&module_artifact);
        let metadata = OcioGpuWgpuWrapperShaderModuleKeyMetadata {
            resource_key: module_artifact.resource_key,
            pipeline_layout_hash: module_artifact.pipeline_layout_hash,
            render_descriptor_hash: module_artifact.render_descriptor_hash,
            cache_key: module_cache_key,
            module_key: module_artifact.module_key,
        };

        let key = render_pipeline_cache_key(
            &render_descriptor,
            pipeline_layout.resource_key,
            pipeline_layout.layout_hash,
            metadata,
        )
        .expect("render pipeline cache key");

        assert_ne!(module_cache_key, 0);
        assert_ne!(key, 0);
    }

    #[test]
    fn render_pipeline_cache_key_rejects_mismatched_contracts() {
        let resources = bind_resource_test_plan(53);
        let shader_plan = shader_plan_with_text(callable_ocio_program_text());
        let ocio_layout =
            resources.binding_layout_plan().expect("binding layout with separated samplers");
        let wrapper_layout =
            OcioGpuWgpuWrapperBindingPlan::for_contract(&resources.wrapper_contract);
        let pipeline_layout = OcioGpuWgpuPipelineLayoutPlan::for_bind_groups(
            &resources,
            &ocio_layout,
            &wrapper_layout,
        );
        let link_plan = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&shader_plan, &resources);
        let source = OcioGpuWgpuWrapperShaderSourceArtifact::generate(&shader_plan, &link_plan)
            .expect("generate wrapper shader source");
        let render_descriptor = OcioGpuWgpuRenderPipelineDescriptorPlan::for_pipeline_layout(
            &resources,
            &pipeline_layout,
            &link_plan,
            OcioGpuWgpuColorTargetFormat::Rgba16Float,
        );
        let module_artifact = OcioGpuWgpuWrapperShaderModuleArtifact::translate(
            &source,
            &pipeline_layout,
            &render_descriptor,
        )
        .expect("translate wrapper modules");
        let metadata = OcioGpuWgpuWrapperShaderModuleKeyMetadata {
            resource_key: module_artifact.resource_key,
            pipeline_layout_hash: module_artifact.pipeline_layout_hash,
            render_descriptor_hash: module_artifact.render_descriptor_hash,
            cache_key: wrapper_backend_shader_modules_cache_key(&module_artifact),
            module_key: module_artifact.module_key,
        };

        assert!(matches!(
            render_pipeline_cache_key(
                &render_descriptor,
                pipeline_layout.resource_key.wrapping_add(1),
                pipeline_layout.layout_hash,
                metadata,
            ),
            Err(OcioGpuWgpuRenderPipelineError::PipelineLayoutResourceKeyMismatch { .. })
        ));

        let mut mismatched_modules = metadata;
        mismatched_modules.pipeline_layout_hash =
            mismatched_modules.pipeline_layout_hash.wrapping_add(1);
        assert!(matches!(
            render_pipeline_cache_key(
                &render_descriptor,
                pipeline_layout.resource_key,
                pipeline_layout.layout_hash,
                mismatched_modules,
            ),
            Err(OcioGpuWgpuRenderPipelineError::ShaderModulePipelineLayoutHashMismatch { .. })
        ));

        let mut mismatched_descriptor = metadata;
        mismatched_descriptor.render_descriptor_hash =
            mismatched_descriptor.render_descriptor_hash.wrapping_add(1);
        assert!(matches!(
            render_pipeline_cache_key(
                &render_descriptor,
                pipeline_layout.resource_key,
                pipeline_layout.layout_hash,
                mismatched_descriptor,
            ),
            Err(OcioGpuWgpuRenderPipelineError::ShaderModuleRenderDescriptorHashMismatch { .. })
        ));
    }

    #[test]
    fn render_pass_node_plan_binds_pipeline_bind_groups_and_target_contract() {
        let pipeline = OcioGpuWgpuRenderPipelineMetadata {
            resource_key: 54,
            cache_key: 101,
            descriptor_hash: 202,
        };

        let plan = OcioGpuWgpuRenderPassNodePlan::for_pipeline_metadata_and_bind_groups(
            pipeline,
            54,
            303,
            404,
            OcioGpuWgpuColorTargetFormat::Rgba16Float,
        )
        .expect("render pass node plan");

        assert_eq!(plan.resource_key, 54);
        assert_eq!(plan.render_pipeline_cache_key, 101);
        assert_eq!(plan.render_descriptor_hash, 202);
        assert_eq!(plan.ocio_layout_hash, 303);
        assert_eq!(plan.wrapper_layout_hash, 404);
        assert_eq!(
            plan.output_format,
            OcioGpuWgpuColorTargetFormat::Rgba16Float
        );
        assert_eq!(plan.vertex_count, 4);
        assert_ne!(plan.node_hash, 0);
    }

    #[test]
    fn render_pass_node_plan_rejects_mismatched_ocio_resource_key() {
        let pipeline = OcioGpuWgpuRenderPipelineMetadata {
            resource_key: 55,
            cache_key: 101,
            descriptor_hash: 202,
        };

        let err = OcioGpuWgpuRenderPassNodePlan::for_pipeline_metadata_and_bind_groups(
            pipeline,
            56,
            303,
            404,
            OcioGpuWgpuColorTargetFormat::Rgba16Float,
        )
        .expect_err("resource key mismatch must fail");

        assert_eq!(
            err,
            OcioGpuWgpuRenderPassError::OcioBindGroupResourceKeyMismatch {
                expected: 55,
                actual: 56
            }
        );
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
            src: ColorSpace::SonySLog3SGamut3Cine.into(),
            dst: ColorSpace::Rec709.into(),
            language: GpuLanguage::Glsl4_0,
        };

        let first = cache.get_or_extract(request.clone()).expect("extract color shader");
        let second = cache.get_or_extract(request).expect("cache color shader");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.shader_len > 0);
        assert_eq!(first.bundle().src_color_space, "S-Log3 S-Gamut3.Cine");
        assert_eq!(first.bundle().dst_color_space, "Camera Rec.709");
        assert!(first.processor_cache_id.as_deref().is_some_and(|id| !id.is_empty()));
        let resources = OcioGpuWgpuResourcePlan::for_shader_plan(&first).expect("resource plan");
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
        let program_contract = OcioGpuGeneratedProgramContract::for_shader_plan(&first);
        assert!(program_contract.function_present);
        assert!(program_contract.pixel_name_present);
        assert_ne!(
            program_contract.source_kind,
            OcioGpuGeneratedProgramSourceKind::Unknown
        );
        assert_ne!(
            program_contract.call_style,
            OcioGpuGeneratedProgramCallStyle::Unknown
        );
        let wrapper_link = OcioGpuWgpuWrapperLinkPlan::for_shader_plan(&first, &resources);
        assert_eq!(wrapper_link.resource_key, resources.resource_key);
        assert_eq!(wrapper_link.shader_hash, first.shader_hash);
        assert_ne!(wrapper_link.link_hash, 0);
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
                src: ColorSpace::Rec709.into(),
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
    fn legacy_sampler_declaration_matching_accepts_glsl_qualifiers() {
        assert!(legacy_sampler_declaration_matches(
            "uniform sampler1D lut_sampler;",
            LegacySamplerDeclarationKind::Sampler1D,
            "lut_sampler"
        ));
        assert!(legacy_sampler_declaration_matches(
            "layout(set = 0, binding = 4) uniform highp sampler2D lut_sampler; // OCIO LUT",
            LegacySamplerDeclarationKind::Sampler2D,
            "lut_sampler"
        ));
        assert!(legacy_sampler_declaration_matches(
            "layout(binding = 7) uniform sampler3D cube_sampler;",
            LegacySamplerDeclarationKind::Sampler3D,
            "cube_sampler"
        ));
        assert!(!legacy_sampler_declaration_matches(
            "vec4 value = texture(lut_sampler, uv);",
            LegacySamplerDeclarationKind::Sampler2D,
            "lut_sampler"
        ));
        assert!(!legacy_sampler_declaration_matches(
            "uniform sampler2D unrelated_sampler;",
            LegacySamplerDeclarationKind::Sampler2D,
            "lut_sampler"
        ));
    }

    #[test]
    fn backend_prep_lowers_default_display_view_legacy_samplers_to_wgpu_glsl() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let (display, view) = ocio_default_display_view().expect("default display/view");
        let mut cache = OcioGpuShaderCache::default();
        let shader_plan = cache
            .get_or_extract(OcioGpuShaderRequest::DisplayView {
                src: ColorSpace::Rec709.into(),
                display,
                view,
                language: GpuLanguage::Glsl4_0,
            })
            .expect("extract display shader");
        let extracted_source = &shader_plan.bundle().shader_text;
        let has_legacy_1d = extracted_source.contains("uniform sampler1D");
        let has_legacy_2d = extracted_source.contains("uniform sampler2D");
        let has_legacy_3d = extracted_source.contains("uniform sampler3D");
        assert!(
            has_legacy_1d || has_legacy_2d || has_legacy_3d,
            "the stock display View must exercise OCIO legacy sampler lowering"
        );
        let mut prep = OcioGpuWgpuBackendPrepRuntime::default();

        let pipeline = prep
            .prepare_static_pipeline(&shader_plan, OcioGpuWgpuColorTargetFormat::Rgba8Unorm)
            .expect("prepare display/view static GPU pipeline");

        let lowered_source = &pipeline.wrapper_source.fragment_source;
        if has_legacy_1d {
            assert!(!lowered_source.contains("uniform sampler1D"));
            assert!(lowered_source.contains("uniform texture2D"));
            assert!(lowered_source.contains("sampler2D("));
        }
        if has_legacy_2d {
            assert!(!lowered_source.contains("uniform sampler2D"));
            assert!(lowered_source.contains("uniform texture2D"));
            assert!(lowered_source.contains("sampler2D("));
        }
        if has_legacy_3d {
            assert!(!lowered_source.contains("uniform sampler3D"));
            assert!(lowered_source.contains("uniform texture3D"));
            assert!(lowered_source.contains("sampler3D("));
        }
        assert_eq!(
            pipeline.wrapper_module_artifact.fragment.entry_point_count,
            1
        );
        assert_eq!(
            pipeline.pipeline_layout.ocio_layout_hash,
            OcioGpuWgpuBindGroupLayoutDescriptorPlan::for_ocio_resources(
                &pipeline.resources.binding_layout
            )
            .layout_hash
        );
    }

    #[test]
    fn wgpu_execution_preparation_returns_backend_ready_resource_contract() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let mut cache = OcioGpuShaderCache::default();

        let prepared = cache
            .prepare_wgpu_execution(OcioGpuShaderRequest::ColorSpace {
                src: ColorSpace::AppleLogBt2020.into(),
                dst: ColorSpace::Rec709.into(),
                language: GpuLanguage::Glsl4_0,
            })
            .expect("prepare wgpu execution");

        assert!(prepared.can_execute());
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
        assert!(prepared.blockers.is_empty());

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
                src: ColorSpace::SonySLog3SGamut3Cine.into(),
                dst: ColorSpace::Rec709.into(),
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
            src: ColorSpace::Rec709.into(),
            dst: ColorSpace::Srgb.into(),
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
    fn shader_translation_cache_keys_identical_source_by_binding_contract() {
        let shader_text = r#"
            #version 450 core
            layout(location = 0) out vec4 frag_color;

            void main() {
                frag_color = vec4(1.0, 0.5, 0.25, 1.0);
            }
        "#;
        let first_plan = fragment_shader_plan_with_single_2d_texture_binding(shader_text, 1);
        let second_plan = fragment_shader_plan_with_single_2d_texture_binding(shader_text, 2);
        let mut cache = OcioGpuShaderTranslationCache::default();

        let first = cache.translate(&first_plan).expect("translate first contract");
        let second = cache.translate(&second_plan).expect("translate second contract");
        let first_again = cache.translate(&first_plan).expect("reuse first contract");

        assert_eq!(
            first.request.source_shader_hash,
            second.request.source_shader_hash
        );
        assert_ne!(
            first.request.binding_contract_hash,
            second.request.binding_contract_hash
        );
        assert_eq!(first.required_bindings.textures_2d[0].binding_index, 1);
        assert_eq!(second.required_bindings.textures_2d[0].binding_index, 2);
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(&first, &first_again));

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 2);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.misses, 2);
        assert_eq!(diagnostics.failures, 0);
    }

    #[test]
    fn backend_shader_module_cache_key_rejects_contract_mismatch() {
        let request = OcioGpuShaderRequest::ColorSpace {
            src: ColorSpace::Rec709.into(),
            dst: ColorSpace::Srgb.into(),
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
        let resources = OcioGpuWgpuResourcePlan::for_shader_plan(&plan).expect("resource plan");
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
            src: ColorSpace::Rec709.into(),
            dst: ColorSpace::Srgb.into(),
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
                src: ColorSpace::SonySLog3SGamut3Cine.into(),
                dst: ColorSpace::Rec709.into(),
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
                src: ColorSpace::SonySLog3SGamut3Cine.into(),
                dst: ColorSpace::Rec709.into(),
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
