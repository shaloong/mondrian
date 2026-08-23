use image::ImageFormat;
use mondrian_renderer::{
    ColorReferenceDecoder, ColorReferenceDescriptor, ColorReferenceEncoding,
    ColorReferencePayloadFormat, ColorReferencePixels,
};

pub struct ImageColorReferenceDecoder;

impl ColorReferenceDecoder for ImageColorReferenceDecoder {
    fn decode(
        &self,
        encoded: &[u8],
        descriptor: &ColorReferenceDescriptor,
    ) -> Result<ColorReferencePixels, String> {
        if descriptor.payload_format == ColorReferencePayloadFormat::JsonFloat {
            return serde_json::from_slice::<Vec<[f32; 4]>>(encoded)
                .map(ColorReferencePixels::RgbaF32)
                .map_err(|error| error.to_string());
        }
        let format = match descriptor.payload_format {
            ColorReferencePayloadFormat::Png => ImageFormat::Png,
            ColorReferencePayloadFormat::OpenExr => ImageFormat::OpenExr,
            ColorReferencePayloadFormat::JsonFloat => unreachable!("handled above"),
        };
        let image = image::load_from_memory_with_format(encoded, format)
            .map_err(|error| error.to_string())?;
        match descriptor.encoding {
            ColorReferenceEncoding::SrgbDisplayRgba8 | ColorReferenceEncoding::DisplayP3Rgba8 => {
                Ok(ColorReferencePixels::Rgba8(image.to_rgba8().into_raw()))
            }
            ColorReferenceEncoding::Bt2100PqRgbaF32
            | ColorReferenceEncoding::Bt2100HlgRgbaF32
            | ColorReferenceEncoding::SceneLinearRec2020RgbaF32
            | ColorReferenceEncoding::CieLabD50F32 => Ok(ColorReferencePixels::RgbaF32(
                image.to_rgba32f().pixels().map(|pixel| pixel.0).collect(),
            )),
        }
    }
}
