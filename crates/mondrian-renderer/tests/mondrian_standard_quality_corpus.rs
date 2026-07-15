use mondrian_core::{
    ensure_mondrian_default_ocio_loaded, mondrian_standard_output_display_view, ColorEngine,
    ColorSpace, MondrianStandardPackageIdentity, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_renderer::{
    execute_cpu_output_boundary_float, CpuColorFrame, RenderOutputColorBoundary,
};
use std::collections::BTreeSet;

#[path = "support/quality_corpus.rs"]
mod quality_corpus;

use quality_corpus::{QualityCase, QualityCorpus};

const NORMALIZED_SIGNAL_EPSILON: f32 = 1.0 / 4_095.0;

const CORPUS_JSON: &str =
    include_str!("../../../tests/fixtures/color/metadata/mondrian-standard-quality-corpus-v1.json");

#[test]
fn quality_corpus_contract_is_complete_independently_sourced_and_package_pinned() {
    let corpus = parse_corpus();
    assert_eq!(corpus.schema_version, 1);
    assert_eq!(corpus.corpus_id, "mondrian-standard-quality-v1");
    assert_eq!(
        corpus.package_sha256,
        MondrianStandardPackageIdentity::V1.package_sha256()
    );

    let required = corpus.required_categories.iter().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(corpus.category_set(), required);
    assert_eq!(required.len(), 22);

    let mut case_ids = BTreeSet::new();
    for case in &corpus.cases {
        assert!(
            case_ids.insert(case.id.as_str()),
            "duplicate case id {}",
            case.id
        );
        assert!(
            !case.categories.is_empty(),
            "{} has no quality category",
            case.id
        );
        assert!(
            !corpus.pixels_for(case).is_empty(),
            "{} has no generated samples",
            case.id
        );
    }

    let color_checker = corpus
        .source_references
        .iter()
        .find(|reference| reference.id == "colour-science-colorchecker-2005-xyy")
        .expect("ColorChecker reference");
    assert!(color_checker.source_uri.starts_with("https://"));
    assert_eq!(color_checker.producer, "Colour Developers");
    assert_eq!(color_checker.producer_version, "0.4.7");
    assert_eq!(
        color_checker.coordinate_space,
        "CIE xyY; ColorChecker 2005; D50"
    );
    assert_eq!(color_checker.license, "BSD-3-Clause");
    assert!(color_checker.license_uri.starts_with("https://"));
    assert!(color_checker.copyright_notice.contains("Colour Developers"));
    assert_eq!(color_checker.patches.len(), 24);
    let patch_ids = color_checker
        .patches
        .iter()
        .map(|patch| patch.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(patch_ids.len(), 24);
    assert!(color_checker
        .patches
        .iter()
        .all(|patch| patch.coordinates.iter().all(|value| value.is_finite())));

    let color_checker_case = corpus
        .cases
        .iter()
        .find(|case| case.id == "color-checker-2005")
        .expect("ColorChecker stimulus");
    assert_eq!(corpus.pixels_for(color_checker_case).len(), 24);

    let ten_bit = corpus
        .cases
        .iter()
        .find(|case| case.id == "ten-bit-neutral-gradient")
        .expect("10-bit neutral ramp");
    let ten_bit_pixels = corpus.pixels_for(ten_bit);
    assert_eq!(ten_bit_pixels.len(), 1_024);
    assert_eq!(ten_bit_pixels.first(), Some(&[0.0, 0.0, 0.0, 1.0]));
    assert_eq!(ten_bit_pixels.last(), Some(&[1.0, 1.0, 1.0, 1.0]));

    let range = corpus
        .cases
        .iter()
        .find(|case| case.id == "ten-bit-video-range-contract")
        .expect("10-bit legal/full range contract");
    assert!(!range.render_through_standard);
    assert_eq!(
        corpus.pixels_for(range),
        vec![
            [64.0 / 1_023.0, 64.0 / 1_023.0, 64.0 / 1_023.0, 1.0],
            [940.0 / 1_023.0, 940.0 / 1_023.0, 940.0 / 1_023.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
        ]
    );
}

#[test]
fn production_standard_sdr_and_pq_views_satisfy_objective_corpus_invariants() {
    ensure_mondrian_default_ocio_loaded().expect("Mondrian Standard OCIO package");
    let corpus = parse_corpus();

    for case in corpus.cases.iter().filter(|case| case.render_through_standard) {
        let input = corpus.pixels_for(case);
        for output in [ColorSpace::Srgb, ColorSpace::Rec2100Pq] {
            let rendered = render_standard(&input, output);
            assert_eq!(rendered.len(), input.len(), "{} {output:?}", case.id);
            for (pixel_index, (source, result)) in input.iter().zip(&rendered).enumerate() {
                assert!(
                    result.iter().all(|value| value.is_finite()),
                    "{} {output:?} produced non-finite pixel {pixel_index}: {result:?}",
                    case.id
                );
                assert!(
                    result[..3].iter().all(|value| {
                        (-NORMALIZED_SIGNAL_EPSILON..=1.0 + NORMALIZED_SIGNAL_EPSILON)
                            .contains(value)
                    }),
                    "{} {output:?} produced out-of-domain pixel {pixel_index}: {result:?}",
                    case.id
                );
                assert!(
                    (result[3] - source[3]).abs() <= 1.0e-6,
                    "{} {output:?} changed alpha at pixel {pixel_index}: {} -> {}",
                    case.id,
                    source[3],
                    result[3]
                );
            }
        }
    }

    let neutral = corpus
        .cases
        .iter()
        .find(|case| case.id == "neutral-stop-ramp")
        .expect("neutral stop ramp");
    assert_neutral_monotonic(&corpus, neutral, ColorSpace::Srgb, 2.0e-4);
    assert_neutral_monotonic(&corpus, neutral, ColorSpace::Rec2100Pq, 5.0e-4);

    let hue_boundary = corpus
        .cases
        .iter()
        .find(|case| case.id == "high-saturation-hue-boundary-sweep")
        .expect("high-saturation hue boundary sweep");
    assert_hue_boundary_continuity(&corpus, hue_boundary, ColorSpace::Srgb);
    assert_hue_boundary_continuity(&corpus, hue_boundary, ColorSpace::Rec2100Pq);

    let negative_boundary = corpus
        .cases
        .iter()
        .find(|case| case.id == "negative-channel-zero-boundary-line")
        .expect("negative-channel boundary line");
    assert_local_continuity(&corpus, negative_boundary, ColorSpace::Srgb, 0.005);
    assert_local_continuity(&corpus, negative_boundary, ColorSpace::Rec2100Pq, 0.005);
}

fn parse_corpus() -> QualityCorpus {
    serde_json::from_str(CORPUS_JSON).expect("strict Mondrian Standard quality corpus")
}

fn render_standard(input: &[[f32; 4]], output: ColorSpace) -> Vec<[f32; 4]> {
    let width = u32::try_from(input.len()).expect("quality corpus width");
    let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
        width,
        height: 1,
        data: input.to_vec(),
        color_space: WorkingColorSpace::LinearRec2020,
    });
    let (display, view) =
        mondrian_standard_output_display_view(output).expect("Standard output display/view");
    let boundary = RenderOutputColorBoundary::display_view(
        output,
        display,
        view,
        false,
        ColorEngine::mondrian_standard(),
    );
    execute_cpu_output_boundary_float(&frame, &boundary)
        .expect("production CPU OCIO output boundary")
        .frame
        .rgba_f32()
        .data
        .clone()
}

fn assert_neutral_monotonic(
    corpus: &QualityCorpus,
    case: &QualityCase,
    output: ColorSpace,
    spread_limit: f32,
) {
    let rendered = render_standard(&corpus.pixels_for(case), output);
    let mut previous = f32::NEG_INFINITY;
    for (pixel_index, pixel) in rendered.iter().enumerate() {
        let minimum = pixel[..3].iter().copied().fold(f32::INFINITY, f32::min);
        let maximum = pixel[..3].iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            maximum - minimum <= spread_limit,
            "{output:?} neutral spread at {pixel_index}: {pixel:?}"
        );
        assert!(
            pixel[1] + 1.0e-6 >= previous,
            "{output:?} tone reversal at {pixel_index}: {previous} -> {}",
            pixel[1]
        );
        previous = pixel[1];
    }
}

fn assert_hue_boundary_continuity(corpus: &QualityCorpus, case: &QualityCase, output: ColorSpace) {
    let source = corpus.pixels_for(case);
    let rendered = render_standard(&source, output);
    let mut max_adjacent_delta = 0.0_f32;
    let mut worst_adjacent_index = 0;
    let mut max_hue_delta_degrees = 0.0_f32;
    let mut worst_hue_index = 0;
    for index in 0..source.len() {
        let next = (index + 1) % source.len();
        let adjacent_delta = rendered[index][..3]
            .iter()
            .zip(&rendered[next][..3])
            .map(|(left, right)| (left - right).powi(2))
            .sum::<f32>()
            .sqrt();
        if adjacent_delta > max_adjacent_delta {
            max_adjacent_delta = adjacent_delta;
            worst_adjacent_index = index;
        }

        let source_hue = opponent_hue(source[index]);
        let rendered_hue = opponent_hue(rendered[index]);
        let hue_delta = wrapped_angle_delta(source_hue, rendered_hue).to_degrees();
        if hue_delta > max_hue_delta_degrees {
            max_hue_delta_degrees = hue_delta;
            worst_hue_index = index;
        }
    }
    eprintln!(
        "{output:?} hue boundary: max_adjacent_delta={max_adjacent_delta:.6} at {worst_adjacent_index}, source={:?}->{:?}, output={:?}->{:?}; max_hue_delta_degrees={max_hue_delta_degrees:.3} at {worst_hue_index}, source={:?}, output={:?}",
        source[worst_adjacent_index],
        source[(worst_adjacent_index + 1) % source.len()],
        rendered[worst_adjacent_index],
        rendered[(worst_adjacent_index + 1) % rendered.len()],
        source[worst_hue_index],
        rendered[worst_hue_index]
    );
    assert!(
        max_adjacent_delta <= 0.025,
        "{output:?} gamut boundary discontinuity: {max_adjacent_delta}"
    );
    assert!(
        max_hue_delta_degrees <= 30.0,
        "{output:?} severe hue rotation: {max_hue_delta_degrees} degrees"
    );
}

fn assert_local_continuity(
    corpus: &QualityCorpus,
    case: &QualityCase,
    output: ColorSpace,
    limit: f32,
) {
    let rendered = render_standard(&corpus.pixels_for(case), output);
    let max_adjacent_delta = rendered
        .windows(2)
        .map(|pair| {
            pair[0][..3]
                .iter()
                .zip(&pair[1][..3])
                .map(|(left, right)| (left - right).powi(2))
                .sum::<f32>()
                .sqrt()
        })
        .fold(0.0_f32, f32::max);
    eprintln!(
        "{} {output:?} local continuity: max_adjacent_delta={max_adjacent_delta:.8}",
        case.id
    );
    assert!(
        max_adjacent_delta <= limit,
        "{} {output:?} local discontinuity: {max_adjacent_delta} > {limit}",
        case.id
    );
}

fn opponent_hue(pixel: [f32; 4]) -> f32 {
    let x = 2.0 * pixel[0] - pixel[1] - pixel[2];
    let y = 3.0_f32.sqrt() * (pixel[1] - pixel[2]);
    y.atan2(x)
}

fn wrapped_angle_delta(first: f32, second: f32) -> f32 {
    let mut delta = (first - second).abs() % std::f32::consts::TAU;
    if delta > std::f32::consts::PI {
        delta = std::f32::consts::TAU - delta;
    }
    delta
}
