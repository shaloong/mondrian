//! Color management primitives shared by preview, render and export.

use crate::icc::parse_icc_display_profile;
use crate::types::{ColorManagementBackend, ColorSpace};
use moxcms::{
    CicpColorPrimaries, CicpProfile, ColorProfile as CmsColorProfile, Layout as CmsLayout,
    MatrixCoefficients, TransferCharacteristics, TransformOptions,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorPipeline {
    pub input: ColorSpace,
    pub working: ColorSpace,
    pub output: ColorSpace,
    pub tone_map: bool,
    pub backend: ColorManagementBackend,
}

impl ColorPipeline {
    pub const fn new(
        input: ColorSpace,
        working: ColorSpace,
        output: ColorSpace,
        tone_map: bool,
    ) -> Self {
        Self {
            input,
            working,
            output,
            tone_map,
            backend: ColorManagementBackend::MondrianSmart,
        }
    }

    pub const fn with_backend(mut self, backend: ColorManagementBackend) -> Self {
        self.backend = backend;
        self
    }

    pub fn is_noop(self) -> bool {
        self.input == self.output
            && self.working == self.output
            && !(self.tone_map && self.input.is_hdr() && !self.output.is_hdr())
    }

    pub fn transform_plan(self) -> ColorTransformPlan {
        ColorTransformPlan::from_pipeline(self)
    }

    pub fn signature_hash(self) -> u64 {
        self.transform_plan().signature_hash()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorLut3D {
    pub name: String,
    pub size: u32,
    pub data: Vec<[f32; 3]>,
}

impl ColorLut3D {
    pub fn identity(size: u32) -> Result<Self, String> {
        if !(2..=129).contains(&size) {
            return Err(format!("unsupported 3D LUT size: {size}"));
        }
        let mut data = Vec::with_capacity((size * size * size) as usize);
        let denom = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 / denom, g as f32 / denom, b as f32 / denom]);
                }
            }
        }
        Ok(Self { name: format!("identity-{size}"), size, data })
    }

    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.size < 2 || self.data.is_empty() {
            return rgb;
        }

        let max = (self.size - 1) as f32;
        let r = rgb[0].clamp(0.0, 1.0) * max;
        let g = rgb[1].clamp(0.0, 1.0) * max;
        let b = rgb[2].clamp(0.0, 1.0) * max;
        let r0 = r.floor() as u32;
        let g0 = g.floor() as u32;
        let b0 = b.floor() as u32;
        let r1 = (r0 + 1).min(self.size - 1);
        let g1 = (g0 + 1).min(self.size - 1);
        let b1 = (b0 + 1).min(self.size - 1);
        let fr = r - r0 as f32;
        let fg = g - g0 as f32;
        let fb = b - b0 as f32;

        let c000 = self.at(r0, g0, b0);
        let c100 = self.at(r1, g0, b0);
        let c010 = self.at(r0, g1, b0);
        let c110 = self.at(r1, g1, b0);
        let c001 = self.at(r0, g0, b1);
        let c101 = self.at(r1, g0, b1);
        let c011 = self.at(r0, g1, b1);
        let c111 = self.at(r1, g1, b1);

        lerp3(
            lerp3(lerp3(c000, c100, fr), lerp3(c010, c110, fr), fg),
            lerp3(lerp3(c001, c101, fr), lerp3(c011, c111, fr), fg),
            fb,
        )
    }

    pub fn apply_rgba8_in_place(&self, rgba: &mut [u8], intensity: f32) {
        let intensity = intensity.clamp(0.0, 1.0);
        if intensity <= 1.0e-4 {
            return;
        }
        for px in rgba.chunks_exact_mut(4) {
            let src = [
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ];
            let graded = self.sample(src);
            let out = lerp3(src, graded, intensity);
            px[0] = (out[0].clamp(0.0, 1.0) * 255.0).round() as u8;
            px[1] = (out[1].clamp(0.0, 1.0) * 255.0).round() as u8;
            px[2] = (out[2].clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }

    pub fn signature_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.name.hash(&mut hasher);
        self.size.hash(&mut hasher);
        for rgb in &self.data {
            rgb[0].to_bits().hash(&mut hasher);
            rgb[1].to_bits().hash(&mut hasher);
            rgb[2].to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    fn at(&self, r: u32, g: u32, b: u32) -> [f32; 3] {
        let idx = (b * self.size * self.size + g * self.size + r) as usize;
        self.data.get(idx).copied().unwrap_or([0.0, 0.0, 0.0])
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColorTransformNode {
    ManagementBackend(ColorManagementBackend),
    DecodeTransfer(ColorSpace),
    ConvertPrimaries { from: ColorSpace, to: ColorSpace },
    ToneMapAces,
    Lut3D { lut: ColorLut3D, intensity: f32 },
    DisplayProfile(DisplayColorProfile),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorTransformPlan {
    pub source: ColorSpace,
    pub working: ColorSpace,
    pub output: ColorSpace,
    pub tone_map: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<ColorTransformNode>,
}

impl ColorTransformPlan {
    pub fn from_pipeline(pipeline: ColorPipeline) -> Self {
        let mut nodes = vec![
            ColorTransformNode::ManagementBackend(pipeline.backend),
            ColorTransformNode::DecodeTransfer(pipeline.input),
            ColorTransformNode::ConvertPrimaries { from: pipeline.input, to: pipeline.working },
        ];
        if pipeline.tone_map && pipeline.working.is_hdr() && !pipeline.output.is_hdr() {
            nodes.push(ColorTransformNode::ToneMapAces);
        }
        nodes.push(ColorTransformNode::ConvertPrimaries {
            from: pipeline.working,
            to: pipeline.output,
        });

        Self {
            source: pipeline.input,
            working: pipeline.working,
            output: pipeline.output,
            tone_map: pipeline.tone_map,
            nodes,
        }
    }

    pub fn with_display_profile(mut self, profile: DisplayColorProfile) -> Self {
        self.nodes.push(ColorTransformNode::DisplayProfile(profile));
        self
    }

    pub fn with_lut(mut self, lut: ColorLut3D, intensity: f32) -> Self {
        self.nodes.push(ColorTransformNode::Lut3D { lut, intensity });
        self
    }

    pub fn nodes(&self) -> &[ColorTransformNode] {
        &self.nodes
    }

    pub fn signature_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.source.hash(&mut hasher);
        self.working.hash(&mut hasher);
        self.output.hash(&mut hasher);
        self.tone_map.hash(&mut hasher);
        for node in &self.nodes {
            node.hash_signature(&mut hasher);
        }
        hasher.finish()
    }

    pub fn apply_rgba8_in_place(&self, data: &mut [u8]) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }

        let mut frame = RgbaF32Frame::from_rgba8(
            1,
            (data.len() / 4) as u32,
            data,
            self.source,
            self.working,
            false,
        );

        for node in &self.nodes {
            match node {
                ColorTransformNode::ManagementBackend(_) => {}
                ColorTransformNode::DecodeTransfer(_) => {}
                ColorTransformNode::ConvertPrimaries { from: _, to } => {
                    frame.convert_to(*to, false);
                }
                ColorTransformNode::ToneMapAces => {
                    if frame.color_space.is_hdr() && !self.output.is_hdr() {
                        for px in &mut frame.data {
                            px[0] = aces_tone_map(px[0]);
                            px[1] = aces_tone_map(px[1]);
                            px[2] = aces_tone_map(px[2]);
                        }
                    }
                }
                ColorTransformNode::Lut3D { lut, intensity } => {
                    let mut rgba = frame.to_rgba8(frame.color_space, false);
                    lut.apply_rgba8_in_place(&mut rgba, *intensity);
                    frame = RgbaF32Frame::from_rgba8(
                        1,
                        (rgba.len() / 4) as u32,
                        &rgba,
                        frame.color_space,
                        frame.color_space,
                        false,
                    );
                }
                ColorTransformNode::DisplayProfile(profile) => {
                    let mut rgba = frame.to_rgba8(frame.color_space, self.tone_map);
                    apply_display_profile_rgba8_in_place(
                        &mut rgba,
                        frame.color_space,
                        profile,
                        self.tone_map,
                    )?;
                    data.copy_from_slice(&rgba[..data.len()]);
                    return Ok(());
                }
            }
        }

        let converted = frame.to_rgba8(self.output, false);
        data.copy_from_slice(&converted[..data.len()]);
        Ok(())
    }
}

impl ColorTransformNode {
    pub fn hash_signature<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;

        match self {
            ColorTransformNode::ManagementBackend(backend) => {
                0u8.hash(state);
                backend.hash(state);
            }
            ColorTransformNode::DecodeTransfer(space) => {
                1u8.hash(state);
                space.hash(state);
            }
            ColorTransformNode::ConvertPrimaries { from, to } => {
                2u8.hash(state);
                from.hash(state);
                to.hash(state);
            }
            ColorTransformNode::ToneMapAces => {
                3u8.hash(state);
            }
            ColorTransformNode::Lut3D { lut, intensity } => {
                4u8.hash(state);
                lut.signature_hash().hash(state);
                intensity.to_bits().hash(state);
            }
            ColorTransformNode::DisplayProfile(profile) => {
                5u8.hash(state);
                profile.signature_hash().hash(state);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegColorTags {
    pub color_primaries: &'static str,
    pub color_trc: &'static str,
    pub colorspace: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RgbaF32Frame {
    pub width: u32,
    pub height: u32,
    /// Linear-light RGBA values. RGB may exceed 1.0 for HDR working spaces.
    pub data: Vec<[f32; 4]>,
    pub color_space: ColorSpace,
}

impl RgbaF32Frame {
    pub fn from_rgba8(
        width: u32,
        height: u32,
        rgba: &[u8],
        input: ColorSpace,
        working: ColorSpace,
        tone_map: bool,
    ) -> Self {
        let expected = width as usize * height as usize * 4;
        let pixels = rgba.get(..expected).unwrap_or(rgba);
        let mut data = Vec::with_capacity(pixels.len() / 4);
        for px in pixels.chunks_exact(4) {
            let mut rgb = [
                decode_transfer(input, px[0] as f32 / 255.0),
                decode_transfer(input, px[1] as f32 / 255.0),
                decode_transfer(input, px[2] as f32 / 255.0),
            ];
            rgb = convert_primaries(rgb, input, working);
            if tone_map && input.is_hdr() && !working.is_hdr() {
                rgb = rgb.map(aces_tone_map);
            }
            data.push([rgb[0], rgb[1], rgb[2], px[3] as f32 / 255.0]);
        }
        Self { width, height, data, color_space: working }
    }

    pub fn to_rgba8(&self, output: ColorSpace, tone_map: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        for px in &self.data {
            let mut rgb = [px[0], px[1], px[2]];
            if tone_map && self.color_space.is_hdr() && !output.is_hdr() {
                rgb = rgb.map(aces_tone_map);
            }
            rgb = convert_primaries(rgb, self.color_space, output);
            out.push(encode_u8(output, rgb[0]));
            out.push(encode_u8(output, rgb[1]));
            out.push(encode_u8(output, rgb[2]));
            out.push((px[3].clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        out
    }

    pub fn convert_to(&mut self, output: ColorSpace, tone_map: bool) {
        if self.color_space == output {
            return;
        }
        for px in &mut self.data {
            let mut rgb = [px[0], px[1], px[2]];
            if tone_map && self.color_space.is_hdr() && !output.is_hdr() {
                rgb = rgb.map(aces_tone_map);
            }
            rgb = convert_primaries(rgb, self.color_space, output);
            px[0] = rgb[0];
            px[1] = rgb[1];
            px[2] = rgb[2];
        }
        self.color_space = output;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayColorProfile {
    pub name: String,
    pub color_space: ColorSpace,
    pub linear_matrix: [[f32; 3]; 3],
    pub gamma: f32,
    pub black_luminance_nits: f32,
    pub white_luminance_nits: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icc_bytes: Option<Vec<u8>>,
}

impl DisplayColorProfile {
    pub fn rec709_reference() -> Self {
        Self {
            name: "Rec.709 Reference".to_string(),
            color_space: ColorSpace::Rec709,
            linear_matrix: IDENTITY_3,
            gamma: 1.0,
            black_luminance_nits: 0.0,
            white_luminance_nits: 100.0,
            icc_bytes: None,
        }
    }

    pub fn display_p3_reference() -> Self {
        Self {
            name: "Display P3 Reference".to_string(),
            color_space: ColorSpace::DciP3,
            linear_matrix: IDENTITY_3,
            gamma: 1.0,
            black_luminance_nits: 0.0,
            white_luminance_nits: 100.0,
            icc_bytes: None,
        }
    }

    pub fn from_icc_file(path: &Path) -> Result<Self, String> {
        let data = std::fs::read(path)
            .map_err(|err| format!("failed to read ICC profile {}: {err}", path.display()))?;
        let mut profile = Self::from_icc_bytes(&data)?;
        if profile.name.trim().is_empty() {
            profile.name =
                path.file_stem().and_then(|s| s.to_str()).unwrap_or("ICC Profile").to_string();
        }
        Ok(profile)
    }

    pub fn from_icc_bytes(bytes: &[u8]) -> Result<Self, String> {
        let parsed = parse_icc_display_profile(bytes)?;

        let profile = Self {
            name: parsed.name,
            color_space: parsed.color_space,
            linear_matrix: parsed.linear_matrix,
            gamma: parsed.gamma_compensation,
            black_luminance_nits: 0.0,
            white_luminance_nits: 100.0,
            icc_bytes: Some(bytes.to_vec()),
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("display profile name is empty".to_string());
        }
        if !(0.1..=10.0).contains(&self.gamma) {
            return Err(format!(
                "display profile gamma out of range: {}",
                self.gamma
            ));
        }
        if self.white_luminance_nits <= self.black_luminance_nits
            || self.white_luminance_nits <= 0.0
        {
            return Err("display profile luminance range is invalid".to_string());
        }
        for row in self.linear_matrix {
            for value in row {
                if !value.is_finite() {
                    return Err("display profile matrix contains non-finite value".to_string());
                }
            }
        }
        if let Some(icc) = &self.icc_bytes {
            if icc.is_empty() {
                return Err("display profile ICC payload is empty".to_string());
            }
        }
        Ok(())
    }

    pub fn signature_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.name.hash(&mut hasher);
        self.color_space.hash(&mut hasher);
        self.gamma.to_bits().hash(&mut hasher);
        self.black_luminance_nits.to_bits().hash(&mut hasher);
        self.white_luminance_nits.to_bits().hash(&mut hasher);
        for row in self.linear_matrix {
            for value in row {
                value.to_bits().hash(&mut hasher);
            }
        }
        if let Some(icc) = &self.icc_bytes {
            for b in icc {
                b.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaveformMode {
    Luma,
    RgbParade,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaveformScope {
    pub mode: WaveformMode,
    pub width: usize,
    pub bins: usize,
    pub values: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistogramScope {
    pub bins: usize,
    pub red: Vec<u32>,
    pub green: Vec<u32>,
    pub blue: Vec<u32>,
    pub luma: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorscopeSample {
    pub u: f32,
    pub v: f32,
    pub weight: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorScopes {
    pub histogram: HistogramScope,
    pub waveform: WaveformScope,
    pub vectorscope: Vec<VectorscopeSample>,
}

pub fn apply_display_profile_rgba8_in_place(
    data: &mut [u8],
    source: ColorSpace,
    profile: &DisplayColorProfile,
    tone_map: bool,
) -> Result<(), String> {
    profile.validate()?;
    let mut frame =
        RgbaF32Frame::from_rgba8(1, (data.len() / 4) as u32, data, source, source, tone_map);
    frame.convert_to(profile.color_space, tone_map);

    if let Some(icc_bytes) = profile.icc_bytes.as_deref() {
        let mut converted = frame.to_rgba8(profile.color_space, tone_map);
        if apply_icc_transform_rgba8(&mut converted, profile.color_space, icc_bytes).is_ok() {
            data.copy_from_slice(&converted[..data.len()]);
            return Ok(());
        }
    }

    for px in &mut frame.data {
        let rgb = mul3(profile.linear_matrix, [px[0], px[1], px[2]]);
        px[0] = rgb[0].max(0.0).powf(profile.gamma);
        px[1] = rgb[1].max(0.0).powf(profile.gamma);
        px[2] = rgb[2].max(0.0).powf(profile.gamma);
    }
    let converted = frame.to_rgba8(profile.color_space, tone_map);
    data.copy_from_slice(&converted[..data.len()]);
    Ok(())
}

fn apply_icc_transform_rgba8(
    rgba: &mut [u8],
    source_color_space: ColorSpace,
    display_icc: &[u8],
) -> Result<(), String> {
    let src_profile = cms_profile_for_color_space(source_color_space).ok_or_else(|| {
        format!("unsupported source color space for ICC transform: {source_color_space:?}")
    })?;
    let dst_profile = CmsColorProfile::new_from_slice(display_icc)
        .map_err(|err| format!("invalid ICC profile payload: {err}"))?;

    let transform = src_profile
        .create_transform_8bit(
            CmsLayout::Rgb,
            &dst_profile,
            CmsLayout::Rgb,
            TransformOptions::default(),
        )
        .map_err(|err| format!("failed to build ICC transform: {err}"))?;

    let pixels = rgba.len() / 4;
    let mut src_rgb = Vec::with_capacity(pixels * 3);
    for px in rgba.chunks_exact(4) {
        src_rgb.push(px[0]);
        src_rgb.push(px[1]);
        src_rgb.push(px[2]);
    }
    let mut dst_rgb = vec![0_u8; src_rgb.len()];
    transform
        .transform(&src_rgb, &mut dst_rgb)
        .map_err(|err| format!("failed to apply ICC transform: {err}"))?;

    for (px, rgb) in rgba.chunks_exact_mut(4).zip(dst_rgb.chunks_exact(3)) {
        px[0] = rgb[0];
        px[1] = rgb[1];
        px[2] = rgb[2];
    }
    Ok(())
}

fn cms_profile_for_color_space(color_space: ColorSpace) -> Option<CmsColorProfile> {
    match color_space {
        ColorSpace::Srgb => Some(CmsColorProfile::new_srgb()),
        ColorSpace::Rec709 => Some(CmsColorProfile::new_from_cicp(CicpProfile {
            color_primaries: CicpColorPrimaries::Bt709,
            transfer_characteristics: TransferCharacteristics::Bt709,
            matrix_coefficients: MatrixCoefficients::Bt709,
            full_range: false,
        })),
        ColorSpace::Rec2020 => Some(CmsColorProfile::new_bt2020()),
        ColorSpace::Rec2100Pq => Some(CmsColorProfile::new_bt2020_pq()),
        ColorSpace::Rec2100Hlg => Some(CmsColorProfile::new_bt2020_hlg()),
        ColorSpace::DciP3 => Some(CmsColorProfile::new_dci_p3()),
        ColorSpace::AppleLog | ColorSpace::SLog3 | ColorSpace::ArriLogC4 => None,
    }
}

pub fn compute_color_scopes(
    rgba: &[u8],
    width: u32,
    height: u32,
    waveform_mode: WaveformMode,
    bins: usize,
) -> ColorScopes {
    let bins = bins.clamp(16, 1024);
    let width_usize = width.max(1) as usize;
    let expected_pixels = width as usize * height as usize;
    let mut histogram = HistogramScope {
        bins,
        red: vec![0; bins],
        green: vec![0; bins],
        blue: vec![0; bins],
        luma: vec![0; bins],
    };
    let waveform_channels = match waveform_mode {
        WaveformMode::Luma => 1,
        WaveformMode::RgbParade => 3,
    };
    let mut waveform = WaveformScope {
        mode: waveform_mode,
        width: width_usize,
        bins,
        values: vec![0; width_usize * bins * waveform_channels],
    };
    let mut vectors = vec![VectorscopeSample { u: 0.0, v: 0.0, weight: 0 }; 64 * 64];

    for (idx, px) in rgba.chunks_exact(4).take(expected_pixels).enumerate() {
        let x = idx % width_usize;
        let r = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let b = px[2] as f32 / 255.0;
        let y = luma(r, g, b).clamp(0.0, 1.0);
        let rb = scope_bin(r, bins);
        let gb = scope_bin(g, bins);
        let bb = scope_bin(b, bins);
        let yb = scope_bin(y, bins);
        histogram.red[rb] += 1;
        histogram.green[gb] += 1;
        histogram.blue[bb] += 1;
        histogram.luma[yb] += 1;

        match waveform_mode {
            WaveformMode::Luma => waveform.values[x * bins + yb] += 1,
            WaveformMode::RgbParade => {
                let plane = width_usize * bins;
                waveform.values[x * bins + rb] += 1;
                waveform.values[plane + x * bins + gb] += 1;
                waveform.values[plane * 2 + x * bins + bb] += 1;
            }
        }

        let u = (b - y) * 0.565;
        let v = (r - y) * 0.713;
        let ux = ((u + 0.5).clamp(0.0, 0.999) * 64.0) as usize;
        let vy = ((v + 0.5).clamp(0.0, 0.999) * 64.0) as usize;
        let sample = &mut vectors[vy * 64 + ux];
        sample.u = (ux as f32 + 0.5) / 64.0 - 0.5;
        sample.v = (vy as f32 + 0.5) / 64.0 - 0.5;
        sample.weight = sample.weight.saturating_add(1);
    }

    ColorScopes {
        histogram,
        waveform,
        vectorscope: vectors.into_iter().filter(|sample| sample.weight > 0).collect(),
    }
}

impl ColorSpace {
    pub fn is_hdr(self) -> bool {
        matches!(self, Self::Rec2100Hlg | Self::Rec2100Pq)
    }

    pub fn ffmpeg_tags(self) -> FfmpegColorTags {
        match self {
            Self::Rec2100Hlg => FfmpegColorTags {
                color_primaries: "bt2020",
                color_trc: "arib-std-b67",
                colorspace: "bt2020nc",
            },
            Self::Rec2100Pq => FfmpegColorTags {
                color_primaries: "bt2020",
                color_trc: "smpte2084",
                colorspace: "bt2020nc",
            },
            Self::Rec2020 => FfmpegColorTags {
                color_primaries: "bt2020",
                color_trc: "bt709",
                colorspace: "bt2020nc",
            },
            Self::DciP3 | Self::AppleLog => FfmpegColorTags {
                color_primaries: "smpte432",
                color_trc: "bt709",
                colorspace: "bt709",
            },
            Self::Srgb => FfmpegColorTags {
                color_primaries: "bt709",
                color_trc: "iec61966-2-1",
                colorspace: "rgb",
            },
            Self::SLog3 | Self::ArriLogC4 | Self::Rec709 => FfmpegColorTags {
                color_primaries: "bt709",
                color_trc: "bt709",
                colorspace: "bt709",
            },
        }
    }
}

pub fn convert_rgba8_in_place(data: &mut [u8], pipeline: ColorPipeline) {
    if data.is_empty() || pipeline.is_noop() {
        return;
    }
    let _ = pipeline.transform_plan().apply_rgba8_in_place(data);
}

pub fn convert_rgba8(data: &[u8], pipeline: ColorPipeline) -> Vec<u8> {
    let mut out = data.to_vec();
    convert_rgba8_in_place(&mut out, pipeline);
    out
}

fn decode_transfer(space: ColorSpace, v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    match space {
        ColorSpace::Srgb => srgb_to_linear(v),
        ColorSpace::Rec2100Pq => pq_to_linear(v),
        ColorSpace::Rec2100Hlg => hlg_to_linear(v),
        ColorSpace::AppleLog => apple_log_to_linear(v),
        ColorSpace::SLog3 => slog3_to_linear(v),
        ColorSpace::ArriLogC4 => logc4_to_linear(v),
        ColorSpace::Rec709 | ColorSpace::Rec2020 | ColorSpace::DciP3 => rec709_to_linear(v),
    }
}

fn encode_transfer(space: ColorSpace, v: f32) -> f32 {
    let v = v.max(0.0);
    match space {
        ColorSpace::Srgb => linear_to_srgb(v),
        ColorSpace::Rec2100Pq => linear_to_pq(v),
        ColorSpace::Rec2100Hlg => linear_to_hlg(v),
        ColorSpace::AppleLog => linear_to_apple_log(v),
        ColorSpace::SLog3 => linear_to_slog3(v),
        ColorSpace::ArriLogC4 => linear_to_logc4(v),
        ColorSpace::Rec709 | ColorSpace::Rec2020 | ColorSpace::DciP3 => linear_to_rec709(v),
    }
}

fn encode_u8(space: ColorSpace, v: f32) -> u8 {
    (encode_transfer(space, v).clamp(0.0, 1.0) * 255.0).round() as u8
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn rec709_to_linear(v: f32) -> f32 {
    if v < 0.081 {
        v / 4.5
    } else {
        ((v + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

fn linear_to_rec709(v: f32) -> f32 {
    if v < 0.018 {
        v * 4.5
    } else {
        1.099 * v.powf(0.45) - 0.099
    }
}

fn pq_to_linear(v: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 32.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 128.0;
    const C3: f32 = 2392.0 / 128.0;
    let p = v.powf(1.0 / M2);
    let n = (p - C1).max(0.0);
    let d = C2 - C3 * p;
    100.0 * (n / d.max(1.0e-6)).powf(1.0 / M1)
}

fn linear_to_pq(v: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 32.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 128.0;
    const C3: f32 = 2392.0 / 128.0;
    let l = (v / 100.0).max(0.0).powf(M1);
    ((C1 + C2 * l) / (1.0 + C3 * l)).powf(M2)
}

fn hlg_to_linear(v: f32) -> f32 {
    const A: f32 = 0.17883277;
    const B: f32 = 0.28466892;
    const C: f32 = 0.559_910_7;
    let scene = if v <= 0.5 {
        (v * v) / 3.0
    } else {
        ((v - C) / A).exp() + B
    };
    scene * 12.0
}

fn linear_to_hlg(v: f32) -> f32 {
    const A: f32 = 0.17883277;
    const B: f32 = 0.28466892;
    const C: f32 = 0.559_910_7;
    let scene = (v / 12.0).max(0.0);
    if scene <= 1.0 / 12.0 {
        (3.0 * scene).sqrt()
    } else {
        A * (scene - B).max(1.0e-6).ln() + C
    }
}

fn apple_log_to_linear(v: f32) -> f32 {
    ((v - 0.385_537) / 0.143_894).exp2().max(0.0) * 0.18
}

fn linear_to_apple_log(v: f32) -> f32 {
    ((v.max(1.0e-6) / 0.18).log2() * 0.143_894) + 0.385_537
}

fn slog3_to_linear(v: f32) -> f32 {
    let x = ((v - 0.410_557) / 0.255).exp10();
    ((x - 0.01) / 5.0).max(0.0)
}

fn linear_to_slog3(v: f32) -> f32 {
    ((v.max(0.0) * 5.0 + 0.01).log10() * 0.255) + 0.410_557
}

fn logc4_to_linear(v: f32) -> f32 {
    ((v - 0.391_007) / 0.181_311).exp2().max(0.0) * 0.18
}

fn linear_to_logc4(v: f32) -> f32 {
    ((v.max(1.0e-6) / 0.18).log2() * 0.181_311) + 0.391_007
}

fn aces_tone_map(v: f32) -> f32 {
    let x = v.max(0.0);
    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0)
}

fn convert_primaries(rgb: [f32; 3], from: ColorSpace, to: ColorSpace) -> [f32; 3] {
    let from = primary_family(from);
    let to = primary_family(to);
    if from == to {
        return rgb;
    }

    match (from, to) {
        (PrimaryFamily::Rec709, PrimaryFamily::Rec2020) => mul3(M_REC709_TO_REC2020, rgb),
        (PrimaryFamily::Rec2020, PrimaryFamily::Rec709) => mul3(M_REC2020_TO_REC709, rgb),
        (PrimaryFamily::Rec709, PrimaryFamily::P3) => mul3(M_REC709_TO_P3, rgb),
        (PrimaryFamily::P3, PrimaryFamily::Rec709) => mul3(M_P3_TO_REC709, rgb),
        (PrimaryFamily::P3, PrimaryFamily::Rec2020) => {
            mul3(M_REC709_TO_REC2020, mul3(M_P3_TO_REC709, rgb))
        }
        (PrimaryFamily::Rec2020, PrimaryFamily::P3) => {
            mul3(M_REC709_TO_P3, mul3(M_REC2020_TO_REC709, rgb))
        }
        _ => rgb,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimaryFamily {
    Rec709,
    Rec2020,
    P3,
}

fn primary_family(space: ColorSpace) -> PrimaryFamily {
    match space {
        ColorSpace::Rec2100Hlg
        | ColorSpace::Rec2100Pq
        | ColorSpace::Rec2020
        | ColorSpace::SLog3 => PrimaryFamily::Rec2020,
        ColorSpace::DciP3 | ColorSpace::AppleLog => PrimaryFamily::P3,
        ColorSpace::Rec709 | ColorSpace::Srgb | ColorSpace::ArriLogC4 => PrimaryFamily::Rec709,
    }
}

const M_REC709_TO_REC2020: [[f32; 3]; 3] = [
    [0.627_404, 0.329_283, 0.043_313],
    [0.069_097, 0.919_540, 0.011_362],
    [0.016_391, 0.088_013, 0.895_596],
];
const M_REC2020_TO_REC709: [[f32; 3]; 3] = [
    [1.660_491, -0.587_641, -0.072_850],
    [-0.124_550, 1.132_9, -0.008_349],
    [-0.018_151, -0.100_579, 1.118_73],
];
const M_REC709_TO_P3: [[f32; 3]; 3] = [
    [0.822_462, 0.177_538, 0.0],
    [0.033_194, 0.966_806, 0.0],
    [0.017_083, 0.072_397, 0.910_520],
];
const M_P3_TO_REC709: [[f32; 3]; 3] = [
    [1.224_94, -0.224_940, 0.0],
    [-0.042_057, 1.042_057, 0.0],
    [-0.019_638, -0.078_636, 1.098_274],
];
const IDENTITY_3: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn scope_bin(v: f32, bins: usize) -> usize {
    (v.clamp(0.0, 1.0) * (bins.saturating_sub(1)) as f32).round() as usize
}

fn mul3(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

trait Exp10 {
    fn exp10(self) -> Self;
}

impl Exp10 for f32 {
    fn exp10(self) -> Self {
        10.0_f32.powf(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    struct ColorReferenceSample {
        name: &'static str,
        input: ColorSpace,
        working: ColorSpace,
        output: ColorSpace,
        tone_map: bool,
        rgba: [u8; 4],
        expected: [u8; 4],
        tolerance: u8,
    }

    const COLOR_REFERENCE_SAMPLES: [ColorReferenceSample; 4] = [
        ColorReferenceSample {
            name: "rec709 identity gray",
            input: ColorSpace::Rec709,
            working: ColorSpace::Rec709,
            output: ColorSpace::Rec709,
            tone_map: false,
            rgba: [128, 128, 128, 77],
            expected: [128, 128, 128, 77],
            tolerance: 1,
        },
        ColorReferenceSample {
            name: "srgb identity red ramp",
            input: ColorSpace::Srgb,
            working: ColorSpace::Srgb,
            output: ColorSpace::Srgb,
            tone_map: false,
            rgba: [204, 32, 16, 201],
            expected: [204, 32, 16, 201],
            tolerance: 1,
        },
        ColorReferenceSample {
            name: "rec2020 green primary to rec709 clips predictably",
            input: ColorSpace::Rec2020,
            working: ColorSpace::Rec2020,
            output: ColorSpace::Rec709,
            tone_map: false,
            rgba: [0, 255, 0, 255],
            expected: [0, 255, 0, 255],
            tolerance: 1,
        },
        ColorReferenceSample {
            name: "pq white tone maps to sdr white without alpha change",
            input: ColorSpace::Rec2100Pq,
            working: ColorSpace::Rec2100Pq,
            output: ColorSpace::Rec709,
            tone_map: true,
            rgba: [255, 255, 255, 33],
            expected: [255, 255, 255, 33],
            tolerance: 1,
        },
    ];

    #[test]
    fn identity_pipeline_keeps_rgba() {
        let mut rgba = vec![12, 34, 56, 78, 200, 210, 220, 230];
        let original = rgba.clone();
        convert_rgba8_in_place(
            &mut rgba,
            ColorPipeline::new(
                ColorSpace::Rec709,
                ColorSpace::Rec709,
                ColorSpace::Rec709,
                true,
            ),
        );
        assert_eq!(rgba, original);
    }

    #[test]
    fn color_reference_samples_stay_within_expected_tolerance() {
        for sample in COLOR_REFERENCE_SAMPLES {
            let mut rgba = sample.rgba.to_vec();
            convert_rgba8_in_place(
                &mut rgba,
                ColorPipeline::new(sample.input, sample.working, sample.output, sample.tone_map),
            );
            for (index, (actual, expected)) in rgba.iter().zip(sample.expected).enumerate() {
                assert!(
                    (*actual as i16 - expected as i16).unsigned_abs() <= sample.tolerance as u16,
                    "{} channel {index}: expected {expected}, got {actual}",
                    sample.name
                );
            }
        }
    }

    #[test]
    fn log_curve_reference_values_are_stable() {
        let apple_mid = linear_to_apple_log(0.18);
        let slog_mid = linear_to_slog3(0.18);
        let logc_mid = linear_to_logc4(0.18);
        assert!((apple_mid - 0.385_537).abs() < 1.0e-6);
        assert!((slog3_to_linear(slog_mid) - 0.18).abs() < 1.0e-5);
        assert!((logc4_to_linear(logc_mid) - 0.18).abs() < 1.0e-5);
    }

    #[test]
    fn hdr_to_sdr_tone_map_preserves_alpha_and_clamps() {
        let mut rgba = vec![255, 255, 255, 123];
        convert_rgba8_in_place(
            &mut rgba,
            ColorPipeline::new(
                ColorSpace::Rec2100Pq,
                ColorSpace::Rec2100Pq,
                ColorSpace::Rec709,
                true,
            ),
        );
        assert!(rgba[0] > 0 && rgba[1] > 0 && rgba[2] > 0);
        assert_eq!(rgba[3], 123);
    }

    #[test]
    fn rec2020_to_rec709_keeps_neutral_axis_close() {
        let mut rgba = vec![128, 128, 128, 255];
        convert_rgba8_in_place(
            &mut rgba,
            ColorPipeline::new(
                ColorSpace::Rec2020,
                ColorSpace::Rec2020,
                ColorSpace::Rec709,
                false,
            ),
        );
        assert!((rgba[0] as i16 - rgba[1] as i16).abs() <= 2);
        assert!((rgba[1] as i16 - rgba[2] as i16).abs() <= 2);
    }

    #[test]
    fn f32_frame_round_trip_preserves_sdr_values() {
        let rgba = vec![0, 64, 128, 255, 255, 128, 64, 32];
        let frame =
            RgbaF32Frame::from_rgba8(2, 1, &rgba, ColorSpace::Rec709, ColorSpace::Rec709, false);
        let out = frame.to_rgba8(ColorSpace::Rec709, false);
        for (actual, expected) in out.iter().zip(rgba) {
            assert!((*actual as i16 - expected as i16).abs() <= 1);
        }
    }

    #[test]
    fn display_profile_rejects_invalid_gamma() {
        let mut profile = DisplayColorProfile::rec709_reference();
        profile.gamma = 0.0;
        assert!(profile.validate().is_err());
    }

    #[test]
    fn display_profile_signature_changes_with_matrix() {
        let mut a = DisplayColorProfile::rec709_reference();
        let mut b = DisplayColorProfile::rec709_reference();
        assert_eq!(a.signature_hash(), b.signature_hash());
        b.linear_matrix[0][0] = 0.99;
        assert_ne!(a.signature_hash(), b.signature_hash());
        a.linear_matrix[0][0] = 0.99;
        assert_eq!(a.signature_hash(), b.signature_hash());
    }

    #[test]
    fn display_profile_preserves_alpha() {
        let mut rgba = vec![64, 128, 192, 17];
        apply_display_profile_rgba8_in_place(
            &mut rgba,
            ColorSpace::Rec709,
            &DisplayColorProfile::rec709_reference(),
            false,
        )
        .expect("valid display profile");
        assert_eq!(rgba[3], 17);
    }

    #[test]
    fn display_profile_from_icc_rejects_invalid_payload() {
        let bad_payload = vec![0_u8; 16];
        assert!(DisplayColorProfile::from_icc_bytes(&bad_payload).is_err());
    }

    #[test]
    fn scopes_count_pixels_and_rgb_parade_channels() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 255];
        let scopes = compute_color_scopes(&rgba, 2, 2, WaveformMode::RgbParade, 16);
        assert_eq!(scopes.histogram.red.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.green.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.blue.iter().sum::<u32>(), 4);
        assert_eq!(scopes.histogram.luma.iter().sum::<u32>(), 4);
        assert_eq!(scopes.waveform.values.iter().sum::<u32>(), 12);
        assert!(!scopes.vectorscope.is_empty());
    }
}
