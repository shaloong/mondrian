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
//! 1. **MondrianStandard** — an exact immutable Mondrian Standard package
//! 2. **Builtin** — named built-in config (e.g. `"aces_1.2"`)
//! 3. **Path** — explicit `config.ocio` file path
//! 4. **Environment** — explicit `$OCIO` env var

use crate::types::{
    ColorEngine, ColorSpace, CustomOcioLookIdentity, CustomOcioProjectIdentity,
    CustomOcioRoleIdentity, MondrianStandardPackageIdentity, MondrianStandardVersion,
    OcioColorSpaceIdentity, OcioConfigSource, WorkingColorSpace,
};
use lru::LruCache;
pub use ocio_rs::GpuLanguage;
use ocio_rs::{
    grading::GradingCurvePoint,
    transform::{
        AllocationTransform, BuiltinTransform, ColorSpaceTransform, FixedFunctionTransform,
        GradingRGBCurveTransform, GroupTransform, Lut1DTransform, Lut3DTransform, MatrixTransform,
        RangeTransform,
    },
    Allocation, BuiltinConfigRegistry, CPUProcessor, Config, FixedFunctionStyle, GpuShaderDesc,
    GpuTextureChannel as OcioRsGpuTextureChannel,
    GpuTextureDimensions as OcioRsGpuTextureDimensions, GpuUniformType as OcioRsGpuUniformType,
    GpuUniformValue as OcioRsGpuUniformValue, GradingStyle, Interpolation as OcioRsInterpolation,
    RGBCurveType, RangeStyle, ReferenceSpaceType, TransformDirection, ViewTransform,
    ViewTransformDirection,
};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Full Custom project identity last validated against the loaded config.
    validated_custom_identity: Option<CustomOcioProjectIdentity>,
    /// Monotonically increasing generation counter. Incremented on every
    /// config load. Callers can use this to detect config changes for cache
    /// invalidation without holding the lock.
    generation: u64,
}

static OCIO_STATE: std::sync::Mutex<OcioGlobalState> = std::sync::Mutex::new(OcioGlobalState {
    path: None,
    source: None,
    validated_custom_identity: None,
    generation: 0,
});

/// Serializes config selection with processor/shader construction.
///
/// The lease is intentionally released before CPU pixel application or GPU
/// execution. OCIO processors are baked objects; only their construction must
/// observe one exact process-global config.
static OCIO_CONFIG_OPERATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_ocio_config_operation() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    OCIO_CONFIG_OPERATION
        .lock()
        .map_err(|_| "OCIO config operation lock is poisoned".to_owned())
}

impl OcioGlobalState {
    /// Set the current config atomically: update path, source, increment
    /// generation, and call `ocio_rs::set_current_config`.
    ///
    /// The mutex is held for the entire operation so concurrent
    /// `current_config()` callers cannot see a half-updated state.
    fn set_config(
        &mut self,
        path: PathBuf,
        source: OcioConfigSource,
        config: &Config,
    ) -> Result<(), String> {
        ocio_rs::try_set_current_config(config)
            .map_err(|err| format!("failed to install process-global OCIO config: {err}"))?;
        self.path = Some(path);
        self.source = Some(source);
        self.validated_custom_identity = None;
        self.generation = self.generation.wrapping_add(1);
        Ok(())
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

/// Return the cache revision for the exact config selected by a color engine.
///
/// Immutable embedded/builtin packages are fully identified by their source
/// and therefore use revision zero without touching global OCIO state. Mutable
/// path/environment sources use the selected config generation so renderer
/// shader caches cannot survive a reload of the same source identity.
pub fn ocio_gpu_config_revision_for_engine(engine: &ColorEngine) -> Result<u64, String> {
    let source = engine.ocio_source();
    if matches!(engine, ColorEngine::CustomOcio { .. }) {
        return with_ocio_config_for_engine(engine, |_config, generation| {
            Ok(ocio_cpu_processor_config_revision(&source, generation))
        });
    }
    match source {
        OcioConfigSource::MondrianStandard { .. } | OcioConfigSource::Builtin { .. } => Ok(0),
        OcioConfigSource::Environment | OcioConfigSource::Path { .. } => {
            let _lease = lock_ocio_config_operation()?;
            ensure_ocio_loaded_locked(&source)?;
            OCIO_STATE
                .lock()
                .map(|state| state.generation)
                .map_err(|_| "OCIO global state lock is poisoned".to_owned())
        }
    }
}

/// Return the current config source identity, if any.
pub fn ocio_config_source() -> Option<OcioConfigSource> {
    OCIO_STATE.lock().ok().and_then(|g| g.source.clone())
}

/// Return whether the exact engine identity is loaded and validated.
pub(crate) fn ocio_engine_is_validated(engine: &ColorEngine) -> bool {
    let Ok(state) = OCIO_STATE.lock() else {
        return false;
    };
    if state.source.as_ref() != Some(&engine.ocio_source()) {
        return false;
    }
    match engine {
        ColorEngine::CustomOcio { identity } => {
            state.validated_custom_identity.as_ref() == Some(identity.as_ref())
        }
        ColorEngine::MondrianStandard { .. } | ColorEngine::Aces { .. } => true,
    }
}

/// Intended name for Mondrian's bundled default OCIO config.
pub const MONDRIAN_DEFAULT_OCIO_CONFIG_NAME: &str = "mondrian_default_ocio_v2";

/// SHA-256 digest pinned to the exact OCIO text shipped by Mondrian Standard v2.
pub const MONDRIAN_DEFAULT_OCIO_CONFIG_SHA256: &str =
    MondrianStandardPackageIdentity::V2.config_sha256();
/// SHA-256 over the versioned config text and every embedded Standard resource.
pub const MONDRIAN_DEFAULT_OCIO_PACKAGE_SHA256: &str =
    MondrianStandardPackageIdentity::V3.package_sha256();

const MONDRIAN_STANDARD_V2_OCIO_VIRTUAL_PATH: &str = "embedded:mondrian_default_ocio_v2";
const MONDRIAN_STANDARD_V3_OCIO_VIRTUAL_PATH: &str =
    "embedded:mondrian_default_ocio_v2:mondrian_standard_v3";
const MONDRIAN_DEFAULT_OCIO_CONFIG: &str =
    include_str!("../assets/ocio/mondrian_default_ocio_v2.ocio");

const MONDRIAN_STANDARD_SDR_VIEW_NAME: &str = "Mondrian Standard SDR v1";
const MONDRIAN_STANDARD_SDR_V2_VIEW_NAME: &str = "Mondrian Standard SDR v2";
const MONDRIAN_STANDARD_SDR_V2_SRGB_TRANSFORM_NAME: &str = "Mondrian Standard SDR v2 - sRGB";
const MONDRIAN_STANDARD_SDR_V2_REC709_TRANSFORM_NAME: &str = "Mondrian Standard SDR v2 - Rec.709";
const MONDRIAN_STANDARD_SDR_V2_P3_TRANSFORM_NAME: &str = "Mondrian Standard SDR v2 - Display P3";
const MONDRIAN_STANDARD_SDR_V2_REC2020_TRANSFORM_NAME: &str = "Mondrian Standard SDR v2 - Rec.2020";
const MONDRIAN_STANDARD_HDR_1000_VIEW_NAME: &str = "Mondrian Standard HDR 1000 nits v1";
const MONDRIAN_STANDARD_SDR_LUT_NAME: &str = "mondrian_standard_sdr_rec709_v1.cube";
const MONDRIAN_STANDARD_SDR_LUT_SHA256: &str =
    "02f4d185608daa67fda01a1a48529bbc1533c8afdc826cde5c78f2eb5bb1b839";
const MONDRIAN_STANDARD_SDR_LUT: &str =
    include_str!("../assets/ocio/mondrian_standard_sdr_rec709_v1.cube");
const MONDRIAN_STANDARD_SDR_LUT_EDGE: usize = 57;
const MONDRIAN_STANDARD_HDR_1000_LUT_NAME: &str = "mondrian_standard_hdr_1000_p3_v1.cube";
const MONDRIAN_STANDARD_HDR_1000_LUT_SHA256: &str =
    "80238cf1ed300a377d8c3b30bd3d05f23e7e2065b7e973ea62a5ee0e4f7a8a79";
const MONDRIAN_STANDARD_HDR_1000_LUT: &str =
    include_str!("../assets/ocio/mondrian_standard_hdr_1000_p3_v1.cube");
const MONDRIAN_STANDARD_HDR_1000_LUT_EDGE: usize = 57;
const MONDRIAN_STANDARD_V2_ASSEMBLY_MANIFEST: &str = concat!(
    "mondrian-standard-assembly-v2\n",
    "working=Linear Rec.2020\n",
    "view=Mondrian Standard SDR v1\n",
    "scene_reference=UTILITY - ACES-AP0_to_CIE-XYZ-D65_BFD\n",
    "formation_gamut=FilmLight E-Gamut\n",
    "shaper=log2[-12.47393,12.5260688117]\n",
    "formation_lut=mondrian_standard_sdr_rec709_v1.cube;edge=57;interpolation=tetrahedral\n",
    "hdr_view=Mondrian Standard HDR 1000 nits v1\n",
    "hdr_formation_lut=mondrian_standard_hdr_1000_p3_v1.cube;edge=57;interpolation=tetrahedral;peak_nits=1000;reference_white_nits=100;limit=P3-D65\n",
    "display_reference=CIE XYZ-D65 - Display-referred\n",
    "displays=sRGB - Display,Gamma 2.2 Rec.709 - Display,Rec.1886 Rec.709 - Display,Rec.2020 SDR - Display,Display P3 - Display,Rec.2100-HLG - Display,Rec.2100-PQ - Display\n",
);
const MONDRIAN_STANDARD_V3_ASSEMBLY_MANIFEST: &str = concat!(
    "mondrian-standard-assembly-v3\n",
    "working=Linear Rec.2020\n",
    "sdr_view=Mondrian Standard SDR v2\n",
    "sdr_method=target-linear;HSV-value-shoulder-and-saturation-containment;range-safety;target-signal-encoding\n",
    "sdr_targets=sRGB,Rec.709,Display P3,Rec.2020 SDR\n",
    "sdr_tone_curve=ocio-grading-rgb-curve-baked-to-1d;edge=4096;domain=V[0,4];interpolation=linear;points=(-16,-16,1);(0,0,1);(.75,.75,1);(.9,.9,1);(.98,.98,1);(1,.995,.4);(2,.9995,.005);(16,1,0)\n",
    "sdr_gamut_surface=HSV(H identity,S soft-capped by V);domain=H[0,1],S[0,1.25],V[0,4];edge=61;interpolation=tetrahedral\n",
    "hdr_view=Mondrian Standard HDR 1000 nits v1\n",
    "hdr_formation_lut=mondrian_standard_hdr_1000_p3_v1.cube;edge=57;interpolation=tetrahedral;peak_nits=1000;reference_white_nits=100;limit=P3-D65\n",
    "display_reference=CIE XYZ-D65 - Display-referred\n",
    "displays=sRGB - Display,Rec.1886 Rec.709 - Display,Rec.2020 SDR - Display,Display P3 - Display,Rec.2100-HLG - Display,Rec.2100-PQ - Display\n",
);
const MONDRIAN_STANDARD_SDR_ALLOCATION_VARS: [f32; 2] = [-12.47393, 12.526_069];
const MONDRIAN_STANDARD_SDR_V2_GAMUT_LUT_EDGE: usize = 61;
const MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE: usize = 4096;
const MONDRIAN_STANDARD_SDR_V2_SATURATION_DOMAIN_MAX: f64 = 1.25;
const MONDRIAN_STANDARD_SDR_V2_VALUE_DOMAIN_MAX: f64 = 4.0;

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

/// Versioned program-output contract resolved by Mondrian Standard.
///
/// The OCIO display/view defines pixel semantics; luminance and gamut fields
/// bind delivery metadata and diagnostics to that same immutable View.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MondrianStandardOutputTargetContract {
    /// Encoded program-output color space.
    pub output_color_space: ColorSpace,
    /// Pinned OCIO display.
    pub display: &'static str,
    /// Pinned OCIO View Transform.
    pub view: &'static str,
    /// Stable versioned identity of the rendering transform behind the View.
    pub view_transform_id: &'static str,
    /// Canonical encoded primaries/transfer/matrix contract.
    pub encoding: crate::ColorEncodingSpec,
    /// Gamut limit authored into the rendering transform.
    pub rendering_gamut_limit: crate::ColorPrimaries,
    /// Diffuse/reference white used by the rendering transform, in cd/m².
    pub reference_white_nits: u32,
    /// Nominal peak represented by the rendering transform, in cd/m².
    pub nominal_peak_nits: u32,
    /// Nominal black level in thousandths of a cd/m².
    pub black_level_millinits: u32,
}

impl MondrianStandardOutputTargetContract {
    /// Returns true when this target is display-referred HDR.
    pub fn is_hdr(self) -> bool {
        self.encoding.is_hdr()
    }
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

const MONDRIAN_STANDARD_V2_OCIO_DISPLAY_VIEWS: [MondrianDefaultOcioDisplayView; 7] = [
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
        display: "Rec.2020 SDR - Display",
        view: MONDRIAN_STANDARD_SDR_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "Display P3 - Display",
        view: MONDRIAN_STANDARD_SDR_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.2100-HLG - Display",
        view: MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.2100-PQ - Display",
        view: MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
    },
];

const MONDRIAN_STANDARD_V3_OCIO_DISPLAY_VIEWS: [MondrianDefaultOcioDisplayView; 7] = [
    MondrianDefaultOcioDisplayView {
        display: "sRGB - Display",
        view: MONDRIAN_STANDARD_SDR_V2_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "sRGB - Display",
        view: "Video (colorimetric)",
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.1886 Rec.709 - Display",
        view: MONDRIAN_STANDARD_SDR_V2_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.2020 SDR - Display",
        view: MONDRIAN_STANDARD_SDR_V2_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "Display P3 - Display",
        view: MONDRIAN_STANDARD_SDR_V2_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.2100-HLG - Display",
        view: MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
    },
    MondrianDefaultOcioDisplayView {
        display: "Rec.2100-PQ - Display",
        view: MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
    },
];

const MONDRIAN_STANDARD_V2_OCIO_RESOURCES: [MondrianDefaultOcioResource; 2] = [
    MondrianDefaultOcioResource {
        name: MONDRIAN_STANDARD_SDR_LUT_NAME,
        content_sha256: MONDRIAN_STANDARD_SDR_LUT_SHA256,
    },
    MondrianDefaultOcioResource {
        name: MONDRIAN_STANDARD_HDR_1000_LUT_NAME,
        content_sha256: MONDRIAN_STANDARD_HDR_1000_LUT_SHA256,
    },
];

const MONDRIAN_STANDARD_V3_OCIO_RESOURCES: [MondrianDefaultOcioResource; 1] =
    [MondrianDefaultOcioResource {
        name: MONDRIAN_STANDARD_HDR_1000_LUT_NAME,
        content_sha256: MONDRIAN_STANDARD_HDR_1000_LUT_SHA256,
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
    mondrian_standard_ocio_contract(MondrianStandardPackageIdentity::V3)
        .expect("current Mondrian Standard package identity is valid")
}

fn mondrian_standard_ocio_contract(
    package: MondrianStandardPackageIdentity,
) -> Result<MondrianDefaultOcioContract, String> {
    let (standard_version, package_sha256, virtual_path, default_view, display_views, resources) =
        if package == MondrianStandardPackageIdentity::V2 {
            (
                MondrianStandardVersion::V2,
                MondrianStandardPackageIdentity::V2.package_sha256(),
                MONDRIAN_STANDARD_V2_OCIO_VIRTUAL_PATH,
                MONDRIAN_STANDARD_SDR_VIEW_NAME,
                MONDRIAN_STANDARD_V2_OCIO_DISPLAY_VIEWS.as_slice(),
                MONDRIAN_STANDARD_V2_OCIO_RESOURCES.as_slice(),
            )
        } else if package == MondrianStandardPackageIdentity::V3 {
            (
                MondrianStandardVersion::V3,
                MondrianStandardPackageIdentity::V3.package_sha256(),
                MONDRIAN_STANDARD_V3_OCIO_VIRTUAL_PATH,
                MONDRIAN_STANDARD_SDR_V2_VIEW_NAME,
                MONDRIAN_STANDARD_V3_OCIO_DISPLAY_VIEWS.as_slice(),
                MONDRIAN_STANDARD_V3_OCIO_RESOURCES.as_slice(),
            )
        } else {
            return Err(format!(
                "unsupported or internally inconsistent Mondrian Standard package identity with SHA-256 '{}'",
                package.package_sha256()
            ));
        };
    Ok(MondrianDefaultOcioContract {
        standard_version,
        config_name: MONDRIAN_DEFAULT_OCIO_CONFIG_NAME,
        content_sha256: MONDRIAN_DEFAULT_OCIO_CONFIG_SHA256,
        package_sha256,
        virtual_path,
        default_display: "sRGB - Display",
        default_view,
        scene_linear_role: "Linear Rec.2020",
        working_space: WorkingColorSpace::LinearRec2020,
        color_spaces: &MONDRIAN_DEFAULT_OCIO_COLOR_SPACES,
        display_views,
        resources,
    })
}

/// Validate the embedded Mondrian default OCIO config against its product contract.
///
/// This is the production gate for `mondrian_default_ocio_v2`: it parses the
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
    let config = match build_mondrian_default_ocio_config(
        config_text,
        MondrianStandardPackageIdentity::V3,
    ) {
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

fn finish_sha256_hex(digest: Sha256) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = digest.finalize();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
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
    let assembly_manifest = match contract.standard_version {
        MondrianStandardVersion::V2 => MONDRIAN_STANDARD_V2_ASSEMBLY_MANIFEST,
        MondrianStandardVersion::V3 => MONDRIAN_STANDARD_V3_ASSEMBLY_MANIFEST,
        MondrianStandardVersion::V1 => {
            return Err("Mondrian Standard v1 has no assembled OCIO package".to_owned());
        }
    };
    digest.update(assembly_manifest.as_bytes());
    digest.update([0]);
    for resource in contract.resources {
        digest.update(resource.name.as_bytes());
        digest.update([0]);
        let bytes = match resource.name {
            MONDRIAN_STANDARD_SDR_LUT_NAME => MONDRIAN_STANDARD_SDR_LUT.as_bytes(),
            MONDRIAN_STANDARD_HDR_1000_LUT_NAME => MONDRIAN_STANDARD_HDR_1000_LUT.as_bytes(),
            unknown => {
                return Err(format!(
                    "Mondrian Standard package digest has no bytes for resource '{unknown}'"
                ));
            }
        };
        digest.update(bytes);
        digest.update([0]);
    }
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
            MONDRIAN_STANDARD_HDR_1000_LUT_NAME
                if resource.content_sha256 == MONDRIAN_STANDARD_HDR_1000_LUT_SHA256
                    && sha256_hex(MONDRIAN_STANDARD_HDR_1000_LUT.as_bytes())
                        == resource.content_sha256 =>
            {
                report.resources_checked += 1;
            }
            MONDRIAN_STANDARD_HDR_1000_LUT_NAME => errors.push(format!(
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
    xyz_to_egamut
        .set_matrix(&XYZ_D65_TO_FILMLIGHT_E_GAMUT)
        .map_err(|err| format!("Mondrian Standard SDR gamut matrix: {err}"))?;

    let allocation = AllocationTransform::create().map_err(|err| err.to_string())?;
    allocation.set_allocation(Allocation::Lg2);
    allocation
        .set_vars(&MONDRIAN_STANDARD_SDR_ALLOCATION_VARS)
        .map_err(|err| format!("Mondrian Standard SDR allocation variables: {err}"))?;

    let formation = Lut3DTransform::create().map_err(|err| err.to_string())?;
    formation
        .set_grid_size(cube.edge as u64)
        .map_err(|err| format!("Mondrian Standard SDR LUT grid size: {err}"))?;
    formation.set_interpolation(OcioRsInterpolation::Tetrahedral);
    formation
        .set_values(&cube.values)
        .map_err(|err| format!("Mondrian Standard SDR LUT values: {err}"))?;

    let to_display_reference = ColorSpaceTransform::create().map_err(|err| err.to_string())?;
    to_display_reference
        .set_src("Rec.1886 Rec.709 - Display")
        .map_err(|err| err.to_string())?;
    to_display_reference
        .set_dst("CIE XYZ-D65 - Display-referred")
        .map_err(|err| err.to_string())?;

    let group = GroupTransform::create().map_err(|err| err.to_string())?;
    group
        .append_transform(&ap0_to_xyz_d65)
        .map_err(|err| format!("Mondrian Standard SDR reference transform: {err}"))?;
    group
        .append_transform(&xyz_to_egamut)
        .map_err(|err| format!("Mondrian Standard SDR gamut transform: {err}"))?;
    group
        .append_transform(&allocation)
        .map_err(|err| format!("Mondrian Standard SDR allocation transform: {err}"))?;
    group
        .append_transform(&formation)
        .map_err(|err| format!("Mondrian Standard SDR formation transform: {err}"))?;
    group
        .append_transform(&to_display_reference)
        .map_err(|err| format!("Mondrian Standard SDR display transform: {err}"))?;

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
        "Rec.2020 SDR - Display",
        "Display P3 - Display",
    ] {
        config
            .add_display_shared_view(display, MONDRIAN_STANDARD_SDR_VIEW_NAME)
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn build_mondrian_standard_hdr_1000_view(config: &Config) -> Result<(), String> {
    build_mondrian_standard_hdr_1000_view_with_interpolation(
        config,
        MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
        OcioRsInterpolation::Tetrahedral,
    )
}

fn build_mondrian_standard_hdr_1000_view_with_interpolation(
    config: &Config,
    view_name: &str,
    interpolation: OcioRsInterpolation,
) -> Result<(), String> {
    let actual_digest = sha256_hex(MONDRIAN_STANDARD_HDR_1000_LUT.as_bytes());
    if actual_digest != MONDRIAN_STANDARD_HDR_1000_LUT_SHA256 {
        return Err(format!(
            "embedded Mondrian Standard resource '{MONDRIAN_STANDARD_HDR_1000_LUT_NAME}' failed integrity validation: expected SHA-256 '{MONDRIAN_STANDARD_HDR_1000_LUT_SHA256}', got '{actual_digest}'"
        ));
    }
    let cube = parse_cube_3d(MONDRIAN_STANDARD_HDR_1000_LUT)
        .map_err(|err| format!("failed to parse '{MONDRIAN_STANDARD_HDR_1000_LUT_NAME}': {err}"))?;
    if cube.edge != MONDRIAN_STANDARD_HDR_1000_LUT_EDGE {
        return Err(format!(
            "'{MONDRIAN_STANDARD_HDR_1000_LUT_NAME}' edge mismatch: expected {MONDRIAN_STANDARD_HDR_1000_LUT_EDGE}, got {}",
            cube.edge
        ));
    }

    let ap0_to_xyz_d65 = BuiltinTransform::create().map_err(|err| err.to_string())?;
    ap0_to_xyz_d65
        .set_style("UTILITY - ACES-AP0_to_CIE-XYZ-D65_BFD")
        .map_err(|err| err.to_string())?;

    let xyz_to_egamut = MatrixTransform::create().map_err(|err| err.to_string())?;
    xyz_to_egamut
        .set_matrix(&XYZ_D65_TO_FILMLIGHT_E_GAMUT)
        .map_err(|err| format!("Mondrian Standard HDR gamut matrix: {err}"))?;

    let allocation = AllocationTransform::create().map_err(|err| err.to_string())?;
    allocation.set_allocation(Allocation::Lg2);
    allocation
        .set_vars(&MONDRIAN_STANDARD_SDR_ALLOCATION_VARS)
        .map_err(|err| format!("Mondrian Standard HDR allocation variables: {err}"))?;

    let formation = Lut3DTransform::create().map_err(|err| err.to_string())?;
    formation
        .set_grid_size(cube.edge as u64)
        .map_err(|err| format!("Mondrian Standard HDR LUT grid size: {err}"))?;
    formation.set_interpolation(interpolation);
    formation
        .set_values(&cube.values)
        .map_err(|err| format!("Mondrian Standard HDR LUT values: {err}"))?;

    // The pinned formation resource defines a 1000-nit, P3-limited HDR image
    // in Rec.2100 HLG encoding. Decode that image back to OCIO's common
    // display-reference XYZ so the selected display color space performs the
    // one and only final HLG or PQ encoding.
    let to_display_reference = ColorSpaceTransform::create().map_err(|err| err.to_string())?;
    to_display_reference
        .set_src("Rec.2100-HLG - Display")
        .map_err(|err| err.to_string())?;
    to_display_reference
        .set_dst("CIE XYZ-D65 - Display-referred")
        .map_err(|err| err.to_string())?;

    let group = GroupTransform::create().map_err(|err| err.to_string())?;
    group
        .append_transform(&ap0_to_xyz_d65)
        .map_err(|err| format!("Mondrian Standard HDR reference transform: {err}"))?;
    group
        .append_transform(&xyz_to_egamut)
        .map_err(|err| format!("Mondrian Standard HDR gamut transform: {err}"))?;
    group
        .append_transform(&allocation)
        .map_err(|err| format!("Mondrian Standard HDR allocation transform: {err}"))?;
    group
        .append_transform(&formation)
        .map_err(|err| format!("Mondrian Standard HDR formation transform: {err}"))?;
    group
        .append_transform(&to_display_reference)
        .map_err(|err| format!("Mondrian Standard HDR display transform: {err}"))?;

    let view = ViewTransform::create(ReferenceSpaceType::Scene).map_err(|err| err.to_string())?;
    view.set_name(view_name).map_err(|err| err.to_string())?;
    view.set_family("Mondrian Standard").map_err(|err| err.to_string())?;
    view.set_description(
        "Mondrian Standard v1 scene-to-HDR formation: AP0 reference to FilmLight E-Gamut, log2 shaper, pinned 1000-nit P3-limited AgX formation LUT, then target display encoding.",
    )
    .map_err(|err| err.to_string())?;
    view.set_transform(Some(&group), ViewTransformDirection::FromReference);
    config.add_view_transform(&view);

    config
        .add_shared_view(
            view_name,
            view_name,
            "<USE_DISPLAY_NAME>",
            "",
            "Any Scene-linear or Log",
            "Mondrian Standard v1 1000-nit HDR rendering transform",
        )
        .map_err(|err| err.to_string())?;
    for display in ["Rec.2100-HLG - Display", "Rec.2100-PQ - Display"] {
        config
            .add_display_shared_view(display, view_name)
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct StandardSdrTarget {
    display: &'static str,
    view_transform: &'static str,
    target_linear: &'static str,
    target_signal: &'static str,
    target_display_encoding: &'static str,
}

const STANDARD_SDR_TARGETS: [StandardSdrTarget; 4] = [
    StandardSdrTarget {
        display: "sRGB - Display",
        view_transform: MONDRIAN_STANDARD_SDR_V2_SRGB_TRANSFORM_NAME,
        target_linear: "Linear Rec.709 (sRGB)",
        target_signal: "sRGB Encoded Rec.709 (sRGB)",
        target_display_encoding: "sRGB - Display",
    },
    StandardSdrTarget {
        display: "Rec.1886 Rec.709 - Display",
        view_transform: MONDRIAN_STANDARD_SDR_V2_REC709_TRANSFORM_NAME,
        target_linear: "Linear Rec.709 (sRGB)",
        target_signal: "Camera Rec.709",
        target_display_encoding: "Rec.1886 Rec.709 - Display",
    },
    StandardSdrTarget {
        display: "Display P3 - Display",
        view_transform: MONDRIAN_STANDARD_SDR_V2_P3_TRANSFORM_NAME,
        target_linear: "Linear P3-D65",
        target_signal: "sRGB Encoded P3-D65",
        target_display_encoding: "Display P3 - Display",
    },
    StandardSdrTarget {
        display: "Rec.2020 SDR - Display",
        view_transform: MONDRIAN_STANDARD_SDR_V2_REC2020_TRANSFORM_NAME,
        target_linear: "Linear Rec.2020",
        target_signal: "Camera Rec.2020",
        target_display_encoding: "Rec.2020 SDR - Display",
    },
];

fn grading_curve(points: &[(f32, f32, f32)]) -> Vec<GradingCurvePoint> {
    points
        .iter()
        .map(|&(x, y, slope)| GradingCurvePoint::new(x, y, slope))
        .collect()
}

fn set_grading_curve(
    transform: &GradingRGBCurveTransform,
    curve_type: RGBCurveType,
    points: &[GradingCurvePoint],
) -> Result<(), String> {
    // ocio-rs validates the whole curve on every mutation. Normalize the
    // existing points first, then populate new points from high to low so a
    // growing curve remains ordered throughout the operation.
    let current = transform.num_control_points(curve_type).map_err(|err| err.to_string())?;
    for index in 0..current {
        transform
            .set_control_point(curve_type, index, -16.0, -16.0)
            .map_err(|err| err.to_string())?;
    }
    transform
        .set_num_control_points(curve_type, points.len() as i32)
        .map_err(|err| err.to_string())?;
    for (index, point) in points.iter().enumerate().rev() {
        transform
            .set_control_point(curve_type, index as i32, point.x, point.y)
            .map_err(|err| err.to_string())?;
    }
    for (index, point) in points.iter().enumerate() {
        transform
            .set_slope(curve_type, index as i32, point.slope)
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn color_space_transform(src: &str, dst: &str) -> Result<ColorSpaceTransform, String> {
    let transform = ColorSpaceTransform::create().map_err(|err| err.to_string())?;
    transform.set_src(src).map_err(|err| err.to_string())?;
    transform.set_dst(dst).map_err(|err| err.to_string())?;
    Ok(transform)
}

fn standard_sdr_v2_compressed_saturation(saturation: f64, value: f64) -> f64 {
    let excursion = ((value - 1.0) / 1.0).clamp(0.0, 1.0);
    let excursion = excursion * excursion * (3.0 - 2.0 * excursion);
    let cap = 1.0 - 0.092 * excursion;
    let width = 0.0005 + 0.0295 * excursion;
    let knee = cap - width;
    if saturation <= knee {
        saturation
    } else {
        knee + width * (1.0 - (-(saturation - knee) / width).exp())
    }
}

fn build_standard_sdr_v2_gamut_surface() -> Result<(MatrixTransform, Lut3DTransform), String> {
    let normalize = MatrixTransform::create().map_err(|err| err.to_string())?;
    normalize
        .set_matrix(&[
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0 / MONDRIAN_STANDARD_SDR_V2_SATURATION_DOMAIN_MAX,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0 / MONDRIAN_STANDARD_SDR_V2_VALUE_DOMAIN_MAX,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
        .map_err(|err| format!("Mondrian Standard SDR v2 gamut domain: {err}"))?;

    let lut = Lut3DTransform::create().map_err(|err| err.to_string())?;
    lut.set_grid_size(MONDRIAN_STANDARD_SDR_V2_GAMUT_LUT_EDGE as u64)
        .map_err(|err| format!("Mondrian Standard SDR v2 gamut surface edge: {err}"))?;
    lut.set_interpolation(OcioRsInterpolation::Tetrahedral);
    let mut values = Vec::with_capacity(MONDRIAN_STANDARD_SDR_V2_GAMUT_LUT_EDGE.pow(3) * 3);
    let denominator = (MONDRIAN_STANDARD_SDR_V2_GAMUT_LUT_EDGE - 1) as f64;
    for value_index in 0..MONDRIAN_STANDARD_SDR_V2_GAMUT_LUT_EDGE {
        let value = value_index as f64 / denominator * MONDRIAN_STANDARD_SDR_V2_VALUE_DOMAIN_MAX;
        for saturation_index in 0..MONDRIAN_STANDARD_SDR_V2_GAMUT_LUT_EDGE {
            let saturation = saturation_index as f64 / denominator
                * MONDRIAN_STANDARD_SDR_V2_SATURATION_DOMAIN_MAX;
            let compressed = standard_sdr_v2_compressed_saturation(saturation, value);
            for hue_index in 0..MONDRIAN_STANDARD_SDR_V2_GAMUT_LUT_EDGE {
                let hue = hue_index as f64 / denominator;
                values.extend_from_slice(&[
                    hue,
                    compressed,
                    value / MONDRIAN_STANDARD_SDR_V2_VALUE_DOMAIN_MAX,
                ]);
            }
        }
    }
    lut.set_values(&values)
        .map_err(|err| format!("Mondrian Standard SDR v2 gamut surface values: {err}"))?;
    Ok((normalize, lut))
}

fn build_standard_sdr_v2_tone_lut(config: &Config) -> Result<Lut1DTransform, String> {
    let identity = grading_curve(&[(-16.0, -16.0, 1.0), (16.0, 16.0, 1.0)]);
    let tone = grading_curve(&[
        (-16.0, -16.0, 1.0),
        (0.0, 0.0, 1.0),
        (0.75, 0.75, 1.0),
        (0.9, 0.9, 1.0),
        (0.98, 0.98, 1.0),
        (1.0, 0.995, 0.4),
        (2.0, 0.9995, 0.005),
        (16.0, 1.0, 0.0),
    ]);
    let grading =
        GradingRGBCurveTransform::create(GradingStyle::Lin).map_err(|err| err.to_string())?;
    grading.try_set_bypass_lin_to_log(true).map_err(|err| err.to_string())?;
    set_grading_curve(&grading, RGBCurveType::Red, &identity)?;
    set_grading_curve(&grading, RGBCurveType::Green, &identity)?;
    set_grading_curve(&grading, RGBCurveType::Blue, &tone)?;
    set_grading_curve(&grading, RGBCurveType::Master, &identity)?;

    let denominator = (MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE - 1) as f32;
    let mut samples = Vec::with_capacity(MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE * 4);
    for index in 0..MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE {
        let normalized = index as f32 / denominator;
        samples.extend_from_slice(&[
            normalized,
            normalized,
            normalized * MONDRIAN_STANDARD_SDR_V2_VALUE_DOMAIN_MAX as f32,
            1.0,
        ]);
    }
    config
        .processor_from_transform(&grading, TransformDirection::Forward)
        .map_err(|err| format!("Mondrian Standard SDR v2 tone bake processor: {err}"))?
        .default_cpu_processor()
        .map_err(|err| format!("Mondrian Standard SDR v2 tone bake CPU processor: {err}"))?
        .try_apply_rgba_pixels(
            &mut samples,
            MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE as i64,
            4,
        )
        .map_err(|err| format!("Mondrian Standard SDR v2 tone bake: {err}"))?;

    let mut values = Vec::with_capacity(MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE * 3);
    for (index, pixel) in samples.chunks_exact(4).enumerate() {
        let normalized = index as f64 / (MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE - 1) as f64;
        values.extend_from_slice(&[normalized, normalized, f64::from(pixel[2])]);
    }
    let lut = Lut1DTransform::create().map_err(|err| err.to_string())?;
    lut.set_length(MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE as u64)
        .map_err(|err| format!("Mondrian Standard SDR v2 tone LUT edge: {err}"))?;
    lut.try_set_interpolation(OcioRsInterpolation::Linear)
        .map_err(|err| format!("Mondrian Standard SDR v2 tone LUT interpolation: {err}"))?;
    lut.set_values(&values)
        .map_err(|err| format!("Mondrian Standard SDR v2 tone LUT values: {err}"))?;
    Ok(lut)
}

fn build_mondrian_standard_sdr_v2_target(
    config: &Config,
    target: StandardSdrTarget,
    tone_lut: &Lut1DTransform,
) -> Result<(), String> {
    let to_target_linear = color_space_transform("ACES2065-1", target.target_linear)?;

    let to_hsv = FixedFunctionTransform::create(FixedFunctionStyle::RgbToHsv)
        .map_err(|err| err.to_string())?;
    let (gamut_normalize, gamut_surface) = build_standard_sdr_v2_gamut_surface()?;
    let from_hsv = FixedFunctionTransform::create(FixedFunctionStyle::RgbToHsv)
        .map_err(|err| err.to_string())?;
    from_hsv.set_direction(TransformDirection::Inverse);

    let output_range = RangeTransform::create().map_err(|err| err.to_string())?;
    output_range.set_style(RangeStyle::Clamp);
    output_range.set_min_in_value(0.0);
    output_range.set_max_in_value(1.0);
    output_range.set_min_out_value(0.0);
    output_range.set_max_out_value(1.0);
    let to_signal = color_space_transform(target.target_linear, target.target_signal)?;
    let to_display_reference = color_space_transform(
        target.target_display_encoding,
        "CIE XYZ-D65 - Display-referred",
    )?;

    let group = GroupTransform::create().map_err(|err| err.to_string())?;
    macro_rules! append {
        ($transform:expr) => {
            group
                .append_transform(&$transform)
                .map_err(|err| format!("{} graph assembly failed: {err}", target.view_transform))?
        };
    }
    append!(to_target_linear);
    append!(to_hsv);
    append!(gamut_normalize);
    append!(gamut_surface);
    group
        .append_transform(tone_lut)
        .map_err(|err| format!("{} graph assembly failed: {err}", target.view_transform))?;
    append!(from_hsv);
    append!(output_range);
    append!(to_signal);
    append!(to_display_reference);

    let view = ViewTransform::create(ReferenceSpaceType::Scene).map_err(|err| err.to_string())?;
    view.set_name(target.view_transform).map_err(|err| err.to_string())?;
    view.set_family("Mondrian Standard").map_err(|err| err.to_string())?;
    view.set_description(
        "Mondrian Standard SDR v2 target-aware scene-to-display transform: preserves normal SDR, applies a luminance shoulder only near output peak, contains target-gamut excursions, and performs one target signal encoding.",
    )
    .map_err(|err| err.to_string())?;
    view.set_transform(Some(&group), ViewTransformDirection::FromReference);
    config.add_view_transform(&view);
    config
        .add_display_view_detailed(
            target.display,
            MONDRIAN_STANDARD_SDR_V2_VIEW_NAME,
            target.view_transform,
            target.display,
            "",
            "Any Scene-linear or Log",
            "Mondrian Standard SDR v2 target-aware rendering transform",
        )
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn build_mondrian_standard_sdr_v2_views(config: &Config) -> Result<(), String> {
    let tone_lut = build_standard_sdr_v2_tone_lut(config)?;
    for target in STANDARD_SDR_TARGETS {
        build_mondrian_standard_sdr_v2_target(config, target, &tone_lut)?;
    }
    Ok(())
}

fn build_mondrian_default_ocio_config(
    config_text: &str,
    package: MondrianStandardPackageIdentity,
) -> Result<Config, String> {
    let config = Config::from_stream(config_text)
        .map_err(|err| format!("failed to parse base Mondrian Standard OCIO config: {err}"))?;
    let sdr_view = if package == MondrianStandardPackageIdentity::V2 {
        build_mondrian_standard_sdr_view(&config)?;
        MONDRIAN_STANDARD_SDR_VIEW_NAME
    } else if package == MondrianStandardPackageIdentity::V3 {
        build_mondrian_standard_sdr_v2_views(&config)?;
        MONDRIAN_STANDARD_SDR_V2_VIEW_NAME
    } else {
        return Err(format!(
            "unsupported or internally inconsistent Mondrian Standard package identity with SHA-256 '{}'",
            package.package_sha256()
        ));
    };
    build_mondrian_standard_hdr_1000_view(&config)?;
    config
        .set_active_views(format!(
            "{sdr_view},{MONDRIAN_STANDARD_HDR_1000_VIEW_NAME},Video (colorimetric),Un-tone-mapped,Raw"
        ))
        .map_err(|err| err.to_string())?;
    config.validate().map_err(|err| {
        format!("Mondrian Standard in-memory OCIO package failed validation: {err}")
    })?;
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
    if let Err(err) = gpu.try_extract_shader_info(&mut desc) {
        errors.push(format!("{label} OCIO GPU shader extraction failed: {err}"));
        return false;
    }
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
    let _lease = lock_ocio_config_operation()?;
    init_ocio_locked(path)
}

fn init_ocio_locked(path: &Path) -> Result<(), String> {
    init_ocio_from_source_locked(path, OcioConfigSource::Path { path: path.to_path_buf() })
}

fn init_ocio_from_source_locked(path: &Path, source: OcioConfigSource) -> Result<(), String> {
    let config = Config::from_file(path.to_string_lossy().as_ref())
        .map_err(|e| format!("failed to load OCIO config from {}: {e}", path.display()))?;

    config
        .validate()
        .map_err(|e| format!("invalid OCIO config from {}: {e}", path.display()))?;

    OCIO_STATE
        .lock()
        .map_err(|_| "OCIO global state lock is poisoned".to_owned())?
        .set_config(path.to_path_buf(), source, &config)?;

    // The global OCIO context now holds a reference (ref-counted by the C++
    // library).  We deliberately forget the Rust wrapper so the ref-count
    // never reaches zero while the process is alive.
    std::mem::forget(config);

    tracing::info!(path=%path.display(), "OCIO config loaded");
    Ok(())
}

/// Load an OCIO built-in config by name and set it as the current config.
pub fn init_ocio_builtin(name: &str) -> Result<(), String> {
    let _lease = lock_ocio_config_operation()?;
    init_ocio_builtin_locked(name)
}

fn init_ocio_builtin_locked(name: &str) -> Result<(), String> {
    let registry = BuiltinConfigRegistry::get()
        .map_err(|e| format!("failed to access built-in config registry: {e}"))?;

    let config = registry
        .config_by_name(name)
        .ok_or_else(|| format!("built-in OCIO config not found: '{name}'"))?;

    let virtual_path = PathBuf::from(format!("builtin:{name}"));
    OCIO_STATE
        .lock()
        .map_err(|_| "OCIO global state lock is poisoned".to_owned())?
        .set_config(
            virtual_path,
            OcioConfigSource::Builtin { name: name.to_string() },
            &config,
        )?;

    // Keep the registry alive — its Config references need it.
    std::mem::forget(registry);

    tracing::info!(builtin=%name, "OCIO built-in config loaded");
    Ok(())
}

/// Load Mondrian's embedded default OCIO config and set it as the current config.
pub fn init_mondrian_default_ocio() -> Result<(), String> {
    let _lease = lock_ocio_config_operation()?;
    init_mondrian_standard_ocio_locked(MondrianStandardPackageIdentity::V3)
}

fn init_mondrian_standard_ocio_locked(
    package: MondrianStandardPackageIdentity,
) -> Result<(), String> {
    let contract = mondrian_standard_ocio_contract(package)?;
    let actual_digest = sha256_hex(MONDRIAN_DEFAULT_OCIO_CONFIG.as_bytes());
    if actual_digest != contract.content_sha256 {
        return Err(format!(
            "embedded Mondrian OCIO config '{}' failed integrity validation: expected SHA-256 '{}', got '{}'",
            contract.config_name,
            contract.content_sha256,
            actual_digest
        ));
    }
    let config = build_mondrian_default_ocio_config(MONDRIAN_DEFAULT_OCIO_CONFIG, package)
        .map_err(|e| {
            format!(
                "failed to load embedded Mondrian OCIO package '{}': {e}",
                contract.config_name
            )
        })?;
    let actual_package_digest =
        mondrian_default_package_sha256(MONDRIAN_DEFAULT_OCIO_CONFIG, &config, contract)?;
    if actual_package_digest != contract.package_sha256 {
        return Err(format!(
            "embedded Mondrian OCIO package '{}' failed integrity validation: expected SHA-256 '{}', got '{}'",
            contract.config_name,
            contract.package_sha256,
            actual_package_digest
        ));
    }
    let source = OcioConfigSource::MondrianStandard { package };
    OCIO_STATE
        .lock()
        .map_err(|_| "OCIO global state lock is poisoned".to_owned())?
        .set_config(PathBuf::from(contract.virtual_path), source, &config)?;
    std::mem::forget(config);

    tracing::info!(
        config = contract.config_name,
        package_sha256 = contract.package_sha256,
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
    let _lease = lock_ocio_config_operation()?;
    ensure_ocio_loaded_locked(source)
}

fn ensure_ocio_loaded_locked(source: &OcioConfigSource) -> Result<(), String> {
    // Check if the requested source is already loaded.
    if let Ok(guard) = OCIO_STATE.lock() {
        if guard.source.as_ref() == Some(source) {
            return Ok(());
        }
    }
    // Different source requested — load it.
    match source {
        OcioConfigSource::MondrianStandard { package } => {
            init_mondrian_standard_ocio_locked(*package)
        }
        OcioConfigSource::Builtin { name } => init_ocio_builtin_locked(name),
        OcioConfigSource::Path { path } => {
            if path.exists() {
                init_ocio_locked(path)
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
            init_ocio_from_source_locked(&resolved, OcioConfigSource::Environment)
        }
    }
}

fn with_ocio_config_for_source<T>(
    source: &OcioConfigSource,
    operation: impl FnOnce(&Config, u64) -> Result<T, String>,
) -> Result<T, String> {
    let _lease = lock_ocio_config_operation()?;
    ensure_ocio_loaded_locked(source)?;
    let generation = OCIO_STATE
        .lock()
        .map_err(|_| "OCIO global state lock is poisoned".to_owned())?
        .generation;
    let config = ocio_rs::current_config()
        .ok_or_else(|| "OCIO selected source has no current config".to_owned())?;
    operation(&config, generation)
}

fn resolved_config_cache_id(config: &Config) -> Result<String, String> {
    let cache_id = config
        .current_context()
        .and_then(|context| config.cache_id_for_context(&context))
        .or_else(|| config.cache_id())
        .ok_or_else(|| "OCIO config returned no resolved cache identity".to_owned())?;
    if cache_id.trim().is_empty() {
        return Err("OCIO config returned a blank resolved cache identity".to_owned());
    }
    Ok(cache_id)
}

fn primary_config_sha256(source: &OcioConfigSource, config: &Config) -> Result<String, String> {
    let bytes = match source {
        OcioConfigSource::Path { path } => std::fs::read(path)
            .map_err(|e| format!("failed to read Custom OCIO config {}: {e}", path.display()))?,
        OcioConfigSource::Environment => {
            let path = resolve_from_environment()?;
            std::fs::read(&path)
                .map_err(|e| format!("failed to read Custom OCIO config {}: {e}", path.display()))?
        }
        OcioConfigSource::Builtin { name } => config
            .serialize()
            .map_err(|err| format!("built-in OCIO config '{name}' serialization failed: {err}"))?
            .ok_or_else(|| format!("built-in OCIO config '{name}' could not be serialized"))?
            .into_bytes(),
        OcioConfigSource::MondrianStandard { .. } => {
            return Err(
                "Mondrian's embedded config belongs to Mondrian Standard, not Custom OCIO"
                    .to_owned(),
            );
        }
    };
    Ok(sha256_hex(&bytes))
}

fn custom_ocio_roles(config: &Config) -> Result<Vec<CustomOcioRoleIdentity>, String> {
    let mut roles = Vec::with_capacity(config.num_roles().max(0) as usize);
    for index in 0..config.num_roles() {
        let role = config
            .role_name(index)
            .ok_or_else(|| format!("OCIO role at index {index} has no name"))?;
        let color_space = config.role_color_space_by_index(index).ok_or_else(|| {
            format!("OCIO role '{role}' at index {index} has no color-space binding")
        })?;
        roles.push(CustomOcioRoleIdentity::new(role, color_space));
    }
    roles.sort_by(|left, right| left.role().cmp(right.role()));
    Ok(roles)
}

fn custom_ocio_processor_graph_sha256(
    config: &Config,
    working_space: &str,
    display: &str,
    view: &str,
) -> Result<String, String> {
    let mut color_spaces = (0..config.num_color_spaces())
        .filter_map(|index| config.color_space_name_by_index(index))
        .collect::<Vec<_>>();
    color_spaces.sort();

    let mut digest = Sha256::new();
    digest.update(b"mondrian-custom-ocio-processor-graph-v1\0");
    update_fingerprint_field(&mut digest, "working-space", working_space);
    for color_space in color_spaces {
        if color_space == working_space {
            continue;
        }
        if let Ok(processor) = config.processor(&color_space, working_space) {
            update_custom_processor_fingerprint(
                &mut digest,
                &format!("colorspace:{color_space}->{working_space}"),
                processor,
            )?;
        }
        if let Ok(processor) = config.processor(working_space, &color_space) {
            update_custom_processor_fingerprint(
                &mut digest,
                &format!("colorspace:{working_space}->{color_space}"),
                processor,
            )?;
        }
    }
    let display_processor = config
        .processor_display(
            working_space,
            display,
            view,
            ocio_rs::TransformDirection::Forward,
        )
        .map_err(|error| {
            format!(
                "Custom OCIO display processor '{working_space}' -> {display}/{view} failed: {error}"
            )
        })?;
    update_custom_processor_fingerprint(
        &mut digest,
        &format!("display:{working_space}->{display}/{view}"),
        display_processor,
    )?;
    Ok(finish_sha256_hex(digest))
}

fn update_custom_processor_fingerprint(
    digest: &mut Sha256,
    label: &str,
    processor: ocio_rs::Processor,
) -> Result<(), String> {
    update_fingerprint_field(digest, "processor", label);
    let cache_id = processor
        .cache_id()
        .ok_or_else(|| format!("Custom OCIO processor '{label}' has no cache-id"))?;
    update_fingerprint_field(digest, "processor-cache-id", &cache_id);
    Ok(())
}

fn custom_ocio_display_view_look(
    config: &Config,
    display: &str,
    view: &str,
) -> CustomOcioLookIdentity {
    match config.display_view_looks(display, view) {
        Some(looks) if !looks.trim().is_empty() => CustomOcioLookIdentity::DisplayView { looks },
        _ => CustomOcioLookIdentity::None,
    }
}

fn validate_custom_ocio_display_view(
    config: &Config,
    display: &str,
    view: &str,
) -> Result<(), String> {
    let display_exists = (0..config.num_displays())
        .filter_map(|index| config.display(index))
        .any(|candidate| candidate == display);
    if !display_exists {
        return Err(format!(
            "Custom OCIO config has no display named '{display}'"
        ));
    }
    let view_exists = (0..config.num_views(display))
        .filter_map(|index| config.view(display, index))
        .any(|candidate| candidate == view);
    if !view_exists {
        return Err(format!(
            "Custom OCIO display '{display}' has no view named '{view}'"
        ));
    }
    Ok(())
}

fn validate_custom_ocio_identity(
    identity: &CustomOcioProjectIdentity,
    config: &Config,
) -> Result<(), String> {
    if !identity.dynamic_properties().is_empty() {
        return Err(format!(
            "Custom OCIO project contains {} dynamic-property override(s), but this build does not support applying project-level dynamic properties",
            identity.dynamic_properties().len()
        ));
    }
    let actual_sha256 = primary_config_sha256(identity.source(), config)?;
    if actual_sha256 != identity.config_sha256() {
        return Err(format!(
            "Custom OCIO config content changed: expected SHA-256 '{}', got '{}' from {}",
            identity.config_sha256(),
            actual_sha256,
            identity.source()
        ));
    }
    let actual_cache_id = resolved_config_cache_id(config)?;
    if actual_cache_id != identity.resolved_cache_id() {
        return Err(format!(
            "Custom OCIO resolved config graph changed: expected cache-id '{}', got '{}'",
            identity.resolved_cache_id(),
            actual_cache_id
        ));
    }
    let actual_processor_graph_sha256 = custom_ocio_processor_graph_sha256(
        config,
        identity.working_space(),
        identity.display(),
        identity.view(),
    )?;
    if actual_processor_graph_sha256 != identity.processor_graph_sha256() {
        return Err(format!(
            "Custom OCIO executable processor graph or dependency resources changed: expected SHA-256 '{}', got '{}'",
            identity.processor_graph_sha256(),
            actual_processor_graph_sha256
        ));
    }
    if config.color_space(identity.working_space()).is_none() {
        return Err(format!(
            "Custom OCIO config has no pinned working color space '{}'",
            identity.working_space()
        ));
    }
    validate_custom_ocio_display_view(config, identity.display(), identity.view())?;
    let actual_look = custom_ocio_display_view_look(config, identity.display(), identity.view());
    if &actual_look != identity.look() {
        return Err(format!(
            "Custom OCIO display/view look changed for '{}/{}': expected {:?}, got {:?}",
            identity.display(),
            identity.view(),
            identity.look(),
            actual_look
        ));
    }
    let actual_roles = custom_ocio_roles(config)?;
    if actual_roles != identity.roles() {
        return Err(format!(
            "Custom OCIO role bindings changed: expected {:?}, got {:?}",
            identity.roles(),
            actual_roles
        ));
    }
    Ok(())
}

fn ensure_custom_ocio_identity_loaded_locked(
    identity: &CustomOcioProjectIdentity,
    force_reload: bool,
) -> Result<(), String> {
    let already_validated = OCIO_STATE
        .lock()
        .map_err(|_| "OCIO global state lock is poisoned".to_owned())?
        .validated_custom_identity
        .as_ref()
        == Some(identity);
    if already_validated && !force_reload {
        return Ok(());
    }

    if matches!(
        identity.source(),
        OcioConfigSource::Path { .. } | OcioConfigSource::Environment
    ) {
        // OCIO caches FileTransform resources process-wide. Explicit Custom
        // config reloads must invalidate that cache before rebuilding the
        // processor graph, otherwise a changed LUT at the same path retains
        // the old cache-id and pixel semantics until process restart.
        ocio_rs::try_clear_all_caches()
            .map_err(|err| format!("failed to clear process-global OCIO caches: {err}"))?;
    }

    match identity.source() {
        OcioConfigSource::Path { path } => {
            if !path.is_file() {
                return Err(format!(
                    "OCIO config file not found: {}\nPlace a config.ocio file at this path or change the OCIO source in project settings.",
                    path.display()
                ));
            }
            init_ocio_from_source_locked(path, identity.source().clone())?;
        }
        OcioConfigSource::Environment => {
            let path = resolve_from_environment()?;
            init_ocio_from_source_locked(&path, OcioConfigSource::Environment)?;
        }
        OcioConfigSource::Builtin { .. } => ensure_ocio_loaded_locked(identity.source())?,
        OcioConfigSource::MondrianStandard { .. } => {
            return Err("Custom OCIO cannot load the Mondrian Standard config source".to_owned());
        }
    }

    let config = ocio_rs::current_config()
        .ok_or_else(|| "Custom OCIO source has no current config".to_owned())?;
    validate_custom_ocio_identity(identity, &config)?;
    OCIO_STATE
        .lock()
        .map_err(|_| "OCIO global state lock is poisoned".to_owned())?
        .validated_custom_identity = Some(identity.clone());
    Ok(())
}

fn with_ocio_config_for_engine<T>(
    engine: &ColorEngine,
    operation: impl FnOnce(&Config, u64) -> Result<T, String>,
) -> Result<T, String> {
    let ColorEngine::CustomOcio { identity } = engine else {
        return with_ocio_config_for_source(&engine.ocio_source(), operation);
    };
    let _lease = lock_ocio_config_operation()?;
    ensure_custom_ocio_identity_loaded_locked(identity, false)?;
    let generation = OCIO_STATE
        .lock()
        .map_err(|_| "OCIO global state lock is poisoned".to_owned())?
        .generation;
    let config = ocio_rs::current_config()
        .ok_or_else(|| "Custom OCIO source has no current config".to_owned())?;
    operation(&config, generation)
}

/// Resolve a Custom OCIO source into a complete reproducible project identity.
pub fn pin_custom_ocio_project(
    source: OcioConfigSource,
    working_space: WorkingColorSpace,
    display: String,
    view: String,
) -> Result<ColorEngine, String> {
    pin_custom_ocio_project_with_selection(source, working_space, Some((display, view)))
}

/// Resolve a Custom OCIO source and pin its declared default display/view.
pub fn pin_custom_ocio_project_default(
    source: OcioConfigSource,
    working_space: WorkingColorSpace,
) -> Result<ColorEngine, String> {
    pin_custom_ocio_project_with_selection(source, working_space, None)
}

fn pin_custom_ocio_project_with_selection(
    source: OcioConfigSource,
    working_space: WorkingColorSpace,
    display_view: Option<(String, String)>,
) -> Result<ColorEngine, String> {
    if matches!(source, OcioConfigSource::MondrianStandard { .. }) {
        return Err(
            "Mondrian's embedded config must be selected through Mondrian Standard".to_owned(),
        );
    }
    with_ocio_config_for_source(&source, |config, _generation| {
        config
            .validate()
            .map_err(|e| format!("Custom OCIO config validation failed: {e}"))?;
        let working_space =
            ocio_color_space_identity_name(OcioColorSpaceIdentity::Working(working_space))
                .to_owned();
        if config.color_space(&working_space).is_none() {
            return Err(format!(
                "Custom OCIO config has no requested working color space '{working_space}'"
            ));
        }
        let (display, view) = match display_view {
            Some((display, view)) => (display, view),
            None => {
                let display = config.default_display().ok_or_else(|| {
                    "Custom OCIO config has no declared default display".to_owned()
                })?;
                let view = config.default_view(&display).ok_or_else(|| {
                    format!("Custom OCIO display '{display}' has no declared default view")
                })?;
                (display, view)
            }
        };
        validate_custom_ocio_display_view(config, &display, &view)?;
        let identity = CustomOcioProjectIdentity::from_resolved(
            source.clone(),
            primary_config_sha256(&source, config)?,
            resolved_config_cache_id(config)?,
            custom_ocio_processor_graph_sha256(config, &working_space, &display, &view)?,
            working_space,
            display.clone(),
            view.clone(),
            custom_ocio_display_view_look(config, &display, &view),
            custom_ocio_roles(config)?,
        );
        if let Ok(mut state) = OCIO_STATE.lock() {
            state.validated_custom_identity = Some(identity.clone());
        }
        Ok(ColorEngine::CustomOcio { identity: Box::new(identity) })
    })
}

/// Load and verify the exact config identity pinned by a color engine.
pub fn ensure_color_engine_ocio_loaded(engine: &ColorEngine) -> Result<(), String> {
    let ColorEngine::CustomOcio { identity } = engine else {
        return with_ocio_config_for_engine(engine, |_config, _generation| Ok(()));
    };
    let _lease = lock_ocio_config_operation()?;
    ensure_custom_ocio_identity_loaded_locked(identity, true)
}

fn current_ocio_generation_for_source(source: &OcioConfigSource) -> Option<u64> {
    OCIO_STATE
        .lock()
        .ok()
        .and_then(|state| (state.source.as_ref() == Some(source)).then_some(state.generation))
}

/// Return the OCIO source used by Mondrian Standard/Simple mode.
pub fn mondrian_default_ocio_source() -> OcioConfigSource {
    OcioConfigSource::MondrianStandard { package: MondrianStandardPackageIdentity::V3 }
}

/// Ensure Mondrian's default OCIO config is loaded.
pub fn ensure_mondrian_default_ocio_loaded() -> Result<(), String> {
    ensure_ocio_loaded(&mondrian_default_ocio_source())
}

/// Return true when Mondrian's default OCIO config is currently loaded.
pub fn mondrian_default_ocio_available() -> bool {
    already_loaded_with(&PathBuf::from(MONDRIAN_STANDARD_V3_OCIO_VIRTUAL_PATH))
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
/// against the embedded `mondrian_default_ocio_v2` config. Custom OCIO configs
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

fn ocio_cpu_processor_from_config(
    config: &Config,
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
) -> Result<CPUProcessor, String> {
    let src_name = ocio_color_space_identity_name(src);
    let dst_name = ocio_color_space_identity_name(dst);

    let processor = config
        .processor(src_name, dst_name)
        .map_err(|e| format!("OCIO processor '{src_name}' → '{dst_name}': {e}"))?;

    processor
        .default_cpu_processor()
        .map_err(|e| format!("OCIO CPU processor '{src_name}' → '{dst_name}': {e}"))
}

fn ocio_processor_from_config(
    config: &Config,
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
) -> Result<ocio_rs::Processor, String> {
    let src_name = ocio_color_space_identity_name(src);
    let dst_name = ocio_color_space_identity_name(dst);

    config
        .processor(src_name, dst_name)
        .map_err(|e| format!("OCIO processor '{src_name}' -> '{dst_name}': {e}"))
}

fn ocio_display_processor_from_config(
    config: &Config,
    src: OcioColorSpaceIdentity,
    display: &str,
    view: &str,
) -> Result<ocio_rs::Processor, String> {
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

fn ocio_display_cpu_processor_from_config(
    config: &Config,
    src: OcioColorSpaceIdentity,
    display: &str,
    view: &str,
) -> Result<CPUProcessor, String> {
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

const OCIO_CPU_PROCESSOR_CACHE_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum OcioCpuProcessorRequest {
    ColorSpace {
        src: OcioColorSpaceIdentity,
        dst: OcioColorSpaceIdentity,
    },
    DisplayView {
        src: OcioColorSpaceIdentity,
        display: String,
        view: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OcioCpuProcessorCacheKey {
    engine: ColorEngine,
    revision: u64,
    request: OcioCpuProcessorRequest,
}

fn ocio_cpu_processor_config_revision(source: &OcioConfigSource, generation: u64) -> u64 {
    match source {
        OcioConfigSource::MondrianStandard { .. } | OcioConfigSource::Builtin { .. } => 0,
        OcioConfigSource::Environment | OcioConfigSource::Path { .. } => generation,
    }
}

thread_local! {
    static OCIO_CPU_PROCESSOR_CACHE: RefCell<LruCache<OcioCpuProcessorCacheKey, CPUProcessor>> =
        RefCell::new(LruCache::new(
            NonZeroUsize::new(OCIO_CPU_PROCESSOR_CACHE_CAPACITY)
                .unwrap_or(NonZeroUsize::MIN),
        ));
}

static OCIO_CPU_PROCESSOR_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static OCIO_CPU_PROCESSOR_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static OCIO_CPU_PROCESSOR_CACHE_EVICTIONS: AtomicU64 = AtomicU64::new(0);

/// Process-wide counters plus current-thread occupancy for the CPU processor cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcioCpuProcessorCacheDiagnostics {
    /// Warm processor lookups served without config selection or reconstruction.
    pub hits: u64,
    /// Processor constructions performed after a cache miss.
    pub misses: u64,
    /// Least-recently-used processors evicted from thread-local caches.
    pub evictions: u64,
    /// Entries in the calling thread's bounded cache.
    pub current_thread_entries: usize,
    /// Per-thread entry capacity.
    pub per_thread_capacity: usize,
}

/// Return CPU OCIO processor cache diagnostics.
pub fn ocio_cpu_processor_cache_diagnostics() -> OcioCpuProcessorCacheDiagnostics {
    OcioCpuProcessorCacheDiagnostics {
        hits: OCIO_CPU_PROCESSOR_CACHE_HITS.load(Ordering::Relaxed),
        misses: OCIO_CPU_PROCESSOR_CACHE_MISSES.load(Ordering::Relaxed),
        evictions: OCIO_CPU_PROCESSOR_CACHE_EVICTIONS.load(Ordering::Relaxed),
        current_thread_entries: OCIO_CPU_PROCESSOR_CACHE.with(|cache| cache.borrow().len()),
        per_thread_capacity: OCIO_CPU_PROCESSOR_CACHE_CAPACITY,
    }
}

#[cfg(test)]
fn clear_ocio_cpu_processor_cache_for_current_thread() {
    OCIO_CPU_PROCESSOR_CACHE.with(|cache| cache.borrow_mut().clear());
}

fn try_apply_cached_cpu_processor(key: &OcioCpuProcessorCacheKey, data: &mut [f32]) -> bool {
    OCIO_CPU_PROCESSOR_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let Some(processor) = cache.get(key) else {
            return false;
        };
        apply_cpu_processor_float(processor, data);
        true
    })
}

fn has_cached_cpu_processor(key: &OcioCpuProcessorCacheKey) -> bool {
    OCIO_CPU_PROCESSOR_CACHE.with(|cache| cache.borrow().contains(key))
}

fn build_cpu_processor(
    config: &Config,
    request: &OcioCpuProcessorRequest,
) -> Result<CPUProcessor, String> {
    match request {
        OcioCpuProcessorRequest::ColorSpace { src, dst } => {
            ocio_cpu_processor_from_config(config, *src, *dst)
        }
        OcioCpuProcessorRequest::DisplayView { src, display, view } => {
            ocio_display_cpu_processor_from_config(config, *src, display, view)
        }
    }
}

fn apply_cached_cpu_processor(
    engine: &ColorEngine,
    request: OcioCpuProcessorRequest,
    data: &mut [f32],
) -> Result<(), String> {
    let source = engine.ocio_source();
    if let Some(generation) = current_ocio_generation_for_source(&source) {
        let key = OcioCpuProcessorCacheKey {
            engine: engine.clone(),
            revision: ocio_cpu_processor_config_revision(&source, generation),
            request: request.clone(),
        };
        if try_apply_cached_cpu_processor(&key, data) {
            OCIO_CPU_PROCESSOR_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    }

    let (key, processor) = with_ocio_config_for_engine(engine, |config, generation| {
        let key = OcioCpuProcessorCacheKey {
            engine: engine.clone(),
            revision: ocio_cpu_processor_config_revision(&source, generation),
            request: request.clone(),
        };
        let processor = if has_cached_cpu_processor(&key) {
            None
        } else {
            Some(build_cpu_processor(config, &request)?)
        };
        Ok((key, processor))
    })?;

    if let Some(processor) = processor {
        OCIO_CPU_PROCESSOR_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() == OCIO_CPU_PROCESSOR_CACHE_CAPACITY {
                OCIO_CPU_PROCESSOR_CACHE_EVICTIONS.fetch_add(1, Ordering::Relaxed);
            }
            cache.put(key.clone(), processor);
        });
        OCIO_CPU_PROCESSOR_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    } else {
        OCIO_CPU_PROCESSOR_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    }

    if try_apply_cached_cpu_processor(&key, data) {
        Ok(())
    } else {
        Err("OCIO CPU processor cache lost a selected processor".to_owned())
    }
}

fn validate_engine_display_view_selection(
    engine: &ColorEngine,
    display: &str,
    view: &str,
) -> Result<(), String> {
    match engine {
        ColorEngine::MondrianStandard { package } => {
            let contract = mondrian_standard_ocio_contract(*package)?;
            if contract
                .display_views
                .iter()
                .any(|candidate| candidate.display == display && candidate.view == view)
            {
                return Ok(());
            }
            Err(format!(
                "Mondrian Standard package '{}' ({}) does not permit display/view '{display}/{view}'",
                package.package_id(),
                package.package_sha256()
            ))
        }
        ColorEngine::CustomOcio { identity } => {
            if identity.display() == display && identity.view() == view {
                Ok(())
            } else {
                Err(format!(
                    "Custom OCIO project pins display/view '{}/{}', not '{display}/{view}'",
                    identity.display(),
                    identity.view()
                ))
            }
        }
        ColorEngine::Aces { .. } => Ok(()),
    }
}

fn validate_engine_working_identities(
    engine: &ColorEngine,
    identities: &[OcioColorSpaceIdentity],
) -> Result<(), String> {
    let ColorEngine::CustomOcio { identity } = engine else {
        return Ok(());
    };
    for candidate in identities {
        if let OcioColorSpaceIdentity::Working(working) = candidate {
            let actual = ocio_color_space_identity_name(OcioColorSpaceIdentity::Working(*working));
            if actual != identity.working_space() {
                return Err(format!(
                    "Custom OCIO project pins working space '{}', not '{actual}'",
                    identity.working_space()
                ));
            }
        }
    }
    Ok(())
}

// ── Public entry points ────────────────────────────────────────────────────────

/// Apply an engine-qualified OCIO conversion using the bounded CPU processor cache.
pub(crate) fn apply_ocio_identity_float(
    engine: &ColorEngine,
    data: &mut [f32],
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
) -> Result<(), String> {
    validate_engine_working_identities(engine, &[src, dst])?;
    if data.is_empty() || src == dst {
        return Ok(());
    }
    apply_cached_cpu_processor(
        engine,
        OcioCpuProcessorRequest::ColorSpace { src, dst },
        data,
    )
}

/// Apply an engine-qualified OCIO display/view transform using the CPU cache.
pub(crate) fn apply_ocio_display_identity_float(
    engine: &ColorEngine,
    data: &mut [f32],
    src: OcioColorSpaceIdentity,
    display: &str,
    view: &str,
) -> Result<(), String> {
    validate_engine_display_view_selection(engine, display, view)?;
    validate_engine_working_identities(engine, &[src])?;
    if data.is_empty() {
        return Ok(());
    }
    apply_cached_cpu_processor(
        engine,
        OcioCpuProcessorRequest::DisplayView {
            src,
            display: display.to_owned(),
            view: view.to_owned(),
        },
        data,
    )
}

/// Resolve the stable stock-OCIO processor cache id for one input transform.
///
/// This diagnostic query creates no GPU shader or renderer resource and fails
/// closed when the selected engine/config or either identity is unavailable.
pub fn ocio_identity_processor_cache_id(
    engine: &ColorEngine,
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
) -> Result<String, String> {
    validate_engine_working_identities(engine, &[src, dst])?;
    with_ocio_config_for_engine(engine, |config, _generation| {
        ocio_processor_from_config(config, src, dst)?
            .cache_id()
            .filter(|cache_id| !cache_id.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "OCIO processor '{}' -> '{}' returned an empty cache id",
                    ocio_color_space_identity_name(src),
                    ocio_color_space_identity_name(dst)
                )
            })
    })
}

/// Extract an engine-qualified GPU shader bundle under a short config lease.
///
/// The returned bundle is renderer-facing metadata. It deliberately does not
/// allocate wgpu resources; callers should cache compiled shaders and uploaded
/// texture/uniform resources by `cache_id` plus their render-target contract.
pub fn extract_ocio_identity_gpu_shader_bundle(
    engine: &ColorEngine,
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
    language: GpuLanguage,
) -> Result<OcioGpuShaderBundle, String> {
    validate_engine_working_identities(engine, &[src, dst])?;
    with_ocio_config_for_engine(engine, |config, _generation| {
        extract_ocio_identity_gpu_shader_bundle_from_config(config, src, dst, language)
    })
}

fn extract_ocio_identity_gpu_shader_bundle_from_config(
    config: &Config,
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
    language: GpuLanguage,
) -> Result<OcioGpuShaderBundle, String> {
    let processor = ocio_processor_from_config(config, src, dst)?;
    let cache_id = processor.cache_id();
    let gpu = processor.default_gpu_processor().map_err(|e| {
        format!(
            "OCIO GPU processor '{}' -> '{}': {e}",
            ocio_color_space_identity_name(src),
            ocio_color_space_identity_name(dst)
        )
    })?;
    let mut desc = configured_gpu_shader_desc(language)?;
    gpu.try_extract_shader_info(&mut desc).map_err(|err| {
        format!(
            "OCIO GPU shader extraction '{}' -> '{}': {err}",
            ocio_color_space_identity_name(src),
            ocio_color_space_identity_name(dst)
        )
    })?;
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

/// Extract an engine-qualified display/view GPU shader under a short config lease.
pub fn extract_ocio_display_identity_gpu_shader_bundle(
    engine: &ColorEngine,
    src: OcioColorSpaceIdentity,
    display: &str,
    view: &str,
    language: GpuLanguage,
) -> Result<OcioGpuShaderBundle, String> {
    validate_engine_display_view_selection(engine, display, view)?;
    validate_engine_working_identities(engine, &[src])?;
    with_ocio_config_for_engine(engine, |config, _generation| {
        extract_ocio_display_identity_gpu_shader_bundle_from_config(
            config, src, display, view, language,
        )
    })
}

fn extract_ocio_display_identity_gpu_shader_bundle_from_config(
    config: &Config,
    src: OcioColorSpaceIdentity,
    display: &str,
    view: &str,
    language: GpuLanguage,
) -> Result<OcioGpuShaderBundle, String> {
    let processor = ocio_display_processor_from_config(config, src, display, view)?;
    let cache_id = processor.cache_id();
    let gpu = processor.default_gpu_processor().map_err(|e| {
        format!(
            "OCIO GPU display processor '{}' -> {display}/{view}: {e}",
            ocio_color_space_identity_name(src),
        )
    })?;
    let mut desc = configured_gpu_shader_desc(language)?;
    gpu.try_extract_shader_info(&mut desc).map_err(|err| {
        format!(
            "OCIO GPU display shader extraction '{}' -> {display}/{view}: {err}",
            ocio_color_space_identity_name(src)
        )
    })?;
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
    desc.set_language(language)
        .map_err(|e| format!("OCIO GPU shader language: {e}"))?;
    desc.set_function_name(MONDRIAN_OCIO_GPU_FUNCTION_NAME)
        .map_err(|e| format!("OCIO GPU shader function name: {e}"))?;
    desc.set_pixel_name(MONDRIAN_OCIO_GPU_PIXEL_NAME)
        .map_err(|e| format!("OCIO GPU shader pixel name: {e}"))?;
    desc.set_resource_prefix(MONDRIAN_OCIO_GPU_RESOURCE_PREFIX)
        .map_err(|e| format!("OCIO GPU shader resource prefix: {e}"))?;
    desc.try_set_descriptor_set_index(
        MONDRIAN_OCIO_GPU_DESCRIPTOR_SET_INDEX,
        MONDRIAN_OCIO_GPU_TEXTURE_BINDING_START,
    )
    .map_err(|e| format!("OCIO GPU shader descriptor binding: {e}"))?;
    Ok(desc)
}

fn extracted_shader_text(desc: &GpuShaderDesc) -> Result<String, String> {
    let shader_text = desc
        .try_shader_text()
        .map_err(|e| format!("OCIO GPU shader text query: {e}"))?
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

/// List displays from the exact config selected by an engine.
pub(crate) fn ocio_display_names_for_engine(engine: &ColorEngine) -> Result<Vec<String>, String> {
    with_ocio_config_for_engine(engine, |config, _generation| {
        let count = config.num_displays();
        Ok((0..count).filter_map(|index| config.display(index)).collect())
    })
}

/// List views under a display from the exact config selected by an engine.
pub(crate) fn ocio_view_names_for_engine(
    engine: &ColorEngine,
    display: &str,
) -> Result<Vec<String>, String> {
    with_ocio_config_for_engine(engine, |config, _generation| {
        let count = config.num_views(display);
        Ok((0..count).filter_map(|index| config.view(display, index)).collect())
    })
}

/// Resolve one display's default view from the exact engine config.
pub(crate) fn ocio_default_view_for_display_for_engine(
    engine: &ColorEngine,
    display: &str,
) -> Result<Option<String>, String> {
    with_ocio_config_for_engine(engine, |config, _generation| {
        Ok(config.default_view(display))
    })
}

/// Resolve the default display/view pair from the exact engine config.
pub(crate) fn ocio_default_display_view_for_engine(
    engine: &ColorEngine,
) -> Result<Option<(String, String)>, String> {
    with_ocio_config_for_engine(engine, |config, _generation| {
        let Some(display) = config.default_display() else {
            return Ok(None);
        };
        Ok(config.default_view(&display).map(|view| (display, view)))
    })
}

/// Resolve the exact Standard display/view pair for an encoded output target.
///
/// This target-aware mapping prevents a P3 or HDR program output from silently
/// using the config's sRGB default display. Targets without a completed,
/// versioned Standard View fail closed rather than borrowing an ACES View.
pub fn mondrian_standard_output_display_view(
    output: ColorSpace,
) -> Result<(String, String), String> {
    mondrian_standard_output_display_view_for_package(MondrianStandardPackageIdentity::V3, output)
}

/// Resolve one encoded output target against an exact immutable Standard package.
pub fn mondrian_standard_output_display_view_for_package(
    package: MondrianStandardPackageIdentity,
    output: ColorSpace,
) -> Result<(String, String), String> {
    let contract = mondrian_standard_output_target_contract_for_package(package, output)?;
    mondrian_standard_display_view_named(package, contract.display, contract.view)
}

/// Resolve Mondrian Standard's versioned View for an explicit OCIO display.
pub fn mondrian_standard_display_view(display: &str) -> Result<(String, String), String> {
    mondrian_standard_display_view_for_package(MondrianStandardPackageIdentity::V3, display)
}

/// Resolve an explicit OCIO display against an exact immutable Standard package.
pub fn mondrian_standard_display_view_for_package(
    package: MondrianStandardPackageIdentity,
    display: &str,
) -> Result<(String, String), String> {
    let view = if matches!(display, "Rec.2100-HLG - Display" | "Rec.2100-PQ - Display") {
        MONDRIAN_STANDARD_HDR_1000_VIEW_NAME
    } else if package == MondrianStandardPackageIdentity::V2 {
        MONDRIAN_STANDARD_SDR_VIEW_NAME
    } else {
        MONDRIAN_STANDARD_SDR_V2_VIEW_NAME
    };
    mondrian_standard_display_view_named(package, display, view)
}

fn mondrian_standard_display_view_named(
    package: MondrianStandardPackageIdentity,
    display: &str,
    view: &str,
) -> Result<(String, String), String> {
    let contract = mondrian_standard_ocio_contract(package)?;
    if !contract
        .display_views
        .iter()
        .any(|candidate| candidate.display == display && candidate.view == view)
    {
        return Err(format!(
            "Mondrian Standard package does not declare display/view '{display}/{view}'"
        ));
    }
    let engine = ColorEngine::MondrianStandard { package };
    let views = ocio_view_names_for_engine(&engine, display)?;
    if !views.iter().any(|candidate| candidate == view) {
        return Err(format!(
            "Mondrian Standard package is missing required display/view '{display}/{view}'"
        ));
    }
    Ok((display.to_owned(), view.to_owned()))
}

/// Resolve the OCIO display identity paired with a Standard output target.
pub fn mondrian_standard_output_display_name(output: ColorSpace) -> Result<&'static str, String> {
    Ok(mondrian_standard_output_target_contract(output)?.display)
}

/// Resolve the immutable luminance, gamut, encoding, and OCIO View contract
/// for one Mondrian Standard v2 program-output target.
pub fn mondrian_standard_output_target_contract(
    output: ColorSpace,
) -> Result<MondrianStandardOutputTargetContract, String> {
    mondrian_standard_output_target_contract_for_package(
        MondrianStandardPackageIdentity::V3,
        output,
    )
}

/// Resolve the immutable output contract for one exact Standard package.
pub fn mondrian_standard_output_target_contract_for_package(
    package: MondrianStandardPackageIdentity,
    output: ColorSpace,
) -> Result<MondrianStandardOutputTargetContract, String> {
    mondrian_standard_ocio_contract(package)?;
    let sdr_view = if package == MondrianStandardPackageIdentity::V2 {
        MONDRIAN_STANDARD_SDR_VIEW_NAME
    } else {
        MONDRIAN_STANDARD_SDR_V2_VIEW_NAME
    };
    let (
        display,
        view,
        view_transform_id,
        rendering_gamut_limit,
        reference_white_nits,
        nominal_peak_nits,
    ) = match output {
        ColorSpace::Srgb => (
            "sRGB - Display",
            sdr_view,
            package.sdr_view_transform_id(),
            crate::ColorPrimaries::Bt709,
            100,
            100,
        ),
        ColorSpace::Rec709 => (
            "Rec.1886 Rec.709 - Display",
            sdr_view,
            package.sdr_view_transform_id(),
            crate::ColorPrimaries::Bt709,
            100,
            100,
        ),
        ColorSpace::DisplayP3 => (
            "Display P3 - Display",
            sdr_view,
            package.sdr_view_transform_id(),
            crate::ColorPrimaries::P3D65,
            100,
            100,
        ),
        ColorSpace::Rec2020 => (
            "Rec.2020 SDR - Display",
            sdr_view,
            package.sdr_view_transform_id(),
            crate::ColorPrimaries::Bt2020,
            100,
            100,
        ),
        ColorSpace::Rec2100Hlg => (
            "Rec.2100-HLG - Display",
            MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
            package.hdr_view_transform_id(),
            crate::ColorPrimaries::P3D65,
            100,
            1000,
        ),
        ColorSpace::Rec2100Pq => (
            "Rec.2100-PQ - Display",
            MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
            package.hdr_view_transform_id(),
            crate::ColorPrimaries::P3D65,
            100,
            1000,
        ),
        unsupported => {
            return Err(format!(
                "Mondrian Standard has no rendering View for output target {unsupported:?}"
            ));
        }
    };
    Ok(MondrianStandardOutputTargetContract {
        output_color_space: output,
        display,
        view,
        view_transform_id,
        encoding: output.encoding(),
        rendering_gamut_limit,
        reference_white_nits,
        nominal_peak_nits,
        black_level_millinits: 0,
    })
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
        let config = build_mondrian_default_ocio_config(
            mondrian_default_ocio_config_text(),
            MondrianStandardPackageIdentity::V3,
        )
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
            &ColorEngine::mondrian_standard(),
            &mut samples,
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::AcesCg),
        )
        .expect("Linear Rec.2020 to ACEScg comparison processor");

        assert!(samples.chunks_exact(4).flatten().any(|channel| *channel < 0.0));
        assert!(samples.chunks_exact(4).flatten().any(|channel| *channel > 1.0));

        apply_ocio_identity_float(
            &ColorEngine::mondrian_standard(),
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
                &ColorEngine::mondrian_standard(),
                &mut samples,
                OcioColorSpaceIdentity::Color(source),
                OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            )
            .unwrap_or_else(|error| panic!("{source:?} input processor failed: {error}"));
            assert!(samples[..3].iter().all(|channel| channel.is_finite()));
            assert_eq!(samples[3], original[3], "{source:?} input changed alpha");

            apply_ocio_identity_float(
                &ColorEngine::mondrian_standard(),
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
    fn mondrian_standard_hdr_resource_has_pinned_domain_and_resolution() {
        assert_eq!(
            sha256_hex(MONDRIAN_STANDARD_HDR_1000_LUT.as_bytes()),
            MONDRIAN_STANDARD_HDR_1000_LUT_SHA256
        );
        let cube = parse_cube_3d(MONDRIAN_STANDARD_HDR_1000_LUT)
            .expect("pinned Mondrian Standard HDR LUT should parse");
        assert_eq!(cube.edge, MONDRIAN_STANDARD_HDR_1000_LUT_EDGE);
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
    fn standard_sdr_v2_gamut_surface_tracks_its_analytic_boundary_model() {
        let config =
            Config::from_stream(mondrian_default_ocio_config_text()).expect("embedded base config");
        let (normalize, surface) = build_standard_sdr_v2_gamut_surface().expect("gamut surface");
        let group = GroupTransform::create().expect("gamut surface group");
        group.append_transform(&normalize).expect("normalize domain");
        group.append_transform(&surface).expect("sample surface");

        let mut samples = Vec::new();
        let mut expected = Vec::new();
        for value_index in 0..=160 {
            let value = value_index as f64 / 20.0;
            for saturation_index in 0..=200 {
                let saturation = saturation_index as f64 / 100.0;
                samples.extend_from_slice(&[0.371_f32, saturation as f32, value as f32, 0.42]);
                let bounded_saturation =
                    saturation.min(MONDRIAN_STANDARD_SDR_V2_SATURATION_DOMAIN_MAX);
                let bounded_value = value.min(MONDRIAN_STANDARD_SDR_V2_VALUE_DOMAIN_MAX);
                expected.push((
                    standard_sdr_v2_compressed_saturation(bounded_saturation, bounded_value) as f32,
                    (bounded_value / MONDRIAN_STANDARD_SDR_V2_VALUE_DOMAIN_MAX) as f32,
                ));
            }
        }
        let pixel_count = (samples.len() / 4) as i64;
        config
            .processor_from_transform(&group, TransformDirection::Forward)
            .expect("gamut surface processor")
            .default_cpu_processor()
            .expect("gamut surface CPU processor")
            .try_apply_rgba_pixels(&mut samples, pixel_count, 4)
            .expect("apply gamut surface");

        let mut max_saturation_error = 0.0_f32;
        for (pixel, (expected_saturation, expected_value)) in samples.chunks_exact(4).zip(expected)
        {
            max_saturation_error = max_saturation_error.max((pixel[1] - expected_saturation).abs());
            assert!((pixel[0] - 0.371).abs() <= 2.0e-5, "hue drift: {pixel:?}");
            assert!(
                (pixel[2] - expected_value).abs() <= 2.0e-5,
                "normalized value drift: expected {expected_value}, got {pixel:?}"
            );
            assert!((pixel[3] - 0.42).abs() <= 1.0e-6, "alpha drift: {pixel:?}");
        }
        assert!(
            max_saturation_error <= 0.004,
            "61^3 gamut surface approximation error {max_saturation_error}"
        );
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
            &ColorEngine::mondrian_standard(),
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
            &ColorEngine::mondrian_standard(),
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
    fn mondrian_standard_hdr_views_are_finite_neutral_and_monotonic() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        for output in [ColorSpace::Rec2100Hlg, ColorSpace::Rec2100Pq] {
            let (display, view) =
                mondrian_standard_output_display_view(output).expect("Standard HDR output view");
            let mut samples = (-320..=480)
                .flat_map(|index| {
                    let linear = 0.18_f32 * 2.0_f32.powf(index as f32 / 32.0);
                    [linear, linear, linear, 1.0]
                })
                .collect::<Vec<_>>();

            apply_ocio_display_identity_float(
                &ColorEngine::mondrian_standard(),
                &mut samples,
                OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
                &display,
                &view,
            )
            .unwrap_or_else(|error| panic!("{output:?} Standard HDR processor: {error}"));

            let mut previous = f32::NEG_INFINITY;
            for pixel in samples.chunks_exact(4) {
                assert!(
                    pixel.iter().all(|channel| channel.is_finite()),
                    "{output:?} produced non-finite {pixel:?}"
                );
                let neutral_spread = pixel[..3].iter().copied().fold(f32::NEG_INFINITY, f32::max)
                    - pixel[..3].iter().copied().fold(f32::INFINITY, f32::min);
                assert!(
                    neutral_spread <= 5.0e-4,
                    "{output:?} neutral axis spread {neutral_spread} for {pixel:?}"
                );
                assert!(
                    pixel[1] + 1.0e-6 >= previous,
                    "{output:?} tone reversal: previous {previous}, current {}",
                    pixel[1]
                );
                assert!(
                    (-1.0e-5..=1.000_01).contains(&pixel[1]),
                    "{output:?} neutral code value outside normalized signal range: {}",
                    pixel[1]
                );
                previous = pixel[1];
            }
        }
    }

    #[test]
    fn mondrian_standard_hdr_views_share_one_display_referred_formation() {
        let config = build_mondrian_default_ocio_config(
            mondrian_default_ocio_config_text(),
            MondrianStandardPackageIdentity::V3,
        )
        .expect("embedded Mondrian OCIO package should build");
        let mut hlg = [
            0.001, 0.001, 0.001, 1.0, 0.18, 0.18, 0.18, 1.0, 1.0, 1.0, 1.0, 1.0, 8.0, 2.0, 0.25,
            1.0,
        ];
        let mut pq = hlg;

        for (display, pixels) in [
            ("Rec.2100-HLG - Display", &mut hlg[..]),
            ("Rec.2100-PQ - Display", &mut pq[..]),
        ] {
            let processor = config
                .processor_display(
                    ocio_working_color_space_name(WorkingColorSpace::LinearRec2020),
                    display,
                    MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
                    ocio_rs::TransformDirection::Forward,
                )
                .unwrap_or_else(|error| panic!("{display} processor: {error}"));
            processor
                .default_cpu_processor()
                .expect("HDR CPU processor")
                .try_apply_rgba_pixels(pixels, (pixels.len() / 4) as i64, 4)
                .unwrap_or_else(|error| panic!("{display} apply: {error}"));
        }

        let pq_to_hlg = config
            .processor("Rec.2100-PQ - Display", "Rec.2100-HLG - Display")
            .expect("PQ to HLG display-encoding processor")
            .default_cpu_processor()
            .expect("PQ to HLG CPU processor");
        let pq_pixel_count = (pq.len() / 4) as i64;
        pq_to_hlg
            .try_apply_rgba_pixels(&mut pq, pq_pixel_count, 4)
            .expect("convert PQ rendering to HLG encoding");

        for (index, (actual, expected)) in pq.iter().zip(hlg).enumerate() {
            assert!(
                (actual - expected).abs() <= 2.0e-4,
                "HLG/PQ formation mismatch at channel {index}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn hdr_linear_lut_interpolation_is_not_equivalent_to_tetrahedral_policy() {
        const LINEAR_VIEW: &str = "Mondrian Standard HDR linear interpolation test";
        let config = build_mondrian_default_ocio_config(
            mondrian_default_ocio_config_text(),
            MondrianStandardPackageIdentity::V3,
        )
        .expect("embedded Mondrian OCIO package should build");
        build_mondrian_standard_hdr_1000_view_with_interpolation(
            &config,
            LINEAR_VIEW,
            OcioRsInterpolation::Linear,
        )
        .expect("linear-interpolation comparison view");

        let levels = [
            -0.05_f32, 0.0, 0.000_1, 0.001, 0.01, 0.18, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0,
        ];
        let mut tetrahedral = Vec::with_capacity(levels.len().pow(3) * 4);
        for red in levels {
            for green in levels {
                for blue in levels {
                    tetrahedral.extend_from_slice(&[red, green, blue, 1.0]);
                }
            }
        }
        let mut linear = tetrahedral.clone();
        for (view, pixels) in [
            (MONDRIAN_STANDARD_HDR_1000_VIEW_NAME, &mut tetrahedral),
            (LINEAR_VIEW, &mut linear),
        ] {
            let pixel_count = (pixels.len() / 4) as i64;
            config
                .processor_display(
                    ocio_working_color_space_name(WorkingColorSpace::LinearRec2020),
                    "Rec.2100-PQ - Display",
                    view,
                    ocio_rs::TransformDirection::Forward,
                )
                .unwrap_or_else(|error| panic!("{view} processor: {error}"))
                .default_cpu_processor()
                .unwrap_or_else(|error| panic!("{view} CPU processor: {error}"))
                .try_apply_rgba_pixels(pixels, pixel_count, 4)
                .unwrap_or_else(|error| panic!("{view} apply: {error}"));
        }

        let mut errors = tetrahedral
            .chunks_exact(4)
            .zip(linear.chunks_exact(4))
            .flat_map(|(tetrahedral, linear)| {
                (0..3).map(move |channel| (tetrahedral[channel] - linear[channel]).abs())
            })
            .collect::<Vec<_>>();
        assert!(errors.iter().all(|error| error.is_finite()));
        errors.sort_unstable_by(f32::total_cmp);
        let p99_index = errors.len().saturating_mul(99).div_ceil(100).saturating_sub(1);
        let p99 = errors[p99_index];
        let max = *errors.last().expect("comparison errors");
        assert!(
            p99 > 0.002,
            "linear interpolation unexpectedly became equivalent at p99={p99}"
        );
        assert!(
            max > 0.01,
            "linear interpolation unexpectedly became equivalent at max={max}"
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
        let config = build_mondrian_default_ocio_config(
            mondrian_default_ocio_config_text(),
            MondrianStandardPackageIdentity::V3,
        )
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
    fn pinned_aces_default_views_match_the_bundled_ocio_registry() {
        for preset in [
            crate::types::AcesConfigPreset::StudioV4Aces2Ocio25,
            crate::types::AcesConfigPreset::CgV4Aces2Ocio25,
        ] {
            let actual =
                with_ocio_config_for_source(&preset.ocio_source(), |config, _generation| {
                    let display = config
                        .default_display()
                        .ok_or_else(|| "ACES preset has no default display".to_owned())?;
                    let view = config.default_view(&display).ok_or_else(|| {
                        "ACES preset default display has no default view".to_owned()
                    })?;
                    Ok((display, view))
                })
                .expect("bundled ACES default display/view");
            let expected = preset.default_display_view();
            assert_eq!(actual.0, expected.0, "preset={preset:?}");
            assert_eq!(actual.1, expected.1, "preset={preset:?}");
        }
    }

    #[test]
    fn cpu_processor_cache_reuses_engine_qualified_processor() {
        clear_ocio_cpu_processor_cache_for_current_thread();
        let before = ocio_cpu_processor_cache_diagnostics();
        let engine = ColorEngine::mondrian_standard();
        let original = [0.18, 0.42, 0.73, 0.375];
        let mut reference = None;

        for _ in 0..4 {
            let mut sample = original;
            engine
                .convert_identity_float(
                    &mut sample,
                    OcioColorSpaceIdentity::Color(ColorSpace::SonySLog3SGamut3Cine),
                    OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
                )
                .expect("Standard input processor");
            assert_eq!(sample[3], original[3]);
            if let Some(expected) = reference {
                assert_eq!(sample, expected);
            } else {
                reference = Some(sample);
            }
        }

        let after = ocio_cpu_processor_cache_diagnostics();
        assert!(after.misses > before.misses);
        assert!(after.hits > before.hits);
        assert!((1..=after.per_thread_capacity).contains(&after.current_thread_entries));
    }

    #[test]
    fn immutable_cpu_processor_survives_switch_to_another_engine() {
        clear_ocio_cpu_processor_cache_for_current_thread();
        let standard = ColorEngine::mondrian_standard();
        let mut first = [0.18, 0.42, 0.73, 0.375];
        standard
            .convert_identity_float(
                &mut first,
                OcioColorSpaceIdentity::Color(ColorSpace::SonySLog3SGamut3Cine),
                OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            )
            .expect("first Standard input processor");
        assert_eq!(
            ocio_cpu_processor_cache_diagnostics().current_thread_entries,
            1
        );

        ColorEngine::Aces {
            preset: crate::types::AcesConfigPreset::StudioV4Aces2Ocio25,
        }
        .default_display_view()
        .expect("ACES config switch");

        let hits_before = ocio_cpu_processor_cache_diagnostics().hits;
        let mut second = [0.18, 0.42, 0.73, 0.375];
        standard
            .convert_identity_float(
                &mut second,
                OcioColorSpaceIdentity::Color(ColorSpace::SonySLog3SGamut3Cine),
                OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            )
            .expect("reselected Standard input processor");

        let after = ocio_cpu_processor_cache_diagnostics();
        assert_eq!(second, first);
        assert!(after.hits > hits_before);
        assert_eq!(after.current_thread_entries, 1);
    }

    #[test]
    fn concurrent_engine_queries_never_observe_another_config() {
        let standard = ColorEngine::mondrian_standard();
        let aces = ColorEngine::Aces {
            preset: crate::types::AcesConfigPreset::StudioV4Aces2Ocio25,
        };
        let expected_standard = standard.default_display_view().expect("Standard display/view");
        let expected_aces = aces.default_display_view().expect("ACES display/view");
        assert_ne!(expected_standard, expected_aces);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let standard_barrier = std::sync::Arc::clone(&barrier);
        let standard_thread = std::thread::spawn(move || {
            standard_barrier.wait();
            for _ in 0..16 {
                assert_eq!(
                    standard.default_display_view().expect("Standard display/view"),
                    expected_standard
                );
            }
        });
        let aces_barrier = std::sync::Arc::clone(&barrier);
        let aces_thread = std::thread::spawn(move || {
            aces_barrier.wait();
            for _ in 0..16 {
                assert_eq!(
                    aces.default_display_view().expect("ACES display/view"),
                    expected_aces
                );
            }
        });

        barrier.wait();
        standard_thread.join().expect("Standard query thread");
        aces_thread.join().expect("ACES query thread");
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
        let config = build_mondrian_default_ocio_config(
            mondrian_default_ocio_config_text(),
            MondrianStandardPackageIdentity::V3,
        )
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
            ColorEngine::mondrian_standard()
                .default_display_view()
                .ok()
                .as_ref()
                .map(|(display, view)| { (display.as_str(), view.as_str()) }),
            Some((contract.default_display, contract.default_view))
        );
    }

    #[test]
    fn custom_ocio_project_reopens_exact_identity_and_rejects_content_drift() {
        let path = std::env::temp_dir().join(format!(
            "mondrian-custom-ocio-identity-{}-{}.ocio",
            std::process::id(),
            ocio_config_generation()
        ));
        std::fs::write(&path, mondrian_default_ocio_config_text())
            .expect("write Custom OCIO test config");

        let engine = ColorEngine::custom_ocio(
            OcioConfigSource::Path { path: path.clone() },
            WorkingColorSpace::LinearRec2020,
            "sRGB - Display",
            "ACES 2.0 - SDR 100 nits (Rec.709)",
        )
        .expect("pin real Custom OCIO config");
        let serialized = serde_json::to_string(&engine).expect("serialize pinned Custom OCIO");
        let reopened: ColorEngine =
            serde_json::from_str(&serialized).expect("reopen pinned Custom OCIO");

        assert_eq!(reopened, engine);
        assert_eq!(
            reopened.default_display_view().expect("pinned display/view"),
            (
                "sRGB - Display".to_owned(),
                "ACES 2.0 - SDR 100 nits (Rec.709)".to_owned()
            )
        );
        reopened.ensure_loaded().expect("unchanged config identity");

        let mut changed = mondrian_default_ocio_config_text().to_owned();
        changed.push_str("\n# external edit after project save\n");
        std::fs::write(&path, changed).expect("mutate Custom OCIO test config");

        let error = reopened
            .ensure_loaded()
            .expect_err("same path with changed config content must fail closed");
        assert!(error.contains("config content changed"), "{error}");

        std::fs::remove_file(path).expect("remove Custom OCIO test config");
    }

    #[test]
    fn custom_ocio_project_pins_config_default_display_and_view() {
        let path = std::env::temp_dir().join(format!(
            "mondrian-custom-ocio-default-view-{}-{}.ocio",
            std::process::id(),
            ocio_config_generation()
        ));
        std::fs::write(&path, mondrian_default_ocio_config_text())
            .expect("write Custom OCIO test config");

        let engine = ColorEngine::custom_ocio_default(
            OcioConfigSource::Path { path: path.clone() },
            WorkingColorSpace::LinearRec2020,
        )
        .expect("pin Custom OCIO defaults");
        assert_eq!(
            engine.default_display_view().expect("pinned defaults"),
            (
                "sRGB - Display".to_owned(),
                "ACES 2.0 - SDR 100 nits (Rec.709)".to_owned()
            )
        );
        engine.ensure_loaded().expect("reopen pinned defaults");

        std::fs::remove_file(path).expect("remove Custom OCIO test config");
    }

    #[test]
    fn custom_ocio_project_rejects_dependency_resource_drift() {
        const CONFIG: &str = r#"ocio_profile_version: 2.1
search_path: .
strictparsing: true
roles:
  default: Linear Rec.2020
  scene_linear: Linear Rec.2020
displays:
  Test Display:
    - !<View> {name: Test View, colorspace: LUT Output}
active_displays: [Test Display]
active_views: [Test View]
colorspaces:
  - !<ColorSpace>
    name: Linear Rec.2020
    isdata: false
  - !<ColorSpace>
    name: LUT Output
    isdata: false
    from_scene_reference: !<FileTransform> {src: test.cube}
"#;
        const IDENTITY_LUT: &str =
            "LUT_3D_SIZE 2\n0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n";
        const CHANGED_LUT: &str =
            "LUT_3D_SIZE 2\n0 0 0\n0 0 0.8\n0 0.8 0\n0 0.8 0.8\n0.8 0 0\n0.8 0 0.8\n0.8 0.8 0\n0.8 0.8 0.8\n";

        let directory = std::env::temp_dir().join(format!(
            "mondrian-custom-ocio-resources-{}-{}",
            std::process::id(),
            ocio_config_generation()
        ));
        std::fs::create_dir_all(&directory).expect("create Custom OCIO resource directory");
        let config_path = directory.join("config.ocio");
        let lut_path = directory.join("test.cube");
        std::fs::write(&config_path, CONFIG).expect("write Custom OCIO config");
        std::fs::write(&lut_path, IDENTITY_LUT).expect("write Custom OCIO LUT");

        let engine = ColorEngine::custom_ocio(
            OcioConfigSource::Path { path: config_path },
            WorkingColorSpace::LinearRec2020,
            "Test Display",
            "Test View",
        )
        .expect("pin Custom OCIO config with LUT dependency");
        engine.ensure_loaded().expect("unchanged Custom OCIO resources");

        std::fs::write(&lut_path, CHANGED_LUT).expect("mutate Custom OCIO LUT dependency");
        let error = engine.ensure_loaded().expect_err("changed LUT dependency must fail closed");
        assert!(error.contains("dependency resources changed"), "{error}");

        std::fs::remove_dir_all(directory).expect("remove Custom OCIO resource directory");
    }

    #[test]
    fn standard_output_targets_resolve_target_specific_sdr_and_hdr_views() {
        ensure_mondrian_default_ocio_loaded().expect("Standard package");

        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::Srgb).expect("sRGB target"),
            (
                "sRGB - Display".to_owned(),
                MONDRIAN_STANDARD_SDR_V2_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::Rec709).expect("Rec.709 target"),
            (
                "Rec.1886 Rec.709 - Display".to_owned(),
                MONDRIAN_STANDARD_SDR_V2_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::DisplayP3).expect("P3 target"),
            (
                "Display P3 - Display".to_owned(),
                MONDRIAN_STANDARD_SDR_V2_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::Rec2020)
                .expect("Rec.2020 SDR target"),
            (
                "Rec.2020 SDR - Display".to_owned(),
                MONDRIAN_STANDARD_SDR_V2_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_display_view("Display P3 - Display").expect("explicit P3 display"),
            (
                "Display P3 - Display".to_owned(),
                MONDRIAN_STANDARD_SDR_V2_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_display_view("Rec.2100-PQ - Display").expect("explicit PQ display"),
            (
                "Rec.2100-PQ - Display".to_owned(),
                MONDRIAN_STANDARD_HDR_1000_VIEW_NAME.to_owned()
            )
        );
        assert!(mondrian_standard_display_view("Gamma 2.2 Rec.709 - Display").is_err());
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
        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::Rec2100Hlg).expect("HLG output view"),
            (
                "Rec.2100-HLG - Display".to_owned(),
                MONDRIAN_STANDARD_HDR_1000_VIEW_NAME.to_owned()
            )
        );
        assert_eq!(
            mondrian_standard_output_display_view(ColorSpace::Rec2100Pq).expect("PQ output view"),
            (
                "Rec.2100-PQ - Display".to_owned(),
                MONDRIAN_STANDARD_HDR_1000_VIEW_NAME.to_owned()
            )
        );
        let pq = mondrian_standard_output_target_contract(ColorSpace::Rec2100Pq)
            .expect("PQ output contract");
        assert!(pq.is_hdr());
        assert_eq!(pq.output_color_space, ColorSpace::Rec2100Pq);
        assert_eq!(pq.view_transform_id, "mondrian_standard_hdr_1000_nits_v1");
        assert_eq!(pq.encoding.primaries, crate::ColorPrimaries::Bt2020);
        assert_eq!(pq.rendering_gamut_limit, crate::ColorPrimaries::P3D65);
        assert_eq!(pq.reference_white_nits, 100);
        assert_eq!(pq.nominal_peak_nits, 1000);
        assert_eq!(pq.black_level_millinits, 0);

        let rec709 = mondrian_standard_output_target_contract(ColorSpace::Rec709)
            .expect("Rec.709 output contract");
        assert!(!rec709.is_hdr());
        assert_eq!(rec709.view_transform_id, "mondrian_standard_sdr_v2");
        assert_eq!(rec709.reference_white_nits, 100);
        assert_eq!(rec709.nominal_peak_nits, 100);
        let rec2020 = mondrian_standard_output_target_contract(ColorSpace::Rec2020)
            .expect("Rec.2020 SDR output contract");
        assert_eq!(rec2020.rendering_gamut_limit, crate::ColorPrimaries::Bt2020);
        assert_eq!(rec2020.encoding.primaries, crate::ColorPrimaries::Bt2020);
        assert_eq!(rec2020.reference_white_nits, 100);
        assert_eq!(rec2020.nominal_peak_nits, 100);
    }

    #[test]
    fn standard_engine_rejects_cross_mode_aces_views_on_cpu_and_gpu() {
        let engine = ColorEngine::mondrian_standard();
        let source = OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020);
        let mut pixel = [0.18, 0.18, 0.18, 1.0];
        let cpu_error = apply_ocio_display_identity_float(
            &engine,
            &mut pixel,
            source,
            "sRGB - Display",
            "ACES 2.0 - SDR 100 nits (Rec.709)",
        )
        .expect_err("Standard CPU path must reject an ACES View");
        assert!(cpu_error.contains("does not permit display/view"));

        let gpu_error = extract_ocio_display_identity_gpu_shader_bundle(
            &engine,
            source,
            "Rec.2100-PQ - Display",
            "ACES 2.0 - HDR 1000 nits (Rec.2020)",
            GpuLanguage::Glsl4_0,
        )
        .expect_err("Standard GPU path must reject an ACES View");
        assert!(gpu_error.contains("does not permit display/view"));
    }

    #[test]
    fn standard_mode_extracts_gpu_shader_bundle() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        let bundle = extract_ocio_identity_gpu_shader_bundle(
            &ColorEngine::mondrian_standard(),
            OcioColorSpaceIdentity::Color(ColorSpace::SonySLog3SGamut3Cine),
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
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
    fn input_processor_cache_id_query_matches_gpu_processor_identity() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");
        let engine = ColorEngine::mondrian_standard();
        let source = OcioColorSpaceIdentity::Color(ColorSpace::SonySLog3SGamut3Cine);
        let working = OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020);
        let cache_id = ocio_identity_processor_cache_id(&engine, source, working)
            .expect("input processor cache id");
        let bundle =
            extract_ocio_identity_gpu_shader_bundle(&engine, source, working, GpuLanguage::Glsl4_0)
                .expect("matching GPU processor bundle");

        assert!(!cache_id.trim().is_empty());
        assert_eq!(bundle.cache_id.as_deref(), Some(cache_id.as_str()));
    }

    #[test]
    fn standard_mode_extracts_explicit_working_identity_gpu_shader_bundle() {
        ensure_mondrian_default_ocio_loaded().expect("standard mode default config should load");

        let bundle = extract_ocio_identity_gpu_shader_bundle(
            &ColorEngine::mondrian_standard(),
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
            &ColorEngine::mondrian_standard(),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
            GpuLanguage::Glsl4_0,
        )
        .expect("working to target-linear endpoint should produce a GPU program");
        let to_encoded_output = extract_ocio_identity_gpu_shader_bundle(
            &ColorEngine::mondrian_standard(),
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
            &ColorEngine::mondrian_standard(),
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
        let engine = ColorEngine::mondrian_standard();
        let (display, view) = engine.default_display_view().expect("default display/view");

        let bundle = extract_ocio_display_identity_gpu_shader_bundle(
            &engine,
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
            &display,
            &view,
            GpuLanguage::Glsl4_0,
        )
        .expect("default config should produce a display GPU shader bundle");

        assert_eq!(bundle.language, GpuLanguage::Glsl4_0);
        assert_eq!(bundle.src_color_space, "Camera Rec.709");
        assert_eq!(
            bundle.dst_color_space,
            format!("sRGB - Display/{MONDRIAN_STANDARD_SDR_V2_VIEW_NAME}")
        );
        assert!(bundle.shader_text.contains("mondrian_ocio_main"));
        assert!(bundle.cache_id.as_deref().is_some_and(|id| !id.trim().is_empty()));
        assert_eq!(bundle.texture_2d_count, 1);
        assert_eq!(bundle.texture_3d_count, 1);
        assert_eq!(bundle.uniform_count, 0);
        assert_eq!(bundle.textures_2d.len(), 1);
        assert_eq!(
            bundle.textures_2d[0].width,
            MONDRIAN_STANDARD_SDR_V2_TONE_LUT_EDGE as u32
        );
        assert_eq!(bundle.textures_2d[0].height, 2);
        assert_eq!(
            bundle.textures_2d[0].dimensions,
            OcioGpuTextureDimensions::Texture1D
        );
        assert_eq!(
            bundle.textures_2d[0].interpolation,
            OcioGpuTextureInterpolation::Linear
        );
        assert_eq!(bundle.textures_3d.len(), 1);
        assert_eq!(
            bundle.textures_3d[0].edge_len,
            MONDRIAN_STANDARD_SDR_V2_GAMUT_LUT_EDGE as u32
        );
        assert_eq!(
            bundle.textures_3d[0].interpolation,
            OcioGpuTextureInterpolation::Nearest
        );
        assert!(bundle.uniforms.is_empty());
        assert!(!bundle.shader_text.contains("grading_rgbcurve"));
        assert!(
            bundle.shader_text.len() <= 16_384,
            "Standard SDR v2 shader unexpectedly grew to {} bytes",
            bundle.shader_text.len()
        );
    }

    #[test]
    fn legacy_standard_v2_projects_reopen_their_pinned_sdr_lut_graph() {
        let engine = ColorEngine::MondrianStandard { package: MondrianStandardPackageIdentity::V2 };
        let (display, view) = engine.default_display_view().expect("v2 default display/view");
        assert_eq!(view, MONDRIAN_STANDARD_SDR_VIEW_NAME);
        let bundle = extract_ocio_display_identity_gpu_shader_bundle(
            &engine,
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
            &display,
            &view,
            GpuLanguage::Glsl4_0,
        )
        .expect("legacy v2 display GPU shader bundle");

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
        assert_eq!(source, Some(mondrian_default_ocio_source()));
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

    #[test]
    fn immutable_gpu_config_revisions_are_generation_independent() {
        assert_eq!(
            ocio_gpu_config_revision_for_engine(&ColorEngine::mondrian_standard())
                .expect("Mondrian revision"),
            0
        );
        assert_eq!(
            ocio_gpu_config_revision_for_engine(&ColorEngine::Aces {
                preset: crate::types::AcesConfigPreset::StudioV4Aces2Ocio25,
            })
            .expect("ACES revision"),
            0
        );
    }
}
