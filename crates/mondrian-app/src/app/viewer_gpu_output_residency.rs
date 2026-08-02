//! Canonical Viewer GPU-output residency diagnostics.
//!
//! The Window supplies platform and renderer facts. This Module alone lowers
//! declared preview input or an executed renderer record into serializable
//! residency evidence, so planned work is never reported as observed zero-copy
//! execution.

use crate::app::native_video_import::{
    evaluate_native_video_import_readiness, NativeVideoImportReadiness,
    NativeVideoImportReadinessInput,
};
use crate::app::preview_execution::{PreviewGpuFrame, PreviewGpuWorkingInput};
use mondrian_platform::NativeVideoTextureImportProbeResult;
use mondrian_renderer::{
    GpuNativeDecodedFrameImportSupport, ViewerGpuExecutionLayer, ViewerGpuExecutionResidency,
    ViewerGpuNativeVideoFacts, ViewerGpuSourceLayer, ViewerGpuTransitionInput,
};

/// Serializable residency evidence for one Viewer GPU-output attempt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ViewerGpuOutputFrameResidency {
    /// Decoder residency composition observed or declared for the frame.
    pub decode_residency: ViewerGpuOutputDecodeResidency,
    /// Whether the working composite is merely planned or actually executed.
    pub working_residency: ViewerGpuOutputWorkingResidency,
    /// Input-transform paths used by the working composite.
    pub input_transform_path: ViewerGpuOutputInputTransformPath,
    /// Whether renderer execution facts were observed for this record.
    pub execution_observed: bool,
    /// Whether actual execution remained zero-copy for every media layer.
    pub zero_copy: bool,
    /// Whether actual execution used a declared low-copy/upload path.
    pub low_copy: bool,
    /// Uploads observed during actual execution.
    pub upload_count: u32,
    /// Readbacks observed during actual execution.
    pub readback_count: u32,
    /// Stable human-readable explanation of the residency evidence.
    pub reason: String,
    /// Native decoded-frame import readiness, when media is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_video_import: Option<NativeVideoImportReadiness>,
}

/// Decoder-residency composition for a Viewer output frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum ViewerGpuOutputDecodeResidency {
    /// Every media layer was CPU decoded.
    CpuDecodedRgba,
    /// Every media layer was natively GPU decoded.
    NativeGpuDecoded,
    /// The frame contains only procedural GPU-native layers.
    ProceduralGpuNative,
    /// CPU-decoded media and procedural layers are mixed.
    MixedCpuAndProcedural,
    /// Native-GPU and CPU-decoded media layers are mixed.
    MixedNativeGpuAndCpuDecoded,
    /// Native-GPU media and procedural layers are mixed.
    MixedNativeGpuAndProcedural,
    /// Native-GPU, CPU-decoded, and procedural layers are mixed.
    MixedNativeGpuCpuAndProcedural,
}

/// Evidence grade for working-composite residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum ViewerGpuOutputWorkingResidency {
    /// A GPU working composite is declared but has not completed execution.
    GpuWorkingCompositePlanned,
    /// Renderer execution completed a GPU working composite.
    GpuWorkingCompositeExecuted,
}

/// Input-transform paths contributing to a Viewer GPU working composite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum ViewerGpuOutputInputTransformPath {
    /// CPU OCIO output was uploaded into the GPU working composite.
    CpuOcio,
    /// Encoded source media used GPU OCIO input conversion.
    GpuOcio,
    /// Native decoder surfaces used the renderer native-video import path.
    GpuNativeVideoImport,
    /// The working composite contains only GPU-native procedural content.
    GpuNativeProcedural,
    /// Native-video media and GPU-native procedural content are mixed.
    MixedNativeVideoImportAndGpuNative,
    /// CPU OCIO media and GPU-native procedural content are mixed.
    MixedCpuOcioAndGpuNative,
    /// GPU OCIO media and GPU-native procedural content are mixed.
    MixedGpuOcioAndGpuNative,
    /// CPU and GPU OCIO media paths are mixed.
    MixedCpuOcioAndGpuOcio,
    /// More than two distinct input-transform families are mixed.
    MixedInputTransforms,
}

/// Describe declared frame inputs before renderer execution.
///
/// Planned evidence intentionally reports no zero-copy, upload, or readback
/// success. Actual facts replace this provisional record after execution.
pub(crate) fn declared_viewer_gpu_output_residency(
    frame: &PreviewGpuFrame,
    platform_probe: NativeVideoTextureImportProbeResult,
) -> ViewerGpuOutputFrameResidency {
    match &frame.working_input {
        PreviewGpuWorkingInput::GpuComposite { layers } => {
            let counts =
                layers.iter().fold(DeclaredViewerLayerCounts::default(), |mut counts, layer| {
                    counts.record_execution_layer(layer);
                    counts
                });
            declared_residency_from_layer_counts(
                counts.media_layers,
                counts.gpu_input_eligible_layers,
                counts.native_media_layers,
                counts.media_layers.saturating_add(counts.procedural_layers),
                platform_probe,
            )
        }
    }
}

#[derive(Default)]
struct DeclaredViewerLayerCounts {
    media_layers: u32,
    gpu_input_eligible_layers: u32,
    native_media_layers: u32,
    procedural_layers: u32,
}

impl DeclaredViewerLayerCounts {
    fn record_execution_layer(&mut self, layer: &ViewerGpuExecutionLayer) {
        match layer {
            ViewerGpuExecutionLayer::Source(source) => {
                if source_has_contribution(source) {
                    self.record_source(source);
                }
            }
            ViewerGpuExecutionLayer::Adjustment { opacity, .. } => {
                if opacity.clamp(0.0, 1.0) > 0.0 {
                    self.procedural_layers = self.procedural_layers.saturating_add(1);
                }
            }
            ViewerGpuExecutionLayer::CrossDissolve(transition) => {
                let mondrian_renderer::ViewerGpuCrossDissolveLayer { left, right, progress } =
                    transition.as_ref();
                let progress = progress.clamp(0.0, 1.0);
                self.record_transition_input(left, 1.0 - progress);
                self.record_transition_input(right, progress);
            }
        }
    }

    fn record_transition_input(&mut self, input: &ViewerGpuTransitionInput, weight: f32) {
        if weight <= 0.0 {
            return;
        }
        let ViewerGpuTransitionInput::Source(source) = input else {
            return;
        };
        if source_has_contribution(source) {
            self.record_source(source);
        }
    }

    fn record_source(&mut self, source: &ViewerGpuSourceLayer) {
        match source {
            ViewerGpuSourceLayer::Media { gpu_source, native_source, .. } => {
                self.media_layers = self.media_layers.saturating_add(1);
                if gpu_source.is_some() {
                    self.gpu_input_eligible_layers =
                        self.gpu_input_eligible_layers.saturating_add(1);
                }
                if native_source.is_some() {
                    self.native_media_layers = self.native_media_layers.saturating_add(1);
                }
            }
            ViewerGpuSourceLayer::SolidColor { .. } => {
                self.procedural_layers = self.procedural_layers.saturating_add(1);
            }
        }
    }
}

fn source_has_contribution(source: &ViewerGpuSourceLayer) -> bool {
    let opacity = match source {
        ViewerGpuSourceLayer::Media { opacity, .. } => *opacity,
        ViewerGpuSourceLayer::SolidColor { layer, .. } => layer.opacity,
    };
    opacity.clamp(0.0, 1.0) > 0.0
}

fn declared_residency_from_layer_counts(
    media_layers: u32,
    gpu_input_eligible_layers: u32,
    native_media_layers: u32,
    total_layers: u32,
    platform_probe: NativeVideoTextureImportProbeResult,
) -> ViewerGpuOutputFrameResidency {
    let has_media = media_layers > 0;
    let has_procedural = total_layers > media_layers;
    let all_media_gpu_input_eligible = has_media && gpu_input_eligible_layers == media_layers;
    let all_media_native = has_media && native_media_layers == media_layers;
    ViewerGpuOutputFrameResidency {
        decode_residency: match (has_media, has_procedural, all_media_native) {
            (true, true, true) => ViewerGpuOutputDecodeResidency::MixedNativeGpuAndProcedural,
            (true, false, true) => ViewerGpuOutputDecodeResidency::NativeGpuDecoded,
            (true, true, false) => ViewerGpuOutputDecodeResidency::MixedCpuAndProcedural,
            (true, false, false) => ViewerGpuOutputDecodeResidency::CpuDecodedRgba,
            (false, _, _) => ViewerGpuOutputDecodeResidency::ProceduralGpuNative,
        },
        working_residency: ViewerGpuOutputWorkingResidency::GpuWorkingCompositePlanned,
        input_transform_path: match (
            all_media_native,
            all_media_gpu_input_eligible,
            has_media,
            has_procedural,
        ) {
            (true, _, true, true) => {
                ViewerGpuOutputInputTransformPath::MixedNativeVideoImportAndGpuNative
            }
            (true, _, true, false) => ViewerGpuOutputInputTransformPath::GpuNativeVideoImport,
            (false, true, true, true) => {
                ViewerGpuOutputInputTransformPath::MixedGpuOcioAndGpuNative
            }
            (false, true, true, false) => ViewerGpuOutputInputTransformPath::GpuOcio,
            (false, false, true, true) => {
                ViewerGpuOutputInputTransformPath::MixedCpuOcioAndGpuNative
            }
            (false, false, true, false) => ViewerGpuOutputInputTransformPath::CpuOcio,
            (_, _, false, _) => ViewerGpuOutputInputTransformPath::GpuNativeProcedural,
        },
        execution_observed: false,
        zero_copy: false,
        low_copy: false,
        upload_count: 0,
        readback_count: 0,
        reason: declared_residency_reason(
            has_media,
            all_media_native,
            all_media_gpu_input_eligible,
        ),
        native_video_import: native_video_import_readiness(
            has_media,
            None,
            platform_probe,
            GpuNativeDecodedFrameImportSupport::unavailable(),
        ),
    }
}

/// Lower one completed renderer execution record into residency evidence.
pub(crate) fn executed_viewer_gpu_output_residency(
    summary: ViewerGpuExecutionResidency,
    renderer_support: GpuNativeDecodedFrameImportSupport,
    platform_probe: NativeVideoTextureImportProbeResult,
) -> ViewerGpuOutputFrameResidency {
    let has_media = summary.media_layers > 0;
    let has_procedural = summary.procedural_layers > 0;
    let native_video_import = native_video_import_readiness(
        has_media,
        summary.native_video_import,
        platform_probe,
        renderer_support,
    );
    let all_media_native_gpu =
        has_media && summary.native_decoder_gpu_layers == summary.media_layers;
    let has_native_gpu_media = summary.native_decoder_gpu_layers > 0;
    let native_zero_copy_ready = native_video_import
        .as_ref()
        .map(|readiness| readiness.zero_copy_ready && all_media_native_gpu)
        .unwrap_or(false);
    ViewerGpuOutputFrameResidency {
        decode_residency: match (
            has_media,
            has_procedural,
            has_native_gpu_media,
            all_media_native_gpu,
        ) {
            (false, _, _, _) => ViewerGpuOutputDecodeResidency::ProceduralGpuNative,
            (true, false, true, true) => ViewerGpuOutputDecodeResidency::NativeGpuDecoded,
            (true, true, true, true) => ViewerGpuOutputDecodeResidency::MixedNativeGpuAndProcedural,
            (true, false, true, false) => {
                ViewerGpuOutputDecodeResidency::MixedNativeGpuAndCpuDecoded
            }
            (true, true, true, false) => {
                ViewerGpuOutputDecodeResidency::MixedNativeGpuCpuAndProcedural
            }
            (true, true, false, _) => ViewerGpuOutputDecodeResidency::MixedCpuAndProcedural,
            (true, false, false, _) => ViewerGpuOutputDecodeResidency::CpuDecodedRgba,
        },
        working_residency: ViewerGpuOutputWorkingResidency::GpuWorkingCompositeExecuted,
        input_transform_path: executed_input_transform_path(summary),
        execution_observed: true,
        zero_copy: !has_media || native_zero_copy_ready,
        low_copy: has_media && !native_zero_copy_ready,
        upload_count: summary.gpu_input_layers.saturating_add(summary.cpu_upload_layers),
        readback_count: 0,
        reason: executed_residency_reason(summary),
        native_video_import,
    }
}

fn native_video_import_readiness(
    has_media: bool,
    facts: Option<ViewerGpuNativeVideoFacts>,
    platform_probe: NativeVideoTextureImportProbeResult,
    renderer_support: GpuNativeDecodedFrameImportSupport,
) -> Option<NativeVideoImportReadiness> {
    let facts = facts.unwrap_or_default();
    has_media.then(|| {
        evaluate_native_video_import_readiness(NativeVideoImportReadinessInput {
            decoder_residency: facts.decoder_residency,
            decoder_handle_kind: facts.decoder_handle_kind,
            source_texture_format: facts.source_texture_format,
            source_video_sampling: facts.source_video_sampling,
            platform_probe,
            renderer_support,
        })
    })
}

fn executed_input_transform_path(
    summary: ViewerGpuExecutionResidency,
) -> ViewerGpuOutputInputTransformPath {
    let has_native_video = summary.native_decoder_gpu_layers > 0;
    match (
        has_native_video,
        summary.gpu_input_layers > 0,
        summary.cpu_upload_layers > 0,
        summary.procedural_layers > 0,
    ) {
        (true, false, false, false) => ViewerGpuOutputInputTransformPath::GpuNativeVideoImport,
        (true, false, false, true) => {
            ViewerGpuOutputInputTransformPath::MixedNativeVideoImportAndGpuNative
        }
        (true, _, _, _) => ViewerGpuOutputInputTransformPath::MixedInputTransforms,
        (false, false, false, true) => ViewerGpuOutputInputTransformPath::GpuNativeProcedural,
        (false, true, false, false) => ViewerGpuOutputInputTransformPath::GpuOcio,
        (false, false, true, false) => ViewerGpuOutputInputTransformPath::CpuOcio,
        (false, true, false, true) => ViewerGpuOutputInputTransformPath::MixedGpuOcioAndGpuNative,
        (false, false, true, true) => ViewerGpuOutputInputTransformPath::MixedCpuOcioAndGpuNative,
        (false, true, true, false) => ViewerGpuOutputInputTransformPath::MixedCpuOcioAndGpuOcio,
        (false, true, true, true) => ViewerGpuOutputInputTransformPath::MixedInputTransforms,
        (false, false, false, false) => ViewerGpuOutputInputTransformPath::GpuNativeProcedural,
    }
}

fn declared_residency_reason(
    has_media: bool,
    all_media_native: bool,
    all_media_gpu_input_eligible: bool,
) -> String {
    if !has_media {
        return "GPU procedural composition is planned; execution has not been observed".to_owned();
    }
    if all_media_native {
        return "Native decoded media import is planned; renderer execution and zero-copy readiness have not been observed".to_owned();
    }
    if all_media_gpu_input_eligible {
        return "GPU OCIO input is planned; upload and working-composite execution have not been observed".to_owned();
    }
    "GPU working composition is planned; media upload and fallback execution have not been observed"
        .to_owned()
}

fn executed_residency_reason(summary: ViewerGpuExecutionResidency) -> String {
    if summary.media_layers == 0 {
        return "Procedural layers were generated and composited on the GPU without media uploads"
            .to_owned();
    }
    if summary.native_decoder_gpu_layers == summary.media_layers {
        return "Native decoded media layers were GPU-resident; native import readiness determines whether execution remained zero-copy".to_owned();
    }
    if summary.native_decoder_gpu_layers > 0 {
        return format!(
            "{} media layer(s) reported native GPU decoder residency; {} media layer(s) required CPU decoded upload",
            summary.native_decoder_gpu_layers,
            summary.media_layers.saturating_sub(summary.native_decoder_gpu_layers)
        );
    }
    if summary.gpu_input_layers == summary.media_layers {
        return "CPU decoded source media uploaded once, used GPU OCIO input, and kept working/output frames GPU-resident".to_owned();
    }
    if summary.gpu_input_layers > 0 {
        return format!(
            "GPU OCIO input succeeded for {} media layer(s); {} media layer(s) used CPU working upload after {} GPU input failure(s)",
            summary.gpu_input_layers, summary.cpu_upload_layers, summary.gpu_input_failures
        );
    }
    if summary.gpu_input_failures > 0 {
        return format!(
            "GPU OCIO input failed for {} media layer(s); preview used CPU working uploads for this frame",
            summary.gpu_input_failures
        );
    }
    "GPU working composition executed with CPU working media uploads; hardware decode texture residency was not active"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::ColorSpace;
    use mondrian_media::{DecodedFrameResidency, DecodedGpuFrameHandleKind};
    use mondrian_platform::NativeVideoTextureHandleKind;
    use mondrian_renderer::{
        GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling,
        GpuVideoChromaLocation, GpuVideoRange,
    };

    fn native_platform_probe() -> NativeVideoTextureImportProbeResult {
        NativeVideoTextureImportProbeResult::found(
            vec![NativeVideoTextureHandleKind::D3D11Texture2D],
            true,
            true,
        )
    }

    fn native_renderer_support() -> GpuNativeDecodedFrameImportSupport {
        GpuNativeDecodedFrameImportSupport::ready(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        )
    }

    fn native_video_facts() -> ViewerGpuNativeVideoFacts {
        ViewerGpuNativeVideoFacts {
            decoder_residency: DecodedFrameResidency::GpuTexture,
            decoder_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            source_texture_format: Some(GpuNativeDecodedFrameTextureFormat::Nv12),
            source_video_sampling: Some(
                GpuNativeDecodedFrameVideoSampling::from_source_color_space(
                    ColorSpace::Rec709,
                    GpuVideoRange::Limited,
                    8,
                    GpuVideoChromaLocation::Left,
                ),
            ),
        }
    }

    #[test]
    fn declared_native_media_never_claims_execution_or_zero_copy() {
        let residency = declared_residency_from_layer_counts(1, 1, 1, 1, native_platform_probe());

        assert_eq!(
            residency.decode_residency,
            ViewerGpuOutputDecodeResidency::NativeGpuDecoded
        );
        assert_eq!(
            residency.working_residency,
            ViewerGpuOutputWorkingResidency::GpuWorkingCompositePlanned
        );
        assert!(!residency.execution_observed);
        assert!(!residency.zero_copy);
        assert!(!residency.low_copy);
        assert_eq!(residency.upload_count, 0);
        assert_eq!(residency.readback_count, 0);
        assert!(residency.reason.contains("have not been observed"));
    }

    #[test]
    fn executed_cpu_media_reports_observed_uploads_not_zero_copy() {
        let residency = executed_viewer_gpu_output_residency(
            ViewerGpuExecutionResidency {
                media_layers: 2,
                gpu_input_layers: 1,
                cpu_upload_layers: 1,
                gpu_input_failures: 1,
                ..ViewerGpuExecutionResidency::default()
            },
            GpuNativeDecodedFrameImportSupport::unavailable(),
            NativeVideoTextureImportProbeResult::unsupported("not required for CPU media"),
        );

        assert!(residency.execution_observed);
        assert_eq!(
            residency.working_residency,
            ViewerGpuOutputWorkingResidency::GpuWorkingCompositeExecuted
        );
        assert!(!residency.zero_copy);
        assert!(residency.low_copy);
        assert_eq!(residency.upload_count, 2);
        assert_eq!(
            residency.input_transform_path,
            ViewerGpuOutputInputTransformPath::MixedCpuOcioAndGpuOcio
        );
    }

    #[test]
    fn executed_native_media_requires_platform_and_renderer_proof_for_zero_copy() {
        let summary = ViewerGpuExecutionResidency {
            media_layers: 1,
            native_decoder_gpu_layers: 1,
            native_video_import: Some(native_video_facts()),
            ..ViewerGpuExecutionResidency::default()
        };
        let ready = executed_viewer_gpu_output_residency(
            summary,
            native_renderer_support(),
            native_platform_probe(),
        );
        assert!(ready.execution_observed);
        assert!(ready.zero_copy);
        assert!(!ready.low_copy);
        assert_eq!(ready.upload_count, 0);

        let renderer_unavailable = executed_viewer_gpu_output_residency(
            summary,
            GpuNativeDecodedFrameImportSupport::unavailable(),
            native_platform_probe(),
        );
        assert!(!renderer_unavailable.zero_copy);
        assert!(renderer_unavailable.low_copy);
    }
}
