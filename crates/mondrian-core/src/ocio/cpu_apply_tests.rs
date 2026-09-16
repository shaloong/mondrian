//! Pixel/alpha parity and explicitly non-qualifying CPU layout diagnostics.

use super::*;

fn fixture(count: usize) -> Vec<f32> {
    let alpha = [
        0,
        0x8000_0000,
        0x7fc0_1234,
        0x7f80_0000,
        0x3f00_0000,
        0x3f80_0000,
    ];
    (0..count)
        .flat_map(|index| {
            let value = (index % 257) as f32 / 256.0;
            [
                value * 8.0 - 0.125,
                value * 0.7,
                1.5 - value,
                f32::from_bits(alpha[index % alpha.len()]),
            ]
        })
        .collect()
}

fn assert_bits(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual.to_bits(), expected.to_bits(), "channel {index}");
    }
}

fn assert_bulk_parity(cpu: &CPUProcessor) {
    for count in [0, 1, 3, 1023, 1024, 1025, 4095, 4096, 4097, 8192, 8205] {
        let input = fixture(count);
        let mut expected = input.clone();
        cpu.try_apply_rgb_pixels(&mut expected, count as i64, 4)
            .expect("whole-raster OCIO");
        let mut actual = input.clone();
        apply_cpu_processor_float(cpu, &mut actual);
        assert_bits(&actual, &expected);
        for (before, after) in input.chunks_exact(4).zip(actual.chunks_exact(4)) {
            assert_eq!(before[3].to_bits(), after[3].to_bits(), "alpha bits");
        }
    }
}

#[test]
fn cpu_rgb_chunk_parity_never_exposes_image_alpha_to_custom_matrix() {
    let config = Config::raw().expect("raw config");
    let transform = MatrixTransform::create().expect("matrix");
    transform
        .set_matrix(&[
            1.0, 0.0, 0.0, 0.75, 0.0, 1.0, 0.0, -0.25, 0.0, 0.0, 1.0, 0.5, 0.2, 0.3, 0.4, 1.0,
        ])
        .expect("RGB/alpha coupling");
    let processor = config
        .processor_from_transform(&transform, TransformDirection::Forward)
        .expect("matrix processor");
    let cpu = processor.default_cpu_processor().expect("matrix CPU");
    assert_bulk_parity(&cpu);
    let mut rgba = vec![0.2, 0.4, 0.6, 0.5];
    let mut rgb = rgba.clone();
    cpu.try_apply_rgba_pixels(&mut rgba, 1, 4).expect("RGBA counterexample");
    apply_cpu_processor_float(&cpu, &mut rgb);
    assert_ne!(
        rgb[..3],
        rgba[..3],
        "restoring alpha after direct RGBA would be wrong"
    );
}

#[test]
fn cpu_rgb_chunk_parity_for_standard_views_and_colorimetric_routes() {
    let config = build_mondrian_default_ocio_config(
        mondrian_default_ocio_config_text(),
        MondrianStandardPackageIdentity::V3,
    )
    .expect("pinned Standard config");
    for (display, view) in [
        (
            "Rec.1886 Rec.709 - Display",
            MONDRIAN_STANDARD_SDR_V2_VIEW_NAME,
        ),
        ("Display P3 - Display", MONDRIAN_STANDARD_SDR_V2_VIEW_NAME),
        (
            "Rec.2100-HLG - Display",
            MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
        ),
        (
            "Rec.2100-PQ - Display",
            MONDRIAN_STANDARD_HDR_1000_VIEW_NAME,
        ),
    ] {
        let processor = config
            .processor_display(
                ocio_working_color_space_name(WorkingColorSpace::LinearRec2020),
                display,
                view,
                TransformDirection::Forward,
            )
            .expect("pinned view processor");
        assert_bulk_parity(&processor.default_cpu_processor().expect("view CPU"));
    }
    for (source, target) in [
        (
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
        ),
        (
            OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
            OcioColorSpaceIdentity::Color(ColorSpace::Srgb),
        ),
    ] {
        let processor =
            ocio_processor_from_config(&config, source, target).expect("colorimetric processor");
        assert_bulk_parity(&processor.default_cpu_processor().expect("colorimetric CPU"));
    }
}

#[test]
fn cpu_rgb_chunk_parity_with_owner_dynamic_properties() {
    let config = Config::raw().expect("raw config");
    let transform = ocio_rs::transform::ExposureContrastTransform::create().expect("exposure");
    transform.set_style(ocio_rs::ExposureContrastStyle::Linear);
    transform.set_exposure(0.0);
    transform.set_contrast(1.0);
    transform.set_gamma(1.0);
    transform.make_exposure_dynamic();
    let processor = config
        .processor_from_transform(&transform, TransformDirection::Forward)
        .expect("dynamic processor");
    let cpu = processor.default_cpu_processor().expect("dynamic CPU");
    let property = cpu.dynamic_property(DynamicPropertyType::Exposure).expect("dynamic exposure");
    for exposure in [-2.0, 0.0, 1.25] {
        property.set_double_value(exposure).expect("update exposure");
        assert_bulk_parity(&cpu);
    }
}

#[test]
#[ignore = "manual chunk-size diagnostic, not a performance qualification gate"]
fn cpu_rgb_chunk_sizes_probe() {
    let config = build_mondrian_default_ocio_config(
        mondrian_default_ocio_config_text(),
        MondrianStandardPackageIdentity::V3,
    )
    .expect("pinned Standard config");
    let processor = config
        .processor_display(
            ocio_working_color_space_name(WorkingColorSpace::LinearRec2020),
            "Rec.1886 Rec.709 - Display",
            MONDRIAN_STANDARD_SDR_V2_VIEW_NAME,
            TransformDirection::Forward,
        )
        .expect("SDR processor");
    let display_cpu = processor.default_cpu_processor().expect("SDR CPU");
    let monitor_processor =
        ocio_processor_from_config(&config, ColorSpace::Rec709.into(), ColorSpace::Srgb.into())
            .expect("monitor processor");
    let monitor_cpu = monitor_processor.default_cpu_processor().expect("monitor CPU");
    let input = fixture(960 * 540);
    let mut observations = Vec::new();
    for (route, cpu) in [("program", &display_cpu), ("monitor", &monitor_cpu)] {
        let mut expected = input.clone();
        cpu.try_apply_rgb_pixels(&mut expected, (input.len() / 4) as i64, 4)
            .expect("bulk reference");
        let modes = [
            ("rgb", 960 * 540),
            ("rgb", 16384),
            ("rgb", 4096),
            ("rgb", 1024),
            ("isolated_rgb", 1024),
        ];
        for repeat in 0..10 {
            for offset in 0..modes.len() {
                let (layout, pixels) = modes[(repeat + offset) % modes.len()];
                let mut actual = input.clone();
                let started = std::time::Instant::now();
                if layout == "isolated_rgb" {
                    apply_cpu_processor_float(cpu, &mut actual);
                } else {
                    for chunk in actual.chunks_mut(pixels * 4) {
                        cpu.try_apply_rgb_pixels(chunk, (chunk.len() / 4) as i64, 4)
                            .expect("chunk execution");
                    }
                }
                let elapsed_us = started.elapsed().as_micros();
                assert_bits(&actual, &expected);
                observations.push(serde_json::json!({"route": route, "layout": layout, "repeat": repeat, "pixels_per_call": pixels, "elapsed_us": elapsed_us}));
            }
        }
    }
    eprintln!(
        "MONDRIAN_OCIO_CHUNK_PROBE={}",
        serde_json::json!({"diagnostic_only": true, "qualifying": false, "observations": observations})
    );
}
