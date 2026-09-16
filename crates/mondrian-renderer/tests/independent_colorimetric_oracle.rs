//! Public-specification color oracles for absolute qualification.
//!
//! This test target deliberately does not import production OCIO processors or
//! Mondrian color-transform helpers. Fixed targets and the f64 equations below
//! form an independent reference boundary; renderer reports only measure the
//! observed buffers against those targets.

use mondrian_core::{bt2100_hlg_1000_nit_to_display_linear_rgb, bt2100_pq_to_display_linear_rgb};
use mondrian_renderer::{
    compare_code_values, compare_pq_hdr_display_rgba, compare_srgb_display_rgba8,
    CodeValueAccuracyBudget, PqHdrDisplayAccuracyBudget, SrgbDisplayAccuracyBudget,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const CORPUS_BYTES: &[u8] = include_bytes!(
    "../../../tests/fixtures/color/metadata/independent-colorimetric-oracle-v1.json"
);
const CORPUS_SHA256: &str = "f9b8f14c5648e52ec78ae1fab3e7814943b4c6ab71f6acd970bcaad7d3b1fd7f";

#[derive(Debug, Deserialize)]
struct OracleCorpus {
    schema_version: u32,
    corpus_id: String,
    origin: String,
    numeric_precision: String,
    sources: Vec<Source>,
    sdr_transfer_vectors: Vec<SdrTransferVector>,
    hdr_transfer_vectors: Vec<HdrTransferVector>,
    range_vectors: Vec<RangeVector>,
    alpha_vectors: Vec<AlphaVector>,
    chroma_siting_vectors: Vec<ChromaSitingVector>,
    delta_e_2000_vectors: Vec<DeltaE2000Vector>,
    delta_e_itp_vectors: Vec<DeltaEItpVector>,
}

#[derive(Debug, Deserialize)]
struct Source {
    id: String,
    revision: String,
    scope: String,
}

#[derive(Debug, Deserialize)]
struct SdrTransferVector {
    transfer: String,
    linear: f64,
    encoded: f64,
}

#[derive(Debug, Deserialize)]
struct HdrTransferVector {
    transfer: String,
    input: f64,
    target: f64,
    units: String,
}

#[derive(Debug, Deserialize)]
struct RangeVector {
    bit_depth: u8,
    range: String,
    luma_black: u16,
    luma_white: u16,
    chroma_neutral: u16,
}

#[derive(Debug, Deserialize)]
struct AlphaVector {
    bit_depth: u8,
    codes: Vec<u16>,
}

#[derive(Debug, Deserialize)]
struct ChromaSitingVector {
    subsampling: String,
    location: String,
    origin: [f64; 2],
    source_center: [f64; 2],
    sample_coordinate: [f64; 2],
}

#[derive(Debug, Deserialize)]
struct DeltaE2000Vector {
    reference_lab: [f64; 3],
    sample_lab: [f64; 3],
    target: f64,
}

#[derive(Debug, Deserialize)]
struct DeltaEItpVector {
    reference_itp: [f64; 3],
    sample_itp: [f64; 3],
    target: f64,
}

#[test]
fn oracle_corpus_is_digest_pinned_versioned_and_publicly_sourced() {
    let corpus = corpus();
    assert_eq!(corpus.schema_version, 1);
    assert_eq!(
        corpus.corpus_id,
        "mondrian-independent-colorimetric-oracle-v1"
    );
    assert_eq!(corpus.origin, "public_specification");
    assert!(corpus.numeric_precision.contains("binary64"));
    assert_eq!(format!("{:x}", Sha256::digest(CORPUS_BYTES)), CORPUS_SHA256);
    assert!(corpus.sources.len() >= 6);
    for source in &corpus.sources {
        assert!(!source.id.trim().is_empty());
        assert!(!source.revision.trim().is_empty());
        assert!(!source.scope.trim().is_empty());
    }
}

#[test]
fn independent_f64_transfer_oracles_match_fixed_sdr_and_hdr_targets() {
    let corpus = corpus();
    for vector in &corpus.sdr_transfer_vectors {
        let actual = match vector.transfer.as_str() {
            "srgb" => srgb_oetf(vector.linear),
            "bt709" => bt709_oetf(vector.linear),
            unexpected => panic!("unknown SDR transfer {unexpected}"),
        };
        assert_close(actual, vector.encoded, 2.0e-14, &vector.transfer);
    }
    for vector in &corpus.hdr_transfer_vectors {
        let actual = match vector.transfer.as_str() {
            "pq" => {
                assert_eq!(vector.units, "encoded_from_normalized_10000_nits");
                pq_oetf(vector.input)
            }
            "hlg_1000_nit" => {
                assert_eq!(vector.units, "display_nits_from_encoded_signal");
                hlg_1000_nit_display(vector.input)
            }
            unexpected => panic!("unknown HDR transfer {unexpected}"),
        };
        let tolerance = if vector.transfer == "hlg_1000_nit" {
            5.0e-10
        } else {
            2.0e-14
        };
        assert_close(actual, vector.target, tolerance, &vector.transfer);
    }
}

#[test]
fn independent_metric_oracles_match_sharma_and_bt2124_targets() {
    let corpus = corpus();
    for vector in &corpus.delta_e_2000_vectors {
        let actual = delta_e_2000(vector.reference_lab, vector.sample_lab);
        assert_close(actual, vector.target, 5.0e-5, "CIEDE2000");
    }
    for vector in &corpus.delta_e_itp_vectors {
        let actual = delta_e_itp(vector.reference_itp, vector.sample_itp);
        assert_close(actual, vector.target, 5.0e-4, "Delta E ITP");
    }
}

#[test]
fn production_hdr_color_science_meets_independent_absolute_luminance_targets() {
    let corpus = corpus();
    for vector in &corpus.hdr_transfer_vectors {
        match vector.transfer.as_str() {
            "pq" => {
                let decoded = bt2100_pq_to_display_linear_rgb([vector.target; 3])
                    .expect("production PQ EOTF")
                    .components_nits();
                let expected_nits = vector.input * 10_000.0;
                for component in decoded {
                    assert_close(
                        component,
                        expected_nits,
                        2.0e-6_f64.max(expected_nits * 2.0e-10),
                        "PQ nits",
                    );
                }
            }
            "hlg_1000_nit" => {
                let decoded = bt2100_hlg_1000_nit_to_display_linear_rgb([vector.input; 3])
                    .expect("production HLG inverse OETF/OOTF")
                    .components_nits();
                for component in decoded {
                    assert_close(component, vector.target, 5.0e-10, "HLG nits");
                }
            }
            unexpected => panic!("unknown HDR transfer {unexpected}"),
        }
    }
}

#[test]
fn code_value_gate_covers_ramps_ranges_and_alpha_at_8_10_12_16_bit() {
    let corpus = corpus();
    for range in &corpus.range_vectors {
        let maximum = if range.bit_depth == 16 {
            u16::MAX
        } else {
            (1_u16 << range.bit_depth) - 1
        };
        assert!(range.luma_black < range.luma_white);
        assert!(range.luma_white <= maximum);
        assert!(range.chroma_neutral <= maximum);
        match (range.bit_depth, range.range.as_str()) {
            (8, "limited") => assert_eq!((range.luma_black, range.luma_white), (16, 235)),
            (10, "limited") => assert_eq!((range.luma_black, range.luma_white), (64, 940)),
            (12, "limited") => assert_eq!((range.luma_black, range.luma_white), (256, 3760)),
            (_, "full") => assert_eq!((range.luma_black, range.luma_white), (0, maximum)),
            unexpected => panic!("unexpected range vector {unexpected:?}"),
        }
        let ramp = (range.luma_black..=range.luma_white).collect::<Vec<_>>();
        let report = compare_code_values(
            &ramp,
            &ramp,
            range.bit_depth,
            CodeValueAccuracyBudget::new(0, 0.0, 0),
        )
        .expect("fixed public-specification ramp");
        assert!(report.within_budget, "{range:?}: {report:#?}");
    }
    for alpha in &corpus.alpha_vectors {
        let report = compare_code_values(
            &alpha.codes,
            &alpha.codes,
            alpha.bit_depth,
            CodeValueAccuracyBudget::new(0, 0.0, 0),
        )
        .expect("fixed alpha code targets");
        assert!(report.within_budget, "{alpha:?}: {report:#?}");
        assert_eq!(
            alpha.codes[1], 1,
            "smallest positive alpha must remain represented"
        );
    }
}

#[test]
fn independent_chroma_siting_oracle_pins_420_and_422_sample_geometry() {
    let corpus = corpus();
    for vector in &corpus.chroma_siting_vectors {
        let vertical_scale = match vector.subsampling.as_str() {
            "420" => 0.5,
            "422" => 1.0,
            unexpected => panic!("unknown subsampling {unexpected}"),
        };
        if vector.subsampling == "422" {
            assert_eq!(vector.origin[1], 0.5, "4:2:2 chroma is vertically co-sited");
        }
        assert!(matches!(
            vector.location.as_str(),
            "left" | "center" | "top_left"
        ));
        let actual = [
            (vector.source_center[0] - vector.origin[0]) * 0.5,
            (vector.source_center[1] - vector.origin[1]) * vertical_scale,
        ];
        assert_close(actual[0], vector.sample_coordinate[0], 1.0e-15, "chroma x");
        assert_close(actual[1], vector.sample_coordinate[1], 1.0e-15, "chroma y");
    }
}

#[test]
fn renderer_perceptual_gates_consume_fixed_targets_and_reject_drift() {
    let corpus = corpus();
    let srgb = corpus
        .sdr_transfer_vectors
        .iter()
        .filter(|vector| vector.transfer == "srgb")
        .flat_map(|vector| {
            let code = (vector.encoded * 255.0).round() as u8;
            [code, code, code, u8::MAX]
        })
        .collect::<Vec<_>>();
    let sdr_exact = compare_srgb_display_rgba8(
        &srgb,
        &srgb,
        SrgbDisplayAccuracyBudget::new(0.0, 0.0, 0.0, 0),
    )
    .expect("fixed SDR targets");
    assert!(sdr_exact.within_budget);
    let mut sdr_drift = srgb.clone();
    sdr_drift[4] = sdr_drift[4].saturating_add(4);
    let sdr_rejected = compare_srgb_display_rgba8(
        &srgb,
        &sdr_drift,
        SrgbDisplayAccuracyBudget::new(0.25, 0.05, 0.25, 0),
    )
    .expect("finite SDR drift");
    assert!(!sdr_rejected.within_budget, "{sdr_rejected:#?}");

    let pq = corpus
        .hdr_transfer_vectors
        .iter()
        .filter(|vector| vector.transfer == "pq")
        .map(|vector| {
            let signal = vector.target as f32;
            [signal, signal, signal, 1.0]
        })
        .collect::<Vec<_>>();
    let hdr_exact = compare_pq_hdr_display_rgba(
        &pq,
        &pq,
        PqHdrDisplayAccuracyBudget::new(0.0, 0.0, 0.0, 0.0),
    )
    .expect("fixed HDR targets");
    assert!(hdr_exact.within_budget);
    let mut hdr_drift = pq.clone();
    hdr_drift[2][1] += 0.002;
    let hdr_rejected = compare_pq_hdr_display_rgba(
        &pq,
        &hdr_drift,
        PqHdrDisplayAccuracyBudget::new(0.5, 0.1, 0.5, 0.0),
    )
    .expect("finite HDR drift");
    assert!(!hdr_rejected.within_budget, "{hdr_rejected:#?}");
}

fn corpus() -> OracleCorpus {
    serde_json::from_slice(CORPUS_BYTES).expect("strict independent colorimetric corpus")
}

fn srgb_oetf(linear: f64) -> f64 {
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn bt709_oetf(linear: f64) -> f64 {
    if linear < 0.018 {
        4.5 * linear
    } else {
        1.099 * linear.powf(0.45) - 0.099
    }
}

fn pq_oetf(normalized_luminance: f64) -> f64 {
    const M1: f64 = 2610.0 / 16_384.0;
    const M2: f64 = 2523.0 / 32.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 128.0;
    const C3: f64 = 2392.0 / 128.0;
    if normalized_luminance == 0.0 {
        return 0.0;
    }
    let power = normalized_luminance.powf(M1);
    ((C1 + C2 * power) / (1.0 + C3 * power)).powf(M2)
}

fn hlg_1000_nit_display(encoded: f64) -> f64 {
    const A: f64 = 0.178_832_77;
    const B: f64 = 0.284_668_92;
    const C: f64 = 0.559_910_73;
    let scene_linear = if encoded <= 0.5 {
        encoded * encoded / 3.0
    } else {
        ((encoded - C) / A).exp().mul_add(1.0, B) / 12.0
    };
    1000.0 * scene_linear.powf(1.2)
}

fn delta_e_itp(reference: [f64; 3], sample: [f64; 3]) -> f64 {
    let di = reference[0] - sample[0];
    let dt = reference[1] - sample[1];
    let dp = reference[2] - sample[2];
    // Corpus coordinates are ITP, where T already equals 0.5 * Ct.
    720.0 * (di * di + dt * dt + dp * dp).sqrt()
}

// Independent CIEDE2000 transcription of Sharma, Wu, and Dalal (2005), kL=kC=kH=1.
fn delta_e_2000(first: [f64; 3], second: [f64; 3]) -> f64 {
    let (l1, a1, b1) = (first[0], first[1], first[2]);
    let (l2, a2, b2) = (second[0], second[1], second[2]);
    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let mean_c = (c1 + c2) * 0.5;
    let mean_c7 = mean_c.powi(7);
    let g = 0.5 * (1.0 - (mean_c7 / (mean_c7 + 25_f64.powi(7))).sqrt());
    let ap1 = (1.0 + g) * a1;
    let ap2 = (1.0 + g) * a2;
    let cp1 = ap1.hypot(b1);
    let cp2 = ap2.hypot(b2);
    let hp1 = hue_degrees(ap1, b1);
    let hp2 = hue_degrees(ap2, b2);
    let dl = l2 - l1;
    let dc = cp2 - cp1;
    let dh_degrees = if cp1 * cp2 == 0.0 {
        0.0
    } else if (hp2 - hp1).abs() <= 180.0 {
        hp2 - hp1
    } else if hp2 <= hp1 {
        hp2 - hp1 + 360.0
    } else {
        hp2 - hp1 - 360.0
    };
    let dh = 2.0 * (cp1 * cp2).sqrt() * (0.5 * dh_degrees.to_radians()).sin();
    let mean_l = (l1 + l2) * 0.5;
    let mean_cp = (cp1 + cp2) * 0.5;
    let mean_hp = if cp1 * cp2 == 0.0 {
        hp1 + hp2
    } else if (hp1 - hp2).abs() <= 180.0 {
        (hp1 + hp2) * 0.5
    } else if hp1 + hp2 < 360.0 {
        (hp1 + hp2 + 360.0) * 0.5
    } else {
        (hp1 + hp2 - 360.0) * 0.5
    };
    let t = 1.0 - 0.17 * (mean_hp - 30.0).to_radians().cos()
        + 0.24 * (2.0 * mean_hp).to_radians().cos()
        + 0.32 * (3.0 * mean_hp + 6.0).to_radians().cos()
        - 0.20 * (4.0 * mean_hp - 63.0).to_radians().cos();
    let sl = 1.0 + 0.015 * (mean_l - 50.0).powi(2) / (20.0 + (mean_l - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * mean_cp;
    let sh = 1.0 + 0.015 * mean_cp * t;
    let delta_theta = 30.0 * (-((mean_hp - 275.0) / 25.0).powi(2)).exp();
    let mean_cp7 = mean_cp.powi(7);
    let rc = 2.0 * (mean_cp7 / (mean_cp7 + 25_f64.powi(7))).sqrt();
    let rt = -rc * (2.0 * delta_theta).to_radians().sin();
    let l_term = dl / sl;
    let c_term = dc / sc;
    let h_term = dh / sh;
    (l_term * l_term + c_term * c_term + h_term * h_term + rt * c_term * h_term).sqrt()
}

fn hue_degrees(a: f64, b: f64) -> f64 {
    if a == 0.0 && b == 0.0 {
        0.0
    } else {
        b.atan2(a).to_degrees().rem_euclid(360.0)
    }
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, label: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{label}: expected {expected:.16}, observed {actual:.16}, tolerance {tolerance}"
    );
}
