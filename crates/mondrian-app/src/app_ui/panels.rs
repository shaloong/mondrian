//! Panel adapters for the app UI shell.
//!
//! These adapters translate application-facing panel concepts into generic
//! `mondrian-ui-widgets` view models. Demo data is kept behind explicit model
//! factories so typed `AppState` view-model adapters can replace it without
//! changing dock layout or widget construction.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mondrian_assets::library::FolderRecord;
#[cfg(test)]
use mondrian_assets::AssetMediaProbeCandidate;
use mondrian_assets::{AssetKind, AssetLibrary, AssetRecord};
use mondrian_core::automation::{
    AnimationParameterAddress, ParameterResourceReference, ParameterSchema, PropertyValue,
};
use mondrian_core::display_labels::color_space_label;
use mondrian_core::effect_data::EffectType;
use mondrian_core::types::{
    AssetId, AudioComponentEditId, AudioSourceComponentId, ClipId, ClipLinkGroupId, ColorSpace,
    EffectId, JobId, KeyframeId, Rational, SequenceId, TrackId, VideoTransitionId,
};
use mondrian_core::{
    AudioChannelLayout, Color, FrameRounding, ParameterUnit, TimeScale, TimelineDisplayContract,
    TimelineDisplayFormat, TimelineTime, WorkingColorSpace,
};
use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::Action;
use mondrian_effects::{effect_display_name, effect_library_types};
use mondrian_export::delivery::resolve_export_delivery;
use mondrian_export::preset::{
    AudioCodecConfig, Av1Profile, BuiltinExportPreset, Container, ExportAlphaMode,
    ExportChromaSampling, ExportColorTarget, ExportParameter, ExportPreset, H264Profile,
    HevcProfile, ProResProfile, Resolution as ExportResolution, TimelineExportRange,
    VideoCodecConfig, VideoRateControl,
};
use mondrian_export::queue::{
    ExportColorHealthSeverity, ExportJobColorDiagnostics, ExportProgress, ExportProgressDetail,
    ExportProgressPhase, JobStatus,
};
use mondrian_media::info::ChannelLayout;
use mondrian_media::{
    AudioStreamInfo, VideoColorDiagnosticIssueAggregate, VideoColorDiagnosticIssueSummary,
};
use mondrian_timeline::audio::{
    AudioComponentEdit, AudioComponentSource, AudioFade, AudioFadeCurve, AUDIO_GAIN_DB_MAX,
    AUDIO_GAIN_DB_MIN,
};
use mondrian_timeline::clip::{Clip, Transform2D};
use mondrian_timeline::sequence::{
    DeliveryBitDepth, InputColorResolutionSource, MissingColorMetadataPolicy, Sequence, VideoRange,
};
use mondrian_timeline::track::Track;
use mondrian_timeline::AudioComponentMutation;
use mondrian_timeline::VideoTransitionType;
use mondrian_ui_core::types::SplitDirection;
use mondrian_ui_core::DragPayload;
use mondrian_ui_core::Widget;
use mondrian_ui_theme::current_theme;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::dock_tab_bar::TabInfo;
use mondrian_ui_widgets::NumberInput;
use mondrian_ui_widgets::{
    AssetGrid, AssetGridBadgeTone, AssetGridDropOutcome, AssetGridItem, Button, Checkbox,
    ColorPickerAreaMode, ColorPickerTrigger, CurveEdit, CurveEditor, CurvePoint, CurvePointPolicy,
    DockPanel, DockPanelDropArea, Dropdown, FlexChild, FlexContainer, Label, MenuItem,
    MultilineTextInput, NodeGraphEdge, NodeGraphNode, NodeGraphView, PanelList, PanelListItem,
    PropertyPanel, PropertyPanelOptions, PropertyRow, PropertySection, RasterImage, ScrollView,
    Slider, TextInput, TimelineAssetDrop, TimelineClip, TimelineClipKind, TimelineClipMove,
    TimelineClipRef, TimelineClipSelectionMode, TimelineClipTrim, TimelineCutRef,
    TimelineEditCommand, TimelineInOutPoint, TimelineSeek,
    TimelineSeekSource as WidgetTimelineSeekSource, TimelineToolbarIconSlot, TimelineTrack,
    TimelineTrackControl, TimelineTrackControlIconSlot, TimelineTrackMove, TimelineTrackRef,
    TimelineTransition, TimelineTransitionRef, TimelineTransitionResize, TimelineTrimEdge,
    TimelineView, VideoScopesSurface, VideoScopesTextureSet, ViewerCanvasBackground, ViewerControl,
    ViewerFrameContent, ViewerStatusTone, ViewerSurface, WaveformDisplay,
};

use crate::app::exporting::{builtin_export_presets, export_preset_extension};
use crate::app::preview_unavailability::{PreviewUnavailability, PreviewUnavailabilityDisposition};
pub use crate::app::thumbnail_service::{
    ThumbnailFailure as AssetThumbnailFailure,
    ThumbnailFailureReason as AssetThumbnailFailureReason,
};
use crate::app::ui_actions::{
    app_shell_export_output_dialog_action, app_shell_import_media_dialog_action_with_target,
    app_shell_interpret_asset_dialog_action, app_shell_relink_asset_dialog_action,
    app_shell_relocate_panel_action, app_shell_reveal_in_file_manager_action,
    assets_create_adjustment_layer_action, assets_create_folder_action,
    assets_create_solid_color_action, assets_delete_asset_action, assets_delete_folder_action,
    assets_delete_selection_action, assets_import_files_action, assets_move_asset_action,
    assets_move_folder_action, assets_move_selection_action, assets_open_folder_action,
    assets_prepare_drag_action, assets_rebind_audio_component_action,
    assets_refresh_audio_components_action, assets_rename_asset_action,
    assets_rename_folder_action, assets_set_proxy_mode_action, effects_add_to_clip_action,
    export_cancel_job_action, export_clear_completed_action, export_enqueue_action,
    export_set_draft_action, inspector_edit_clip_curve_action, inspector_remove_effect_action,
    inspector_select_effect_action, inspector_set_audio_component_source_action,
    inspector_set_clip_enabled_action, inspector_set_clip_opacity_action,
    inspector_set_clip_property_action, inspector_set_clip_tint_action,
    inspector_set_clip_transform_field_action, inspector_set_effect_enabled_action,
    inspector_set_effect_property_action, timeline_add_track_action,
    timeline_clear_in_out_points_action, timeline_create_cross_dissolve_action,
    timeline_drop_asset_action, timeline_extract_range_action, timeline_lift_range_action,
    timeline_link_selected_clips_action, timeline_move_clip_action, timeline_move_track_action,
    timeline_open_nested_sequence_action, timeline_roll_selected_cut_to_playhead_action,
    timeline_seek_with_source_action, timeline_select_clip_action,
    timeline_select_video_transition_action, timeline_set_in_out_point_action,
    timeline_set_selected_clips_enabled_action, timeline_set_track_control_action,
    timeline_set_track_targeting_action, timeline_set_video_transition_range_action,
    timeline_trim_clips_action, timeline_trim_selected_clips_to_playhead_action,
    timeline_unlink_selected_clips_action, viewer_set_preview_resolution_scale_action,
    viewer_set_zoom_scale_action, AppShellInputColorPipelineDiagnostics,
    AppShellInterpretAssetDialogPayload, AppShellRelinkAssetDialogPayload,
    AppShellRelocatePanelPayload, AppShellRevealInFileManagerPayload,
    AppShellVideoSignalDiagnostics, AssetsCreateAssetPayload, AssetsCreateFolderPayload,
    AssetsDeleteAssetPayload, AssetsDeleteFolderPayload, AssetsDeleteSelectionPayload,
    AssetsImportFilesPayload, AssetsMoveAssetPayload, AssetsMoveFolderPayload,
    AssetsMoveSelectionPayload, AssetsOpenFolderPayload, AssetsPrepareDragPayload,
    AssetsRebindAudioComponentPayload, AssetsRefreshAudioComponentsPayload,
    AssetsRenameAssetPayload, AssetsRenameFolderPayload, AssetsSetProxyModePayload,
    DockDropAreaPayload, EffectsAddToClipPayload, ExportDraftUpdatePayload, ExportEnqueuePayload,
    ExportJobTargetPayload, ExportOutputDialogPayload, ImportMediaDialogPayload,
    InspectorAudioComponentSourcePayload, InspectorClipRefPayload, InspectorClipTransformField,
    InspectorCurveEditPayload, InspectorCurvePointPayload, InspectorEditClipCurvePayload,
    InspectorRemoveEffectPayload, InspectorSelectEffectPayload,
    InspectorSetAudioComponentSourcePayload, InspectorSetClipEnabledPayload,
    InspectorSetClipOpacityPayload, InspectorSetClipPropertyPayload, InspectorSetClipTintPayload,
    InspectorSetClipTransformFieldPayload, InspectorSetEffectEnabledPayload,
    InspectorSetEffectPropertyPayload, TimelineAddTrackKind, TimelineAddTrackPayload,
    TimelineClipSelectionModePayload, TimelineCreateCrossDissolvePayload, TimelineDropAssetPayload,
    TimelineInOutPointPayloadKind, TimelineMoveClipPayload, TimelineMoveTrackPayload,
    TimelineOpenNestedSequencePayload, TimelineSeekSource as AppTimelineSeekSource,
    TimelineSelectClipPayload, TimelineSelectVideoTransitionPayload, TimelineSetInOutPointPayload,
    TimelineSetSelectedClipsEnabledPayload, TimelineSetTrackControlPayload,
    TimelineSetTrackTargetingPayload, TimelineSetVideoTransitionRangePayload,
    TimelineTrackControlPayloadKind, TimelineTrackTargetingControl, TimelineTrimClipsPayload,
    TimelineTrimPayloadEdge, TimelineTrimSelectedClipsToPlayheadPayload,
    ViewerSetPreviewResolutionScalePayload, ViewerSetZoomScalePayload,
};
use crate::app::waveform_service::AudioWaveformSource;
use crate::app::{
    AppState, SelectedClipRef, SelectedVideoTransitionRef, VideoTransitionHandleState,
};
use crate::app_ui::action_availability::app_state_action_enabled;
use crate::app_ui::audio_automation::{
    audio_automation_curve_edit_action, component_automation_viewport, project_audio_automation,
    AudioAutomationCurveModel,
};
use crate::app_ui::audio_component_mapping::{
    audio_component_mutation_action, project_audio_channel_mapping,
    with_audio_channel_mapping_rows, AudioChannelMappingModel,
};
use crate::app_ui::audio_mixer::{
    create_bus_action as audio_mixer_create_bus_action,
    create_route_action as audio_mixer_create_route_action,
    remove_bus_action as audio_mixer_remove_bus_action,
    remove_route_action as audio_mixer_remove_route_action,
    rename_bus_action as audio_mixer_rename_bus_action,
    rewire_route_action as audio_mixer_rewire_route_action,
    set_fader_action as audio_mixer_set_fader_action,
    set_input_trim_action as audio_mixer_set_input_trim_action,
    set_route_enabled_action as audio_mixer_set_route_enabled_action,
    set_route_gain_action as audio_mixer_set_route_gain_action,
    set_track_mute_action as audio_mixer_set_track_mute_action,
    set_track_solo_action as audio_mixer_set_track_solo_action, AudioMixerChannelKind,
    AudioMixerGainModel, AudioMixerPanelModel,
};
use crate::app_ui::audio_processor_rack::{
    bypass_action as audio_processor_bypass_action, clip_processing_scope_racks,
    insert_action as audio_processor_insert_action,
    move_before_action as audio_processor_move_before_action,
    move_to_end_action as audio_processor_move_to_end_action,
    remove_action as audio_processor_remove_action,
    set_static_parameter_action as audio_processor_set_static_parameter_action,
    AudioProcessorRackModel,
};
use crate::app_ui::icons::AppIcon;
use crate::app_ui::inspector_source_timing::{
    inspector_forward_rate_action, inspector_freeze_action, inspector_source_timing_model,
};
pub use crate::app_ui::inspector_source_timing::{
    InspectorSourceTimingMode, InspectorSourceTimingModel,
};
use crate::app_ui::preview_scale::normalize_preview_resolution_scale;
use crate::app_ui::shortcuts::shortcut_label_for_action;
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
/// Panel models only receive immutable renderer-ready frame content.
pub trait ViewerPreviewSource {
    /// Return the current viewer preview lifecycle state.
    fn viewer_preview_for_state(&self, state: &AppState) -> ViewerPreviewState;

    /// Return the latest color-management rejection for the current viewer request.
    fn viewer_color_rejection(&self) -> Option<ViewerPreviewColorRejectionModel> {
        None
    }

    /// Return the current color pipeline health status for the viewer.
    fn viewer_color_pipeline_status(&self) -> Option<ViewerColorPipelineStatus> {
        None
    }
}

/// Current viewer preview lifecycle state for the active frame.
#[derive(Debug, Clone)]
pub enum ViewerPreviewState {
    /// No preview frame is expected for the current state.
    Unavailable(PreviewUnavailability),
    /// The active Sequence evaluates to a valid transparent canvas.
    Transparent,
    /// A previously presented transparent canvas remains visible while the
    /// current demand awaits exact presentation authority.
    StaleTransparent,
    /// A frame request has been queued or is currently rendering/decoding.
    Loading,
    /// The requested frame is not ready, so the viewer may keep the previous frame visible.
    Stale(ViewerFrameContent),
    /// A render-ready frame is available for the current playhead frame.
    Ready(ViewerFrameContent),
}

/// Viewer-facing color pipeline health status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerColorPipelineStatus {
    /// All composites stayed on the float/linear path with no GPU blockers.
    FloatLinear,
    /// At least one composite required the legacy RGBA8 path.
    LegacyRgba8 { legacy_reasons: u64 },
    /// GPU color path has blockers preventing native GPU execution.
    GpuBlocked { gpu_blockers: u64 },
}

/// Viewer-facing color-management rejection details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerPreviewColorRejectionModel {
    /// Asset that could not be interpreted for preview.
    pub asset_id: AssetId,
    /// Media path shown in the viewer diagnostic.
    pub path: PathBuf,
    /// Active missing-metadata policy.
    pub missing_metadata_policy: MissingColorMetadataPolicy,
    /// Input color-resolution branch that rejected the media.
    pub source: InputColorResolutionSource,
    /// Clip/media color-space override in effect, if any.
    pub override_color_space: Option<ColorSpace>,
    /// Validated metadata identity that was eligible to drive pixels.
    pub executable_color_space: Option<ColorSpace>,
    /// Sequence working color space active during the decision.
    pub working_color_space: WorkingColorSpace,
    /// Compact media diagnostic summary.
    pub diagnostic_summary: String,
    /// Machine-readable media diagnostic issue summary.
    pub diagnostic_issue_summary: VideoColorDiagnosticIssueSummary,
}

/// Current thumbnail lifecycle state for one asset card.
#[derive(Debug, Clone)]
pub enum AssetThumbnailState {
    /// No thumbnail is expected for this asset.
    Unavailable,
    /// A thumbnail request has been queued or is currently decoding.
    Loading,
    /// A thumbnail was expected but could not be loaded.
    Failed(AssetThumbnailFailure),
    /// A render-ready thumbnail is available.
    Ready(RasterImage),
}

/// Complete set of view models needed by the app UI panel shell.
#[derive(Debug, Clone)]
pub struct AppUiPanelModels {
    pub assets: AssetGridModel,
    pub effects: PanelListModel,
    pub viewer: ViewerPanelModel,
    pub scopes: ScopesPanelModel,
    pub timeline: TimelinePanelModel,
    pub inspector: InspectorPanelModel,
    pub(crate) mixer: AudioMixerPanelModel,
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
        let viewer = ViewerPanelModel::from_app_state_with_preview(state, preview);
        let scopes = ScopesPanelModel::from_viewer(&viewer);
        let input_pipeline = asset_input_pipeline_for_state(state);
        Self {
            assets: AssetGridModel::from_asset_library_in_folder_with_thumbnails(
                state.asset_library(),
                asset_folder_id,
                thumbnails,
                Some(state.proxy_mode_assets()),
                Some(&input_pipeline),
            ),
            effects: PanelListModel::from_app_effect_registry(state),
            viewer,
            scopes,
            timeline: TimelinePanelModel::from_app_state(state),
            inspector: InspectorPanelModel::from_app_state(state),
            mixer: AudioMixerPanelModel::from_app_state(state),
            export: ExportPanelModel::from_app_state(state),
            node_graph: NodeGraphPanelModel::from_app_state(state),
        }
    }

    /// Demo fixtures that keep rich browser panels while sourcing timeline and
    /// inspector state from an `AppState` snapshot.
    #[cfg(test)]
    pub fn demo_from_app_state(state: &AppState) -> Self {
        let viewer = ViewerPanelModel::from_app_state(state);
        let scopes = ScopesPanelModel::from_viewer(&viewer);
        Self {
            assets: demo_asset_model(),
            effects: PanelListModel::from_app_effect_registry(state),
            viewer,
            scopes,
            timeline: state
                .active_sequence()
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
            mixer: AudioMixerPanelModel::from_app_state(state),
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

fn asset_input_pipeline_for_state(state: &AppState) -> AppShellInputColorPipelineDiagnostics {
    let Some(sequence) = state.active_sequence() else {
        return AppShellInputColorPipelineDiagnostics {
            engine: state.project_color_environment().engine().clone(),
            working_color_space: state.new_sequence_defaults().color.working_color_space,
        };
    };
    AppShellInputColorPipelineDiagnostics {
        engine: state.project_color_environment().engine().clone(),
        working_color_space: sequence.settings.color.working_color_space,
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
    sequence.playhead = crate::app::tt(76, sequence.time_base());

    if let Some(selection) = demo_selection(&sequence) {
        state.replace_clip_selection(vec![selection]);
    }
    state.test_set_sequence(Some(sequence));
    state.seek(76).expect("seek");
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
        Self::from_asset_library_in_folder_with_thumbnails(
            library,
            current_folder_id,
            None,
            None,
            None,
        )
    }

    /// Build the project asset browser for a shell-local folder selection,
    /// optionally attaching already-decoded thumbnails.
    pub fn from_asset_library_in_folder_with_thumbnails(
        library: Option<&AssetLibrary>,
        current_folder_id: Option<&str>,
        thumbnails: Option<&dyn AssetThumbnailSource>,
        proxy_mode_assets: Option<&BTreeSet<AssetId>>,
        input_pipeline: Option<&AppShellInputColorPipelineDiagnostics>,
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
            input_pipeline,
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
    pub position_label: String,
    pub duration_label: String,
    pub zoom_label: String,
    pub zoom_scale: Option<f32>,
    pub preview_quality_label: String,
    pub preview_resolution_scale: f32,
    pub width: u32,
    pub height: u32,
    pub playing: bool,
    pub preview_waiting: bool,
    pub enabled: bool,
    pub frame_content: Option<ViewerFrameContent>,
    pub canvas_background: ViewerCanvasBackground,
    /// Whether the exact current presentation is a texture-free transparent canvas.
    pub transparent_canvas: bool,
    pub empty_message: Option<String>,
    pub preview_unavailability: Option<PreviewUnavailability>,
    pub color_rejection: Option<ViewerPreviewColorRejectionModel>,
    pub color_pipeline_status: Option<ViewerColorPipelineStatus>,
}

/// Program Output scopes data independent from renderer GPU handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopesPanelModel {
    /// Stable registry keys become available only with a current external GPU frame.
    pub textures: Option<VideoScopesTextureSet>,
}

impl ScopesPanelModel {
    pub(crate) fn from_viewer(viewer: &ViewerPanelModel) -> Self {
        let textures = matches!(
            viewer.frame_content.as_ref(),
            Some(ViewerFrameContent::ExternalTexture(_))
        )
        .then(crate::app_ui::scopes::texture_set);
        Self { textures }
    }
}

impl ViewerPanelModel {
    /// Retain the installed presentation while refreshing transport chrome only.
    ///
    /// Transport actions must not synchronously re-enter Preview production,
    /// but they also must not erase the last usable output or its typed
    /// lifecycle while a later Preview turn proves the replacement.
    pub(crate) fn retain_presentation_from(&mut self, current: &Self, state: &AppState) {
        self.frame_content = current.frame_content.clone();
        self.canvas_background = current.canvas_background;
        self.transparent_canvas = current.transparent_canvas;
        self.preview_waiting = current.preview_waiting;
        self.empty_message = current.empty_message.clone();
        self.preview_unavailability = current.preview_unavailability.clone();
        self.color_rejection = current.color_rejection.clone();
        self.color_pipeline_status = current.color_pipeline_status.clone();

        let color_rejected =
            self.preview_unavailability.is_some() && self.color_rejection.is_some();
        let (status, status_tone) = viewer_status(
            state.is_playing(),
            self.preview_waiting,
            color_rejected,
            self.preview_unavailability.as_ref().map(PreviewUnavailability::disposition),
        );
        self.status = status;
        self.status_tone = status_tone;
    }

    /// Keep a usable fallback visible while a transport intent awaits exact proof.
    pub(crate) fn mark_presentation_pending_after_transport_intent(&mut self, state: &AppState) {
        if self.frame_content.is_none() && !self.transparent_canvas {
            return;
        }
        self.preview_waiting = true;
        let (status, status_tone) = viewer_status(
            state.is_playing(),
            true,
            false,
            self.preview_unavailability.as_ref().map(PreviewUnavailability::disposition),
        );
        self.status = status;
        self.status_tone = status_tone;
    }

    /// Payload-free lifecycle classification for the playback feedback Adapter.
    pub(crate) fn preview_state_kind(
        &self,
    ) -> crate::app_ui::playback_feedback::ViewerPreviewStateKind {
        if self.frame_content.is_some() || self.transparent_canvas {
            if self.preview_waiting {
                crate::app_ui::playback_feedback::ViewerPreviewStateKind::Stale
            } else {
                crate::app_ui::playback_feedback::ViewerPreviewStateKind::Ready
            }
        } else if self.preview_waiting {
            crate::app_ui::playback_feedback::ViewerPreviewStateKind::Loading
        } else {
            crate::app_ui::playback_feedback::ViewerPreviewStateKind::Unavailable
        }
    }

    /// Snapshot viewer chrome data from app state.
    pub fn from_app_state(state: &AppState) -> Self {
        Self::from_app_state_with_preview(state, None)
    }

    /// Snapshot viewer chrome data and attach an optional render-ready frame.
    pub fn from_app_state_with_preview(
        state: &AppState,
        preview: Option<&dyn ViewerPreviewSource>,
    ) -> Self {
        let Some(sequence) = state.active_sequence() else {
            return Self::empty();
        };
        let resolution = sequence.settings.resolution;
        let current_frame = state.current_frame();
        let position_label = sequence
            .settings
            .timeline_display_contract()
            .ok()
            .and_then(|display| {
                display.format_frame_offset(current_frame).ok().map(|label| {
                    match display.format() {
                        TimelineDisplayFormat::Frames => format!("F{label}"),
                        TimelineDisplayFormat::Timecode(_) => label,
                    }
                })
            })
            .unwrap_or_else(|| "--:--:--:--".to_owned());
        let Ok(duration_frame) = sequence.total_duration().and_then(|time| {
            time.to_frame_position(sequence.settings.frame_rate, FrameRounding::Ceil)
                .map_err(Into::into)
        }) else {
            return Self::empty();
        };
        let duration_frame = duration_frame.frame.max(0);
        let fps = sequence.settings.frame_rate.to_f64();
        let preview_state = preview.map(|preview| preview.viewer_preview_for_state(state));
        let color_rejection = preview.and_then(ViewerPreviewSource::viewer_color_rejection);
        let preview_unavailability = preview_state.as_ref().and_then(|state| match state {
            ViewerPreviewState::Unavailable(reason) => Some(reason.clone()),
            ViewerPreviewState::Transparent | ViewerPreviewState::StaleTransparent => None,
            ViewerPreviewState::Loading
            | ViewerPreviewState::Stale(_)
            | ViewerPreviewState::Ready(_) => None,
        });
        let frame_content = preview_state.as_ref().and_then(|state| match state {
            ViewerPreviewState::Ready(frame) | ViewerPreviewState::Stale(frame) => {
                Some(frame.clone())
            }
            ViewerPreviewState::Unavailable(_)
            | ViewerPreviewState::Transparent
            | ViewerPreviewState::StaleTransparent
            | ViewerPreviewState::Loading => None,
        });
        let transparent_canvas = matches!(
            preview_state,
            Some(ViewerPreviewState::Transparent | ViewerPreviewState::StaleTransparent)
        );
        let preview_resolution_scale =
            normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale);
        let preview_quality_label = viewer_preview_quality_label(preview_resolution_scale);
        let preview_waiting = matches!(
            preview_state.as_ref(),
            Some(
                ViewerPreviewState::Loading
                    | ViewerPreviewState::Stale(_)
                    | ViewerPreviewState::StaleTransparent
            )
        );
        let color_rejected = matches!(
            preview_state.as_ref(),
            Some(ViewerPreviewState::Unavailable(_))
        ) && color_rejection.is_some();
        let unavailability_disposition =
            preview_unavailability.as_ref().map(PreviewUnavailability::disposition);

        let (status, status_tone) = viewer_status(
            state.is_playing(),
            preview_waiting,
            color_rejected,
            unavailability_disposition,
        );

        Self {
            title: sequence.name.clone(),
            status,
            status_tone,
            resolution_label: format!(
                "{}x{} @ {:.2} fps",
                resolution.width, resolution.height, fps
            ),
            position_label,
            duration_label: format!("{duration_frame} 帧"),
            zoom_label: "适合".into(),
            zoom_scale: None,
            preview_quality_label,
            preview_resolution_scale,
            width: resolution.width,
            height: resolution.height,
            playing: state.is_playing(),
            preview_waiting,
            enabled: true,
            frame_content,
            canvas_background: ViewerCanvasBackground::default(),
            transparent_canvas,
            empty_message: if let Some(rejection) =
                color_rejection.as_ref().filter(|_| color_rejected)
            {
                Some(viewer_color_rejection_empty_message(rejection))
            } else if matches!(preview_state.as_ref(), Some(ViewerPreviewState::Loading)) {
                Some("预览准备中".into())
            } else {
                preview_unavailability
                    .as_ref()
                    .filter(|reason| {
                        reason.disposition() != PreviewUnavailabilityDisposition::NoContent
                    })
                    .map(|reason| reason.detail().to_owned())
            },
            preview_unavailability,
            color_rejection,
            color_pipeline_status: preview
                .and_then(ViewerPreviewSource::viewer_color_pipeline_status),
        }
    }

    /// Empty viewer shown before a sequence is open.
    pub fn empty() -> Self {
        Self {
            title: "预览".into(),
            status: "没有序列".into(),
            status_tone: ViewerStatusTone::Neutral,
            resolution_label: "无信号".into(),
            position_label: "00:00:00:00".into(),
            duration_label: String::new(),
            zoom_label: "适合".into(),
            zoom_scale: None,
            preview_quality_label: "1/1".into(),
            preview_resolution_scale: 1.0,
            width: 16,
            height: 9,
            playing: false,
            preview_waiting: false,
            enabled: false,
            frame_content: None,
            canvas_background: ViewerCanvasBackground::default(),
            transparent_canvas: false,
            empty_message: Some("未载入序列".into()),
            preview_unavailability: None,
            color_rejection: None,
            color_pipeline_status: None,
        }
    }
}

fn viewer_status(
    playing: bool,
    preview_waiting: bool,
    color_rejected: bool,
    unavailability: Option<PreviewUnavailabilityDisposition>,
) -> (String, ViewerStatusTone) {
    let status = if preview_waiting {
        "预览准备中"
    } else if color_rejected {
        "色彩解释被拒绝"
    } else if unavailability == Some(PreviewUnavailabilityDisposition::Blocked) {
        "预览被阻止"
    } else if unavailability == Some(PreviewUnavailabilityDisposition::Failed) {
        "预览失败"
    } else if playing {
        "播放中"
    } else {
        "就绪"
    };
    let tone = if preview_waiting
        || color_rejected
        || matches!(
            unavailability,
            Some(
                PreviewUnavailabilityDisposition::Blocked
                    | PreviewUnavailabilityDisposition::Failed
            )
        ) {
        ViewerStatusTone::Warning
    } else if playing {
        ViewerStatusTone::Accent
    } else {
        ViewerStatusTone::Neutral
    };
    (status.to_owned(), tone)
}

fn viewer_color_rejection_empty_message(rejection: &ViewerPreviewColorRejectionModel) -> String {
    let summary = &rejection.diagnostic_issue_summary;
    let issue_tags = color_issue_summary_tags(summary);
    let issue_line = if issue_tags.is_empty() {
        "问题：none".to_owned()
    } else {
        format!("问题：{}", issue_tags.join(" / "))
    };
    format!(
        "色彩解释被拒绝\n素材：{}\n策略：{:?} / {:?}\n检测：{:?} / {:?} / warnings {}\n{}\n{}",
        rejection.path.display(),
        rejection.missing_metadata_policy,
        rejection.source,
        summary.method,
        summary.confidence,
        summary.warning_count,
        issue_line,
        rejection.diagnostic_summary
    )
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
    pub timeline_display: TimelineDisplayContract,
    pub enabled: bool,
    pub empty_message: Option<String>,
    edit_availability: Option<TimelineEditAvailability>,
    track_refs: Vec<AppTimelineTrackRef>,
    clip_refs: Vec<Vec<ClipId>>,
    transition_refs: Vec<Vec<VideoTransitionId>>,
    nested_sequence_refs: Vec<Vec<Option<SequenceId>>>,
    pub waveform_display: WaveformDisplay,
    pub(crate) waveform_source: Option<AudioWaveformSource>,
}

impl Default for TimelinePanelModel {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            playhead_frame: 0,
            in_point_frame: 0,
            out_point_frame: None,
            timeline_display: TimelineDisplayContract::default(),
            enabled: false,
            empty_message: None,
            edit_availability: None,
            track_refs: Vec::new(),
            clip_refs: Vec::new(),
            transition_refs: Vec::new(),
            nested_sequence_refs: Vec::new(),
            waveform_display: WaveformDisplay::BottomAligned,
            waveform_source: None,
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
    lift_range: bool,
    extract_range: bool,
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
            lift_range: app_state_action_enabled(&timeline_lift_range_action(), state),
            extract_range: app_state_action_enabled(&timeline_extract_range_action(), state),
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
            TimelineEditCommand::LinkSelection | TimelineEditCommand::UnlinkSelection => true,
            TimelineEditCommand::LiftInOutRange => self.lift_range,
            TimelineEditCommand::ExtractInOutRange => self.extract_range,
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
        state.active_sequence().map_or_else(Self::empty, |sequence| {
            Self::from_sequence_with_library(
                sequence,
                state.selected_clips(),
                state.selected_tracks(),
                state.asset_library(),
                state.selected_video_transition(),
                Some(state),
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
        Self::from_sequence_with_library(
            sequence,
            selected_clips,
            selected_tracks,
            None,
            None,
            None,
        )
    }

    /// Same as [`from_sequence`] but attaches audio waveform peaks for
    /// clips whose backing assets exist in `library`.
    fn from_sequence_with_library(
        sequence: &Sequence,
        selected_clips: &[SelectedClipRef],
        selected_tracks: &[TrackId],
        library: Option<&AssetLibrary>,
        selected_transition: Option<SelectedVideoTransitionRef>,
        state: Option<&AppState>,
    ) -> Self {
        let mut link_groups = BTreeMap::<ClipLinkGroupId, (usize, bool)>::new();
        for track in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
            for clip in &track.clips {
                if let Some(group) = clip.link_group {
                    let entry = link_groups.entry(group).or_insert((0, true));
                    entry.0 += 1;
                    entry.1 &= !track.is_locked;
                }
            }
        }
        let video_tracks = sequence.video_tracks.iter().enumerate().rev().map(|(index, track)| {
            let label = format!("V{}", index + 1);
            let mut projection = timeline_track_projection_from_sequence_track(
                track,
                true,
                sequence.settings.frame_rate,
                selected_clips,
                selected_tracks,
                library,
                &link_groups,
                state.is_none_or(|state| state.timeline_track_targeted(sequence.id, track.id)),
                state.is_none_or(|state| state.timeline_track_sync_locked(sequence.id, track.id)),
            );
            projection.track.label = label;
            let transition_views = timeline_transition_views_for_track(
                sequence,
                track,
                sequence.settings.frame_rate,
                selected_transition,
                state,
            );
            let transition_ids = transition_views
                .iter()
                .map(|(transition_id, _)| *transition_id)
                .collect::<Vec<_>>();
            projection.track.transitions =
                transition_views.into_iter().map(|(_, transition)| transition).collect();
            (
                AppTimelineTrackRef { track_id: track.id, is_video_track: true },
                projection.clip_ids,
                projection.nested_sequence_ids,
                transition_ids,
                projection.track,
            )
        });
        let audio_tracks = sequence.audio_tracks.iter().enumerate().map(|(index, track)| {
            let mut projection = timeline_track_projection_from_sequence_track(
                track,
                false,
                sequence.settings.frame_rate,
                selected_clips,
                selected_tracks,
                library,
                &link_groups,
                state.is_none_or(|state| state.timeline_track_targeted(sequence.id, track.id)),
                state.is_none_or(|state| state.timeline_track_sync_locked(sequence.id, track.id)),
            );
            projection.track.label = format!("A{}", index + 1);
            (
                AppTimelineTrackRef { track_id: track.id, is_video_track: false },
                projection.clip_ids,
                projection.nested_sequence_ids,
                Vec::new(),
                projection.track,
            )
        });

        let mut tracks = Vec::new();
        let mut track_refs = Vec::new();
        let mut clip_refs = Vec::new();
        let mut nested_sequence_refs = Vec::new();
        let mut transition_refs = Vec::new();
        for (track_ref, clip_ids, nested_ids, transition_ids, track) in
            video_tracks.chain(audio_tracks)
        {
            track_refs.push(track_ref);
            clip_refs.push(clip_ids);
            nested_sequence_refs.push(nested_ids);
            transition_refs.push(transition_ids);
            tracks.push(track);
        }
        let display = sequence.settings.timeline_display_contract();
        let display_valid = display.is_ok();
        let display_error =
            display.as_ref().err().map(|error| format!("序列时间显示设置无效\n{error}"));
        let empty_message = display_error.or_else(|| {
            tracks
                .is_empty()
                .then(|| "当前序列没有轨道\n添加视频轨道或音频轨道后开始编辑".to_owned())
        });
        Self {
            tracks,
            playhead_frame: sequence
                .playhead
                .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)
                .map(|position| position.frame.max(0))
                .unwrap_or(0),
            in_point_frame: sequence
                .in_point()
                .to_frame_position(sequence.settings.frame_rate, FrameRounding::Floor)
                .map(|position| position.frame.max(0))
                .unwrap_or(0),
            out_point_frame: sequence.out_point().and_then(|time| {
                time.to_frame_position(sequence.settings.frame_rate, FrameRounding::Floor)
                    .ok()
                    .map(|position| position.frame.max(0))
            }),
            timeline_display: display.unwrap_or_default(),
            enabled: display_valid,
            empty_message,
            edit_availability: None,
            track_refs,
            clip_refs,
            transition_refs,
            nested_sequence_refs,
            waveform_display: WaveformDisplay::BottomAligned,
            waveform_source: None,
        }
    }

    /// Empty timeline shown before a sequence is open.
    pub fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            playhead_frame: 0,
            in_point_frame: 0,
            out_point_frame: None,
            timeline_display: TimelineDisplayContract::default(),
            enabled: false,
            empty_message: Some("未载入序列\n打开项目或创建序列以开始编辑".into()),
            edit_availability: Some(TimelineEditAvailability::from_app_state(&AppState::new())),
            track_refs: Vec::new(),
            clip_refs: Vec::new(),
            transition_refs: Vec::new(),
            nested_sequence_refs: Vec::new(),
            waveform_display: WaveformDisplay::BottomAligned,
            waveform_source: None,
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

    fn clip_identity(
        &self,
        clip_ref: TimelineClipRef,
        mode: TimelineClipSelectionMode,
    ) -> Option<TimelineSelectClipPayload> {
        let clip_id = self.clip_id(clip_ref)?;
        let mode = match mode {
            TimelineClipSelectionMode::Replace => TimelineClipSelectionModePayload::Replace,
            TimelineClipSelectionMode::Toggle => TimelineClipSelectionModePayload::Toggle,
            TimelineClipSelectionMode::Preserve => TimelineClipSelectionModePayload::Preserve,
        };
        Some(TimelineSelectClipPayload { clip_id, mode })
    }

    fn clip_id(&self, clip_ref: TimelineClipRef) -> Option<ClipId> {
        self.clip_refs.get(clip_ref.track_index)?.get(clip_ref.clip_index).copied()
    }

    fn transition_identity(
        &self,
        transition_ref: TimelineTransitionRef,
    ) -> Option<TimelineSelectVideoTransitionPayload> {
        let transition_id = *self
            .transition_refs
            .get(transition_ref.track_index)?
            .get(transition_ref.transition_index)?;
        Some(TimelineSelectVideoTransitionPayload { transition_id })
    }

    fn cut_transition_payload(
        &self,
        cut_ref: TimelineCutRef,
    ) -> Option<TimelineCreateCrossDissolvePayload> {
        let clips = self.clip_refs.get(cut_ref.track_index)?;
        Some(TimelineCreateCrossDissolvePayload {
            left_clip_id: *clips.get(cut_ref.left_clip_index)?,
            right_clip_id: *clips.get(cut_ref.right_clip_index)?,
        })
    }

    fn transition_resize_payload(
        &self,
        resize: TimelineTransitionResize,
    ) -> Option<TimelineSetVideoTransitionRangePayload> {
        let transition_id = self.transition_identity(resize.transition_ref)?.transition_id;
        Some(TimelineSetVideoTransitionRangePayload {
            transition_id,
            start_frame: resize.new_start_frame.max(0),
            end_frame: resize
                .new_start_frame
                .saturating_add(resize.new_duration_frames.max(1))
                .max(1),
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
            TimelineTrackControl::Target | TimelineTrackControl::SyncLock => return None,
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

    fn track_targeting_payload(
        &self,
        control: TimelineTrackControl,
        track_ref: TimelineTrackRef,
        track: &TimelineTrack,
    ) -> Option<TimelineSetTrackTargetingPayload> {
        let identity = self.track_identity(track_ref)?;
        let (control, enabled) = match control {
            TimelineTrackControl::Target => {
                (TimelineTrackTargetingControl::Target, !track.targeted)
            }
            TimelineTrackControl::SyncLock => {
                (TimelineTrackTargetingControl::SyncLock, !track.sync_locked)
            }
            TimelineTrackControl::Visibility
            | TimelineTrackControl::Mute
            | TimelineTrackControl::Lock => return None,
        };
        Some(TimelineSetTrackTargetingPayload { track_id: identity.track_id, control, enabled })
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
        let clip_id = self.clip_id(movement.clip_ref)?;
        let source = *self.track_refs.get(movement.clip_ref.track_index)?;
        let target = *self.track_refs.get(movement.new_track_index)?;
        if source.is_video_track != target.is_video_track {
            return None;
        }
        Some(TimelineMoveClipPayload {
            target_track_id: target.track_id,
            is_video_track: target.is_video_track,
            clip_id,
            frame: movement.new_start_frame.max(0),
        })
    }

    fn trim_payload(&self, trim: TimelineClipTrim) -> Option<TimelineTrimClipsPayload> {
        let clip_id = self.clip_id(trim.clip_ref)?;
        let edge = match trim.edge {
            TimelineTrimEdge::In => TimelineTrimPayloadEdge::In,
            TimelineTrimEdge::Out => TimelineTrimPayloadEdge::Out,
        };
        let frame = match trim.edge {
            TimelineTrimEdge::In => trim.new_start_frame,
            TimelineTrimEdge::Out => trim.new_start_frame + trim.new_duration_frames,
        };
        Some(TimelineTrimClipsPayload { clip_ids: vec![clip_id], edge, frame: frame.max(0) })
    }
}

/// One editable key projected into the normalized Curve Editor viewport.
#[derive(Debug, Clone)]
pub struct InspectorCurveKeyModel {
    /// Stable author identity. Virtual Clip-boundary points have no key yet.
    pub keyframe_id: Option<KeyframeId>,
    /// Normalized screen-space position.
    pub point: CurvePoint,
}

/// Stable author target plus editable keys and read-only evaluated samples.
#[derive(Debug, Clone)]
pub struct InspectorCurveModel {
    /// Stable property instance and definition identity.
    pub property: AnimationParameterAddress,
    /// Editable keys in monotonic-time order.
    pub keys: Vec<InspectorCurveKeyModel>,
    /// Evaluated samples used to paint Hold/Bezier semantics faithfully.
    pub display_points: Vec<CurvePoint>,
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
    /// Whether the selected Clip owns the editable solid-color property.
    pub shows_tint: bool,
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
    /// Canonical source-time state for file-backed or nested content.
    pub source_timing: Option<InspectorSourceTimingModel>,
    /// Preferred color-picker area style for this inspector instance.
    pub tint_area_mode: ColorPickerAreaMode,
    /// Opacity animation projected through stable property/key identities.
    pub opacity_curve: Option<InspectorCurveModel>,
    /// Placement-local audio Component Edits and their source choices.
    pub audio_components: Vec<InspectorAudioComponentModel>,
    /// Unique Clip Processing Scope Racks projected through the shared Rack Module.
    pub(crate) audio_processor_racks: Vec<AudioProcessorRackModel>,
    /// Definition-backed properties owned by the selected Clip content.
    pub clip_properties: Vec<InspectorEffectPropertyModel>,
    /// Effects currently attached to the selected clip.
    pub effects: Vec<InspectorEffectModel>,
}

/// One placement-local audio Component Edit shown by the Inspector.
#[derive(Debug, Clone)]
pub struct InspectorAudioComponentModel {
    /// Stable edit identity targeted by source-selection actions.
    pub edit_id: AudioComponentEditId,
    /// Trigger text for the selected logical source.
    pub source_label: String,
    /// Valid logical sources from the owning Asset or nested Sequence.
    pub source_options: Vec<InspectorAudioSourceOptionModel>,
    /// Asset-global physical binding editor for media Components only.
    pub binding: Option<InspectorAudioBindingModel>,
    /// Exact channel-mapping policy, observed source evidence, and review matrix.
    pub(crate) channel_mapping: AudioChannelMappingModel,
    /// Whether this Component contributes signal.
    pub enabled: bool,
    /// Static post-processing placement volume in dB.
    pub volume_db: f64,
    /// Exact Component-local volume curve projected into the visible Clip span.
    pub(crate) volume_automation: Option<AudioAutomationCurveModel>,
    /// Static stereo pan/balance in normalized `[-1, 1]` units.
    pub pan: f64,
    /// Exact Component-local pan curve projected into the visible Clip span.
    pub(crate) pan_automation: Option<AudioAutomationCurveModel>,
    /// Optional exact unary fade beginning at the Clip in edge.
    pub fade_in: Option<AudioFade>,
    /// Optional exact unary fade ending at the Clip out edge.
    pub fade_out: Option<AudioFade>,
    /// Exact upper bound shared by both edge fades.
    pub clip_duration: TimelineTime,
}

/// One logical source option for a Clip audio Component Edit.
#[derive(Debug, Clone)]
pub struct InspectorAudioSourceOptionModel {
    /// Human-readable Component or public-output label.
    pub label: String,
    /// Typed target; physical media indices never enter Timeline authoring.
    pub source: InspectorAudioComponentSourcePayload,
    /// Whether this option is the edit's current source.
    pub selected: bool,
    /// Whether selecting it preserves the Clip's author invariants.
    pub selectable: bool,
}

/// Asset-global physical stream mapping shown under one media Component Edit.
#[derive(Debug, Clone)]
pub struct InspectorAudioBindingModel {
    /// Asset that owns the Component catalog.
    pub asset_id: AssetId,
    /// Stable Component identity preserved by a rebind.
    pub component_id: AudioSourceComponentId,
    /// Trigger text describing the current physical binding.
    pub label: String,
    /// Current stored probe candidates. Rebind re-probes before committing.
    pub options: Vec<InspectorAudioStreamOptionModel>,
}

/// One physical stream candidate for an explicit Asset Component repair.
#[derive(Debug, Clone)]
pub struct InspectorAudioStreamOptionModel {
    /// Human-readable stream evidence.
    pub label: String,
    /// Absolute container stream index passed to the Asset rebind transaction.
    pub stream_index: u32,
    /// Whether current stored evidence matches the Component binding exactly.
    pub selected: bool,
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
    /// Stable parameter schema consumed independently from the instance address.
    pub schema: ParameterSchema,
    /// Namespaced property path, e.g. `effect.<id>.exposure`.
    pub path: String,
    /// Human-readable property name from the descriptor.
    pub label: String,
    /// The evaluated value at the current playback time.
    pub value: PropertyValue,
    /// UI min/max bounds extracted from the descriptor.
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Author-valid bounds; soft min/max above only shape slider travel.
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub step: Option<f64>,
    /// Whether the property supports animation.
    pub is_animatable: bool,
}

fn inspector_property_model(
    path: &str,
    property: &mondrian_core::automation::AnimatedProperty,
    author_time: TimelineTime,
) -> InspectorEffectPropertyModel {
    let numeric = property.descriptor.schema.numeric;
    InspectorEffectPropertyModel {
        schema: property.descriptor.schema.clone(),
        path: path.to_owned(),
        label: property.descriptor.display_name.clone(),
        value: property.evaluate(author_time),
        min: numeric.map(|contract| contract.soft_range.min),
        max: numeric.map(|contract| contract.soft_range.max),
        hard_min: numeric.map(|contract| contract.hard_range.min),
        hard_max: numeric.map(|contract| contract.hard_range.max),
        step: numeric.and_then(|contract| contract.step),
        is_animatable: property.descriptor.schema.is_animatable,
    }
}

impl InspectorPanelModel {
    pub fn from_app_state(state: &AppState) -> Self {
        let Some(sequence) = state.active_sequence() else {
            return Self::empty();
        };
        let Some(selection) = state.primary_selected_clip() else {
            return Self::empty();
        };
        let Some((resolved_selection, clip)) = clip_for_selection(sequence, &selection) else {
            return Self::empty();
        };

        let time = state.current_timeline_time().ok().flatten().unwrap_or(sequence.playhead);
        let clip_author_time = clip_visual_author_time_at(clip, time).unwrap_or(clip.clip_time_in);
        let opacity = (clip.transform.evaluate_opacity(clip_author_time) * 100.0).clamp(0.0, 100.0);
        let position = clip.transform.get_position(clip_author_time);
        let scale = clip.transform.get_scale(clip_author_time);
        let is_editable = !selected_clip_track_is_locked(state, resolved_selection);
        let clip_properties = clip
            .content
            .basic_title()
            .map(|title| {
                mondrian_core::BasicTitle::PROPERTY_PATHS
                    .iter()
                    .filter_map(|path| {
                        title.property_bag().property(path).map(|property| {
                            inspector_property_model(path, property, clip_author_time)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
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
                .content
                .solid_color()
                .or_else(|| timeline_clip_color(clip, resolved_selection.is_video_track))
                .unwrap_or_else(|| current_theme().colors.media_video),
            shows_tint: clip.is_solid_color(),
            position_x: position.x,
            position_y: position.y,
            scale_percent: scale.x * 100.0,
            rotation_degrees: clip_rotation_degrees(clip, time),
            in_frame: clip
                .position
                .to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)
                .map(|position| position.frame as f32)
                .unwrap_or(0.0),
            out_frame: clip
                .end_position()
                .and_then(|time| {
                    time.to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)
                        .map_err(Into::into)
                })
                .map(|position| position.frame as f32)
                .unwrap_or(0.0),
            max_frame: sequence
                .total_duration()
                .and_then(|time| {
                    time.to_frame_position(sequence.settings.frame_rate, FrameRounding::Ceil)
                        .map_err(Into::into)
                })
                .map(|position| position.frame.max(1) as f32)
                .unwrap_or(1.0),
            source_timing: inspector_source_timing_model(
                state,
                sequence,
                resolved_selection,
                clip,
                time,
            ),
            tint_area_mode: ColorPickerAreaMode::Wheel,
            opacity_curve: opacity_curve_model_for_clip(clip, time),
            audio_components: inspector_audio_components(state, sequence, resolved_selection, clip),
            audio_processor_racks: clip_processing_scope_racks(sequence, clip),
            clip_properties,
            effects: clip
                .effects
                .iter()
                .map(|effect| InspectorEffectModel {
                    effect_id: effect.id,
                    label: effect_display_name(&effect.effect_type),
                    enabled: effect.is_enabled,
                    properties: effect
                        .properties
                        .iter()
                        .map(|(path, property)| {
                            inspector_property_model(path, property, clip_author_time)
                        })
                        .collect(),
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
            shows_tint: false,
            position_x: 0.0,
            position_y: 0.0,
            scale_percent: 100.0,
            rotation_degrees: 0.0,
            in_frame: 0.0,
            out_frame: 1.0,
            max_frame: 1.0,
            source_timing: None,
            tint_area_mode: ColorPickerAreaMode::Wheel,
            opacity_curve: None,
            audio_components: Vec::new(),
            audio_processor_racks: Vec::new(),
            clip_properties: Vec::new(),
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
            shows_tint: true,
            position_x: 12.0,
            position_y: -8.0,
            scale_percent: 100.0,
            rotation_degrees: 0.0,
            in_frame: 0.0,
            out_frame: 96.0,
            max_frame: 240.0,
            source_timing: None,
            tint_area_mode: ColorPickerAreaMode::Wheel,
            opacity_curve: None,
            audio_components: Vec::new(),
            audio_processor_racks: Vec::new(),
            clip_properties: Vec::new(),
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
    /// Materialized editable delivery settings, independent from catalog order.
    pub preset: ExportPreset,
    /// Whether the materialized settings differ from their selected reset point.
    pub preset_customized: bool,
    pub sequences: Vec<ExportSequenceOptionModel>,
    pub selected_sequence_id: Option<SequenceId>,
    pub range: TimelineExportRange,
    pub output_path: String,
    pub delivery_error: Option<String>,
    pub status: Option<(String, bool)>,
    pub jobs: Vec<ExportJobModel>,
    pub can_clear_completed_jobs: bool,
}

#[derive(Debug, Clone)]
pub struct ExportPresetOptionModel {
    pub id: BuiltinExportPreset,
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
    pub color_diagnostics: Option<String>,
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
        let Some(sequence) = state.active_sequence() else {
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
            .map(|option| ExportPresetOptionModel {
                id: option.id,
                label: option.label,
                preset: option.preset,
            })
            .collect::<Vec<_>>();
        let selected_preset_idx = presets
            .iter()
            .position(|option| option.id == state.export_draft.selected_builtin_preset)
            .unwrap_or_default();
        let preset = state.export_draft.preset.clone();
        let preset_customized =
            presets.get(selected_preset_idx).is_none_or(|option| option.preset != preset);
        let sequence_snapshots = state.export_sequences_snapshot();
        let sequences = sequence_snapshots
            .iter()
            .map(|sequence| ExportSequenceOptionModel {
                id: sequence.id,
                name: sequence.name.clone(),
                video_clips: sequence.video_tracks.iter().map(|track| track.clips.len()).sum(),
                audio_clips: sequence.audio_tracks.iter().map(|track| track.clips.len()).sum(),
            })
            .collect::<Vec<_>>();
        let selected_sequence_id = state
            .export_draft
            .selected_sequence_id
            .filter(|id| sequences.iter().any(|sequence| sequence.id == *id))
            .or(state.active_sequence_id())
            .or(state.default_sequence_id())
            .filter(|id| sequences.iter().any(|sequence| sequence.id == *id))
            .or_else(|| sequences.first().map(|sequence| sequence.id));
        let delivery_error = selected_sequence_id
            .and_then(|id| sequence_snapshots.iter().find(|sequence| sequence.id == id))
            .and_then(|sequence| {
                resolve_export_delivery(
                    &preset,
                    &sequence.settings,
                    state.project_color_environment(),
                )
                .err()
                .map(|error| error.to_string())
            });

        let jobs = state.export_jobs_snapshot();
        let queue_count = jobs.len();
        let can_clear_completed_jobs = jobs.iter().any(|job| job.status.is_terminal());
        let jobs = jobs
            .into_iter()
            .rev()
            .take(6)
            .map(|job| ExportJobModel {
                id: job.id,
                title: export_job_title(job.output_path.as_path()),
                status: export_job_status_label(&job.status, job.progress),
                color_diagnostics: export_job_color_diagnostics_label(job.diagnostics.color),
                progress_percent: (job.progress.fraction.clamp(0.0, 1.0) * 100.0).round() as u8,
                can_cancel: job.status.can_cancel(),
                is_completed: job.status.is_terminal(),
            })
            .collect();

        Self {
            queue_count,
            presets,
            selected_preset_idx,
            preset,
            preset_customized,
            sequences,
            selected_sequence_id,
            range: state.export_draft.range,
            output_path: state.export_draft.output_path.clone(),
            delivery_error,
            status: state.status_hint.clone(),
            jobs,
            can_clear_completed_jobs,
        }
    }

    fn selected_preset(&self) -> Option<&ExportPreset> {
        (!self.presets.is_empty()).then_some(&self.preset)
    }

    fn can_enqueue(&self) -> bool {
        self.selected_preset().is_some()
            && self.selected_sequence_id.is_some()
            && !self.output_path.trim().is_empty()
            && self.delivery_error.is_none()
    }

    fn can_choose_output(&self) -> bool {
        self.selected_preset().is_some() && self.selected_sequence_id.is_some()
    }

    fn can_select_range(&self) -> bool {
        self.selected_sequence_id.is_some()
    }

    fn readiness_status(&self) -> String {
        if self.selected_sequence_id.is_none() {
            "导出前请打开或选择序列".to_owned()
        } else if self.selected_preset().is_none() {
            "没有可用导出预设".to_owned()
        } else if let Some(error) = &self.delivery_error {
            format!("交付设置不兼容：{error}")
        } else if let Some((message, true)) = &self.status {
            format!("错误：{message}")
        } else if self.output_path.trim().is_empty() {
            "选择输出路径后即可加入队列".to_owned()
        } else if let Some((message, false)) = &self.status {
            message.clone()
        } else {
            "就绪".to_owned()
        }
    }

    fn enqueue_payload(&self) -> Option<ExportEnqueuePayload> {
        let preset = self.selected_preset()?.clone();
        let sequence_id = self.selected_sequence_id?;
        if self.delivery_error.is_some() {
            return None;
        }
        let output_path = self.output_path.trim();
        if output_path.is_empty() {
            return None;
        }
        Some(ExportEnqueuePayload {
            preset,
            sequence_id: Some(sequence_id),
            range: self.range,
            output_path: output_path.into(),
            output_policy: mondrian_export::preset::ExportOutputPolicy::CreateNew,
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
        slot(PanelKind::Scopes, models.clone()),
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
        slot(PanelKind::Mixer, models.clone()),
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
        PanelKind::Scopes => &[PanelKind::Scopes],
        PanelKind::Timeline => &[PanelKind::Timeline],
        PanelKind::Inspector => &[PanelKind::Inspector],
        PanelKind::Mixer => &[PanelKind::Mixer, PanelKind::Inspector],
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
        PanelKind::Scopes => Box::new(scopes_panel(&models.scopes)),
        PanelKind::Timeline => Box::new(timeline_panel(&models.timeline)),
        PanelKind::Export => Box::new(ScrollView::new(Some(Box::new(export_panel(
            &models.export,
        ))))),
        PanelKind::Inspector => Box::new(ScrollView::new(Some(Box::new(inspector_panel(
            &models.inspector,
        ))))),
        PanelKind::Mixer => Box::new(ScrollView::new(Some(Box::new(audio_mixer_panel(
            &models.mixer,
        ))))),
        PanelKind::NodeGraph => Box::new(node_graph_panel(&models.node_graph)),
    }
}

fn scopes_panel(model: &ScopesPanelModel) -> VideoScopesSurface {
    let surface = VideoScopesSurface::new();
    match model.textures.clone() {
        Some(textures) => surface.with_textures(textures),
        None => surface,
    }
}

fn viewer_panel(model: &ViewerPanelModel) -> ViewerSurface {
    let surface = ViewerSurface::new(model.title.clone(), model.width, model.height)
        .with_status(model.status.clone())
        .with_status_tone(model.status_tone)
        .with_resolution_label(model.resolution_label.clone())
        .with_position_label(model.position_label.clone())
        .with_duration_label(model.duration_label.clone())
        .with_zoom_label(model.zoom_label.clone())
        .with_zoom_scale(model.zoom_scale)
        .with_preview_quality_label(model.preview_quality_label.clone())
        .with_canvas_background(model.canvas_background)
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
    match model.frame_content.clone() {
        Some(frame_content) => surface.with_frame_content(frame_content),
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

struct TimelineTrackProjection {
    track: TimelineTrack,
    clip_ids: Vec<ClipId>,
    nested_sequence_ids: Vec<Option<SequenceId>>,
}

fn timeline_track_projection_from_sequence_track(
    track: &Track,
    is_video_track: bool,
    frame_rate: Rational,
    selected_clips: &[SelectedClipRef],
    selected_tracks: &[TrackId],
    library: Option<&AssetLibrary>,
    link_groups: &BTreeMap<ClipLinkGroupId, (usize, bool)>,
    targeted: bool,
    sync_locked: bool,
) -> TimelineTrackProjection {
    let muted = track.is_muted;
    let locked = track.is_locked;
    let visible = track.is_visible;
    let selected = selected_tracks.contains(&track.id);
    let mut clips = Vec::with_capacity(track.clips.len());
    let mut clip_ids = Vec::with_capacity(track.clips.len());
    let mut nested_sequence_ids = Vec::with_capacity(track.clips.len());
    for clip in &track.clips {
        let Some(view) = timeline_clip_from_sequence_clip(
            is_video_track,
            clip,
            frame_rate,
            selected_clips,
            library,
            track.is_locked,
            link_groups,
        ) else {
            continue;
        };
        clips.push(view);
        clip_ids.push(clip.id);
        nested_sequence_ids.push(clip.nested_sequence_id());
    }

    let track = if is_video_track {
        TimelineTrack::video(track.name.clone(), clips)
    } else {
        TimelineTrack::audio(track.name.clone(), clips)
    };
    TimelineTrackProjection {
        track: track
            .selected(selected)
            .targeted(targeted)
            .sync_locked(sync_locked)
            .visible(visible)
            .muted(muted)
            .locked(locked),
        clip_ids,
        nested_sequence_ids,
    }
}

fn timeline_transition_views_for_track(
    sequence: &Sequence,
    track: &Track,
    frame_rate: Rational,
    selected_transition: Option<SelectedVideoTransitionRef>,
    state: Option<&AppState>,
) -> Vec<(VideoTransitionId, TimelineTransition)> {
    sequence
        .video_transitions
        .iter()
        .filter_map(|transition| {
            let left = track.clips.iter().find(|clip| clip.id == transition.left)?;
            let right = track.clips.iter().find(|clip| clip.id == transition.right)?;
            let start_frame = transition
                .sequence_range
                .start
                .to_frame_position(frame_rate, FrameRounding::Floor)
                .ok()?
                .frame
                .max(0);
            let end_frame = transition
                .sequence_range
                .end()
                .ok()?
                .to_frame_position(frame_rate, FrameRounding::Ceil)
                .ok()?
                .frame
                .max(start_frame.saturating_add(1));
            let cut_frame = left
                .end_position()
                .ok()?
                .to_frame_position(frame_rate, FrameRounding::Nearest)
                .ok()?
                .frame
                .max(0);
            let minimum_start_frame = left
                .position
                .to_frame_position(frame_rate, FrameRounding::Floor)
                .ok()?
                .frame
                .max(0);
            let maximum_end_frame = right
                .end_position()
                .ok()?
                .to_frame_position(frame_rate, FrameRounding::Ceil)
                .ok()?
                .frame
                .max(end_frame);
            let label = match &transition.transition_type {
                VideoTransitionType::CrossDissolve => "Cross Dissolve".to_owned(),
                VideoTransitionType::Plugin { definition_id } => definition_id.clone(),
            };
            let mut view = TimelineTransition::new(
                label,
                start_frame,
                end_frame.saturating_sub(start_frame),
                cut_frame,
                minimum_start_frame,
                maximum_end_frame,
            )
            .selected(
                selected_transition
                    .is_some_and(|selection| selection.transition_id == transition.id),
            )
            .enabled(transition.is_enabled);
            if let Some(state) = state {
                let issue = match state.video_transition_handle_state(transition.id) {
                    Ok(VideoTransitionHandleState::Available) => None,
                    Ok(VideoTransitionHandleState::Insufficient) => {
                        Some("当前源素材句柄不足，预览和导出将失败关闭".to_owned())
                    }
                    Ok(VideoTransitionHandleState::Unresolved { reason }) => {
                        Some(format!("无法解析当前源素材句柄：{reason}"))
                    }
                    Err(error) => Some(format!("无法解析当前源素材句柄：{error}")),
                };
                if let Some(issue) = issue {
                    view = view.with_handle_issue(issue);
                }
            }
            Some((transition.id, view))
        })
        .collect()
}

fn timeline_clip_from_sequence_clip(
    is_video_track: bool,
    clip: &Clip,
    frame_rate: Rational,
    selected_clips: &[SelectedClipRef],
    library: Option<&AssetLibrary>,
    track_locked: bool,
    link_groups: &BTreeMap<ClipLinkGroupId, (usize, bool)>,
) -> Option<TimelineClip> {
    let selected = selected_clips.iter().any(|selection| selection.clip_id == clip.id);
    let label = clip.label.clone().unwrap_or_else(|| default_clip_label(clip));
    let is_video = is_video_track;
    let kind = if clip.is_adjustment_layer() {
        TimelineClipKind::Adjustment
    } else if clip.is_nested_sequence() {
        TimelineClipKind::NestedSequence
    } else if clip.is_solid_color() {
        TimelineClipKind::SolidColor
    } else if clip.is_basic_title() {
        TimelineClipKind::BasicTitle
    } else if is_video {
        TimelineClipKind::Video
    } else {
        TimelineClipKind::Audio
    };
    let start = clip
        .position
        .to_frame_position(frame_rate, FrameRounding::Nearest)
        .ok()?
        .frame
        .max(0);
    let duration = clip
        .duration
        .to_frame_position(frame_rate, FrameRounding::Ceil)
        .ok()?
        .frame
        .max(1);
    let mut view = TimelineClip::new(label, start, duration)
        .kind(kind)
        .selected(selected)
        .disabled(clip.is_disabled)
        .nested(clip.is_nested_sequence());
    view.link_group_editable = !track_locked;
    if let Some(group) = clip.link_group {
        let (member_count, editable) = link_groups.get(&group).copied().unwrap_or((2, false));
        view = view.linked(group, member_count, editable);
    }
    if let Some(color) = timeline_clip_color(clip, is_video) {
        view = view.with_color(color);
    }
    if kind == TimelineClipKind::Audio {
        if let Some(lib) = library {
            if let Some(asset_id) = clip.media_asset_id() {
                if let Ok(Some(record)) = lib.get_asset(asset_id) {
                    if let Some(selection) =
                        record.admitted_audio_source_selection(AudioSourceComponentId::primary())
                    {
                        view = view.with_source_identity(
                            record.id,
                            selection,
                            clip.source_origin().to_f64(),
                            clip.source_terminal_boundary().ok()?.to_f64(),
                        );
                    }
                }
            }
        }
    }
    Some(view)
}

fn default_clip_label(clip: &Clip) -> String {
    if clip.is_adjustment_layer() {
        "Adjustment".to_string()
    } else if clip.is_nested_sequence() {
        "Nested Sequence".to_string()
    } else if clip.is_solid_color() {
        "Solid Color".to_string()
    } else if clip.is_basic_title() {
        "Basic Title".to_string()
    } else {
        format!("Clip {}", clip.id)
    }
}

fn timeline_clip_color(clip: &Clip, is_video_track: bool) -> Option<Color> {
    if let Some(color) = clip.content.solid_color() {
        return Some(color);
    }
    let colors = current_theme().colors.clone();
    if clip.is_adjustment_layer() {
        Some(colors.timeline_clip_adjustment)
    } else if clip.is_basic_title() {
        Some(colors.timeline_clip_title)
    } else if clip.is_nested_sequence() {
        Some(colors.timeline_clip_nested)
    } else if is_video_track {
        Some(colors.timeline_clip_video)
    } else {
        Some(colors.timeline_clip_audio)
    }
}

fn clip_visual_author_time_at(
    clip: &Clip,
    timeline_time: TimelineTime,
) -> mondrian_core::Result<TimelineTime> {
    let placement_end = clip.end_position()?;
    clip.timeline_to_clip_time(timeline_time.clamp(clip.position, placement_end))
}

fn clip_rotation_degrees(clip: &Clip, time: TimelineTime) -> f32 {
    let author_time = clip_visual_author_time_at(clip, time).unwrap_or(clip.clip_time_in);
    clip.transform
        .to_property_bag()
        .evaluate(Transform2D::ROTATION_PATH, author_time)
        .and_then(|value| value.as_f32())
        .unwrap_or(0.0)
}

fn opacity_curve_model_for_clip(clip: &Clip, _time: TimelineTime) -> Option<InspectorCurveModel> {
    let bag = clip.transform.to_property_bag();
    let opacity = bag.property(Transform2D::OPACITY_PATH)?;
    let numeric = opacity.descriptor.schema.numeric?;
    let value_span = numeric.soft_range.max - numeric.soft_range.min;
    if !value_span.is_finite() || value_span <= 0.0 {
        return None;
    }
    let start_tick = clip.clip_time_in;
    let end_tick = clip.clip_time_out().ok()?;
    let duration_ticks = end_tick.checked_sub(start_tick).ok()?;
    if duration_ticks.is_zero() {
        return None;
    }

    let normalized_value = |value: PropertyValue| {
        let value = f64::from(value.as_f32()?);
        Some(((value - numeric.soft_range.min) / value_span).clamp(0.0, 1.0) as f32)
    };
    let normalized_time = |keyframe_time: TimelineTime| {
        let elapsed = keyframe_time.checked_sub(start_tick).ok()?.to_f64();
        Some((elapsed / duration_ticks.to_f64()).clamp(0.0, 1.0) as f32)
    };

    let mut keys = BTreeMap::<TimelineTime, InspectorCurveKeyModel>::new();
    keys.insert(
        start_tick,
        InspectorCurveKeyModel {
            keyframe_id: None,
            point: CurvePoint::new(0.0, normalized_value(opacity.evaluate(start_tick))?),
        },
    );
    keys.insert(
        end_tick,
        InspectorCurveKeyModel {
            keyframe_id: None,
            point: CurvePoint::new(1.0, normalized_value(opacity.evaluate(end_tick))?),
        },
    );
    for keyframe_time in opacity.keyframe_times() {
        if keyframe_time < start_tick || keyframe_time > end_tick {
            continue;
        }
        let keyframe = opacity.keyframe_at(keyframe_time)?;
        keys.insert(
            keyframe_time,
            InspectorCurveKeyModel {
                keyframe_id: Some(keyframe.id),
                point: CurvePoint::new(
                    normalized_time(keyframe_time)?,
                    normalized_value(keyframe.value)?,
                ),
            },
        );
    }

    const DISPLAY_SEGMENTS: i64 = 128;
    let display_points = (0..=DISPLAY_SEGMENTS)
        .map(|index| {
            let scale = TimeScale::new(index, DISPLAY_SEGMENTS).ok()?;
            let sample_time =
                start_tick.checked_add(duration_ticks.checked_scale(scale).ok()?).ok()?;
            Some(CurvePoint::new(
                index as f32 / DISPLAY_SEGMENTS as f32,
                normalized_value(opacity.evaluate(sample_time))?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;

    Some(InspectorCurveModel {
        property: opacity.address(),
        keys: keys.into_values().collect(),
        display_points,
    })
}

fn asset_grid_item_from_asset(
    asset: AssetRecord,
    thumbnails: Option<&dyn AssetThumbnailSource>,
    proxy_mode: bool,
    input_pipeline: Option<&AppShellInputColorPipelineDiagnostics>,
) -> AssetGridItem {
    let badge = asset_kind_badge(&asset.kind);
    let accent = asset_kind_accent(&asset.kind);
    let icon = asset_kind_icon(&asset.kind);
    let thumbnail_state = thumbnails.map(|source| source.thumbnail_for_asset(&asset));
    let context_menu_items =
        asset_grid_asset_context_menu_items(&asset, proxy_mode, input_pipeline);
    let offline = asset_is_offline(&asset);
    let proxied = proxy_mode && matches!(asset.kind, AssetKind::Video);
    let duration_label = asset
        .media_probe()
        .map(|probe| asset_duration_label(probe.duration))
        .unwrap_or_default();
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
            AssetThumbnailState::Failed(_) => item.with_thumbnail_failed(),
            AssetThumbnailState::Ready(thumbnail) => item.with_thumbnail(thumbnail),
        };
    }
    with_asset_icon(item, icon)
}

fn asset_grid_asset_context_menu_items(
    asset: &AssetRecord,
    proxy_mode: bool,
    input_pipeline: Option<&AppShellInputColorPipelineDiagnostics>,
) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if let Some(file_path) = asset.file_path() {
        let media_probe = asset.media_probe();
        items.push(asset_menu_item(
            MenuItem::new(
                "解释素材...",
                app_shell_interpret_asset_dialog_action(AppShellInterpretAssetDialogPayload {
                    asset_id: asset.id,
                    asset_name: asset.name.clone(),
                    interpretation: asset.interpretation,
                    auto_interpretation: media_probe
                        .and_then(|probe| probe.primary_video())
                        .map(|video| video.color_interpretation.clone()),
                    video_signal: media_probe.and_then(|probe| probe.primary_video()).map(
                        |video| AppShellVideoSignalDiagnostics {
                            range: video.color_range,
                            sampling: video.proven_sampling(),
                            color_metadata: video.color_metadata.clone(),
                            color_metadata_hints: video.color_metadata_hints.clone(),
                        },
                    ),
                    input_pipeline: input_pipeline.cloned(),
                }),
            ),
            AppIcon::Film,
        ));
        items.push(asset_menu_item(
            MenuItem::new(
                "在文件管理器中显示",
                app_shell_reveal_in_file_manager_action(AppShellRevealInFileManagerPayload {
                    path: file_path.to_path_buf(),
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

fn asset_is_offline(asset: &AssetRecord) -> bool {
    asset.file_path().is_some_and(|path| !path.exists())
}

fn asset_grid_items_from_library_records(
    folders: &[FolderRecord],
    assets: Vec<AssetRecord>,
    current_folder: Option<&FolderRecord>,
    thumbnails: Option<&dyn AssetThumbnailSource>,
    proxy_mode_assets: Option<&BTreeSet<AssetId>>,
    input_pipeline: Option<&AppShellInputColorPipelineDiagnostics>,
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
                asset_grid_item_from_asset(asset, thumbnails, proxy_mode, input_pipeline)
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
    action: Option<Action>,
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
        AssetKind::StillImage => "静帧",
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
        AssetKind::StillImage => colors.media_solid,
        AssetKind::Audio => colors.media_audio,
        AssetKind::AdjustmentLayer => colors.media_adjustment,
        AssetKind::SolidColor => colors.media_solid,
    }
}

fn asset_kind_icon(kind: &AssetKind) -> AppIcon {
    match kind {
        AssetKind::Video => AppIcon::Film,
        AssetKind::StillImage => AppIcon::Rectangle,
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

fn inspector_audio_components(
    state: &AppState,
    sequence: &Sequence,
    selection: SelectedClipRef,
    clip: &Clip,
) -> Vec<InspectorAudioComponentModel> {
    let asset = (!clip.is_nested_sequence())
        .then(|| {
            state.asset_library().and_then(|library| {
                clip.media_asset_id()
                    .and_then(|asset_id| library.get_asset(asset_id).ok().flatten())
            })
        })
        .flatten();
    let child = clip.nested_sequence_id().and_then(|sequence_id| {
        state.sequences().iter().find(|sequence| sequence.id == sequence_id)
    });

    clip.audio_components
        .iter()
        .map(|edit| match edit.source {
            AudioComponentSource::Media { component_id } => {
                let Some(asset) = asset.as_ref() else {
                    return inspector_audio_component_model(
                        edit,
                        format!("Component {component_id}（素材不可用）"),
                        Vec::new(),
                        None,
                        None,
                        clip.duration,
                        sequence,
                        selection.track_id,
                        clip.id,
                    );
                };
                let source_options = asset
                    .audio_components
                    .components
                    .iter()
                    .map(|component| {
                        let selected = component.id == component_id;
                        let used_by_sibling = clip.audio_components.iter().any(|sibling| {
                            sibling.id != edit.id
                                && matches!(
                                    sibling.source,
                                    AudioComponentSource::Media {
                                        component_id: sibling_id
                                    } if sibling_id == component.id
                                )
                        });
                        InspectorAudioSourceOptionModel {
                            label: asset_audio_component_label(asset, component.id),
                            source: InspectorAudioComponentSourcePayload::Media {
                                component_id: component.id,
                            },
                            selected,
                            selectable: selected || !used_by_sibling,
                        }
                    })
                    .collect::<Vec<_>>();
                let source_label = source_options
                    .iter()
                    .find(|option| option.selected)
                    .map(|option| option.label.clone())
                    .unwrap_or_else(|| format!("Component {component_id}（目录中不存在）"));
                let binding = asset
                    .audio_components
                    .components
                    .iter()
                    .find(|component| component.id == component_id)
                    .map(|component| InspectorAudioBindingModel {
                        asset_id: asset.id,
                        component_id,
                        label: asset_audio_binding_label(asset, component_id),
                        options: asset
                            .media_probe()
                            .map(|probe| {
                                probe
                                    .audio_streams
                                    .iter()
                                    .map(|stream| InspectorAudioStreamOptionModel {
                                        label: audio_stream_label(stream),
                                        stream_index: stream.index,
                                        selected: component.binding.matches_stream(stream),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                let source_layout = asset
                    .admitted_audio_source_selection(component_id)
                    .and_then(|selection| selection.source_layout().exact_signal_layout());
                inspector_audio_component_model(
                    edit,
                    source_label,
                    source_options,
                    binding,
                    source_layout,
                    clip.duration,
                    sequence,
                    selection.track_id,
                    clip.id,
                )
            }
            AudioComponentSource::NestedOutput { output_id } => {
                let source_options = child
                    .map(|sequence| {
                        sequence
                            .audio_program
                            .outputs
                            .iter()
                            .map(|output| InspectorAudioSourceOptionModel {
                                label: format!("{} · {}", output.name, output.id),
                                source: InspectorAudioComponentSourcePayload::NestedOutput {
                                    output_id: output.id,
                                },
                                selected: output.id == output_id,
                                selectable: true,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let source_label = source_options
                    .iter()
                    .find(|option| option.selected)
                    .map(|option| option.label.clone())
                    .unwrap_or_else(|| format!("嵌套输出 {output_id}（不可用）"));
                inspector_audio_component_model(
                    edit,
                    source_label,
                    source_options,
                    None,
                    child.and_then(|child| {
                        child
                            .audio_program
                            .outputs
                            .iter()
                            .any(|output| output.id == output_id)
                            .then_some(child.settings.audio_channel_layout)
                    }),
                    clip.duration,
                    sequence,
                    selection.track_id,
                    clip.id,
                )
            }
        })
        .collect()
}

fn inspector_audio_component_model(
    edit: &AudioComponentEdit,
    source_label: String,
    source_options: Vec<InspectorAudioSourceOptionModel>,
    binding: Option<InspectorAudioBindingModel>,
    observed_source_layout: Option<AudioChannelLayout>,
    clip_duration: TimelineTime,
    sequence: &Sequence,
    track_id: TrackId,
    clip_id: ClipId,
) -> InspectorAudioComponentModel {
    let viewport = component_automation_viewport(edit.id, edit.local_time_in, clip_duration);
    InspectorAudioComponentModel {
        edit_id: edit.id,
        source_label,
        source_options,
        binding,
        channel_mapping: project_audio_channel_mapping(
            &edit.channel_mapping,
            observed_source_layout,
            sequence.settings.audio_channel_layout,
        ),
        enabled: edit.enabled,
        volume_db: edit.volume_db,
        volume_automation: viewport.and_then(|viewport| {
            project_audio_automation(
                sequence,
                mondrian_timeline::AudioAutomationTarget::ComponentVolume {
                    track_id,
                    clip_id,
                    edit_id: edit.id,
                },
                viewport,
            )
        }),
        pan: edit.pan,
        pan_automation: viewport.and_then(|viewport| {
            project_audio_automation(
                sequence,
                mondrian_timeline::AudioAutomationTarget::ComponentPan {
                    track_id,
                    clip_id,
                    edit_id: edit.id,
                },
                viewport,
            )
        }),
        fade_in: edit.fades.fade_in,
        fade_out: edit.fades.fade_out,
        clip_duration,
    }
}

fn asset_audio_component_label(
    asset: &AssetRecord,
    component_id: AudioSourceComponentId,
) -> String {
    let Some(component) = asset
        .audio_components
        .components
        .iter()
        .find(|component| component.id == component_id)
    else {
        return format!("Component {component_id}（目录中不存在）");
    };
    let primary = (component_id == AudioSourceComponentId::primary()).then_some("Primary · ");
    let stream = asset.media_probe().and_then(|probe| {
        probe
            .audio_streams
            .iter()
            .find(|stream| component.binding.matches_stream(stream))
    });
    match stream {
        Some(stream) => format!(
            "{}{}",
            primary.unwrap_or_default(),
            audio_stream_label(stream)
        ),
        None => format!(
            "{}流 #{} · {}（需重绑定）",
            primary.unwrap_or_default(),
            component.binding.stream_index,
            native_audio_layout_label(&component.binding.channel_layout)
        ),
    }
}

fn asset_audio_binding_label(asset: &AssetRecord, component_id: AudioSourceComponentId) -> String {
    asset
        .audio_components
        .components
        .iter()
        .find(|component| component.id == component_id)
        .map(|component| {
            asset
                .media_probe()
                .and_then(|probe| {
                    probe
                        .audio_streams
                        .iter()
                        .find(|stream| component.binding.matches_stream(stream))
                })
                .map(audio_stream_label)
                .unwrap_or_else(|| format!("流 #{}（需重绑定）", component.binding.stream_index))
        })
        .unwrap_or_else(|| "无有效物理映射".to_owned())
}

fn audio_stream_label(stream: &AudioStreamInfo) -> String {
    let mut details = vec![
        format!("流 #{}", stream.index),
        native_audio_layout_label(&stream.channel_layout),
    ];
    if let Some(language) = stream.language.as_deref().filter(|value| !value.trim().is_empty()) {
        details.push(language.to_owned());
    }
    if let Some(title) = stream.title.as_deref().filter(|value| !value.trim().is_empty()) {
        details.push(title.to_owned());
    }
    if stream.is_default {
        details.push("Default".to_owned());
    }
    details.join(" · ")
}

fn native_audio_layout_label(layout: &ChannelLayout) -> String {
    match layout {
        ChannelLayout::Unspecified(channels) => format!("未指定 {channels}ch"),
        ChannelLayout::Mono => "Mono".to_owned(),
        ChannelLayout::Stereo => "Stereo".to_owned(),
        ChannelLayout::Surround51Side => "5.1(side)".to_owned(),
        ChannelLayout::Surround51Back => "5.1(back)".to_owned(),
        ChannelLayout::Surround71 => "7.1".to_owned(),
        ChannelLayout::Other(channels) => format!("其他 {channels}ch"),
    }
}

fn selected_clip_track_is_locked(state: &AppState, selection: SelectedClipRef) -> bool {
    let Some(sequence) = state.active_sequence() else {
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
    let Some(sequence) = state.active_sequence() else {
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
                let Some(target_folder_id) = item.id.strip_prefix("folder:").map(str::to_owned)
                else {
                    return AssetGridDropOutcome::Unhandled;
                };
                match payload {
                    DragPayload::Asset(asset_id) => AssetGridDropOutcome::Dispatch(
                        assets_move_asset_action(AssetsMoveAssetPayload {
                            asset_id: *asset_id,
                            folder_id: Some(target_folder_id),
                        }),
                    ),
                    DragPayload::AssetFolder(folder_id) => {
                        if folder_id == &target_folder_id {
                            AssetGridDropOutcome::Consumed
                        } else {
                            AssetGridDropOutcome::Dispatch(assets_move_folder_action(
                                AssetsMoveFolderPayload {
                                    folder_id: folder_id.clone(),
                                    parent_folder_id: Some(target_folder_id),
                                },
                            ))
                        }
                    }
                    DragPayload::AssetSelection { assets, folders } => {
                        move_asset_selection_action(assets, folders, Some(target_folder_id)).map_or(
                            AssetGridDropOutcome::Consumed,
                            AssetGridDropOutcome::Dispatch,
                        )
                    }
                    _ => AssetGridDropOutcome::Unhandled,
                }
            })
            .with_context_menu(asset_grid_context_menu_items(
                model.current_folder_id.as_deref(),
            ))
            .with_selection_context_menu(asset_grid_selection_context_menu_items);
    }
    grid
}

fn asset_grid_rename_action(_index: usize, item: &AssetGridItem, name: &str) -> Option<Action> {
    match item.drag_payload.as_ref() {
        Some(DragPayload::Asset(asset_id)) => {
            Some(assets_rename_asset_action(AssetsRenameAssetPayload {
                asset_id: *asset_id,
                name: name.to_owned(),
            }))
        }
        Some(DragPayload::AssetFolder(folder_id)) => {
            Some(assets_rename_folder_action(AssetsRenameFolderPayload {
                folder_id: folder_id.clone(),
                name: name.to_owned(),
            }))
        }
        _ => None,
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
        return None;
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

    let mut adjustment = Clip::new_adjustment_layer(
        AssetId::new(),
        crate::app::tt(36, tb),
        crate::app::tt(84, tb),
    )
    .expect("valid clip");
    adjustment.label = Some("Adjustment".to_string());
    sequence.video_tracks[2].add_clip(adjustment).expect("add adjustment");

    let title = Clip::new_nested_sequence(
        nested_id,
        crate::app::tt(132, tb),
        crate::app::tt(48, tb),
        Some("Title".to_string()),
    )
    .expect("valid clip");
    sequence.video_tracks[2].add_clip(title).expect("add title");

    let mut b_roll = Clip::new(
        AssetId::new(),
        crate::app::tt(18, tb),
        crate::app::tt(72, tb),
    )
    .expect("valid clip");
    b_roll.label = Some("B-roll".to_string());
    sequence.video_tracks[1].add_clip(b_roll).expect("add b-roll");

    let mut overlay = Clip::new_solid_color(
        AssetId::new(),
        Color::from_hex(0x805AD5),
        crate::app::tt(112, tb),
        crate::app::tt(56, tb),
    )
    .expect("valid clip");
    overlay.label = Some("Overlay".to_string());
    sequence.video_tracks[1].add_clip(overlay).expect("add overlay");

    let mut interview = Clip::new(
        AssetId::new(),
        crate::app::tt(0, tb),
        crate::app::tt(96, tb),
    )
    .expect("valid clip");
    interview.label = Some("Interview".to_string());
    sequence.video_tracks[0].add_clip(interview).expect("add interview");

    let mut cutaway = Clip::new(
        AssetId::new(),
        crate::app::tt(104, tb),
        crate::app::tt(72, tb),
    )
    .expect("valid clip");
    cutaway.label = Some("Cutaway".to_string());
    sequence.video_tracks[0].add_clip(cutaway).expect("add cutaway");

    let mut outro = Clip::new(
        AssetId::new(),
        crate::app::tt(190, tb),
        crate::app::tt(44, tb),
    )
    .expect("valid clip");
    outro.label = Some("Outro".to_string());
    sequence.video_tracks[0].add_clip(outro).expect("add outro");

    let mut dialogue = Clip::new(
        AssetId::new(),
        crate::app::tt(0, tb),
        crate::app::tt(176, tb),
    )
    .expect("valid clip");
    dialogue.label = Some("Dialogue".to_string());
    sequence.audio_tracks[0].add_clip(dialogue).expect("add dialogue");

    let mut music = Clip::new(
        AssetId::new(),
        crate::app::tt(24, tb),
        crate::app::tt(210, tb),
    )
    .expect("valid clip");
    music.label = Some("Music Bed".to_string());
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
        .with_timeline_display(model.timeline_display)
        .with_playhead(model.playhead_frame)
        .with_in_out_points(model.in_point_frame, model.out_point_frame)
        .on_clip_select({
            let action_model = action_model.clone();
            move |clip_ref, _clip, mode| {
                action_model.clip_identity(clip_ref, mode).map(timeline_select_clip_action)
            }
        })
        .on_transition_select({
            let action_model = action_model.clone();
            move |transition_ref, _transition| {
                action_model
                    .transition_identity(transition_ref)
                    .map(timeline_select_video_transition_action)
            }
        })
        .on_transition_resize({
            let action_model = action_model.clone();
            move |resize, _transition| {
                action_model
                    .transition_resize_payload(resize)
                    .map(timeline_set_video_transition_range_action)
            }
        })
        .on_cut_transition_create({
            let action_model = action_model.clone();
            move |cut_ref| {
                action_model
                    .cut_transition_payload(cut_ref)
                    .map(timeline_create_cross_dissolve_action)
            }
        })
        .on_track_select({
            let action_model = action_model.clone();
            move |track_ref, _track| {
                action_model.track_identity(track_ref).map(|track| {
                    Action::Select(mondrian_editor_state::action::SelectionTarget::Track(
                        track.track_id,
                    ))
                })
            }
        })
        .on_track_move({
            let action_model = action_model.clone();
            move |movement, _track| {
                action_model.track_move_payload(movement).map(timeline_move_track_action)
            }
        })
        .on_track_control({
            let action_model = action_model.clone();
            move |control, track_ref, track| {
                if matches!(
                    control,
                    TimelineTrackControl::Target | TimelineTrackControl::SyncLock
                ) {
                    action_model
                        .track_targeting_payload(control, track_ref, track)
                        .map(timeline_set_track_targeting_action)
                } else {
                    action_model
                        .track_control_payload(control, track_ref, track)
                        .map(timeline_set_track_control_action)
                }
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
                action_model.asset_drop_payload(drop).map(timeline_drop_asset_action)
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
                action_model.move_payload(movement).map(timeline_move_clip_action)
            }
        })
        .on_clip_trim({
            let action_model = action_model.clone();
            move |trim, _clip| action_model.trim_payload(trim).map(timeline_trim_clips_action)
        })
        .on_in_out_point(|point, frame| {
            timeline_set_in_out_point_action(TimelineSetInOutPointPayload {
                point: timeline_in_out_point_payload_kind(point),
                frame: frame.max(0),
            })
        })
        .on_seek(timeline_seek_action_from_widget)
        .with_waveform_display(model.waveform_display);
    let timeline = if let Some(source) = model.waveform_source.clone() {
        timeline.with_waveform_lookup(
            move |asset_id, selection, start_secs, end_secs, pixel_width| {
                source.lookup(asset_id, selection, start_secs, end_secs, pixel_width)
            },
        )
    } else {
        timeline
    };
    let timeline = if let Some(message) = model.empty_message.clone() {
        timeline.with_empty_message(message)
    } else {
        timeline
    };
    with_timeline_toolbar_icons(timeline)
}

fn timeline_seek_action_from_widget(seek: TimelineSeek) -> Action {
    timeline_seek_with_source_action(seek.frame, timeline_seek_source_from_widget(seek.source))
}

fn timeline_seek_source_from_widget(source: WidgetTimelineSeekSource) -> AppTimelineSeekSource {
    match source {
        WidgetTimelineSeekSource::PointerDrag => AppTimelineSeekSource::PointerDrag,
        WidgetTimelineSeekSource::Settled => AppTimelineSeekSource::Settled,
    }
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
) -> Option<Action> {
    match command {
        TimelineEditCommand::CutSelection => Some(Action::Cut),
        TimelineEditCommand::CopySelection => Some(Action::Copy),
        TimelineEditCommand::PasteAtPlayhead => Some(Action::Paste),
        TimelineEditCommand::DuplicateSelection => Some(Action::Duplicate),
        TimelineEditCommand::DeleteSelection => Some(Action::DeleteSelection),
        TimelineEditCommand::RippleDeleteSelection => Some(Action::RippleDeleteSelection),
        TimelineEditCommand::SplitAtPlayhead => Some(Action::SplitClipAtPlayhead),
        TimelineEditCommand::TrimSelectionInToPlayhead => {
            Some(timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::In },
            ))
        }
        TimelineEditCommand::TrimSelectionOutToPlayhead => {
            Some(timeline_trim_selected_clips_to_playhead_action(
                TimelineTrimSelectedClipsToPlayheadPayload { edge: TimelineTrimPayloadEdge::Out },
            ))
        }
        TimelineEditCommand::RollSelectedCutToPlayhead => {
            Some(timeline_roll_selected_cut_to_playhead_action())
        }
        TimelineEditCommand::EnableSelection => Some(timeline_set_selected_clips_enabled_action(
            TimelineSetSelectedClipsEnabledPayload { enabled: true },
        )),
        TimelineEditCommand::DisableSelection => Some(timeline_set_selected_clips_enabled_action(
            TimelineSetSelectedClipsEnabledPayload { enabled: false },
        )),
        TimelineEditCommand::LinkSelection => Some(timeline_link_selected_clips_action()),
        TimelineEditCommand::UnlinkSelection => Some(timeline_unlink_selected_clips_action()),
        TimelineEditCommand::LiftInOutRange => Some(timeline_lift_range_action()),
        TimelineEditCommand::ExtractInOutRange => Some(timeline_extract_range_action()),
        TimelineEditCommand::OpenNestedSequence(clip_ref) => {
            model.open_nested_payload(clip_ref).map(timeline_open_nested_sequence_action)
        }
        TimelineEditCommand::MarkInAtPlayhead => Some(Action::MarkInAtPlayhead),
        TimelineEditCommand::MarkOutAtPlayhead => Some(Action::MarkOutAtPlayhead),
        TimelineEditCommand::ClearInOutPoints => Some(timeline_clear_in_out_points_action()),
        TimelineEditCommand::TogglePlayback => Some(Action::TogglePlay),
    }
}

fn timeline_edit_command_shortcut_label(command: TimelineEditCommand) -> Option<String> {
    let action = match command {
        TimelineEditCommand::OpenNestedSequence(_)
        | TimelineEditCommand::LinkSelection
        | TimelineEditCommand::UnlinkSelection
        | TimelineEditCommand::LiftInOutRange
        | TimelineEditCommand::ExtractInOutRange => return None,
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

fn export_preset_update_action(preset: ExportPreset) -> Action {
    export_set_draft_action(ExportDraftUpdatePayload::Preset(preset))
}

fn export_container_label(container: &Container) -> &'static str {
    match container {
        Container::Mp4 => "MP4",
        Container::Mov => "MOV",
        Container::Mkv => "Matroska (MKV)",
        Container::Gif => "GIF",
        Container::Mxf => "MXF",
        Container::Webm => "WebM",
    }
}

fn export_container_items(preset: &ExportPreset) -> Vec<MenuItem> {
    [
        Container::Mp4,
        Container::Mov,
        Container::Mkv,
        Container::Mxf,
        Container::Webm,
        Container::Gif,
    ]
    .into_iter()
    .map(|container| {
        let label = export_container_label(&container);
        let mut updated = preset.clone();
        updated.container = container;
        MenuItem::new(label, export_preset_update_action(updated))
    })
    .collect()
}

fn export_video_codec_label(video: &VideoCodecConfig) -> &'static str {
    match video {
        VideoCodecConfig::H264 { profile: H264Profile::High, .. } => "H.264 High",
        VideoCodecConfig::Hevc { profile: HevcProfile::Main, .. } => "HEVC Main",
        VideoCodecConfig::Hevc { profile: HevcProfile::Main10, .. } => "HEVC Main 10",
        VideoCodecConfig::Av1 { profile: Av1Profile::Main, .. } => "AV1 Main",
        VideoCodecConfig::ProRes { profile: ProResProfile::Proxy } => "ProRes 422 Proxy",
        VideoCodecConfig::ProRes { profile: ProResProfile::Lt } => "ProRes 422 LT",
        VideoCodecConfig::ProRes { profile: ProResProfile::Standard } => "ProRes 422",
        VideoCodecConfig::ProRes { profile: ProResProfile::Hq } => "ProRes 422 HQ",
        VideoCodecConfig::ProRes { profile: ProResProfile::FourFourFourFour } => "ProRes 4444",
        VideoCodecConfig::ProRes { profile: ProResProfile::FourFourFourFourXq } => "ProRes 4444 XQ",
        VideoCodecConfig::Gif { .. } => "GIF palette",
    }
}

fn export_video_rate_control(video: &VideoCodecConfig) -> Option<(VideoRateControl, u8)> {
    match video {
        VideoCodecConfig::H264 { rate_control, .. }
        | VideoCodecConfig::Hevc { rate_control, .. } => Some((*rate_control, 51)),
        VideoCodecConfig::Av1 { rate_control, .. } => Some((*rate_control, 63)),
        VideoCodecConfig::ProRes { .. } | VideoCodecConfig::Gif { .. } => None,
    }
}

fn export_video_codec_items(preset: &ExportPreset) -> Vec<MenuItem> {
    let rate_control = export_video_rate_control(&preset.video)
        .map(|(rate_control, _)| rate_control)
        .unwrap_or_else(|| VideoRateControl::constant_quality(20));
    let (gif_colors, gif_dither) = match preset.video {
        VideoCodecConfig::Gif { colors, dither } => (colors, dither),
        _ => (256, true),
    };
    let choices = vec![
        VideoCodecConfig::H264 { profile: H264Profile::High, rate_control },
        VideoCodecConfig::Hevc { profile: HevcProfile::Main, rate_control },
        VideoCodecConfig::Hevc { profile: HevcProfile::Main10, rate_control },
        VideoCodecConfig::Av1 { profile: Av1Profile::Main, rate_control },
        VideoCodecConfig::ProRes { profile: ProResProfile::Proxy },
        VideoCodecConfig::ProRes { profile: ProResProfile::Lt },
        VideoCodecConfig::ProRes { profile: ProResProfile::Standard },
        VideoCodecConfig::ProRes { profile: ProResProfile::Hq },
        VideoCodecConfig::ProRes { profile: ProResProfile::FourFourFourFour },
        VideoCodecConfig::ProRes { profile: ProResProfile::FourFourFourFourXq },
        VideoCodecConfig::Gif { colors: gif_colors, dither: gif_dither },
    ];
    choices
        .into_iter()
        .map(|video| {
            let label = export_video_codec_label(&video);
            let mut updated = preset.clone();
            updated.video = video;
            MenuItem::new(label, export_preset_update_action(updated))
        })
        .collect()
}

fn export_audio_codec_label(audio: &AudioCodecConfig) -> &'static str {
    match audio {
        AudioCodecConfig::Disabled => "无音频",
        AudioCodecConfig::Aac { .. } => "AAC",
        AudioCodecConfig::Pcm { .. } => "PCM",
        AudioCodecConfig::Mp3 { .. } => "MP3",
    }
}

fn export_audio_codec_items(preset: &ExportPreset) -> Vec<MenuItem> {
    let aac_bitrate = match preset.audio {
        AudioCodecConfig::Aac { bitrate_kbps } => bitrate_kbps,
        _ => 192,
    };
    let pcm_bit_depth = match preset.audio {
        AudioCodecConfig::Pcm { bit_depth } => bit_depth,
        _ => 24,
    };
    let mp3_bitrate = match preset.audio {
        AudioCodecConfig::Mp3 { bitrate_kbps } => bitrate_kbps,
        _ => 192,
    };
    [
        AudioCodecConfig::Disabled,
        AudioCodecConfig::Aac { bitrate_kbps: aac_bitrate },
        AudioCodecConfig::Pcm { bit_depth: pcm_bit_depth },
        AudioCodecConfig::Mp3 { bitrate_kbps: mp3_bitrate },
    ]
    .into_iter()
    .map(|audio| {
        let label = export_audio_codec_label(&audio);
        let mut updated = preset.clone();
        updated.audio = audio;
        MenuItem::new(label, export_preset_update_action(updated))
    })
    .collect()
}

fn export_resolution_label(resolution: Option<ExportResolution>) -> String {
    resolution
        .map(|resolution| format!("{} × {}", resolution.width, resolution.height))
        .unwrap_or_else(|| "跟随序列".to_owned())
}

fn export_resolution_items(preset: &ExportPreset) -> Vec<MenuItem> {
    [
        ("跟随序列", None),
        (
            "1280 × 720",
            Some(ExportResolution { width: 1280, height: 720 }),
        ),
        (
            "1920 × 1080",
            Some(ExportResolution { width: 1920, height: 1080 }),
        ),
        (
            "UHD 3840 × 2160",
            Some(ExportResolution { width: 3840, height: 2160 }),
        ),
        (
            "DCI 4096 × 2160",
            Some(ExportResolution { width: 4096, height: 2160 }),
        ),
    ]
    .into_iter()
    .map(|(label, resolution)| {
        let mut updated = preset.clone();
        updated.resolution = resolution;
        MenuItem::new(label, export_preset_update_action(updated))
    })
    .collect()
}

fn export_bit_depth_label(bit_depth: ExportParameter<DeliveryBitDepth>) -> &'static str {
    match bit_depth {
        ExportParameter::FollowSequence => "跟随序列",
        ExportParameter::Explicit(DeliveryBitDepth::Eight) => "8-bit",
        ExportParameter::Explicit(DeliveryBitDepth::Ten) => "10-bit",
        ExportParameter::Explicit(DeliveryBitDepth::Twelve) => "12-bit",
    }
}

fn export_bit_depth_items(preset: &ExportPreset) -> Vec<MenuItem> {
    [
        ExportParameter::FollowSequence,
        ExportParameter::Explicit(DeliveryBitDepth::Eight),
        ExportParameter::Explicit(DeliveryBitDepth::Ten),
        ExportParameter::Explicit(DeliveryBitDepth::Twelve),
    ]
    .into_iter()
    .map(|bit_depth| {
        let mut updated = preset.clone();
        updated.video_signal.bit_depth = bit_depth;
        MenuItem::new(
            export_bit_depth_label(bit_depth),
            export_preset_update_action(updated),
        )
    })
    .collect()
}

fn export_video_range_label(range: ExportParameter<VideoRange>) -> &'static str {
    match range {
        ExportParameter::FollowSequence => "跟随序列",
        ExportParameter::Explicit(VideoRange::Full) => "Full",
        ExportParameter::Explicit(VideoRange::Legal) => "Legal / Video",
    }
}

fn export_video_range_items(preset: &ExportPreset) -> Vec<MenuItem> {
    [
        ExportParameter::FollowSequence,
        ExportParameter::Explicit(VideoRange::Full),
        ExportParameter::Explicit(VideoRange::Legal),
    ]
    .into_iter()
    .map(|range| {
        let mut updated = preset.clone();
        updated.video_signal.range = range;
        MenuItem::new(
            export_video_range_label(range),
            export_preset_update_action(updated),
        )
    })
    .collect()
}

fn export_chroma_label(chroma: ExportChromaSampling) -> &'static str {
    match chroma {
        ExportChromaSampling::Yuv420 => "YUV 4:2:0",
        ExportChromaSampling::Yuv422 => "YUV 4:2:2",
        ExportChromaSampling::Yuv444 => "YUV 4:4:4",
        ExportChromaSampling::Rgb => "RGB",
    }
}

fn export_chroma_items(preset: &ExportPreset) -> Vec<MenuItem> {
    [
        ExportChromaSampling::Yuv420,
        ExportChromaSampling::Yuv422,
        ExportChromaSampling::Yuv444,
        ExportChromaSampling::Rgb,
    ]
    .into_iter()
    .map(|chroma| {
        let mut updated = preset.clone();
        updated.video_signal.chroma_sampling = chroma;
        MenuItem::new(
            export_chroma_label(chroma),
            export_preset_update_action(updated),
        )
    })
    .collect()
}

fn export_alpha_mode_label(alpha_mode: ExportAlphaMode) -> &'static str {
    match alpha_mode {
        ExportAlphaMode::FlattenBlack => "合成到黑色",
        ExportAlphaMode::Preserve => "保留 Straight Alpha",
    }
}

fn export_alpha_mode_items(preset: &ExportPreset) -> Vec<MenuItem> {
    [ExportAlphaMode::FlattenBlack, ExportAlphaMode::Preserve]
        .into_iter()
        .map(|alpha_mode| {
            let mut updated = preset.clone();
            updated.alpha_mode = alpha_mode;
            MenuItem::new(
                export_alpha_mode_label(alpha_mode),
                export_preset_update_action(updated),
            )
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportColorTargetMode {
    FollowSequence,
    RenderingView,
    Colorimetric,
}

fn export_color_target_mode(target: ExportColorTarget) -> ExportColorTargetMode {
    match target {
        ExportColorTarget::FollowSequence => ExportColorTargetMode::FollowSequence,
        ExportColorTarget::RenderingView(_) => ExportColorTargetMode::RenderingView,
        ExportColorTarget::Colorimetric(_) => ExportColorTargetMode::Colorimetric,
    }
}

fn export_color_target_mode_label(mode: ExportColorTargetMode) -> &'static str {
    match mode {
        ExportColorTargetMode::FollowSequence => "跟随序列 Program Output",
        ExportColorTargetMode::RenderingView => "项目引擎 Rendering View",
        ExportColorTargetMode::Colorimetric => "直接 Colorimetric 转换",
    }
}

fn export_color_target_label(target: ExportColorTarget) -> String {
    match target {
        ExportColorTarget::FollowSequence => {
            export_color_target_mode_label(ExportColorTargetMode::FollowSequence).to_owned()
        }
        ExportColorTarget::RenderingView(color_space) => format!(
            "{} / {}",
            export_color_target_mode_label(ExportColorTargetMode::RenderingView),
            color_space_label(color_space)
        ),
        ExportColorTarget::Colorimetric(color_space) => format!(
            "{} / {}",
            export_color_target_mode_label(ExportColorTargetMode::Colorimetric),
            color_space_label(color_space)
        ),
    }
}

fn export_color_target_with_mode(
    target: ExportColorTarget,
    mode: ExportColorTargetMode,
) -> ExportColorTarget {
    match mode {
        ExportColorTargetMode::FollowSequence => ExportColorTarget::FollowSequence,
        ExportColorTargetMode::RenderingView => {
            let color_space = match target {
                ExportColorTarget::RenderingView(color_space)
                | ExportColorTarget::Colorimetric(color_space)
                    if color_space.is_display_referred() =>
                {
                    color_space
                }
                ExportColorTarget::FollowSequence
                | ExportColorTarget::RenderingView(_)
                | ExportColorTarget::Colorimetric(_) => ColorSpace::Rec709,
            };
            ExportColorTarget::RenderingView(color_space)
        }
        ExportColorTargetMode::Colorimetric => {
            let color_space = match target {
                ExportColorTarget::RenderingView(color_space)
                | ExportColorTarget::Colorimetric(color_space)
                    if is_explicit_export_color_space(color_space) =>
                {
                    color_space
                }
                ExportColorTarget::FollowSequence
                | ExportColorTarget::RenderingView(_)
                | ExportColorTarget::Colorimetric(_) => ColorSpace::Rec709,
            };
            ExportColorTarget::Colorimetric(color_space)
        }
    }
}

fn is_explicit_export_color_space(color_space: ColorSpace) -> bool {
    color_space.is_display_referred() || color_space.encoding().is_scene_log()
}

fn export_color_target_spaces(mode: ExportColorTargetMode) -> Vec<ColorSpace> {
    ColorSpace::ALL
        .into_iter()
        .filter(|color_space| match mode {
            ExportColorTargetMode::FollowSequence => false,
            ExportColorTargetMode::RenderingView => color_space.is_display_referred(),
            ExportColorTargetMode::Colorimetric => is_explicit_export_color_space(*color_space),
        })
        .collect()
}

fn export_color_target_mode_items(preset: &ExportPreset) -> Vec<MenuItem> {
    [
        ExportColorTargetMode::FollowSequence,
        ExportColorTargetMode::RenderingView,
        ExportColorTargetMode::Colorimetric,
    ]
    .into_iter()
    .map(|mode| {
        let mut updated = preset.clone();
        updated.color_target = export_color_target_with_mode(updated.color_target, mode);
        MenuItem::new(
            export_color_target_mode_label(mode),
            export_preset_update_action(updated),
        )
    })
    .collect()
}

fn export_color_target_space_items(preset: &ExportPreset) -> Vec<MenuItem> {
    let mode = export_color_target_mode(preset.color_target);
    export_color_target_spaces(mode)
        .into_iter()
        .map(|color_space| {
            let mut updated = preset.clone();
            updated.color_target = match mode {
                ExportColorTargetMode::FollowSequence => ExportColorTarget::FollowSequence,
                ExportColorTargetMode::RenderingView => {
                    ExportColorTarget::RenderingView(color_space)
                }
                ExportColorTargetMode::Colorimetric => ExportColorTarget::Colorimetric(color_space),
            };
            MenuItem::new(
                color_space_label(color_space),
                export_preset_update_action(updated),
            )
        })
        .collect()
}

fn export_with_rate_control(
    mut preset: ExportPreset,
    rate_control: VideoRateControl,
) -> ExportPreset {
    match &mut preset.video {
        VideoCodecConfig::H264 { rate_control: current, .. }
        | VideoCodecConfig::Hevc { rate_control: current, .. }
        | VideoCodecConfig::Av1 { rate_control: current, .. } => *current = rate_control,
        VideoCodecConfig::ProRes { .. } | VideoCodecConfig::Gif { .. } => {}
    }
    preset
}

fn export_panel(model: &ExportPanelModel) -> PropertyPanel {
    let mut preset_label = model
        .presets
        .get(model.selected_preset_idx)
        .or_else(|| model.presets.first())
        .map(|option| option.label.clone())
        .unwrap_or_else(|| "No presets".to_owned());
    if model.preset_customized {
        preset_label.push_str("（已修改）");
    }
    let preset_items = model
        .presets
        .iter()
        .map(|option| {
            MenuItem::new(
                option.label.clone(),
                export_set_draft_action(ExportDraftUpdatePayload::BuiltinPreset(option.id)),
            )
        })
        .collect::<Vec<_>>();
    let preset_dropdown = Dropdown::new(preset_label, preset_items).with_max_visible_items(6);
    let container_dropdown = Dropdown::new(
        export_container_label(&model.preset.container),
        export_container_items(&model.preset),
    )
    .with_max_visible_items(6);
    let video_codec_dropdown = Dropdown::new(
        export_video_codec_label(&model.preset.video),
        export_video_codec_items(&model.preset),
    )
    .with_max_visible_items(8);
    let resolution_dropdown = Dropdown::new(
        export_resolution_label(model.preset.resolution),
        export_resolution_items(&model.preset),
    )
    .with_max_visible_items(5);
    let bit_depth_dropdown = Dropdown::new(
        export_bit_depth_label(model.preset.video_signal.bit_depth),
        export_bit_depth_items(&model.preset),
    )
    .with_max_visible_items(4);
    let color_target_mode = export_color_target_mode(model.preset.color_target);
    let color_target_mode_dropdown = Dropdown::new(
        export_color_target_mode_label(color_target_mode),
        export_color_target_mode_items(&model.preset),
    )
    .with_max_visible_items(3);
    let color_target_space_label = match model.preset.color_target {
        ExportColorTarget::FollowSequence => "由序列 Program Output 决定".to_owned(),
        ExportColorTarget::RenderingView(color_space)
        | ExportColorTarget::Colorimetric(color_space) => color_space_label(color_space).to_owned(),
    };
    let color_target_space_dropdown = Dropdown::new(
        color_target_space_label,
        export_color_target_space_items(&model.preset),
    )
    .with_max_visible_items(8)
    .enabled(color_target_mode != ExportColorTargetMode::FollowSequence);
    let video_range_dropdown = Dropdown::new(
        export_video_range_label(model.preset.video_signal.range),
        export_video_range_items(&model.preset),
    )
    .with_max_visible_items(3);
    let chroma_dropdown = Dropdown::new(
        export_chroma_label(model.preset.video_signal.chroma_sampling),
        export_chroma_items(&model.preset),
    )
    .with_max_visible_items(4);
    let alpha_dropdown = Dropdown::new(
        export_alpha_mode_label(model.preset.alpha_mode),
        export_alpha_mode_items(&model.preset),
    )
    .with_max_visible_items(2);
    let audio_codec_dropdown = Dropdown::new(
        export_audio_codec_label(&model.preset.audio),
        export_audio_codec_items(&model.preset),
    )
    .with_max_visible_items(4);

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
    let enqueue_action = model.enqueue_payload().map(export_enqueue_action);
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

    let mut format_section = PropertySection::new("格式")
        .with_row(PropertyRow::new("容器", Box::new(container_dropdown)))
        .with_row(PropertyRow::new("视频编码", Box::new(video_codec_dropdown)))
        .with_row(PropertyRow::new("画幅", Box::new(resolution_dropdown)));
    if let Some(resolution) = model.preset.resolution {
        let width_preset = model.preset.clone();
        let width_input = NumberInput::new(
            resolution.width as f64,
            mondrian_timeline::SequenceSettings::MIN_WIDTH as f64,
            mondrian_timeline::SequenceSettings::MAX_WIDTH as f64,
        )
        .with_step(1.0)
        .on_change(move |width| {
            let mut updated = width_preset.clone();
            if let Some(resolution) = &mut updated.resolution {
                resolution.width = width.round() as u32;
            }
            export_preset_update_action(updated)
        });
        let height_preset = model.preset.clone();
        let height_input = NumberInput::new(
            resolution.height as f64,
            mondrian_timeline::SequenceSettings::MIN_HEIGHT as f64,
            mondrian_timeline::SequenceSettings::MAX_HEIGHT as f64,
        )
        .with_step(1.0)
        .on_change(move |height| {
            let mut updated = height_preset.clone();
            if let Some(resolution) = &mut updated.resolution {
                resolution.height = height.round() as u32;
            }
            export_preset_update_action(updated)
        });
        format_section = format_section
            .with_row(PropertyRow::new("宽度", Box::new(width_input)))
            .with_row(PropertyRow::new("高度", Box::new(height_input)));
    }

    let signal_section = PropertySection::new("视频信号")
        .with_row(PropertyRow::new("位深", Box::new(bit_depth_dropdown)))
        .with_row(PropertyRow::new("范围", Box::new(video_range_dropdown)))
        .with_row(PropertyRow::new("色度采样", Box::new(chroma_dropdown)))
        .with_row(PropertyRow::new("Alpha", Box::new(alpha_dropdown)));
    let color_section = PropertySection::new("色彩输出")
        .with_row(PropertyRow::new(
            "处理方式",
            Box::new(color_target_mode_dropdown),
        ))
        .with_row(PropertyRow::new(
            "目标空间",
            Box::new(color_target_space_dropdown),
        ));

    let mut encoding_section = PropertySection::new("编码参数");
    if let Some((rate_control, max_crf)) = export_video_rate_control(&model.preset.video) {
        let crf_preset = model.preset.clone();
        let crf_input = NumberInput::new(rate_control.crf as f64, 0.0, max_crf as f64)
            .with_step(1.0)
            .on_change(move |crf| {
                let mut updated = rate_control;
                updated.crf = crf.round() as u8;
                export_preset_update_action(export_with_rate_control(crf_preset.clone(), updated))
            });
        let max_bitrate_preset = model.preset.clone();
        let max_bitrate_input = NumberInput::new(
            rate_control.max_bitrate_kbps.unwrap_or_default() as f64,
            0.0,
            2_000_000.0,
        )
        .with_step(100.0)
        .on_change(move |max_bitrate| {
            let mut updated = rate_control;
            let value = max_bitrate.round() as u32;
            updated.max_bitrate_kbps = (value > 0).then_some(value);
            export_preset_update_action(export_with_rate_control(
                max_bitrate_preset.clone(),
                updated,
            ))
        });
        let buffer_preset = model.preset.clone();
        let buffer_input = NumberInput::new(
            rate_control.buffer_size_kbits.unwrap_or_default() as f64,
            0.0,
            4_000_000.0,
        )
        .with_step(100.0)
        .on_change(move |buffer_size| {
            let mut updated = rate_control;
            let value = buffer_size.round() as u32;
            updated.buffer_size_kbits = (value > 0).then_some(value);
            export_preset_update_action(export_with_rate_control(buffer_preset.clone(), updated))
        });
        encoding_section = encoding_section
            .with_row(PropertyRow::new("CRF", Box::new(crf_input)))
            .with_row(PropertyRow::new(
                "最大码率 kbps",
                Box::new(max_bitrate_input),
            ))
            .with_row(PropertyRow::new("VBV buffer kbit", Box::new(buffer_input)));
    } else if let VideoCodecConfig::Gif { colors, dither } = model.preset.video {
        let colors_preset = model.preset.clone();
        let colors_input =
            NumberInput::new(colors as f64, 2.0, 256.0)
                .with_step(1.0)
                .on_change(move |colors| {
                    let mut updated = colors_preset.clone();
                    if let VideoCodecConfig::Gif { colors: current, .. } = &mut updated.video {
                        *current = colors.round() as u16;
                    }
                    export_preset_update_action(updated)
                });
        let dither_preset = model.preset.clone();
        let dither_checkbox = Checkbox::new("允许调色板抖动", dither).on_change(move |enabled| {
            let mut updated = dither_preset.clone();
            if let VideoCodecConfig::Gif { dither, .. } = &mut updated.video {
                *dither = enabled;
            }
            export_preset_update_action(updated)
        });
        encoding_section = encoding_section
            .with_row(PropertyRow::new("调色板颜色", Box::new(colors_input)))
            .with_row(PropertyRow::new("抖动", Box::new(dither_checkbox)));
    } else {
        encoding_section = encoding_section.with_row(PropertyRow::new(
            "控制",
            Box::new(Label::new("由已验证的固定 Profile 决定").muted()),
        ));
    }

    let mut audio_section = PropertySection::new("音频")
        .with_row(PropertyRow::new("编码", Box::new(audio_codec_dropdown)));
    match model.preset.audio {
        AudioCodecConfig::Disabled => {}
        AudioCodecConfig::Aac { bitrate_kbps } | AudioCodecConfig::Mp3 { bitrate_kbps } => {
            let bitrate_preset = model.preset.clone();
            let bitrate_input = NumberInput::new(bitrate_kbps as f64, 1.0, 1_536.0)
                .with_step(8.0)
                .on_change(move |bitrate| {
                    let mut updated = bitrate_preset.clone();
                    match &mut updated.audio {
                        AudioCodecConfig::Aac { bitrate_kbps }
                        | AudioCodecConfig::Mp3 { bitrate_kbps } => {
                            *bitrate_kbps = bitrate.round() as u32;
                        }
                        AudioCodecConfig::Disabled | AudioCodecConfig::Pcm { .. } => {}
                    }
                    export_preset_update_action(updated)
                });
            audio_section =
                audio_section.with_row(PropertyRow::new("码率 kbps", Box::new(bitrate_input)));
        }
        AudioCodecConfig::Pcm { bit_depth } => {
            let items = [16u8, 24, 32]
                .into_iter()
                .map(|candidate| {
                    let mut updated = model.preset.clone();
                    updated.audio = AudioCodecConfig::Pcm { bit_depth: candidate };
                    MenuItem::new(
                        format!("{candidate}-bit integer"),
                        export_preset_update_action(updated),
                    )
                })
                .collect();
            audio_section = audio_section.with_row(PropertyRow::new(
                "采样位深",
                Box::new(
                    Dropdown::new(format!("{bit_depth}-bit integer"), items)
                        .with_max_visible_items(3),
                ),
            ));
        }
    }

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
                    .with_height(72.0),
                ),
        )
        .with_section(format_section)
        .with_section(color_section)
        .with_section(signal_section)
        .with_section(encoding_section)
        .with_section(audio_section)
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
    let mut summary_children = vec![
        FlexChild::fixed(Box::new(
            Label::new(job.title.clone()).with_padding(0.0, 0.0),
        )),
        FlexChild::fixed(Box::new(
            Label::new(format!("{} / {}%", job.status, job.progress_percent))
                .muted()
                .wrapped()
                .with_padding(0.0, 0.0),
        )),
    ];
    if let Some(color_diagnostics) = &job.color_diagnostics {
        summary_children.push(FlexChild::fixed(Box::new(
            Label::new(color_diagnostics.clone()).muted().wrapped().with_padding(0.0, 0.0),
        )));
    }
    let summary = FlexContainer::column(summary_children).with_gap(4.0);

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

    let base_height = if job.can_cancel { 48.0 } else { 42.0 };
    let height = if job.color_diagnostics.is_some() {
        base_height + 18.0
    } else {
        base_height
    };
    PropertyRow::new("任务", content).with_height(height)
}

fn export_job_color_diagnostics_label(diagnostics: ExportJobColorDiagnostics) -> Option<String> {
    let report = diagnostics.health_report("export-panel")?;
    let summary = report.summary;
    let asset_issue_tags = color_issue_aggregate_tags(&summary.asset_issue_summary);
    let asset_issue_segment = if asset_issue_tags.is_empty() {
        format!(
            "assets {} / issues none",
            summary.asset_issue_summary.diagnostics
        )
    } else {
        format!(
            "assets {} / issues {}",
            summary.asset_issue_summary.diagnostics,
            asset_issue_tags.join(" ")
        )
    };
    let root_causes = if report.root_causes.is_empty() {
        "none".to_owned()
    } else {
        report
            .root_causes
            .iter()
            .map(|root| {
                if root.severity == ExportColorHealthSeverity::Warn {
                    format!("warn:{}", root.code)
                } else {
                    root.code.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let actions = if report.actions.is_empty() {
        "none".to_owned()
    } else {
        report.actions.iter().map(|action| action.code).collect::<Vec<_>>().join(" ")
    };
    Some(format!(
        "色彩: report {:?} / {} 帧 / metadata {} / override {} / policy {} / data {} / reject {} / {} / checks {} / causes {} / actions {} / stages cpu-in {} cpu-out {} gpu {} blockers {} transfer {} / gpu blockers shader {} resource {} wrapper {} pipeline {} / composite float {} legacy {} reasons {}",
        report.verdict,
        summary.diagnosed_frames,
        summary.detected_metadata,
        summary.override_count,
        summary.policy_assumptions,
        summary.data_textures,
        summary.policy_rejections,
        asset_issue_segment,
        report.checks.len(),
        root_causes,
        actions,
        summary.cpu_input_stages,
        summary.cpu_output_stages,
        summary.gpu_color_stages,
        summary.gpu_blockers,
        summary.transfer_stages,
        summary.gpu_blocker_breakdown.shader_module_not_prepared,
        summary.gpu_blocker_breakdown.ocio_resource_bind_group_not_prepared,
        summary.gpu_blocker_breakdown.fullscreen_wrapper_not_prepared,
        summary.gpu_blocker_breakdown.render_pipeline_not_prepared,
        summary.float_linear_composites,
        summary.legacy_rgba8_composites,
        summary.legacy_reason_total
    ))
}

fn color_issue_summary_tags(summary: &VideoColorDiagnosticIssueSummary) -> Vec<String> {
    let mut tags = Vec::new();
    push_issue_metric(&mut tags, "missing-cicp", summary.missing_cicp_tags);
    push_issue_metric(&mut tags, "unsupported-cicp", summary.unsupported_cicp_tags);
    push_issue_metric(&mut tags, "decoder", summary.decoder_unavailable);
    push_issue_metric(&mut tags, "partial-cicp", summary.partial_cicp_tags);
    push_issue_metric(
        &mut tags,
        "hint-conflict",
        summary.metadata_hint_overrides_cicp_tags,
    );
    push_issue_metric(&mut tags, "multi-hint", summary.multiple_metadata_hints);
    push_issue_metric(&mut tags, "ignored-hints", summary.ignored_metadata_hints);
    push_issue_metric(&mut tags, "metadata-hints", summary.metadata_hint_count);
    push_issue_metric(&mut tags, "hdr", summary.hdr_side_data_count);
    if summary.has_raw_cicp_metadata {
        tags.push("raw-cicp".to_owned());
    }
    if summary.has_icc_profile {
        tags.push("icc".to_owned());
    }
    tags
}

fn color_issue_aggregate_tags(summary: &VideoColorDiagnosticIssueAggregate) -> Vec<String> {
    let mut tags = Vec::new();
    push_issue_metric(&mut tags, "warn", summary.diagnostics_with_warnings);
    push_issue_metric(&mut tags, "missing-cicp", summary.missing_cicp_tags);
    push_issue_metric(&mut tags, "unsupported-cicp", summary.unsupported_cicp_tags);
    push_issue_metric(&mut tags, "decoder", summary.decoder_unavailable);
    push_issue_metric(
        &mut tags,
        "hint-conflict",
        summary.metadata_hint_overrides_cicp_tags,
    );
    push_issue_metric(&mut tags, "multi-hint", summary.multiple_metadata_hints);
    push_issue_metric(&mut tags, "partial-cicp", summary.partial_cicp_tags);
    push_issue_metric(&mut tags, "hdr", summary.hdr_side_data_count);
    tags
}

fn push_issue_metric(tags: &mut Vec<String>, label: &str, value: u64) {
    if value > 0 {
        tags.push(format!("{label} {value}"));
    }
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
    let bitrate = match &preset.video {
        VideoCodecConfig::H264 { rate_control, .. } => export_rate_control_label(*rate_control),
        VideoCodecConfig::Hevc { rate_control, .. } => export_rate_control_label(*rate_control),
        VideoCodecConfig::Av1 { rate_control, .. } => export_rate_control_label(*rate_control),
        VideoCodecConfig::ProRes { .. } => "固定 Profile 质量".to_owned(),
        VideoCodecConfig::Gif { colors, dither } => {
            format!(
                "{colors} 色 / dither {}",
                if *dither { "on" } else { "off" }
            )
        }
    };
    let audio = match preset.audio {
        AudioCodecConfig::Disabled => "无音频".to_owned(),
        AudioCodecConfig::Aac { bitrate_kbps } => format!("AAC {bitrate_kbps} kbps"),
        AudioCodecConfig::Pcm { bit_depth } => format!("PCM {bit_depth}-bit"),
        AudioCodecConfig::Mp3 { bitrate_kbps } => format!("MP3 {bitrate_kbps} kbps"),
    };
    format!(
        "{resolution} / {} / {bitrate} / {} / {} / {} / {} / {} / {audio} / .{}",
        export_video_codec_label(&preset.video),
        export_color_target_label(preset.color_target),
        export_bit_depth_label(preset.video_signal.bit_depth),
        export_video_range_label(preset.video_signal.range),
        export_chroma_label(preset.video_signal.chroma_sampling),
        export_alpha_mode_label(preset.alpha_mode),
        export_preset_extension(preset)
    )
}

fn export_rate_control_label(rate_control: mondrian_export::preset::VideoRateControl) -> String {
    match rate_control.max_bitrate_kbps {
        Some(max_bitrate) => format!("CRF {} / max {max_bitrate} kbps", rate_control.crf),
        None => format!("CRF {}", rate_control.crf),
    }
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

fn export_job_status_label(status: &JobStatus, progress: ExportProgress) -> String {
    match status {
        JobStatus::Pending => "Pending".to_owned(),
        JobStatus::Running { phase } => match progress.detail {
            ExportProgressDetail::Frames { completed, total }
                if *phase == ExportProgressPhase::Rendering =>
            {
                format!("Rendering {completed}/{total}")
            }
            _ => export_progress_phase_label(*phase).to_owned(),
        },
        JobStatus::Cancelling { .. } => "Cancelling".to_owned(),
        JobStatus::Completed => "Completed".to_owned(),
        JobStatus::Failed(failure) => format!("Failed: {}", failure.detail),
        JobStatus::Cancelled => "Cancelled".to_owned(),
    }
}

fn export_progress_phase_label(phase: ExportProgressPhase) -> &'static str {
    match phase {
        ExportProgressPhase::Preparing => "Preparing",
        ExportProgressPhase::Rendering => "Rendering",
        ExportProgressPhase::Encoding => "Encoding",
        ExportProgressPhase::Validating => "Validating",
        ExportProgressPhase::Publishing => "Publishing",
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

fn audio_mixer_panel(model: &AudioMixerPanelModel) -> PropertyPanel {
    if let Some(message) = model.empty_message.as_deref().filter(|message| !message.is_empty()) {
        let (title, description) = message
            .split_once('\n')
            .map_or((message, ""), |(title, description)| (title, description));
        let mut panel = PropertyPanel::with_options(
            "音频混音器",
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
        if let Ok(icon) = AppIcon::Music.vector_icon() {
            panel = panel.with_empty_state_icon(icon);
        }
        return panel;
    }

    let mut panel = PropertyPanel::with_options(
        "音频混音器",
        PropertyPanelOptions {
            label_width: 104.0,
            control_gap: 8.0,
            row_height: 30.0,
            section_gap: 8.0,
            ..PropertyPanelOptions::default()
        },
    )
    .with_embedded_panel_chrome();

    let create_bus = audio_mixer_create_bus_action(model);
    let create_bus_label = model
        .next_bus_name
        .as_deref()
        .map_or("新建 Bus".to_owned(), |name| format!("新建 {name}"));
    panel = panel.with_section(PropertySection::new("路由图").with_row(PropertyRow::new(
        "Bus",
        Box::new(Button::new(create_bus_label).enabled(create_bus.is_some()).on_click(create_bus)),
    )));

    for channel in &model.channels {
        let kind = match channel.kind {
            AudioMixerChannelKind::Track => "轨道",
            AudioMixerChannelKind::Bus => "Bus",
            AudioMixerChannelKind::ProgramOutput => "节目输出",
        };
        let mut section = PropertySection::new(format!("{kind} · {}", channel.name));
        if let (Some(muted), mondrian_timeline::AudioChannelStripOwner::Track { track_id }) =
            (channel.track_muted, channel.owner)
        {
            section = section.with_row(PropertyRow::new(
                "静音",
                Box::new(
                    Checkbox::new("M", muted)
                        .on_change(move |value| audio_mixer_set_track_mute_action(track_id, value)),
                ),
            ));
        }
        if let (Some(soloed), mondrian_timeline::AudioChannelStripOwner::Track { track_id }) =
            (channel.track_soloed, channel.owner)
        {
            section = section.with_row(PropertyRow::new(
                "独奏",
                Box::new(
                    Checkbox::new("S", soloed)
                        .on_change(move |value| audio_mixer_set_track_solo_action(track_id, value)),
                ),
            ));
        }
        section = section.with_row(PropertyRow::new(
            "电平",
            Box::new(Label::new(audio_mixer_meter_label(channel)).muted()),
        ));
        let trim_channel = channel.clone();
        section = section.with_row(PropertyRow::new(
            "输入增益",
            numeric_slider_input_control_with_hard_range(
                channel.input_trim_db as f32,
                -60.0,
                12.0,
                AUDIO_GAIN_DB_MIN as f32,
                AUDIO_GAIN_DB_MAX as f32,
                Some(0.1),
                1,
                channel.is_editable,
                move |value| audio_mixer_set_input_trim_action(&trim_channel, value),
            ),
        ));
        match channel.fader {
            AudioMixerGainModel::Static { value_db } => {
                let fader_channel = channel.clone();
                section = section.with_row(PropertyRow::new(
                    "推子",
                    numeric_slider_input_control_with_hard_range(
                        value_db as f32,
                        -60.0,
                        12.0,
                        AUDIO_GAIN_DB_MIN as f32,
                        AUDIO_GAIN_DB_MAX as f32,
                        Some(0.1),
                        1,
                        channel.is_editable,
                        move |value| audio_mixer_set_fader_action(&fader_channel, value),
                    ),
                ));
            }
            AudioMixerGainModel::Automated { keyframe_count } => {
                section = section.with_row(PropertyRow::new(
                    "推子",
                    Box::new(Label::new(format!("自动化 · {keyframe_count} 个关键帧")).muted()),
                ));
            }
        }
        if let Some(automation) = &channel.fader_automation {
            section = section.with_row(
                PropertyRow::new("推子曲线", audio_automation_curve_control(automation))
                    .with_height(118.0),
            );
        }
        if let Some(reason) = &channel.edit_disabled_reason {
            section = section.with_row(PropertyRow::new(
                "只读",
                Box::new(Label::new(reason.clone()).muted()),
            ));
        }
        if channel.incoming_route_count > 0
            || matches!(
                channel.kind,
                AudioMixerChannelKind::Bus | AudioMixerChannelKind::ProgramOutput
            )
        {
            section = section.with_row(PropertyRow::new(
                "输入路由",
                Box::new(Label::new(format!("{} 条", channel.incoming_route_count)).muted()),
            ));
        }
        if matches!(channel.kind, AudioMixerChannelKind::Bus) {
            let rename_channel = channel.clone();
            section = section.with_row(PropertyRow::new(
                "名称",
                Box::new(
                    TextInput::new("Bus 名称")
                        .with_text(&channel.name)
                        .enabled(channel.is_editable)
                        .on_commit(move |name| {
                            audio_mixer_rename_bus_action(&rename_channel, name)
                        }),
                ),
            ));
        }
        if !channel.route_create_options.is_empty() {
            let items = channel
                .route_create_options
                .iter()
                .map(|option| {
                    MenuItem::new(
                        option.label.clone(),
                        audio_mixer_create_route_action(option),
                    )
                })
                .collect();
            section = section.with_row(PropertyRow::new(
                "添加路由",
                Box::new(Dropdown::new("选择 tap 与目标…", items).with_max_visible_items(12)),
            ));
        }
        if let Some(removal) = &channel.bus_removal {
            let action = audio_mixer_remove_bus_action(removal);
            let label = if removal.connected_route_count == 0 {
                "删除 Bus".to_owned()
            } else {
                format!("删除 Bus 与 {} 条路由", removal.connected_route_count)
            };
            section = section.with_row(PropertyRow::new(
                "Bus",
                Box::new(Button::new(label).enabled(action.is_some()).on_click(action)),
            ));
            if let Some(reason) = &removal.edit_disabled_reason {
                section = section.with_row(PropertyRow::new(
                    "删除受阻",
                    Box::new(Label::new(reason.clone()).muted()),
                ));
            }
        }
        panel = panel.with_section(section);
        for route in &channel.outbound_routes {
            let enabled_route = route.clone();
            let remove_action = audio_mixer_remove_route_action(route);
            let mut route_section =
                PropertySection::new(format!("Route · {}", route.destination_label))
                    .with_row(PropertyRow::new(
                        "Tap",
                        Box::new(Label::new(route.source_port_label).muted()),
                    ))
                    .with_row(PropertyRow::new(
                        "启用",
                        Box::new(
                            Checkbox::new("传递信号", route.enabled)
                                .enabled(route.is_editable)
                                .on_change(move |enabled| {
                                    audio_mixer_set_route_enabled_action(&enabled_route, enabled)
                                }),
                        ),
                    ));
            match route.gain {
                AudioMixerGainModel::Static { value_db } => {
                    let gain_route = route.clone();
                    route_section = route_section.with_row(PropertyRow::new(
                        "电平",
                        numeric_slider_input_control_with_hard_range(
                            value_db as f32,
                            -60.0,
                            12.0,
                            AUDIO_GAIN_DB_MIN as f32,
                            AUDIO_GAIN_DB_MAX as f32,
                            Some(0.1),
                            1,
                            route.is_editable,
                            move |value| audio_mixer_set_route_gain_action(&gain_route, value),
                        ),
                    ));
                }
                AudioMixerGainModel::Automated { keyframe_count } => {
                    route_section = route_section.with_row(PropertyRow::new(
                        "电平",
                        Box::new(Label::new(format!("自动化 · {keyframe_count} 个关键帧")).muted()),
                    ));
                }
            }
            if let Some(automation) = &route.gain_automation {
                route_section = route_section.with_row(
                    PropertyRow::new("电平曲线", audio_automation_curve_control(automation))
                        .with_height(118.0),
                );
            }
            route_section = route_section.with_row(PropertyRow::new(
                "控制",
                effect_icon_button(
                    AppIcon::Trash,
                    "删除 Route",
                    "删除这条 Route 或 Send",
                    remove_action.is_some(),
                    remove_action,
                ),
            ));
            let rewire_items = channel
                .route_create_options
                .iter()
                .filter_map(|option| {
                    audio_mixer_rewire_route_action(route, option)
                        .map(|action| MenuItem::new(option.label.clone(), action))
                })
                .collect::<Vec<_>>();
            if !rewire_items.is_empty() {
                route_section = route_section.with_row(PropertyRow::new(
                    "重连",
                    Box::new(
                        Dropdown::new("选择新的 tap 与目标…", rewire_items)
                            .with_max_visible_items(12),
                    ),
                ));
            }
            if let Some(reason) = &route.edit_disabled_reason {
                route_section = route_section.with_row(PropertyRow::new(
                    "只读",
                    Box::new(Label::new(reason.clone()).muted()),
                ));
            }
            panel = panel.with_section(route_section);
        }
        panel = with_audio_processor_rack_sections(
            panel,
            &channel.processor_racks,
            channel.is_editable,
        );
    }
    panel
}

fn audio_mixer_meter_label(channel: &crate::app_ui::audio_mixer::AudioMixerChannelModel) -> String {
    let Some(meter) = &channel.meter else {
        return "未执行".to_owned();
    };
    let peak = meter
        .channels
        .iter()
        .map(|reading| reading.sample_peak_linear)
        .fold(0.0_f32, f32::max);
    let rms = meter.channels.iter().map(|reading| reading.rms_linear).fold(0.0_f64, f64::max);
    let clipped = meter.channels.iter().map(|reading| reading.clipped_sample_count).sum::<u64>();
    let invalid = meter
        .channels
        .iter()
        .map(|reading| reading.non_finite_sample_count)
        .sum::<u64>();
    let warning = match (clipped, invalid) {
        (0, 0) => String::new(),
        (clipped, 0) => format!(" · CLIP {clipped}"),
        (0, invalid) => format!(" · 非有限 {invalid}"),
        (clipped, invalid) => format!(" · CLIP {clipped} · 非有限 {invalid}"),
    };
    format!(
        "P {} · RMS {}{warning}",
        audio_meter_dbfs_label(f64::from(peak)),
        audio_meter_dbfs_label(rms),
    )
}

fn audio_meter_dbfs_label(linear: f64) -> String {
    if linear <= 0.0 {
        "−∞ dBFS".to_owned()
    } else {
        format!("{:.1} dBFS", 20.0 * linear.log10())
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
    let curve = if let Some(curve_model) = model.opacity_curve.clone() {
        let points = curve_model.keys.iter().map(|key| key.point).collect();
        let point_policies = curve_model
            .keys
            .iter()
            .map(|key| {
                if key.keyframe_id.is_some() {
                    CurvePointPolicy::editable()
                } else {
                    CurvePointPolicy::anchor()
                }
            })
            .collect();
        let display_points = curve_model.display_points.clone();
        CurveEditor::with_points(points)
            .with_point_policies(point_policies)
            .with_display_points(display_points)
            .enabled(can_edit)
            .on_edit(move |edit| inspector_curve_edit_action(selected_clip, &curve_model, edit))
    } else {
        CurveEditor::with_points(vec![
            CurvePoint::new(0.0, model.opacity / 100.0),
            CurvePoint::new(1.0, model.opacity / 100.0),
        ])
        .disabled()
    };
    let mut style_section = PropertySection::new("剪辑样式")
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
        ));
    if model.shows_tint {
        let mut tint = color_picker_trigger(model.tint).enabled(can_edit);
        tint.picker_mut().set_area_mode(model.tint_area_mode);
        style_section = style_section.with_row(PropertyRow::new(
            "颜色",
            Box::new(tint.on_change(move |color| inspector_color_action(selected_clip, color))),
        ));
    }
    let mut panel = PropertyPanel::new("检查器")
        .with_subtitle(subtitle)
        .with_embedded_panel_chrome()
        .with_section(style_section);

    if !model.clip_properties.is_empty() {
        let mut section = PropertySection::new("基础标题");
        for property in &model.clip_properties {
            section = section.with_row(clip_property_row(property, can_edit, selected_clip));
        }
        panel = panel.with_section(section);
    }

    if !model.audio_components.is_empty() {
        for (index, component) in model.audio_components.iter().enumerate() {
            let edit_id = component.edit_id;
            let volume_static_editable = can_edit
                && component.volume_automation.as_ref().is_none_or(|curve| !curve.is_automated());
            let pan_static_editable = can_edit
                && component.pan_automation.as_ref().is_none_or(|curve| !curve.is_automated());
            let section_title = if model.audio_components.len() == 1 {
                "音频 Component".to_owned()
            } else {
                format!("音频 Component {}", index + 1)
            };
            let mut section = PropertySection::new(section_title)
                .with_row(PropertyRow::new(
                    "启用",
                    Box::new(
                        Checkbox::new("参与混音", component.enabled).enabled(can_edit).on_change(
                            move |value| {
                                audio_component_mutation_action(
                                    selected_clip,
                                    edit_id,
                                    AudioComponentMutation::SetEnabled { value },
                                )
                            },
                        ),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "音量 (dB)",
                    numeric_slider_input_control_with_hard_range(
                        component.volume_db as f32,
                        -60.0,
                        12.0,
                        AUDIO_GAIN_DB_MIN as f32,
                        AUDIO_GAIN_DB_MAX as f32,
                        Some(0.1),
                        1,
                        volume_static_editable,
                        move |value| {
                            audio_component_mutation_action(
                                selected_clip,
                                edit_id,
                                AudioComponentMutation::SetVolumeDb { value: f64::from(value) },
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    "声像 / Balance",
                    numeric_slider_input_control(
                        (component.pan * 100.0) as f32,
                        -100.0,
                        100.0,
                        Some(1.0),
                        0,
                        pan_static_editable,
                        move |value| {
                            audio_component_mutation_action(
                                selected_clip,
                                edit_id,
                                AudioComponentMutation::SetPan { value: f64::from(value) / 100.0 },
                            )
                        },
                    ),
                ));
            if let Some(automation) = &component.volume_automation {
                section = section.with_row(
                    PropertyRow::new("音量曲线", audio_automation_curve_control(automation))
                        .with_height(118.0),
                );
            }
            if let Some(automation) = &component.pan_automation {
                section = section.with_row(
                    PropertyRow::new("声像曲线", audio_automation_curve_control(automation))
                        .with_height(118.0),
                );
            }
            let source_items = component
                .source_options
                .iter()
                .map(|option| {
                    let mut item = MenuItem::new(
                        option.label.clone(),
                        inspector_audio_source_action(selected_clip, edit_id, option.source),
                    )
                    .checked(option.selected);
                    if !option.selectable {
                        item = item.disabled();
                    }
                    item
                })
                .collect::<Vec<_>>();
            let source_enabled = can_edit && !source_items.is_empty();
            section = section.with_row(PropertyRow::new(
                "逻辑源",
                Box::new(
                    Dropdown::new(component.source_label.clone(), source_items)
                        .with_max_visible_items(8)
                        .enabled(source_enabled),
                ),
            ));

            if let Some(binding) = &component.binding {
                let mut binding_items = vec![
                    MenuItem::new(
                        "重新探测当前文件…",
                        inspector_audio_refresh_action(binding.asset_id),
                    ),
                    MenuItem::separator(),
                ];
                binding_items.extend(binding.options.iter().map(|option| {
                    MenuItem::new(
                        option.label.clone(),
                        inspector_audio_rebind_action(
                            binding.asset_id,
                            binding.component_id,
                            option.stream_index,
                        ),
                    )
                    .checked(option.selected)
                }));
                let binding_enabled = can_edit;
                section = section.with_row(PropertyRow::new(
                    "资产流映射",
                    Box::new(
                        Dropdown::new(binding.label.clone(), binding_items)
                            .with_max_visible_items(8)
                            .enabled(binding_enabled),
                    ),
                ));
            }
            section = with_audio_channel_mapping_rows(
                section,
                selected_clip,
                edit_id,
                &component.channel_mapping,
                can_edit,
            );
            let max_fade_seconds = component.clip_duration.to_f64().max(0.0) as f32;
            let fade_in_curve =
                component.fade_in.map(|fade| fade.curve).unwrap_or(AudioFadeCurve::EqualPower);
            let fade_out_curve =
                component.fade_out.map(|fade| fade.curve).unwrap_or(AudioFadeCurve::EqualPower);
            let fade_in_seconds =
                component.fade_in.map(|fade| fade.duration.to_f64() as f32).unwrap_or(0.0);
            let fade_out_seconds =
                component.fade_out.map(|fade| fade.duration.to_f64() as f32).unwrap_or(0.0);
            section = section
                .with_row(PropertyRow::new(
                    "淡入 (s)",
                    numeric_slider_input_control(
                        fade_in_seconds,
                        0.0,
                        max_fade_seconds,
                        Some(0.01),
                        3,
                        can_edit,
                        move |value| {
                            inspector_audio_fade_duration_action(
                                selected_clip,
                                edit_id,
                                true,
                                value,
                                fade_in_curve,
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    "淡入曲线",
                    Box::new(
                        Dropdown::new(
                            audio_fade_curve_label(fade_in_curve),
                            audio_fade_curve_items(selected_clip, edit_id, true, component.fade_in),
                        )
                        .enabled(can_edit && component.fade_in.is_some()),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "淡出 (s)",
                    numeric_slider_input_control(
                        fade_out_seconds,
                        0.0,
                        max_fade_seconds,
                        Some(0.01),
                        3,
                        can_edit,
                        move |value| {
                            inspector_audio_fade_duration_action(
                                selected_clip,
                                edit_id,
                                false,
                                value,
                                fade_out_curve,
                            )
                        },
                    ),
                ))
                .with_row(PropertyRow::new(
                    "淡出曲线",
                    Box::new(
                        Dropdown::new(
                            audio_fade_curve_label(fade_out_curve),
                            audio_fade_curve_items(
                                selected_clip,
                                edit_id,
                                false,
                                component.fade_out,
                            ),
                        )
                        .enabled(can_edit && component.fade_out.is_some()),
                    ),
                ));
            panel = panel.with_section(section);
        }
    }

    panel = with_audio_processor_rack_sections(panel, &model.audio_processor_racks, can_edit);

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

    let mut timing_section = PropertySection::new("时间")
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
        ));
    if let Some(source_timing) = model.source_timing {
        match source_timing.mode {
            InspectorSourceTimingMode::Forward { rate_percent } => {
                let rate_enabled = can_edit && source_timing.can_set_forward_rate;
                timing_section = timing_section.with_row(PropertyRow::new(
                    "速度 (%)",
                    numeric_slider_input_control_with_hard_range(
                        rate_percent,
                        1.0,
                        400.0,
                        0.01,
                        10_000.0,
                        Some(0.01),
                        2,
                        rate_enabled,
                        move |value| inspector_forward_rate_action(selected_clip, value),
                    ),
                ));
            }
            InspectorSourceTimingMode::Hold => {
                timing_section = timing_section.with_row(PropertyRow::new(
                    "源时间",
                    Box::new(Label::new("定格").muted()),
                ));
                let rate_enabled = can_edit && source_timing.can_set_forward_rate;
                timing_section = timing_section.with_row(PropertyRow::new(
                    "恢复速度 (%)",
                    numeric_slider_input_control_with_hard_range(
                        100.0,
                        1.0,
                        400.0,
                        0.01,
                        10_000.0,
                        Some(0.01),
                        2,
                        rate_enabled,
                        move |value| inspector_forward_rate_action(selected_clip, value),
                    ),
                ));
            }
            InspectorSourceTimingMode::ReverseUnsupported { rate_percent } => {
                timing_section = timing_section.with_row(PropertyRow::new(
                    "源时间",
                    Box::new(Label::new(format!("反向 {rate_percent:.2}%（暂不可编辑）")).muted()),
                ));
            }
        }
        if source_timing.supports_picture_hold {
            let freeze_target = source_timing.freeze_at_playhead;
            let label = match (source_timing.mode, freeze_target) {
                (_, None) => "先将播放头移入片段",
                (InspectorSourceTimingMode::Hold, Some(_)) => "更新为播放头画面",
                _ => "在播放头创建定格",
            };
            timing_section = timing_section.with_row(PropertyRow::new(
                "定格帧",
                Box::new(
                    Button::new(label)
                        .enabled(can_edit && freeze_target.is_some())
                        .on_click(inspector_freeze_action(selected_clip, freeze_target)),
                ),
            ));
        }
    }
    panel = panel.with_section(timing_section);

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

fn with_audio_processor_rack_sections(
    mut panel: PropertyPanel,
    racks: &[AudioProcessorRackModel],
    surface_editable: bool,
) -> PropertyPanel {
    for (rack_index, rack) in racks.iter().enumerate() {
        let rack_can_edit = surface_editable && rack.is_editable;
        let duplicate_title_count =
            racks.iter().filter(|candidate| candidate.title == rack.title).count();
        let title = if duplicate_title_count == 1 {
            rack.title.clone()
        } else {
            format!("{} {}", rack.title, rack_index + 1)
        };
        let insert_items = rack
            .insert_options
            .iter()
            .map(|option| {
                MenuItem::new(
                    option.label,
                    audio_processor_insert_action(rack, option.preset),
                )
            })
            .collect();
        let mut rack_section = PropertySection::new(title)
            .with_row(PropertyRow::new(
                "作用域",
                Box::new(Label::new(rack.ownership_label.clone()).muted()),
            ))
            .with_row(PropertyRow::new(
                "添加",
                Box::new(
                    Dropdown::new("添加处理器…", insert_items)
                        .with_max_visible_items(8)
                        .enabled(rack_can_edit),
                ),
            ));
        if let Some(reason) = &rack.edit_disabled_reason {
            rack_section = rack_section.with_row(PropertyRow::new(
                "只读",
                Box::new(Label::new(reason.clone()).muted()),
            ));
        }
        if let Some(automation) = &rack.scope_input_gain_automation {
            rack_section = rack_section.with_row(
                PropertyRow::new("Scope 输入曲线", audio_automation_curve_control(automation))
                    .with_height(118.0),
            );
        }
        panel = panel.with_section(rack_section);

        for (processor_index, processor) in rack.processors.iter().enumerate() {
            let rack_for_bypass = rack.clone();
            let processor_for_bypass = processor.clone();
            let can_move_up = rack_can_edit && processor_index > 0;
            let can_move_down = rack_can_edit && processor_index + 1 < rack.processors.len();
            let move_up = if processor_index > 0 {
                audio_processor_move_before_action(
                    rack,
                    processor,
                    rack.processors[processor_index - 1].processor_id,
                )
            } else {
                audio_processor_move_before_action(rack, processor, processor.processor_id)
            };
            let move_down = if processor_index + 2 < rack.processors.len() {
                audio_processor_move_before_action(
                    rack,
                    processor,
                    rack.processors[processor_index + 2].processor_id,
                )
            } else {
                audio_processor_move_to_end_action(rack, processor)
            };
            let mut section =
                PropertySection::new(processor.label.clone()).with_row(PropertyRow::new(
                    "控制",
                    Box::new(
                        FlexContainer::row(vec![
                            FlexChild::flex(
                                Box::new(
                                    Checkbox::new("旁路", processor.bypassed)
                                        .enabled(rack_can_edit)
                                        .on_change(move |bypassed| {
                                            audio_processor_bypass_action(
                                                &rack_for_bypass,
                                                &processor_for_bypass,
                                                bypassed,
                                            )
                                        }),
                                ),
                                1.0,
                            ),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretUp,
                                "Up",
                                "Move processor up",
                                can_move_up,
                                Some(move_up),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::CaretDown,
                                "Down",
                                "Move processor down",
                                can_move_down,
                                Some(move_down),
                            )),
                            FlexChild::fixed(effect_icon_button(
                                AppIcon::Trash,
                                "Remove",
                                "Remove processor",
                                rack_can_edit,
                                Some(audio_processor_remove_action(rack, processor)),
                            )),
                        ])
                        .with_gap(8.0),
                    ),
                ));
            for parameter in &processor.parameters {
                let label = audio_processor_parameter_label(parameter);
                if parameter.keyframe_count > 0 {
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        Box::new(
                            Label::new(format!("自动化 · {} 个关键帧", parameter.keyframe_count))
                                .muted(),
                        ),
                    ));
                } else if let Some(numeric) = parameter.schema.numeric {
                    let rack_for_parameter = rack.clone();
                    let processor_for_parameter = processor.clone();
                    let parameter_for_action = parameter.clone();
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        numeric_slider_input_control_with_hard_range(
                            parameter.static_value as f32,
                            numeric.soft_range.min as f32,
                            numeric.soft_range.max as f32,
                            numeric.hard_range.min as f32,
                            numeric.hard_range.max as f32,
                            numeric.step.map(|step| step as f32),
                            audio_processor_parameter_decimals(numeric.step),
                            rack_can_edit && parameter.is_static_editable(),
                            move |value| {
                                audio_processor_set_static_parameter_action(
                                    &rack_for_parameter,
                                    &processor_for_parameter,
                                    &parameter_for_action,
                                    value,
                                )
                            },
                        ),
                    ));
                } else {
                    section = section.with_row(PropertyRow::new(
                        label.clone(),
                        Box::new(Label::new("此参数没有数值编辑契约").muted()),
                    ));
                }
                if let Some(automation) = &parameter.automation {
                    section = section.with_row(
                        PropertyRow::new(
                            format!("{label} 曲线"),
                            audio_automation_curve_control(automation),
                        )
                        .with_height(118.0),
                    );
                }
            }
            panel = panel.with_section(section);
        }
    }
    panel
}

fn audio_processor_parameter_label(
    parameter: &crate::app_ui::audio_processor_rack::AudioProcessorParameterModel,
) -> String {
    let unit = match parameter.schema.unit {
        ParameterUnit::Decibels => "dB",
        ParameterUnit::Milliseconds => "ms",
        ParameterUnit::Samples => "samples",
        ParameterUnit::Percent => "%",
        ParameterUnit::Degrees => "°",
        ParameterUnit::Pixels => "px",
        ParameterUnit::Stops => "stops",
        ParameterUnit::Nits => "nits",
        ParameterUnit::Unitless | ParameterUnit::Normalized | ParameterUnit::TimelineTime => "",
    };
    if unit.is_empty() {
        parameter.label.clone()
    } else {
        format!("{} ({unit})", parameter.label)
    }
}

fn audio_processor_parameter_decimals(step: Option<f64>) -> usize {
    match step {
        Some(step) if step >= 1.0 => 0,
        Some(step) if step >= 0.1 => 1,
        Some(step) if step >= 0.01 => 2,
        Some(_) => 3,
        None => 2,
    }
}

fn audio_automation_curve_control(model: &AudioAutomationCurveModel) -> Box<dyn Widget> {
    let action_model = model.clone();
    Box::new(
        CurveEditor::with_points(model.points())
            .with_point_policies(model.point_policies())
            .with_display_points(model.display_points.clone())
            .enabled(model.is_editable)
            .on_edit(move |edit| audio_automation_curve_edit_action(&action_model, edit)),
    )
}

fn numeric_slider_input_control<R>(
    value: f32,
    min: f32,
    max: f32,
    step: Option<f32>,
    decimals: usize,
    enabled: bool,
    action: impl Fn(f32) -> R + 'static,
) -> Box<dyn Widget>
where
    R: Into<Option<Action>>,
{
    numeric_slider_input_control_with_hard_range(
        value, min, max, min, max, step, decimals, enabled, action,
    )
}

#[allow(clippy::too_many_arguments)]
fn numeric_slider_input_control_with_hard_range<R>(
    value: f32,
    soft_min: f32,
    soft_max: f32,
    hard_min: f32,
    hard_max: f32,
    step: Option<f32>,
    decimals: usize,
    enabled: bool,
    action: impl Fn(f32) -> R + 'static,
) -> Box<dyn Widget>
where
    R: Into<Option<Action>>,
{
    let action: Rc<dyn Fn(f32) -> Option<Action>> = Rc::new(move |value| action(value).into());
    let mut slider =
        Slider::new(value.clamp(soft_min, soft_max), soft_min, soft_max).enabled(enabled);
    if let Some(step) = step.filter(|step| step.is_finite() && *step > 0.0) {
        slider = slider.with_step(step);
    }
    let slider_action = Rc::clone(&action);
    slider = slider.on_change(move |value| slider_action(value));

    let mut input = NumberInput::new(value as f64, hard_min as f64, hard_max as f64)
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
) -> Option<Action> {
    if name == "opacity" {
        if let Some(selection) = selection {
            return Some(inspector_set_clip_opacity_action(
                InspectorSetClipOpacityPayload {
                    clip: inspector_clip_payload(selection),
                    opacity_percent: value,
                },
            ));
        }
    }
    None
}

fn inspector_bool_action(selection: Option<SelectedClipRef>, value: bool) -> Option<Action> {
    if let Some(selection) = selection {
        return Some(inspector_set_clip_enabled_action(
            InspectorSetClipEnabledPayload {
                clip: inspector_clip_payload(selection),
                enabled: value,
            },
        ));
    }
    None
}

fn inspector_audio_source_action(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    source: InspectorAudioComponentSourcePayload,
) -> Option<Action> {
    selection.map(|selection| {
        inspector_set_audio_component_source_action(InspectorSetAudioComponentSourcePayload {
            clip: inspector_clip_payload(selection),
            edit_id,
            source,
        })
    })
}

const INSPECTOR_AUDIO_FADE_TIMESCALE: u32 = 1_000;

fn inspector_audio_fade_duration_action(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    fade_in: bool,
    seconds: f32,
    curve: AudioFadeCurve,
) -> Option<Action> {
    if !seconds.is_finite() {
        return None;
    }
    let fade = if seconds <= 0.0 {
        None
    } else {
        let Ok(duration) =
            TimelineTime::from_f64_quantized(f64::from(seconds), INSPECTOR_AUDIO_FADE_TIMESCALE)
        else {
            return None;
        };
        (duration > TimelineTime::ZERO).then_some(AudioFade { duration, curve })
    };
    let mutation = if fade_in {
        AudioComponentMutation::SetFadeIn { value: fade }
    } else {
        AudioComponentMutation::SetFadeOut { value: fade }
    };
    audio_component_mutation_action(selection, edit_id, mutation)
}

fn audio_fade_curve_items(
    selection: Option<SelectedClipRef>,
    edit_id: AudioComponentEditId,
    fade_in: bool,
    fade: Option<AudioFade>,
) -> Vec<MenuItem> {
    let Some(fade) = fade else {
        return Vec::new();
    };
    [AudioFadeCurve::ConstantGain, AudioFadeCurve::EqualPower]
        .into_iter()
        .map(|curve| {
            let updated = Some(AudioFade { duration: fade.duration, curve });
            let mutation = if fade_in {
                AudioComponentMutation::SetFadeIn { value: updated }
            } else {
                AudioComponentMutation::SetFadeOut { value: updated }
            };
            MenuItem::new(
                audio_fade_curve_label(curve),
                audio_component_mutation_action(selection, edit_id, mutation),
            )
            .checked(curve == fade.curve)
        })
        .collect()
}

fn audio_fade_curve_label(curve: AudioFadeCurve) -> &'static str {
    match curve {
        AudioFadeCurve::ConstantGain => "Constant Gain",
        AudioFadeCurve::EqualPower => "Equal Power",
    }
}

fn inspector_audio_rebind_action(
    asset_id: AssetId,
    component_id: AudioSourceComponentId,
    stream_index: u32,
) -> Action {
    assets_rebind_audio_component_action(AssetsRebindAudioComponentPayload {
        asset_id,
        component_id,
        stream_index,
    })
}

fn inspector_audio_refresh_action(asset_id: AssetId) -> Action {
    assets_refresh_audio_components_action(AssetsRefreshAudioComponentsPayload { asset_id })
}

fn inspector_color_action(selection: Option<SelectedClipRef>, color: Color) -> Option<Action> {
    if let Some(selection) = selection {
        return Some(inspector_set_clip_tint_action(
            InspectorSetClipTintPayload { clip: inspector_clip_payload(selection), color },
        ));
    }
    None
}

fn inspector_transform_action(
    selection: Option<SelectedClipRef>,
    field: InspectorClipTransformField,
    value: f32,
) -> Option<Action> {
    if let Some(selection) = selection {
        return Some(inspector_set_clip_transform_field_action(
            InspectorSetClipTransformFieldPayload {
                clip: inspector_clip_payload(selection),
                field,
                value,
            },
        ));
    }
    None
}

fn inspector_timing_action(
    selection: Option<SelectedClipRef>,
    edge: TimelineTrimPayloadEdge,
    frame: f32,
) -> Option<Action> {
    let frame = if frame.is_finite() {
        frame.round() as i64
    } else {
        0
    };
    if let Some(selection) = selection {
        return Some(timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![selection.clip_id],
            edge,
            frame: frame.max(0),
        }));
    }
    None
}

fn inspector_effect_enabled_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    enabled: bool,
) -> Option<Action> {
    if let Some(selection) = selection {
        return Some(inspector_set_effect_enabled_action(
            InspectorSetEffectEnabledPayload {
                clip: inspector_clip_payload(selection),
                effect_id,
                enabled,
            },
        ));
    }
    None
}

fn inspector_effect_select_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> Option<Action> {
    if let Some(selection) = selection {
        return Some(inspector_select_effect_action(
            InspectorSelectEffectPayload { clip: inspector_clip_payload(selection), effect_id },
        ));
    }
    None
}

fn inspector_remove_effect_row_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
) -> Option<Action> {
    if let Some(selection) = selection {
        return Some(inspector_remove_effect_action(
            InspectorRemoveEffectPayload { clip: inspector_clip_payload(selection), effect_id },
        ));
    }
    None
}

fn inspector_reorder_effect_action(
    selection: Option<SelectedClipRef>,
    from: usize,
    to: usize,
) -> Option<Action> {
    if from == to {
        return None;
    }
    selection.map(|selection| Action::ReorderEffects { clip_id: selection.clip_id, from, to })
}

fn inspector_curve_edit_action(
    selection: Option<SelectedClipRef>,
    model: &InspectorCurveModel,
    edit: CurveEdit,
) -> Option<Action> {
    let selection = selection?;
    let edit = match edit {
        CurveEdit::Insert { point, .. } => InspectorCurveEditPayload::Upsert {
            keyframe_id: None,
            point: inspector_curve_point_payload(point),
        },
        CurveEdit::Move { index, point } => {
            let key = model.keys.get(index)?;
            InspectorCurveEditPayload::Upsert {
                keyframe_id: key.keyframe_id,
                point: inspector_curve_point_payload(point),
            }
        }
        CurveEdit::Delete { index } => {
            let keyframe_id = model.keys.get(index).and_then(|key| key.keyframe_id)?;
            InspectorCurveEditPayload::Remove { keyframe_id }
        }
    };
    Some(inspector_edit_clip_curve_action(
        InspectorEditClipCurvePayload {
            clip: inspector_clip_payload(selection),
            property: model.property.clone(),
            edit,
        },
    ))
}

fn inspector_curve_point_payload(point: CurvePoint) -> InspectorCurvePointPayload {
    InspectorCurvePointPayload {
        x: point.x.clamp(0.0, 1.0),
        y: point.y.clamp(0.0, 1.0),
    }
}

fn node_graph_node_action(
    selection: Option<SelectedClipRef>,
    targets: &[NodeGraphNodeTarget],
    node_id: &str,
) -> Option<Action> {
    let selection = selection?;
    match targets
        .iter()
        .find_map(|entry| (entry.node_id == node_id).then_some(entry.target))
    {
        Some(NodeGraphTarget::Effect(effect_id)) => Some(inspector_select_effect_action(
            InspectorSelectEffectPayload { clip: inspector_clip_payload(selection), effect_id },
        )),
        Some(NodeGraphTarget::Clip | NodeGraphTarget::Output) => {
            node_graph_clip_action(Some(selection))
        }
        None => None,
    }
}

fn node_graph_clip_action(selection: Option<SelectedClipRef>) -> Option<Action> {
    selection.map(|selection| {
        timeline_select_clip_action(TimelineSelectClipPayload {
            clip_id: selection.clip_id,
            mode: TimelineClipSelectionModePayload::Replace,
        })
    })
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
    inspector_property_row(
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Effect(effect_id),
    )
}

fn clip_property_row(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
) -> PropertyRow {
    inspector_property_row(property, can_edit, selection, InspectorPropertyTarget::Clip)
}

fn inspector_property_row(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
) -> PropertyRow {
    let row = PropertyRow::new(
        property.label.clone(),
        inspector_property_value_widget(
            property,
            can_edit,
            selection,
            target,
            property.path.clone(),
        ),
    );
    let height = if target == InspectorPropertyTarget::Clip
        && property.path == mondrian_core::BasicTitle::TEXT_PATH
    {
        Some(92.0)
    } else {
        effect_property_row_height(&property.value)
    };
    if let Some(height) = height {
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
#[cfg(test)]
fn effect_property_value_widget(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    path: String,
) -> Box<dyn Widget> {
    inspector_property_value_widget(
        property,
        can_edit,
        selection,
        InspectorPropertyTarget::Effect(effect_id),
        path,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectorPropertyTarget {
    Clip,
    Effect(EffectId),
}

fn inspector_property_value_widget(
    property: &InspectorEffectPropertyModel,
    can_edit: bool,
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    path: String,
) -> Box<dyn Widget> {
    match &property.value {
        PropertyValue::Bool(value) => {
            let selected_clip = selection;
            Box::new(
                Checkbox::new(&property.label, *value).enabled(can_edit).on_change(move |v| {
                    inspector_property_action(selected_clip, target, &path, PropertyValue::Bool(v))
                }),
            )
        }
        PropertyValue::Float(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_numeric_ranges(property, 0.0, 1.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control_with_hard_range(
                *value,
                min,
                max,
                hard_min,
                hard_max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value),
                can_edit,
                move |v| {
                    inspector_property_action(
                        selected_clip,
                        target,
                        &path,
                        PropertyValue::Float(v.clamp(hard_min, hard_max)),
                    )
                },
            )
        }
        PropertyValue::Double(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_numeric_ranges(property, 0.0, 1.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control_with_hard_range(
                *value as f32,
                min,
                max,
                hard_min,
                hard_max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value as f32),
                can_edit,
                move |v: f32| {
                    inspector_property_action(
                        selected_clip,
                        target,
                        &path,
                        PropertyValue::Double((v as f64).clamp(hard_min as f64, hard_max as f64)),
                    )
                },
            )
        }
        PropertyValue::Int(value) => {
            let ((min, max), (hard_min, hard_max)) = parameter_numeric_ranges(property, 0.0, 100.0);
            let selected_clip = selection;
            let path = path.clone();
            numeric_slider_input_control_with_hard_range(
                *value as f32,
                min,
                max,
                hard_min,
                hard_max,
                property_step(property.step, Some(1.0)),
                0,
                can_edit,
                move |v: f32| {
                    inspector_property_action(
                        selected_clip,
                        target,
                        &path,
                        PropertyValue::Int(
                            (v.round() as i64).clamp(hard_min as i64, hard_max as i64),
                        ),
                    )
                },
            )
        }
        PropertyValue::Color(value) => {
            let selected_clip = selection;
            let path = path.clone();
            let trigger = color_picker_trigger(*value).enabled(can_edit);
            Box::new(trigger.on_change(move |color| {
                inspector_property_action(selected_clip, target, &path, PropertyValue::Color(color))
            }))
        }
        PropertyValue::Text(value) => {
            let text = value.clone();
            if target == InspectorPropertyTarget::Clip
                && path == mondrian_core::BasicTitle::TEXT_PATH
            {
                let selected_clip = selection;
                return Box::new(
                    MultilineTextInput::new("标题文本")
                        .with_text(text)
                        .min_lines(3)
                        .enabled(can_edit)
                        .on_change(move |text| {
                            inspector_property_action(
                                selected_clip,
                                target,
                                &path,
                                PropertyValue::Text(text.to_owned()),
                            )
                        }),
                );
            }
            let max_width = 180.0;
            if target != InspectorPropertyTarget::Clip && text.len() > 60 {
                Box::new(Label::new(text).with_max_width(max_width))
            } else {
                let selected_clip = selection;
                let path = path.clone();
                Box::new(
                    TextInput::new(text).enabled(can_edit).on_change(move |text| {
                        inspector_property_action(
                            selected_clip,
                            target,
                            &path,
                            PropertyValue::Text(text.to_string()),
                        )
                    }),
                )
            }
        }
        PropertyValue::Enum(value) => {
            let items = property
                .schema
                .enum_options
                .iter()
                .map(|option| {
                    MenuItem::new(
                        option.key.clone(),
                        inspector_property_action(
                            selection,
                            target,
                            &path,
                            PropertyValue::Enum(option.key.clone()),
                        ),
                    )
                })
                .collect();
            Box::new(Dropdown::new(value.clone(), items).enabled(can_edit))
        }
        PropertyValue::Resource(reference) => {
            let text = match reference {
                ParameterResourceReference::Unbound => String::new(),
                ParameterResourceReference::ExternalFile { path } => path.display().to_string(),
                ParameterResourceReference::ProjectAsset { asset_id } => asset_id.to_string(),
                ParameterResourceReference::Uri { uri } => uri.clone(),
            };
            let selected_clip = selection;
            let path = path.clone();
            Box::new(
                TextInput::new(text).enabled(can_edit).on_change(move |text| {
                    let value = if text.trim().is_empty() {
                        ParameterResourceReference::Unbound
                    } else {
                        ParameterResourceReference::ExternalFile { path: PathBuf::from(text) }
                    };
                    inspector_property_action(
                        selected_clip,
                        target,
                        &path,
                        PropertyValue::Resource(value),
                    )
                }),
            )
        }
        PropertyValue::Vec2(value) => vector_property_widget(
            &["X", "Y"],
            &[value.x, value.y],
            property,
            can_edit,
            selection,
            target,
            path,
            |values| PropertyValue::Vec2(glam::Vec2::new(values[0], values[1])),
        ),
        PropertyValue::Vec3(value) => vector_property_widget(
            &["X", "Y", "Z"],
            &[value.x, value.y, value.z],
            property,
            can_edit,
            selection,
            target,
            path,
            |values| PropertyValue::Vec3(glam::Vec3::new(values[0], values[1], values[2])),
        ),
        PropertyValue::Vec4(value) => vector_property_widget(
            &["X", "Y", "Z", "W"],
            value,
            property,
            can_edit,
            selection,
            target,
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
    target: InspectorPropertyTarget,
    path: String,
    build_value: fn(&[f32]) -> PropertyValue,
) -> Box<dyn Widget> {
    let ((min, max), (hard_min, hard_max)) = parameter_numeric_ranges(property, 0.0, 1.0);
    let values = values
        .iter()
        .map(|value| finite_f32_from_f32(*value).unwrap_or(hard_min).clamp(hard_min, hard_max))
        .collect::<Vec<_>>();
    let rows = labels
        .iter()
        .zip(values.iter())
        .enumerate()
        .map(|(component_index, (label, value))| {
            let base_values = values.to_vec();
            let selected_clip = selection;
            let path = path.clone();
            let control = numeric_slider_input_control_with_hard_range(
                *value,
                min,
                max,
                hard_min,
                hard_max,
                property_step(property.step, None),
                numeric_decimals(property.step, *value),
                can_edit,
                move |v| {
                    let mut next_values = base_values.clone();
                    next_values[component_index] = v.clamp(hard_min, hard_max);
                    inspector_property_action(
                        selected_clip,
                        target,
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

fn parameter_numeric_ranges(
    property: &InspectorEffectPropertyModel,
    default_min: f32,
    default_max: f32,
) -> ((f32, f32), (f32, f32)) {
    let soft = numeric_property_range(property.min, property.max, default_min, default_max);
    let hard = numeric_property_range(property.hard_min, property.hard_max, soft.0, soft.1);
    (soft, hard)
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

#[cfg(test)]
fn inspector_effect_property_action(
    selection: Option<SelectedClipRef>,
    effect_id: EffectId,
    path: &str,
    value: PropertyValue,
) -> Option<Action> {
    inspector_property_action(
        selection,
        InspectorPropertyTarget::Effect(effect_id),
        path,
        value,
    )
}

fn inspector_property_action(
    selection: Option<SelectedClipRef>,
    target: InspectorPropertyTarget,
    path: &str,
    value: PropertyValue,
) -> Option<Action> {
    let selection = selection?;
    Some(match target {
        InspectorPropertyTarget::Clip => {
            inspector_set_clip_property_action(InspectorSetClipPropertyPayload {
                clip: inspector_clip_payload(selection),
                path: path.to_owned(),
                value,
            })
        }
        InspectorPropertyTarget::Effect(effect_id) => {
            inspector_set_effect_property_action(InspectorSetEffectPropertyPayload {
                clip: inspector_clip_payload(selection),
                effect_id,
                path: path.to_owned(),
                value,
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::preview_unavailability::PreviewOutputStage;
    use mondrian_export::queue::{ExportFailure, ExportFailureReason};

    fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
        let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
        mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
    }
    use crate::app::ui_actions::{
        AppShellInterpretAssetDialogPayload, AppShellRelinkAssetDialogPayload,
        AssetsDeleteAssetPayload, AssetsDeleteFolderPayload, AssetsDeleteSelectionPayload,
        AssetsImportFilesPayload, AssetsMoveAssetPayload, AssetsMoveFolderPayload,
        AssetsMoveSelectionPayload, AssetsOpenFolderPayload, AssetsRenameAssetPayload,
        AssetsSetProxyModePayload, ImportMediaDialogPayload, APP_SHELL_IMPORT_MEDIA_DIALOG,
        APP_SHELL_INTERPRET_ASSET_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_RELINK_ASSET_DIALOG,
        ASSETS_CREATE_ADJUSTMENT_LAYER, ASSETS_CREATE_FOLDER, ASSETS_CREATE_SOLID_COLOR,
        ASSETS_DELETE_ASSET, ASSETS_DELETE_FOLDER, ASSETS_DELETE_SELECTION, ASSETS_IMPORT_FILES,
        ASSETS_MOVE_ASSET, ASSETS_MOVE_FOLDER, ASSETS_MOVE_SELECTION, ASSETS_NAMESPACE,
        ASSETS_OPEN_FOLDER, ASSETS_PREPARE_DRAG, ASSETS_REBIND_AUDIO_COMPONENT,
        ASSETS_REFRESH_AUDIO_COMPONENTS, ASSETS_RENAME_ASSET, ASSETS_SET_PROXY_MODE,
        AUDIO_EDIT_COMPONENT, AUDIO_NAMESPACE, EFFECTS_ADD_TO_CLIP, EFFECTS_NAMESPACE,
        INSPECTOR_EDIT_CLIP_CURVE, INSPECTOR_NAMESPACE, INSPECTOR_SELECT_EFFECT,
        INSPECTOR_SET_AUDIO_COMPONENT_SOURCE, INSPECTOR_SET_CLIP_TRANSFORM_FIELD,
        INSPECTOR_SET_EFFECT_PROPERTY, TIMELINE_ADD_TRACK, TIMELINE_CLEAR_IN_OUT_POINTS,
        TIMELINE_CREATE_CROSS_DISSOLVE, TIMELINE_DROP_ASSET, TIMELINE_MOVE_TRACK,
        TIMELINE_NAMESPACE, TIMELINE_OPEN_NESTED_SEQUENCE, TIMELINE_SELECT_CLIP,
        TIMELINE_SELECT_VIDEO_TRANSITION, TIMELINE_SET_IN_OUT_POINT,
        TIMELINE_SET_SELECTED_CLIPS_ENABLED, TIMELINE_SET_VIDEO_TRANSITION_RANGE,
        TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD,
    };
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::automation::{Keyframe, PropertyHost, PropertyMutation, PropertyValue};
    use mondrian_core::types::AssetId;
    use mondrian_core::{FramePosition, SmpteCountingMode, TimelineDisplaySettings};
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
        let clip = Clip::new(asset_id, TimelineTime::ZERO, tt(25, sequence.time_base()))
            .expect("audio Clip");
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
        let mut clip =
            Clip::new(asset_id, TimelineTime::ZERO, tt(12, time_base)).expect("video Clip");
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
            InspectorSourceTimingMode::Forward { rate_percent: 150.0 }
        );
        assert!(source_timing.can_set_forward_rate);
        assert!(source_timing.supports_picture_hold);
        assert_eq!(
            source_timing.freeze_at_playhead,
            Some(FramePosition::new(4, time_base))
        );

        state.active_sequence_mut_uncommitted().expect("active Sequence").video_tracks[0].clips[0]
            .set_constant_source_time_map(
                tt(6, time_base),
                TimeScale::new(0, 1).expect("exact hold"),
            )
            .expect("set hold");
        let held_model = InspectorPanelModel::from_app_state(&state);
        let held_timing = held_model.source_timing.expect("held source-timing model");
        assert_eq!(held_timing.mode, InspectorSourceTimingMode::Hold);
        assert!(held_timing.can_set_forward_rate);
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
            InspectorSourceTimingMode::ReverseUnsupported { rate_percent: 100.0 }
        );
        assert!(!reverse_timing.can_set_forward_rate);
        assert!(!reverse_timing.supports_picture_hold);
        assert_eq!(reverse_timing.freeze_at_playhead, None);

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
        let clip = Clip::new_still_image(asset_id, TimelineTime::ZERO, tt(25, time_base))
            .expect("still Clip");
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
            inspector_forward_rate_action(Some(selection), 150.0),
            Some(Action::SetClipForwardRate {
                clip_id: selection.clip_id,
                rate: TimeScale::new(3, 2).expect("exact 150 percent rate"),
                include_linked: true,
            })
        );
        assert_eq!(
            inspector_forward_rate_action(Some(selection), 33.33),
            Some(Action::SetClipForwardRate {
                clip_id: selection.clip_id,
                rate: TimeScale::new(3_333, 10_000).expect("basis-point rate"),
                include_linked: true,
            })
        );
        assert_eq!(inspector_forward_rate_action(None, 100.0), None);
        for invalid in [f32::NAN, f32::INFINITY, -1.0, 0.0, 10_000.01] {
            assert_eq!(
                inspector_forward_rate_action(Some(selection), invalid),
                None
            );
        }

        let sequence_time = FramePosition::new(42, Rational::new(1, 25));
        assert_eq!(
            inspector_freeze_action(Some(selection), Some(sequence_time)),
            Some(Action::FreezeVideoClipAt { clip_id: selection.clip_id, sequence_time })
        );
        assert_eq!(inspector_freeze_action(Some(selection), None), None);
        assert_eq!(
            inspector_freeze_action(
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
        let payload = model.enqueue_payload().expect("enqueue payload");
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
        let payload = ready.enqueue_payload().expect("valid edited preset");

        assert!(!ready.preset_customized);
        assert_eq!(payload.preset, valid);
    }

    #[test]
    fn export_color_target_modes_expose_only_semantically_valid_spaces() {
        let rendering = export_color_target_spaces(ExportColorTargetMode::RenderingView);
        assert!(rendering.contains(&ColorSpace::Rec709));
        assert!(rendering.contains(&ColorSpace::Rec2100Hlg));
        assert!(rendering.contains(&ColorSpace::Rec2100Pq));
        assert!(!rendering.contains(&ColorSpace::AppleLogBt2020));
        assert!(!rendering.contains(&ColorSpace::LinearRec709));

        let colorimetric = export_color_target_spaces(ExportColorTargetMode::Colorimetric);
        assert!(colorimetric.contains(&ColorSpace::Rec709));
        assert!(colorimetric.contains(&ColorSpace::AppleLogBt2020));
        assert!(!colorimetric.contains(&ColorSpace::LinearRec709));
        assert!(!colorimetric.contains(&ColorSpace::Aces2065_1));
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
        let payload = model.enqueue_payload().expect("valid explicit log target");

        assert_eq!(payload.preset, preset);
        assert_eq!(
            state.active_sequence().expect("active sequence").settings.color.program_output,
            program_output
        );
    }

    #[test]
    fn export_panel_model_does_not_build_enqueue_payload_when_disabled() {
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
        assert!(model.enqueue_payload().is_none());
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
        assert!(model.enqueue_payload().is_none());
        assert!(model.delivery_error.as_deref().is_some_and(|error| error.contains("HDR")));
        assert!(model.readiness_status().starts_with("交付设置不兼容："));
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
    fn export_panel_formats_structured_queue_status_and_diagnostics() {
        let mut input_counts =
            mondrian_timeline::sequence::InputColorResolutionSourceCounts::default();
        input_counts
            .record(mondrian_timeline::sequence::InputColorResolutionSource::DetectedMetadata);
        input_counts.record(mondrian_timeline::sequence::InputColorResolutionSource::Override);
        let mut diagnostics = ExportJobColorDiagnostics::default();
        diagnostics.record_frame_diagnostics(
            input_counts,
            mondrian_renderer::RenderColorStageDiagnostics {
                total_stages: 2,
                cpu_input_stages: 1,
                cpu_output_stages: 1,
                gpu_blockers: 1,
                gpu_blocker_breakdown: mondrian_renderer::RenderColorStageGpuBlockerBreakdown {
                    render_pipeline_not_prepared: 1,
                    ..mondrian_renderer::RenderColorStageGpuBlockerBreakdown::default()
                },
                stage_pixels: 960 * 540 * 2,
                ..mondrian_renderer::RenderColorStageDiagnostics::default()
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
        assert!(
            color_diagnostics.contains("metadata 1 / override 1 / policy 0 / data 0 / reject 0")
        );
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_DELETE_ASSET);
        let payload: AssetsDeleteAssetPayload =
            serde_json::from_value(payload.clone()).expect("delete payload");
        assert_eq!(payload.asset_id, asset_id);

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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_SET_PROXY_MODE);
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
            TimelineCreateCrossDissolvePayload { left_clip_id: left_id, right_clip_id: right_id }
        );
        let Action::Custom { namespace, name, .. } =
            timeline_create_cross_dissolve_action(create_payload)
        else {
            panic!("expected create action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_CREATE_CROSS_DISSOLVE);

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
            Some(TimelineSelectVideoTransitionPayload { transition_id })
        );
        let Action::Custom { namespace, name, .. } = timeline_select_video_transition_action(
            model.transition_identity(transition_ref).expect("selection payload"),
        ) else {
            panic!("expected selection action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_SELECT_VIDEO_TRANSITION);
        assert_eq!(
            model.transition_resize_payload(TimelineTransitionResize {
                transition_ref,
                edge: mondrian_ui_widgets::TimelineTransitionEdge::In,
                old_start_frame: 8,
                old_duration_frames: 4,
                new_start_frame: 7,
                new_duration_frames: 6,
            }),
            Some(TimelineSetVideoTransitionRangePayload {
                transition_id,
                start_frame: 7,
                end_frame: 13,
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
        let Action::Custom { namespace, name, .. } =
            timeline_set_video_transition_range_action(resize_payload)
        else {
            panic!("expected resize action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_SET_VIDEO_TRANSITION_RANGE);
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
        assert_eq!(payload.point, TimelineInOutPointPayloadKind::In);
        assert_eq!(payload.frame, 20);
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
        let Some(Action::Custom { namespace, name, payload }) = trim_action else {
            panic!("expected selected trim action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD);
        let payload: TimelineTrimSelectedClipsToPlayheadPayload =
            serde_json::from_value(payload).expect("trim payload");
        assert_eq!(payload.edge, TimelineTrimPayloadEdge::In);

        let roll_action =
            timeline_edit_command_action(&model, TimelineEditCommand::RollSelectedCutToPlayhead);
        let Some(Action::Custom { namespace, name, payload }) = roll_action else {
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
        let Some(Action::Custom { namespace, name, payload }) = disable_action else {
            panic!("expected selected enable action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_SET_SELECTED_CLIPS_ENABLED);
        let payload: TimelineSetSelectedClipsEnabledPayload =
            serde_json::from_value(payload).expect("enabled payload");
        assert!(!payload.enabled);

        let clear_action =
            timeline_edit_command_action(&model, TimelineEditCommand::ClearInOutPoints);
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
                Clip::new_nested_sequence(
                    nested_id,
                    tt(0, tb),
                    tt(24, tb),
                    Some("Nested".to_owned()),
                )
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

        let Some(Action::Custom { namespace, name, payload }) = action else {
            panic!("expected open nested custom action");
        };
        assert_eq!(namespace, TIMELINE_NAMESPACE);
        assert_eq!(name, TIMELINE_OPEN_NESTED_SEQUENCE);
        let payload: TimelineOpenNestedSequencePayload =
            serde_json::from_value(payload).expect("open nested payload");
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
        let mut clip = Clip::new_solid_color(AssetId::new(), color, tt(4, tb), tt(18, tb))
            .expect("valid clip");
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
        assert_eq!(models.inspector.scale_percent, 125.0);
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
                AssetThumbnailFailure::new(
                    AssetThumbnailFailureReason::DecodeFailed,
                    "test failure",
                ),
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
            schema: ParameterSchema::v1(
                mondrian_core::ParameterId::new_static("mondrian.test.lighting_direction"),
                PropertyValue::Vec3(glam::Vec3::new(0.1, 0.2, 0.3)),
            ),
            path: "lighting.direction".to_string(),
            label: "Direction".to_string(),
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
            schema: ParameterSchema::v1(
                mondrian_core::ParameterId::new_static("mondrian.test.color_exposure"),
                PropertyValue::Float(0.2),
            ),
            path: "color.exposure".to_string(),
            label: "Exposure".to_string(),
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
            schema: ParameterSchema::v1(
                mondrian_core::ParameterId::new_static("mondrian.test.blur_radius"),
                PropertyValue::Float(0.2),
            ),
            path: "blur.radius".to_string(),
            label: "Radius".to_string(),
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
            schema: ParameterSchema::v1(
                mondrian_core::ParameterId::new_static("mondrian.test.color_exposure"),
                PropertyValue::Float(0.2),
            ),
            path: "color.exposure".to_string(),
            label: "Exposure".to_string(),
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
            schema: ParameterSchema::v1(
                mondrian_core::ParameterId::new_static("mondrian.test.level_iterations"),
                PropertyValue::Int(10),
            ),
            path: "levels.iterations".to_string(),
            label: "Iterations".to_string(),
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

        let Some(Action::Custom { namespace, name, payload }) = action else {
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
        assert!(
            !at_clip_start.edit_command_available(TimelineEditCommand::TrimSelectionInToPlayhead)
        );
        assert!(
            at_clip_start.edit_command_available(TimelineEditCommand::TrimSelectionOutToPlayhead)
        );

        state.seek(29).expect("seek");
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
        let solid = Clip::new_solid_color(AssetId::new(), color, tt(0, tb), tt(30, tb))
            .expect("valid clip");
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
                Clip::new_adjustment_layer(AssetId::new(), tt(40, tb), tt(30, tb))
                    .expect("valid clip"),
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
                assert_eq!(namespace, INSPECTOR_NAMESPACE);
                assert_eq!(name, INSPECTOR_EDIT_CLIP_CURVE);
                let payload: InspectorEditClipCurvePayload =
                    serde_json::from_value(payload).expect("curve payload");
                assert_eq!(payload.clip.clip_id, selection.clip_id);
                assert_eq!(payload.property, property);
                assert_eq!(
                    payload.edit,
                    InspectorCurveEditPayload::Upsert {
                        keyframe_id: Some(keyframe_id),
                        point: InspectorCurvePointPayload { x: 0.6, y: 0.7 },
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
        let payload: InspectorEditClipCurvePayload =
            serde_json::from_value(payload).expect("boundary curve payload");
        assert_eq!(
            payload.edit,
            InspectorCurveEditPayload::Remove { keyframe_id: boundary_keyframe_id }
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
            InspectorAudioComponentSourcePayload::Media { component_id },
        );
        let Some(Action::Custom { namespace, name, payload }) = source_action else {
            panic!("expected typed Inspector audio source action");
        };
        assert_eq!(namespace, INSPECTOR_NAMESPACE);
        assert_eq!(name, INSPECTOR_SET_AUDIO_COMPONENT_SOURCE);
        let payload: InspectorSetAudioComponentSourcePayload =
            serde_json::from_value(payload).expect("audio source payload");
        assert_eq!(payload.clip.clip_id, selection.clip_id);
        assert_eq!(payload.edit_id, edit_id);
        assert_eq!(
            payload.source,
            InspectorAudioComponentSourcePayload::Media { component_id }
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
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_REBIND_AUDIO_COMPONENT);
        let payload: AssetsRebindAudioComponentPayload =
            serde_json::from_value(payload).expect("audio rebind payload");
        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(payload.component_id, component_id);
        assert_eq!(payload.stream_index, 7);

        let refresh_action = inspector_audio_refresh_action(asset_id);
        let Action::Custom { namespace, name, payload } = refresh_action else {
            panic!("expected typed Asset audio refresh action");
        };
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_REFRESH_AUDIO_COMPONENTS);
        let payload: AssetsRefreshAudioComponentsPayload =
            serde_json::from_value(payload).expect("audio refresh payload");
        assert_eq!(payload.asset_id, asset_id);
    }

    #[test]
    fn inspector_actions_without_selection_produce_no_command() {
        assert_eq!(inspector_value_action(None, "opacity", 42.0), None);
        assert_eq!(inspector_bool_action(None, true), None);
        assert_eq!(
            inspector_audio_source_action(
                None,
                AudioComponentEditId::new(),
                InspectorAudioComponentSourcePayload::Media {
                    component_id: AudioSourceComponentId::new(),
                },
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
            inspector_transform_action(None, InspectorClipTransformField::PositionX, 12.0),
            None
        );
        assert_eq!(
            inspector_timing_action(None, TimelineTrimPayloadEdge::In, 10.0),
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
        assert_eq!(inspector_reorder_effect_action(None, 1, 0), None);
        assert_eq!(
            inspector_effect_property_action(
                None,
                EffectId::new(),
                "color.tint",
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
            is_editable: false,
            edit_disabled_reason: Some("所选剪辑所在轨道已锁定".to_owned()),
            enabled: true,
            opacity: 100.0,
            tint: Color::from_rgba8(64, 128, 192, 255),
            shows_tint: true,
            position_x: 0.0,
            position_y: 0.0,
            scale_percent: 100.0,
            rotation_degrees: 0.0,
            in_frame: 0.0,
            out_frame: 30.0,
            max_frame: 60.0,
            source_timing: None,
            tint_area_mode: ColorPickerAreaMode::Wheel,
            opacity_curve: None,
            audio_components: Vec::new(),
            audio_processor_racks: Vec::new(),
            clip_properties: Vec::new(),
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
            shows_tint: true,
            position_x: 0.0,
            position_y: 0.0,
            scale_percent: 100.0,
            rotation_degrees: 0.0,
            in_frame: 0.0,
            out_frame: 30.0,
            max_frame: 60.0,
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
            Some(Action::ReorderEffects { clip_id: selection.clip_id, from: 2, to: 0 })
        );
        assert_eq!(inspector_reorder_effect_action(Some(selection), 1, 1), None);
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
                assert_eq!(namespace, ASSETS_NAMESPACE);
                assert_eq!(action_name, name);
            }
            other => panic!("expected assets action, got {other:?}"),
        }
    }
}
