//! Opaque exact values exchanged by resolved visual and artifact Modules.

use sha2::{Digest, Sha256};
use std::fmt;

const RESOLVED_VISUAL_SEMANTICS_EPOCH: u16 = 1;

/// Adapter-owned canonical identity of every materialized element in one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedVisualNodeMaterializationIdentity([u8; 32]);

impl ResolvedVisualNodeMaterializationIdentity {
    /// Hash versioned canonical bytes emitted by one exhaustive source Adapter.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"mondrian.resolved-visual.node-materialization.v1");
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }

    /// Stable digest consumed by the Prepared Visual Module.
    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

/// Provider-independent semantic identity of one fully resolved visual frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedVisualFrameIdentity([u8; 32]);

impl ResolvedVisualFrameIdentity {
    /// Build a generated or test frame whose complete semantics are already
    /// represented by one exhaustive Adapter materialization.
    pub fn from_materialization(identity: ResolvedVisualNodeMaterializationIdentity) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"mondrian.resolved-visual.standalone-frame.v1");
        hasher.update(identity.digest());
        Self(hasher.finalize().into())
    }

    /// Bind the complete Prepared Visual semantic envelope to one exhaustive
    /// Adapter materialization.
    pub fn from_prepared_visual_semantics(
        materialization: ResolvedVisualNodeMaterializationIdentity,
        time: crate::TimelineTime,
        author_resolution: crate::Resolution,
        execution_resolution: crate::Resolution,
        working_color_space: crate::WorkingColorSpace,
        color_engine: &crate::ColorEngine,
    ) -> Result<Self, ResolvedVisualIdentityError> {
        let mut hasher = Sha256::new();
        hasher.update(b"mondrian.resolved-visual.frame.v1");
        field(
            &mut hasher,
            b"semantics-epoch",
            &RESOLVED_VISUAL_SEMANTICS_EPOCH.to_le_bytes(),
        );
        field(&mut hasher, b"materialization", &materialization.digest());
        field(
            &mut hasher,
            b"time-numerator",
            &time.numerator().to_le_bytes(),
        );
        field(
            &mut hasher,
            b"time-denominator",
            &time.denominator().to_le_bytes(),
        );
        field(
            &mut hasher,
            b"author-width",
            &author_resolution.width.to_le_bytes(),
        );
        field(
            &mut hasher,
            b"author-height",
            &author_resolution.height.to_le_bytes(),
        );
        field(
            &mut hasher,
            b"execution-width",
            &execution_resolution.width.to_le_bytes(),
        );
        field(
            &mut hasher,
            b"execution-height",
            &execution_resolution.height.to_le_bytes(),
        );
        field(
            &mut hasher,
            b"working-space",
            &serde_json::to_vec(&working_color_space)?,
        );
        field(
            &mut hasher,
            b"color-engine",
            &serde_json::to_vec(color_engine)?,
        );
        Ok(Self(hasher.finalize().into()))
    }

    /// Stable digest consumed by physical artifact Modules.
    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

/// Failure while encoding a resolved visual semantic envelope.
#[derive(Debug, thiserror::Error)]
pub enum ResolvedVisualIdentityError {
    /// A versioned semantic value could not be encoded canonically.
    #[error("resolved visual color semantics could not be encoded: {0}")]
    ColorEncoding(#[from] serde_json::Error),
}

impl fmt::Display for ResolvedVisualFrameIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
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

    fn identity(time: crate::TimelineTime, working: crate::WorkingColorSpace) -> [u8; 32] {
        ResolvedVisualFrameIdentity::from_prepared_visual_semantics(
            ResolvedVisualNodeMaterializationIdentity::from_canonical_bytes(b"solid-layer"),
            time,
            crate::Resolution { width: 1920, height: 1080 },
            crate::Resolution { width: 960, height: 540 },
            working,
            &crate::ColorEngine::mondrian_standard(),
        )
        .expect("canonical identity")
        .digest()
    }

    #[test]
    fn prepared_visual_semantics_are_deterministic_and_complete() {
        let first = identity(
            crate::TimelineTime::new(1, 24).expect("time"),
            crate::WorkingColorSpace::LinearRec709,
        );
        assert_eq!(
            first,
            identity(
                crate::TimelineTime::new(1, 24).expect("time"),
                crate::WorkingColorSpace::LinearRec709,
            )
        );
        assert_ne!(
            first,
            identity(
                crate::TimelineTime::new(2, 24).expect("time"),
                crate::WorkingColorSpace::LinearRec709,
            )
        );
        assert_ne!(
            first,
            identity(
                crate::TimelineTime::new(1, 24).expect("time"),
                crate::WorkingColorSpace::AcesCg,
            )
        );
    }
}
