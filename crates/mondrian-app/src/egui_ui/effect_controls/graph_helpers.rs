//! Graph time/value axis, curve drawing, drag/mutation, speed, bezier helpers.
use super::*;

pub(crate) fn visible_interpolation_modes() -> [InterpolationType; 5] {
    [
        InterpolationType::Linear,
        InterpolationType::Bezier,
        InterpolationType::AutoBezier,
        InterpolationType::ContinuousBezier,
        InterpolationType::Hold,
    ]
}

pub(crate) fn interpolation_mode_label(interpolation: InterpolationType) -> &'static str {
    match interpolation {
        InterpolationType::Linear => "线性",
        InterpolationType::Bezier => "贝塞尔曲线",
        InterpolationType::AutoBezier => "自动贝塞尔曲线",
        InterpolationType::ContinuousBezier => "连续贝塞尔曲线",
        InterpolationType::Hold => "定格",
        InterpolationType::EaseIn | InterpolationType::EaseOut => "",
    }
}

pub(crate) fn interpolation_action_label(interpolation: InterpolationType) -> &'static str {
    match interpolation {
        InterpolationType::EaseIn => "缓入",
        InterpolationType::EaseOut => "缓出",
        _ => "",
    }
}

pub(crate) fn draw_keyframe_interpolation_menu(
    ui: &mut Ui,
    current_mode: Option<InterpolationType>,
    available_modes: &[InterpolationType],
    mut apply: impl FnMut(InterpolationType),
) {
    let widest_label = available_modes
        .iter()
        .map(|mode| interpolation_mode_label(*mode))
        .chain(
            [InterpolationType::EaseIn, InterpolationType::EaseOut]
                .into_iter()
                .map(interpolation_action_label),
        )
        .max_by_key(|label| label.chars().count())
        .unwrap_or("线性");
    let text_width = ui
        .painter()
        .layout_no_wrap(
            widest_label.to_string(),
            typography::body_small(),
            palette::text_primary(),
        )
        .size()
        .x;
    let menu_width = 18.0 + 10.0 + text_width + 16.0;
    ui.spacing_mut().menu_width = menu_width;
    ui.set_min_width(menu_width);
    ui.set_max_width(menu_width);

    for mode in available_modes {
        if theme::checkmark_menu_action_fill(
            ui,
            current_mode == Some(*mode),
            interpolation_mode_label(*mode),
        )
        .clicked()
        {
            apply(*mode);
            ui.close();
        }
    }
    ui.separator();
    for action in [InterpolationType::EaseIn, InterpolationType::EaseOut] {
        if theme::menu_action_fill(ui, interpolation_action_label(action)).clicked() {
            apply(action);
            ui.close();
        }
    }
}

pub(crate) fn collect_inspector_groups(
    property_bag: &mondrian_core::automation::PropertyBag,
) -> Vec<InspectorGroup<'_>> {
    let mut entries = property_bag
        .iter()
        .map(|(path, property)| (path, property_group_meta(path, property), property))
        .collect::<Vec<_>>();
    entries.sort_by(
        |(path_a, meta_a, property_a), (path_b, meta_b, property_b)| {
            meta_a
                .order
                .cmp(&meta_b.order)
                .then_with(|| meta_a.title.cmp(&meta_b.title))
                .then_with(|| property_order(path_a).cmp(&property_order(path_b)))
                .then_with(|| {
                    property_display_name(property_a).cmp(&property_display_name(property_b))
                })
        },
    );

    let mut groups = Vec::<InspectorGroup<'_>>::new();
    for (path, meta, property) in entries {
        if let Some(group) = groups.iter_mut().find(|group| group.meta.id == meta.id) {
            group.properties.push((path, property));
        } else {
            groups.push(InspectorGroup { meta, properties: vec![(path, property)] });
        }
    }
    groups
}

pub(crate) fn draw_group_header_row(
    ui: &mut Ui,
    meta: &AnimationGroupMeta,
    collapsed: bool,
    enabled_state: Option<bool>,
    show_effect_controls: bool,
    can_delete: bool,
) -> GroupHeaderRowResponse {
    let desired_size = Vec2::new(
        ui.available_width(),
        tokens::inspector_group_header_height(),
    );
    let (rect, row_response) = ui.allocate_exact_size(desired_size, Sense::click());
    let visuals = ui.visuals();
    let mut toggle_effect = None;
    let mut delete_effect = None;

    if ui.is_rect_visible(rect) {
        let fill = if row_response.hovered() {
            palette::bg_surface_active()
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, visuals.menu_corner_radius, fill);

        let mut title = meta.title.clone();
        if meta.kind == AnimationGroupKind::TimeRemap {
            title = "时间重映射".to_string();
        }

        let ghost_button_size = tokens::timeline_toolbar_button_size();
        let button_size = Vec2::new(ghost_button_size[0], ghost_button_size[1]);
        let button_gap = 4.0;
        let button_count = usize::from(enabled_state.is_some()) + usize::from(can_delete);
        let action_width = if show_effect_controls && button_count > 0 {
            button_count as f32 * button_size.x + button_count.saturating_sub(1) as f32 * button_gap
        } else {
            0.0
        };
        let action_rect = if action_width > 0.0 {
            Some(Rect::from_min_max(
                Pos2::new(
                    (rect.right() - 4.0 - action_width).max(rect.left() + 4.0),
                    rect.top(),
                ),
                Pos2::new(rect.right() - 4.0, rect.bottom()),
            ))
        } else {
            None
        };
        let title_rect = Rect::from_min_max(
            Pos2::new(rect.left() + 4.0, rect.top()),
            Pos2::new(
                action_rect
                    .map(|action| (action.left() - 8.0).max(rect.left() + 4.0))
                    .unwrap_or(rect.right() - 4.0),
                rect.bottom(),
            ),
        );
        let content_center_y = rect.center().y;
        let caret_rect = Rect::from_center_size(
            Pos2::new(
                title_rect.left() + tokens::icon_size() * 0.5,
                content_center_y,
            ),
            Vec2::splat(tokens::icon_size()),
        );
        let title_x = caret_rect.right() + tokens::inspector_group_indent() * 0.45;
        let title_galley = ui.painter().layout_no_wrap(
            title.clone(),
            typography::body_small(),
            palette::text_primary(),
        );
        let label_rect = Rect::from_min_size(
            Pos2::new(title_x, content_center_y - title_galley.size().y * 0.5),
            title_galley.size(),
        );

        theme::draw_icon(
            ui.painter(),
            caret_rect,
            if collapsed {
                theme::UiIcon::ArrowRight
            } else {
                theme::UiIcon::ArrowDown
            },
            palette::text_muted(),
        );
        ui.painter().with_clip_rect(title_rect).galley(
            label_rect.min,
            title_galley,
            palette::text_primary(),
        );

        if let Some(action_bounds) = action_rect {
            let action_rect = Rect::from_center_size(
                Pos2::new(action_bounds.center().x, content_center_y),
                Vec2::new(action_width, button_size.y),
            );
            let delete_rect = can_delete.then(|| {
                Rect::from_center_size(
                    Pos2::new(action_rect.right() - button_size.x * 0.5, content_center_y),
                    button_size,
                )
            });
            let toggle_rect = enabled_state.map(|enabled| {
                let right_edge =
                    delete_rect.map(|rect| rect.left() - button_gap).unwrap_or(action_rect.right());
                let rect = Rect::from_center_size(
                    Pos2::new(right_edge - button_size.x * 0.5, content_center_y),
                    button_size,
                );
                let icon = if enabled {
                    theme::UiIcon::Eye
                } else {
                    theme::UiIcon::EyeOff
                };
                let response = theme::icon_ghost_button_at(
                    ui,
                    rect,
                    row_response.id.with("toggle_effect"),
                    icon,
                );
                toggle_effect = Some(response);
                rect
            });
            if let Some(rect) = delete_rect {
                let response = theme::icon_ghost_button_at(
                    ui,
                    rect,
                    row_response.id.with("delete_effect"),
                    theme::UiIcon::Trash,
                );
                delete_effect = Some(response);
            }
            let _ = toggle_rect;
        }
    }

    GroupHeaderRowResponse { row: row_response, toggle_effect, delete_effect }
}

pub(crate) fn group_effect_id(group: &InspectorGroup<'_>) -> Option<EffectId> {
    group
        .properties
        .first()
        .and_then(|(path, _)| parse_effect_id_from_property_path(path))
}

pub(crate) fn group_mask_id(group: &InspectorGroup<'_>) -> Option<mondrian_effects::mask::MaskId> {
    group
        .properties
        .first()
        .and_then(|(path, _)| parse_mask_id_from_property_path(path))
}

pub(crate) fn parse_mask_id_from_property_path(
    path: &str,
) -> Option<mondrian_effects::mask::MaskId> {
    let mut segments = path.split('.');
    if segments.next()? != "mask" {
        return None;
    }
    let id_raw = segments.next()?;
    uuid::Uuid::parse_str(id_raw).ok().map(mondrian_effects::mask::MaskId)
}

pub(crate) fn parse_effect_id_from_property_path(path: &str) -> Option<EffectId> {
    mondrian_core::effect_data::parse_effect_id_from_property_path(path)
}

pub(crate) fn graph_channel_labels(value: &PropertyValue) -> &'static [&'static str] {
    match value {
        PropertyValue::Vec2(_) => &["X", "Y"],
        PropertyValue::Vec3(_) => &["X", "Y", "Z"],
        PropertyValue::Color(_) => &["R", "G", "B", "A"],
        PropertyValue::Vec4(_) => &["1", "2", "3", "4"],
        _ => &["值"],
    }
}

pub(crate) fn graph_channel_color(index: usize) -> Color32 {
    match index {
        0 => palette::interaction_highlight(),
        1 => palette::accent_audio(),
        2 => palette::status_warning(),
        3 => palette::status_success(),
        _ => palette::text_primary(),
    }
}

pub(crate) fn selected_keyframe_ids(
    channel: &mondrian_core::automation::AnimationChannel,
    selected_on_active: &[AnimationKeyframeSelection],
) -> HashSet<KeyframeId> {
    selected_on_active
        .iter()
        .filter_map(|selected| channel.keyframe_at(selected.time).map(|keyframe| keyframe.id))
        .collect()
}

pub(crate) fn selected_active_keyframe_id(
    channel: &mondrian_core::automation::AnimationChannel,
    selected_on_active: &[AnimationKeyframeSelection],
) -> Option<KeyframeId> {
    if selected_on_active.len() == 1 {
        channel.keyframe_at(selected_on_active[0].time).map(|keyframe| keyframe.id)
    } else {
        None
    }
}

pub(crate) fn graph_time_range(clip: &Clip) -> (TimeTicks, TimeTicks) {
    let start = timecode_to_ticks(clip.position);
    let end = timecode_to_ticks(clip.end_position());
    if start == end {
        let pad = SUBFRAME_TICKS_PER_FRAME * 2;
        (start.saturating_sub(pad), end + pad)
    } else {
        (start, end)
    }
}

pub(crate) fn graph_time_axis_ticks(
    time_min: TimeTicks,
    time_max: TimeTicks,
    time_base: mondrian_core::types::Rational,
    width: f32,
) -> Vec<TimeTicks> {
    if time_max <= time_min {
        return vec![time_min];
    }

    let desired_tick_count = ((width / 92.0).round() as usize).clamp(3, 7);
    let frame_span = ((time_max - time_min) as f64 / SUBFRAME_TICKS_PER_FRAME as f64).max(1.0);
    let raw_step_frames = (frame_span / (desired_tick_count.saturating_sub(1)) as f64).max(1.0);
    let step_frames = nice_frame_step(raw_step_frames, time_base);
    let start_frame = (time_min / SUBFRAME_TICKS_PER_FRAME).max(0);
    let end_frame = (time_max / SUBFRAME_TICKS_PER_FRAME).max(start_frame);
    let first_tick_frame = (start_frame / step_frames) * step_frames;

    let mut ticks = Vec::new();
    let mut frame = first_tick_frame;
    while frame <= end_frame {
        let tick = frame * SUBFRAME_TICKS_PER_FRAME;
        if tick >= time_min && tick <= time_max {
            ticks.push(tick);
        }
        frame += step_frames;
    }
    if ticks.first().copied() != Some(time_min) {
        ticks.insert(0, time_min);
    }
    if ticks.last().copied() != Some(time_max) {
        ticks.push(time_max);
    }
    ticks.sort_unstable();
    ticks.dedup();
    ticks
}

pub(crate) fn nice_frame_step(
    raw_step_frames: f64,
    time_base: mondrian_core::types::Rational,
) -> i64 {
    let fps = (1.0 / time_base.to_f64()).round().max(1.0) as i64;
    let candidates = [1, 2, 5, 10, 15, fps / 2, fps, fps * 2, fps * 5, fps * 10];
    candidates
        .into_iter()
        .filter(|step| *step > 0)
        .find(|step| *step as f64 >= raw_step_frames)
        .unwrap_or((raw_step_frames.ceil() as i64).max(1))
}

pub(crate) fn format_graph_time_label(
    time: TimeTicks,
    time_base: mondrian_core::types::Rational,
) -> String {
    let frame = (time as f64 / SUBFRAME_TICKS_PER_FRAME as f64).round() as i64;
    let smpte = TimeCode::new(frame.max(0), time_base).to_smpte();
    if let Some(stripped) = smpte.strip_prefix("00:") {
        stripped.to_string()
    } else {
        smpte
    }
}

#[derive(Clone)]
pub(crate) struct GraphTimeAxisLabel {
    pub(crate) x: f32,
    pub(crate) text: String,
}

pub(crate) fn graph_time_label_positions(
    painter: &egui::Painter,
    ticks: &[TimeTicks],
    plot_rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    time_base: mondrian_core::types::Rational,
) -> Vec<GraphTimeAxisLabel> {
    if ticks.is_empty() {
        return Vec::new();
    }

    let candidates = ticks
        .iter()
        .enumerate()
        .map(|(index, time)| {
            let text = format_graph_time_label(*time, time_base);
            let width = painter
                .layout_no_wrap(
                    text.clone(),
                    typography::body_small(),
                    palette::text_muted(),
                )
                .size()
                .x;
            let x = graph_x_for_time(plot_rect, time_min, time_max, *time);
            let mut left = x - width * 0.5;
            let mut right = x + width * 0.5;
            if index == 0 {
                left = plot_rect.left();
                right = left + width;
            } else if index + 1 == ticks.len() {
                right = plot_rect.right();
                left = right - width;
            }
            (index, GraphTimeAxisLabel { x, text }, left, right)
        })
        .collect::<Vec<_>>();

    let min_gap = 10.0;
    let mut visible = Vec::<(GraphTimeAxisLabel, f32, f32)>::new();
    for (index, label, left, right) in candidates {
        if index == 0 {
            visible.push((label, left, right));
            continue;
        }

        if index + 1 == ticks.len() {
            while visible.len() > 1 {
                let Some((_, _, last_right)) = visible.last() else {
                    break;
                };
                if left >= *last_right + min_gap {
                    break;
                }
                visible.pop();
            }
            if visible.last().is_some_and(|(_, _, last_right)| left < *last_right + min_gap) {
                let keep_first =
                    visible.first().map(|(_, left, right)| right - left).unwrap_or(0.0)
                        <= right - left;
                return if keep_first {
                    visible.first().map(|(label, _, _)| vec![label.clone()]).unwrap_or_default()
                } else {
                    vec![label]
                };
            }
            visible.push((label, left, right));
            continue;
        }

        if visible.last().is_some_and(|(_, _, last_right)| left < *last_right + min_gap) {
            continue;
        }
        visible.push((label, left, right));
    }

    visible.into_iter().map(|(label, _, _)| label).collect()
}

pub(crate) fn graph_value_range(
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    current_time: TimeTicks,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> (f64, f64) {
    let mut values = property
        .channel(channel_index)
        .map(|channel| {
            channel.keyframes().iter().map(|keyframe| keyframe.value).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    values.push(
        property
            .evaluate(time_min)
            .to_channel_values()
            .get(channel_index)
            .copied()
            .unwrap_or(0.0),
    );
    values.push(
        property
            .evaluate(time_max)
            .to_channel_values()
            .get(channel_index)
            .copied()
            .unwrap_or(0.0),
    );
    values.push(
        property
            .evaluate(current_time)
            .to_channel_values()
            .get(channel_index)
            .copied()
            .unwrap_or(0.0),
    );

    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() || !max.is_finite() {
        return (-1.0, 1.0);
    }
    if (max - min).abs() < f64::EPSILON {
        let pad = max.abs().max(1.0) * 0.25;
        (min - pad, max + pad)
    } else {
        let pad = (max - min) * 0.12;
        (min - pad, max + pad)
    }
}

pub(crate) fn graph_speed_range(
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    clip: &Clip,
    current_time: TimeTicks,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> (f64, f64) {
    let mut values = property
        .channel(channel_index)
        .map(|channel| {
            channel
                .keyframes()
                .iter()
                .map(|keyframe| {
                    speed_per_second_at_time(
                        property,
                        channel_index,
                        clip,
                        keyframe.time,
                        time_min,
                        time_max,
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    values.push(speed_per_second_at_time(
        property,
        channel_index,
        clip,
        current_time,
        time_min,
        time_max,
    ));

    let samples = 48usize;
    for sample in 0..=samples {
        let t = sample as f32 / samples as f32;
        let time = time_min + ((time_max - time_min) as f32 * t) as i64;
        values.push(speed_per_second_at_time(
            property,
            channel_index,
            clip,
            time,
            time_min,
            time_max,
        ));
    }

    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() || !max.is_finite() {
        return (-1.0, 1.0);
    }
    if (max - min).abs() < f64::EPSILON {
        let pad = max.abs().max(1.0) * 0.25;
        (min - pad, max + pad)
    } else {
        let pad = (max - min) * 0.12;
        (min - pad, max + pad)
    }
}

pub(crate) fn graph_x_for_time(
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    time: TimeTicks,
) -> f32 {
    if time_max <= time_min {
        return rect.left();
    }
    let t = ((time - time_min) as f32 / (time_max - time_min) as f32).clamp(0.0, 1.0);
    egui::lerp(rect.left()..=rect.right(), t)
}

pub(crate) fn graph_y_for_value(rect: Rect, value_min: f64, value_max: f64, value: f64) -> f32 {
    if (value_max - value_min).abs() < f64::EPSILON {
        return rect.center().y;
    }
    let t = ((value - value_min) / (value_max - value_min)).clamp(0.0, 1.0) as f32;
    egui::lerp(rect.bottom()..=rect.top(), t)
}

pub(crate) fn graph_point_for_keyframe(
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    time: TimeTicks,
    value: f64,
) -> Pos2 {
    Pos2::new(
        graph_x_for_time(rect, time_min, time_max, time),
        graph_y_for_value(rect, value_min, value_max, value),
    )
}

pub(crate) fn floating_toolbar_position(
    selected_bounds: Rect,
    avoid_bounds: Rect,
    container_rect: Rect,
    icon_count: usize,
    preset_count: usize,
) -> Pos2 {
    let button_size = tokens::timeline_toolbar_button_size();
    let estimated_width = 18.0
        + icon_count as f32 * button_size[0]
        + preset_count as f32 * 64.0
        + if preset_count > 0 { 16.0 } else { 0.0 };
    let estimated_height = button_size[1] + 12.0;
    let gap = 10.0;
    let x = (selected_bounds.center().x - estimated_width * 0.5).clamp(
        container_rect.left() + 8.0,
        (container_rect.right() - estimated_width - 8.0).max(container_rect.left() + 8.0),
    );

    let above = Rect::from_min_size(
        Pos2::new(x, selected_bounds.top() - estimated_height - gap),
        Vec2::new(estimated_width, estimated_height),
    );
    let below = Rect::from_min_size(
        Pos2::new(x, selected_bounds.bottom() + gap),
        Vec2::new(estimated_width, estimated_height),
    );
    let avoid = avoid_bounds.expand(8.0);
    let fits_above = above.top() >= container_rect.top() + 4.0 && !above.intersects(avoid);
    let fits_below = below.bottom() <= container_rect.bottom() - 4.0 && !below.intersects(avoid);

    if fits_above {
        above.min
    } else if fits_below {
        below.min
    } else if selected_bounds.top() - container_rect.top()
        >= container_rect.bottom() - selected_bounds.bottom()
    {
        above.min
    } else {
        below.min
    }
}

pub(crate) fn draw_graph_curve(
    painter: &egui::Painter,
    rect: Rect,
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    preview_map: Option<&HashMap<TimeTicks, Pos2>>,
) {
    let samples = 96usize;
    let mut points = Vec::with_capacity(samples + 1);
    for sample in 0..=samples {
        let t = sample as f32 / samples as f32;
        let time = time_min + ((time_max - time_min) as f32 * t) as i64;
        let value = property
            .evaluate(time)
            .to_channel_values()
            .get(channel_index)
            .copied()
            .unwrap_or(0.0);
        points.push(
            preview_map.and_then(|map| map.get(&time).copied()).unwrap_or_else(|| {
                graph_point_for_keyframe(
                    rect, time_min, time_max, value_min, value_max, time, value,
                )
            }),
        );
    }
    painter.add(Shape::line(
        points,
        Stroke::new(
            tokens::graph_editor_curve_stroke_width(),
            graph_channel_color(channel_index),
        ),
    ));
}

pub(crate) fn draw_speed_graph_curve(
    painter: &egui::Painter,
    rect: Rect,
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    clip: &Clip,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) {
    let samples = 128usize;
    let mut points = Vec::with_capacity(samples + 1);
    for sample in 0..=samples {
        let t = sample as f32 / samples as f32;
        let time = time_min + ((time_max - time_min) as f32 * t) as i64;
        let speed =
            speed_per_second_at_time(property, channel_index, clip, time, time_min, time_max);
        points.push(graph_point_for_keyframe(
            rect, time_min, time_max, value_min, value_max, time, speed,
        ));
    }
    painter.add(Shape::line(
        points,
        Stroke::new(
            tokens::graph_editor_curve_stroke_width(),
            graph_channel_color(channel_index),
        ),
    ));
}

pub(crate) fn speed_per_second_at_time(
    property: &mondrian_core::automation::AnimatedProperty,
    channel_index: usize,
    clip: &Clip,
    time: TimeTicks,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> f64 {
    let step = (SUBFRAME_TICKS_PER_FRAME / 8).max(1);
    let left = time.saturating_sub(step).max(time_min);
    let right = (time + step).min(time_max);
    if right <= left {
        return 0.0;
    }

    let left_value = property
        .evaluate(left)
        .to_channel_values()
        .get(channel_index)
        .copied()
        .unwrap_or(0.0);
    let right_value = property
        .evaluate(right)
        .to_channel_values()
        .get(channel_index)
        .copied()
        .unwrap_or(0.0);
    let seconds =
        (right - left) as f64 / SUBFRAME_TICKS_PER_FRAME as f64 * clip.position.time_base.to_f64();
    if seconds.abs() < f64::EPSILON {
        0.0
    } else {
        (right_value - left_value) / seconds
    }
}

pub(crate) fn graph_time_from_x(
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    x: f32,
) -> TimeTicks {
    if time_max <= time_min || rect.width() <= 1.0 {
        return time_min;
    }
    let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    time_min + ((time_max - time_min) as f32 * t).round() as i64
}

pub(crate) fn graph_drag_preview_map_with_snap(
    drag: &GraphKeyframeDragState,
    pointer_pos: Option<Pos2>,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    time_snap_candidates: &[TimeTicks],
    time_collision_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> (HashMap<TimeTicks, Pos2>, GraphSnapGuides) {
    let mut map = HashMap::new();
    let Some(pointer_pos) = pointer_pos else {
        return (map, GraphSnapGuides::default());
    };
    let raw_delta_time = graph_time_from_x(rect, time_min, time_max, pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let raw_delta_time = snap_graph_delta_ticks(raw_delta_time);
    let anchor_times = drag.anchors.iter().map(|anchor| anchor.time).collect::<Vec<_>>();
    let (delta_time, snapped_time) = resolve_graph_drag_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        time_collision_candidates,
        rect,
        time_min,
        time_max,
    );
    let raw_delta_value = graph_value_from_y(rect, value_min, value_max, pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    let anchor_values = drag.anchors.iter().map(|anchor| anchor.value).collect::<Vec<_>>();
    let (delta_value, snapped_value) = snap_graph_value_delta(
        raw_delta_value,
        &anchor_values,
        value_snap_candidates,
        rect,
        value_min,
        value_max,
    );
    for anchor in &drag.anchors {
        let time = (anchor.time + delta_time).max(0);
        let value = anchor.value + delta_value;
        map.insert(
            anchor.time,
            graph_point_for_keyframe(rect, time_min, time_max, value_min, value_max, time, value),
        );
    }
    (
        map,
        GraphSnapGuides { time: snapped_time, value: snapped_value },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn graph_keyframe_preview_property(
    property: &mondrian_core::automation::AnimatedProperty,
    drag: &GraphKeyframeDragState,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    time_snap_candidates: &[TimeTicks],
    time_collision_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> Option<mondrian_core::automation::AnimatedProperty> {
    let mutations = graph_keyframe_drag_property_mutations(
        drag,
        rect,
        time_min,
        time_max,
        value_min,
        value_max,
        time_snap_candidates,
        time_collision_candidates,
        value_snap_candidates,
    )?;
    let mut preview = property.clone();
    for mutation in mutations {
        preview.apply_mutation(mutation).ok()?;
    }
    Some(preview)
}

pub(crate) fn graph_speed_drag_preview_map_with_snap(
    drag: &GraphSpeedKeyframeDragState,
    pointer_pos: Option<Pos2>,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    speed_snap_candidates: &[f64],
) -> (HashMap<TimeTicks, Pos2>, GraphSnapGuides) {
    let mut map = HashMap::new();
    let Some(pointer_pos) = pointer_pos else {
        return (map, GraphSnapGuides::default());
    };
    let raw_delta_speed = graph_value_from_y(rect, value_min, value_max, pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    let anchor_speeds = drag.anchors.iter().map(|anchor| anchor.speed).collect::<Vec<_>>();
    let (delta_speed, snapped_speed) = snap_graph_value_delta(
        raw_delta_speed,
        &anchor_speeds,
        speed_snap_candidates,
        rect,
        value_min,
        value_max,
    );
    for anchor in &drag.anchors {
        let speed = anchor.speed + delta_speed;
        map.insert(
            anchor.time,
            graph_point_for_keyframe(
                rect,
                time_min,
                time_max,
                value_min,
                value_max,
                anchor.time,
                speed,
            ),
        );
    }
    (map, GraphSnapGuides { time: None, value: snapped_speed })
}

pub(crate) fn graph_keyframe_drag_mutations(
    drag: &GraphKeyframeDragState,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    clip_id: ClipId,
    time_snap_candidates: &[TimeTicks],
    time_collision_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> Option<(Vec<PropertyMutation>, Vec<AnimationKeyframeSelection>)> {
    let mutations = graph_keyframe_drag_property_mutations(
        drag,
        rect,
        time_min,
        time_max,
        value_min,
        value_max,
        time_snap_candidates,
        time_collision_candidates,
        value_snap_candidates,
    )?;
    let raw_delta_time = graph_time_from_x(rect, time_min, time_max, drag.pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let raw_delta_time = snap_graph_delta_ticks(raw_delta_time);
    let anchor_times = drag.anchors.iter().map(|anchor| anchor.time).collect::<Vec<_>>();
    let (delta_time, _) = resolve_graph_drag_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        time_collision_candidates,
        rect,
        time_min,
        time_max,
    );
    let selections = drag
        .anchors
        .iter()
        .map(|anchor| AnimationKeyframeSelection {
            clip_id,
            path: drag.path.clone(),
            time: (anchor.time + delta_time).max(0),
        })
        .collect::<Vec<_>>();

    Some((mutations, selections))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn graph_keyframe_drag_property_mutations(
    drag: &GraphKeyframeDragState,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
    time_snap_candidates: &[TimeTicks],
    time_collision_candidates: &[TimeTicks],
    value_snap_candidates: &[f64],
) -> Option<Vec<PropertyMutation>> {
    let raw_delta_time = graph_time_from_x(rect, time_min, time_max, drag.pointer_pos.x)
        - graph_time_from_x(rect, time_min, time_max, drag.start_pointer_pos.x);
    let raw_delta_time = snap_graph_delta_ticks(raw_delta_time);
    let anchor_times = drag.anchors.iter().map(|anchor| anchor.time).collect::<Vec<_>>();
    let (delta_time, _) = resolve_graph_drag_time_delta(
        raw_delta_time,
        &anchor_times,
        time_snap_candidates,
        time_collision_candidates,
        rect,
        time_min,
        time_max,
    );
    let raw_delta_value = graph_value_from_y(rect, value_min, value_max, drag.pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    let anchor_values = drag.anchors.iter().map(|anchor| anchor.value).collect::<Vec<_>>();
    let (delta_value, _) = snap_graph_value_delta(
        raw_delta_value,
        &anchor_values,
        value_snap_candidates,
        rect,
        value_min,
        value_max,
    );

    if delta_time == 0 && delta_value.abs() < f64::EPSILON {
        return None;
    }

    let mut anchors = drag.anchors.clone();
    anchors.sort_by(|a, b| {
        if delta_time >= 0 {
            b.time.cmp(&a.time)
        } else {
            a.time.cmp(&b.time)
        }
    });

    let mut mutations = Vec::new();
    for anchor in anchors {
        let new_time = (anchor.time + delta_time).max(0);
        let new_value = anchor.value + delta_value;
        if new_time != anchor.time {
            mutations.push(PropertyMutation::MoveKeyframe {
                path: drag.path.clone(),
                from_time: anchor.time,
                to_time: new_time,
            });
        }
        mutations.push(PropertyMutation::UpdateChannelKeyframeValue {
            path: drag.path.clone(),
            time: new_time,
            channel_index: drag.channel_index,
            value: new_value,
        });
    }
    Some(mutations)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn graph_speed_drag_mutations(
    drag: &GraphSpeedKeyframeDragState,
    property: &mondrian_core::automation::AnimatedProperty,
    clip: &Clip,
    time_min: TimeTicks,
    time_max: TimeTicks,
    rect: Rect,
    value_min: f64,
    value_max: f64,
    clip_id: ClipId,
    speed_snap_candidates: &[f64],
) -> Option<(Vec<PropertyMutation>, Vec<AnimationKeyframeSelection>)> {
    let raw_delta_speed = graph_value_from_y(rect, value_min, value_max, drag.pointer_pos.y)
        - graph_value_from_y(rect, value_min, value_max, drag.start_pointer_pos.y);
    let anchor_speeds = drag.anchors.iter().map(|anchor| anchor.speed).collect::<Vec<_>>();
    let (delta_speed, _) = snap_graph_value_delta(
        raw_delta_speed,
        &anchor_speeds,
        speed_snap_candidates,
        rect,
        value_min,
        value_max,
    );
    if delta_speed.abs() < f64::EPSILON {
        return None;
    }

    let channel = property.channel(drag.channel_index)?;
    let mut mutations = Vec::new();
    let mut selections = Vec::new();

    for anchor in &drag.anchors {
        let index = channel.keyframes().iter().position(|keyframe| keyframe.time == anchor.time)?;
        let keyframe = &channel.keyframes()[index];
        let target_speed = anchor.speed + delta_speed;
        let (interp_in, interp_out) = interpolations_for_target_speed(
            channel.keyframes(),
            index,
            keyframe.temporal_flags,
            clip,
            target_speed,
        )?;
        mutations.push(PropertyMutation::UpdateChannelKeyframeHandles {
            path: drag.path.clone(),
            time: anchor.time,
            channel_index: drag.channel_index,
            interp_in,
            interp_out,
        });
        selections.push(AnimationKeyframeSelection {
            clip_id,
            path: drag.path.clone(),
            time: anchor.time,
        });
    }

    let _ = (time_min, time_max);
    if mutations.is_empty() {
        None
    } else {
        Some((mutations, selections))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn graph_speed_preview_property(
    property: &mondrian_core::automation::AnimatedProperty,
    drag: &GraphSpeedKeyframeDragState,
    clip: &Clip,
    time_min: TimeTicks,
    time_max: TimeTicks,
    rect: Rect,
    value_min: f64,
    value_max: f64,
    speed_snap_candidates: &[f64],
) -> Option<mondrian_core::automation::AnimatedProperty> {
    let mut preview = property.clone();
    let mutations = graph_speed_drag_mutations(
        drag,
        property,
        clip,
        time_min,
        time_max,
        rect,
        value_min,
        value_max,
        drag.clip_id,
        speed_snap_candidates,
    )?
    .0;
    for mutation in mutations {
        let _ = preview.apply_mutation(mutation);
    }
    Some(preview)
}

pub(crate) fn graph_selection_scale_preview_map(
    drag: &GraphSelectionScaleDragState,
) -> HashMap<TimeTicks, Pos2> {
    let mut map = HashMap::new();
    let factor = graph_selection_scale_factor(drag);
    for entry in &drag.entries {
        let (time, value) = graph_selection_scaled_point(entry, drag, factor);
        map.insert(
            entry.selection.time,
            graph_point_for_keyframe(
                drag.plot_rect,
                drag.time_min,
                drag.time_max,
                drag.value_min,
                drag.value_max,
                time,
                value,
            ),
        );
    }
    map
}

pub(crate) fn graph_selection_scale_mutations(
    drag: &GraphSelectionScaleDragState,
    clip_id: ClipId,
) -> Option<(Vec<PropertyMutation>, Vec<AnimationKeyframeSelection>)> {
    let factor = graph_selection_scale_factor(drag);
    if (factor - 1.0).abs() < 1e-3 {
        return None;
    }

    let mut mutations = Vec::new();
    let mut selections = Vec::new();

    match drag.axis {
        GraphSelectionScaleAxis::Time => {
            let remapped = drag
                .entries
                .iter()
                .map(|entry| {
                    let (time, _) = graph_selection_scaled_point(entry, drag, factor);
                    (entry.selection.time, time)
                })
                .collect::<Vec<_>>();

            let mut dedup = std::collections::HashSet::new();
            if remapped.iter().any(|(_, new_time)| !dedup.insert(*new_time)) {
                return None;
            }

            let mut ordered = remapped.clone();
            ordered.sort_by(|a, b| {
                if factor >= 1.0 {
                    b.0.cmp(&a.0)
                } else {
                    a.0.cmp(&b.0)
                }
            });
            for (old_time, new_time) in ordered {
                if old_time != new_time {
                    mutations.push(PropertyMutation::MoveKeyframe {
                        path: drag.path.clone(),
                        from_time: old_time,
                        to_time: new_time,
                    });
                }
            }
            selections.extend(
                drag.entries.iter().zip(remapped).map(|(entry, (_, new_time))| {
                    AnimationKeyframeSelection {
                        clip_id,
                        path: entry.selection.path.clone(),
                        time: new_time,
                    }
                }),
            );
        }
        GraphSelectionScaleAxis::Value => {
            for entry in &drag.entries {
                let (_, value) = graph_selection_scaled_point(entry, drag, factor);
                mutations.push(PropertyMutation::UpdateChannelKeyframeValue {
                    path: drag.path.clone(),
                    time: entry.selection.time,
                    channel_index: drag.channel_index,
                    value,
                });
                selections.push(AnimationKeyframeSelection {
                    clip_id,
                    path: entry.selection.path.clone(),
                    time: entry.selection.time,
                });
            }
        }
    }

    if mutations.is_empty() {
        None
    } else {
        Some((mutations, selections))
    }
}

pub(crate) fn graph_selection_scale_factor(drag: &GraphSelectionScaleDragState) -> f64 {
    match drag.axis {
        GraphSelectionScaleAxis::Time => {
            let min_time = drag.entries.iter().map(|entry| entry.selection.time).min().unwrap_or(0);
            let max_time =
                drag.entries.iter().map(|entry| entry.selection.time).max().unwrap_or(min_time);
            let (pivot, start) = match drag.edge {
                GraphSelectionScaleEdge::Min => (max_time, min_time),
                GraphSelectionScaleEdge::Max => (min_time, max_time),
            };
            let current = graph_time_from_x(
                drag.plot_rect,
                drag.time_min,
                drag.time_max,
                drag.pointer_pos.x,
            );
            scale_factor_from_axis(start as f64, pivot as f64, current as f64, 0.1, 10.0)
        }
        GraphSelectionScaleAxis::Value => {
            let min_value = drag
                .entries
                .iter()
                .map(|entry| entry.channel_value)
                .fold(f64::INFINITY, f64::min);
            let max_value = drag
                .entries
                .iter()
                .map(|entry| entry.channel_value)
                .fold(f64::NEG_INFINITY, f64::max);
            let (pivot, start) = match drag.edge {
                GraphSelectionScaleEdge::Min => (max_value, min_value),
                GraphSelectionScaleEdge::Max => (min_value, max_value),
            };
            let current = graph_value_from_y(
                drag.plot_rect,
                drag.value_min,
                drag.value_max,
                drag.pointer_pos.y,
            );
            scale_factor_from_axis(start, pivot, current, 0.1, 10.0)
        }
    }
}

pub(crate) fn graph_selection_scaled_point(
    entry: &SelectedGraphKeyframeData,
    drag: &GraphSelectionScaleDragState,
    factor: f64,
) -> (TimeTicks, f64) {
    match drag.axis {
        GraphSelectionScaleAxis::Time => {
            let min_time = drag.entries.iter().map(|entry| entry.selection.time).min().unwrap_or(0);
            let max_time =
                drag.entries.iter().map(|entry| entry.selection.time).max().unwrap_or(min_time);
            let pivot = match drag.edge {
                GraphSelectionScaleEdge::Min => max_time as f64,
                GraphSelectionScaleEdge::Max => min_time as f64,
            };
            let scaled = pivot + (entry.selection.time as f64 - pivot) * factor;
            (
                snap_time_ticks(scaled.round() as TimeTicks).max(0),
                entry.channel_value,
            )
        }
        GraphSelectionScaleAxis::Value => {
            let min_value = drag
                .entries
                .iter()
                .map(|entry| entry.channel_value)
                .fold(f64::INFINITY, f64::min);
            let max_value = drag
                .entries
                .iter()
                .map(|entry| entry.channel_value)
                .fold(f64::NEG_INFINITY, f64::max);
            let pivot = match drag.edge {
                GraphSelectionScaleEdge::Min => max_value,
                GraphSelectionScaleEdge::Max => min_value,
            };
            (
                entry.selection.time,
                pivot + (entry.channel_value - pivot) * factor,
            )
        }
    }
}

pub(crate) fn interpolations_for_target_speed(
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    temporal_flags: KeyframeTemporalFlags,
    clip: &Clip,
    target_speed: f64,
) -> Option<(KeyframeInterpolation, KeyframeInterpolation)> {
    let keyframe = keyframes.get(index)?;
    let mut interp_in = keyframe.interp_in;
    let mut interp_out = keyframe.interp_out;

    if index > 0 {
        let previous = &keyframes[index - 1];
        interp_in = KeyframeInterpolation::Bezier(speed_handle_for_segment(
            previous,
            keyframe,
            clip,
            target_speed,
            handle_time_offset_in(keyframe.interp_in),
        ));
    }
    if let Some(next) = keyframes.get(index + 1) {
        interp_out = KeyframeInterpolation::Bezier(speed_handle_for_segment(
            keyframe,
            next,
            clip,
            target_speed,
            handle_time_offset_out(keyframe.interp_out),
        ));
    }

    if temporal_flags.continuous
        && !temporal_flags.broken_handles
        && index > 0
        && index + 1 < keyframes.len()
    {
        let previous = &keyframes[index - 1];
        let next = &keyframes[index + 1];
        interp_in = KeyframeInterpolation::Bezier(speed_handle_for_segment(
            previous,
            keyframe,
            clip,
            target_speed,
            handle_time_offset_in(keyframe.interp_in),
        ));
        interp_out = KeyframeInterpolation::Bezier(speed_handle_for_segment(
            keyframe,
            next,
            clip,
            target_speed,
            handle_time_offset_out(keyframe.interp_out),
        ));
    }

    Some((interp_in, interp_out))
}

pub(crate) fn speed_handle_for_segment(
    start: &mondrian_core::automation::Keyframe<f64>,
    end: &mondrian_core::automation::Keyframe<f64>,
    clip: &Clip,
    target_speed: f64,
    time_offset: f64,
) -> BezierHandle {
    let dv = end.value - start.value;
    let dt_seconds = (end.time - start.time) as f64 / SUBFRAME_TICKS_PER_FRAME as f64
        * clip.position.time_base.to_f64();
    if dv.abs() < f64::EPSILON || dt_seconds.abs() < f64::EPSILON {
        BezierHandle { time_offset, value_offset: 0.0 }
    } else {
        let value_offset = (target_speed * dt_seconds / dv * time_offset).clamp(-2.0, 2.0);
        BezierHandle { time_offset, value_offset }
    }
}

pub(crate) fn handle_time_offset_in(interpolation: KeyframeInterpolation) -> f64 {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle.time_offset.clamp(-0.95, -0.05),
        _ => -1.0 / 3.0,
    }
}

pub(crate) fn handle_time_offset_out(interpolation: KeyframeInterpolation) -> f64 {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle.time_offset.clamp(0.05, 0.95),
        _ => 1.0 / 3.0,
    }
}

pub(crate) fn scale_factor_from_axis(
    start: f64,
    pivot: f64,
    current: f64,
    min_factor: f64,
    max_factor: f64,
) -> f64 {
    let denom = start - pivot;
    if denom.abs() < f64::EPSILON {
        1.0
    } else {
        ((current - pivot) / denom).clamp(min_factor, max_factor)
    }
}

pub(crate) fn snap_graph_time_delta(
    raw_delta: TimeTicks,
    anchor_times: &[TimeTicks],
    candidate_times: &[TimeTicks],
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> (TimeTicks, Option<TimeTicks>) {
    if anchor_times.is_empty() || candidate_times.is_empty() || rect.width() <= 1.0 {
        return (raw_delta, None);
    }
    let threshold_ticks = (((time_max - time_min).max(1) as f32)
        * (tokens::timeline_drag_snap_pixels() / rect.width()))
    .ceil()
    .max(1.0) as TimeTicks;

    let mut best: Option<(TimeTicks, TimeTicks)> = None;
    for anchor_time in anchor_times {
        let moved = (*anchor_time + raw_delta).max(0);
        for candidate in candidate_times {
            let delta = candidate.saturating_sub(*anchor_time);
            let diff = (candidate - moved).abs();
            if diff > threshold_ticks {
                continue;
            }
            if best.as_ref().map(|(_, best_diff)| diff < *best_diff).unwrap_or(true) {
                best = Some((delta, diff));
            }
        }
    }

    if let Some((delta, _)) = best {
        let snapped_anchor = anchor_times[0].saturating_add(delta).max(0);
        (delta, Some(snapped_anchor))
    } else {
        (raw_delta, None)
    }
}

pub(crate) fn graph_drag_time_delta_is_valid(
    delta: TimeTicks,
    anchor_times: &[TimeTicks],
    occupied_times: &[TimeTicks],
) -> bool {
    let mut moved_times = HashSet::with_capacity(anchor_times.len());
    for anchor_time in anchor_times {
        let moved_time = (*anchor_time + delta).max(0);
        if occupied_times.contains(&moved_time) || !moved_times.insert(moved_time) {
            return false;
        }
    }
    true
}

pub(crate) fn resolve_graph_drag_time_delta(
    raw_delta: TimeTicks,
    anchor_times: &[TimeTicks],
    snap_candidates: &[TimeTicks],
    occupied_times: &[TimeTicks],
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
) -> (TimeTicks, Option<TimeTicks>) {
    let (snapped_delta, snapped_time) = snap_graph_time_delta(
        raw_delta,
        anchor_times,
        snap_candidates,
        rect,
        time_min,
        time_max,
    );

    if snapped_time.is_some()
        && graph_drag_time_delta_is_valid(snapped_delta, anchor_times, occupied_times)
    {
        return (snapped_delta, snapped_time);
    }

    if graph_drag_time_delta_is_valid(raw_delta, anchor_times, occupied_times) {
        (raw_delta, None)
    } else {
        (0, None)
    }
}

pub(crate) fn snap_graph_value_delta(
    raw_delta: f64,
    anchor_values: &[f64],
    candidate_values: &[f64],
    rect: Rect,
    value_min: f64,
    value_max: f64,
) -> (f64, Option<f64>) {
    if anchor_values.is_empty() || candidate_values.is_empty() || rect.height() <= 1.0 {
        return (raw_delta, None);
    }
    let threshold_value = ((value_max - value_min).abs()
        * (tokens::timeline_drag_snap_pixels() / rect.height()).max(0.0) as f64)
        .max(1e-6);

    let mut best: Option<(f64, f64)> = None;
    for anchor_value in anchor_values {
        let moved = *anchor_value + raw_delta;
        for candidate in candidate_values {
            let delta = *candidate - *anchor_value;
            let diff = (*candidate - moved).abs();
            if diff > threshold_value {
                continue;
            }
            if best.as_ref().map(|(_, best_diff)| diff < *best_diff).unwrap_or(true) {
                best = Some((delta, diff));
            }
        }
    }

    if let Some((delta, _)) = best {
        (delta, Some(anchor_values[0] + delta))
    } else {
        (raw_delta, None)
    }
}

pub(crate) fn snap_graph_delta_ticks(delta_ticks: TimeTicks) -> TimeTicks {
    ((delta_ticks as f64 / SUBFRAME_TICKS_PER_FRAME as f64).round() as i64)
        * SUBFRAME_TICKS_PER_FRAME
}

pub(crate) fn snap_time_ticks(time_ticks: TimeTicks) -> TimeTicks {
    ((time_ticks as f64 / SUBFRAME_TICKS_PER_FRAME as f64).round() as i64)
        * SUBFRAME_TICKS_PER_FRAME
}

pub(crate) fn graph_handles_for_keyframe(
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) -> Vec<GraphEditorHandle> {
    let mut handles = Vec::new();
    let keyframe = &keyframes[index];
    if matches!(keyframe.interp_in, KeyframeInterpolation::Bezier(_)) && index > 0 {
        let previous = &keyframes[index - 1];
        let handle = handle_from_in(keyframe.interp_in);
        let point = Pos2::new(
            graph_x_for_time(
                rect,
                time_min,
                time_max,
                keyframe.time
                    + ((keyframe.time - previous.time) as f64 * handle.time_offset) as i64,
            ),
            graph_y_for_value(
                rect,
                value_min,
                value_max,
                previous.value + (keyframe.value - previous.value) * (1.0 + handle.value_offset),
            ),
        );
        handles.push(GraphEditorHandle { kind: GraphHandleKind::In, position: point });
    }
    if matches!(keyframe.interp_out, KeyframeInterpolation::Bezier(_))
        && index + 1 < keyframes.len()
    {
        let next = &keyframes[index + 1];
        let handle = handle_from_out(keyframe.interp_out);
        let point = Pos2::new(
            graph_x_for_time(
                rect,
                time_min,
                time_max,
                keyframe.time + ((next.time - keyframe.time) as f64 * handle.time_offset) as i64,
            ),
            graph_y_for_value(
                rect,
                value_min,
                value_max,
                keyframe.value + (next.value - keyframe.value) * handle.value_offset,
            ),
        );
        handles.push(GraphEditorHandle { kind: GraphHandleKind::Out, position: point });
    }
    handles
}

pub(crate) fn handle_from_out(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle,
        KeyframeInterpolation::Linear => BezierHandle { time_offset: 1.0 / 3.0, value_offset: 0.0 },
        KeyframeInterpolation::Hold => BezierHandle { time_offset: 0.0, value_offset: 0.0 },
    }
}

pub(crate) fn handle_from_in(interpolation: KeyframeInterpolation) -> BezierHandle {
    match interpolation {
        KeyframeInterpolation::Bezier(handle) => handle,
        KeyframeInterpolation::Linear => {
            BezierHandle { time_offset: -1.0 / 3.0, value_offset: 0.0 }
        }
        KeyframeInterpolation::Hold => BezierHandle { time_offset: 0.0, value_offset: 0.0 },
    }
}

pub(crate) fn preview_handle_position(
    kind: GraphHandleKind,
    pointer_pos: Pos2,
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    temporal_flags: KeyframeTemporalFlags,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) -> Option<Pos2> {
    let (interp_in, interp_out) = compute_handle_interpolation(
        kind,
        pointer_pos,
        keyframes,
        index,
        temporal_flags,
        rect,
        time_min,
        time_max,
        value_min,
        value_max,
    )?;
    let fake = mondrian_core::automation::Keyframe {
        id: keyframes[index].id,
        time: keyframes[index].time,
        value: keyframes[index].value,
        interp_in,
        interp_out,
        temporal_flags: keyframes[index].temporal_flags,
    };
    graph_handles_for_keyframe(
        &replace_keyframe(keyframes, index, fake),
        index,
        rect,
        time_min,
        time_max,
        value_min,
        value_max,
    )
    .into_iter()
    .find(|handle| handle.kind == kind)
    .map(|handle| handle.position)
}

pub(crate) fn replace_keyframe(
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    keyframe: mondrian_core::automation::Keyframe<f64>,
) -> Vec<mondrian_core::automation::Keyframe<f64>> {
    let mut replaced = keyframes.to_vec();
    replaced[index] = keyframe;
    replaced
}

pub(crate) fn compute_handle_interpolation(
    kind: GraphHandleKind,
    pointer_pos: Pos2,
    keyframes: &[mondrian_core::automation::Keyframe<f64>],
    index: usize,
    temporal_flags: KeyframeTemporalFlags,
    rect: Rect,
    time_min: TimeTicks,
    time_max: TimeTicks,
    value_min: f64,
    value_max: f64,
) -> Option<(KeyframeInterpolation, KeyframeInterpolation)> {
    let keyframe = &keyframes[index];
    let mut interp_in = keyframe.interp_in;
    let mut interp_out = keyframe.interp_out;
    match kind {
        GraphHandleKind::Out if index + 1 < keyframes.len() => {
            let next = &keyframes[index + 1];
            let segment_width = (next.time - keyframe.time).max(1) as f32;
            let dx = ((pointer_pos.x - graph_x_for_time(rect, time_min, time_max, keyframe.time))
                / (graph_x_for_time(rect, time_min, time_max, next.time)
                    - graph_x_for_time(rect, time_min, time_max, keyframe.time))
                .max(1.0))
            .clamp(0.05, 0.95);
            let dy = normalized_handle_value(
                pointer_pos.y,
                keyframe.value,
                next.value,
                rect,
                value_min,
                value_max,
            )
            .clamp(-2.0, 2.0);
            let _ = segment_width;
            interp_out = KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: dx as f64,
                value_offset: dy as f64,
            });
            if temporal_flags.continuous && !temporal_flags.broken_handles {
                if let Some(previous) = keyframes.get(index.wrapping_sub(1)).filter(|_| index > 0) {
                    interp_in = KeyframeInterpolation::Bezier(mirror_handle_for_in(
                        previous,
                        keyframe,
                        next,
                        BezierHandle { time_offset: dx as f64, value_offset: dy as f64 },
                    ));
                }
            }
        }
        GraphHandleKind::In if index > 0 => {
            let previous = &keyframes[index - 1];
            let dx = ((pointer_pos.x - graph_x_for_time(rect, time_min, time_max, keyframe.time))
                / (graph_x_for_time(rect, time_min, time_max, keyframe.time)
                    - graph_x_for_time(rect, time_min, time_max, previous.time))
                .max(1.0))
            .clamp(-0.95, -0.05);
            let dy = normalized_handle_value_from_end(
                pointer_pos.y,
                previous.value,
                keyframe.value,
                rect,
                value_min,
                value_max,
            )
            .clamp(-2.0, 2.0);
            interp_in = KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: dx as f64,
                value_offset: dy as f64,
            });
            if temporal_flags.continuous && !temporal_flags.broken_handles {
                if let Some(next) = keyframes.get(index + 1) {
                    interp_out = KeyframeInterpolation::Bezier(mirror_handle_for_out(
                        previous,
                        keyframe,
                        next,
                        BezierHandle { time_offset: dx as f64, value_offset: dy as f64 },
                    ));
                }
            }
        }
        _ => return None,
    }
    Some((interp_in, interp_out))
}

pub(crate) fn mirror_handle_for_in(
    previous: &mondrian_core::automation::Keyframe<f64>,
    current: &mondrian_core::automation::Keyframe<f64>,
    next: &mondrian_core::automation::Keyframe<f64>,
    out_handle: BezierHandle,
) -> BezierHandle {
    let out_slope = actual_slope_from_out(current, next, out_handle);
    let time_offset = -out_handle.time_offset.abs().clamp(0.05, 0.95);
    let dt_prev = (current.time - previous.time).max(1) as f64;
    let dv_prev = current.value - previous.value;
    let value_offset = if dv_prev.abs() < f64::EPSILON {
        0.0
    } else {
        (out_slope * time_offset * dt_prev / dv_prev).clamp(-2.0, 2.0)
    };
    BezierHandle { time_offset, value_offset }
}

pub(crate) fn mirror_handle_for_out(
    previous: &mondrian_core::automation::Keyframe<f64>,
    current: &mondrian_core::automation::Keyframe<f64>,
    next: &mondrian_core::automation::Keyframe<f64>,
    in_handle: BezierHandle,
) -> BezierHandle {
    let in_slope = actual_slope_from_in(previous, current, in_handle);
    let time_offset = in_handle.time_offset.abs().clamp(0.05, 0.95);
    let dt_next = (next.time - current.time).max(1) as f64;
    let dv_next = next.value - current.value;
    let value_offset = if dv_next.abs() < f64::EPSILON {
        0.0
    } else {
        (in_slope * time_offset * dt_next / dv_next).clamp(-2.0, 2.0)
    };
    BezierHandle { time_offset, value_offset }
}

pub(crate) fn actual_slope_from_out(
    current: &mondrian_core::automation::Keyframe<f64>,
    next: &mondrian_core::automation::Keyframe<f64>,
    handle: BezierHandle,
) -> f64 {
    let dt = (next.time - current.time).max(1) as f64;
    let dv = next.value - current.value;
    let dx = handle.time_offset.abs().max(0.05);
    if dv.abs() < f64::EPSILON {
        0.0
    } else {
        handle.value_offset * dv / (dx * dt)
    }
}

pub(crate) fn actual_slope_from_in(
    previous: &mondrian_core::automation::Keyframe<f64>,
    current: &mondrian_core::automation::Keyframe<f64>,
    handle: BezierHandle,
) -> f64 {
    let dt = (current.time - previous.time).max(1) as f64;
    let dv = current.value - previous.value;
    let dx = handle.time_offset.abs().max(0.05);
    if dv.abs() < f64::EPSILON {
        0.0
    } else {
        handle.value_offset * dv / (dx * dt)
    }
}

pub(crate) fn normalized_handle_value(
    pointer_y: f32,
    start: f64,
    end: f64,
    rect: Rect,
    value_min: f64,
    value_max: f64,
) -> f32 {
    let actual_value = graph_value_from_y(rect, value_min, value_max, pointer_y);
    if (end - start).abs() < f64::EPSILON {
        0.0
    } else {
        ((actual_value - start) / (end - start)) as f32
    }
}

pub(crate) fn normalized_handle_value_from_end(
    pointer_y: f32,
    start: f64,
    end: f64,
    rect: Rect,
    value_min: f64,
    value_max: f64,
) -> f32 {
    let actual_value = graph_value_from_y(rect, value_min, value_max, pointer_y);
    if (end - start).abs() < f64::EPSILON {
        0.0
    } else {
        ((actual_value - start) / (end - start) - 1.0) as f32
    }
}

pub(crate) fn graph_value_from_y(rect: Rect, value_min: f64, value_max: f64, y: f32) -> f64 {
    let t = ((rect.bottom() - y) / rect.height()).clamp(0.0, 1.0);
    value_min + (value_max - value_min) * t as f64
}
