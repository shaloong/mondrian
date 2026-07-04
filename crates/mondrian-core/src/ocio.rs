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
//! 4. **Environment** — explicit `$OCIO` env var

use crate::types::{ColorSpace, OcioConfigSource};
pub use ocio_rs::GpuLanguage;
use ocio_rs::{
    BuiltinConfigRegistry, CPUProcessor, Config, GpuShaderDesc,
    GpuTextureChannel as OcioRsGpuTextureChannel,
    GpuTextureDimensions as OcioRsGpuTextureDimensions, GpuUniformType as OcioRsGpuUniformType,
    GpuUniformValue as OcioRsGpuUniformValue, Interpolation as OcioRsInterpolation,
};
use std::path::{Path, PathBuf};

// ── Global OCIO state ──────────────────────────────────────────────────────────

/// Centralized OCIO global state with proper locking, source identity, and
/// generation counting for cache invalidation.
///
/// All OCIO config mutations must go through this module. The mutex protects
/// both the Rust-side metadata and the C++ global config atomically.
///
/// # Concurrency Constraint
///
/// OCIO's C++ backend uses a process-global current config (`set_current_config`).
/// This means **only one OCIO config can be active at a time**. Concurrent
/// rendering with different OCIO configs (e.g., multi-project or multi-sequence
/// with different color science) is NOT supported and will produce incorrect
/// results.
///
/// The mutex in [`OCIO_STATE`] serializes config mutations, but does NOT prevent
/// concurrent `current_config()` calls from seeing a config that was loaded for
/// a different project/sequence. Callers must ensure that:
///
/// 1. All rendering within a process uses the same OCIO config, OR
/// 2. Config switches are serialized with rendering (no concurrent access during
///    config transition), OR
/// 3. The application architecture prevents multi-config scenarios (current
///    Mondrian design: one project = one config).
///
/// Full config isolation (per-config processor caches, no process-global state)
/// requires upstream OCIO changes or a wrapper layer. This is deferred to a
/// future phase.
struct OcioGlobalState {
    /// Path or virtual path of the currently loaded config.
    path: Option<PathBuf>,
    /// Source identity that loaded the current config.
    source: Option<OcioConfigSource>,
    /// Monotonically increasing generation counter. Incremented on every
    /// config load. Callers can use this to detect config changes for cache
    /// invalidation without holding the lock.
    generation: u64,
}

static OCIO_STATE: std::sync::Mutex<OcioGlobalState> =
    std::sync::Mutex::new(OcioGlobalState { path: None, source: None, generation: 0 });

impl OcioGlobalState {
    /// Set the current config atomically: update path, source, increment
    /// generation, and call `ocio_rs::set_current_config`.
    ///
    /// The mutex is held for the entire operation so concurrent
    /// `current_config()` callers cannot see a half-updated state.
    fn set_config(&mut self, path: PathBuf, source: OcioConfigSource, config: &Config) {
        ocio_rs::set_current_config(config);
        self.path = Some(path);
        self.source = Some(source);
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Return the current config generation. This is a monotonic counter that
/// increments on every config load. Use it for cache invalidation.
pub fn ocio_config_generation() -> u64 {
    OCIO_STATE.lock().map(|g| g.generation).unwrap_or(0)
}

/// Check whether the config has changed since the given generation.
///
/// Callers should capture the generation at the start of a rendering operation
/// and check it again before submitting GPU work. If the config changed, the
/// cached shader plans and backend objects may be stale and must be invalidated.
///
/// Returns `true` if the config has changed (generation increased), `false` if
/// it's still the same config.
pub fn ocio_config_changed_since(since_generation: u64) -> bool {
    ocio_config_generation() != since_generation
}

/// Return the current config source identity, if any.
pub fn ocio_config_source() -> Option<OcioConfigSource> {
    OCIO_STATE.lock().ok().and_then(|g| g.source.clone())
}

/// Intended name for Mondrian's bundled default OCIO config.
pub const MONDRIAN_DEFAULT_OCIO_CONFIG_NAME: &str = "mondrian_default_ocio_v1";

const MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH: &str = "embedded:mondrian_default_ocio_v1";
const MONDRIAN_DEFAULT_OCIO_CONFIG: &str =
    include_str!("../assets/ocio/mondrian_default_ocio_v1.ocio");

/// Product-level contract for Mondrian's embedded default OCIO config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MondrianDefaultOcioContract {
    /// Pinned OCIO config name.
    pub config_name: &'static str,
    /// Virtual path used when the embedded config is loaded into process state.
    pub virtual_path: &'static str,
    /// Default display selected by Standard mode.
    pub default_display: &'static str,
    /// Default view selected for [`Self::default_display`].
    pub default_view: &'static str,
    /// Scene-linear working role expected by Mondrian's internal compositor.
    pub scene_linear_role: &'static str,
    /// Mondrian color spaces that must be present in the embedded config.
    pub color_spaces: &'static [MondrianDefaultOcioColorSpace],
    /// Product-supported display/view pairs that must be present in the embedded config.
    pub display_views: &'static [MondrianDefaultOcioDisplayView],
}

/// One Mondrian [`ColorSpace`] mapping guaranteed by the embedded OCIO config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MondrianDefaultOcioColorSpace {
    /// Mondrian domain color-space enum value.
    pub color_space: ColorSpace,
    /// Pinned OCIO color-space name or alias.
    pub ocio_name: &'static str,
}

/// One product-supported display/view pair in the embedded OCIO config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MondrianDefaultOcioDisplayView {
    /// OCIO display name.
    pub display: &'static str,
    /// OCIO view name.
    pub view: &'static str,
}

/// Structured validation summary for Mondrian's embedded OCIO config asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MondrianDefaultOcioValidationReport {
    /// Parsed config name.
    pub config_name: String,
    /// Number of OCIO color spaces in the embedded config.
    pub color_space_count: i32,
    /// Number of Mondrian color-space mappings validated against OCIO canonical names.
    pub color_space_mappings_checked: usize,
    /// Number of product display/view pairs validated against the config.
    pub display_views_checked: usize,
    /// Number of scene/display roles validated.
    pub roles_checked: usize,
    /// Number of color-space CPU processors built from the contract matrix.
    pub color_space_cpu_processors_checked: usize,
    /// Number of color-space GPU shader processors built from the non-identity contract matrix.
    pub color_space_gpu_processors_checked: usize,
    /// Number of display/view CPU processors built from contract inputs.
    pub display_cpu_processors_checked: usize,
    /// Number of display/view GPU shader processors built from contract inputs.
    pub display_gpu_processors_checked: usize,
}

/// Validation failure for Mondrian's embedded OCIO config asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MondrianDefaultOcioValidationError {
    /// Individual validation issues found while checking the embedded config.
    pub issues: Vec<String>,
}

impl MondrianDefaultOcioValidationError {
    fn new(issues: Vec<String>) -> Self {
        Self { issues }
    }
}

impl std::fmt::Display for MondrianDefaultOcioValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "embedded Mondrian OCIO config failed {} validation check(s)",
            self.issues.len()
        )
    }
}

impl std::error::Error for MondrianDefaultOcioValidationError {}

const MONDRIAN_DEFAULT_OCIO_COLOR_SPACES: [MondrianDefaultOcioColorSpace; 9] = [
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Rec709,
        ocio_name: "Camera Rec.709",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Rec2100Hlg,
        ocio_name: "Rec.2100-HLG - Display",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Rec2100Pq,
        ocio_name: "Rec.2100-PQ - Display",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Srgb,
        ocio_name: "sRGB Encoded Rec.709 (sRGB)",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Rec2020,
        ocio_name: "Linear Rec.2020",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::DciP3,
        ocio_name: "sRGB Encoded P3-D65",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::AppleLog,
        ocio_name: "Apple Log",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::SLog3,
        ocio_name: "S-Log3 S-Gamut3.Cine",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::ArriLogC4,
        ocio_name: "ARRI LogC4",
    },
];

const MONDRIAN_DEFAULT_OCIO_DISPLAY_VIEWS: [MondrianDefaultOcioDisplayView; 7] = [
    MondrianDefaultOcioDisplayView {
        display: "sRGB - Display",
        view: "ACES 2.0 - SDR 100 nits (Rec.709)",
    },
    MondrianDefaultOcioDisplayView {
        display: "sRGB - Display",
        view: "Video (colorimetric)",
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.1886 Rec.709 - Display",
        view: "ACES 2.0 - SDR 100 nits (Rec.709)",
    },
    MondrianDefaultOcioDisplayView {
        display: "Display P3 - Display",
        view: "ACES 2.0 - SDR 100 nits (P3 D65)",
    },
    MondrianDefaultOcioDisplayView {
        display: "Display P3 HDR - Display",
        view: "ACES 2.0 - HDR 1000 nits (P3 D65)",
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.2100-HLG - Display",
        view: "ACES 2.0 - HDR 1000 nits (P3 D65)",
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.2100-PQ - Display",
        view: "ACES 2.0 - HDR 1000 nits (Rec.2020)",
    },
];

/// OCIO GPU function name generated for Mondrian wrapper shaders.
pub const MONDRIAN_OCIO_GPU_FUNCTION_NAME: &str = "mondrian_ocio_main";
/// OCIO GPU pixel variable name generated for Mondrian wrapper shaders.
pub const MONDRIAN_OCIO_GPU_PIXEL_NAME: &str = "mondrian_ocio_pixel";
/// OCIO GPU resource symbol prefix generated for Mondrian wrapper shaders.
pub const MONDRIAN_OCIO_GPU_RESOURCE_PREFIX: &str = "mondrian_ocio_";
/// OCIO GPU descriptor set used by Mondrian.
pub const MONDRIAN_OCIO_GPU_DESCRIPTOR_SET_INDEX: u32 = 0;
/// First OCIO GPU texture binding slot. Binding 0 is reserved for uniforms.
pub const MONDRIAN_OCIO_GPU_TEXTURE_BINDING_START: u32 = 1;

/// Return the pinned OCIO config text used by Mondrian Standard mode.
pub fn mondrian_default_ocio_config_text() -> &'static str {
    MONDRIAN_DEFAULT_OCIO_CONFIG
}

/// Return the product contract for Mondrian Standard mode's embedded OCIO config.
pub fn mondrian_default_ocio_contract() -> MondrianDefaultOcioContract {
    MondrianDefaultOcioContract {
        config_name: MONDRIAN_DEFAULT_OCIO_CONFIG_NAME,
        virtual_path: MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH,
        default_display: "sRGB - Display",
        default_view: "ACES 2.0 - SDR 100 nits (Rec.709)",
        scene_linear_role: "ACEScg",
        color_spaces: &MONDRIAN_DEFAULT_OCIO_COLOR_SPACES,
        display_views: &MONDRIAN_DEFAULT_OCIO_DISPLAY_VIEWS,
    }
}

/// Validate the embedded Mondrian default OCIO config against its product contract.
///
/// This is the production gate for `mondrian_default_ocio_v1`: it parses the
/// embedded asset, verifies pinned names/roles/display views, and proves that
/// every contract color-space pair can build a CPU processor while every
/// non-identity pair and display/view transform can extract a GPU shader.
pub fn validate_mondrian_default_ocio_contract(
) -> Result<MondrianDefaultOcioValidationReport, MondrianDefaultOcioValidationError> {
    let contract = mondrian_default_ocio_contract();
    let config = match Config::from_stream(MONDRIAN_DEFAULT_OCIO_CONFIG) {
        Ok(config) => config,
        Err(err) => {
            return Err(MondrianDefaultOcioValidationError::new(vec![format!(
                "embedded Mondrian OCIO config '{}' failed to parse: {err}",
                contract.config_name
            )]));
        }
    };

    let mut errors = Vec::new();
    let mut report = MondrianDefaultOcioValidationReport {
        config_name: config.name().unwrap_or_default(),
        color_space_count: config.num_color_spaces(),
        color_space_mappings_checked: 0,
        display_views_checked: 0,
        roles_checked: 0,
        color_space_cpu_processors_checked: 0,
        color_space_gpu_processors_checked: 0,
        display_cpu_processors_checked: 0,
        display_gpu_processors_checked: 0,
    };

    validate_mondrian_default_config_identity(&config, contract, &mut report, &mut errors);
    validate_mondrian_default_color_spaces(&config, contract, &mut report, &mut errors);
    validate_mondrian_default_display_views(&config, contract, &mut report, &mut errors);
    validate_mondrian_default_color_space_processors(&config, contract, &mut report, &mut errors);
    validate_mondrian_default_display_processors(&config, contract, &mut report, &mut errors);

    if errors.is_empty() {
        Ok(report)
    } else {
        Err(MondrianDefaultOcioValidationError::new(errors))
    }
}

fn validate_mondrian_default_config_identity(
    config: &Config,
    contract: MondrianDefaultOcioContract,
    report: &mut MondrianDefaultOcioValidationReport,
    errors: &mut Vec<String>,
) {
    if report.config_name != contract.config_name {
        errors.push(format!(
            "embedded Mondrian OCIO config name mismatch: expected '{}', got '{}'",
            contract.config_name, report.config_name
        ));
    }
    if report.color_space_count < contract.color_spaces.len() as i32 {
        errors.push(format!(
            "embedded Mondrian OCIO config has only {} color spaces for {} contract mappings",
            report.color_space_count,
            contract.color_spaces.len()
        ));
    }

    match config.default_display() {
        Some(display) if display == contract.default_display => report.roles_checked += 1,
        Some(display) => errors.push(format!(
            "embedded Mondrian OCIO default display mismatch: expected '{}', got '{display}'",
            contract.default_display
        )),
        None => errors.push("embedded Mondrian OCIO config has no default display".to_string()),
    }
    match config.default_view(contract.default_display) {
        Some(view) if view == contract.default_view => report.roles_checked += 1,
        Some(view) => errors.push(format!(
            "embedded Mondrian OCIO default view mismatch for '{}': expected '{}', got '{view}'",
            contract.default_display, contract.default_view
        )),
        None => errors.push(format!(
            "embedded Mondrian OCIO config has no default view for '{}'",
            contract.default_display
        )),
    }
    match config.role_color_space("scene_linear") {
        Some(role) if role == contract.scene_linear_role => report.roles_checked += 1,
        Some(role) => errors.push(format!(
            "embedded Mondrian OCIO scene_linear role mismatch: expected '{}', got '{role}'",
            contract.scene_linear_role
        )),
        None => errors.push("embedded Mondrian OCIO config has no scene_linear role".to_string()),
    }
}

fn validate_mondrian_default_color_spaces(
    config: &Config,
    contract: MondrianDefaultOcioContract,
    report: &mut MondrianDefaultOcioValidationReport,
    errors: &mut Vec<String>,
) {
    for mapped in contract.color_spaces {
        if mapped.ocio_name != ocio_color_space_name(mapped.color_space) {
            errors.push(format!(
                "{:?} contract name mismatch: contract '{}', mapper '{}'",
                mapped.color_space,
                mapped.ocio_name,
                ocio_color_space_name(mapped.color_space)
            ));
            continue;
        }
        if config.canonical_name(mapped.ocio_name).is_some() {
            report.color_space_mappings_checked += 1;
        } else {
            errors.push(format!(
                "{:?} maps to missing OCIO color space '{}'",
                mapped.color_space, mapped.ocio_name
            ));
        }
    }
}

fn validate_mondrian_default_display_views(
    config: &Config,
    contract: MondrianDefaultOcioContract,
    report: &mut MondrianDefaultOcioValidationReport,
    errors: &mut Vec<String>,
) {
    for display_view in contract.display_views {
        if config_has_display_view(config, display_view.display, display_view.view) {
            report.display_views_checked += 1;
        } else {
            let views = config_view_names(config, display_view.display);
            errors.push(format!(
                "display '{}' is missing view '{}'; views: {views:?}",
                display_view.display, display_view.view
            ));
        }
    }
}

fn validate_mondrian_default_color_space_processors(
    config: &Config,
    contract: MondrianDefaultOcioContract,
    report: &mut MondrianDefaultOcioValidationReport,
    errors: &mut Vec<String>,
) {
    for src in contract.color_spaces {
        for dst in contract.color_spaces {
            let src_name = ocio_color_space_name(src.color_space);
            let dst_name = ocio_color_space_name(dst.color_space);
            let processor = match config.processor(src_name, dst_name) {
                Ok(processor) => processor,
                Err(err) => {
                    errors.push(format!(
                        "{:?}->{:?} OCIO processor missing: {err}",
                        src.color_space, dst.color_space
                    ));
                    continue;
                }
            };

            match processor.default_cpu_processor() {
                Ok(_) => report.color_space_cpu_processors_checked += 1,
                Err(err) => errors.push(format!(
                    "{:?}->{:?} OCIO CPU processor missing: {err}",
                    src.color_space, dst.color_space
                )),
            }

            if src.color_space != dst.color_space
                && validate_gpu_shader_processor(
                    &processor,
                    GpuLanguage::Glsl4_0,
                    &format!("{:?}->{:?}", src.color_space, dst.color_space),
                    errors,
                )
            {
                report.color_space_gpu_processors_checked += 1;
            }
        }
    }
}

fn validate_mondrian_default_display_processors(
    config: &Config,
    contract: MondrianDefaultOcioContract,
    report: &mut MondrianDefaultOcioValidationReport,
    errors: &mut Vec<String>,
) {
    for src in contract.color_spaces {
        let src_name = ocio_color_space_name(src.color_space);
        for display_view in contract.display_views {
            let processor = match config.processor_display(
                src_name,
                display_view.display,
                display_view.view,
                ocio_rs::TransformDirection::Forward,
            ) {
                Ok(processor) => processor,
                Err(err) => {
                    errors.push(format!(
                        "{:?}->{}/{} OCIO display processor missing: {err}",
                        src.color_space, display_view.display, display_view.view
                    ));
                    continue;
                }
            };

            match processor.default_cpu_processor() {
                Ok(_) => report.display_cpu_processors_checked += 1,
                Err(err) => errors.push(format!(
                    "{:?}->{}/{} OCIO display CPU processor missing: {err}",
                    src.color_space, display_view.display, display_view.view
                )),
            }

            if validate_gpu_shader_processor(
                &processor,
                GpuLanguage::Glsl4_0,
                &format!(
                    "{:?}->{}/{}",
                    src.color_space, display_view.display, display_view.view
                ),
                errors,
            ) {
                report.display_gpu_processors_checked += 1;
            }
        }
    }
}

fn config_view_names(config: &Config, display: &str) -> Vec<String> {
    (0..config.num_views(display))
        .filter_map(|index| config.view(display, index))
        .collect()
}

fn config_has_display_view(config: &Config, display: &str, view: &str) -> bool {
    config_view_names(config, display).iter().any(|candidate| candidate == view)
}

fn validate_gpu_shader_processor(
    processor: &ocio_rs::Processor,
    language: GpuLanguage,
    label: &str,
    errors: &mut Vec<String>,
) -> bool {
    let gpu = match processor.default_gpu_processor() {
        Ok(gpu) => gpu,
        Err(err) => {
            errors.push(format!("{label} OCIO GPU processor missing: {err}"));
            return false;
        }
    };
    let mut desc = match configured_gpu_shader_desc(language) {
        Ok(desc) => desc,
        Err(err) => {
            errors.push(format!("{label} OCIO GPU shader descriptor failed: {err}"));
            return false;
        }
    };
    gpu.extract_shader_info(&mut desc);
    match extracted_shader_text(&desc) {
        Ok(shader_text) if shader_text.contains(MONDRIAN_OCIO_GPU_FUNCTION_NAME) => true,
        Ok(_) => {
            errors.push(format!(
                "{label} OCIO GPU shader missing function '{}'",
                MONDRIAN_OCIO_GPU_FUNCTION_NAME
            ));
            false
        }
        Err(err) => {
            errors.push(format!("{label} OCIO GPU shader extraction failed: {err}"));
            false
        }
    }
}

/// Load an OCIO config from `path` and set it as the process-wide current config.
///
/// Safe to call again when the user switches configs.
pub fn init_ocio(path: &Path) -> Result<(), String> {
    let config = Config::from_file(path.to_string_lossy().as_ref())
        .map_err(|e| format!("failed to load OCIO config from {}: {e}", path.display()))?;

    if let Ok(mut guard) = OCIO_STATE.lock() {
        guard.set_config(
            path.to_path_buf(),
            OcioConfigSource::Path { path: path.to_path_buf() },
            &config,
        );
    }

    // The global OCIO context now holds a reference (ref-counted by the C++
    // library).  We deliberately forget the Rust wrapper so the ref-count
    // never reaches zero while the process is alive.
    std::mem::forget(config);

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

    let virtual_path = PathBuf::from(format!("builtin:{name}"));
    if let Ok(mut guard) = OCIO_STATE.lock() {
        guard.set_config(
            virtual_path,
            OcioConfigSource::Builtin { name: name.to_string() },
            &config,
        );
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

    if let Ok(mut guard) = OCIO_STATE.lock() {
        guard.set_config(
            PathBuf::from(MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH),
            OcioConfigSource::MondrianDefault,
            &config,
        );
    }
    std::mem::forget(config);

    tracing::info!(
        config = MONDRIAN_DEFAULT_OCIO_CONFIG_NAME,
        "Mondrian embedded OCIO config loaded"
    );
    Ok(())
}

/// Return the currently-loaded OCIO config path, if any.
pub fn ocio_config_path() -> Option<PathBuf> {
    OCIO_STATE.lock().ok().and_then(|g| g.path.clone())
}

/// Return `true` when an OCIO config has been loaded.
pub fn ocio_available() -> bool {
    OCIO_STATE.lock().map(|g| g.path.is_some()).unwrap_or(false)
}

// ── Resolver (explicit source only) ─────────────────────────────────────────────

/// Resolve an [`OcioConfigSource`] and load the corresponding config.
///
/// This is the single entry point that callers should use.  It is idempotent:
/// calling it again with the same effective source is a no-op.
///
/// When a different source is requested while another is loaded, the old config
/// is replaced. This is the only supported way to switch OCIO configs.
pub fn ensure_ocio_loaded(source: &OcioConfigSource) -> Result<(), String> {
    // Check if the requested source is already loaded.
    if let Ok(guard) = OCIO_STATE.lock() {
        if guard.source.as_ref() == Some(source) {
            return Ok(());
        }
    }
    // Different source requested — load it.
    match source {
        OcioConfigSource::MondrianDefault => init_mondrian_default_ocio(),
        OcioConfigSource::Builtin { name } => init_ocio_builtin(name),
        OcioConfigSource::Path { path } => {
            if path.exists() {
                init_ocio(path)
            } else {
                Err(format!(
                    "OCIO config file not found: {}\n\
                     Place a config.ocio file at this path or change the OCIO source in project settings.",
                    path.display()
                ))
            }
        }
        OcioConfigSource::Environment => {
            let resolved = resolve_from_environment()?;
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
    OCIO_STATE.lock().map(|g| g.path.as_deref() == Some(path)).unwrap_or(false)
}

/// Resolve an OCIO config path from the explicit `OCIO` environment variable.
fn resolve_from_environment() -> Result<PathBuf, String> {
    let env_path = std::env::var("OCIO").map_err(|_| {
        "OCIO environment source selected, but the OCIO environment variable is not set. \
         Set OCIO to a config.ocio path, choose Mondrian Standard, or choose an explicit config path."
            .to_string()
    })?;
    let p = PathBuf::from(&env_path);
    if p.exists() {
        tracing::info!(path=%p.display(), "using OCIO config from $OCIO");
        return Ok(p);
    }
    Err(format!(
        "OCIO environment source selected, but $OCIO points to a non-existent config file: {env_path}"
    ))
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
    /// OCIO uniform metadata and current values.
    pub uniforms: Vec<OcioGpuUniformBinding>,
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

/// Uniform value encoding reported by OCIO GPU shader extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcioGpuUniformType {
    /// Scalar double uniform. OCIO exposes packed values through f32 helper payloads.
    Double,
    /// Boolean uniform.
    Bool,
    /// Three-component floating-point uniform.
    Float3,
    /// Floating-point vector uniform.
    VectorFloat,
    /// Integer vector uniform.
    VectorInt,
    /// Unsupported or unknown uniform type.
    Unknown,
}

/// Uniform value payload copied from OCIO.
#[derive(Debug, Clone, PartialEq)]
pub enum OcioGpuUniformValue {
    /// Floating-point uniform payload.
    F32(Vec<f32>),
    /// Integer uniform payload.
    I32(Vec<i32>),
    /// Payload could not be copied by the current OCIO binding.
    Unsupported,
}

/// OCIO uniform metadata and current value payload.
#[derive(Debug, Clone, PartialEq)]
pub struct OcioGpuUniformBinding {
    /// Uniform index in the OCIO descriptor.
    pub index: u32,
    /// Uniform symbol name used in the emitted shader.
    pub name: String,
    /// OCIO-reported uniform type.
    pub uniform_type: OcioGpuUniformType,
    /// Byte offset into OCIO's packed uniform buffer layout.
    pub buffer_offset: usize,
    /// Logical scalar count for this payload.
    pub value_count: usize,
    /// Typed payload copied from OCIO.
    pub value: OcioGpuUniformValue,
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
            uniforms: ocio_uniform_bindings(desc),
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
            uniforms: ocio_uniform_bindings(desc),
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

fn ocio_uniform_bindings(desc: &GpuShaderDesc) -> Vec<OcioGpuUniformBinding> {
    desc.uniforms()
        .into_iter()
        .enumerate()
        .map(|(index, uniform)| OcioGpuUniformBinding {
            index: index as u32,
            name: uniform.name,
            uniform_type: ocio_uniform_type(uniform.uniform_type),
            buffer_offset: uniform.buffer_offset,
            value_count: uniform.value_count,
            value: ocio_uniform_value(uniform.value),
        })
        .collect()
}

fn ocio_uniform_type(uniform_type: OcioRsGpuUniformType) -> OcioGpuUniformType {
    match uniform_type {
        OcioRsGpuUniformType::Double => OcioGpuUniformType::Double,
        OcioRsGpuUniformType::Bool => OcioGpuUniformType::Bool,
        OcioRsGpuUniformType::Float3 => OcioGpuUniformType::Float3,
        OcioRsGpuUniformType::VectorFloat => OcioGpuUniformType::VectorFloat,
        OcioRsGpuUniformType::VectorInt => OcioGpuUniformType::VectorInt,
        OcioRsGpuUniformType::Unknown => OcioGpuUniformType::Unknown,
    }
}

fn ocio_uniform_value(value: OcioRsGpuUniformValue) -> OcioGpuUniformValue {
    match value {
        OcioRsGpuUniformValue::F32(values) => OcioGpuUniformValue::F32(values),
        OcioRsGpuUniformValue::I32(values) => OcioGpuUniformValue::I32(values),
        OcioRsGpuUniformValue::Unsupported => OcioGpuUniformValue::Unsupported,
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

/// Apply an OCIO color-space conversion to an `&mut [f32]` RGBA linear-light
/// buffer without u8 quantization.
///
/// The buffer is treated as `num_pixels × 4` channels in the **source**
/// encoding. OCIO decodes, converts primaries, and re-encodes into the
/// destination encoding. Alpha is passed through unchanged.
///
/// This is the precision-preserving alternative to [`apply_ocio_rgba8`].
pub fn apply_ocio_float(data: &mut [f32], src: ColorSpace, dst: ColorSpace) -> Result<(), String> {
    if data.is_empty() || src == dst {
        return Ok(());
    }

    let cpu = ocio_cpu_processor(src, dst)?;
    apply_cpu_processor_float(&cpu, data);
    Ok(())
}

/// Apply an OCIO source -> working -> output conversion to an RGBA f32 buffer
/// without u8 quantization.
pub fn apply_ocio_pipeline_float(
    data: &mut [f32],
    src: ColorSpace,
    working: ColorSpace,
    dst: ColorSpace,
) -> Result<(), String> {
    if data.is_empty() || (src == working && working == dst) {
        return Ok(());
    }

    if src != working {
        apply_ocio_float(data, src, working)?;
    }
    if working != dst {
        apply_ocio_float(data, working, dst)?;
    }
    Ok(())
}

/// Apply an OCIO display transform to an RGBA f32 buffer without u8
/// quantization.
pub fn apply_ocio_display_float(
    data: &mut [f32],
    src: ColorSpace,
    display: &str,
    view: &str,
) -> Result<(), String> {
    if data.is_empty() {
        return Ok(());
    }

    let cpu = ocio_display_cpu_processor(src, display, view)?;
    apply_cpu_processor_float(&cpu, data);
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
    desc.set_function_name(MONDRIAN_OCIO_GPU_FUNCTION_NAME)
        .map_err(|e| format!("OCIO GPU shader function name: {e}"))?;
    desc.set_pixel_name(MONDRIAN_OCIO_GPU_PIXEL_NAME)
        .map_err(|e| format!("OCIO GPU shader pixel name: {e}"))?;
    desc.set_resource_prefix(MONDRIAN_OCIO_GPU_RESOURCE_PREFIX)
        .map_err(|e| format!("OCIO GPU shader resource prefix: {e}"))?;
    desc.set_descriptor_set_index(
        MONDRIAN_OCIO_GPU_DESCRIPTOR_SET_INDEX,
        MONDRIAN_OCIO_GPU_TEXTURE_BINDING_START,
    );
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

/// Low-level: run an already-obtained [`CPUProcessor`] over an RGBA f32 buffer.
///
/// Unlike [`apply_cpu_processor_rgba8`], this operates directly on f32 data
/// without any u8 quantization round-trip. The caller must ensure `data` contains
/// RGBA f32 pixels (4 floats per pixel, linear light).
fn apply_cpu_processor_float(cpu: &CPUProcessor, data: &mut [f32]) {
    let num_pixels = (data.len() / 4) as i64;
    if num_pixels == 0 {
        return;
    }
    cpu.apply_rgba_pixels(data, num_pixels, 4);
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
    use std::sync::{Mutex, OnceLock};

    fn ocio_env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().expect("OCIO env test lock")
    }

    fn set_ocio_env_for_test(value: Option<&std::path::Path>) {
        // Process environment mutation is serialized by `ocio_env_test_lock`.
        unsafe {
            match value {
                Some(path) => std::env::set_var("OCIO", path),
                None => std::env::remove_var("OCIO"),
            }
        }
    }

    #[test]
    fn mondrian_default_config_asset_parses_and_is_named() {
        let config = Config::from_stream(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO config should parse");
        let contract = mondrian_default_ocio_contract();

        assert_eq!(config.name().as_deref(), Some(contract.config_name));
        assert!(config.num_color_spaces() > contract.color_spaces.len() as i32);
        assert_eq!(
            config.default_display().as_deref(),
            Some(contract.default_display)
        );
        assert_eq!(
            config.default_view(contract.default_display).as_deref(),
            Some(contract.default_view)
        );
    }

    #[test]
    fn mondrian_default_config_covers_color_space_contract() {
        let config = Config::from_stream(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO config should parse");
        let contract = mondrian_default_ocio_contract();

        for mapped in contract.color_spaces {
            assert_eq!(mapped.ocio_name, ocio_color_space_name(mapped.color_space));
            assert!(
                config.canonical_name(mapped.ocio_name).is_some(),
                "{:?} mapped to missing OCIO color space '{}'",
                mapped.color_space,
                mapped.ocio_name
            );
        }
    }

    #[test]
    fn mondrian_default_config_covers_display_view_contract() {
        let config = Config::from_stream(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO config should parse");
        let contract = mondrian_default_ocio_contract();

        for display_view in contract.display_views {
            let views = (0..config.num_views(display_view.display))
                .filter_map(|index| config.view(display_view.display, index))
                .collect::<Vec<_>>();
            assert!(
                views.iter().any(|view| view == display_view.view),
                "display '{}' is missing view '{}'; views: {views:?}",
                display_view.display,
                display_view.view
            );
        }
    }

    #[test]
    fn environment_ocio_source_fails_closed_when_env_path_is_missing() {
        let _guard = ocio_env_test_lock();
        let original = std::env::var_os("OCIO");
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-env-ocio-config-{}.ocio",
            std::process::id()
        ));
        if missing_path.exists() {
            std::fs::remove_file(&missing_path).expect("remove stale missing OCIO test file");
        }
        set_ocio_env_for_test(Some(&missing_path));

        let err = ensure_ocio_loaded(&OcioConfigSource::Environment)
            .expect_err("environment OCIO source must not fall back from a missing $OCIO path");

        assert!(err.contains("OCIO environment source selected"));
        assert!(err.contains("$OCIO points to a non-existent config file"));
        assert!(err.contains(missing_path.to_string_lossy().as_ref()));

        set_ocio_env_for_test(original.as_deref().map(std::path::Path::new));
    }

    #[test]
    fn mondrian_default_contract_validation_checks_cpu_and_gpu_processors() {
        let contract = mondrian_default_ocio_contract();
        let report = validate_mondrian_default_ocio_contract()
            .unwrap_or_else(|err| panic!("default OCIO contract errors: {:#?}", err.issues));

        assert_eq!(report.config_name, contract.config_name);
        assert_eq!(
            report.color_space_mappings_checked,
            contract.color_spaces.len()
        );
        assert_eq!(report.display_views_checked, contract.display_views.len());
        assert_eq!(report.roles_checked, 3);
        assert_eq!(
            report.color_space_cpu_processors_checked,
            contract.color_spaces.len() * contract.color_spaces.len()
        );
        assert_eq!(
            report.color_space_gpu_processors_checked,
            contract.color_spaces.len() * (contract.color_spaces.len() - 1)
        );
        assert_eq!(
            report.display_cpu_processors_checked,
            contract.color_spaces.len() * contract.display_views.len()
        );
        assert_eq!(
            report.display_gpu_processors_checked,
            contract.color_spaces.len() * contract.display_views.len()
        );
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
        let contract = mondrian_default_ocio_contract();

        assert!(mondrian_default_ocio_available());
        assert_eq!(
            ocio_config_path().as_deref(),
            Some(Path::new(contract.virtual_path))
        );
        assert_eq!(
            ocio_default_display_view()
                .as_ref()
                .map(|(display, view)| { (display.as_str(), view.as_str()) }),
            Some((contract.default_display, contract.default_view))
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
        assert_eq!(bundle.uniforms.len() as u32, bundle.uniform_count);
        assert_eq!(bundle.textures_2d.len() as u32, bundle.texture_2d_count);
        assert_eq!(bundle.textures_3d.len() as u32, bundle.texture_3d_count);
        for uniform in &bundle.uniforms {
            assert!(!uniform.name.trim().is_empty());
            assert!(uniform.buffer_offset <= bundle.uniform_buffer_size);
            match &uniform.value {
                OcioGpuUniformValue::F32(values) => assert_eq!(values.len(), uniform.value_count),
                OcioGpuUniformValue::I32(values) => assert_eq!(values.len(), uniform.value_count),
                OcioGpuUniformValue::Unsupported => {}
            }
        }
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

    #[test]
    fn config_generation_increments_on_load() {
        let gen_before = ocio_config_generation();
        ensure_mondrian_default_ocio_loaded().expect("load default config");
        let gen_after = ocio_config_generation();
        assert!(gen_after >= gen_before, "generation should not decrease");
        // If the config was already loaded, generation stays the same.
        // If it was freshly loaded, generation increments.
    }

    #[test]
    fn config_source_tracks_identity() {
        ensure_mondrian_default_ocio_loaded().expect("load default config");
        let source = ocio_config_source();
        assert_eq!(source, Some(OcioConfigSource::MondrianDefault));
    }

    #[test]
    fn ensure_ocio_loaded_is_idempotent() {
        // Ensure config is loaded first.
        ensure_mondrian_default_ocio_loaded().expect("load default config");
        let gen1 = ocio_config_generation();
        ensure_mondrian_default_ocio_loaded().expect("second load");
        let gen2 = ocio_config_generation();
        assert_eq!(gen1, gen2, "repeated load should not change generation");
    }
}
