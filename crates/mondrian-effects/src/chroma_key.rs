//! Luminance-independent chromaticity key selection for scene-linear RGB.

use mondrian_core::Color;

/// Validated color-key controls shared by graph execution and cache identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreparedChromaKey {
    target: [f32; 3],
    similarity: f32,
    edge_softness: f32,
}

impl PreparedChromaKey {
    /// Prepare a non-black finite key color and bounded similarity controls.
    pub fn new(color: Color, similarity: f32, edge_softness: f32) -> Result<Self, ChromaKeyError> {
        let rgb = [color.r, color.g, color.b];
        if rgb.iter().any(|channel| !channel.is_finite() || *channel < 0.0) {
            return Err(ChromaKeyError::InvalidColor);
        }
        let sum = f64::from(rgb[0]) + f64::from(rgb[1]) + f64::from(rgb[2]);
        if sum <= 1.0e-8 {
            return Err(ChromaKeyError::AchromaticBlack);
        }
        if !similarity.is_finite() || !(0.0..=1.0).contains(&similarity) {
            return Err(ChromaKeyError::Similarity);
        }
        if !edge_softness.is_finite() || !(0.0..=1.0).contains(&edge_softness) {
            return Err(ChromaKeyError::EdgeSoftness);
        }
        let target = rgb.map(|channel| (f64::from(channel) / sum) as f32);
        Ok(Self { target, similarity, edge_softness })
    }

    /// Coverage of the selected key color before the graph Mask inverts it.
    pub fn matte(self, rgb: [f32; 3]) -> f32 {
        if rgb.iter().any(|channel| !channel.is_finite()) {
            return 0.0;
        }
        let positive = rgb.map(|channel| channel.max(0.0));
        let sum = f64::from(positive[0]) + f64::from(positive[1]) + f64::from(positive[2]);
        if sum <= 1.0e-8 {
            return 0.0;
        }
        let coordinate = positive.map(|channel| (f64::from(channel) / sum) as f32);
        let distance = coordinate
            .into_iter()
            .zip(self.target)
            .map(|(actual, target)| (actual - target).powi(2))
            .sum::<f32>()
            .sqrt();
        let outer = self.similarity + self.edge_softness;
        let selection = if outer <= self.similarity {
            f32::from(distance <= self.similarity)
        } else {
            let t = ((distance - self.similarity) / self.edge_softness).clamp(0.0, 1.0);
            1.0 - t * t * (3.0 - 2.0 * t)
        };
        selection.clamp(0.0, 1.0)
    }

    /// Exact controls included in compiled graph and cache identity.
    pub const fn controls(self) -> [f32; 5] {
        [
            self.target[0],
            self.target[1],
            self.target[2],
            self.similarity,
            self.edge_softness,
        ]
    }
}

/// Invalid authored chroma-key control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChromaKeyError {
    /// Key color has a negative or non-finite channel.
    #[error("chroma key color must have finite nonnegative RGB channels")]
    InvalidColor,
    /// Black has no chromaticity; use a luminance key instead.
    #[error("black has no chromaticity; use Luma Key for black")]
    AchromaticBlack,
    /// Similarity is outside the authored range.
    #[error("chroma key similarity must be finite and within [0, 1]")]
    Similarity,
    /// Edge softness is outside the authored range.
    #[error("chroma key edge softness must be finite and within [0, 1]")]
    EdgeSoftness,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_screen_match_is_independent_of_luminance() {
        let key = PreparedChromaKey::new(Color::from_hex(0x00FF00), 0.2, 0.1).expect("green key");
        assert_eq!(key.matte([0.0, 0.1, 0.0]), 1.0);
        assert_eq!(key.matte([0.0, 2.0, 0.0]), 1.0);
        assert_eq!(key.matte([2.0, 0.0, 0.0]), 0.0);
        assert_eq!(key.matte([0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn black_key_color_is_rejected() {
        assert_eq!(
            PreparedChromaKey::new(Color::BLACK, 0.2, 0.1),
            Err(ChromaKeyError::AchromaticBlack),
        );
    }
}
