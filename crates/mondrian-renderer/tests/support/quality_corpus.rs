use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityCorpus {
    pub schema_version: u32,
    pub corpus_id: String,
    pub package_sha256: String,
    pub required_categories: Vec<String>,
    pub source_references: Vec<SourceReference>,
    pub cases: Vec<QualityCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReference {
    pub id: String,
    pub source_uri: String,
    pub producer: String,
    pub producer_version: String,
    pub coordinate_space: String,
    pub license: String,
    pub license_uri: String,
    pub copyright_notice: String,
    pub patches: Vec<ColorimetricPatch>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorimetricPatch {
    pub id: String,
    pub coordinates: [f32; 3],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityCase {
    pub id: String,
    pub categories: Vec<String>,
    pub render_through_standard: bool,
    pub stimulus: QualityStimulus,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QualityStimulus {
    NeutralStopRamp {
        middle_gray: f32,
        start_stop: i32,
        end_stop: i32,
        samples_per_stop: u32,
    },
    RgbaPatches {
        pixels: Vec<[f32; 4]>,
    },
    ColorimetricReference {
        source_reference_id: String,
    },
    CubeGrid {
        levels: Vec<f32>,
        alpha: f32,
    },
    HueSweep {
        center: f32,
        radius: f32,
        samples: u32,
    },
    RgbaLine {
        start: [f32; 4],
        end: [f32; 4],
        samples: u32,
    },
    AlphaRamp {
        rgb: [f32; 3],
        steps: u32,
    },
    SpatialImpulse {
        width: u32,
        background: [f32; 4],
        impulse: [f32; 4],
    },
    QuantizedRamp {
        bit_depth: u8,
    },
    VideoRangeCodes {
        bit_depth: u8,
        legal_min: u16,
        legal_max: u16,
        full_min: u16,
        full_max: u16,
    },
}

impl QualityCorpus {
    pub fn category_set(&self) -> BTreeSet<&str> {
        self.cases
            .iter()
            .flat_map(|case| case.categories.iter().map(String::as_str))
            .collect()
    }

    pub fn pixels_for(&self, case: &QualityCase) -> Vec<[f32; 4]> {
        if let QualityStimulus::ColorimetricReference { source_reference_id } = &case.stimulus {
            return self
                .source_references
                .iter()
                .find(|reference| reference.id == *source_reference_id)
                .unwrap_or_else(|| panic!("missing source reference {source_reference_id}"))
                .patches
                .iter()
                .map(|patch| xyy_d50_to_linear_rec2020(patch.coordinates))
                .collect();
        }
        case.pixels()
    }
}

impl QualityCase {
    pub fn pixels(&self) -> Vec<[f32; 4]> {
        match &self.stimulus {
            QualityStimulus::NeutralStopRamp {
                middle_gray,
                start_stop,
                end_stop,
                samples_per_stop,
            } => {
                let sample_count = (end_stop - start_stop) as u32 * samples_per_stop + 1;
                (0..sample_count)
                    .map(|index| {
                        let stop = *start_stop as f32 + index as f32 / *samples_per_stop as f32;
                        let value = *middle_gray * 2.0_f32.powf(stop);
                        [value, value, value, 1.0]
                    })
                    .collect()
            }
            QualityStimulus::RgbaPatches { pixels } => pixels.clone(),
            QualityStimulus::ColorimetricReference { source_reference_id } => {
                panic!("unresolved colorimetric reference {source_reference_id}")
            }
            QualityStimulus::CubeGrid { levels, alpha } => levels
                .iter()
                .flat_map(|red| {
                    levels.iter().flat_map(move |green| {
                        levels.iter().map(move |blue| [*red, *green, *blue, *alpha])
                    })
                })
                .collect(),
            QualityStimulus::HueSweep { center, radius, samples } => (0..*samples)
                .map(|sample| {
                    let angle = std::f32::consts::TAU * sample as f32 / *samples as f32;
                    let red = *center + *radius * angle.cos();
                    let green =
                        *center + *radius * (angle - 2.0 * std::f32::consts::PI / 3.0).cos();
                    let blue = *center + *radius * (angle + 2.0 * std::f32::consts::PI / 3.0).cos();
                    [red, green, blue, 1.0]
                })
                .collect(),
            QualityStimulus::RgbaLine { start, end, samples } => (0..*samples)
                .map(|sample| {
                    let amount = sample as f32 / samples.saturating_sub(1) as f32;
                    std::array::from_fn(|channel| {
                        start[channel] + (end[channel] - start[channel]) * amount
                    })
                })
                .collect(),
            QualityStimulus::AlphaRamp { rgb, steps } => (0..=*steps)
                .map(|step| {
                    let alpha = step as f32 / *steps as f32;
                    [rgb[0], rgb[1], rgb[2], alpha]
                })
                .collect(),
            QualityStimulus::SpatialImpulse { width, background, impulse } => {
                let mut pixels = vec![*background; *width as usize];
                pixels[*width as usize / 2] = *impulse;
                pixels
            }
            QualityStimulus::QuantizedRamp { bit_depth } => {
                let max_code = (1_u32 << *bit_depth) - 1;
                (0..=max_code)
                    .map(|code| {
                        let value = code as f32 / max_code as f32;
                        [value, value, value, 1.0]
                    })
                    .collect()
            }
            QualityStimulus::VideoRangeCodes {
                bit_depth,
                legal_min,
                legal_max,
                full_min,
                full_max,
            } => {
                let max_code = (1_u32 << *bit_depth) - 1;
                [*legal_min, *legal_max, *full_min, *full_max]
                    .into_iter()
                    .map(|code| {
                        let value = f32::from(code) / max_code as f32;
                        [value, value, value, 1.0]
                    })
                    .collect()
            }
        }
    }
}

fn xyy_d50_to_linear_rec2020(xyy: [f32; 3]) -> [f32; 4] {
    let [x, y, luminance] = xyy;
    let xyz_d50 = [x * luminance / y, luminance, (1.0 - x - y) * luminance / y];
    let xyz_d65 = multiply_matrix_vector(
        [
            [0.955_473_4, -0.023_098_5, 0.063_259_3],
            [-0.028_369_7, 1.009_995_5, 0.021_041_4],
            [0.012_314, -0.020_507_7, 1.330_365_9],
        ],
        xyz_d50,
    );
    let rgb = multiply_matrix_vector(
        [
            [1.716_651_2, -0.355_670_78, -0.253_366_3],
            [-0.666_684_3, 1.616_481_2, 0.015_768_546],
            [0.017_639_857, -0.042_770_613, 0.942_103_15],
        ],
        xyz_d65,
    );
    [rgb[0], rgb[1], rgb[2], 1.0]
}

fn multiply_matrix_vector(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    matrix.map(|row| row.iter().zip(vector).map(|(coefficient, value)| coefficient * value).sum())
}
