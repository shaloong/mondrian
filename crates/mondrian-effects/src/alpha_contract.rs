//! Definition/provider-owned alpha admission and output negotiation.

use mondrian_core::{types::BlendMode, ColorFrameAlpha};

pub use mondrian_core::ColorFrameAlphaFormats;

/// Exact output policy retained before a prepared provider resolves a format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectAlphaOutputContract {
    /// Preserve the incoming exact representation.
    Preserve,
    /// The operation always writes this exact representation.
    Fixed(ColorFrameAlpha),
    /// A prepared provider resolves one of these explicitly admitted formats.
    Negotiated(ColorFrameAlphaFormats),
}

/// Definition-owned alpha requirements retained beside the existing stage IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectAlphaContract {
    inputs: ColorFrameAlphaFormats,
    output: EffectAlphaOutputContract,
}

impl EffectAlphaContract {
    /// Construct a complete provider declaration.
    pub const fn new(inputs: ColorFrameAlphaFormats, output: EffectAlphaOutputContract) -> Self {
        Self { inputs, output }
    }
    /// Existing normalized straight operations preserve their exact input.
    pub const STRAIGHT_COVERAGE: Self = Self::new(
        ColorFrameAlphaFormats::STRAIGHT_COVERAGE,
        EffectAlphaOutputContract::Preserve,
    );
    /// Coverage operations may change opaque input into normalized coverage.
    pub const COVERAGE_OPERATION: Self = Self::new(
        ColorFrameAlphaFormats::STRAIGHT_COVERAGE,
        EffectAlphaOutputContract::Negotiated(ColorFrameAlphaFormats::STRAIGHT_COVERAGE),
    );
    /// Native Float input/output admission with actual metadata negotiation.
    pub const NATIVE_FLOAT: Self = Self::new(
        ColorFrameAlphaFormats::ALL,
        EffectAlphaOutputContract::Negotiated(ColorFrameAlphaFormats::FLOAT_OUTPUT),
    );
    /// Zero-input Float producers explicitly negotiate their output association.
    pub const SOURCE_FLOAT: Self = Self::new(
        ColorFrameAlphaFormats::NONE,
        EffectAlphaOutputContract::Negotiated(ColorFrameAlphaFormats::FLOAT_OUTPUT),
    );
    /// Normal Over resolves the representation from both exact input values.
    pub const NORMAL_OVER: Self = Self::new(
        ColorFrameAlphaFormats::ALL,
        EffectAlphaOutputContract::Negotiated(ColorFrameAlphaFormats::ALL),
    );
    /// Identity operations do not reinterpret their pixels or alpha channels.
    pub const IDENTITY: Self = Self::new(
        ColorFrameAlphaFormats::ALL,
        EffectAlphaOutputContract::Preserve,
    );
    /// Exact accepted input formats.
    pub const fn inputs(self) -> ColorFrameAlphaFormats {
        self.inputs
    }
    /// Prepared output resolution policy.
    pub const fn output(self) -> EffectAlphaOutputContract {
        self.output
    }
    /// Whether the retained declaration admits an executable input and output.
    pub const fn is_valid(self) -> bool {
        !self.inputs.is_empty()
            && match self.output {
                EffectAlphaOutputContract::Negotiated(formats) => !formats.is_empty(),
                _ => true,
            }
    }
    /// Validate a zero-input producer without inventing an incoming alpha format.
    pub const fn is_valid_source(self) -> bool {
        self.inputs.is_empty()
            && match self.output {
                EffectAlphaOutputContract::Preserve => false,
                EffectAlphaOutputContract::Fixed(_) => true,
                EffectAlphaOutputContract::Negotiated(formats) => !formats.is_empty(),
            }
    }
    /// Formats covered by a zero-input producer's output storage declaration.
    pub const fn source_outputs(self) -> ColorFrameAlphaFormats {
        match self.output {
            EffectAlphaOutputContract::Preserve => ColorFrameAlphaFormats::NONE,
            EffectAlphaOutputContract::Fixed(alpha) => ColorFrameAlphaFormats::only(alpha),
            EffectAlphaOutputContract::Negotiated(formats) => formats,
        }
    }
    /// Validate an exact zero-input output association.
    pub const fn admits_source_output(self, output: ColorFrameAlpha) -> bool {
        self.is_valid_source() && self.source_outputs().contains(output)
    }
    /// Validate the exact output against the retained declaration and input.
    pub const fn admits_output(self, input: ColorFrameAlpha, output: ColorFrameAlpha) -> bool {
        if !self.inputs.contains(input) {
            return false;
        }
        match self.output {
            EffectAlphaOutputContract::Preserve => input.identity_code() == output.identity_code(),
            EffectAlphaOutputContract::Fixed(expected) => {
                expected.identity_code() == output.identity_code()
            }
            EffectAlphaOutputContract::Negotiated(formats) => formats.contains(output),
        }
    }
}

/// Shared consumer semantics used in typed alpha-admission diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectAlphaConsumer {
    /// A Definition operation currently requires normalized straight color.
    StraightCoverageOperation,
    /// An artistic blend requires its declared straight-color interpretation.
    ArtisticBlend(BlendMode),
    /// An AlphaMask operation uses normalized scalar coverage.
    AlphaMask,
    /// A stock-OCIO transition has a declared straight-color input contract.
    ColorDomainTransition,
    /// A heterogeneous route currently carries normalized straight samples.
    HeterogeneousRoute,
    /// A delivery format does not express this exact image association.
    OutputEncoding,
}
