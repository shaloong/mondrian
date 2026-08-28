//! Deterministic, bounded CPU tracking for authored Mask geometry.
//!
//! This module owns image analysis only. It receives explicit CPU luminance
//! rasters and emits normalized-coordinate transforms plus quality evidence;
//! media decoding, job scheduling, caching, and author transactions belong to
//! the application adapter.

use glam::Vec2;
use mondrian_core::mask_data::{BezierPoint, MaskShape, MaskTrackingModel, MaskTrackingSettings};
use thiserror::Error;

const MIN_OBJECT_MATCHES: usize = 4;
const MIN_PLANAR_MATCHES: usize = 8;
const PLANAR_RANSAC_ITERATIONS: usize = 96;
const FEATURE_SPACING_PX: f64 = 7.0;
const MIN_PATCH_CORRELATION: f64 = 0.62;
const MIN_PATCH_MARGIN: f64 = 0.015;
const MAX_PROJECTIVE_COORDINATE: f64 = 8.0;

/// One immutable single-channel analysis frame.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackingFrame {
    width: u32,
    height: u32,
    luminance: Vec<f32>,
}

impl TrackingFrame {
    /// Validate and construct a finite normalized luminance raster.
    pub fn new(width: u32, height: u32, luminance: Vec<f32>) -> Result<Self, TrackingError> {
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height).ok().and_then(|height| width.checked_mul(height))
            })
            .ok_or(TrackingError::InvalidFrameExtent)?;
        if width < 16 || height < 16 || luminance.len() != expected {
            return Err(TrackingError::InvalidFrameExtent);
        }
        if luminance.iter().any(|sample| !sample.is_finite()) {
            return Err(TrackingError::NonFiniteFrame);
        }
        Ok(Self { width, height, luminance })
    }

    /// Convert straight RGBA8 samples to analysis luminance.
    pub fn from_rgba8(width: u32, height: u32, rgba: &[u8]) -> Result<Self, TrackingError> {
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height).ok().and_then(|height| width.checked_mul(height))
            })
            .ok_or(TrackingError::InvalidFrameExtent)?;
        if rgba.len() != pixels.checked_mul(4).ok_or(TrackingError::InvalidFrameExtent)? {
            return Err(TrackingError::InvalidFrameExtent);
        }
        let luminance = rgba
            .chunks_exact(4)
            .map(|pixel| {
                (0.2126 * f32::from(pixel[0])
                    + 0.7152 * f32::from(pixel[1])
                    + 0.0722 * f32::from(pixel[2]))
                    / 255.0
            })
            .collect();
        Self::new(width, height, luminance)
    }

    /// Convert straight RGBA32F samples to analysis luminance.
    pub fn from_rgba_f32(width: u32, height: u32, rgba: &[f32]) -> Result<Self, TrackingError> {
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height).ok().and_then(|height| width.checked_mul(height))
            })
            .ok_or(TrackingError::InvalidFrameExtent)?;
        if rgba.len() != pixels.checked_mul(4).ok_or(TrackingError::InvalidFrameExtent)? {
            return Err(TrackingError::InvalidFrameExtent);
        }
        let mut luminance = Vec::with_capacity(pixels);
        for pixel in rgba.chunks_exact(4) {
            if pixel.iter().any(|sample| !sample.is_finite()) {
                return Err(TrackingError::NonFiniteFrame);
            }
            luminance.push(0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2]);
        }
        Self::new(width, height, luminance)
    }

    /// Raster width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Raster height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    fn sample(&self, x: i32, y: i32) -> f64 {
        let index = usize::try_from(y).expect("validated positive y")
            * usize::try_from(self.width).expect("u32 width fits usize")
            + usize::try_from(x).expect("validated positive x");
        f64::from(self.luminance[index])
    }
}

/// Normalized axis-aligned feature-search region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackingRegion {
    /// Inclusive normalized minimum corner.
    pub min: Vec2,
    /// Inclusive normalized maximum corner.
    pub max: Vec2,
}

impl TrackingRegion {
    /// Build the conservative normalized bounds of a Mask shape.
    pub fn from_shape(shape: &MaskShape) -> Result<Self, TrackingError> {
        let (min, max) = match shape {
            MaskShape::Rectangle { x, y, width, height, .. } => {
                (Vec2::new(*x, *y), Vec2::new(*x + *width, *y + *height))
            }
            MaskShape::Ellipse { center, radii } => (*center - *radii, *center + *radii),
            MaskShape::Path { points, .. } => {
                let first = points.first().ok_or(TrackingError::InvalidRegion)?.position;
                points.iter().fold((first, first), |(min, max), point| {
                    (min.min(point.position), max.max(point.position))
                })
            }
        };
        if !min.is_finite() || !max.is_finite() || min.x >= max.x || min.y >= max.y {
            return Err(TrackingError::InvalidRegion);
        }
        Ok(Self { min, max })
    }
}

/// One normalized-coordinate projective transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackingTransform {
    matrix: [f64; 9],
}

impl TrackingTransform {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        matrix: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    };

    /// Return row-major normalized-coordinate matrix coefficients.
    pub const fn matrix(self) -> [f64; 9] {
        self.matrix
    }

    /// Compose `self` after an earlier accumulated transform.
    pub fn compose_after(self, earlier: Self) -> Result<Self, TrackingError> {
        let a = self.matrix;
        let b = earlier.matrix;
        let mut result = [0.0; 9];
        for row in 0..3 {
            for column in 0..3 {
                result[row * 3 + column] =
                    (0..3).map(|index| a[row * 3 + index] * b[index * 3 + column]).sum();
            }
        }
        let transform = Self { matrix: result };
        transform.validate()?;
        Ok(transform)
    }

    /// Transform one normalized point and reject projective singularities.
    pub fn transform_point(self, point: Vec2) -> Result<Vec2, TrackingError> {
        let x = f64::from(point.x);
        let y = f64::from(point.y);
        let w = self.matrix[6] * x + self.matrix[7] * y + self.matrix[8];
        if !w.is_finite() || w.abs() < 1.0e-8 {
            return Err(TrackingError::DegenerateTransform);
        }
        let transformed = Vec2::new(
            ((self.matrix[0] * x + self.matrix[1] * y + self.matrix[2]) / w) as f32,
            ((self.matrix[3] * x + self.matrix[4] * y + self.matrix[5]) / w) as f32,
        );
        if !transformed.is_finite()
            || f64::from(transformed.x.abs()) > MAX_PROJECTIVE_COORDINATE
            || f64::from(transformed.y.abs()) > MAX_PROJECTIVE_COORDINATE
        {
            return Err(TrackingError::DegenerateTransform);
        }
        Ok(transformed)
    }

    fn translation(dx: f64, dy: f64) -> Self {
        Self {
            matrix: [1.0, 0.0, dx, 0.0, 1.0, dy, 0.0, 0.0, 1.0],
        }
    }

    fn validate(self) -> Result<(), TrackingError> {
        if self.matrix.iter().any(|value| !value.is_finite()) {
            return Err(TrackingError::DegenerateTransform);
        }
        let determinant = self.matrix[0]
            * (self.matrix[4] * self.matrix[8] - self.matrix[5] * self.matrix[7])
            - self.matrix[1] * (self.matrix[3] * self.matrix[8] - self.matrix[5] * self.matrix[6])
            + self.matrix[2] * (self.matrix[3] * self.matrix[7] - self.matrix[4] * self.matrix[6]);
        if !determinant.is_finite() || determinant.abs() < 1.0e-10 {
            return Err(TrackingError::DegenerateTransform);
        }
        for point in [Vec2::ZERO, Vec2::X, Vec2::Y, Vec2::ONE, Vec2::splat(0.5)] {
            self.transform_point(point)?;
        }
        Ok(())
    }
}

/// Quality evidence for one accepted frame-pair observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackingQuality {
    /// Feature correspondences surviving patch ambiguity checks.
    pub matched_features: u16,
    /// Correspondences agreeing with the fitted motion model.
    pub inlier_features: u16,
    /// Normalized inlier fraction.
    pub inlier_ratio: f32,
    /// Root-mean-square model residual in analysis pixels.
    pub rms_error_px: f32,
    /// Fraction of the tracked region spanned by inlier points.
    pub feature_coverage: f32,
}

/// Accepted motion and quality for one adjacent frame pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackingObservation {
    /// Transform from the reference frame into the candidate frame.
    pub transform: TrackingTransform,
    /// Evidence used to reject weak or degenerate observations.
    pub quality: TrackingQuality,
}

/// Structured analysis failure.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum TrackingError {
    /// Raster extent or channel payload is invalid.
    #[error("tracking frame extent or payload is invalid")]
    InvalidFrameExtent,
    /// Raster contains non-finite samples.
    #[error("tracking frame contains non-finite samples")]
    NonFiniteFrame,
    /// Frames in one pair do not share an analysis raster.
    #[error("tracking frame extents do not match")]
    FrameExtentMismatch,
    /// Region is empty or non-finite.
    #[error("tracking region is invalid")]
    InvalidRegion,
    /// Cooperative cancellation was observed.
    #[error("tracking analysis was canceled")]
    Canceled,
    /// Region does not contain enough repeatable texture.
    #[error("tracking region has insufficient repeatable texture")]
    InsufficientTexture,
    /// Patch matching did not produce enough unambiguous correspondences.
    #[error("tracking produced insufficient feature correspondences")]
    InsufficientMatches,
    /// Fitted model did not satisfy the configured inlier threshold.
    #[error("tracking motion model did not reach the required quality")]
    LowConfidence,
    /// Linear solve or resulting projective mapping was singular.
    #[error("tracking motion model is degenerate")]
    DegenerateTransform,
    /// Persisted analysis settings are invalid.
    #[error("tracking settings are invalid: {0}")]
    InvalidSettings(String),
}

#[derive(Debug, Clone, Copy)]
struct Feature {
    x: i32,
    y: i32,
    response: f64,
}

#[derive(Debug, Clone, Copy)]
struct Match {
    from: [f64; 2],
    to: [f64; 2],
}

/// Track one adjacent CPU frame pair with a deterministic bounded workload.
pub fn track_frame_pair(
    reference: &TrackingFrame,
    candidate: &TrackingFrame,
    region: TrackingRegion,
    model: MaskTrackingModel,
    settings: MaskTrackingSettings,
    mut canceled: impl FnMut() -> bool,
) -> Result<TrackingObservation, TrackingError> {
    settings
        .validate()
        .map_err(|error| TrackingError::InvalidSettings(error.to_string()))?;
    if reference.width != candidate.width || reference.height != candidate.height {
        return Err(TrackingError::FrameExtentMismatch);
    }
    if canceled() {
        return Err(TrackingError::Canceled);
    }
    let features = select_features(reference, region, settings, &mut canceled)?;
    let matches = match_features(reference, candidate, &features, settings, &mut canceled)?;
    let minimum = match model {
        MaskTrackingModel::ObjectTranslation => MIN_OBJECT_MATCHES,
        MaskTrackingModel::PlanarHomography => MIN_PLANAR_MATCHES,
    };
    if matches.len() < minimum {
        return Err(TrackingError::InsufficientMatches);
    }
    let (transform, inliers, rms_error_px) = match model {
        MaskTrackingModel::ObjectTranslation => fit_translation(reference, &matches),
        MaskTrackingModel::PlanarHomography => {
            fit_planar_homography(reference, &matches, &mut canceled)
        }
    }?;
    let inlier_ratio = inliers.len() as f32 / matches.len() as f32;
    if inlier_ratio + f32::EPSILON < settings.minimum_inlier_ratio {
        return Err(TrackingError::LowConfidence);
    }
    let feature_coverage = feature_coverage(&matches, &inliers, region);
    let quality = TrackingQuality {
        matched_features: matches.len().min(usize::from(u16::MAX)) as u16,
        inlier_features: inliers.len().min(usize::from(u16::MAX)) as u16,
        inlier_ratio,
        rms_error_px: rms_error_px as f32,
        feature_coverage,
    };
    Ok(TrackingObservation { transform, quality })
}

fn select_features(
    frame: &TrackingFrame,
    region: TrackingRegion,
    settings: MaskTrackingSettings,
    canceled: &mut impl FnMut() -> bool,
) -> Result<Vec<Feature>, TrackingError> {
    let margin = i32::from(settings.patch_radius) + i32::from(settings.search_radius) + 2;
    let width = i32::try_from(frame.width).map_err(|_| TrackingError::InvalidFrameExtent)?;
    let height = i32::try_from(frame.height).map_err(|_| TrackingError::InvalidFrameExtent)?;
    let min_x = ((f64::from(region.min.x) * f64::from(frame.width)).floor() as i32)
        .clamp(margin, width - margin - 1);
    let max_x = ((f64::from(region.max.x) * f64::from(frame.width)).ceil() as i32)
        .clamp(margin, width - margin - 1);
    let min_y = ((f64::from(region.min.y) * f64::from(frame.height)).floor() as i32)
        .clamp(margin, height - margin - 1);
    let max_y = ((f64::from(region.max.y) * f64::from(frame.height)).ceil() as i32)
        .clamp(margin, height - margin - 1);
    if min_x >= max_x || min_y >= max_y {
        return Err(TrackingError::InvalidRegion);
    }
    let mut candidates = Vec::new();
    for y in (min_y..=max_y).step_by(3) {
        if canceled() {
            return Err(TrackingError::Canceled);
        }
        for x in (min_x..=max_x).step_by(3) {
            let mut xx = 0.0;
            let mut yy = 0.0;
            let mut xy = 0.0;
            for oy in -1..=1 {
                for ox in -1..=1 {
                    let gx = frame.sample(x + ox + 1, y + oy) - frame.sample(x + ox - 1, y + oy);
                    let gy = frame.sample(x + ox, y + oy + 1) - frame.sample(x + ox, y + oy - 1);
                    xx += gx * gx;
                    yy += gy * gy;
                    xy += gx * gy;
                }
            }
            let trace = xx + yy;
            let discriminant = ((xx - yy) * (xx - yy) + 4.0 * xy * xy).sqrt();
            let response = 0.5 * (trace - discriminant);
            if response > 1.0e-5 {
                candidates.push(Feature { x, y, response });
            }
        }
    }
    candidates.sort_by(|left, right| {
        right
            .response
            .total_cmp(&left.response)
            .then_with(|| left.y.cmp(&right.y))
            .then_with(|| left.x.cmp(&right.x))
    });
    let mut selected = Vec::with_capacity(usize::from(settings.max_features));
    let spacing_squared = FEATURE_SPACING_PX * FEATURE_SPACING_PX;
    for feature in candidates {
        if selected.iter().all(|existing: &Feature| {
            let dx = f64::from(existing.x - feature.x);
            let dy = f64::from(existing.y - feature.y);
            dx * dx + dy * dy >= spacing_squared
        }) {
            selected.push(feature);
            if selected.len() == usize::from(settings.max_features) {
                break;
            }
        }
    }
    if selected.len() < MIN_PLANAR_MATCHES {
        return Err(TrackingError::InsufficientTexture);
    }
    Ok(selected)
}

fn match_features(
    reference: &TrackingFrame,
    candidate: &TrackingFrame,
    features: &[Feature],
    settings: MaskTrackingSettings,
    canceled: &mut impl FnMut() -> bool,
) -> Result<Vec<Match>, TrackingError> {
    let search = i32::from(settings.search_radius);
    let patch = i32::from(settings.patch_radius);
    let mut matches = Vec::with_capacity(features.len());
    for feature in features {
        if canceled() {
            return Err(TrackingError::Canceled);
        }
        let mut best = (f64::NEG_INFINITY, feature.x, feature.y);
        let mut second = f64::NEG_INFINITY;
        for dy in -search..=search {
            if canceled() {
                return Err(TrackingError::Canceled);
            }
            for dx in -search..=search {
                let score = patch_correlation(
                    reference,
                    candidate,
                    feature.x,
                    feature.y,
                    feature.x + dx,
                    feature.y + dy,
                    patch,
                );
                if score > best.0 {
                    second = best.0;
                    best = (score, feature.x + dx, feature.y + dy);
                } else if score > second {
                    second = score;
                }
            }
        }
        let coarse = best;
        for y in coarse.2.saturating_sub(2)..=coarse.2.saturating_add(2) {
            for x in coarse.1.saturating_sub(2)..=coarse.1.saturating_add(2) {
                let score =
                    patch_correlation(reference, candidate, feature.x, feature.y, x, y, patch);
                if score > best.0 {
                    second = best.0;
                    best = (score, x, y);
                } else if score > second && (x != best.1 || y != best.2) {
                    second = score;
                }
            }
        }
        if best.0 >= MIN_PATCH_CORRELATION && best.0 - second >= MIN_PATCH_MARGIN {
            matches.push(Match {
                from: [
                    f64::from(feature.x) / f64::from(reference.width),
                    f64::from(feature.y) / f64::from(reference.height),
                ],
                to: [
                    f64::from(best.1) / f64::from(reference.width),
                    f64::from(best.2) / f64::from(reference.height),
                ],
            });
        }
    }
    Ok(matches)
}

fn patch_correlation(
    reference: &TrackingFrame,
    candidate: &TrackingFrame,
    reference_x: i32,
    reference_y: i32,
    candidate_x: i32,
    candidate_y: i32,
    radius: i32,
) -> f64 {
    let width = i32::try_from(candidate.width).unwrap_or(i32::MAX);
    let height = i32::try_from(candidate.height).unwrap_or(i32::MAX);
    if candidate_x - radius < 0
        || candidate_y - radius < 0
        || candidate_x + radius >= width
        || candidate_y + radius >= height
    {
        return f64::NEG_INFINITY;
    }
    let mut reference_sum = 0.0;
    let mut candidate_sum = 0.0;
    let mut count = 0.0;
    for y in (-radius..=radius).step_by(2) {
        for x in (-radius..=radius).step_by(2) {
            reference_sum += reference.sample(reference_x + x, reference_y + y);
            candidate_sum += candidate.sample(candidate_x + x, candidate_y + y);
            count += 1.0;
        }
    }
    let reference_mean = reference_sum / count;
    let candidate_mean = candidate_sum / count;
    let mut covariance = 0.0;
    let mut reference_energy = 0.0;
    let mut candidate_energy = 0.0;
    for y in (-radius..=radius).step_by(2) {
        for x in (-radius..=radius).step_by(2) {
            let left = reference.sample(reference_x + x, reference_y + y) - reference_mean;
            let right = candidate.sample(candidate_x + x, candidate_y + y) - candidate_mean;
            covariance += left * right;
            reference_energy += left * left;
            candidate_energy += right * right;
        }
    }
    let denominator = (reference_energy * candidate_energy).sqrt();
    if denominator <= 1.0e-12 {
        f64::NEG_INFINITY
    } else {
        covariance / denominator
    }
}

fn fit_translation(
    frame: &TrackingFrame,
    matches: &[Match],
) -> Result<(TrackingTransform, Vec<usize>, f64), TrackingError> {
    let mut dx = matches.iter().map(|pair| pair.to[0] - pair.from[0]).collect::<Vec<_>>();
    let mut dy = matches.iter().map(|pair| pair.to[1] - pair.from[1]).collect::<Vec<_>>();
    dx.sort_by(f64::total_cmp);
    dy.sort_by(f64::total_cmp);
    let translation = [median(&dx), median(&dy)];
    let threshold = 2.5 / f64::from(frame.width.max(frame.height));
    let mut inliers = Vec::new();
    let mut squared_error_px = 0.0;
    let scale = f64::from(frame.width.max(frame.height));
    for (index, pair) in matches.iter().enumerate() {
        let ex = pair.to[0] - pair.from[0] - translation[0];
        let ey = pair.to[1] - pair.from[1] - translation[1];
        let error = ex.hypot(ey);
        if error <= threshold {
            inliers.push(index);
            squared_error_px += (error * scale).powi(2);
        }
    }
    if inliers.len() < MIN_OBJECT_MATCHES {
        return Err(TrackingError::LowConfidence);
    }
    let mut inlier_dx = inliers
        .iter()
        .map(|index| matches[*index].to[0] - matches[*index].from[0])
        .collect::<Vec<_>>();
    let mut inlier_dy = inliers
        .iter()
        .map(|index| matches[*index].to[1] - matches[*index].from[1])
        .collect::<Vec<_>>();
    inlier_dx.sort_by(f64::total_cmp);
    inlier_dy.sort_by(f64::total_cmp);
    let transform = TrackingTransform::translation(median(&inlier_dx), median(&inlier_dy));
    let rms = (squared_error_px / inliers.len() as f64).sqrt();
    Ok((transform, inliers, rms))
}

fn median(values: &[f64]) -> f64 {
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    }
}

fn fit_planar_homography(
    frame: &TrackingFrame,
    matches: &[Match],
    canceled: &mut impl FnMut() -> bool,
) -> Result<(TrackingTransform, Vec<usize>, f64), TrackingError> {
    let threshold = 3.0 / f64::from(frame.width.max(frame.height));
    let mut rng = 0x9e37_79b9_7f4a_7c15_u64 ^ matches.len() as u64;
    let mut best_inliers = Vec::new();
    let mut best_error = f64::INFINITY;
    for _ in 0..PLANAR_RANSAC_ITERATIONS {
        if canceled() {
            return Err(TrackingError::Canceled);
        }
        let sample = four_distinct_indices(matches.len(), &mut rng);
        let sample_matches = sample.map(|index| matches[index]);
        let Ok(candidate) = solve_homography(&sample_matches) else {
            continue;
        };
        let (inliers, error) = homography_inliers(candidate, matches, threshold);
        if inliers.len() > best_inliers.len()
            || (inliers.len() == best_inliers.len() && error < best_error)
        {
            best_inliers = inliers;
            best_error = error;
        }
    }
    if best_inliers.len() < MIN_PLANAR_MATCHES {
        return Err(TrackingError::LowConfidence);
    }
    let refit = best_inliers.iter().map(|index| matches[*index]).collect::<Vec<_>>();
    let transform = solve_homography(&refit)?;
    let (inliers, squared_error) = homography_inliers(transform, matches, threshold);
    if inliers.len() < MIN_PLANAR_MATCHES {
        return Err(TrackingError::LowConfidence);
    }
    let scale = f64::from(frame.width.max(frame.height));
    let rms = (squared_error / inliers.len() as f64).sqrt() * scale;
    Ok((transform, inliers, rms))
}

fn four_distinct_indices(length: usize, state: &mut u64) -> [usize; 4] {
    let mut result = [0; 4];
    for index in 0..4 {
        loop {
            *state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let candidate = (*state as usize) % length;
            if !result[..index].contains(&candidate) {
                result[index] = candidate;
                break;
            }
        }
    }
    result
}

fn solve_homography(matches: &[Match]) -> Result<TrackingTransform, TrackingError> {
    if matches.len() < 4 {
        return Err(TrackingError::DegenerateTransform);
    }
    let mut normal = [[0.0_f64; 9]; 8];
    for pair in matches {
        let [x, y] = pair.from;
        let [u, v] = pair.to;
        let rows = [
            ([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u),
            ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v),
        ];
        for (row, target) in rows {
            for column in 0..8 {
                for other in 0..8 {
                    normal[column][other] += row[column] * row[other];
                }
                normal[column][8] += row[column] * target;
            }
        }
    }
    let solution = solve_linear_8(normal)?;
    let transform = TrackingTransform {
        matrix: [
            solution[0],
            solution[1],
            solution[2],
            solution[3],
            solution[4],
            solution[5],
            solution[6],
            solution[7],
            1.0,
        ],
    };
    transform.validate()?;
    Ok(transform)
}

fn solve_linear_8(mut matrix: [[f64; 9]; 8]) -> Result<[f64; 8], TrackingError> {
    for column in 0..8 {
        let pivot = (column..8)
            .max_by(|left, right| {
                matrix[*left][column].abs().total_cmp(&matrix[*right][column].abs())
            })
            .ok_or(TrackingError::DegenerateTransform)?;
        if matrix[pivot][column].abs() < 1.0e-10 {
            return Err(TrackingError::DegenerateTransform);
        }
        matrix.swap(column, pivot);
        let divisor = matrix[column][column];
        for value in &mut matrix[column][column..] {
            *value /= divisor;
        }
        let pivot_row = matrix[column];
        for (row_index, row) in matrix.iter_mut().enumerate() {
            if row_index == column {
                continue;
            }
            let factor = row[column];
            for (value, pivot_value) in row[column..].iter_mut().zip(&pivot_row[column..]) {
                *value -= factor * pivot_value;
            }
        }
    }
    let mut result = [0.0; 8];
    for index in 0..8 {
        result[index] = matrix[index][8];
    }
    Ok(result)
}

fn homography_inliers(
    transform: TrackingTransform,
    matches: &[Match],
    threshold: f64,
) -> (Vec<usize>, f64) {
    let mut inliers = Vec::new();
    let mut squared_error = 0.0;
    for (index, pair) in matches.iter().enumerate() {
        let point = transform.transform_point(Vec2::new(pair.from[0] as f32, pair.from[1] as f32));
        let Ok(point) = point else {
            continue;
        };
        let error = (f64::from(point.x) - pair.to[0]).hypot(f64::from(point.y) - pair.to[1]);
        if error <= threshold {
            inliers.push(index);
            squared_error += error * error;
        }
    }
    (inliers, squared_error)
}

fn feature_coverage(matches: &[Match], inliers: &[usize], region: TrackingRegion) -> f32 {
    if inliers.is_empty() {
        return 0.0;
    }
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for index in inliers {
        let point = matches[*index].from;
        min = min.min(Vec2::new(point[0] as f32, point[1] as f32));
        max = max.max(Vec2::new(point[0] as f32, point[1] as f32));
    }
    let covered = (max.x - min.x).max(0.0) * (max.y - min.y).max(0.0);
    let region_area = (region.max.x - region.min.x) * (region.max.y - region.min.y);
    if region_area <= f32::EPSILON {
        0.0
    } else {
        (covered / region_area).clamp(0.0, 1.0)
    }
}

/// Canonicalize a shape for the selected motion model.
///
/// Projective transforms cannot truthfully remain axis-aligned Rectangle or
/// Ellipse values, so planar tracking converts them once to a fixed-topology
/// closed Bezier path. Translation tracking preserves the original type.
pub fn canonicalize_tracking_shape(shape: &MaskShape, model: MaskTrackingModel) -> MaskShape {
    if model == MaskTrackingModel::ObjectTranslation {
        return shape.clone();
    }
    match shape {
        MaskShape::Path { .. } => shape.clone(),
        MaskShape::Rectangle { x, y, width, height, .. } => MaskShape::Path {
            points: vec![
                BezierPoint::new(Vec2::new(*x, *y)),
                BezierPoint::new(Vec2::new(*x + *width, *y)),
                BezierPoint::new(Vec2::new(*x + *width, *y + *height)),
                BezierPoint::new(Vec2::new(*x, *y + *height)),
            ],
            closed: true,
        },
        MaskShape::Ellipse { center, radii } => ellipse_path(*center, *radii),
    }
}

fn ellipse_path(center: Vec2, radii: Vec2) -> MaskShape {
    const KAPPA: f32 = 0.552_284_8;
    MaskShape::Path {
        points: vec![
            BezierPoint {
                position: center + Vec2::new(radii.x, 0.0),
                control_in: Vec2::new(0.0, -radii.y * KAPPA),
                control_out: Vec2::new(0.0, radii.y * KAPPA),
            },
            BezierPoint {
                position: center + Vec2::new(0.0, radii.y),
                control_in: Vec2::new(radii.x * KAPPA, 0.0),
                control_out: Vec2::new(-radii.x * KAPPA, 0.0),
            },
            BezierPoint {
                position: center + Vec2::new(-radii.x, 0.0),
                control_in: Vec2::new(0.0, radii.y * KAPPA),
                control_out: Vec2::new(0.0, -radii.y * KAPPA),
            },
            BezierPoint {
                position: center + Vec2::new(0.0, -radii.y),
                control_in: Vec2::new(-radii.x * KAPPA, 0.0),
                control_out: Vec2::new(radii.x * KAPPA, 0.0),
            },
        ],
        closed: true,
    }
}

/// Transform complete Mask geometry in normalized coordinates.
pub fn transform_tracking_shape(
    shape: &MaskShape,
    transform: TrackingTransform,
    model: MaskTrackingModel,
) -> Result<MaskShape, TrackingError> {
    let shape = canonicalize_tracking_shape(shape, model);
    match shape {
        MaskShape::Rectangle { x, y, width, height, corner_radius } => {
            let origin = transform.transform_point(Vec2::new(x, y))?;
            Ok(MaskShape::Rectangle {
                x: origin.x,
                y: origin.y,
                width,
                height,
                corner_radius,
            })
        }
        MaskShape::Ellipse { center, radii } => {
            Ok(MaskShape::Ellipse { center: transform.transform_point(center)?, radii })
        }
        MaskShape::Path { points, closed } => {
            let points = points
                .into_iter()
                .map(|point| {
                    let position = transform.transform_point(point.position)?;
                    let control_in_endpoint =
                        transform.transform_point(point.position + point.control_in)?;
                    let control_out_endpoint =
                        transform.transform_point(point.position + point.control_out)?;
                    Ok(BezierPoint {
                        position,
                        control_in: control_in_endpoint - position,
                        control_out: control_out_endpoint - position,
                    })
                })
                .collect::<Result<Vec<_>, TrackingError>>()?;
            Ok(MaskShape::Path { points, closed })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translated_texture(width: u32, height: u32, dx: i32, dy: i32) -> TrackingFrame {
        let mut luma = vec![0.05; (width * height) as usize];
        for y in 12..height as i32 - 12 {
            for x in 12..width as i32 - 12 {
                let source_x = x - dx;
                let source_y = y - dy;
                if source_x >= 12
                    && source_x < width as i32 - 12
                    && source_y >= 12
                    && source_y < height as i32 - 12
                {
                    let mut hash = (source_x as u32).wrapping_mul(0x9e37_79b9)
                        ^ (source_y as u32).wrapping_mul(0x85eb_ca6b);
                    hash ^= hash >> 16;
                    hash = hash.wrapping_mul(0x7feb_352d);
                    hash ^= hash >> 15;
                    luma[(y as u32 * width + x as u32) as usize] =
                        0.1 + (hash & 0xffff) as f32 / 81_918.0;
                }
            }
        }
        TrackingFrame::new(width, height, luma).expect("test frame")
    }

    #[test]
    fn object_tracker_recovers_translation_with_quality_evidence() {
        let reference = translated_texture(160, 120, 0, 0);
        let candidate = translated_texture(160, 120, 5, -3);
        let observation = track_frame_pair(
            &reference,
            &candidate,
            TrackingRegion {
                min: Vec2::new(0.15, 0.15),
                max: Vec2::new(0.85, 0.85),
            },
            MaskTrackingModel::ObjectTranslation,
            MaskTrackingSettings { search_radius: 10, ..Default::default() },
            || false,
        )
        .expect("translation observation");
        let point = observation
            .transform
            .transform_point(Vec2::new(0.5, 0.5))
            .expect("transformed point");
        assert!(
            (point.x - (0.5 + 5.0 / 160.0)).abs() < 0.01,
            "point={point:?} quality={:?}",
            observation.quality
        );
        assert!(
            (point.y - (0.5 - 3.0 / 120.0)).abs() < 0.01,
            "point={point:?} quality={:?}",
            observation.quality
        );
        assert!(observation.quality.inlier_ratio >= 0.8);
    }

    #[test]
    fn planar_solver_recovers_projective_transform() {
        let expected = TrackingTransform {
            matrix: [1.02, 0.03, 0.04, -0.02, 0.98, 0.03, 0.05, -0.04, 1.0],
        };
        let matches = (0..5)
            .flat_map(|y| {
                (0..6).map(move |x| {
                    let from = [0.12 + x as f64 * 0.13, 0.14 + y as f64 * 0.15];
                    let to = expected
                        .transform_point(Vec2::new(from[0] as f32, from[1] as f32))
                        .expect("expected transform");
                    Match { from, to: [f64::from(to.x), f64::from(to.y)] }
                })
            })
            .collect::<Vec<_>>();
        let actual = solve_homography(&matches).expect("homography");
        for point in [Vec2::new(0.2, 0.3), Vec2::new(0.8, 0.7)] {
            let expected_point = expected.transform_point(point).expect("expected point");
            let actual_point = actual.transform_point(point).expect("actual point");
            assert!((expected_point - actual_point).length() < 1.0e-5);
        }
    }

    #[test]
    fn planar_shape_conversion_preserves_bezier_handles_under_projective_mapping() {
        let ellipse = MaskShape::Ellipse {
            center: Vec2::splat(0.5),
            radii: Vec2::new(0.25, 0.15),
        };
        let transform = TrackingTransform {
            matrix: [1.0, 0.1, 0.02, 0.03, 0.95, -0.01, 0.04, 0.02, 1.0],
        };
        let transformed =
            transform_tracking_shape(&ellipse, transform, MaskTrackingModel::PlanarHomography)
                .expect("shape transform");
        let MaskShape::Path { points, closed } = transformed else {
            panic!("planar ellipse must become Path");
        };
        assert!(closed);
        assert_eq!(points.len(), 4);
        assert!(points.iter().all(|point| point.position.is_finite()));
        assert!(points.iter().any(|point| point.control_in.length() > 0.01));
    }

    #[test]
    fn cancellation_stops_before_analysis() {
        let frame = translated_texture(64, 64, 0, 0);
        assert_eq!(
            track_frame_pair(
                &frame,
                &frame,
                TrackingRegion { min: Vec2::splat(0.1), max: Vec2::splat(0.9) },
                MaskTrackingModel::ObjectTranslation,
                MaskTrackingSettings::default(),
                || true,
            ),
            Err(TrackingError::Canceled)
        );
    }
}
