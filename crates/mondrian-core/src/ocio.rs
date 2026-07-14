//! OCIO (OpenColorIO) integration for color management.
//!
//! Color transforms are delegated to an OCIO v2.5.2 config whenever a config /
//! processor can be resolved. `ColorEngine::mondrian_standard()` is the productized
//! default policy and resolves to Mondrian's built-in OCIO config; custom OCIO
//! ACES resolves from a pinned official preset, while Custom OCIO resolves from
//! an explicit [`OcioConfigSource`].
//!
//! The config source is determined by [`OcioConfigSource`]:
//!
//! 1. **MondrianDefault** — Mondrian Standard/Simple built-in config
//! 2. **Builtin** — named built-in config (e.g. `"aces_1.2"`)
//! 3. **Path** — explicit `config.ocio` file path
//! 4. **Environment** — explicit `$OCIO` env var

use crate::types::{
    ColorSpace, MondrianStandardPackageIdentity, MondrianStandardVersion, OcioColorSpaceIdentity,
    OcioConfigSource, WorkingColorSpace,
};
pub use ocio_rs::GpuLanguage;
use ocio_rs::{
    transform::{
        AllocationTransform, BuiltinTransform, ColorSpaceTransform, GroupTransform, Lut3DTransform,
        MatrixTransform,
    },
    Allocation, BuiltinConfigRegistry, CPUProcessor, Config, GpuShaderDesc,
    GpuTextureChannel as OcioRsGpuTextureChannel,
    GpuTextureDimensions as OcioRsGpuTextureDimensions, GpuUniformType as OcioRsGpuUniformType,
    GpuUniformValue as OcioRsGpuUniformValue, Interpolation as OcioRsInterpolation,
    ReferenceSpaceType, ViewTransform, ViewTransformDirection,
};
use sha2::{Digest, Sha256};
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

/// SHA-256 digest pinned to the exact OCIO text shipped by Mondrian Standard v1.
pub const MONDRIAN_DEFAULT_OCIO_CONFIG_SHA256: &str =
    MondrianStandardPackageIdentity::V1.config_sha256();
/// SHA-256 over the versioned config text and every embedded Standard resource.
pub const MONDRIAN_DEFAULT_OCIO_PACKAGE_SHA256: &str =
    MondrianStandardPackageIdentity::V1.package_sha256();

const MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH: &str = "embedded:mondrian_default_ocio_v1";
const MONDRIAN_DEFAULT_OCIO_CONFIG: &str =
    include_str!("../assets/ocio/mondrian_default_ocio_v1.ocio");

const MONDRIAN_STANDARD_SDR_VIEW_NAME: &str = "Mondrian Standard SDR v1";
const MONDRIAN_STANDARD_SDR_LUT_NAME: &str = "mondrian_standard_sdr_rec709_v1.cube";
const MONDRIAN_STANDARD_SDR_LUT_SHA256: &str =
    "02f4d185608daa67fda01a1a48529bbc1533c8afdc826cde5c78f2eb5bb1b839";
const MONDRIAN_STANDARD_SDR_LUT: &str =
    include_str!("../assets/ocio/mondrian_standard_sdr_rec709_v1.cube");
const MONDRIAN_STANDARD_SDR_LUT_EDGE: usize = 57;
const MONDRIAN_STANDARD_ASSEMBLY_MANIFEST: &str = concat!(
    "mondrian-standard-assembly-v1\n",
    "working=Linear Rec.2020\n",
    "view=Mondrian Standard SDR v1\n",
    "scene_reference=UTILITY - ACES-AP0_to_CIE-XYZ-D65_BFD\n",
    "formation_gamut=FilmLight E-Gamut\n",
    "shaper=log2[-12.47393,12.5260688117]\n",
    "formation_lut=mondrian_standard_sdr_rec709_v1.cube;edge=57;interpolation=tetrahedral\n",
    "display_reference=CIE XYZ-D65 - Display-referred\n",
    "displays=sRGB - Display,Gamma 2.2 Rec.709 - Display,Rec.1886 Rec.709 - Display,Display P3 - Display\n",
);
const MONDRIAN_STANDARD_SDR_ALLOCATION_VARS: [f32; 2] = [-12.47393, 12.526_069];

// FilmLight E-Gamut XYZ D65 -> RGB matrix used by the pinned AgX formation
// resource. The preceding built-in converts Mondrian's AP0 scene reference to
// CIE XYZ D65, keeping chromatic adaptation inside stock OCIO.
const XYZ_D65_TO_FILMLIGHT_E_GAMUT: [f64; 16] = [
    1.525_052_8,
    -0.315_913_5,
    -0.122_658_3,
    0.0,
    -0.509_152_6,
    1.333_327_4,
    0.138_284_4,
    0.0,
    0.095_715_3,
    0.050_897_4,
    0.787_955_8,
    0.0,
    0.0,
    0.0,
    0.0,
    1.0,
];

/// Product-level contract for Mondrian's embedded default OCIO config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MondrianDefaultOcioContract {
    /// Versioned Mondrian Standard package semantics.
    pub standard_version: MondrianStandardVersion,
    /// Pinned OCIO config name.
    pub config_name: &'static str,
    /// SHA-256 digest of the exact embedded OCIO config text.
    pub content_sha256: &'static str,
    /// SHA-256 digest covering the config and all embedded resources.
    pub package_sha256: &'static str,
    /// Virtual path used when the embedded config is loaded into process state.
    pub virtual_path: &'static str,
    /// Default display selected by Standard mode.
    pub default_display: &'static str,
    /// Default view selected for [`Self::default_display`].
    pub default_view: &'static str,
    /// Scene-linear working role expected by Mondrian's internal compositor.
    pub scene_linear_role: &'static str,
    /// Scene-linear wide-gamut working space pinned by Standard v1.
    pub working_space: WorkingColorSpace,
    /// Mondrian color spaces that must be present in the embedded config.
    pub color_spaces: &'static [MondrianDefaultOcioColorSpace],
    /// Product-supported display/view pairs that must be present in the embedded config.
    pub display_views: &'static [MondrianDefaultOcioDisplayView],
    /// Immutable non-config resources assembled into the in-memory OCIO package.
    pub resources: &'static [MondrianDefaultOcioResource],
}

/// One immutable resource assembled into Mondrian Standard's stock OCIO config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MondrianDefaultOcioResource {
    /// Stable package-local resource name.
    pub name: &'static str,
    /// SHA-256 of the exact embedded bytes.
    pub content_sha256: &'static str,
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
    /// Mondrian Standard package version that was validated.
    pub standard_version: MondrianStandardVersion,
    /// Parsed config name.
    pub config_name: String,
    /// SHA-256 digest computed from the validated embedded config text.
    pub content_sha256: String,
    /// SHA-256 digest computed across config and embedded resources.
    pub package_sha256: String,
    /// Number of OCIO color spaces in the embedded config.
    pub color_space_count: i32,
    /// Number of Mondrian color-space mappings validated against OCIO canonical names.
    pub color_space_mappings_checked: usize,
    /// Number of product display/view pairs validated against the config.
    pub display_views_checked: usize,
    /// Number of immutable package resources whose digest and structure were validated.
    pub resources_checked: usize,
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

const MONDRIAN_DEFAULT_OCIO_COLOR_SPACES: [MondrianDefaultOcioColorSpace; 27] = [
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Rec709,
        ocio_name: "Camera Rec.709",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Rec601Pal,
        ocio_name: "Camera Rec.601 PAL",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Rec601Ntsc,
        ocio_name: "Camera Rec.601 NTSC",
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
        ocio_name: "Camera Rec.2020",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::DisplayP3,
        ocio_name: "sRGB Encoded P3-D65",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::LinearRec709,
        ocio_name: "Linear Rec.709 (sRGB)",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::LinearRec2020,
        ocio_name: "Linear Rec.2020",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::LinearP3D65,
        ocio_name: "Linear P3-D65",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::Aces2065_1,
        ocio_name: "ACES2065-1",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::AcesCg,
        ocio_name: "ACEScg",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::AcesCct,
        ocio_name: "ACEScct",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::AppleLogBt2020,
        ocio_name: "Apple Log",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::SonySLog2SGamut,
        ocio_name: "S-Log2 S-Gamut",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::SonySLog3SGamut3,
        ocio_name: "S-Log3 S-Gamut3",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::SonySLog3SGamut3Cine,
        ocio_name: "S-Log3 S-Gamut3.Cine",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::ArriLogC3WideGamut3,
        ocio_name: "ARRI LogC3 (EI800)",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::ArriLogC4WideGamut4,
        ocio_name: "ARRI LogC4",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::CanonLog2CinemaGamutD55,
        ocio_name: "CanonLog2 CinemaGamut D55",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::CanonLog3CinemaGamutD55,
        ocio_name: "CanonLog3 CinemaGamut D55",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::PanasonicVLogVGamut,
        ocio_name: "V-Log V-Gamut",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::RedLog3G10WideGamutRgb,
        ocio_name: "Log3G10 REDWideGamutRGB",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::BlackmagicFilmWideGamutGen5,
        ocio_name: "BMDFilm WideGamut Gen5",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::DjiDLogDGamut,
        ocio_name: "D-Log D-Gamut",
    },
    MondrianDefaultOcioColorSpace {
        color_space: ColorSpace::DavinciIntermediateWideGamut,
        ocio_name: "DaVinci Intermediate WideGamut",
    },
];

const MONDRIAN_DEFAULT_OCIO_DISPLAY_VIEWS: [MondrianDefaultOcioDisplayView; 4] = [
    MondrianDefaultOcioDisplayView {
        display: "sRGB - Display",
        view: MONDRIAN_STANDARD_SDR_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "sRGB - Display",
        view: "Video (colorimetric)",
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.1886 Rec.709 - Display",
        view: MONDRIAN_STANDARD_SDR_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "Display P3 - Display",
        view: MONDRIAN_STANDARD_SDR_VIEW_NAME,
    },
];

const MONDRIAN_DEFAULT_OCIO_RESOURCES: [MondrianDefaultOcioResource; 1] =
    [MondrianDefaultOcioResource {
        name: MONDRIAN_STANDARD_SDR_LUT_NAME,
        content_sha256: MONDRIAN_STANDARD_SDR_LUT_SHA256,
    }];

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
        standard_version: MondrianStandardVersion::V1,
        config_name: MONDRIAN_DEFAULT_OCIO_CONFIG_NAME,
        content_sha256: MONDRIAN_DEFAULT_OCIO_CONFIG_SHA256,
        package_sha256: MONDRIAN_DEFAULT_OCIO_PACKAGE_SHA256,
        virtual_path: MONDRIAN_DEFAULT_OCIO_VIRTUAL_PATH,
        default_display: "sRGB - Display",
        default_view: MONDRIAN_STANDARD_SDR_VIEW_NAME,
        scene_linear_role: "Linear Rec.2020",
        working_space: WorkingColorSpace::LinearRec2020,
        color_spaces: &MONDRIAN_DEFAULT_OCIO_COLOR_SPACES,
        display_views: &MONDRIAN_DEFAULT_OCIO_DISPLAY_VIEWS,
        resources: &MONDRIAN_DEFAULT_OCIO_RESOURCES,
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
    validate_mondrian_default_ocio_contract_text(MONDRIAN_DEFAULT_OCIO_CONFIG)
}

fn validate_mondrian_default_ocio_contract_text(
    config_text: &str,
) -> Result<MondrianDefaultOcioValidationReport, MondrianDefaultOcioValidationError> {
    let contract = mondrian_default_ocio_contract();
    let content_sha256 = sha256_hex(config_text.as_bytes());
    if content_sha256 != contract.content_sha256 {
        return Err(MondrianDefaultOcioValidationError::new(vec![format!(
            "embedded Mondrian OCIO config SHA-256 mismatch: expected '{}', got '{}'",
            contract.content_sha256, content_sha256
        )]));
    }
    let config = match build_mondrian_default_ocio_config(config_text) {
        Ok(config) => config,
        Err(err) => {
            return Err(MondrianDefaultOcioValidationError::new(vec![format!(
                "embedded Mondrian OCIO package '{}' failed to build: {err}",
                contract.config_name
            )]));
        }
    };
    let package_sha256 = match mondrian_default_package_sha256(config_text, &config, contract) {
        Ok(digest) => digest,
        Err(err) => return Err(MondrianDefaultOcioValidationError::new(vec![err])),
    };
    if package_sha256 != contract.package_sha256 {
        return Err(MondrianDefaultOcioValidationError::new(vec![format!(
            "embedded Mondrian OCIO package SHA-256 mismatch: expected '{}', got '{}'",
            contract.package_sha256, package_sha256
        )]));
    }

    let mut errors = Vec::new();
    let mut report = MondrianDefaultOcioValidationReport {
        standard_version: contract.standard_version,
        config_name: config.name().unwrap_or_default(),
        content_sha256,
        package_sha256,
        color_space_count: config.num_color_spaces(),
        color_space_mappings_checked: 0,
        display_views_checked: 0,
        resources_checked: 0,
        roles_checked: 0,
        color_space_cpu_processors_checked: 0,
        color_space_gpu_processors_checked: 0,
        display_cpu_processors_checked: 0,
        display_gpu_processors_checked: 0,
    };

    validate_mondrian_default_config_identity(&config, contract, &mut report, &mut errors);
    validate_mondrian_default_resources(contract, &mut report, &mut errors);
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

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn mondrian_default_package_sha256(
    config_text: &str,
    config: &Config,
    contract: MondrianDefaultOcioContract,
) -> Result<String, String> {
    let mut digest = Sha256::new();
    digest.update(b"mondrian-standard-assembled-ocio-package-v1\0");
    digest.update(config_text.as_bytes());
    digest.update([0]);
    digest.update(MONDRIAN_STANDARD_ASSEMBLY_MANIFEST.as_bytes());
    digest.update([0]);
    digest.update(MONDRIAN_STANDARD_SDR_LUT_NAME.as_bytes());
    digest.update([0]);
    digest.update(MONDRIAN_STANDARD_SDR_LUT.as_bytes());
    digest.update([0]);
    update_mondrian_default_processor_fingerprint(&mut digest, config, contract)?;
    let digest = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(encoded)
}

fn update_mondrian_default_processor_fingerprint(
    digest: &mut Sha256,
    config: &Config,
    contract: MondrianDefaultOcioContract,
) -> Result<(), String> {
    let working_name = ocio_working_color_space_name(contract.working_space);

    for mapped in contract.color_spaces {
        let source_name = ocio_color_space_name(mapped.color_space);
        update_processor_fingerprint(
            digest,
            &format!("colorspace:{source_name}->{working_name}"),
            config.processor(source_name, working_name).map_err(|err| {
                format!(
                    "embedded Mondrian OCIO package could not build semantic fingerprint processor '{source_name}->{working_name}': {err}"
                )
            })?,
        )?;
        update_processor_fingerprint(
            digest,
            &format!("colorspace:{working_name}->{source_name}"),
            config.processor(working_name, source_name).map_err(|err| {
                format!(
                    "embedded Mondrian OCIO package could not build semantic fingerprint processor '{working_name}->{source_name}': {err}"
                )
            })?,
        )?;
    }

    for display_view in contract.display_views {
        update_processor_fingerprint(
            digest,
            &format!(
                "display:{working_name}->{}/{}",
                display_view.display, display_view.view
            ),
            config
                .processor_display(
                    working_name,
                    display_view.display,
                    display_view.view,
                    ocio_rs::TransformDirection::Forward,
                )
                .map_err(|err| {
                    format!(
                        "embedded Mondrian OCIO package could not build semantic fingerprint display processor '{working_name}->{}/{}': {err}",
                        display_view.display, display_view.view
                    )
                })?,
        )?;
    }

    Ok(())
}

fn update_processor_fingerprint(
    digest: &mut Sha256,
    label: &str,
    processor: ocio_rs::Processor,
) -> Result<(), String> {
    update_fingerprint_field(digest, "processor", label);
    let processor_cache_id = processor.cache_id().ok_or_else(|| {
        format!("embedded Mondrian OCIO processor '{label}' has no semantic cache-id")
    })?;
    update_fingerprint_field(digest, "processor-cache-id", &processor_cache_id);

    let cpu = processor.default_cpu_processor().map_err(|err| {
        format!("embedded Mondrian OCIO processor '{label}' has no CPU implementation: {err}")
    })?;
    let cpu_cache_id = cpu.cache_id().ok_or_else(|| {
        format!("embedded Mondrian OCIO CPU processor '{label}' has no semantic cache-id")
    })?;
    update_fingerprint_field(digest, "cpu-cache-id", &cpu_cache_id);

    let gpu = processor.default_gpu_processor().map_err(|err| {
        format!("embedded Mondrian OCIO processor '{label}' has no GPU implementation: {err}")
    })?;
    let gpu_cache_id = gpu.cache_id().ok_or_else(|| {
        format!("embedded Mondrian OCIO GPU processor '{label}' has no semantic cache-id")
    })?;
    update_fingerprint_field(digest, "gpu-cache-id", &gpu_cache_id);
    Ok(())
}

fn update_fingerprint_field(digest: &mut Sha256, kind: &str, value: &str) {
    digest.update((kind.len() as u64).to_le_bytes());
    digest.update(kind.as_bytes());
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
}

fn validate_mondrian_default_resources(
    contract: MondrianDefaultOcioContract,
    report: &mut MondrianDefaultOcioValidationReport,
    errors: &mut Vec<String>,
) {
    for resource in contract.resources {
        match resource.name {
            MONDRIAN_STANDARD_SDR_LUT_NAME
                if resource.content_sha256 == MONDRIAN_STANDARD_SDR_LUT_SHA256
                    && sha256_hex(MONDRIAN_STANDARD_SDR_LUT.as_bytes())
                        == resource.content_sha256 =>
            {
                report.resources_checked += 1;
            }
            MONDRIAN_STANDARD_SDR_LUT_NAME => errors.push(format!(
                "resource '{}' digest contract does not match embedded content",
                resource.name
            )),
            _ => errors.push(format!(
                "resource '{}' has no embedded package implementation",
                resource.name
            )),
        }
    }
}

#[derive(Debug, PartialEq)]
struct ParsedCube3d {
    edge: usize,
    domain_min: [f64; 3],
    domain_max: [f64; 3],
    values: Vec<f64>,
}

fn parse_cube_3d(text: &str) -> Result<ParsedCube3d, String> {
    let mut edge = None;
    let mut domain_min = [0.0, 0.0, 0.0];
    let mut domain_max = [1.0, 1.0, 1.0];
    let mut values = Vec::new();

    for (line_index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("TITLE") {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(first) = fields.next() else {
            continue;
        };
        let line_number = line_index + 1;
        match first {
            "LUT_3D_SIZE" => {
                let parsed = fields
                    .next()
                    .ok_or_else(|| format!("line {line_number}: LUT_3D_SIZE is missing"))?
                    .parse::<usize>()
                    .map_err(|err| format!("line {line_number}: invalid LUT_3D_SIZE: {err}"))?;
                if fields.next().is_some() || !(2..=129).contains(&parsed) {
                    return Err(format!(
                        "line {line_number}: LUT_3D_SIZE must contain one edge in 2..=129"
                    ));
                }
                edge = Some(parsed);
            }
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let mut parsed = [0.0; 3];
                for channel in &mut parsed {
                    *channel = fields
                        .next()
                        .ok_or_else(|| format!("line {line_number}: {first} needs 3 values"))?
                        .parse::<f64>()
                        .map_err(|err| {
                            format!("line {line_number}: invalid {first} value: {err}")
                        })?;
                }
                if fields.next().is_some() || parsed.iter().any(|value| !value.is_finite()) {
                    return Err(format!(
                        "line {line_number}: {first} must contain 3 finite values"
                    ));
                }
                if first == "DOMAIN_MIN" {
                    domain_min = parsed;
                } else {
                    domain_max = parsed;
                }
            }
            _ => {
                let mut row = Vec::with_capacity(3);
                row.push(first);
                row.extend(fields);
                if row.len() != 3 {
                    return Err(format!(
                        "line {line_number}: LUT row must contain exactly 3 values"
                    ));
                }
                for value in row {
                    let parsed = value.parse::<f64>().map_err(|err| {
                        format!("line {line_number}: invalid LUT value '{value}': {err}")
                    })?;
                    if !parsed.is_finite() {
                        return Err(format!("line {line_number}: LUT value must be finite"));
                    }
                    values.push(parsed);
                }
            }
        }
    }

    let edge = edge.ok_or_else(|| "LUT_3D_SIZE is missing".to_string())?;
    let expected_values = edge
        .checked_pow(3)
        .and_then(|entries| entries.checked_mul(3))
        .ok_or_else(|| "LUT_3D_SIZE overflows the host address space".to_string())?;
    if values.len() != expected_values {
        return Err(format!(
            "LUT contains {} channel values; {edge}^3 requires {expected_values}",
            values.len()
        ));
    }
    if domain_min != [0.0, 0.0, 0.0] || domain_max != [1.0, 1.0, 1.0] {
        return Err(format!(
            "Mondrian Standard LUT requires normalized 0..1 domain, got {domain_min:?}..{domain_max:?}"
        ));
    }

    Ok(ParsedCube3d { edge, domain_min, domain_max, values })
}

fn build_mondrian_standard_sdr_view(config: &Config) -> Result<(), String> {
    let actual_digest = sha256_hex(MONDRIAN_STANDARD_SDR_LUT.as_bytes());
    if actual_digest != MONDRIAN_STANDARD_SDR_LUT_SHA256 {
        return Err(format!(
            "embedded Mondrian Standard resource '{MONDRIAN_STANDARD_SDR_LUT_NAME}' failed integrity validation: expected SHA-256 '{MONDRIAN_STANDARD_SDR_LUT_SHA256}', got '{actual_digest}'"
        ));
    }
    let cube = parse_cube_3d(MONDRIAN_STANDARD_SDR_LUT)
        .map_err(|err| format!("failed to parse '{MONDRIAN_STANDARD_SDR_LUT_NAME}': {err}"))?;
    if cube.edge != MONDRIAN_STANDARD_SDR_LUT_EDGE {
        return Err(format!(
            "'{MONDRIAN_STANDARD_SDR_LUT_NAME}' edge mismatch: expected {MONDRIAN_STANDARD_SDR_LUT_EDGE}, got {}",
            cube.edge
        ));
    }

    let ap0_to_xyz_d65 = BuiltinTransform::create().map_err(|err| err.to_string())?;
    ap0_to_xyz_d65
        .set_style("UTILITY - ACES-AP0_to_CIE-XYZ-D65_BFD")
        .map_err(|err| err.to_string())?;

    let xyz_to_egamut = MatrixTransform::create().map_err(|err| err.to_string())?;
    xyz_to_egamut.set_matrix(&XYZ_D65_TO_FILMLIGHT_E_GAMUT);

    let allocation = AllocationTransform::create().map_err(|err| err.to_string())?;
    allocation.set_allocation(Allocation::Lg2);
    allocation.set_vars(&MONDRIAN_STANDARD_SDR_ALLOCATION_VARS);

    let formation = Lut3DTransform::create().map_err(|err| err.to_string())?;
    formation.set_grid_size(cube.edge as u64);
    formation.set_interpolation(OcioRsInterpolation::Tetrahedral);
    formation.set_values(&cube.values);

    let to_display_reference = ColorSpaceTransform::create().map_err(|err| err.to_string())?;
    to_display_reference
        .set_src("Rec.1886 Rec.709 - Display")
        .map_err(|err| err.to_string())?;
    to_display_reference
        .set_dst("CIE XYZ-D65 - Display-referred")
        .map_err(|err| err.to_string())?;

    let group = GroupTransform::create().map_err(|err| err.to_string())?;
    group.append_transform(&ap0_to_xyz_d65);
    group.append_transform(&xyz_to_egamut);
    group.append_transform(&allocation);
    group.append_transform(&formation);
    group.append_transform(&to_display_reference);

    let view = ViewTransform::create(ReferenceSpaceType::Scene).map_err(|err| err.to_string())?;
    view.set_name(MONDRIAN_STANDARD_SDR_VIEW_NAME).map_err(|err| err.to_string())?;
    view.set_family("Mondrian Standard").map_err(|err| err.to_string())?;
    view.set_description(
        "Mondrian Standard v1 neutral scene-to-SDR formation: AP0 reference to FilmLight E-Gamut, log2 shaper, pinned AgX formation LUT, then display encoding.",
    )
    .map_err(|err| err.to_string())?;
    view.set_transform(Some(&group), ViewTransformDirection::FromReference);
    config.add_view_transform(&view);

    config
        .add_shared_view(
            MONDRIAN_STANDARD_SDR_VIEW_NAME,
            MONDRIAN_STANDARD_SDR_VIEW_NAME,
            "<USE_DISPLAY_NAME>",
            "",
            "Any Scene-linear or Log",
            "Mondrian Standard v1 SDR rendering transform",
        )
        .map_err(|err| err.to_string())?;
    for display in [
        "sRGB - Display",
        "Gamma 2.2 Rec.709 - Display",
        "Rec.1886 Rec.709 - Display",
        "Display P3 - Display",
    ] {
        config
            .add_display_shared_view(display, MONDRIAN_STANDARD_SDR_VIEW_NAME)
            .map_err(|err| err.to_string())?;
    }
    config
        .set_active_views(format!(
            "{MONDRIAN_STANDARD_SDR_VIEW_NAME},Video (colorimetric),Un-tone-mapped,Raw"
        ))
        .map_err(|err| err.to_string())?;
    config.validate().map_err(|err| {
        format!("Mondrian Standard in-memory OCIO package failed validation: {err}")
    })?;
    Ok(())
}

fn build_mondrian_default_ocio_config(config_text: &str) -> Result<Config, String> {
    let config = Config::from_stream(config_text)
        .map_err(|err| format!("failed to parse base Mondrian Standard OCIO config: {err}"))?;
    build_mondrian_standard_sdr_view(&config)?;
    Ok(config)
}

fn validate_mondrian_default_config_identity(
    config: &Config,
    contract: MondrianDefaultOcioContract,
    report: &mut MondrianDefaultOcioValidationReport,
    errors: &mut Vec<String>,
) {
    let working_space_name = ocio_working_color_space_name(contract.working_space);
    if contract.scene_linear_role != working_space_name {
        errors.push(format!(
            "Mondrian Standard working-space contract mismatch: {:?} maps to '{}', role pins '{}'",
            contract.working_space, working_space_name, contract.scene_linear_role
        ));
    }
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
    let actual_digest = sha256_hex(MONDRIAN_DEFAULT_OCIO_CONFIG.as_bytes());
    if actual_digest != MONDRIAN_DEFAULT_OCIO_CONFIG_SHA256 {
        return Err(format!(
            "embedded Mondrian OCIO config '{}' failed integrity validation: expected SHA-256 '{}', got '{}'",
            MONDRIAN_DEFAULT_OCIO_CONFIG_NAME,
            MONDRIAN_DEFAULT_OCIO_CONFIG_SHA256,
            actual_digest
        ));
    }
    let config = build_mondrian_default_ocio_config(MONDRIAN_DEFAULT_OCIO_CONFIG).map_err(|e| {
        format!(
            "failed to load embedded Mondrian OCIO package '{}': {e}",
            MONDRIAN_DEFAULT_OCIO_CONFIG_NAME
        )
    })?;
    let actual_package_digest = mondrian_default_package_sha256(
        MONDRIAN_DEFAULT_OCIO_CONFIG,
        &config,
        mondrian_default_ocio_contract(),
    )?;
    if actual_package_digest != MONDRIAN_DEFAULT_OCIO_PACKAGE_SHA256 {
        return Err(format!(
            "embedded Mondrian OCIO package '{}' failed integrity validation: expected SHA-256 '{}', got '{}'",
            MONDRIAN_DEFAULT_OCIO_CONFIG_NAME,
            MONDRIAN_DEFAULT_OCIO_PACKAGE_SHA256,
            actual_package_digest
        ));
    }
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
        ColorSpace::Rec601Pal => "Camera Rec.601 PAL",
        ColorSpace::Rec601Ntsc => "Camera Rec.601 NTSC",
        ColorSpace::Rec2020 => "Camera Rec.2020",
        ColorSpace::Rec2100Pq => "Rec.2100-PQ - Display",
        ColorSpace::Rec2100Hlg => "Rec.2100-HLG - Display",
        ColorSpace::DisplayP3 => "sRGB Encoded P3-D65",
        ColorSpace::LinearRec709 => "Linear Rec.709 (sRGB)",
        ColorSpace::LinearRec2020 => "Linear Rec.2020",
        ColorSpace::LinearP3D65 => "Linear P3-D65",
        ColorSpace::Aces2065_1 => "ACES2065-1",
        ColorSpace::AcesCg => "ACEScg",
        ColorSpace::AcesCct => "ACEScct",
        ColorSpace::AppleLogBt2020 => "Apple Log",
        ColorSpace::SonySLog2SGamut => "S-Log2 S-Gamut",
        ColorSpace::SonySLog3SGamut3 => "S-Log3 S-Gamut3",
        ColorSpace::SonySLog3SGamut3Cine => "S-Log3 S-Gamut3.Cine",
        ColorSpace::ArriLogC3WideGamut3 => "ARRI LogC3 (EI800)",
        ColorSpace::ArriLogC4WideGamut4 => "ARRI LogC4",
        ColorSpace::CanonLog2CinemaGamutD55 => "CanonLog2 CinemaGamut D55",
        ColorSpace::CanonLog3CinemaGamutD55 => "CanonLog3 CinemaGamut D55",
        ColorSpace::PanasonicVLogVGamut => "V-Log V-Gamut",
        ColorSpace::RedLog3G10WideGamutRgb => "Log3G10 REDWideGamutRGB",
        ColorSpace::BlackmagicFilmWideGamutGen5 => "BMDFilm WideGamut Gen5",
        ColorSpace::DjiDLogDGamut => "D-Log D-Gamut",
        ColorSpace::DavinciIntermediateWideGamut => "DaVinci Intermediate WideGamut",
    }
}

/// Map a linear working space to its pinned OCIO color-space identity.
pub fn ocio_working_color_space_name(space: WorkingColorSpace) -> &'static str {
    match space {
        WorkingColorSpace::LinearRec709 => "Linear Rec.709 (sRGB)",
        WorkingColorSpace::LinearRec2020 => "Linear Rec.2020",
        WorkingColorSpace::LinearP3D65 => "Linear P3-D65",
        WorkingColorSpace::AcesCg => "ACEScg",
    }
}

/// Resolve an explicit encoded/working OCIO endpoint without role inference.
pub fn ocio_color_space_identity_name(identity: OcioColorSpaceIdentity) -> &'static str {
    match identity {
        OcioColorSpaceIdentity::Color(space) => ocio_color_space_name(space),
        OcioColorSpaceIdentity::Working(space) => ocio_working_color_space_name(space),
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
        src: OcioColorSpaceIdentity,
        dst: OcioColorSpaceIdentity,
        language: GpuLanguage,
        shader_text: String,
        desc: &GpuShaderDesc,
        cache_id: Option<String>,
    ) -> Self {
        Self {
            src_color_space: ocio_color_space_identity_name(src).to_string(),
            dst_color_space: ocio_color_space_identity_name(dst).to_string(),
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
        src: OcioColorSpaceIdentity,
        display: &str,
        view: &str,
        language: GpuLanguage,
        shader_text: String,
        desc: &GpuShaderDesc,
        cache_id: Option<String>,
    ) -> Self {
        Self {
            src_color_space: ocio_color_space_identity_name(src).to_string(),
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
fn ocio_cpu_processor(
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
) -> Result<CPUProcessor, String> {
    let config = ocio_rs::current_config()
        .ok_or_else(|| "no OCIO config loaded (call ensure_ocio_loaded first)".to_string())?;

    let src_name = ocio_color_space_identity_name(src);
    let dst_name = ocio_color_space_identity_name(dst);

    let processor = config
        .processor(src_name, dst_name)
        .map_err(|e| format!("OCIO processor '{src_name}' → '{dst_name}': {e}"))?;

    processor
        .default_cpu_processor()
        .map_err(|e| format!("OCIO CPU processor '{src_name}' → '{dst_name}': {e}"))
}

fn ocio_processor(
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
) -> Result<ocio_rs::Processor, String> {
    let config = ocio_rs::current_config()
        .ok_or_else(|| "no OCIO config loaded (call ensure_ocio_loaded first)".to_string())?;

    let src_name = ocio_color_space_identity_name(src);
    let dst_name = ocio_color_space_identity_name(dst);

    config
        .processor(src_name, dst_name)
        .map_err(|e| format!("OCIO processor '{src_name}' -> '{dst_name}': {e}"))
}

fn ocio_display_processor(
    src: OcioColorSpaceIdentity,
    display: &str,
    view: &str,
) -> Result<ocio_rs::Processor, String> {
    let config = ocio_rs::current_config()
        .ok_or_else(|| "no OCIO config loaded (call ensure_ocio_loaded first)".to_string())?;

    let src_name = ocio_color_space_identity_name(src);

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
    src: OcioColorSpaceIdentity,
    display: &str,
    view: &str,
) -> Result<CPUProcessor, String> {
    let config = ocio_rs::current_config()
        .ok_or_else(|| "no OCIO config loaded (call ensure_ocio_loaded first)".to_string())?;

    let src_name = ocio_color_space_identity_name(src);

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

/// Apply an OCIO conversion between explicit encoded/working identities.
pub fn apply_ocio_identity_float(
    data: &mut [f32],
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
) -> Result<(), String> {
    if data.is_empty() || src == dst {
        return Ok(());
    }

    let cpu = ocio_cpu_processor(src, dst)?;
    apply_cpu_processor_float(&cpu, data);
    Ok(())
}

/// Apply an OCIO display transform from an explicit encoded/working identity.
pub fn apply_ocio_display_identity_float(
    data: &mut [f32],
    src: OcioColorSpaceIdentity,
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
    extract_ocio_identity_gpu_shader_bundle(src.into(), dst.into(), language)
}

/// Extract a GPU shader bundle between explicit encoded/working identities.
pub fn extract_ocio_identity_gpu_shader_bundle(
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
    language: GpuLanguage,
) -> Result<OcioGpuShaderBundle, String> {
    let processor = ocio_processor(src, dst)?;
    let cache_id = processor.cache_id();
    let gpu = processor.default_gpu_processor().map_err(|e| {
        format!(
            "OCIO GPU processor '{}' -> '{}': {e}",
            ocio_color_space_identity_name(src),
            ocio_color_space_identity_name(dst)
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
    extract_ocio_display_identity_gpu_shader_bundle(src.into(), display, view, language)
}

/// Extract a GPU display/view shader from an explicit encoded/working identity.
pub fn extract_ocio_display_identity_gpu_shader_bundle(
    src: OcioColorSpaceIdentity,
    display: &str,
    view: &str,
    language: GpuLanguage,
) -> Result<OcioGpuShaderBundle, String> {
    let processor = ocio_display_processor(src, display, view)?;
    let cache_id = processor.cache_id();
    let gpu = processor.default_gpu_processor().map_err(|e| {
        format!(
            "OCIO GPU display processor '{}' -> {display}/{view}: {e}",
            ocio_color_space_identity_name(src),
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

/// Low-level: run an already-obtained [`CPUProcessor`] over an RGBA f32 buffer.
///
/// This operates directly on f32 data without any u8 quantization round-trip.
/// The caller must ensure `data` contains
/// RGBA f32 pixels (4 floats per pixel, linear light).
fn apply_cpu_processor_float(cpu: &CPUProcessor, data: &mut [f32]) {
    let num_pixels = (data.len() / 4) as i64;
    if num_pixels == 0 {
        return;
    }
    // Program color transforms own RGB only. Processing the interleaved buffer
    // through OCIO's RGB entry point preserves straight/premultiplied alpha
    // byte-for-byte and avoids a per-frame alpha side buffer.
    cpu.apply_rgb_pixels(data, num_pixels, 4);
}

// ── Utility: list available displays / views ───────────────────────────────────

/// Return the list of display names from the current OCIO config.
pub fn ocio_display_names() -> Vec<String> {
    if !ocio_available() {
        return Vec::new();
    }
    let Some(config) = ocio_rs::current_config() else {
        return Vec::new();
    };
    let n = config.num_displays();
    (0..n).filter_map(|i| config.display(i)).collect()
}

/// Return the list of view names for a given display.
pub fn ocio_view_names(display: &str) -> Vec<String> {
    if !ocio_available() {
        return Vec::new();
    }
    let Some(config) = ocio_rs::current_config() else {
        return Vec::new();
    };
    let n = config.num_views(display);
    (0..n).filter_map(|i| config.view(display, i)).collect()
}

/// Return the default view for a display from the current OCIO config.
pub fn ocio_default_view_for_display(display: &str) -> Option<String> {
    if !ocio_available() {
        return None;
    }
    let config = ocio_rs::current_config()?;
    config.default_view(display)
}

/// Return the default display / view pair from the current OCIO config.
pub fn ocio_default_display_view() -> Option<(String, String)> {
    if !ocio_available() {
        return None;
    }
    let config = ocio_rs::current_config()?;
    let display = config.default_display()?;
    let view = config.default_view(&display)?;
    Some((display, view))
}

/// Resolve the exact Standard display/view pair for an encoded output target.
///
/// This target-aware mapping prevents a P3 or HDR program output from silently
/// using the config's sRGB default display. Targets without a completed,
/// versioned Standard View fail closed rather than borrowing an ACES View.
pub fn mondrian_standard_output_display_view(
    output: ColorSpace,
) -> Result<(String, String), String> {
    let display = mondrian_standard_output_display_name(output)?;
    match output {
        ColorSpace::Rec2100Hlg => {
            return Err("Mondrian Standard HLG View is not available in package v1".to_owned());
        }
        ColorSpace::Rec2100Pq => {
            return Err(
                "Mondrian Standard PQ 1000-nit View is not available in package v1".to_owned(),
            );
        }
        _ => {}
    }

    ensure_mondrian_default_ocio_loaded()?;
    let views = ocio_view_names(display);
    if !views.iter().any(|view| view == MONDRIAN_STANDARD_SDR_VIEW_NAME) {
        return Err(format!(
            "Mondrian Standard package is missing required display/view '{display}/{MONDRIAN_STANDARD_SDR_VIEW_NAME}'"
        ));
    }
    Ok((
        display.to_owned(),
        MONDRIAN_STANDARD_SDR_VIEW_NAME.to_owned(),
    ))
}

/// Resolve the OCIO display identity paired with a Standard output target.
pub fn mondrian_standard_output_display_name(output: ColorSpace) -> Result<&'static str, String> {
    match output {
        ColorSpace::Srgb => Ok("sRGB - Display"),
        ColorSpace::Rec709 => Ok("Rec.1886 Rec.709 - Display"),
        ColorSpace::DisplayP3 => Ok("Display P3 - Display"),
        ColorSpace::Rec2100Hlg => Ok("Rec.2100-HLG - Display"),
        ColorSpace::Rec2100Pq => Ok("Rec.2100-PQ - Display"),
        unsupported => Err(format!(
            "Mondrian Standard has no rendering View for output target {unsupported:?}"
        )),
    }
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
        let config = build_mondrian_default_ocio_config(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO package should build");
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
    fn mondrian_standard_package_pins_linear_rec2020_working_space() {
        let contract = mondrian_default_ocio_contract();
        let config = Config::from_stream(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO config should parse");

        assert_eq!(contract.working_space, WorkingColorSpace::LinearRec2020);
        assert_eq!(
            contract.scene_linear_role,
            ocio_working_color_space_name(contract.working_space)
        );
        assert_eq!(
            config.role_color_space("scene_linear").as_deref(),
            Some(contract.scene_linear_role)
        );
    }

    #[test]
    fn encoded_rec2020_is_not_aliased_to_linear_rec2020_working_space() {
        let encoded_name = ocio_color_space_name(ColorSpace::Rec2020);
        let linear_name = ocio_working_color_space_name(WorkingColorSpace::LinearRec2020);
        assert_ne!(encoded_name, linear_name);

        let config = Config::from_stream(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO config should parse");
        let processor = config
            .processor(encoded_name, linear_name)
            .expect("encoded Rec.2020 to linear Rec.2020 processor");
        let cpu = processor.default_cpu_processor().expect("encoded Rec.2020 CPU processor");
        let mut mid_gray = [0.5, 0.5, 0.5, 0.375];
        apply_cpu_processor_float(&cpu, &mut mid_gray);

        for channel in &mid_gray[..3] {
            assert!(
                (0.24..0.28).contains(channel),
                "BT.2020 SDR code value 0.5 must decode near 0.26 linear, got {channel}"
            );
        }
        assert_eq!(mid_gray[3], 0.375, "OCIO must preserve alpha");
    }

    #[test]
    fn linear_rec2020_working_round_trip_preserves_unbounded_scene_values() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");
        let original = [
            -0.25, 0.18, 4.0, 0.25, 16.0, -1.0, 0.5, 0.75, 0.001, 2.0, -0.125, 1.0,
        ];
        let mut samples = original;

        apply_ocio_identity_float(
            &mut samples,
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::AcesCg),
        )
        .expect("Linear Rec.2020 to ACEScg comparison processor");

        assert!(samples.chunks_exact(4).flatten().any(|channel| *channel < 0.0));
        assert!(samples.chunks_exact(4).flatten().any(|channel| *channel > 1.0));

        apply_ocio_identity_float(
            &mut samples,
            OcioColorSpaceIdentity::Working(WorkingColorSpace::AcesCg),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
        )
        .expect("ACEScg to Linear Rec.2020 comparison processor");

        for (actual, expected) in samples.iter().zip(original) {
            let tolerance = 2.0e-5 * expected.abs().max(1.0);
            assert!(
                (actual - expected).abs() <= tolerance,
                "round-trip mismatch: expected {expected}, got {actual}, tolerance {tolerance}"
            );
        }
    }

    #[test]
    fn new_scene_linear_and_log_inputs_round_trip_through_standard_working_space() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        for source in [
            ColorSpace::LinearRec709,
            ColorSpace::LinearRec2020,
            ColorSpace::LinearP3D65,
            ColorSpace::Aces2065_1,
            ColorSpace::AcesCg,
            ColorSpace::AcesCct,
            ColorSpace::SonySLog2SGamut,
        ] {
            let original = [0.18, 0.42, 0.73, 0.375];
            let mut samples = original;
            apply_ocio_identity_float(
                &mut samples,
                OcioColorSpaceIdentity::Color(source),
                OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            )
            .unwrap_or_else(|error| panic!("{source:?} input processor failed: {error}"));
            assert!(samples[..3].iter().all(|channel| channel.is_finite()));
            assert_eq!(samples[3], original[3], "{source:?} input changed alpha");

            apply_ocio_identity_float(
                &mut samples,
                OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
                OcioColorSpaceIdentity::Color(source),
            )
            .unwrap_or_else(|error| panic!("{source:?} inverse processor failed: {error}"));

            for (actual, expected) in samples.iter().zip(original) {
                assert!(
                    (actual - expected).abs() <= 3.0e-5,
                    "{source:?} round-trip mismatch: expected {expected}, got {actual}"
                );
            }
        }
    }

    #[test]
    fn mondrian_standard_package_rejects_content_outside_its_versioned_digest() {
        let mut modified = mondrian_default_ocio_config_text().to_owned();
        modified.push('\n');

        let error = validate_mondrian_default_ocio_contract_text(&modified)
            .expect_err("modified Standard package content must fail closed");

        assert!(
            error.issues.iter().any(|issue| {
                issue.contains("SHA-256 mismatch")
                    && issue.contains(mondrian_default_ocio_contract().content_sha256)
            }),
            "unexpected validation issues: {:#?}",
            error.issues
        );
    }

    #[test]
    fn mondrian_standard_sdr_resource_has_pinned_domain_and_resolution() {
        assert_eq!(
            sha256_hex(MONDRIAN_STANDARD_SDR_LUT.as_bytes()),
            MONDRIAN_STANDARD_SDR_LUT_SHA256
        );
        let cube = parse_cube_3d(MONDRIAN_STANDARD_SDR_LUT)
            .expect("pinned Mondrian Standard SDR LUT should parse");
        assert_eq!(cube.edge, MONDRIAN_STANDARD_SDR_LUT_EDGE);
        assert_eq!(cube.domain_min, [0.0, 0.0, 0.0]);
        assert_eq!(cube.domain_max, [1.0, 1.0, 1.0]);
        assert_eq!(cube.values.len(), cube.edge.pow(3) * 3);
        assert!(cube.values.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn cube_parser_rejects_truncated_and_non_finite_payloads() {
        let truncated = "LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n0 0 0\n";
        assert!(parse_cube_3d(truncated)
            .expect_err("truncated cube must fail")
            .contains("requires 24"));

        let non_finite = "LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\nNaN 0 0\n";
        assert!(parse_cube_3d(non_finite)
            .expect_err("non-finite cube value must fail")
            .contains("must be finite"));
    }

    #[test]
    fn mondrian_standard_sdr_view_is_finite_neutral_and_monotonic() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");
        let contract = mondrian_default_ocio_contract();
        let mut samples = (-256..=256)
            .flat_map(|index| {
                let linear = 0.18_f32 * 2.0_f32.powf(index as f32 / 32.0);
                [linear, linear, linear, 1.0]
            })
            .collect::<Vec<_>>();

        apply_ocio_display_identity_float(
            &mut samples,
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            contract.default_display,
            contract.default_view,
        )
        .expect("Mondrian Standard SDR CPU processor");

        let mut previous = f32::NEG_INFINITY;
        for pixel in samples.chunks_exact(4) {
            assert!(pixel.iter().all(|channel| channel.is_finite()));
            let neutral_spread = pixel[..3].iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - pixel[..3].iter().copied().fold(f32::INFINITY, f32::min);
            assert!(
                neutral_spread <= 2.0e-4,
                "neutral axis spread {neutral_spread} for {pixel:?}"
            );
            assert!(
                pixel[1] + 1.0e-6 >= previous,
                "tone reversal: previous {previous}, current {}",
                pixel[1]
            );
            previous = pixel[1];
        }
    }

    #[test]
    fn mondrian_standard_sdr_view_handles_negative_and_extended_gamut_without_non_finite_output() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");
        let contract = mondrian_default_ocio_contract();
        let mut samples = [
            -1.0, 0.25, 4.0, 1.0, 16.0, -0.5, 0.125, 1.0, 4.0, 0.0, 32.0, 0.5,
        ];

        apply_ocio_display_identity_float(
            &mut samples,
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            contract.default_display,
            contract.default_view,
        )
        .expect("Mondrian Standard extended-range SDR CPU processor");

        assert!(samples.iter().all(|channel| channel.is_finite()));
        assert_eq!(samples[3], 1.0);
        assert_eq!(samples[7], 1.0);
        assert_eq!(samples[11], 0.5);
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
        let config = build_mondrian_default_ocio_config(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO package should build");
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
    fn pinned_aces_mode_presets_exist_in_the_bundled_ocio_registry() {
        let available = builtin_config_names();
        for preset in [
            crate::types::AcesConfigPreset::StudioV4Aces2Ocio25,
            crate::types::AcesConfigPreset::CgV4Aces2Ocio25,
        ] {
            assert!(
                available.iter().any(|name| name == preset.builtin_name()),
                "missing pinned ACES preset '{}' in bundled OCIO registry; available={available:?}",
                preset.builtin_name()
            );
        }
    }

    #[test]
    fn mondrian_default_contract_validation_checks_cpu_and_gpu_processors() {
        let contract = mondrian_default_ocio_contract();
        let report = validate_mondrian_default_ocio_contract()
            .unwrap_or_else(|err| panic!("default OCIO contract errors: {:#?}", err.issues));

        assert_eq!(report.standard_version, contract.standard_version);
        assert_eq!(report.config_name, contract.config_name);
        assert_eq!(report.content_sha256, contract.content_sha256);
        assert_eq!(report.package_sha256, contract.package_sha256);
        assert_eq!(report.resources_checked, contract.resources.len());
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
        let config = build_mondrian_default_ocio_config(mondrian_default_ocio_config_text())
            .expect("embedded Mondrian OCIO package should build");

        let processor_pairs = [
            (ColorSpace::Rec709, ColorSpace::Srgb),
            (ColorSpace::Srgb, ColorSpace::Rec709),
            (ColorSpace::Rec2020, ColorSpace::Rec709),
            (ColorSpace::Rec2100Pq, ColorSpace::Rec709),
            (ColorSpace::Rec2100Hlg, ColorSpace::Rec709),
            (ColorSpace::DisplayP3, ColorSpace::Rec709),
            (ColorSpace::LinearRec709, ColorSpace::Rec709),
            (ColorSpace::LinearRec2020, ColorSpace::Rec709),
            (ColorSpace::LinearP3D65, ColorSpace::Rec709),
            (ColorSpace::Aces2065_1, ColorSpace::Rec709),
            (ColorSpace::AcesCg, ColorSpace::Rec709),
            (ColorSpace::AcesCct, ColorSpace::Rec709),
            (ColorSpace::AppleLogBt2020, ColorSpace::Rec709),
            (ColorSpace::SonySLog2SGamut, ColorSpace::Rec709),
            (ColorSpace::SonySLog3SGamut3, ColorSpace::Rec709),
            (ColorSpace::SonySLog3SGamut3Cine, ColorSpace::Rec709),
            (ColorSpace::ArriLogC3WideGamut3, ColorSpace::Rec709),
            (ColorSpace::ArriLogC4WideGamut4, ColorSpace::Rec709),
            (ColorSpace::CanonLog2CinemaGamutD55, ColorSpace::Rec709),
            (ColorSpace::CanonLog3CinemaGamutD55, ColorSpace::Rec709),
            (ColorSpace::PanasonicVLogVGamut, ColorSpace::Rec709),
            (ColorSpace::RedLog3G10WideGamutRgb, ColorSpace::Rec709),
            (ColorSpace::BlackmagicFilmWideGamutGen5, ColorSpace::Rec709),
            (ColorSpace::DjiDLogDGamut, ColorSpace::Rec709),
            (ColorSpace::DavinciIntermediateWideGamut, ColorSpace::Rec709),
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
    fn standard_output_targets_resolve_matching_sdr_displays_and_reject_unfinished_hdr_views() {
        ensure_mondrian_default_ocio_loaded().expect("Standard package");

        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::Srgb).expect("sRGB target"),
            (
                "sRGB - Display".to_owned(),
                MONDRIAN_STANDARD_SDR_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::Rec709).expect("Rec.709 target"),
            (
                "Rec.1886 Rec.709 - Display".to_owned(),
                MONDRIAN_STANDARD_SDR_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::DisplayP3).expect("P3 target"),
            (
                "Display P3 - Display".to_owned(),
                MONDRIAN_STANDARD_SDR_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_output_display_name(ColorSpace::Rec2100Hlg)
                .expect("HLG target display"),
            "Rec.2100-HLG - Display"
        );
        assert_eq!(
            mondrian_standard_output_display_name(ColorSpace::Rec2100Pq)
                .expect("PQ target display"),
            "Rec.2100-PQ - Display"
        );
        assert!(mondrian_standard_output_display_view(ColorSpace::Rec2100Hlg).is_err());
        assert!(mondrian_standard_output_display_view(ColorSpace::Rec2100Pq).is_err());
    }

    #[test]
    fn standard_mode_extracts_gpu_shader_bundle() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        let bundle = extract_ocio_gpu_shader_bundle(
            ColorSpace::SonySLog3SGamut3Cine,
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
    fn standard_mode_extracts_explicit_working_identity_gpu_shader_bundle() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        let bundle = extract_ocio_identity_gpu_shader_bundle(
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
            OcioColorSpaceIdentity::Color(ColorSpace::Srgb),
            GpuLanguage::Glsl4_0,
        )
        .expect("linear working identity should produce a GPU shader bundle");

        assert_eq!(bundle.src_color_space, "Linear Rec.709 (sRGB)");
        assert_eq!(bundle.dst_color_space, "sRGB Encoded Rec.709 (sRGB)");
        assert!(bundle.shader_text.contains("mondrian_ocio_main"));
    }

    #[test]
    fn mondrian_standard_sdr_endpoints_are_analytic_gpu_programs() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        let to_target_linear = extract_ocio_identity_gpu_shader_bundle(
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
            GpuLanguage::Glsl4_0,
        )
        .expect("working to target-linear endpoint should produce a GPU program");
        let to_encoded_output = extract_ocio_identity_gpu_shader_bundle(
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
            OcioColorSpaceIdentity::Color(ColorSpace::Srgb),
            GpuLanguage::Glsl4_0,
        )
        .expect("target-linear to encoded endpoint should produce a GPU program");

        for bundle in [&to_target_linear, &to_encoded_output] {
            assert_eq!(bundle.texture_2d_count, 0);
            assert_eq!(bundle.texture_3d_count, 0);
            assert_eq!(bundle.uniform_count, 0);
            assert_eq!(bundle.uniform_buffer_size, 0);
            assert!(bundle.textures_2d.is_empty());
            assert!(bundle.textures_3d.is_empty());
            assert!(bundle.uniforms.is_empty());
        }
    }

    #[test]
    fn standard_mode_applies_explicit_working_identity_on_cpu() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");
        let mut rgba = [0.18, 0.18, 0.18, 1.0];

        apply_ocio_identity_float(
            &mut rgba,
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
            OcioColorSpaceIdentity::Color(ColorSpace::Srgb),
        )
        .expect("linear working identity should produce a CPU processor");

        assert!(rgba[..3].iter().all(|channel| *channel > 0.18));
        assert_eq!(rgba[3], 1.0);
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
            format!("sRGB - Display/{MONDRIAN_STANDARD_SDR_VIEW_NAME}")
        );
        assert!(bundle.shader_text.contains("mondrian_ocio_main"));
        assert!(bundle.cache_id.as_deref().is_some_and(|id| !id.trim().is_empty()));
        assert_eq!(bundle.texture_3d_count, 1);
        assert_eq!(bundle.textures_3d.len(), 1);
        assert_eq!(
            bundle.textures_3d[0].edge_len,
            MONDRIAN_STANDARD_SDR_LUT_EDGE as u32
        );
        // OCIO implements tetrahedral reconstruction in generated shader code
        // and requests nearest texel reads for the underlying 3D texture.
        assert_eq!(
            bundle.textures_3d[0].interpolation,
            OcioGpuTextureInterpolation::Nearest
        );
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
