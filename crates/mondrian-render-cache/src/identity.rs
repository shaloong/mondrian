use mondrian_core::{ColorFrameAlphaFormats, ImageSampling, ResolvedVisualFrameIdentity};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    hash::{Hash, Hasher},
};

/// Stable on-disk payload representation selected by the cache author.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineRenderCacheFormat {
    /// Lossless little-endian RGBA32F compressed as one independent Zstandard frame.
    LosslessRgba32FloatZstd,
}

impl TimelineRenderCacheFormat {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::LosslessRgba32FloatZstd => 1,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::LosslessRgba32FloatZstd),
            _ => None,
        }
    }
}

/// Complete content-addressed identity of one cached Timeline frame.
#[derive(Debug, Clone, Copy)]
pub struct TimelineRenderCacheIdentity {
    digest: [u8; 32],
    expected_extent: Option<(u32, u32)>,
    expected_sampling: Option<ImageSampling>,
    alpha_admission: ColorFrameAlphaFormats,
}

impl TimelineRenderCacheIdentity {
    pub(crate) const fn from_digest(digest: [u8; 32]) -> Self {
        Self {
            digest,
            expected_extent: None,
            expected_sampling: None,
            alpha_admission: ColorFrameAlphaFormats::ALL,
        }
    }

    /// Bind one complete resolved visual identity to its physical cache envelope.
    pub fn for_resolved_visual(
        visual: ResolvedVisualFrameIdentity,
        width: u32,
        height: u32,
        format: TimelineRenderCacheFormat,
        alpha_admission: ColorFrameAlphaFormats,
        sampling: ImageSampling,
    ) -> Self {
        let mut hasher = Sha256::new();
        // v6 invalidates former decoded-storage precision and YUV alpha semantics.
        hasher.update(b"mondrian.timeline-render-cache.identity.v6");
        field(&mut hasher, b"resolved-visual", &visual.digest());
        field(&mut hasher, b"width", &width.to_le_bytes());
        field(&mut hasher, b"height", &height.to_le_bytes());
        field(&mut hasher, b"format", &[format.code()]);
        field(&mut hasher, b"sampling", &sampling_bytes(sampling));
        field(
            &mut hasher,
            b"alpha-admission",
            &[alpha_admission.identity_code()],
        );
        Self {
            digest: hasher.finalize().into(),
            expected_extent: Some((width, height)),
            expected_sampling: Some(sampling),
            alpha_admission,
        }
    }

    /// Stable digest used for artifact verification and path fan-out.
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }

    pub(crate) const fn expected_extent(self) -> Option<(u32, u32)> {
        self.expected_extent
    }

    pub(crate) const fn expected_sampling(self) -> Option<ImageSampling> {
        self.expected_sampling
    }

    /// Complete actual-association admission bound into the semantic signature.
    pub const fn alpha_admission(self) -> ColorFrameAlphaFormats {
        self.alpha_admission
    }

    /// Lowercase hexadecimal filename identity.
    pub fn hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.digest {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
        }
        output
    }
}

// Fixed-width canonical representation shared by the identity and artifact.
pub(crate) fn sampling_bytes(sampling: ImageSampling) -> [u8; 40] {
    let mut bytes = [0; 40];
    let par = sampling.sample_aspect_ratio();
    bytes[0..4].copy_from_slice(&par.numerator().to_le_bytes());
    bytes[4..8].copy_from_slice(&par.denominator().to_le_bytes());
    for (axis, scale) in sampling.render_scale().into_iter().enumerate() {
        let start = 8 + axis * 16;
        bytes[start..start + 8].copy_from_slice(&scale.num.to_le_bytes());
        bytes[start + 8..start + 16].copy_from_slice(&scale.den.to_le_bytes());
    }
    bytes
}

impl PartialEq for TimelineRenderCacheIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.digest == other.digest
    }
}

impl Eq for TimelineRenderCacheIdentity {}

impl Hash for TimelineRenderCacheIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.digest.hash(state);
    }
}

impl fmt::Display for TimelineRenderCacheIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.hex())
    }
}

fn field(hasher: &mut Sha256, label: &[u8], value: &[u8]) {
    hasher.update((label.len() as u64).to_le_bytes());
    hasher.update(label);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(bytes: &[u8], width: u32) -> TimelineRenderCacheIdentity {
        let materialization =
            mondrian_core::ResolvedVisualNodeMaterializationIdentity::from_canonical_bytes(bytes);
        let visual = ResolvedVisualFrameIdentity::from_materialization(materialization);
        TimelineRenderCacheIdentity::for_resolved_visual(
            visual,
            width,
            2160,
            TimelineRenderCacheFormat::LosslessRgba32FloatZstd,
            ColorFrameAlphaFormats::only(mondrian_core::ColorFrameAlpha::StraightCoverage),
            ImageSampling::UNIT_SQUARE,
        )
    }

    #[test]
    fn resolved_visual_and_extent_are_identity() {
        assert_ne!(identity(b"frame-10", 3840), identity(b"frame-11", 3840));
        assert_ne!(identity(b"frame-10", 3840), identity(b"frame-10", 1920));
    }

    #[test]
    fn sampling_shape_and_both_rates_are_identity() {
        let materialization =
            mondrian_core::ResolvedVisualNodeMaterializationIdentity::from_canonical_bytes(
                b"sampling",
            );
        let visual = ResolvedVisualFrameIdentity::from_materialization(materialization);
        let key = |par, scales| {
            TimelineRenderCacheIdentity::for_resolved_visual(
                visual,
                2,
                1,
                TimelineRenderCacheFormat::LosslessRgba32FloatZstd,
                ColorFrameAlphaFormats::ALL,
                ImageSampling::new(par, scales).expect("sampling"),
            )
        };
        let unit = [mondrian_core::Rational::new(1, 1); 2];
        let square = mondrian_core::SampleAspectRatio::SQUARE;
        let baseline = key(square, unit);
        assert_ne!(
            baseline,
            key(mondrian_core::SampleAspectRatio::new(4, 3).unwrap(), unit)
        );
        assert_ne!(
            baseline,
            key(square, [mondrian_core::Rational::new(1, 2), unit[1]])
        );
        assert_ne!(
            baseline,
            key(square, [unit[0], mondrian_core::Rational::new(1, 2)])
        );
        assert_eq!(
            baseline,
            key(square, [mondrian_core::Rational::new(2, 2); 2])
        );
    }

    #[test]
    fn hexadecimal_identity_is_complete() {
        let hex = identity(b"frame-10", 3840).hex();
        assert_eq!(
            hex,
            "5506f25b8ab5a8eb6bcdc8a31913612e27fb2f87d4c6c809f4ed2df0c6155c9e"
        );
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }
}
