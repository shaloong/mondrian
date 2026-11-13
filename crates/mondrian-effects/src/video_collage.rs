//! Shared geometry, timing and Float32 reference for Video Collage.

use crate::auxiliary::{sample_auxiliary_associated_f32, PIXEL_CENTRE};
use crate::{
    AuxiliaryFloatFrame, AuxiliaryPolicy, AuxiliarySampling, EffectFloatFrameError,
    EffectFloatFrameView, EffectFrameExtent, EffectPixelRoi,
};
use glam::Vec2;
use mondrian_core::{
    effect_data::EffectNode, ImageSampling, ParameterId, PropertyValue, TimelineTime,
    TimelineTimeError, VisualSourceReference,
};
use std::hash::{Hash, Hasher};

pub const COLLAGE_MAX_AXIS: u32 = 8;
const DELAY_TIMESCALE: u32 = 1_000_000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum VideoCollagePlayback {
    #[default]
    Sequential,
    Simultaneous,
    Random,
}

impl VideoCollagePlayback {
    pub const OPTIONS: [(&'static str, &'static str, Self); 3] = [
        ("sequential", "顺序", Self::Sequential),
        ("simultaneous", "同时", Self::Simultaneous),
        ("random", "随机", Self::Random),
    ];

    pub fn from_key(key: &str) -> Option<Self> {
        Self::OPTIONS.into_iter().find_map(|(id, _, mode)| (id == key).then_some(mode))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoCollageGrade {
    columns: u32,
    rows: u32,
    spacing: f32,
    roundness: f32,
    border_width: f32,
    border: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VideoCollageError {
    #[error("invalid Video Collage grid, spacing, roundness, or border")]
    Controls,
    #[error("invalid Video Collage delay")]
    Delay,
    #[error("Video Collage tile sources do not match its grid")]
    Sources,
    #[error("Video Collage raster or physical sampling is not representable")]
    Geometry,
    #[error(transparent)]
    Time(#[from] TimelineTimeError),
}

impl VideoCollageGrade {
    pub fn new(
        columns: u32,
        rows: u32,
        spacing: f32,
        roundness: f32,
        border_width: f32,
        border: [f32; 4],
    ) -> Result<Self, VideoCollageError> {
        if !(1..=COLLAGE_MAX_AXIS).contains(&columns)
            || !(1..=COLLAGE_MAX_AXIS).contains(&rows)
            || !(0.0..=100.0).contains(&spacing)
            || !(0.0..=100.0).contains(&roundness)
            || !(0.0..=20.0).contains(&border_width)
            || !border.iter().all(|channel| channel.is_finite())
            || !(0.0..=1.0).contains(&border[3])
        {
            return Err(VideoCollageError::Controls);
        }
        Ok(Self {
            columns,
            rows,
            spacing,
            roundness,
            border_width,
            border,
        })
    }

    pub const fn tile_count(self) -> usize {
        (self.columns * self.rows) as usize
    }

    pub(crate) fn hash_signature(self, state: &mut impl Hasher) {
        self.columns.hash(state);
        self.rows.hash(state);
        [self.spacing, self.roundness, self.border_width].map(f32::to_bits).hash(state);
        self.border.map(f32::to_bits).hash(state);
    }

    /// Derive disjoint, integer cell rectangles in row-major order.
    pub fn tiles(self, extent: EffectFrameExtent, sampling: ImageSampling) -> Vec<EffectPixelRoi> {
        let pixels = physical_pixel_size(sampling);
        let x = axis_tiles(extent.width(), self.columns, self.spacing / pixels.x);
        let y = axis_tiles(extent.height(), self.rows, self.spacing / pixels.y);
        y.into_iter()
            .flat_map(|(top, height)| {
                x.iter()
                    .map(move |&(left, width)| EffectPixelRoi::new(left, top, width, height))
            })
            .collect()
    }

    /// Compute shared center-crop, rounded border and coverage uniforms.
    pub fn gpu_controls(
        self,
        tile: EffectPixelRoi,
        output_sampling: ImageSampling,
        source_extent: EffectFrameExtent,
        source_sampling: ImageSampling,
    ) -> Result<VideoCollageGpuControls, VideoCollageError> {
        if tile.is_empty() || source_extent.is_empty() {
            return Err(VideoCollageError::Geometry);
        }
        let pixel_size = physical_pixel_size(output_sampling);
        let half = Vec2::new(tile.width() as f32, tile.height() as f32) * pixel_size * 0.5;
        let source = Vec2::new(source_extent.width() as f32, source_extent.height() as f32)
            * physical_pixel_size(source_sampling);
        let aspect = (half.x * source.y) / (half.y * source.x);
        if !aspect.is_finite() || aspect <= 0.0 {
            return Err(VideoCollageError::Geometry);
        }
        let crop = Vec2::new(aspect.min(1.0), aspect.recip().min(1.0));
        let shortest = half.min_element();
        Ok(VideoCollageGpuControls {
            origin: [tile.x() as f32, tile.y() as f32],
            pixel_size: pixel_size.to_array(),
            half_size: half.to_array(),
            crop: crop.to_array(),
            radius: shortest * (self.roundness / 100.0),
            border_width: self.border_width.min(shortest),
            feather: pixel_size.min_element(),
            border: self.border,
        })
    }
}

/// Shared inputs to CPU sampling and one GPU scissored draw.
#[derive(Debug, Clone, Copy)]
pub struct VideoCollageGpuControls {
    pub origin: [f32; 2],
    pub pixel_size: [f32; 2],
    pub half_size: [f32; 2],
    pub crop: [f32; 2],
    pub radius: f32,
    pub border_width: f32,
    pub feather: f32,
    pub border: [f32; 4],
}

fn physical_pixel_size(sampling: ImageSampling) -> Vec2 {
    let [sx, sy] = sampling.render_scale();
    let par = sampling.sample_aspect_ratio();
    Vec2::new(
        ((f64::from(par.numerator()) / f64::from(par.denominator())) / sx.to_f64()) as f32,
        sy.to_f64().recip() as f32,
    )
}

fn axis_tiles(length: u32, count: u32, spacing: f32) -> Vec<(u32, u32)> {
    let maximum_gap = length.saturating_sub(count) / count.saturating_sub(1).max(1);
    let gap = if count > 1 {
        (spacing.round() as u32).min(maximum_gap)
    } else {
        0
    };
    let inside = length - gap * (count - 1);
    let partition = |i| (u64::from(inside) * u64::from(i) / u64::from(count)) as u32;
    (0..count)
        .map(|i| (partition(i) + i * gap, partition(i + 1) - partition(i)))
        .collect()
}

/// Stable per-position author field. Changing the active width preserves the other rows' sources.
pub fn video_collage_source_key(column: u32, row: u32) -> String {
    format!("tile_source_{:02}", row * COLLAGE_MAX_AXIS + column + 1)
}

/// Return a collage source parameter's persistent zero-based column and row.
pub fn video_collage_source_position(parameter: &ParameterId) -> Option<(u32, u32)> {
    let tile = parameter
        .as_str()
        .strip_prefix("mondrian.effect.builtin.video_collage.tile_source_")?
        .parse::<u32>()
        .ok()?
        .checked_sub(1)?;
    (tile < COLLAGE_MAX_AXIS * COLLAGE_MAX_AXIS)
        .then_some((tile % COLLAGE_MAX_AXIS, tile / COLLAGE_MAX_AXIS))
}

pub(crate) struct VideoCollageSource {
    pub(crate) parameter: ParameterId,
    pub(crate) offset: TimelineTime,
    pub(crate) reference: VisualSourceReference,
}

pub(crate) struct VideoCollageSources {
    pub(crate) columns: u32,
    pub(crate) rows: u32,
    pub(crate) tiles: Vec<VideoCollageSource>,
}

impl VideoCollageSource {
    pub(crate) fn effective_reference(&self) -> Result<VisualSourceReference, TimelineTimeError> {
        self.reference.checked_offset(self.offset)
    }
}

/// Resolve static routing once using the same rules at frame evaluation and range capture.
pub(crate) fn video_collage_sources(
    effect: &EffectNode,
) -> Result<VideoCollageSources, VideoCollageError> {
    let read = |field| {
        effect
            .effect_type
            .parameter_id(field)
            .ok()
            .and_then(|id| effect.evaluate_parameter(&id, TimelineTime::ZERO))
    };
    let axis = |field| match read(field) {
        Some(PropertyValue::Int(value)) => {
            u32::try_from(value).map_err(|_| VideoCollageError::Controls)
        }
        _ => Err(VideoCollageError::Controls),
    };
    let columns = axis("columns")?;
    let rows = axis("rows")?;
    let mode = match read("playback") {
        Some(PropertyValue::Enum(value)) => VideoCollagePlayback::from_key(&value),
        _ => None,
    }
    .ok_or(VideoCollageError::Controls)?;
    let delay = read("tile_delay")
        .and_then(|value| value.as_f64())
        .ok_or(VideoCollageError::Delay)?;
    let offsets = video_collage_offsets(columns, rows, mode, delay)?;
    let tiles = offsets
        .into_iter()
        .enumerate()
        .map(|(i, offset)| {
            let key = video_collage_source_key(i as u32 % columns, i as u32 / columns);
            let parameter =
                effect.effect_type.parameter_id(&key).map_err(|_| VideoCollageError::Sources)?;
            let reference = effect
                .evaluate_visual_source_parameter(&parameter)
                .ok_or(VideoCollageError::Sources)?;
            Ok(VideoCollageSource { parameter, offset, reference })
        })
        .collect::<Result<_, VideoCollageError>>()?;
    Ok(VideoCollageSources { columns, rows, tiles })
}

/// Exact per-tile offsets on the consumer's Sequence clock.
pub(crate) fn video_collage_offsets(
    columns: u32,
    rows: u32,
    playback: VideoCollagePlayback,
    delay: f64,
) -> Result<Vec<TimelineTime>, VideoCollageError> {
    if !(1..=COLLAGE_MAX_AXIS).contains(&columns) || !(1..=COLLAGE_MAX_AXIS).contains(&rows) {
        return Err(VideoCollageError::Controls);
    }
    if !(0.0..=2.0).contains(&delay) {
        return Err(VideoCollageError::Delay);
    }
    let delay = TimelineTime::from_f64_quantized(-delay, DELAY_TIMESCALE)?;
    let mut order = (0..columns * rows).collect::<Vec<_>>();
    if playback == VideoCollagePlayback::Random {
        order.sort_unstable_by_key(|index| {
            let tile = (index / columns) * COLLAGE_MAX_AXIS + index % columns;
            let mut hash = (tile + 1).wrapping_mul(0x9e37_79b9);
            hash ^= hash >> 16;
            hash.wrapping_mul(0x85eb_ca6b)
        });
    }
    let mut offsets = vec![TimelineTime::ZERO; order.len()];
    if playback != VideoCollagePlayback::Simultaneous {
        for (rank, tile) in order.into_iter().enumerate() {
            offsets[tile as usize] =
                TimelineTime::new(delay.numerator() * rank as i64, delay.denominator())?;
        }
    }
    Ok(offsets)
}

/// Render the complete frame using the same controls as the GPU draw.
/// A `None` tile reads the entering picture on its actual grid.
pub fn video_collage_frame_f32(
    primary: EffectFloatFrameView<'_>,
    sources: &[Option<AuxiliaryFloatFrame<'_>>],
    grade: VideoCollageGrade,
) -> Result<Vec<[f32; 4]>, crate::EffectFloatExecutionError> {
    primary.validate()?;
    for source in sources.iter().flatten() {
        source.frame.validate()?;
    }
    apply_video_collage_controlled(primary, sources, grade, &mut || Ok(()))
}

pub(crate) fn apply_video_collage_controlled(
    primary: EffectFloatFrameView<'_>,
    sources: &[Option<AuxiliaryFloatFrame<'_>>],
    grade: VideoCollageGrade,
    checkpoint: &mut impl FnMut() -> Result<(), crate::EffectFloatExecutionError>,
) -> Result<Vec<[f32; 4]>, crate::EffectFloatExecutionError> {
    if sources.len() != grade.tile_count() {
        return Err(VideoCollageError::Sources.into());
    }
    let mut output = Vec::new();
    output.try_reserve_exact(primary.pixels().len()).map_err(|_| {
        EffectFloatFrameError::AllocationUnavailable {
            required_bytes: std::mem::size_of_val(primary.pixels()),
        }
    })?;
    for chunk in primary.pixels().chunks(4096) {
        checkpoint()?;
        output.resize(output.len() + chunk.len(), [0.0; 4]);
    }
    let primary_raster = AuxiliaryFloatFrame::new(primary, AuxiliaryPolicy::MATCH_PRIMARY);
    for (tile, source) in grade.tiles(primary.extent(), primary.sampling()).into_iter().zip(sources)
    {
        if tile.is_empty() {
            continue;
        }
        let source = source.unwrap_or(primary_raster);
        let controls = grade.gpu_controls(
            tile,
            primary.sampling(),
            source.frame.extent(),
            source.frame.sampling(),
        )?;
        for y in tile.y()..tile.y() + tile.height() {
            checkpoint()?;
            for x in tile.x()..tile.x() + tile.width() {
                let position = Vec2::new(x as f32 + PIXEL_CENTRE, y as f32 + PIXEL_CENTRE);
                output[y as usize * primary.extent().width() as usize + x as usize] =
                    sample_tile(position, source, controls);
            }
        }
    }
    checkpoint()?;
    Ok(output)
}

fn sample_tile(
    position: Vec2,
    source: AuxiliaryFloatFrame<'_>,
    controls: VideoCollageGpuControls,
) -> [f32; 4] {
    let half = Vec2::from_array(controls.half_size);
    let local = (position - Vec2::from_array(controls.origin))
        * Vec2::from_array(controls.pixel_size)
        - half;
    let q = local.abs() - half + Vec2::splat(controls.radius);
    let distance = q.max(Vec2::ZERO).length() + q.max_element().min(0.0) - controls.radius;
    let outer = (0.5 - distance / controls.feather).clamp(0.0, 1.0);
    if outer == 0.0 {
        return [0.0; 4];
    }
    let inner = (0.5 - (distance + controls.border_width) / controls.feather).clamp(0.0, 1.0);
    let uv =
        Vec2::splat(PIXEL_CENTRE) + PIXEL_CENTRE * local / half * Vec2::from_array(controls.crop);
    let picture = sample_auxiliary_associated_f32(
        source,
        uv.to_array(),
        AuxiliarySampling::FRAME_TRANSFORM_LINEAR_CLAMP,
    );
    let [red, green, blue, alpha] = controls.border;
    let border = [red * alpha, green * alpha, blue * alpha, alpha];
    let ring = outer - inner;
    crate::coverage::straight_rgba_from_premultiplied(std::array::from_fn(|channel| {
        picture[channel] * inner + border[channel] * ring
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        instantiate_effect_node, lower_effect_graph_to_gpu_pipeline, AuxiliaryFloatInput,
        AuxiliaryResolution, EffectColorDomain, EffectExecutionSession, EffectGpuStage,
        EffectProcessorControl, EffectValueFormat, EffectWorkingPrecision, PreparedEffectProgram,
    };
    use mondrian_core::{
        effect_data::EffectType, AssetId, ColorFrameAlpha, VisualSourceBoundary,
        VisualSourceRetime, WorkingColorSpace,
    };

    #[test]
    fn extreme_spacing_preserves_pixels_and_does_not_overlap() {
        let grade = VideoCollageGrade::new(8, 8, 100.0, 0.0, 0.0, [0.0; 4]).unwrap();
        let tiles = grade.tiles(EffectFrameExtent::new(29, 17), ImageSampling::UNIT_SQUARE);
        assert_eq!(tiles.len(), 64);
        assert!(tiles.iter().all(|tile| !tile.is_empty()));
        assert_eq!(tiles[7].x() + tiles[7].width(), 29);
        assert_eq!(tiles[63].y() + tiles[63].height(), 17);
        assert!(tiles[..8].windows(2).all(|p| p[0].x() + p[0].width() <= p[1].x()));
    }

    #[test]
    fn delay_uses_exact_ranks_and_stable_random_order() {
        let offsets = |mode| video_collage_offsets(3, 2, mode, 0.125).unwrap();
        let sequential = offsets(VideoCollagePlayback::Sequential);
        assert_eq!(sequential[5], TimelineTime::new(-5, 8).unwrap());
        assert_eq!(
            offsets(VideoCollagePlayback::Simultaneous),
            [TimelineTime::ZERO; 6]
        );
        let mut random = offsets(VideoCollagePlayback::Random);
        assert_eq!(random, offsets(VideoCollagePlayback::Random));
        assert_ne!(random, sequential);
        random.sort_unstable_by(|left, right| right.cmp(left));
        assert_eq!(random, sequential);
        assert_eq!(video_collage_source_key(0, 1), "tile_source_09");
    }

    #[test]
    fn authored_video_collage_shares_one_external_value_across_sixty_four_cells() {
        let kind = EffectType::VideoCollage;
        let mut effect = instantiate_effect_node(kind.clone()).unwrap();
        for field in ["columns", "rows"] {
            effect
                .set_static_value_by_parameter(
                    &kind.parameter_id(field).unwrap(),
                    PropertyValue::Int(8),
                )
                .unwrap();
        }
        let source = VisualSourceReference::Asset {
            asset_id: AssetId::new(),
            retime: Default::default(),
        };
        for row in 0..8 {
            for column in 0..8 {
                let parameter = kind.parameter_id(&video_collage_source_key(column, row)).unwrap();
                effect
                    .set_static_value_by_parameter(&parameter, PropertyValue::VisualSource(source))
                    .unwrap();
            }
        }
        let graph = crate::compile_clip_effect_graph(
            &[effect],
            &[],
            TimelineTime::ZERO,
            WorkingColorSpace::LinearRec709,
        )
        .unwrap();
        assert_eq!(graph.auxiliary_inputs().len(), 1);
        assert_eq!(graph.auxiliary_source_bindings().count(), 1);
        let size = EffectFrameExtent::new(1920, 1080);
        let bytes = crate::estimate_compiled_float_raster_working_bytes(&graph, size, &[(0, size)])
            .unwrap();
        assert_eq!(bytes, 2 * 1920 * 1080 * 16);
        let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
        let [EffectGpuStage::Spatial { pass, .. }] = pipeline.stages() else {
            panic!("one spatial stage must retain the 64 cells");
        };
        assert_eq!(pass.external_slots().collect::<Vec<_>>(), [0; 64]);
    }

    #[test]
    fn authored_video_collage_keeps_distinct_exact_slots_and_ignores_hidden_sources() {
        let kind = EffectType::VideoCollage;
        let mut effect = instantiate_effect_node(kind.clone()).unwrap();
        let reference = |offset| VisualSourceReference::Asset {
            asset_id: AssetId::new(),
            retime: VisualSourceRetime {
                time_offset: offset,
                mode: VisualSourceBoundary::Transparent,
            },
        };
        let a = reference(TimelineTime::ZERO);
        let b = reference(TimelineTime::new(1, 24).unwrap());
        let inactive = reference(TimelineTime::ZERO);
        for (field, value) in [
            ("columns", PropertyValue::Int(2)),
            ("rows", PropertyValue::Int(1)),
            ("spacing", PropertyValue::Float(0.0)),
            ("tile_delay", PropertyValue::Float(0.125)),
            ("tile_source_01", PropertyValue::VisualSource(a)),
            ("tile_source_02", PropertyValue::VisualSource(b)),
            ("tile_source_09", PropertyValue::VisualSource(inactive)),
        ] {
            effect
                .set_static_value_by_parameter(&kind.parameter_id(field).unwrap(), value)
                .unwrap();
        }
        let program = PreparedEffectProgram::prepare(
            std::slice::from_ref(&effect),
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .unwrap();
        let graph = program.evaluate(TimelineTime::ONE).unwrap();
        assert!(!graph.requires_primary_temporal_execution());
        let effective = b.checked_offset(TimelineTime::new(-1, 8).unwrap()).unwrap();
        assert_eq!(
            effective.retime().unwrap().time_offset,
            TimelineTime::new(-1, 12).unwrap()
        );
        let mut dependencies = Vec::new();
        program
            .visit_authored_visual_sources(|source| dependencies.push(source))
            .unwrap();
        assert_eq!(dependencies, [a, effective]);
        assert_eq!(
            graph
                .auxiliary_source_bindings()
                .map(|source| source.reference())
                .collect::<Vec<_>>(),
            dependencies
        );
        let pipeline = lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
        let [EffectGpuStage::Spatial { pass, .. }] = pipeline.stages() else {
            panic!("both external slots must feed a single spatial stage");
        };
        assert_eq!(pass.external_slots().collect::<Vec<_>>(), [0, 1]);

        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        );
        let make = |extent, pixels| {
            EffectFloatFrameView::new(extent, format, ImageSampling::UNIT_SQUARE, pixels)
        };
        let extent = EffectFrameExtent::new(8, 2);
        let primary = [[0.0, 0.0, 1.0, 1.0]; 16];
        let red = [[1.25, -0.1, 0.0, 0.25]; 2];
        let green = [[0.0, 1.0, 0.0, 0.75]; 3];
        let binding = |slot, extent, pixels| AuxiliaryFloatInput {
            slot,
            raster: AuxiliaryFloatFrame::new(
                make(extent, pixels),
                AuxiliaryPolicy::new(AuxiliaryResolution::Authored),
            ),
        };
        let bindings = [
            binding(1, EffectFrameExtent::new(1, 3), &green),
            binding(0, EffectFrameExtent::new(2, 1), &red),
        ];
        let output = EffectExecutionSession::default()
            .apply_compiled_rgba_f32_with_auxiliary(
                make(extent, &primary),
                &graph,
                0,
                &bindings,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        for row in output.pixels().chunks_exact(8) {
            for (pixel, expected) in row.iter().zip([red[0]; 4].into_iter().chain([green[0]; 4])) {
                assert!(pixel.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1.0e-6));
            }
        }
        let preview = PreparedEffectProgram::prepare_for_auxiliary_preview(
            &[effect],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .unwrap();
        let fallback = preview.evaluate_auxiliary_preview(TimelineTime::ONE, &[effective]).unwrap();
        assert!(fallback.graph().is_identity());
        assert_eq!(
            fallback.auxiliary_preview_fallbacks().next().unwrap().reference(),
            effective
        );
    }
}
