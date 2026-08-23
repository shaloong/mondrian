//! Headless Golden Project acceptance over production App Interfaces.
//!
//! Contract interpretation, fixture identity, and executable workflow slices
//! are deliberately separate so adding later Golden coverage does not create a
//! second monolithic acceptance harness.

mod audio_authoring_evidence;
mod color_media_roundtrip;
mod composed_workflow;
mod editorial_transport;
mod fixture;
mod foundation_audio;
mod generated_delivery;
mod harness;
mod headless_preview;
#[cfg(test)]
mod long_work_area_delivery;
mod media_execution;
mod plan;
mod proxy_relink;
mod recovery_nesting;
#[cfg(test)]
mod retime_execution;
mod retime_media_evidence;
mod visual_authoring;
mod workflow;

use anyhow::{ensure, Context};
use mondrian_core::{AudioChannelLayout, ColorSpace, Rational, Resolution, WorkingColorSpace};
use mondrian_export::delivery::resolve_export_delivery;
use mondrian_export::expected_export_video_signal;
use mondrian_export::preset::{
    AudioCodecConfig, BuiltinExportPreset, Container, ExportAlphaMode, ExportChromaSampling,
    H264Profile, HevcProfile, VideoCodecConfig,
};
use mondrian_timeline::sequence::{
    DeliveryBitDepth, FieldOrder, PixelAspectRatio, SequenceSettings, VideoRange,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Execute one complete single-Project Golden run in the dedicated validation
/// process and return the durable typed report path.
///
/// This entrypoint intentionally belongs to the optional `validation` feature:
/// media, proxy, export, and process-global GPU runtimes must execute under the
/// process main lifetime rather than a short-lived libtest worker.
pub fn run_complete_golden_project(output: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    composed_workflow::run_complete_golden_project(output)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GoldenProjectContract {
    pub(super) schema_version: u32,
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) hero_sequence: GoldenHeroSequenceContract,
    pub(super) timeline: GoldenTimelineContract,
    pub(super) required_fixture_roles: Vec<GoldenFixtureRole>,
    pub(super) required_operations: Vec<String>,
    pub(super) required_content: Vec<String>,
    pub(super) execution_slices: Vec<GoldenExecutionSlice>,
    exports: Vec<GoldenExportContract>,
    pub(super) acceptance: GoldenAcceptanceContract,
}

/// Acceptance identity for the one authored Sequence that must carry the
/// complete five-minute product workflow.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GoldenHeroSequenceContract {
    pub(super) role: String,
    pub(super) duration_frames: i64,
    pub(super) requires_all_obligations: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GoldenTimelineContract {
    pub(super) duration_frames: i64,
    pub(super) frame_rate: String,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pixel_aspect_ratio: String,
    pub(super) field_order: String,
    pub(super) working_color_space: String,
    pub(super) output_color_space: String,
    pub(super) video_range: String,
    pub(super) delivery_bit_depth: u8,
    pub(super) audio_sample_rate: u32,
    pub(super) audio_channel_layout: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GoldenFixtureRole {
    pub(super) role: String,
    pub(super) fixture_id: Option<String>,
    pub(super) required: bool,
    pub(super) required_purpose: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GoldenExecutionSlice {
    pub(super) id: String,
    pub(super) sequence_role: String,
    pub(super) required_fixture_roles: Vec<String>,
    pub(super) required_operations: Vec<String>,
    pub(super) required_content: Vec<String>,
    pub(super) required_exports: Vec<String>,
    pub(super) timeline_window: Option<GoldenTimelineWindow>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GoldenTimelineWindow {
    pub(super) start_frame: i64,
    pub(super) end_frame_exclusive: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GoldenAcceptanceContract {
    pub(super) consecutive_passes: u32,
    pub(super) duration_error_max_frames: u32,
    pub(super) av_boundary_error_max_ms: u32,
    pub(super) silent_fallback_allowed: bool,
    pub(super) unexecuted_requirement_may_pass: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoldenExportContract {
    id: String,
    builtin_preset_id: String,
    expected_delivery: ExpectedDeliveryContract,
    required_probe_fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedDeliveryContract {
    container: String,
    video_codec: String,
    video_profile: String,
    width: u32,
    height: u32,
    bit_depth: u8,
    chroma_sampling: String,
    pixel_format: String,
    range: String,
    color_primaries: String,
    color_transfer: String,
    color_matrix: String,
    static_hdr_metadata: String,
    alpha: String,
    audio_codec: String,
    audio_bitrate_kbps: u32,
}

pub(super) fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("mondrian-app must live under <repository>/crates")
        .to_path_buf()
}

pub(super) fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

pub(super) fn load_golden_contract(root: &Path) -> anyhow::Result<GoldenProjectContract> {
    let contract: GoldenProjectContract =
        load_json(&root.join("tests/validation/golden-project.json"))?;
    validate_golden_contract(&contract)?;
    Ok(contract)
}

fn validate_golden_contract(contract: &GoldenProjectContract) -> anyhow::Result<()> {
    ensure!(
        contract.schema_version == 4,
        "unsupported Golden Project schema"
    );
    ensure!(contract.kind == "golden", "contract kind is not golden");
    ensure!(
        contract.acceptance.consecutive_passes == 3,
        "Golden Project must require three consecutive passes"
    );
    ensure!(
        !contract.acceptance.unexecuted_requirement_may_pass,
        "unexecuted Golden requirements may not pass"
    );
    ensure!(
        contract.acceptance.duration_error_max_frames == 1,
        "Golden duration tolerance must remain one frame"
    );
    ensure!(
        contract.acceptance.av_boundary_error_max_ms == 20,
        "Golden A/V boundary tolerance must remain 20 ms"
    );
    ensure!(
        !contract.acceptance.silent_fallback_allowed,
        "Golden execution may not silently fall back"
    );
    ensure!(
        contract.timeline.duration_frames > 0,
        "Golden duration must be positive"
    );
    ensure!(
        !contract.hero_sequence.role.trim().is_empty(),
        "Golden Hero Sequence role is empty"
    );
    ensure!(
        contract.hero_sequence.duration_frames == contract.timeline.duration_frames,
        "Golden Hero Sequence duration differs from the complete Timeline contract"
    );
    ensure!(
        contract.hero_sequence.requires_all_obligations,
        "Golden Hero Sequence must carry every required fixture, operation, content, and export"
    );

    ensure_unique(
        contract.required_fixture_roles.iter().map(|role| role.role.as_str()),
        "fixture role",
    )?;
    ensure_unique(
        contract.required_operations.iter().map(String::as_str),
        "operation",
    )?;
    ensure_unique(
        contract.required_content.iter().map(String::as_str),
        "content",
    )?;
    ensure_unique(
        contract.execution_slices.iter().map(|slice| slice.id.as_str()),
        "slice",
    )?;
    ensure_unique(
        contract.exports.iter().map(|export| export.id.as_str()),
        "export",
    )?;

    let role_ids = contract
        .required_fixture_roles
        .iter()
        .map(|role| role.role.as_str())
        .collect::<BTreeSet<_>>();
    let operation_ids =
        contract.required_operations.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let content_ids = contract.required_content.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let export_ids = contract
        .exports
        .iter()
        .map(|export| export.id.as_str())
        .collect::<BTreeSet<_>>();
    for role in &contract.required_fixture_roles {
        ensure!(!role.role.trim().is_empty(), "fixture role id is empty");
        ensure!(
            !role.required_purpose.trim().is_empty(),
            "fixture role purpose is empty"
        );
        if role.required {
            ensure!(
                role.fixture_id.as_deref().is_none_or(|id| !id.trim().is_empty()),
                "required fixture role {} has an empty fixture id",
                role.role
            );
        }
    }
    for slice in &contract.execution_slices {
        ensure!(!slice.id.trim().is_empty(), "slice id is empty");
        ensure!(
            !slice.sequence_role.trim().is_empty(),
            "slice {} has an empty Sequence role",
            slice.id
        );
        ensure_unique(
            slice.required_fixture_roles.iter().map(String::as_str),
            "slice fixture role",
        )?;
        ensure_unique(
            slice.required_operations.iter().map(String::as_str),
            "slice operation",
        )?;
        ensure_unique(
            slice.required_content.iter().map(String::as_str),
            "slice content",
        )?;
        ensure_unique(
            slice.required_exports.iter().map(String::as_str),
            "slice export",
        )?;
        ensure!(
            slice.required_fixture_roles.iter().all(|id| role_ids.contains(id.as_str())),
            "slice {} references an unknown fixture role",
            slice.id
        );
        ensure!(
            slice.required_operations.iter().all(|id| operation_ids.contains(id.as_str())),
            "slice {} references an unknown operation",
            slice.id
        );
        ensure!(
            slice.required_content.iter().all(|id| content_ids.contains(id.as_str())),
            "slice {} references unknown content",
            slice.id
        );
        ensure!(
            slice.required_exports.iter().all(|id| export_ids.contains(id.as_str())),
            "slice {} references an unknown export",
            slice.id
        );
        if let Some(window) = slice.timeline_window {
            ensure!(
                window.start_frame >= 0
                    && window.end_frame_exclusive > window.start_frame
                    && window.end_frame_exclusive <= contract.timeline.duration_frames,
                "slice {} has an invalid timeline window",
                slice.id
            );
        }
    }
    ensure!(
        contract
            .execution_slices
            .iter()
            .any(|slice| slice.sequence_role == contract.hero_sequence.role),
        "Golden contract has no slice assigned to the Hero Sequence role"
    );
    Ok(())
}

fn ensure_unique<'a>(
    values: impl Iterator<Item = &'a str>,
    description: &str,
) -> anyhow::Result<()> {
    let mut observed = BTreeSet::new();
    for value in values {
        ensure!(!value.trim().is_empty(), "{description} id is empty");
        ensure!(
            observed.insert(value),
            "duplicate {description} id: {value}"
        );
    }
    Ok(())
}

pub(super) fn sequence_settings_from_contract(
    timeline: &GoldenTimelineContract,
) -> anyhow::Result<SequenceSettings> {
    let frame_rate = parse_rational(&timeline.frame_rate)?;
    ensure!(
        timeline.pixel_aspect_ratio == "1/1",
        "unsupported pixel aspect ratio"
    );
    ensure!(
        timeline.field_order == "progressive",
        "unsupported field order"
    );
    ensure!(
        timeline.working_color_space == "linear_rec2020",
        "unsupported Golden working color space"
    );
    ensure!(
        timeline.output_color_space == "rec709",
        "unsupported Golden output color space"
    );
    ensure!(
        timeline.video_range == "legal",
        "unsupported Golden video range"
    );
    ensure!(
        timeline.delivery_bit_depth == 10,
        "unsupported Golden delivery bit depth"
    );
    ensure!(
        timeline.audio_channel_layout == "stereo",
        "unsupported Golden audio layout"
    );

    let mut settings = SequenceSettings {
        resolution: Resolution { width: timeline.width, height: timeline.height },
        frame_rate,
        pixel_aspect_ratio: PixelAspectRatio::Square,
        field_order: FieldOrder::Progressive,
        audio_sample_rate: timeline.audio_sample_rate,
        audio_channel_layout: AudioChannelLayout::Stereo,
        ..SequenceSettings::default()
    };
    settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
    settings.color.program_output.color_space = ColorSpace::Rec709;
    settings.delivery.video_range = VideoRange::Legal;
    settings.delivery.bit_depth = DeliveryBitDepth::Ten;
    settings.validate()?;
    Ok(settings)
}

pub(super) fn parse_rational(value: &str) -> anyhow::Result<Rational> {
    let (numerator, denominator) =
        value.split_once('/').context("rational must use numerator/denominator")?;
    let numerator = numerator.parse::<i64>().context("parse rational numerator")?;
    let denominator = denominator.parse::<i64>().context("parse rational denominator")?;
    ensure!(
        denominator > 0 && numerator > 0,
        "rational values must be positive"
    );
    Ok(Rational::new(numerator, denominator).reduce())
}

fn builtin_preset(id: &str) -> anyhow::Result<BuiltinExportPreset> {
    BuiltinExportPreset::ALL
        .into_iter()
        .find(|preset| preset.id() == id)
        .with_context(|| format!("unknown built-in export preset: {id}"))
}

fn assert_export_contract(
    export: &GoldenExportContract,
    settings: &SequenceSettings,
) -> anyhow::Result<()> {
    ensure!(
        !export.required_probe_fields.is_empty(),
        "export probe contract is empty"
    );
    ensure_unique(
        export.required_probe_fields.iter().map(String::as_str),
        "export probe field",
    )?;
    for required_field in [
        "bit_depth",
        "primaries",
        "transfer",
        "matrix",
        "range",
        "hdr_static_metadata",
    ] {
        ensure!(
            export.required_probe_fields.iter().any(|field| field == required_field),
            "export probe contract does not require {required_field}"
        );
    }
    let preset = builtin_preset(&export.builtin_preset_id)?.preset();
    let resolved = resolve_export_delivery(
        &preset,
        settings,
        &mondrian_core::ProjectColorEnvironment::default(),
    )?;
    let expected_signal = expected_export_video_signal(settings, &resolved)
        .map_err(anyhow::Error::msg)
        .context("resolve expected encoded video signal")?;
    let expected = &export.expected_delivery;

    let container = match preset.container {
        Container::Mp4 => "mp4",
        Container::Mov => "mov",
        Container::Mkv => "mkv",
        Container::Gif => "gif",
        Container::Mxf => "mxf",
        Container::Webm => "webm",
    };
    let (video_codec, video_profile) = match preset.video {
        VideoCodecConfig::H264 { profile: H264Profile::High, .. } => ("h264", "high"),
        VideoCodecConfig::Hevc { profile: HevcProfile::Main, .. } => ("hevc", "main"),
        VideoCodecConfig::Hevc { profile: HevcProfile::Main10, .. } => ("hevc", "main10"),
        VideoCodecConfig::Av1 { .. } => ("av1", "main"),
        VideoCodecConfig::ProRes { .. } => ("prores", "typed"),
        VideoCodecConfig::Gif { .. } => ("gif", "gif"),
    };
    let bit_depth = match resolved.bit_depth {
        DeliveryBitDepth::Eight => 8,
        DeliveryBitDepth::Ten => 10,
        DeliveryBitDepth::Twelve => 12,
    };
    let chroma = match resolved.chroma_sampling {
        ExportChromaSampling::Yuv420 => "yuv420",
        ExportChromaSampling::Yuv422 => "yuv422",
        ExportChromaSampling::Yuv444 => "yuv444",
        ExportChromaSampling::Rgb => "rgb",
    };
    let range = match resolved.video_range {
        VideoRange::Full => "full",
        VideoRange::Legal => "legal",
    };
    let alpha = match preset.alpha_mode {
        ExportAlphaMode::FlattenBlack => "flatten_black",
        ExportAlphaMode::Preserve => "preserve",
    };
    let (audio_codec, audio_bitrate_kbps) = match preset.audio {
        AudioCodecConfig::Aac { bitrate_kbps } => ("aac", bitrate_kbps),
        AudioCodecConfig::Mp3 { bitrate_kbps } => ("mp3", bitrate_kbps),
        AudioCodecConfig::Pcm { .. } => ("pcm", 0),
        AudioCodecConfig::Disabled => ("disabled", 0),
    };

    ensure!(
        container == expected.container,
        "{} container contract drifted",
        export.id
    );
    ensure!(
        video_codec == expected.video_codec && video_profile == expected.video_profile,
        "{} video codec/profile contract drifted",
        export.id
    );
    ensure!(
        resolved.resolution.width == expected.width
            && resolved.resolution.height == expected.height,
        "{} resolution contract drifted",
        export.id
    );
    ensure!(
        bit_depth == expected.bit_depth,
        "{} bit-depth contract drifted",
        export.id
    );
    ensure!(
        chroma == expected.chroma_sampling,
        "{} chroma contract drifted",
        export.id
    );
    ensure!(
        resolved.pixel_format == expected.pixel_format,
        "{} pixel-format contract drifted",
        export.id
    );
    ensure!(
        range == expected.range,
        "{} range contract drifted",
        export.id
    );
    ensure!(
        expected_signal.color_primaries.as_deref() == Some(expected.color_primaries.as_str())
            && expected_signal.color_transfer.as_deref() == Some(expected.color_transfer.as_str())
            && expected_signal.color_matrix.as_deref() == Some(expected.color_matrix.as_str()),
        "{} encoded CICP contract drifted",
        export.id
    );
    let static_hdr_metadata = match expected_signal.static_hdr_metadata {
        mondrian_export::validator::ExpectedStaticHdrMetadata::Absent => "absent",
        mondrian_export::validator::ExpectedStaticHdrMetadata::Exact(_) => "present",
        mondrian_export::validator::ExpectedStaticHdrMetadata::Unspecified => {
            anyhow::bail!("{} has no static HDR validation policy", export.id)
        }
    };
    ensure!(
        static_hdr_metadata == expected.static_hdr_metadata,
        "{} static HDR metadata contract drifted",
        export.id
    );
    ensure!(
        alpha == expected.alpha,
        "{} alpha contract drifted",
        export.id
    );
    ensure!(
        audio_codec == expected.audio_codec && audio_bitrate_kbps == expected.audio_bitrate_kbps,
        "{} audio delivery contract drifted",
        export.id
    );
    Ok(())
}

#[test]
fn golden_contract_is_closed_and_matches_product_delivery_presets() -> anyhow::Result<()> {
    let root = repository_root();
    let contract = load_golden_contract(&root)?;
    let settings = sequence_settings_from_contract(&contract.timeline)?;
    ensure!(
        contract.id == "windows-alpha-golden-v14"
            && contract
                .required_operations
                .iter()
                .any(|operation| operation == "constant-retime"),
        "M1 Golden Project must retain the versioned constant-retime obligation"
    );
    let proxy_relink = contract
        .execution_slices
        .iter()
        .find(|slice| slice.id == proxy_relink::PROXY_RELINK_SLICE_ID)
        .context("M1 Golden Project must retain the Proxy/Relink slice")?;
    ensure!(
        proxy_relink.required_operations
            == ["proxy-original-switch", "offline-relink", "constant-retime"]
            && proxy_relink.required_fixture_roles
                == ["rec709-h264-picture", "rec709-h264-vfr-picture"]
            && proxy_relink.required_exports == ["h264-aac-sdr"],
        "Proxy/Relink must close constant retime with real CFR/VFR H.264 deliveries"
    );
    ensure!(
        contract.exports.len() == 2,
        "M1 Golden Project must define two delivery gates"
    );
    for export in &contract.exports {
        assert_export_contract(export, &settings)?;
    }
    Ok(())
}

#[test]
fn golden_contract_rejects_unknown_fields() -> anyhow::Result<()> {
    let root = repository_root();
    let path = root.join("tests/validation/golden-project.json");
    let mut value: serde_json::Value = load_json(&path)?;
    value.as_object_mut().context("Golden contract root must be an object")?.insert(
        "misspelled_future_requirement".to_owned(),
        serde_json::json!(true),
    );

    let error = serde_json::from_value::<GoldenProjectContract>(value)
        .expect_err("unknown Golden fields must fail closed");
    ensure!(
        error.to_string().contains("unknown field"),
        "unexpected closed-schema diagnostic: {error}"
    );
    Ok(())
}

#[test]
fn golden_acceptance_plan_requires_every_obligation_on_the_hero_sequence() -> anyhow::Result<()> {
    use plan::{GoldenAcceptancePlan, GoldenAcceptancePlanStatus};

    let root = repository_root();
    let mut contract = load_golden_contract(&root)?;
    let plan = GoldenAcceptancePlan::compile(&contract);

    assert_eq!(plan.schema_version, 1);
    assert_eq!(plan.status, GoldenAcceptancePlanStatus::Complete);
    assert!(!plan.complete_golden_project);
    assert_eq!(plan.required_consecutive_passes, 3);
    assert!(plan.missing.fixture_roles.is_empty());
    assert!(plan.missing.operations.is_empty());
    assert!(plan.missing.content.is_empty());
    assert!(plan.missing.exports.is_empty());
    assert!(plan.hero_missing.fixture_roles.is_empty());
    assert!(plan.hero_missing.operations.is_empty());
    assert!(plan.hero_missing.content.is_empty());
    assert!(plan.hero_missing.exports.is_empty());
    assert!(plan.unassigned_required_fixture_roles.is_empty());
    assert_eq!(plan.slices.len(), 7);

    let color_slice = contract
        .execution_slices
        .iter_mut()
        .find(|slice| slice.id == color_media_roundtrip::COLOR_MEDIA_SLICE_ID)
        .context("Color Media slice is absent")?;
    color_slice.sequence_role = "diagnostic-color-media".to_owned();
    let isolated = GoldenAcceptancePlan::compile(&contract);
    assert_eq!(isolated.status, GoldenAcceptancePlanStatus::Blocked);
    assert_eq!(
        isolated.hero_missing.fixture_roles,
        BTreeSet::from([
            "hlg-main10-picture".to_owned(),
            "srgb-alpha-still".to_owned(),
        ])
    );
    assert!(isolated.hero_missing.operations.is_empty());
    assert!(isolated.hero_missing.content.is_empty());
    assert!(isolated.hero_missing.exports.is_empty());
    assert!(!isolated.complete_golden_project);

    eprintln!(
        "MONDRIAN_GOLDEN_ACCEPTANCE_PLAN_JSON={}",
        serde_json::to_string(&plan)?
    );
    Ok(())
}
