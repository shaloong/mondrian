//! Exact CPU Float32 graph-value payloads and shared association algebra.

use crate::{EffectAlphaConsumer, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision};
use mondrian_core::{ColorFrameAlpha, ImageSampling};

mod exact_over;

/// Failure to materialize or consume one exact Float image representation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectFloatFrameError {
    /// The frame extent cannot be represented in this process.
    #[error("Float frame extent cannot be represented")]
    ExtentOverflow,
    /// The requested materialization cannot acquire its exact storage.
    #[error("Float frame cannot allocate {required_bytes} bytes")]
    AllocationUnavailable {
        /// Exact requested raster bytes.
        required_bytes: usize,
    },
    /// Storage does not contain one RGBA sample for every addressed pixel.
    #[error("Float frame has {actual} pixels, expected {expected}")]
    ExtentMismatch {
        /// Pixel count implied by the extent.
        expected: usize,
        /// Pixel count in supplied storage.
        actual: usize,
    },
    /// The materialization format is not Float32.
    #[error("Float frame cannot carry {precision:?} sample precision")]
    WrongPrecision {
        /// Unsupported physical sample precision.
        precision: EffectWorkingPrecision,
    },
    /// A sample is nonfinite or contradicts the declared alpha contract.
    #[error("Float frame sample {index} violates {alpha:?}")]
    InvalidSample {
        /// Zero-based row-major sample index.
        index: usize,
        /// Exact declared association and alpha domain.
        alpha: ColorFrameAlpha,
    },
    /// An operation requires a conversion with no finite destination sample.
    #[error("Float image cannot convert {from:?} to {to:?}: {reason:?}")]
    AlphaConversion {
        /// Original exact representation.
        from: ColorFrameAlpha,
        /// Requested exact representation.
        to: ColorFrameAlpha,
        /// Failure to express the requested finite representation.
        reason: EffectAlphaConversionFailure,
    },
    /// This consumer has no defined semantics for the incoming association.
    #[error("Float consumer {consumer:?} does not admit {alpha:?}")]
    ConsumerNotAdmitted {
        /// Consumer whose semantics were not defined for this representation.
        consumer: EffectAlphaConsumer,
        /// Incoming exact representation.
        alpha: ColorFrameAlpha,
    },
    /// Actual stage output contradicts its immutable provider declaration.
    #[error("processor alpha contract {contract:?} does not admit {input:?} to {output:?}")]
    ProcessorOutputContract {
        /// Declaration retained by the compiled graph.
        contract: crate::EffectAlphaContract,
        /// Exact input representation.
        input: ColorFrameAlpha,
        /// Exact staged output representation.
        output: ColorFrameAlpha,
    },
    /// The value does not satisfy the exact retained materialization admission.
    #[error("Float materialization {admitted:?} does not admit {actual:?}")]
    MaterializationAlpha {
        /// Every representation admitted by the concrete execution plan.
        admitted: crate::ColorFrameAlphaFormats,
        /// Actual canonical image association.
        actual: ColorFrameAlpha,
    },
    /// Exact graph representation differs from the required materialization.
    #[error("Float value format {actual:?} differs from materialization {expected:?}")]
    FormatMismatch {
        /// Exact representation required by the compiled materialization.
        expected: EffectValueFormat,
        /// Actual staged representation.
        actual: EffectValueFormat,
    },
    /// Pixelwise inputs address different canonical sampling grids.
    #[error("Float image sampling {actual:?} differs from required {expected:?}")]
    SamplingMismatch {
        /// Sampling of the destination or admitted input.
        expected: ImageSampling,
        /// Sampling of the incompatible image.
        actual: ImageSampling,
    },
    /// Author opacity does not satisfy its normalized, finite domain.
    #[error("Float compositor opacity is outside its finite normalized domain")]
    InvalidOpacity,
}

/// Exact reason an association conversion cannot preserve its image signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectAlphaConversionFailure {
    /// Associated emission cannot be represented as finite straight RGB.
    ZeroAlphaEmission,
    /// The requested numeric alpha domain cannot represent the original alpha.
    AlphaDomain,
    /// Multiplication or division overflows destination Float32 precision.
    NumericOverflow,
    /// The source contains nonfinite components.
    Nonfinite,
}

/// One borrowed complete Float32 graph value with an explicit representation.
#[derive(Debug, Clone, Copy)]
pub struct EffectFloatFrameView<'a> {
    extent: EffectFrameExtent,
    format: EffectValueFormat,
    sampling: ImageSampling,
    pixels: &'a [[f32; 4]],
}

impl<'a> EffectFloatFrameView<'a> {
    /// Bind pixels to one complete extent/format; execution validates samples.
    pub const fn new(
        extent: EffectFrameExtent,
        format: EffectValueFormat,
        sampling: ImageSampling,
        pixels: &'a [[f32; 4]],
    ) -> Self {
        Self { extent, format, sampling, pixels }
    }
    /// Addressed image extent.
    pub const fn extent(self) -> EffectFrameExtent {
        self.extent
    }
    /// Exact sample and alpha representation.
    pub const fn format(self) -> EffectValueFormat {
        self.format
    }
    /// Original pixel shape and current X/Y sampling density.
    pub const fn sampling(self) -> ImageSampling {
        self.sampling
    }
    /// Borrow the original samples without normalizing their channels.
    pub const fn pixels(self) -> &'a [[f32; 4]] {
        self.pixels
    }
    /// Validate borrowed storage against its complete representation.
    pub fn validate(self) -> Result<(), EffectFloatFrameError> {
        validate_storage(self.extent, self.format, self.pixels)
    }
    /// Validate borrowed storage with the owning scheduler's bounded checkpoints.
    ///
    /// This reuses the sole extent, all-lane finite and actual alpha-domain
    /// validator. It neither clones nor converts the raster. The checkpoint is
    /// polled before each 65,536-pixel block and before successful publication;
    /// its exact error is preserved without classifying it as provider failure.
    pub fn validate_controlled<E: From<EffectFloatFrameError>>(
        self,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), E> {
        require_float32(self.format)?;
        validate_float32_frame_storage_controlled(
            self.extent,
            self.format.alpha(),
            self.pixels,
            checkpoint,
        )
    }
    /// Materialize this complete value in one newly owned raster.
    pub fn to_owned(self) -> Result<EffectFloatFrame, EffectFloatFrameError> {
        EffectFloatFrame::from_samples(
            self.extent,
            self.format,
            self.sampling,
            self.pixels.iter().copied(),
        )
    }
}
impl std::ops::Deref for EffectFloatFrameView<'_> {
    type Target = [[f32; 4]];
    fn deref(&self) -> &Self::Target {
        self.pixels
    }
}

/// One owned exact CPU Float32 graph materialization.
///
/// Pixel data and its representation commit together. Inactive allocation pools
/// may retain bare vectors; every live graph value retains this complete type.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectFloatFrame {
    extent: EffectFrameExtent,
    format: EffectValueFormat,
    sampling: ImageSampling,
    pixels: Vec<[f32; 4]>,
}

impl EffectFloatFrame {
    /// Validate and bind existing owned storage without copying its raster.
    pub fn from_pixels(
        extent: EffectFrameExtent,
        format: EffectValueFormat,
        sampling: ImageSampling,
        pixels: Vec<[f32; 4]>,
    ) -> Result<Self, EffectFloatFrameError> {
        validate_storage(extent, format, &pixels)?;
        Ok(Self { extent, format, sampling, pixels })
    }
    /// Admit one existing owned raster with bounded scheduler checkpoints.
    ///
    /// Samples use the sole storage validator, with no color conversion or
    /// raster clone. On failure the supplied allocation is dropped; no frame is
    /// published and the exact scheduler error is preserved.
    pub fn from_pixels_controlled<E: From<EffectFloatFrameError>>(
        extent: EffectFrameExtent,
        format: EffectValueFormat,
        sampling: ImageSampling,
        pixels: Vec<[f32; 4]>,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        require_float32(format)?;
        validate_float32_frame_storage_controlled(extent, format.alpha(), &pixels, checkpoint)?;
        Ok(Self { extent, format, sampling, pixels })
    }
    /// Build an output while validating its samples in the same collection pass.
    pub fn from_samples(
        extent: EffectFrameExtent,
        format: EffectValueFormat,
        sampling: ImageSampling,
        samples: impl IntoIterator<Item = [f32; 4]>,
    ) -> Result<Self, EffectFloatFrameError> {
        require_float32(format)?;
        let expected = pixel_count(extent)?;
        let required_bytes = expected
            .checked_mul(std::mem::size_of::<[f32; 4]>())
            .ok_or(EffectFloatFrameError::ExtentOverflow)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(expected)
            .map_err(|_| EffectFloatFrameError::AllocationUnavailable { required_bytes })?;
        for (index, sample) in samples.into_iter().enumerate() {
            if index >= expected {
                return Err(EffectFloatFrameError::ExtentMismatch {
                    expected,
                    actual: index.saturating_add(1),
                });
            }
            validate_sample(sample, format.alpha(), index)?;
            pixels.push(sample);
        }
        if pixels.len() != expected {
            return Err(EffectFloatFrameError::ExtentMismatch { expected, actual: pixels.len() });
        }
        Ok(Self { extent, format, sampling, pixels })
    }
    /// Collect transport samples with fallible owner checkpoints and one validation pass.
    pub fn try_from_samples<E: From<EffectFloatFrameError>>(
        extent: EffectFrameExtent,
        format: EffectValueFormat,
        sampling: ImageSampling,
        samples: impl IntoIterator<Item = Result<[f32; 4], E>>,
    ) -> Result<Self, E> {
        require_float32(format)?;
        let expected = pixel_count(extent)?;
        let required_bytes = expected
            .checked_mul(std::mem::size_of::<[f32; 4]>())
            .ok_or(EffectFloatFrameError::ExtentOverflow)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(expected)
            .map_err(|_| EffectFloatFrameError::AllocationUnavailable { required_bytes })?;
        for (index, sample) in samples.into_iter().enumerate() {
            if index >= expected {
                return Err(EffectFloatFrameError::ExtentMismatch {
                    expected,
                    actual: index.saturating_add(1),
                }
                .into());
            }
            let sample = sample?;
            validate_sample(sample, format.alpha(), index)?;
            pixels.push(sample);
        }
        if pixels.len() != expected {
            return Err(
                EffectFloatFrameError::ExtentMismatch { expected, actual: pixels.len() }.into(),
            );
        }
        Ok(Self { extent, format, sampling, pixels })
    }
    /// Addressed image extent.
    pub const fn extent(&self) -> EffectFrameExtent {
        self.extent
    }
    /// Width from the sole addressed image extent.
    pub const fn width(&self) -> u32 {
        self.extent.width()
    }
    /// Height from the sole addressed image extent.
    pub const fn height(&self) -> u32 {
        self.extent.height()
    }
    /// Exact graph-value representation.
    pub const fn format(&self) -> EffectValueFormat {
        self.format
    }
    /// Original pixel shape and current X/Y sampling density.
    pub const fn sampling(&self) -> ImageSampling {
        self.sampling
    }
    /// Storage association and numeric alpha domain.
    pub const fn alpha(&self) -> ColorFrameAlpha {
        self.format.alpha()
    }
    /// Borrow all complete samples.
    pub fn pixels(&self) -> &[[f32; 4]] {
        &self.pixels
    }
    /// Mutate samples without permitting an accidental extent change.
    ///
    /// The executor validates a processor's atomic staging value before commit.
    pub fn pixels_mut(&mut self) -> &mut [[f32; 4]] {
        &mut self.pixels
    }
    /// Borrow this exact value for another invocation.
    pub fn view(&self) -> EffectFloatFrameView<'_> {
        EffectFloatFrameView::new(self.extent, self.format, self.sampling, &self.pixels)
    }
    /// Consume the complete extent, format, sampling and owned storage.
    pub fn into_parts(
        self,
    ) -> (
        EffectFrameExtent,
        EffectValueFormat,
        ImageSampling,
        Vec<[f32; 4]>,
    ) {
        (self.extent, self.format, self.sampling, self.pixels)
    }
    /// Validate a processor's complete staged result before publication.
    pub fn validate(&self) -> Result<(), EffectFloatFrameError> {
        validate_storage(self.extent, self.format, &self.pixels)
    }
    /// Consume a staged frame while converting association in its owned storage.
    ///
    /// Failure destroys the unpublished staged value. The executor retains the
    /// original borrowed input; no extra raster is allocated for this conversion.
    /// No epsilon, range clamp or zero-alpha emission erasure is performed.
    pub fn into_alpha(mut self, alpha: ColorFrameAlpha) -> Result<Self, EffectFloatFrameError> {
        if self.alpha() == alpha {
            return Ok(self);
        }
        let from = self.alpha();
        for sample in &mut self.pixels {
            *sample = convert_alpha_sample(*sample, from, alpha)?;
        }
        self.format = self.format.with_alpha(alpha);
        Ok(self)
    }
    pub(crate) fn from_validated_parts(
        extent: EffectFrameExtent,
        format: EffectValueFormat,
        sampling: ImageSampling,
        pixels: Vec<[f32; 4]>,
    ) -> Self {
        Self { extent, format, sampling, pixels }
    }
    pub(crate) fn ensure_same_sampling(
        &self,
        actual: ImageSampling,
    ) -> Result<(), EffectFloatFrameError> {
        if self.sampling != actual {
            return Err(EffectFloatFrameError::SamplingMismatch {
                expected: self.sampling,
                actual,
            });
        }
        Ok(())
    }
    pub(crate) fn allocation_mut(&mut self) -> &mut Vec<[f32; 4]> {
        &mut self.pixels
    }
    pub(crate) fn set_format_after_write(&mut self, format: EffectValueFormat) {
        self.format = format;
    }
    /// Logical owned pixel capacity, including spare Vec capacity. Cross-crate
    /// graph/native adapters use this to admit simultaneously held values.
    /// This excludes allocator overhead and is not a physical heap/RSS bound.
    pub fn retained_bytes(&self) -> usize {
        self.pixels.capacity().saturating_mul(std::mem::size_of::<[f32; 4]>())
    }
}

impl std::ops::Deref for EffectFloatFrame {
    type Target = [[f32; 4]];
    fn deref(&self) -> &Self::Target {
        self.pixels()
    }
}
impl std::ops::DerefMut for EffectFloatFrame {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.pixels_mut()
    }
}
impl<'a> IntoIterator for &'a EffectFloatFrame {
    type Item = &'a [f32; 4];
    type IntoIter = std::slice::Iter<'a, [f32; 4]>;
    fn into_iter(self) -> Self::IntoIter {
        self.pixels().iter()
    }
}
impl<'a> IntoIterator for &'a mut EffectFloatFrame {
    type Item = &'a mut [f32; 4];
    type IntoIter = std::slice::IterMut<'a, [f32; 4]>;
    fn into_iter(self) -> Self::IntoIter {
        self.pixels_mut().iter_mut()
    }
}

/// Resolve the exact output representation of the shared Normal Over operation.
pub fn normal_over_output_alpha(
    base: ColorFrameAlpha,
    source: ColorFrameAlpha,
    opacity: f32,
) -> Result<ColorFrameAlpha, EffectFloatFrameError> {
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(EffectFloatFrameError::InvalidOpacity);
    }
    if opacity == 0.0 {
        return Ok(base);
    }
    if base == ColorFrameAlpha::Opaque || (source == ColorFrameAlpha::Opaque && opacity == 1.0) {
        return Ok(ColorFrameAlpha::Opaque);
    }
    if base.is_coverage() && source.is_coverage() {
        return Ok(
            if base.is_straight_compatible() && source.is_straight_compatible() {
                ColorFrameAlpha::StraightCoverage
            } else {
                ColorFrameAlpha::PremultipliedCoverage
            },
        );
    }
    Ok(ColorFrameAlpha::PremultipliedFloat)
}

/// Consume a staged value at a consumer with defined normalized straight semantics.
///
/// A finite change of numeric/association declaration is allowed. Emission,
/// extended alpha and overflow refuse the materialization before publication.
pub fn materialize_straight_coverage(
    frame: EffectFloatFrame,
    consumer: EffectAlphaConsumer,
) -> Result<EffectFloatFrame, EffectFloatFrameError> {
    let incoming = frame.alpha();
    if !incoming.is_coverage()
        && frame.pixels().iter().any(|pixel| !(0.0..=1.0).contains(&pixel[3]))
    {
        return Err(EffectFloatFrameError::ConsumerNotAdmitted { consumer, alpha: incoming });
    }
    let target = if incoming == ColorFrameAlpha::Opaque {
        ColorFrameAlpha::Opaque
    } else {
        ColorFrameAlpha::StraightCoverage
    };
    frame.into_alpha(target)
}

/// Blend complete typed Float graph values using the sole Normal Over algebra.
pub fn blend_float_frames_in_place(
    base: &mut EffectFloatFrame,
    source: EffectFloatFrameView<'_>,
    opacity: f32,
    mode: mondrian_core::types::BlendMode,
    frame_seed: i64,
) -> Result<(), EffectFloatFrameError> {
    blend_float_frames_controlled(base, source, opacity, mode, frame_seed, &mut || Ok(()))
}

/// Execute the same complete-value blend with owner-supplied bounded checkpoints.
pub fn blend_float_frames_controlled<E: From<EffectFloatFrameError>>(
    base: &mut EffectFloatFrame,
    source: EffectFloatFrameView<'_>,
    opacity: f32,
    mode: mondrian_core::types::BlendMode,
    frame_seed: i64,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    checkpoint()?;
    if base.extent() != source.extent() {
        return Err(EffectFloatFrameError::ExtentMismatch {
            expected: base.len(),
            actual: source.pixels().len(),
        }
        .into());
    }
    if base.sampling() != source.sampling() {
        return Err(EffectFloatFrameError::SamplingMismatch {
            expected: base.sampling(),
            actual: source.sampling(),
        }
        .into());
    }
    if base.format().domain() != source.format().domain()
        || base.format().precision() != source.format().precision()
    {
        return Err(EffectFloatFrameError::FormatMismatch {
            expected: base.format(),
            actual: source.format(),
        }
        .into());
    }
    source.validate()?;
    base.validate()?;
    let output = if mode == mondrian_core::types::BlendMode::Normal {
        normal_over_output_alpha(base.alpha(), source.format().alpha(), opacity)?
    } else {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(EffectFloatFrameError::InvalidOpacity.into());
        }
        // Artistic modes use independent normalized coverage color. Their
        // finite Float extension remains an explicit product-design boundary.
        if !base.alpha().is_coverage() || !base.alpha().is_straight_compatible() {
            return Err(EffectFloatFrameError::ConsumerNotAdmitted {
                consumer: EffectAlphaConsumer::ArtisticBlend(mode),
                alpha: base.alpha(),
            }
            .into());
        }
        if !source.format().alpha().is_coverage()
            || !source.format().alpha().is_straight_compatible()
        {
            return Err(EffectFloatFrameError::ConsumerNotAdmitted {
                consumer: EffectAlphaConsumer::ArtisticBlend(mode),
                alpha: source.format().alpha(),
            }
            .into());
        }
        ColorFrameAlpha::StraightCoverage
    };
    if opacity == 0.0 {
        return Ok(());
    }
    let base_alpha = base.alpha();
    // Existing normalized independent-color graphs retain their canonical
    // coverage primitive. Complete Float/associated values use the extended
    // finite polynomial; there is no second raster interpretation.
    let coverage_normal = mode == mondrian_core::types::BlendMode::Normal
        && base_alpha.is_coverage()
        && base_alpha.is_straight_compatible()
        && source.format().alpha().is_coverage()
        && source.format().alpha().is_straight_compatible();
    for (index, (destination, source_pixel)) in
        base.pixels_mut().iter_mut().zip(source.pixels()).enumerate()
    {
        if index % 4096 == 0 {
            checkpoint()?;
        }
        *destination = if coverage_normal {
            let mut pixel =
                crate::blend_rgba_f32_pixel_seeded(*destination, *source_pixel, opacity, mode, 0);
            if output == ColorFrameAlpha::Opaque {
                pixel[3] = 1.0;
            }
            pixel
        } else if mode == mondrian_core::types::BlendMode::Normal {
            convert_alpha_sample(
                over_float_sample(
                    *destination,
                    base_alpha,
                    *source_pixel,
                    source.format().alpha(),
                    opacity,
                )?,
                ColorFrameAlpha::PremultipliedFloat,
                output,
            )?
        } else {
            crate::blend_rgba_f32_pixel_seeded(
                *destination,
                *source_pixel,
                opacity,
                mode,
                crate::execution::effect_graph_dither_seed(index as u32, frame_seed),
            )
        };
    }
    base.set_format_after_write(base.format().with_alpha(output));
    base.validate()?;
    checkpoint()
}

/// Materialize one concrete route frontier as exact normalized straight RGBA.
///
/// The retained provider has already completed. Failure means this route cannot
/// publish the value; it never means the provider or its valid instance failed.
pub fn materialize_finite_coverage_controlled<E: From<EffectFloatFrameError>>(
    mut frame: EffectFloatFrame,
    consumer: EffectAlphaConsumer,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<EffectFloatFrame, E> {
    checkpoint()?;
    let incoming = frame.alpha();
    for (index, sample) in frame.pixels_mut().iter_mut().enumerate() {
        if index % 4096 == 0 {
            checkpoint()?;
        }
        if !(0.0..=1.0).contains(&sample[3]) {
            return Err(
                EffectFloatFrameError::ConsumerNotAdmitted { consumer, alpha: incoming }.into(),
            );
        }
        *sample = convert_alpha_sample(*sample, incoming, ColorFrameAlpha::StraightCoverage)?;
    }
    frame.set_format_after_write(frame.format().with_alpha(ColorFrameAlpha::StraightCoverage));
    checkpoint()?;
    Ok(frame)
}

fn pixel_count(extent: EffectFrameExtent) -> Result<usize, EffectFloatFrameError> {
    (extent.width() as usize)
        .checked_mul(extent.height() as usize)
        .ok_or(EffectFloatFrameError::ExtentOverflow)
}
fn require_float32(format: EffectValueFormat) -> Result<(), EffectFloatFrameError> {
    if format.precision() != EffectWorkingPrecision::Float32 {
        return Err(EffectFloatFrameError::WrongPrecision { precision: format.precision() });
    }
    Ok(())
}
// Generic frame scans are instantiated in consumers. Make this small numeric
// predicate available there instead of relying on ThinLTO to import it into
// each pixel iteration across the crate boundary.
#[inline]
fn validate_sample(
    sample: [f32; 4],
    alpha: ColorFrameAlpha,
    index: usize,
) -> Result<(), EffectFloatFrameError> {
    if !sample.iter().all(|channel| channel.is_finite()) || !alpha.admits_alpha(sample[3]) {
        return Err(EffectFloatFrameError::InvalidSample { index, alpha });
    }
    Ok(())
}
/// Admit borrowed Float32 RGBA storage using its exact extent and declared alpha.
///
/// This allocation-free check is shared by complete Effects values and raw GPU
/// transfers, including non-color data. It requires finite values in every lane
/// and the declared alpha domain without clamping, conversion, color-space
/// interpretation, or mutation. A successful check does not admit a consumer
/// whose color or association contract excludes this representation.
pub fn validate_float32_frame_storage(
    extent: EffectFrameExtent,
    alpha: ColorFrameAlpha,
    pixels: &[[f32; 4]],
) -> Result<(), EffectFloatFrameError> {
    validate_float32_frame_storage_controlled(extent, alpha, pixels, &mut || Ok(()))
}

fn validate_float32_frame_storage_controlled<E: From<EffectFloatFrameError>>(
    extent: EffectFrameExtent,
    alpha: ColorFrameAlpha,
    pixels: &[[f32; 4]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let expected = pixel_count(extent)?;
    if pixels.len() != expected {
        return Err(
            EffectFloatFrameError::ExtentMismatch { expected, actual: pixels.len() }.into(),
        );
    }
    for (block_index, block) in pixels.chunks(65_536).enumerate() {
        checkpoint()?;
        for (local_index, sample) in block.iter().copied().enumerate() {
            validate_sample(sample, alpha, block_index * 65_536 + local_index)?;
        }
    }
    checkpoint()
}

fn validate_storage(
    extent: EffectFrameExtent,
    format: EffectValueFormat,
    pixels: &[[f32; 4]],
) -> Result<(), EffectFloatFrameError> {
    require_float32(format)?;
    validate_float32_frame_storage(extent, format.alpha(), pixels)
}

/// Convert one exact Float32 association without silently changing alpha.
pub fn convert_alpha_sample(
    sample: [f32; 4],
    from: ColorFrameAlpha,
    to: ColorFrameAlpha,
) -> Result<[f32; 4], EffectFloatFrameError> {
    let failure = |reason| EffectFloatFrameError::AlphaConversion { from, to, reason };
    if !sample.iter().all(|channel| channel.is_finite()) {
        return Err(failure(EffectAlphaConversionFailure::Nonfinite));
    }
    if !from.admits_alpha(sample[3]) || !to.admits_alpha(sample[3]) {
        return Err(failure(EffectAlphaConversionFailure::AlphaDomain));
    }
    if from.is_premultiplied() == to.is_premultiplied() {
        return Ok(sample);
    }
    let alpha = sample[3];
    let mut out = sample;
    if from.is_premultiplied() {
        if alpha == 0.0 {
            if sample[..3].iter().any(|channel| *channel != 0.0) {
                return Err(failure(EffectAlphaConversionFailure::ZeroAlphaEmission));
            }
            return Ok([0.0, 0.0, 0.0, alpha]);
        }
        for channel in &mut out[..3] {
            *channel /= alpha;
        }
    } else {
        for channel in &mut out[..3] {
            *channel *= alpha;
        }
    }
    if !out.iter().all(|channel| channel.is_finite()) {
        return Err(failure(EffectAlphaConversionFailure::NumericOverflow));
    }
    Ok(out)
}

/// Composite one finite Float image sample with the shared associated Over law.
///
/// Opacity is normalized author opacity; image alpha remains in its declared
/// domain. Zero-alpha associated RGB contributes light. An opaque background
/// produces exact opaque alpha. This function defines the NLE's finite Float
/// extension; it does not infer an OFX-mandated artistic blend algorithm.
pub fn over_float_sample(
    base: [f32; 4],
    base_alpha: ColorFrameAlpha,
    source: [f32; 4],
    source_alpha: ColorFrameAlpha,
    opacity: f32,
) -> Result<[f32; 4], EffectFloatFrameError> {
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(EffectFloatFrameError::InvalidOpacity);
    }
    validate_sample(base, base_alpha, 0)?;
    validate_sample(source, source_alpha, 0)?;
    // The sample primitive delivers associated RGB even at zero opacity;
    // the frame compositor owns its no-op identity path. Its exact dyadic
    // polynomial has one final nearest-even Float32 rounding, including
    // when cancellation spans more than two Float64 exponent levels.
    let out = exact_over::sample(base, base_alpha, source, source_alpha, opacity);
    if !out.iter().all(|channel| channel.is_finite()) {
        return Err(EffectFloatFrameError::AlphaConversion {
            from: source_alpha,
            to: ColorFrameAlpha::PremultipliedFloat,
            reason: EffectAlphaConversionFailure::NumericOverflow,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_storage_admission_preserves_exact_borrowed_float_bits_and_alpha_domain() {
        let extent = EffectFrameExtent::new(2, 1);
        for (alpha, samples) in [
            (
                ColorFrameAlpha::Opaque,
                [[f32::MAX, -f32::MAX, -0.0, 1.0], [16.0, -2.0, 0.5, 1.0]],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [[f32::MAX, -f32::MAX, -0.0, 0.0], [16.0, -2.0, 0.5, 1.0]],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [[f32::MAX, -f32::MAX, -0.0, -0.5], [16.0, -2.0, 0.5, 1.5]],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [[1.0, -2.0, -0.0, 0.0], [16.0, -2.0, 0.5, 1.5]],
            ),
        ] {
            let before = samples.map(|pixel| pixel.map(f32::to_bits));
            let address = samples.as_ptr();
            validate_float32_frame_storage(extent, alpha, &samples)
                .expect("finite raw storage does not require a color identity or normalize alpha");
            assert_eq!(samples.as_ptr(), address);
            assert_eq!(samples.map(|pixel| pixel.map(f32::to_bits)), before);
            let format = EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                alpha,
            );
            EffectFloatFrameView::new(
                extent,
                format,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &samples,
            )
            .validate()
            .expect("the existing complete-frame validator shares the same storage contract");
        }
    }

    #[test]
    fn gpu_storage_admission_refuses_extent_nonfinite_and_invalid_declared_alpha() {
        let extent = EffectFrameExtent::new(2, 1);
        let valid = [[0.25, -2.0, 8.0, 1.0], [0.5, -0.0, 16.0, 0.5]];
        assert!(matches!(
            validate_float32_frame_storage(extent, ColorFrameAlpha::StraightCoverage, &valid[..1]),
            Err(EffectFloatFrameError::ExtentMismatch { expected: 2, actual: 1 })
        ));
        for lane in 0..4 {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut samples = valid;
                samples[1][lane] = value;
                let before = samples.map(|pixel| pixel.map(f32::to_bits));
                assert!(matches!(
                    validate_float32_frame_storage(
                        extent,
                        ColorFrameAlpha::StraightCoverage,
                        &samples
                    ),
                    Err(EffectFloatFrameError::InvalidSample {
                        index: 1,
                        alpha: ColorFrameAlpha::StraightCoverage
                    })
                ));
                assert_eq!(samples.map(|pixel| pixel.map(f32::to_bits)), before);
            }
        }
        for alpha in [
            ColorFrameAlpha::Opaque,
            ColorFrameAlpha::StraightCoverage,
            ColorFrameAlpha::PremultipliedCoverage,
        ] {
            for value in [-0.5, 1.5] {
                let sample = [[0.25, -2.0, 8.0, value]];
                assert!(
                    matches!(validate_float32_frame_storage(EffectFrameExtent::new(1, 1), alpha, &sample),
                    Err(EffectFloatFrameError::InvalidSample { index: 0, alpha: actual }) if actual == alpha)
                );
            }
        }
        assert!(validate_float32_frame_storage(
            EffectFrameExtent::new(1, 1),
            ColorFrameAlpha::Opaque,
            &[[0.25, 0.5, 0.75, 0.5]]
        )
        .is_err());
    }
}

#[cfg(test)]
mod controlled_storage_tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Stop {
        Canceled,
        Frame(EffectFloatFrameError),
    }
    impl From<EffectFloatFrameError> for Stop {
        fn from(value: EffectFloatFrameError) -> Self {
            Self::Frame(value)
        }
    }

    #[test]
    fn controlled_frame_admission_keeps_one_owned_allocation_and_raw_bits() {
        let extent = EffectFrameExtent::new(1, 1);
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::PremultipliedFloat,
        );
        let pixels = vec![[7.0, -0.0, -f32::from_bits(1), 0.0]];
        let pointer = pixels.as_ptr();
        let expected = pixels[0].map(f32::to_bits);
        let frame = EffectFloatFrame::from_pixels_controlled(
            extent,
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            pixels,
            &mut || Ok::<(), Stop>(()),
        )
        .expect("canonical raw admission");
        assert_eq!(
            frame.pixels().as_ptr(),
            pointer,
            "binding does not allocate or clone the raster"
        );
        assert_eq!(frame.pixels()[0].map(f32::to_bits), expected);
        assert_eq!(frame.alpha(), ColorFrameAlpha::PremultipliedFloat);
    }

    #[test]
    fn controlled_frame_admission_has_bounded_stops_and_original_global_invalid_index() {
        let extent = EffectFrameExtent::new(1024, 65);
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightFloat,
        );
        for stop_at in 1..=3 {
            let pixels = vec![[0.25, -2.0, 8.0, 1.5]; 1024 * 65];
            let mut calls = 0;
            let result = EffectFloatFrame::from_pixels_controlled(
                extent,
                format,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                pixels,
                &mut || {
                    calls += 1;
                    if calls == stop_at {
                        Err(Stop::Canceled)
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result, Err(Stop::Canceled));
            assert_eq!(calls, stop_at);
        }
        let mut pixels = vec![[0.25, -2.0, 8.0, 1.5]; 1024 * 65];
        pixels[65_536][2] = f32::INFINITY;
        assert!(matches!(
            EffectFloatFrame::from_pixels_controlled(
                extent,
                format,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                pixels,
                &mut || Ok::<(), Stop>(())
            ),
            Err(Stop::Frame(EffectFloatFrameError::InvalidSample {
                index: 65_536,
                alpha: ColorFrameAlpha::StraightFloat
            }))
        ));
    }

    #[test]
    fn controlled_borrowed_frame_admission_preserves_pointer_bits_and_checkpoint_errors() {
        let extent = EffectFrameExtent::new(65_537, 1);
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::PremultipliedFloat,
        );
        let pixels = vec![[7.0, -0.0, -f32::from_bits(1), -2.0]; 65_537];
        let view = EffectFloatFrameView::new(
            extent,
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &pixels,
        );
        let pointer = view.pixels().as_ptr();
        let original = pixels[0].map(f32::to_bits);
        let mut calls = 0;
        view.validate_controlled(&mut || {
            calls += 1;
            Ok::<(), Stop>(())
        })
        .expect("borrowed complete-frame admission");
        assert_eq!(calls, 3);
        assert_eq!(view.pixels().as_ptr(), pointer);
        assert_eq!(view.pixels()[0].map(f32::to_bits), original);
        assert_eq!(view.format().alpha(), ColorFrameAlpha::PremultipliedFloat);
        for stop_at in 1..=3 {
            let mut calls = 0;
            let result = view.validate_controlled(&mut || {
                calls += 1;
                if calls == stop_at {
                    Err(Stop::Canceled)
                } else {
                    Ok(())
                }
            });
            assert_eq!(result, Err(Stop::Canceled));
            assert_eq!(calls, stop_at);
        }
    }

    #[test]
    fn controlled_borrowed_frame_admission_uses_exact_precision_extent_and_global_sample_index() {
        let extent = EffectFrameExtent::new(65_537, 1);
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightFloat,
        );
        let mut pixels = vec![[0.25, -2.0, 8.0, 1.5]; 65_537];
        pixels[65_536][2] = f32::INFINITY;
        assert!(matches!(
            EffectFloatFrameView::new(
                extent,
                format,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &pixels
            )
            .validate_controlled(&mut || Ok::<(), Stop>(())),
            Err(Stop::Frame(EffectFloatFrameError::InvalidSample {
                index: 65_536,
                alpha: ColorFrameAlpha::StraightFloat,
            }))
        ));
        let mut calls = 0;
        assert!(matches!(
            EffectFloatFrameView::new(
                extent,
                format,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &pixels[..65_536]
            )
            .validate_controlled(&mut || {
                calls += 1;
                Ok::<(), Stop>(())
            }),
            Err(Stop::Frame(EffectFloatFrameError::ExtentMismatch {
                expected: 65_537,
                actual: 65_536,
            }))
        ));
        assert_eq!(calls, 0);
        let wrong = EffectValueFormat::new(
            EffectWorkingPrecision::Float16,
            crate::EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightFloat,
        );
        assert!(matches!(
            EffectFloatFrameView::new(
                extent,
                wrong,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &pixels
            )
            .validate_controlled(&mut || Ok::<(), Stop>(())),
            Err(Stop::Frame(EffectFloatFrameError::WrongPrecision {
                precision: EffectWorkingPrecision::Float16,
            }))
        ));
    }
}

/// Interpolate a processed Effect result with its original complete Float image.
/// RGB is associated in the existing declared alpha domains before mixing;
/// output association is declared from those two formats, never guessed from
/// samples. Extended finite alpha is not clamped to normalized coverage.
/// Association and interpolation use bounded exact dyadic arithmetic. Straight
/// delivery divides the exact numerator by exact alpha before one nearest-even
/// Float32 rounding; tiny amounts and cancellation do not lose residuals.
/// The base allocation is reused; admission and bounded cancellation precede
/// writes, and a stopped/invalid partial owner is never a completed image.
pub fn mix_float_frames_controlled<E: From<EffectFloatFrameError>>(
    base: &mut EffectFloatFrame,
    processed: EffectFloatFrameView<'_>,
    amount: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    checkpoint()?;
    if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
        return Err(EffectFloatFrameError::InvalidOpacity.into());
    }
    if base.extent() != processed.extent() {
        return Err(EffectFloatFrameError::ExtentMismatch {
            expected: base.len(),
            actual: processed.pixels().len(),
        }
        .into());
    }
    if base.sampling() != processed.sampling() {
        return Err(EffectFloatFrameError::SamplingMismatch {
            expected: base.sampling(),
            actual: processed.sampling(),
        }
        .into());
    }
    if base.format().domain() != processed.format().domain()
        || base.format().precision() != processed.format().precision()
    {
        return Err(EffectFloatFrameError::FormatMismatch {
            expected: base.format(),
            actual: processed.format(),
        }
        .into());
    }
    base.view().validate_controlled(checkpoint)?;
    processed.validate_controlled(checkpoint)?;
    if amount == 0.0 {
        return checkpoint();
    }
    if amount == 1.0 {
        for (index, (left, right)) in
            base.pixels_mut().iter_mut().zip(processed.pixels()).enumerate()
        {
            if index % 4096 == 0 {
                checkpoint()?;
            }
            *left = *right;
        }
        base.set_format_after_write(processed.format());
        return checkpoint();
    }
    let left_alpha = base.alpha();
    let right_alpha = processed.format().alpha();
    let output = if left_alpha == right_alpha {
        left_alpha
    } else if left_alpha.is_coverage() && right_alpha.is_coverage() {
        if left_alpha.is_straight_compatible() && right_alpha.is_straight_compatible() {
            ColorFrameAlpha::StraightCoverage
        } else {
            ColorFrameAlpha::PremultipliedCoverage
        }
    } else if left_alpha.is_straight_compatible() && right_alpha.is_straight_compatible() {
        ColorFrameAlpha::StraightFloat
    } else {
        ColorFrameAlpha::PremultipliedFloat
    };
    for (index, (left, right)) in base.pixels_mut().iter_mut().zip(processed.pixels()).enumerate() {
        if index % 4096 == 0 {
            checkpoint()?;
        }
        if left_alpha == right_alpha && *left == *right {
            continue;
        }
        // Evaluate B + t*S - t*B without first rounding 1-t or an
        // association. Straight delivery divides the exact dyadic values,
        // then rounds only the final quotient to nearest-even Float32.
        let value = exact_over::mix_sample(*left, left_alpha, *right, right_alpha, amount, output)
            .ok_or_else(|| {
                E::from(EffectFloatFrameError::AlphaConversion {
                    from: ColorFrameAlpha::PremultipliedFloat,
                    to: output,
                    reason: EffectAlphaConversionFailure::ZeroAlphaEmission,
                })
            })?;
        if !value.iter().all(|channel| channel.is_finite()) || !output.admits_alpha(value[3]) {
            return Err(EffectFloatFrameError::AlphaConversion {
                from: ColorFrameAlpha::PremultipliedFloat,
                to: output,
                reason: EffectAlphaConversionFailure::NumericOverflow,
            }
            .into());
        }
        *left = value;
    }
    base.set_format_after_write(base.format().with_alpha(output));
    base.view().validate_controlled(checkpoint)?;
    checkpoint()
}

#[cfg(test)]
mod effect_pass_mix_float_contracts {
    use super::*;
    fn format(alpha: ColorFrameAlpha) -> EffectValueFormat {
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            alpha,
        )
    }
    fn owned(pixel: [f32; 4], alpha: ColorFrameAlpha) -> EffectFloatFrame {
        EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(1, 1),
            format(alpha),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![pixel],
        )
        .expect("actual owner")
    }
    // Independent high-precision mathematical image reference; it does not call
    // an adjustment, compositor, alpha converter or shader implementation.
    fn reference(
        left: [f32; 4],
        right: [f32; 4],
        la: ColorFrameAlpha,
        ra: ColorFrameAlpha,
        amount: f32,
        output: ColorFrameAlpha,
    ) -> [f64; 4] {
        let t = f64::from(amount);
        let a = f64::from(left[3]) * (1.0 - t) + f64::from(right[3]) * t;
        let associated = std::array::from_fn::<_, 3, _>(|c| {
            let l = if la.is_premultiplied() {
                f64::from(left[c])
            } else {
                f64::from(left[c]) * f64::from(left[3])
            };
            let r = if ra.is_premultiplied() {
                f64::from(right[c])
            } else {
                f64::from(right[c]) * f64::from(right[3])
            };
            l * (1.0 - t) + r * t
        });
        let rgb = associated.map(|value| {
            if output.is_premultiplied() {
                value
            } else if a != 0.0 {
                value / a
            } else {
                0.0
            }
        });
        [rgb[0], rgb[1], rgb[2], a]
    }
    #[test]
    fn effect_pass_mix_float_corpus_matches_independent_associated_reference() {
        use ColorFrameAlpha::*;
        let cases = [
            (
                [-4.0, 8.0, 0.25, 0.25],
                [2.0, -1.0, 7.0, 0.75],
                StraightCoverage,
                StraightCoverage,
                StraightCoverage,
            ),
            (
                [8.0, -4.0, 0.125, 1.0 / 65535.0],
                [-1.0, 3.0, 4.0, 1.0 / 32768.0],
                StraightCoverage,
                StraightCoverage,
                StraightCoverage,
            ),
            (
                [1.0, -2.0, 3.0, 0.0],
                [4.0, 5.0, -6.0, 0.5],
                PremultipliedCoverage,
                PremultipliedCoverage,
                PremultipliedCoverage,
            ),
            (
                [-2.0, 4.0, 1.0, 1.0],
                [8.0, -1.0, 3.0, 0.0],
                Opaque,
                StraightCoverage,
                StraightCoverage,
            ),
            (
                [1.0e30, -1.0e29, 7.0, 2.5],
                [2.0e30, -3.0e29, 5.0, 1.5],
                StraightFloat,
                StraightFloat,
                StraightFloat,
            ),
            (
                [8.0, -2.0, 4.0, -2.0],
                [2.0, -1.0, 3.0, 3.0],
                PremultipliedFloat,
                PremultipliedFloat,
                PremultipliedFloat,
            ),
        ];
        for (left, right, la, ra, output) in cases {
            for t in [0.125, 0.4, 0.875] {
                let source = owned(right, ra);
                let mut base = owned(left, la);
                let address = base.pixels().as_ptr();
                mix_float_frames_controlled(&mut base, source.view(), t, &mut || {
                    Ok::<(), EffectFloatFrameError>(())
                })
                .expect("complete typed Mix");
                assert_eq!(
                    base.pixels().as_ptr(),
                    address,
                    "same admitted base allocation"
                );
                assert_eq!(base.alpha(), output, "declared representation");
                let expected = reference(left, right, la, ra, t, output);
                for (actual, expected) in base.pixels()[0].into_iter().zip(expected) {
                    assert!(
                        (f64::from(actual) - expected).abs()
                            <= 8.0 * f64::from(f32::EPSILON) * expected.abs().max(1.0)
                    );
                }
            }
        }
    }
    #[test]
    fn effect_pass_mix_float_identity_and_endpoints_preserve_exact_declared_samples() {
        for (alpha, pixel) in [
            (ColorFrameAlpha::StraightCoverage, [-0.0, 3.0, -7.0, 0.0]),
            (
                ColorFrameAlpha::StraightFloat,
                [f32::MAX, -f32::MAX, 0.25, 2.5],
            ),
            (ColorFrameAlpha::PremultipliedFloat, [4.0, -8.0, 2.0, 0.0]),
        ] {
            for t in [0.0, 0.125, 0.4, 1.0] {
                let mut base = owned(pixel, alpha);
                let source = owned(pixel, alpha);
                mix_float_frames_controlled(&mut base, source.view(), t, &mut || {
                    Ok::<(), EffectFloatFrameError>(())
                })
                .expect("identity law");
                assert_eq!(base.pixels()[0].map(f32::to_bits), pixel.map(f32::to_bits));
                assert_eq!(base.alpha(), alpha);
            }
        }
        let left = owned([1.0, -2.0, 3.0, 0.5], ColorFrameAlpha::StraightCoverage);
        let right = owned(
            [4.0, -5.0, 6.0, 0.25],
            ColorFrameAlpha::PremultipliedCoverage,
        );
        for t in [0.0, 1.0] {
            let mut base = left.clone();
            mix_float_frames_controlled(&mut base, right.view(), t, &mut || {
                Ok::<(), EffectFloatFrameError>(())
            })
            .expect("exact endpoint");
            assert_eq!(
                base,
                if t == 0.0 {
                    left.clone()
                } else {
                    right.clone()
                }
            );
        }
    }
    #[test]
    fn effect_pass_mix_float_transparency_emission_and_invalid_input_are_explicit() {
        let source = owned([7.0, -3.0, 4.0, 0.0], ColorFrameAlpha::StraightCoverage);
        let mut base = owned([2.0, 5.0, -6.0, 0.0], ColorFrameAlpha::StraightCoverage);
        mix_float_frames_controlled(&mut base, source.view(), 0.5, &mut || {
            Ok::<(), EffectFloatFrameError>(())
        })
        .expect("zero-associated mixture");
        assert_eq!(base.pixels()[0], [0.0; 4]);
        let source = owned([3.0, 0.0, 0.0, 1.0], ColorFrameAlpha::StraightFloat);
        let mut base = owned([1.0, 0.0, 0.0, -1.0], ColorFrameAlpha::StraightFloat);
        assert!(matches!(
            mix_float_frames_controlled(&mut base, source.view(), 0.5, &mut || Ok::<
                (),
                EffectFloatFrameError,
            >(())),
            Err(EffectFloatFrameError::AlphaConversion {
                reason: EffectAlphaConversionFailure::ZeroAlphaEmission,
                ..
            })
        ));
        let source = owned([4.0, 0.0, 0.0, 0.5], ColorFrameAlpha::StraightCoverage);
        let mut base = owned([2.0, 0.0, 0.0, 0.5], ColorFrameAlpha::StraightCoverage);
        let original = base.clone();
        assert_eq!(
            mix_float_frames_controlled(&mut base, source.view(), f32::NAN, &mut || Ok::<
                (),
                EffectFloatFrameError,
            >(
                ()
            )),
            Err(EffectFloatFrameError::InvalidOpacity)
        );
        assert_eq!(base, original, "invalid amount fails before writing");
    }
    #[test]
    fn effect_pass_mix_float_control_stop_precedes_invalid_tail_and_any_write() {
        #[derive(Debug, PartialEq)]
        enum Stop {
            Cancelled,
            Frame(EffectFloatFrameError),
        }
        impl From<EffectFloatFrameError> for Stop {
            fn from(e: EffectFloatFrameError) -> Self {
                Self::Frame(e)
            }
        }
        let extent = EffectFrameExtent::new(10000, 1);
        let mut source = vec![[1.0, 2.0, 3.0, 0.5]; 10000];
        source[9999][0] = f32::NAN;
        let source = EffectFloatFrameView::new(
            extent,
            format(ColorFrameAlpha::StraightCoverage),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &source,
        );
        let mut base = EffectFloatFrame::from_pixels(
            extent,
            format(ColorFrameAlpha::StraightCoverage),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[4.0, 5.0, 6.0, 0.5]; 10000],
        )
        .expect("admitted owner");
        let before = base.clone();
        let mut calls = 0;
        let result = mix_float_frames_controlled(&mut base, source, 0.4, &mut || {
            calls += 1;
            if calls == 2 {
                Err(Stop::Cancelled)
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err(Stop::Cancelled));
        assert_eq!(base, before);
    }
}

#[cfg(test)]
mod effect_pass_mix_exact_contracts {
    use super::*;

    fn mix(left: [f32; 4], right: [f32; 4], alpha: ColorFrameAlpha, amount: f32) -> [u32; 4] {
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            alpha,
        );
        let mut base = EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(1, 1),
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![left],
        )
        .expect("independent finite typed input");
        let source_pixels = [right];
        let source = EffectFloatFrameView::new(
            EffectFrameExtent::new(1, 1),
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &source_pixels,
        );
        let original = base.pixels().as_ptr();
        mix_float_frames_controlled(&mut base, source, amount, &mut || {
            Ok::<(), EffectFloatFrameError>(())
        })
        .expect("legal finite exact Mix");
        assert_eq!(
            base.pixels().as_ptr(),
            original,
            "reuse the original admitted owner"
        );
        assert_eq!(base.alpha(), alpha, "preserve the declared output domain");
        base.pixels()[0].map(f32::to_bits)
    }

    #[test]
    fn effect_pass_mix_exact_extended_alpha_retains_tiny_weight_cancellation() {
        let huge = f32::from_bits(0x6480_0000); // 2^74
        let tiny = f32::from_bits(0x1a00_0000); // 2^-75
                                                // Exact associated RGB is -1/2; exact alpha is 2-2^-75.
                                                // The exact rational quotient rounds to -1/4, and alpha rounds to 2.
                                                // These fixed bits were derived with integer/Fraction arithmetic;
                                                // this oracle calls no blending, conversion, or approximate lerp.
        assert_eq!(
            mix(
                [huge, -huge, 0.0, 1.0],
                [-huge, huge, 0.0, f32::from_bits(0x6500_0000)],
                ColorFrameAlpha::StraightFloat,
                tiny
            ),
            [0xbe80_0000, 0x3e80_0000, 0, 0x4000_0000]
        );
    }

    #[test]
    fn effect_pass_mix_exact_coverage_retains_tiny_weight_cancellation() {
        let huge = f32::from_bits(0x6480_0000); // 2^74
        let tiny = f32::from_bits(0x1a00_0000); // 2^-75
                                                // Associated RGB=-2^-76; alpha=2^-74-2^-150.
                                                // Both inputs remain in normalized Coverage, with finite HDR RGB.
        assert_eq!(
            mix(
                [huge, -huge, 0.0, tiny],
                [-huge, huge, 0.0, 1.0],
                ColorFrameAlpha::StraightCoverage,
                tiny
            ),
            [0xbe80_0000, 0x3e80_0000, 0, 0x1a80_0000]
        );
    }
}

#[cfg(test)]
mod effect_pass_mix_exact_fraction_contracts {
    use super::*;

    #[test]
    fn effect_pass_mix_exact_fraction_corpus_preserves_full_finite_domains() {
        // Fixed expected bits come from a separate exact Fraction evaluator
        // and a binary search of Float32 values with exact midpoint ties.
        // No Float64 reference, compositor, or production divider is used.
        let cases = [
            (
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x80800000, 0x80800000, 0x3f800000],
                [0x80000000, 0xbf800000, 0x3f800000, 0x00800000],
                0x3f7fffff,
                [0xe4800000, 0x8c800000, 0x0c7ffffe, 0x33800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x7f7fffff, 0x00800000, 0x00000000],
                [0xe4800000, 0x7f7fffff, 0x7f7fffff, 0x3f800000],
                0x3f600000,
                [0xe4800000, 0x7f7fffff, 0x7f7fffff, 0x3f600000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0xe4800000, 0x3e800000, 0x3f400000],
                [0xff7fffff, 0x00000000, 0xe4800000, 0x3f400000],
                0x3ecccccd,
                [0xfecccccc, 0xe419999a, 0xe3cccccd, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x00800000, 0xbe800000, 0x80000001, 0x00000001],
                [0xc0400000, 0x7f7fffff, 0x00000001, 0x3f400000],
                0x00800000,
                [0xc03ffffe, 0x7f7ffffc, 0x00000001, 0x00600001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0xbe800000, 0xbf800000, 0x3f400000],
                [0x00800000, 0x00800000, 0x64800000, 0x3f400000],
                0x1a000000,
                [0xe4800000, 0xbe800000, 0xbf000000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0xff7fffff, 0x64800000, 0x00000000],
                [0xbe800000, 0xff7fffff, 0x00000001, 0x3f400000],
                0x3e000000,
                [0xbe800000, 0xff7fffff, 0x00000001, 0x3dc00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x64800000, 0xff7fffff, 0x80000000, 0x00800000],
                [0x80000000, 0xc0400000, 0x00800000, 0x00000000],
                0x00800000,
                [0x64800000, 0xff7fffff, 0x00000000, 0x00800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x40400000, 0x64800000, 0xbf800000, 0x00000001],
                [0x7f7fffff, 0x80000000, 0x00000001, 0x00000000],
                0x00800000,
                [0x40400000, 0x64800000, 0xbf800000, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0x80800000, 0xe4800000, 0x1a000000],
                [0x7f7fffff, 0x80000001, 0xc0400000, 0x3f000000],
                0x1a000000,
                [0x7eaaaaaa, 0x80555556, 0xe42aaaab, 0x1a400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x64800000, 0x00800000, 0x3f800000, 0x1a000000],
                [0x80000001, 0xff7fffff, 0x3f800000, 0x3f000000],
                0x3f7fffff,
                [0x33800001, 0xff7fffff, 0x3f800000, 0x3effffff],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0xbf800000, 0x7f7fffff, 0x00000000],
                [0xbf800000, 0x80000000, 0xbf800000, 0x3e800000],
                0x1a000000,
                [0xbf800000, 0x00000000, 0xbf800000, 0x19000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0xff7fffff, 0x00800000, 0x3f000000],
                [0xe4800000, 0x80000001, 0xe4800000, 0x00000001],
                0x00800000,
                [0x80000000, 0xff7fffff, 0x00800000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x80000000, 0x7f7fffff, 0x1a000000],
                [0xe4800000, 0x80000001, 0x80800000, 0x3f000000],
                0x1a000000,
                [0xe3aaaaab, 0x80000000, 0x7f2aaaaa, 0x1a400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0x64800000, 0x7f7fffff, 0x3f800000],
                [0x80000000, 0xc0400000, 0xbf800000, 0x00800000],
                0x3f7fffff,
                [0x00000000, 0x64800000, 0x7f7fffff, 0x33800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x00800000, 0x7f7fffff, 0x80000001, 0x00800000],
                [0x40400000, 0xbf800000, 0x80000001, 0x3e800000],
                0x00800000,
                [0x3f19999a, 0x7f4ccccc, 0x80000001, 0x00a00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0xff7fffff, 0x40400000, 0x1a000000],
                [0xc0400000, 0x3f800000, 0xbf800000, 0x3f000000],
                0x00800000,
                [0xa6400000, 0xff7fffff, 0x40400000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0x00800000, 0x80000001, 0x00800000],
                [0x00000000, 0x80000001, 0x40400000, 0x00000000],
                0x3f600000,
                [0x00000000, 0x00800000, 0x80000001, 0x00100000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0xe4800000, 0xbf800000, 0x00800000],
                [0x40400000, 0xff7fffff, 0x3f800000, 0x3f800000],
                0x3f600000,
                [0x40400000, 0xff7fffff, 0x3f800000, 0x3f600000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x00000000, 0x00000000, 0x00000000],
                [0x3f800000, 0x64800000, 0x00800000, 0x1a000000],
                0x3ecccccd,
                [0x3f800000, 0x64800000, 0x00800000, 0x194ccccd],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0xff7fffff, 0xff7fffff, 0x00800000],
                [0xff7fffff, 0xff7fffff, 0xff7fffff, 0x3f800000],
                0x3f600000,
                [0xff7fffff, 0xff7fffff, 0xff7fffff, 0x3f600000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x00000000, 0x00800000, 0x00800000],
                [0x80800000, 0x3f800000, 0xc0400000, 0x3e800000],
                0x3ecccccd,
                [0x80800000, 0x3f800000, 0xc0400000, 0x3dcccccd],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0xe4800000, 0x3e800000, 0x3f400000],
                [0x80800000, 0x3e800000, 0x80800000, 0x3f000000],
                0x00800000,
                [0x80000001, 0xe4800000, 0x3e800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0xe4800000, 0x64800000, 0x3f800000],
                [0x64800000, 0xff7fffff, 0x40400000, 0x3f000000],
                0x3e000000,
                [0x62888889, 0xfd888888, 0x646eeeef, 0x3f700000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x64800000, 0xc0400000, 0xc0400000, 0x3f400000],
                [0xbf800000, 0x00800000, 0x80000000, 0x3f400000],
                0x00800000,
                [0x64800000, 0xc0400000, 0xc0400000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x80000001, 0x64800000, 0x3f800000],
                [0x40400000, 0x64800000, 0x00000001, 0x00800000],
                0x00800000,
                [0xc0400000, 0x80000001, 0x64800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0xbe800000, 0x3e800000, 0xff7fffff, 0x3e800000],
                [0x00000000, 0x3e800000, 0x80800000, 0x3f000000],
                0x00800000,
                [0xbe800000, 0x3e800000, 0xff7fffff, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0xbf800000, 0x00000001, 0x3f400000],
                [0xc0400000, 0x80000001, 0x3e800000, 0x00000001],
                0x00800000,
                [0x80000000, 0xbf800000, 0x00000001, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x3f800000, 0x3f800000, 0x00000000],
                [0x80800000, 0x80800000, 0x00000001, 0x00000001],
                0x00800000,
                [0x80800000, 0x80800000, 0x00000001, 0x00000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0xbe800000, 0x00000000, 0x3e800000],
                [0x3e800000, 0x40400000, 0x64800000, 0x00800000],
                0x00000001,
                [0x80800000, 0xbe800000, 0x00000000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x3f800000, 0x00000000, 0xe4800000, 0x1a000000],
                [0xc0400000, 0x80800000, 0x80000000, 0x3f000000],
                0x3e000000,
                [0xc0400000, 0x80800000, 0xc0e00000, 0x3d800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x00000001, 0x3e800000, 0x3f400000],
                [0x80800000, 0xbf800000, 0x00000001, 0x00800000],
                0x3e000000,
                [0x00000001, 0x80186185, 0x3e800000, 0x3f280000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                [0x7f7fffff, 0xbf800000, 0x3f800000, 0x00000001],
                [0x00800000, 0x64800000, 0x80800000, 0x3f000000],
                0x3ecccccd,
                [0x35bfffff, 0x64800000, 0x807ffffd, 0x3e4ccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x40400000, 0xe4800000, 0xff7fffff, 0x00800000],
                [0xc0400000, 0x00000001, 0x00000001, 0x00000001],
                0x3f600000,
                [0x403fffeb, 0xe47ffff2, 0xff7ffff1, 0x00100001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0x00000000, 0x80800000, 0x3f400000],
                [0x80000000, 0xbf800000, 0xbf800000, 0x00800000],
                0x00800000,
                [0x3f800000, 0x80000000, 0x80800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0x80800000, 0xff7fffff, 0x00800000],
                [0x3e800000, 0xbe800000, 0xff7fffff, 0xe5000000],
                0x3f600000,
                [0x3e800000, 0xbe800000, 0xff7fffff, 0xe4e00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xbf800000, 0x00800000, 0x64800000, 0x3e800000],
                [0xff7fffff, 0x80000001, 0x00800000, 0xba000000],
                0x00800000,
                [0xbf7e0000, 0x00800000, 0x64800000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x40400000, 0x40400000, 0x00000001, 0xbf000000],
                [0x80000000, 0xbe800000, 0x00000000, 0x3f000000],
                0x00000001,
                [0x40400000, 0x40400000, 0x00000001, 0xbf000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0x80000001, 0x00000001, 0xbf800000],
                [0xbf800000, 0x40400000, 0x3f800000, 0x3f400000],
                0x1a000000,
                [0x3f800000, 0x9a900000, 0x99c00000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xff7fffff, 0x80000000, 0x80000000, 0xff7fffff],
                [0x40400000, 0x00800000, 0xbe800000, 0x3f000000],
                0x1a000000,
                [0xff7fffff, 0x80000000, 0x00000000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x80000001, 0x3e800000, 0xbf800000, 0x00000001],
                [0x00800000, 0x00000001, 0x80000000, 0xe5000000],
                0x3ecccccd,
                [0x00800000, 0x00000001, 0x00000000, 0xe44ccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xbe800000, 0x80000000, 0xbf800000, 0xff7fffff],
                [0xbf800000, 0x7f7fffff, 0xe4800000, 0x3f000000],
                0x3e000000,
                [0xbe800000, 0xbd924925, 0xbf800000, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0x40400000, 0x80000000, 0x80000001],
                [0xbf800000, 0x00800000, 0x80800000, 0x00800000],
                0x3ecccccd,
                [0xd9400002, 0xb5100001, 0x80800002, 0x00333333],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x3e800000, 0xc0400000, 0x80000001, 0xbf000000],
                [0xff7fffff, 0x3e800000, 0xff7fffff, 0x00800000],
                0x3e000000,
                [0x3fb24924, 0xc0400000, 0x3f924924, 0xbee00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x80000000, 0x40400000, 0x3e800000, 0x00000001],
                [0x7f7fffff, 0x80800000, 0xe4800000, 0xbf800000],
                0x3e000000,
                [0x7f7fffff, 0x80800015, 0xe4800000, 0xbe000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x00000001, 0x40400000, 0x64800000, 0x1a000000],
                [0x80800000, 0x64800000, 0x00800000, 0x00000001],
                0x3f7fffff,
                [0x00000001, 0x4b800001, 0x64800000, 0x0e000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x80800000, 0x64800000, 0x00800000, 0x3f800000],
                [0x64800000, 0x64800000, 0xbf800000, 0x1a000000],
                0x00000001,
                [0x807fffff, 0x64800000, 0x00800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0xbf800000, 0x3e800000, 0x00800000],
                [0x7f7fffff, 0xbe800000, 0x00000000, 0x3e800000],
                0x00800000,
                [0x7e4ccccc, 0xbf59999a, 0x3e4ccccd, 0x00a00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x3e800000, 0x80000000, 0xc0400000, 0xbf800000],
                [0x7f7fffff, 0x40400000, 0x40400000, 0x3e800000],
                0x00000001,
                [0x3e7ffff8, 0x80000001, 0xc0400000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x80000000, 0x80000001, 0x80000000, 0xbf000000],
                [0x80000000, 0x7f7fffff, 0x7f7fffff, 0x1a000000],
                0x3ecccccd,
                [0x00000000, 0xda2aaaaa, 0xda2aaaaa, 0xbe999999],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x80000001, 0x80000000, 0x7f7fffff, 0x3f800000],
                [0x00000000, 0x7f7fffff, 0x7f7fffff, 0xbf800000],
                0x3e000000,
                [0x80000001, 0xfe2aaaaa, 0x7f7fffff, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xff7fffff, 0x80800000, 0xc0400000, 0x3e800000],
                [0xbf800000, 0x80800000, 0x7f7fffff, 0x1a000000],
                0x00000001,
                [0xff7fffff, 0x80800000, 0xc0400000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0x00800000, 0x80000000, 0x00800000],
                [0xbe800000, 0x00000000, 0x00000000, 0x3e800000],
                0x00000001,
                [0x7f7fffff, 0x00800000, 0x00000000, 0x00800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x80800000, 0x3e800000, 0x00000000, 0x1a000000],
                [0xbf800000, 0x3e800000, 0x80000001, 0xff7fffff],
                0x3e000000,
                [0xbf800000, 0x3e800000, 0x80000001, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x40400000, 0xbe800000, 0x80000000, 0x00000000],
                [0x80000000, 0x40400000, 0x64800000, 0x00000001],
                0x1a000000,
                [0x00000000, 0x40400000, 0x64800000, 0x00000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xc0400000, 0x00800000, 0x3f800000, 0xbf800000],
                [0x7f7fffff, 0x7f7fffff, 0xbe800000, 0x00000000],
                0x3f7fffff,
                [0xc0400000, 0x00800000, 0x3f800000, 0xb3800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xbf800000, 0x00000001, 0xe4800000, 0x3f800000],
                [0x80000000, 0x80000000, 0x80000000, 0x3f000000],
                0x3e000000,
                [0xbf6eeeef, 0x00000001, 0xe46eeeef, 0x3f700000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xc0400000, 0xc0400000, 0xbe800000, 0x3f400000],
                [0xbf800000, 0x80800000, 0x80800000, 0xbf000000],
                0x1a000000,
                [0xc0400000, 0xc0400000, 0xbe800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x40400000, 0x64800000, 0xe4800000, 0x7f7fffff],
                [0x80000000, 0x00800000, 0xc0400000, 0x7f7fffff],
                0x3f7fffff,
                [0x34400000, 0x58800000, 0xd8800000, 0x7f7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x40400000, 0x80000000, 0xbf800000, 0x7f7fffff],
                [0x00800000, 0x80000000, 0x7f7fffff, 0x1a000000],
                0x3f600000,
                [0x40400000, 0x00000000, 0xbf800000, 0x7dffffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xbe800000, 0x3e800000, 0x40400000, 0x1a000000],
                [0x00000000, 0x64800000, 0x00000001, 0xbf800000],
                0x3f7fffff,
                [0x0d000001, 0x64800000, 0x8ec00001, 0xbf7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x00000000, 0x80000000, 0x80000001, 0x00800000],
                [0xe4800000, 0x3e800000, 0x64800000, 0x00800000],
                0x3f7fffff,
                [0xe47fffff, 0x3e7fffff, 0x647fffff, 0x00800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x00800000, 0x80000001, 0x80000001, 0xbf800000],
                [0xbe800000, 0x80000001, 0x64800000, 0x7f7fffff],
                0x00000001,
                [0x34000004, 0x80000001, 0xda000004, 0xbf7ffff8],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0xff7fffff, 0xbf800000, 0x00000001],
                [0xc0400000, 0xc0400000, 0xbe800000, 0x00000001],
                0x3e000000,
                [0x7f5fffff, 0xff5fffff, 0xbf680000, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                [0xbf800000, 0xbe800000, 0x3e800000, 0x3f400000],
                [0x3e800000, 0x80800000, 0x64800000, 0xba000000],
                0x00800000,
                [0xbf800000, 0xbe800000, 0x3e800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0x7f7fffff, 0xc0400000, 0xe5000000],
                [0x80000001, 0xbf800000, 0x00000001, 0x1a000000],
                0x1a000000,
                [0x80000000, 0x7f7fffff, 0xc0400000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x00000000, 0x64800000, 0xbf800000],
                [0xe4800000, 0x40400000, 0x80800000, 0x3f800000],
                0x1a000000,
                [0x7f7fffff, 0x1ac00000, 0x64800000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0xbe800000, 0xbf800000, 0x7f7fffff],
                [0xff7fffff, 0xbf800000, 0x40400000, 0x1a000000],
                0x00000001,
                [0xb4ffffff, 0xbe800000, 0xbf800000, 0x7f7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0x80800000, 0x00000000, 0x3f000000],
                [0x3e800000, 0xff7fffff, 0x64800000, 0x3f000000],
                0x3f7fffff,
                [0xf37fffff, 0xff7ffffe, 0x647fffff, 0x3f000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0xc0400000, 0x40400000, 0x3f400000],
                [0x7f7fffff, 0xbf800000, 0xc0400000, 0xba000000],
                0x3f600000,
                [0x7f5fffff, 0xbfa00000, 0xc0100000, 0x3dbf2000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0xe4800000, 0x40400000, 0x80000001],
                [0x80000001, 0x80000001, 0x3f800000, 0x00000001],
                0x3e000000,
                [0x64600000, 0xe4600000, 0x40300000, 0x80000001],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x64800000, 0x80800000, 0xff7fffff],
                [0x00800000, 0x00800000, 0x80000000, 0x80000001],
                0x3f600000,
                [0x00600000, 0x63000000, 0x80100000, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0x00000001, 0x3f800000, 0x00000000],
                [0xbf800000, 0x7f7fffff, 0x00000000, 0xbf000000],
                0x00800000,
                [0x80800000, 0x407fffff, 0x3f800000, 0x80400000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x80000001, 0xe4800000, 0x3f000000],
                [0xbe800000, 0xbe800000, 0xbe800000, 0x3f400000],
                0x3f600000,
                [0x7dffffff, 0xbe600000, 0xe3000000, 0x3f380000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0xc0400000, 0x64800000, 0x1a000000],
                [0xff7fffff, 0x00000000, 0x3e800000, 0xbf000000],
                0x00000001,
                [0xb4ffffff, 0xc0400000, 0x64800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0xe4800000, 0x80000000, 0xff7fffff],
                [0xff7fffff, 0xff7fffff, 0x00800000, 0xba000000],
                0x3f600000,
                [0xff5fffff, 0xff5fffff, 0x00700000, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x80000001, 0x00800000, 0x1a000000],
                [0xc0400000, 0x80000001, 0xbe800000, 0x1a000000],
                0x00000001,
                [0x64800000, 0x80000001, 0x00800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x40400000, 0x64800000, 0xff7fffff],
                [0x3e800000, 0x80000001, 0x00000000, 0x3f400000],
                0x3f7fffff,
                [0x3e800006, 0x34400000, 0x58800000, 0xf37fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0x3f800000, 0xff7fffff, 0xbf800000],
                [0x00800000, 0x3f800000, 0x80000001, 0x7f7fffff],
                0x00000001,
                [0xe4800000, 0x3f800000, 0xff7fffff, 0xbf7ffff8],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x80000000, 0xbe800000, 0x00000001],
                [0x40400000, 0x7f7fffff, 0x80000000, 0xbf000000],
                0x3e000000,
                [0xc0100000, 0x7dffffff, 0xbe600000, 0xbd800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0xe4800000, 0x80800000, 0x3f800000],
                [0x80000000, 0x00800000, 0x80000000, 0x7f7fffff],
                0x3e000000,
                [0xc0280000, 0xe4600000, 0x80700000, 0x7dffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0xff7fffff, 0x40400000, 0x3f800000],
                [0x00000000, 0x80000001, 0x80000000, 0x7f7fffff],
                0x3f7fffff,
                [0x00000000, 0xf37fffff, 0x34400000, 0x7f7ffffe],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x7f7fffff, 0x00000000, 0x7f7fffff],
                [0x3e800000, 0x00000000, 0x80000001, 0x7f7fffff],
                0x00800000,
                [0x3e800000, 0x7f7fffff, 0x80000000, 0x7f7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0x7f7fffff, 0xc0400000, 0x00000001],
                [0xe4800000, 0x40400000, 0x40400000, 0x00000000],
                0x1a000000,
                [0xbf000000, 0x7f7fffff, 0xc0400000, 0x00000001],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x80000000, 0xe4800000, 0x00000000],
                [0xbf800000, 0x64800000, 0x80000001, 0xbf000000],
                0x3ecccccd,
                [0xbe800000, 0x63cccccd, 0xe419999a, 0xbe4ccccd],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0x64800000, 0x3f800000, 0xba000000],
                [0xc0400000, 0x3f800000, 0xff7fffff, 0x00000001],
                0x3f600000,
                [0xfdffffff, 0x63000000, 0xff5fffff, 0xb8800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x00000001, 0x3e800000, 0x65000000],
                [0x00000000, 0x3f800000, 0x80000000, 0x80000001],
                0x1a000000,
                [0xbe800000, 0x1a000000, 0x3e800000, 0x65000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0xe4800000, 0xbf800000, 0x00000001],
                [0x40400000, 0xc0400000, 0xff7fffff, 0xff7fffff],
                0x3ecccccd,
                [0x40400000, 0xe419999a, 0xfecccccc, 0xfecccccc],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x40400000, 0x40400000, 0xbf000000],
                [0xc0400000, 0xbe800000, 0x80800000, 0xbf800000],
                0x3f600000,
                [0xc0260000, 0x3e200000, 0x3ec00000, 0xbf700000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0x80000001, 0xbf800000, 0x1a000000],
                [0x64800000, 0x00000001, 0xff7fffff, 0x80000001],
                0x3f7fffff,
                [0xf37fffff, 0x00000001, 0xff7ffffe, 0x0e000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x40400000, 0x64800000, 0xbf800000],
                [0x3e800000, 0x64800000, 0xbe800000, 0x00800000],
                0x3ecccccd,
                [0x3dcccccd, 0x63cccccd, 0x64199999, 0xbf199999],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x40400000, 0xff7fffff, 0x3f000000],
                [0x00800000, 0x40400000, 0x00000000, 0x3f800000],
                0x00800000,
                [0xbe800000, 0x40400000, 0xff7fffff, 0x3f000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0xbf800000, 0xff7fffff, 0x3f800000],
                [0x3e800000, 0x3f800000, 0x80000001, 0x3f000000],
                0x00800000,
                [0x80600000, 0xbf800000, 0xff7fffff, 0x3f800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0xff7fffff, 0xbe800000, 0x3f400000],
                [0xe4800000, 0xc0400000, 0x00000001, 0xe5000000],
                0x3ecccccd,
                [0xe3cccccd, 0xff199999, 0xbe199999, 0xe44ccccd],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x80000001, 0xc0400000, 0x7f7fffff],
                [0xbe800000, 0x80800000, 0x64800000, 0xba000000],
                0x3ecccccd,
                [0xbff33333, 0x80333334, 0x63cccccd, 0x7f199999],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0xc0400000, 0xc0400000, 0xff7fffff],
                [0x3e800000, 0xff7fffff, 0x64800000, 0xff7fffff],
                0x3e000000,
                [0x3d000000, 0xfdffffff, 0x63000000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x3f800000, 0xbf800000, 0x1a000000],
                [0x40400000, 0xbf800000, 0xbf800000, 0xba000000],
                0x3f7fffff,
                [0x58800000, 0xbf7ffffe, 0xbf800000, 0xb9ffffff],
            ),
        ];
        for (alpha, left, right, amount, expected) in cases {
            let format = EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                alpha,
            );
            let mut base = EffectFloatFrame::from_pixels(
                EffectFrameExtent::new(1, 1),
                format,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                vec![left.map(f32::from_bits)],
            )
            .expect("exact admitted input");
            let right = [right.map(f32::from_bits)];
            let source = EffectFloatFrameView::new(
                EffectFrameExtent::new(1, 1),
                format,
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &right,
            );
            let address = base.pixels().as_ptr();
            mix_float_frames_controlled(&mut base, source, f32::from_bits(amount), &mut || {
                Ok::<(), EffectFloatFrameError>(())
            })
            .expect("finite exact result");
            assert_eq!(
                base.pixels()[0].map(f32::to_bits),
                expected,
                "{alpha:?} left={left:x?} right={right:?} amount={amount:x}"
            );
            assert_eq!(base.pixels().as_ptr(), address);
            assert_eq!(base.alpha(), alpha);
        }
    }
}

#[cfg(test)]
mod sampling_tests {
    use super::*;
    use mondrian_core::{Rational, SampleAspectRatio};

    fn sampling() -> ImageSampling {
        ImageSampling::new(
            SampleAspectRatio::new(3, 2).unwrap(),
            [Rational::new(1, 2), Rational::new(3, 4)],
        )
        .unwrap()
    }
    fn format() -> EffectValueFormat {
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            crate::EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        )
    }

    #[test]
    fn borrowed_owned_and_alpha_conversions_preserve_nonuniform_sampling() {
        let extent = EffectFrameExtent::new(2, 1);
        let pixels = [[0.5, 0.25, 0.125, 0.5], [0.25, 0.5, 0.75, 1.0]];
        let view = EffectFloatFrameView::new(extent, format(), sampling(), &pixels);
        assert_eq!(view.sampling(), sampling());
        let owned = view
            .to_owned()
            .unwrap()
            .into_alpha(ColorFrameAlpha::PremultipliedCoverage)
            .unwrap()
            .into_alpha(ColorFrameAlpha::StraightCoverage)
            .unwrap();
        assert_eq!(owned.view().sampling(), sampling());
        assert_eq!(owned.pixels(), &pixels);
        assert_eq!(
            owned.into_parts(),
            (extent, format(), sampling(), pixels.to_vec())
        );
        let controlled = EffectFloatFrame::from_pixels_controlled(
            extent,
            format(),
            sampling(),
            pixels.to_vec(),
            &mut || Ok::<(), EffectFloatFrameError>(()),
        )
        .unwrap();
        let fallible = EffectFloatFrame::try_from_samples(
            extent,
            format(),
            sampling(),
            pixels.into_iter().map(Ok::<_, EffectFloatFrameError>),
        )
        .unwrap();
        assert_eq!(controlled, fallible);
    }

    #[test]
    fn pixelwise_blend_and_mix_refuse_different_sampling_before_writes() {
        let pixels = vec![[0.25, 0.5, 0.75, 1.0]; 2];
        let base = EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(2, 1),
            format(),
            sampling(),
            pixels.clone(),
        )
        .unwrap();
        let other =
            EffectFloatFrameView::new(base.extent(), format(), ImageSampling::UNIT_SQUARE, &pixels);
        let expected = EffectFloatFrameError::SamplingMismatch {
            expected: sampling(),
            actual: ImageSampling::UNIT_SQUARE,
        };
        let mut blended = base.clone();
        assert_eq!(
            blend_float_frames_in_place(
                &mut blended,
                other,
                0.0,
                mondrian_core::BlendMode::Normal,
                0
            ),
            Err(expected.clone())
        );
        assert_eq!(blended, base);
        let mut mixed = base.clone();
        assert_eq!(
            mix_float_frames_controlled(&mut mixed, other, 1.0, &mut || Ok::<
                (),
                EffectFloatFrameError,
            >(())),
            Err(expected)
        );
        assert_eq!(mixed, base);
    }
}
