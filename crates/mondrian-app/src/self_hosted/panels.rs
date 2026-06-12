//! Panel adapters for the self-hosted UI shell.
//!
//! These adapters translate application-facing panel concepts into generic
//! `mondrian-ui-widgets` view models. Demo data is kept behind explicit model
//! factories so real `AppState` / `EditorState` adapters can replace it without
//! changing dock layout or widget construction.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::Sequence;
use mondrian_timeline::track::Track;
use mondrian_ui_core::types::SplitDirection;
use mondrian_ui_core::widgets::ColoredBox;
use mondrian_ui_core::Widget;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::dock_tab_bar::TabInfo;
use mondrian_ui_widgets::panel_slot::SlotKind;
use mondrian_ui_widgets::{
    Checkbox, ColorPickerAreaMode, ColorPickerTrigger, CurveEditor, CurvePoint, DockPanel,
    PanelList, PanelListItem, PropertyPanel, PropertyRow, PropertySection, ScrollView, Slider,
    TimelineClip, TimelineClipRef, TimelineTrack, TimelineView,
};

use crate::app::SelectedClipRef;

/// Complete set of view models needed by the self-hosted panel shell.
#[derive(Debug, Clone)]
pub struct SelfHostedPanelModels {
    pub assets: PanelListModel,
    pub effects: PanelListModel,
    pub console: PanelListModel,
    pub timeline: TimelinePanelModel,
    pub inspector: InspectorPanelModel,
}

impl SelfHostedPanelModels {
    /// Demo fixtures used by developer binaries before the real editor state is
    /// wired into the self-hosted shell.
    pub fn demo() -> Self {
        Self {
            assets: demo_asset_model(),
            effects: demo_effects_model(),
            console: demo_console_model(),
            timeline: demo_timeline_model(),
            inspector: InspectorPanelModel::demo(),
        }
    }
}

/// List panel data independent from a concrete widget instance.
#[derive(Debug, Clone)]
pub struct PanelListModel {
    pub title: String,
    pub subtitle: String,
    pub items: Vec<PanelListItem>,
    pub activate_prefix: Option<String>,
}

impl PanelListModel {
    pub fn new(title: impl Into<String>, items: Vec<PanelListItem>) -> Self {
        Self {
            title: title.into(),
            subtitle: String::new(),
            items,
            activate_prefix: None,
        }
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    pub fn with_activate_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.activate_prefix = Some(prefix.into());
        self
    }
}

/// Timeline panel data in frame space.
#[derive(Debug, Clone)]
pub struct TimelinePanelModel {
    pub tracks: Vec<TimelineTrack>,
    pub playhead_frame: i64,
}

impl TimelinePanelModel {
    /// Map the current timeline sequence into self-hosted timeline view models.
    ///
    /// The widget layer stays index-based and domain-light; this adapter is the
    /// app-side boundary that carries stable track/clip ids into emitted
    /// actions.
    pub fn from_sequence(sequence: &Sequence, selected_clips: &[SelectedClipRef]) -> Self {
        let video_tracks = sequence
            .video_tracks
            .iter()
            .map(|track| timeline_track_from_sequence_track(track, true, selected_clips));
        let audio_tracks = sequence
            .audio_tracks
            .iter()
            .map(|track| timeline_track_from_sequence_track(track, false, selected_clips));

        Self {
            tracks: video_tracks.chain(audio_tracks).collect(),
            playhead_frame: sequence.playhead.frame.max(0),
        }
    }
}

/// Inspector fixture data independent from a concrete property widget tree.
#[derive(Debug, Clone)]
pub struct InspectorPanelModel {
    pub enabled: bool,
    pub opacity: f32,
    pub tint: Color,
    pub tint_area_mode: ColorPickerAreaMode,
    pub curve_points: Vec<CurvePoint>,
}

impl InspectorPanelModel {
    pub fn demo() -> Self {
        Self {
            enabled: true,
            opacity: 72.0,
            tint: Color::from_rgba8(132, 180, 255, 220),
            tint_area_mode: ColorPickerAreaMode::Wheel,
            curve_points: vec![
                CurvePoint::new(0.0, 0.0),
                CurvePoint::new(0.35, 0.68),
                CurvePoint::new(0.72, 0.42),
                CurvePoint::new(1.0, 1.0),
            ],
        }
    }
}

/// Build the default self-hosted dock tree from explicit panel models.
pub fn build_dock_tree(models: SelfHostedPanelModels) -> DockSplitter {
    let left = DockSplitter::new(
        SplitDirection::Vertical,
        0.6,
        slot(SlotKind::Assets, models.clone()),
        slot(SlotKind::Console, models.clone()),
    );
    let right_bottom = DockSplitter::new(
        SplitDirection::Horizontal,
        0.7,
        slot(SlotKind::Timeline, models.clone()),
        slot(SlotKind::Inspector, models.clone()),
    );
    let right = DockSplitter::new(
        SplitDirection::Vertical,
        0.65,
        slot(SlotKind::Viewer, models.clone()),
        Box::new(right_bottom),
    );
    DockSplitter::new(
        SplitDirection::Horizontal,
        0.28,
        Box::new(left),
        Box::new(right),
    )
}

/// Build a dock tree using built-in demo panel fixtures.
pub fn build_demo_dock_tree() -> DockSplitter {
    build_dock_tree(SelfHostedPanelModels::demo())
}

fn slot(kind: SlotKind, models: SelfHostedPanelModels) -> Box<dyn Widget> {
    if kind == SlotKind::Inspector {
        let inspector = models.inspector.clone();
        return Box::new(DockPanel::new(
            kind,
            single_tab(kind),
            move |_kind, _active| {
                Box::new(ScrollView::new(Some(Box::new(inspector_panel(&inspector)))))
            },
        ));
    }

    Box::new(DockPanel::new(
        kind,
        single_tab(kind),
        move |kind, _active| panel_content_for_slot(kind, &models),
    ))
}

fn single_tab(kind: SlotKind) -> Vec<TabInfo> {
    vec![TabInfo {
        label: kind.display_name().to_string(),
        active: true,
    }]
}

fn panel_content_for_slot(kind: SlotKind, models: &SelfHostedPanelModels) -> Box<dyn Widget> {
    match kind {
        SlotKind::Assets => Box::new(panel_list(&models.assets)),
        SlotKind::Effects => Box::new(panel_list(&models.effects)),
        SlotKind::Console => Box::new(panel_list(&models.console)),
        SlotKind::Viewer => Box::new(ColoredBox::new(Color::from_hex(0x1A1A2E), 1.0, 1.0)),
        SlotKind::Timeline => Box::new(timeline_panel(&models.timeline)),
        SlotKind::Project => Box::new(ColoredBox::new(Color::from_hex(0x1E3A2A), 1.0, 1.0)),
        _ => Box::new(ColoredBox::new(Color::from_hex(0x1A1A1A), 1.0, 1.0)),
    }
}

fn timeline_track_from_sequence_track(
    track: &Track,
    is_video_track: bool,
    selected_clips: &[SelectedClipRef],
) -> TimelineTrack {
    let muted = track.is_muted;
    let locked = track.is_locked;
    let clips = track
        .clips
        .iter()
        .map(|clip| timeline_clip_from_sequence_clip(track, is_video_track, clip, selected_clips))
        .collect();

    let track = if is_video_track {
        TimelineTrack::video(track.name.clone(), clips)
    } else {
        TimelineTrack::audio(track.name.clone(), clips)
    };
    track.muted(muted).locked(locked)
}

fn timeline_clip_from_sequence_clip(
    track: &Track,
    is_video_track: bool,
    clip: &Clip,
    selected_clips: &[SelectedClipRef],
) -> TimelineClip {
    let selected = selected_clips.iter().any(|selection| {
        selection.track_id == track.id
            && selection.clip_id == clip.id
            && selection.is_video_track == is_video_track
    });
    let label = clip.label.clone().unwrap_or_else(|| default_clip_label(clip));
    let mut view = TimelineClip::new(
        label,
        clip.position.frame.max(0),
        clip.duration.frame.max(1),
    )
    .selected(selected)
    .disabled(clip.is_disabled)
    .with_select_action(panel_action(&format!(
        "timeline.select.{}.{}",
        track.id, clip.id
    )));
    if let Some(color) = timeline_clip_color(clip, is_video_track) {
        view = view.with_color(color);
    }
    view
}

fn default_clip_label(clip: &Clip) -> String {
    if clip.is_adjustment_layer() {
        "Adjustment".to_string()
    } else if clip.is_nested_sequence() {
        "Nested Sequence".to_string()
    } else if clip.is_solid_color() {
        "Solid Color".to_string()
    } else {
        format!("Clip {}", clip.id)
    }
}

fn timeline_clip_color(clip: &Clip, is_video_track: bool) -> Option<Color> {
    if let Some(color) = clip.solid_color {
        return Some(color);
    }
    if clip.is_adjustment_layer() {
        Some(Color::from_hex(0x6D5DD3))
    } else if clip.is_nested_sequence() {
        Some(Color::from_hex(0x4B7BE5))
    } else if is_video_track {
        Some(Color::from_hex(0x1E3A5F))
    } else {
        Some(Color::from_hex(0x1D587B))
    }
}

fn panel_list(model: &PanelListModel) -> PanelList {
    let mut list = PanelList::new(model.title.clone(), model.items.clone())
        .with_subtitle(model.subtitle.clone());
    if let Some(prefix) = model.activate_prefix.clone() {
        list = list.on_activate(move |index, item| {
            panel_action(&format!("{prefix}.{index}.{}", item.title))
        });
    }
    list
}

fn demo_asset_model() -> PanelListModel {
    PanelListModel::new(
        "Assets",
        vec![
            PanelListItem::new("Footage")
                .with_subtitle("Imported camera clips")
                .with_badge("12")
                .with_select_action(panel_action("assets.select.footage")),
            PanelListItem::new("Audio")
                .with_subtitle("Music, voiceover, and ambience")
                .with_badge("5")
                .with_select_action(panel_action("assets.select.audio")),
            PanelListItem::new("Images")
                .with_subtitle("Still frames and references")
                .with_badge("8")
                .with_select_action(panel_action("assets.select.images")),
            PanelListItem::new("Sequences")
                .with_subtitle("Nested edits and reusable timelines")
                .with_badge("2")
                .with_select_action(panel_action("assets.select.sequences")),
        ],
    )
    .with_subtitle("Project library")
    .with_activate_prefix("assets.activate")
}

fn demo_effects_model() -> PanelListModel {
    PanelListModel::new(
        "Effects",
        vec![
            PanelListItem::new("Color Balance")
                .with_subtitle("Lift, gamma, gain")
                .with_badge("GPU")
                .with_select_action(panel_action("effects.select.color_balance")),
            PanelListItem::new("Gaussian Blur")
                .with_subtitle("Radius and edge behavior")
                .with_badge("GPU")
                .with_select_action(panel_action("effects.select.blur")),
            PanelListItem::new("LUT")
                .with_subtitle("Creative look transform")
                .with_badge("3D")
                .with_select_action(panel_action("effects.select.lut")),
            PanelListItem::new("Optical Flow")
                .with_subtitle("Coming after render cache integration")
                .with_badge("Soon")
                .disabled(true),
        ],
    )
    .with_subtitle("Effect browser")
    .with_activate_prefix("effects.apply")
}

fn demo_console_model() -> PanelListModel {
    PanelListModel::new(
        "Console",
        vec![
            PanelListItem::new("UI runtime ready").with_subtitle("Event router attached"),
            PanelListItem::new("Renderer warm").with_subtitle("wgpu command encoder active"),
            PanelListItem::new("Text atlas").with_subtitle("cosmic-text layout path"),
        ],
    )
    .with_subtitle("Runtime messages")
}

fn demo_timeline_model() -> TimelinePanelModel {
    TimelinePanelModel {
        tracks: vec![
            TimelineTrack::video(
                "V3",
                vec![
                    TimelineClip::new("Adjustment", 36, 84)
                        .with_color(Color::from_hex(0x6D5DD3))
                        .with_select_action(panel_action("timeline.select.adjustment")),
                    TimelineClip::new("Title", 132, 48)
                        .with_color(Color::from_hex(0x4B7BE5))
                        .with_select_action(panel_action("timeline.select.title")),
                ],
            ),
            TimelineTrack::video(
                "V2",
                vec![
                    TimelineClip::new("B-roll", 18, 72)
                        .with_color(Color::from_hex(0x2C7A7B))
                        .with_select_action(panel_action("timeline.select.broll")),
                    TimelineClip::new("Overlay", 112, 56)
                        .with_color(Color::from_hex(0x805AD5))
                        .selected(true)
                        .with_select_action(panel_action("timeline.select.overlay")),
                ],
            ),
            TimelineTrack::video(
                "V1",
                vec![
                    TimelineClip::new("Interview", 0, 96)
                        .with_color(Color::from_hex(0x1E3A5F))
                        .with_select_action(panel_action("timeline.select.interview")),
                    TimelineClip::new("Cutaway", 104, 72)
                        .with_color(Color::from_hex(0x2F855A))
                        .with_select_action(panel_action("timeline.select.cutaway")),
                    TimelineClip::new("Outro", 190, 44)
                        .with_color(Color::from_hex(0x744210))
                        .with_select_action(panel_action("timeline.select.outro")),
                ],
            ),
            TimelineTrack::audio(
                "A1",
                vec![TimelineClip::new("Dialogue", 0, 176)
                    .with_color(Color::from_hex(0x1D587B))
                    .with_select_action(panel_action("timeline.select.dialogue"))],
            ),
            TimelineTrack::audio(
                "A2",
                vec![TimelineClip::new("Music Bed", 24, 210)
                    .with_color(Color::from_hex(0x2B6CB0))
                    .with_select_action(panel_action("timeline.select.music"))],
            ),
        ],
        playhead_frame: 76,
    }
}

fn timeline_panel(model: &TimelinePanelModel) -> TimelineView {
    TimelineView::new(model.tracks.clone())
        .with_playhead(model.playhead_frame)
        .on_clip_select(|clip_ref, clip| {
            timeline_clip_action("timeline.select", clip_ref, &clip.label)
        })
        .on_clip_move(|movement, clip| {
            panel_action(&format!(
                "timeline.move.{}.{}.track{}->track{}.{}->{}.{}",
                movement.clip_ref.track_index,
                movement.clip_ref.clip_index,
                movement.clip_ref.track_index,
                movement.new_track_index,
                movement.old_start_frame,
                movement.new_start_frame,
                clip.label
            ))
        })
        .on_clip_trim(|trim, clip| {
            panel_action(&format!(
                "timeline.trim.{}.{}.{:?}.{}+{}->{}+{}.{}",
                trim.clip_ref.track_index,
                trim.clip_ref.clip_index,
                trim.edge,
                trim.old_start_frame,
                trim.old_duration_frames,
                trim.new_start_frame,
                trim.new_duration_frames,
                clip.label
            ))
        })
        .on_seek(|frame| panel_action(&format!("timeline.seek.{frame}")))
}

fn timeline_clip_action(prefix: &str, clip_ref: TimelineClipRef, label: &str) -> Action {
    panel_action(&format!(
        "{prefix}.{}.{}.{}",
        clip_ref.track_index, clip_ref.clip_index, label
    ))
}

fn panel_action(name: &str) -> Action {
    Action::Custom {
        namespace: "ui.panel".into(),
        name: name.into(),
        payload: serde_json::Value::Null,
    }
}

fn inspector_panel(model: &InspectorPanelModel) -> PropertyPanel {
    let mut tint = ColorPickerTrigger::new(model.tint);
    tint.picker_mut().set_area_mode(model.tint_area_mode);
    let curve =
        CurveEditor::with_points(model.curve_points.clone()).on_change(inspector_curve_action);
    PropertyPanel::new("Inspector")
        .with_subtitle("Selected clip")
        .with_section(
            PropertySection::new("Clip Style")
                .with_row(PropertyRow::new(
                    "Enabled",
                    Box::new(
                        Checkbox::new("启用效果", model.enabled).on_change(inspector_bool_action),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "Opacity",
                    Box::new(
                        Slider::new(model.opacity, 0.0, 100.0)
                            .on_change(|value| inspector_value_action("opacity", value)),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "Tint",
                    Box::new(tint.on_change(inspector_color_action)),
                )),
        )
        .with_section(
            PropertySection::new("Animation")
                .with_row(PropertyRow::new("Curve", Box::new(curve)).with_height(118.0)),
        )
}

fn inspector_value_action(name: &'static str, value: f32) -> Action {
    Action::Custom {
        namespace: "ui.inspector".into(),
        name: format!("{name}:{value:.3}"),
        payload: serde_json::Value::Null,
    }
}

fn inspector_bool_action(value: bool) -> Action {
    Action::Custom {
        namespace: "ui.inspector".into(),
        name: format!("enabled:{value}"),
        payload: serde_json::Value::Null,
    }
}

fn inspector_color_action(color: Color) -> Action {
    let [r, g, b, a] = color.to_rgba8();
    Action::Custom {
        namespace: "ui.inspector".into(),
        name: format!("tint:{r},{g},{b},{a}"),
        payload: serde_json::Value::Null,
    }
}

fn inspector_curve_action(points: &[CurvePoint]) -> Action {
    let mut name = String::from("curve");
    for point in points {
        name.push_str(&format!(":{:.3},{:.3}", point.x, point.y));
    }
    Action::Custom {
        namespace: "ui.inspector".into(),
        name,
        payload: serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::{AssetId, TimeCode};

    #[test]
    fn demo_panel_models_cover_primary_editor_surfaces() {
        let models = SelfHostedPanelModels::demo();

        assert!(!models.assets.items.is_empty());
        assert!(!models.effects.items.is_empty());
        assert!(!models.console.items.is_empty());
        assert!(!models.timeline.tracks.is_empty());
        assert!(!models.inspector.curve_points.is_empty());
    }

    #[test]
    fn demo_timeline_model_has_valid_frame_ranges() {
        let model = demo_timeline_model();
        let mut max_end = 0;

        for track in &model.tracks {
            assert!(!track.label.is_empty());
            for clip in &track.clips {
                assert!(clip.duration_frames > 0);
                max_end = max_end.max(clip.start_frame + clip.duration_frames);
            }
        }

        assert!(model.playhead_frame >= 0);
        assert!(model.playhead_frame <= max_end);
    }

    #[test]
    fn timeline_model_maps_sequence_tracks_clips_and_selection() {
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        sequence.playhead = TimeCode::new(42, tb);

        let mut video = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        video.label = Some("Interview".to_string());
        let video_id = video.id;
        let video_track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(video).expect("add video clip");

        let mut audio = Clip::new(AssetId::new(), TimeCode::new(12, tb), TimeCode::new(48, tb));
        audio.label = Some("Dialogue".to_string());
        audio.is_disabled = true;
        sequence.audio_tracks[0].add_clip(audio).expect("add audio clip");
        sequence.audio_tracks[0].is_muted = true;
        sequence.audio_tracks[0].is_locked = true;

        let selected = SelectedClipRef {
            track_id: video_track_id,
            is_video_track: true,
            clip_id: video_id,
        };
        let model = TimelinePanelModel::from_sequence(&sequence, &[selected]);

        assert_eq!(model.playhead_frame, 42);
        assert_eq!(
            model.tracks.len(),
            sequence.video_tracks.len() + sequence.audio_tracks.len()
        );
        assert_eq!(model.tracks[0].label, "V1");
        assert_eq!(
            model.tracks[0].kind,
            mondrian_ui_widgets::TimelineTrackKind::Video
        );
        assert_eq!(model.tracks[0].clips[0].label, "Interview");
        assert_eq!(model.tracks[0].clips[0].start_frame, 10);
        assert_eq!(model.tracks[0].clips[0].duration_frames, 20);
        assert!(model.tracks[0].clips[0].selected);

        let first_audio = sequence.video_tracks.len();
        assert_eq!(
            model.tracks[first_audio].kind,
            mondrian_ui_widgets::TimelineTrackKind::Audio
        );
        assert!(model.tracks[first_audio].muted);
        assert!(model.tracks[first_audio].locked);
        assert!(model.tracks[first_audio].clips[0].disabled);
    }

    #[test]
    fn timeline_model_uses_solid_color_clip_color() {
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        let color = Color::from_rgba8(12, 34, 56, 200);
        let solid = Clip::new_solid_color(
            AssetId::new(),
            color,
            TimeCode::new(0, tb),
            TimeCode::new(30, tb),
        );
        sequence.video_tracks[0].add_clip(solid).expect("add solid clip");

        let model = TimelinePanelModel::from_sequence(&sequence, &[]);

        assert_eq!(model.tracks[0].clips[0].label, "纯色层");
        assert_eq!(
            model.tracks[0].clips[0].color.map(|c| c.to_rgba8()),
            Some(color.to_rgba8())
        );
    }
}
