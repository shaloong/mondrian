//! Typed storage contract for frames crossing the FFmpeg raw-video pipe.
//!
//! Renderer execution precision and pipe sample representation are related but
//! distinct facts. In particular, a renderer `Rgba16Float` texture may be the
//! precision-preserving source for an encoded 16-bit UNORM pipe; that does not
//! make `rgba64le` a floating-point format.

use half::f16;
use mondrian_renderer::GpuColorFrameTextureFormat;
use mondrian_timeline::sequence::DeliveryBitDepth;

/// Exact sample representation and layout of one FFmpeg raw-video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFrameContract {
    /// Interleaved encoded RGBA with four 8-bit unsigned-normalized channels.
    EncodedRgba8Unorm,
    /// Interleaved encoded RGBA with four little-endian 16-bit
    /// unsigned-normalized channels.
    EncodedRgba16Unorm,
    /// Interleaved true IEEE-754 binary16 RGBA master samples.
    FloatMasterRgba16,
    /// Planar true IEEE-754 binary32 GBR+A master samples.
    ///
    /// FFmpeg names this layout `gbrapf32le`; planes are ordered G, B, R, A.
    FloatMasterRgba32,
}

/// Failure to materialize or inspect an exact export pipe frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExportFramePackingError {
    /// An interleaved input did not contain a complete number of RGBA pixels.
    #[error("export RGBA component count {components} is not divisible by four")]
    InvalidRgbaComponentCount {
        /// Number of supplied components.
        components: usize,
    },
    /// Packed bytes did not contain a complete number of pixels for the
    /// selected frame contract.
    #[error(
        "export pipe byte count {bytes} is not divisible by {bytes_per_pixel} bytes per pixel"
    )]
    InvalidPipeByteCount {
        /// Number of supplied bytes.
        bytes: usize,
        /// Exact storage bytes per pixel for the selected contract.
        bytes_per_pixel: usize,
    },
    /// A float output sample was NaN or infinite.
    #[error("export float component {component_index} is not finite")]
    NonFiniteComponent {
        /// Component index in interleaved RGBA order.
        component_index: usize,
    },
    /// A finite Float32 sample cannot be represented as finite Float16.
    #[error("export float component {component_index} exceeds the finite Float16 range")]
    Float16OutOfRange {
        /// Component index in interleaved RGBA order.
        component_index: usize,
    },
}

impl ExportFrameContract {
    /// Select the encoded pipe representation for an admitted codec sample
    /// depth.
    ///
    /// Ten- and twelve-bit codecs consume a 16-bit UNORM staging pipe. They do
    /// not implicitly select a floating-point master contract.
    pub const fn from_bit_depth(bit_depth: DeliveryBitDepth) -> Self {
        match bit_depth {
            DeliveryBitDepth::Eight => Self::EncodedRgba8Unorm,
            DeliveryBitDepth::Ten | DeliveryBitDepth::Twelve => Self::EncodedRgba16Unorm,
        }
    }

    /// GPU output-boundary texture used before pipe serialization.
    ///
    /// `EncodedRgba16Unorm` intentionally uses an `Rgba16Float` render target
    /// so color transforms are not quantized until the explicit UNORM pack.
    pub const fn gpu_boundary_texture_format(self) -> GpuColorFrameTextureFormat {
        match self {
            Self::EncodedRgba8Unorm => GpuColorFrameTextureFormat::Rgba8Unorm,
            Self::EncodedRgba16Unorm | Self::FloatMasterRgba16 => {
                GpuColorFrameTextureFormat::Rgba16Float
            }
            Self::FloatMasterRgba32 => GpuColorFrameTextureFormat::Rgba32Float,
        }
    }

    /// FFmpeg input pixel format for this exact raw-video byte layout.
    pub const fn ffmpeg_pix_fmt(self) -> &'static str {
        match self {
            Self::EncodedRgba8Unorm => "rgba",
            Self::EncodedRgba16Unorm => "rgba64le",
            Self::FloatMasterRgba16 => "rgbaf16le",
            Self::FloatMasterRgba32 => "gbrapf32le",
        }
    }

    /// Exact storage bytes per pixel, including all four channels.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::EncodedRgba8Unorm => 4,
            Self::EncodedRgba16Unorm | Self::FloatMasterRgba16 => 8,
            Self::FloatMasterRgba32 => 16,
        }
    }

    /// Canvas byte length for the supplied dimensions.
    pub fn canvas_len(self, width: u32, height: u32) -> usize {
        width as usize * height as usize * self.bytes_per_pixel()
    }

    /// Whether serialization requires the renderer's encoded-float output
    /// boundary rather than an RGBA8 boundary.
    pub const fn requires_float_output_boundary(self) -> bool {
        !matches!(self, Self::EncodedRgba8Unorm)
    }

    /// Whether the pipe stores true floating-point master samples.
    pub const fn is_float_master(self) -> bool {
        matches!(self, Self::FloatMasterRgba16 | Self::FloatMasterRgba32)
    }

    /// Whether finite input components are clamped to `[0, 1]` during packing.
    pub const fn clamps_to_normalized_range(self) -> bool {
        matches!(self, Self::EncodedRgba8Unorm | Self::EncodedRgba16Unorm)
    }

    /// Pack interleaved RGBA8 values into this contract's exact pipe layout.
    pub fn pack_rgba8(self, rgba: &[u8]) -> Result<Vec<u8>, ExportFramePackingError> {
        validate_rgba_component_count(rgba.len())?;
        match self {
            Self::EncodedRgba8Unorm => Ok(rgba.to_vec()),
            Self::EncodedRgba16Unorm => {
                let mut out = Vec::with_capacity(rgba.len() * 2);
                for channel in rgba {
                    out.extend_from_slice(&u16::from(*channel).saturating_mul(257).to_le_bytes());
                }
                Ok(out)
            }
            Self::FloatMasterRgba16 => {
                let mut out = Vec::with_capacity(rgba.len() * 2);
                for channel in rgba {
                    out.extend_from_slice(
                        &f16::from_f32(f32::from(*channel) / 255.0).to_le_bytes(),
                    );
                }
                Ok(out)
            }
            Self::FloatMasterRgba32 => Ok(pack_gbrap_f32_from_rgba8(rgba)),
        }
    }

    /// Pack interleaved encoded Float32 RGBA values into the exact pipe layout.
    ///
    /// UNORM contracts validate finite input, clamp to `[0, 1]`, and quantize
    /// once. Float master contracts validate finite input but never clamp.
    pub fn pack_rgba_f32(self, rgba: &[f32]) -> Result<Vec<u8>, ExportFramePackingError> {
        validate_rgba_component_count(rgba.len())?;
        validate_finite(rgba)?;
        match self {
            Self::EncodedRgba8Unorm => Ok(rgba
                .iter()
                .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
                .collect()),
            Self::EncodedRgba16Unorm => {
                let mut out = Vec::with_capacity(rgba.len() * 2);
                for channel in rgba {
                    let value = (channel.clamp(0.0, 1.0) * 65_535.0).round() as u16;
                    out.extend_from_slice(&value.to_le_bytes());
                }
                Ok(out)
            }
            Self::FloatMasterRgba16 => {
                let mut out = Vec::with_capacity(rgba.len() * 2);
                let max = f16::MAX.to_f32();
                for (component_index, channel) in rgba.iter().copied().enumerate() {
                    if channel.abs() > max {
                        return Err(ExportFramePackingError::Float16OutOfRange { component_index });
                    }
                    out.extend_from_slice(&f16::from_f32(channel).to_le_bytes());
                }
                Ok(out)
            }
            Self::FloatMasterRgba32 => pack_gbrap_f32(rgba),
        }
    }

    /// Convert exact pipe bytes to an RGBA8 inspection boundary.
    ///
    /// This is an explicit lossy inspection helper, never an encoder path.
    pub fn to_rgba8_boundary(self, bytes: &[u8]) -> Result<Vec<u8>, ExportFramePackingError> {
        validate_pipe_byte_count(bytes.len(), self.bytes_per_pixel())?;
        match self {
            Self::EncodedRgba8Unorm => Ok(bytes.to_vec()),
            Self::EncodedRgba16Unorm => Ok(bytes
                .chunks_exact(2)
                .map(|channel| {
                    let value = u16::from_le_bytes([channel[0], channel[1]]);
                    (f32::from(value) * (255.0 / 65_535.0)).round() as u8
                })
                .collect()),
            Self::FloatMasterRgba16 => {
                let mut out = Vec::with_capacity(bytes.len() / 2);
                for (component_index, channel) in bytes.chunks_exact(2).enumerate() {
                    let value = f16::from_le_bytes([channel[0], channel[1]]).to_f32();
                    if !value.is_finite() {
                        return Err(ExportFramePackingError::NonFiniteComponent {
                            component_index,
                        });
                    }
                    out.push((value.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
                Ok(out)
            }
            Self::FloatMasterRgba32 => unpack_gbrap_f32_to_rgba8(bytes),
        }
    }

    /// Replace `canvas` with an opaque black frame in this contract's exact
    /// storage layout without allocating an intermediate Float32 raster.
    pub fn fill_black_opaque(self, canvas: &mut Vec<u8>, width: u32, height: u32) {
        let pixels = width as usize * height as usize;
        canvas.clear();
        canvas.resize(self.canvas_len(width, height), 0);
        match self {
            Self::EncodedRgba8Unorm => {
                for pixel in canvas.chunks_exact_mut(4) {
                    pixel[3] = u8::MAX;
                }
            }
            Self::EncodedRgba16Unorm => {
                for pixel in canvas.chunks_exact_mut(8) {
                    pixel[6..8].copy_from_slice(&u16::MAX.to_le_bytes());
                }
            }
            Self::FloatMasterRgba16 => {
                let one = f16::ONE.to_le_bytes();
                for pixel in canvas.chunks_exact_mut(8) {
                    pixel[6..8].copy_from_slice(&one);
                }
            }
            Self::FloatMasterRgba32 => {
                let alpha_plane = 3 * pixels * std::mem::size_of::<f32>();
                for alpha in canvas[alpha_plane..].chunks_exact_mut(4) {
                    alpha.copy_from_slice(&1.0f32.to_le_bytes());
                }
            }
        }
    }
}

fn validate_rgba_component_count(components: usize) -> Result<(), ExportFramePackingError> {
    if components.is_multiple_of(4) {
        Ok(())
    } else {
        Err(ExportFramePackingError::InvalidRgbaComponentCount { components })
    }
}

fn validate_pipe_byte_count(
    bytes: usize,
    bytes_per_pixel: usize,
) -> Result<(), ExportFramePackingError> {
    if bytes.is_multiple_of(bytes_per_pixel) {
        Ok(())
    } else {
        Err(ExportFramePackingError::InvalidPipeByteCount { bytes, bytes_per_pixel })
    }
}

fn validate_finite(rgba: &[f32]) -> Result<(), ExportFramePackingError> {
    if let Some((component_index, _)) =
        rgba.iter().enumerate().find(|(_, value)| !value.is_finite())
    {
        return Err(ExportFramePackingError::NonFiniteComponent { component_index });
    }
    Ok(())
}

fn pack_gbrap_f32(rgba: &[f32]) -> Result<Vec<u8>, ExportFramePackingError> {
    validate_rgba_component_count(rgba.len())?;
    validate_finite(rgba)?;
    let pixels = rgba.len() / 4;
    let plane_bytes = pixels * std::mem::size_of::<f32>();
    let mut out = vec![0u8; std::mem::size_of_val(rgba)];
    for (pixel_index, pixel) in rgba.chunks_exact(4).enumerate() {
        for (plane, component) in [1usize, 2, 0, 3].into_iter().enumerate() {
            let offset = plane * plane_bytes + pixel_index * std::mem::size_of::<f32>();
            out[offset..offset + 4].copy_from_slice(&pixel[component].to_le_bytes());
        }
    }
    Ok(out)
}

fn pack_gbrap_f32_from_rgba8(rgba: &[u8]) -> Vec<u8> {
    let pixels = rgba.len() / 4;
    let plane_bytes = pixels * std::mem::size_of::<f32>();
    let mut out = vec![0u8; pixels * 4 * std::mem::size_of::<f32>()];
    for (pixel_index, pixel) in rgba.chunks_exact(4).enumerate() {
        for (plane, component) in [1usize, 2, 0, 3].into_iter().enumerate() {
            let offset = plane * plane_bytes + pixel_index * std::mem::size_of::<f32>();
            let value = f32::from(pixel[component]) / 255.0;
            out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    out
}

fn unpack_gbrap_f32_to_rgba8(bytes: &[u8]) -> Result<Vec<u8>, ExportFramePackingError> {
    validate_pipe_byte_count(bytes.len(), 16)?;
    let pixels = bytes.len() / 16;
    let plane_bytes = pixels * std::mem::size_of::<f32>();
    let mut out = Vec::with_capacity(pixels * 4);
    for pixel_index in 0..pixels {
        let mut rgba = [0.0f32; 4];
        for (plane, component) in [1usize, 2, 0, 3].into_iter().enumerate() {
            let offset = plane * plane_bytes + pixel_index * std::mem::size_of::<f32>();
            rgba[component] = f32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);
        }
        validate_finite(&rgba)?;
        out.extend(rgba.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Stdio;

    #[test]
    fn codec_depth_selects_only_encoded_unorm_pipe_contracts() {
        assert_eq!(
            ExportFrameContract::from_bit_depth(DeliveryBitDepth::Eight),
            ExportFrameContract::EncodedRgba8Unorm
        );
        assert_eq!(
            ExportFrameContract::from_bit_depth(DeliveryBitDepth::Ten),
            ExportFrameContract::EncodedRgba16Unorm
        );
        assert_eq!(
            ExportFrameContract::from_bit_depth(DeliveryBitDepth::Twelve),
            ExportFrameContract::EncodedRgba16Unorm
        );
    }

    #[test]
    fn pipe_metadata_names_real_storage_not_renderer_intermediate() {
        let cases = [
            (
                ExportFrameContract::EncodedRgba8Unorm,
                "rgba",
                4,
                false,
                true,
            ),
            (
                ExportFrameContract::EncodedRgba16Unorm,
                "rgba64le",
                8,
                false,
                true,
            ),
            (
                ExportFrameContract::FloatMasterRgba16,
                "rgbaf16le",
                8,
                true,
                false,
            ),
            (
                ExportFrameContract::FloatMasterRgba32,
                "gbrapf32le",
                16,
                true,
                false,
            ),
        ];

        for (contract, pix_fmt, bytes, float_master, clamps) in cases {
            assert_eq!(contract.ffmpeg_pix_fmt(), pix_fmt);
            assert_eq!(contract.bytes_per_pixel(), bytes);
            assert_eq!(contract.is_float_master(), float_master);
            assert_eq!(contract.clamps_to_normalized_range(), clamps);
        }
    }

    #[test]
    fn unorm16_clamps_and_quantizes_normalized_float_components() {
        let packed = ExportFrameContract::EncodedRgba16Unorm
            .pack_rgba_f32(&[-0.25, 0.5, 1.0, 1.5])
            .expect("pack UNORM16");
        let values: Vec<_> = packed
            .chunks_exact(2)
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
            .collect();
        assert_eq!(values, [0, 32_768, 65_535, 65_535]);
    }

    #[test]
    fn float16_master_preserves_extended_range_without_unorm_clamp() {
        let input = [-0.25, 0.5, 1.0, 1.5];
        let packed = ExportFrameContract::FloatMasterRgba16
            .pack_rgba_f32(&input)
            .expect("pack Float16");
        let actual: Vec<_> = packed
            .chunks_exact(2)
            .map(|value| f16::from_le_bytes([value[0], value[1]]).to_f32())
            .collect();
        assert_eq!(actual, input);
    }

    #[test]
    fn float32_master_packs_ffmpeg_gbrap_planes_bit_exactly() {
        let input = [
            -0.25f32, 0.5, 1.5, 0.25, // pixel 0 RGBA
            2.0, -1.0, 0.125, 1.0, // pixel 1 RGBA
        ];
        let packed = ExportFrameContract::FloatMasterRgba32
            .pack_rgba_f32(&input)
            .expect("pack Float32");
        let actual: Vec<_> = packed
            .chunks_exact(4)
            .map(|value| f32::from_le_bytes([value[0], value[1], value[2], value[3]]))
            .collect();
        assert_eq!(actual, [0.5, -1.0, 1.5, 0.125, -0.25, 2.0, 0.25, 1.0]);
    }

    #[test]
    fn float_contracts_fail_closed_on_non_finite_or_half_overflow() {
        assert!(matches!(
            ExportFrameContract::FloatMasterRgba32.pack_rgba_f32(&[0.0, f32::NAN, 0.0, 1.0]),
            Err(ExportFramePackingError::NonFiniteComponent { component_index: 1 })
        ));
        assert!(matches!(
            ExportFrameContract::FloatMasterRgba16.pack_rgba_f32(&[
                0.0,
                f32::from(f16::MAX) * 2.0,
                0.0,
                1.0
            ]),
            Err(ExportFramePackingError::Float16OutOfRange { component_index: 1 })
        ));
    }

    #[test]
    fn black_opaque_fill_honors_interleaved_and_planar_alpha_layouts() {
        for contract in [
            ExportFrameContract::EncodedRgba8Unorm,
            ExportFrameContract::EncodedRgba16Unorm,
            ExportFrameContract::FloatMasterRgba16,
            ExportFrameContract::FloatMasterRgba32,
        ] {
            let mut canvas = Vec::new();
            contract.fill_black_opaque(&mut canvas, 2, 1);
            assert_eq!(canvas.len(), contract.canvas_len(2, 1));
            assert_eq!(
                contract.to_rgba8_boundary(&canvas).expect("inspect black"),
                [0, 0, 0, 255, 0, 0, 0, 255]
            );
        }
    }

    #[test]
    fn packing_rejects_partial_pixels_and_truncated_pipe_bytes() {
        assert!(matches!(
            ExportFrameContract::EncodedRgba8Unorm.pack_rgba_f32(&[0.0; 3]),
            Err(ExportFramePackingError::InvalidRgbaComponentCount { components: 3 })
        ));
        assert!(matches!(
            ExportFrameContract::EncodedRgba16Unorm.to_rgba8_boundary(&[0; 7]),
            Err(ExportFramePackingError::InvalidPipeByteCount { bytes: 7, bytes_per_pixel: 8 })
        ));
    }

    #[test]
    fn bundled_ffmpeg_accepts_every_declared_rawvideo_pipe_layout() {
        for contract in [
            ExportFrameContract::EncodedRgba8Unorm,
            ExportFrameContract::EncodedRgba16Unorm,
            ExportFrameContract::FloatMasterRgba16,
            ExportFrameContract::FloatMasterRgba32,
        ] {
            let mut frame = Vec::new();
            contract.fill_black_opaque(&mut frame, 1, 1);
            let mut child = mondrian_media::ffmpeg_command();
            child
                .arg("-hide_banner")
                .arg("-loglevel")
                .arg("error")
                .arg("-f")
                .arg("rawvideo")
                .arg("-pix_fmt")
                .arg(contract.ffmpeg_pix_fmt())
                .arg("-s")
                .arg("1x1")
                .arg("-i")
                .arg("pipe:0")
                .arg("-frames:v")
                .arg("1")
                .arg("-f")
                .arg("null")
                .arg("-")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            let mut child = child.spawn().expect("launch bundled FFmpeg");
            child
                .stdin
                .take()
                .expect("FFmpeg stdin")
                .write_all(&frame)
                .expect("write exact raw-video frame");
            let output = child.wait_with_output().expect("wait for FFmpeg");
            assert!(
                output.status.success(),
                "FFmpeg rejected {}: {}",
                contract.ffmpeg_pix_fmt(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
