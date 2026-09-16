//! Interlaced Program Output assembly.
//!
//! The renderer supplies two complete progressive Program samples in display
//! order. This Module owns the field-safe vertical prefilter and row extraction;
//! FFmpeg receives one already-woven encoded picture and owns only codec/muxer
//! signaling. Pair assembly is in-memory and atomic, so cancellation can never
//! publish half a field pair.

use crate::frame_contract::ExportFrameContract;
use mondrian_renderer::picture_sampling::{
    PictureFieldLineParity, PictureSampleAddress, PictureSamplePhase,
};

/// Assemble one encoded interlaced picture from two progressive Program samples.
pub(crate) fn assemble_interlaced_program_frame(
    contract: ExportFrameContract,
    width: u32,
    height: u32,
    first_address: PictureSampleAddress,
    first: &[u8],
    second_address: PictureSampleAddress,
    second: &[u8],
    destination: &mut Vec<u8>,
) -> Result<(), InterlacedDeliveryError> {
    if height == 0 || width == 0 || !height.is_multiple_of(2) {
        return Err(InterlacedDeliveryError::InvalidRaster { width, height });
    }
    if first_address.encoded_frame() != second_address.encoded_frame()
        || first_address.phase() != PictureSamplePhase::FirstField
        || second_address.phase() != PictureSamplePhase::SecondField
    {
        return Err(InterlacedDeliveryError::InvalidFieldPair);
    }
    let expected = contract.canvas_len(width, height);
    if first.len() != expected || second.len() != expected {
        return Err(InterlacedDeliveryError::InvalidCanvasLength {
            expected,
            first: first.len(),
            second: second.len(),
        });
    }
    let first_parity = first_address
        .field_line_parity()
        .ok_or(InterlacedDeliveryError::InvalidFieldPair)?;
    let second_parity = second_address
        .field_line_parity()
        .ok_or(InterlacedDeliveryError::InvalidFieldPair)?;
    if first_parity == second_parity {
        return Err(InterlacedDeliveryError::InvalidFieldPair);
    }
    destination.resize(expected, 0);
    match contract {
        ExportFrameContract::EncodedRgba8Unorm => assemble_u8(
            width,
            height,
            first_parity,
            first,
            second_parity,
            second,
            destination,
        ),
        ExportFrameContract::EncodedRgba16Unorm => assemble_u16_le(
            width,
            height,
            first_parity,
            first,
            second_parity,
            second,
            destination,
        ),
        ExportFrameContract::FloatMasterRgba16 | ExportFrameContract::FloatMasterRgba32 => {
            return Err(InterlacedDeliveryError::FloatMasterUnsupported)
        }
    }
    Ok(())
}

fn assemble_u8(
    width: u32,
    height: u32,
    first_parity: PictureFieldLineParity,
    first: &[u8],
    second_parity: PictureFieldLineParity,
    second: &[u8],
    destination: &mut [u8],
) {
    let row_components = width as usize * 4;
    for row in 0..height as usize {
        let parity = if row % 2 == 0 {
            PictureFieldLineParity::Top
        } else {
            PictureFieldLineParity::Bottom
        };
        let source = if parity == first_parity {
            first
        } else {
            debug_assert_eq!(parity, second_parity);
            second
        };
        let previous = row.saturating_sub(1);
        let next = (row + 1).min(height as usize - 1);
        for component in 0..row_components {
            let above = u16::from(source[previous * row_components + component]);
            let current = u16::from(source[row * row_components + component]);
            let below = u16::from(source[next * row_components + component]);
            destination[row * row_components + component] =
                ((above + current * 2 + below + 2) / 4) as u8;
        }
    }
}

fn assemble_u16_le(
    width: u32,
    height: u32,
    first_parity: PictureFieldLineParity,
    first: &[u8],
    second_parity: PictureFieldLineParity,
    second: &[u8],
    destination: &mut [u8],
) {
    let row_components = width as usize * 4;
    let sample = |bytes: &[u8], row: usize, component: usize| {
        let offset = (row * row_components + component) * 2;
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    };
    for row in 0..height as usize {
        let parity = if row % 2 == 0 {
            PictureFieldLineParity::Top
        } else {
            PictureFieldLineParity::Bottom
        };
        let source = if parity == first_parity {
            first
        } else {
            debug_assert_eq!(parity, second_parity);
            second
        };
        let previous = row.saturating_sub(1);
        let next = (row + 1).min(height as usize - 1);
        for component in 0..row_components {
            let filtered = (u32::from(sample(source, previous, component))
                + u32::from(sample(source, row, component)) * 2
                + u32::from(sample(source, next, component))
                + 2)
                / 4;
            let offset = (row * row_components + component) * 2;
            destination[offset..offset + 2].copy_from_slice(&(filtered as u16).to_le_bytes());
        }
    }
}

/// Stable interlaced assembly rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum InterlacedDeliveryError {
    /// Field extraction requires an even, non-empty raster.
    #[error("interlaced delivery requires an even non-empty raster, got {width}x{height}")]
    InvalidRaster { width: u32, height: u32 },
    /// Addresses did not form one first/second display-time pair.
    #[error("interlaced delivery addresses do not form one complete field pair")]
    InvalidFieldPair,
    /// One sample did not match the exact pipe layout.
    #[error("interlaced delivery canvas length mismatch: expected={expected}, first={first}, second={second}")]
    InvalidCanvasLength {
        expected: usize,
        first: usize,
        second: usize,
    },
    /// Float/image masters remain progressive-only in the qualified matrix.
    #[error("interlaced float-master delivery is not qualified")]
    FloatMasterUnsupported,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{timeline_data::FieldOrder, Rational};
    use mondrian_renderer::picture_sampling::{ProgramPictureSamples, ProgramPictureSampling};

    #[test]
    fn tff_assembly_extracts_even_rows_from_first_time_and_odd_from_second() {
        let ProgramPictureSamples::Interlaced { first, second } =
            ProgramPictureSampling::new(Rational::new(1, 25), FieldOrder::UpperFirst)
                .samples(0)
                .expect("samples")
        else {
            panic!("interlaced")
        };
        let width = 1;
        let height = 4;
        let first_pixels = vec![
            10, 10, 10, 255, 20, 20, 20, 255, 30, 30, 30, 255, 40, 40, 40, 255,
        ];
        let second_pixels = vec![
            100, 100, 100, 255, 110, 110, 110, 255, 120, 120, 120, 255, 130, 130, 130, 255,
        ];
        let mut woven = Vec::new();
        assemble_interlaced_program_frame(
            ExportFrameContract::EncodedRgba8Unorm,
            width,
            height,
            first,
            &first_pixels,
            second,
            &second_pixels,
            &mut woven,
        )
        .expect("assemble");
        // 3-tap field-safe prefilter: row 0 -> 13, row 1 -> 110,
        // row 2 -> 30, row 3 -> 128.
        assert_eq!(
            [woven[0], woven[4], woven[8], woven[12]],
            [13, 110, 30, 128]
        );
    }
}
