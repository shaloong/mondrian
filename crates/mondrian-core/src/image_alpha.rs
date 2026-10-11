//! Storage association and numeric alpha contracts shared by image execution.

use serde::{Deserialize, Serialize};

/// Exact relationship between RGB storage and the alpha channel.
///
/// Alpha is never color-managed. Coverage variants require alpha in `[0,1]`;
/// Float variants admit finite signed and greater-than-one values. Associated
/// RGB at alpha zero may carry emission and is not automatically transparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorFrameAlpha {
    /// Independent RGB with normalized coverage alpha.
    StraightCoverage,
    /// Associated RGB with normalized coverage alpha, including emission.
    PremultipliedCoverage,
    /// Independent RGB with an unbounded finite Float alpha channel.
    StraightFloat,
    /// Associated RGB with an unbounded finite Float alpha channel.
    PremultipliedFloat,
    /// RGB is fully opaque and every alpha sample is exactly one.
    Opaque,
}

impl ColorFrameAlpha {
    /// Whether RGB samples are stored associated with alpha.
    pub const fn is_premultiplied(self) -> bool {
        matches!(self, Self::PremultipliedCoverage | Self::PremultipliedFloat)
    }

    /// Whether a straight RGB consumer can read the channels without division.
    pub const fn is_straight_compatible(self) -> bool {
        matches!(
            self,
            Self::StraightCoverage | Self::StraightFloat | Self::Opaque
        )
    }

    /// Whether the contract promises normalized coverage or opaque samples.
    pub const fn is_coverage(self) -> bool {
        matches!(
            self,
            Self::StraightCoverage | Self::PremultipliedCoverage | Self::Opaque
        )
    }

    /// Whether one alpha sample satisfies the declared numeric domain.
    pub fn admits_alpha(self, alpha: f32) -> bool {
        if !alpha.is_finite() {
            return false;
        }
        match self {
            Self::StraightCoverage | Self::PremultipliedCoverage => (0.0..=1.0).contains(&alpha),
            Self::StraightFloat | Self::PremultipliedFloat => true,
            Self::Opaque => alpha == 1.0,
        }
    }

    /// Stable representation code for complete execution/artifact identities.
    pub const fn identity_code(self) -> u8 {
        match self {
            Self::StraightCoverage => 0,
            Self::PremultipliedCoverage => 1,
            Self::StraightFloat => 2,
            Self::PremultipliedFloat => 3,
            Self::Opaque => 4,
        }
    }

    /// Decode one current representation code; unknown schemas are refused.
    pub const fn from_identity_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::StraightCoverage),
            1 => Some(Self::PremultipliedCoverage),
            2 => Some(Self::StraightFloat),
            3 => Some(Self::PremultipliedFloat),
            4 => Some(Self::Opaque),
            _ => None,
        }
    }
}

/// Finite set of exact alpha formats admitted by an immutable execution contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorFrameAlphaFormats(u8);

impl ColorFrameAlphaFormats {
    /// No admitted representation.
    pub const NONE: Self = Self(0);
    /// Every current finite image-alpha representation.
    pub const ALL: Self = Self(0b1_1111);
    /// Normalized independent RGB, plus fully opaque RGB.
    pub const STRAIGHT_COVERAGE: Self = Self((1 << 0) | (1 << 4));
    /// Independent RGB with normalized or unbounded finite alpha, plus opaque RGB.
    pub const STRAIGHT_FLOAT: Self = Self((1 << 0) | (1 << 2) | (1 << 4));
    /// Exact association reports provided by a native Float image ABI.
    pub const FLOAT_OUTPUT: Self = Self((1 << 2) | (1 << 3) | (1 << 4));
    /// Build a set containing exactly one representation.
    pub const fn only(alpha: ColorFrameAlpha) -> Self {
        Self(1 << alpha.identity_code())
    }
    /// Whether this declaration admits one actual representation.
    pub const fn contains(self, alpha: ColorFrameAlpha) -> bool {
        self.0 & Self::only(alpha).0 != 0
    }
    /// Union of two representation declarations.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    /// Intersection of two declarations.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
    /// Whether another contract admits every format declared here.
    pub const fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }
    /// Whether this declaration admits no actual representation.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    /// Canonical current-schema bytes for signatures and artifact admission.
    pub const fn identity_code(self) -> u8 {
        self.0
    }
    /// Decode only declarations containing current canonical representation bits.
    pub const fn from_identity_code(code: u8) -> Option<Self> {
        if code & !Self::ALL.0 == 0 {
            Some(Self(code))
        } else {
            None
        }
    }
}
