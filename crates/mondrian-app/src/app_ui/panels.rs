//! Panel adapters for the app UI shell.
//!
//! These adapters translate application-facing panel concepts into generic
//! `mondrian-ui-widgets` view models. Demo data is kept behind explicit model
//! factories so real `AppState` / `EditorState` adapters can replace it without
//! changing dock layout or widget construction.

use std::path::Path;
use std::rc::Rc;

use mondrian_assets::library::FolderRecord;
use mondrian_assets::{AssetKind, AssetLibrary, AssetRecord};
use mondrian_core::automation::timecode_to_ticks;
use mondrian_core::automation::PropertyValue;
use mondrian_core::effect_data::EffectType;
use mondrian_core::types::{
    AssetId, ClipId, EffectId, JobId, Rational, SequenceId, TimeCode, TrackId,
};
use mondrian_core::Color;
use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::Action;
use mondrian_effects::{effect_display_name, effect_library_types};
use mondrian_export::preset::{ExportPreset, TimelineExportRange, VideoCodecConfig};
use mondrian_export::queue::JobStatus;
use mondrian_timeline::clip::{Clip, Transform2D};
use mondrian_timeline::sequence::Sequence;
use mondrian_timeline::track::Track;
use mondrian_ui_core::types::SplitDirection;
use mondrian_ui_core::DragPayload;
use mondrian_ui_core::Widget;
use mondrian_ui_theme::current_theme;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::dock_tab_bar::TabInfo;
use mondrian_ui_widgets::NumberInput;
use mondrian_ui_widgets::{
    AssetGrid, AssetGridBadgeTone, AssetGridItem, Button, Checkbox, ColorPickerAreaMode,
    ColorPickerTrigger, CurveEditor, CurvePoint, DockPanel, DockPanelDropArea, Dropdown, FlexChild,
    FlexContainer, Label, MenuItem, NodeGraphEdge, NodeGraphNode, NodeGraphView, PanelList,
    PanelListItem, PropertyPanel, PropertyPanelOptions, PropertyRow, PropertySection, RasterImage,
    ScrollView, Slider, TextInput, TimelineAssetDrop, TimelineClip, TimelineClipKind,
    TimelineClipMove, TimelineClipRef, TimelineClipTrim, TimelineEditCommand, TimelineInOutPoint,
    TimelineToolbarIconSlot, TimelineTrack, TimelineTrackControl, TimelineTrackControlIconSlot,
    TimelineTrackMove, TimelineTrackRef, TimelineTrimEdge, TimelineView, ViewerControl,
    ViewerFrameImage, ViewerStatusTone, ViewerSurface, WaveformDisplay,
};

use crate::app::exporting::{builtin_export_presets, export_preset_extension};
use crate::app::ui_actions::{
    app_shell_export_output_dialog_action, app_shell_import_media_dialog_action_with_target,
    app_shell_relink_asset_dialog_action, app_shell_relocate_panel_action,
    app_shell_reveal_in_file_manager_action, assets_create_adjustment_layer_action,
    assets_create_folder_action, assets_create_solid_color_action, assets_delete_asset_action,
    assets_delete_folder_action, assets_delete_selection_action, assets_import_files_action,
    assets_move_asset_action, assets_move_folder_action, assets_move_selection_action,
    assets_open_folder_action, assets_prepare_drag_action, assets_rename_asset_action,
    assets_rename_folder_action, assets_set_proxy_mode_action, effects_add_to_clip_action,
    export_cancel_job_action, export_clear_completed_action, export_enqueue_action,
    export_set_draft_action, inspector_remove_effect_action, inspector_select_effect_action,
    inspector_set_clip_curve_action, inspector_set_clip_enabled_action,
    inspector_set_clip_opacity_action, inspector_set_clip_tint_action,
    inspector_set_clip_transform_field_action, inspector_set_effect_enabled_action,
    inspector_set_effect_property_action, timeline_add_track_action,
    timeline_clear_in_out_points_action, timeline_drop_asset_action, timeline_move_clip_action,
    timeline_move_track_action, timeline_open_nested_sequence_action,
    timeline_roll_selected_cut_to_playhead_action, timeline_seek_action,
    timeline_select_clip_action, timeline_set_in_out_point_action,
    timeline_set_selected_clips_enabled_action, timeline_set_track_control_action,
    timeline_trim_clips_action, timeline_trim_selected_clips_to_playhead_action,
    viewer_set_preview_resolution_scale_action, viewer_set_zoom_scale_action,
    AppShellRelinkAssetDialogPayload, AppShellRelocatePanelPayload,
    AppShellRevealInFileManagerPayload, AssetsCreateAssetPayload, AssetsCreateFolderPayload,
    AssetsDeleteAssetPayload, AssetsDeleteFolderPayload, AssetsDeleteSelectionPayload,
    AssetsImportFilesPayload, AssetsMoveAssetPayload, AssetsMoveFolderPayload,
    AssetsMoveSelectionPayload, AssetsOpenFolderPayload, AssetsPrepareDragPayload,
    AssetsRenameAssetPayload, AssetsRenameFolderPayload, AssetsSetProxyModePayload,
    DockDropAreaPayload, EffectsAddToClipPayload, ExportDraftUpdatePayload, ExportEnqueuePayload,
    ExportJobTargetPayload, ExportOutputDialogPayload, ImportMediaDialogPayload,
    InspectorClipRefPayload, InspectorClipTransformField, InspectorCurvePointPayload,
    InspectorRemoveEffectPayload, InspectorSelectEffectPayload, InspectorSetClipCurvePayload,
    InspectorSetClipEnabledPayload, InspectorSetClipOpacityPayload, InspectorSetClipTintPayload,
    InspectorSetClipTransformFieldPayload, InspectorSetEffectEnabledPayload,
    InspectorSetEffectPropertyPayload, TimelineAddTrackKind, TimelineAddTrackPayload,
    TimelineDropAssetPayload, TimelineInOutPointPayloadKind, TimelineMoveClipPayload,
    TimelineMoveTrackPayload, TimelineOpenNestedSequencePayload, TimelineSelectClipPayload,
    TimelineSetInOutPointPayload, TimelineSetSelectedClipsEnabledPayload,
    TimelineSetTrackControlPayload, TimelineTrackControlPayloadKind, TimelineTrimClipsPayload,
    TimelineTrimPayloadEdge, TimelineTrimSelectedClipsToPlayheadPayload,
    ViewerSetPreviewResolutionScalePayload, ViewerSetZoomScalePayload,
};
use crate::app::{AppState, SelectedClipRef};
use crate::app_ui::action_availability::app_state_action_enabled;
use crate::app_ui::icons::AppIcon;
use crate::app_ui::preview_scale::normalize_preview_resolution_scale;
use crate::app_ui::shortcuts::shortcut_label_for_action;
use crate::app_ui::waveform_cache::AudioWaveformCache;
use crate::app_ui::workspace_layout::AppUiWorkspaceLayout;

/// Supplies already-decoded thumbnails for asset-grid cards.
///
/// Implementations may cache, schedule background work, or return `None` while
/// a thumbnail is unavailable. The panel adapter stays read-only and never
/// decodes media directly.
pub trait AssetThumbnailSource {
    /// Return the current thumbnail lifecycle state for an asset.
    fn thumbnail_for_asset(&self, asset: &AssetRecord) -> AssetThumbnailState;
}

/// Supplies render-ready viewer preview frames for the active application state.
///
/// Implementations own preview caching, media decode, and render-plan execution.
/// Panel models only receive immutable `ViewerFrameImage` payloads.
pub trait ViewerPreviewSource {
    /// Return the current viewer preview frame, or `None` while unavailable.
    fn viewer_frame_for_state(&self, state: &AppState) -> Option<ViewerFrameImage>;
}

/// Current thumbnail lifecycle state for one asset card.
#[derive(Debug, Clone)]
pub enum AssetThumbnailState {
    /// No thumbnail is expected for this asset.
    Unavailable,
    /// A thumbnail request has been queued or is currently decoding.
    Loading,
    /// A thumbnail was expected but could not be loaded.
    Failed,
    /// A render-ready thumbnail is available.
    Ready(RasterImage),
}

/// Complete set of view models needed by the app UI panel shell.
#[derive(Debug, Clone)]
pub struct AppUiPanelModels {
    pub assets: AssetGridModel,
    pub effects: PanelListModel,
    pub viewer: ViewerPanelModel,
    pub timeline: TimelinePanelModel,
    pub inspector: InspectorPanelModel,
    pub export: ExportPanelModel,
    pub node_graph: NodeGraphPanelModel,
}

impl AppUiPanelModels {
    /// Snapshot the current application state into app UI panel models.
    ///
    /// This is a read-only boundary: widgets receive generic view models and
    /// emit actions, while domain mutations stay in `AppState` handlers.
    pub fn from_app_state(state: &AppState) -> Self {
        Self::from_app_state_with_asset_folder(state, None)
    }

    /// Snapshot app state while keeping shell-local asset browser navigation.
    pub fn from_app_state_with_asset_folder(
        state: &AppState,
        asset_folder_id: Option<&str>,
    ) -> Self {
        Self::from_app_state_with_asset_folder_and_thumbnails(state, asset_folder_id, None)
    }

    /// Snapshot app state with an optional thumbnail source for asset cards.
    pub fn from_app_state_with_asset_folder_and_thumbnails(
        state: &AppState,
        asset_folder_id: Option<&str>,
        thumbnails: Option<&dyn AssetThumbnailSource>,
    ) -> Self {
        Self::from_app_state_with_asset_folder_thumbnails_and_preview(
            state,
            asset_folder_id,
            thumbnails,
            None,
        )
    }

    /// Snapshot app state with optional thumbnail and viewer preview sources.
    pub fn from_app_state_with_asset_folder_thumbnails_and_preview(
        state: &AppState,
        asset_folder_id: Option<&str>,
        thumbnails: Option<&dyn AssetThumbnailSource>,
        preview: Option<&dyn ViewerPreviewSource>,
    ) -> Self {
        Self {
            assets: AssetGridModel::from_asset_library_in_folder_with_thumbnails(
                state.asset_library.as_deref(),
                asset_folder_id,
                thumbnails,
                Some(&state.proxy_mode_assets),
            ),
            effects: PanelListModel::from_app_effect_registry(state),
            viewer: ViewerPanelModel::from_app_state_with_preview(state, preview),
            timeline: TimelinePanelModel::from_app_state(state),
            inspector: InspectorPanelModel::from_app_state(state),
            export: ExportPanelModel::from_app_state(state),
            node_graph: NodeGraphPanelModel::from_app_state(state),
        }
    }

    /// Demo fixtures that keep rich browser panels while sourcing timeline and
    /// inspector state from an `AppState` snapshot.
    #[cfg(test)]
    pub fn demo_from_app_state(state: &AppState) -> Self {
        Self {
            assets: demo_asset_model(),
            effects: PanelListModel::from_app_effect_registry(state),
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
                    .with_app_edit_availability(state)
                })
                .unwrap_or_else(demo_timeline_model),
            inspector: InspectorPanelModel::from_app_state(state),
            export: ExportPanelModel::from_app_state(state),
            node_graph: NodeGraphPanelModel::from_app_state(state),
        }
    }

    /// Demo fixtures used by tests before the real editor state is wired into
    /// the app UI shell.
    #[cfg(test)]
    pub fn demo() -> Self {
        let state = demo_app_state();
        Self::demo_from_app_state(&state)
    }
}

/// Build a synthetic app state for app UI tests.
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
    pub filter_placeholder: Option<String>,
}

/// Asset-browser card-grid data independent from a concrete widget instance.
#[derive(Debug, Clone)]
pub struct AssetGridModel {
    pub title: String,
    pub subtitle: String,
    pub items: Vec<AssetGridItem>,
    pub filter_placeholder: Option<String>,
    pub accepts_file_drop: bool,
    pub current_folder_id: Option<String>,
}

impl AssetGridModel {
    pub fn new(title: impl Into<String>, items: Vec<AssetGridItem>) -> Self {
        Self {
            title: title.into(),
            subtitle: String::new(),
            items,
            filter_placeholder: None,
            accepts_file_drop: false,
            current_folder_id: None,
        }
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    pub fn with_filter_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.filter_placeholder = Some(placeholder.into());
        self
    }

    pub fn accepts_file_drop(mut self, accepts: bool) -> Self {
        self.accepts_file_drop = accepts;
        self
    }

    /// Build the project asset browser. Database read failures are represented
    /// as disabled cards so the panel can render without owning app errors.
    pub fn from_asset_library(library: Option<&AssetLibrary>) -> Self {
        Self::from_asset_library_in_folder(library, None)
    }

    /// Build the project asset browser for a shell-local folder selection.
    pub fn from_asset_library_in_folder(
        library: Option<&AssetLibrary>,
        current_folder_id: Option<&str>,
    ) -> Self {
        Self::from_asset_library_in_folder_with_thumbnails(library, current_folder_id, None, None)
    }

    /// Build the project asset browser for a shell-local folder selection,
    /// optionally attaching already-decoded thumbnails.
    pub fn from_asset_library_in_folder_with_thumbnails(
        library: Option<&AssetLibrary>,
        current_folder_id: Option<&str>,
        thumbnails: Option<&dyn AssetThumbnailSource>,
        proxy_mode_assets: Option<&std::collections::HashSet<AssetId>>,
    ) -> Self {
        let colors = current_theme().colors.clone();
        let Some(library) = library else {
            return AssetGridModel::new(
                "Assets",
                vec![asset_empty_item(
                    "asset-library-disconnected",
                    "没有项目素材库",
                    "打开或创建项目后浏览素材",
                    colors.muted_foreground,
                    AppIcon::Folder,
                )],
            )
            .with_subtitle("项目素材库")
            .with_filter_placeholder("搜索素材");
        };

        let folders = match library.list_folders() {
            Ok(folders) => folders,
            Err(err) => {
                return AssetGridModel::new(
                    "Assets",
                    vec![asset_empty_item(
                        "asset-library-error",
                        "素材库不可用",
                        err.to_string(),
                        colors.error,
                        AppIcon::Warning,
                    )],
                )
                .with_subtitle("项目素材库")
                .with_filter_placeholder("搜索素材");
            }
        };
        let assets = match library.list_assets() {
            Ok(assets) => assets,
            Err(err) => {
                return AssetGridModel::new(
                    "Assets",
                    vec![asset_empty_item(
                        "asset-library-error",
                        "素材库不可用",
                        err.to_string(),
                        colors.error,
                        AppIcon::Warning,
                    )],
                )
                .with_subtitle("项目素材库")
                .with_filter_placeholder("搜索素材");
            }
        };

        let current_folder =
            current_folder_id.and_then(|id| folders.iter().find(|folder| folder.id == id));
        let subtitle = current_folder
            .map(|folder| format!("项目素材库 / {}", folder.name))
            .unwrap_or_else(|| "项目素材库".to_owned());
        let current_folder_id = current_folder.map(|folder| folder.id.clone());
        let items = asset_grid_items_from_library_records(
            &folders,
            assets,
            current_folder,
            thumbnails,
            proxy_mode_assets,
        );
        if items.is_empty() {
            return AssetGridModel::new("Assets", Vec::new())
                .with_subtitle(subtitle)
                .with_filter_placeholder("搜索素材")
                .accepts_file_drop(true)
                .with_current_folder_id(current_folder_id);
        }

        AssetGridModel::new("Assets", items)
            .with_subtitle(subtitle)
            .with_filter_placeholder("搜索素材")
            .accepts_file_drop(true)
            .with_current_folder_id(current_folder_id)
    }

    pub fn with_current_folder_id(mut self, folder_id: Option<String>) -> Self {
        self.current_folder_id = folder_id;
        self
    }
}

impl PanelListModel {
    pub fn new(title: impl Into<String>, items: Vec<PanelListItem>) -> Self {
        Self {
            title: title.into(),
            subtitle: String::new(),
            items,
            filter_placeholder: None,
        }
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    pub fn with_filter_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.filter_placeholder = Some(placeholder.into());
        self
    }

    /// Build the visible effect browser from the current app state.
    ///
    /// The widget rows stay domain-light. Browsing remains available even when
    /// the current selection cannot receive an effect; only the apply action is
    /// withheld in those states.
    pub fn from_app_effect_registry(state: &AppState) -> Self {
        let target = state.primary_selected_clip().filter(|selection| selection.is_video_track);
        let apply_blocker = match target {
            Some(selection) if selected_clip_track_is_locked(state, selection) => {
                Some("所选剪辑所在轨道已锁定")
            }
            Some(_) => None,
            None => Some("选择视频剪辑后应用效果"),
        };
        let action_target = if apply_blocker.is_none() {
            target
        } else {
            None
        };
        Self::effect_registry_model(action_target, apply_blocker)
    }

    /// Build the visible effect browser from the shared effect registry.
    pub fn from_effect_registry(selected_clip: Option<SelectedClipRef>) -> Self {
        let effect_target = selected_clip.filter(|selection| selection.is_video_track);
        let apply_blocker = effect_target.is_none().then_some("选择视频剪辑后应用效果");
        Self::effect_registry_model(effect_target, apply_blocker)
    }

    fn effect_registry_model(
        effect_target: Option<SelectedClipRef>,
        _apply_blocker: Option<&'static str>,
    ) -> Self {
        let effects = effect_library_types();
        let items = if effects.is_empty() {
            vec![PanelListItem::new("没有可用效果").disabled(true)]
        } else {
            let mut items = Vec::new();
            let mut emitted_categories = std::collections::BTreeSet::<String>::new();
            for effect_type in effects {
                let categories = effect_type.category_path();
                let mut prefix = String::new();
                for (depth, category) in categories.iter().enumerate() {
                    if !prefix.is_empty() {
                        prefix.push('/');
                    }
                    prefix.push_str(category);
                    if emitted_categories.insert(prefix.clone()) {
                        items.push(
                            PanelListItem::new((*category).to_owned())
                                .with_tree_depth(depth as u8)
                                .with_tree_node(prefix.clone(), true),
                        );
                    }
                }

                let name = effect_display_name(&effect_type);
                let depth = categories.len();
                let mut item = PanelListItem::new(name).with_tree_depth(depth as u8);
                if let Some(selection) = effect_target {
                    item = item.with_activate_action(effects_add_to_clip_action(
                        EffectsAddToClipPayload {
                            clip: inspector_clip_payload(selection),
                            effect_type,
                        },
                    ));
                }
                items.push(item);
            }
            items
        };

        PanelListModel::new("Effects", items).with_filter_placeholder("搜索效果")
    }
}

/// Viewer panel data independent from preview texture plumbing.
#[derive(Debug, Clone)]
pub struct ViewerPanelModel {
    pub title: String,
    pub status: String,
    pub status_tone: ViewerStatusTone,
    pub resolution_label: String,
    pub timecode_label: String,
    pub frame_label: String,
    pub duration_label: String,
    pub zoom_label: String,
    pub zoom_scale: Option<f32>,
    pub preview_quality_label: String,
    pub preview_resolution_scale: f32,
    pub width: u32,
    pub height: u32,
    pub playing: bool,
    pub enabled: bool,
    pub frame_image: Option<ViewerFrameImage>,
    pub empty_message: Option<String>,
}

impl ViewerPanelModel {
    /// Snapshot viewer chrome data from app state.
    pub fn from_app_state(state: &AppState) -> Self {
        Self::from_app_state_with_preview(state, None)
    }

    /// Snapshot viewer chrome data and attach an optional render-ready frame.
    pub fn from_app_state_with_preview(
        state: &AppState,
        preview: Option<&dyn ViewerPreviewSource>,
    ) -> Self {
        let Some(sequence) = state.sequence.as_ref() else {
            return Self::empty();
        };
        let resolution = sequence.settings.resolution;
        let current_frame = state.current_frame().max(0);
        let timecode_label = state
            .current_time_code()
            .map(|timecode| timecode.to_smpte())
            .unwrap_or_else(|| TimeCode::new(current_frame, sequence.time_base()).to_smpte());
        let duration_frame = sequence.total_duration().frame.max(0);
        let fps = sequence.settings.frame_rate.to_f64();
        let frame_image = preview.and_then(|preview| preview.viewer_frame_for_state(state));
        let preview_resolution_scale =
            normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale);
        let preview_quality_label = viewer_preview_quality_label(preview_resolution_scale);

        Self {
            title: sequence.name.clone(),
            status: if state.is_playing() {
                "播放中".into()
            } else {
                "就绪".into()
            },
            status_tone: if state.is_playing() {
                ViewerStatusTone::Accent
            } else {
                ViewerStatusTone::Neutral
            },
            resolution_label: format!(
                "{}x{} @ {:.2} fps",
                resolution.width, resolution.height, fps
            ),
            timecode_label,
            frame_label: format!("F{current_frame}"),
            duration_label: format!("{duration_frame} 帧"),
            zoom_label: "适合".into(),
            zoom_scale: None,
            preview_quality_label,
            preview_resolution_scale,
            width: resolution.width,
            height: resolution.height,
            playing: state.is_playing(),
            enabled: true,
            frame_image,
            empty_message: None,
        }
    }

    /// Empty viewer shown before a sequence is open.
    pub fn empty() -> Self {
        Self {
            title: "预览".into(),
            status: "没有序列".into(),
            status_tone: ViewerStatusTone::Neutral,
            resolution_label: "无信号".into(),
            timecode_label: "00:00:00:00".into(),
            frame_label: "F0".into(),
            duration_label: String::new(),
            zoom_label: "适合".into(),
            zoom_scale: None,
            preview_quality_label: "1/1".into(),
            preview_resolution_scale: 1.0,
            width: 16,
            height: 9,
            playing: false,
            enabled: false,
            frame_image: None,
            empty_message: Some("未载入序列".into()),
        }
    }
}

fn viewer_preview_quality_label(scale: f32) -> String {
    let scale = normalize_preview_resolution_scale(scale);
    if (scale - 1.0).abs() <= f32::EPSILON {
        "1/1".into()
    } else if (scale - 0.5).abs() <= f32::EPSILON {
        "1/2".into()
    } else if (scale - 0.25).abs() <= f32::EPSILON {
        "1/4".into()
    } else {
        "1/8".into()
    }
}

/// Timeline panel data in frame space.
#[derive(Debug, Clone)]
pub struct TimelinePanelModel {
    pub tracks: Vec<TimelineTrack>,
    pub playhead_frame: i64,
    pub in_point_frame: i64,
    pub out_point_frame: Option<i64>,
    pub frame_rate: Rational,
    pub enabled: bool,
    pub empty_message: Option<String>,
    edit_availability: Option<TimelineEditAvailability>,
    track_refs: Vec<AppTimelineTrackRef>,
    clip_refs: Vec<Vec<ClipId>>,
    nested_sequence_refs: Vec<Vec<Option<SequenceId>>>,
    pub waveform_display: WaveformDisplay,
}

impl Default for TimelinePanelModel {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            playhead_frame: 0,
            in_point_frame: 0,
            out_point_frame: None,
            frame_rate: Rational::FPS_30,
            enabled: false,
            empty_message: None,
            edit_availability: None,
            track_refs: Vec::new(),
            clip_refs: Vec::new(),
            nested_sequence_refs: Vec::new(),
            waveform_display: WaveformDisplay::BottomAligned,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineEditAvailability {
    cut: bool,
    copy: bool,
    paste: bool,
    duplicate: bool,
    delete: bool,
    ripple_delete: bool,
    split: bool,
    trim_in_to_playhead: bool,
    trim_out_to_playhead: bool,
    roll_cut_to_playhead: bool,
    set_selected_enabled: bool,
    mark_in: bool,
    mark_out: bool,
    clear_in_out: bool,
    toggle_playback: bool,
}

impl TimelineEditAvailability {
    fn from_app_state(state: &AppState) -> Self {
        Self {
            cut: app_state_action_enabled(&Action::Cut, state),
            copy: app_state_action_enabled(&Action::Copy, state),
            paste: app_state_action_enabled(&Action::Paste, state),
            duplicate: app_state_action_enabled(&Action::Duplicate, state),
            delete: app_state_action_enabled(&Action::DeleteSelection, state),
            ripple_delete: app_state_action_enabled(&Action::RippleDeleteSelection, state),
            split: app_state_action_enabled(&Action::SplitClipAtPlayhead, state),
            trim_in_to_playhead: app_state_action_enabled(
                &timeline_trim_selected_clips_to_playhead_action(
                    TimelineTrimSelectedClipsToPlayheadPayload {
                        edge: TimelineTrimPayloadEdge::In,
                    },
                ),
                state,
            ),
            trim_out_to_playhead: app_state_action_enabled(
                &timeline_trim_selected_clips_to_playhead_action(
                    TimelineTrimSelectedClipsToPlayheadPayload {
                        edge: TimelineTrimPayloadEdge::Out,
                    },
                ),
                state,
            ),
            roll_cut_to_playhead: app_state_action_enabled(
                &timeline_roll_selected_cut_to_playhead_action(),
                state,
            ),
            set_selected_enabled: selected_clip_tracks_are_editable(state),
            mark_in: app_state_action_enabled(&Action::MarkInAtPlayhead, state),
            mark_out: app_state_action_enabled(&Action::MarkOutAtPlayhead, state),
            clear_in_out: app_state_action_enabled(&timeline_clear_in_out_points_action(), state),
            toggle_playback: app_state_action_enabled(&Action::TogglePlay, state),
        }
    }

    fn allows(self, command: TimelineEditCommand) -> bool {
        match command {
            TimelineEditCommand::CutSelection => self.cut,
            TimelineEditCommand::CopySelection => self.copy,
            TimelineEditCommand::PasteAtPlayhead => self.paste,
            TimelineEditCommand::DuplicateSelection => self.duplicate,
            TimelineEditCommand::DeleteSelection => self.delete,
            TimelineEditCommand::RippleDeleteSelection => self.ripple_delete,
            TimelineEditCommand::SplitAtPlayhead => self.split,
            TimelineEditCommand::TrimSelectionInToPlayhead => self.trim_in_to_playhead,
            TimelineEditCommand::TrimSelectionOutToPlayhead => self.trim_out_to_playhead,
            TimelineEditCommand::RollSelectedCutToPlayhead => self.roll_cut_to_playhead,
            TimelineEditCommand::EnableSelection | TimelineEditCommand::DisableSelection => {
                self.set_selected_enabled
            }
            TimelineEditCommand::OpenNestedSequence(_) => true,
            TimelineEditCommand::MarkInAtPlayhead => self.mark_in,
            TimelineEditCommand::MarkOutAtPlayhead => self.mark_out,
            TimelineEditCommand::ClearInOutPoints => self.clear_in_out,
            TimelineEditCommand::TogglePlayback => self.toggle_playback,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppTimelineTrackRef {
    track_id: TrackId,
    is_video_track: bool,
}

impl TimelinePanelModel {
    /// Snapshot the app timeline with app-level command availability.
    pub fn from_app_state(state: &AppState) -> Self {
        state.sequence.as_ref().map_or_else(Self::empty, |sequence| {
            Self::from_sequence_with_library(
                sequence,
                state.selected_clips(),
                state.selected_tracks(),
                state.asset_library.as_deref(),
            )
            .with_playhead_frame(state.current_frame())
            .with_app_edit_availability(state)
        })
    }

    /// Map the current timeline sequence into app UI timeline view models.
    ///
    /// The widget layer stays index-based and domain-light; this adapter is the
    /// app-side boundary that carries stable track/clip ids into emitted
    /// actions.
    pub fn from_sequence(
        sequence: &Sequence,
        selected_clips: &[SelectedClipRef],
        selected_tracks: &[TrackId],
    ) -> Self {
        Self::from_sequence_with_library(sequence, selected_clips, selected_tracks, None)
    }

    /// Same as [`from_sequence`] but attaches audio waveform peaks for
    /// clips whose backing assets exist in `library`.
    fn from_sequence_with_library(
        sequence: &Sequence,
        selected_clips: &[SelectedClipRef],
        selected_tracks: &[TrackId],
        library: Option<&AssetLibrary>,
    ) -> Self {
        let video_tracks = sequence.video_tracks.iter().enumerate().rev().map(|(index, track)| {
            let label = format!("V{}", index + 1);
            let mut view_track = timeline_track_from_sequence_track(
                track,
                true,
                selected_clips,
                selected_tracks,
                library,
            );
            view_track.label = label;
            (
                AppTimelineTrackRef { track_id: track.id, is_video_track: true },
                track.clips.iter().map(|clip| clip.id).collect::<Vec<_>>(),
                track.clips.iter().map(|clip| clip.nested_sequence_id).collect::<Vec<_>>(),
                view_track,
            )
        });
        let audio_tracks = sequence.audio_tracks.iter().enumerate().map(|(index, track)| {
            let mut view_track = timeline_track_from_sequence_track(
                track,
                false,
                selected_clips,
                selected_tracks,
                library,
            );
            view_track.label = format!("A{}", index + 1);
            (
                AppTimelineTrackRef { track_id: track.id, is_video_track: false },
                track.clips.iter().map(|clip| clip.id).collect::<Vec<_>>(),
                track.clips.iter().map(|clip| clip.nested_sequence_id).collect::<Vec<_>>(),
                view_track,
            )
        });

        let mut tracks = Vec::new();
        let mut track_refs = Vec::new();
        let mut clip_refs = Vec::new();
        let mut nested_sequence_refs = Vec::new();
        for (track_ref, clip_ids, nested_ids, track) in video_tracks.chain(audio_tracks) {
            track_refs.push(track_ref);
            clip_refs.push(clip_ids);
            nested_sequence_refs.push(nested_ids);
            tracks.push(track);
        }
        let empty_message = tracks
            .is_empty()
            .then(|| "当前序列没有轨道\n添加视频轨道或音频轨道后开始编辑".to_owned());
        Self {
            tracks,
            playhead_frame: sequence.playhead.frame.max(0),
            in_point_frame: sequence.in_point_frame(),
            out_point_frame: sequence.out_point_frame(),
            frame_rate: sequence.settings.frame_rate,
            enabled: true,
            empty_message,
            edit_availability: None,
            track_refs,
            clip_refs,
            nested_sequence_refs,
            waveform_display: WaveformDisplay::BottomAligned,
        }
    }

    /// Empty timeline shown before a sequence is open.
    pub fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            playhead_frame: 0,
            in_point_frame: 0,
            out_point_frame: None,
            frame_rate: Rational::FPS_30,
            enabled: false,
            empty_message: Some("未载入序列\n打开项目或创建序列以开始编辑".into()),
            edit_availability: Some(TimelineEditAvailability::from_app_state(&AppState::new())),
            track_refs: Vec::new(),
            clip_refs: Vec::new(),
            nested_sequence_refs: Vec::new(),
            waveform_display: WaveformDisplay::BottomAligned,
        }
    }

    /// Override the playhead frame when the app playback state is newer than
    /// the serialized sequence playhead.
    pub fn with_playhead_frame(mut self, frame: i64) -> Self {
        self.playhead_frame = frame.max(0);
        self
    }

    fn with_app_edit_availability(mut self, state: &AppState) -> Self {
        self.edit_availability = Some(TimelineEditAvailability::from_app_state(state));
        self
    }

    fn edit_command_available(&self, command: TimelineEditCommand) -> bool {
        self.edit_availability.is_none_or(|availability| availability.allows(command))
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

    fn open_nested_payload(
        &self,
        clip_ref: TimelineClipRef,
    ) -> Option<TimelineOpenNestedSequencePayload> {
        let sequence_id = self
            .nested_sequence_refs
            .get(clip_ref.track_index)?
            .get(clip_ref.clip_index)
            .copied()
            .flatten()?;
        Some(TimelineOpenNestedSequencePayload { sequence_id })
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

    fn track_move_payload(&self, movement: TimelineTrackMove) -> Option<TimelineMoveTrackPayload> {
        let source = self.track_identity(movement.track_ref)?;
        let target = *self.track_refs.get(movement.new_track_index)?;
        if source.is_video_track != target.is_video_track {
            return None;
        }
        let display_target_index = self
            .track_refs
            .iter()
            .take(movement.new_track_index)
            .filter(|track| track.is_video_track == source.is_video_track)
            .count();
        let target_kind_count = self
            .track_refs
            .iter()
            .filter(|track| track.is_video_track == source.is_video_track)
            .count();
        let target_index = if source.is_video_track {
            target_kind_count.saturating_sub(1).saturating_sub(display_target_index)
        } else {
            display_target_index
        };
        Some(TimelineMoveTrackPayload {
            track_id: source.track_id,
            is_video_track: source.is_video_track,
            target_index,
        })
    }

    fn asset_drop_payload(&self, drop: TimelineAssetDrop) -> Option<TimelineDropAssetPayload> {
        let target = self.track_identity(drop.track_ref)?;
        Some(TimelineDropAssetPayload {
            asset_id: drop.asset_id,
            target_track_id: target.track_id,
            is_video_track: target.is_video_track,
            frame: drop.frame.max(0),
        })
    }

    fn move_payload(&self, movement: TimelineClipMove) -> Option<TimelineMoveClipPayload> {
        let clip = self.clip_identity(movement.clip_ref)?;
        let target = *self.track_refs.get(movement.new_track_index)?;
        if clip.is_video_track != target.is_video_track {
            return None;
        }
        Some(TimelineMoveClipPayload {
            target_track_id: target.track_id,
            is_video_track: target.is_video_track,
            clip_id: clip.clip_id,
            frame: movement.new_start_frame.max(0),
        })
    }

    fn trim_payload(&self, trim: TimelineClipTrim) -> Option<TimelineTrimClipsPayload> {
        let clip = self.clip_identity(trim.clip_ref)?;
        let edge = match trim.edge {
            TimelineTrimEdge::In => TimelineTrimPayloadEdge::In,
            TimelineTrimEdge::Out => TimelineTrimPayloadEdge::Out,
        };
        let frame = match trim.edge {
            TimelineTrimEdge::In => trim.new_start_frame,
            TimelineTrimEdge::Out => trim.new_start_frame + trim.new_duration_frames,
        };
        Some(TimelineTrimClipsPayload {
            clip_ids: vec![clip.clip_id],
            edge,
            frame: frame.max(0),
        })
    }
}

/// Inspector fixture data independent from a concrete property widget tree.
#[derive(Debug, Clone)]
pub struct InspectorPanelModel {
    /// Selected clip targeted by value edits, if the model is backed by app state.
    pub selected_clip: Option<SelectedClipRef>,
    /// Empty-state message shown instead of clip controls when no target exists.
    pub empty_message: Option<String>,
    /// Selected effect nested inside the selected clip.
    pub selected_effect_id: Option<EffectId>,
    /// Whether inspector controls may dispatch mutations for the selected clip.
    pub is_editable: bool,
    /// Human-readable reason shown when a selected clip cannot be edited.
    pub edit_disabled_reason: Option<String>,
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

/// Effect row data shown by the app UI inspector.
#[derive(Debug, Clone)]
pub struct InspectorEffectModel {
    /// Effect instance id targeted by enable/disable actions.
    pub effect_id: EffectId,
    /// Human-readable effect name.
    pub label: String,
    /// Whether the effect is enabled.
    pub enabled: bool,
    /// Per-property editor values, one per row in the PropertyBag.
    pub properties: Vec<InspectorEffectPropertyModel>,
}

/// One property row inside an effect inspector section.
#[derive(Debug, Clone)]
pub struct InspectorEffectPropertyModel {
    /// Namespaced property path, e.g. `effect.<id>.exposure`.
    pub path: String,
    /// Human-readable property name from the descriptor.
    pub label: String,
    /// The evaluated value at the current playback time.
    pub value: PropertyValue,
    /// UI min/max bounds extracted from the descriptor.
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    /// Whether the property supports animation.
    pub is_animatable: bool,
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
        let is_editable = !selected_clip_track_is_locked(state, resolved_selection);
        Self {
            selected_clip: Some(resolved_selection),
            empty_message: None,
            selected_effect_id: state.primary_selected_effect().and_then(|selection| {
                (selection.clip.clip_id == resolved_selection.clip_id)
                    .then_some(selection.effect_id)
            }),
            is_editable,
            edit_disabled_reason: (!is_editable).then(|| "所选剪辑所在轨道已锁定".to_owned()),
            enabled: !clip.is_disabled,
            opacity,
            tint: clip
                .solid_color
                .or_else(|| timeline_clip_color(clip, resolved_selection.is_video_track))
                .unwrap_or_else(|| current_theme().colors.media_video),
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
                .map(|effect| {
                    let time_ticks = timecode_to_ticks(time);
                    InspectorEffectModel {
                        effect_id: effect.id,
                        label: effect_display_name(&effect.effect_type),
                        enabled: effect.is_enabled,
                        properties: effect
                            .properties
                            .iter()
                            .map(|(path, property)| InspectorEffectPropertyModel {
                                path: path.to_string(),
                                label: property.descriptor.display_name.clone(),
                                value: property.evaluate(time_ticks),
                                min: property.descriptor.ui_metadata.min,
                                max: property.descriptor.ui_metadata.max,
                                step: property.descriptor.ui_metadata.step,
                                is_animatable: property.descriptor.is_animatable,
                            })
                            .collect(),
                    }
                })
                .collect(),
        }
    }

    pub fn empty() -> Self {
        Self {
            selected_clip: None,
            empty_message: Some("未选择剪辑\n选择剪辑、图层或效果后，可在这里调整参数。".into()),
            selected_effect_id: None,
            is_editable: false,
            edit_disabled_reason: None,
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
            empty_message: None,
            selected_effect_id: None,
            is_editable: false,
            edit_disabled_reason: None,
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

/// 导出 panel data independent from a concrete widget tree.
#[derive(Debug, Clone)]
pub struct ExportPanelModel {
    pub queue_count: usize,
    pub presets: Vec<ExportPresetOptionModel>,
    pub selected_preset_idx: usize,
    pub sequences: Vec<ExportSequenceOptionModel>,
    pub selected_sequence_id: Option<SequenceId>,
    pub range: TimelineExportRange,
    pub output_path: String,
    pub status: Option<(String, bool)>,
    pub jobs: Vec<ExportJobModel>,
    pub can_clear_completed_jobs: bool,
}

#[derive(Debug, Clone)]
pub struct ExportPresetOptionModel {
    pub label: String,
    pub preset: ExportPreset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSequenceOptionModel {
    pub id: SequenceId,
    pub name: String,
    pub video_clips: usize,
    pub audio_clips: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportJobModel {
    pub id: JobId,
    pub title: String,
    pub status: String,
    pub progress_percent: u8,
    pub can_cancel: bool,
    pub is_completed: bool,
}

/// Node graph panel data independent from a concrete widget tree.
#[derive(Debug, Clone)]
pub struct NodeGraphPanelModel {
    pub title: String,
    pub subtitle: String,
    pub selected_clip: Option<SelectedClipRef>,
    pub nodes: Vec<NodeGraphNode>,
    pub edges: Vec<NodeGraphEdge>,
    pub node_targets: Vec<NodeGraphNodeTarget>,
    pub selected_node_id: Option<String>,
}

/// Application target attached to one domain-light graph node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeGraphNodeTarget {
    pub node_id: String,
    pub target: NodeGraphTarget,
}

/// Semantic target for a node graph item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeGraphTarget {
    Clip,
    Effect(EffectId),
    Output,
}

impl NodeGraphPanelModel {
    pub fn from_app_state(state: &AppState) -> Self {
        let Some(sequence) = state.sequence.as_ref() else {
            return Self::empty();
        };
        let Some(selection) = state.primary_selected_clip() else {
            return Self::empty();
        };
        let Some((selected_clip, clip)) = clip_for_selection(sequence, &selection) else {
            return Self::empty();
        };

        let clip_label = clip.label.clone().unwrap_or_else(|| default_clip_label(clip));
        let selected_effect_id = state.primary_selected_effect().and_then(|selection| {
            (selection.clip.clip_id == selected_clip.clip_id).then_some(selection.effect_id)
        });
        let mut nodes = vec![NodeGraphNode::new("source", "源")
            .with_subtitle(clip_source_subtitle(clip))
            .with_accent(current_theme().colors.node_source)
            .disabled(clip.is_disabled)];
        let mut node_targets = vec![NodeGraphNodeTarget {
            node_id: "source".to_owned(),
            target: NodeGraphTarget::Clip,
        }];
        let mut edges = Vec::new();
        let mut previous_id = "source".to_owned();

        for (index, effect) in clip.effects.iter().enumerate() {
            let id = format!("effect:{}", effect.id);
            nodes.push(
                NodeGraphNode::new(id.clone(), effect_display_name(&effect.effect_type))
                    .with_subtitle(format!(
                        "{} {}",
                        effect_badge(&effect.effect_type),
                        index + 1
                    ))
                    .with_accent(effect_node_accent(&effect.effect_type))
                    .disabled(!effect.is_enabled),
            );
            node_targets.push(NodeGraphNodeTarget {
                node_id: id.clone(),
                target: NodeGraphTarget::Effect(effect.id),
            });
            edges.push(NodeGraphEdge::new(previous_id, id.clone()));
            previous_id = id;
        }

        nodes.push(
            NodeGraphNode::new("output", "输出")
                .with_subtitle("合成")
                .with_accent(current_theme().colors.node_output),
        );
        node_targets.push(NodeGraphNodeTarget {
            node_id: "output".to_owned(),
            target: NodeGraphTarget::Output,
        });
        edges.push(NodeGraphEdge::new(previous_id, "output"));

        Self {
            title: "节点图".to_owned(),
            subtitle: format!("{clip_label} / {} 个效果", clip.effects.len()),
            selected_clip: Some(selected_clip),
            nodes,
            edges,
            node_targets,
            selected_node_id: selected_effect_id
                .map(|effect_id| format!("effect:{effect_id}"))
                .or_else(|| Some("source".to_owned())),
        }
    }

    pub fn empty() -> Self {
        Self {
            title: "节点图".to_owned(),
            subtitle: "选择剪辑以检查渲染链".to_owned(),
            selected_clip: None,
            nodes: Vec::new(),
            edges: Vec::new(),
            node_targets: Vec::new(),
            selected_node_id: None,
        }
    }
}

impl ExportPanelModel {
    pub fn from_app_state(state: &AppState) -> Self {
        let presets = builtin_export_presets()
            .into_iter()
            .map(|option| ExportPresetOptionModel { label: option.label, preset: option.preset })
            .collect::<Vec<_>>();
        let max_preset = presets.len().saturating_sub(1);
        let selected_preset_idx = state.export_draft.selected_preset_idx.min(max_preset);
        let sequences = state
            .export_sequences_snapshot()
            .into_iter()
            .map(|sequence| ExportSequenceOptionModel {
                id: sequence.id,
                name: sequence.name,
                video_clips: sequence.video_tracks.iter().map(|track| track.clips.len()).sum(),
                audio_clips: sequence.audio_tracks.iter().map(|track| track.clips.len()).sum(),
            })
            .collect::<Vec<_>>();
        let selected_sequence_id = state
            .export_draft
            .selected_sequence_id
            .filter(|id| sequences.iter().any(|sequence| sequence.id == *id))
            .or(state.active_sequence_id)
            .or(state.default_sequence_id)
            .filter(|id| sequences.iter().any(|sequence| sequence.id == *id))
            .or_else(|| sequences.first().map(|sequence| sequence.id));

        let jobs = state.render_queue.list_jobs();
        let queue_count = jobs.len();
        let can_clear_completed_jobs = jobs.iter().any(|job| {
            matches!(
                job.status,
                JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled
            )
        });
        let jobs = jobs
            .into_iter()
            .rev()
            .take(6)
            .map(|job| ExportJobModel {
                id: job.id,
                title: export_job_title(job.config.output_path.as_path()),
                status: export_job_status_label(&job.status),
                progress_percent: (job.progress.clamp(0.0, 1.0) * 100.0).round() as u8,
                can_cancel: matches!(
                    job.status,
                    JobStatus::Pending | JobStatus::Rendering { .. } | JobStatus::Encoding
                ),
                is_completed: matches!(
                    job.status,
                    JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled
                ),
            })
            .collect();

        Self {
            queue_count,
            presets,
            selected_preset_idx,
            sequences,
            selected_sequence_id,
            range: state.export_draft.range,
            output_path: state.export_draft.output_path.clone(),
            status: state.status_hint.clone(),
            jobs,
            can_clear_completed_jobs,
        }
    }

    fn selected_preset(&self) -> Option<&ExportPreset> {
        self.presets
            .get(self.selected_preset_idx)
            .or_else(|| self.presets.first())
            .map(|option| &option.preset)
    }

    fn can_enqueue(&self) -> bool {
        self.selected_preset().is_some()
            && self.selected_sequence_id.is_some()
            && !self.output_path.trim().is_empty()
    }

    fn can_choose_output(&self) -> bool {
        self.selected_preset().is_some() && self.selected_sequence_id.is_some()
    }

    fn can_select_range(&self) -> bool {
        self.selected_sequence_id.is_some()
    }

    fn readiness_status(&self) -> String {
        if let Some((message, is_error)) = &self.status {
            if *is_error {
                return format!("错误：{message}");
            }
            return message.clone();
        }
        if self.selected_sequence_id.is_none() {
            "导出前请打开或选择序列".to_owned()
        } else if self.selected_preset().is_none() {
            "没有可用导出预设".to_owned()
        } else if self.output_path.trim().is_empty() {
            "选择输出路径后即可加入队列".to_owned()
        } else {
            "就绪".to_owned()
        }
    }

    fn enqueue_payload(&self) -> Option<ExportEnqueuePayload> {
        let preset = self.selected_preset()?.clone();
        let sequence_id = self.selected_sequence_id?;
        let output_path = self.output_path.trim();
        if output_path.is_empty() {
            return None;
        }
        Some(ExportEnqueuePayload {
            preset,
            sequence_id: Some(sequence_id),
            range: self.range,
            output_path: output_path.into(),
        })
    }

    fn selected_sequence(&self) -> Option<&ExportSequenceOptionModel> {
        self.selected_sequence_id
            .and_then(|id| self.sequences.iter().find(|sequence| sequence.id == id))
            .or_else(|| self.sequences.first())
    }
}

/// Build the default app UI dock tree from explicit panel models.
pub fn build_dock_tree(models: AppUiPanelModels) -> DockSplitter {
    build_dock_tree_for_preset(models, WorkspacePreset::Editing)
}

/// Build an app UI dock tree for one built-in workspace preset.
pub fn build_dock_tree_for_preset(
    models: AppUiPanelModels,
    preset: WorkspacePreset,
) -> DockSplitter {
    match preset {
        WorkspacePreset::Editing | WorkspacePreset::Custom => editing_workspace(models),
        WorkspacePreset::Color => color_workspace(models),
        WorkspacePreset::Audio => audio_workspace(models),
        WorkspacePreset::Compositing => compositing_workspace(models),
        WorkspacePreset::Export => export_workspace(models),
    }
}

/// Build an app UI dock tree from a persisted custom workspace layout.
pub fn build_dock_tree_from_layout(
    models: AppUiPanelModels,
    layout: &AppUiWorkspaceLayout,
) -> Option<DockSplitter> {
    match layout {
        AppUiWorkspaceLayout::Split { direction, ratio, first, second } => Some(DockSplitter::new(
            *direction,
            *ratio,
            dock_widget_from_layout(models.clone(), first),
            dock_widget_from_layout(models, second),
        )),
        AppUiWorkspaceLayout::Panel { .. } => None,
    }
}

fn dock_widget_from_layout(
    models: AppUiPanelModels,
    layout: &AppUiWorkspaceLayout,
) -> Box<dyn Widget> {
    match layout {
        AppUiWorkspaceLayout::Split { direction, ratio, first, second } => {
            Box::new(DockSplitter::new(
                *direction,
                *ratio,
                dock_widget_from_layout(models.clone(), first),
                dock_widget_from_layout(models, second),
            ))
        }
        AppUiWorkspaceLayout::Panel { kind, active_index, hidden_tabs, tabs } => {
            let tab_kinds = if tabs.is_empty() {
                visible_tabs_for_slot(*kind, hidden_tabs)
            } else {
                tabs.clone()
            };
            let mut panel = slot_with_tabs(tab_kinds, models);
            if let Some(panel) =
                panel.as_mut().as_any_mut().and_then(|any| any.downcast_mut::<DockPanel>())
            {
                panel.set_active_index(*active_index);
            }
            panel
        }
    }
}

fn editing_workspace(models: AppUiPanelModels) -> DockSplitter {
    let viewer_and_inspector = DockSplitter::new(
        SplitDirection::Horizontal,
        0.70,
        slot(PanelKind::Viewer, models.clone()),
        slot(PanelKind::Inspector, models.clone()),
    );
    let upper = DockSplitter::new(
        SplitDirection::Horizontal,
        0.22,
        slot(PanelKind::Assets, models.clone()),
        Box::new(viewer_and_inspector),
    );
    DockSplitter::new(
        SplitDirection::Vertical,
        0.66,
        Box::new(upper),
        slot(PanelKind::Timeline, models),
    )
}

fn color_workspace(models: AppUiPanelModels) -> DockSplitter {
    let right = DockSplitter::new(
        SplitDirection::Vertical,
        0.56,
        slot(PanelKind::Inspector, models.clone()),
        slot(PanelKind::Effects, models.clone()),
    );
    let center = DockSplitter::new(
        SplitDirection::Vertical,
        0.66,
        slot(PanelKind::Viewer, models.clone()),
        slot(PanelKind::Timeline, models.clone()),
    );
    DockSplitter::new(
        SplitDirection::Horizontal,
        0.72,
        Box::new(center),
        Box::new(right),
    )
}

fn audio_workspace(models: AppUiPanelModels) -> DockSplitter {
    let upper = DockSplitter::new(
        SplitDirection::Horizontal,
        0.34,
        slot(PanelKind::Assets, models.clone()),
        slot(PanelKind::Viewer, models.clone()),
    );
    let lower = DockSplitter::new(
        SplitDirection::Horizontal,
        0.74,
        slot(PanelKind::Timeline, models.clone()),
        slot(PanelKind::Inspector, models.clone()),
    );
    DockSplitter::new(
        SplitDirection::Vertical,
        0.38,
        Box::new(upper),
        Box::new(lower),
    )
}

fn compositing_workspace(models: AppUiPanelModels) -> DockSplitter {
    let right = DockSplitter::new(
        SplitDirection::Vertical,
        0.58,
        slot(PanelKind::Viewer, models.clone()),
        slot(PanelKind::Inspector, models.clone()),
    );
    let left = DockSplitter::new(
        SplitDirection::Vertical,
        0.68,
        slot(PanelKind::NodeGraph, models.clone()),
        slot(PanelKind::Effects, models.clone()),
    );
    DockSplitter::new(
        SplitDirection::Horizontal,
        0.42,
        Box::new(left),
        Box::new(right),
    )
}

fn export_workspace(models: AppUiPanelModels) -> DockSplitter {
    DockSplitter::new(
        SplitDirection::Horizontal,
        0.42,
        slot(PanelKind::Export, models.clone()),
        slot(PanelKind::Viewer, models),
    )
}

/// Build a dock tree using built-in demo panel fixtures.
#[cfg(test)]
pub fn build_demo_dock_tree() -> DockSplitter {
    build_dock_tree(AppUiPanelModels::demo())
}

fn slot(kind: PanelKind, models: AppUiPanelModels) -> Box<dyn Widget> {
    slot_with_hidden_tabs(kind, models, &[])
}

fn slot_with_hidden_tabs(
    kind: PanelKind,
    models: AppUiPanelModels,
    hidden_tabs: &[PanelKind],
) -> Box<dyn Widget> {
    slot_with_tabs(visible_tabs_for_slot(kind, hidden_tabs), models)
}

fn slot_with_tabs(mut tab_kinds: Vec<PanelKind>, models: AppUiPanelModels) -> Box<dyn Widget> {
    if tab_kinds.is_empty() {
        tab_kinds.push(PanelKind::Viewer);
    }
    let owner = tab_kinds[0];
    let tabs = tab_kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| TabInfo {
            label: kind.display_name().to_string(),
            active: index == 0,
            panel_kind: Some(*kind),
        })
        .collect();
    Box::new(
        DockPanel::new(owner, tabs, move |kind, _active| {
            panel_content_for_slot(kind, &models)
        })
        .on_panel_drop(|panel, target, area, tab_index| {
            app_shell_relocate_panel_action(AppShellRelocatePanelPayload {
                panel,
                target,
                area: dock_drop_area_payload(area),
                tab_index,
            })
        }),
    )
}

fn dock_drop_area_payload(area: DockPanelDropArea) -> DockDropAreaPayload {
    match area {
        DockPanelDropArea::Center => DockDropAreaPayload::Center,
        DockPanelDropArea::Left => DockDropAreaPayload::Left,
        DockPanelDropArea::Right => DockDropAreaPayload::Right,
        DockPanelDropArea::Top => DockDropAreaPayload::Top,
        DockPanelDropArea::Bottom => DockDropAreaPayload::Bottom,
    }
}

fn visible_tabs_for_slot(kind: PanelKind, hidden_tabs: &[PanelKind]) -> Vec<PanelKind> {
    default_tabs_for_slot(kind)
        .iter()
        .copied()
        .filter(|tab| *tab == kind || !hidden_tabs.contains(tab))
        .collect()
}

fn default_tabs_for_slot(kind: PanelKind) -> &'static [PanelKind] {
    match kind {
        PanelKind::Assets => &[PanelKind::Assets, PanelKind::Effects],
        PanelKind::Viewer => &[PanelKind::Viewer],
        PanelKind::Timeline => &[PanelKind::Timeline],
        PanelKind::Inspector => &[PanelKind::Inspector],
        PanelKind::Effects => &[PanelKind::Effects],
        PanelKind::NodeGraph => &[PanelKind::NodeGraph],
        PanelKind::Export => &[PanelKind::Export],
    }
}

fn panel_content_for_slot(kind: PanelKind, models: &AppUiPanelModels) -> Box<dyn Widget> {
    match kind {
        PanelKind::Assets => Box::new(ScrollView::new(Some(Box::new(asset_grid(&models.assets))))),
        PanelKind::Effects => Box::new(panel_list(&models.effects)),
        PanelKind::Viewer => Box::new(viewer_panel(&models.viewer)),
        PanelKind::Timeline => Box::new(timeline_panel(&models.timeline)),
        PanelKind::Export => Box::new(ScrollView::new(Some(Box::new(export_panel(
            &models.export,
        ))))),
        PanelKind::Inspector => Box::new(ScrollView::new(Some(Box::new(inspector_panel(
            &models.inspector,
        ))))),
        PanelKind::NodeGraph => Box::new(node_graph_panel(&models.node_graph)),
    }
}

fn viewer_panel(model: &ViewerPanelModel) -> ViewerSurface {
    let surface = ViewerSurface::new(model.title.clone(), model.width, model.height)
        .with_status(model.status.clone())
        .with_status_tone(model.status_tone)
        .with_resolution_label(model.resolution_label.clone())
        .with_timecode_label(model.timecode_label.clone())
        .with_frame_label(model.frame_label.clone())
        .with_duration_label(model.duration_label.clone())
        .with_zoom_label(model.zoom_label.clone())
        .with_zoom_scale(model.zoom_scale)
        .with_preview_quality_label(model.preview_quality_label.clone())
        .playing(model.playing)
        .enabled(model.enabled)
        .on_control(viewer_control_action)
        .on_zoom(|scale| viewer_set_zoom_scale_action(ViewerSetZoomScalePayload { scale }))
        .on_preview_quality(|scale| {
            viewer_set_preview_resolution_scale_action(ViewerSetPreviewResolutionScalePayload {
                scale,
            })
        });
    let surface = with_viewer_transport_icons(surface);
    let surface = if let Some(message) = model.empty_message.clone() {
        surface.with_empty_message(message)
    } else {
        surface
    };
    match model.frame_image.clone() {
        Some(frame_image) => surface.with_frame_image(frame_image),
        None => surface,
    }
}

fn with_viewer_transport_icons(mut surface: ViewerSurface) -> ViewerSurface {
    if let (Ok(play), Ok(pause)) = (
        AppIcon::PlayFilled.vector_icon(),
        AppIcon::PauseFilled.vector_icon(),
    ) {
        surface = surface.with_play_pause_icons(play, pause);
    }
    for (control, icon) in [
        (ViewerControl::JumpStart, AppIcon::HomeFrameFilled),
        (ViewerControl::StepBack, AppIcon::LeftFrameFilled),
        (ViewerControl::StepForward, AppIcon::RightFrameFilled),
        (ViewerControl::JumpEnd, AppIcon::EndFrameFilled),
    ] {
        if let Ok(vector_icon) = icon.vector_icon() {
            surface = surface.with_control_icon(control, vector_icon);
        }
    }
    surface
}

fn with_timeline_toolbar_icons(mut timeline: TimelineView) -> TimelineView {
    for (slot, icon) in [
        (TimelineToolbarIconSlot::SelectTool, AppIcon::CursorFilled),
        (TimelineToolbarIconSlot::BladeTool, AppIcon::Cut),
        (TimelineToolbarIconSlot::Snapping, AppIcon::Magnet),
        (
            TimelineToolbarIconSlot::MarkInAtPlayhead,
            AppIcon::BracketsLeft,
        ),
        (
            TimelineToolbarIconSlot::MarkOutAtPlayhead,
            AppIcon::BracketsRight,
        ),
    ] {
        if let Ok(vector_icon) = icon.vector_icon() {
            timeline = timeline.with_toolbar_icon(slot, vector_icon);
        }
    }
    for (slot, icon) in [
        (
            TimelineTrackControlIconSlot::VisibilityOn,
            AppIcon::EyeVisible,
        ),
        (
            TimelineTrackControlIconSlot::VisibilityOff,
            AppIcon::EyeHidden,
        ),
        (TimelineTrackControlIconSlot::MuteOff, AppIcon::Speaker),
        (TimelineTrackControlIconSlot::MuteOn, AppIcon::SpeakerMuted),
        (TimelineTrackControlIconSlot::LockOff, AppIcon::Unlock),
        (TimelineTrackControlIconSlot::LockOn, AppIcon::Lock),
    ] {
        if let Ok(vector_icon) = icon.vector_icon() {
            timeline = timeline.with_track_control_icon(slot, vector_icon);
        }
    }
    timeline
}

fn viewer_control_action(control: ViewerControl) -> Action {
    match control {
        ViewerControl::MarkIn => Action::MarkInAtPlayhead,
        ViewerControl::MarkOut => Action::MarkOutAtPlayhead,
        ViewerControl::JumpStart => Action::GoToStart,
        ViewerControl::StepBack => Action::StepBack,
        ViewerControl::PlayPause => Action::TogglePlay,
        ViewerControl::StepForward => Action::StepForward,
        ViewerControl::JumpEnd => Action::GoToEnd,
    }
}

fn timeline_track_from_sequence_track(
    track: &Track,
    is_video_track: bool,
    selected_clips: &[SelectedClipRef],
    selected_tracks: &[TrackId],
    library: Option<&AssetLibrary>,
) -> TimelineTrack {
    let muted = track.is_muted;
    let locked = track.is_locked;
    let visible = track.is_visible;
    let selected = selected_tracks.contains(&track.id);
    let clips = track
        .clips
        .iter()
        .map(|clip| timeline_clip_from_sequence_clip(is_video_track, clip, selected_clips, library))
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
    library: Option<&AssetLibrary>,
) -> TimelineClip {
    let selected = selected_clips.iter().any(|selection| selection.clip_id == clip.id);
    let label = clip.label.clone().unwrap_or_else(|| default_clip_label(clip));
    let is_video = is_video_track;
    let kind = if clip.is_adjustment_layer() {
        TimelineClipKind::Adjustment
    } else if clip.is_nested_sequence() {
        TimelineClipKind::NestedSequence
    } else if clip.is_solid_color() {
        TimelineClipKind::SolidColor
    } else if is_video {
        TimelineClipKind::Video
    } else {
        TimelineClipKind::Audio
    };
    let mut view = TimelineClip::new(
        label,
        clip.position.frame.max(0),
        clip.duration.frame.max(1),
    )
    .kind(kind)
    .selected(selected)
    .disabled(clip.is_disabled)
    .nested(clip.is_nested_sequence());
    if let Some(color) = timeline_clip_color(clip, is_video) {
        view = view.with_color(color);
    }
    if kind == TimelineClipKind::Audio {
        if let Some(lib) = library {
            if let Ok(Some(record)) = lib.get_asset(clip.asset_id) {
                view = view.with_source_identity(
                    record.id,
                    clip.source_in.to_secs(),
                    clip.source_out.to_secs(),
                );
            }
        }
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
    let colors = current_theme().colors.clone();
    if clip.is_adjustment_layer() {
        Some(colors.timeline_clip_adjustment)
    } else if clip.is_nested_sequence() {
        Some(colors.timeline_clip_nested)
    } else if is_video_track {
        Some(colors.timeline_clip_video)
    } else {
        Some(colors.timeline_clip_audio)
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

fn asset_grid_item_from_asset(
    asset: AssetRecord,
    thumbnails: Option<&dyn AssetThumbnailSource>,
    proxy_mode: bool,
) -> AssetGridItem {
    let badge = asset_kind_badge(&asset.kind);
    let accent = asset_kind_accent(&asset.kind);
    let icon = asset_kind_icon(&asset.kind);
    let thumbnail_state = thumbnails.map(|source| source.thumbnail_for_asset(&asset));
    let context_menu_items = asset_grid_asset_context_menu_items(&asset, proxy_mode);
    let offline = asset_is_offline(&asset);
    let proxied = proxy_mode && matches!(asset.kind, AssetKind::Video);
    let duration_label = asset_duration_label(asset.media_info.duration);
    let mut item = AssetGridItem::new(asset.id.to_string(), asset.name, accent)
        .with_subtitle(duration_label)
        .with_badge(badge)
        .with_drag_payload(DragPayload::Asset(asset.id))
        .with_activate_action(assets_prepare_drag_action(AssetsPrepareDragPayload {
            asset_id: asset.id,
        }))
        .with_context_menu(context_menu_items)
        .renamable(true);
    if offline {
        item = item.with_badge_tone("离线", AssetGridBadgeTone::Warning);
    } else if proxied {
        item = item.with_badge_tone("代理", AssetGridBadgeTone::Success);
    }
    if let Some(state) = thumbnail_state {
        item = match state {
            AssetThumbnailState::Unavailable => item,
            AssetThumbnailState::Loading => item.with_thumbnail_loading(),
            AssetThumbnailState::Failed => item.with_thumbnail_failed(),
            AssetThumbnailState::Ready(thumbnail) => item.with_thumbnail(thumbnail),
        };
    }
    with_asset_icon(item, icon)
}

fn asset_grid_asset_context_menu_items(asset: &AssetRecord, proxy_mode: bool) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if asset_has_file_manager_target(asset) {
        items.push(asset_menu_item(
            MenuItem::new(
                "在文件管理器中显示",
                app_shell_reveal_in_file_manager_action(AppShellRevealInFileManagerPayload {
                    path: asset.path.clone(),
                }),
            ),
            AppIcon::FolderOpenFilled,
        ));
        if asset_is_offline(asset) {
            items.push(asset_menu_item(
                MenuItem::new(
                    "重新链接媒体...",
                    app_shell_relink_asset_dialog_action(AppShellRelinkAssetDialogPayload {
                        asset_id: asset.id,
                    }),
                ),
                AppIcon::Import,
            ));
        } else if matches!(asset.kind, AssetKind::Video) {
            let (label, enabled) = if proxy_mode {
                ("关闭代理模式", false)
            } else {
                ("启用代理模式", true)
            };
            items.push(asset_menu_item(
                MenuItem::new(
                    label,
                    assets_set_proxy_mode_action(AssetsSetProxyModePayload {
                        asset_id: asset.id,
                        enabled,
                    }),
                ),
                AppIcon::Film,
            ));
        }
        items.push(MenuItem::separator());
    }
    items.push(asset_menu_item(
        MenuItem::new(
            "删除素材",
            assets_delete_asset_action(AssetsDeleteAssetPayload { asset_id: asset.id }),
        ),
        AppIcon::Trash,
    ));
    items
}

fn asset_has_file_manager_target(asset: &AssetRecord) -> bool {
    matches!(asset.kind, AssetKind::Video | AssetKind::Audio)
}

fn asset_is_offline(asset: &AssetRecord) -> bool {
    asset_has_file_manager_target(asset) && !asset.path.exists()
}

fn asset_grid_items_from_library_records(
    folders: &[FolderRecord],
    assets: Vec<AssetRecord>,
    current_folder: Option<&FolderRecord>,
    thumbnails: Option<&dyn AssetThumbnailSource>,
    proxy_mode_assets: Option<&std::collections::HashSet<AssetId>>,
) -> Vec<AssetGridItem> {
    let mut items =
        Vec::with_capacity(folders.len() + assets.len() + usize::from(current_folder.is_some()));
    let parent_id = current_folder.map(|folder| folder.id.as_str());
    if let Some(folder) = current_folder {
        items.push(asset_grid_parent_item(folder.parent_id.clone()));
    }
    for folder in folders.iter().filter(|folder| folder.parent_id.as_deref() == parent_id) {
        let item_count = asset_folder_direct_item_count(folders, &assets, folder);
        items.push(asset_grid_item_from_folder(folder, item_count));
    }
    items.extend(
        assets
            .into_iter()
            .filter(|asset| asset.folder_id.as_deref() == parent_id)
            .map(|asset| {
                let proxy_mode = proxy_mode_assets.is_some_and(|ids| ids.contains(&asset.id));
                asset_grid_item_from_asset(asset, thumbnails, proxy_mode)
            }),
    );
    items
}

fn asset_folder_direct_item_count(
    folders: &[FolderRecord],
    assets: &[AssetRecord],
    folder: &FolderRecord,
) -> usize {
    let folder_id = folder.id.as_str();
    let child_folders = folders
        .iter()
        .filter(|child| child.parent_id.as_deref() == Some(folder_id))
        .count();
    let child_assets = assets
        .iter()
        .filter(|asset| asset.folder_id.as_deref() == Some(folder_id))
        .count();
    child_folders + child_assets
}

fn asset_grid_parent_item(parent_id: Option<String>) -> AssetGridItem {
    let (title, badge) = if parent_id.is_some() {
        ("返回", "上级")
    } else {
        ("全部素材", "全部")
    };
    with_asset_icon(
        AssetGridItem::new("asset-folder-up", title, current_theme().colors.secondary)
            .with_badge(badge)
            .with_activate_action(assets_open_folder_action(AssetsOpenFolderPayload {
                folder_id: parent_id,
            })),
        AppIcon::CaretLeft,
    )
}

fn asset_grid_item_from_folder(folder: &FolderRecord, item_count: usize) -> AssetGridItem {
    let badge = format!("{item_count} 项");
    let folder_id = folder.id.clone();
    with_asset_icon(
        AssetGridItem::new(
            format!("folder:{folder_id}"),
            folder.name.clone(),
            current_theme().colors.secondary,
        )
        .with_badge(badge)
        .with_drag_payload(DragPayload::AssetFolder(folder_id.clone()))
        .with_activate_action(assets_open_folder_action(AssetsOpenFolderPayload {
            folder_id: Some(folder_id.clone()),
        }))
        .with_context_menu(vec![asset_menu_item(
            MenuItem::new(
                "删除文件夹",
                assets_delete_folder_action(AssetsDeleteFolderPayload { folder_id }),
            ),
            AppIcon::Trash,
        )])
        .renamable(true),
        AppIcon::Folder,
    )
}

fn asset_empty_item(
    id: impl Into<String>,
    title: impl Into<String>,
    _subtitle: impl Into<String>,
    accent: Color,
    icon: AppIcon,
) -> AssetGridItem {
    with_asset_icon(AssetGridItem::new(id, title, accent).disabled(true), icon)
}

fn with_asset_icon(item: AssetGridItem, icon: AppIcon) -> AssetGridItem {
    match icon.vector_icon() {
        Ok(icon) => item.with_icon(icon),
        Err(_) => item,
    }
}

fn effect_icon_button(
    icon: AppIcon,
    fallback_label: &'static str,
    tooltip: &'static str,
    enabled: bool,
    action: Action,
) -> Box<dyn Widget> {
    match icon.icon_button() {
        Ok(button) => Box::new(button.with_tooltip(tooltip).enabled(enabled).on_click(action)),
        Err(_) => Box::new(Button::new(fallback_label).enabled(enabled).on_click(action)),
    }
}

fn color_picker_trigger(color: Color) -> ColorPickerTrigger {
    let mut trigger = ColorPickerTrigger::new(color);
    if let Ok(icon) = AppIcon::Eyedropper.vector_icon() {
        trigger.picker_mut().set_eyedropper_icon(icon);
    }
    trigger
}

fn asset_kind_badge(kind: &AssetKind) -> &'static str {
    match kind {
        AssetKind::Video => "视频",
        AssetKind::Audio => "音频",
        AssetKind::AdjustmentLayer => "序列",
        AssetKind::SolidColor => "图片",
    }
}

fn asset_duration_label(duration: std::time::Duration) -> String {
    let total_secs = duration.as_secs();
    if total_secs == 0 {
        return String::new();
    }
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn asset_kind_accent(kind: &AssetKind) -> Color {
    let colors = current_theme().colors.clone();
    match kind {
        AssetKind::Video => colors.media_video,
        AssetKind::Audio => colors.media_audio,
        AssetKind::AdjustmentLayer => colors.media_adjustment,
        AssetKind::SolidColor => colors.media_solid,
    }
}

fn asset_kind_icon(kind: &AssetKind) -> AppIcon {
    match kind {
        AssetKind::Video => AppIcon::Film,
        AssetKind::Audio => AppIcon::Music,
        AssetKind::AdjustmentLayer => AppIcon::Grid,
        AssetKind::SolidColor => AppIcon::Rectangle,
    }
}

fn effect_node_accent(effect_type: &EffectType) -> Color {
    let colors = current_theme().colors.clone();
    match effect_type {
        EffectType::Plugin(_) => colors.effect_plugin,
        EffectType::GaussianBlur | EffectType::Sharpen => colors.effect_filter,
        EffectType::Lut3D => colors.effect_lut,
        EffectType::ChromaKey | EffectType::LumaKey => colors.effect_key,
        _ => colors.effect_default,
    }
}

fn clip_source_subtitle(clip: &Clip) -> &'static str {
    if clip.is_adjustment_layer() {
        "Adjustment"
    } else if clip.is_nested_sequence() {
        "Nested Sequence"
    } else if clip.is_solid_color() {
        "Solid Color"
    } else {
        "Media Clip"
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

fn selected_clip_track_is_locked(state: &AppState, selection: SelectedClipRef) -> bool {
    let Some(sequence) = state.sequence.as_ref() else {
        return false;
    };
    if selection.is_video_track {
        sequence
            .video_tracks
            .iter()
            .find(|track| track.clips.iter().any(|clip| clip.id == selection.clip_id))
            .is_some_and(|track| track.is_locked)
    } else {
        sequence
            .audio_tracks
            .iter()
            .find(|track| track.clips.iter().any(|clip| clip.id == selection.clip_id))
            .is_some_and(|track| track.is_locked)
    }
}

fn selected_clip_tracks_are_editable(state: &AppState) -> bool {
    let Some(sequence) = state.sequence.as_ref() else {
        return false;
    };
    if state.selected_clips().is_empty() {
        return false;
    }
    state.selected_clips().iter().all(|selection| {
        let tracks = if selection.is_video_track {
            &sequence.video_tracks
        } else {
            &sequence.audio_tracks
        };
        tracks
            .iter()
            .find(|track| track.clips.iter().any(|clip| clip.id == selection.clip_id))
            .is_some_and(|track| !track.is_locked)
    })
}

fn panel_list(model: &PanelListModel) -> PanelList {
    let mut list = PanelList::new(model.title.clone(), model.items.clone())
        .with_subtitle(model.subtitle.clone())
        .with_embedded_panel_chrome();
    if let Some(placeholder) = &model.filter_placeholder {
        list = list.with_filter(placeholder.clone());
    }
    if model.title == "Effects" {
        list = list.with_row_height(30.0);
        if let (Ok(collapsed), Ok(expanded)) = (
            AppIcon::CaretRight.vector_icon(),
            AppIcon::CaretDown.vector_icon(),
        ) {
            list = list.with_tree_icons(collapsed, expanded);
        }
    }
    list
}

fn asset_grid(model: &AssetGridModel) -> AssetGrid {
    let mut grid = AssetGrid::new(model.title.clone(), model.items.clone())
        .with_subtitle(model.subtitle.clone())
        .with_embedded_panel_chrome()
        .on_rename(asset_grid_rename_action);
    if let Some(placeholder) = &model.filter_placeholder {
        grid = grid.with_filter(placeholder.clone());
    }
    if model.accepts_file_drop {
        let drop_folder_id = model.current_folder_id.clone();
        grid = grid
            .on_drop(move |payload, _position| match payload {
                DragPayload::File(paths) if !paths.is_empty() => {
                    Some(assets_import_files_action(AssetsImportFilesPayload {
                        paths: paths.clone(),
                        folder_id: drop_folder_id.clone(),
                    }))
                }
                DragPayload::Asset(asset_id) => {
                    Some(assets_move_asset_action(AssetsMoveAssetPayload {
                        asset_id: *asset_id,
                        folder_id: drop_folder_id.clone(),
                    }))
                }
                DragPayload::AssetFolder(folder_id) => {
                    Some(assets_move_folder_action(AssetsMoveFolderPayload {
                        folder_id: folder_id.clone(),
                        parent_folder_id: drop_folder_id.clone(),
                    }))
                }
                DragPayload::AssetSelection { assets, folders } => {
                    move_asset_selection_action(assets, folders, drop_folder_id.clone())
                }
                _ => None,
            })
            .on_item_drop(|payload, _index, item| {
                let target_folder_id = item.id.strip_prefix("folder:")?.to_string();
                match payload {
                    DragPayload::Asset(asset_id) => {
                        Some(assets_move_asset_action(AssetsMoveAssetPayload {
                            asset_id: *asset_id,
                            folder_id: Some(target_folder_id),
                        }))
                    }
                    DragPayload::AssetFolder(folder_id) => {
                        if folder_id == &target_folder_id {
                            Some(Action::NoOp)
                        } else {
                            Some(assets_move_folder_action(AssetsMoveFolderPayload {
                                folder_id: folder_id.clone(),
                                parent_folder_id: Some(target_folder_id),
                            }))
                        }
                    }
                    DragPayload::AssetSelection { assets, folders } => {
                        move_asset_selection_action(assets, folders, Some(target_folder_id))
                    }
                    _ => None,
                }
            })
            .with_context_menu(asset_grid_context_menu_items(
                model.current_folder_id.as_deref(),
            ))
            .with_selection_context_menu(asset_grid_selection_context_menu_items);
    }
    grid
}

fn asset_grid_rename_action(_index: usize, item: &AssetGridItem, name: &str) -> Action {
    match item.drag_payload.as_ref() {
        Some(DragPayload::Asset(asset_id)) => {
            assets_rename_asset_action(AssetsRenameAssetPayload {
                asset_id: *asset_id,
                name: name.to_owned(),
            })
        }
        Some(DragPayload::AssetFolder(folder_id)) => {
            assets_rename_folder_action(AssetsRenameFolderPayload {
                folder_id: folder_id.clone(),
                name: name.to_owned(),
            })
        }
        _ => Action::NoOp,
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

fn asset_grid_selection_context_menu_items(
    _indices: &[usize],
    items: &[&AssetGridItem],
) -> Vec<MenuItem> {
    let mut asset_ids = Vec::new();
    let mut folder_ids = Vec::new();
    for item in items {
        match item.drag_payload.as_ref() {
            Some(DragPayload::Asset(asset_id)) => asset_ids.push(*asset_id),
            Some(DragPayload::AssetFolder(folder_id)) => folder_ids.push(folder_id.clone()),
            _ => {}
        }
    }
    if asset_ids.is_empty() && folder_ids.is_empty() {
        return Vec::new();
    }
    vec![asset_menu_item(
        MenuItem::new(
            "Delete selected",
            assets_delete_selection_action(AssetsDeleteSelectionPayload { asset_ids, folder_ids }),
        ),
        AppIcon::Trash,
    )]
}

fn move_asset_selection_action(
    assets: &[mondrian_core::types::AssetId],
    folders: &[String],
    target_folder_id: Option<String>,
) -> Option<Action> {
    let folder_ids: Vec<String> = folders
        .iter()
        .filter(|folder_id| Some(folder_id.as_str()) != target_folder_id.as_deref())
        .cloned()
        .collect();
    if assets.is_empty() && folder_ids.is_empty() {
        return Some(Action::NoOp);
    }
    Some(assets_move_selection_action(AssetsMoveSelectionPayload {
        asset_ids: assets.to_vec(),
        folder_ids,
        target_folder_id,
    }))
}

fn asset_grid_context_menu_items(current_folder_id: Option<&str>) -> Vec<MenuItem> {
    vec![
        asset_menu_item(
            MenuItem::new(
                "导入媒体...",
                app_shell_import_media_dialog_action_with_target(ImportMediaDialogPayload {
                    folder_id: current_folder_id.map(str::to_owned),
                }),
            ),
            AppIcon::Import,
        ),
        MenuItem::separator(),
        asset_menu_item(
            MenuItem::submenu(
                "新建",
                vec![
                    asset_menu_item(
                        MenuItem::new(
                            "调整图层",
                            assets_create_adjustment_layer_action(AssetsCreateAssetPayload {
                                folder_id: current_folder_id.map(str::to_owned),
                            }),
                        ),
                        AppIcon::Grid,
                    ),
                    asset_menu_item(
                        MenuItem::new(
                            "纯色",
                            assets_create_solid_color_action(AssetsCreateAssetPayload {
                                folder_id: current_folder_id.map(str::to_owned),
                            }),
                        ),
                        AppIcon::Rectangle,
                    ),
                    asset_menu_item(
                        MenuItem::new(
                            "文件夹",
                            assets_create_folder_action(AssetsCreateFolderPayload {
                                parent_folder_id: current_folder_id.map(str::to_owned),
                            }),
                        ),
                        AppIcon::Folder,
                    ),
                ],
            ),
            AppIcon::Grid,
        ),
    ]
}

fn asset_menu_item(item: MenuItem, icon: AppIcon) -> MenuItem {
    match icon.vector_icon() {
        Ok(icon) => item.with_icon(icon),
        Err(_) => item,
    }
}

#[cfg(test)]
fn demo_asset_model() -> AssetGridModel {
    let colors = current_theme().colors.clone();
    AssetGridModel::new(
        "Assets",
        vec![
            with_asset_icon(
                AssetGridItem::new("demo-footage", "Footage", colors.media_video)
                    .with_subtitle("Imported camera clips")
                    .with_badge("12")
                    .with_select_action(demo_panel_action("assets.select.footage"))
                    .with_activate_action(demo_panel_action("assets.activate.footage")),
                AppIcon::Film,
            ),
            with_asset_icon(
                AssetGridItem::new("demo-audio", "Audio", colors.media_audio)
                    .with_subtitle("Music, voiceover, and ambience")
                    .with_badge("5")
                    .with_select_action(demo_panel_action("assets.select.audio"))
                    .with_activate_action(demo_panel_action("assets.activate.audio")),
                AppIcon::Music,
            ),
            with_asset_icon(
                AssetGridItem::new("demo-images", "Images", colors.media_solid)
                    .with_subtitle("Still frames and references")
                    .with_badge("8")
                    .with_select_action(demo_panel_action("assets.select.images"))
                    .with_activate_action(demo_panel_action("assets.activate.images")),
                AppIcon::Rectangle,
            ),
            with_asset_icon(
                AssetGridItem::new("demo-sequences", "Sequences", colors.media_adjustment)
                    .with_subtitle("Nested edits and reusable timelines")
                    .with_badge("2")
                    .with_select_action(demo_panel_action("assets.select.sequences"))
                    .with_activate_action(demo_panel_action("assets.activate.sequences")),
                AppIcon::Film,
            ),
        ],
    )
    .with_subtitle("Project library")
    .with_filter_placeholder("搜索素材")
    .accepts_file_drop(true)
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
    sequence.video_tracks[0].name = "V1".to_string();
    sequence.video_tracks[1].name = "V2".to_string();
    sequence.video_tracks[2].name = "V3".to_string();
    sequence.audio_tracks[0].name = "A1".to_string();
    sequence.audio_tracks[1].name = "A2".to_string();

    let tb = sequence.time_base();
    let nested_id = SequenceId::new();

    let mut adjustment =
        Clip::new_adjustment_layer(AssetId::new(), TimeCode::new(36, tb), TimeCode::new(84, tb));
    adjustment.label = Some("Adjustment".to_string());
    sequence.video_tracks[2].add_clip(adjustment).expect("add adjustment");

    let mut title = Clip::new_nested_sequence(
        nested_id,
        TimeCode::new(132, tb),
        TimeCode::new(48, tb),
        Some("Title".to_string()),
    );
    title.solid_color = Some(Color::from_hex(0x4B7BE5));
    sequence.video_tracks[2].add_clip(title).expect("add title");

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
    sequence.video_tracks[0].add_clip(interview).expect("add interview");

    let mut cutaway = Clip::new(
        AssetId::new(),
        TimeCode::new(104, tb),
        TimeCode::new(72, tb),
    );
    cutaway.label = Some("Cutaway".to_string());
    cutaway.solid_color = Some(Color::from_hex(0x2F855A));
    sequence.video_tracks[0].add_clip(cutaway).expect("add cutaway");

    let mut outro = Clip::new(
        AssetId::new(),
        TimeCode::new(190, tb),
        TimeCode::new(44, tb),
    );
    outro.label = Some("Outro".to_string());
    outro.solid_color = Some(Color::from_hex(0x744210));
    sequence.video_tracks[0].add_clip(outro).expect("add outro");

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
    let timeline = TimelineView::new(model.tracks.clone())
        .enabled(model.enabled)
        .with_header_width(144.0)
        .with_frame_rate(model.frame_rate)
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
        .on_track_move({
            let action_model = action_model.clone();
            move |movement, _track| {
                action_model
                    .track_move_payload(movement)
                    .map(timeline_move_track_action)
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
        .on_asset_drop({
            let action_model = action_model.clone();
            move |drop, _track| {
                action_model
                    .asset_drop_payload(drop)
                    .map(timeline_drop_asset_action)
                    .unwrap_or(Action::NoOp)
            }
        })
        .on_edit_command({
            let action_model = action_model.clone();
            move |command| timeline_edit_command_action(&action_model, command)
        })
        .on_edit_command_available({
            let action_model = action_model.clone();
            move |command| action_model.edit_command_available(command)
        })
        .on_edit_command_shortcut(timeline_edit_command_shortcut_label)
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
                    .map(timeline_trim_clips_action)
                    .unwrap_or_else(|| Action::NoOp)
            }
        })
        .on_in_out_point(|point, frame| {
            timeline_set_in_out_point_action(TimelineSetInOutPointPayload {
                point: timeline_in_out_point_payload_kind(point),
                frame: frame.max(0),
            })
        })
        .on_seek(timeline_seek_action)
        .with_waveform_lookup(|asset_id, start_secs, end_secs, pixel_width| {
            AudioWaveformCache::try_with(|cache| {
                cache.lookup(asset_id, 0, start_secs, end_secs, pixel_width)
            })
            .flatten()
        })
        .with_waveform_display(model.waveform_display);
    let timeline = if let Some(message) = model.empty_message.clone() {
        timeline.with_empty_message(message)
    } else {
        timeline
    };
    with_timeline_toolbar_icons(timeline)
}

fn timeline_in_out_point_payload_kind(point: TimelineInOutPoint) -> TimelineInOutPointPayloadKind {
    match point {
        TimelineInOutPoint::In => TimelineInOutPointPayloadKind::In,
        TimelineInOutPoint::Out => TimelineInOutPointPayloadKind::Out,
    }
}

fn timeline_edit_command_action(
    model: &TimelinePanelModel,
    command: TimelineEditCommand,
) -> Action {
    match command {
        TimelineEditCommand::CutSelection => Action::Cut,
        TimelineEditCommand::CopySelection => Action::Copy,
        TimelineEditCommand::PasteAtPlayhead => Action::Paste,
        TimelineEditCommand::DuplicateSelection => Action::Duplicate,
        TimelineEditCommand::DeleteSelection => Action::DeleteSelection,
        TimelineEditCommand::RippleDeleteSelection => Action::RippleDeleteSelection,
        TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
        TimelineEditCommand::TrimSelectionInToPlayhead => {
            timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::In },
            )
        }
        TimelineEditCommand::TrimSelectionOutToPlayhead => {
            timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::Out },
            )
        }
        TimelineEditCommand::RollSelectedCutToPlayhead => {
            timeline_roll_selected_cut_to_playhead_action()
        }
        TimelineEditCommand::EnableSelection => {
            timeline_set_selected_clips_enabled_action(TimelineSetSelectedClipsEnabledPayload {
                enabled: true,
            })
        }
        TimelineEditCommand::DisableSelection => {
            timeline_set_selected_clips_enabled_action(TimelineSetSelectedClipsEnabledPayload {
                enabled: false,
            })
        }
        TimelineEditCommand::OpenNestedSequence(clip_ref) => model
            .open_nested_payload(clip_ref)
            .map(timeline_open_nested_sequence_action)
            .unwrap_or(Action::NoOp),
        TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
        TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
        TimelineEditCommand::ClearInOutPoints => timeline_clear_in_out_points_action(),
        TimelineEditCommand::TogglePlayback => Action::TogglePlay,
    }
}

fn timeline_edit_command_shortcut_label(command: TimelineEditCommand) -> Option<String> {
    let action = match command {
        TimelineEditCommand::OpenNestedSequence(_) => return None,
        TimelineEditCommand::ClearInOutPoints => return None,
        TimelineEditCommand::TogglePlayback => return Some("Space".to_owned()),
        TimelineEditCommand::CutSelection => Action::Cut,
        TimelineEditCommand::CopySelection => Action::Copy,
        TimelineEditCommand::PasteAtPlayhead => Action::Paste,
        TimelineEditCommand::DuplicateSelection => Action::Duplicate,
        TimelineEditCommand::DeleteSelection => Action::DeleteSelection,
        TimelineEditCommand::RippleDeleteSelection => Action::RippleDeleteSelection,
        TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
        TimelineEditCommand::TrimSelectionInToPlayhead => {
            timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::In },
            )
        }
        TimelineEditCommand::TrimSelectionOutToPlayhead => {
            timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::Out },
            )
        }
        TimelineEditCommand::RollSelectedCutToPlayhead => {
            timeline_roll_selected_cut_to_playhead_action()
        }
        TimelineEditCommand::EnableSelection => {
            timeline_set_selected_clips_enabled_action(TimelineSetSelectedClipsEnabledPayload {
                enabled: true,
            })
        }
        TimelineEditCommand::DisableSelection => {
            timeline_set_selected_clips_enabled_action(TimelineSetSelectedClipsEnabledPayload {
                enabled: false,
            })
        }
        TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
        TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
    };
    shortcut_label_for_action(&action)
}

fn node_graph_panel(model: &NodeGraphPanelModel) -> NodeGraphView {
    let selected_clip = model.selected_clip;
    let node_targets = model.node_targets.clone();
    let mut graph = NodeGraphView::new(model.nodes.clone(), model.edges.clone())
        .with_title(model.title.clone())
        .with_subtitle(model.subtitle.clone())
        .on_select(move |node_id| node_graph_node_action(selected_clip, &node_targets, node_id));
    if model.nodes.is_empty() {
        graph = graph.with_empty_message(model.subtitle.clone()).disabled();
    }
    if let Some(selected_node_id) = &model.selected_node_id {
        graph = graph.with_selected_node(selected_node_id.clone());
    }
    graph
}

fn export_panel(model: &ExportPanelModel) -> PropertyPanel {
    let preset_label = model
        .presets
        .get(model.selected_preset_idx)
        .or_else(|| model.presets.first())
        .map(|option| option.label.clone())
        .unwrap_or_else(|| "No presets".to_owned());
    let preset_items = model
        .presets
        .iter()
        .enumerate()
        .map(|(index, option)| {
            MenuItem::new(
                option.label.clone(),
                export_set_draft_action(ExportDraftUpdatePayload::PresetIndex(index)),
            )
        })
        .collect::<Vec<_>>();
    let preset_dropdown = Dropdown::new(preset_label, preset_items).with_max_visible_items(6);

    let selected_sequence = model.selected_sequence();
    let sequence_label = selected_sequence
        .map(|sequence| sequence.name.clone())
        .unwrap_or_else(|| "没有序列".to_owned());
    let sequence_items = model
        .sequences
        .iter()
        .map(|sequence| {
            MenuItem::new(
                sequence.name.clone(),
                export_set_draft_action(ExportDraftUpdatePayload::Sequence(Some(sequence.id))),
            )
        })
        .collect::<Vec<_>>();
    let sequence_dropdown =
        Dropdown::new(sequence_label, sequence_items).enabled(!model.sequences.is_empty());

    let range_dropdown = Dropdown::new(
        export_range_label(model.range),
        vec![
            MenuItem::new(
                export_range_label(TimelineExportRange::SequenceInOut),
                export_set_draft_action(ExportDraftUpdatePayload::Range(
                    TimelineExportRange::SequenceInOut,
                )),
            ),
            MenuItem::new(
                export_range_label(TimelineExportRange::EntireSequence),
                export_set_draft_action(ExportDraftUpdatePayload::Range(
                    TimelineExportRange::EntireSequence,
                )),
            ),
        ],
    )
    .enabled(model.can_select_range());

    let selected_preset = model.selected_preset();
    let output_extension = selected_preset.map(export_preset_extension).unwrap_or("mp4").to_owned();
    let can_choose_output = model.can_choose_output();
    let output_browse = AppIcon::Folder
        .text_button_or_label("Browse...")
        .on_click(app_shell_export_output_dialog_action(
            ExportOutputDialogPayload {
                default_file_name: export_default_file_name(selected_preset),
                extension: output_extension,
            },
        ))
        .enabled(can_choose_output);
    let output_input = TextInput::new("输出路径")
        .with_text(model.output_path.clone())
        .enabled(can_choose_output)
        .on_change(|text| {
            export_set_draft_action(ExportDraftUpdatePayload::OutputPath(text.to_owned()))
        });
    let output_row = FlexContainer::row(vec![
        FlexChild::flex(Box::new(output_input), 1.0),
        FlexChild::fixed(Box::new(output_browse)),
    ])
    .with_gap(8.0);
    let enqueue_action = model.enqueue_payload().map(export_enqueue_action).unwrap_or(Action::NoOp);
    let enqueue_button = AppIcon::Export
        .text_button_or_label("Add to queue")
        .enabled(model.can_enqueue())
        .on_click(enqueue_action);
    let clear_completed_button = AppIcon::Trash
        .text_button_or_label("Clear completed")
        .enabled(model.can_clear_completed_jobs)
        .on_click(export_clear_completed_action());

    let sequence_summary = selected_sequence
        .map(|sequence| {
            format!(
                "时间线渲染（V{} / A{}）",
                sequence.video_clips, sequence.audio_clips
            )
        })
        .unwrap_or_else(|| "No exportable sequence".to_owned());
    let status_text = model.readiness_status();

    let mut queue_section = PropertySection::new("队列");
    if model.jobs.is_empty() {
        queue_section = queue_section.with_row(PropertyRow::new(
            "Jobs",
            Box::new(Label::new("没有排队任务").muted()),
        ));
    } else {
        for job in &model.jobs {
            queue_section = queue_section.with_row(export_job_row(job));
        }
        queue_section =
            queue_section.with_row(PropertyRow::new("", Box::new(clear_completed_button)));
    }

    PropertyPanel::new("导出")
        .with_subtitle(format!("{} queued job(s)", model.queue_count))
        .with_section(
            PropertySection::new("预设")
                .with_row(PropertyRow::new("预设", Box::new(preset_dropdown)))
                .with_row(
                    PropertyRow::new(
                        "Details",
                        Box::new(
                            Label::new(export_preset_summary(selected_preset)).muted().wrapped(),
                        ),
                    )
                    .with_height(54.0),
                ),
        )
        .with_section(
            PropertySection::new("输入")
                .with_row(PropertyRow::new("序列", Box::new(sequence_dropdown)))
                .with_row(PropertyRow::new(
                    "Summary",
                    Box::new(Label::new(sequence_summary).muted()),
                ))
                .with_row(PropertyRow::new("范围", Box::new(range_dropdown))),
        )
        .with_section(
            PropertySection::new("输出")
                .with_row(PropertyRow::new("路径", Box::new(output_row)))
                .with_row(PropertyRow::new(
                    "Status",
                    Box::new(Label::new(status_text).muted()),
                ))
                .with_row(PropertyRow::new("", Box::new(enqueue_button))),
        )
        .with_section(queue_section)
}

fn export_job_row(job: &ExportJobModel) -> PropertyRow {
    let summary = FlexContainer::column(vec![
        FlexChild::fixed(Box::new(
            Label::new(job.title.clone()).with_padding(0.0, 0.0),
        )),
        FlexChild::fixed(Box::new(
            Label::new(format!("{} / {}%", job.status, job.progress_percent))
                .muted()
                .wrapped()
                .with_padding(0.0, 0.0),
        )),
    ])
    .with_gap(4.0);

    let content: Box<dyn Widget> = if job.can_cancel {
        let cancel = AppIcon::Trash.text_button_or_label("取消").on_click(
            export_cancel_job_action(ExportJobTargetPayload { job_id: job.id }),
        );
        Box::new(
            FlexContainer::row(vec![
                FlexChild::flex(Box::new(summary), 1.0),
                FlexChild::fixed(Box::new(cancel)),
            ])
            .with_gap(8.0),
        )
    } else {
        Box::new(summary)
    };

    PropertyRow::new("任务", content).with_height(if job.can_cancel { 48.0 } else { 42.0 })
}

fn export_default_file_name(preset: Option<&ExportPreset>) -> String {
    format!(
        "mondrian-export.{}",
        preset.map(export_preset_extension).unwrap_or("mp4")
    )
}

fn export_preset_summary(preset: Option<&ExportPreset>) -> String {
    let Some(preset) = preset else {
        return "No preset available".to_owned();
    };
    let resolution = preset
        .resolution
        .as_ref()
        .map(|resolution| format!("{}x{}", resolution.width, resolution.height))
        .unwrap_or_else(|| "Follow sequence".to_owned());
    let (codec, bitrate) = match &preset.video {
        VideoCodecConfig::H264 { bitrate_kbps, .. } => (
            "H.264",
            bitrate_kbps
                .map(|value| format!("{value} kbps"))
                .unwrap_or_else(|| "Auto".to_owned()),
        ),
        VideoCodecConfig::H265 { bitrate_kbps, .. } => (
            "H.265",
            bitrate_kbps
                .map(|value| format!("{value} kbps"))
                .unwrap_or_else(|| "Auto".to_owned()),
        ),
        VideoCodecConfig::Av1 { .. } => ("AV1", "Auto".to_owned()),
        VideoCodecConfig::ProRes { .. } => ("ProRes", "N/A".to_owned()),
        VideoCodecConfig::Gif { .. } => ("GIF", "N/A".to_owned()),
    };
    format!(
        "{resolution} / {codec} / {bitrate} / .{}",
        export_preset_extension(preset)
    )
}

fn export_range_label(range: TimelineExportRange) -> &'static str {
    match range {
        TimelineExportRange::SequenceInOut => "序列入点/出点",
        TimelineExportRange::EntireSequence => "整个序列",
        TimelineExportRange::WorkArea { .. } => "工作区域",
    }
}

fn export_job_title(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn export_job_status_label(status: &JobStatus) -> String {
    match status {
        JobStatus::Pending => "Pending".to_owned(),
        JobStatus::Rendering { frame, total_frames } => {
            format!("Rendering {}/{}", frame, total_frames)
        }
        JobStatus::Encoding => "Encoding".to_owned(),
        JobStatus::Completed => "Completed".to_owned(),
        JobStatus::Failed(reason) => format!("Failed: {reason}"),
        JobStatus::Cancelled => "Cancelled".to_owned(),
    }
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
    let can_edit = has_target && model.is_editable;
    let subtitle = model.edit_disabled_reason.as_deref().unwrap_or(if has_target {
        "Selected clip"
    } else {
        "No clip selected"
    });
    if let Some(message) = model.empty_message.as_deref().filter(|message| !message.is_empty()) {
        let (title, description) = message
            .split_once('\n')
            .map_or((message, ""), |(title, description)| (title, description));
        let mut panel = PropertyPanel::with_options(
            "检查器",
            PropertyPanelOptions {
                label_width: 0.0,
                control_gap: 0.0,
                row_height: 28.0,
                section_gap: 0.0,
                ..PropertyPanelOptions::default()
            },
        )
        .with_embedded_panel_chrome()
        .with_empty_state(title, description);
        if let Ok(icon) = AppIcon::Info.vector_icon() {
            panel = panel.with_empty_state_icon(icon);
        }
        return panel;
    }
    let mut tint = color_picker_trigger(model.tint).enabled(can_edit);
    tint.picker_mut().set_area_mode(model.tint_area_mode);
    let curve = CurveEditor::with_points(model.curve_points.clone())
        .enabled(can_edit)
        .on_change(move |points| inspector_curve_action(selected_clip, points));
    let mut panel = PropertyPanel::new("检查器")
        .with_subtitle(subtitle)
        .with_embedded_panel_chrome()
        .with_section(
            PropertySection::new("剪辑样式")
                .with_row(PropertyRow::new(
                    "启用",
                    Box::new(
                        Checkbox::new("启用效果", model.enabled)
                            .enabled(can_edit)
                            .on_change(move |value| inspector_bool_action(selected_clip, value)),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "不透明度",
                    numeric_slider_input_control(
                        model.opacity,
                        0.0,
                        100.0,
                        Some(1.0),
                        0,
                        can_edit,
                        move |value| inspector_value_action(selected_clip, "opacity", value),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "Tint",
                    Box::new(
                        tint.on_change(move |color| inspector_color_action(selected_clip, color)),
                    ),
                )),
        );

    panel = panel.with_section(
        PropertySection::new("变换")
            .with_row(PropertyRow::new(
                "Position X",
                numeric_slider_input_control(
                    model.position_x,
                    -4096.0,
                    4096.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_transform_action(
                            selected_clip,
                            InspectorClipTransformField::PositionX,
                            value,
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "Position Y",
                numeric_slider_input_control(
                    model.position_y,
                    -4096.0,
                    4096.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_transform_action(
                            selected_clip,
                            InspectorClipTransformField::PositionY,
                            value,
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "Scale",
                numeric_slider_input_control(
                    model.scale_percent,
                    0.0,
                    400.0,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_transform_action(
                            selected_clip,
                            InspectorClipTransformField::ScalePercent,
                            value,
                        )
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "Rotation",
                numeric_slider_input_control(
                    model.rotation_degrees,
                    -180.0,
                    180.0,
                    Some(0.1),
                    1,
                    can_edit,
                    move |value| {
                        inspector_transform_action(
                            selected_clip,
                            InspectorClipTransformField::RotationDegrees,
                            value,
                        )
                    },
                ),
            )),
    );

    panel = panel.with_section(
        PropertySection::new("时间")
            .with_row(PropertyRow::new(
                "In",
                numeric_slider_input_control(
                    model.in_frame,
                    0.0,
                    model.max_frame,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_timing_action(selected_clip, TimelineTrimPayloadEdge::In, value)
                    },
                ),
            ))
            .with_row(PropertyRow::new(
                "Out",
                numeric_slider_input_control(
                    model.out_frame,
                    0.0,
                    model.max_frame,
                    Some(1.0),
                    0,
                    can_edit,
                    move |value| {
                        inspector_timing_action(selected_clip, TimelineTrimPayloadEdge::Out, value)
                    },
                ),
            )),
    );

    if !model.effects.is_empty() {
        for (index, effect) in model.effects.iter().enumerate() {
            let effect_id = effect.effect_id;
            let can_move_up = can_edit && index > 0;
            let can_move_down = can_edit && index + 1 < model.effects.len();
            let mut section = PropertySection::new(effect.label.clone())
                .selected(model.selected_effect_id == Some(effect_id))
                .on_select(inspector_effect_select_action(selected_clip, effect_id))
                .with_row(PropertyRow::new(
                    "控制",
                    Box::new(
                        FlexContainer::row(vec![
                            FlexChild::flex(
                                Box::new(
                                    Checkbox::new("启用", effect.enabled)
                                        .enabled(can_edit)
                                        .on_change(move |enabled| {
                                            inspector_effect_enabled_action(
                                                selected_clip,
                                                effect_id,
                                                enabled,
                                            )
                                        }),
                                ),
                                1.0,
                            ),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretUp,
                                "Up",
                                "Move effect up",
                                can_move_up,
                                inspector_reorder_effect_action(
                                    selected_clip,
                                    index,
                                    index.saturating_sub(1),
                                ),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretDown,
                                "Down",
                                "Move effect down",
                                can_move_down,
                                inspector_reorder_effect_action(
                                    selected_clip,
                                    index,
                                    (index + 1).min(model.effects.len().saturating_sub(1)),
                                ),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::Trash,
                                "Remove",
                                "Remove effect",
                                can_edit,
                                inspector_remove_effect_row_action(selected_clip, effect_id),
                            )),
                        ])
                        .with_gap(8.0),
                    ),
                ));
            for property in &effect.properties {
                section = section.with_row(effect_property_row(
                    property,
                    can_edit,
                    selected_clip,
                    effect_id,
                ));
            }
            panel = panel.with_section(section);
        }
    }

    panel.with_section(
        PropertySection::new("动画")
            .with_row(PropertyRow::new("曲线", Box::new(curve)).with_height(118.0)),
    )
}

fn numeric_slider_input_control(
    value: f32,
    min: f32,
    max: f32,
    step: Option<f32>,
    decimals: usize,
    enabled: bool,
    action: impl Fn(f32) -> Action + 'static,
) -> Box<dyn Widget> {
    let action: Rc<dyn Fn(f32) -> Action> = Rc::new(action);
    let mut slider = Slider::new(value, min, max).enabled(enabled);
    if let Some(step) = step.filter(|step| step.is_finite() && *step > 0.0) {
        slider = slider.with_step(step);
    }
    let slider_action = Rc::clone(&action);
    slider = slider.on_change(move |value| slider_action(value));

    let mut input = NumberInput::new(value as f64, min as f64, max as f64)
        .with_width(72.0)
        .with_decimals(decimals)
        .enabled(enabled);
    if let Some(step) = step.filter(|step| step.is_finite() && *step > 0.0) {
        input = input.with_step(step as f64);
    }
    let input_action = Rc::clone(&action);
    input = input.on_change(move |value| input_action(value as f32));

    Box::new(
        FlexContainer::row(vec![
            FlexChild::flex(Box::new(slider), 1.0),
            FlexChild::fixed(Box::new(input)),
        ])
        .with_gap(8.0),
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
        return timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![selection.clip_id],
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

fn inspector_effect_select_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> Action {
    if let Some(selection) = selection {
        return inspector_select_effect_action(InspectorSelectEffectPayload {
            clip: inspector_clip_payload(selection),
            effect_id,
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

fn inspector_reorder_effect_action(
    selection: Option<SelectedClipRef>,
    from: usize,
    to: usize,
) -> Action {
    if from == to {
        return Action::NoOp;
    }
    selection
        .map(|selection| Action::ReorderEffects { clip_id: selection.clip_id, from, to })
        .unwrap_or(Action::NoOp)
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

fn node_graph_node_action(
    selection: Option<SelectedClipRef>,
    targets: &[NodeGraphNodeTarget],
    node_id: &str,
) -> Action {
    let Some(selection) = selection else {
        return Action::NoOp;
    };
    match targets
        .iter()
        .find_map(|entry| (entry.node_id == node_id).then_some(entry.target))
    {
        Some(NodeGraphTarget::Effect(effect_id)) => {
            inspector_select_effect_action(InspectorSelectEffectPayload {
                clip: inspector_clip_payload(selection),
                effect_id,
            })
        }
        Some(NodeGraphTarget::Clip | NodeGraphTarget::Output) => {
            node_graph_clip_action(Some(selection))
        }
        None => Action::NoOp,
    }
}

fn node_graph_clip_action(selection: Option<SelectedClipRef>) -> Action {
    selection
        .map(|selection| {
            timeline_select_clip_action(TimelineSelectClipPayload {
                track_id: selection.track_id,
                is_video_track: selection.is_video_track,
                clip_id: selection.clip_id,
            })
        })
        .unwrap_or(Action::NoOp)
}

fn inspector_clip_payload(selection: SelectedClipRef) -> InspectorClipRefPayload {
    InspectorClipRefPayload {
        track_id: selection.track_id,
        is_video_track: selection.is_video_track,
        clip_id: selection.clip_id,
    }
}

fn effect_property_row(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> PropertyRow {
    let row = PropertyRow::new(
        property.label.clone(),
        effect_property_value_widget(
            property,
            can_edit,
            selection,
            effect_id,
            property.path.clone(),
        ),
    );
    if let Some(height) = effect_property_row_height(&property.value) {
        row.with_height(height)
    } else {
        row
    }
}

fn effect_property_row_height(value: &PropertyValue) -> Option<f32> {
    let components: usize = match value {
        PropertyValue::Vec2(_) => 2,
        PropertyValue::Vec3(_) => 3,
        PropertyValue::Vec4(_) => 4,
        _ => return None,
    };
    Some(components as f32 * 30.0 + components.saturating_sub(1) as f32 * 4.0)
}

/// Build a typed value widget for one effect property row.
///
/// Widget construction depends on the `PropertyValue` variant present in the
/// snapshot. The returned widget dispatches `INSPECTOR_SET_EFFECT_PROPERTY`
/// through the existing inspector custom-action path.
fn effect_property_value_widget(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    path: String,
) -> Box<dyn Widget> {
    match &property.value {
        PropertyValue::Bool(value) => {
            let selected_clip = selection;
            Box::new(
                Checkbox::new(&property.label, *value).enabled(can_edit).on_change(move |v| {
                    inspector_effect_property_action(
                        selected_clip,
                        effect_id,
                        &path,
                        PropertyValue::Bool(v),
                    )
                }),
            )
        }
        PropertyValue::Float(value) => {
            let (min, max) = numeric_property_range(property.min, property.max, 0.0, 1.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control(
                *value,
                min,
                max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value),
                can_edit,
                move |v| {
                    inspector_effect_property_action(
                        selected_clip,
                        effect_id,
                        &path,
                        PropertyValue::Float(v.clamp(min, max)),
                    )
                },
            )
        }
        PropertyValue::Double(value) => {
            let (min, max) = numeric_property_range(property.min, property.max, 0.0, 1.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control(
                *value as f32,
                min,
                max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value as f32),
                can_edit,
                move |v: f32| {
                    inspector_effect_property_action(
                        selected_clip,
                        effect_id,
                        &path,
                        PropertyValue::Double((v as f64).clamp(min as f64, max as f64)),
                    )
                },
            )
        }
        PropertyValue::Int(value) => {
            let (min, max) = numeric_property_range(property.min, property.max, 0.0, 100.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control(
                *value as f32,
                min,
                max,
                property_step(property.step, Some(1.0)),
                0,
                can_edit,
                move |v: f32| {
                    inspector_effect_property_action(
                        selected_clip,
                        effect_id,
                        &path,
                        PropertyValue::Int((v.round() as i64).clamp(min as i64, max as i64)),
                    )
                },
            )
        }
        PropertyValue::Color(value) => {
            let selected_clip = selection;
            let path = path.clone();
            let trigger = color_picker_trigger(*value).enabled(can_edit);
            Box::new(trigger.on_change(move |color| {
                inspector_effect_property_action(
                    selected_clip,
                    effect_id,
                    &path,
                    PropertyValue::Color(color),
                )
            }))
        }
        PropertyValue::Text(value) => {
            let text = value.clone();
            let max_width = 180.0;
            if text.len() > 60 {
                Box::new(Label::new(text).with_max_width(max_width))
            } else {
                let selected_clip = selection;
                let path = path.clone();
                Box::new(
                    TextInput::new(text).enabled(can_edit).on_change(move |text| {
                        inspector_effect_property_action(
                            selected_clip,
                            effect_id,
                            &path,
                            PropertyValue::Text(text.to_string()),
                        )
                    }),
                )
            }
        }
        PropertyValue::Vec2(value) => vector_property_widget(
            &["X", "Y"],
            &[value.x, value.y],
            property,
            can_edit,
            selection,
            effect_id,
            path,
            |values| PropertyValue::Vec2(glam::Vec2::new(values[0], values[1])),
        ),
        PropertyValue::Vec3(value) => vector_property_widget(
            &["X", "Y", "Z"],
            &[value.x, value.y, value.z],
            property,
            can_edit,
            selection,
            effect_id,
            path,
            |values| PropertyValue::Vec3(glam::Vec3::new(values[0], values[1], values[2])),
        ),
        PropertyValue::Vec4(value) => vector_property_widget(
            &["X", "Y", "Z", "W"],
            value,
            property,
            can_edit,
            selection,
            effect_id,
            path,
            |values| PropertyValue::Vec4([values[0], values[1], values[2], values[3]]),
        ),
    }
}

fn vector_property_widget(
    labels: &[&'static str],
    values: &[f32],
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    path: String,
    build_value: fn(&[f32]) -> PropertyValue,
) -> Box<dyn Widget> {
    let (min, max) = numeric_property_range(property.min, property.max, 0.0, 1.0);
    let values = values
        .iter()
        .map(|value| finite_f32_from_f32(*value).unwrap_or(min).clamp(min, max))
        .collect::<Vec<_>>();
    let rows = labels
        .iter()
        .zip(values.iter())
        .enumerate()
        .map(|(component_index, (label, value))| {
            let base_values = values.to_vec();
            let selected_clip = selection;
            let path = path.clone();
            let control = numeric_slider_input_control(
                *value,
                min,
                max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value),
                can_edit,
                move |v| {
                    let mut next_values = base_values.clone();
                    next_values[component_index] = v.clamp(min, max);
                    inspector_effect_property_action(
                        selected_clip,
                        effect_id,
                        &path,
                        build_value(&next_values),
                    )
                },
            );
            FlexChild::fixed(Box::new(
                FlexContainer::row(vec![
                    FlexChild::fixed(Box::new(
                        Label::new(*label).muted().with_font_size(11.0).with_padding(0.0, 0.0),
                    )),
                    FlexChild::flex(control, 1.0),
                ])
                .with_gap(8.0),
            ))
        })
        .collect();
    Box::new(FlexContainer::column(rows).with_gap(4.0))
}

fn numeric_property_range(
    descriptor_min: Option<f64>,
    descriptor_max: Option<f64>,
    default_min: f32,
    default_max: f32,
) -> (f32, f32) {
    let (default_min, default_max) = ordered_numeric_range(default_min, default_max);
    let min = descriptor_min.and_then(finite_f32);
    let max = descriptor_max.and_then(finite_f32);
    match (min, max) {
        (Some(min), Some(max)) => ordered_numeric_range(min, max),
        (Some(min), None) => (min, default_max.max(min)),
        (None, Some(max)) => (default_min.min(max), max),
        (None, None) => (default_min, default_max),
    }
}

fn ordered_numeric_range(min: f32, max: f32) -> (f32, f32) {
    let min = finite_f32_from_f32(min).unwrap_or(0.0);
    let max = finite_f32_from_f32(max).unwrap_or(1.0);
    if min <= max {
        (min, max)
    } else {
        (max, min)
    }
}

fn finite_f32(value: f64) -> Option<f32> {
    finite_f32_from_f32(value as f32)
}

fn finite_f32_from_f32(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

fn property_step(descriptor_step: Option<f64>, fallback_step: Option<f32>) -> Option<f32> {
    descriptor_step
        .filter(|step| step.is_finite() && *step > 0.0)
        .map(|step| step as f32)
        .or(fallback_step)
        .filter(|step| step.is_finite() && *step > 0.0)
}

fn numeric_decimals(descriptor_step: Option<f64>, value: f32) -> usize {
    if let Some(step) = descriptor_step.filter(|step| step.is_finite() && *step > 0.0) {
        return decimal_places_for_step(step);
    }
    if value.fract().abs() > f32::EPSILON {
        2
    } else {
        0
    }
}

fn decimal_places_for_step(step: f64) -> usize {
    let mut scaled = step.abs();
    for decimals in 0..=4 {
        if (scaled.round() - scaled).abs() < 1.0e-6 {
            return decimals;
        }
        scaled *= 10.0;
    }
    4
}

fn inspector_effect_property_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    path: &str,
    value: PropertyValue,
) -> Action {
    let Some(selection) = selection else {
        return Action::NoOp;
    };
    inspector_set_effect_property_action(InspectorSetEffectPropertyPayload {
        clip: inspector_clip_payload(selection),
        effect_id,
        path: path.to_string(),
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        AppShellRelinkAssetDialogPayload, AppShellRevealInFileManagerPayload,
        AssetsDeleteAssetPayload, AssetsDeleteFolderPayload, AssetsDeleteSelectionPayload,
        AssetsImportFilesPayload, AssetsMoveAssetPayload, AssetsMoveFolderPayload,
        AssetsMoveSelectionPayload, AssetsOpenFolderPayload, AssetsRenameAssetPayload,
        AssetsSetProxyModePayload, ImportMediaDialogPayload, APP_SHELL_IMPORT_MEDIA_DIALOG,
        APP_SHELL_NAMESPACE, APP_SHELL_RELINK_ASSET_DIALOG, APP_SHELL_REVEAL_IN_FILE_MANAGER,
        ASSETS_CREATE_ADJUSTMENT_LAYER, ASSETS_CREATE_FOLDER, ASSETS_CREATE_SOLID_COLOR,
        ASSETS_DELETE_ASSET, ASSETS_DELETE_FOLDER, ASSETS_DELETE_SELECTION, ASSETS_IMPORT_FILES,
        ASSETS_MOVE_ASSET, ASSETS_MOVE_FOLDER, ASSETS_MOVE_SELECTION, ASSETS_NAMESPACE,
        ASSETS_OPEN_FOLDER, ASSETS_PREPARE_DRAG, ASSETS_RENAME_ASSET, ASSETS_SET_PROXY_MODE,
        EFFECTS_ADD_TO_CLIP, EFFECTS_NAMESPACE, INSPECTOR_NAMESPACE, INSPECTOR_SELECT_EFFECT,
        INSPECTOR_SET_CLIP_CURVE, INSPECTOR_SET_CLIP_TRANSFORM_FIELD,
        INSPECTOR_SET_EFFECT_PROPERTY, TIMELINE_ADD_TRACK, TIMELINE_CLEAR_IN_OUT_POINTS,
        TIMELINE_DROP_ASSET, TIMELINE_MOVE_TRACK, TIMELINE_NAMESPACE,
        TIMELINE_OPEN_NESTED_SEQUENCE, TIMELINE_SELECT_CLIP, TIMELINE_SET_IN_OUT_POINT,
        TIMELINE_SET_SELECTED_CLIPS_ENABLED, TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD,
    };
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::automation::{Keyframe, PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::types::{AssetId, TimeCode};
    use mondrian_effects::EffectNodeExt;
    use mondrian_ui_core::tree::WidgetTreeView;
    use mondrian_ui_core::types::{
        DragPayload, EventResult, KeyCode, LayoutConstraint, Modifiers, MouseButton, Point, Rect,
        Size, WidgetId,
    };
    use mondrian_ui_core::widget::{EventContext, EventRequests, PaintContext};
    use mondrian_ui_core::UiEvent;
    use mondrian_ui_events::EventRouter;
    use mondrian_ui_widgets::menu::MenuItemKind;
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
        assert!(!models.inspector.curve_points.is_empty());
        assert!(!models.node_graph.nodes.is_empty());
    }

    #[test]
    fn asset_browser_tabs_expose_effect_browser() {
        let tabs = visible_tabs_for_slot(PanelKind::Assets, &[]);

        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0], PanelKind::Assets);
        assert_eq!(tabs[1], PanelKind::Effects);
    }

    fn dock_panel_for_kind(widget: &dyn Widget, kind: PanelKind) -> Option<&DockPanel> {
        if let Some(panel) = widget.as_any().and_then(|any| any.downcast_ref::<DockPanel>()) {
            if panel.kind() == kind {
                return Some(panel);
            }
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index) {
                if let Some(panel) = dock_panel_for_kind(child, kind) {
                    return Some(panel);
                }
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

        let dock =
            build_dock_tree_from_layout(AppUiPanelModels::demo(), &layout).expect("dock tree");
        let assets = dock_panel_for_kind(&dock, PanelKind::Assets).expect("assets panel");

        assert_eq!(assets.tab_count(), 1);
        assert_eq!(assets.active_index(), 0);
    }

    fn panel_at_point(widget: &dyn Widget, point: Point) -> Option<PanelKind> {
        if !widget.hit_test(point) {
            return None;
        }
        for index in (0..widget.child_count()).rev() {
            if let Some(child) = widget.child(index) {
                if let Some(kind) = panel_at_point(child, point) {
                    return Some(kind);
                }
            }
        }
        widget.panel_kind()
    }

    #[test]
    fn editing_workspace_uses_full_width_bottom_timeline() {
        let mut dock =
            build_dock_tree_for_preset(AppUiPanelModels::demo(), WorkspacePreset::Editing);
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
                PanelKind::Inspector,
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
        assert_eq!(models.viewer.timecode_label, "00:00:00:00");
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
        state.sequence = Some(Sequence::new("edit"));

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
        state.sequence = Some(sequence);
        state.set_export_draft_preset_index(1);
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
        let payload = model.enqueue_payload().expect("enqueue payload");
        assert_eq!(payload.sequence_id, Some(sequence_id));
        assert_eq!(payload.range, TimelineExportRange::EntireSequence);
        assert_eq!(
            payload.output_path,
            std::path::PathBuf::from("E:/renders/deliverable.mp4")
        );
    }

    #[test]
    fn export_panel_model_does_not_build_enqueue_payload_when_disabled() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Deliverable");
        let sequence_id = sequence.id;
        state.sequence = Some(sequence);
        state.set_export_draft_sequence_id(Some(sequence_id));
        state.set_export_draft_output_path("   ");

        let model = ExportPanelModel::from_app_state(&state);

        assert!(!model.can_enqueue());
        assert!(model.can_select_range());
        assert!(model.can_choose_output());
        assert_eq!(model.readiness_status(), "选择输出路径后即可加入队列");
        assert!(model.enqueue_payload().is_none());
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
        assert!(model.enqueue_payload().is_none());
    }

    #[test]
    fn export_panel_model_exposes_render_queue_jobs() {
        let state = AppState::new();
        let render_job = |output_path: &str| {
            mondrian_export::queue::RenderJob::new(mondrian_export::preset::ExportConfig {
                preset: mondrian_export::preset::ExportPreset::youtube_1080p(),
                input: mondrian_export::preset::ExportInput::File {
                    input_path: "missing-source.mov".into(),
                    in_point: None,
                    out_point: None,
                },
                output_path: output_path.into(),
            })
        };
        let mut encoding = render_job("E:/renders/encoding.mp4");
        encoding.status = JobStatus::Encoding;
        encoding.progress = 0.82;
        let encoding_id = state.render_queue.enqueue(encoding);
        let mut failed = render_job("E:/renders/failed.mp4");
        failed.status = JobStatus::Failed("disk full".to_owned());
        let failed_id = state.render_queue.enqueue(failed);
        let mut completed = render_job("E:/renders/completed.mp4");
        completed.status = JobStatus::Completed;
        completed.progress = 1.0;
        let completed_id = state.render_queue.enqueue(completed);

        let model = ExportPanelModel::from_app_state(&state);

        assert_eq!(model.queue_count, 3);
        assert_eq!(model.jobs.len(), 3);
        assert!(model.can_clear_completed_jobs);
        assert_eq!(
            model.jobs.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![completed_id, failed_id, encoding_id]
        );

        let encoding =
            model.jobs.iter().find(|job| job.id == encoding_id).expect("encoding job model");
        assert_eq!(encoding.title, "encoding.mp4");
        assert_eq!(encoding.status, "Encoding");
        assert_eq!(encoding.progress_percent, 82);
        assert!(encoding.can_cancel);
        assert!(!encoding.is_completed);

        let failed = model.jobs.iter().find(|job| job.id == failed_id).expect("failed job model");
        assert_eq!(failed.title, "failed.mp4");
        assert_eq!(failed.status, "Failed: disk full");
        assert!(!failed.can_cancel);
        assert!(failed.is_completed);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_IMPORT_FILES);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_MOVE_ASSET);
        let payload: AssetsMoveAssetPayload =
            serde_json::from_value(payload.clone()).expect("move asset payload");
        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(payload.folder_id.as_deref(), Some("rushes"));
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_MOVE_FOLDER);
        let payload: AssetsMoveFolderPayload =
            serde_json::from_value(payload.clone()).expect("move folder payload");
        assert_eq!(payload.folder_id, "child");
        assert_eq!(payload.parent_folder_id.as_deref(), Some("parent"));
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_MOVE_SELECTION);
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
                assert_assets_action(children[0].action(), ASSETS_CREATE_ADJUSTMENT_LAYER);
                assert_assets_action(children[1].action(), ASSETS_CREATE_SOLID_COLOR);
                assert_assets_action(children[2].action(), ASSETS_CREATE_FOLDER);
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
        assert_assets_action(children[0].action(), ASSETS_CREATE_ADJUSTMENT_LAYER);
        assert_assets_action(children[1].action(), ASSETS_CREATE_SOLID_COLOR);
        assert_assets_action(children[2].action(), ASSETS_CREATE_FOLDER);
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
        state.asset_library = Some(library);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_DELETE_ASSET);
        let payload: AssetsDeleteAssetPayload =
            serde_json::from_value(payload.clone()).expect("delete payload");
        assert_eq!(payload.asset_id, asset_id);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn assets_panel_file_card_context_menu_dispatches_reveal_first() {
        let asset_id = AssetId::new();
        let path = PathBuf::from("E:/media/shot.mov");
        let item =
            asset_grid_item_from_asset(test_video_asset(asset_id, path.clone()), None, false);
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
            panic!("expected reveal action");
        };
        assert_eq!(namespace, APP_SHELL_NAMESPACE);
        assert_eq!(name, APP_SHELL_REVEAL_IN_FILE_MANAGER);
        let payload: AppShellRevealInFileManagerPayload =
            serde_json::from_value(payload.clone()).expect("reveal payload");
        assert_eq!(payload.path, path);
    }

    #[test]
    fn assets_panel_inline_rename_dispatches_asset_rename_action() {
        let root = unique_temp_dir("asset-panel-inline-rename");
        let library = AssetLibrary::open(root.clone()).expect("open asset library");
        let asset_id = library.create_solid_color_asset(Some("Old Plate")).expect("create asset");
        let mut state = AppState::new();
        state.asset_library = Some(library);
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

        grid.event(&UiEvent::FocusGained, &mut ctx);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_RENAME_ASSET);
        let payload: AssetsRenameAssetPayload =
            serde_json::from_value(payload.clone()).expect("rename payload");
        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(payload.name, "New Plate");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn assets_panel_offline_file_card_context_menu_includes_relink() {
        let asset_id = AssetId::new();
        let item = asset_grid_item_from_asset(
            test_video_asset(asset_id, PathBuf::from("E:/missing/shot.mov")),
            None,
            false,
        );

        assert_eq!(badge_labels(&item), ["视频", "离线"]);
        assert_eq!(item.badges[1].tone, AssetGridBadgeTone::Warning);
        assert_eq!(item.context_menu_items.len(), 4);
        assert_eq!(item.context_menu_items[0].label, "在文件管理器中显示");
        assert_eq!(item.context_menu_items[1].label, "重新链接媒体...");
        assert!(item.context_menu_items[2].is_separator());
        assert_eq!(item.context_menu_items[3].label, "删除素材");
        let Action::Custom { namespace, name, payload } =
            item.context_menu_items[1].action().expect("relink shell action")
        else {
            panic!("expected relink shell action");
        };
        assert_eq!(namespace, APP_SHELL_NAMESPACE);
        assert_eq!(name, APP_SHELL_RELINK_ASSET_DIALOG);
        let payload: AppShellRelinkAssetDialogPayload =
            serde_json::from_value(payload.clone()).expect("relink payload");
        assert_eq!(payload.asset_id, asset_id);
    }

    #[test]
    fn assets_panel_online_video_card_context_menu_toggles_proxy_mode() {
        let root = unique_temp_dir("asset-panel-proxy-menu");
        std::fs::create_dir_all(&root).expect("create temp root");
        let media_path = root.join("shot.mov");
        std::fs::write(&media_path, b"not decoded in this view-model test").expect("write media");
        let asset_id = AssetId::new();
        let item =
            asset_grid_item_from_asset(test_video_asset(asset_id, media_path.clone()), None, false);

        assert_eq!(badge_labels(&item), ["视频"]);
        assert_eq!(item.context_menu_items.len(), 4);
        assert_eq!(item.context_menu_items[0].label, "在文件管理器中显示");
        assert_eq!(item.context_menu_items[1].label, "启用代理模式");
        assert!(item.context_menu_items[2].is_separator());
        let Action::Custom { namespace, name, payload } =
            item.context_menu_items[1].action().expect("proxy mode action")
        else {
            panic!("expected proxy mode custom action");
        };
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_SET_PROXY_MODE);
        let payload: AssetsSetProxyModePayload =
            serde_json::from_value(payload.clone()).expect("proxy payload");
        assert_eq!(payload.asset_id, asset_id);
        assert!(payload.enabled);

        let proxied =
            asset_grid_item_from_asset(test_video_asset(asset_id, media_path), None, true);
        assert_eq!(badge_labels(&proxied), ["视频", "代理"]);
        assert_eq!(proxied.badges[1].tone, AssetGridBadgeTone::Success);
        assert_eq!(proxied.context_menu_items[1].label, "关闭代理模式");
        let Action::Custom { payload, .. } =
            proxied.context_menu_items[1].action().expect("proxy mode action")
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
        state.asset_library = Some(library);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_DELETE_FOLDER);
        let payload: AssetsDeleteFolderPayload =
            serde_json::from_value(payload.clone()).expect("delete folder payload");
        assert_eq!(payload.folder_id, folder_id);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn assets_panel_multi_selection_context_menu_dispatches_delete_selection() {
        let root = unique_temp_dir("asset-panel-delete-selection-menu");
        let library = AssetLibrary::open(root.clone()).expect("open asset library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let asset_id = library.create_solid_color_asset(Some("Temp Plate")).expect("create asset");
        let mut state = AppState::new();
        state.asset_library = Some(library);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_DELETE_SELECTION);
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
        state.asset_library = Some(library);
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

        let _ = grid.event(&UiEvent::FocusGained, &mut ctx);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_DELETE_SELECTION);
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
    fn timeline_model_maps_track_move_payload_to_media_local_index() {
        let mut sequence = Sequence::new("edit");
        sequence.add_video_track();
        sequence.add_audio_track();
        let video_count = sequence.video_tracks.len();
        let moved_track_id = sequence.video_tracks[1].id;
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
        assert!(payload.is_video_track);
        assert_eq!(payload.target_index, video_count - 1);
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
            .asset_drop_payload(TimelineAssetDrop {
                asset_id,
                track_ref: first_audio_ref,
                frame: -12,
            })
            .expect("asset drop payload");

        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(payload.target_track_id, target_track_id);
        assert!(!payload.is_video_track);
        assert_eq!(payload.frame, 0);
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
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_ADD_TRACK);
        let payload: TimelineAddTrackPayload =
            serde_json::from_value(payload.clone()).expect("add track payload");
        assert_eq!(payload.kind, TimelineAddTrackKind::Video);
    }

    #[test]
    fn timeline_panel_track_header_drag_emits_typed_move_track_action() {
        let model = demo_timeline_model();
        let moved = model.track_identity(TimelineTrackRef { track_index: 1 }).expect("track");
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
                        if namespace == TIMELINE_NAMESPACE && name == TIMELINE_MOVE_TRACK
                )
            })
            .expect("move track action");
        let Action::Custom { payload, .. } = move_action else {
            panic!("expected custom move-track action");
        };
        let payload: TimelineMoveTrackPayload =
            serde_json::from_value(payload.clone()).expect("move track payload");
        assert_eq!(payload.track_id, moved.track_id);
        assert!(payload.is_video_track);
        assert_eq!(payload.target_index, 2);
    }

    #[test]
    fn timeline_panel_disabled_clip_still_selects_for_inspection() {
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(24, tb));
        clip.is_disabled = true;
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;
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
                    position: timeline_content_point(
                        140.0,
                        42.0 + display_track_index as f32 * 42.0,
                    ),
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
        assert_eq!(payload.track_id, track_id);
        assert_eq!(payload.clip_id, clip_id);
        assert!(payload.is_video_track);
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
        let drop_action = recorded
            .iter()
            .find(|action| {
                matches!(
                    action,
                    Action::Custom { namespace, name, .. }
                        if namespace == TIMELINE_NAMESPACE && name == TIMELINE_DROP_ASSET
                )
            })
            .expect("drop asset action");
        let Action::Custom { payload, .. } = drop_action else {
            panic!("expected custom drop-asset action");
        };
        let payload: TimelineDropAssetPayload =
            serde_json::from_value(payload.clone()).expect("drop asset payload");
        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(payload.target_track_id, target.track_id);
        assert!(payload.is_video_track);
        assert_eq!(payload.frame, 10);
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
        let drop_action = recorded
            .iter()
            .find(|action| {
                matches!(
                    action,
                    Action::Custom { namespace, name, .. }
                        if namespace == TIMELINE_NAMESPACE && name == TIMELINE_DROP_ASSET
                )
            })
            .expect("drop asset action");
        let Action::Custom { payload, .. } = drop_action else {
            panic!("expected custom drop-asset action");
        };
        let payload: TimelineDropAssetPayload =
            serde_json::from_value(payload.clone()).expect("drop asset payload");
        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(payload.target_track_id, target.track_id);
        assert!(payload.is_video_track);
        assert_eq!(payload.frame, 10);
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

        panel.event(&UiEvent::FocusGained, &mut ctx);
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
        assert_eq!(payload.point, TimelineInOutPointPayloadKind::In);
        assert_eq!(payload.frame, 20);
    }

    #[test]
    fn timeline_edit_command_mapping_emits_selection_trim_and_enable_actions() {
        let model = TimelinePanelModel::default();
        assert_eq!(
            timeline_edit_command_action(&model, TimelineEditCommand::CutSelection),
            Action::Cut
        );
        assert_eq!(
            timeline_edit_command_action(&model, TimelineEditCommand::CopySelection),
            Action::Copy
        );
        assert_eq!(
            timeline_edit_command_action(&model, TimelineEditCommand::PasteAtPlayhead),
            Action::Paste
        );
        assert_eq!(
            timeline_edit_command_action(&model, TimelineEditCommand::DuplicateSelection),
            Action::Duplicate
        );
        assert_eq!(
            timeline_edit_command_action(&model, TimelineEditCommand::TogglePlayback),
            Action::TogglePlay
        );
        assert_eq!(
            timeline_edit_command_shortcut_label(TimelineEditCommand::CopySelection).as_deref(),
            Some("Ctrl+C")
        );
        assert_eq!(
            timeline_edit_command_shortcut_label(TimelineEditCommand::DuplicateSelection)
                .as_deref(),
            Some("Ctrl+D")
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
            timeline_edit_command_action(&model, TimelineEditCommand::TrimSelectionInToPlayhead);
        let Action::Custom { namespace, name, payload } = trim_action else {
            panic!("expected selected trim action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD);
        let payload: TimelineTrimSelectedClipsToPlayheadPayload =
            serde_json::from_value(payload).expect("trim payload");
        assert_eq!(payload.edge, TimelineTrimPayloadEdge::In);

        let roll_action =
            timeline_edit_command_action(&model, TimelineEditCommand::RollSelectedCutToPlayhead);
        let Action::Custom { namespace, name, payload } = roll_action else {
            panic!("expected roll cut action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(
            name,
            crate::app::ui_actions::TIMELINE_ROLL_SELECTED_CUT_TO_PLAYHEAD
        );
        assert!(payload.is_null());

        let disable_action =
            timeline_edit_command_action(&model, TimelineEditCommand::DisableSelection);
        let Action::Custom { namespace, name, payload } = disable_action else {
            panic!("expected selected enable action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_SET_SELECTED_CLIPS_ENABLED);
        let payload: TimelineSetSelectedClipsEnabledPayload =
            serde_json::from_value(payload).expect("enabled payload");
        assert!(!payload.enabled);

        let clear_action =
            timeline_edit_command_action(&model, TimelineEditCommand::ClearInOutPoints);
        let Action::Custom { namespace, name, payload } = clear_action else {
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
            .add_clip(Clip::new_nested_sequence(
                nested_id,
                TimeCode::new(0, tb),
                TimeCode::new(24, tb),
                Some("Nested".to_owned()),
            ))
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

        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected open nested custom action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_OPEN_NESTED_SEQUENCE);
        let payload: TimelineOpenNestedSequencePayload =
            serde_json::from_value(payload).expect("open nested payload");
        assert_eq!(payload.sequence_id, nested_id);
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
        let display_track_index = video_display_index(&sequence, 0);
        state.sequence = Some(sequence);
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
        let display_track_index = video_display_index(&sequence, 0);
        state.sequence = Some(sequence);
        assert!(
            state.select_effect_by_id(clip_id, effect_id).is_some(),
            "seed selected effect"
        );
        state.seek(7);

        let models = AppUiPanelModels::from_app_state(&state);
        let colors = current_theme().colors.clone();

        assert_eq!(models.viewer.title, "edit");
        assert_eq!(models.viewer.frame_label, "F7");
        assert!(models.viewer.timecode_label.ends_with(":07"));
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
            models.inspector.curve_points,
            vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 1.0)]
        );

        state.play();
        let playing_models = AppUiPanelModels::from_app_state(&state);
        assert_eq!(playing_models.viewer.status, "播放中");
        assert_eq!(playing_models.viewer.status_tone, ViewerStatusTone::Accent);
        assert!(playing_models.viewer.playing);
        assert_eq!(models.inspector.tint.to_rgba8(), color.to_rgba8());
        assert_eq!(models.inspector.position_x, 192.0);
        assert_eq!(models.inspector.position_y, 108.0);
        assert_eq!(models.inspector.scale_percent, 125.0);
        assert_eq!(models.inspector.rotation_degrees, 15.0);
        assert_eq!(models.inspector.in_frame, 4.0);
        assert_eq!(models.inspector.out_frame, 22.0);
        assert_eq!(models.inspector.max_frame, 22.0);
        assert_eq!(models.inspector.effects.len(), 1);
        assert_eq!(models.inspector.selected_effect_id, Some(effect_id));
        assert_eq!(models.inspector.effects[0].effect_id, effect_id);
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
    fn app_state_models_attach_viewer_preview_frame_from_source() {
        struct TestPreview;

        impl ViewerPreviewSource for TestPreview {
            fn viewer_frame_for_state(&self, _state: &AppState) -> Option<ViewerFrameImage> {
                ViewerFrameImage::new("test-preview", 320, 180, vec![128; 320 * 180 * 4])
            }
        }

        let mut state = AppState::new();
        state.sequence = Some(Sequence::new("edit"));

        let models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
            &state,
            None,
            None,
            Some(&TestPreview),
        );

        let frame = models.viewer.frame_image.expect("preview frame");
        assert_eq!(frame.key, "test-preview");
        assert_eq!(frame.width, 320);
        assert_eq!(frame.height, 180);
        assert_eq!(models.viewer.preview_quality_label, "1/2");
        assert_eq!(models.viewer.preview_resolution_scale, 0.5);
    }

    #[test]
    fn app_state_models_do_not_request_viewer_preview_without_sequence() {
        struct UnexpectedPreview;

        impl ViewerPreviewSource for UnexpectedPreview {
            fn viewer_frame_for_state(&self, _state: &AppState) -> Option<ViewerFrameImage> {
                ViewerFrameImage::new("unexpected-preview", 320, 180, vec![128; 320 * 180 * 4])
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
        assert!(models.viewer.frame_image.is_none());
        assert_eq!(models.viewer.empty_message.as_deref(), Some("未载入序列"));
    }

    #[test]
    fn app_state_models_label_full_resolution_viewer_preview_scale() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        sequence.settings.preview.resolution_scale = 1.0;
        state.sequence = Some(sequence);

        let models = AppUiPanelModels::from_app_state(&state);

        assert_eq!(models.viewer.preview_quality_label, "1/1");
        assert_eq!(models.viewer.preview_resolution_scale, 1.0);
    }

    #[test]
    fn app_state_models_clamp_viewer_preview_scale_label() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        sequence.settings.preview.resolution_scale = 0.0;
        state.sequence = Some(sequence);

        let models = AppUiPanelModels::from_app_state(&state);

        assert_eq!(models.viewer.preview_quality_label, "1/8");
        assert_eq!(models.viewer.preview_resolution_scale, 0.125);
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

        let models = AppUiPanelModels::from_app_state(&state);

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

        let models = AppUiPanelModels::from_app_state(&state);

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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_PREPARE_DRAG);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_DELETE_ASSET);
        let payload: AssetsDeleteAssetPayload =
            serde_json::from_value(payload.clone()).expect("asset delete payload");
        assert_eq!(payload.asset_id, asset_id);

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
        state.asset_library = Some(library);

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
        state.asset_library = Some(library);

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
            Some(&TestThumbnails(AssetThumbnailState::Failed)),
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
        state.asset_library = Some(library);

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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_DELETE_FOLDER);
        let payload: AssetsDeleteFolderPayload =
            serde_json::from_value(payload.clone()).expect("folder delete payload");
        assert_eq!(payload.folder_id, folder_id);
        let action = folder.activate_action.as_ref().expect("folder activate action");
        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected asset folder custom action, got {action:?}");
        };
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_OPEN_FOLDER);
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
        state.asset_library = Some(library);

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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_OPEN_FOLDER);
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_DELETE_FOLDER);
        let payload: AssetsDeleteFolderPayload =
            serde_json::from_value(payload.clone()).expect("nested folder delete payload");
        assert_eq!(payload.folder_id, nested_id);

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
        state.asset_library = Some(library);

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
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
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
        assert_eq!(namespace, EFFECTS_NAMESPACE);
        assert_eq!(name, EFFECTS_ADD_TO_CLIP);
        let payload: EffectsAddToClipPayload =
            serde_json::from_value(payload.clone()).expect("effect add payload");
        assert_eq!(payload.clip.clip_id, clip_id);
        assert_eq!(payload.clip.track_id, track_id);
        assert!(payload.clip.is_video_track);
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
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(clip).expect("add video clip");
        state.sequence = Some(sequence);
        state.selection.selected_clips.push(SelectedClipRef {
            track_id,
            is_video_track: true,
            clip_id,
        });

        state
            .dispatch_action(effects_add_to_clip_action(EffectsAddToClipPayload {
                clip: inspector_clip_payload(SelectedClipRef {
                    track_id,
                    is_video_track: true,
                    clip_id,
                }),
                effect_type: EffectType::GaussianBlur,
            }))
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
    fn node_graph_model_falls_back_to_source_after_selected_effect_removal() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let remove_effect = mondrian_effects::EffectNode::with_defaults(EffectType::GaussianBlur);
        let keep_effect = mondrian_effects::EffectNode::with_defaults(EffectType::Sharpen);
        let remove_id = remove_effect.id;
        let keep_id = keep_effect.id;
        clip.add_effect_node(remove_effect);
        clip.add_effect_node(keep_effect);
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add video clip");
        state.sequence = Some(sequence);
        let selection = state
            .select_effect_by_id(clip_id, remove_id)
            .expect("seed selected effect")
            .clip;

        state
            .dispatch_action(inspector_remove_effect_action(
                InspectorRemoveEffectPayload {
                    clip: inspector_clip_payload(selection),
                    effect_id: remove_id,
                },
            ))
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
        let mut clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
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
        state.sequence = Some(sequence);
        state.select_effect_by_id(clip_id, second_id).expect("seed selected effect");

        state
            .dispatch_action(Action::ReorderEffects { clip_id, from: 1, to: 0 })
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
            path: "lighting.direction".to_string(),
            label: "Direction".to_string(),
            value: PropertyValue::Vec3(glam::Vec3::new(0.1, 0.2, 0.3)),
            min: Some(0.0),
            max: Some(1.0),
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
        assert_eq!(namespace, INSPECTOR_NAMESPACE);
        assert_eq!(name, INSPECTOR_SET_EFFECT_PROPERTY);
        let payload: InspectorSetEffectPropertyPayload =
            serde_json::from_value(payload.clone()).expect("set effect property payload");
        assert_eq!(payload.clip.clip_id, selection.clip_id);
        assert_eq!(payload.effect_id, effect_id);
        assert_eq!(payload.path, "lighting.direction");
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
            path: "color.exposure".to_string(),
            label: "Exposure".to_string(),
            value: PropertyValue::Float(0.2),
            min: Some(0.0),
            max: Some(1.0),
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
        let payload: InspectorSetEffectPropertyPayload =
            serde_json::from_value(payload.clone()).expect("set effect property payload");
        assert_eq!(payload.path, "color.exposure");
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
            path: "blur.radius".to_string(),
            label: "Radius".to_string(),
            value: PropertyValue::Float(0.2),
            min: Some(0.0),
            max: Some(1.0),
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
        assert_eq!(namespace, INSPECTOR_NAMESPACE);
        assert_eq!(name, INSPECTOR_SET_EFFECT_PROPERTY);
        let payload: InspectorSetEffectPropertyPayload =
            serde_json::from_value(payload.clone()).expect("set effect property payload");
        assert_eq!(payload.clip.clip_id, selection.clip_id);
        assert_eq!(payload.effect_id, effect_id);
        assert_eq!(payload.path, "blur.radius");
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
            path: "color.exposure".to_string(),
            label: "Exposure".to_string(),
            value: PropertyValue::Float(0.2),
            min: Some(f64::NAN),
            max: Some(f64::INFINITY),
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
        let payload: InspectorSetEffectPropertyPayload =
            serde_json::from_value(payload.clone()).expect("set effect property payload");
        assert_eq!(payload.path, "color.exposure");
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
            path: "levels.iterations".to_string(),
            label: "Iterations".to_string(),
            value: PropertyValue::Int(10),
            min: Some(0.0),
            max: Some(1000.0),
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
        let payload: InspectorSetEffectPropertyPayload =
            serde_json::from_value(payload.clone()).expect("set effect property payload");
        assert_eq!(payload.path, "levels.iterations");
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
            "key.color",
            PropertyValue::Color(color),
        );

        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected inspector set effect property action");
        };
        assert_eq!(namespace, INSPECTOR_NAMESPACE);
        assert_eq!(name, INSPECTOR_SET_EFFECT_PROPERTY);
        let payload: InspectorSetEffectPropertyPayload =
            serde_json::from_value(payload).expect("set effect property payload");
        assert_eq!(payload.clip.track_id, selection.track_id);
        assert_eq!(payload.clip.clip_id, selection.clip_id);
        assert_eq!(payload.effect_id, effect_id);
        assert_eq!(payload.path, "key.color");
        assert_eq!(payload.value, PropertyValue::Color(color));
    }

    #[test]
    fn numeric_slider_input_control_text_input_dispatches_typed_transform_action() {
        let selection = SelectedClipRef {
            track_id: TrackId::new(),
            is_video_track: true,
            clip_id: ClipId::new(),
        };
        let mut widget =
            numeric_slider_input_control(12.0, -180.0, 180.0, Some(0.1), 1, true, move |value| {
                inspector_transform_action(
                    Some(selection),
                    InspectorClipTransformField::RotationDegrees,
                    value,
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
        assert_eq!(namespace, INSPECTOR_NAMESPACE);
        assert_eq!(name, INSPECTOR_SET_CLIP_TRANSFORM_FIELD);
        let payload: InspectorSetClipTransformFieldPayload =
            serde_json::from_value(payload.clone()).expect("transform payload");
        assert_eq!(payload.clip.clip_id, selection.clip_id);
        assert_eq!(payload.field, InspectorClipTransformField::RotationDegrees);
        assert!((payload.value - 45.6).abs() < 0.0001);
    }

    #[test]
    fn numeric_slider_input_control_disabled_text_input_does_not_dispatch() {
        let selection = SelectedClipRef {
            track_id: TrackId::new(),
            is_video_track: true,
            clip_id: ClipId::new(),
        };
        let mut widget =
            numeric_slider_input_control(50.0, 0.0, 100.0, Some(1.0), 0, false, move |value| {
                inspector_value_action(Some(selection), "opacity", value)
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
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(clip).expect("add video clip");
        sequence.video_tracks[0].is_locked = true;
        state.sequence = Some(sequence);
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
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(clip).expect("add video clip");
        sequence.video_tracks[0].is_locked = true;
        state.sequence = Some(sequence);
        state.selection.selected_clips.push(SelectedClipRef {
            track_id,
            is_video_track: true,
            clip_id,
        });
        state.seek(15);

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
        let clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(clip).expect("add video clip");
        state.sequence = Some(sequence);
        state.selection.selected_clips.push(SelectedClipRef {
            track_id,
            is_video_track: true,
            clip_id,
        });

        state.seek(10);
        let at_clip_start = TimelinePanelModel::from_app_state(&state);
        assert!(
            !at_clip_start.edit_command_available(TimelineEditCommand::TrimSelectionInToPlayhead)
        );
        assert!(
            at_clip_start.edit_command_available(TimelineEditCommand::TrimSelectionOutToPlayhead)
        );

        state.seek(29);
        let at_clip_end = TimelinePanelModel::from_app_state(&state);
        assert!(at_clip_end.edit_command_available(TimelineEditCommand::TrimSelectionInToPlayhead));
        assert!(
            !at_clip_end.edit_command_available(TimelineEditCommand::TrimSelectionOutToPlayhead)
        );
    }

    #[test]
    fn app_state_models_mark_inspector_readonly_for_locked_selected_track() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("edit");
        let tb = sequence.time_base();
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(clip).expect("add video clip");
        sequence.video_tracks[0].is_locked = true;
        state.sequence = Some(sequence);
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
        let solid = Clip::new_solid_color(
            AssetId::new(),
            color,
            TimeCode::new(0, tb),
            TimeCode::new(30, tb),
        );
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
            .add_clip(Clip::new(
                AssetId::new(),
                TimeCode::new(0, tb),
                TimeCode::new(30, tb),
            ))
            .expect("add video clip");
        sequence.video_tracks[0]
            .add_clip(Clip::new_adjustment_layer(
                AssetId::new(),
                TimeCode::new(40, tb),
                TimeCode::new(30, tb),
            ))
            .expect("add adjustment clip");
        sequence.audio_tracks[0]
            .add_clip(Clip::new(
                AssetId::new(),
                TimeCode::new(0, tb),
                TimeCode::new(30, tb),
            ))
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
        assert_eq!(inspector_reorder_effect_action(None, 1, 0), Action::NoOp);
        assert_eq!(
            inspector_effect_property_action(
                None,
                EffectId::new(),
                "color.tint",
                PropertyValue::Color(Color::from_rgba8(1, 2, 3, 4)),
            ),
            Action::NoOp
        );
        assert_eq!(
            inspector_curve_action(None, &[CurvePoint::new(0.0, 1.0)]),
            Action::NoOp
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
            is_editable: false,
            edit_disabled_reason: Some("所选剪辑所在轨道已锁定".to_owned()),
            enabled: true,
            opacity: 100.0,
            tint: Color::from_rgba8(64, 128, 192, 255),
            position_x: 0.0,
            position_y: 0.0,
            scale_percent: 100.0,
            rotation_degrees: 0.0,
            in_frame: 0.0,
            out_frame: 30.0,
            max_frame: 60.0,
            tint_area_mode: ColorPickerAreaMode::Wheel,
            curve_points: vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 1.0)],
            effects: Vec::new(),
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
            is_editable: true,
            edit_disabled_reason: None,
            enabled: true,
            opacity: 100.0,
            tint: Color::from_rgba8(64, 128, 192, 255),
            position_x: 0.0,
            position_y: 0.0,
            scale_percent: 100.0,
            rotation_degrees: 0.0,
            in_frame: 0.0,
            out_frame: 30.0,
            max_frame: 60.0,
            tint_area_mode: ColorPickerAreaMode::Wheel,
            curve_points: vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 1.0)],
            effects: vec![InspectorEffectModel {
                effect_id,
                label: "Gaussian Blur".to_owned(),
                enabled: true,
                properties: Vec::new(),
            }],
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

        let result = panel.event(
            &UiEvent::MouseDown {
                position: Point::new(24.0, 408.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(requests.repaint);
        let recorded = actions.borrow();
        assert_eq!(recorded.len(), 1);
        let Action::Custom { namespace, name, payload } = &recorded[0] else {
            panic!(
                "expected inspector select effect action, got {:?}",
                recorded[0]
            );
        };
        assert_eq!(namespace, INSPECTOR_NAMESPACE);
        assert_eq!(name, INSPECTOR_SELECT_EFFECT);
        let payload: InspectorSelectEffectPayload =
            serde_json::from_value(payload.clone()).expect("inspector select effect payload");
        assert_eq!(payload.clip.clip_id, selection.clip_id);
        assert_eq!(payload.clip.track_id, selection.track_id);
        assert_eq!(payload.effect_id, effect_id);
    }

    #[test]
    fn inspector_reorder_effect_action_uses_typed_app_action() {
        let selection = SelectedClipRef {
            track_id: TrackId::new(),
            is_video_track: true,
            clip_id: ClipId::new(),
        };

        assert_eq!(
            inspector_reorder_effect_action(Some(selection), 2, 0),
            Action::ReorderEffects { clip_id: selection.clip_id, from: 2, to: 0 }
        );
        assert_eq!(
            inspector_reorder_effect_action(Some(selection), 1, 1),
            Action::NoOp
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
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, TIMELINE_NAMESPACE);
                assert_eq!(name, TIMELINE_SELECT_CLIP);
                let payload: TimelineSelectClipPayload =
                    serde_json::from_value(payload).expect("timeline select payload");
                assert_eq!(payload.track_id, selection.track_id);
                assert!(payload.is_video_track);
                assert_eq!(payload.clip_id, selection.clip_id);
            }
            other => panic!("expected timeline select action, got {other:?}"),
        }

        assert_eq!(node_graph_clip_action(None), Action::NoOp);
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
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, INSPECTOR_NAMESPACE);
                assert_eq!(name, INSPECTOR_SELECT_EFFECT);
                let payload: InspectorSelectEffectPayload =
                    serde_json::from_value(payload).expect("inspector select effect payload");
                assert_eq!(payload.clip.clip_id, selection.clip_id);
                assert_eq!(payload.effect_id, effect_id);
            }
            other => panic!("expected inspector select effect action, got {other:?}"),
        }

        assert_eq!(
            node_graph_node_action(Some(selection), &targets, "missing-node"),
            Action::NoOp
        );
        assert_eq!(
            node_graph_node_action(None, &targets, "effect-node"),
            Action::NoOp
        );
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
            panel.event(&UiEvent::FocusGained, &mut ctx),
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
        assert_eq!(payload.track_id, selection.track_id);
        assert!(!payload.is_video_track);
        assert_eq!(payload.clip_id, selection.clip_id);
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
        assert_eq!(namespace, INSPECTOR_NAMESPACE);
        assert_eq!(name, INSPECTOR_SELECT_EFFECT);
        let payload: InspectorSelectEffectPayload =
            serde_json::from_value(payload.clone()).expect("inspector select effect payload");
        assert_eq!(payload.clip.clip_id, selection.clip_id);
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
        assert_eq!(payload.track_id, selection.track_id);
        assert!(payload.is_video_track);
        assert_eq!(payload.clip_id, selection.clip_id);
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
            panel.event(&UiEvent::FocusGained, &mut ctx),
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
        assert_eq!(namespace, INSPECTOR_NAMESPACE);
        assert_eq!(name, INSPECTOR_SELECT_EFFECT);
        let effect_payload: InspectorSelectEffectPayload =
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

    fn test_video_asset(asset_id: AssetId, path: PathBuf) -> AssetRecord {
        AssetRecord {
            id: asset_id,
            name: path.file_name().and_then(|name| name.to_str()).unwrap_or("shot.mov").to_owned(),
            kind: AssetKind::Video,
            path,
            source: None,
            folder_id: None,
            media_info: mondrian_media::MediaInfo::synthetic_adjustment_layer(),
            created_at: "2026-06-19T00:00:00Z".to_owned(),
            updated_at: "2026-06-19T00:00:00Z".to_owned(),
        }
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
                assert_eq!(namespace, ASSETS_NAMESPACE);
                assert_eq!(action_name, name);
            }
            other => panic!("expected assets action, got {other:?}"),
        }
    }
}
