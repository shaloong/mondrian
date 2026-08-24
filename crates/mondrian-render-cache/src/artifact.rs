use crate::{TimelineRenderCacheAlpha, TimelineRenderCacheFormat, TimelineRenderCacheIdentity};
use mondrian_core::{WorkingColorSpace, WorkingRgbaF32Frame};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};

const MAGIC: &[u8; 8] = b"MNDRC001";
const SCHEMA_VERSION: u16 = 1;
const HEADER_BYTES: usize = 101;
const FLOATS_PER_PIXEL: u64 = 4;
const BYTES_PER_FLOAT: u64 = 4;

/// Immutable post-composite working-linear frame admitted to the cache.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineRenderCacheFrame {
    identity: TimelineRenderCacheIdentity,
    frame: WorkingRgbaF32Frame,
}

impl TimelineRenderCacheFrame {
    /// Validate and bind one frame to its complete semantic identity.
    pub fn new(
        identity: TimelineRenderCacheIdentity,
        frame: WorkingRgbaF32Frame,
    ) -> Result<Self, TimelineRenderCacheFrameValidationError> {
        let expected_pixels = u64::from(frame.width)
            .checked_mul(u64::from(frame.height))
            .ok_or(TimelineRenderCacheFrameValidationError::ExtentOverflow)?;
        let actual_pixels = u64::try_from(frame.data.len())
            .map_err(|_| TimelineRenderCacheFrameValidationError::ExtentOverflow)?;
        if actual_pixels != expected_pixels {
            return Err(TimelineRenderCacheFrameValidationError::StorageLength {
                expected_pixels,
                actual_pixels,
            });
        }
        Ok(Self { identity, frame })
    }

    /// Complete semantic artifact identity.
    pub const fn identity(&self) -> TimelineRenderCacheIdentity {
        self.identity
    }

    /// Borrow the immutable working-linear pixels.
    pub const fn frame(&self) -> &WorkingRgbaF32Frame {
        &self.frame
    }

    /// Consume the artifact and return its working-linear pixels.
    pub fn into_frame(self) -> WorkingRgbaF32Frame {
        self.frame
    }
}

/// Invalid working-frame shape supplied by a cache producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimelineRenderCacheFrameValidationError {
    /// Width/height byte arithmetic overflowed.
    #[error("Timeline render-cache frame extent overflowed")]
    ExtentOverflow,
    /// Pixel storage is not exactly width × height.
    #[error(
        "Timeline render-cache frame storage has {actual_pixels} pixels; expected {expected_pixels}"
    )]
    StorageLength {
        /// Expected pixel count.
        expected_pixels: u64,
        /// Actual pixel count.
        actual_pixels: u64,
    },
}

/// Failure while encoding or validating one persistent cache artifact.
#[derive(Debug, thiserror::Error)]
pub enum TimelineRenderCacheArtifactError {
    /// Artifact header is incomplete or malformed.
    #[error("invalid Timeline render-cache artifact header: {0}")]
    InvalidHeader(&'static str),
    /// Artifact identity differs from the requested content address.
    #[error("Timeline render-cache artifact identity does not match its requested key")]
    IdentityMismatch,
    /// Artifact working-space tag differs from the semantic lookup contract.
    #[error("Timeline render-cache artifact working color space does not match its request")]
    WorkingColorSpaceMismatch,
    /// Artifact exceeds the configured compressed or decoded byte bound.
    #[error("Timeline render-cache artifact requires {required} bytes; limit is {limit}")]
    ByteLimit { required: u64, limit: u64 },
    /// Width/height/sample-size arithmetic overflowed.
    #[error("Timeline render-cache artifact extent overflowed")]
    ExtentOverflow,
    /// Compressed payload length differs from the header.
    #[error("Timeline render-cache payload has {actual} bytes; expected {expected}")]
    PayloadLength { expected: u64, actual: u64 },
    /// Decoded payload length differs from the exact frame contract.
    #[error("Timeline render-cache decoded {actual} bytes; expected {expected}")]
    DecodedLength { expected: u64, actual: u64 },
    /// Payload checksum does not match the durable header.
    #[error("Timeline render-cache payload checksum mismatch")]
    ChecksumMismatch,
    /// Compression or decompression failed.
    #[error("Timeline render-cache codec failed: {0}")]
    Codec(#[from] std::io::Error),
    /// Reconstructed frame failed its public shape contract.
    #[error(transparent)]
    Frame(#[from] TimelineRenderCacheFrameValidationError),
}

pub(crate) fn encode_artifact(
    cached: &TimelineRenderCacheFrame,
    max_artifact_bytes: u64,
) -> Result<Vec<u8>, TimelineRenderCacheArtifactError> {
    let frame = cached.frame();
    let decoded_bytes = decoded_frame_bytes(frame.width, frame.height)?;
    enforce_limit(decoded_bytes, max_artifact_bytes)?;
    let mut raw = Vec::with_capacity(usize::try_from(decoded_bytes).map_err(|_| {
        TimelineRenderCacheArtifactError::ByteLimit {
            required: decoded_bytes,
            limit: usize::MAX as u64,
        }
    })?);
    for pixel in &frame.data {
        for component in pixel {
            raw.extend_from_slice(&component.to_bits().to_le_bytes());
        }
    }
    let payload_checksum: [u8; 32] = Sha256::digest(&raw).into();
    let compressed = zstd::stream::encode_all(Cursor::new(raw), 1)?;
    let compressed_len = u64::try_from(compressed.len())
        .map_err(|_| TimelineRenderCacheArtifactError::ExtentOverflow)?;
    let total_len = (HEADER_BYTES as u64)
        .checked_add(compressed_len)
        .ok_or(TimelineRenderCacheArtifactError::ExtentOverflow)?;
    enforce_limit(total_len, max_artifact_bytes)?;

    let mut bytes = Vec::with_capacity(HEADER_BYTES.saturating_add(compressed.len()));
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    bytes.push(TimelineRenderCacheFormat::LosslessRgba32FloatZstd.code());
    bytes.push(TimelineRenderCacheAlpha::StraightCoverage.code());
    bytes.push(working_color_space_code(frame.color_space));
    bytes.extend_from_slice(&frame.width.to_le_bytes());
    bytes.extend_from_slice(&frame.height.to_le_bytes());
    bytes.extend_from_slice(&decoded_bytes.to_le_bytes());
    bytes.extend_from_slice(&compressed_len.to_le_bytes());
    bytes.extend_from_slice(&cached.identity().digest());
    bytes.extend_from_slice(&payload_checksum);
    debug_assert_eq!(bytes.len(), HEADER_BYTES);
    bytes.extend_from_slice(&compressed);
    Ok(bytes)
}

pub(crate) fn decode_artifact(
    requested: TimelineRenderCacheIdentity,
    color_space: WorkingColorSpace,
    bytes: &[u8],
    max_artifact_bytes: u64,
) -> Result<TimelineRenderCacheFrame, TimelineRenderCacheArtifactError> {
    let artifact_len =
        u64::try_from(bytes.len()).map_err(|_| TimelineRenderCacheArtifactError::ExtentOverflow)?;
    enforce_limit(artifact_len, max_artifact_bytes)?;
    if bytes.len() < HEADER_BYTES {
        return Err(TimelineRenderCacheArtifactError::InvalidHeader("truncated"));
    }
    let mut cursor = Cursor::new(bytes);
    let mut magic = [0_u8; 8];
    cursor.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(TimelineRenderCacheArtifactError::InvalidHeader("magic"));
    }
    let schema = read_u16(&mut cursor)?;
    if schema != SCHEMA_VERSION {
        return Err(TimelineRenderCacheArtifactError::InvalidHeader(
            "schema version",
        ));
    }
    let format = read_u8(&mut cursor)?;
    if TimelineRenderCacheFormat::from_code(format)
        != Some(TimelineRenderCacheFormat::LosslessRgba32FloatZstd)
    {
        return Err(TimelineRenderCacheArtifactError::InvalidHeader("format"));
    }
    let alpha = read_u8(&mut cursor)?;
    if TimelineRenderCacheAlpha::from_code(alpha)
        != Some(TimelineRenderCacheAlpha::StraightCoverage)
    {
        return Err(TimelineRenderCacheArtifactError::InvalidHeader("alpha"));
    }
    let stored_working_color_space = read_u8(&mut cursor)?;
    if stored_working_color_space != working_color_space_code(color_space) {
        return Err(TimelineRenderCacheArtifactError::WorkingColorSpaceMismatch);
    }
    let width = read_u32(&mut cursor)?;
    let height = read_u32(&mut cursor)?;
    let declared_decoded_len = read_u64(&mut cursor)?;
    let compressed_len = read_u64(&mut cursor)?;
    let mut identity = [0_u8; 32];
    cursor.read_exact(&mut identity)?;
    if identity != requested.digest() {
        return Err(TimelineRenderCacheArtifactError::IdentityMismatch);
    }
    let mut expected_checksum = [0_u8; 32];
    cursor.read_exact(&mut expected_checksum)?;
    let expected_decoded_len = decoded_frame_bytes(width, height)?;
    if declared_decoded_len != expected_decoded_len {
        return Err(TimelineRenderCacheArtifactError::DecodedLength {
            expected: expected_decoded_len,
            actual: declared_decoded_len,
        });
    }
    enforce_limit(expected_decoded_len, max_artifact_bytes)?;
    let actual_compressed_len = artifact_len.saturating_sub(HEADER_BYTES as u64);
    if actual_compressed_len != compressed_len {
        return Err(TimelineRenderCacheArtifactError::PayloadLength {
            expected: compressed_len,
            actual: actual_compressed_len,
        });
    }
    let decoder = zstd::stream::read::Decoder::new(&bytes[HEADER_BYTES..])?;
    let decode_capacity = usize::try_from(expected_decoded_len)
        .map_err(|_| TimelineRenderCacheArtifactError::ExtentOverflow)?;
    let mut raw = Vec::with_capacity(decode_capacity);
    decoder.take(expected_decoded_len.saturating_add(1)).read_to_end(&mut raw)?;
    let actual_decoded_len =
        u64::try_from(raw.len()).map_err(|_| TimelineRenderCacheArtifactError::ExtentOverflow)?;
    if actual_decoded_len != expected_decoded_len {
        return Err(TimelineRenderCacheArtifactError::DecodedLength {
            expected: expected_decoded_len,
            actual: actual_decoded_len,
        });
    }
    let actual_checksum: [u8; 32] = Sha256::digest(&raw).into();
    if actual_checksum != expected_checksum {
        return Err(TimelineRenderCacheArtifactError::ChecksumMismatch);
    }
    let mut data = Vec::with_capacity(raw.len() / 16);
    for pixel in raw.chunks_exact(16) {
        data.push([
            f32::from_bits(u32::from_le_bytes(pixel[0..4].try_into().map_err(
                |_| TimelineRenderCacheArtifactError::InvalidHeader("pixel"),
            )?)),
            f32::from_bits(u32::from_le_bytes(pixel[4..8].try_into().map_err(
                |_| TimelineRenderCacheArtifactError::InvalidHeader("pixel"),
            )?)),
            f32::from_bits(u32::from_le_bytes(pixel[8..12].try_into().map_err(
                |_| TimelineRenderCacheArtifactError::InvalidHeader("pixel"),
            )?)),
            f32::from_bits(u32::from_le_bytes(pixel[12..16].try_into().map_err(
                |_| TimelineRenderCacheArtifactError::InvalidHeader("pixel"),
            )?)),
        ]);
    }
    TimelineRenderCacheFrame::new(
        requested,
        WorkingRgbaF32Frame { width, height, data, color_space },
    )
    .map_err(Into::into)
}

fn decoded_frame_bytes(width: u32, height: u32) -> Result<u64, TimelineRenderCacheArtifactError> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(FLOATS_PER_PIXEL))
        .and_then(|floats| floats.checked_mul(BYTES_PER_FLOAT))
        .ok_or(TimelineRenderCacheArtifactError::ExtentOverflow)
}

fn enforce_limit(required: u64, limit: u64) -> Result<(), TimelineRenderCacheArtifactError> {
    if required > limit {
        return Err(TimelineRenderCacheArtifactError::ByteLimit { required, limit });
    }
    Ok(())
}

const fn working_color_space_code(color_space: WorkingColorSpace) -> u8 {
    match color_space {
        WorkingColorSpace::LinearRec709 => 1,
        WorkingColorSpace::LinearRec2020 => 2,
        WorkingColorSpace::LinearP3D65 => 3,
        WorkingColorSpace::AcesCg => 4,
    }
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> Result<u8, std::io::Error> {
    let mut bytes = [0_u8; 1];
    cursor.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> Result<u16, std::io::Error> {
    let mut bytes = [0_u8; 2];
    cursor.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, std::io::Error> {
    let mut bytes = [0_u8; 4];
    cursor.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> Result<u64, std::io::Error> {
    let mut bytes = [0_u8; 8];
    cursor.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached_frame() -> TimelineRenderCacheFrame {
        TimelineRenderCacheFrame::new(
            TimelineRenderCacheIdentity::from_digest([7; 32]),
            WorkingRgbaF32Frame {
                width: 2,
                height: 1,
                data: vec![[1.25, -0.5, 8.0, 1.0], [f32::NAN, 0.0, -1.0, 0.25]],
                color_space: WorkingColorSpace::LinearRec709,
            },
        )
        .expect("valid frame")
    }

    #[test]
    fn lossless_artifact_roundtrips_float_bits() {
        let frame = cached_frame();
        let bytes = encode_artifact(&frame, 1_048_576).expect("encode");
        let decoded = decode_artifact(
            frame.identity(),
            WorkingColorSpace::LinearRec709,
            &bytes,
            1_048_576,
        )
        .expect("decode");
        for (actual, expected) in decoded.frame().data.iter().zip(&frame.frame().data) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
    }

    #[test]
    fn corrupt_payload_fails_checksum_or_codec() {
        let frame = cached_frame();
        let mut bytes = encode_artifact(&frame, 1_048_576).expect("encode");
        let last = bytes.len() - 1;
        bytes[last] ^= 0x40;
        assert!(decode_artifact(
            frame.identity(),
            WorkingColorSpace::LinearRec709,
            &bytes,
            1_048_576,
        )
        .is_err());
    }

    #[test]
    fn decode_bound_is_checked_before_decompression() {
        let frame = cached_frame();
        let bytes = encode_artifact(&frame, 1_048_576).expect("encode");
        assert!(matches!(
            decode_artifact(
                frame.identity(),
                WorkingColorSpace::LinearRec709,
                &bytes,
                16,
            ),
            Err(TimelineRenderCacheArtifactError::ByteLimit { .. })
        ));
    }

    #[test]
    fn artifact_rejects_wrong_working_color_contract() {
        let frame = cached_frame();
        let bytes = encode_artifact(&frame, 1_048_576).expect("encode");
        assert!(matches!(
            decode_artifact(
                frame.identity(),
                WorkingColorSpace::AcesCg,
                &bytes,
                1_048_576,
            ),
            Err(TimelineRenderCacheArtifactError::WorkingColorSpaceMismatch)
        ));
    }
}
