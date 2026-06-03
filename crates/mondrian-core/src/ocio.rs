//! OCIO (OpenColorIO) integration for color management.
//!
//! When [`crate::ColorEngine::Ocio`] is selected, color transforms are delegated to
//! an OCIO v2.5.1 config instead of the built-in MondrianSmart math.
//! The config source is determined by [`OcioConfigSource`]:
//!
//! 1. **Builtin** — named built-in config (e.g. `"aces_1.2"`)
//! 2. **Path**      — explicit `config.ocio` file path
//! 3. **Environment** — `$OCIO` env var → standard system paths

use crate::types::{ColorSpace, OcioConfigSource};
use ocio_rs::{BuiltinConfigRegistry, CPUProcessor, Config};
use std::path::{Path, PathBuf};

// ── Global OCIO state ──────────────────────────────────────────────────────────

static OCIO_CONFIG_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

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
/// Returns an empty vec in stub mode or when no built-in configs are compiled in.
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

/// Map a Mondrian [`ColorSpace`] to its conventional OCIO color-space name.
///
/// These names match the built-in ACES and CG configs shipped with OCIO.
/// If a config uses different names the user must align them in their
/// `.ocio` file or contribute additional mappings here.
pub fn ocio_color_space_name(cs: ColorSpace) -> &'static str {
    match cs {
        ColorSpace::Srgb => "sRGB",
        ColorSpace::Rec709 => "Rec.709",
        ColorSpace::Rec2020 => "Rec.2020",
        ColorSpace::Rec2100Pq => "Rec.2100-PQ",
        ColorSpace::Rec2100Hlg => "Rec.2100-HLG",
        ColorSpace::DciP3 => "P3-D65",
        ColorSpace::AppleLog => "Apple Log",
        ColorSpace::SLog3 => "S-Log3",
        ColorSpace::ArriLogC4 => "ARRI LogC4",
    }
}

// ── CPU transform helpers ──────────────────────────────────────────────────────

/// Obtain a CPU processor for `src → dst` using the current global config.
fn ocio_cpu_processor(src: ColorSpace, dst: ColorSpace) -> Result<CPUProcessor, String> {
    let config = ocio_rs::get_current_config()
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

/// Obtain a CPU processor for a display transform using the current global config.
fn ocio_display_cpu_processor(
    src: ColorSpace,
    display: &str,
    view: &str,
) -> Result<CPUProcessor, String> {
    let config = ocio_rs::get_current_config()
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
    let Some(config) = ocio_rs::get_current_config() else {
        return Vec::new();
    };
    let n = config.num_displays();
    (0..n).filter_map(|i| config.display(i)).collect()
}

/// Return the list of view names for a given display.
pub fn ocio_view_names(display: &str) -> Vec<String> {
    let Some(config) = ocio_rs::get_current_config() else {
        return Vec::new();
    };
    let n = config.num_views(display);
    (0..n).filter_map(|i| config.view(display, i)).collect()
}

/// Return the default display / view pair from the current OCIO config.
pub fn ocio_default_display_view() -> Option<(String, String)> {
    let config = ocio_rs::get_current_config()?;
    let display = config.default_display()?;
    let view = config.default_view(&display)?;
    Some((display, view))
}
