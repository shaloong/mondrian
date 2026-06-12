//! Drawing helpers.
use super::*;

pub(crate) fn quantize_dimension(value: u32, step: u32) -> u32 {
    let step = step.max(1);
    let rounded = ((value + (step / 2)) / step).saturating_mul(step);
    if rounded % 2 == 1 {
        rounded.saturating_sub(1).max(1)
    } else {
        rounded.max(1)
    }
}

/// Draw action-safe (90%) and title-safe (80%) overlays.
pub(crate) fn draw_safe_margins(
    painter: &egui::Painter,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    seq: &Sequence,
) {
    let w = seq.settings.resolution.width as f32;
    let h = seq.settings.resolution.height as f32;
    let action_margin = seq.settings.action_safe_margin * 0.5;
    let title_margin = seq.settings.title_safe_margin * 0.5;

    let action_rect = Rect::from_min_max(
        ct.seq_to_screen(w * action_margin, h * action_margin),
        ct.seq_to_screen(w * (1.0 - action_margin), h * (1.0 - action_margin)),
    );
    let title_rect = Rect::from_min_max(
        ct.seq_to_screen(w * title_margin, h * title_margin),
        ct.seq_to_screen(w * (1.0 - title_margin), h * (1.0 - title_margin)),
    );

    let action_color = egui::Color32::from_white_alpha(40);
    let title_color = egui::Color32::from_white_alpha(30);

    painter.rect_stroke(
        action_rect,
        egui::CornerRadius::same(0),
        egui::Stroke::new(1.0, action_color),
        egui::StrokeKind::Inside,
    );
    painter.rect_stroke(
        title_rect,
        egui::CornerRadius::same(0),
        egui::Stroke::new(1.0, title_color),
        egui::StrokeKind::Inside,
    );
}

/// Uniformly scale an affine matrix [a, b, tx, c, d, ty] by factor.
pub(crate) fn scale_affine(t: [f32; 6], factor: f32) -> [f32; 6] {
    [
        t[0] * factor,
        t[1] * factor,
        t[2] * factor,
        t[3] * factor,
        t[4] * factor,
        t[5] * factor,
    ]
}

pub(crate) fn is_near_corner(rect: Option<Rect>, point: Pos2, radius: f32) -> bool {
    rect.is_some_and(|r| {
        let corners = [
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
        ];
        corners.iter().any(|&c| c.distance(point) <= radius)
    })
}

/// Screen bounds for a clip, using actual media dimensions from the asset library.
pub(crate) fn clip_screen_bounds_with_media(
    clip: &mondrian_timeline::clip::Clip,
    mat: glam::Mat3,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    state: &AppState,
) -> Option<Rect> {
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    let corners = [
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::new(mw, 0.0),
        glam::Vec2::new(mw, mh),
        glam::Vec2::new(0.0, mh),
    ];
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for &c in &corners {
        let t = mat * c.extend(1.0);
        let p = ct.seq_to_screen(t.x, t.y);
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    if min_x >= max_x || min_y >= max_y {
        None
    } else {
        Some(Rect::from_min_max(
            Pos2::new(min_x, min_y),
            Pos2::new(max_x, max_y),
        ))
    }
}

#[allow(dead_code)]
pub(crate) fn clip_screen_bounds(
    _clip: &mondrian_timeline::clip::Clip,
    mat: glam::Mat3,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
) -> Option<Rect> {
    let corners = [
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::new(1.0, 0.0),
        glam::Vec2::new(1.0, 1.0),
        glam::Vec2::new(0.0, 1.0),
    ];
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for c in &corners {
        let t = mat * c.extend(1.0);
        let p = ct.seq_to_screen(t.x, t.y);
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    if min_x >= max_x || min_y >= max_y {
        return None;
    }
    Some(Rect::from_min_max(
        Pos2::new(min_x, min_y),
        Pos2::new(max_x, max_y),
    ))
}

/// Draw corner handles for the selected clip on the canvas.
pub(crate) fn draw_transform_handles(
    painter: &egui::Painter,
    state: &AppState,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    clip_id: mondrian_core::types::ClipId,
) {
    let Some(seq) = state.sequence.as_ref() else {
        return;
    };
    let Some(library) = state.asset_library.as_ref() else {
        return;
    };
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let Some(ac) = active.iter().find(|a| a.clip.id == clip_id) else {
        return;
    };

    // Get media dimensions for the bounding box.
    let (mw, mh) = library
        .get_asset(ac.clip.asset_id)
        .ok()
        .flatten()
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));

    let corners = [
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::new(mw, 0.0),
        glam::Vec2::new(mw, mh),
        glam::Vec2::new(0.0, mh),
    ];
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for &c in &corners {
        let t = ac.transform_matrix * c.extend(1.0);
        let p = ct.seq_to_screen(t.x, t.y);
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    if min_x >= max_x || min_y >= max_y {
        return;
    }
    let rect = Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y));

    let handle_color = egui::Color32::from_rgb(0, 180, 255);
    let handle_radius = 5.0;
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(0),
        egui::Stroke::new(2.0, handle_color),
        egui::StrokeKind::Inside,
    );
    for &c in &corners {
        painter.circle_filled(c, handle_radius, handle_color);
    }
    // Draw anchor point (position = anchor's location in sequence space).
    let anchor_pos = ac.clip.transform.get_position(current);
    let ap = ct.seq_to_screen(anchor_pos.x, anchor_pos.y);
    painter.circle_filled(ap, 5.0, egui::Color32::from_rgb(255, 200, 0));
}

/// 绘制棋盘格背景（表示空帧/透明）
pub(crate) fn draw_checkerboard(painter: &egui::Painter, rect: Rect) {
    let cell = 12.0_f32;
    let cols = ((rect.width() / cell).ceil() as usize).max(1);
    let rows = ((rect.height() / cell).ceil() as usize).max(1);

    for row in 0..rows {
        for col in 0..cols {
            let color = if (row + col) % 2 == 0 {
                palette::bg_surface()
            } else {
                palette::bg_surface_hover()
            };
            let x = rect.left() + col as f32 * cell;
            let y = rect.top() + row as f32 * cell;
            let cell_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(cell));
            painter.rect_filled(cell_rect, 0.0, color);
        }
    }
}

pub(crate) fn draw_empty_canvas_meta(
    painter: &egui::Painter,
    rect: Rect,
    state: &AppState,
    current_frame: i64,
) {
    let fps = state
        .sequence
        .as_ref()
        .map(|s| s.settings.frame_rate)
        .unwrap_or(Rational::new(25, 1));
    let tc = TimeCode::new(current_frame.max(0), Rational::new(fps.den, fps.num)).to_smpte();

    let resolution = state
        .sequence
        .as_ref()
        .map(|s| {
            format!(
                "{}x{}",
                s.settings.resolution.width, s.settings.resolution.height
            )
        })
        .unwrap_or_else(|| "--x--".to_string());

    painter.text(
        rect.center() + Vec2::new(0.0, -8.0),
        egui::Align2::CENTER_CENTER,
        tc,
        typography::mono_large(),
        palette::text_muted().gamma_multiply(0.6),
    );
    painter.text(
        rect.center() + Vec2::new(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        resolution,
        typography::body(),
        palette::text_muted().gamma_multiply(0.6),
    );
}

pub(crate) fn draw_mask_overlays(
    painter: &egui::Painter,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    state: &AppState,
    timeline_frame: i64,
    selected_mask: Option<(
        mondrian_effects::mask::MaskId,
        mondrian_core::types::ClipId,
        mondrian_core::types::TrackId,
    )>,
) {
    let Some(seq) = state.sequence.as_ref() else {
        return;
    };
    let Some(lib) = state.asset_library.as_ref() else {
        return;
    };
    let active = seq.active_clips_at(TimeCode::new(timeline_frame.max(0), seq.time_base()));
    if active.is_empty() {
        return;
    }
    let to_scr = |x: f32, y: f32| ct.seq_to_screen(x, y);
    let ticks = timecode_to_ticks(TimeCode::new(timeline_frame.max(0), seq.time_base()));
    let colors = [
        egui::Color32::from_rgb(0, 180, 240),
        egui::Color32::from_rgb(220, 120, 0),
        egui::Color32::from_rgb(120, 200, 80),
        egui::Color32::from_rgb(200, 80, 200),
    ];
    for ac in &active {
        if ac.clip.masks.is_empty() {
            continue;
        }
        let mat = ac.transform_matrix;
        // Get media dimensions for normalized → pixel conversion.
        let (mw, mh) = lib
            .get_asset(ac.clip.asset_id)
            .ok()
            .flatten()
            .and_then(|a| a.media_info.primary_video().cloned())
            .map(|v| (v.width as f32, v.height as f32))
            .unwrap_or((1.0, 1.0));
        for (i, mask) in ac.clip.masks.iter().enumerate() {
            if !mask.enabled {
                continue;
            }
            let is_selected =
                selected_mask.is_some_and(|(mid, cid, _)| mid == mask.id && cid == ac.clip.id);
            let p = mask.evaluate_at(ticks);
            let c = colors[i % colors.len()];
            let st = if is_selected {
                egui::Stroke::new(3.0, c)
            } else {
                egui::Stroke::new(2.0, c)
            };
            match &p.shape {
                MaskShape::Rectangle { x, y, width, height, .. } => {
                    let crn = [
                        glam::Vec2::new(x * mw, y * mh),
                        glam::Vec2::new((x + width) * mw, y * mh),
                        glam::Vec2::new((x + width) * mw, (y + height) * mh),
                        glam::Vec2::new(x * mw, (y + height) * mh),
                    ];
                    draw_mask_polygon(painter, &crn, &mat, &to_scr, st, true);
                    // Draw corner handles for selected mask.
                    if is_selected {
                        for &corner in &crn {
                            let sc = mask_xform(corner, &mat, &to_scr);
                            painter.rect_filled(
                                egui::Rect::from_center_size(sc, egui::vec2(8.0, 8.0)),
                                2.0,
                                egui::Color32::WHITE,
                            );
                        }
                    }
                }
                MaskShape::Ellipse { center, radii } => {
                    let n = 64usize;
                    let mut pts = Vec::with_capacity(n + 1);
                    let cx = center.x * mw;
                    let cy = center.y * mh;
                    let rx = radii.x * mw;
                    let ry = radii.y * mh;
                    for s in 0..=n {
                        let a = s as f32 * std::f32::consts::TAU / n as f32;
                        pts.push(glam::Vec2::new(cx + rx * a.cos(), cy + ry * a.sin()));
                    }
                    draw_mask_polygon(painter, &pts, &mat, &to_scr, st, false);
                    // Draw bounding-box handles for selected ellipse.
                    if is_selected {
                        let bbox_corners = [
                            glam::Vec2::new(cx - rx, cy - ry),
                            glam::Vec2::new(cx + rx, cy - ry),
                            glam::Vec2::new(cx + rx, cy + ry),
                            glam::Vec2::new(cx - rx, cy + ry),
                        ];
                        for &bc in &bbox_corners {
                            let sc = mask_xform(bc, &mat, &to_scr);
                            painter.rect_filled(
                                egui::Rect::from_center_size(sc, egui::vec2(8.0, 8.0)),
                                2.0,
                                egui::Color32::WHITE,
                            );
                        }
                    }
                }
                MaskShape::Path { points, closed } => {
                    // Convert normalized coords to media-pixel space and render Bézier segments.
                    let px_pts: Vec<mondrian_effects::mask::BezierPoint> = points
                        .iter()
                        .map(|p| mondrian_effects::mask::BezierPoint {
                            position: glam::Vec2::new(p.position.x * mw, p.position.y * mh),
                            control_in: glam::Vec2::new(p.control_in.x * mw, p.control_in.y * mh),
                            control_out: glam::Vec2::new(
                                p.control_out.x * mw,
                                p.control_out.y * mh,
                            ),
                        })
                        .collect();
                    let segs = mask_path_segments(&px_pts, *closed);
                    for &(a, b) in &segs {
                        let ta = mask_xform(a, &mat, &to_scr);
                        let tb = mask_xform(b, &mat, &to_scr);
                        painter.line_segment([ta, tb], st);
                    }
                    // Draw anchor points as white squares.
                    for pt in &px_pts {
                        let sp = mask_xform(pt.position, &mat, &to_scr);
                        painter.rect_filled(
                            egui::Rect::from_center_size(sp, egui::vec2(8.0, 8.0)),
                            2.0,
                            egui::Color32::WHITE,
                        );
                        // Draw control handle lines and endpoints.
                        let handle_stroke = egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 150),
                        );
                        if pt.control_in.length_squared() > 0.01 {
                            let cp = mask_xform(pt.position + pt.control_in, &mat, &to_scr);
                            painter.line_segment([sp, cp], handle_stroke);
                            painter.circle_filled(cp, 3.0, egui::Color32::WHITE);
                        }
                        if pt.control_out.length_squared() > 0.01 {
                            let cp = mask_xform(pt.position + pt.control_out, &mat, &to_scr);
                            painter.line_segment([sp, cp], handle_stroke);
                            painter.circle_filled(cp, 3.0, egui::Color32::WHITE);
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn mask_path_segments(
    points: &[BezierPoint],
    closed: bool,
) -> Vec<(glam::Vec2, glam::Vec2)> {
    let mut out = Vec::new();
    let n = points.len();
    for i in 0..n {
        let ni = if i + 1 < n {
            i + 1
        } else if closed {
            0
        } else {
            break;
        };
        let a = points[i];
        let b = points[ni];
        let s = 32usize;
        let mut prev = a.position;
        for k in 1..=s {
            let t = k as f32 / s as f32;
            let u = 1.0 - t;
            let pt = a.position * u * u * u
                + (a.position + a.control_out) * (3.0 * u * u * t)
                + (b.position + b.control_in) * (3.0 * u * t * t)
                + b.position * t * t * t;
            out.push((prev, pt));
            prev = pt;
        }
    }
    out
}

pub(crate) fn mask_xform(
    pt: glam::Vec2,
    mat: &glam::Mat3,
    to_scr: &impl Fn(f32, f32) -> Pos2,
) -> Pos2 {
    let t = *mat * pt.extend(1.0);
    to_scr(t.x, t.y)
}

pub(crate) fn draw_mask_polygon(
    painter: &egui::Painter,
    pts: &[glam::Vec2],
    mat: &glam::Mat3,
    to_scr: &impl Fn(f32, f32) -> Pos2,
    stroke: egui::Stroke,
    closed: bool,
) {
    if pts.len() < 2 {
        return;
    }
    let cp: Vec<Pos2> = pts.iter().map(|&p| mask_xform(p, mat, to_scr)).collect();
    let n = if closed { cp.len() } else { cp.len() - 1 };
    for i in 0..n {
        painter.line_segment([cp[i], cp[(i + 1) % cp.len()]], stroke);
    }
}

/// Mask tool toolbar — a thin row of tool buttons above the canvas.
pub(crate) fn draw_mask_toolbar(ui: &mut egui::Ui, panel: &mut ViewerPanel) {
    let btn_size = [26.0, 20.0];
    egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(4, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;

                // Selection tool
                let sel_active = panel.mask_tool.is_none();
                if theme::icon_ghost_toggle_button(ui, btn_size, theme::UiIcon::Cursor, sel_active)
                    .on_hover_text("选择工具 (V)")
                    .clicked()
                {
                    panel.mask_tool = None;
                    panel.mask_draw = None;
                    panel.mask_edit = None;
                    panel.selected_mask = None;
                }

                // Rectangle mask
                let rect_active = panel.mask_tool == Some(MaskTool::Rect);
                if theme::icon_ghost_toggle_button(
                    ui,
                    btn_size,
                    theme::UiIcon::Rectangle,
                    rect_active,
                )
                .on_hover_text("矩形蒙版 (R)")
                .clicked()
                {
                    panel.mask_tool = if rect_active {
                        None
                    } else {
                        Some(MaskTool::Rect)
                    };
                    panel.mask_draw = None;
                    panel.mask_edit = None;
                    panel.selected_mask = None;
                }

                // Ellipse mask
                let ell_active = panel.mask_tool == Some(MaskTool::Ellipse);
                if theme::icon_ghost_toggle_button(ui, btn_size, theme::UiIcon::Circle, ell_active)
                    .on_hover_text("椭圆蒙版 (E)")
                    .clicked()
                {
                    panel.mask_tool = if ell_active {
                        None
                    } else {
                        Some(MaskTool::Ellipse)
                    };
                    panel.mask_draw = None;
                    panel.mask_edit = None;
                    panel.selected_mask = None;
                }

                // Pen tool
                let pen_active = panel.mask_tool == Some(MaskTool::Pen);
                if theme::icon_ghost_toggle_button(ui, btn_size, theme::UiIcon::Pen, pen_active)
                    .on_hover_text("钢笔工具 (P)")
                    .clicked()
                {
                    panel.mask_tool = if pen_active {
                        None
                    } else {
                        Some(MaskTool::Pen)
                    };
                    panel.mask_draw = None;
                    panel.mask_edit = None;
                    panel.selected_mask = None;
                }
            });
        });
}

/// Convert a screen-space delta to normalized [0,1] mask coordinate delta.
pub(crate) fn seq_delta_to_norm(
    state: &AppState,
    clip_id: mondrian_core::types::ClipId,
    delta: glam::Vec2,
) -> glam::Vec2 {
    let Some(seq) = state.sequence.as_ref() else {
        return delta;
    };
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let Some(ac) = active.iter().find(|a| a.clip.id == clip_id) else {
        return delta;
    };
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    // Extract rotation-free scale from the inverse transform.
    let inv = ac.transform_matrix.inverse();
    let local = inv.transform_vector2(delta);
    glam::Vec2::new(local.x / mw, local.y / mh)
}

/// Translate a mask shape by a normalized delta.
pub(crate) fn translate_shape(shape: &mut MaskShape, delta: glam::Vec2) {
    match shape {
        MaskShape::Rectangle { x, y, .. } => {
            *x += delta.x;
            *y += delta.y;
        }
        MaskShape::Ellipse { center, .. } => {
            *center += delta;
        }
        MaskShape::Path { points, .. } => {
            for pt in points {
                pt.position += delta;
                pt.control_in += delta;
                pt.control_out += delta;
            }
        }
    }
}

/// Resize a mask shape by dragging a corner. `corner` is 0=TL, 1=TR, 2=BR, 3=BL.
pub(crate) fn resize_shape_corner(shape: &mut MaskShape, corner: usize, delta: glam::Vec2) {
    let bbox = shape_bbox(shape);
    let (mut x1, mut y1, mut x2, mut y2) = (bbox.0.x, bbox.0.y, bbox.1.x, bbox.1.y);
    match corner {
        0 => {
            x1 += delta.x;
            y1 += delta.y;
        }
        1 => {
            x2 += delta.x;
            y1 += delta.y;
        }
        2 => {
            x2 += delta.x;
            y2 += delta.y;
        }
        3 => {
            x1 += delta.x;
            y2 += delta.y;
        }
        _ => return,
    }
    // Maintain minimum size.
    if x2 - x1 < 0.01 {
        x2 = x1 + 0.01;
    }
    if y2 - y1 < 0.01 {
        y2 = y1 + 0.01;
    }
    bbox_to_shape(shape, glam::Vec2::new(x1, y1), glam::Vec2::new(x2, y2));
}

pub(crate) fn shape_bbox(shape: &MaskShape) -> (glam::Vec2, glam::Vec2) {
    match shape {
        MaskShape::Rectangle { x, y, width, height, .. } => (
            glam::Vec2::new(*x, *y),
            glam::Vec2::new(*x + *width, *y + *height),
        ),
        MaskShape::Ellipse { center, radii } => (*center - *radii, *center + *radii),
        MaskShape::Path { points, .. } => {
            let mut min = glam::Vec2::splat(f32::MAX);
            let mut max = glam::Vec2::splat(f32::MIN);
            for pt in points {
                min = min.min(pt.position);
                max = max.max(pt.position);
            }
            (min, max)
        }
    }
}

pub(crate) fn bbox_to_shape(shape: &mut MaskShape, min: glam::Vec2, max: glam::Vec2) {
    match shape {
        MaskShape::Rectangle { x, y, width, height, .. } => {
            *x = min.x;
            *y = min.y;
            *width = max.x - min.x;
            *height = max.y - min.y;
        }
        MaskShape::Ellipse { center, radii } => {
            *center = (min + max) * 0.5;
            *radii = (max - min) * 0.5;
        }
        MaskShape::Path { .. } => {
            // Path resize is more complex; skip for now.
        }
    }
}

/// Directly update mask shape without going through the full undo command.
/// Used for real-time drag updates. The undo snapshot is captured once on release.
pub(crate) fn update_mask_shape_direct(
    state: &mut AppState,
    clip_id: mondrian_core::types::ClipId,
    mask_id: mondrian_effects::mask::MaskId,
    new_shape: MaskShape,
    current_frame: i64,
) {
    let Some(seq) = state.sequence.as_mut() else {
        return;
    };
    let current = TimeCode::new(current_frame.max(0), seq.time_base());
    let ticks = timecode_to_ticks(current);
    for track in &mut seq.video_tracks {
        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
            if let Some(mask) = clip.masks.iter_mut().find(|m| m.id == mask_id) {
                if mask.shape_animation_enabled {
                    // Animated: write a keyframe at the current time.
                    if let Some(pos) = mask.shape_keyframes.iter().position(|(t, _)| *t == ticks) {
                        mask.shape_keyframes[pos].1 = new_shape;
                    } else {
                        mask.shape_keyframes.push((ticks, new_shape));
                        mask.shape_keyframes.sort_by_key(|(t, _)| *t);
                    }
                } else {
                    // Static: update the single stored shape directly.
                    if let Some(first) = mask.shape_keyframes.first_mut() {
                        first.1 = new_shape;
                    }
                }
            }
            break;
        }
    }
}

/// Hit-test a mask shape at screen position. Returns (corner_hit, outline_hit).
/// corner_hit is Some(index) if near a bounding-box corner (0=TL,1=TR,2=BR,3=BL).
/// outline_hit is true if near the shape outline but not a corner.
pub(crate) fn mask_hit_test(
    shape: &MaskShape,
    mw: f32,
    mh: f32,
    mat: &glam::Mat3,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    screen_pos: Pos2,
) -> (Option<usize>, bool) {
    let to_scr = |x: f32, y: f32| -> Pos2 {
        let t = *mat * glam::Vec3::new(x, y, 1.0);
        ct.seq_to_screen(t.x, t.y)
    };
    const CORNER_RADIUS: f32 = 10.0;

    // For Path shapes: check individual anchor points AND handle endpoints.
    if let MaskShape::Path { points, .. } = shape {
        for (i, pt) in points.iter().enumerate() {
            // Check handle endpoints first (smaller hit target).
            const HANDLE_RADIUS: f32 = 12.0;
            if pt.control_in.length_squared() > 0.01 {
                let cp = to_scr(
                    (pt.position.x + pt.control_in.x) * mw,
                    (pt.position.y + pt.control_in.y) * mh,
                );
                if cp.distance(screen_pos) <= HANDLE_RADIUS {
                    return (Some(i), true);
                }
            }
            if pt.control_out.length_squared() > 0.01 {
                let cp = to_scr(
                    (pt.position.x + pt.control_out.x) * mw,
                    (pt.position.y + pt.control_out.y) * mh,
                );
                if cp.distance(screen_pos) <= HANDLE_RADIUS {
                    return (Some(i), true);
                }
            }
            // Then check anchor point.
            let sp = to_scr(pt.position.x * mw, pt.position.y * mh);
            if sp.distance(screen_pos) <= CORNER_RADIUS {
                return (Some(i), true);
            }
        }
    }

    let bbox = shape_bbox(shape);
    let corners = [
        to_scr(bbox.0.x * mw, bbox.0.y * mh),
        to_scr(bbox.1.x * mw, bbox.0.y * mh),
        to_scr(bbox.1.x * mw, bbox.1.y * mh),
        to_scr(bbox.0.x * mw, bbox.1.y * mh),
    ];
    for (i, &c) in corners.iter().enumerate() {
        if c.distance(screen_pos) <= CORNER_RADIUS {
            return (Some(i), true);
        }
    }
    // Check proximity to outline by sampling points.
    let outline_pts = shape_outline_points(shape, mw, mh);
    const OUTLINE_RADIUS: f32 = 8.0;
    for pt in &outline_pts {
        let sp = to_scr(pt.x, pt.y);
        if sp.distance(screen_pos) <= OUTLINE_RADIUS {
            return (None, true);
        }
    }
    // Check if inside the shape.
    if is_point_in_mask(shape, mw, mh, mat, ct, screen_pos) {
        return (None, true);
    }
    (None, false)
}

/// Get outline sample points for a mask shape.
pub(crate) fn shape_outline_points(shape: &MaskShape, mw: f32, mh: f32) -> Vec<glam::Vec2> {
    match shape {
        MaskShape::Rectangle { x, y, width, height, .. } => {
            let (x1, y1) = (x * mw, y * mh);
            let (x2, y2) = ((x + width) * mw, (y + height) * mh);
            let mut pts = Vec::new();
            let n = 24;
            for i in 0..n {
                let t = i as f32 / n as f32;
                pts.push(glam::Vec2::new(x1 + (x2 - x1) * t, y1));
                pts.push(glam::Vec2::new(x2, y1 + (y2 - y1) * t));
                pts.push(glam::Vec2::new(x1 + (x2 - x1) * t, y2));
                pts.push(glam::Vec2::new(x1, y1 + (y2 - y1) * t));
            }
            pts
        }
        MaskShape::Ellipse { center, radii } => {
            let (cx, cy) = (center.x * mw, center.y * mh);
            let (rx, ry) = (radii.x * mw, radii.y * mh);
            let n = 64;
            (0..=n)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / n as f32;
                    glam::Vec2::new(cx + rx * a.cos(), cy + ry * a.sin())
                })
                .collect()
        }
        MaskShape::Path { points, .. } => points
            .iter()
            .map(|p| glam::Vec2::new(p.position.x * mw, p.position.y * mh))
            .collect(),
    }
}

/// Check if a screen point is inside a mask shape.
pub(crate) fn is_point_in_mask(
    shape: &MaskShape,
    mw: f32,
    mh: f32,
    mat: &glam::Mat3,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    screen_pos: Pos2,
) -> bool {
    // Convert screen → seq → local media coords → normalized.
    let Some((sx, sy)) = ct.screen_to_seq(screen_pos) else {
        return false;
    };
    let inv = mat.inverse();
    let local = inv.transform_point2(glam::Vec2::new(sx, sy));
    let nx = local.x / mw;
    let ny = local.y / mh;
    match shape {
        MaskShape::Rectangle { x, y, width, height, .. } => {
            nx >= *x && nx <= *x + *width && ny >= *y && ny <= *y + *height
        }
        MaskShape::Ellipse { center, radii } => {
            let dx = (nx - center.x) / radii.x.max(0.001);
            let dy = (ny - center.y) / radii.y.max(0.001);
            dx * dx + dy * dy <= 1.0
        }
        MaskShape::Path { points, closed } => {
            if !closed {
                return false;
            }
            // Build polygon from sampled Bézier segments in media-pixel space.
            let segs = mask_path_segments(points, true);
            if segs.is_empty() {
                return false;
            }
            let mut poly: Vec<glam::Vec2> = Vec::with_capacity(segs.len());
            for &(a, _) in &segs {
                poly.push(glam::Vec2::new(a.x * mw, a.y * mh));
            }
            point_in_polygon(&poly, local)
        }
    }
}

/// Even-odd rule point-in-polygon test.
pub(crate) fn point_in_polygon(poly: &[glam::Vec2], pt: glam::Vec2) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let yi = poly[i].y;
        let yj = poly[j].y;
        if (yi > pt.y) != (yj > pt.y) {
            let x_intersect = poly[i].x + (poly[j].x - poly[i].x) * (pt.y - yi) / (yj - yi);
            if pt.x < x_intersect {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Convert a rectangle in sequence space to clip-local normalized [0,1] coordinates.
pub(crate) fn seq_rect_to_clip_normalized(
    state: &AppState,
    clip_id: mondrian_core::types::ClipId,
    min_seq: glam::Vec2,
    max_seq: glam::Vec2,
) -> (glam::Vec2, glam::Vec2) {
    let Some(seq) = state.sequence.as_ref() else {
        return (min_seq, max_seq);
    };
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let Some(ac) = active.iter().find(|a| a.clip.id == clip_id) else {
        return (min_seq, max_seq);
    };
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    let inv = ac.transform_matrix.inverse();
    let local_min = inv.transform_point2(min_seq);
    let local_max = inv.transform_point2(max_seq);
    let norm_min = glam::Vec2::new(local_min.x / mw, local_min.y / mh);
    let norm_max = glam::Vec2::new(local_max.x / mw, local_max.y / mh);
    (norm_min, norm_max)
}

/// Convert a single sequence-space point to clip-local normalized [0,1].
pub(crate) fn seq_point_to_clip_normalized(
    state: &AppState,
    clip_id: mondrian_core::types::ClipId,
    seq_pt: glam::Vec2,
) -> (f32, f32) {
    let Some(seq) = state.sequence.as_ref() else {
        return (seq_pt.x, seq_pt.y);
    };
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let Some(ac) = active.iter().find(|a| a.clip.id == clip_id) else {
        return (seq_pt.x, seq_pt.y);
    };
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    let inv = ac.transform_matrix.inverse();
    let local = inv.transform_point2(seq_pt);
    (local.x / mw, local.y / mh)
}

/// Draw a mask shape preview directly in sequence space (no clip transform).
pub(crate) fn draw_mask_preview_polygon(
    painter: &egui::Painter,
    pts: &[glam::Vec2],
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    stroke: egui::Stroke,
    closed: bool,
) {
    if pts.len() < 2 {
        return;
    }
    let cp: Vec<Pos2> = pts.iter().map(|&p| ct.seq_to_screen(p.x, p.y)).collect();
    let n = if closed { cp.len() } else { cp.len() - 1 };
    for i in 0..n {
        painter.line_segment([cp[i], cp[(i + 1) % cp.len()]], stroke);
    }
}

/// Generate the next auto-incremented mask name for a clip.
pub(crate) fn mask_next_name(state: &AppState, clip_id: mondrian_core::types::ClipId) -> String {
    let seq = state.sequence.as_ref();
    let count = seq
        .and_then(|s| {
            s.video_tracks
                .iter()
                .find_map(|t| t.clips.iter().find(|c| c.id == clip_id).map(|c| c.masks.len()))
        })
        .unwrap_or(0);
    format!("蒙版 {}", count + 1)
}

/// Render the canvas context menu popup. Returns true when the menu should close.
/// Draw selection labels at top-left of selected clips and masks on the canvas.
pub(crate) fn draw_selection_labels(
    painter: &egui::Painter,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    state: &AppState,
    timeline_frame: i64,
    selected_clip: Option<(
        mondrian_core::types::TrackId,
        bool,
        mondrian_core::types::ClipId,
    )>,
    selected_mask: Option<(
        mondrian_effects::mask::MaskId,
        mondrian_core::types::ClipId,
        mondrian_core::types::TrackId,
    )>,
) {
    let Some(seq) = state.sequence.as_ref() else {
        return;
    };
    let current = TimeCode::new(timeline_frame.max(0), seq.time_base());
    let active = seq.active_clips_at(current);
    let ticks = timecode_to_ticks(current);

    if let Some((_, _, sel_cid)) = selected_clip {
        if let Some(ac) = active.iter().find(|a| a.clip.id == sel_cid) {
            let bb = clip_screen_bounds_with_media(&ac.clip, ac.transform_matrix, ct, state);
            if let Some(bb) = bb {
                let label = ac.clip.label.as_deref().filter(|l| !l.is_empty()).unwrap_or("片段");
                draw_label_badge(
                    painter,
                    bb.left_top(),
                    label,
                    egui::Color32::from_rgb(0, 180, 255),
                );

                // Mask labels
                if let Some((_mid, _mcid, _tid)) = selected_mask {
                    for mask in &ac.clip.masks {
                        if !mask.enabled {
                            continue;
                        }
                        if !selected_mask.is_some_and(|(mid, _, _)| mid == mask.id) {
                            continue;
                        }
                        let kf = mask.evaluate_at(ticks);
                        let bbox = shape_bbox(&kf.shape);
                        let (mw, mh) = state
                            .asset_library
                            .as_ref()
                            .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
                            .and_then(|a| a.media_info.primary_video().cloned())
                            .map(|v| (v.width as f32, v.height as f32))
                            .unwrap_or((1.0, 1.0));
                        // Convert normalized bbox corners to screen
                        let tl = glam::Vec2::new(bbox.0.x * mw, bbox.0.y * mh);
                        let sp = ct.seq_to_screen(
                            (ac.transform_matrix * tl.extend(1.0)).x,
                            (ac.transform_matrix * tl.extend(1.0)).y,
                        );
                        draw_label_badge(
                            painter,
                            sp,
                            &mask.name,
                            egui::Color32::from_rgb(200, 120, 0),
                        );
                    }
                }
            }
        }
    }
}

/// Draw a rounded-rectangle label badge at a screen position.
pub(crate) fn draw_label_badge(
    painter: &egui::Painter,
    top_left: Pos2,
    text: &str,
    color: egui::Color32,
) {
    let font = typography::body_small();
    let galley = painter.layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE);
    let pad = egui::vec2(6.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let rect = egui::Rect::from_min_size(top_left - egui::vec2(0.0, size.y + 4.0), size);
    let bg = egui::Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 200);
    painter.rect_filled(rect, egui::CornerRadius::same(4), bg);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(4),
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    let max_w = 140.0;
    let display = if galley.size().x > max_w {
        let mut s = text.to_string();
        let mut best = s.clone();
        while s.len() > 3 {
            s.pop();
            let g = painter.layout_no_wrap(format!("{s}…"), font.clone(), egui::Color32::WHITE);
            if g.size().x <= max_w {
                best = format!("{s}…");
                break;
            }
        }
        best
    } else {
        text.to_string()
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        display,
        font,
        egui::Color32::WHITE,
    );
}

/// Move a single path point by a normalized delta.
pub(crate) fn move_path_point(shape: &mut MaskShape, idx: usize, delta: glam::Vec2) {
    if let MaskShape::Path { points, .. } = shape {
        if let Some(pt) = points.get_mut(idx) {
            pt.position += delta;
            // Handles are relative to position; position move suffices.
        }
    }
}

/// Move a single path control handle by a normalized delta.
pub(crate) fn move_path_handle(shape: &mut MaskShape, idx: usize, delta: glam::Vec2, is_in: bool) {
    if let MaskShape::Path { points, .. } = shape {
        if let Some(pt) = points.get_mut(idx) {
            if is_in {
                pt.control_in += delta;
            } else {
                pt.control_out += delta;
            }
        }
    }
}

/// Determine edit mode for a path point click: handle drag vs anchor move.
pub(crate) fn path_point_edit_mode(
    state: &AppState,
    ct: &crate::egui_ui::viewer::canvas::CanvasTransform,
    clip_id: mondrian_core::types::ClipId,
    points: &[mondrian_effects::mask::BezierPoint],
    idx: usize,
    screen_pos: Pos2,
) -> Option<MaskEditMode> {
    let pt = points.get(idx)?;
    let seq = state.sequence.as_ref()?;
    let current = TimeCode::new(state.current_frame().max(0), seq.time_base());
    let ac = seq.active_clips_at(current).into_iter().find(|a| a.clip.id == clip_id)?;
    let (mw, mh) = state
        .asset_library
        .as_ref()
        .and_then(|lib| lib.get_asset(ac.clip.asset_id).ok().flatten())
        .and_then(|a| a.media_info.primary_video().cloned())
        .map(|v| (v.width as f32, v.height as f32))
        .unwrap_or((1.0, 1.0));
    let mat = &ac.transform_matrix;
    let to_scr = |px: f32, py: f32| -> Pos2 {
        let t = *mat * glam::Vec3::new(px, py, 1.0);
        ct.seq_to_screen(t.x, t.y)
    };
    const HANDLE_RADIUS: f32 = 10.0;
    // Check control_in handle.
    if pt.control_in.length_squared() > 0.01 || pt.control_out.length_squared() > 0.01 {
        let cp_in = to_scr(
            (pt.position.x + pt.control_in.x) * mw,
            (pt.position.y + pt.control_in.y) * mh,
        );
        if cp_in.distance(screen_pos) <= HANDLE_RADIUS {
            return Some(MaskEditMode::MovePathHandle(idx, true));
        }
        let cp_out = to_scr(
            (pt.position.x + pt.control_out.x) * mw,
            (pt.position.y + pt.control_out.y) * mh,
        );
        if cp_out.distance(screen_pos) <= HANDLE_RADIUS {
            return Some(MaskEditMode::MovePathHandle(idx, false));
        }
    }
    // Default: move the anchor point.
    Some(MaskEditMode::MovePathPoint(idx))
}
