//! OCIO (OpenColorIO) integration for color management.
//!
//! Color transforms are delegated to an OCIO v2.5.2 config whenever a config /
//! processor can be resolved. `ColorEngine::MondrianSmart` is the productized
//! default policy and resolves to Mondrian's built-in OCIO config; custom OCIO
//! mode resolves from [`OcioConfigSource`].
//!
//! The config source is determined by [`OcioConfigSource`]:
//!
//! 1. **MondrianDefault** — Mondrian Standard/Simple built-in config
//! 2. **Builtin** — named built-in config (e.g. `"aces_1.2"`)
//! 3. **Path** — explicit `config.ocio` file path
//! 4. **Environment** — `$OCIO` env var → standard system paths

use crate::types::{ColorSpace, OcioConfigSource};
pub use ocio_rs::GpuLanguage;
use ocio_rs::{
    BuiltinConfigRegistry, CPUProcessor, Config, GpuShaderDesc,
    GpuTextureChannel as OcioRsGpuTextureChannel,
    GpuTextureDimensions as OcioRsGpuTextureDimensions, Interpolation as OcioRsInterpolation,
};
use std::path::{Path, PathBuf};

// ── Global OCIO state ──────────────────────────────────────────────────────────

static OCIO_CONFIG_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Intended name for Mondrian's bundled default OCIO config.
pub const MONDRIAN_DEFAULT_OCIO_CONFIG_NAME: &str = "mondrian_default_ocio_v1";

const MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH: &str = "embedded:mondrian_default_ocio_v1";
const MONDRIAN_DEFAULT_OCIO_CONFIG: &str =
    include_str!("../assets/ocio/mondrian_default_ocio_v1.ocio");

/// Return the pinned OCIO config text used by Mondrian Standard mode.
pub fn mondrian_default_ocio_config_text() -> &'static str {
    MONDRIAN_DEFAULT_OCIO_CONFIG
}

/// Load an OCIO config from `path` and set it as the process-wide current config.
///
/// Safe to call again when the user switches configs.
pub fn init_ocio(path: &Path) -> Result<(), String> {
    let config = Config::from_file(path.to_string_lossy().as_ref())
        .map_err(|e| format!("failed to load OCIO config from {}: {e}", path.display()))?;

    ocio_rs::set_current_config(&config);

    // The global OCIO context now holds a reference (ref-counted by the C++
    // library).  We deliberately forget the Rust wrapper so the ref-count
    // never reaches zero while the process is alive.
    std::mem::forget(config);

    if let Ok(mut guard) = OCIO_CONFIG_PATH.lock() {
        *guard = Some(path.to_path_buf());
    }

    tracing::info!(path=%path.display(), "OCIO config loaded");
    Ok(())
}

/// Load an OCIO built-in config by name and set it as the current config.
pub fn init_ocio_builtin(name: &str) -> Result<(), String> {
    let registry = BuiltinConfigRegistry::get()
        .map_err(|e| format!("failed to access built-in config registry: {e}"))?;

    let config = registry
        .config_by_name(name)
        .ok_or_else(|| format!("built-in OCIO config not found: '{name}'"))?;

    ocio_rs::set_current_config(&config);

    // Mark as loaded with a virtual path so `ensure_ocio_loaded` works.
    let virtual_path = PathBuf::from(format!("builtin:{name}"));
    if let Ok(mut guard) = OCIO_CONFIG_PATH.lock() {
        *guard = Some(virtual_path);
    }

    // Keep the registry alive — its Config references need it.
    std::mem::forget(registry);

    tracing::info!(builtin=%name, "OCIO built-in config loaded");
    Ok(())
}

/// Load Mondrian's embedded default OCIO config and set it as the current config.
pub fn init_mondrian_default_ocio() -> Result<(), String> {
    let config = Config::from_stream(MONDRIAN_DEFAULT_OCIO_CONFIG).map_err(|e| {
        format!(
            "failed to load embedded Mondrian OCIO config '{}': {e}",
            MONDRIAN_DEFAULT_OCIO_CONFIG_NAME
        )
    })?;

    ocio_rs::set_current_config(&config);
    std::mem::forget(config);

    if let Ok(mut guard) = OCIO_CONFIG_PATH.lock() {
        *guard = Some(PathBuf::from(MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH));
    }

    tracing::info!(
        config = MONDRIAN_DEFAULT_OCIO_CONFIG_NAME,
        "Mondrian embedded OCIO config loaded"
    );
    Ok(())
}

/// Return the currently-loaded OCIO config path, if any.
pub fn ocio_config_path() -> Option<PathBuf> {
    OCIO_CONFIG_PATH.lock().ok()?.clone()
}

/// Return `true` when an OCIO config has been loaded.
pub fn ocio_available() -> bool {
    OCIO_CONFIG_PATH.lock().map(|g| g.is_some()).unwrap_or(false)
}

// ── Resolver (env var + standard paths + builtin) ──────────────────────────────

/// Resolve an [`OcioConfigSource`] and load the corresponding config.
///
/// This is the single entry point that callers should use.  It is idempotent:
/// calling it again with the same effective source is a no-op.
pub fn ensure_ocio_loaded(source: &OcioConfigSource) -> Result<(), String> {
    match source {
        OcioConfigSource::MondrianDefault => ensure_mondrian_default_ocio_loaded(),
        OcioConfigSource::Builtin { name } => {
            let virtual_path = PathBuf::from(format!("builtin:{name}"));
            if already_loaded_with(&virtual_path) {
                return Ok(());
            }
            init_ocio_builtin(name)
        }
        OcioConfigSource::Path { path } => {
            if already_loaded_with(path) {
                return Ok(());
            }
            if path.exists() {
                return init_ocio(path);
            }
            Err(format!(
                "OCIO config file not found: {}\n\
                 Place a config.ocio file at this path or change the OCIO source in project settings.",
                path.display()
            ))
        }
        OcioConfigSource::Environment => {
            let resolved = resolve_from_environment()?;
            if already_loaded_with(&resolved) {
                return Ok(());
            }
            init_ocio(&resolved)
        }
    }
}

/// Return the OCIO source used by Mondrian Standard/Simple mode.
pub fn mondrian_default_ocio_source() -> OcioConfigSource {
    OcioConfigSource::MondrianDefault
}

/// Ensure Mondrian's default OCIO config is loaded.
pub fn ensure_mondrian_default_ocio_loaded() -> Result<(), String> {
    let virtual_path = PathBuf::from(MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH);
    if already_loaded_with(&virtual_path) {
        return Ok(());
    }

    init_mondrian_default_ocio()
}

/// Return true when Mondrian's default OCIO config is currently loaded.
pub fn mondrian_default_ocio_available() -> bool {
    already_loaded_with(&PathBuf::from(MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH))
}

/// Check whether the config whose path is `path` is already loaded.
fn already_loaded_with(path: &Path) -> bool {
    OCIO_CONFIG_PATH.lock().map(|g| g.as_deref() == Some(path)).unwrap_or(false)
}

/// Resolve an OCIO config path from environment / standard locations.
///
/// Priority:
/// 1. `OCIO` environment variable
/// 2. Standard system paths (per platform)
fn resolve_from_environment() -> Result<PathBuf, String> {
    // 1. `$OCIO` environment variable (industry standard)
    if let Ok(env_path) = std::env::var("OCIO") {
        let p = PathBuf::from(&env_path);
        if p.exists() {
            tracing::info!(path=%p.display(), "using OCIO config from $OCIO");
            return Ok(p);
        }
        tracing::warn!(path=%env_path, "$OCIO points to a non-existent file");
    }

    // 2. Standard system paths
    for candidate in standard_ocio_paths() {
        if candidate.exists() {
            tracing::info!(path=%candidate.display(), "using OCIO config from standard path");
            return Ok(candidate);
        }
    }

    Err(
        "no OCIO config found — set the OCIO environment variable or place a config.ocio in:\n\
         • $OCIO (environment variable)\n\
         • ~/.config/ocio/config.ocio (Linux)\n\
         • %APPDATA%/ocio/config.ocio (Windows)\n\
         • ~/Library/Preferences/ocio/config.ocio (macOS)"
            .to_string(),
    )
}

/// Standard OCIO config search paths for the current platform.
fn standard_ocio_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(&home).join("Library/Preferences/ocio/config.ocio"));
        }
        paths.push(PathBuf::from(
            "/Library/Application Support/ocio/config.ocio",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(&home).join(".config/ocio/config.ocio"));
        }
        paths.push(PathBuf::from("/etc/ocio/config.ocio"));
        paths.push(PathBuf::from("/usr/share/ocio/config.ocio"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            paths.push(PathBuf::from(&appdata).join("ocio/config.ocio"));
        }
        if let Ok(programdata) = std::env::var("PROGRAMDATA") {
            paths.push(PathBuf::from(&programdata).join("ocio/config.ocio"));
        }
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            paths.push(PathBuf::from(&localappdata).join("ocio/config.ocio"));
        }
    }

    paths
}

// ── Built-in config listing (for UI presets) ───────────────────────────────────

/// Return the list of available built-in OCIO config names.
///
/// These come from the OCIO library bundled with the application.
/// Returns an empty vec when no built-in configs are compiled in.
pub fn builtin_config_names() -> Vec<String> {
    let Ok(registry) = BuiltinConfigRegistry::get() else {
        return Vec::new();
    };
    let n = registry.num_builtin_configs();
    (0..n).filter_map(|i| registry.config_name(i)).collect()
}

/// Return the list of available built-in config names with their UI labels.
pub fn builtin_config_entries() -> Vec<(String, String)> {
    let Ok(registry) = BuiltinConfigRegistry::get() else {
        return Vec::new();
    };
    let n = registry.num_builtin_configs();
    (0..n)
        .filter_map(|i| {
            let name = registry.config_name(i)?;
            let ui_name = registry.config_ui_name(i).unwrap_or_else(|| name.clone());
            Some((name, ui_name))
        })
        .collect()
}

// ── Color-space name mapping ───────────────────────────────────────────────────

/// Map a Mondrian [`ColorSpace`] to its pinned OCIO color-space name.
///
/// These names are part of Mondrian's color-space contract and are validated
/// against the embedded `mondrian_default_ocio_v1` config. Custom OCIO configs
/// should provide the same names or aliases if they want to use Mondrian's
/// built-in `ColorSpace` enum directly.
pub fn ocio_color_space_name(cs: ColorSpace) -> &'static str {
    match cs {
        ColorSpace::Srgb => "sRGB Encoded Rec.709 (sRGB)",
        ColorSpace::Rec709 => "Camera Rec.709",
        ColorSpace::Rec2020 => "Linear Rec.2020",
        ColorSpace::Rec2100Pq => "Rec.2100-PQ - Display",
        ColorSpace::Rec2100Hlg => "Rec.2100-HLG - Display",
        ColorSpace::DciP3 => "sRGB Encoded P3-D65",
        ColorSpace::AppleLog => "Apple Log",
        ColorSpace::SLog3 => "S-Log3 S-Gamut3.Cine",
        ColorSpace::ArriLogC4 => "ARRI LogC4",
    }
}

/// GPU shader resources extracted from an OCIO processor.
#[derive(Debug, Clone)]
pub struct OcioGpuShaderBundle {
    /// The OCIO color-space name used as processor input.
    pub src_color_space: String,
    /// The OCIO color-space or display/view name used as processor output.
    pub dst_color_space: String,
    /// The shader language requested from OCIO.
    pub language: GpuLanguage,
    /// OCIO-generated shader source.
    pub shader_text: String,
    /// OCIO descriptor set index used by generated resource declarations.
    pub descriptor_set_index: u32,
    /// First OCIO texture binding slot. Binding 0 is reserved for uniform data by convention.
    pub texture_binding_start: u32,
    /// Uniform buffer binding slot used by Mondrian's OCIO descriptor policy.
    pub uniform_buffer_binding: u32,
    /// Packed OCIO uniform buffer size in bytes.
    pub uniform_buffer_size: usize,
    /// Number of 1D/2D texture resources referenced by the shader.
    pub texture_2d_count: u32,
    /// Number of 3D texture resources referenced by the shader.
    pub texture_3d_count: u32,
    /// Number of uniforms referenced by the shader.
    pub uniform_count: u32,
    /// OCIO 1D/2D LUT resource binding metadata.
    pub textures_2d: Vec<OcioGpuTexture2DBinding>,
    /// OCIO 3D LUT resource binding metadata.
    pub textures_3d: Vec<OcioGpuTexture3DBinding>,
    /// Stable OCIO processor cache id for renderer-side shader caching.
    pub cache_id: Option<String>,
}

/// OCIO 1D/2D LUT resource binding metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuTextureChannel {
    /// Single-channel red texture payload.
    Red,
    /// Three-channel RGB texture payload.
    Rgb,
}

/// OCIO 1D/2D LUT dimensionality metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuTextureDimensions {
    /// Logical 1D LUT stored in a 1D/2D texture resource.
    Texture1D,
    /// Logical 2D LUT.
    Texture2D,
}

/// Interpolation policy OCIO expects for a GPU LUT resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuTextureInterpolation {
    /// Unknown interpolation mode.
    Unknown,
    /// Nearest-neighbor sampling.
    Nearest,
    /// Linear sampling.
    Linear,
    /// Tetrahedral sampling.
    Tetrahedral,
    /// Cubic sampling.
    Cubic,
    /// OCIO default interpolation policy.
    Default,
    /// OCIO best-quality interpolation policy.
    Best,
}

/// OCIO 1D/2D LUT resource binding metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct OcioGpuTexture2DBinding {
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
    /// Flattened texel payload copied from OCIO as-is.
    pub values: Vec<f32>,
}

/// OCIO 3D LUT resource binding metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct OcioGpuTexture3DBinding {
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
    /// Flattened texel payload copied from OCIO as-is.
    pub values: Vec<f32>,
}

impl OcioGpuShaderBundle {
    fn for_color_space(
        src: ColorSpace,
        dst: ColorSpace,
        language: GpuLanguage,
        shader_text: String,
        desc: &GpuShaderDesc,
        cache_id: Option<String>,
    ) -> Self {
        Self {
            src_color_space: ocio_color_space_name(src).to_string(),
            dst_color_space: ocio_color_space_name(dst).to_string(),
            language,
            shader_text,
            descriptor_set_index: desc.descriptor_set_index(),
            texture_binding_start: desc.texture_binding_start(),
            uniform_buffer_binding: 0,
            uniform_buffer_size: desc.uniform_buffer_size(),
            texture_2d_count: desc.num_textures(),
            texture_3d_count: desc.num_3d_textures(),
            uniform_count: desc.num_uniforms(),
            textures_2d: ocio_texture_2d_bindings(desc),
            textures_3d: ocio_texture_3d_bindings(desc),
            cache_id,
        }
    }

    fn for_display(
        src: ColorSpace,
        display: &str,
        view: &str,
        language: GpuLanguage,
        shader_text: String,
        desc: &GpuShaderDesc,
        cache_id: Option<String>,
    ) -> Self {
        Self {
            src_color_space: ocio_color_space_name(src).to_string(),
            dst_color_space: format!("{display}/{view}"),
            language,
            shader_text,
            descriptor_set_index: desc.descriptor_set_index(),
            texture_binding_start: desc.texture_binding_start(),
            uniform_buffer_binding: 0,
            uniform_buffer_size: desc.uniform_buffer_size(),
            texture_2d_count: desc.num_textures(),
            texture_3d_count: desc.num_3d_textures(),
            uniform_count: desc.num_uniforms(),
            textures_2d: ocio_texture_2d_bindings(desc),
            textures_3d: ocio_texture_3d_bindings(desc),
            cache_id,
        }
    }
}

fn ocio_texture_2d_bindings(desc: &GpuShaderDesc) -> Vec<OcioGpuTexture2DBinding> {
    desc.textures_2d()
        .into_iter()
        .enumerate()
        .map(|(index, texture)| OcioGpuTexture2DBinding {
            index: index as u32,
            texture_name: texture.texture_name,
            sampler_name: texture.sampler_name,
            binding_index: texture.binding_index,
            channel: ocio_texture_channel(texture.channel),
            dimensions: ocio_texture_dimensions(texture.dimensions),
            interpolation: ocio_texture_interpolation(texture.interpolation),
            width: texture.width,
            height: texture.height,
            value_count: texture.values.len(),
            values: texture.values,
        })
        .collect()
}

fn ocio_texture_3d_bindings(desc: &GpuShaderDesc) -> Vec<OcioGpuTexture3DBinding> {
    desc.textures_3d()
        .into_iter()
        .enumerate()
        .map(|(index, texture)| OcioGpuTexture3DBinding {
            index: index as u32,
            texture_name: texture.texture_name,
            sampler_name: texture.sampler_name,
            binding_index: texture.binding_index,
            interpolation: ocio_texture_interpolation(texture.interpolation),
            edge_len: texture.edge_len,
            value_count: texture.values.len(),
            values: texture.values,
        })
        .collect()
}

fn ocio_texture_channel(channel: OcioRsGpuTextureChannel) -> OcioGpuTextureChannel {
    match channel {
        OcioRsGpuTextureChannel::Red => OcioGpuTextureChannel::Red,
        OcioRsGpuTextureChannel::Rgb => OcioGpuTextureChannel::Rgb,
    }
}

fn ocio_texture_dimensions(dimensions: OcioRsGpuTextureDimensions) -> OcioGpuTextureDimensions {
    match dimensions {
        OcioRsGpuTextureDimensions::Texture1D => OcioGpuTextureDimensions::Texture1D,
        OcioRsGpuTextureDimensions::Texture2D => OcioGpuTextureDimensions::Texture2D,
    }
}

fn ocio_texture_interpolation(interpolation: OcioRsInterpolation) -> OcioGpuTextureInterpolation {
    match interpolation {
        OcioRsInterpolation::Unknown => OcioGpuTextureInterpolation::Unknown,
        OcioRsInterpolation::Nearest => OcioGpuTextureInterpolation::Nearest,
        OcioRsInterpolation::Linear => OcioGpuTextureInterpolation::Linear,
        OcioRsInterpolation::Tetrahedral => OcioGpuTextureInterpolation::Tetrahedral,
        OcioRsInterpolation::Cubic => OcioGpuTextureInterpolation::Cubic,
        OcioRsInterpolation::Default => OcioGpuTextureInterpolation::Default,
        OcioRsInterpolation::Best => OcioGpuTextureInterpolation::Best,
    }
}

// ── CPU transform helpers ──────────────────────────────────────────────────────

/// Obtain a CPU processor for `src → dst` using the current global config.
fn ocio_cpu_processor(src: ColorSpace, dst: ColorSpace) -> Result<CPUProcessor, String> {
    let config = ocio_rs::current_config()
        .ok_or_else(|| "no OCIO config loaded (call ensure_ocio_loaded first)".to_string())?;

    let src_name = ocio_color_space_name(src);
    let dst_name = ocio_color_space_name(dst);

    let processor = config
        .processor(src_name, dst_name)
        .map_err(|e| format!("OCIO processor '{src_name}' → '{dst_name}': {e}"))?;

    processor
        .default_cpu_processor()
        .map_err(|e| format!("OCIO CPU processor '{src_name}' → '{dst_name}': {e}"))
}

fn ocio_processor(src: ColorSpace, dst: ColorSpace) -> Result<ocio_rs::Processor, String> {
    let config = ocio_rs::current_config()
        .ok_or_else(|| "no OCIO config loaded (call ensure_ocio_loaded first)".to_string())?;

    let src_name = ocio_color_space_name(src);
    let dst_name = ocio_color_space_name(dst);

    config
        .processor(src_name, dst_name)
        .map_err(|e| format!("OCIO processor '{src_name}' -> '{dst_name}': {e}"))
}

fn ocio_display_processor(
    src: ColorSpace,
    display: &str,
    view: &str,
) -> Result<ocio_rs::Processor, String> {
    let config = ocio_rs::current_config()
        .ok_or_else(|| "no OCIO config loaded (call ensure_ocio_loaded first)".to_string())?;

    let src_name = ocio_color_space_name(src);

    config
        .processor_display(
            src_name,
            display,
            view,
            ocio_rs::TransformDirection::Forward,
        )
        .map_err(|e| format!("OCIO display processor '{src_name}' -> {display}/{view}: {e}"))
}

/// Obtain a CPU processor for a display transform using the current global config.
fn ocio_display_cpu_processor(
    src: ColorSpace,
    display: &str,
    view: &str,
) -> Result<CPUProcessor, String> {
    let config = ocio_rs::current_config()
        .ok_or_else(|| "no OCIO config loaded (call ensure_ocio_loaded first)".to_string())?;

    let src_name = ocio_color_space_name(src);

    let processor = config
        .processor_display(
            src_name,
            display,
            view,
            ocio_rs::TransformDirection::Forward,
        )
        .map_err(|e| format!("OCIO display processor '{src_name}' → {display}/{view}: {e}"))?;

    processor
        .default_cpu_processor()
        .map_err(|e| format!("OCIO CPU display processor '{src_name}' → {display}/{view}: {e}"))
}

// ── Public entry points ────────────────────────────────────────────────────────

/// Apply an OCIO color-space conversion to an `&mut [u8]` RGBA buffer.
///
/// The buffer is treated as `num_pixels × 4` channels in the **source**
/// encoding.  OCIO decodes, converts primaries, and re-encodes into the
/// destination encoding.  Alpha is passed through unchanged.
pub fn apply_ocio_rgba8(data: &mut [u8], src: ColorSpace, dst: ColorSpace) -> Result<(), String> {
    if data.is_empty() || src == dst {
        return Ok(());
    }

    let cpu = ocio_cpu_processor(src, dst)?;
    apply_cpu_processor_rgba8(&cpu, data);
    Ok(())
}

/// Apply an OCIO source -> working -> output conversion to an RGBA8 buffer.
pub fn apply_ocio_pipeline_rgba8(
    data: &mut [u8],
    src: ColorSpace,
    working: ColorSpace,
    dst: ColorSpace,
) -> Result<(), String> {
    if data.is_empty() || (src == working && working == dst) {
        return Ok(());
    }

    if src != working {
        apply_ocio_rgba8(data, src, working)?;
    }
    if working != dst {
        apply_ocio_rgba8(data, working, dst)?;
    }
    Ok(())
}

/// Apply an OCIO display transform (scene-referred → display-referred) to an
/// `&mut [u8]` RGBA buffer.
///
/// `display` and `view` identify the display/view pair in the OCIO config
/// (e.g. `"sRGB"` / `"ACES 1.0 - SDR Video"`).
pub fn apply_ocio_display_rgba8(
    data: &mut [u8],
    src: ColorSpace,
    display: &str,
    view: &str,
) -> Result<(), String> {
    if data.is_empty() {
        return Ok(());
    }

    let cpu = ocio_display_cpu_processor(src, display, view)?;
    apply_cpu_processor_rgba8(&cpu, data);
    Ok(())
}

/// Extract a GPU shader bundle for `src -> dst` from the current OCIO config.
///
/// The returned bundle is renderer-facing metadata. It deliberately does not
/// allocate wgpu resources; callers should cache compiled shaders and uploaded
/// texture/uniform resources by `cache_id` plus their render-target contract.
pub fn extract_ocio_gpu_shader_bundle(
    src: ColorSpace,
    dst: ColorSpace,
    language: GpuLanguage,
) -> Result<OcioGpuShaderBundle, String> {
    let processor = ocio_processor(src, dst)?;
    let cache_id = processor.cache_id();
    let gpu = processor.default_gpu_processor().map_err(|e| {
        format!(
            "OCIO GPU processor '{}' -> '{}': {e}",
            ocio_color_space_name(src),
            ocio_color_space_name(dst)
        )
    })?;
    let mut desc = configured_gpu_shader_desc(language)?;
    gpu.extract_shader_info(&mut desc);
    let shader_text = extracted_shader_text(&desc)?;

    Ok(OcioGpuShaderBundle::for_color_space(
        src,
        dst,
        language,
        shader_text,
        &desc,
        cache_id,
    ))
}

/// Extract a GPU shader bundle for an OCIO display/view transform.
pub fn extract_ocio_display_gpu_shader_bundle(
    src: ColorSpace,
    display: &str,
    view: &str,
    language: GpuLanguage,
) -> Result<OcioGpuShaderBundle, String> {
    let processor = ocio_display_processor(src, display, view)?;
    let cache_id = processor.cache_id();
    let gpu = processor.default_gpu_processor().map_err(|e| {
        format!(
            "OCIO GPU display processor '{}' -> {display}/{view}: {e}",
            ocio_color_space_name(src),
        )
    })?;
    let mut desc = configured_gpu_shader_desc(language)?;
    gpu.extract_shader_info(&mut desc);
    let shader_text = extracted_shader_text(&desc)?;

    Ok(OcioGpuShaderBundle::for_display(
        src,
        display,
        view,
        language,
        shader_text,
        &desc,
        cache_id,
    ))
}

fn configured_gpu_shader_desc(language: GpuLanguage) -> Result<GpuShaderDesc, String> {
    let desc = GpuShaderDesc::create().map_err(|e| format!("OCIO GPU shader desc: {e}"))?;
    desc.set_language(language);
    desc.set_function_name("mondrian_ocio_main")
        .map_err(|e| format!("OCIO GPU shader function name: {e}"))?;
    desc.set_pixel_name("mondrian_ocio_pixel")
        .map_err(|e| format!("OCIO GPU shader pixel name: {e}"))?;
    desc.set_resource_prefix("mondrian_ocio_")
        .map_err(|e| format!("OCIO GPU shader resource prefix: {e}"))?;
    desc.set_descriptor_set_index(0, 1);
    Ok(desc)
}

fn extracted_shader_text(desc: &GpuShaderDesc) -> Result<String, String> {
    let shader_text = desc
        .shader_text()
        .ok_or_else(|| "OCIO GPU shader extraction returned empty shader text".to_string())?;
    if shader_text.trim().is_empty() {
        return Err("OCIO GPU shader extraction returned blank shader text".to_string());
    }
    Ok(shader_text)
}

/// Low-level: run an already-obtained [`CPUProcessor`] over an RGBA8 buffer.
fn apply_cpu_processor_rgba8(cpu: &CPUProcessor, data: &mut [u8]) {
    let num_pixels = (data.len() / 4) as i64;
    if num_pixels == 0 {
        return;
    }

    // Convert u8 → f32 (OCIO operates in f32 linear internally).
    let mut f32_buf: Vec<f32> = Vec::with_capacity(data.len());
    for px in data.chunks_exact(4) {
        f32_buf.push(px[0] as f32 / 255.0);
        f32_buf.push(px[1] as f32 / 255.0);
        f32_buf.push(px[2] as f32 / 255.0);
        f32_buf.push(px[3] as f32 / 255.0);
    }

    cpu.apply_rgba_pixels(&mut f32_buf, num_pixels, 4);

    // Convert f32 → u8.
    for (i, px) in data.chunks_exact_mut(4).enumerate() {
        let base = i * 4;
        px[0] = (f32_buf[base].clamp(0.0, 1.0) * 255.0).round() as u8;
        px[1] = (f32_buf[base + 1].clamp(0.0, 1.0) * 255.0).round() as u8;
        px[2] = (f32_buf[base + 2].clamp(0.0, 1.0) * 255.0).round() as u8;
        px[3] = (f32_buf[base + 3].clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

// ── Utility: list available displays / views ───────────────────────────────────

/// Return the list of display names from the current OCIO config.
pub fn ocio_display_names() -> Vec<String> {
    let Some(config) = ocio_rs::current_config() else {
        return Vec::new();
    };
    let n = config.num_displays();
    (0..n).filter_map(|i| config.display(i)).collect()
}

/// Return the list of view names for a given display.
pub fn ocio_view_names(display: &str) -> Vec<String> {
    let Some(config) = ocio_rs::current_config() else {
        return Vec::new();
    };
    let n = config.num_views(display);
    (0..n).filter_map(|i| config.view(display, i)).collect()
}

/// Return the default display / view pair from the current OCIO config.
pub fn ocio_default_display_view() -> Option<(String, String)> {
    let config = ocio_rs::current_config()?;
    let display = config.default_display()?;
    let view = config.default_view(&display)?;
    Some((display, view))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_COLOR_SPACES: [ColorSpace; 9] = [
        ColorSpace::Rec709,
        ColorSpace::Rec2100Hlg,
        ColorSpace::Rec2100Pq,
        ColorSpace::Srgb,
        ColorSpace::Rec2020,
        ColorSpace::DciP3,
        ColorSpace::AppleLog,
        ColorSpace::SLog3,
        ColorSpace::ArriLogC4,
    ];

    #[test]
    fn mondrian_default_config_asset_parses_and_is_named() {
        let config = Config::from_stream(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO config should parse");

        assert_eq!(
            config.name().as_deref(),
            Some(MONDRIAN_DEFAULT_OCIO_CONFIG_NAME)
        );
        assert!(config.num_color_spaces() > ALL_COLOR_SPACES.len() as i32);
        assert_eq!(config.default_display().as_deref(), Some("sRGB - Display"));
        assert_eq!(
            config.default_view("sRGB - Display").as_deref(),
            Some("ACES 2.0 - SDR 100 nits (Rec.709)")
        );
    }

    #[test]
    fn mondrian_default_config_covers_color_space_contract() {
        let config = Config::from_stream(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO config should parse");

        for color_space in ALL_COLOR_SPACES {
            let ocio_name = ocio_color_space_name(color_space);
            assert!(
                config.canonical_name(ocio_name).is_some(),
                "{color_space:?} mapped to missing OCIO color space '{ocio_name}'"
            );
        }
    }

    #[test]
    fn mondrian_default_processors_cover_delivery_hdr_and_log_inputs() {
        let config = Config::from_stream(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO config should parse");

        let processor_pairs = [
            (ColorSpace::Rec709, ColorSpace::Srgb),
            (ColorSpace::Srgb, ColorSpace::Rec709),
            (ColorSpace::Rec2020, ColorSpace::Rec709),
            (ColorSpace::Rec2100Pq, ColorSpace::Rec709),
            (ColorSpace::Rec2100Hlg, ColorSpace::Rec709),
            (ColorSpace::DciP3, ColorSpace::Rec709),
            (ColorSpace::AppleLog, ColorSpace::Rec709),
            (ColorSpace::SLog3, ColorSpace::Rec709),
            (ColorSpace::ArriLogC4, ColorSpace::Rec709),
        ];

        for (src, dst) in processor_pairs {
            let src_name = ocio_color_space_name(src);
            let dst_name = ocio_color_space_name(dst);
            let processor = config
                .processor(src_name, dst_name)
                .unwrap_or_else(|err| panic!("{src:?}->{dst:?} processor missing: {err}"));
            processor
                .default_cpu_processor()
                .unwrap_or_else(|err| panic!("{src:?}->{dst:?} CPU processor missing: {err}"));
        }
    }

    #[test]
    fn standard_mode_loads_embedded_default_config() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        assert!(mondrian_default_ocio_available());
        assert_eq!(
            ocio_config_path().as_deref(),
            Some(Path::new(MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH))
        );
        assert_eq!(
            ocio_default_display_view()
                .as_ref()
                .map(|(display, view)| { (display.as_str(), view.as_str()) }),
            Some(("sRGB - Display", "ACES 2.0 - SDR 100 nits (Rec.709)"))
        );
    }

    #[test]
    fn standard_mode_extracts_gpu_shader_bundle() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        let bundle = extract_ocio_gpu_shader_bundle(
            ColorSpace::SLog3,
            ColorSpace::Rec709,
            GpuLanguage::Glsl4_0,
        )
        .expect("default config should produce a GPU shader bundle");

        assert_eq!(bundle.language, GpuLanguage::Glsl4_0);
        assert_eq!(bundle.src_color_space, "S-Log3 S-Gamut3.Cine");
        assert_eq!(bundle.dst_color_space, "Camera Rec.709");
        assert!(bundle.shader_text.contains("mondrian_ocio_main"));
        assert!(bundle.cache_id.as_deref().is_some_and(|id| !id.trim().is_empty()));
        assert_eq!(bundle.descriptor_set_index, 0);
        assert_eq!(bundle.texture_binding_start, 1);
        assert_eq!(bundle.uniform_buffer_binding, 0);
        assert_eq!(bundle.textures_2d.len() as u32, bundle.texture_2d_count);
        assert_eq!(bundle.textures_3d.len() as u32, bundle.texture_3d_count);
        for texture in &bundle.textures_2d {
            assert!(!texture.texture_name.trim().is_empty());
            assert!(!texture.sampler_name.trim().is_empty());
            assert_eq!(texture.value_count, texture.values.len());
            assert!(texture.width > 0);
            assert!(texture.height > 0);
        }
        for texture in &bundle.textures_3d {
            assert!(!texture.texture_name.trim().is_empty());
            assert!(!texture.sampler_name.trim().is_empty());
            assert_eq!(texture.value_count, texture.values.len());
            assert!(texture.edge_len > 0);
        }
    }

    #[test]
    fn standard_mode_extracts_display_gpu_shader_bundle() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");
        let (display, view) = ocio_default_display_view().expect("default display/view");

        let bundle = extract_ocio_display_gpu_shader_bundle(
            ColorSpace::Rec709,
            &display,
            &view,
            GpuLanguage::Glsl4_0,
        )
        .expect("default config should produce a display GPU shader bundle");

        assert_eq!(bundle.language, GpuLanguage::Glsl4_0);
        assert_eq!(bundle.src_color_space, "Camera Rec.709");
        assert_eq!(
            bundle.dst_color_space,
            "sRGB - Display/ACES 2.0 - SDR 100 nits (Rec.709)"
        );
        assert!(bundle.shader_text.contains("mondrian_ocio_main"));
        assert!(bundle.cache_id.as_deref().is_some_and(|id| !id.trim().is_empty()));
    }
}
