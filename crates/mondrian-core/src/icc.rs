//! ICC profile parsing helpers used by display profile import.

use crate::types::ColorSpace;
use icc_profile::iccprofile::{
    Curve, Data as IccTagData, DecodedICCProfile, ICCNumber, S15Fixed16Number,
};

#[derive(Debug, Clone)]
pub struct ParsedIccDisplayProfile {
    pub name: String,
    pub color_space: ColorSpace,
    pub linear_matrix: [[f32; 3]; 3],
    pub gamma_compensation: f32,
}

pub fn parse_icc_display_profile(bytes: &[u8]) -> Result<ParsedIccDisplayProfile, String> {
    let decoded = DecodedICCProfile::new(&bytes.to_vec())
        .map_err(|err| format!("failed to parse ICC profile: {err}"))?;

    let name = icc_profile_name(&decoded).unwrap_or_else(|| "ICC Profile".to_string());
    let color_space = infer_color_space_from_icc(&decoded, &name);

    let linear_matrix = icc_rgb_correction_matrix(&decoded, color_space).unwrap_or(IDENTITY_3);
    let gamma_compensation = estimate_gamma_compensation(&decoded, color_space).unwrap_or(1.0);

    Ok(ParsedIccDisplayProfile {
        name,
        color_space,
        linear_matrix,
        gamma_compensation,
    })
}

fn icc_profile_name(decoded: &DecodedICCProfile) -> Option<String> {
    for tag in ["desc", "mluc"] {
        if let Some(value) = decoded.tags.get(tag) {
            let text = match value {
                IccTagData::Descriptor(descriptor) => {
                    if descriptor.local_string.trim().is_empty() {
                        descriptor.ascii_string.clone()
                    } else {
                        descriptor.local_string.clone()
                    }
                }
                IccTagData::ASCII(text) => text.clone(),
                IccTagData::MultiLocalizedUnicode(text) => text.as_string(),
                _ => continue,
            };
            let normalized = text.trim();
            if !normalized.is_empty() {
                return Some(normalized.to_string());
            }
        }
    }
    None
}

fn infer_color_space_from_icc(decoded: &DecodedICCProfile, profile_name: &str) -> ColorSpace {
    let mut hints = profile_name.to_ascii_lowercase();
    hints.push(' ');
    hints.push_str(&icc_fourcc(decoded.color_space).to_ascii_lowercase());

    if hints.contains("display p3") || hints.contains("dci-p3") || hints.contains(" p3") {
        return ColorSpace::DciP3;
    }
    if hints.contains("rec2020") || hints.contains("bt2020") || hints.contains("2020") {
        return ColorSpace::Rec2020;
    }
    if hints.contains("srgb") {
        return ColorSpace::Srgb;
    }
    if hints.contains("pq") || hints.contains("smpte2084") || hints.contains("hdr10") {
        return ColorSpace::Rec2100Pq;
    }
    if hints.contains("hlg") || hints.contains("std-b67") {
        return ColorSpace::Rec2100Hlg;
    }
    if hints.contains("s-log3") || hints.contains("slog3") {
        return ColorSpace::SLog3;
    }
    if hints.contains("logc4") || hints.contains("arri") {
        return ColorSpace::ArriLogC4;
    }
    if hints.contains("apple log") || hints.contains("applelog") {
        return ColorSpace::AppleLog;
    }

    match icc_fourcc(decoded.color_space).as_str() {
        "RGB" | "RGB " | "GRAY" | "GREY" => ColorSpace::Rec709,
        _ => ColorSpace::Rec709,
    }
}

fn icc_fourcc(value: u32) -> String {
    let bytes = value.to_be_bytes();
    let text: String = bytes
        .iter()
        .map(|b| {
            if b.is_ascii_graphic() || *b == b' ' {
                char::from(*b)
            } else {
                '?'
            }
        })
        .collect();
    text.trim().to_string()
}

fn icc_rgb_correction_matrix(
    decoded: &DecodedICCProfile,
    color_space: ColorSpace,
) -> Option<[[f32; 3]; 3]> {
    let icc_rgb_to_xyz_d50 = icc_rgb_to_xyz_d50_matrix(decoded)?;
    let icc_rgb_to_xyz_d65 = mul3x3(BRADFORD_D50_TO_D65, icc_rgb_to_xyz_d50);
    let reference = reference_rgb_to_xyz_d65(color_space);
    let reference_inv = inv3(reference)?;

    let correction = mul3x3(reference_inv, icc_rgb_to_xyz_d65);
    if !is_finite_mat3(correction) {
        return None;
    }

    Some(to_f32_mat3(correction))
}

fn icc_rgb_to_xyz_d50_matrix(decoded: &DecodedICCProfile) -> Option<[[f64; 3]; 3]> {
    if let Some(rgb_xyz) = icc_matrix_from_rgb_xyz_tags(decoded) {
        return Some(rgb_xyz);
    }

    if let Some(a2b) = icc_matrix_from_a2b_tag(decoded) {
        return Some(a2b);
    }

    if let Some(b2a) = icc_matrix_from_b2a_tag(decoded) {
        return inv3(b2a);
    }

    None
}

fn icc_matrix_from_rgb_xyz_tags(decoded: &DecodedICCProfile) -> Option<[[f64; 3]; 3]> {
    let r = icc_xyz_tag(decoded, "rXYZ")?;
    let g = icc_xyz_tag(decoded, "gXYZ")?;
    let b = icc_xyz_tag(decoded, "bXYZ")?;
    Some([[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]])
}

fn icc_matrix_from_a2b_tag(decoded: &DecodedICCProfile) -> Option<[[f64; 3]; 3]> {
    for tag in ["A2B0", "A2B1", "A2B2"] {
        let value = decoded.tags.get(tag)?;
        if let IccTagData::LutAtoB(mab) = value {
            let matrix = matrix3_from_s15(&mab.matrix)?;
            if is_finite_mat3(matrix) {
                return Some(matrix);
            }
        }
    }
    None
}

fn icc_matrix_from_b2a_tag(decoded: &DecodedICCProfile) -> Option<[[f64; 3]; 3]> {
    for tag in ["B2A0", "B2A1", "B2A2"] {
        let value = decoded.tags.get(tag)?;
        if let IccTagData::LutBtoA(mba) = value {
            let matrix = matrix3_from_s15(&mba.matrix)?;
            if is_finite_mat3(matrix) {
                return Some(matrix);
            }
        }
    }
    None
}

fn matrix3_from_s15(values: &[S15Fixed16Number]) -> Option<[[f64; 3]; 3]> {
    if values.len() < 9 {
        return None;
    }
    let matrix = [
        [values[0].as_f64(), values[1].as_f64(), values[2].as_f64()],
        [values[3].as_f64(), values[4].as_f64(), values[5].as_f64()],
        [values[6].as_f64(), values[7].as_f64(), values[8].as_f64()],
    ];
    Some(matrix)
}

fn estimate_gamma_compensation(
    decoded: &DecodedICCProfile,
    color_space: ColorSpace,
) -> Option<f32> {
    let mut estimates = Vec::new();
    for tag in ["rTRC", "gTRC", "bTRC"] {
        if let Some(tag_data) = decoded.tags.get(tag) {
            if let Some(gamma) = estimate_gamma_from_tag(tag_data) {
                estimates.push(gamma as f64);
            }
        }
    }

    if estimates.is_empty() {
        estimates.extend(estimate_gamma_from_lut_tags(decoded));
    }

    if estimates.is_empty() {
        return None;
    }

    estimates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let estimated = estimates[estimates.len() / 2];
    let reference = reference_gamma(color_space);
    let compensation = (reference / estimated).clamp(0.75, 1.25);
    Some(compensation as f32)
}

fn estimate_gamma_from_lut_tags(decoded: &DecodedICCProfile) -> Vec<f64> {
    let mut estimates = Vec::new();

    for tag in ["A2B0", "A2B1", "A2B2"] {
        if let Some(IccTagData::LutAtoB(mab)) = decoded.tags.get(tag) {
            for curve in mab.a_curves.iter().chain(mab.m_curves.iter()).chain(mab.b_curves.iter()) {
                if let Some(gamma) = estimate_gamma_from_curve_descriptor(curve) {
                    estimates.push(gamma as f64);
                }
            }
        }
    }

    for tag in ["B2A0", "B2A1", "B2A2"] {
        if let Some(IccTagData::LutBtoA(mba)) = decoded.tags.get(tag) {
            for curve in mba.a_curves.iter().chain(mba.m_curves.iter()).chain(mba.b_curves.iter()) {
                if let Some(gamma) = estimate_gamma_from_curve_descriptor(curve) {
                    estimates.push(gamma as f64);
                }
            }
        }
    }

    estimates
}

fn estimate_gamma_from_curve_descriptor(curve: &Curve) -> Option<f32> {
    match curve {
        Curve::ParametricCurve(param) => {
            let gamma = param.vals.first()?.as_f64();
            normalize_gamma(gamma)
        }
        Curve::Curve(values) => estimate_gamma_from_curve_samples(values),
    }
}

fn estimate_gamma_from_tag(data: &IccTagData) -> Option<f32> {
    match data {
        IccTagData::ParametricCurve(curve) => {
            let gamma = curve.vals.first()?.as_f64();
            normalize_gamma(gamma)
        }
        IccTagData::Curve(curve) => estimate_gamma_from_curve_samples(curve),
        _ => None,
    }
}

fn estimate_gamma_from_curve_samples(curve: &[u16]) -> Option<f32> {
    if curve.len() < 8 {
        return None;
    }

    let xs = [0.25_f64, 0.5_f64, 0.75_f64];
    let mut samples = Vec::new();
    for x in xs {
        let idx = ((curve.len() - 1) as f64 * x).round() as usize;
        let y = curve.get(idx).copied()? as f64 / 65535.0;
        if !(0.0..1.0).contains(&y) {
            continue;
        }
        let g = (y.ln() / x.ln()).abs();
        if g.is_finite() {
            samples.push(g);
        }
    }

    if samples.is_empty() {
        return None;
    }
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    normalize_gamma(mean)
}

fn normalize_gamma(gamma: f64) -> Option<f32> {
    if !gamma.is_finite() {
        return None;
    }
    let normalized = if gamma < 1.0 { 1.0 / gamma } else { gamma };
    if !(1.0..=4.0).contains(&normalized) {
        return None;
    }
    Some(normalized as f32)
}

fn icc_xyz_tag(decoded: &DecodedICCProfile, tag: &str) -> Option<[f64; 3]> {
    let value = decoded.tags.get(tag)?;
    match value {
        IccTagData::XYZNumberArray(values) => {
            let xyz = values.first()?;
            Some([xyz.x.as_f64(), xyz.y.as_f64(), xyz.z.as_f64()])
        }
        IccTagData::XYZNumber(xyz) => Some([xyz.x.as_f64(), xyz.y.as_f64(), xyz.z.as_f64()]),
        _ => None,
    }
}

fn reference_gamma(color_space: ColorSpace) -> f64 {
    match color_space {
        ColorSpace::Rec2020 | ColorSpace::Rec2100Hlg | ColorSpace::Rec2100Pq => 2.4,
        _ => 2.2,
    }
}

fn reference_rgb_to_xyz_d65(color_space: ColorSpace) -> [[f64; 3]; 3] {
    match color_space {
        ColorSpace::Rec2020 | ColorSpace::Rec2100Hlg | ColorSpace::Rec2100Pq => [
            [0.6369580483, 0.1446169036, 0.1688809752],
            [0.2627002120, 0.6779980715, 0.0593017165],
            [0.0000000000, 0.0280726930, 1.0609850577],
        ],
        ColorSpace::DciP3 => [
            [0.4865709486, 0.2656676932, 0.1982172852],
            [0.2289745641, 0.6917385218, 0.0792869141],
            [0.0000000000, 0.0451133819, 1.0439443689],
        ],
        _ => [
            [0.4124564, 0.3575761, 0.1804375],
            [0.2126729, 0.7151522, 0.0721750],
            [0.0193339, 0.1191920, 0.9503041],
        ],
    }
}

fn mul3x3(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0_f64; 3]; 3];
    for row in 0..3 {
        for col in 0..3 {
            out[row][col] = a[row][0] * b[0][col] + a[row][1] * b[1][col] + a[row][2] * b[2][col];
        }
    }
    out
}

fn det3(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn inv3(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = det3(m);
    if det.abs() < 1.0e-12 || !det.is_finite() {
        return None;
    }

    let inv_det = 1.0 / det;
    let inv = [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ];
    if is_finite_mat3(inv) {
        Some(inv)
    } else {
        None
    }
}

fn to_f32_mat3(m: [[f64; 3]; 3]) -> [[f32; 3]; 3] {
    [
        [m[0][0] as f32, m[0][1] as f32, m[0][2] as f32],
        [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32],
        [m[2][0] as f32, m[2][1] as f32, m[2][2] as f32],
    ]
}

fn is_finite_mat3(m: [[f64; 3]; 3]) -> bool {
    m.iter().all(|row| row.iter().all(|v| v.is_finite()))
}

const IDENTITY_3: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

const BRADFORD_D50_TO_D65: [[f64; 3]; 3] = [
    [0.955576615033105, -0.0230393447160788, 0.0631636322498013],
    [-0.0282895442435549, 1.00994161737158, 0.0210076549961903],
    [0.0122981657172074, -0.0204830252324494, 1.32990982644976],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_matrix_roundtrip_is_stable() {
        let m = reference_rgb_to_xyz_d65(ColorSpace::Rec709);
        let inv = inv3(m).expect("invertible");
        let id = mul3x3(inv, m);
        assert!((id[0][0] - 1.0).abs() < 1.0e-6);
        assert!((id[1][1] - 1.0).abs() < 1.0e-6);
        assert!((id[2][2] - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn curve_gamma_estimation_returns_reasonable_value() {
        let mut curve = Vec::new();
        let n = 256;
        for i in 0..n {
            let x = i as f64 / (n - 1) as f64;
            let y = x.powf(2.2);
            curve.push((y * 65535.0).round().clamp(0.0, 65535.0) as u16);
        }
        let estimated = estimate_gamma_from_curve_samples(&curve).expect("gamma");
        assert!((estimated as f64 - 2.2).abs() < 0.15);
    }
}

/// Core-level display profile access status.
///
/// This reports whether `mondrian-core` itself can provide ICC profile access,
/// EDR information, or HDR display metadata. Real OS adapters live outside
/// core in `mondrian-platform`, so core never performs platform discovery
/// directly.
///
/// The structured status integrates with the Display Output Contract v2:
/// when `os_icc_discovery_available` is false and the user requests an
/// ICC profile, the contract must emit `MonitorProfileStatus::IccProfileUnsupported`
/// rather than silently falling back to Rec.709.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsDisplayProfileStatus {
    /// Whether ICC profile parsing is available (always true — the parser is
    /// built in).
    pub icc_parser_available: bool,
    /// Whether OS-level ICC profile discovery is available from core.
    ///
    /// This is always false; use `mondrian-platform` for OS-backed discovery.
    pub os_icc_discovery_available: bool,
    /// Whether EDR (Extended Dynamic Range) information is available from the OS.
    pub os_edr_available: bool,
    /// Whether HDR display metadata is available from the OS.
    pub os_hdr_metadata_available: bool,
    /// Human-readable status message.
    pub status_message: String,
}

impl OsDisplayProfileStatus {
    /// Check core display profile access capabilities.
    ///
    /// This always reports OS-level access as unavailable because core must not
    /// call platform APIs. Desktop integrations should use the platform display
    /// profile probe instead.
    pub fn check() -> Self {
        Self {
            icc_parser_available: true,
            os_icc_discovery_available: false,
            os_edr_available: false,
            os_hdr_metadata_available: false,
            status_message: "Core does not perform OS display profile discovery; \
             use the platform display profile probe."
                .to_string(),
        }
    }

    /// Whether the OS can provide ICC profile data for the current monitor.
    ///
    /// When this returns `false` and the user configures
    /// `MonitorProfileReference::IccProfile`, the display output contract
    /// **must** emit `MonitorProfileStatus::IccProfileUnsupported` — never
    /// silently fall back to Rec.709.
    pub fn can_discover_os_icc_profile(&self) -> bool {
        self.os_icc_discovery_available
    }

    /// Whether the OS can provide reliable HDR display metadata.
    ///
    /// When this returns `false` and the user requests HDR viewer mode,
    /// the display output contract **must** emit
    /// `HdrStatus::RequestedMonitorUnknown` — never claim HDR correctness.
    pub fn can_query_os_hdr_metadata(&self) -> bool {
        self.os_hdr_metadata_available
    }

    /// Whether the OS can provide EDR (Extended Dynamic Range) information.
    pub fn can_query_os_edr(&self) -> bool {
        self.os_edr_available
    }
}
