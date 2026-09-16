use mondrian_core::ResolvedVisualFrameIdentity;
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

/// Alpha interpretation of cached working-linear samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineRenderCacheAlpha {
    /// RGB is unassociated and alpha is straight coverage.
    StraightCoverage,
}

impl TimelineRenderCacheAlpha {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::StraightCoverage => 1,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::StraightCoverage),
            _ => None,
        }
    }
}

/// Complete content-addressed identity of one cached Timeline frame.
#[derive(Debug, Clone, Copy)]
pub struct TimelineRenderCacheIdentity {
    digest: [u8; 32],
    expected_extent: Option<(u32, u32)>,
}

impl TimelineRenderCacheIdentity {
    pub(crate) const fn from_digest(digest: [u8; 32]) -> Self {
        Self { digest, expected_extent: None }
    }

    /// Bind one complete resolved visual identity to its physical cache envelope.
    pub fn for_resolved_visual(
        visual: ResolvedVisualFrameIdentity,
        width: u32,
        height: u32,
        format: TimelineRenderCacheFormat,
        alpha: TimelineRenderCacheAlpha,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"mondrian.timeline-render-cache.identity.v2");
        field(&mut hasher, b"resolved-visual", &visual.digest());
        field(&mut hasher, b"width", &width.to_le_bytes());
        field(&mut hasher, b"height", &height.to_le_bytes());
        field(&mut hasher, b"format", &[format.code()]);
        field(&mut hasher, b"alpha", &[alpha.code()]);
        Self {
            digest: hasher.finalize().into(),
            expected_extent: Some((width, height)),
        }
    }

    /// Stable digest used for artifact verification and path fan-out.
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }

    pub(crate) const fn expected_extent(self) -> Option<(u32, u32)> {
        self.expected_extent
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
            TimelineRenderCacheAlpha::StraightCoverage,
        )
    }

    #[test]
    fn resolved_visual_and_extent_are_identity() {
        assert_ne!(identity(b"frame-10", 3840), identity(b"frame-11", 3840));
        assert_ne!(identity(b"frame-10", 3840), identity(b"frame-10", 1920));
    }

    #[test]
    fn hexadecimal_identity_is_complete() {
        let hex = identity(b"frame-10", 3840).hex();
        assert_eq!(
            hex,
            "85f05971f32b99e5b72830d8c58d3cfc3bcb021f2f5bc6ac85e10938e8161fd7"
        );
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }
}
