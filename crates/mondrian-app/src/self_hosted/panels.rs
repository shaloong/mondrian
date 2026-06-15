//! Panel adapters for the self-hosted UI shell.
//!
//! These adapters translate application-facing panel concepts into generic
//! `mondrian-ui-widgets` view models. Demo data is kept behind explicit model
//! factories so real `AppState` / `EditorState` adapters can replace it without
//! changing dock layout or widget construction.

use mondrian_assets::{AssetKind, AssetLibrary, AssetRecord};
use mondrian_core::automation::timecode_to_ticks;
use mondrian_core::effect_data::EffectType;
#[cfg(test)]
use mondrian_core::types::{AssetId, SequenceId};
use mondrian_core::types::{ClipId, EffectId, TimeCode, TrackId};
use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_effects::{effect_display_name, effect_library_types};
use mondrian_timeline::clip::{Clip, Transform2D};
use mondrian_timeline::sequence::Sequence;
use mondrian_timeline::track::Track;
use mondrian_ui_core::types::SplitDirection;
use mondrian_ui_core::Widget;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::dock_tab_bar::TabInfo;
use mondrian_ui_widgets::panel_slot::SlotKind;
use mondrian_ui_widgets::{
    Button, Checkbox, ColorPickerAreaMode, ColorPickerTrigger, CurveEditor, CurvePoint, DockPanel,
    FlexChild, FlexContainer, PanelList, PanelListItem, PropertyPanel, PropertyRow,
    PropertySection, ScrollView, Slider, TimelineClip, TimelineClipMove, TimelineClipRef,
    TimelineClipTrim, TimelineEditCommand, TimelineTrack, TimelineTrackControl, TimelineTrackRef,
    TimelineTrimEdge, TimelineView, ViewerSurface,
};

use crate::app::ui_actions::{
    app_shell_import_media_dialog_action, app_shell_new_project_dialog_action,
    app_shell_open_project_dialog_action, app_shell_save_project_as_dialog_action,
    assets_prepare_drag_action, effects_add_to_clip_action, inspector_remove_effect_action,
    inspector_set_clip_curve_action, inspector_set_clip_enabled_action,
    inspector_set_clip_opacity_action, inspector_set_clip_tint_action,
    inspector_set_clip_transform_field_action, inspector_set_effect_enabled_action,
    timeline_add_track_action, timeline_move_clip_action, timeline_seek_action,
    timeline_select_clip_action, timeline_set_track_control_action, timeline_trim_clip_action,
    AssetsPrepareDragPayload, EffectsAddToClipPayload, InspectorClipRefPayload,
    InspectorClipTransformField, InspectorCurvePointPayload, InspectorRemoveEffectPayload,
    InspectorSetClipCurvePayload, InspectorSetClipEnabledPayload, InspectorSetClipOpacityPayload,
    InspectorSetClipTintPayload, InspectorSetClipTransformFieldPayload,
    InspectorSetEffectEnabledPayload, TimelineAddTrackKind, TimelineAddTrackPayload,
    TimelineMoveClipPayload, TimelineSelectClipPayload, TimelineSetTrackControlPayload,
    TimelineTrackControlPayloadKind, TimelineTrimClipPayload, TimelineTrimPayloadEdge,
};
use crate::app::{AppState, SelectedClipRef};

/// Complete set of view models needed by the self-hosted panel shell.
#[derive(Debug, Clone)]
pub struct SelfHostedPanelModels {
    pub project: PanelListModel,
    pub assets: PanelListModel,
    pub effects: PanelListModel,
    pub console: PanelListModel,
    pub viewer: ViewerPanelModel,
    pub timeline: TimelinePanelModel,
    pub inspector: InspectorPanelModel,
}

impl SelfHostedPanelModels {
    /// Snapshot the current application state into self-hosted panel models.
    ///
    /// This is a read-only boundary: widgets receive generic view models and
    /// emit actions, while domain mutations stay in `AppState` handlers.
    pub fn from_app_state(state: &AppState) -> Self {
        Self {
            project: PanelListModel::from_project_status(state),
            assets: PanelListModel::from_asset_library(state.asset_library.as_deref()),
            effects: PanelListModel::from_effect_registry(state.primary_selected_clip()),
            console: PanelListModel::from_app_status(state),
            viewer: ViewerPanelModel::from_app_state(state),
            timeline: state
                .sequence
                .as_ref()
                .map(|sequence| {
                    TimelinePanelModel::from_sequence(
                        sequence,
                        state.selected_clips(),
                        state.selected_tracks(),
                    )
                    .with_playhead_frame(state.current_frame())
                })
                .unwrap_or_default(),
            inspector: InspectorPanelModel::from_app_state(state),
        }
    }

    /// Demo fixtures that keep rich browser panels while sourcing timeline and
    /// inspector state from an `AppState` snapshot.
    #[cfg(test)]
    pub fn demo_from_app_state(state: &AppState) -> Self {
        Self {
            project: PanelListModel::from_project_status(state),
            assets: demo_asset_model(),
            effects: PanelListModel::from_effect_registry(state.primary_selected_clip()),
            console: demo_console_model(),
            viewer: ViewerPanelModel::from_app_state(state),
            timeline: state
                .sequence
                .as_ref()
                .map(|sequence| {
                    TimelinePanelModel::from_sequence(
                        sequence,
                        state.selected_clips(),
                        state.selected_tracks(),
                    )
                    .with_playhead_frame(state.current_frame())
                })
                .unwrap_or_else(demo_timeline_model),
            inspector: InspectorPanelModel::from_app_state(state),
        }
    }

    /// Demo fixtures used by tests before the real editor state is wired into
    /// the self-hosted shell.
    #[cfg(test)]
    pub fn demo() -> Self {
        let state = demo_app_state();
        Self::demo_from_app_state(&state)
    }
}

/// Build a synthetic app state for self-hosted tests.
///
/// The generated timeline is intentionally real domain data so timeline widget
/// actions carry stable ids and can be dispatched through `AppState`.
#[cfg(test)]
pub fn demo_app_state() -> AppState {
    let mut state = AppState::new();
    let mut sequence = demo_sequence();
    sequence.playhead = TimeCode::new(76, sequence.time_base());

    if let Some(selection) = demo_selection(&sequence) {
        state.replace_clip_selection(vec![selection]);
    }
    state.sequence = Some(sequence);
    state.seek(76);
    state
}

/// List panel data independent from a concrete widget instance.
#[derive(Debug, Clone)]
pub struct PanelListModel {
    pub title: String,
    pub subtitle: String,
    pub items: Vec<PanelListItem>,
    #[cfg(test)]
    pub demo_activate_prefix: Option<String>,
}

impl PanelListModel {
    pub fn new(title: impl Into<String>, items: Vec<PanelListItem>) -> Self {
        Self {
            title: title.into(),
            subtitle: String::new(),
            items,
            #[cfg(test)]
            demo_activate_prefix: None,
        }
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Attach a synthetic activation prefix for developer fixtures.
    ///
    /// Product panel models should assign stable explicit actions to each item
    /// instead of deriving commands from list labels.
    #[cfg(test)]
    pub fn with_demo_activate_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.demo_activate_prefix = Some(prefix.into());
        self
    }

    /// Build the project status panel for the product shell.
    pub fn from_project_status(state: &AppState) -> Self {
        let mut items = Vec::new();
        if let Some(path) = &state.current_project_path {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("Project");
            items.push(
                PanelListItem::new(name)
                    .with_subtitle(path.display().to_string())
                    .with_badge("Open"),
            );
        } else if state.sequence.is_some() {
            items.push(
                PanelListItem::new("Unsaved project")
                    .with_subtitle("Save the current edit to create a project file")
                    .with_badge("Draft"),
            );
        } else {
            items.push(
                PanelListItem::new("No project")
                    .with_subtitle("Open or create a project to start editing")
                    .with_badge("Idle")
                    .disabled(true),
            );
        }

        if let Some(sequence) = &state.sequence {
            let track_count = sequence.video_tracks.len() + sequence.audio_tracks.len();
            let duration = sequence.total_duration().frame.max(0);
            let resolution = sequence.settings.resolution;
            items.push(
                PanelListItem::new(sequence.name.clone())
                    .with_subtitle(format!("{track_count} tracks, {duration} frames"))
                    .with_badge(format!("{}x{}", resolution.width, resolution.height)),
            );
        } else {
            items.push(
                PanelListItem::new("No sequence")
                    .with_subtitle("Project timeline is not loaded")
                    .with_badge("SEQ")
                    .disabled(true),
            );
        }

        if let Some((message, is_error)) = &state.status_hint {
            let mut item = PanelListItem::new(if *is_error { "Error" } else { "Status" })
                .with_subtitle(message.clone())
                .with_badge(if *is_error { "!" } else { "OK" });
            if *is_error {
                item = item.with_accent(Color::from_hex(0xB91C1C));
            }
            items.push(item);
        } else {
            items.push(
                PanelListItem::new("Status")
                    .with_subtitle("Ready")
                    .with_badge("OK")
                    .disabled(true),
            );
        }

        items.push(
            PanelListItem::new("Asset library")
                .with_subtitle(if state.asset_library.is_some() {
                    "SQLite library connected"
                } else {
                    "Asset library disconnected"
                })
                .with_badge(if state.asset_library.is_some() {
                    "DB"
                } else {
                    "Off"
                })
                .disabled(state.asset_library.is_none()),
        );

        let has_sequence = state.sequence.is_some();
        let has_project_path = state.current_project_path.is_some();
        items.push(
            PanelListItem::new("New project...")
                .with_subtitle("Create a project file and initialize timeline settings")
                .with_badge("New")
                .with_activate_action(app_shell_new_project_dialog_action()),
        );
        items.push(
            PanelListItem::new("Open project...")
                .with_subtitle("Choose an .mdp project file")
                .with_badge("Open")
                .with_activate_action(app_shell_open_project_dialog_action()),
        );
        items.push(
            PanelListItem::new("Import media...")
                .with_subtitle("Add video or audio files to the project library")
                .with_badge("Import")
                .with_activate_action(app_shell_import_media_dialog_action())
                .disabled(state.asset_library.is_none()),
        );
        items.push(
            PanelListItem::new("Save project")
                .with_subtitle(if has_project_path {
                    "Write changes to the current project file"
                } else {
                    "Save As is required before this project has a file path"
                })
                .with_badge("Save")
                .with_activate_action(Action::SaveProject)
                .disabled(!has_sequence || !has_project_path),
        );
        items.push(
            PanelListItem::new("Save project as...")
                .with_subtitle("Choose a project file path")
                .with_badge("As")
                .with_activate_action(app_shell_save_project_as_dialog_action())
                .disabled(!has_sequence),
        );

        PanelListModel::new("Project", items).with_subtitle("Project state")
    }

    /// Build the project asset list. Database read failures are represented as
    /// disabled rows so the panel can render without owning app error handling.
    pub fn from_asset_library(library: Option<&AssetLibrary>) -> Self {
        let Some(library) = library else {
            return PanelListModel::new(
                "Assets",
                vec![PanelListItem::new("No project library")
                    .with_subtitle("Open or create a project to browse assets")
                    .disabled(true)],
            )
            .with_subtitle("Project library");
        };

        match library.list_assets() {
            Ok(assets) if assets.is_empty() => PanelListModel::new(
                "Assets",
                vec![PanelListItem::new("No assets")
                    .with_subtitle("Import media or create generated assets")
                    .disabled(true)],
            )
            .with_subtitle("Project library"),
            Ok(assets) => PanelListModel::new(
                "Assets",
                assets.into_iter().map(panel_item_from_asset).collect(),
            )
            .with_subtitle("Project library"),
            Err(err) => PanelListModel::new(
                "Assets",
                vec![PanelListItem::new("Asset library unavailable")
                    .with_subtitle(err.to_string())
                    .disabled(true)],
            )
            .with_subtitle("Project library"),
        }
    }

    /// Build the visible effect browser from the shared effect registry.
    pub fn from_effect_registry(selected_clip: Option<SelectedClipRef>) -> Self {
        let effect_target = selected_clip.filter(|selection| selection.is_video_track);
        let effects = effect_library_types();
        let items = if effects.is_empty() {
            vec![PanelListItem::new("No effects available")
                .with_subtitle("Effect registry is empty")
                .disabled(true)]
        } else {
            effects
                .into_iter()
                .map(|effect_type| {
                    let name = effect_display_name(&effect_type);
                    let category = effect_type.category_path().join(" / ");
                    let mut item = PanelListItem::new(name)
                        .with_subtitle(category)
                        .with_badge(effect_badge(&effect_type));
                    if let Some(selection) = effect_target {
                        item = item.with_activate_action(effects_add_to_clip_action(
                            EffectsAddToClipPayload {
                                clip: inspector_clip_payload(selection),
                                effect_type,
                            },
                        ));
                    }
                    item
                })
                .collect()
        };

        PanelListModel::new("Effects", items).with_subtitle("Effect browser")
    }

    /// Summarize app runtime status for the console panel until the real log
    /// buffer is wired into self-hosted panels.
    pub fn from_app_status(state: &AppState) -> Self {
        let mut items = Vec::new();
        if let Some((message, is_error)) = &state.status_hint {
            let mut item = PanelListItem::new(if *is_error { "Error" } else { "Status" })
                .with_subtitle(message.clone());
            if *is_error {
                item = item.with_badge("!");
            }
            items.push(item);
        }

        let sequence_label = state
            .sequence
            .as_ref()
            .map(|sequence| sequence.name.clone())
            .unwrap_or_else(|| "No sequence".to_string());
        items.push(
            PanelListItem::new("Sequence")
                .with_subtitle(sequence_label)
                .with_badge(format!("F{}", state.current_frame().max(0))),
        );
        items.push(
            PanelListItem::new("Timeline")
                .with_subtitle(format!("End frame {}", state.last_content_frame().max(0))),
        );
        items.push(
            PanelListItem::new("Assets").with_subtitle(if state.asset_library.is_some() {
                "Library connected"
            } else {
                "Library disconnected"
            }),
        );

        PanelListModel::new("Console", items).with_subtitle("Runtime messages")
    }

    /// Build an explicit disabled empty state for panel kinds not migrated into
    /// the self-hosted product shell yet.
    pub fn unsupported_panel(kind: SlotKind) -> Self {
        PanelListModel::new(
            kind.display_name(),
            vec![PanelListItem::new("Panel not available")
                .with_subtitle(format!(
                    "{} is not mapped to the self-hosted UI yet",
                    kind.display_name()
                ))
                .with_badge("Pending")
                .disabled(true)],
        )
        .with_subtitle("Self-hosted panel")
    }
}

/// Viewer panel data independent from preview texture plumbing.
#[derive(Debug, Clone)]
pub struct ViewerPanelModel {
    pub title: String,
    pub status: String,
    pub resolution_label: String,
    pub frame_label: String,
    pub duration_label: String,
    pub width: u32,
    pub height: u32,
    pub playing: bool,
    pub enabled: bool,
}

impl ViewerPanelModel {
    /// Snapshot viewer chrome data from app state.
    pub fn from_app_state(state: &AppState) -> Self {
        let Some(sequence) = state.sequence.as_ref() else {
            return Self::empty();
        };
        let resolution = sequence.settings.resolution;
        let current_frame = state.current_frame().max(0);
        let duration_frame = sequence.total_duration().frame.max(0);
        let fps = sequence.settings.frame_rate.to_f64();
        Self {
            title: sequence.name.clone(),
            status: if state.is_playing() {
                "Playing".into()
            } else {
                "Ready".into()
            },
            resolution_label: format!(
                "{}x{} @ {:.2} fps",
                resolution.width, resolution.height, fps
            ),
            frame_label: format!("F{current_frame}"),
            duration_label: format!("{duration_frame} frames"),
            width: resolution.width,
            height: resolution.height,
            playing: state.is_playing(),
            enabled: true,
        }
    }

    /// Empty viewer shown before a sequence is open.
    pub fn empty() -> Self {
        Self {
            title: "Viewer".into(),
            status: "No sequence".into(),
            resolution_label: "No signal".into(),
            frame_label: "F0".into(),
            duration_label: String::new(),
            width: 16,
            height: 9,
            playing: false,
            enabled: false,
        }
    }
}

/// Timeline panel data in frame space.
#[derive(Debug, Clone, Default)]
pub struct TimelinePanelModel {
    pub tracks: Vec<TimelineTrack>,
    pub playhead_frame: i64,
    pub in_point_frame: i64,
    pub out_point_frame: Option<i64>,
    track_refs: Vec<AppTimelineTrackRef>,
    clip_refs: Vec<Vec<ClipId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppTimelineTrackRef {
    track_id: TrackId,
    is_video_track: bool,
}

impl TimelinePanelModel {
    /// Map the current timeline sequence into self-hosted timeline view models.
    ///
    /// The widget layer stays index-based and domain-light; this adapter is the
    /// app-side boundary that carries stable track/clip ids into emitted
    /// actions.
    pub fn from_sequence(
        sequence: &Sequence,
        selected_clips: &[SelectedClipRef],
        selected_tracks: &[TrackId],
    ) -> Self {
        let video_tracks = sequence.video_tracks.iter().map(|track| {
            (
                AppTimelineTrackRef { track_id: track.id, is_video_track: true },
                track.clips.iter().map(|clip| clip.id).collect::<Vec<_>>(),
                timeline_track_from_sequence_track(track, true, selected_clips, selected_tracks),
            )
        });
        let audio_tracks = sequence.audio_tracks.iter().map(|track| {
            (
                AppTimelineTrackRef { track_id: track.id, is_video_track: false },
                track.clips.iter().map(|clip| clip.id).collect::<Vec<_>>(),
                timeline_track_from_sequence_track(track, false, selected_clips, selected_tracks),
            )
        });

        let mut tracks = Vec::new();
        let mut track_refs = Vec::new();
        let mut clip_refs = Vec::new();
        for (track_ref, clip_ids, track) in video_tracks.chain(audio_tracks) {
            track_refs.push(track_ref);
            clip_refs.push(clip_ids);
            tracks.push(track);
        }

        Self {
            tracks,
            playhead_frame: sequence.playhead.frame.max(0),
            in_point_frame: sequence.in_point_frame(),
            out_point_frame: sequence.out_point_frame(),
            track_refs,
            clip_refs,
        }
    }

    /// Override the playhead frame when the app playback state is newer than
    /// the serialized sequence playhead.
    pub fn with_playhead_frame(mut self, frame: i64) -> Self {
        self.playhead_frame = frame.max(0);
        self
    }

    fn clip_identity(&self, clip_ref: TimelineClipRef) -> Option<TimelineSelectClipPayload> {
        let track = *self.track_refs.get(clip_ref.track_index)?;
        let clip_id = *self.clip_refs.get(clip_ref.track_index)?.get(clip_ref.clip_index)?;
        Some(TimelineSelectClipPayload {
            track_id: track.track_id,
            is_video_track: track.is_video_track,
            clip_id,
        })
    }

    fn track_identity(&self, track_ref: TimelineTrackRef) -> Option<AppTimelineTrackRef> {
        self.track_refs.get(track_ref.track_index).copied()
    }

    fn track_control_payload(
        &self,
        control: TimelineTrackControl,
        track_ref: TimelineTrackRef,
        track: &TimelineTrack,
    ) -> Option<TimelineSetTrackControlPayload> {
        let identity = self.track_identity(track_ref)?;
        let (control, enabled) = match control {
            TimelineTrackControl::Visibility => {
                (TimelineTrackControlPayloadKind::Visibility, !track.visible)
            }
            TimelineTrackControl::Mute => (TimelineTrackControlPayloadKind::Mute, !track.muted),
            TimelineTrackControl::Lock => (TimelineTrackControlPayloadKind::Lock, !track.locked),
        };
        Some(TimelineSetTrackControlPayload {
            track_id: identity.track_id,
            is_video_track: identity.is_video_track,
            control,
            enabled,
        })
    }

    fn move_payload(&self, movement: TimelineClipMove) -> Option<TimelineMoveClipPayload> {
        let clip = self.clip_identity(movement.clip_ref)?;
        let target = *self.track_refs.get(movement.new_track_index)?;
        Some(TimelineMoveClipPayload {
            target_track_id: target.track_id,
            is_video_track: target.is_video_track,
            clip_id: clip.clip_id,
            frame: movement.new_start_frame.max(0),
        })
    }

    fn trim_payload(&self, trim: TimelineClipTrim) -> Option<TimelineTrimClipPayload> {
        let clip = self.clip_identity(trim.clip_ref)?;
        let edge = match trim.edge {
            TimelineTrimEdge::In => TimelineTrimPayloadEdge::In,
            TimelineTrimEdge::Out => TimelineTrimPayloadEdge::Out,
        };
        let frame = match trim.edge {
            TimelineTrimEdge::In => trim.new_start_frame,
            TimelineTrimEdge::Out => trim.new_start_frame + trim.new_duration_frames,
        };
        Some(TimelineTrimClipPayload { clip_id: clip.clip_id, edge, frame: frame.max(0) })
    }
}

/// Inspector fixture data independent from a concrete property widget tree.
#[derive(Debug, Clone)]
pub struct InspectorPanelModel {
    /// Selected clip targeted by value edits, if the model is backed by app state.
    pub selected_clip: Option<SelectedClipRef>,
    /// Whether the selected clip is enabled.
    pub enabled: bool,
    /// Opacity shown in UI percent units.
    pub opacity: f32,
    /// Solid/tint color shown by the color trigger.
    pub tint: Color,
    /// Horizontal transform position in sequence pixels.
    pub position_x: f32,
    /// Vertical transform position in sequence pixels.
    pub position_y: f32,
    /// Uniform transform scale shown in UI percent units.
    pub scale_percent: f32,
    /// Transform rotation shown in degrees.
    pub rotation_degrees: f32,
    /// Clip in point shown as an absolute timeline frame.
    pub in_frame: f32,
    /// Clip out point shown as an absolute timeline frame.
    pub out_frame: f32,
    /// Maximum timeline frame used by timing sliders.
    pub max_frame: f32,
    /// Preferred color-picker area style for this inspector instance.
    pub tint_area_mode: ColorPickerAreaMode,
    /// Opacity animation curve points normalized over the selected clip span.
    pub curve_points: Vec<CurvePoint>,
    /// Effects currently attached to the selected clip.
    pub effects: Vec<InspectorEffectModel>,
}

/// Effect row data shown by the self-hosted inspector.
#[derive(Debug, Clone)]
pub struct InspectorEffectModel {
    /// Effect instance id targeted by enable/disable actions.
    pub effect_id: EffectId,
    /// Human-readable effect name.
    pub label: String,
    /// Whether the effect is enabled.
    pub enabled: bool,
}

impl InspectorPanelModel {
    pub fn from_app_state(state: &AppState) -> Self {
        let Some(sequence) = state.sequence.as_ref() else {
            return Self::empty();
        };
        let Some(selection) = state.primary_selected_clip() else {
            return Self::empty();
        };
        let Some((resolved_selection, clip)) = clip_for_selection(sequence, &selection) else {
            return Self::empty();
        };

        let time = state.current_time_code().unwrap_or(sequence.playhead);
        let opacity = (clip.transform.evaluate_opacity(time) * 100.0).clamp(0.0, 100.0);
        let position = clip.transform.get_position(time);
        let scale = clip.transform.get_scale(time);
        Self {
            selected_clip: Some(resolved_selection),
            enabled: !clip.is_disabled,
            opacity,
            tint: clip
                .solid_color
                .or_else(|| timeline_clip_color(clip, resolved_selection.is_video_track))
                .unwrap_or_else(|| Color::from_hex(0x84B4FF)),
            position_x: position.x,
            position_y: position.y,
            scale_percent: scale.x * 100.0,
            rotation_degrees: clip_rotation_degrees(clip, time),
            in_frame: clip.position.frame as f32,
            out_frame: clip.end_position().frame as f32,
            max_frame: sequence.total_duration().frame.max(1) as f32,
            tint_area_mode: ColorPickerAreaMode::Wheel,
            curve_points: opacity_curve_points_for_clip(clip, time),
            effects: clip
                .effects
                .iter()
                .map(|effect| InspectorEffectModel {
                    effect_id: effect.id,
                    label: effect_display_name(&effect.effect_type),
                    enabled: effect.is_enabled,
                })
                .collect(),
        }
    }

    pub fn empty() -> Self {
        Self {
            selected_clip: None,
            enabled: false,
            opacity: 100.0,
            tint: Color::from_rgba8(128, 128, 128, 255),
            position_x: 0.0,
            position_y: 0.0,
            scale_percent: 100.0,
            rotation_degrees: 0.0,
            in_frame: 0.0,
            out_frame: 1.0,
            max_frame: 1.0,
            tint_area_mode: ColorPickerAreaMode::Wheel,
            curve_points: vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)],
            effects: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn demo() -> Self {
        Self {
            selected_clip: None,
            enabled: true,
            opacity: 72.0,
            tint: Color::from_rgba8(132, 180, 255, 220),
            position_x: 12.0,
            position_y: -8.0,
            scale_percent: 100.0,
            rotation_degrees: 0.0,
            in_frame: 0.0,
            out_frame: 96.0,
            max_frame: 240.0,
            tint_area_mode: ColorPickerAreaMode::Wheel,
            curve_points: vec![
                CurvePoint::new(0.0, 0.0),
                CurvePoint::new(0.35, 0.68),
                CurvePoint::new(0.72, 0.42),
                CurvePoint::new(1.0, 1.0),
            ],
            effects: Vec::new(),
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
#[cfg(test)]
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

    if kind == SlotKind::Assets {
        return Box::new(DockPanel::new(
            kind,
            asset_browser_tabs(),
            move |_kind, active| {
                let active_kind = if active == 1 {
                    SlotKind::Effects
                } else {
                    SlotKind::Assets
                };
                panel_content_for_slot(active_kind, &models)
            },
        ));
    }

    if kind == SlotKind::Console {
        return Box::new(DockPanel::new(
            kind,
            project_console_tabs(),
            move |_kind, active| {
                let active_kind = if active == 0 {
                    SlotKind::Project
                } else {
                    SlotKind::Console
                };
                panel_content_for_slot(active_kind, &models)
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

fn asset_browser_tabs() -> Vec<TabInfo> {
    vec![
        TabInfo { label: "Assets".into(), active: true },
        TabInfo { label: "Effects".into(), active: false },
    ]
}

fn project_console_tabs() -> Vec<TabInfo> {
    vec![
        TabInfo { label: "Project".into(), active: true },
        TabInfo { label: "Console".into(), active: false },
    ]
}

fn panel_content_for_slot(kind: SlotKind, models: &SelfHostedPanelModels) -> Box<dyn Widget> {
    match kind {
        SlotKind::Assets => Box::new(panel_list(&models.assets)),
        SlotKind::Effects => Box::new(panel_list(&models.effects)),
        SlotKind::Console => Box::new(panel_list(&models.console)),
        SlotKind::Viewer => Box::new(viewer_panel(&models.viewer)),
        SlotKind::Timeline => Box::new(timeline_panel(&models.timeline)),
        SlotKind::Project => Box::new(panel_list(&models.project)),
        SlotKind::Inspector => Box::new(ScrollView::new(Some(Box::new(inspector_panel(
            &models.inspector,
        ))))),
        SlotKind::NodeGraph | SlotKind::Export => {
            Box::new(panel_list(&PanelListModel::unsupported_panel(kind)))
        }
    }
}

fn viewer_panel(model: &ViewerPanelModel) -> ViewerSurface {
    ViewerSurface::new(model.title.clone(), model.width, model.height)
        .with_status(model.status.clone())
        .with_resolution_label(model.resolution_label.clone())
        .with_frame_label(model.frame_label.clone())
        .with_duration_label(model.duration_label.clone())
        .playing(model.playing)
        .enabled(model.enabled)
}

fn timeline_track_from_sequence_track(
    track: &Track,
    is_video_track: bool,
    selected_clips: &[SelectedClipRef],
    selected_tracks: &[TrackId],
) -> TimelineTrack {
    let muted = track.is_muted;
    let locked = track.is_locked;
    let visible = track.is_visible;
    let selected = selected_tracks.contains(&track.id);
    let clips = track
        .clips
        .iter()
        .map(|clip| timeline_clip_from_sequence_clip(is_video_track, clip, selected_clips))
        .collect();

    let track = if is_video_track {
        TimelineTrack::video(track.name.clone(), clips)
    } else {
        TimelineTrack::audio(track.name.clone(), clips)
    };
    track.selected(selected).visible(visible).muted(muted).locked(locked)
}

fn timeline_clip_from_sequence_clip(
    is_video_track: bool,
    clip: &Clip,
    selected_clips: &[SelectedClipRef],
) -> TimelineClip {
    let selected = selected_clips.iter().any(|selection| selection.clip_id == clip.id);
    let label = clip.label.clone().unwrap_or_else(|| default_clip_label(clip));
    let mut view = TimelineClip::new(
        label,
        clip.position.frame.max(0),
        clip.duration.frame.max(1),
    )
    .selected(selected)
    .disabled(clip.is_disabled);
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

fn clip_rotation_degrees(clip: &Clip, time: TimeCode) -> f32 {
    clip.transform
        .to_property_bag()
        .evaluate(Transform2D::ROTATION_PATH, timecode_to_ticks(time))
        .and_then(|value| value.as_f32())
        .unwrap_or(0.0)
}

fn opacity_curve_points_for_clip(clip: &Clip, time: TimeCode) -> Vec<CurvePoint> {
    let bag = clip.transform.to_property_bag();
    let Some(opacity) = bag.property(Transform2D::OPACITY_PATH) else {
        return default_opacity_curve(clip.transform.evaluate_opacity(time));
    };
    let start_tick = timecode_to_ticks(clip.position);
    let end_tick = timecode_to_ticks(clip.end_position());
    let duration_ticks = (end_tick - start_tick).max(1);

    let mut points = vec![CurvePoint::new(
        0.0,
        clip.transform.evaluate_opacity(clip.position),
    )];
    points.extend(
        opacity.keyframe_times().into_iter().filter_map(|keyframe_time| {
            if keyframe_time <= start_tick || keyframe_time >= end_tick {
                return None;
            }
            let keyframe = opacity.keyframe_at(keyframe_time)?;
            let y = keyframe.value.as_f32()?.clamp(0.0, 1.0);
            let x = ((keyframe_time - start_tick) as f32 / duration_ticks as f32).clamp(0.0, 1.0);
            Some(CurvePoint::new(x, y))
        }),
    );
    points.push(CurvePoint::new(
        1.0,
        clip.transform.evaluate_opacity(clip.end_position()),
    ));
    points.sort_by(|a, b| a.x.total_cmp(&b.x));
    points.dedup_by(|a, b| (a.x - b.x).abs() < f32::EPSILON);

    if points.len() < 2 {
        default_opacity_curve(clip.transform.evaluate_opacity(time))
    } else {
        points
    }
}

fn default_opacity_curve(opacity: f32) -> Vec<CurvePoint> {
    let opacity = opacity.clamp(0.0, 1.0);
    vec![CurvePoint::new(0.0, opacity), CurvePoint::new(1.0, opacity)]
}

fn panel_item_from_asset(asset: AssetRecord) -> PanelListItem {
    let badge = asset_kind_badge(&asset.kind);
    let accent = asset_kind_accent(&asset.kind);
    let subtitle = asset.path.display().to_string();
    PanelListItem::new(asset.name)
        .with_subtitle(subtitle)
        .with_badge(badge)
        .with_accent(accent)
        .with_activate_action(assets_prepare_drag_action(AssetsPrepareDragPayload {
            asset_id: asset.id,
        }))
}

fn asset_kind_badge(kind: &AssetKind) -> &'static str {
    match kind {
        AssetKind::Video => "VID",
        AssetKind::Audio => "AUD",
        AssetKind::AdjustmentLayer => "ADJ",
        AssetKind::SolidColor => "CLR",
    }
}

fn asset_kind_accent(kind: &AssetKind) -> Color {
    match kind {
        AssetKind::Video => Color::from_hex(0x4B7BE5),
        AssetKind::Audio => Color::from_hex(0x1D587B),
        AssetKind::AdjustmentLayer => Color::from_hex(0x6D5DD3),
        AssetKind::SolidColor => Color::from_hex(0xD946EF),
    }
}

fn effect_badge(effect_type: &EffectType) -> &'static str {
    match effect_type {
        EffectType::Plugin(_) => "PLG",
        EffectType::GaussianBlur | EffectType::Sharpen => "GPU",
        EffectType::Lut3D => "3D",
        EffectType::ChromaKey | EffectType::LumaKey => "KEY",
        _ => "FX",
    }
}

fn clip_for_selection<'a>(
    sequence: &'a Sequence,
    selection: &SelectedClipRef,
) -> Option<(SelectedClipRef, &'a Clip)> {
    for track in &sequence.video_tracks {
        if let Some(clip) = track.clips.iter().find(|clip| clip.id == selection.clip_id) {
            return Some((
                SelectedClipRef {
                    track_id: track.id,
                    is_video_track: true,
                    clip_id: clip.id,
                },
                clip,
            ));
        }
    }

    for track in &sequence.audio_tracks {
        if let Some(clip) = track.clips.iter().find(|clip| clip.id == selection.clip_id) {
            return Some((
                SelectedClipRef {
                    track_id: track.id,
                    is_video_track: false,
                    clip_id: clip.id,
                },
                clip,
            ));
        }
    }

    None
}

fn panel_list(model: &PanelListModel) -> PanelList {
    let list = PanelList::new(model.title.clone(), model.items.clone())
        .with_subtitle(model.subtitle.clone());
    #[cfg(test)]
    if let Some(prefix) = model.demo_activate_prefix.clone() {
        return list.on_activate(move |index, item| {
            demo_panel_action(&format!("{prefix}.{index}.{}", item.title))
        });
    }
    list
}

#[cfg(test)]
fn demo_asset_model() -> PanelListModel {
    PanelListModel::new(
        "Assets",
        vec![
            PanelListItem::new("Footage")
                .with_subtitle("Imported camera clips")
                .with_badge("12")
                .with_select_action(demo_panel_action("assets.select.footage")),
            PanelListItem::new("Audio")
                .with_subtitle("Music, voiceover, and ambience")
                .with_badge("5")
                .with_select_action(demo_panel_action("assets.select.audio")),
            PanelListItem::new("Images")
                .with_subtitle("Still frames and references")
                .with_badge("8")
                .with_select_action(demo_panel_action("assets.select.images")),
            PanelListItem::new("Sequences")
                .with_subtitle("Nested edits and reusable timelines")
                .with_badge("2")
                .with_select_action(demo_panel_action("assets.select.sequences")),
        ],
    )
    .with_subtitle("Project library")
    .with_demo_activate_prefix("assets.activate")
}

#[cfg(test)]
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

#[cfg(test)]
fn demo_timeline_model() -> TimelinePanelModel {
    let sequence = demo_sequence();
    let selected = demo_selection(&sequence).into_iter().collect::<Vec<_>>();
    TimelinePanelModel::from_sequence(&sequence, &selected, &[]).with_playhead_frame(76)
}

#[cfg(test)]
fn demo_sequence() -> Sequence {
    let mut sequence = Sequence::new("Demo edit");
    while sequence.video_tracks.len() < 3 {
        sequence.add_video_track();
    }
    while sequence.audio_tracks.len() < 2 {
        sequence.add_audio_track();
    }
    sequence.video_tracks[0].name = "V3".to_string();
    sequence.video_tracks[1].name = "V2".to_string();
    sequence.video_tracks[2].name = "V1".to_string();
    sequence.audio_tracks[0].name = "A1".to_string();
    sequence.audio_tracks[1].name = "A2".to_string();

    let tb = sequence.time_base();
    let nested_id = SequenceId::new();

    let mut adjustment =
        Clip::new_adjustment_layer(AssetId::new(), TimeCode::new(36, tb), TimeCode::new(84, tb));
    adjustment.label = Some("Adjustment".to_string());
    sequence.video_tracks[0].add_clip(adjustment).expect("add adjustment");

    let mut title = Clip::new_nested_sequence(
        nested_id,
        TimeCode::new(132, tb),
        TimeCode::new(48, tb),
        Some("Title".to_string()),
    );
    title.solid_color = Some(Color::from_hex(0x4B7BE5));
    sequence.video_tracks[0].add_clip(title).expect("add title");

    let mut b_roll = Clip::new(AssetId::new(), TimeCode::new(18, tb), TimeCode::new(72, tb));
    b_roll.label = Some("B-roll".to_string());
    b_roll.solid_color = Some(Color::from_hex(0x2C7A7B));
    sequence.video_tracks[1].add_clip(b_roll).expect("add b-roll");

    let mut overlay = Clip::new_solid_color(
        AssetId::new(),
        Color::from_hex(0x805AD5),
        TimeCode::new(112, tb),
        TimeCode::new(56, tb),
    );
    overlay.label = Some("Overlay".to_string());
    sequence.video_tracks[1].add_clip(overlay).expect("add overlay");

    let mut interview = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(96, tb));
    interview.label = Some("Interview".to_string());
    sequence.video_tracks[2].add_clip(interview).expect("add interview");

    let mut cutaway = Clip::new(
        AssetId::new(),
        TimeCode::new(104, tb),
        TimeCode::new(72, tb),
    );
    cutaway.label = Some("Cutaway".to_string());
    cutaway.solid_color = Some(Color::from_hex(0x2F855A));
    sequence.video_tracks[2].add_clip(cutaway).expect("add cutaway");

    let mut outro = Clip::new(
        AssetId::new(),
        TimeCode::new(190, tb),
        TimeCode::new(44, tb),
    );
    outro.label = Some("Outro".to_string());
    outro.solid_color = Some(Color::from_hex(0x744210));
    sequence.video_tracks[2].add_clip(outro).expect("add outro");

    let mut dialogue = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(176, tb));
    dialogue.label = Some("Dialogue".to_string());
    sequence.audio_tracks[0].add_clip(dialogue).expect("add dialogue");

    let mut music = Clip::new(
        AssetId::new(),
        TimeCode::new(24, tb),
        TimeCode::new(210, tb),
    );
    music.label = Some("Music Bed".to_string());
    music.solid_color = Some(Color::from_hex(0x2B6CB0));
    sequence.audio_tracks[1].add_clip(music).expect("add music");

    sequence
}

#[cfg(test)]
fn demo_selection(sequence: &Sequence) -> Option<SelectedClipRef> {
    sequence.video_tracks.get(1).and_then(|track| {
        track.clips.get(1).map(|clip| SelectedClipRef {
            track_id: track.id,
            is_video_track: true,
            clip_id: clip.id,
        })
    })
}

fn timeline_panel(model: &TimelinePanelModel) -> TimelineView {
    let action_model = model.clone();
    TimelineView::new(model.tracks.clone())
        .enabled(!model.tracks.is_empty())
        .with_header_width(128.0)
        .with_playhead(model.playhead_frame)
        .with_in_out_points(model.in_point_frame, model.out_point_frame)
        .on_clip_select({
            let action_model = action_model.clone();
            move |clip_ref, _clip| {
                action_model
                    .clip_identity(clip_ref)
                    .map(timeline_select_clip_action)
                    .unwrap_or(Action::NoOp)
            }
        })
        .on_track_select({
            let action_model = action_model.clone();
            move |track_ref, _track| {
                action_model
                    .track_identity(track_ref)
                    .map(|track| {
                        Action::Select(mondrian_editor_state::action::SelectionTarget::Track(
                            track.track_id,
                        ))
                    })
                    .unwrap_or(Action::NoOp)
            }
        })
        .on_track_control({
            let action_model = action_model.clone();
            move |control, track_ref, track| {
                action_model
                    .track_control_payload(control, track_ref, track)
                    .map(timeline_set_track_control_action)
                    .unwrap_or(Action::NoOp)
            }
        })
        .on_track_add(|kind| {
            let kind = match kind {
                mondrian_ui_widgets::TimelineTrackKind::Video => TimelineAddTrackKind::Video,
                mondrian_ui_widgets::TimelineTrackKind::Audio => TimelineAddTrackKind::Audio,
            };
            timeline_add_track_action(TimelineAddTrackPayload { kind })
        })
        .on_edit_command(|command| match command {
            TimelineEditCommand::DeleteSelection => Action::DeleteSelection,
            TimelineEditCommand::RippleDeleteSelection => Action::RippleDeleteSelection,
            TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
            TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
            TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
        })
        .on_clip_move({
            let action_model = action_model.clone();
            move |movement, _clip| {
                action_model
                    .move_payload(movement)
                    .map(timeline_move_clip_action)
                    .unwrap_or(Action::NoOp)
            }
        })
        .on_clip_trim({
            let action_model = action_model.clone();
            move |trim, _clip| {
                action_model
                    .trim_payload(trim)
                    .map(timeline_trim_clip_action)
                    .unwrap_or_else(|| Action::NoOp)
            }
        })
        .on_seek(timeline_seek_action)
}

#[cfg(test)]
fn demo_panel_action(name: &str) -> Action {
    Action::Custom {
        namespace: "ui.demo_panel".into(),
        name: name.into(),
        payload: serde_json::Value::Null,
    }
}

fn inspector_panel(model: &InspectorPanelModel) -> PropertyPanel {
    let selected_clip = model.selected_clip;
    let has_target = selected_clip.is_some();
    let mut tint = ColorPickerTrigger::new(model.tint).enabled(has_target);
    tint.picker_mut().set_area_mode(model.tint_area_mode);
    let curve = CurveEditor::with_points(model.curve_points.clone())
        .enabled(has_target)
        .on_change(move |points| inspector_curve_action(selected_clip, points));
    let mut panel = PropertyPanel::new("Inspector").with_subtitle("Selected clip").with_section(
        PropertySection::new("Clip Style")
            .with_row(PropertyRow::new(
                "Enabled",
                Box::new(
                    Checkbox::new("启用效果", model.enabled)
                        .enabled(has_target)
                        .on_change(move |value| inspector_bool_action(selected_clip, value)),
                ),
            ))
            .with_row(PropertyRow::new(
                "Opacity",
                Box::new(
                    Slider::new(model.opacity, 0.0, 100.0).enabled(has_target).on_change(
                        move |value| inspector_value_action(selected_clip, "opacity", value),
                    ),
                ),
            ))
            .with_row(PropertyRow::new(
                "Tint",
                Box::new(tint.on_change(move |color| inspector_color_action(selected_clip, color))),
            )),
    );

    panel = panel.with_section(
        PropertySection::new("Transform")
            .with_row(PropertyRow::new(
                "Position X",
                Box::new(
                    Slider::new(model.position_x, -4096.0, 4096.0).enabled(has_target).on_change(
                        move |value| {
                            inspector_transform_action(
                                selected_clip,
                                InspectorClipTransformField::PositionX,
                                value,
                            )
                        },
                    ),
                ),
            ))
            .with_row(PropertyRow::new(
                "Position Y",
                Box::new(
                    Slider::new(model.position_y, -4096.0, 4096.0).enabled(has_target).on_change(
                        move |value| {
                            inspector_transform_action(
                                selected_clip,
                                InspectorClipTransformField::PositionY,
                                value,
                            )
                        },
                    ),
                ),
            ))
            .with_row(PropertyRow::new(
                "Scale",
                Box::new(
                    Slider::new(model.scale_percent, 0.0, 400.0).enabled(has_target).on_change(
                        move |value| {
                            inspector_transform_action(
                                selected_clip,
                                InspectorClipTransformField::ScalePercent,
                                value,
                            )
                        },
                    ),
                ),
            ))
            .with_row(PropertyRow::new(
                "Rotation",
                Box::new(
                    Slider::new(model.rotation_degrees, -180.0, 180.0)
                        .enabled(has_target)
                        .on_change(move |value| {
                            inspector_transform_action(
                                selected_clip,
                                InspectorClipTransformField::RotationDegrees,
                                value,
                            )
                        }),
                ),
            )),
    );

    panel = panel.with_section(
        PropertySection::new("Timing")
            .with_row(PropertyRow::new(
                "In",
                Box::new(
                    Slider::new(model.in_frame, 0.0, model.max_frame)
                        .enabled(has_target)
                        .on_change(move |value| {
                            inspector_timing_action(
                                selected_clip,
                                TimelineTrimPayloadEdge::In,
                                value,
                            )
                        }),
                ),
            ))
            .with_row(PropertyRow::new(
                "Out",
                Box::new(
                    Slider::new(model.out_frame, 0.0, model.max_frame)
                        .enabled(has_target)
                        .on_change(move |value| {
                            inspector_timing_action(
                                selected_clip,
                                TimelineTrimPayloadEdge::Out,
                                value,
                            )
                        }),
                ),
            )),
    );

    if !model.effects.is_empty() {
        let mut section = PropertySection::new("Effects");
        for effect in &model.effects {
            let effect_id = effect.effect_id;
            section = section.with_row(PropertyRow::new(
                effect.label.clone(),
                Box::new(
                    FlexContainer::row(vec![
                        FlexChild::flex(
                            Box::new(Checkbox::new("Enabled", effect.enabled).on_change(
                                move |enabled| {
                                    inspector_effect_enabled_action(
                                        selected_clip,
                                        effect_id,
                                        enabled,
                                    )
                                },
                            )),
                            1.0,
                        ),
                        FlexChild::fixed(Box::new(Button::new("Remove").on_click(
                            inspector_remove_effect_row_action(selected_clip, effect_id),
                        ))),
                    ])
                    .with_gap(8.0),
                ),
            ));
        }
        panel = panel.with_section(section);
    }

    panel.with_section(
        PropertySection::new("Animation")
            .with_row(PropertyRow::new("Curve", Box::new(curve)).with_height(118.0)),
    )
}

fn inspector_value_action(
    selection: Option<SelectedClipRef>,
    name: &'static str,
    value: f32,
) -> Action {
    if name == "opacity" {
        if let Some(selection) = selection {
            return inspector_set_clip_opacity_action(InspectorSetClipOpacityPayload {
                clip: inspector_clip_payload(selection),
                opacity_percent: value,
            });
        }
    }
    Action::NoOp
}

fn inspector_bool_action(selection: Option<SelectedClipRef>, value: bool) -> Action {
    if let Some(selection) = selection {
        return inspector_set_clip_enabled_action(InspectorSetClipEnabledPayload {
            clip: inspector_clip_payload(selection),
            enabled: value,
        });
    }
    Action::NoOp
}

fn inspector_color_action(selection: Option<SelectedClipRef>, color: Color) -> Action {
    if let Some(selection) = selection {
        return inspector_set_clip_tint_action(InspectorSetClipTintPayload {
            clip: inspector_clip_payload(selection),
            color,
        });
    }
    Action::NoOp
}

fn inspector_transform_action(
    selection: Option<SelectedClipRef>,
    field: InspectorClipTransformField,
    value: f32,
) -> Action {
    if let Some(selection) = selection {
        return inspector_set_clip_transform_field_action(InspectorSetClipTransformFieldPayload {
            clip: inspector_clip_payload(selection),
            field,
            value,
        });
    }
    Action::NoOp
}

fn inspector_timing_action(
    selection: Option<SelectedClipRef>,
    edge: TimelineTrimPayloadEdge,
    frame: f32,
) -> Action {
    let frame = if frame.is_finite() {
        frame.round() as i64
    } else {
        0
    };
    if let Some(selection) = selection {
        return timeline_trim_clip_action(TimelineTrimClipPayload {
            clip_id: selection.clip_id,
            edge,
            frame: frame.max(0),
        });
    }
    Action::NoOp
}

fn inspector_effect_enabled_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    enabled: bool,
) -> Action {
    if let Some(selection) = selection {
        return inspector_set_effect_enabled_action(InspectorSetEffectEnabledPayload {
            clip: inspector_clip_payload(selection),
            effect_id,
            enabled,
        });
    }
    Action::NoOp
}

fn inspector_remove_effect_row_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> Action {
    if let Some(selection) = selection {
        return inspector_remove_effect_action(InspectorRemoveEffectPayload {
            clip: inspector_clip_payload(selection),
            effect_id,
        });
    }
    Action::NoOp
}

fn inspector_curve_action(selection: Option<SelectedClipRef>, points: &[CurvePoint]) -> Action {
    if let Some(selection) = selection {
        let points = points
            .iter()
            .filter(|point| point.x.is_finite() && point.y.is_finite())
            .map(|point| InspectorCurvePointPayload {
                x: point.x.clamp(0.0, 1.0),
                y: point.y.clamp(0.0, 1.0),
            })
            .collect();
        return inspector_set_clip_curve_action(InspectorSetClipCurvePayload {
            clip: inspector_clip_payload(selection),
            points,
        });
    }
    Action::NoOp
}

fn inspector_clip_payload(selection: SelectedClipRef) -> InspectorClipRefPayload {
    InspectorClipRefPayload {
        track_id: selection.track_id,
        is_video_track: selection.is_video_track,
        clip_id: selection.clip_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_NEW_PROJECT_DIALOG,
        APP_SHELL_OPEN_PROJECT_DIALOG, APP_SHELL_SAVE_PROJECT_AS_DIALOG, ASSETS_NAMESPACE,
        ASSETS_PREPARE_DRAG, EFFECTS_ADD_TO_CLIP, EFFECTS_NAMESPACE, INSPECTOR_NAMESPACE,
        INSPECTOR_SET_CLIP_CURVE, TIMELINE_ADD_TRACK, TIMELINE_NAMESPACE,
    };
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::automation::{Keyframe, PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::types::{AssetId, TimeCode};
    use mondrian_effects::EffectNodeExt;
    use mondrian_ui_core::types::{
        EventResult, KeyCode, LayoutConstraint, Modifiers, MouseButton, Point, Size,
    };
    use mondrian_ui_core::widget::EventRequests;
    use mondrian_ui_core::UiEvent;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[test]
    fn demo_panel_models_cover_primary_editor_surfaces() {
        let models = SelfHostedPanelModels::demo();

        assert_eq!(models.project.title, "Project");
        assert!(models.project.items.iter().any(|item| item.title == "Unsaved project"));
        assert!(!models.assets.items.is_empty());
        assert!(!models.effects.items.is_empty());
        assert!(!models.console.items.is_empty());
        assert_eq!(models.viewer.title, "Demo edit");
        assert!(models.viewer.enabled);
        assert!(!models.timeline.tracks.is_empty());
        assert!(!models.inspector.curve_points.is_empty());
    }

    #[test]
    fn asset_browser_tabs_expose_effect_browser() {
        let tabs = asset_browser_tabs();

        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].label, "Assets");
        assert!(tabs[0].active);
        assert_eq!(tabs[1].label, "Effects");
    }

    #[test]
    fn project_console_tabs_expose_project_status_first() {
        let tabs = project_console_tabs();

        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].label, "Project");
        assert!(tabs[0].active);
        assert_eq!(tabs[1].label, "Console");
        assert!(!tabs[1].active);
    }

    #[test]
    fn app_state_models_are_safe_without_an_open_project() {
        let state = AppState::new();
        let models = SelfHostedPanelModels::from_app_state(&state);

        assert!(models.timeline.tracks.is_empty());
        assert_eq!(models.timeline.playhead_frame, 0);
        assert!(!timeline_panel(&models.timeline).can_focus());
        assert_eq!(models.project.items[0].title, "No project");
        assert!(models.project.items[0].disabled);
        assert_eq!(models.assets.items[0].title, "No project library");
        assert!(models.assets.items[0].disabled);
        assert!(!models.effects.items.is_empty());
        assert_eq!(models.console.title, "Console");
        assert!(!models.console.items.is_empty());
        assert_eq!(models.viewer.title, "Viewer");
        assert!(!models.viewer.enabled);
        assert_eq!(models.viewer.resolution_label, "No signal");
        assert_eq!(models.inspector.selected_clip, None);
        assert_eq!(models.inspector.opacity, 100.0);
        assert!(models.inspector.effects.is_empty());
    }

    #[test]
    fn unsupported_panel_model_is_explicit_disabled_empty_state() {
        let model = PanelListModel::unsupported_panel(SlotKind::NodeGraph);

        assert_eq!(model.title, SlotKind::NodeGraph.display_name());
        assert_eq!(model.subtitle, "Self-hosted panel");
        assert_eq!(model.items.len(), 1);
        assert_eq!(model.items[0].title, "Panel not available");
        assert!(model.items[0].disabled);
        assert_eq!(model.items[0].badge.as_deref(), Some("Pending"));
    }

    #[test]
    fn self_hosted_content_factory_covers_every_panel_kind() {
        let models = SelfHostedPanelModels::from_app_state(&AppState::new());
        let constraint = LayoutConstraint { min: Size::ZERO, max: Size::new(320.0, 240.0) };

        for kind in SlotKind::ALL {
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
    fn project_panel_model_reads_project_path_sequence_and_status() {
        let mut state = demo_app_state();
        state.current_project_path = Some(PathBuf::from("E:/projects/cut.mdp"));
        state.set_status_hint("Saved", false);

        let model = PanelListModel::from_project_status(&state);

        assert_eq!(model.title, "Project");
        assert_eq!(model.items[0].title, "cut.mdp");
        assert_eq!(model.items[0].badge.as_deref(), Some("Open"));
        assert!(model.items[0].select_action.is_none());
        assert!(model.items.iter().any(|item| item.title == "Demo edit"));
        assert!(model.items.iter().any(|item| item.subtitle == "Saved"));
    }

    #[test]
    fn project_panel_commands_share_app_shell_actions() {
        let mut state = demo_app_state();
        state.current_project_path = Some(PathBuf::from("E:/projects/cut.mdp"));

        let model = PanelListModel::from_project_status(&state);

        assert_shell_action(
            project_item(&model, "New project...").activate_action.as_ref(),
            APP_SHELL_NEW_PROJECT_DIALOG,
        );
        assert_shell_action(
            project_item(&model, "Open project...").activate_action.as_ref(),
            APP_SHELL_OPEN_PROJECT_DIALOG,
        );
        assert_shell_action(
            project_item(&model, "Import media...").activate_action.as_ref(),
            APP_SHELL_IMPORT_MEDIA_DIALOG,
        );
        assert_eq!(
            project_item(&model, "Save project").activate_action.as_ref(),
            Some(&Action::SaveProject)
        );
        assert!(!project_item(&model, "Save project").disabled);
        assert_shell_action(
            project_item(&model, "Save project as...").activate_action.as_ref(),
            APP_SHELL_SAVE_PROJECT_AS_DIALOG,
        );
        assert!(!project_item(&model, "Save project as...").disabled);
    }

    #[test]
    fn project_panel_commands_disable_file_mutations_without_sequence() {
        let state = AppState::new();
        let model = PanelListModel::from_project_status(&state);

        assert!(!project_item(&model, "New project...").disabled);
        assert!(!project_item(&model, "Open project...").disabled);
        assert!(project_item(&model, "Import media...").disabled);
        assert!(project_item(&model, "Save project").disabled);
        assert!(project_item(&model, "Save project as...").disabled);
    }

    #[test]
    fn demo_timeline_model_carries_stable_clip_identity() {
        let model = demo_timeline_model();
        let identity = model
            .clip_identity(TimelineClipRef { track_index: 1, clip_index: 1 })
            .expect("demo overlay clip identity");
        let movement = model
            .move_payload(TimelineClipMove {
                clip_ref: TimelineClipRef { track_index: 1, clip_index: 1 },
                old_start_frame: 112,
                new_start_frame: 120,
                new_track_index: 2,
            })
            .expect("demo move payload");

        assert!(identity.is_video_track);
        assert_eq!(movement.clip_id, identity.clip_id);
        assert_eq!(movement.frame, 120);
        assert_eq!(movement.target_track_id, model.track_refs[2].track_id);
    }

    #[test]
    fn timeline_model_rejects_stale_clip_refs() {
        let model = demo_timeline_model();
        let stale_ref = TimelineClipRef { track_index: usize::MAX, clip_index: 0 };

        assert!(model.clip_identity(stale_ref).is_none());
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
    fn timeline_model_maps_sequence_tracks_clips_and_selection() {
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        sequence.playhead = TimeCode::new(42, tb);
        sequence.mark_in(12);
        sequence.mark_out(64);

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
        let selected_track_id = sequence.audio_tracks[0].id;
        let model = TimelinePanelModel::from_sequence(&sequence, &[selected], &[selected_track_id]);

        assert_eq!(model.playhead_frame, 42);
        assert_eq!(model.in_point_frame, 12);
        assert_eq!(model.out_point_frame, Some(64));
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
        assert!(model.tracks[0].clips[0].select_action.is_none());
        assert!(!model.tracks[0].selected);
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
    fn timeline_model_maps_track_control_payloads_to_stable_track_ids() {
        let mut sequence = Sequence::new("edit");
        sequence.video_tracks[0].is_visible = false;
        sequence.audio_tracks[0].is_muted = true;

        let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);
        let video_ref = TimelineTrackRef { track_index: 0 };
        let first_audio_ref = TimelineTrackRef { track_index: sequence.video_tracks.len() };

        let visibility = model
            .track_control_payload(
                TimelineTrackControl::Visibility,
                video_ref,
                &model.tracks[video_ref.track_index],
            )
            .expect("visibility payload");
        assert_eq!(visibility.track_id, sequence.video_tracks[0].id);
        assert!(visibility.is_video_track);
        assert_eq!(
            visibility.control,
            TimelineTrackControlPayloadKind::Visibility
        );
        assert!(visibility.enabled);

        let mute = model
            .track_control_payload(
                TimelineTrackControl::Mute,
                first_audio_ref,
                &model.tracks[first_audio_ref.track_index],
            )
            .expect("mute payload");
        assert_eq!(mute.track_id, sequence.audio_tracks[0].id);
        assert!(!mute.is_video_track);
        assert_eq!(mute.control, TimelineTrackControlPayloadKind::Mute);
        assert!(!mute.enabled);
    }

    #[test]
    fn timeline_panel_corner_add_buttons_emit_typed_timeline_actions() {
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
                position: Point::new(16.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        let recorded = actions.borrow();
        assert_eq!(recorded.len(), 1);
        let Action::Custom { namespace, name, payload } = &recorded[0] else {
            panic!("expected custom add-track action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_ADD_TRACK);
        let payload: TimelineAddTrackPayload =
            serde_json::from_value(payload.clone()).expect("add track payload");
        assert_eq!(payload.kind, TimelineAddTrackKind::Video);
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

        panel.event(&UiEvent::FocusGained, &mut ctx);
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

        panel.event(&UiEvent::FocusGained, &mut ctx);
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

        panel.event(&UiEvent::FocusGained, &mut ctx);
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

        panel.event(&UiEvent::FocusGained, &mut ctx);
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
    fn app_state_models_resolve_stale_selected_clip_metadata_by_clip_id() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        sequence.add_video_track();
        let tb = sequence.time_base();
        let actual_track_id = sequence.video_tracks[0].id;
        let stale_track_id = sequence.video_tracks[1].id;
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(24, tb));
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.sequence = Some(sequence);
        state.selection.selected_clips = vec![SelectedClipRef {
            track_id: stale_track_id,
            is_video_track: false,
            clip_id,
        }];

        let models = SelfHostedPanelModels::from_app_state(&state);

        assert!(models.timeline.tracks[0].clips[0].selected);
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
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        let color = Color::from_rgba8(20, 90, 160, 180);
        let mut clip = Clip::new_solid_color(
            AssetId::new(),
            color,
            TimeCode::new(4, tb),
            TimeCode::new(18, tb),
        );
        clip.is_disabled = true;
        clip.transform.set_position(glam::Vec2::new(192.0, 108.0));
        clip.transform.set_scale(glam::Vec2::splat(1.25));
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
        state.sequence = Some(sequence);
        state.selection.selected_clips.push(SelectedClipRef {
            track_id,
            is_video_track: true,
            clip_id,
        });
        state.seek(7);

        let models = SelfHostedPanelModels::from_app_state(&state);

        assert_eq!(models.viewer.title, "edit");
        assert_eq!(models.viewer.frame_label, "F7");
        assert!(models.viewer.resolution_label.contains("1920x1080"));
        assert_eq!(models.timeline.playhead_frame, 7);
        assert!(models.timeline.tracks[0].clips[0].selected);
        assert!(models.timeline.tracks[0].clips[0].disabled);
        assert!(!models.inspector.enabled);
        assert_eq!(models.inspector.opacity, 100.0);
        assert_eq!(
            models.inspector.curve_points,
            vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 1.0)]
        );
        assert_eq!(models.inspector.tint.to_rgba8(), color.to_rgba8());
        assert_eq!(models.inspector.position_x, 192.0);
        assert_eq!(models.inspector.position_y, 108.0);
        assert_eq!(models.inspector.scale_percent, 125.0);
        assert_eq!(models.inspector.rotation_degrees, 15.0);
        assert_eq!(models.inspector.in_frame, 4.0);
        assert_eq!(models.inspector.out_frame, 22.0);
        assert_eq!(models.inspector.max_frame, 22.0);
        assert_eq!(models.inspector.effects.len(), 1);
        assert_eq!(models.inspector.effects[0].effect_id, effect_id);
        assert_eq!(
            models.inspector.effects[0].label,
            effect_display_name(&EffectType::GaussianBlur)
        );
        assert!(!models.inspector.effects[0].enabled);
    }

    #[test]
    fn app_state_models_read_opacity_keyframes_as_curve_points() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;

        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::OPACITY_PATH.to_string(),
            keyframe: Keyframe::linear(
                timecode_to_ticks(TimeCode::new(10, tb)),
                PropertyValue::Float(0.0),
            ),
        })
        .expect("set start opacity");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::OPACITY_PATH.to_string(),
            keyframe: Keyframe::linear(
                timecode_to_ticks(TimeCode::new(20, tb)),
                PropertyValue::Float(0.5),
            ),
        })
        .expect("set mid opacity");
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::OPACITY_PATH.to_string(),
            keyframe: Keyframe::linear(
                timecode_to_ticks(TimeCode::new(30, tb)),
                PropertyValue::Float(1.0),
            ),
        })
        .expect("set end opacity");

        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.sequence = Some(sequence);
        state.selection.selected_clips.push(SelectedClipRef {
            track_id,
            is_video_track: true,
            clip_id,
        });

        let models = SelfHostedPanelModels::from_app_state(&state);

        assert_eq!(
            models.inspector.curve_points,
            vec![
                CurvePoint::new(0.0, 0.0),
                CurvePoint::new(0.5, 0.5),
                CurvePoint::new(1.0, 1.0),
            ]
        );
    }

    #[test]
    fn app_state_models_synthesize_opacity_curve_endpoints_from_evaluated_values() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;

        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::OPACITY_PATH.to_string(),
            keyframe: Keyframe::linear(
                timecode_to_ticks(TimeCode::new(20, tb)),
                PropertyValue::Float(0.5),
            ),
        })
        .expect("set midpoint opacity");

        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.sequence = Some(sequence);
        state.selection.selected_clips.push(SelectedClipRef {
            track_id,
            is_video_track: true,
            clip_id,
        });

        let models = SelfHostedPanelModels::from_app_state(&state);

        assert_eq!(
            models.inspector.curve_points,
            vec![
                CurvePoint::new(0.0, 0.5),
                CurvePoint::new(0.5, 0.5),
                CurvePoint::new(1.0, 0.5),
            ]
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
        state.asset_library = Some(library);

        let models = SelfHostedPanelModels::from_app_state(&state);

        assert_eq!(models.assets.items.len(), 1);
        assert!(models.assets.demo_activate_prefix.is_none());
        let item = &models.assets.items[0];
        assert_eq!(item.title, "Brand Purple");
        assert_eq!(item.badge.as_deref(), Some("CLR"));
        assert!(item.select_action.is_none());
        let action = item.activate_action.as_ref().expect("activate action");
        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected asset custom action, got {action:?}");
        };
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_PREPARE_DRAG);
        let payload: AssetsPrepareDragPayload =
            serde_json::from_value(payload.clone()).expect("asset drag payload");
        assert_eq!(payload.asset_id, asset_id);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn effect_panel_model_uses_stable_item_actions_without_dynamic_prefix() {
        let model = PanelListModel::from_effect_registry(None);

        assert!(model.demo_activate_prefix.is_none());
        assert!(model.items.iter().all(|item| item.select_action.is_none()));
        assert!(model.items.iter().all(|item| item.activate_action.is_none()));
    }

    #[test]
    fn effect_panel_model_adds_effect_actions_for_selected_video_clip() {
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(clip).expect("add video clip");
        let selection = SelectedClipRef { track_id, is_video_track: true, clip_id };

        let model = PanelListModel::from_effect_registry(Some(selection));

        assert!(model.demo_activate_prefix.is_none());
        let action = model
            .items
            .iter()
            .find_map(|item| item.activate_action.as_ref())
            .expect("effect activate action");
        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected effect custom action, got {action:?}");
        };
        assert_eq!(namespace, EFFECTS_NAMESPACE);
        assert_eq!(name, EFFECTS_ADD_TO_CLIP);
        let payload: EffectsAddToClipPayload =
            serde_json::from_value(payload.clone()).expect("effect add payload");
        assert_eq!(payload.clip.clip_id, clip_id);
        assert_eq!(payload.clip.track_id, track_id);
        assert!(payload.clip.is_video_track);
    }

    #[test]
    fn demo_asset_model_is_the_only_dynamic_activation_fixture() {
        let model = demo_asset_model();

        assert_eq!(
            model.demo_activate_prefix.as_deref(),
            Some("assets.activate")
        );
        assert!(model.items.iter().all(|item| item.activate_action.is_none()));
        assert!(model.items.iter().all(|item| item.select_action.is_some()));
        let action = model.items[0].select_action.as_ref().expect("select");
        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected demo custom action, got {action:?}");
        };
        assert_eq!(namespace, "ui.demo_panel");
        assert_eq!(name, "assets.select.footage");
        assert!(payload.is_null());
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

        let model = TimelinePanelModel::from_sequence(&sequence, &[], &[]);

        assert_eq!(model.tracks[0].clips[0].label, "纯色层");
        assert_eq!(
            model.tracks[0].clips[0].color.map(|c| c.to_rgba8()),
            Some(color.to_rgba8())
        );
    }

    #[test]
    fn inspector_curve_action_uses_typed_payload_for_selected_clip() {
        let selection = SelectedClipRef {
            track_id: TrackId::new(),
            is_video_track: true,
            clip_id: ClipId::new(),
        };
        let action = inspector_curve_action(
            Some(selection),
            &[
                CurvePoint::new(-0.2, 0.25),
                CurvePoint::new(0.5, f32::NAN),
                CurvePoint::new(1.2, 0.75),
            ],
        );

        match action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, INSPECTOR_NAMESPACE);
                assert_eq!(name, INSPECTOR_SET_CLIP_CURVE);
                let payload: InspectorSetClipCurvePayload =
                    serde_json::from_value(payload).expect("curve payload");
                assert_eq!(payload.clip.clip_id, selection.clip_id);
                assert_eq!(
                    payload.points,
                    vec![
                        InspectorCurvePointPayload { x: 0.0, y: 0.25 },
                        InspectorCurvePointPayload { x: 1.0, y: 0.75 },
                    ]
                );
            }
            other => panic!("expected inspector curve action, got {other:?}"),
        }
    }

    #[test]
    fn inspector_actions_without_selection_are_noops() {
        assert_eq!(inspector_value_action(None, "opacity", 42.0), Action::NoOp);
        assert_eq!(inspector_bool_action(None, true), Action::NoOp);
        assert_eq!(
            inspector_color_action(None, Color::from_rgba8(1, 2, 3, 4)),
            Action::NoOp
        );
        assert_eq!(
            inspector_transform_action(None, InspectorClipTransformField::PositionX, 12.0),
            Action::NoOp
        );
        assert_eq!(
            inspector_timing_action(None, TimelineTrimPayloadEdge::In, 10.0),
            Action::NoOp
        );
        assert_eq!(
            inspector_effect_enabled_action(None, EffectId::new(), false),
            Action::NoOp
        );
        assert_eq!(
            inspector_remove_effect_row_action(None, EffectId::new()),
            Action::NoOp
        );
        assert_eq!(
            inspector_curve_action(None, &[CurvePoint::new(0.0, 1.0)]),
            Action::NoOp
        );
    }

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("mondrian-{prefix}-{suffix}"))
    }

    fn project_item<'a>(model: &'a PanelListModel, title: &str) -> &'a PanelListItem {
        model.items.iter().find(|item| item.title == title).expect("project panel item")
    }

    fn assert_shell_action(action: Option<&Action>, name: &str) {
        match action {
            Some(Action::Custom { namespace, name: action_name, payload }) => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(action_name, name);
                assert!(payload.is_null());
            }
            other => panic!("expected app shell action, got {other:?}"),
        }
    }
}
