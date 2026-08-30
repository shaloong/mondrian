use super::*;
use crate::app::preview_unavailability::PreviewOutputStage;
use crate::app::product_action::TimelineSelectionEdit;
use mondrian_export::queue::{ExportFailure, ExportFailureReason};

fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
    let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
    mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
}

fn test_parameter_address(parameter_id: &'static str) -> AnimationParameterAddress {
    AnimationParameterAddress {
        animation_track_id: mondrian_core::types::AnimationTrackId::new(),
        parameter_id: mondrian_core::ParameterId::new_static(parameter_id),
    }
}
use crate::app::product_action::{
    ExportProductAction, ProductAction, SequenceProductAction, TimelineProductAction,
};
use crate::app::ui_actions::{
    AppShellInterpretAssetDialogPayload, AppShellRelinkAssetDialogPayload,
    AssetsDeleteSelectionPayload, AssetsImportFilesPayload, AssetsMoveSelectionPayload,
    AssetsOpenFolderPayload, AssetsRenameAssetPayload, AssetsSetProxyModePayload,
    ImportMediaDialogPayload, APP_SHELL_ASSET_BROWSER_OPEN_FOLDER, APP_SHELL_IMPORT_MEDIA_DIALOG,
    APP_SHELL_INTERPRET_ASSET_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_RELINK_ASSET_DIALOG,
    ASSET_CREATE_FOLDER, ASSET_CREATE_GENERATED, ASSET_IMPORT_FILES, ASSET_MOVE_ENTRIES,
    ASSET_NAMESPACE, ASSET_PREPARE_DRAG, ASSET_REBIND_AUDIO_COMPONENT,
    ASSET_REFRESH_AUDIO_COMPONENTS, ASSET_REMOVE_ENTRIES, ASSET_RENAME, ASSET_SET_PROXY_MODE,
    AUDIO_EDIT_COMPONENT, AUDIO_NAMESPACE, CLIP_EDIT_NUMERIC_CURVE, CLIP_NAMESPACE,
    CLIP_WRITE_PARAMETER_VALUES, TIMELINE_CLEAR_IN_OUT_POINTS, TIMELINE_NAMESPACE,
    TIMELINE_SELECT_CLIP, TIMELINE_SET_IN_OUT_POINT, TRACK_ADD, TRACK_MOVE, TRACK_NAMESPACE,
    VIDEO_TRANSITION_CREATE_CROSS_DISSOLVE, VIDEO_TRANSITION_NAMESPACE, VIDEO_TRANSITION_SELECT,
    VIDEO_TRANSITION_SET_RANGE, VISUAL_EFFECT_ADD_TO_CLIP, VISUAL_EFFECT_NAMESPACE,
    VISUAL_EFFECT_SELECT, VISUAL_EFFECT_SET_PARAMETER_VALUE,
};
use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
use mondrian_core::automation::{
    Keyframe, PropertyHost, PropertyMutation, PropertyValue, QualifierSample,
    QualifierSampleOperation, QualifierSampleSet,
};
use mondrian_core::types::AssetId;
use mondrian_core::{SmpteCountingMode, TimelineDisplaySettings};
use mondrian_effects::EffectNodeExt;
use mondrian_ui_core::tree::WidgetTreeView;
use mondrian_ui_core::types::{
    DragPayload, EventResult, KeyCode, LayoutConstraint, Modifiers, MouseButton, Point, Rect, Size,
    WidgetId,
};
use mondrian_ui_core::widget::{EventContext, EventRequests, PaintContext};
use mondrian_ui_core::UiEvent;
use mondrian_ui_events::EventRouter;
use mondrian_ui_widgets::menu::MenuItemKind;
use mondrian_ui_widgets::{ViewerExternalTextureFrame, ViewerFrameImage};
use std::cell::RefCell;
use std::path::PathBuf;

fn timeline_content_point(x: f32, y: f32) -> Point {
    const LEGACY_APP_TIMELINE_HEADER_WIDTH: f32 = 104.0;
    const CURRENT_TIMELINE_HEADER_WIDTH: f32 = 144.0;
    Point::new(
        x + CURRENT_TIMELINE_HEADER_WIDTH - LEGACY_APP_TIMELINE_HEADER_WIDTH,
        y + 30.0,
    )
}

fn video_display_index(sequence: &Sequence, domain_index: usize) -> usize {
    sequence.video_tracks.len() - 1 - domain_index
}

fn badge_labels(item: &AssetGridItem) -> Vec<&str> {
    item.badges.iter().map(|badge| badge.label.as_str()).collect()
}

struct AssetTimelineDragHarness {
    id: WidgetId,
    bounds: Rect,
    assets: AssetGrid,
    timeline: TimelineView,
}

impl AssetTimelineDragHarness {
    fn new(assets: AssetGrid, timeline: TimelineView) -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            assets,
            timeline,
        }
    }
}

impl Widget for AssetTimelineDragHarness {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(840.0, 240.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.assets.layout(Rect::new(bounds.x, bounds.y, 260.0, 180.0));
        self.timeline.layout(Rect::new(bounds.x + 300.0, bounds.y, 520.0, 220.0));
    }

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, _ctx: &mut PaintContext) {}

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        2
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.assets),
            1 => Some(&self.timeline),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.assets),
            1 => Some(&mut self.timeline),
            _ => None,
        }
    }
}

#[test]
fn demo_panel_models_cover_primary_editor_surfaces() {
    let models = AppUiPanelModels::demo();

    assert!(!models.assets.items.is_empty());
    assert!(models.assets.items.iter().all(|item| item.icon.is_some()));
    assert!(!models.effects.items.is_empty());
    assert_eq!(models.viewer.title, "Demo edit");
    assert!(models.viewer.enabled);
    assert_eq!(models.viewer.status_tone, ViewerStatusTone::Neutral);
    assert_eq!(models.viewer.empty_message, None);
    assert_eq!(models.viewer.zoom_label, "适合");
    assert_eq!(models.viewer.preview_quality_label, "1/2");
    assert_eq!(models.viewer.preview_resolution_scale, 0.5);
    assert!(!models.timeline.tracks.is_empty());
    let opacity_curve = models
        .inspector
        .opacity_curve
        .as_ref()
        .expect("selected demo Clip exposes its opacity curve");
    assert_eq!(
        opacity_curve.property.parameter_id,
        mondrian_core::ParameterId::new_static("mondrian.transform.opacity")
    );
    assert_eq!(opacity_curve.keys.len(), 2);
    assert!(opacity_curve.keys.iter().all(|key| key.keyframe_id.is_none()));
    assert_eq!(opacity_curve.keys[0].point.x, 0.0);
    assert_eq!(opacity_curve.keys[1].point.x, 1.0);
    assert_eq!(opacity_curve.display_points.len(), 129);
    assert!(!models.node_graph.nodes.is_empty());
}

#[test]
fn inspector_model_projects_asset_components_and_physical_binding_separately() {
    let root = unique_temp_dir("inspector-audio-components");
    let library = AssetLibrary::open(root.join("library")).expect("asset library");
    let media_path = root.join("dual-audio.mov");
    std::fs::write(&media_path, [0u8]).expect("media fixture");
    let stream = |index, stream_id, language: &str, is_default| AudioStreamInfo {
        index,
        stream_id: Some(stream_id),
        language: Some(language.to_owned()),
        title: None,
        is_default,
        codec: mondrian_media::info::AudioCodec::Aac,
        duration: Some(std::time::Duration::from_secs(1)),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: ChannelLayout::Stereo,
        bit_depth: 24,
        avg_bitrate: 256_000,
    };
    let info = mondrian_media::MediaInfo {
        duration: std::time::Duration::from_secs(1),
        file_size: 1,
        container: "mov".to_owned(),
        video_streams: Vec::new(),
        audio_streams: vec![stream(1, 10, "eng", false), stream(3, 30, "jpn", true)],
        has_video: false,
        has_audio: true,
    };
    let asset_id = commit_test_media_asset(&library, media_path.clone(), info);
    let mut sequence = Sequence::new("Inspector audio");
    let track_id = sequence.audio_tracks[0].id;
    let clip =
        Clip::new(asset_id, TimelineTime::ZERO, tt(25, sequence.time_base())).expect("audio Clip");
    let clip_id = clip.id;
    sequence
        .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
        .expect("add audio Clip");
    let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    sequence
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|scope| scope.id == scope_id)
        .expect("Processing Scope")
        .processors
        .processors
        .push(mondrian_timeline::audio::AudioProcessorInstance::built_in(
            mondrian_timeline::audio::BUILTIN_GAIN_DEFINITION_ID,
            1,
        ));
    let authored_fade = AudioFade {
        duration: TimelineTime::new(1, 4).expect("fade duration"),
        curve: AudioFadeCurve::EqualPower,
    };
    let edit = &mut sequence.audio_tracks[0].clips[0].audio_components[0];
    edit.enabled = false;
    edit.volume_db = -6.0;
    edit.pan = 0.25;
    edit.fades.fade_in = Some(authored_fade);
    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library));
    state.selection.selected_clips =
        vec![SelectedClipRef { track_id, is_video_track: false, clip_id }];

    let model = InspectorPanelModel::from_app_state(&state);

    assert_eq!(model.audio_components.len(), 1);
    let component = &model.audio_components[0];
    assert!(!component.enabled);
    assert_eq!(component.volume_db, -6.0);
    assert_eq!(component.pan, 0.25);
    assert_eq!(component.fade_in, Some(authored_fade));
    assert_eq!(component.fade_out, None);
    assert_eq!(
        component.clip_duration,
        tt(25, state.active_sequence().unwrap().time_base())
    );
    assert_eq!(component.source_options.len(), 2);
    assert_eq!(
        component.source_options.iter().filter(|option| option.selected).count(),
        1
    );
    let binding = component.binding.as_ref().expect("media binding editor");
    assert_eq!(binding.asset_id, asset_id);
    assert_eq!(binding.options.len(), 2);
    assert_eq!(
        binding.options.iter().filter(|option| option.selected).count(),
        1
    );
    assert_eq!(
        component.channel_mapping.observed_source_layout,
        Some(AudioChannelLayout::Stereo)
    );
    assert_eq!(
        component.channel_mapping.destination_layout,
        AudioChannelLayout::Stereo
    );
    assert!(component
        .channel_mapping
        .review_matrix
        .as_ref()
        .is_some_and(mondrian_core::AudioChannelMixMatrix::is_identity));
    assert!(!component.channel_mapping.matrix_is_explicit);
    assert!(component.channel_mapping.diagnostic.is_none());
    assert_eq!(model.audio_processor_racks.len(), 1);
    assert_eq!(model.audio_processor_racks[0].processors.len(), 1);
    assert_eq!(model.audio_processor_racks[0].processors[0].label, "增益");

    drop(model);
    drop(state);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn inspector_model_uses_child_sequence_layout_for_nested_output_mapping() {
    let mut child = Sequence::new("Nested source");
    child.settings.audio_channel_layout = AudioChannelLayout::Surround51Side;
    let output_id = child.audio_program.outputs[0].id;

    let mut parent = Sequence::new("Nested parent");
    let track_id = parent.audio_tracks[0].id;
    let clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(25, parent.time_base()),
        None,
    )
    .expect("nested Clip");
    let clip_id = clip.id;
    parent
        .add_nested_audio_clip(track_id, clip, output_id)
        .expect("nested audio Clip");
    let mut state = AppState::new();
    state.test_set_sequence(Some(parent));
    state.test_set_sequences(vec![child]);
    state.selection.selected_clips =
        vec![SelectedClipRef { track_id, is_video_track: false, clip_id }];

    let model = InspectorPanelModel::from_app_state(&state);
    let mapping = &model.audio_components[0].channel_mapping;
    assert_eq!(
        mapping.observed_source_layout,
        Some(AudioChannelLayout::Surround51Side)
    );
    assert_eq!(mapping.destination_layout, AudioChannelLayout::Stereo);
    assert_eq!(
        mapping.review_matrix.as_ref().map(|matrix| matrix.entries().len()),
        Some(6)
    );
    assert!(mapping.diagnostic.is_none());
}

#[test]
fn inspector_model_projects_exact_forward_rate_and_playhead_hold_target() {
    let root = unique_temp_dir("inspector-source-timing");
    let library = AssetLibrary::open(root.join("library")).expect("asset library");
    let media_path = root.join("retime-source.mp4");
    std::fs::write(&media_path, [0u8]).expect("media fixture");
    let media_info = test_video_media_info(&media_path);
    let asset_id = commit_test_media_asset(&library, media_path.clone(), media_info);
    let mut sequence = Sequence::new("Inspector source timing");
    let time_base = sequence.time_base();
    let track_id = sequence.video_tracks[0].id;
    let mut clip = Clip::new(asset_id, TimelineTime::ZERO, tt(12, time_base)).expect("video Clip");
    clip.set_constant_source_time_map(
        TimelineTime::ZERO,
        TimeScale::new(3, 2).expect("exact 150 percent rate"),
    )
    .expect("set source-time map");
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add video Clip");
    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library));
    state.selection.selected_clips =
        vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
    state.seek(4).expect("seek");

    let model = InspectorPanelModel::from_app_state(&state);
    let source_timing = model.source_timing.expect("source-timing model");
    assert_eq!(
        source_timing.mode,
        InspectorSourceTimingMode::Rate { rate_percent: 150.0 }
    );
    assert!(source_timing.can_set_rate);
    assert!(source_timing.supports_picture_hold);
    assert_eq!(
        source_timing.freeze_at_playhead,
        Some(FramePosition::new(4, time_base))
    );

    state.active_sequence_mut_uncommitted().expect("active Sequence").video_tracks[0].clips[0]
        .set_constant_source_time_map(tt(6, time_base), TimeScale::new(0, 1).expect("exact hold"))
        .expect("set hold");
    let held_model = InspectorPanelModel::from_app_state(&state);
    let held_timing = held_model.source_timing.expect("held source-timing model");
    assert_eq!(held_timing.mode, InspectorSourceTimingMode::Hold);
    assert!(held_timing.can_set_rate);
    assert_eq!(
        held_timing.freeze_at_playhead,
        Some(FramePosition::new(4, time_base))
    );

    state.active_sequence_mut_uncommitted().expect("active Sequence").video_tracks[0].clips[0]
        .set_constant_source_time_map(tt(12, time_base), TimeScale::NEGATIVE_ONE)
        .expect("set reverse map fixture");
    let reverse_model = InspectorPanelModel::from_app_state(&state);
    let reverse_timing = reverse_model.source_timing.expect("reverse source-timing model");
    assert_eq!(
        reverse_timing.mode,
        InspectorSourceTimingMode::Rate { rate_percent: -100.0 }
    );
    assert!(reverse_timing.can_set_rate);
    assert!(reverse_timing.supports_picture_hold);
    assert_eq!(
        reverse_timing.freeze_at_playhead,
        Some(FramePosition::new(4, time_base))
    );

    drop(state);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn inspector_model_does_not_offer_retime_for_known_still_images() {
    let root = unique_temp_dir("inspector-still-source-timing");
    let library = AssetLibrary::open(root.join("library")).expect("asset library");
    let media_path = root.join("still.png");
    std::fs::write(&media_path, [0u8]).expect("media fixture");
    let mut media_info = test_video_media_info(&media_path);
    media_info.video_streams[0].total_frames = Some(1);
    let asset_id = commit_test_media_asset(&library, media_path.clone(), media_info);
    let mut sequence = Sequence::new("Inspector still");
    let time_base = sequence.time_base();
    let track_id = sequence.video_tracks[0].id;
    let clip =
        Clip::new_still_image(asset_id, TimelineTime::ZERO, tt(25, time_base)).expect("still Clip");
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add still Clip");
    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library));
    state.selection.selected_clips =
        vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

    let model = InspectorPanelModel::from_app_state(&state);

    assert_eq!(model.source_timing, None);
    drop(state);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn inspector_source_timing_actions_use_exact_typed_author_commands() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    assert_eq!(
        inspector_rate_action(Some(selection), 150.0),
        Some(crate::app::ui_actions::clip_set_rate_action(
            crate::app::product_action::ClipSetRatePayload {
                clip_id: selection.clip_id,
                rate: TimeScale::new(3, 2).expect("exact 150 percent rate"),
                include_linked: true,
            },
        ))
    );
    assert_eq!(
        inspector_rate_action(Some(selection), -33.33),
        Some(crate::app::ui_actions::clip_set_rate_action(
            crate::app::product_action::ClipSetRatePayload {
                clip_id: selection.clip_id,
                rate: TimeScale::new(-3_333, 10_000).expect("signed basis-point rate"),
                include_linked: true,
            },
        ))
    );
    assert_eq!(inspector_rate_action(None, 100.0), None);
    for invalid in [f32::NAN, f32::INFINITY, 0.0, 10_000.01, -10_000.01] {
        assert_eq!(inspector_rate_action(Some(selection), invalid), None);
    }

    let sequence_time = FramePosition::new(42, Rational::new(1, 25));
    assert_eq!(
        inspector_hold_action(Some(selection), Some(sequence_time)),
        Some(crate::app::ui_actions::clip_hold_frame_action(
            crate::app::product_action::ClipHoldFramePayload {
                clip_id: selection.clip_id,
                sequence_time,
            },
        ))
    );
    assert_eq!(inspector_hold_action(Some(selection), None), None);
    assert_eq!(
        inspector_hold_action(
            Some(SelectedClipRef { is_video_track: false, ..selection }),
            Some(sequence_time),
        ),
        None
    );
}

#[test]
fn inspector_model_exposes_basic_title_properties_in_canonical_order() {
    let mut sequence = Sequence::new("Inspector Basic Title");
    let track_id = sequence.video_tracks[0].id;
    let clip = Clip::new_basic_title(
        "Mondrian",
        mondrian_core::default_basic_title_font_family(),
        TimelineTime::ZERO,
        tt(25, sequence.time_base()),
    )
    .expect("Basic Title");
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("title placement");
    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips =
        vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];

    let model = InspectorPanelModel::from_app_state(&state);

    assert_eq!(
        model.clip_properties.len(),
        mondrian_core::BasicTitle::PROPERTY_PATHS.len()
    );
    assert_eq!(
        model
            .clip_properties
            .iter()
            .map(|property| property.path.as_str())
            .collect::<Vec<_>>(),
        mondrian_core::BasicTitle::PROPERTY_PATHS
    );
    assert!(!model.shows_tint);
    assert_eq!(model.source_timing, None);
    assert_eq!(
        model.clip_properties[0].value,
        PropertyValue::Text("Mondrian".to_owned())
    );
}

#[test]
fn asset_browser_tabs_expose_effect_browser() {
    let tabs = visible_tabs_for_slot(PanelKind::Assets, &[]);

    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0], PanelKind::Assets);
    assert_eq!(tabs[1], PanelKind::Effects);
}

fn dock_panel_for_kind(widget: &dyn Widget, kind: PanelKind) -> Option<&DockPanel> {
    if let Some(panel) = widget.as_any().and_then(|any| any.downcast_ref::<DockPanel>())
        && panel.kind() == kind
    {
        return Some(panel);
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index)
            && let Some(panel) = dock_panel_for_kind(child, kind)
        {
            return Some(panel);
        }
    }
    None
}

#[test]
fn custom_layout_hidden_grouped_tabs_filter_asset_browser_tabs() {
    let layout = AppUiWorkspaceLayout::Split {
        direction: SplitDirection::Horizontal,
        ratio: 0.37,
        first: Box::new(AppUiWorkspaceLayout::Panel {
            kind: PanelKind::Assets,
            active_index: 1,
            hidden_tabs: vec![PanelKind::Effects],
            tabs: Vec::new(),
        }),
        second: Box::new(AppUiWorkspaceLayout::Panel {
            kind: PanelKind::Viewer,
            active_index: 0,
            hidden_tabs: Vec::new(),
            tabs: vec![PanelKind::Viewer],
        }),
    };

    let dock = build_dock_tree_from_layout(AppUiPanelModels::demo(), &layout).expect("dock tree");
    let assets = dock_panel_for_kind(&dock, PanelKind::Assets).expect("assets panel");

    assert_eq!(assets.tab_count(), 1);
    assert_eq!(assets.active_index(), 0);
}

fn panel_at_point(widget: &dyn Widget, point: Point) -> Option<PanelKind> {
    if !widget.hit_test(point) {
        return None;
    }
    for index in (0..widget.child_count()).rev() {
        if let Some(child) = widget.child(index)
            && let Some(kind) = panel_at_point(child, point)
        {
            return Some(kind);
        }
    }
    widget.panel_kind()
}

#[test]
fn editing_workspace_uses_full_width_bottom_timeline() {
    let mut dock = build_dock_tree_for_preset(AppUiPanelModels::demo(), WorkspacePreset::Editing);
    dock.layout(Rect::new(0.0, 0.0, 1000.0, 600.0));

    assert_eq!(
        panel_at_point(&dock, Point::new(120.0, 90.0)),
        Some(PanelKind::Assets)
    );
    assert_eq!(
        panel_at_point(&dock, Point::new(520.0, 90.0)),
        Some(PanelKind::Viewer)
    );
    assert_eq!(
        panel_at_point(&dock, Point::new(900.0, 90.0)),
        Some(PanelKind::Inspector)
    );
    assert_eq!(
        panel_at_point(&dock, Point::new(500.0, 520.0)),
        Some(PanelKind::Timeline)
    );
}

#[test]
fn built_in_workspace_presets_route_primary_regions_to_expected_panels() {
    let cases = [
        (
            WorkspacePreset::Color,
            Point::new(500.0, 90.0),
            PanelKind::Viewer,
        ),
        (
            WorkspacePreset::Color,
            Point::new(500.0, 520.0),
            PanelKind::Timeline,
        ),
        (
            WorkspacePreset::Color,
            Point::new(900.0, 90.0),
            PanelKind::Inspector,
        ),
        (
            WorkspacePreset::Audio,
            Point::new(500.0, 520.0),
            PanelKind::Timeline,
        ),
        (
            WorkspacePreset::Audio,
            Point::new(900.0, 520.0),
            PanelKind::Mixer,
        ),
        (
            WorkspacePreset::Compositing,
            Point::new(180.0, 90.0),
            PanelKind::NodeGraph,
        ),
        (
            WorkspacePreset::Compositing,
            Point::new(180.0, 520.0),
            PanelKind::Effects,
        ),
        (
            WorkspacePreset::Export,
            Point::new(180.0, 90.0),
            PanelKind::Export,
        ),
        (
            WorkspacePreset::Export,
            Point::new(700.0, 90.0),
            PanelKind::Viewer,
        ),
    ];

    for (preset, point, expected) in cases {
        let mut dock = build_dock_tree_for_preset(AppUiPanelModels::demo(), preset);
        dock.layout(Rect::new(0.0, 0.0, 1000.0, 600.0));

        assert_eq!(
            panel_at_point(&dock, point),
            Some(expected),
            "{preset:?} should route {point:?} to {expected:?}"
        );
    }
}

#[test]
fn app_state_models_are_safe_without_an_open_project() {
    let state = AppState::new();
    let models = AppUiPanelModels::from_app_state(&state);

    assert!(models.timeline.tracks.is_empty());
    assert_eq!(models.timeline.playhead_frame, 0);
    assert!(!models.timeline.enabled);
    assert_eq!(
        models.timeline.empty_message.as_deref(),
        Some("未载入序列\n打开项目或创建序列以开始编辑")
    );
    assert!(!timeline_panel(&models.timeline).can_focus());
    assert_eq!(models.assets.items[0].title, "没有项目素材库");
    assert!(models.assets.items[0].icon.is_some());
    assert!(models.assets.items[0].disabled);
    assert!(!models.effects.items.is_empty());
    assert_eq!(models.viewer.title, "预览");
    assert!(!models.viewer.enabled);
    assert_eq!(models.viewer.status_tone, ViewerStatusTone::Neutral);
    assert_eq!(models.viewer.empty_message.as_deref(), Some("未载入序列"));
    assert_eq!(models.viewer.resolution_label, "无信号");
    assert_eq!(models.viewer.position_label, "00:00:00:00");
    assert_eq!(models.viewer.zoom_label, "适合");
    assert_eq!(models.viewer.preview_quality_label, "1/1");
    assert_eq!(models.inspector.selected_clip, None);
    assert_eq!(
        models.inspector.empty_message.as_deref(),
        Some("未选择剪辑\n选择剪辑、图层或效果后，可在这里调整参数。")
    );
    assert!(!models.inspector.is_editable);
    assert_eq!(models.inspector.edit_disabled_reason, None);
    assert_eq!(models.inspector.opacity, 100.0);
    assert!(models.inspector.effects.is_empty());
    assert!(models.export.sequences.is_empty());
    assert!(!models.export.can_enqueue());
    assert!(models.node_graph.nodes.is_empty());
    assert!(models.node_graph.edges.is_empty());
    assert_eq!(models.node_graph.subtitle, "选择剪辑以检查渲染链");
    let node_graph = node_graph_panel(&models.node_graph);
    assert!(!node_graph.is_enabled());
    assert!(!node_graph.can_focus());
}

#[test]
fn sequence_backed_empty_timeline_keeps_add_track_entrypoints_enabled() {
    let mut sequence = Sequence::new("empty edit");
    sequence.video_tracks.clear();
    sequence.audio_tracks.clear();
    sequence.audio_program = mondrian_timeline::AudioProgram::for_tracks([]);

    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);

    assert!(model.enabled);
    assert!(model.tracks.is_empty());
    assert_eq!(
        model.empty_message.as_deref(),
        Some("当前序列没有轨道\n添加视频轨道或音频轨道后开始编辑")
    );
    assert!(timeline_panel(&model).can_focus());
}

#[test]
fn empty_inspector_panel_shows_status_only_and_does_not_dispatch_clip_controls() {
    let model = InspectorPanelModel::empty();
    let mut panel = inspector_panel(&model);
    panel.layout(Rect::new(0.0, 0.0, 320.0, 220.0));

    assert_eq!(
        model.empty_message.as_deref(),
        Some("未选择剪辑\n选择剪辑、图层或效果后，可在这里调整参数。")
    );
    assert_eq!(panel.section_count(), 0);

    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        panel.event(
            &UiEvent::MouseDown {
                position: Point::new(132.0, 94.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Ignored
    );
    assert!(actions.borrow().is_empty());
}

#[test]
fn node_graph_without_clip_selection_uses_disabled_empty_state() {
    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));

    let model = NodeGraphPanelModel::from_app_state(&state);

    assert!(model.nodes.is_empty());
    assert!(model.edges.is_empty());
    assert_eq!(model.selected_clip, None);
    assert_eq!(model.subtitle, "选择剪辑以检查渲染链");
    let panel = node_graph_panel(&model);
    assert!(!panel.is_enabled());
    assert!(!panel.can_focus());
}

#[test]
fn viewer_panel_play_pause_control_dispatches_toggle_play() {
    let mut viewer = viewer_panel(&AppUiPanelModels::demo().viewer);
    viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
    let play_pause_center = Point::new(250.0, 299.0);
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        viewer.event(
            &UiEvent::MouseDown {
                position: play_pause_center,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        viewer.event(
            &UiEvent::MouseUp {
                position: play_pause_center,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );

    assert_eq!(actions.borrow().as_slice(), &[Action::TogglePlay]);
}

#[test]
fn viewer_panel_disabled_controls_do_not_dispatch() {
    let mut viewer = viewer_panel(&ViewerPanelModel::empty());
    viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
    let play_pause_center = Point::new(281.0, 299.0);
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        viewer.event(
            &UiEvent::MouseDown {
                position: play_pause_center,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            &mut ctx,
        ),
        EventResult::Ignored
    );
    assert_eq!(
        viewer.event(
            &UiEvent::MouseUp {
                position: play_pause_center,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            &mut ctx,
        ),
        EventResult::Ignored
    );

    assert!(actions.borrow().is_empty());
}

#[test]
fn viewer_control_action_maps_every_transport_control() {
    let cases = [
        (ViewerControl::MarkIn, Action::MarkInAtPlayhead),
        (ViewerControl::MarkOut, Action::MarkOutAtPlayhead),
        (ViewerControl::JumpStart, Action::GoToStart),
        (ViewerControl::StepBack, Action::StepBack),
        (ViewerControl::PlayPause, Action::TogglePlay),
        (ViewerControl::StepForward, Action::StepForward),
        (ViewerControl::JumpEnd, Action::GoToEnd),
    ];

    for (control, action) in cases {
        assert_eq!(viewer_control_action(control), action);
    }
}

#[test]
fn export_panel_model_reads_app_export_draft() {
    let mut state = AppState::new();
    let sequence = Sequence::new("Deliverable");
    let sequence_id = sequence.id;
    state.test_set_sequence(Some(sequence));
    state.set_export_draft_builtin_preset(BuiltinExportPreset::HevcMain10Aac);
    state.set_export_draft_sequence_id(Some(sequence_id));
    state.set_export_draft_range(TimelineExportRange::EntireSequence);
    state.set_export_draft_output_path("E:/renders/deliverable.mp4");
    state.set_status_hint("Ready to export", false);

    let model = ExportPanelModel::from_app_state(&state);

    assert_eq!(model.selected_preset_idx, 1);
    assert_eq!(model.selected_sequence_id, Some(sequence_id));
    assert_eq!(model.range, TimelineExportRange::EntireSequence);
    assert_eq!(model.output_path, "E:/renders/deliverable.mp4");
    assert_eq!(model.sequences.len(), 1);
    assert_eq!(model.sequences[0].name, "Deliverable");
    assert!(model.can_select_range());
    assert!(model.can_choose_output());
    assert!(model.can_enqueue());
    assert_eq!(model.readiness_status(), "Ready to export");
    let payload = model.enqueue_request().expect("enqueue request");
    assert_eq!(payload.sequence_id, Some(sequence_id));
    assert_eq!(payload.range, TimelineExportRange::EntireSequence);
    assert_eq!(
        payload.output_path,
        std::path::PathBuf::from("E:/renders/deliverable.mp4")
    );
    assert_eq!(payload.preset, BuiltinExportPreset::HevcMain10Aac.preset());
    assert!(!model.preset_customized);
}

#[test]
fn export_codec_menu_exposes_the_complete_professional_mezzanine_matrix() {
    let items = export_video_codec_items(&ExportPreset::h264_aac_sdr_1080p());
    let labels = items.iter().map(|item| item.label.as_str()).collect::<Vec<_>>();
    for expected in [
        "DNxHR LB",
        "DNxHR SQ",
        "DNxHR HQ",
        "DNxHR HQX",
        "DNxHR 444 RGB",
        "AVC-Intra Class 100",
        "AVC-Intra Class 200",
        "Uncompressed YUV 4:2:2 8-bit (2vuy)",
        "Uncompressed YUV 4:2:2 10-bit (v210)",
        "Uncompressed RGB 8-bit",
        "Uncompressed RGB 10-bit (r210)",
    ] {
        assert!(
            labels.contains(&expected),
            "missing codec menu item: {expected}"
        );
    }

    let presets = builtin_export_presets();
    for expected in [
        BuiltinExportPreset::DnxHrHqx,
        BuiltinExportPreset::AvcIntra100,
        BuiltinExportPreset::UncompressedV210,
        BuiltinExportPreset::UncompressedR210,
    ] {
        assert!(presets.iter().any(|preset| preset.id == expected));
    }
}

#[test]
fn export_panel_validates_the_materialized_signal_draft_and_submits_it_exactly() {
    let mut state = AppState::new();
    let sequence = Sequence::new("Deliverable");
    let sequence_id = sequence.id;
    state.test_set_sequence(Some(sequence));
    state.set_export_draft_builtin_preset(BuiltinExportPreset::H264AacSdr1080p);
    state.set_export_draft_sequence_id(Some(sequence_id));
    state.set_export_draft_output_path("E:/renders/deliverable.mp4");

    let mut invalid = state.export_draft.preset.clone();
    invalid.video_signal.bit_depth = ExportParameter::Explicit(DeliveryBitDepth::Ten);
    state.set_export_draft_preset(invalid);
    let blocked = ExportPanelModel::from_app_state(&state);

    assert!(blocked.preset_customized);
    assert!(!blocked.can_enqueue());
    assert!(blocked.delivery_error.as_deref().is_some_and(|error| error.contains("位深")));

    let mut valid = state.export_draft.preset.clone();
    valid.video_signal.bit_depth = ExportParameter::Explicit(DeliveryBitDepth::Eight);
    state.set_export_draft_preset(valid.clone());
    let ready = ExportPanelModel::from_app_state(&state);
    let payload = ready.enqueue_request().expect("valid edited preset");

    assert!(!ready.preset_customized);
    assert_eq!(payload.preset, valid);
}

#[test]
fn export_color_target_modes_expose_only_semantically_valid_spaces() {
    let media_preset = ExportPreset::h264_aac_sdr_1080p();
    let rendering = export_color_target_spaces(ExportColorTargetMode::RenderingView, &media_preset);
    assert!(rendering.contains(&ColorSpace::Rec709));
    assert!(rendering.contains(&ColorSpace::Rec2100Hlg));
    assert!(rendering.contains(&ColorSpace::Rec2100Pq));
    assert!(!rendering.contains(&ColorSpace::AppleLogBt2020));
    assert!(!rendering.contains(&ColorSpace::LinearRec709));

    let colorimetric =
        export_color_target_spaces(ExportColorTargetMode::Colorimetric, &media_preset);
    assert!(colorimetric.contains(&ColorSpace::Rec709));
    assert!(colorimetric.contains(&ColorSpace::AppleLogBt2020));
    assert!(!colorimetric.contains(&ColorSpace::LinearRec709));
    assert!(!colorimetric.contains(&ColorSpace::Aces2065_1));

    let image_master = export_color_target_spaces(
        ExportColorTargetMode::Colorimetric,
        &ExportPreset::open_exr_float_sequence(),
    );
    assert!(image_master.contains(&ColorSpace::LinearRec709));
    assert!(image_master.contains(&ColorSpace::Aces2065_1));
    assert!(image_master.contains(&ColorSpace::AcesCg));
}

#[test]
fn export_color_target_mode_change_preserves_only_legal_endpoints() {
    assert_eq!(
        export_color_target_with_mode(
            ExportColorTarget::Colorimetric(ColorSpace::Rec2100Pq),
            ExportColorTargetMode::RenderingView,
        ),
        ExportColorTarget::RenderingView(ColorSpace::Rec2100Pq)
    );
    assert_eq!(
        export_color_target_with_mode(
            ExportColorTarget::Colorimetric(ColorSpace::AppleLogBt2020),
            ExportColorTargetMode::RenderingView,
        ),
        ExportColorTarget::RenderingView(ColorSpace::Rec709)
    );
    assert_eq!(
        export_color_target_with_mode(
            ExportColorTarget::FollowSequence,
            ExportColorTargetMode::Colorimetric,
        ),
        ExportColorTarget::Colorimetric(ColorSpace::Rec709)
    );
}

#[test]
fn export_panel_submits_an_explicit_log_target_without_mutating_the_sequence() {
    let mut state = AppState::new();
    let sequence = Sequence::new("Log Deliverable");
    let sequence_id = sequence.id;
    let program_output = sequence.settings.color.program_output.clone();
    state.test_set_sequence(Some(sequence));
    state.set_export_draft_sequence_id(Some(sequence_id));
    state.set_export_draft_output_path("E:/renders/log.mov");

    let mut preset = ExportPreset::prores_4444_alpha();
    preset.alpha_mode = ExportAlphaMode::FlattenBlack;
    preset.color_target = ExportColorTarget::Colorimetric(ColorSpace::AppleLogBt2020);
    state.set_export_draft_preset(preset.clone());

    let model = ExportPanelModel::from_app_state(&state);
    let payload = model.enqueue_request().expect("valid explicit log target");

    assert_eq!(payload.preset, preset);
    assert_eq!(
        state.active_sequence().expect("active sequence").settings.color.program_output,
        program_output
    );
}

#[test]
fn export_panel_model_does_not_build_enqueue_request_when_disabled() {
    let mut state = AppState::new();
    let sequence = Sequence::new("Deliverable");
    let sequence_id = sequence.id;
    state.test_set_sequence(Some(sequence));
    state.set_export_draft_sequence_id(Some(sequence_id));
    state.set_export_draft_output_path("   ");

    let model = ExportPanelModel::from_app_state(&state);

    assert!(!model.can_enqueue());
    assert!(model.can_select_range());
    assert!(model.can_choose_output());
    assert_eq!(model.readiness_status(), "选择输出路径后即可加入队列");
    assert!(model.enqueue_request().is_none());
}

#[test]
fn export_panel_rejects_incompatible_delivery_before_building_an_action() {
    let mut state = AppState::default();
    let mut sequence = Sequence::new("HDR Deliverable");
    sequence.settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
    let sequence_id = sequence.id;
    state.test_set_sequence(Some(sequence));
    state.set_export_draft_builtin_preset(BuiltinExportPreset::H264AacSdr1080p);
    let mut incompatible = state.export_draft.preset.clone();
    incompatible.color_target = ExportColorTarget::RenderingView(ColorSpace::Rec2100Pq);
    state.set_export_draft_preset(incompatible);
    state.set_export_draft_sequence_id(Some(sequence_id));
    state.set_export_draft_output_path("E:/renders/hdr.mp4");
    state.set_status_hint("stale success must not hide the blocker", false);

    let model = ExportPanelModel::from_app_state(&state);

    assert!(!model.can_enqueue());
    assert!(model.enqueue_request().is_none());
    assert!(model.delivery_error.as_deref().is_some_and(|error| error.contains("HDR")));
    assert!(model.readiness_status().starts_with("交付设置不兼容："));
}

#[test]
fn export_panel_preserves_professional_metadata_and_reports_the_physical_artifact() {
    let mut state = AppState::new();
    let sequence = Sequence::new("IMF Deliverable");
    let sequence_id = sequence.id;
    state.test_set_sequence(Some(sequence));
    state.set_export_draft_builtin_preset(BuiltinExportPreset::ImfAppProResRdd45);
    state.set_export_draft_sequence_id(Some(sequence_id));
    state.set_export_draft_output_path("E:/renders/master.imf");
    let preset = state.export_draft.preset.clone();

    let action = export_professional_metadata_action(
        preset,
        ProfessionalMetadataField::Title,
        "Festival Master",
    );
    let ProductAction::Export(ExportProductAction::EditDraft(edit)) =
        ProductAction::decode_external(&action)
            .expect("valid export metadata payload")
            .expect("recognized export metadata action")
    else {
        panic!("expected export draft edit");
    };
    let ExportDraftEdit::Preset(updated) = *edit else {
        panic!("expected materialized preset edit");
    };
    assert_eq!(
        updated.professional_delivery().expect("professional output").metadata.title,
        "Festival Master"
    );

    let model = ExportPanelModel::from_app_state(&state);
    assert!(model.can_enqueue());
    let summary = export_preset_summary(Some(&model.preset));
    assert!(summary.contains("IMF RDD 45"));
    assert!(summary.contains("ProRes 422 HQ 10-bit 4:2:2"));
    assert!(summary.contains("PCM 24-bit 48 kHz stereo"));
    assert!(summary.contains("directory package"));
    assert_eq!(
        export_default_file_name(Some(&model.preset)),
        "mondrian-export.imf"
    );
}

#[test]
fn export_panel_model_disables_sequence_scoped_controls_without_sequences() {
    let state = AppState::new();

    let model = ExportPanelModel::from_app_state(&state);

    assert!(model.sequences.is_empty());
    assert_eq!(model.selected_sequence_id, None);
    assert!(!model.can_select_range());
    assert!(!model.can_choose_output());
    assert!(!model.can_enqueue());
    assert_eq!(model.readiness_status(), "导出前请打开或选择序列");
    assert!(model.enqueue_request().is_none());
}

#[test]
fn export_panel_formats_structured_queue_status_and_diagnostics() {
    let mut input_counts = mondrian_timeline::sequence::InputColorResolutionSourceCounts::default();
    input_counts.record(mondrian_timeline::sequence::InputColorResolutionSource::DetectedMetadata);
    input_counts.record(mondrian_timeline::sequence::InputColorResolutionSource::Override);
    let mut diagnostics = ExportJobColorDiagnostics::default();
    diagnostics.record_frame_diagnostics(
        input_counts,
        mondrian_renderer::color::RenderColorStageDiagnostics {
            total_stages: 2,
            cpu_input_stages: 1,
            cpu_output_stages: 1,
            gpu_blockers: 1,
            gpu_blocker_breakdown: mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown {
                render_pipeline_not_prepared: 1,
                ..mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown::default()
            },
            stage_pixels: 960 * 540 * 2,
            ..mondrian_renderer::color::RenderColorStageDiagnostics::default()
        },
        mondrian_renderer::TimelineCompositeDiagnostics {
            elements: 2,
            float_linear_composites: 1,
            ..mondrian_renderer::TimelineCompositeDiagnostics::default()
        },
    );
    diagnostics.record_asset_issue_summary(VideoColorDiagnosticIssueAggregate {
        diagnostics: 2,
        diagnostics_with_warnings: 2,
        missing_cicp_tags: 1,
        unsupported_cicp_tags: 0,
        decoder_unavailable: 1,
        ..VideoColorDiagnosticIssueAggregate::default()
    });
    let legacy_summary = diagnostics.composite_color_path_summary();
    assert_eq!(legacy_summary.float_linear_composites, 1);
    assert_eq!(legacy_summary.legacy_rgba8_composites, 0);
    let progress = ExportProgress {
        phase: ExportProgressPhase::Encoding,
        fraction: 0.82,
        detail: ExportProgressDetail::None,
    };
    assert_eq!(
        export_job_status_label(
            &JobStatus::Running { phase: ExportProgressPhase::Encoding },
            progress,
        ),
        "Encoding"
    );
    assert_eq!(
        export_job_status_label(
            &JobStatus::Running { phase: ExportProgressPhase::Packaging },
            ExportProgress {
                phase: ExportProgressPhase::Packaging,
                fraction: 0.9,
                detail: ExportProgressDetail::None,
            },
        ),
        "Packaging"
    );
    assert_eq!(
        export_job_status_label(
            &JobStatus::Failed(ExportFailure {
                reason: ExportFailureReason::ExecutionFailed,
                detail: "disk full".to_owned(),
            }),
            progress,
        ),
        "Failed: disk full"
    );
    let color_diagnostics =
        export_job_color_diagnostics_label(diagnostics).expect("encoding color diagnostics");
    assert!(color_diagnostics.contains("metadata 1 / override 1 / policy 0 / data 0 / reject 0"));
    assert!(color_diagnostics.contains("assets 2 / issues warn 2 missing-cicp 1 decoder 1"));
    assert!(color_diagnostics.contains("report Fail"));
    assert!(color_diagnostics.contains("warn:asset_color_diagnostics_warning"));
    assert!(color_diagnostics.contains("export_gpu_color_stage_blocked"));
    assert!(color_diagnostics.contains("actions inspect_asset_color_warning_evidence"));
    assert!(color_diagnostics.contains("gpu blockers shader 0 resource 0 wrapper 0 pipeline 1"));
}

#[test]
fn app_ui_content_factory_covers_every_panel_kind() {
    let models = AppUiPanelModels::from_app_state(&AppState::new());
    let constraint = LayoutConstraint { min: Size::ZERO, max: Size::new(320.0, 240.0) };

    for kind in PanelKind::ALL {
        let widget = panel_content_for_slot(kind, &models);
        let measured = widget.measure(constraint);

        assert!(
            measured.width.is_finite(),
            "{kind:?} width should be finite"
        );
        assert!(
            measured.height.is_finite(),
            "{kind:?} height should be finite"
        );
    }
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
fn assets_panel_file_drop_dispatches_import_files_action_for_current_folder() {
    let model = AssetGridModel::new(
        "Assets",
        vec![AssetGridItem::new(
            "drop-target",
            "Drop target",
            current_theme().colors.media_video,
        )
        .with_subtitle("Project library")],
    )
    .accepts_file_drop(true)
    .with_current_folder_id(Some("rushes".to_owned()));
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 320.0, 180.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );
    let path = PathBuf::from("E:/media/clip.mov");

    let result = grid.event(
        &UiEvent::Drop {
            payload: DragPayload::File(vec![path.clone()]),
            position: Point::new(24.0, 76.0),
        },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected import-files custom action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_IMPORT_FILES);
    let payload: AssetsImportFilesPayload =
        serde_json::from_value(payload.clone()).expect("import files payload");
    assert_eq!(payload.paths, vec![path]);
    assert_eq!(payload.folder_id.as_deref(), Some("rushes"));
}

#[test]
fn assets_panel_card_drop_moves_asset_into_folder_card() {
    let asset_id = AssetId::new();
    let model = AssetGridModel::new(
        "Assets",
        vec![AssetGridItem::new(
            "folder:rushes",
            "Rushes",
            current_theme().colors.secondary,
        )],
    )
    .accepts_file_drop(true);
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 360.0, 240.0));
    let card = grid.card_rect_for_index(0).expect("folder card");
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    let result = grid.event(
        &UiEvent::Drop {
            payload: DragPayload::Asset(asset_id),
            position: card.center(),
        },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected move asset custom action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_MOVE_ENTRIES);
    let payload: AssetsMoveSelectionPayload =
        serde_json::from_value(payload.clone()).expect("move asset payload");
    assert_eq!(payload.asset_ids, vec![asset_id]);
    assert!(payload.folder_ids.is_empty());
    assert_eq!(payload.target_folder_id.as_deref(), Some("rushes"));
}

#[test]
fn assets_panel_grid_drop_moves_folder_to_current_folder() {
    let model = AssetGridModel::new("Assets", Vec::new())
        .accepts_file_drop(true)
        .with_current_folder_id(Some("parent".to_owned()));
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 360.0, 240.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    let result = grid.event(
        &UiEvent::Drop {
            payload: DragPayload::AssetFolder("child".to_owned()),
            position: Point::new(24.0, 96.0),
        },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected move folder custom action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_MOVE_ENTRIES);
    let payload: AssetsMoveSelectionPayload =
        serde_json::from_value(payload.clone()).expect("move folder payload");
    assert!(payload.asset_ids.is_empty());
    assert_eq!(payload.folder_ids, vec!["child"]);
    assert_eq!(payload.target_folder_id.as_deref(), Some("parent"));
}

#[test]
fn assets_panel_card_drop_moves_asset_selection_into_folder_card() {
    let first_asset = AssetId::new();
    let second_asset = AssetId::new();
    let model = AssetGridModel::new(
        "Assets",
        vec![AssetGridItem::new(
            "folder:rushes",
            "Rushes",
            current_theme().colors.secondary,
        )],
    )
    .accepts_file_drop(true);
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 360.0, 240.0));
    let card = grid.card_rect_for_index(0).expect("folder card");
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    let result = grid.event(
        &UiEvent::Drop {
            payload: DragPayload::AssetSelection {
                assets: vec![first_asset, second_asset],
                folders: vec!["rushes".to_owned(), "selects".to_owned()],
            },
            position: card.center(),
        },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected move selection custom action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_MOVE_ENTRIES);
    let payload: AssetsMoveSelectionPayload =
        serde_json::from_value(payload.clone()).expect("move selection payload");
    assert_eq!(payload.asset_ids, vec![first_asset, second_asset]);
    assert_eq!(payload.folder_ids, vec!["selects"]);
    assert_eq!(payload.target_folder_id.as_deref(), Some("rushes"));
}

#[test]
fn assets_panel_context_menu_uses_shell_and_asset_actions() {
    let items = asset_grid_context_menu_items(None);

    assert_eq!(items.len(), 3);
    assert_shell_action(items[0].action(), APP_SHELL_IMPORT_MEDIA_DIALOG);
    assert!(items[1].is_separator());
    assert_eq!(items[2].label, "新建");
    // Verify submenu children
    match &items[2].kind {
        MenuItemKind::Submenu { children } => {
            assert_eq!(children.len(), 3);
            assert_assets_action(children[0].action(), ASSET_CREATE_GENERATED);
            assert_assets_action(children[1].action(), ASSET_CREATE_GENERATED);
            assert_assets_action(children[2].action(), ASSET_CREATE_FOLDER);
        }
        _ => panic!("expected 新建 submenu"),
    }
}

#[test]
fn assets_panel_context_menu_creates_folders_inside_current_folder() {
    let items = asset_grid_context_menu_items(Some("rushes"));

    let Action::Custom { namespace, name, payload } =
        items[0].action().expect("import dialog action")
    else {
        panic!("expected import dialog custom action");
    };
    assert_eq!(namespace, APP_SHELL_NAMESPACE);
    assert_eq!(name, APP_SHELL_IMPORT_MEDIA_DIALOG);
    let payload: ImportMediaDialogPayload =
        serde_json::from_value(payload.clone()).expect("import dialog payload");
    assert_eq!(payload.folder_id.as_deref(), Some("rushes"));

    // Verify 新建 submenu children carry the folder context.
    let children = match &items[2].kind {
        MenuItemKind::Submenu { children } => children,
        _ => panic!("expected 新建 submenu"),
    };
    assert_assets_action(children[0].action(), ASSET_CREATE_GENERATED);
    assert_assets_action(children[1].action(), ASSET_CREATE_GENERATED);
    assert_assets_action(children[2].action(), ASSET_CREATE_FOLDER);
}

#[test]
fn assets_panel_context_menu_dispatches_import_from_grid_overlay() {
    let model = AssetGridModel::new(
        "Assets",
        vec![AssetGridItem::new(
            "context-target",
            "Context target",
            current_theme().colors.media_video,
        )],
    )
    .accepts_file_drop(true);
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 320.0, 180.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        grid.event(
            &UiEvent::MouseDown {
                position: Point::new(24.0, 76.0),
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert!(grid.overlay_hit_test(Point::new(640.0, 480.0)));
    assert_eq!(
        grid.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    assert_eq!(actions.borrow().len(), 1);
    assert_shell_action(actions.borrow().first(), APP_SHELL_IMPORT_MEDIA_DIALOG);
}

#[test]
fn assets_panel_card_context_menu_dispatches_delete_asset() {
    let root = unique_temp_dir("asset-panel-delete-menu");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let asset_id = library.create_solid_color_asset(Some("Temp Plate")).expect("create asset");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));
    let model = AppUiPanelModels::from_app_state(&state).assets;
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 360.0, 240.0));
    let card = grid.card_rect_for_index(0).expect("asset card");
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        grid.event(
            &UiEvent::MouseDown {
                position: card.center(),
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    for _ in 0..2 {
        assert_eq!(
            grid.event(
                &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
    }
    assert_eq!(
        grid.event(
            &UiEvent::MouseDown {
                position: Point::new(card.center().x + 20.0, card.center().y + 69.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected asset delete action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REMOVE_ENTRIES);
    let payload: AssetsDeleteSelectionPayload =
        serde_json::from_value(payload.clone()).expect("delete payload");
    assert_eq!(payload.asset_ids, vec![asset_id]);
    assert!(payload.folder_ids.is_empty());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn assets_panel_file_card_context_menu_dispatches_interpret_first() {
    let root = unique_temp_dir("asset-panel-file-context-menu");
    std::fs::create_dir_all(&root).expect("create temp root");
    let path = root.join("shot.mov");
    std::fs::write(&path, b"fixture").expect("write media");
    let asset = test_video_asset(path.clone());
    let asset_id = asset.id;
    let item = asset_grid_item_from_asset(asset, None, false, None);
    let model = AssetGridModel::new("Assets", vec![item]);
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 360.0, 240.0));
    let card = grid.card_rect_for_index(0).expect("asset card");
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        grid.event(
            &UiEvent::MouseDown {
                position: card.center(),
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    for _ in 0..2 {
        assert_eq!(
            grid.event(
                &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
    }
    assert_eq!(
        grid.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected interpret action");
    };
    assert_eq!(namespace, APP_SHELL_NAMESPACE);
    assert_eq!(name, APP_SHELL_INTERPRET_ASSET_DIALOG);
    let payload: AppShellInterpretAssetDialogPayload =
        serde_json::from_value(payload.clone()).expect("interpret payload");
    assert_eq!(payload.asset_id, asset_id);
    assert_eq!(payload.asset_name, "shot.mov");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn assets_panel_inline_rename_dispatches_asset_rename_action() {
    let root = unique_temp_dir("asset-panel-inline-rename");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let asset_id = library.create_solid_color_asset(Some("Old Plate")).expect("create asset");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));
    let model = AppUiPanelModels::from_app_state(&state).assets;
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 360.0, 240.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    grid.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
    grid.event(
        &UiEvent::MouseDown {
            position: grid.card_rect_for_index(0).expect("asset card").center(),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    grid.event(
        &UiEvent::KeyDown { key: KeyCode::F2, modifiers: Modifiers::none() },
        &mut ctx,
    );
    grid.event(&UiEvent::TextInput("New Plate".to_owned()), &mut ctx);
    grid.event(
        &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
        &mut ctx,
    );

    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected asset rename action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_RENAME);
    let payload: AssetsRenameAssetPayload =
        serde_json::from_value(payload.clone()).expect("rename payload");
    assert_eq!(payload.asset_id, asset_id);
    assert_eq!(payload.name, "New Plate");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn assets_panel_offline_file_card_context_menu_includes_relink() {
    let root = unique_temp_dir("asset-panel-offline-context-menu");
    std::fs::create_dir_all(&root).expect("create temp root");
    let media_path = root.join("shot.mov");
    std::fs::write(&media_path, b"fixture").expect("write media");
    let asset = test_video_asset(media_path.clone());
    let asset_id = asset.id;
    std::fs::remove_file(&media_path).expect("make fixture offline");
    let input_pipeline = AppShellInputColorPipelineDiagnostics {
        engine: mondrian_core::ColorEngine::mondrian_standard(),
        working_color_space: WorkingColorSpace::LinearP3D65,
    };
    let item = asset_grid_item_from_asset(asset, None, false, Some(&input_pipeline));

    assert_eq!(badge_labels(&item), ["视频", "离线"]);
    assert_eq!(item.badges[1].tone, AssetGridBadgeTone::Warning);
    assert_eq!(item.context_menu_items.len(), 5);
    assert_eq!(item.context_menu_items[0].label, "解释素材...");
    assert_eq!(item.context_menu_items[1].label, "在文件管理器中显示");
    assert_eq!(item.context_menu_items[2].label, "重新链接媒体...");
    assert!(item.context_menu_items[3].is_separator());
    assert_eq!(item.context_menu_items[4].label, "删除素材");
    let Action::Custom { namespace, name, payload } =
        item.context_menu_items[2].action().expect("relink shell action")
    else {
        panic!("expected relink shell action");
    };
    assert_eq!(namespace, APP_SHELL_NAMESPACE);
    assert_eq!(name, APP_SHELL_RELINK_ASSET_DIALOG);
    let payload: AppShellRelinkAssetDialogPayload =
        serde_json::from_value(payload.clone()).expect("relink payload");
    assert_eq!(payload.asset_id, asset_id);

    let Action::Custom { namespace, name, payload } =
        item.context_menu_items[0].action().expect("interpret shell action")
    else {
        panic!("expected interpret shell action");
    };
    assert_eq!(namespace, APP_SHELL_NAMESPACE);
    assert_eq!(name, APP_SHELL_INTERPRET_ASSET_DIALOG);
    let payload: AppShellInterpretAssetDialogPayload =
        serde_json::from_value(payload.clone()).expect("interpret payload");
    assert_eq!(payload.asset_id, asset_id);
    assert_eq!(payload.asset_name, "shot.mov");
    let signal = payload.video_signal.expect("primary-video signal diagnostics");
    assert_eq!(signal.range, mondrian_media::DecodedVideoRange::Limited);
    assert_eq!(
        signal.color_metadata.expect("raw CICP metadata").primaries.name.as_deref(),
        Some("bt709")
    );
    assert_eq!(
        payload.input_pipeline.expect("effective input pipeline").working_color_space,
        WorkingColorSpace::LinearP3D65
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn assets_panel_online_video_card_context_menu_toggles_proxy_mode() {
    let root = unique_temp_dir("asset-panel-proxy-menu");
    std::fs::create_dir_all(&root).expect("create temp root");
    let media_path = root.join("shot.mov");
    std::fs::write(&media_path, b"not decoded in this view-model test").expect("write media");
    let asset = test_video_asset(media_path.clone());
    let asset_id = asset.id;
    let item = asset_grid_item_from_asset(asset.clone(), None, false, None);

    assert_eq!(badge_labels(&item), ["视频"]);
    assert_eq!(item.context_menu_items.len(), 5);
    assert_eq!(item.context_menu_items[0].label, "解释素材...");
    assert_eq!(item.context_menu_items[1].label, "在文件管理器中显示");
    assert_eq!(item.context_menu_items[2].label, "启用代理模式");
    assert!(item.context_menu_items[3].is_separator());
    let Action::Custom { namespace, name, payload } =
        item.context_menu_items[2].action().expect("proxy mode action")
    else {
        panic!("expected proxy mode custom action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_SET_PROXY_MODE);
    let payload: AssetsSetProxyModePayload =
        serde_json::from_value(payload.clone()).expect("proxy payload");
    assert_eq!(payload.asset_id, asset_id);
    assert!(payload.enabled);

    let proxied = asset_grid_item_from_asset(asset, None, true, None);
    assert_eq!(badge_labels(&proxied), ["视频", "代理"]);
    assert_eq!(proxied.badges[1].tone, AssetGridBadgeTone::Success);
    assert_eq!(proxied.context_menu_items[2].label, "关闭代理模式");
    let Action::Custom { payload, .. } =
        proxied.context_menu_items[2].action().expect("proxy mode action")
    else {
        panic!("expected proxy mode custom action");
    };
    let payload: AssetsSetProxyModePayload =
        serde_json::from_value(payload.clone()).expect("proxy payload");
    assert!(!payload.enabled);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn assets_panel_folder_card_context_menu_dispatches_delete_folder() {
    let root = unique_temp_dir("asset-panel-delete-folder-menu");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let folder_id = library.create_folder("Rushes", None).expect("create folder");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));
    let model = AppUiPanelModels::from_app_state(&state).assets;
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 360.0, 240.0));
    let card = grid.card_rect_for_index(0).expect("folder card");
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        grid.event(
            &UiEvent::MouseDown {
                position: card.center(),
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        grid.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        grid.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        grid.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected folder delete action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REMOVE_ENTRIES);
    let payload: AssetsDeleteSelectionPayload =
        serde_json::from_value(payload.clone()).expect("delete folder payload");
    assert!(payload.asset_ids.is_empty());
    assert_eq!(payload.folder_ids, vec![folder_id]);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn assets_panel_multi_selection_context_menu_dispatches_delete_selection() {
    let root = unique_temp_dir("asset-panel-delete-selection-menu");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let folder_id = library.create_folder("Rushes", None).expect("create folder");
    let asset_id = library.create_solid_color_asset(Some("Temp Plate")).expect("create asset");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));
    let model = AppUiPanelModels::from_app_state(&state).assets;
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 520.0, 260.0));
    let first = grid.card_rect_for_index(0).expect("first card").center();
    let second = grid.card_rect_for_index(1).expect("second card").center();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    let _ = grid.event(
        &UiEvent::MouseDown {
            position: first,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    let _ = grid.event(
        &UiEvent::MouseDown {
            position: second,
            button: MouseButton::Left,
            modifiers: Modifiers::ctrl(),
        },
        &mut ctx,
    );
    assert_eq!(
        grid.event(
            &UiEvent::MouseDown {
                position: second,
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        grid.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected delete selection action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REMOVE_ENTRIES);
    let payload: AssetsDeleteSelectionPayload =
        serde_json::from_value(payload.clone()).expect("delete selection payload");
    assert_eq!(payload.folder_ids, vec![folder_id]);
    assert_eq!(payload.asset_ids, vec![asset_id]);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn assets_panel_delete_key_dispatches_delete_selection() {
    let root = unique_temp_dir("asset-panel-delete-selection-key");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let folder_id = library.create_folder("Rushes", None).expect("create folder");
    let asset_id = library.create_solid_color_asset(Some("Temp Plate")).expect("create asset");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));
    let model = AppUiPanelModels::from_app_state(&state).assets;
    let mut grid = asset_grid(&model);
    grid.layout(Rect::new(0.0, 0.0, 520.0, 260.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    let _ = grid.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
    assert_eq!(
        grid.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        grid.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let actions = actions.borrow();
    assert_eq!(actions.len(), 1);
    let Action::Custom { namespace, name, payload } = &actions[0] else {
        panic!("expected delete selection action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REMOVE_ENTRIES);
    let payload: AssetsDeleteSelectionPayload =
        serde_json::from_value(payload.clone()).expect("delete selection payload");
    assert_eq!(payload.folder_ids, vec![folder_id]);
    assert_eq!(payload.asset_ids, vec![asset_id]);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn demo_timeline_model_carries_stable_clip_identity() {
    let model = demo_timeline_model();
    let identity = model
        .clip_identity(
            TimelineClipRef { track_index: 1, clip_index: 1 },
            TimelineClipSelectionMode::Replace,
        )
        .expect("demo overlay clip identity");
    let movement = model
        .move_payload(TimelineClipMove {
            clip_ref: TimelineClipRef { track_index: 1, clip_index: 1 },
            old_start_frame: 112,
            new_start_frame: 120,
            new_track_index: 2,
        })
        .expect("demo move payload");

    assert_eq!(identity.mode, TimelineClipSelectionModePayload::Replace);
    assert_eq!(movement.clip_id, identity.clip_id);
    assert_eq!(movement.position.frame, 120);
    let frame_rate = model.timeline_display.frame_rate();
    assert_eq!(
        movement.position.time_base,
        Rational::new(frame_rate.den, frame_rate.num)
    );
    assert_eq!(movement.target_track_id, model.track_refs[2].track_id);
}

#[test]
fn timeline_model_rejects_cross_media_clip_moves() {
    let model = demo_timeline_model();
    let audio_track_index = model
        .track_refs
        .iter()
        .position(|track| !track.is_video_track)
        .expect("demo audio track");

    assert!(model
        .move_payload(TimelineClipMove {
            clip_ref: TimelineClipRef { track_index: 1, clip_index: 1 },
            old_start_frame: 112,
            new_start_frame: 120,
            new_track_index: audio_track_index,
        })
        .is_none());
}

#[test]
fn timeline_model_rejects_stale_clip_refs() {
    let model = demo_timeline_model();
    let stale_ref = TimelineClipRef { track_index: usize::MAX, clip_index: 0 };

    assert!(model.clip_identity(stale_ref, TimelineClipSelectionMode::Replace).is_none());
    assert!(model
        .move_payload(TimelineClipMove {
            clip_ref: stale_ref,
            old_start_frame: 0,
            new_start_frame: 12,
            new_track_index: 0,
        })
        .is_none());
    assert!(model
        .trim_payload(TimelineClipTrim {
            clip_ref: stale_ref,
            edge: TimelineTrimEdge::In,
            old_start_frame: 0,
            old_duration_frames: 24,
            new_start_frame: 4,
            new_duration_frames: 20,
        })
        .is_none());
}

#[test]
fn timeline_model_keeps_clip_identity_paired_when_one_projection_fails() {
    let mut sequence = Sequence::new("projection pairing");
    let unprojectable = Clip::new(
        AssetId::new(),
        TimelineTime::new(i64::MIN, 1).expect("canonical extreme time"),
        TimelineTime::ONE,
    )
    .expect("structurally valid Clip");
    let expected =
        Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).expect("visible Clip");
    let expected_id = expected.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0]
        .add_clip(unprojectable)
        .expect("add unprojectable Clip");
    sequence.video_tracks[0].add_clip(expected).expect("add visible Clip");

    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);
    let track_index = model
        .track_refs
        .iter()
        .position(|track| track.track_id == track_id)
        .expect("projected track");

    assert_eq!(model.tracks[track_index].clips.len(), 1);
    assert_eq!(
        model
            .clip_identity(
                TimelineClipRef { track_index, clip_index: 0 },
                TimelineClipSelectionMode::Replace,
            )
            .expect("visible identity")
            .clip_id,
        expected_id
    );
}

#[test]
fn timeline_model_maps_sequence_tracks_clips_and_selection() {
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    sequence.playhead = tt(42, tb);
    sequence.mark_in(tt(12, tb));
    sequence.mark_out(tt(64, tb));

    let mut video = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    video.label = Some("Interview".to_string());
    let video_id = video.id;
    let video_track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(video).expect("add video clip");

    let mut audio = Clip::new(AssetId::new(), tt(12, tb), tt(48, tb)).expect("valid clip");
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
    let selected_track_id = sequence.audio_tracks[0].id;
    let model = TimelinePanelModel::from_sequence(&sequence, &[selected], &[selected_track_id]);

    assert_eq!(model.playhead_frame, 42);
    assert_eq!(model.in_point_frame, 12);
    assert_eq!(model.out_point_frame, Some(64));
    assert_eq!(
        model.tracks.len(),
        sequence.video_tracks.len() + sequence.audio_tracks.len()
    );
    assert_eq!(model.tracks[0].label, "V3");
    let selected_video = sequence.video_tracks.len() - 1;
    assert_eq!(model.tracks[selected_video].label, "V1");
    assert_eq!(
        model.tracks[selected_video].kind,
        mondrian_ui_widgets::TimelineTrackKind::Video
    );
    assert_eq!(model.tracks[selected_video].clips[0].label, "Interview");
    assert_eq!(model.tracks[selected_video].clips[0].start_frame, 10);
    assert_eq!(model.tracks[selected_video].clips[0].duration_frames, 20);
    assert!(model.tracks[selected_video].clips[0].selected);
    assert!(model.tracks[selected_video].clips[0].select_action.is_none());
    assert!(!model.tracks[selected_video].selected);
    let first_audio = sequence.video_tracks.len();
    assert_eq!(
        model.track_identity(TimelineTrackRef { track_index: first_audio }),
        Some(AppTimelineTrackRef { track_id: selected_track_id, is_video_track: false })
    );

    assert_eq!(
        model.tracks[first_audio].kind,
        mondrian_ui_widgets::TimelineTrackKind::Audio
    );
    assert!(model.tracks[first_audio].selected);
    assert!(model.tracks[first_audio].muted);
    assert!(model.tracks[first_audio].locked);
    assert!(model.tracks[first_audio].clips[0].disabled);
}

#[test]
fn timeline_model_maps_transition_identity_geometry_and_resize_payload() {
    let mut sequence = Sequence::new("transition edit");
    let tb = sequence.time_base();
    let left = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(255, 0, 0, 255),
        tt(0, tb),
        tt(10, tb),
    )
    .expect("left Clip");
    let right = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(0, 0, 255, 255),
        tt(10, tb),
        tt(10, tb),
    )
    .expect("right Clip");
    let (left_id, right_id) = (left.id, right.id);
    sequence.video_tracks[0].add_clip(left).expect("left placement");
    sequence.video_tracks[0].add_clip(right).expect("right placement");

    let model_without_transition = TimelinePanelModel::from_sequence(&sequence, &[], &[]);
    let display_track_index = sequence.video_tracks.len() - 1;
    let create_payload = model_without_transition
        .cut_transition_payload(TimelineCutRef {
            track_index: display_track_index,
            left_clip_index: 0,
            right_clip_index: 1,
        })
        .expect("create payload");
    assert_eq!(
        create_payload,
        VideoTransitionCreateCrossDissolvePayload {
            left_clip_id: left_id,
            right_clip_id: right_id,
            handle_policy: VideoTransitionHandlePolicy::Reject,
        }
    );
    let Action::Custom { namespace, name, .. } =
        video_transition_create_cross_dissolve_action(create_payload)
    else {
        panic!("expected create action");
    };
    assert_eq!(namespace, VIDEO_TRANSITION_NAMESPACE);
    assert_eq!(name, VIDEO_TRANSITION_CREATE_CROSS_DISSOLVE);

    let transition = mondrian_timeline::VideoTransition::cross_dissolve(
        left_id,
        right_id,
        mondrian_core::TimelineTimeRange::new(tt(8, tb), tt(4, tb)).expect("Transition range"),
    );
    let transition_id = transition.id;
    sequence.video_transitions.push(transition);
    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence));
    state.select_video_transition_by_id(transition_id).expect("select Transition");

    let model = TimelinePanelModel::from_app_state(&state);
    let transition_ref = TimelineTransitionRef {
        track_index: display_track_index,
        transition_index: 0,
    };
    let view = &model.tracks[display_track_index].transitions[0];
    assert_eq!(view.start_frame, 8);
    assert_eq!(view.duration_frames, 4);
    assert_eq!(view.cut_frame, 10);
    assert!(view.selected);
    assert!(view.handle_issue.is_none());
    assert_eq!(
        model.transition_identity(transition_ref),
        Some(VideoTransitionTargetPayload { transition_id })
    );
    let Action::Custom { namespace, name, .. } = video_transition_select_action(
        model.transition_identity(transition_ref).expect("selection payload"),
    ) else {
        panic!("expected selection action");
    };
    assert_eq!(namespace, VIDEO_TRANSITION_NAMESPACE);
    assert_eq!(name, VIDEO_TRANSITION_SELECT);
    assert_eq!(
        model.transition_resize_payload(TimelineTransitionResize {
            transition_ref,
            edge: mondrian_ui_widgets::TimelineTransitionEdge::In,
            old_start_frame: 8,
            old_duration_frames: 4,
            new_start_frame: 7,
            new_duration_frames: 6,
        }),
        Some(VideoTransitionSetRangePayload {
            transition_id,
            requested_range: TimelineTimeRange::new(tt(7, tb), tt(6, tb))
                .expect("exact resized range"),
            handle_policy: VideoTransitionHandlePolicy::Reject,
        })
    );
    let resize_payload = model
        .transition_resize_payload(TimelineTransitionResize {
            transition_ref,
            edge: mondrian_ui_widgets::TimelineTransitionEdge::Out,
            old_start_frame: 8,
            old_duration_frames: 4,
            new_start_frame: 8,
            new_duration_frames: 5,
        })
        .expect("resize payload");
    let Action::Custom { namespace, name, .. } = video_transition_set_range_action(resize_payload)
    else {
        panic!("expected resize action");
    };
    assert_eq!(namespace, VIDEO_TRANSITION_NAMESPACE);
    assert_eq!(name, VIDEO_TRANSITION_SET_RANGE);
}

#[test]
fn timeline_model_maps_track_control_payloads_to_stable_track_ids() {
    let mut sequence = Sequence::new("edit");
    sequence.video_tracks[0].is_visible = false;
    sequence.audio_tracks[0].is_muted = true;

    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);
    let video_ref = TimelineTrackRef { track_index: sequence.video_tracks.len() - 1 };
    let first_audio_ref = TimelineTrackRef { track_index: sequence.video_tracks.len() };

    let visibility = model
        .track_control_payload(
            TimelineTrackControl::Visibility,
            video_ref,
            &model.tracks[video_ref.track_index],
        )
        .expect("visibility payload");
    assert_eq!(visibility.track_id, sequence.video_tracks[0].id);
    assert_eq!(visibility.control, TrackAuthorControl::Visibility);
    assert!(visibility.enabled);

    let mute = model
        .track_control_payload(
            TimelineTrackControl::Mute,
            first_audio_ref,
            &model.tracks[first_audio_ref.track_index],
        )
        .expect("mute payload");
    assert_eq!(mute.track_id, sequence.audio_tracks[0].id);
    assert_eq!(mute.control, TrackAuthorControl::Mute);
    assert!(!mute.enabled);
}

#[test]
fn timeline_model_maps_track_move_to_stable_author_order_anchor() {
    let mut sequence = Sequence::new("edit");
    sequence.add_video_track();
    sequence.add_audio_track();
    let video_count = sequence.video_tracks.len();
    let moved_track_id = sequence.video_tracks[1].id;
    let anchor_track_id = sequence.video_tracks[video_count - 1].id;
    let first_audio_ref = TimelineTrackRef { track_index: sequence.video_tracks.len() };
    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);
    let moved_display_index = video_count - 1 - 1;

    let payload = model
        .track_move_payload(TimelineTrackMove {
            track_ref: TimelineTrackRef { track_index: moved_display_index },
            old_track_index: moved_display_index,
            new_track_index: 0,
        })
        .expect("same-kind video move payload");

    assert_eq!(payload.track_id, moved_track_id);
    assert_eq!(
        payload.placement,
        TrackRelativePlacement::After(anchor_track_id)
    );
    assert!(model
        .track_move_payload(TimelineTrackMove {
            track_ref: TimelineTrackRef { track_index: 0 },
            old_track_index: 0,
            new_track_index: first_audio_ref.track_index,
        })
        .is_none());
}

#[test]
fn timeline_model_maps_asset_drop_payload_to_stable_track_id() {
    let mut sequence = Sequence::new("edit");
    sequence.add_audio_track();
    let first_audio_ref = TimelineTrackRef { track_index: sequence.video_tracks.len() };
    let target_track_id = sequence.audio_tracks[0].id;
    let asset_id = AssetId::new();
    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);

    let payload = model
        .asset_drop_payload(TimelineAssetDrop { asset_id, track_ref: first_audio_ref, frame: -12 })
        .expect("asset drop payload");

    assert_eq!(payload.asset_id, asset_id);
    assert_eq!(payload.target_track_id, target_track_id);
    assert_eq!(payload.position.frame, 0);
    assert_eq!(payload.position.time_base, sequence.time_base());
    assert!(model
        .asset_drop_payload(TimelineAssetDrop {
            asset_id,
            track_ref: TimelineTrackRef { track_index: usize::MAX },
            frame: 24,
        })
        .is_none());
}

#[test]
fn timeline_panel_context_menu_add_track_emits_typed_timeline_actions() {
    let model = demo_timeline_model();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 180.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    let result = panel.event(
        &UiEvent::MouseDown {
            position: Point::new(500.0, 42.0),
            button: MouseButton::Right,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    assert!(panel.overlay_hit_test(Point::new(900.0, 900.0)));
    actions.borrow_mut().clear();
    assert_eq!(
        panel.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected custom add-track action");
    };
    assert_eq!(namespace, TRACK_NAMESPACE);
    assert_eq!(name, TRACK_ADD);
    let payload: TrackAddPayload =
        serde_json::from_value(payload.clone()).expect("add track payload");
    assert_eq!(payload.kind, TrackAddKind::Video);
}

#[test]
fn timeline_panel_track_header_drag_emits_typed_move_track_action() {
    let model = demo_timeline_model();
    let moved = model.track_identity(TimelineTrackRef { track_index: 1 }).expect("track");
    let anchor = model.track_identity(TimelineTrackRef { track_index: 0 }).expect("anchor");
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 220.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    panel.event(
        &UiEvent::MouseDown {
            position: timeline_content_point(12.0, 105.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    panel.event(
        &UiEvent::MouseMove {
            position: timeline_content_point(12.0, 55.0),
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    panel.event(
        &UiEvent::MouseUp {
            position: timeline_content_point(12.0, 55.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );

    let recorded = actions.borrow();
    let move_action = recorded
        .iter()
        .find(|action| {
            matches!(
                action,
                Action::Custom { namespace, name, .. }
                    if namespace == TRACK_NAMESPACE && name == TRACK_MOVE
            )
        })
        .expect("move track action");
    let Action::Custom { payload, .. } = move_action else {
        panic!("expected custom move-track action");
    };
    let payload: TrackMovePayload =
        serde_json::from_value(payload.clone()).expect("move track payload");
    assert_eq!(payload.track_id, moved.track_id);
    assert_eq!(
        payload.placement,
        TrackRelativePlacement::After(anchor.track_id)
    );
}

#[test]
fn timeline_panel_disabled_clip_still_selects_for_inspection() {
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(24, tb)).expect("valid clip");
    clip.is_disabled = true;
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add clip");
    let display_track_index = video_display_index(&sequence, 0);
    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);
    assert!(model.tracks[display_track_index].clips[0].disabled);

    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 180.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        panel.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(140.0, 42.0 + display_track_index as f32 * 42.0,),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected custom timeline select action");
    };
    assert_eq!(namespace, TIMELINE_NAMESPACE);
    assert_eq!(name, TIMELINE_SELECT_CLIP);
    let payload: TimelineSelectClipPayload =
        serde_json::from_value(payload.clone()).expect("select payload");
    assert_eq!(payload.clip_id, clip_id);
    assert_eq!(payload.mode, TimelineClipSelectionModePayload::Replace);
}

#[test]
fn timeline_panel_asset_drop_emits_typed_drop_asset_action() {
    let model = demo_timeline_model();
    let target = model.track_identity(TimelineTrackRef { track_index: 0 }).expect("track");
    let asset_id = AssetId::new();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 220.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    panel.event(
        &UiEvent::Drop {
            payload: DragPayload::Asset(asset_id),
            position: timeline_content_point(144.0, 55.0),
        },
        &mut ctx,
    );

    let recorded = actions.borrow();
    let payload = recorded
        .iter()
        .find_map(
            |action| match ProductAction::decode_external(action).ok().flatten()? {
                ProductAction::Timeline(TimelineProductAction::PlaceAsset(payload)) => {
                    Some(payload)
                }
                _ => None,
            },
        )
        .expect("drop asset action");
    assert_eq!(payload.asset_id, asset_id);
    assert_eq!(payload.target_track_id, target.track_id);
    assert_eq!(
        payload.position,
        FramePosition::new(10, Rational::new(1, 25))
    );
}

#[test]
fn router_drags_asset_grid_card_to_timeline_drop_action() {
    let model = demo_timeline_model();
    let target = model.track_identity(TimelineTrackRef { track_index: 0 }).expect("track");
    let asset_id = AssetId::new();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let assets = AssetGrid::new(
        "Assets",
        vec![
            AssetGridItem::new("clip-a", "Clip A", current_theme().colors.media_video)
                .with_drag_payload(DragPayload::Asset(asset_id)),
        ],
    );
    let timeline = timeline_panel(&model);
    let mut root = AssetTimelineDragHarness::new(assets, timeline);
    root.layout(Rect::new(0.0, 0.0, 840.0, 240.0));
    let asset_card = root.assets.card_rect_for_index(0).expect("asset card");
    let drag_start = asset_card.center();
    let mut router = EventRouter::new(root.id());

    {
        let mut tree = WidgetTreeView::new(&mut root);
        router.route(
            UiEvent::MouseDown {
                position: drag_start,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            &mut tree,
            &dispatch,
        );
    }
    {
        let mut tree = WidgetTreeView::new(&mut root);
        router.route(
            UiEvent::MouseMove {
                position: Point::new(drag_start.x + 12.0, drag_start.y),
                modifiers: Modifiers::default(),
            },
            &mut tree,
            &dispatch,
        );
    }

    assert_eq!(
        router.active_drag_payload(),
        Some(&DragPayload::Asset(asset_id))
    );

    {
        let mut tree = WidgetTreeView::new(&mut root);
        router.route(
            UiEvent::MouseMove {
                position: timeline_content_point(444.0, 55.0),
                modifiers: Modifiers::default(),
            },
            &mut tree,
            &dispatch,
        );
    }
    {
        let mut tree = WidgetTreeView::new(&mut root);
        router.route(
            UiEvent::MouseUp {
                position: timeline_content_point(444.0, 55.0),
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
            &mut tree,
            &dispatch,
        );
    }

    assert!(router.active_drag_payload().is_none());
    let recorded = actions.borrow();
    let payload = recorded
        .iter()
        .find_map(
            |action| match ProductAction::decode_external(action).ok().flatten()? {
                ProductAction::Timeline(TimelineProductAction::PlaceAsset(payload)) => {
                    Some(payload)
                }
                _ => None,
            },
        )
        .expect("drop asset action");
    assert_eq!(payload.asset_id, asset_id);
    assert_eq!(payload.target_track_id, target.track_id);
    assert_eq!(
        payload.position,
        FramePosition::new(10, Rational::new(1, 25))
    );
}

#[test]
fn timeline_panel_delete_key_emits_shared_delete_selection_action() {
    let model = demo_timeline_model();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 180.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    panel.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
    let result = panel.event(
        &UiEvent::KeyDown {
            key: KeyCode::Backspace,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    assert_eq!(actions.borrow().as_slice(), &[Action::DeleteSelection]);
}

#[test]
fn timeline_panel_shift_delete_key_emits_shared_ripple_delete_action() {
    let model = demo_timeline_model();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 180.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    panel.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
    let result = panel.event(
        &UiEvent::KeyDown {
            key: KeyCode::Delete,
            modifiers: Modifiers::shift(),
        },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    assert_eq!(
        actions.borrow().as_slice(),
        &[Action::RippleDeleteSelection]
    );
}

#[test]
fn timeline_panel_clipboard_keys_emit_shared_edit_actions() {
    let model = demo_timeline_model();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 180.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    panel.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
    for key in [KeyCode::X, KeyCode::C, KeyCode::V, KeyCode::D, KeyCode::K] {
        assert_eq!(
            panel.event(
                &UiEvent::KeyDown { key, modifiers: Modifiers::ctrl() },
                &mut ctx,
            ),
            EventResult::Handled
        );
    }

    assert_eq!(
        actions.borrow().as_slice(),
        &[
            Action::Cut,
            Action::Copy,
            Action::Paste,
            Action::Duplicate,
            Action::SplitClipAtPlayhead,
        ]
    );
}

#[test]
fn timeline_panel_ctrl_b_emits_shared_split_action() {
    let model = demo_timeline_model();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 180.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    panel.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
    let result = panel.event(
        &UiEvent::KeyDown { key: KeyCode::B, modifiers: Modifiers::ctrl() },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    assert_eq!(actions.borrow().as_slice(), &[Action::SplitClipAtPlayhead]);
}

#[test]
fn timeline_panel_i_o_emit_shared_mark_actions() {
    let model = demo_timeline_model();
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 180.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    panel.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
    let in_result = panel.event(
        &UiEvent::KeyDown { key: KeyCode::I, modifiers: Modifiers::none() },
        &mut ctx,
    );
    let out_result = panel.event(
        &UiEvent::KeyDown { key: KeyCode::O, modifiers: Modifiers::none() },
        &mut ctx,
    );

    assert_eq!(in_result, EventResult::Handled);
    assert_eq!(out_result, EventResult::Handled);
    assert_eq!(
        actions.borrow().as_slice(),
        &[Action::MarkInAtPlayhead, Action::MarkOutAtPlayhead]
    );
}

#[test]
fn timeline_panel_dragged_in_marker_emits_typed_range_payload() {
    let mut model = demo_timeline_model();
    model.in_point_frame = 10;
    model.out_point_frame = Some(30);
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut panel = timeline_panel(&model);
    panel.layout(mondrian_ui_core::types::Rect::new(0.0, 0.0, 520.0, 180.0));

    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        panel.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(144.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        panel.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(184.0, 12.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        panel.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(184.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected timeline custom action, got {:?}", recorded[0]);
    };
    assert_eq!(namespace, TIMELINE_NAMESPACE);
    assert_eq!(name, TIMELINE_SET_IN_OUT_POINT);
    let payload: TimelineSetInOutPointPayload =
        serde_json::from_value(payload.clone()).expect("timeline in/out payload");
    assert_eq!(payload.point, TimelineInOutPointKind::In);
    assert_eq!(payload.position.frame, 20);
    assert_eq!(payload.position.time_base, Rational::new(1, 25));
}

#[test]
fn timeline_edit_command_mapping_emits_selection_trim_and_enable_actions() {
    let model = TimelinePanelModel::default();
    assert_eq!(
        timeline_edit_command_action(&model, TimelineEditCommand::CutSelection),
        Some(Action::Cut)
    );
    assert_eq!(
        timeline_edit_command_action(&model, TimelineEditCommand::CopySelection),
        Some(Action::Copy)
    );
    assert_eq!(
        timeline_edit_command_action(&model, TimelineEditCommand::PasteAtPlayhead),
        Some(Action::Paste)
    );
    assert_eq!(
        timeline_edit_command_action(&model, TimelineEditCommand::DuplicateSelection),
        Some(Action::Duplicate)
    );
    assert_eq!(
        timeline_edit_command_action(&model, TimelineEditCommand::TogglePlayback),
        Some(Action::TogglePlay)
    );
    let expected_copy = crate::app_ui::test_utils::expected_shortcut("Ctrl+C");
    assert_eq!(
        timeline_edit_command_shortcut_label(TimelineEditCommand::CopySelection).as_deref(),
        Some(expected_copy.as_str())
    );
    let expected_duplicate = crate::app_ui::test_utils::expected_shortcut("Ctrl+D");
    assert_eq!(
        timeline_edit_command_shortcut_label(TimelineEditCommand::DuplicateSelection).as_deref(),
        Some(expected_duplicate.as_str())
    );
    assert_eq!(
        timeline_edit_command_shortcut_label(TimelineEditCommand::TrimSelectionInToPlayhead),
        None
    );
    assert_eq!(
        timeline_edit_command_shortcut_label(TimelineEditCommand::TogglePlayback).as_deref(),
        Some("Space")
    );

    let trim_action =
        timeline_edit_command_action(&model, TimelineEditCommand::TrimSelectionInToPlayhead)
            .expect("selected trim action");
    assert_eq!(
        ProductAction::decode_external(&trim_action)
            .expect("valid product payload")
            .expect("recognized product action"),
        ProductAction::Timeline(TimelineProductAction::EditSelection(
            TimelineSelectionEdit::TrimClipsToPlayhead { edge: TimelineTrimPayloadEdge::In }
        ))
    );

    let roll_action =
        timeline_edit_command_action(&model, TimelineEditCommand::RollSelectedCutToPlayhead)
            .expect("roll cut action");
    assert_eq!(
        ProductAction::decode_external(&roll_action)
            .expect("valid product payload")
            .expect("recognized product action"),
        ProductAction::Timeline(TimelineProductAction::EditSelection(
            TimelineSelectionEdit::RollCutToPlayhead
        ))
    );

    let disable_action =
        timeline_edit_command_action(&model, TimelineEditCommand::DisableSelection)
            .expect("selected enabled action");
    assert_eq!(
        ProductAction::decode_external(&disable_action)
            .expect("valid product payload")
            .expect("recognized product action"),
        ProductAction::Timeline(TimelineProductAction::EditSelection(
            TimelineSelectionEdit::SetClipsEnabled { enabled: false }
        ))
    );

    let clear_action = timeline_edit_command_action(&model, TimelineEditCommand::ClearInOutPoints);
    let Some(Action::Custom { namespace, name, payload }) = clear_action else {
        panic!("expected clear in/out custom action");
    };
    assert_eq!(namespace, TIMELINE_NAMESPACE);
    assert_eq!(name, TIMELINE_CLEAR_IN_OUT_POINTS);
    assert!(payload.is_null());
}

#[test]
fn timeline_open_nested_command_maps_clip_ref_to_nested_sequence_action() {
    let nested_id = SequenceId::new();
    let mut sequence = Sequence::new("parent");
    let tb = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(nested_id, tt(0, tb), tt(24, tb), Some("Nested".to_owned()))
                .expect("valid clip"),
        )
        .expect("add nested clip");
    let display_track_index = video_display_index(&sequence, 0);
    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);

    let action = timeline_edit_command_action(
        &model,
        TimelineEditCommand::OpenNestedSequence(TimelineClipRef {
            track_index: display_track_index,
            clip_index: 0,
        }),
    );

    let Some(action) = action else {
        panic!("expected open nested action");
    };
    let Some(ProductAction::Sequence(SequenceProductAction::OpenNested(payload))) =
        ProductAction::decode_external(&action).expect("decode open nested action")
    else {
        panic!("expected typed open nested action");
    };
    assert_eq!(payload.sequence_id, nested_id);

    assert_eq!(
        timeline_edit_command_action(
            &model,
            TimelineEditCommand::OpenNestedSequence(TimelineClipRef {
                track_index: usize::MAX,
                clip_index: usize::MAX,
            }),
        ),
        None
    );
}

#[test]
fn app_state_models_resolve_stale_selected_clip_metadata_by_clip_id() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    sequence.add_video_track();
    let tb = sequence.time_base();
    let actual_track_id = sequence.video_tracks[0].id;
    let stale_track_id = sequence.video_tracks[1].id;
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(24, tb)).expect("valid clip");
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add clip");
    let display_track_index = video_display_index(&sequence, 0);
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips = vec![SelectedClipRef {
        track_id: stale_track_id,
        is_video_track: false,
        clip_id,
    }];

    let models = AppUiPanelModels::from_app_state(&state);

    assert!(models.timeline.tracks[display_track_index].clips[0].selected);
    assert_eq!(
        models.inspector.selected_clip,
        Some(SelectedClipRef {
            track_id: actual_track_id,
            is_video_track: true,
            clip_id
        })
    );
    assert!(
        models.effects.items.iter().any(|item| item.activate_action.is_some()),
        "video clip selection should keep Effects rows actionable"
    );
}

#[test]
fn app_state_models_map_sequence_selection_and_basic_inspector_values() {
    let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let color = Color::from_rgba8(20, 90, 160, 180);
    let mut clip =
        Clip::new_solid_color(AssetId::new(), color, tt(4, tb), tt(18, tb)).expect("valid clip");
    clip.is_disabled = true;
    clip.transform.set_position(glam::Vec2::new(192.0, 108.0));
    clip.transform.set_scale(glam::Vec2::new(1.25, 0.75));
    clip.transform.set_anchor_point(glam::Vec2::new(320.0, 180.0));
    clip.apply_property_mutation(PropertyMutation::SetStaticValue {
        path: Transform2D::ROTATION_PATH.to_string(),
        value: PropertyValue::Float(15.0),
    })
    .expect("set rotation");
    let mut effect = mondrian_effects::EffectNode::with_defaults(EffectType::GaussianBlur);
    effect.is_enabled = false;
    let effect_id = effect.id;
    clip.add_effect_node(effect);
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add solid clip");
    let display_track_index = video_display_index(&sequence, 0);
    state.test_set_sequence(Some(sequence));
    assert!(
        state.select_effect_by_id(clip_id, effect_id).is_some(),
        "seed selected effect"
    );
    state.seek(7).expect("seek");

    let models = AppUiPanelModels::from_app_state(&state);
    let colors = current_theme().colors.clone();

    assert_eq!(models.viewer.title, "edit");
    assert_eq!(models.viewer.position_label, "F7");
    assert_eq!(models.viewer.status_tone, ViewerStatusTone::Neutral);
    assert_eq!(models.viewer.empty_message, None);
    assert!(models.viewer.resolution_label.contains("1920x1080"));
    assert_eq!(models.viewer.zoom_label, "适合");
    assert_eq!(models.viewer.preview_quality_label, "1/2");
    assert_eq!(models.timeline.playhead_frame, 7);
    assert!(models.timeline.tracks[display_track_index].clips[0].selected);
    assert!(models.timeline.tracks[display_track_index].clips[0].disabled);
    assert!(models.inspector.is_editable);
    assert_eq!(models.inspector.edit_disabled_reason, None);
    assert!(!models.inspector.enabled);
    assert_eq!(models.inspector.opacity, 100.0);
    assert_eq!(
        models
            .inspector
            .opacity_curve
            .as_ref()
            .expect("opacity curve")
            .keys
            .iter()
            .map(|key| key.point)
            .collect::<Vec<_>>(),
        vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 1.0)]
    );

    state.play().expect("play");
    let playing_models = AppUiPanelModels::from_app_state(&state);
    assert_eq!(playing_models.viewer.status, "播放中");
    assert_eq!(playing_models.viewer.status_tone, ViewerStatusTone::Accent);
    assert!(playing_models.viewer.playing);
    assert_eq!(models.inspector.tint.to_rgba8(), color.to_rgba8());
    assert_eq!(models.inspector.position_x, 192.0);
    assert_eq!(models.inspector.position_y, 108.0);
    assert_eq!(models.inspector.scale_x_percent, 125.0);
    assert_eq!(models.inspector.scale_y_percent, 75.0);
    assert_eq!(models.inspector.anchor_x, 320.0);
    assert_eq!(models.inspector.anchor_y, 180.0);
    assert_eq!(models.inspector.rotation_degrees, 15.0);
    assert_eq!(models.inspector.in_frame, 4.0);
    assert_eq!(models.inspector.out_frame, 22.0);
    assert_eq!(models.inspector.max_frame, 22.0);
    assert_eq!(models.inspector.effects.len(), 1);
    assert_eq!(models.inspector.selected_effect_id, Some(effect_id));
    assert_eq!(models.inspector.effects[0].effect_id, effect_id);
    let blur_property = &models.inspector.effects[0].properties[0];
    assert_eq!(
        blur_property.schema.parameter_id.as_str(),
        "mondrian.effect.builtin.gaussian_blur.radius"
    );
    assert_eq!(blur_property.schema.schema_version, 1);
    assert_eq!(
        blur_property.schema.message_id,
        "mondrian.effect.builtin.gaussian_blur.radius.label"
    );
    assert!(blur_property.path.contains(&effect_id.to_string()));
    assert_eq!(
        models.inspector.effects[0].label,
        effect_display_name(&EffectType::GaussianBlur)
    );
    assert!(!models.inspector.effects[0].enabled);
    assert_eq!(
        models.node_graph.selected_clip,
        Some(SelectedClipRef { track_id, is_video_track: true, clip_id })
    );
    assert_eq!(models.node_graph.nodes.len(), 3);
    assert_eq!(models.node_graph.edges.len(), 2);
    assert_eq!(models.node_graph.nodes[0].id, "source");
    assert_eq!(models.node_graph.nodes[0].accent, Some(colors.node_source));
    assert_eq!(models.node_graph.nodes[1].id, format!("effect:{effect_id}"));
    assert_eq!(
        models.node_graph.nodes[1].accent,
        Some(colors.effect_filter)
    );
    assert_eq!(
        models.node_graph.selected_node_id,
        Some(format!("effect:{effect_id}"))
    );
    assert_eq!(
        models.node_graph.node_targets,
        vec![
            NodeGraphNodeTarget {
                node_id: "source".to_owned(),
                target: NodeGraphTarget::Clip,
            },
            NodeGraphNodeTarget {
                node_id: format!("effect:{effect_id}"),
                target: NodeGraphTarget::Effect(effect_id),
            },
            NodeGraphNodeTarget {
                node_id: "output".to_owned(),
                target: NodeGraphTarget::Output,
            },
        ]
    );
    assert_eq!(
        models.node_graph.nodes[1].title,
        effect_display_name(&EffectType::GaussianBlur)
    );
    assert!(models.node_graph.nodes[1].disabled);
    assert_eq!(models.node_graph.nodes[2].id, "output");
    assert_eq!(models.node_graph.nodes[2].accent, Some(colors.node_output));
    assert_eq!(
        models.node_graph.edges[0],
        NodeGraphEdge::new("source", format!("effect:{effect_id}"))
    );
    assert_eq!(
        models.node_graph.edges[1],
        NodeGraphEdge::new(format!("effect:{effect_id}"), "output")
    );
}

#[test]
fn crop_definition_projects_four_percent_parameters_into_inspector() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("Crop inspector");
    let tb = sequence.time_base();
    let mut clip = Clip::new_solid_color(AssetId::new(), Color::WHITE, tt(0, tb), tt(24, tb))
        .expect("valid solid Clip");
    let effect = mondrian_effects::EffectNode::with_defaults(EffectType::Crop);
    let effect_id = effect.id;
    clip.add_effect_node(effect);
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add Crop Clip");
    state.test_set_sequence(Some(sequence));
    assert!(state.select_effect_by_id(clip_id, effect_id).is_some());

    let models = AppUiPanelModels::from_app_state(&state);
    let crop = models
        .inspector
        .effects
        .iter()
        .find(|effect| effect.effect_id == effect_id)
        .expect("Crop inspector section");
    let parameter_ids = crop
        .properties
        .iter()
        .map(|property| property.schema.parameter_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(crop.label, effect_display_name(&EffectType::Crop));
    assert_eq!(
        parameter_ids,
        vec![
            "mondrian.effect.builtin.crop.left",
            "mondrian.effect.builtin.crop.top",
            "mondrian.effect.builtin.crop.right",
            "mondrian.effect.builtin.crop.bottom",
        ]
    );
    assert!(crop.properties.iter().all(|property| {
        property.hard_min == Some(0.0)
            && property.hard_max == Some(100.0)
            && property.step == Some(0.1)
            && property.is_animatable
    }));
}

#[test]
fn viewer_and_timeline_share_the_sequence_drop_frame_display_contract() {
    let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
    let mut state = AppState::new();
    let mut sequence = Sequence::new("drop-frame");
    sequence.settings.frame_rate = Rational::FPS_2997;
    sequence.settings.timeline_display =
        TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 107_892);
    state.test_set_sequence(Some(sequence));
    state.seek(1_800).expect("seek");

    let models = AppUiPanelModels::from_app_state(&state);

    assert_eq!(models.viewer.position_label, "01:01:00;02");
    assert_eq!(
        models
            .timeline
            .timeline_display
            .format_frame_offset(models.timeline.playhead_frame)
            .expect("Timeline display label"),
        models.viewer.position_label
    );
}

#[test]
fn app_state_models_attach_viewer_preview_frame_from_source() {
    struct TestPreview;

    impl ViewerPreviewSource for TestPreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Ready(ViewerFrameContent::Raster(
                ViewerFrameImage::new(
                    "test-preview",
                    320,
                    180,
                    mondrian_ui_core::RasterImageColorSpace::Srgb,
                    vec![128; 320 * 180 * 4],
                )
                .expect("preview frame"),
            ))
        }
    }

    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));

    let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&TestPreview),
    );

    let ViewerFrameContent::Raster(frame) = models.viewer.frame_content.expect("preview frame")
    else {
        panic!("expected raster preview frame");
    };
    assert_eq!(frame.key, "test-preview");
    assert_eq!(frame.width, 320);
    assert_eq!(frame.height, 180);
    assert_eq!(models.viewer.preview_quality_label, "1/2");
    assert_eq!(models.viewer.preview_resolution_scale, 0.5);
}

#[test]
fn app_state_models_surface_viewer_preview_loading_state() {
    struct LoadingPreview;

    impl ViewerPreviewSource for LoadingPreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Loading
        }
    }

    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));

    let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&LoadingPreview),
    );

    assert_eq!(models.viewer.status, "预览准备中");
    assert_eq!(models.viewer.status_tone, ViewerStatusTone::Warning);
    assert!(models.viewer.preview_waiting);
    assert!(models.viewer.frame_content.is_none());
    assert_eq!(models.viewer.empty_message.as_deref(), Some("预览准备中"));
}

#[test]
fn app_state_models_project_typed_preview_blocker_without_reclassification() {
    struct BlockedPreview;

    impl ViewerPreviewSource for BlockedPreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Unavailable(PreviewUnavailability::blocked(
                PreviewOutputStage::MediaResolution,
                "源素材离线",
            ))
        }
    }

    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));
    let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&BlockedPreview),
    );

    assert_eq!(models.viewer.status, "预览被阻止");
    assert_eq!(models.viewer.status_tone, ViewerStatusTone::Warning);
    assert_eq!(models.viewer.empty_message.as_deref(), Some("源素材离线"));
    let reason = models.viewer.preview_unavailability.expect("typed blocker");
    assert_eq!(
        reason.disposition(),
        PreviewUnavailabilityDisposition::Blocked
    );
    assert_eq!(reason.stage(), PreviewOutputStage::MediaResolution);
}

#[test]
fn app_state_models_treat_timeline_no_content_as_a_clean_canvas() {
    struct NoContentPreview;

    impl ViewerPreviewSource for NoContentPreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Unavailable(PreviewUnavailability::no_content(
                PreviewOutputStage::TimelineEvaluation,
                "current Timeline position contains no visible elements",
            ))
        }
    }

    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));
    let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&NoContentPreview),
    );

    assert_eq!(models.viewer.status, "就绪");
    assert_eq!(models.viewer.status_tone, ViewerStatusTone::Neutral);
    assert!(models.viewer.frame_content.is_none());
    assert!(
        models.viewer.empty_message.is_none(),
        "expected empty Timeline detail to remain diagnostic-only"
    );
}

#[test]
fn app_state_models_project_an_exact_transparent_canvas_as_ready() {
    struct TransparentPreview;

    impl ViewerPreviewSource for TransparentPreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Transparent
        }
    }

    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));
    let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&TransparentPreview),
    );

    assert!(models.viewer.transparent_canvas);
    assert!(models.viewer.frame_content.is_none());
    assert!(models.viewer.empty_message.is_none());
    assert_eq!(
        models.viewer.preview_state_kind(),
        crate::app_ui::playback_feedback::ViewerPreviewStateKind::Ready
    );
}

#[test]
fn app_state_models_surface_viewer_color_rejection() {
    struct RejectedPreview;

    impl ViewerPreviewSource for RejectedPreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Unavailable(PreviewUnavailability::blocked(
                PreviewOutputStage::InputColor,
                "test color rejection",
            ))
        }

        fn viewer_color_rejection(&self) -> Option<ViewerPreviewColorRejectionModel> {
            Some(ViewerPreviewColorRejectionModel {
                asset_id: AssetId::new(),
                path: PathBuf::from("E:/media/missing-color-tags.mov"),
                missing_metadata_policy: MissingColorMetadataPolicy::RejectMedia,
                source: InputColorResolutionSource::MissingPolicyRejectMedia,
                override_color_space: None,
                executable_color_space: None,
                working_color_space: WorkingColorSpace::LinearRec2020,
                diagnostic_summary: "source=MissingMetadata,warnings=missing_cicp".to_string(),
                diagnostic_issue_summary: VideoColorDiagnosticIssueSummary {
                    executable_color_space: None,
                    source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                    method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                    confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                    has_raw_cicp_metadata: false,
                    metadata_hint_count: 0,
                    evidence_count: 0,
                    warning_count: 1,
                    multiple_metadata_hints: 0,
                    ignored_metadata_hints: 0,
                    metadata_hint_overrides_cicp_tags: 0,
                    lower_priority_metadata_hints: 0,
                    ignored_lower_priority_metadata_hints: 0,
                    partial_cicp_tags: 0,
                    missing_cicp_tags: 1,
                    unsupported_cicp_tags: 0,
                    decoder_unavailable: 0,
                    hdr_side_data_count: 0,
                    has_mastering_display_metadata: false,
                    has_content_light_metadata: false,
                    has_dynamic_hdr10_plus: false,
                    has_dolby_vision_config: false,
                    has_icc_profile: false,
                    icc_cicp_mismatch: 0,
                    icc_profile_unmapped: 0,
                    has_user_visible_warnings: true,
                },
            })
        }
    }

    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));

    let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&RejectedPreview),
    );

    assert_eq!(models.viewer.status, "色彩解释被拒绝");
    assert_eq!(models.viewer.status_tone, ViewerStatusTone::Warning);
    assert!(models.viewer.frame_content.is_none());
    assert!(models.viewer.color_rejection.is_some());
    let empty = models.viewer.empty_message.as_deref().expect("empty message");
    assert!(empty.contains("色彩解释被拒绝"));
    assert!(empty.contains("missing-color-tags.mov"));
    assert!(empty.contains("MissingPolicyRejectMedia"));
    assert!(empty.contains("检测：MissingMetadata / None / warnings 1"));
    assert!(empty.contains("问题：missing-cicp 1"));
    assert!(empty.contains("missing_cicp"));
}

#[test]
fn app_state_models_keep_stale_viewer_preview_frame_visible() {
    struct StalePreview;

    impl ViewerPreviewSource for StalePreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Stale(ViewerFrameContent::Raster(
                ViewerFrameImage::new(
                    "stale-preview",
                    320,
                    180,
                    mondrian_ui_core::RasterImageColorSpace::Srgb,
                    vec![96; 320 * 180 * 4],
                )
                .expect("stale preview frame"),
            ))
        }
    }

    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));

    let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&StalePreview),
    );

    let ViewerFrameContent::Raster(frame) =
        models.viewer.frame_content.expect("stale frame remains visible")
    else {
        panic!("expected raster stale preview frame");
    };
    assert_eq!(models.viewer.status, "预览准备中");
    assert_eq!(models.viewer.status_tone, ViewerStatusTone::Warning);
    assert!(models.viewer.preview_waiting);
    assert_eq!(frame.key, "stale-preview");
    assert_eq!(models.viewer.empty_message, None);
}

#[test]
fn scopes_model_exposes_gpu_registry_keys_only_for_external_viewer_frames() {
    struct ExternalPreview;

    impl ViewerPreviewSource for ExternalPreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(
                ViewerExternalTextureFrame::new("viewer-current", 320, 180)
                    .expect("external frame"),
            ))
        }
    }

    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("edit")));
    let external = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&ExternalPreview),
    );
    let textures = external.scopes.textures.expect("GPU scope texture keys");
    assert_eq!(
        textures.waveform,
        crate::app_ui::scopes::WAVEFORM_TEXTURE_KEY
    );

    let cpu = AppUiPanelModels::from_app_state(&state);
    assert!(cpu.scopes.textures.is_none());
}

#[test]
fn app_state_models_do_not_request_viewer_preview_without_sequence() {
    struct UnexpectedPreview;

    impl ViewerPreviewSource for UnexpectedPreview {
        fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
            ViewerPreviewState::Ready(ViewerFrameContent::Raster(
                ViewerFrameImage::new(
                    "unexpected-preview",
                    320,
                    180,
                    mondrian_ui_core::RasterImageColorSpace::Srgb,
                    vec![128; 320 * 180 * 4],
                )
                .expect("preview frame"),
            ))
        }
    }

    let state = AppState::new();

    let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
        &state,
        None,
        None,
        Some(&UnexpectedPreview),
    );

    assert!(!models.viewer.enabled);
    assert!(models.viewer.frame_content.is_none());
    assert_eq!(models.viewer.empty_message.as_deref(), Some("未载入序列"));
}

#[test]
fn app_state_models_label_full_resolution_viewer_preview_scale() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    sequence.settings.preview.resolution_scale = 1.0;
    state.test_set_sequence(Some(sequence));

    let models = AppUiPanelModels::from_app_state(&state);

    assert_eq!(models.viewer.preview_quality_label, "1/1");
    assert_eq!(models.viewer.preview_resolution_scale, 1.0);
}

#[test]
fn app_state_models_clamp_viewer_preview_scale_label() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    sequence.settings.preview.resolution_scale = 0.0;
    state.test_set_sequence(Some(sequence));

    let models = AppUiPanelModels::from_app_state(&state);

    assert_eq!(models.viewer.preview_quality_label, "1/8");
    assert_eq!(models.viewer.preview_resolution_scale, 0.125);
}

#[test]
fn app_state_models_read_opacity_keyframes_as_curve_points() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let mut clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;

    clip.apply_property_mutation(PropertyMutation::SetKeyframe {
        path: Transform2D::OPACITY_PATH.to_string(),
        keyframe: Keyframe::linear(tt(0, tb), PropertyValue::Float(0.0)),
    })
    .expect("set start opacity");
    clip.apply_property_mutation(PropertyMutation::SetKeyframe {
        path: Transform2D::OPACITY_PATH.to_string(),
        keyframe: Keyframe::linear(tt(10, tb), PropertyValue::Float(0.5)),
    })
    .expect("set mid opacity");
    clip.apply_property_mutation(PropertyMutation::SetKeyframe {
        path: Transform2D::OPACITY_PATH.to_string(),
        keyframe: Keyframe::linear(tt(20, tb), PropertyValue::Float(1.0)),
    })
    .expect("set end opacity");

    sequence.video_tracks[0].add_clip(clip).expect("add clip");
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips.push(SelectedClipRef {
        track_id,
        is_video_track: true,
        clip_id,
    });

    let models = AppUiPanelModels::from_app_state(&state);

    let opacity_curve = models.inspector.opacity_curve.expect("opacity curve");
    assert_eq!(
        opacity_curve.keys.iter().map(|key| key.point).collect::<Vec<_>>(),
        vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]
    );
    assert!(opacity_curve.keys.iter().all(|key| key.keyframe_id.is_some()));
    assert_eq!(opacity_curve.display_points.len(), 129);
}

#[test]
fn app_state_models_synthesize_opacity_curve_endpoints_from_evaluated_values() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let mut clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;

    clip.apply_property_mutation(PropertyMutation::SetKeyframe {
        path: Transform2D::OPACITY_PATH.to_string(),
        keyframe: Keyframe::linear(tt(10, tb), PropertyValue::Float(0.5)),
    })
    .expect("set midpoint opacity");

    sequence.video_tracks[0].add_clip(clip).expect("add clip");
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips.push(SelectedClipRef {
        track_id,
        is_video_track: true,
        clip_id,
    });

    let models = AppUiPanelModels::from_app_state(&state);

    let opacity_curve = models.inspector.opacity_curve.expect("opacity curve");
    assert_eq!(
        opacity_curve.keys.iter().map(|key| key.point).collect::<Vec<_>>(),
        vec![
            CurvePoint::new(0.0, 0.5),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 0.5),
        ]
    );
    assert_eq!(
        opacity_curve
            .keys
            .iter()
            .map(|key| key.keyframe_id.is_some())
            .collect::<Vec<_>>(),
        vec![false, true, false]
    );
}

#[test]
fn asset_panel_model_reads_project_library() {
    let root = unique_temp_dir("asset-panel-model");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let asset_id = library
        .create_solid_color_asset(Some("Brand Purple"))
        .expect("create solid color asset");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));

    let models = AppUiPanelModels::from_app_state(&state);

    assert_eq!(models.assets.items.len(), 1);
    assert_eq!(
        models.assets.filter_placeholder.as_deref(),
        Some("搜索素材")
    );
    let item = &models.assets.items[0];
    assert_eq!(item.title, "Brand Purple");
    assert_eq!(badge_labels(item), ["图片"]);
    assert!(item.icon.is_some());
    assert!(item.select_action.is_none());
    assert_eq!(item.drag_payload, Some(DragPayload::Asset(asset_id)));
    let action = item.activate_action.as_ref().expect("activate action");
    let Action::Custom { namespace, name, payload } = action else {
        panic!("expected asset custom action, got {action:?}");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_PREPARE_DRAG);
    let payload: AssetsPrepareDragPayload =
        serde_json::from_value(payload.clone()).expect("asset drag payload");
    assert_eq!(payload.asset_id, asset_id);
    assert_eq!(item.context_menu_items.len(), 1);
    assert_eq!(item.context_menu_items[0].label, "删除素材");
    assert!(item.context_menu_items[0].icon.is_some());
    let Action::Custom { namespace, name, payload } =
        item.context_menu_items[0].action().expect("asset delete action")
    else {
        panic!("expected asset delete custom action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REMOVE_ENTRIES);
    let payload: AssetsDeleteSelectionPayload =
        serde_json::from_value(payload.clone()).expect("asset delete payload");
    assert_eq!(payload.asset_ids, vec![asset_id]);
    assert!(payload.folder_ids.is_empty());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn asset_panel_model_attaches_available_thumbnails_to_asset_cards() {
    struct TestThumbnails;

    impl AssetThumbnailSource for TestThumbnails {
        fn thumbnail_for_asset(&self, asset: &AssetRecord) -> AssetThumbnailState {
            AssetThumbnailState::Ready(
                RasterImage::new(
                    format!("test-thumb:{}", asset.id),
                    2,
                    2,
                    mondrian_ui_core::RasterImageColorSpace::Srgb,
                    vec![0, 0, 0, 255, 80, 0, 0, 255, 0, 80, 0, 255, 0, 0, 80, 255],
                )
                .expect("valid test thumbnail"),
            )
        }
    }

    let root = unique_temp_dir("asset-panel-thumbnails");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let asset_id = library
        .create_solid_color_asset(Some("Brand Purple"))
        .expect("create solid color asset");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));

    let models = AppUiPanelModels::from_app_state_with_asset_folder_and_thumbnails(
        &state,
        None,
        Some(&TestThumbnails),
    );

    let thumbnail = models.assets.items[0].thumbnail.as_ref().expect("thumbnail");
    assert_eq!(thumbnail.key, format!("test-thumb:{asset_id}"));
    assert_eq!((thumbnail.width, thumbnail.height), (2, 2));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn asset_panel_model_maps_thumbnail_loading_and_failed_states() {
    struct TestThumbnails(AssetThumbnailState);

    impl AssetThumbnailSource for TestThumbnails {
        fn thumbnail_for_asset(&self, _asset: &AssetRecord) -> AssetThumbnailState {
            self.0.clone()
        }
    }

    let root = unique_temp_dir("asset-panel-thumbnail-states");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    library
        .create_solid_color_asset(Some("Brand Purple"))
        .expect("create solid color asset");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));

    let loading = AppUiPanelModels::from_app_state_with_asset_folder_and_thumbnails(
        &state,
        None,
        Some(&TestThumbnails(AssetThumbnailState::Loading)),
    );
    assert_eq!(
        loading.assets.items[0].thumbnail_status,
        mondrian_ui_widgets::AssetGridThumbnailStatus::Loading
    );

    let failed = AppUiPanelModels::from_app_state_with_asset_folder_and_thumbnails(
        &state,
        None,
        Some(&TestThumbnails(AssetThumbnailState::Failed(
            AssetThumbnailFailure::new(AssetThumbnailFailureReason::DecodeFailed, "test failure"),
        ))),
    );
    assert_eq!(
        failed.assets.items[0].thumbnail_status,
        mondrian_ui_widgets::AssetGridThumbnailStatus::Failed
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn asset_panel_model_shows_top_level_folders_before_root_assets() {
    let root = unique_temp_dir("asset-panel-folders");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let folder_id = library.create_folder("Rushes", None).expect("create folder");
    let _nested_id =
        library.create_folder("Nested", Some(&folder_id)).expect("create nested folder");
    let filed_asset_id = library
        .create_solid_color_asset(Some("Filed Solid"))
        .expect("create filed solid");
    library
        .move_asset_to_folder(filed_asset_id, Some(&folder_id))
        .expect("move into folder");
    let root_asset_id = library
        .create_adjustment_layer_asset(Some("Root Adjustment"))
        .expect("create root adjustment");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));

    let models = AppUiPanelModels::from_app_state(&state);

    assert_eq!(models.assets.items.len(), 2);
    let folder = &models.assets.items[0];
    assert_eq!(folder.id, format!("folder:{folder_id}"));
    assert_eq!(folder.title, "Rushes");
    assert!(folder.subtitle.is_empty());
    assert_eq!(badge_labels(folder), ["2 项"]);
    assert!(folder.icon.is_some());
    assert_eq!(
        folder.drag_payload,
        Some(DragPayload::AssetFolder(folder_id.clone()))
    );
    assert_eq!(folder.context_menu_items.len(), 1);
    assert_eq!(folder.context_menu_items[0].label, "删除文件夹");
    assert!(folder.context_menu_items[0].icon.is_some());
    let Action::Custom { namespace, name, payload } =
        folder.context_menu_items[0].action().expect("asset folder delete action")
    else {
        panic!("expected asset folder delete custom action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REMOVE_ENTRIES);
    let payload: AssetsDeleteSelectionPayload =
        serde_json::from_value(payload.clone()).expect("folder delete payload");
    assert!(payload.asset_ids.is_empty());
    assert_eq!(payload.folder_ids, vec![folder_id.clone()]);
    let action = folder.activate_action.as_ref().expect("folder activate action");
    let Action::Custom { namespace, name, payload } = action else {
        panic!("expected asset folder custom action, got {action:?}");
    };
    assert_eq!(namespace, APP_SHELL_NAMESPACE);
    assert_eq!(name, APP_SHELL_ASSET_BROWSER_OPEN_FOLDER);
    let payload: AssetsOpenFolderPayload =
        serde_json::from_value(payload.clone()).expect("folder open payload");
    assert_eq!(payload.folder_id.as_deref(), Some(folder_id.as_str()));

    let asset = &models.assets.items[1];
    assert_eq!(asset.title, "Root Adjustment");
    assert_eq!(badge_labels(asset), ["序列"]);
    assert_eq!(asset.drag_payload, Some(DragPayload::Asset(root_asset_id)));
    assert!(
        !models.assets.items.iter().any(|item| item.title == "Nested"),
        "root asset view should not flatten nested folders"
    );
    assert!(
        !models.assets.items.iter().any(|item| item.title == "Filed Solid"),
        "root asset view should not flatten assets inside folders"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn asset_panel_model_can_show_one_folder_with_parent_navigation() {
    let root = unique_temp_dir("asset-panel-folder-view");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let folder_id = library.create_folder("Rushes", None).expect("create folder");
    let nested_id = library
        .create_folder("Selects", Some(&folder_id))
        .expect("create nested folder");
    let filed_asset_id = library
        .create_solid_color_asset(Some("Filed Solid"))
        .expect("create filed solid");
    library
        .move_asset_to_folder(filed_asset_id, Some(&folder_id))
        .expect("move into folder");
    let nested_asset_id = library
        .create_adjustment_layer_asset(Some("Nested Adjustment"))
        .expect("create nested asset");
    library
        .move_asset_to_folder(nested_asset_id, Some(&nested_id))
        .expect("move into nested folder");
    let _root_asset_id = library
        .create_adjustment_layer_asset(Some("Root Adjustment"))
        .expect("create root adjustment");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));

    let models = AppUiPanelModels::from_app_state_with_asset_folder(&state, Some(&folder_id));

    assert_eq!(models.assets.subtitle, "项目素材库 / Rushes");
    assert_eq!(
        models.assets.current_folder_id.as_deref(),
        Some(folder_id.as_str())
    );
    assert_eq!(models.assets.items.len(), 3);

    let parent = &models.assets.items[0];
    assert_eq!(parent.id, "asset-folder-up");
    assert_eq!(parent.title, "全部素材");
    assert!(parent.subtitle.is_empty());
    assert_eq!(badge_labels(parent), ["全部"]);
    let Action::Custom { namespace, name, payload } =
        parent.activate_action.as_ref().expect("parent activate action")
    else {
        panic!("expected parent navigation action");
    };
    assert_eq!(namespace, APP_SHELL_NAMESPACE);
    assert_eq!(name, APP_SHELL_ASSET_BROWSER_OPEN_FOLDER);
    let payload: AssetsOpenFolderPayload =
        serde_json::from_value(payload.clone()).expect("parent open payload");
    assert_eq!(payload.folder_id, None);
    assert!(parent.context_menu_items.is_empty());

    let nested = &models.assets.items[1];
    assert_eq!(nested.id, format!("folder:{nested_id}"));
    assert_eq!(nested.title, "Selects");
    assert!(nested.subtitle.is_empty());
    assert_eq!(badge_labels(nested), ["1 项"]);
    assert_eq!(
        nested.drag_payload,
        Some(DragPayload::AssetFolder(nested_id.clone()))
    );
    assert_eq!(nested.context_menu_items.len(), 1);
    let Action::Custom { namespace, name, payload } =
        nested.context_menu_items[0].action().expect("nested folder delete action")
    else {
        panic!("expected nested folder delete custom action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REMOVE_ENTRIES);
    let payload: AssetsDeleteSelectionPayload =
        serde_json::from_value(payload.clone()).expect("nested folder delete payload");
    assert!(payload.asset_ids.is_empty());
    assert_eq!(payload.folder_ids, vec![nested_id]);

    let asset = &models.assets.items[2];
    assert_eq!(asset.title, "Filed Solid");
    assert_eq!(asset.drag_payload, Some(DragPayload::Asset(filed_asset_id)));
    assert!(
        !models.assets.items.iter().any(|item| item.title == "Root Adjustment"),
        "folder view should not include root assets"
    );
    assert!(
        !models.assets.items.iter().any(|item| item.title == "Nested Adjustment"),
        "folder view should not flatten assets from nested folders"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn asset_panel_empty_library_uses_true_empty_grid_state() {
    let root = unique_temp_dir("asset-panel-empty-library");
    let library = AssetLibrary::open(root.clone()).expect("open asset library");
    let mut state = AppState::new();
    state.test_set_asset_library(Some(library));

    let models = AppUiPanelModels::from_app_state(&state);

    assert!(models.assets.items.is_empty());
    assert!(models.assets.accepts_file_drop);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn effect_panel_model_keeps_catalog_browsable_without_apply_target() {
    let model = PanelListModel::from_effect_registry(None);

    assert!(model.subtitle.is_empty());
    assert_eq!(model.filter_placeholder.as_deref(), Some("搜索效果"));
    assert!(model.items.iter().all(|item| item.select_action.is_none()));
    assert!(model.items.iter().all(|item| item.activate_action.is_none()));
    assert!(model.items.iter().all(|item| item.icon.is_none()));
    assert!(model.items.iter().all(|item| item.badge.is_none()));
    assert!(model.items.iter().all(|item| item.subtitle.is_empty()));
    assert!(model.items.iter().any(|item| {
        item.title == "颜色" && item.tree_depth == 0 && item.tree_expanded == Some(true)
    }));
    assert!(model
        .items
        .iter()
        .any(|item| { item.tree_depth > 0 && item.tree_expanded.is_none() && !item.disabled }));
}

#[test]
fn effect_panel_model_adds_effect_actions_for_selected_video_clip() {
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    let selection = SelectedClipRef { track_id, is_video_track: true, clip_id };

    let model = PanelListModel::from_effect_registry(Some(selection));

    assert!(model.subtitle.is_empty());
    assert!(model.items.iter().all(|item| item.icon.is_none()));
    assert!(model.items.iter().all(|item| item.badge.is_none()));
    assert!(model.items.iter().all(|item| item.subtitle.is_empty()));
    assert!(model.items.iter().any(|item| item.tree_expanded.is_some()));
    let action = model
        .items
        .iter()
        .find_map(|item| item.activate_action.as_ref())
        .expect("effect activate action");
    let Action::Custom { namespace, name, payload } = action else {
        panic!("expected effect custom action, got {action:?}");
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_ADD_TO_CLIP);
    let payload: VisualEffectAddToClipPayload =
        serde_json::from_value(payload.clone()).expect("effect add payload");
    assert_eq!(payload.clip_id, clip_id);
}

#[test]
fn effect_panel_model_keeps_category_rows_separate_from_effect_apply_rows() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };

    let model = PanelListModel::from_effect_registry(Some(selection));

    let color_category = model
        .items
        .iter()
        .find(|item| item.title == "颜色" && item.tree_depth == 0)
        .expect("top-level color category");
    assert_eq!(color_category.tree_id.as_deref(), Some("颜色"));
    assert_eq!(color_category.tree_expanded, Some(true));
    assert!(color_category.activate_action.is_none());

    let blur_effect = model
        .items
        .iter()
        .find(|item| item.title == effect_display_name(&EffectType::GaussianBlur))
        .expect("Gaussian blur effect row");
    assert_eq!(
        blur_effect.tree_depth,
        EffectType::GaussianBlur.category_path().len() as u8
    );
    assert!(blur_effect.tree_id.is_none());
    assert!(blur_effect.tree_expanded.is_none());
    assert!(blur_effect.activate_action.is_some());
    assert_eq!(model.filter_placeholder.as_deref(), Some("搜索效果"));
    assert_eq!(
        blur_effect.title,
        effect_display_name(&EffectType::GaussianBlur)
    );
}

#[test]
fn effects_add_refreshes_inspector_and_node_graph_selection_models() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips.push(SelectedClipRef {
        track_id,
        is_video_track: true,
        clip_id,
    });

    state
        .dispatch_action(visual_effect_add_to_clip_action(
            VisualEffectAddToClipPayload { clip_id, effect_type: EffectType::GaussianBlur },
        ))
        .expect("dispatch add effect");

    let selected = state.primary_selected_effect().expect("new effect selected");
    assert_eq!(selected.clip.clip_id, clip_id);
    let models = AppUiPanelModels::from_app_state(&state);

    assert_eq!(
        models.inspector.selected_effect_id,
        Some(selected.effect_id)
    );
    assert_eq!(models.inspector.effects.len(), 1);
    assert_eq!(models.inspector.effects[0].effect_id, selected.effect_id);
    assert_eq!(
        models.node_graph.selected_node_id,
        Some(format!("effect:{}", selected.effect_id))
    );
    assert!(models.node_graph.node_targets.iter().any(|target| {
        target.target == NodeGraphTarget::Effect(selected.effect_id)
            && target.node_id == format!("effect:{}", selected.effect_id)
    }));
}

#[test]
fn primary_grade_catalog_and_inspector_are_typed_animatable_and_undoable() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("primary-grade-ui");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    state.test_set_sequence(Some(sequence));
    let selection = SelectedClipRef { track_id, is_video_track: true, clip_id };
    state.selection.selected_clips.push(selection);

    let catalog = PanelListModel::from_effect_registry(Some(selection));
    for effect_type in [
        EffectType::WhiteBalance,
        EffectType::ColorWheel,
        EffectType::AscCdl,
    ] {
        let row = catalog
            .items
            .iter()
            .find(|item| item.title == effect_display_name(&effect_type))
            .unwrap_or_else(|| panic!("missing {} catalog row", effect_type.key()));
        assert!(row.activate_action.is_some());
        assert!(!row.disabled);
    }

    for effect_type in [
        EffectType::WhiteBalance,
        EffectType::ColorWheel,
        EffectType::AscCdl,
    ] {
        state
            .dispatch_action(visual_effect_add_to_clip_action(
                VisualEffectAddToClipPayload { clip_id, effect_type },
            ))
            .expect("add primary grade effect");
    }

    let models = AppUiPanelModels::from_app_state(&state);
    assert_eq!(models.inspector.effects.len(), 3);
    let white_balance = &models.inspector.effects[0];
    assert_eq!(white_balance.label, "白平衡");
    assert_eq!(
        white_balance
            .properties
            .iter()
            .map(|property| property.label.as_str())
            .collect::<Vec<_>>(),
        ["色温", "色调"]
    );
    assert!(white_balance.properties.iter().all(|property| {
        property.is_animatable && matches!(property.value, PropertyValue::Float(_))
    }));

    let primaries = &models.inspector.effects[1];
    assert_eq!(primaries.label, "Primaries");
    assert_eq!(
        primaries
            .properties
            .iter()
            .map(|property| property.label.as_str())
            .collect::<Vec<_>>(),
        ["Offset", "Lift", "Gamma", "Gain"]
    );
    assert!(primaries.properties.iter().all(|property| {
        property.is_animatable && matches!(property.value, PropertyValue::Vec3(_))
    }));

    let cdl = &models.inspector.effects[2];
    assert_eq!(cdl.label, "ASC CDL");
    assert_eq!(
        cdl.properties
            .iter()
            .map(|property| property.label.as_str())
            .collect::<Vec<_>>(),
        ["Slope", "Offset", "Power", "Saturation"]
    );
    assert!(cdl.properties.iter().all(|property| property.is_animatable));
    let slope = &cdl.properties[0];
    let cdl_id = cdl.effect_id;
    let edited_slope = glam::Vec3::new(1.1, 0.9, 1.2);
    let action = inspector_effect_property_action(
        Some(selection),
        cdl_id,
        slope.address.clone(),
        PropertyValue::Vec3(edited_slope),
    )
    .expect("typed CDL slope action");
    state.dispatch_action(action).expect("edit CDL slope");

    let edited = AppUiPanelModels::from_app_state(&state);
    assert_eq!(
        edited.inspector.effects[2].properties[0].value,
        PropertyValue::Vec3(edited_slope)
    );
    state
        .dispatch_action(mondrian_editor_state::Action::Undo)
        .expect("undo CDL slope edit");
    let undone = AppUiPanelModels::from_app_state(&state);
    assert_eq!(
        undone.inspector.effects[2].properties[0].value,
        PropertyValue::Vec3(glam::Vec3::ONE)
    );
}

#[test]
fn inspector_exposes_gamut_compression_and_highlight_recovery_schema() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("gamut-inspector");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add clip");
    state.test_set_sequence(Some(sequence));
    let selection = SelectedClipRef { track_id, is_video_track: true, clip_id };
    state.selection.selected_clips.push(selection);

    for effect_type in [EffectType::GamutCompression, EffectType::HighlightRecovery] {
        state
            .dispatch_action(visual_effect_add_to_clip_action(
                VisualEffectAddToClipPayload { clip_id, effect_type },
            ))
            .expect("add gamut/highlight effect");
    }

    let models = AppUiPanelModels::from_app_state(&state);
    assert_eq!(models.inspector.effects.len(), 2);
    let gamut = &models.inspector.effects[0];
    assert_eq!(gamut.label, "色域压缩");
    assert_eq!(
        gamut
            .properties
            .iter()
            .map(|property| property.label.as_str())
            .collect::<Vec<_>>(),
        ["强度"]
    );
    assert!(gamut.properties[0].is_animatable);

    let highlight = &models.inspector.effects[1];
    assert_eq!(highlight.label, "高光恢复");
    assert_eq!(
        highlight
            .properties
            .iter()
            .map(|property| property.label.as_str())
            .collect::<Vec<_>>(),
        ["起始阈值", "过渡宽度", "恢复强度"]
    );
    assert!(highlight.properties.iter().all(|property| {
        property.is_animatable && matches!(property.value, PropertyValue::Float(_))
    }));
}

#[test]
fn inspector_exposes_grouped_typed_hdr_grading_and_supports_undo() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("hdr-grading-inspector");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add clip");
    state.test_set_sequence(Some(sequence));
    let selection = SelectedClipRef { track_id, is_video_track: true, clip_id };
    state.selection.selected_clips.push(selection);

    state
        .dispatch_action(visual_effect_add_to_clip_action(
            VisualEffectAddToClipPayload { clip_id, effect_type: EffectType::HdrGrading },
        ))
        .expect("add HDR grading effect");

    let models = AppUiPanelModels::from_app_state(&state);
    let hdr = &models.inspector.effects[0];
    assert_eq!(hdr.label, "HDR Grading");
    assert_eq!(hdr.properties.len(), 33);
    let groups = hdr
        .properties
        .iter()
        .filter_map(|property| property.group_name.as_deref())
        .fold(Vec::<&str>::new(), |mut groups, group| {
            if groups.last().copied() != Some(group) {
                groups.push(group);
            }
            groups
        });
    assert_eq!(
        groups,
        [
            "HDR · Global",
            "HDR · Blacks",
            "HDR · Dark",
            "HDR · Shadows",
            "HDR · Light",
            "HDR · Highlights",
            "HDR · Specular",
        ]
    );
    assert!(hdr.properties.iter().all(|property| property.is_animatable));
    assert!(matches!(hdr.properties[2].value, PropertyValue::Vec3(_)));

    let exposure = &hdr.properties[0];
    let effect_id = hdr.effect_id;
    let action = inspector_effect_property_action(
        Some(selection),
        effect_id,
        exposure.address.clone(),
        PropertyValue::Float(1.5),
    )
    .expect("typed HDR exposure action");
    state.dispatch_action(action).expect("edit HDR exposure");
    let edited = AppUiPanelModels::from_app_state(&state);
    assert_eq!(
        edited.inspector.effects[0].properties[0].value,
        PropertyValue::Float(1.5)
    );

    state
        .dispatch_action(mondrian_editor_state::Action::Undo)
        .expect("undo HDR exposure edit");
    let undone = AppUiPanelModels::from_app_state(&state);
    assert_eq!(
        undone.inspector.effects[0].properties[0].value,
        PropertyValue::Float(0.0)
    );
}

#[test]
fn node_graph_model_falls_back_to_source_after_selected_effect_removal() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let remove_effect = mondrian_effects::EffectNode::with_defaults(EffectType::GaussianBlur);
    let keep_effect = mondrian_effects::EffectNode::with_defaults(EffectType::Sharpen);
    let remove_id = remove_effect.id;
    let keep_id = keep_effect.id;
    clip.add_effect_node(remove_effect);
    clip.add_effect_node(keep_effect);
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    state.test_set_sequence(Some(sequence));
    let selection = state
        .select_effect_by_id(clip_id, remove_id)
        .expect("seed selected effect")
        .clip;

    state
        .dispatch_action(visual_effect_remove_action(VisualEffectTargetPayload {
            clip_id: selection.clip_id,
            effect_id: remove_id,
        }))
        .expect("dispatch remove selected effect");

    assert!(state.primary_selected_effect().is_none());
    let models = AppUiPanelModels::from_app_state(&state);

    assert_eq!(models.node_graph.selected_clip, Some(selection));
    assert_eq!(
        models.node_graph.selected_node_id,
        Some("source".to_owned())
    );
    assert_eq!(
        models.node_graph.nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>(),
        vec![
            "source".to_owned(),
            format!("effect:{keep_id}"),
            "output".to_owned(),
        ]
    );
    assert!(!models
        .node_graph
        .node_targets
        .iter()
        .any(|target| target.target == NodeGraphTarget::Effect(remove_id)));
}

#[test]
fn node_graph_model_preserves_selected_effect_after_reorder() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let mut clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let first = mondrian_effects::EffectNode::with_defaults(EffectType::GaussianBlur);
    let second = mondrian_effects::EffectNode::with_defaults(EffectType::Sharpen);
    let third = mondrian_effects::EffectNode::with_defaults(EffectType::BasicCorrection);
    let first_id = first.id;
    let second_id = second.id;
    let third_id = third.id;
    clip.add_effect_node(first);
    clip.add_effect_node(second);
    clip.add_effect_node(third);
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    state.test_set_sequence(Some(sequence));
    state.select_effect_by_id(clip_id, second_id).expect("seed selected effect");

    state
        .dispatch_action(visual_effect_reorder_action(VisualEffectReorderPayload {
            clip_id,
            effect_id: second_id,
            placement: EffectRelativePlacement::Before(first_id),
        }))
        .expect("dispatch reorder selected effect");

    let models = AppUiPanelModels::from_app_state(&state);
    let selected = state.primary_selected_effect().expect("selected effect survives reorder");

    assert_eq!(selected.effect_id, second_id);
    assert_eq!(models.node_graph.selected_clip, Some(selected.clip));
    assert_eq!(
        models.node_graph.selected_node_id,
        Some(format!("effect:{second_id}"))
    );
    assert_eq!(
        models.node_graph.nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>(),
        vec![
            "source".to_owned(),
            format!("effect:{second_id}"),
            format!("effect:{first_id}"),
            format!("effect:{third_id}"),
            "output".to_owned(),
        ]
    );
    assert_eq!(
        models.node_graph.edges,
        vec![
            NodeGraphEdge::new("source", format!("effect:{second_id}")),
            NodeGraphEdge::new(format!("effect:{second_id}"), format!("effect:{first_id}")),
            NodeGraphEdge::new(format!("effect:{first_id}"), format!("effect:{third_id}")),
            NodeGraphEdge::new(format!("effect:{third_id}"), "output"),
        ]
    );
    assert!(models.node_graph.node_targets.iter().any(|target| {
        target.node_id == format!("effect:{second_id}")
            && target.target == NodeGraphTarget::Effect(second_id)
    }));
}

#[test]
fn inspector_effect_vector_property_rows_get_multi_component_height() {
    assert_eq!(
        effect_property_row_height(&PropertyValue::Vec2(glam::Vec2::ZERO)),
        Some(64.0)
    );
    assert_eq!(
        effect_property_row_height(&PropertyValue::Vec4([0.0, 0.0, 0.0, 0.0])),
        Some(132.0)
    );
    assert_eq!(effect_property_row_height(&PropertyValue::Float(0.5)), None);
    assert_eq!(
        effect_property_row_height(&PropertyValue::Curve(NormalizedCurve::identity())),
        Some(150.0)
    );
}

#[test]
fn inspector_qualifier_sample_editor_preserves_typed_stable_address_and_validity() {
    let effect_id = EffectId::new();
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let parameter = test_parameter_address("mondrian.effect.builtin.qualifier.samples");
    let path = "effect.qualifier.samples".to_owned();
    let samples = QualifierSampleSet::new(vec![
        QualifierSample::new([0.0, 1.0, 0.0], QualifierSampleOperation::Include),
        QualifierSample::new([1.0, 0.0, 0.0], QualifierSampleOperation::Exclude),
    ])
    .expect("valid Qualifier samples");
    let property = InspectorEffectPropertyModel {
        schema: ParameterSchema::v1(
            parameter.parameter_id.clone(),
            PropertyValue::QualifierSamples(samples.clone()),
        ),
        address: parameter.clone(),
        path: path.clone(),
        label: "Samples".to_owned(),
        group_name: None,
        value: PropertyValue::QualifierSamples(samples.clone()),
        min: None,
        max: None,
        hard_min: None,
        hard_max: None,
        step: None,
        is_animatable: false,
    };
    assert_eq!(
        effect_property_row_height(&property.value),
        Some(102.0),
        "two samples plus the add row own their full inspector height"
    );
    let mut widget =
        effect_property_value_widget(&property, true, Some(selection), effect_id, path.clone());
    widget.layout(Rect::new(0.0, 0.0, 220.0, 102.0));
    assert_eq!(widget.child_count(), 3);

    let mut changed = samples.samples().to_vec();
    changed[0].rgb = [0.1, 0.8, 0.2];
    changed.push(QualifierSample::new(
        [0.2, 0.2, 0.9],
        QualifierSampleOperation::Exclude,
    ));
    let Some(Action::Custom { namespace, name, payload }) = qualifier_sample_set_action(
        Some(selection),
        InspectorPropertyTarget::Effect { effect_id, parameter: parameter.clone() },
        &path,
        changed.clone(),
    ) else {
        panic!("valid Qualifier edit must dispatch");
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_SET_PARAMETER_VALUE);
    let payload: VisualEffectSetParameterValuePayload =
        serde_json::from_value(payload).expect("Qualifier action payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.effect_id, effect_id);
    assert_eq!(payload.parameter, parameter);
    assert_eq!(
        payload.value,
        PropertyValue::QualifierSamples(
            QualifierSampleSet::new(changed).expect("same valid sample payload")
        )
    );

    assert!(qualifier_sample_set_action(
        Some(selection),
        InspectorPropertyTarget::Effect {
            effect_id,
            parameter: test_parameter_address("mondrian.effect.builtin.qualifier.samples"),
        },
        &path,
        vec![QualifierSample::new(
            [1.0, 0.0, 0.0],
            QualifierSampleOperation::Exclude,
        )],
    )
    .is_none());
    assert!(qualifier_sample_set_action(
        Some(selection),
        InspectorPropertyTarget::Effect {
            effect_id,
            parameter: test_parameter_address("mondrian.effect.builtin.qualifier.samples"),
        },
        &path,
        Vec::new(),
    )
    .is_none());
}

#[test]
fn inspector_effect_curve_editor_dispatches_structured_curve_value() {
    let effect_id = EffectId::new();
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let property = InspectorEffectPropertyModel {
        schema: ParameterSchema::v1(
            mondrian_core::ParameterId::new_static("mondrian.test.curves.master"),
            PropertyValue::Curve(NormalizedCurve::identity()),
        ),
        address: test_parameter_address("mondrian.test.curves.master"),
        path: "curves.master".to_string(),
        label: "Master".to_string(),
        group_name: None,
        value: PropertyValue::Curve(NormalizedCurve::identity()),
        min: None,
        max: None,
        hard_min: None,
        hard_max: None,
        step: None,
        is_animatable: false,
    };
    let mut widget = effect_property_value_widget(
        &property,
        true,
        Some(selection),
        effect_id,
        property.path.clone(),
    );
    widget.layout(Rect::new(0.0, 0.0, 220.0, 150.0));
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(110.0, 75.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected inspector curve action, got {:?}", recorded[0]);
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_SET_PARAMETER_VALUE);
    let payload: VisualEffectSetParameterValuePayload =
        serde_json::from_value(payload.clone()).expect("curve property payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.effect_id, effect_id);
    assert_eq!(payload.parameter, property.address);
    let PropertyValue::Curve(curve) = payload.value else {
        panic!("expected structured Curve payload");
    };
    assert_eq!(curve.points().len(), 3);
    assert_eq!(curve.points()[0], NormalizedCurvePoint::new(0.0, 0.0));
    assert_eq!(curve.points()[2], NormalizedCurvePoint::new(1.0, 1.0));
    assert!(curve.points()[1].x > 0.4 && curve.points()[1].x < 0.6);
}

#[test]
fn inspector_effect_vec3_property_widget_dispatches_component_change() {
    let effect_id = EffectId::new();
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let property = InspectorEffectPropertyModel {
        schema: ParameterSchema::v1(
            mondrian_core::ParameterId::new_static("mondrian.test.lighting_direction"),
            PropertyValue::Vec3(glam::Vec3::new(0.1, 0.2, 0.3)),
        ),
        address: test_parameter_address("mondrian.test.lighting_direction"),
        path: "lighting.direction".to_string(),
        label: "Direction".to_string(),
        group_name: None,
        value: PropertyValue::Vec3(glam::Vec3::new(0.1, 0.2, 0.3)),
        min: Some(0.0),
        max: Some(1.0),
        hard_min: None,
        hard_max: None,
        step: Some(0.01),
        is_animatable: true,
    };
    let mut widget = effect_property_value_widget(
        &property,
        true,
        Some(selection),
        effect_id,
        property.path.clone(),
    );
    widget.layout(Rect::new(0.0, 0.0, 220.0, 94.0));

    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    let result = widget.event(
        &UiEvent::MouseDown {
            position: Point::new(80.0, 75.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );

    assert_eq!(result, EventResult::Handled);
    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected inspector custom action, got {:?}", recorded[0]);
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_SET_PARAMETER_VALUE);
    let payload: VisualEffectSetParameterValuePayload =
        serde_json::from_value(payload.clone()).expect("set effect property payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.effect_id, effect_id);
    assert_eq!(payload.parameter, property.address);
    let PropertyValue::Vec3(value) = payload.value else {
        panic!("expected Vec3 payload");
    };
    assert_eq!(value.x, 0.1);
    assert_eq!(value.y, 0.2);
    assert!(
        value.z > 0.3,
        "clicking the third component slider should update z, got {value:?}"
    );
}

#[test]
fn inspector_effect_float_property_number_input_honors_descriptor_step() {
    let effect_id = EffectId::new();
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let property = InspectorEffectPropertyModel {
        schema: ParameterSchema::v1(
            mondrian_core::ParameterId::new_static("mondrian.test.color_exposure"),
            PropertyValue::Float(0.2),
        ),
        address: test_parameter_address("mondrian.test.color_exposure"),
        path: "color.exposure".to_string(),
        label: "Exposure".to_string(),
        group_name: None,
        value: PropertyValue::Float(0.2),
        min: Some(0.0),
        max: Some(1.0),
        hard_min: None,
        hard_max: None,
        step: Some(0.25),
        is_animatable: true,
    };
    let mut widget = effect_property_value_widget(
        &property,
        true,
        Some(selection),
        effect_id,
        property.path.clone(),
    );
    widget.layout(Rect::new(0.0, 0.0, 220.0, 28.0));
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(190.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        widget.event(&UiEvent::TextInput("0.62".to_string()), &mut ctx),
        EventResult::Handled
    );
    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { payload, .. } = &recorded[0] else {
        panic!("expected inspector custom action, got {:?}", recorded[0]);
    };
    let payload: VisualEffectSetParameterValuePayload =
        serde_json::from_value(payload.clone()).expect("set effect property payload");
    assert_eq!(payload.parameter, property.address);
    let PropertyValue::Float(value) = payload.value else {
        panic!("expected Float payload");
    };
    assert!(
        (value - 0.5).abs() <= 0.0001,
        "expected stepped value 0.5 from typed 0.62, got {value}"
    );
}

#[test]
fn inspector_disabled_effect_property_row_remains_editable_for_unlocked_clip() {
    let effect_id = EffectId::new();
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let property = InspectorEffectPropertyModel {
        schema: ParameterSchema::v1(
            mondrian_core::ParameterId::new_static("mondrian.test.blur_radius"),
            PropertyValue::Float(0.2),
        ),
        address: test_parameter_address("mondrian.test.blur_radius"),
        path: "blur.radius".to_string(),
        label: "Radius".to_string(),
        group_name: None,
        value: PropertyValue::Float(0.2),
        min: Some(0.0),
        max: Some(1.0),
        hard_min: None,
        hard_max: None,
        step: Some(0.1),
        is_animatable: true,
    };
    let disabled_effect = InspectorEffectModel {
        effect_id,
        label: "Gaussian Blur".to_string(),
        enabled: false,
        properties: vec![property.clone()],
    };
    assert!(
        !disabled_effect.enabled,
        "effect runtime bypass state should not imply read-only property rows"
    );

    let mut widget = effect_property_value_widget(
        &property,
        true,
        Some(selection),
        effect_id,
        property.path.clone(),
    );
    widget.layout(Rect::new(0.0, 0.0, 220.0, 28.0));
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(80.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected inspector custom action, got {:?}", recorded[0]);
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_SET_PARAMETER_VALUE);
    let payload: VisualEffectSetParameterValuePayload =
        serde_json::from_value(payload.clone()).expect("set effect property payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.effect_id, effect_id);
    assert_eq!(payload.parameter, property.address);
    let PropertyValue::Float(value) = payload.value else {
        panic!("expected Float payload");
    };
    assert!(
        value > 0.2,
        "clicking the enabled property row should update a disabled effect's property, got {value}"
    );
}

#[test]
fn inspector_effect_float_property_keyboard_nudge_sanitizes_descriptor_bounds() {
    let effect_id = EffectId::new();
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let property = InspectorEffectPropertyModel {
        schema: ParameterSchema::v1(
            mondrian_core::ParameterId::new_static("mondrian.test.color_exposure"),
            PropertyValue::Float(0.2),
        ),
        address: test_parameter_address("mondrian.test.color_exposure"),
        path: "color.exposure".to_string(),
        label: "Exposure".to_string(),
        group_name: None,
        value: PropertyValue::Float(0.2),
        min: Some(f64::NAN),
        max: Some(f64::INFINITY),
        hard_min: None,
        hard_max: None,
        step: Some(0.25),
        is_animatable: true,
    };
    let mut widget = effect_property_value_widget(
        &property,
        true,
        Some(selection),
        effect_id,
        property.path.clone(),
    );
    widget.layout(Rect::new(0.0, 0.0, 220.0, 28.0));
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(190.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { payload, .. } = &recorded[0] else {
        panic!("expected inspector custom action, got {:?}", recorded[0]);
    };
    let payload: VisualEffectSetParameterValuePayload =
        serde_json::from_value(payload.clone()).expect("set effect property payload");
    assert_eq!(payload.parameter, property.address);
    let PropertyValue::Float(value) = payload.value else {
        panic!("expected Float payload");
    };
    assert!(
        (value - 0.5).abs() <= 0.0001,
        "expected finite stepped nudge, got {value}"
    );
}

#[test]
fn inspector_effect_int_property_number_input_defaults_to_unit_step() {
    let effect_id = EffectId::new();
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let property = InspectorEffectPropertyModel {
        schema: ParameterSchema::v1(
            mondrian_core::ParameterId::new_static("mondrian.test.level_iterations"),
            PropertyValue::Int(10),
        ),
        address: test_parameter_address("mondrian.test.level_iterations"),
        path: "levels.iterations".to_string(),
        label: "Iterations".to_string(),
        group_name: None,
        value: PropertyValue::Int(10),
        min: Some(0.0),
        max: Some(1000.0),
        hard_min: None,
        hard_max: None,
        step: None,
        is_animatable: false,
    };
    let mut widget = effect_property_value_widget(
        &property,
        true,
        Some(selection),
        effect_id,
        property.path.clone(),
    );
    widget.layout(Rect::new(0.0, 0.0, 220.0, 28.0));
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(190.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        widget.event(&UiEvent::TextInput("12.4".to_string()), &mut ctx),
        EventResult::Handled
    );
    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { payload, .. } = &recorded[0] else {
        panic!("expected inspector custom action, got {:?}", recorded[0]);
    };
    let payload: VisualEffectSetParameterValuePayload =
        serde_json::from_value(payload.clone()).expect("set effect property payload");
    assert_eq!(payload.parameter, property.address);
    assert_eq!(payload.value, PropertyValue::Int(12));
}

#[test]
fn inspector_effect_color_property_action_uses_typed_payload() {
    let effect_id = EffectId::new();
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let color = Color::from_rgba8(24, 96, 180, 220);

    let action = inspector_effect_property_action(
        Some(selection),
        effect_id,
        test_parameter_address("mondrian.test.key_color"),
        PropertyValue::Color(color),
    );

    let Some(Action::Custom { namespace, name, payload }) = action else {
        panic!("expected inspector set effect property action");
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_SET_PARAMETER_VALUE);
    let payload: VisualEffectSetParameterValuePayload =
        serde_json::from_value(payload).expect("set effect property payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.effect_id, effect_id);
    assert_eq!(
        payload.parameter.parameter_id,
        mondrian_core::ParameterId::new_static("mondrian.test.key_color")
    );
    assert_eq!(payload.value, PropertyValue::Color(color));
}

#[test]
fn numeric_slider_input_control_text_input_dispatches_typed_transform_action() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let parameter = test_parameter_address("mondrian.transform.rotation");
    let expected_parameter = parameter.clone();
    let mut widget =
        numeric_slider_input_control(12.0, -180.0, 180.0, Some(0.1), 1, true, move |value| {
            inspector_parameter_action(
                Some(selection),
                Some(parameter.clone()),
                PropertyValue::Float(value),
            )
        });
    widget.layout(Rect::new(0.0, 0.0, 220.0, 28.0));

    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(190.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        widget.event(&UiEvent::TextInput("45.6".to_string()), &mut ctx),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected inspector custom action, got {:?}", recorded[0]);
    };
    assert_eq!(namespace, CLIP_NAMESPACE);
    assert_eq!(name, CLIP_WRITE_PARAMETER_VALUES);
    let payload: ClipWriteParameterValuesPayload =
        serde_json::from_value(payload.clone()).expect("transform payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.writes.len(), 1);
    assert_eq!(payload.writes[0].parameter, expected_parameter);
    let PropertyValue::Float(value) = payload.writes[0].value else {
        panic!("rotation payload must remain Float");
    };
    assert!((value - 45.6).abs() < 0.0001);
}

#[test]
fn numeric_slider_input_control_disabled_text_input_does_not_dispatch() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let parameter = test_parameter_address("mondrian.transform.opacity");
    let mut widget =
        numeric_slider_input_control(50.0, 0.0, 100.0, Some(1.0), 0, false, move |value| {
            inspector_parameter_action(
                Some(selection),
                Some(parameter.clone()),
                PropertyValue::Float(value / 100.0),
            )
        });
    widget.layout(Rect::new(0.0, 0.0, 220.0, 28.0));

    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(190.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Ignored
    );
    assert_eq!(
        widget.event(&UiEvent::TextInput("75".to_string()), &mut ctx),
        EventResult::Ignored
    );
    assert!(actions.borrow().is_empty());
}

#[test]
fn app_state_models_disable_effect_actions_for_locked_selected_track() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    sequence.video_tracks[0].is_locked = true;
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips.push(SelectedClipRef {
        track_id,
        is_video_track: true,
        clip_id,
    });

    let models = AppUiPanelModels::from_app_state(&state);

    assert!(models.effects.subtitle.is_empty());
    assert!(models.effects.items.iter().all(|item| item.activate_action.is_none()));
    assert!(models
        .effects
        .items
        .iter()
        .all(|item| item.icon.is_none() && item.badge.is_none() && item.subtitle.is_empty()));
}

#[test]
fn app_state_timeline_model_uses_app_command_availability_for_locked_tracks() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    sequence.video_tracks[0].is_locked = true;
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips.push(SelectedClipRef {
        track_id,
        is_video_track: true,
        clip_id,
    });
    state.seek(15).expect("seek");

    let model = TimelinePanelModel::from_app_state(&state);

    assert!(model.edit_command_available(TimelineEditCommand::CopySelection));
    for command in [
        TimelineEditCommand::CutSelection,
        TimelineEditCommand::DuplicateSelection,
        TimelineEditCommand::DeleteSelection,
        TimelineEditCommand::RippleDeleteSelection,
        TimelineEditCommand::SplitAtPlayhead,
        TimelineEditCommand::TrimSelectionInToPlayhead,
        TimelineEditCommand::TrimSelectionOutToPlayhead,
        TimelineEditCommand::RollSelectedCutToPlayhead,
        TimelineEditCommand::EnableSelection,
        TimelineEditCommand::DisableSelection,
    ] {
        assert!(!model.edit_command_available(command), "{command:?}");
    }
}

#[test]
fn app_state_timeline_model_uses_edge_specific_trim_availability() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(10, tb), tt(20, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips.push(SelectedClipRef {
        track_id,
        is_video_track: true,
        clip_id,
    });

    state.seek(10).expect("seek");
    let at_clip_start = TimelinePanelModel::from_app_state(&state);
    assert!(!at_clip_start.edit_command_available(TimelineEditCommand::TrimSelectionInToPlayhead));
    assert!(at_clip_start.edit_command_available(TimelineEditCommand::TrimSelectionOutToPlayhead));

    state.seek(29).expect("seek");
    let at_clip_end = TimelinePanelModel::from_app_state(&state);
    assert!(at_clip_end.edit_command_available(TimelineEditCommand::TrimSelectionInToPlayhead));
    assert!(!at_clip_end.edit_command_available(TimelineEditCommand::TrimSelectionOutToPlayhead));
}

#[test]
fn app_state_models_mark_inspector_readonly_for_locked_selected_track() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let clip = Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip");
    let clip_id = clip.id;
    let track_id = sequence.video_tracks[0].id;
    sequence.video_tracks[0].add_clip(clip).expect("add video clip");
    sequence.video_tracks[0].is_locked = true;
    state.test_set_sequence(Some(sequence));
    state.selection.selected_clips.push(SelectedClipRef {
        track_id,
        is_video_track: true,
        clip_id,
    });

    let models = AppUiPanelModels::from_app_state(&state);

    assert_eq!(
        models.inspector.selected_clip,
        Some(SelectedClipRef { track_id, is_video_track: true, clip_id })
    );
    assert!(!models.inspector.is_editable);
    assert_eq!(
        models.inspector.edit_disabled_reason.as_deref(),
        Some("所选剪辑所在轨道已锁定")
    );
}

#[test]
fn demo_asset_model_uses_explicit_item_actions() {
    let model = demo_asset_model();

    assert!(model.items.iter().all(|item| item.activate_action.is_some()));
    assert!(model.items.iter().all(|item| item.select_action.is_some()));
    let action = model.items[0].select_action.as_ref().expect("select");
    let Action::Custom { namespace, name, payload } = action else {
        panic!("expected demo custom action, got {action:?}");
    };
    assert_eq!(namespace, "ui.demo_panel");
    assert_eq!(name, "assets.select.footage");
    assert!(payload.is_null());

    let action = model.items[0].activate_action.as_ref().expect("activate");
    let Action::Custom { namespace, name, payload } = action else {
        panic!("expected demo custom action, got {action:?}");
    };
    assert_eq!(namespace, "ui.demo_panel");
    assert_eq!(name, "assets.activate.footage");
    assert!(payload.is_null());
}

#[test]
fn timeline_model_uses_solid_color_clip_color() {
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    let color = Color::from_rgba8(12, 34, 56, 200);
    let solid =
        Clip::new_solid_color(AssetId::new(), color, tt(0, tb), tt(30, tb)).expect("valid clip");
    sequence.video_tracks[0].add_clip(solid).expect("add solid clip");
    let display_track_index = video_display_index(&sequence, 0);

    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);

    assert_eq!(model.tracks[display_track_index].clips[0].label, "纯色层");
    assert_eq!(
        model.tracks[display_track_index].clips[0].color.map(|c| c.to_rgba8()),
        Some(color.to_rgba8())
    );
}

#[test]
fn timeline_clip_fallback_colors_use_theme_tokens() {
    let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
    mondrian_ui_theme::set_theme_preset(mondrian_ui_theme::ThemePreset::Light);
    let mut sequence = Sequence::new("edit");
    let tb = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip"))
        .expect("add video clip");
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_adjustment_layer(AssetId::new(), tt(40, tb), tt(30, tb)).expect("valid clip"),
        )
        .expect("add adjustment clip");
    sequence.audio_tracks[0]
        .add_clip(Clip::new(AssetId::new(), tt(0, tb), tt(30, tb)).expect("valid clip"))
        .expect("add audio clip");

    let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);
    let colors = current_theme().colors.clone();
    let video_track = video_display_index(&sequence, 0);
    let first_audio_track = sequence.video_tracks.len();

    assert_eq!(
        model.tracks[video_track].clips[0].color,
        Some(colors.timeline_clip_video)
    );
    assert_eq!(
        model.tracks[video_track].clips[1].color,
        Some(colors.timeline_clip_adjustment)
    );
    assert_eq!(
        model.tracks[first_audio_track].clips[0].color,
        Some(colors.timeline_clip_audio)
    );
}

#[test]
fn panel_domain_accents_use_theme_tokens() {
    let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
    mondrian_ui_theme::set_theme_preset(mondrian_ui_theme::ThemePreset::Light);
    let colors = current_theme().colors.clone();

    assert_eq!(asset_kind_accent(&AssetKind::Video), colors.media_video);
    assert_eq!(asset_kind_accent(&AssetKind::Audio), colors.media_audio);
    assert_eq!(
        asset_kind_accent(&AssetKind::AdjustmentLayer),
        colors.media_adjustment
    );
    assert_eq!(
        asset_kind_accent(&AssetKind::SolidColor),
        colors.media_solid
    );
    assert_eq!(
        effect_node_accent(&EffectType::Plugin("demo.plugin".to_owned())),
        colors.effect_plugin
    );
    assert_eq!(
        effect_node_accent(&EffectType::GaussianBlur),
        colors.effect_filter
    );
    assert_eq!(
        effect_node_accent(&EffectType::Sharpen),
        colors.effect_filter
    );
    assert_eq!(effect_node_accent(&EffectType::Lut3D), colors.effect_lut);
    assert_eq!(
        effect_node_accent(&EffectType::ChromaKey),
        colors.effect_key
    );
    assert_eq!(effect_node_accent(&EffectType::LumaKey), colors.effect_key);
    assert_eq!(
        effect_node_accent(&EffectType::Vignette),
        colors.effect_default
    );
}

#[test]
fn inspector_curve_edit_action_uses_stable_typed_payload_for_selected_clip() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let keyframe_id = KeyframeId::new();
    let property = AnimationParameterAddress {
        animation_track_id: mondrian_core::types::AnimationTrackId::new(),
        parameter_id: mondrian_core::ParameterId::new_static("mondrian.transform.opacity"),
    };
    let model = InspectorCurveModel {
        property: property.clone(),
        keys: vec![
            InspectorCurveKeyModel {
                keyframe_id: None,
                point: CurvePoint::new(0.0, 0.25),
            },
            InspectorCurveKeyModel {
                keyframe_id: Some(keyframe_id),
                point: CurvePoint::new(0.5, 0.5),
            },
            InspectorCurveKeyModel {
                keyframe_id: None,
                point: CurvePoint::new(1.0, 0.75),
            },
        ],
        display_points: Vec::new(),
    };
    let action = inspector_curve_edit_action(
        Some(selection),
        &model,
        CurveEdit::Move { index: 1, point: CurvePoint::new(0.6, 0.7) },
    );

    match action {
        Some(Action::Custom { namespace, name, payload }) => {
            assert_eq!(namespace, CLIP_NAMESPACE);
            assert_eq!(name, CLIP_EDIT_NUMERIC_CURVE);
            let payload: ClipEditNumericCurvePayload =
                serde_json::from_value(payload).expect("curve payload");
            assert_eq!(payload.clip_id, selection.clip_id);
            assert_eq!(payload.parameter, property);
            assert_eq!(
                payload.edit,
                ClipCurveEditPayload::Upsert {
                    keyframe_id: Some(keyframe_id),
                    point: ClipNormalizedCurvePointPayload {
                        time_ratio: f64::from(0.6_f32),
                        value_ratio: f64::from(0.7_f32),
                    },
                }
            );
        }
        other => panic!("expected inspector curve action, got {other:?}"),
    }

    let boundary_keyframe_id = KeyframeId::new();
    let boundary_model = InspectorCurveModel {
        property: property.clone(),
        keys: vec![
            InspectorCurveKeyModel {
                keyframe_id: Some(boundary_keyframe_id),
                point: CurvePoint::new(0.0, 0.25),
            },
            InspectorCurveKeyModel {
                keyframe_id: None,
                point: CurvePoint::new(1.0, 0.75),
            },
        ],
        display_points: Vec::new(),
    };
    let boundary_delete = inspector_curve_edit_action(
        Some(selection),
        &boundary_model,
        CurveEdit::Delete { index: 0 },
    );
    let Some(Action::Custom { payload, .. }) = boundary_delete else {
        panic!("expected boundary keyframe removal action");
    };
    let payload: ClipEditNumericCurvePayload =
        serde_json::from_value(payload).expect("boundary curve payload");
    assert_eq!(
        payload.edit,
        ClipCurveEditPayload::Remove { keyframe_id: boundary_keyframe_id }
    );
}

#[test]
fn inspector_audio_actions_keep_logical_selection_separate_from_asset_rebind() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: false,
        clip_id: ClipId::new(),
    };
    let edit_id = AudioComponentEditId::new();
    let component_id = AudioSourceComponentId::new();
    let source_action = inspector_audio_source_action(
        Some(selection),
        edit_id,
        AudioComponentSource::Media { component_id },
    );
    let Some(Action::Custom { namespace, name, payload }) = source_action else {
        panic!("expected typed Inspector audio source action");
    };
    assert_eq!(namespace, AUDIO_NAMESPACE);
    assert_eq!(name, AUDIO_EDIT_COMPONENT);
    let payload: mondrian_timeline::AudioComponentEditRequest =
        serde_json::from_value(payload).expect("audio source payload");
    assert_eq!(payload.address.track_id, selection.track_id);
    assert_eq!(payload.address.clip_id, selection.clip_id);
    assert_eq!(payload.address.edit_id, edit_id);
    assert_eq!(
        payload.mutation,
        AudioComponentMutation::SetSource {
            value: AudioComponentSource::Media { component_id }
        }
    );

    let field_action = audio_component_mutation_action(
        Some(selection),
        edit_id,
        AudioComponentMutation::SetVolumeDb { value: -3.5 },
    );
    let Some(Action::Custom { namespace, name, payload }) = field_action else {
        panic!("expected typed Inspector audio field action");
    };
    assert_eq!(namespace, AUDIO_NAMESPACE);
    assert_eq!(name, AUDIO_EDIT_COMPONENT);
    let payload: mondrian_timeline::AudioComponentEditRequest =
        serde_json::from_value(payload).expect("audio field payload");
    assert_eq!(payload.address.clip_id, selection.clip_id);
    assert_eq!(payload.address.edit_id, edit_id);
    assert_eq!(
        payload.mutation,
        AudioComponentMutation::SetVolumeDb { value: -3.5 }
    );

    let zero_fade_action = inspector_audio_fade_duration_action(
        Some(selection),
        edit_id,
        true,
        0.0,
        AudioFadeCurve::EqualPower,
    );
    let Some(Action::Custom { payload, .. }) = zero_fade_action else {
        panic!("expected typed zero-fade action");
    };
    let payload: mondrian_timeline::AudioComponentEditRequest =
        serde_json::from_value(payload).expect("zero-fade payload");
    assert_eq!(
        payload.mutation,
        AudioComponentMutation::SetFadeIn { value: None }
    );

    let asset_id = AssetId::new();
    let rebind_action = inspector_audio_rebind_action(asset_id, component_id, 7);
    let Action::Custom { namespace, name, payload } = rebind_action else {
        panic!("expected typed Asset audio rebind action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REBIND_AUDIO_COMPONENT);
    let payload: AssetsRebindAudioComponentPayload =
        serde_json::from_value(payload).expect("audio rebind payload");
    assert_eq!(payload.asset_id, asset_id);
    assert_eq!(payload.component_id, component_id);
    assert_eq!(payload.stream_index, 7);

    let refresh_action = inspector_audio_refresh_action(asset_id);
    let Action::Custom { namespace, name, payload } = refresh_action else {
        panic!("expected typed Asset audio refresh action");
    };
    assert_eq!(namespace, ASSET_NAMESPACE);
    assert_eq!(name, ASSET_REFRESH_AUDIO_COMPONENTS);
    let payload: AssetsRefreshAudioComponentsPayload =
        serde_json::from_value(payload).expect("audio refresh payload");
    assert_eq!(payload.asset_id, asset_id);
}

#[test]
fn inspector_actions_without_selection_produce_no_command() {
    assert_eq!(
        inspector_parameter_action(
            None,
            Some(test_parameter_address("mondrian.transform.opacity")),
            PropertyValue::Float(0.42),
        ),
        None
    );
    assert_eq!(inspector_bool_action(None, true), None);
    assert_eq!(
        inspector_audio_source_action(
            None,
            AudioComponentEditId::new(),
            AudioComponentSource::Media { component_id: AudioSourceComponentId::new() },
        ),
        None
    );
    assert_eq!(
        audio_component_mutation_action(
            None,
            AudioComponentEditId::new(),
            AudioComponentMutation::SetEnabled { value: false },
        ),
        None
    );
    assert_eq!(
        inspector_color_action(None, Color::from_rgba8(1, 2, 3, 4)),
        None
    );
    assert_eq!(
        inspector_parameter_action(
            None,
            Some(test_parameter_address("mondrian.transform.position")),
            PropertyValue::Vec2(glam::Vec2::new(12.0, 0.0)),
        ),
        None
    );
    assert_eq!(
        inspector_timing_action(
            None,
            TimelineTrimPayloadEdge::In,
            10.0,
            Rational::new(1, 25),
        ),
        None
    );
    assert_eq!(
        inspector_effect_enabled_action(None, EffectId::new(), false),
        None
    );
    assert_eq!(
        inspector_remove_effect_row_action(None, EffectId::new()),
        None
    );
    assert_eq!(
        inspector_reorder_effect_action(
            None,
            EffectId::new(),
            EffectRelativePlacement::Before(EffectId::new()),
        ),
        None
    );
    assert_eq!(
        inspector_effect_property_action(
            None,
            EffectId::new(),
            test_parameter_address("mondrian.test.color_tint"),
            PropertyValue::Color(Color::from_rgba8(1, 2, 3, 4)),
        ),
        None
    );
    let curve_model = InspectorCurveModel {
        property: AnimationParameterAddress {
            animation_track_id: mondrian_core::types::AnimationTrackId::new(),
            parameter_id: mondrian_core::ParameterId::new_static("mondrian.transform.opacity"),
        },
        keys: Vec::new(),
        display_points: Vec::new(),
    };
    assert_eq!(
        inspector_curve_edit_action(
            None,
            &curve_model,
            CurveEdit::Insert { index: 0, point: CurvePoint::new(0.0, 1.0) },
        ),
        None
    );
}

#[test]
fn inspector_panel_locked_target_controls_do_not_dispatch() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let model = InspectorPanelModel {
        selected_clip: Some(selection),
        empty_message: None,
        selected_effect_id: None,
        selected_mask_id: None,
        is_editable: false,
        edit_disabled_reason: Some("所选剪辑所在轨道已锁定".to_owned()),
        enabled: true,
        opacity: 100.0,
        tint: Color::from_rgba8(64, 128, 192, 255),
        shows_tint: true,
        position_x: 0.0,
        position_y: 0.0,
        scale_x_percent: 100.0,
        scale_y_percent: 100.0,
        anchor_x: 0.0,
        anchor_y: 0.0,
        rotation_degrees: 0.0,
        visual_parameters: None,
        in_frame: 0.0,
        out_frame: 30.0,
        max_frame: 60.0,
        timeline_time_base: Rational::new(1, 25),
        source_timing: None,
        tint_area_mode: ColorPickerAreaMode::Wheel,
        opacity_curve: None,
        audio_components: Vec::new(),
        audio_processor_racks: Vec::new(),
        clip_properties: Vec::new(),
        effects: Vec::new(),
        grade: InspectorGradeHierarchyModel::default(),
        masks: Vec::new(),
    };
    let mut panel = inspector_panel(&model);
    panel.layout(Rect::new(0.0, 0.0, 320.0, 220.0));

    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );
    let enabled_checkbox_point = Point::new(132.0, 94.0);

    let down = panel.event(
        &UiEvent::MouseDown {
            position: enabled_checkbox_point,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    let up = panel.event(
        &UiEvent::MouseUp {
            position: enabled_checkbox_point,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );

    assert_eq!(down, EventResult::Ignored);
    assert_eq!(up, EventResult::Ignored);
    assert!(actions.borrow().is_empty());
}

#[test]
fn inspector_projects_mask_identity_and_emits_closed_product_actions() {
    let mut sequence = Sequence::new("Inspector Mask");
    sequence.add_video_track();
    let track_id = sequence.video_tracks[0].id;
    let mut clip = Clip::new(
        AssetId::new(),
        TimelineTime::ZERO,
        TimelineTime::new(5, 1).expect("duration"),
    )
    .expect("Clip");
    let mask_id = clip.add_mask_component(mondrian_core::mask_data::MaskComponent::new(
        "Subject".to_owned(),
        mondrian_core::mask_data::MaskEvaluation::default(),
    ));
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add Clip");
    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence));
    state.select_clip_by_id(clip_id).expect("select Clip");

    let model = InspectorPanelModel::from_app_state(&state);
    assert_eq!(
        model.selected_clip.map(|selection| selection.track_id),
        Some(track_id)
    );
    assert_eq!(model.masks.len(), 1);
    assert_eq!(model.masks[0].mask_id, mask_id);
    assert_eq!(model.masks[0].properties.len(), 5);

    let action = inspector_mask_shape_animation_action(model.selected_clip, mask_id, true)
        .expect("Mask animation action");
    let decoded = crate::app::product_action::ProductAction::decode_external(&action)
        .expect("valid payload")
        .expect("recognized Mask action");
    assert_eq!(
        decoded,
        crate::app::product_action::ProductAction::VisualMask(
            crate::app::product_action::VisualMaskProductAction::SetShapeAnimationEnabled(
                VisualMaskSetShapeAnimationEnabledPayload { clip_id, mask_id, enabled: true },
            ),
        )
    );
}

#[test]
fn viewer_power_window_projection_round_trips_bezier_handles_without_loss() {
    let shape = MaskShape::Path {
        points: vec![
            BezierPoint {
                position: glam::Vec2::new(0.12, 0.24),
                control_in: glam::Vec2::new(-0.07, 0.03),
                control_out: glam::Vec2::new(0.11, -0.05),
            },
            BezierPoint {
                position: glam::Vec2::new(0.83, 0.31),
                control_in: glam::Vec2::new(-0.13, -0.09),
                control_out: glam::Vec2::new(0.04, 0.17),
            },
            BezierPoint {
                position: glam::Vec2::new(0.61, 0.86),
                control_in: glam::Vec2::new(0.08, -0.14),
                control_out: glam::Vec2::new(-0.16, 0.02),
            },
        ],
        closed: false,
    };

    assert_eq!(
        mask_shape_from_viewer(viewer_power_window_shape(&shape)),
        shape
    );
}

#[test]
fn viewer_power_window_fails_closed_for_playback_track_lock_and_mask_lock() {
    let mut state = AppState::new();
    let mut sequence = Sequence::new("Viewer Power Window");
    let tb = sequence.time_base();
    let mut clip = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(32, 64, 128, 255),
        tt(0, tb),
        tt(24, tb),
    )
    .expect("solid Clip");
    let mask_id = clip.add_mask_component(mondrian_core::mask_data::MaskComponent::new(
        "Power Window".to_owned(),
        mondrian_core::mask_data::MaskEvaluation {
            shape: default_bezier_power_window(),
            ..Default::default()
        },
    ));
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("add Clip");
    state.test_set_sequence(Some(sequence));
    state.select_mask_by_id(clip_id, mask_id).expect("select Power Window");

    let editable = AppUiPanelModels::from_app_state(&state)
        .viewer
        .power_window
        .expect("project selected Power Window");
    assert!(editable.overlay.editable);

    state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0].masks[0]
        .locked = true;
    assert!(
        !AppUiPanelModels::from_app_state(&state)
            .viewer
            .power_window
            .expect("locked Power Window")
            .overlay
            .editable
    );

    state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].clips[0].masks[0]
        .locked = false;
    state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
    assert!(
        !AppUiPanelModels::from_app_state(&state)
            .viewer
            .power_window
            .expect("track-locked Power Window")
            .overlay
            .editable
    );

    state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = false;
    state.play().expect("play synthetic Clip");
    assert!(
        !AppUiPanelModels::from_app_state(&state)
            .viewer
            .power_window
            .expect("playing Power Window")
            .overlay
            .editable
    );
}

#[test]
fn inspector_bezier_power_window_creation_emits_complete_closed_shape() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let action = inspector_add_mask_action(Some(selection), default_bezier_power_window())
        .expect("Bezier Power Window action");
    let decoded = ProductAction::decode_external(&action)
        .expect("valid Power Window payload")
        .expect("recognized Power Window action");
    let ProductAction::VisualMask(crate::app::product_action::VisualMaskProductAction::AddToClip(
        payload,
    )) = decoded
    else {
        panic!("expected add Power Window action");
    };
    let MaskShape::Path { points, closed } = payload.shape else {
        panic!("expected Bezier Power Window");
    };
    assert!(closed);
    assert_eq!(points.len(), 4);
    assert!(points.iter().all(|point| {
        point.control_in != glam::Vec2::ZERO && point.control_out != glam::Vec2::ZERO
    }));
}

#[test]
fn inspector_effect_section_header_selects_effect_for_graph_sync() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let effect_id = EffectId::new();
    let model = InspectorPanelModel {
        selected_clip: Some(selection),
        empty_message: None,
        selected_effect_id: None,
        selected_mask_id: None,
        is_editable: true,
        edit_disabled_reason: None,
        enabled: true,
        opacity: 100.0,
        tint: Color::from_rgba8(64, 128, 192, 255),
        shows_tint: true,
        position_x: 0.0,
        position_y: 0.0,
        scale_x_percent: 100.0,
        scale_y_percent: 100.0,
        anchor_x: 0.0,
        anchor_y: 0.0,
        rotation_degrees: 0.0,
        visual_parameters: None,
        in_frame: 0.0,
        out_frame: 30.0,
        max_frame: 60.0,
        timeline_time_base: Rational::new(1, 25),
        source_timing: None,
        tint_area_mode: ColorPickerAreaMode::Wheel,
        opacity_curve: None,
        audio_components: Vec::new(),
        audio_processor_racks: Vec::new(),
        clip_properties: Vec::new(),
        effects: vec![InspectorEffectModel {
            effect_id,
            label: "Gaussian Blur".to_owned(),
            enabled: true,
            properties: Vec::new(),
        }],
        grade: InspectorGradeHierarchyModel::default(),
        masks: Vec::new(),
    };
    let mut panel = inspector_panel(&model);
    panel.layout(Rect::new(0.0, 0.0, 340.0, 720.0));

    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    let selected_at = (0..1_440).find_map(|step| {
        let position = Point::new(24.0, step as f32 * 0.5);
        (panel.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ) == EventResult::Handled)
            .then_some(position)
    });

    assert!(
        selected_at.is_some(),
        "laid-out effect section must expose a selectable label gutter"
    );
    assert!(requests.repaint);
    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!(
            "expected inspector select effect action, got {:?}",
            recorded[0]
        );
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_SELECT);
    let payload: VisualEffectTargetPayload =
        serde_json::from_value(payload.clone()).expect("inspector select effect payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.effect_id, effect_id);
}

#[test]
fn inspector_reorder_effect_action_uses_typed_app_action() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let effect_id = EffectId::new();
    let anchor_id = EffectId::new();

    assert_eq!(
        inspector_reorder_effect_action(
            Some(selection),
            effect_id,
            EffectRelativePlacement::Before(anchor_id),
        ),
        Some(visual_effect_reorder_action(VisualEffectReorderPayload {
            clip_id: selection.clip_id,
            effect_id,
            placement: EffectRelativePlacement::Before(anchor_id),
        }))
    );
}

#[test]
fn node_graph_clip_action_uses_timeline_select_payload() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };

    match node_graph_clip_action(Some(selection)) {
        Some(Action::Custom { namespace, name, payload }) => {
            assert_eq!(namespace, TIMELINE_NAMESPACE);
            assert_eq!(name, TIMELINE_SELECT_CLIP);
            let payload: TimelineSelectClipPayload =
                serde_json::from_value(payload).expect("timeline select payload");
            assert_eq!(payload.clip_id, selection.clip_id);
            assert_eq!(payload.mode, TimelineClipSelectionModePayload::Replace);
        }
        other => panic!("expected timeline select action, got {other:?}"),
    }

    assert_eq!(node_graph_clip_action(None), None);
}

#[test]
fn node_graph_effect_node_action_selects_effect_scope() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let effect_id = EffectId::new();
    let targets = vec![
        NodeGraphNodeTarget {
            node_id: "source".to_owned(),
            target: NodeGraphTarget::Clip,
        },
        NodeGraphNodeTarget {
            node_id: "effect-node".to_owned(),
            target: NodeGraphTarget::Effect(effect_id),
        },
    ];

    match node_graph_node_action(Some(selection), &targets, "effect-node") {
        Some(Action::Custom { namespace, name, payload }) => {
            assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
            assert_eq!(name, VISUAL_EFFECT_SELECT);
            let payload: VisualEffectTargetPayload =
                serde_json::from_value(payload).expect("inspector select effect payload");
            assert_eq!(payload.clip_id, selection.clip_id);
            assert_eq!(payload.effect_id, effect_id);
        }
        other => panic!("expected inspector select effect action, got {other:?}"),
    }

    assert_eq!(
        node_graph_node_action(Some(selection), &targets, "missing-node"),
        None
    );
    assert_eq!(node_graph_node_action(None, &targets, "effect-node"), None);
}

#[test]
fn node_graph_panel_dispatches_clip_selection_from_keyboard() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: false,
        clip_id: ClipId::new(),
    };
    let model = NodeGraphPanelModel {
        title: "Node Graph".to_owned(),
        subtitle: "Audio / 0 effect(s)".to_owned(),
        selected_clip: Some(selection),
        nodes: vec![
            NodeGraphNode::new("source", "Source"),
            NodeGraphNode::new("output", "Output"),
        ],
        edges: vec![NodeGraphEdge::new("source", "output")],
        node_targets: vec![
            NodeGraphNodeTarget {
                node_id: "source".to_owned(),
                target: NodeGraphTarget::Clip,
            },
            NodeGraphNodeTarget {
                node_id: "output".to_owned(),
                target: NodeGraphTarget::Output,
            },
        ],
        selected_node_id: Some("source".to_owned()),
    };
    let mut panel = node_graph_panel(&model);
    panel.layout(Rect::new(0.0, 0.0, 420.0, 220.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        panel.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
        EventResult::Handled
    );
    assert_eq!(
        panel.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected timeline select action, got {:?}", recorded[0]);
    };
    assert_eq!(namespace, TIMELINE_NAMESPACE);
    assert_eq!(name, TIMELINE_SELECT_CLIP);
    let payload: TimelineSelectClipPayload =
        serde_json::from_value(payload.clone()).expect("timeline select payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.mode, TimelineClipSelectionModePayload::Replace);
}

#[test]
fn node_graph_panel_pointer_selection_focuses_keyboard_navigation() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let effect_id = EffectId::new();
    let model = NodeGraphPanelModel {
        title: "Node Graph".to_owned(),
        subtitle: "Video / 1 effect(s)".to_owned(),
        selected_clip: Some(selection),
        nodes: vec![
            NodeGraphNode::new("source", "Source"),
            NodeGraphNode::new("effect:grade", "Grade"),
            NodeGraphNode::new("output", "Output"),
        ],
        edges: vec![
            NodeGraphEdge::new("source", "effect:grade"),
            NodeGraphEdge::new("effect:grade", "output"),
        ],
        node_targets: vec![
            NodeGraphNodeTarget {
                node_id: "source".to_owned(),
                target: NodeGraphTarget::Clip,
            },
            NodeGraphNodeTarget {
                node_id: "effect:grade".to_owned(),
                target: NodeGraphTarget::Effect(effect_id),
            },
            NodeGraphNodeTarget {
                node_id: "output".to_owned(),
                target: NodeGraphTarget::Output,
            },
        ],
        selected_node_id: None,
    };
    let mut panel = node_graph_panel(&model);
    panel.layout(Rect::new(0.0, 0.0, 520.0, 240.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        panel.event(
            &UiEvent::MouseDown {
                position: Point::new(103.0, 120.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        panel.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 2);
    let Action::Custom { namespace, name, payload } = &recorded[1] else {
        panic!(
            "expected inspector select effect action, got {:?}",
            recorded[1]
        );
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_SELECT);
    let payload: VisualEffectTargetPayload =
        serde_json::from_value(payload.clone()).expect("inspector select effect payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.effect_id, effect_id);
}

#[test]
fn node_graph_panel_background_click_focuses_keyboard_navigation() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let effect_id = EffectId::new();
    let model = NodeGraphPanelModel {
        title: "Node Graph".to_owned(),
        subtitle: "Video / 1 effect(s)".to_owned(),
        selected_clip: Some(selection),
        nodes: vec![
            NodeGraphNode::new("source", "Source"),
            NodeGraphNode::new("effect:grade", "Grade"),
            NodeGraphNode::new("output", "Output"),
        ],
        edges: vec![
            NodeGraphEdge::new("source", "effect:grade"),
            NodeGraphEdge::new("effect:grade", "output"),
        ],
        node_targets: vec![
            NodeGraphNodeTarget {
                node_id: "source".to_owned(),
                target: NodeGraphTarget::Clip,
            },
            NodeGraphNodeTarget {
                node_id: "effect:grade".to_owned(),
                target: NodeGraphTarget::Effect(effect_id),
            },
            NodeGraphNodeTarget {
                node_id: "output".to_owned(),
                target: NodeGraphTarget::Output,
            },
        ],
        selected_node_id: None,
    };
    let mut panel = node_graph_panel(&model);
    panel.layout(Rect::new(0.0, 0.0, 520.0, 240.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        panel.event(
            &UiEvent::MouseDown {
                position: Point::new(500.0, 220.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert!(actions.borrow().is_empty());
    assert_eq!(
        panel.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 1);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected timeline select action, got {:?}", recorded[0]);
    };
    assert_eq!(namespace, TIMELINE_NAMESPACE);
    assert_eq!(name, TIMELINE_SELECT_CLIP);
    let payload: TimelineSelectClipPayload =
        serde_json::from_value(payload.clone()).expect("timeline select payload");
    assert_eq!(payload.clip_id, selection.clip_id);
    assert_eq!(payload.mode, TimelineClipSelectionModePayload::Replace);
}

#[test]
fn node_graph_panel_home_end_dispatches_edge_node_actions() {
    let selection = SelectedClipRef {
        track_id: TrackId::new(),
        is_video_track: true,
        clip_id: ClipId::new(),
    };
    let effect_id = EffectId::new();
    let model = NodeGraphPanelModel {
        title: "Node Graph".to_owned(),
        subtitle: "Video / 1 effect(s)".to_owned(),
        selected_clip: Some(selection),
        nodes: vec![
            NodeGraphNode::new("source", "Source"),
            NodeGraphNode::new("effect:grade", "Grade"),
        ],
        edges: vec![NodeGraphEdge::new("source", "effect:grade")],
        node_targets: vec![
            NodeGraphNodeTarget {
                node_id: "source".to_owned(),
                target: NodeGraphTarget::Clip,
            },
            NodeGraphNodeTarget {
                node_id: "effect:grade".to_owned(),
                target: NodeGraphTarget::Effect(effect_id),
            },
        ],
        selected_node_id: Some("effect:grade".to_owned()),
    };
    let mut panel = node_graph_panel(&model);
    panel.layout(Rect::new(0.0, 0.0, 520.0, 240.0));
    let actions = RefCell::new(Vec::<Action>::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut requests = EventRequests::default();
    let mut ctx = event_ctx(
        &mut focus,
        &mut shortcut,
        &mut tooltip,
        &mut requests,
        &dispatch,
    );

    assert_eq!(
        panel.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
        EventResult::Handled
    );
    assert_eq!(
        panel.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        panel.event(
            &UiEvent::KeyDown { key: KeyCode::End, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );

    let recorded = actions.borrow();
    assert_eq!(recorded.len(), 2);
    let Action::Custom { namespace, name, payload } = &recorded[0] else {
        panic!("expected timeline select action, got {:?}", recorded[0]);
    };
    assert_eq!(namespace, TIMELINE_NAMESPACE);
    assert_eq!(name, TIMELINE_SELECT_CLIP);
    let clip_payload: TimelineSelectClipPayload =
        serde_json::from_value(payload.clone()).expect("timeline select payload");
    assert_eq!(clip_payload.clip_id, selection.clip_id);

    let Action::Custom { namespace, name, payload } = &recorded[1] else {
        panic!(
            "expected inspector select effect action, got {:?}",
            recorded[1]
        );
    };
    assert_eq!(namespace, VISUAL_EFFECT_NAMESPACE);
    assert_eq!(name, VISUAL_EFFECT_SELECT);
    let effect_payload: VisualEffectTargetPayload =
        serde_json::from_value(payload.clone()).expect("inspector select effect payload");
    assert_eq!(effect_payload.effect_id, effect_id);
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("mondrian-{prefix}-{suffix}"))
}

fn test_video_media_info(path: &Path) -> mondrian_media::MediaInfo {
    let primaries = mondrian_media::VideoColorTag {
        code: 1,
        name: Some("bt709".to_owned()),
        specified: true,
    };
    let transfer = primaries.clone();
    let matrix = primaries.clone();
    let color_metadata = mondrian_media::VideoColorMetadata {
        primaries: primaries.clone(),
        transfer: transfer.clone(),
        matrix: matrix.clone(),
    };
    let duration = std::time::Duration::from_secs(10);
    mondrian_media::MediaInfo {
        duration,
        file_size: std::fs::metadata(path).expect("media fixture metadata").len(),
        container: "mov".to_owned(),
        video_streams: vec![mondrian_media::VideoStreamInfo {
            index: 0,
            codec: mondrian_media::info::VideoCodec::H264,
            duration: Some(duration),
            codec_profile: mondrian_media::VideoCodecProfile::Unknown,
            width: 1920,
            height: 1080,
            picture: Default::default(),
            frame_rate: Rational::FPS_24,
            frame_rate_proven: true,
            pixel_format: mondrian_media::info::PixelFormat::Yuv420p,
            pixel_format_proven: true,
            color_range: mondrian_media::DecodedVideoRange::Limited,
            color_interpretation: mondrian_media::DetectedColorInterpretation {
                candidate_color_space: Some(ColorSpace::Rec709),
                confidence: mondrian_media::VideoColorInterpretationConfidence::High,
                source: mondrian_media::VideoColorSpaceSource::Metadata,
                method: mondrian_media::VideoColorDetectionMethod::CicpTags,
                evidence: vec![
                    mondrian_media::VideoColorInterpretationEvidence::ExactCicpTags {
                        primaries,
                        transfer,
                        matrix,
                        detected_color_space: ColorSpace::Rec709,
                    },
                ],
                warnings: Vec::new(),
                user_overridable: true,
            },
            color_metadata: Some(color_metadata),
            color_metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
            camera_raw: None,
            bit_depth: 8,
            has_alpha: false,
            avg_bitrate: 10_000_000,
            total_frames: Some(240),
        }],
        audio_streams: Vec::new(),
        has_video: true,
        has_audio: false,
    }
}

fn commit_test_media_asset(
    library: &AssetLibrary,
    path: PathBuf,
    media_info: mondrian_media::MediaInfo,
) -> AssetId {
    let path = std::fs::canonicalize(path).expect("canonical panel test media fixture");
    let fingerprint = mondrian_media::MediaFileFingerprint::capture(&path);
    let candidate =
        AssetMediaProbeCandidate::new(path, fingerprint, media_info).expect("media candidate");
    library.commit_media_probe(candidate, None).expect("register Asset")
}

fn test_video_asset(path: PathBuf) -> AssetRecord {
    let library_root = path.parent().expect("media fixture parent").join("asset-library");
    let library = AssetLibrary::open(library_root).expect("fixture Asset Library");
    let media_info = test_video_media_info(&path);
    let asset_id = commit_test_media_asset(&library, path, media_info);
    library.get_asset(asset_id).expect("read fixture Asset").expect("fixture Asset")
}

fn assert_shell_action(action: Option<&Action>, name: &str) {
    match action {
        Some(Action::Custom { namespace, name: action_name, .. }) => {
            assert_eq!(namespace, APP_SHELL_NAMESPACE);
            assert_eq!(action_name, name);
        }
        other => panic!("expected app shell action, got {other:?}"),
    }
}

fn assert_assets_action(action: Option<&Action>, name: &str) {
    match action {
        Some(Action::Custom { namespace, name: action_name, .. }) => {
            assert_eq!(namespace, ASSET_NAMESPACE);
            assert_eq!(action_name, name);
        }
        other => panic!("expected assets action, got {other:?}"),
    }
}
