//! Camera translation analysis and exact-time stabilization paths.

use glam::Vec2;
use mondrian_core::{
    mask_data::MaskTrackingSettings, AssetId, MediaFileFingerprint, SourceSampleTarget,
    TimelineTime,
};
use serde::{Deserialize, Serialize};

use crate::tracking::{
    track_stabilization_translation, TrackingError, TrackingFrame, TrackingQuality, TrackingRegion,
};
use crate::EffectNode;

/// Evidence for the connection between a decoded frame and its predecessor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StabilizationLink {
    /// The first analyzed image anchors a path.
    FirstFrame,
    /// The host supplied a shot boundary at this exact author time.
    ShotBoundary,
    /// The consecutive analysis images were exactly equal.
    IdenticalFrame,
    /// A whole-picture translation was measured with these quality metrics.
    Measured(TrackingQuality),
    /// There were too few textured, repeatable features to connect the images.
    InsufficientTexture,
    /// Matching or fitting could not establish an admissible common motion.
    UnreliableMotion,
}

impl StabilizationLink {
    fn starts_segment(self) -> bool {
        matches!(
            self,
            Self::FirstFrame
                | Self::ShotBoundary
                | Self::InsufficientTexture
                | Self::UnreliableMotion
        )
    }
}

/// One camera position relative to its segment's reference image.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StabilizationSample {
    /// Exact visual Clip author time of the decoded frame.
    pub time: TimelineTime,
    /// Camera translation in normalized complete-raster coordinates.
    pub translation: [f64; 2],
    /// Evidence at this timestamp, including explicit analysis breaks.
    pub link: StabilizationLink,
}

/// Failure to construct or evaluate a stabilization path.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum StabilizationError {
    /// Analysis was interrupted by its owner.
    #[error("stabilization analysis was canceled")]
    Canceled,
    /// Frames and coverage must describe an increasing half-open author-time range.
    #[error("stabilization times must increase within a nonempty analysis range")]
    InvalidTimeRange,
    /// Time is outside the range analyzed by the host.
    #[error("stabilization was not analyzed at the requested author time")]
    OutsideAnalysis,
    /// Authored smoothing or framing values are invalid.
    #[error("invalid stabilization smoothing or framing control")]
    InvalidControls,
    /// Captured observations violated their segment or quality invariants.
    #[error("invalid stabilization analysis observations")]
    InvalidAnalysis,
    /// The persisted analyzer revision is unsupported.
    #[error("unsupported stabilization analysis revision {0}")]
    UnsupportedAnalysisRevision(u32),
    /// The stored analysis no longer describes this media source or time mapping.
    #[error("stabilization analysis source or sample mapping changed")]
    StaleSource,
    /// An input or tracking invariant failed.
    #[error(transparent)]
    Tracking(#[from] TrackingError),
}

/// Streams decoded time-ordered frames while retaining only one analysis image.
#[derive(Debug)]
pub struct StabilizationAnalyzer {
    tracking: MaskTrackingSettings,
    previous_frame: Option<TrackingFrame>,
    samples: Vec<StabilizationSample>,
}

impl StabilizationAnalyzer {
    /// Start an empty session with the shared bounded feature-matching settings.
    pub fn new(tracking: MaskTrackingSettings) -> Result<Self, StabilizationError> {
        tracking
            .validate()
            .map_err(|error| TrackingError::InvalidSettings(error.to_string()))?;
        Ok(Self {
            tracking,
            previous_frame: None,
            samples: Vec::new(),
        })
    }

    /// Analyze one decoded image. `shot_boundary` prevents correspondence across a known edit.
    pub fn push_frame(
        &mut self,
        time: TimelineTime,
        frame: TrackingFrame,
        shot_boundary: bool,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<StabilizationSample, StabilizationError> {
        if canceled() {
            return Err(StabilizationError::Canceled);
        }
        if self.samples.last().is_some_and(|sample| time <= sample.time) {
            return Err(StabilizationError::InvalidTimeRange);
        }
        let mut translation = [0.0; 2];
        let link = match (&self.previous_frame, self.samples.last()) {
            (Some(previous), Some(sample)) if !shot_boundary => {
                if previous == &frame {
                    translation = sample.translation;
                    StabilizationLink::IdenticalFrame
                } else {
                    match track_stabilization_translation(
                        previous,
                        &frame,
                        TrackingRegion { min: Vec2::ZERO, max: Vec2::ONE },
                        self.tracking,
                        canceled,
                    ) {
                        Ok(observation) => {
                            let matrix = observation.transform.matrix();
                            translation = [
                                sample.translation[0] + matrix[2],
                                sample.translation[1] + matrix[5],
                            ];
                            StabilizationLink::Measured(observation.quality)
                        }
                        Err(TrackingError::InsufficientTexture) => {
                            StabilizationLink::InsufficientTexture
                        }
                        Err(
                            TrackingError::InsufficientMatches
                            | TrackingError::LowConfidence
                            | TrackingError::DegenerateTransform,
                        ) => StabilizationLink::UnreliableMotion,
                        Err(TrackingError::Canceled) => return Err(StabilizationError::Canceled),
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            (Some(_), _) => StabilizationLink::ShotBoundary,
            (None, _) => StabilizationLink::FirstFrame,
        };
        let sample = StabilizationSample { time, translation, link };
        self.samples.push(sample);
        self.previous_frame = Some(frame);
        Ok(sample)
    }

    /// Freeze the observations through an exclusive host-provided coverage endpoint.
    pub fn finish(self, end: TimelineTime) -> Result<StabilizationAnalysis, StabilizationError> {
        StabilizationAnalysis::from_samples(self.samples.into_boxed_slice(), end)
    }
}

/// Immutable camera observations, independent of the user's smoothing and framing settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StabilizationAnalysisWire")]
pub struct StabilizationAnalysis {
    samples: Box<[StabilizationSample]>,
    end: TimelineTime,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StabilizationAnalysisWire {
    samples: Box<[StabilizationSample]>,
    end: TimelineTime,
}

impl TryFrom<StabilizationAnalysisWire> for StabilizationAnalysis {
    type Error = StabilizationError;

    fn try_from(wire: StabilizationAnalysisWire) -> Result<Self, Self::Error> {
        Self::from_samples(wire.samples, wire.end)
    }
}

/// Media evidence against which a saved stabilization analysis is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StabilizationSource {
    /// Clip source Asset.
    pub asset: AssetId,
    /// Complete revision of the opened source media.
    pub media: MediaFileFingerprint,
    /// Explicit video stream used for analysis.
    pub video_stream: u32,
}

const ANALYSIS_REVISION: u32 = 1;
const EFFECT_RECORD_KEY: &str = "stabilization_analysis";

/// A serializable analysis tied to the exact decoded sources of the observations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StabilizationRecordWire")]
pub struct StabilizationRecord {
    algorithm_revision: u32,
    source: StabilizationSource,
    settings: MaskTrackingSettings,
    targets: Box<[SourceSampleTarget]>,
    analysis: StabilizationAnalysis,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StabilizationRecordWire {
    algorithm_revision: u32,
    source: StabilizationSource,
    settings: MaskTrackingSettings,
    targets: Box<[SourceSampleTarget]>,
    analysis: StabilizationAnalysis,
}

impl TryFrom<StabilizationRecordWire> for StabilizationRecord {
    type Error = StabilizationError;

    fn try_from(wire: StabilizationRecordWire) -> Result<Self, Self::Error> {
        if wire.algorithm_revision != ANALYSIS_REVISION {
            return Err(StabilizationError::UnsupportedAnalysisRevision(
                wire.algorithm_revision,
            ));
        }
        Self::new(wire.source, wire.settings, wire.targets, wire.analysis)
    }
}

impl StabilizationRecord {
    /// Whether this effect carries a saved record. Decoding verifies its validity.
    pub fn has_saved_analysis(effect: &EffectNode) -> bool {
        effect.params.get(EFFECT_RECORD_KEY).is_some()
    }

    /// Read and validate the analysis published in an effect's authoring data.
    pub fn from_effect(effect: &EffectNode) -> Result<Option<Self>, serde_json::Error> {
        effect
            .params
            .get(EFFECT_RECORD_KEY)
            .cloned()
            .map(serde_json::from_value)
            .transpose()
    }

    /// Publish an analysis through the effect's persistent authoring data.
    pub fn store_in_effect(&self, effect: &mut EffectNode) -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(self)?;
        if let Some(params) = effect.params.as_object_mut() {
            params.insert(EFFECT_RECORD_KEY.to_owned(), value);
        } else {
            effect.params = serde_json::json!({ (EFFECT_RECORD_KEY): value });
        }
        Ok(())
    }

    /// Media identity that the host must verify before executing this analysis.
    pub const fn source(&self) -> StabilizationSource {
        self.source
    }

    /// The exact tracker settings frozen with the observations.
    pub const fn settings(&self) -> MaskTrackingSettings {
        self.settings
    }

    pub(crate) const fn analysis(&self) -> &StabilizationAnalysis {
        &self.analysis
    }

    /// Bind one explicit source request to each exact analysis time.
    pub fn new(
        source: StabilizationSource,
        settings: MaskTrackingSettings,
        targets: Box<[SourceSampleTarget]>,
        analysis: StabilizationAnalysis,
    ) -> Result<Self, StabilizationError> {
        if !source.media.authorizes_reuse() {
            return Err(StabilizationError::StaleSource);
        }
        settings
            .validate()
            .map_err(|error| TrackingError::InvalidSettings(error.to_string()))?;
        if targets.len() != analysis.samples.len() {
            return Err(StabilizationError::InvalidAnalysis);
        }
        Ok(Self {
            algorithm_revision: ANALYSIS_REVISION,
            source,
            settings,
            targets,
            analysis,
        })
    }

    /// Admit observations only if media, analysis settings and exact sample mapping agree.
    pub fn analysis_for_source(
        &self,
        source: StabilizationSource,
        settings: MaskTrackingSettings,
        mut resolve: impl FnMut(TimelineTime) -> Option<SourceSampleTarget>,
    ) -> Result<&StabilizationAnalysis, StabilizationError> {
        if source != self.source
            || settings != self.settings
            || self
                .analysis
                .samples
                .iter()
                .zip(&self.targets)
                .any(|(sample, &target)| resolve(sample.time) != Some(target))
        {
            return Err(StabilizationError::StaleSource);
        }
        Ok(&self.analysis)
    }
}

/// Target camera motion inside a connected segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StabilizationMotion {
    /// Fit a local straight path in time inside this radius, in seconds.
    Smooth { radius_seconds: f64 },
    /// Stabilize to the first camera position in each segment.
    Lock,
}

/// How the corrected frame is placed within the complete raster.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StabilizationFraming {
    /// Retain the full output extent and leave uncovered areas transparent.
    ExposeEdges,
    /// Apply a constant zoom per connected segment, limited to this scale.
    AutoZoom { max_scale: f64 },
}

/// One extent-independent inverse mapping to the source raster.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StabilizedView {
    /// Translation added to the inverse sample coordinate after center-based scaling.
    pub source_offset: [f64; 2],
    /// Magnification about the output center. Zero produces transparent output.
    pub zoom: f64,
}

impl StabilizedView {
    /// Evaluate an inverse mapping in complete-frame edge-normalized coordinates.
    pub fn source_position(self, output: [f64; 2]) -> Option<[f64; 2]> {
        (self.zoom > 0.0).then(|| {
            std::array::from_fn(|axis| {
                0.5 + (output[axis] - 0.5) / self.zoom + self.source_offset[axis]
            })
        })
    }

    /// Whether the rectangle extends beyond the source even after framing.
    pub fn exposes_edges(self) -> bool {
        if self.zoom == 0.0 {
            return true;
        }
        let half_width = 0.5 / self.zoom;
        self.source_offset
            .iter()
            .any(|offset| offset.abs() + half_width > 0.5 + f64::EPSILON)
    }
}

/// Float32 inverse warp shared by the complete-frame CPU and device samplers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StabilizationWarp {
    inverse_zoom: f32,
    offset: [f32; 2],
    source: Option<StabilizationSource>,
}

impl StabilizationWarp {
    /// Convert a stabilized view to the precision used on both execution backends.
    pub fn new(view: StabilizedView) -> Result<Self, StabilizationError> {
        if view.zoom == 0.0 {
            return Ok(Self { inverse_zoom: 0.0, offset: [0.0; 2], source: None });
        }
        let inverse_zoom = view.zoom.recip() as f32;
        let offset = view.source_offset.map(|value| value as f32);
        if inverse_zoom <= 0.0
            || !inverse_zoom.is_finite()
            || !offset.iter().all(|value| value.is_finite())
        {
            return Err(StabilizationError::InvalidControls);
        }
        Ok(Self { inverse_zoom, offset, source: None })
    }

    pub(crate) fn with_source(mut self, source: StabilizationSource) -> Self {
        self.source = Some(source);
        self
    }

    pub(crate) const fn source(self) -> Option<StabilizationSource> {
        self.source
    }

    /// Whether every output center samples the corresponding source texel.
    pub fn is_identity(self) -> bool {
        self.inverse_zoom == 1.0 && self.offset == [0.0; 2]
    }

    /// Inverse zoom and normalized source offset for the device pass.
    pub const fn gpu_controls(self) -> [f32; 4] {
        [self.inverse_zoom, self.offset[0], self.offset[1], 0.0]
    }
}

/// Warp a straight RGBA Float32 frame, filtering color and alpha together.
pub fn stabilize_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    warp: StabilizationWarp,
) -> bool {
    apply_stabilization_f32_controlled(pixels, width, height, warp, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

pub(crate) fn apply_stabilization_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    warp: StabilizationWarp,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(pixels.len()) {
        return Ok(false);
    }
    if warp.is_identity() {
        return Ok(true);
    }
    checkpoint()?;
    if warp.inverse_zoom == 0.0 {
        for row in pixels.chunks_exact_mut(width) {
            checkpoint()?;
            row.fill([0.0; 4]);
        }
        return Ok(true);
    }
    let mut source = pixels.to_vec();
    crate::straight_alpha::premultiply_rgba_controlled(&mut source, checkpoint)?;
    let size = [width as f32, height as f32];
    let center = size.map(|value| value * 0.5);
    let offset = std::array::from_fn::<_, 2, _>(|axis| warp.offset[axis] * size[axis]);
    let edge_epsilon = size.map(|value| value * 1.0e-6);
    let map = |pixel_center: f32, axis: usize| {
        (pixel_center - center[axis]) * warp.inverse_zoom + center[axis] + offset[axis]
    };
    for (row, output) in pixels.chunks_exact_mut(width).enumerate() {
        checkpoint()?;
        let y = map(row as f32 + 0.5, 1);
        if !(-edge_epsilon[1]..=size[1] + edge_epsilon[1]).contains(&y) {
            output.fill([0.0; 4]);
            continue;
        }
        for (col, pixel) in output.iter_mut().enumerate() {
            let x = map(col as f32 + 0.5, 0);
            *pixel = if (-edge_epsilon[0]..=size[0] + edge_epsilon[0]).contains(&x) {
                crate::coverage::straight_rgba_from_premultiplied(
                    crate::resample::sample_bilinear_clamped(
                        &source,
                        width,
                        height,
                        f64::from(x - 0.5),
                        f64::from(y - 0.5),
                    ),
                )
            } else {
                [0.0; 4]
            };
        }
    }
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PreparedSample {
    time: TimelineTime,
    view: StabilizedView,
    starts_segment: bool,
}

/// Frozen inverse maps shared by preview and export for one analysis and control snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct StabilizationPath {
    samples: Box<[PreparedSample]>,
    end: TimelineTime,
}

impl StabilizationAnalysis {
    fn from_samples(
        samples: Box<[StabilizationSample]>,
        end: TimelineTime,
    ) -> Result<Self, StabilizationError> {
        if samples.last().is_none_or(|sample| end <= sample.time)
            || samples.windows(2).any(|pair| pair[0].time >= pair[1].time)
        {
            return Err(StabilizationError::InvalidTimeRange);
        }
        if samples[0].link != StabilizationLink::FirstFrame {
            return Err(StabilizationError::InvalidAnalysis);
        }
        for (index, sample) in samples.iter().enumerate() {
            if !sample.translation.iter().all(|value| value.is_finite())
                || (sample.link.starts_segment() && sample.translation != [0.0; 2])
                || (index > 0 && sample.link == StabilizationLink::FirstFrame)
                || (sample.link == StabilizationLink::IdenticalFrame
                    && sample.translation != samples[index - 1].translation)
            {
                return Err(StabilizationError::InvalidAnalysis);
            }
            if let StabilizationLink::Measured(quality) = sample.link
                && (quality.inlier_features == 0
                    || quality.inlier_features > quality.matched_features
                    || !(0.0..=1.0).contains(&quality.inlier_ratio)
                    || !(0.0..=1.0).contains(&quality.feature_coverage)
                    || !quality.rms_error_px.is_finite()
                    || quality.rms_error_px < 0.0)
            {
                return Err(StabilizationError::InvalidAnalysis);
            }
        }
        Ok(Self { samples, end })
    }

    /// Recorded samples in strictly increasing Clip author time.
    pub fn samples(&self) -> &[StabilizationSample] {
        &self.samples
    }

    /// Start and exclusive end of the analyzed visual author-time range.
    pub fn analysis_interval(&self) -> (TimelineTime, TimelineTime) {
        (self.samples[0].time, self.end)
    }

    /// Smooth each segment in physical time and solve a constant zoom over it.
    pub fn prepare_path(
        &self,
        motion: StabilizationMotion,
        framing: StabilizationFraming,
        additional_scale: f64,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<StabilizationPath, StabilizationError> {
        if !additional_scale.is_finite()
            || additional_scale < 0.0
            || matches!(motion, StabilizationMotion::Smooth { radius_seconds } if !radius_seconds.is_finite() || radius_seconds < 0.0)
            || matches!(framing, StabilizationFraming::AutoZoom { max_scale } if !max_scale.is_finite() || max_scale < 1.0)
        {
            return Err(StabilizationError::InvalidControls);
        }

        let mut samples = Vec::with_capacity(self.samples.len());
        let starts = self
            .samples
            .iter()
            .enumerate()
            .filter_map(|(index, sample)| sample.link.starts_segment().then_some(index));
        let ends = starts.clone().skip(1).chain(std::iter::once(self.samples.len()));
        for (start, end) in starts.zip(ends) {
            let segment = &self.samples[start..end];
            let mut largest_offset = 0.0_f64;
            for (index, sample) in segment.iter().enumerate() {
                if canceled() {
                    return Err(StabilizationError::Canceled);
                }
                let target = match motion {
                    StabilizationMotion::Lock => segment[0].translation,
                    StabilizationMotion::Smooth { radius_seconds } => {
                        local_line(segment, index, radius_seconds)
                    }
                };
                let source_offset = std::array::from_fn(|axis| {
                    let offset = sample.translation[axis] - target[axis];
                    if offset.abs() <= 1.0e-12 {
                        0.0
                    } else {
                        offset
                    }
                });
                largest_offset =
                    largest_offset.max(source_offset[0].abs()).max(source_offset[1].abs());
                samples.push(PreparedSample {
                    time: sample.time,
                    view: StabilizedView { source_offset, zoom: 1.0 },
                    starts_segment: index == 0,
                });
            }
            let zoom = match framing {
                StabilizationFraming::ExposeEdges => 1.0,
                StabilizationFraming::AutoZoom { max_scale } => {
                    let available = 1.0 - 2.0 * largest_offset;
                    if available > 0.0 {
                        available.recip().min(max_scale)
                    } else {
                        max_scale
                    }
                }
            } * additional_scale;
            if !zoom.is_finite() || zoom < 0.0 || (zoom > 0.0 && !zoom.recip().is_finite()) {
                return Err(StabilizationError::InvalidControls);
            }
            for sample in &mut samples[start..end] {
                sample.view.zoom = zoom;
            }
        }
        Ok(StabilizationPath { samples: samples.into_boxed_slice(), end: self.end })
    }
}

impl StabilizationPath {
    pub(crate) fn retained_bytes_estimate(&self) -> usize {
        std::mem::size_of_val(self.samples.as_ref())
    }

    /// Evaluate inside the half-open analyzed interval, with no interpolation across cuts.
    pub fn view_at(&self, time: TimelineTime) -> Result<StabilizedView, StabilizationError> {
        if time < self.samples[0].time || time >= self.end {
            return Err(StabilizationError::OutsideAnalysis);
        }
        let right = self.samples.partition_point(|sample| sample.time <= time);
        let left = self.samples[right - 1];
        let Some(right) = self.samples.get(right).filter(|next| !next.starts_segment) else {
            return Ok(left.view);
        };
        let phase = elapsed_seconds(time, left.time) / elapsed_seconds(right.time, left.time);
        let source_offset = std::array::from_fn(|axis| {
            left.view.source_offset[axis]
                + phase * (right.view.source_offset[axis] - left.view.source_offset[axis])
        });
        Ok(StabilizedView { source_offset, zoom: left.view.zoom })
    }
}

fn local_line(samples: &[StabilizationSample], index: usize, radius: f64) -> [f64; 2] {
    let center = samples[index];
    if radius == 0.0 {
        return center.translation;
    }
    let start = samples[..index]
        .partition_point(|sample| elapsed_seconds(center.time, sample.time) >= radius);
    let end = index
        + samples[index..]
            .partition_point(|sample| elapsed_seconds(sample.time, center.time) < radius);
    let scale = elapsed_seconds(center.time, samples[start].time)
        .max(elapsed_seconds(samples[end - 1].time, center.time));
    if scale == 0.0 {
        return center.translation;
    }
    let (mut sw, mut sx, mut sxx) = (0.0_f64, 0.0_f64, 0.0_f64);
    let (mut sy, mut sxy) = ([0.0; 2], [0.0; 2]);
    for sample in &samples[start..end] {
        let elapsed = elapsed_seconds(sample.time, center.time);
        let x = elapsed / scale;
        let weight = 1.0 - elapsed.abs() / radius;
        sw += weight;
        sx += weight * x;
        sxx += weight * x * x;
        for axis in 0..2 {
            let delta = sample.translation[axis] - center.translation[axis];
            sy[axis] += weight * delta;
            sxy[axis] += weight * x * delta;
        }
    }
    let determinant = sw * sxx - sx * sx;
    if determinant <= f64::EPSILON {
        return center.translation;
    }
    std::array::from_fn(|axis| {
        center.translation[axis] + (sxx * sy[axis] - sx * sxy[axis]) / determinant
    })
}

fn elapsed_seconds(later: TimelineTime, earlier: TimelineTime) -> f64 {
    let n = i128::from(later.numerator()) * i128::from(earlier.denominator())
        - i128::from(earlier.numerator()) * i128::from(later.denominator());
    n as f64 / (later.denominator() as f64 * earlier.denominator() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_stabilizer_reopens_and_builds_a_trimmed_clip_graph_with_source_evidence() {
        use crate::{
            instantiate_effect_node, EffectGraphBuildError, EffectType, PreparedEffectProgram,
        };
        use mondrian_core::{MediaFileChangeStamp, MediaFileObjectIdentity, WorkingColorSpace};

        let mut effect = instantiate_effect_node(EffectType::Stabilizer).unwrap();
        let source = StabilizationSource {
            asset: AssetId::new(),
            video_stream: 2,
            media: MediaFileFingerprint {
                len: Some(201),
                modified_secs: Some(1),
                modified_nanos: Some(2),
                object_identity: Some(MediaFileObjectIdentity::Unix { device: 1, inode: 2 }),
                change_stamp: Some(MediaFileChangeStamp::Unix { seconds: 4, nanoseconds: 0 }),
            },
        };
        let color = WorkingColorSpace::default();
        assert!(matches!(
            PreparedEffectProgram::prepare(std::slice::from_ref(&effect), &[], color),
            Err(EffectGraphBuildError::ResourceUnavailable { .. })
        ));
        let pending = PreparedEffectProgram::prepare_for_auxiliary_preview(
            std::slice::from_ref(&effect),
            &[],
            color,
        )
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap();
        assert_eq!(pending.stabilization_sources().count(), 0);
        let clip_in = time(1, 2);
        let sample = time(2, 3);
        let clip_out = time(3, 4);
        let record = StabilizationRecord::new(
            source,
            MaskTrackingSettings::default(),
            [clip_in, sample].into_iter().map(SourceSampleTarget::covering).collect(),
            freeze(
                vec![observed(clip_in, 0.0, 0.0), observed(sample, 0.1, -0.1)],
                clip_out,
            ),
        )
        .unwrap();
        record.store_in_effect(&mut effect).unwrap();
        let reopened =
            serde_json::from_slice::<EffectNode>(&serde_json::to_vec(&effect).unwrap()).unwrap();
        let program = PreparedEffectProgram::prepare(&[reopened], &[], color).unwrap();
        let graph = program.evaluate(sample).unwrap();
        assert_eq!(graph.stabilization_sources().collect::<Vec<_>>(), [source]);
        assert!(program.evaluate(clip_out).is_err());
    }

    #[test]
    fn persisted_analysis_rejects_stale_source_samples_and_corrupt_segments() {
        use mondrian_core::{MediaFileChangeStamp, MediaFileObjectIdentity};
        let source = StabilizationSource {
            asset: AssetId::new(),
            video_stream: 1,
            media: MediaFileFingerprint {
                len: Some(514),
                modified_secs: Some(31),
                modified_nanos: Some(3),
                object_identity: Some(MediaFileObjectIdentity::Unix { device: 1, inode: 2 }),
                change_stamp: Some(MediaFileChangeStamp::Unix { seconds: 42, nanoseconds: 0 }),
            },
        };
        let settings = MaskTrackingSettings::default();
        let times = [time(1, 3), time(11, 30)];
        let mapped = |at: TimelineTime| {
            Some(SourceSampleTarget::covering(
                at.checked_add(TimelineTime::ONE).unwrap(),
            ))
        };
        let targets = times.iter().map(|&at| mapped(at).unwrap()).collect();
        let analysis = freeze(
            vec![
                observed(times[0], 0.0, 0.0),
                observed(times[1], 0.03125, -0.125),
            ],
            time(2, 5),
        );
        let record = StabilizationRecord::new(source, settings, targets, analysis.clone()).unwrap();
        let payload = serde_json::to_value(record).unwrap();
        let reopened: StabilizationRecord = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(
            reopened.analysis_for_source(source, settings, mapped),
            Ok(&analysis)
        );
        assert_eq!(
            reopened.analysis_for_source(
                StabilizationSource { video_stream: 0, ..source },
                settings,
                mapped
            ),
            Err(StabilizationError::StaleSource)
        );
        assert_eq!(
            reopened.analysis_for_source(source, settings, |at| Some(
                SourceSampleTarget::covering(at)
            )),
            Err(StabilizationError::StaleSource)
        );
        assert_eq!(
            reopened.analysis_for_source(
                source,
                MaskTrackingSettings { search_radius: 12, ..settings },
                mapped
            ),
            Err(StabilizationError::StaleSource)
        );
        let mut old_revision = payload.clone();
        old_revision["algorithm_revision"] = serde_json::json!(0);
        assert!(serde_json::from_value::<StabilizationRecord>(old_revision).is_err());
        let mut invalid = payload;
        invalid["analysis"]["samples"][1]["time"] = serde_json::to_value(times[0]).unwrap();
        assert!(serde_json::from_value::<StabilizationRecord>(invalid).is_err());
    }

    #[test]
    fn stabilized_graph_uses_real_raster_extent_and_preserves_coverage_in_bilinear_taps() {
        use crate::{
            apply_compiled_effect_graph_rgba_f32, compile_reference_render_graph,
            lower_effect_graph_to_gpu_pipeline, EffectColorDomain, EffectFloatFrameView,
            EffectFrameExtent, EffectGpuSpatialPass, EffectGpuStage, EffectGraphBuilderState,
            EffectRenderOp, EffectValueFormat, EffectWorkingPrecision,
        };
        use mondrian_core::{ColorFrameAlpha, ImageSampling};

        let warp =
            StabilizationWarp::new(StabilizedView { source_offset: [0.125, 0.0], zoom: 1.0 })
                .unwrap();
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::StabilizationWarp { warp });
        let graph = compile_reference_render_graph(builder.finish()).unwrap();
        let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
        assert!(matches!(pipeline.stages(), [EffectGpuStage::Spatial {
            pass: EffectGpuSpatialPass::StabilizationWarp { warp: lowered }, ..
        }] if *lowered == warp));
        let input = [
            [1.5, -0.2, 0.4, 1.0],
            [200.0, 1.0, 0.0, 0.0],
            [0.25, 2.0, 0.5, 1.0],
            [0.0, 0.0, 4.0, 1.0],
        ];
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        );
        let view = EffectFloatFrameView::new(
            EffectFrameExtent::new(4, 1),
            format,
            ImageSampling::UNIT_SQUARE,
            &input,
        );
        let output = apply_compiled_effect_graph_rgba_f32(view, &graph, 0).unwrap();
        assert_eq!(output.pixels()[0], [1.5, -0.2, 0.4, 0.5]);
        assert_eq!(output.pixels()[1], [0.25, 2.0, 0.5, 0.5]);
        assert_eq!(output.pixels()[2], [0.125, 1.0, 2.25, 1.0]);
        assert_eq!(output.pixels()[3], [0.0, 0.0, 4.0, 1.0]);
    }

    fn time(numerator: i64, denominator: i64) -> TimelineTime {
        TimelineTime::new(numerator, denominator).unwrap()
    }

    fn observed(time: TimelineTime, tx: f64, ty: f64) -> StabilizationSample {
        StabilizationSample {
            time,
            translation: [tx, ty],
            link: StabilizationLink::Measured(TrackingQuality {
                matched_features: 45,
                inlier_features: 45,
                inlier_ratio: 1.0,
                rms_error_px: 0.0,
                feature_coverage: 0.8,
            }),
        }
    }

    fn freeze(mut samples: Vec<StabilizationSample>, end: TimelineTime) -> StabilizationAnalysis {
        samples[0].link = StabilizationLink::FirstFrame;
        StabilizationAnalysis { samples: samples.into_boxed_slice(), end }
    }

    #[test]
    fn local_fit_preserves_irregular_constant_velocity_including_the_endpoints() {
        let times = [0, 3, 4, 8, 9, 11, 16];
        let samples = times
            .iter()
            .map(|&t| observed(time(t, 24), t as f64 * 0.007, t as f64 * -0.002))
            .collect();
        let path = freeze(samples, time(17, 24))
            .prepare_path(
                StabilizationMotion::Smooth { radius_seconds: 1.0 },
                StabilizationFraming::ExposeEdges,
                1.0,
                || false,
            )
            .unwrap();
        for t in [16, 8, 1, 4, 0, 16] {
            let view = path.view_at(time(t, 24)).unwrap();
            assert_eq!(view.source_offset, [0.0; 2]);
            assert_eq!(view.zoom, 1.0);
        }
    }

    #[test]
    fn local_fit_attenuates_alternating_image_jitter_without_removing_camera_drift() {
        let samples = (0..=60)
            .map(|i| {
                let jitter = if i % 2 == 0 { 0.02 } else { -0.02 };
                observed(time(i, 30), i as f64 * 0.002 + jitter, 0.0)
            })
            .collect();
        let analysis = freeze(samples, time(61, 30));
        let path = analysis
            .prepare_path(
                StabilizationMotion::Smooth { radius_seconds: 0.5 },
                StabilizationFraming::ExposeEdges,
                1.0,
                || false,
            )
            .unwrap();
        for (i, sample) in analysis.samples().iter().enumerate().take(46).skip(15) {
            let stabilized =
                sample.translation[0] - path.view_at(sample.time).unwrap().source_offset[0];
            assert!(
                (stabilized - i as f64 * 0.002).abs() < 0.002,
                "frame {i}: {stabilized}"
            );
        }
        let disabled = analysis
            .prepare_path(
                StabilizationMotion::Smooth { radius_seconds: 0.0 },
                StabilizationFraming::ExposeEdges,
                1.0,
                || false,
            )
            .unwrap();
        assert_eq!(
            disabled.view_at(time(30, 30)).unwrap().source_offset,
            [0.0; 2]
        );
    }

    #[test]
    fn locking_breaks_at_the_exact_cut_and_frames_each_segment_without_zoom_pumping() {
        let samples = vec![
            observed(time(0, 1), 0.0, 0.0),
            observed(time(1, 1), 0.10, -0.05),
            StabilizationSample {
                time: time(2, 1),
                translation: [0.0; 2],
                link: StabilizationLink::ShotBoundary,
            },
            observed(time(3, 1), -0.04, 0.0),
        ];
        let analysis = freeze(samples, time(4, 1));
        let path = analysis
            .prepare_path(
                StabilizationMotion::Lock,
                StabilizationFraming::AutoZoom { max_scale: 1.5 },
                1.0,
                || false,
            )
            .unwrap();
        let middle = path.view_at(time(1, 2)).unwrap();
        let before = path.view_at(time(1999, 1000)).unwrap();
        let after = path.view_at(time(2, 1)).unwrap();
        assert_eq!(
            middle,
            StabilizedView { source_offset: [0.05, -0.025], zoom: 1.25 }
        );
        assert_eq!(before.source_offset, [0.10, -0.05]);
        assert_eq!(before.zoom, path.view_at(TimelineTime::ZERO).unwrap().zoom);
        assert_eq!(after.source_offset, [0.0; 2]);
        assert!(after.zoom < before.zoom);
        assert!(!before.exposes_edges());
        assert_eq!(
            path.view_at(time(4, 1)),
            Err(StabilizationError::OutsideAnalysis)
        );
        assert_eq!(
            path.view_at(time(-1, 1000)),
            Err(StabilizationError::OutsideAnalysis)
        );
        let limited = analysis
            .prepare_path(
                StabilizationMotion::Lock,
                StabilizationFraming::AutoZoom { max_scale: 1.1 },
                1.0,
                || false,
            )
            .unwrap();
        assert!(limited.view_at(time(1, 1)).unwrap().exposes_edges());
        let collapsed = analysis
            .prepare_path(
                StabilizationMotion::Lock,
                StabilizationFraming::AutoZoom { max_scale: 1.5 },
                0.0,
                || false,
            )
            .unwrap()
            .view_at(time(1, 1))
            .unwrap();
        assert_eq!(collapsed.source_position([0.5; 2]), None);
        let mut pixels = [[1.5, -0.2, 0.4, 0.8]; 4];
        assert!(stabilize_frame_f32(
            &mut pixels,
            2,
            2,
            StabilizationWarp::new(collapsed).unwrap()
        ));
        assert_eq!(pixels, [[0.0; 4]; 4]);
    }

    fn texture(width: u32, height: u32, shift: [f64; 2]) -> TrackingFrame {
        let grid_value = |x: i32, y: i32| {
            let bits = (x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b);
            let bits = (bits ^ (bits >> 16)).wrapping_mul(0x7feb_352d);
            f64::from((bits ^ (bits >> 15)) as u16) / 65_535.0
        };
        let grid_sample = |x: f64, y: f64| {
            let (ix, iy) = (x.floor() as i32, y.floor() as i32);
            let (fx, fy) = (x - f64::from(ix), y - f64::from(iy));
            let top = grid_value(ix, iy) * (1.0 - fx) + grid_value(ix + 1, iy) * fx;
            let bottom = grid_value(ix, iy + 1) * (1.0 - fx) + grid_value(ix + 1, iy + 1) * fx;
            top * (1.0 - fy) + bottom * fy
        };
        let luma = (0..width * height)
            .map(|i| {
                let x = f64::from(i % width) - shift[0];
                let y = f64::from(i / width) - shift[1];
                (0.7 * grid_sample(x / 3.5, y / 3.5)
                    + 0.3 * grid_sample((x + 91.3) / 6.7, (y - 27.1) / 6.7)) as f32
            })
            .collect();
        TrackingFrame::new(width, height, luma).unwrap()
    }

    #[test]
    fn decoded_pair_recovers_subpixel_camera_offset_and_lock_uses_the_inverse_sign() {
        let mut analyzer = StabilizationAnalyzer::new(MaskTrackingSettings {
            search_radius: 9,
            max_features: 96,
            ..Default::default()
        })
        .unwrap();
        let (w, h) = (160, 96);
        analyzer
            .push_frame(TimelineTime::ZERO, texture(w, h, [0.0; 2]), false, || false)
            .unwrap();
        let sample = analyzer
            .push_frame(time(1, 30), texture(w, h, [3.35, -2.25]), false, || false)
            .unwrap();
        let StabilizationLink::Measured(quality) = sample.link else {
            panic!("{sample:?}")
        };
        let x = sample.translation[0] * f64::from(w);
        let y = sample.translation[1] * f64::from(h);
        eprintln!(
            "stabilization tracked {x}, {y}, {} inliers, fit {} px",
            quality.inlier_features, quality.rms_error_px
        );
        assert!((x - 3.35).abs() < 0.20);
        assert!((y + 2.25).abs() < 0.20);
        let path = analyzer
            .finish(time(2, 30))
            .unwrap()
            .prepare_path(
                StabilizationMotion::Lock,
                StabilizationFraming::ExposeEdges,
                1.0,
                || false,
            )
            .unwrap();
        let coord = path.view_at(time(1, 30)).unwrap().source_position([0.5; 2]).unwrap();
        assert!((coord[0] - 0.5 - sample.translation[0]).abs() < 1e-13);
        assert!((coord[1] - 0.5 - sample.translation[1]).abs() < 1e-13);
    }

    #[test]
    fn transparent_rgb_is_not_motion_evidence_and_tracking_breaks_remain_distinguishable() {
        let (w, h) = (120, 90);
        let flat = |value| TrackingFrame::new(w, h, vec![value; (w * h) as usize]).unwrap();
        let mut analyzer = StabilizationAnalyzer::new(MaskTrackingSettings::default()).unwrap();
        let first = analyzer.push_frame(time(0, 1), flat(0.4), false, || false).unwrap();
        let repeated = analyzer.push_frame(time(1, 1), flat(0.4), false, || false).unwrap();
        let other = analyzer.push_frame(time(2, 1), flat(0.6), false, || false).unwrap();
        let hidden = (0..w * h).flat_map(|i| [(i % 21) as f32, 0.0, 0.0, 0.0]).collect::<Vec<_>>();
        let hidden = TrackingFrame::from_covered_rgba_f32(w, h, &hidden).unwrap();
        let invisible = analyzer.push_frame(time(3, 1), hidden.clone(), true, || false).unwrap();
        let textured = analyzer
            .push_frame(time(4, 1), texture(w, h, [0.0; 2]), false, || false)
            .unwrap();
        assert_eq!(first.link, StabilizationLink::FirstFrame);
        assert_eq!(repeated.link, StabilizationLink::IdenticalFrame);
        assert_eq!(other.link, StabilizationLink::InsufficientTexture);
        assert_eq!(invisible.link, StabilizationLink::ShotBoundary);
        assert_eq!(textured.link, StabilizationLink::InsufficientTexture);
    }

    #[test]
    fn cancellation_and_invalid_frame_sizes_never_advance_the_session() {
        let mut analyzer = StabilizationAnalyzer::new(MaskTrackingSettings::default()).unwrap();
        let flat = |value| TrackingFrame::new(16, 16, vec![value; 16 * 16]).unwrap();
        let first = time(0, 1);
        let second = time(1, 1);
        analyzer.push_frame(first, flat(0.0), false, || false).unwrap();
        assert_eq!(
            analyzer.push_frame(second, flat(0.0), false, || true),
            Err(StabilizationError::Canceled)
        );
        assert_eq!(
            analyzer.push_frame(second, flat(0.1), false, || false),
            Err(TrackingError::InvalidRegion.into())
        );
        assert_eq!(
            analyzer.push_frame(second, flat(0.0), false, || false).unwrap().link,
            StabilizationLink::IdenticalFrame
        );
        assert_eq!(
            analyzer.finish(second),
            Err(StabilizationError::InvalidTimeRange)
        );
    }
}
