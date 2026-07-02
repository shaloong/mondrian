use lru::LruCache;
use mondrian_core::{
    extract_ocio_display_gpu_shader_bundle, extract_ocio_gpu_shader_bundle, ColorSpace,
    GpuLanguage, OcioGpuShaderBundle,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Arc;

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

fn hash_value<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
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
}
