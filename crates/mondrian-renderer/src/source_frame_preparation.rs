//! Decoded CPU source-frame admission and input-color preparation.
//!
//! Media owns decode payload evidence. This Module validates that evidence,
//! normalizes coverage, and binds the exact renderer input transform once so
//! Preview and Export cannot reinterpret the same payload independently.

use std::sync::Arc;

use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{ColorSpace, WorkingColorSpace};
use mondrian_core::WorkingRgbaF32Frame;
use mondrian_media::{DecodedRgbaEncoding, DecodedRgbaFrameContract, FloatRgbaFrame, RgbaFrame};

use crate::color_frame::{
    normalize_rgba8_alpha, normalize_rgba_f32_alpha, CpuColorFrame, CpuEncodedColorFrame,
    CpuEncodedFloatColorFrame, CpuSourceColorFrame, LinearFloatSource,
    SourceAlphaInterpretationError,
};
use crate::color_stage::execute_cpu_source_input_stage_with_session;
use crate::color_transform::{
    RenderColorTransformDiagnostics, RenderColorTransformError, RenderCpuColorExecutionSession,
    RenderInputTransform,
};

#[derive(Debug, Clone)]
enum DecodedCpuSourcePixels {
    Rgba8(Arc<Vec<u8>>),
    RgbaF32(Arc<Vec<f32>>),
}

/// One decoded CPU RGBA payload retaining its media-owned frame contract.
///
/// Callers construct this value from the typed Media payload. Pixel storage and
/// contract fields stay private so the preparation Module remains the only
/// place that classifies encoded versus linear samples.
#[derive(Debug, Clone)]
pub struct DecodedCpuSourceFrame {
    width: u32,
    height: u32,
    color_contract: DecodedRgbaFrameContract,
    pixels: DecodedCpuSourcePixels,
}

impl From<RgbaFrame> for DecodedCpuSourceFrame {
    fn from(frame: RgbaFrame) -> Self {
        let width = frame.width;
        let height = frame.height;
        let color_contract = frame.color_contract;
        Self {
            width,
            height,
            color_contract,
            pixels: DecodedCpuSourcePixels::Rgba8(frame.into_shared_data()),
        }
    }
}

impl From<FloatRgbaFrame> for DecodedCpuSourceFrame {
    fn from(frame: FloatRgbaFrame) -> Self {
        let width = frame.width;
        let height = frame.height;
        let color_contract = frame.color_contract;
        Self {
            width,
            height,
            color_contract,
            pixels: DecodedCpuSourcePixels::RgbaF32(frame.into_shared_data()),
        }
    }
}

/// How one decoded source enters the Sequence working domain.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceFramePreparationIntent {
    /// Execute the exact source-to-working color transform.
    ColorManaged(RenderInputTransform),
    /// Preserve normalized numeric channels and bypass every OCIO processor.
    DataTexture {
        /// Working identity assigned only after the explicit numeric bypass.
        working_color_space: WorkingColorSpace,
    },
}

impl From<RenderInputTransform> for SourceFramePreparationIntent {
    fn from(transform: RenderInputTransform) -> Self {
        Self::ColorManaged(transform)
    }
}

impl SourceFramePreparationIntent {
    /// Build an explicit non-color data-texture bypass intent.
    pub const fn data_texture(working_color_space: WorkingColorSpace) -> Self {
        Self::DataTexture { working_color_space }
    }

    /// Working domain produced by either execution route.
    pub const fn working_color_space(&self) -> WorkingColorSpace {
        match self {
            Self::ColorManaged(transform) => transform.working_color_space,
            Self::DataTexture { working_color_space } => *working_color_space,
        }
    }

    /// Borrow the color transform only when this is a color-managed route.
    pub const fn color_transform(&self) -> Option<&RenderInputTransform> {
        match self {
            Self::ColorManaged(transform) => Some(transform),
            Self::DataTexture { .. } => None,
        }
    }

    /// Whether the source must bypass OCIO as numeric data.
    pub const fn is_data_texture(&self) -> bool {
        matches!(self, Self::DataTexture { .. })
    }
}

/// A validated, coverage-normalized source frame bound to one exact preparation intent.
#[derive(Debug, Clone)]
pub enum PreparedSourceFrame {
    /// A color-managed source bound atomically to its source-to-working transform.
    ColorManaged {
        source: Arc<CpuSourceColorFrame>,
        transform: RenderInputTransform,
    },
    /// A normalized numeric payload that bypasses every OCIO processor.
    DataTexture {
        frame: CpuColorFrame,
        working_color_space: WorkingColorSpace,
    },
}

/// CPU working result of executing one prepared source route.
#[derive(Debug, Clone)]
pub struct PreparedSourceFrameExecution {
    /// Working frame produced by the color transform or explicit numeric bypass.
    pub frame: CpuColorFrame,
    /// OCIO execution evidence; absent exactly for a DataTexture bypass.
    pub color_diagnostics: Option<RenderColorTransformDiagnostics>,
    /// Executed renderer color stages; empty exactly for a DataTexture bypass.
    pub stage_diagnostics: crate::RenderColorStageDiagnostics,
}

impl PreparedSourceFrame {
    /// Bind an already typed and coverage-normalized renderer source.
    ///
    /// Generated sources and tests may already own a renderer frame rather than
    /// a Media decode payload. File-backed decoded media must enter through
    /// [`prepare_decoded_cpu_source_frame`] so its media contract is validated.
    pub fn from_typed_source(
        source: impl Into<CpuSourceColorFrame>,
        input_transform: RenderInputTransform,
    ) -> Self {
        Self::ColorManaged {
            source: Arc::new(source.into()),
            transform: input_transform,
        }
    }

    /// Source raster extent retained by this prepared value.
    pub fn extent(&self) -> (u32, u32) {
        let descriptor = match self {
            Self::ColorManaged { source, .. } => source.descriptor(),
            Self::DataTexture { frame, .. } => frame.descriptor(),
        };
        (descriptor.width, descriptor.height)
    }

    /// CPU bytes retained by the decoded/prepared payload.
    pub fn retained_bytes(&self) -> usize {
        match self {
            Self::ColorManaged { source, .. } => source.retained_bytes(),
            Self::DataTexture { frame, .. } => {
                frame.descriptor().pixel_count().saturating_mul(std::mem::size_of::<[f32; 4]>())
            }
        }
    }

    /// Borrow the typed color-managed source, if this route uses OCIO.
    pub fn color_managed_source(&self) -> Option<&CpuSourceColorFrame> {
        match self {
            Self::ColorManaged { source, .. } => Some(source.as_ref()),
            Self::DataTexture { .. } => None,
        }
    }

    /// Whether this prepared source is an explicit numeric DataTexture bypass.
    pub const fn is_data_texture(&self) -> bool {
        matches!(self, Self::DataTexture { .. })
    }

    /// Clone the normalized numeric payload for the compositor-owned typed
    /// DataTexture upload seam.
    ///
    /// The returned frame carries a working-space storage descriptor only so
    /// the existing float frame container can own its pixels. Consumers must
    /// preserve the accompanying DataTexture route identity and must not pass
    /// it through a color transform.
    pub fn data_texture_frame(&self) -> Option<CpuColorFrame> {
        match self {
            Self::DataTexture { frame, .. } => Some(frame.clone()),
            Self::ColorManaged { .. } => None,
        }
    }

    /// Working domain produced by this prepared route.
    pub fn working_color_space(&self) -> WorkingColorSpace {
        match self {
            Self::ColorManaged { transform, .. } => transform.working_color_space,
            Self::DataTexture { working_color_space, .. } => *working_color_space,
        }
    }

    /// Clone the GPU-capable color-managed source and transform.
    ///
    /// Data textures deliberately return `None`: their GPU route is the
    /// compositor-owned typed numeric upload exposed by
    /// [`Self::data_texture_frame`], never a color input transform.
    pub fn color_managed_gpu_input(
        &self,
    ) -> Option<(Arc<CpuSourceColorFrame>, RenderInputTransform)> {
        match self {
            Self::ColorManaged { source, transform } => {
                Some((Arc::clone(source), transform.clone()))
            }
            Self::DataTexture { .. } => None,
        }
    }

    /// Execute this prepared source through its exact CPU route.
    pub fn execute_cpu_with_session(
        &self,
        session: &mut RenderCpuColorExecutionSession,
    ) -> Result<PreparedSourceFrameExecution, RenderColorTransformError> {
        match self {
            Self::ColorManaged { source, transform } => {
                let execution = execute_cpu_source_input_stage_with_session(
                    source.as_ref(),
                    transform,
                    session,
                )?;
                Ok(PreparedSourceFrameExecution {
                    frame: execution.result.frame,
                    color_diagnostics: Some(execution.result.diagnostics),
                    stage_diagnostics: execution.stage_diagnostics,
                })
            }
            Self::DataTexture { frame, .. } => Ok(PreparedSourceFrameExecution {
                frame: frame.clone(),
                color_diagnostics: None,
                stage_diagnostics: crate::RenderColorStageDiagnostics::default(),
            }),
        }
    }
}

/// Failure to admit and normalize a decoded CPU source frame.
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum SourceFramePreparationError {
    /// The decoded extent cannot be represented as an RGBA component count.
    #[error("decoded RGBA extent {width}x{height} overflows the component count")]
    ComponentCountOverflow {
        /// Decoded width.
        width: u32,
        /// Decoded height.
        height: u32,
    },
    /// The payload length contradicts its decoded extent.
    #[error(
        "decoded RGBA component count mismatch for {width}x{height}: expected {expected}, got {actual}"
    )]
    ComponentCountMismatch {
        /// Decoded width.
        width: u32,
        /// Decoded height.
        height: u32,
        /// Required scalar component count.
        expected: usize,
        /// Actual scalar component count.
        actual: usize,
    },
    /// An eight-bit decode payload claimed scene-linear source samples.
    #[error("decoded RGBA8 payload cannot claim source-linear RGB")]
    Rgba8MarkedSourceLinear,
    /// A color-managed intent was paired with an explicit data-texture decode.
    #[error("decoded data-texture payload cannot enter a color-managed source transform")]
    DataTextureEnteredColorTransform,
    /// A DataTexture intent was paired with a color-managed decode contract.
    #[error("data-texture bypass requires an explicit decoded data-texture contract")]
    ColorPayloadEnteredDataTextureBypass,
    /// A decoded data-texture contract carried a color encoding classification.
    #[error("decoded data-texture payload must carry DataTexture encoding, got {encoding:?}")]
    InvalidDataTextureEncoding {
        /// Contradictory decoded encoding.
        encoding: DecodedRgbaEncoding,
    },
    /// A float payload claimed linear samples under a non-linear color identity.
    #[error(
        "decoded float RGBA contract marks non-linear color space {color_space:?} as source-linear"
    )]
    NonLinearIdentityMarkedSourceLinear {
        /// Contradictory source identity.
        color_space: ColorSpace,
    },
    /// Decoded coverage could not be normalized to the public straight-alpha contract.
    #[error(transparent)]
    Alpha(#[from] SourceAlphaInterpretationError),
}

/// Validate, classify, normalize, and bind one decoded CPU source frame.
pub fn prepare_decoded_cpu_source_frame(
    decoded: impl Into<DecodedCpuSourceFrame>,
    alpha_interpretation: AlphaInterpretation,
    intent: impl Into<SourceFramePreparationIntent>,
) -> Result<PreparedSourceFrame, SourceFramePreparationError> {
    let decoded = decoded.into();
    let intent = intent.into();
    let expected_components = expected_rgba_components(decoded.width, decoded.height)?;
    let source_color_space = decoded.color_contract.source.color_space();

    match (
        decoded.color_contract.source.is_data_texture(),
        intent.is_data_texture(),
    ) {
        (true, false) => {
            return Err(SourceFramePreparationError::DataTextureEnteredColorTransform);
        }
        (false, true) => {
            return Err(SourceFramePreparationError::ColorPayloadEnteredDataTextureBypass);
        }
        _ => {}
    }

    if intent.is_data_texture() {
        if decoded.color_contract.encoding != DecodedRgbaEncoding::DataTexture {
            return Err(SourceFramePreparationError::InvalidDataTextureEncoding {
                encoding: decoded.color_contract.encoding,
            });
        }
        let frame = prepare_data_texture_frame(
            decoded,
            expected_components,
            alpha_interpretation,
            intent.working_color_space(),
        )?;
        return Ok(PreparedSourceFrame::DataTexture {
            frame,
            working_color_space: intent.working_color_space(),
        });
    }

    let color_space = source_color_space
        .ok_or(SourceFramePreparationError::ColorPayloadEnteredDataTextureBypass)?;

    let source = match decoded.pixels {
        DecodedCpuSourcePixels::Rgba8(data) => {
            validate_component_count(
                decoded.width,
                decoded.height,
                expected_components,
                data.len(),
            )?;
            if decoded.color_contract.encoding != DecodedRgbaEncoding::SourceEncodedRgb {
                return Err(SourceFramePreparationError::Rgba8MarkedSourceLinear);
            }
            CpuSourceColorFrame::from(CpuEncodedColorFrame::source_rgba8_shared(
                decoded.width,
                decoded.height,
                color_space,
                data,
            ))
        }
        DecodedCpuSourcePixels::RgbaF32(data) => {
            validate_component_count(
                decoded.width,
                decoded.height,
                expected_components,
                data.len(),
            )?;
            match decoded.color_contract.encoding {
                DecodedRgbaEncoding::SourceEncodedRgb => {
                    let data =
                        Arc::try_unwrap(data).unwrap_or_else(|shared| shared.as_ref().clone());
                    CpuSourceColorFrame::from(CpuEncodedFloatColorFrame::source_flat_rgba_f32(
                        decoded.width,
                        decoded.height,
                        color_space,
                        data,
                    ))
                }
                DecodedRgbaEncoding::SourceLinearRgb => {
                    if !color_space.is_scene_linear() {
                        return Err(
                            SourceFramePreparationError::NonLinearIdentityMarkedSourceLinear {
                                color_space,
                            },
                        );
                    }
                    CpuSourceColorFrame::from(LinearFloatSource::new_shared(
                        decoded.width,
                        decoded.height,
                        color_space,
                        data,
                    ))
                }
                DecodedRgbaEncoding::DataTexture => {
                    return Err(SourceFramePreparationError::DataTextureEnteredColorTransform);
                }
            }
        }
    };
    let source = source.normalize_alpha(alpha_interpretation)?;
    let SourceFramePreparationIntent::ColorManaged(transform) = intent else {
        return Err(SourceFramePreparationError::ColorPayloadEnteredDataTextureBypass);
    };
    Ok(PreparedSourceFrame::ColorManaged { source: Arc::new(source), transform })
}

fn prepare_data_texture_frame(
    decoded: DecodedCpuSourceFrame,
    expected_components: usize,
    alpha_interpretation: AlphaInterpretation,
    working_color_space: WorkingColorSpace,
) -> Result<CpuColorFrame, SourceFramePreparationError> {
    let data = match decoded.pixels {
        DecodedCpuSourcePixels::Rgba8(shared) => {
            validate_component_count(
                decoded.width,
                decoded.height,
                expected_components,
                shared.len(),
            )?;
            let mut rgba = Arc::try_unwrap(shared).unwrap_or_else(|data| data.as_ref().clone());
            normalize_rgba8_alpha(&mut rgba, alpha_interpretation);
            rgba.chunks_exact(4)
                .map(|pixel| {
                    [
                        f32::from(pixel[0]) / 255.0,
                        f32::from(pixel[1]) / 255.0,
                        f32::from(pixel[2]) / 255.0,
                        f32::from(pixel[3]) / 255.0,
                    ]
                })
                .collect()
        }
        DecodedCpuSourcePixels::RgbaF32(shared) => {
            validate_component_count(
                decoded.width,
                decoded.height,
                expected_components,
                shared.len(),
            )?;
            let mut rgba = Arc::try_unwrap(shared).unwrap_or_else(|data| data.as_ref().clone());
            normalize_rgba_f32_alpha(&mut rgba, alpha_interpretation)?;
            bytemuck::allocation::cast_vec(rgba)
        }
    };
    Ok(CpuColorFrame::working(WorkingRgbaF32Frame {
        width: decoded.width,
        height: decoded.height,
        data,
        color_space: working_color_space,
    }))
}

fn expected_rgba_components(width: u32, height: u32) -> Result<usize, SourceFramePreparationError> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(SourceFramePreparationError::ComponentCountOverflow { width, height })
}

fn validate_component_count(
    width: u32,
    height: u32,
    expected: usize,
    actual: usize,
) -> Result<(), SourceFramePreparationError> {
    if actual == expected {
        Ok(())
    } else {
        Err(SourceFramePreparationError::ComponentCountMismatch { width, height, expected, actual })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::{ColorEngine, WorkingColorSpace};
    use mondrian_media::{
        DecodedRgbaAlphaMode, DecodedVideoMatrix, DecodedVideoRange, DecodedVideoRangeContract,
        PreviewSourceColorContract,
    };

    fn decoded_float(
        color_space: ColorSpace,
        encoding: DecodedRgbaEncoding,
        samples: Vec<f32>,
    ) -> DecodedCpuSourceFrame {
        DecodedCpuSourceFrame {
            width: 1,
            height: 1,
            color_contract: DecodedRgbaFrameContract {
                source: PreviewSourceColorContract::new(
                    color_space,
                    DecodedVideoRangeContract::OverrideFull,
                ),
                encoding,
                alpha_mode: DecodedRgbaAlphaMode::Straight,
                applied_matrix: DecodedVideoMatrix::Rgb,
                applied_range: DecodedVideoRange::Full,
            },
            pixels: DecodedCpuSourcePixels::RgbaF32(Arc::new(samples)),
        }
    }

    fn decoded_data_rgba8(samples: Vec<u8>) -> DecodedCpuSourceFrame {
        DecodedCpuSourceFrame {
            width: 1,
            height: 1,
            color_contract: DecodedRgbaFrameContract {
                source: PreviewSourceColorContract::data_texture(
                    DecodedVideoRangeContract::OverrideFull,
                ),
                encoding: DecodedRgbaEncoding::DataTexture,
                alpha_mode: DecodedRgbaAlphaMode::Straight,
                applied_matrix: DecodedVideoMatrix::Rgb,
                applied_range: DecodedVideoRange::Full,
            },
            pixels: DecodedCpuSourcePixels::Rgba8(Arc::new(samples)),
        }
    }

    fn input_transform() -> RenderInputTransform {
        RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec2020,
            false,
            ColorEngine::mondrian_standard(),
        )
    }

    #[test]
    fn encoded_and_linear_float_classification_is_owned_by_preparation() {
        let encoded = prepare_decoded_cpu_source_frame(
            decoded_float(
                ColorSpace::Rec2100Pq,
                DecodedRgbaEncoding::SourceEncodedRgb,
                vec![0.25, 0.5, 0.75, 1.0],
            ),
            AlphaInterpretation::Straight,
            input_transform(),
        )
        .expect("encoded float source");
        assert!(matches!(
            encoded.color_managed_source().expect("color-managed source"),
            CpuSourceColorFrame::EncodedFloat(_)
        ));

        let linear = prepare_decoded_cpu_source_frame(
            decoded_float(
                ColorSpace::LinearRec2020,
                DecodedRgbaEncoding::SourceLinearRgb,
                vec![-0.25, 0.5, 2.0, 1.0],
            ),
            AlphaInterpretation::Straight,
            input_transform(),
        )
        .expect("linear float source");
        let CpuSourceColorFrame::LinearFloat(linear) =
            linear.color_managed_source().expect("color-managed source")
        else {
            panic!("source-linear decode must remain linear float");
        };
        assert_eq!(linear.data(), &[-0.25, 0.5, 2.0, 1.0]);
    }

    #[test]
    fn preparation_normalizes_alpha_before_any_color_execution() {
        let prepared = prepare_decoded_cpu_source_frame(
            decoded_float(
                ColorSpace::LinearRec2020,
                DecodedRgbaEncoding::SourceLinearRgb,
                vec![0.125, 0.25, 0.5, 0.5],
            ),
            AlphaInterpretation::Premultiplied,
            input_transform(),
        )
        .expect("premultiplied source normalization");
        let CpuSourceColorFrame::LinearFloat(linear) =
            prepared.color_managed_source().expect("color-managed source")
        else {
            panic!("linear source");
        };
        assert_eq!(linear.data(), &[0.25, 0.5, 1.0, 0.5]);
    }

    #[test]
    fn prepared_encoded_source_executes_the_bound_input_transform() {
        let prepared = prepare_decoded_cpu_source_frame(
            decoded_float(
                ColorSpace::Rec2100Hlg,
                DecodedRgbaEncoding::SourceEncodedRgb,
                vec![0.25, 0.5, 0.75, 1.0],
            ),
            AlphaInterpretation::Straight,
            input_transform(),
        )
        .expect("encoded source preparation");
        let mut session = RenderCpuColorExecutionSession::new(2);
        let execution = prepared
            .execute_cpu_with_session(&mut session)
            .expect("source-to-working execution");
        assert_eq!(
            execution.frame.rgba_f32().color_space,
            WorkingColorSpace::LinearRec2020
        );
    }

    #[test]
    fn preparation_rejects_inconsistent_linear_identity_and_payload_extent() {
        let inconsistent = prepare_decoded_cpu_source_frame(
            decoded_float(
                ColorSpace::Rec2100Pq,
                DecodedRgbaEncoding::SourceLinearRgb,
                vec![0.25, 0.5, 0.75, 1.0],
            ),
            AlphaInterpretation::Straight,
            input_transform(),
        )
        .expect_err("non-linear identity cannot claim source-linear samples");
        assert_eq!(
            inconsistent,
            SourceFramePreparationError::NonLinearIdentityMarkedSourceLinear {
                color_space: ColorSpace::Rec2100Pq,
            }
        );

        let malformed = prepare_decoded_cpu_source_frame(
            decoded_float(
                ColorSpace::Rec709,
                DecodedRgbaEncoding::SourceEncodedRgb,
                vec![0.0; 3],
            ),
            AlphaInterpretation::Straight,
            input_transform(),
        )
        .expect_err("malformed component count");
        assert_eq!(
            malformed,
            SourceFramePreparationError::ComponentCountMismatch {
                width: 1,
                height: 1,
                expected: 4,
                actual: 3,
            }
        );
    }

    #[test]
    fn data_texture_bypasses_ocio_and_preserves_normalized_numeric_channels() {
        let prepared = prepare_decoded_cpu_source_frame(
            decoded_data_rgba8(vec![17, 64, 255, 128]),
            AlphaInterpretation::Straight,
            SourceFramePreparationIntent::data_texture(WorkingColorSpace::LinearRec2020),
        )
        .expect("explicit data-texture preparation");

        assert!(prepared.is_data_texture());
        assert!(prepared.color_managed_source().is_none());
        assert!(prepared.color_managed_gpu_input().is_none());
        let mut session = RenderCpuColorExecutionSession::new(0);
        let execution = prepared
            .execute_cpu_with_session(&mut session)
            .expect("numeric bypass execution");
        assert_eq!(execution.color_diagnostics, None);
        assert_eq!(
            execution.stage_diagnostics,
            crate::RenderColorStageDiagnostics::default()
        );
        assert_eq!(
            execution.frame.rgba_f32().data,
            vec![[17.0 / 255.0, 64.0 / 255.0, 1.0, 128.0 / 255.0]]
        );
        assert_eq!(
            execution.frame.rgba_f32().color_space,
            WorkingColorSpace::LinearRec2020
        );
    }

    #[test]
    fn preparation_fails_closed_when_payload_and_intent_domains_disagree() {
        let data_into_color = prepare_decoded_cpu_source_frame(
            decoded_data_rgba8(vec![0, 64, 255, 255]),
            AlphaInterpretation::Straight,
            input_transform(),
        )
        .expect_err("data texture cannot enter OCIO");
        assert_eq!(
            data_into_color,
            SourceFramePreparationError::DataTextureEnteredColorTransform
        );

        let color_into_data = prepare_decoded_cpu_source_frame(
            decoded_float(
                ColorSpace::Rec709,
                DecodedRgbaEncoding::SourceEncodedRgb,
                vec![0.0, 0.25, 1.0, 1.0],
            ),
            AlphaInterpretation::Straight,
            SourceFramePreparationIntent::data_texture(WorkingColorSpace::LinearRec2020),
        )
        .expect_err("color payload cannot enter numeric bypass");
        assert_eq!(
            color_into_data,
            SourceFramePreparationError::ColorPayloadEnteredDataTextureBypass
        );
    }
}
