//! SMPTE ST 428-1 DCDM X'Y'Z' output encoding.

/// One rejected DCDM frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DcdmEncodingError {
    /// Interleaved input is not complete RGBA pixels.
    #[error("DCDM input component count {0} is not divisible by four")]
    InvalidComponentCount(usize),
    /// A component was NaN or infinite.
    #[error("DCDM input component {component} is not finite")]
    NonFinite {
        /// Interleaved component index.
        component: usize,
    },
}

/// Convert display-linear Rec.709/D65 RGBA to ST 428-1 packed `xyz12le`.
///
/// The alpha channel is ignored after the export boundary has explicitly
/// flattened coverage. X, Y, and Z are normalized to projector white
/// luminance `L=48 cd/m²`, encoded with the ST 428-1 `1/2.6` transfer, rounded,
/// clamped to 12 bits, and stored in the most-significant 12 bits of LE words.
pub fn encode_linear_rec709_as_dcdm_xyz12le(rgba: &[f32]) -> Result<Vec<u8>, DcdmEncodingError> {
    if !rgba.len().is_multiple_of(4) {
        return Err(DcdmEncodingError::InvalidComponentCount(rgba.len()));
    }
    for (component, value) in rgba.iter().enumerate() {
        if !value.is_finite() {
            return Err(DcdmEncodingError::NonFinite { component });
        }
    }
    let mut encoded = Vec::with_capacity(rgba.len() / 4 * 6);
    for pixel in rgba.chunks_exact(4) {
        let r = f64::from(pixel[0].max(0.0));
        let g = f64::from(pixel[1].max(0.0));
        let b = f64::from(pixel[2].max(0.0));
        let x =
            0.412_390_799_265_959_5 * r + 0.357_584_339_383_878 * g + 0.180_480_788_401_834_3 * b;
        let y =
            0.212_639_005_871_510_4 * r + 0.715_168_678_767_756 * g + 0.072_192_315_360_733_7 * b;
        let z =
            0.019_330_818_715_591_8 * r + 0.119_194_779_794_625_9 * g + 0.950_532_152_249_660_7 * b;
        for value in [x, y, z] {
            let normalized = (value * (48.0 / 52.37)).max(0.0);
            let code = (4095.0 * normalized.powf(1.0 / 2.6)).round().clamp(0.0, 4095.0);
            encoded.extend_from_slice(&((code as u16) << 4).to_le_bytes());
        }
    }
    Ok(encoded)
}
