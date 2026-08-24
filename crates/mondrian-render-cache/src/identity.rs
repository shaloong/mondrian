use sha2::{Digest, Sha256};
use std::fmt;

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

/// Spatial/source quality represented by one cached Timeline result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineRenderCacheQuality {
    /// Full-resolution source materialization.
    Full,
    /// Half-resolution source materialization.
    Half,
    /// Proxy-backed source materialization.
    Proxy,
}

impl TimelineRenderCacheQuality {
    const fn code(self) -> u8 {
        match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Proxy => 3,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineRenderCacheIdentity([u8; 32]);

impl TimelineRenderCacheIdentity {
    /// Construct from an already-domain-separated SHA-256 digest.
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Stable digest used for artifact verification and path fan-out.
    pub const fn digest(self) -> [u8; 32] {
        self.0
    }

    /// Lowercase hexadecimal filename identity.
    pub fn hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
        }
        output
    }
}

impl fmt::Display for TimelineRenderCacheIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.hex())
    }
}

/// Canonical builder for a stable Timeline render-cache identity.
///
/// Callers must supply stable semantic fingerprints, never process-local
/// pointer values or cache generations. Length-prefixing every byte string
/// prevents concatenation ambiguity.
pub struct TimelineRenderCacheIdentityBuilder {
    hasher: Sha256,
}

impl TimelineRenderCacheIdentityBuilder {
    /// Start the current cache-key schema.
    pub fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"mondrian.timeline-render-cache.identity.v1");
        Self { hasher }
    }

    /// Bind the complete recursive Prepared Visual author fingerprint.
    pub fn visual_author_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.field(b"visual-author", &fingerprint);
        self
    }

    /// Bind compiled program/effect semantics for this exact resolved frame.
    pub fn program_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.field(b"program", &fingerprint);
        self
    }

    /// Bind all resolved media revisions and exact source samples.
    pub fn media_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.field(b"media", &fingerprint);
        self
    }

    /// Bind the working/output color context that shaped the composite.
    pub fn color_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.field(b"color", &fingerprint);
        self
    }

    /// Bind the exact root Timeline frame.
    pub fn frame(mut self, frame: i64) -> Self {
        self.field(b"frame", &frame.to_le_bytes());
        self
    }

    /// Bind materialized output geometry.
    pub fn extent(mut self, width: u32, height: u32) -> Self {
        self.field(b"width", &width.to_le_bytes());
        self.field(b"height", &height.to_le_bytes());
        self
    }

    /// Bind Preview source/materialization quality.
    pub fn quality(mut self, quality: TimelineRenderCacheQuality) -> Self {
        self.field(b"quality", &[quality.code()]);
        self
    }

    /// Bind alpha and physical cache format contracts.
    pub fn format(
        mut self,
        format: TimelineRenderCacheFormat,
        alpha: TimelineRenderCacheAlpha,
    ) -> Self {
        self.field(b"format", &[format.code()]);
        self.field(b"alpha", &[alpha.code()]);
        self
    }

    /// Finish the canonical digest.
    pub fn finish(self) -> TimelineRenderCacheIdentity {
        TimelineRenderCacheIdentity::from_digest(self.hasher.finalize().into())
    }

    fn field(&mut self, label: &[u8], value: &[u8]) {
        self.hasher.update((label.len() as u64).to_le_bytes());
        self.hasher.update(label);
        self.hasher.update((value.len() as u64).to_le_bytes());
        self.hasher.update(value);
    }
}

impl Default for TimelineRenderCacheIdentityBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(frame: i64, quality: TimelineRenderCacheQuality) -> TimelineRenderCacheIdentity {
        TimelineRenderCacheIdentityBuilder::new()
            .visual_author_fingerprint([1; 32])
            .program_fingerprint([2; 32])
            .media_fingerprint([3; 32])
            .color_fingerprint([4; 32])
            .frame(frame)
            .extent(3840, 2160)
            .quality(quality)
            .format(
                TimelineRenderCacheFormat::LosslessRgba32FloatZstd,
                TimelineRenderCacheAlpha::StraightCoverage,
            )
            .finish()
    }

    #[test]
    fn exact_frame_and_quality_are_identity() {
        assert_ne!(
            identity(10, TimelineRenderCacheQuality::Full),
            identity(11, TimelineRenderCacheQuality::Full)
        );
        assert_ne!(
            identity(10, TimelineRenderCacheQuality::Full),
            identity(10, TimelineRenderCacheQuality::Half)
        );
    }

    #[test]
    fn hexadecimal_identity_is_complete() {
        let hex = identity(10, TimelineRenderCacheQuality::Full).hex();
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }
}
