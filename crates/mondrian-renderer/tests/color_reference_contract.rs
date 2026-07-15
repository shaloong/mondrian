use mondrian_renderer::{
    import_external_color_reference, ColorReferenceAlpha, ColorReferenceDecoder,
    ColorReferenceDescriptor, ColorReferenceEncoding, ColorReferenceOrigin,
    ColorReferencePayloadFormat, ColorReferencePixels, ColorReferenceValidationError,
};

#[path = "support/color_reference_image.rs"]
mod color_reference_image;

use color_reference_image::ImageColorReferenceDecoder;
use sha2::{Digest, Sha256};

const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[derive(Clone)]
struct StubDecoder {
    pixels: ColorReferencePixels,
}

impl ColorReferenceDecoder for StubDecoder {
    fn decode(
        &self,
        _encoded: &[u8],
        _descriptor: &ColorReferenceDescriptor,
    ) -> Result<ColorReferencePixels, String> {
        Ok(self.pixels.clone())
    }
}

fn descriptor() -> ColorReferenceDescriptor {
    ColorReferenceDescriptor {
        schema_version: 1,
        reference_id: "resolve-pq-reference-001".to_owned(),
        origin: ColorReferenceOrigin::IndependentApplication,
        producer: "DaVinci Resolve".to_owned(),
        producer_version: "fixture-version".to_owned(),
        source_uri: None,
        source_artifact_sha256: ABC_SHA256.to_owned(),
        content_sha256: ABC_SHA256.to_owned(),
        payload_format: ColorReferencePayloadFormat::OpenExr,
        width: 1,
        height: 1,
        encoding: ColorReferenceEncoding::Bt2100PqRgbaF32,
        alpha: ColorReferenceAlpha::Opaque,
        reference_white_nits: Some(203.0),
        nominal_peak_nits: Some(1_000.0),
    }
}

#[test]
fn image_decoder_imports_a_real_pinned_png_without_promoting_it_to_independent_evidence() {
    const PNG: &[u8] = include_bytes!("golden/opaque_white_64x64.png");
    let descriptor = ColorReferenceDescriptor {
        schema_version: 1,
        reference_id: "mondrian-opaque-white-regression-001".to_owned(),
        origin: ColorReferenceOrigin::MondrianRegression,
        producer: "Mondrian renderer golden suite".to_owned(),
        producer_version: "v1".to_owned(),
        source_uri: None,
        source_artifact_sha256: "2b3f0666835b8ff9add30cda294dc5d6bf241355e91a08c558c0abe05d8ceca7"
            .to_owned(),
        content_sha256: "2b3f0666835b8ff9add30cda294dc5d6bf241355e91a08c558c0abe05d8ceca7"
            .to_owned(),
        payload_format: ColorReferencePayloadFormat::Png,
        width: 64,
        height: 64,
        encoding: ColorReferenceEncoding::SrgbDisplayRgba8,
        alpha: ColorReferenceAlpha::Opaque,
        reference_white_nits: None,
        nominal_peak_nits: None,
    };

    let frame = import_external_color_reference(descriptor, PNG, &ImageColorReferenceDecoder)
        .expect("pinned PNG reference should import");

    assert_eq!(frame.pixel_count(), 64 * 64);
    assert!(!frame.descriptor.is_independent_quality_reference());
    assert!(matches!(frame.pixels, ColorReferencePixels::Rgba8(_)));
}

#[test]
fn image_decoder_imports_float_openexr_without_clipping_scene_linear_range() {
    let image =
        image::Rgba32FImage::from_raw(2, 1, vec![-0.25, 0.18, 2.0, 1.0, 4.0, 16.0, 0.0, 1.0])
            .expect("valid float image shape");
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba32F(image)
        .write_to(&mut encoded, image::ImageFormat::OpenExr)
        .expect("encode float OpenEXR reference");
    let encoded = encoded.into_inner();
    let digest = sha256_hex(&encoded);
    let descriptor = ColorReferenceDescriptor {
        schema_version: 1,
        reference_id: "openexr-scene-linear-import-contract-001".to_owned(),
        origin: ColorReferenceOrigin::MondrianRegression,
        producer: "Reference importer contract fixture".to_owned(),
        producer_version: "v1".to_owned(),
        source_uri: None,
        source_artifact_sha256: ABC_SHA256.to_owned(),
        content_sha256: digest,
        payload_format: ColorReferencePayloadFormat::OpenExr,
        width: 2,
        height: 1,
        encoding: ColorReferenceEncoding::SceneLinearRec2020RgbaF32,
        alpha: ColorReferenceAlpha::Opaque,
        reference_white_nits: None,
        nominal_peak_nits: None,
    };

    let frame = import_external_color_reference(descriptor, &encoded, &ImageColorReferenceDecoder)
        .expect("float OpenEXR reference should import");
    let ColorReferencePixels::RgbaF32(pixels) = frame.pixels else {
        panic!("OpenEXR must decode to float pixels");
    };

    assert_eq!(pixels.len(), 2);
    assert_eq!(pixels[0], [-0.25, 0.18, 2.0, 1.0]);
    assert_eq!(pixels[1], [4.0, 16.0, 0.0, 1.0]);
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[test]
fn external_reference_contract_is_strict_and_records_independent_provenance() {
    let json = serde_json::to_string(&descriptor()).expect("serialize reference descriptor");
    let parsed: ColorReferenceDescriptor =
        serde_json::from_str(&json).expect("deserialize strict reference descriptor");

    assert!(parsed.is_independent_quality_reference());
    assert_eq!(parsed.reference_id, "resolve-pq-reference-001");

    let with_unknown = json.replacen('{', "{\"unexpected\":true,", 1);
    assert!(serde_json::from_str::<ColorReferenceDescriptor>(&with_unknown).is_err());
}

#[test]
fn importer_checks_content_hash_shape_signal_domain_and_opaque_alpha() {
    let decoder = StubDecoder {
        pixels: ColorReferencePixels::RgbaF32(vec![[0.1, 0.5, 1.0, 1.0]]),
    };
    let frame = import_external_color_reference(descriptor(), b"abc", &decoder)
        .expect("valid external PQ reference");
    assert_eq!(frame.pixel_count(), 1);

    let mut bad_hash = descriptor();
    bad_hash.content_sha256 = "0".repeat(64);
    assert!(matches!(
        import_external_color_reference(bad_hash, b"abc", &decoder),
        Err(ColorReferenceValidationError::ContentHashMismatch { .. })
    ));

    let out_of_range = StubDecoder {
        pixels: ColorReferencePixels::RgbaF32(vec![[1.01, 0.0, 0.0, 1.0]]),
    };
    assert!(matches!(
        import_external_color_reference(descriptor(), b"abc", &out_of_range),
        Err(ColorReferenceValidationError::DisplaySignalOutOfRange { .. })
    ));

    let translucent = StubDecoder {
        pixels: ColorReferencePixels::RgbaF32(vec![[0.1, 0.2, 0.3, 0.5]]),
    };
    assert!(matches!(
        import_external_color_reference(descriptor(), b"abc", &translucent),
        Err(ColorReferenceValidationError::OpaqueAlphaMismatch { .. })
    ));
}

#[test]
fn scene_linear_reference_accepts_negative_and_extended_values_but_not_non_finite_data() {
    let mut linear = descriptor();
    linear.reference_id = "public-linear-reference-001".to_owned();
    linear.origin = ColorReferenceOrigin::PublicSpecification;
    linear.producer = "Published numeric reference".to_owned();
    linear.encoding = ColorReferenceEncoding::SceneLinearRec2020RgbaF32;
    linear.reference_white_nits = None;
    linear.nominal_peak_nits = None;

    let extended = StubDecoder {
        pixels: ColorReferencePixels::RgbaF32(vec![[-0.25, 2.0, 16.0, 1.0]]),
    };
    let frame = import_external_color_reference(linear.clone(), b"abc", &extended)
        .expect("scene-linear references preserve extended range");
    assert!(frame.descriptor.is_independent_quality_reference());

    let non_finite = StubDecoder {
        pixels: ColorReferencePixels::RgbaF32(vec![[f32::NAN, 0.0, 0.0, 1.0]]),
    };
    assert!(matches!(
        import_external_color_reference(linear, b"abc", &non_finite),
        Err(ColorReferenceValidationError::NonFiniteSample { .. })
    ));
}

#[test]
fn descriptor_rejects_placeholder_identity_and_invalid_hdr_luminance_contract() {
    let mut placeholder = descriptor();
    placeholder.producer_version = "unknown".to_owned();
    assert!(matches!(
        import_external_color_reference(
            placeholder,
            b"abc",
            &StubDecoder {
                pixels: ColorReferencePixels::RgbaF32(vec![[0.0, 0.0, 0.0, 1.0]])
            }
        ),
        Err(ColorReferenceValidationError::PlaceholderIdentity { field: "producer_version" })
    ));

    let mut invalid_peak = descriptor();
    invalid_peak.nominal_peak_nits = Some(100.0);
    assert!(matches!(
        import_external_color_reference(
            invalid_peak,
            b"abc",
            &StubDecoder {
                pixels: ColorReferencePixels::RgbaF32(vec![[0.0, 0.0, 0.0, 1.0]])
            }
        ),
        Err(ColorReferenceValidationError::InvalidLuminanceContract { .. })
    ));
}
