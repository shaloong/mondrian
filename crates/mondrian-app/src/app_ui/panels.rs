//! Panel adapters for the app UI shell.
//!
//! These adapters translate application-facing panel concepts into generic
//! `mondrian-ui-widgets` view models. Demo data is kept behind explicit model
//! factories so typed `AppState` view-model adapters can replace it without
//! changing dock layout or widget construction.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mondrian_assets::library::FolderRecord;
#[cfg(test)]
use mondrian_assets::AssetMediaProbeCandidate;
use mondrian_assets::{AssetKind, AssetLibrary, AssetRecord};
use mondrian_core::automation::{
    AnimationParameterAddress, NormalizedCurve, NormalizedCurvePoint, ParameterResourceReference,
    ParameterSchema, PropertyValue, QualifierSample, QualifierSampleOperation, QualifierSampleSet,
    MAX_QUALIFIER_SAMPLES,
};
use mondrian_core::display_labels::{color_space_label, frame_rate_label};
use mondrian_core::effect_data::EffectType;
use mondrian_core::mask_data::{MaskTrackingDirection, MaskTrackingModel, MaskTrackingSettings};
use mondrian_core::types::{
    AssetId, AudioComponentEditId, AudioSourceComponentId, ClipId, ClipLinkGroupId, ColorSpace,
    EffectId, JobId, KeyframeId, MaskId, Rational, SequenceId, TrackId, VideoTransitionId,
};
use mondrian_core::{
    AudioChannelLayout, Color, DynamicHdrMetadataFamily, FramePosition, FrameRounding,
    ParameterUnit, SampleAspectRatio, SignalLegalizer, TimeScale, TimelineDisplayContract,
    TimelineDisplayFormat, TimelineTime, TimelineTimeRange, WorkingColorSpace,
};
use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::Action;
use mondrian_effects::{
    effect_display_name, effect_library_types, BezierPoint, MaskShape, MaskShapeInterpolation,
};
use mondrian_export::delivery::resolve_export_delivery;
use mondrian_export::preset::{
    AudioCodecConfig, Av1Profile, AvcIntraClass, BuiltinExportPreset, Container, DnxHrProfile,
    ExportAlphaMode, ExportArtifactEncoding, ExportChromaSampling, ExportColorTarget,
    ExportParameter, ExportPreset, H264Profile, HevcProfile, ImageSequenceFormat, ProResProfile,
    ProfessionalDeliveryProfile, Resolution as ExportResolution, TimelineExportRange,
    UncompressedVideoFormat, VideoCodecConfig, VideoRateControl,
};
use mondrian_export::queue::{
    ExportColorHealthSeverity, ExportJobColorDiagnostics, ExportProgress, ExportProgressDetail,
    ExportProgressPhase, JobStatus,
};
use mondrian_export::{VideoCodingStructure, VideoSceneCutPolicy};
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
use mondrian_timeline::VideoTransitionType;
use mondrian_timeline::{
    AudioComponentMutation, DynamicHdrAuthorEdit, DynamicHdrDeliveryIntent,
    EffectRelativePlacement, MaskRelativePlacement, TrackRelativePlacement,
};
use mondrian_ui_core::types::SplitDirection;
use mondrian_ui_core::Widget;
use mondrian_ui_core::{DragPayload, RasterImageColorSpace};
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
    TimelineView, VideoScopesSettings, VideoScopesSurface, VideoScopesTextureSet,
    ViewerCanvasBackground, ViewerComparisonLayout, ViewerComparisonReference, ViewerControl,
    ViewerFrameContent, ViewerPowerWindow, ViewerPowerWindowBezierPoint, ViewerPowerWindowShape,
    ViewerStatusTone, ViewerSurface, WaveformDisplay,
};

use crate::app::exporting::{builtin_export_presets, export_preset_extension};
use crate::app::preview_unavailability::{PreviewUnavailability, PreviewUnavailabilityDisposition};
use crate::app::product_action::{
    GradeAddEffectPayload, GradeCreateDefinitionPayload, GradeProductAction, ProductAction,
};
pub use crate::app::thumbnail_service::{
    ThumbnailFailure as AssetThumbnailFailure,
    ThumbnailFailureReason as AssetThumbnailFailureReason,
};
use crate::app::ui_actions::{
    app_shell_export_output_dialog_action, app_shell_import_media_dialog_action_with_target,
    app_shell_interpret_asset_dialog_action, app_shell_relink_asset_dialog_action,
    app_shell_relocate_panel_action, app_shell_reveal_in_file_manager_action,
    app_shell_scopes_settings_changed_action, assets_create_adjustment_layer_action,
    assets_create_folder_action, assets_create_solid_color_action, assets_delete_asset_action,
    assets_delete_folder_action, assets_delete_selection_action, assets_import_files_action,
    assets_move_asset_action, assets_move_folder_action, assets_move_selection_action,
    assets_open_folder_action, assets_prepare_drag_action, assets_rebind_audio_component_action,
    assets_refresh_audio_components_action, assets_rename_asset_action,
    assets_rename_folder_action, assets_set_proxy_mode_action, clip_edit_numeric_curve_action,
    clip_set_enabled_action, clip_set_solid_color_action, clip_write_parameter_values_action,
    export_cancel_action, export_clear_terminal_history_action, export_edit_draft_action,
    export_enqueue_action, timeline_clear_in_out_points_action, timeline_drop_asset_action,
    timeline_extract_range_action, timeline_lift_range_action, timeline_link_selected_clips_action,
    timeline_move_clip_action, timeline_open_nested_sequence_action,
    timeline_roll_selected_cut_to_playhead_action, timeline_seek_with_source_action,
    timeline_select_clip_action, timeline_set_in_out_point_action,
    timeline_set_selected_clips_enabled_action, timeline_trim_clips_action,
    timeline_trim_selected_clips_to_playhead_action, timeline_unlink_selected_clips_action,
    track_add_action, track_move_action, track_set_author_control_action,
    track_set_edit_policy_action, video_transition_create_cross_dissolve_action,
    video_transition_select_action, video_transition_set_range_action,
    viewer_set_preview_resolution_scale_action, viewer_set_zoom_scale_action,
    visual_effect_add_to_clip_action, visual_effect_remove_action, visual_effect_reorder_action,
    visual_effect_select_action, visual_effect_set_enabled_action,
    visual_effect_set_parameter_value_action, visual_mask_add_to_clip_action,
    visual_mask_cancel_tracking_action, visual_mask_recompute_tracking_action,
    visual_mask_remove_action, visual_mask_reorder_action, visual_mask_select_action,
    visual_mask_set_enabled_action, visual_mask_set_locked_action,
    visual_mask_set_parameter_value_action, visual_mask_set_shape_animation_enabled_action,
    visual_mask_start_tracking_action, visual_mask_write_shape_action,
    AppShellInputColorPipelineDiagnostics, AppShellInterpretAssetDialogPayload,
    AppShellRelinkAssetDialogPayload, AppShellRelocatePanelPayload,
    AppShellRevealInFileManagerPayload, AppShellVideoSignalDiagnostics, AssetsCreateAssetPayload,
    AssetsCreateFolderPayload, AssetsDeleteAssetPayload, AssetsDeleteFolderPayload,
    AssetsDeleteSelectionPayload, AssetsImportFilesPayload, AssetsMoveAssetPayload,
    AssetsMoveFolderPayload, AssetsMoveSelectionPayload, AssetsOpenFolderPayload,
    AssetsPrepareDragPayload, AssetsRebindAudioComponentPayload,
    AssetsRefreshAudioComponentsPayload, AssetsRenameAssetPayload, AssetsRenameFolderPayload,
    AssetsSetProxyModePayload, ClipCurveEditPayload, ClipEditNumericCurvePayload,
    ClipNormalizedCurvePointPayload, ClipParameterValueWrite, ClipSetEnabledPayload,
    ClipSetSolidColorPayload, ClipWriteParameterValuesPayload, DockDropAreaPayload,
    ExportDraftEdit, ExportOutputDialogPayload, ImportMediaDialogPayload, SequenceTargetPayload,
    TimelineClipSelectionModePayload, TimelineDropAssetPayload, TimelineExportRequest,
    TimelineInOutPointKind, TimelineMoveClipPayload, TimelineSeekSource as AppTimelineSeekSource,
    TimelineSelectClipPayload, TimelineSetInOutPointPayload, TimelineTrimClipsPayload,
    TimelineTrimPayloadEdge, TrackAddKind, TrackAddPayload, TrackAuthorControl,
    TrackEditPolicyControl, TrackMovePayload, TrackSetAuthorControlPayload,
    TrackSetEditPolicyPayload, VideoTransitionCreateCrossDissolvePayload,
    VideoTransitionHandlePolicy, VideoTransitionSetRangePayload, VideoTransitionTargetPayload,
    ViewerSetPreviewResolutionScalePayload, ViewerSetZoomScalePayload,
    VisualEffectAddToClipPayload, VisualEffectReorderPayload, VisualEffectSetEnabledPayload,
    VisualEffectSetParameterValuePayload, VisualEffectTargetPayload, VisualMaskAddToClipPayload,
    VisualMaskReorderPayload, VisualMaskSetEnabledPayload, VisualMaskSetLockedPayload,
    VisualMaskSetParameterValuePayload, VisualMaskSetShapeAnimationEnabledPayload,
    VisualMaskStartTrackingPayload, VisualMaskTargetPayload, VisualMaskWriteShapePayload,
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
    inspector_hold_action, inspector_rate_action, inspector_source_timing_model,
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
                    item = item.with_activate_action(visual_effect_add_to_clip_action(
                        VisualEffectAddToClipPayload { clip_id: selection.clip_id, effect_type },
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
    pub sample_aspect_ratio: SampleAspectRatio,
    pub playing: bool,
    pub preview_waiting: bool,
    pub enabled: bool,
    pub frame_content: Option<ViewerFrameContent>,
    /// Frozen Project Gallery reference painted over the current frame.
    pub comparison_reference: Option<ViewerComparisonReference>,
    pub canvas_background: ViewerCanvasBackground,
    /// Whether the exact current presentation is a texture-free transparent canvas.
    pub transparent_canvas: bool,
    pub empty_message: Option<String>,
    pub preview_unavailability: Option<PreviewUnavailability>,
    pub color_rejection: Option<ViewerPreviewColorRejectionModel>,
    pub color_pipeline_status: Option<ViewerColorPipelineStatus>,
    /// Selected Clip-local Power Window projected at the current author time.
    pub power_window: Option<ViewerPowerWindowModel>,
}

/// App-owned identity plus domain-light Viewer geometry for one Power Window.
#[derive(Debug, Clone)]
pub struct ViewerPowerWindowModel {
    pub clip_id: ClipId,
    pub mask_id: MaskId,
    pub overlay: ViewerPowerWindow,
}

/// Program Output scopes data independent from renderer GPU handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopesPanelModel {
    /// Stable registry keys become available only with a current external GPU frame.
    pub textures: Option<VideoScopesTextureSet>,
    /// Machine-local controls shared by the widget and GPU request adapter.
    pub settings: VideoScopesSettings,
    /// Exact encoded signal identity used for guide geometry and labels.
    pub signal_color_space: ColorSpace,
}

impl ScopesPanelModel {
    pub(crate) fn from_viewer(viewer: &ViewerPanelModel) -> Self {
        Self::from_viewer_with_settings(viewer, VideoScopesSettings::default(), ColorSpace::Rec709)
    }

    pub(crate) fn from_viewer_with_settings(
        viewer: &ViewerPanelModel,
        settings: VideoScopesSettings,
        signal_color_space: ColorSpace,
    ) -> Self {
        let textures = matches!(
            viewer.frame_content.as_ref(),
            Some(ViewerFrameContent::ExternalTexture(_))
        )
        .then(crate::app_ui::scopes::texture_set);
        Self { textures, settings, signal_color_space }
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
        let comparison_reference = state.gallery_comparison().and_then(|comparison| {
            let layout = match comparison.layout {
                crate::app::product_action::GalleryComparisonLayout::WipeVertical { position } => {
                    ViewerComparisonLayout::WipeVertical { position }
                }
                crate::app::product_action::GalleryComparisonLayout::WipeHorizontal {
                    position,
                } => ViewerComparisonLayout::WipeHorizontal { position },
                crate::app::product_action::GalleryComparisonLayout::SplitVertical => {
                    ViewerComparisonLayout::SplitVertical
                }
                crate::app::product_action::GalleryComparisonLayout::SplitHorizontal => {
                    ViewerComparisonLayout::SplitHorizontal
                }
            };
            RasterImage::new(
                format!("gallery.still:{}", comparison.still_id),
                comparison.width,
                comparison.height,
                RasterImageColorSpace::Srgb,
                std::sync::Arc::clone(&comparison.rgba),
            )
            .map(|frame| ViewerComparisonReference { frame, layout })
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
            sample_aspect_ratio: sequence
                .settings
                .pixel_aspect_ratio
                .exact_ratio()
                .unwrap_or_default(),
            playing: state.is_playing(),
            preview_waiting,
            enabled: true,
            frame_content,
            comparison_reference,
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
            power_window: viewer_power_window_model(state, sequence),
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
            sample_aspect_ratio: SampleAspectRatio::SQUARE,
            playing: false,
            preview_waiting: false,
            enabled: false,
            frame_content: None,
            comparison_reference: None,
            canvas_background: ViewerCanvasBackground::default(),
            transparent_canvas: false,
            empty_message: Some("未载入序列".into()),
            preview_unavailability: None,
            color_rejection: None,
            color_pipeline_status: None,
            power_window: None,
        }
    }
}

fn viewer_power_window_model(
    state: &AppState,
    sequence: &Sequence,
) -> Option<ViewerPowerWindowModel> {
    let selection = state.primary_selected_clip()?;
    if !selection.is_video_track {
        return None;
    }
    let (mask_id, mask_clip_id, _) = state.primary_selected_mask()?;
    if mask_clip_id != selection.clip_id {
        return None;
    }
    let (resolved_selection, clip) = clip_for_selection(sequence, &selection)?;
    let mask = clip.mask(mask_id)?;
    let timeline_time = state.current_timeline_time().ok().flatten().unwrap_or(sequence.playhead);
    let author_time = clip.clamped_visual_author_time(timeline_time).unwrap_or(clip.clip_time_in);
    let shape = viewer_power_window_shape(&mask.evaluate_at(author_time).shape);
    Some(ViewerPowerWindowModel {
        clip_id: clip.id,
        mask_id,
        overlay: ViewerPowerWindow {
            shape,
            editable: !state.is_playing()
                && !mask.locked
                && !selected_clip_track_is_locked(state, resolved_selection),
        },
    })
}

fn viewer_power_window_shape(shape: &MaskShape) -> ViewerPowerWindowShape {
    match shape {
        MaskShape::Rectangle { x, y, width, height, corner_radius } => {
            ViewerPowerWindowShape::Rectangle {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
                corner_radius: *corner_radius,
            }
        }
        MaskShape::Ellipse { center, radii } => {
            ViewerPowerWindowShape::Ellipse { center: center.to_array(), radii: radii.to_array() }
        }
        MaskShape::Path { points, closed } => ViewerPowerWindowShape::Bezier {
            points: points
                .iter()
                .map(|point| ViewerPowerWindowBezierPoint {
                    position: point.position.to_array(),
                    control_in: point.control_in.to_array(),
                    control_out: point.control_out.to_array(),
                })
                .collect(),
            closed: *closed,
        },
    }
}

fn mask_shape_from_viewer(shape: ViewerPowerWindowShape) -> MaskShape {
    match shape {
        ViewerPowerWindowShape::Rectangle { x, y, width, height, corner_radius } => {
            MaskShape::Rectangle { x, y, width, height, corner_radius }
        }
        ViewerPowerWindowShape::Ellipse { center, radii } => MaskShape::Ellipse {
            center: glam::Vec2::from_array(center),
            radii: glam::Vec2::from_array(radii),
        },
        ViewerPowerWindowShape::Bezier { points, closed } => MaskShape::Path {
            points: points
                .into_iter()
                .map(|point| mondrian_effects::BezierPoint {
                    position: glam::Vec2::from_array(point.position),
                    control_in: glam::Vec2::from_array(point.control_in),
                    control_out: glam::Vec2::from_array(point.control_out),
                })
                .collect(),
            closed,
        },
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
                &timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::In),
                state,
            ),
            trim_out_to_playhead: app_state_action_enabled(
                &timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::Out),
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
        let transition_handle_states = state.map(AppState::video_transition_handle_states);
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
                transition_handle_states.as_deref(),
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
    ) -> Option<VideoTransitionTargetPayload> {
        let transition_id = *self
            .transition_refs
            .get(transition_ref.track_index)?
            .get(transition_ref.transition_index)?;
        Some(VideoTransitionTargetPayload { transition_id })
    }

    fn cut_transition_payload(
        &self,
        cut_ref: TimelineCutRef,
    ) -> Option<VideoTransitionCreateCrossDissolvePayload> {
        let clips = self.clip_refs.get(cut_ref.track_index)?;
        Some(VideoTransitionCreateCrossDissolvePayload {
            left_clip_id: *clips.get(cut_ref.left_clip_index)?,
            right_clip_id: *clips.get(cut_ref.right_clip_index)?,
            handle_policy: VideoTransitionHandlePolicy::Reject,
        })
    }

    fn transition_resize_payload(
        &self,
        resize: TimelineTransitionResize,
    ) -> Option<VideoTransitionSetRangePayload> {
        let transition_id = self.transition_identity(resize.transition_ref)?.transition_id;
        let start_frame = resize.new_start_frame.max(0);
        let end_frame =
            resize.new_start_frame.saturating_add(resize.new_duration_frames.max(1)).max(1);
        let frame_rate = self.timeline_display.frame_rate();
        let time_base = Rational::new(frame_rate.den, frame_rate.num);
        let start =
            TimelineTime::from_frame_position(FramePosition::new(start_frame, time_base)).ok()?;
        let end =
            TimelineTime::from_frame_position(FramePosition::new(end_frame, time_base)).ok()?;
        let requested_range = TimelineTimeRange::new(start, end.checked_sub(start).ok()?).ok()?;
        Some(VideoTransitionSetRangePayload {
            transition_id,
            requested_range,
            handle_policy: VideoTransitionHandlePolicy::Reject,
        })
    }

    fn open_nested_payload(&self, clip_ref: TimelineClipRef) -> Option<SequenceTargetPayload> {
        let sequence_id = self
            .nested_sequence_refs
            .get(clip_ref.track_index)?
            .get(clip_ref.clip_index)
            .copied()
            .flatten()?;
        Some(SequenceTargetPayload { sequence_id })
    }

    fn track_identity(&self, track_ref: TimelineTrackRef) -> Option<AppTimelineTrackRef> {
        self.track_refs.get(track_ref.track_index).copied()
    }

    fn track_control_payload(
        &self,
        control: TimelineTrackControl,
        track_ref: TimelineTrackRef,
        track: &TimelineTrack,
    ) -> Option<TrackSetAuthorControlPayload> {
        let identity = self.track_identity(track_ref)?;
        let (control, enabled) = match control {
            TimelineTrackControl::Target | TimelineTrackControl::SyncLock => return None,
            TimelineTrackControl::Visibility => (TrackAuthorControl::Visibility, !track.visible),
            TimelineTrackControl::Mute => (TrackAuthorControl::Mute, !track.muted),
            TimelineTrackControl::Lock => (TrackAuthorControl::Lock, !track.locked),
        };
        Some(TrackSetAuthorControlPayload { track_id: identity.track_id, control, enabled })
    }

    fn track_targeting_payload(
        &self,
        control: TimelineTrackControl,
        track_ref: TimelineTrackRef,
        track: &TimelineTrack,
    ) -> Option<TrackSetEditPolicyPayload> {
        let identity = self.track_identity(track_ref)?;
        let (control, enabled) = match control {
            TimelineTrackControl::Target => (TrackEditPolicyControl::Target, !track.targeted),
            TimelineTrackControl::SyncLock => {
                (TrackEditPolicyControl::SyncLock, !track.sync_locked)
            }
            TimelineTrackControl::Visibility
            | TimelineTrackControl::Mute
            | TimelineTrackControl::Lock => return None,
        };
        Some(TrackSetEditPolicyPayload { track_id: identity.track_id, control, enabled })
    }

    fn track_move_payload(&self, movement: TimelineTrackMove) -> Option<TrackMovePayload> {
        let source = self.track_identity(movement.track_ref)?;
        let target = *self.track_refs.get(movement.new_track_index)?;
        if source.is_video_track != target.is_video_track
            || movement.old_track_index == movement.new_track_index
            || source.track_id == target.track_id
        {
            return None;
        }
        let moved_toward_display_start = movement.new_track_index < movement.old_track_index;
        let placement = if moved_toward_display_start == source.is_video_track {
            TrackRelativePlacement::After(target.track_id)
        } else {
            TrackRelativePlacement::Before(target.track_id)
        };
        Some(TrackMovePayload { track_id: source.track_id, placement })
    }

    fn asset_drop_payload(&self, drop: TimelineAssetDrop) -> Option<TimelineDropAssetPayload> {
        let target = self.track_identity(drop.track_ref)?;
        let frame_rate = self.timeline_display.frame_rate();
        Some(TimelineDropAssetPayload {
            asset_id: drop.asset_id,
            target_track_id: target.track_id,
            position: FramePosition::new(
                drop.frame.max(0),
                Rational::new(frame_rate.den, frame_rate.num),
            ),
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
            clip_id,
            position: FramePosition::new(
                movement.new_start_frame.max(0),
                Rational::new(
                    self.timeline_display.frame_rate().den,
                    self.timeline_display.frame_rate().num,
                ),
            ),
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
            TimelineTrimEdge::Out => trim.new_start_frame.checked_add(trim.new_duration_frames)?,
        };
        Some(TimelineTrimClipsPayload {
            clip_ids: vec![clip_id],
            edge,
            position: FramePosition::new(
                frame.max(0),
                Rational::new(
                    self.timeline_display.frame_rate().den,
                    self.timeline_display.frame_rate().num,
                ),
            ),
        })
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

/// Stable Clip-owned parameter targets projected for direct Inspector gestures.
#[derive(Debug, Clone)]
pub struct InspectorVisualParameterTargets {
    /// Transform opacity parameter, present for every visual Clip.
    pub opacity: Option<AnimationParameterAddress>,
    /// Transform position parameter, absent for Clip kinds that expose opacity only.
    pub position: Option<AnimationParameterAddress>,
    /// Transform scale parameter, absent for Clip kinds that expose opacity only.
    pub scale: Option<AnimationParameterAddress>,
    /// Transform anchor parameter, absent for Clip kinds that expose opacity only.
    pub anchor: Option<AnimationParameterAddress>,
    /// Transform rotation parameter, absent for Clip kinds that expose opacity only.
    pub rotation: Option<AnimationParameterAddress>,
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
    /// Selected Mask nested inside the selected video Clip.
    pub selected_mask_id: Option<MaskId>,
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
    /// Horizontal transform scale shown in UI percent units.
    pub scale_x_percent: f32,
    /// Vertical transform scale shown in UI percent units.
    pub scale_y_percent: f32,
    /// Horizontal anchor coordinate in source-authoring pixels.
    pub anchor_x: f32,
    /// Vertical anchor coordinate in source-authoring pixels.
    pub anchor_y: f32,
    /// Transform rotation shown in degrees.
    pub rotation_degrees: f32,
    /// Stable author targets for direct visual parameter gestures.
    pub visual_parameters: Option<InspectorVisualParameterTargets>,
    /// Clip in point shown as an absolute timeline frame.
    pub in_frame: f32,
    /// Clip out point shown as an absolute timeline frame.
    pub out_frame: f32,
    /// Maximum timeline frame used by timing sliders.
    pub max_frame: f32,
    /// Explicit Sequence evaluation time base for timing gestures.
    pub timeline_time_base: Rational,
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
    /// Sequence-owned clip/group/timeline Grade Graph hierarchy.
    pub grade: InspectorGradeHierarchyModel,
    /// Masks currently attached to the selected video Clip.
    pub masks: Vec<InspectorMaskModel>,
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
    pub source: AudioComponentSource,
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

/// Compact Inspector projection of the selected Clip's grading hierarchy.
#[derive(Debug, Clone, Default)]
pub struct InspectorGradeHierarchyModel {
    pub clip_definition_id: Option<mondrian_core::GradeDefinitionId>,
    pub clip_grade: Option<String>,
    pub group: Option<String>,
    pub group_pre_grade: Option<String>,
    pub group_post_grade: Option<String>,
    pub timeline_grade: Option<String>,
    pub active_version: Option<String>,
    pub version_count: usize,
    pub node_count: usize,
    /// Clip-grade creation command admitted against current Track authority.
    pub create_clip_grade_action: Option<Action>,
    /// Sequence-level shared-definition edits admitted independently of Clip lock state.
    pub add_node_actions: Vec<InspectorGradeNodeActionModel>,
}

/// One admitted Grade Graph node command shown by the Inspector.
#[derive(Debug, Clone)]
pub struct InspectorGradeNodeActionModel {
    pub label: String,
    pub action: Action,
}

/// One Clip-local visual Mask projected into the Inspector.
#[derive(Debug, Clone)]
pub struct InspectorMaskModel {
    /// Stable Mask identity targeted by all mutations.
    pub mask_id: MaskId,
    /// Human-readable author label.
    pub label: String,
    /// Whether the Mask participates in picture execution.
    pub enabled: bool,
    /// Whether geometry, parameters, order, and removal are locked.
    pub locked: bool,
    /// Whether complete geometry is stored as exact Clip-local shape keys.
    pub shape_animation_enabled: bool,
    /// Current evaluated primitive family shown by the shape control.
    pub shape_label: String,
    /// Latest background tracking state, when any request has run this Session.
    pub tracking_status: Option<crate::app::visual_tracking::VisualTrackingStatus>,
    /// Whether a completed recomputable recipe is persisted on the Mask.
    pub has_tracking_recipe: bool,
    /// Stable-address scalar Mask parameters.
    pub properties: Vec<InspectorEffectPropertyModel>,
}

/// One property row inside an effect inspector section.
#[derive(Debug, Clone)]
pub struct InspectorEffectPropertyModel {
    /// Stable parameter schema consumed independently from the instance address.
    pub schema: ParameterSchema,
    /// Stable owner-local parameter instance used by authoring Actions.
    pub address: AnimationParameterAddress,
    /// Namespaced property path, e.g. `effect.<id>.exposure`.
    ///
    /// This is presentation and resource-editing metadata, never Action identity.
    pub path: String,
    /// Human-readable property name from the descriptor.
    pub label: String,
    /// Definition-owned Inspector group; adjacent equal values form one visual subgroup.
    pub group_name: Option<String>,
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
        address: AnimationParameterAddress {
            animation_track_id: property.track_id,
            parameter_id: property.descriptor.parameter_id().clone(),
        },
        path: path.to_owned(),
        label: property.descriptor.display_name.clone(),
        group_name: property.descriptor.ui_metadata.group_name.clone(),
        value: property.evaluate(author_time),
        min: numeric.map(|contract| contract.soft_range.min),
        max: numeric.map(|contract| contract.soft_range.max),
        hard_min: numeric.map(|contract| contract.hard_range.min),
        hard_max: numeric.map(|contract| contract.hard_range.max),
        step: numeric.and_then(|contract| contract.step),
        is_animatable: property.descriptor.schema.is_animatable,
    }
}

fn inspector_grade_hierarchy_model(
    state: &AppState,
    sequence: &Sequence,
    clip: &Clip,
) -> InspectorGradeHierarchyModel {
    let definition_label =
        |id| sequence.grade_definition(id).map(|definition| definition.name.clone());
    let group = clip
        .grade_group
        .and_then(|id| sequence.grade_groups.iter().find(|group| group.id == id));
    let clip_definition = clip.grade.and_then(|id| sequence.grade_definition(id));
    let active = clip_definition.and_then(|definition| definition.active());
    let create_clip_grade = ProductAction::Grade(GradeProductAction::CreateDefinition(
        GradeCreateDefinitionPayload {
            name: "Clip Grade".to_owned(),
            assign_to: Some(mondrian_timeline::GradeScope::Clip(clip.id)),
        },
    ));
    let create_clip_grade_action = (clip_definition.is_none()
        && state.product_action_availability().allows(&create_clip_grade))
    .then(|| create_clip_grade.into_external_action());
    let add_node_actions = clip_definition.map_or_else(Vec::new, |definition| {
        effect_library_types()
            .iter()
            .filter(|effect_type| {
                matches!(
                    effect_type,
                    EffectType::BasicCorrection
                        | EffectType::WhiteBalance
                        | EffectType::Lut3D
                        | EffectType::ColorWheel
                        | EffectType::HdrGrading
                        | EffectType::AscCdl
                        | EffectType::Curves
                        | EffectType::GamutCompression
                        | EffectType::HighlightRecovery
                        | EffectType::HueSaturationLightness
                )
            })
            .filter_map(|effect_type| {
                let action =
                    ProductAction::Grade(GradeProductAction::AddEffect(GradeAddEffectPayload {
                        definition_id: definition.id,
                        effect_type: effect_type.clone(),
                    }));
                state.product_action_availability().allows(&action).then(|| {
                    InspectorGradeNodeActionModel {
                        label: effect_type.display_name().to_owned(),
                        action: action.into_external_action(),
                    }
                })
            })
            .collect()
    });
    InspectorGradeHierarchyModel {
        clip_definition_id: clip_definition.map(|definition| definition.id),
        clip_grade: clip_definition.map(|definition| definition.name.clone()),
        group: group.map(|group| group.name.clone()),
        group_pre_grade: group.and_then(|group| group.pre_clip_grade).and_then(&definition_label),
        group_post_grade: group.and_then(|group| group.post_clip_grade).and_then(&definition_label),
        timeline_grade: sequence.timeline_grade.and_then(&definition_label),
        active_version: active.map(|version| version.name.clone()),
        version_count: clip_definition.map_or(0, |definition| definition.versions.len()),
        node_count: active.map_or(0, |version| version.graph.nodes.len()),
        create_clip_grade_action,
        add_node_actions,
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
        let clip_author_time = clip.clamped_visual_author_time(time).unwrap_or(clip.clip_time_in);
        let opacity = (clip.transform.evaluate_opacity(clip_author_time) * 100.0).clamp(0.0, 100.0);
        let position = clip.transform.get_position(clip_author_time);
        let scale = clip.transform.get_scale(clip_author_time);
        let anchor = clip.transform.get_anchor_point(clip_author_time);
        let intrinsic_parameters = clip.intrinsic_parameter_bag();
        let visual_parameters = InspectorVisualParameterTargets {
            opacity: intrinsic_parameters.address_for_path(Transform2D::OPACITY_PATH),
            position: intrinsic_parameters.address_for_path(Transform2D::POSITION_PATH),
            scale: intrinsic_parameters.address_for_path(Transform2D::SCALE_PATH),
            anchor: intrinsic_parameters.address_for_path(Transform2D::ANCHOR_POINT_PATH),
            rotation: intrinsic_parameters.address_for_path(Transform2D::ROTATION_PATH),
        };
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
            selected_mask_id: state.primary_selected_mask().and_then(|(mask_id, clip_id, _)| {
                (clip_id == resolved_selection.clip_id).then_some(mask_id)
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
            scale_x_percent: scale.x * 100.0,
            scale_y_percent: scale.y * 100.0,
            anchor_x: anchor.x,
            anchor_y: anchor.y,
            rotation_degrees: clip_rotation_degrees(clip, time),
            visual_parameters: Some(visual_parameters),
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
            timeline_time_base: sequence.time_base(),
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
                    properties: {
                        let mut properties = effect.properties.iter().collect::<Vec<_>>();
                        properties.sort_by(|(left_path, left), (right_path, right)| {
                            left.descriptor
                                .ui_metadata
                                .display_order
                                .unwrap_or(u32::MAX)
                                .cmp(
                                    &right.descriptor.ui_metadata.display_order.unwrap_or(u32::MAX),
                                )
                                .then_with(|| left_path.cmp(right_path))
                        });
                        properties
                            .into_iter()
                            .map(|(path, property)| {
                                inspector_property_model(path, property, clip_author_time)
                            })
                            .collect()
                    },
                })
                .collect(),
            grade: inspector_grade_hierarchy_model(state, sequence, clip),
            masks: clip
                .masks
                .iter()
                .map(|mask| {
                    let evaluated = mask.evaluate_at(clip_author_time);
                    let shape_label = match evaluated.shape {
                        MaskShape::Rectangle { .. } => "矩形",
                        MaskShape::Ellipse { .. } => "椭圆",
                        MaskShape::Path { .. } => "路径",
                    }
                    .to_owned();
                    InspectorMaskModel {
                        mask_id: mask.id,
                        label: mask.name.clone(),
                        enabled: mask.enabled,
                        locked: mask.locked,
                        shape_animation_enabled: mask.shape_animation_enabled,
                        shape_label,
                        tracking_status: state.visual_tracking_status(clip.id, mask.id).cloned(),
                        has_tracking_recipe: mask.tracking.is_some(),
                        properties: mask
                            .properties
                            .iter()
                            .map(|(path, property)| {
                                inspector_property_model(path, property, clip_author_time)
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
            selected_mask_id: None,
            is_editable: false,
            edit_disabled_reason: None,
            enabled: false,
            opacity: 100.0,
            tint: Color::from_rgba8(128, 128, 128, 255),
            shows_tint: false,
            position_x: 0.0,
            position_y: 0.0,
            scale_x_percent: 100.0,
            scale_y_percent: 100.0,
            anchor_x: 0.0,
            anchor_y: 0.0,
            rotation_degrees: 0.0,
            visual_parameters: None,
            in_frame: 0.0,
            out_frame: 1.0,
            max_frame: 1.0,
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
        }
    }

    #[cfg(test)]
    pub fn demo() -> Self {
        Self {
            selected_clip: None,
            empty_message: None,
            selected_effect_id: None,
            selected_mask_id: None,
            is_editable: false,
            edit_disabled_reason: None,
            enabled: true,
            opacity: 72.0,
            tint: Color::from_rgba8(132, 180, 255, 220),
            shows_tint: true,
            position_x: 12.0,
            position_y: -8.0,
            scale_x_percent: 100.0,
            scale_y_percent: 100.0,
            anchor_x: 0.0,
            anchor_y: 0.0,
            rotation_degrees: 0.0,
            visual_parameters: None,
            in_frame: 0.0,
            out_frame: 96.0,
            max_frame: 240.0,
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
    /// Sequence-owned Dynamic HDR author intent and admitted product commands.
    pub dynamic_hdr: ExportDynamicHdrModel,
    pub range: TimelineExportRange,
    pub output_path: String,
    pub delivery_error: Option<String>,
    pub status: Option<(String, bool)>,
    pub jobs: Vec<ExportJobModel>,
    pub can_clear_terminal_history: bool,
}

/// Dynamic HDR delivery state shown by the Export workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportDynamicHdrModel {
    /// Current persistent delivery intent.
    pub intent_label: String,
    /// Honest readiness statement; never a branded-certification claim.
    pub readiness: String,
    /// Number of analyzed final-Program definitions retained by the Sequence.
    pub program_count: usize,
    /// Alternative author intents admitted for the active Sequence.
    pub intent_actions: Vec<ExportDynamicHdrIntentActionModel>,
}

/// One admitted Dynamic HDR delivery-intent command.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportDynamicHdrIntentActionModel {
    /// Product-visible alternative intent label.
    pub label: String,
    /// External typed Product Action emitted by the dropdown.
    pub action: Action,
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
        let selected_sequence_snapshot = selected_sequence_id
            .and_then(|id| sequence_snapshots.iter().find(|sequence| sequence.id == id));
        let dynamic_hdr = selected_sequence_snapshot.map_or_else(
            || ExportDynamicHdrModel {
                intent_label: "没有序列".to_owned(),
                readiness: "选择一个序列以配置 Dynamic HDR 交付".to_owned(),
                program_count: 0,
                intent_actions: Vec::new(),
            },
            |sequence| export_dynamic_hdr_model(state, sequence),
        );
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
        let can_clear_terminal_history = jobs.iter().any(|job| job.status.is_terminal());
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
            dynamic_hdr,
            range: state.export_draft.range,
            output_path: state.export_draft.output_path.clone(),
            delivery_error,
            status: state.status_hint.clone(),
            jobs,
            can_clear_terminal_history,
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

    fn enqueue_request(&self) -> Option<TimelineExportRequest> {
        let preset = self.selected_preset()?.clone();
        let sequence_id = self.selected_sequence_id?;
        if self.delivery_error.is_some() {
            return None;
        }
        let output_path = self.output_path.trim();
        if output_path.is_empty() {
            return None;
        }
        Some(TimelineExportRequest {
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

fn export_dynamic_hdr_model(state: &AppState, sequence: &Sequence) -> ExportDynamicHdrModel {
    let intent_label = match sequence.dynamic_hdr.delivery_intent() {
        DynamicHdrDeliveryIntent::Omit => "省略 Dynamic HDR".to_owned(),
        DynamicHdrDeliveryIntent::PreserveSourceExact { family } => {
            format!("逐字节保留 {}", family.diagnostic_label())
        }
        DynamicHdrDeliveryIntent::Remake { program_id } => sequence
            .dynamic_hdr
            .program(*program_id)
            .map(|program| format!("重制：{}", program.name))
            .unwrap_or_else(|| "重制：缺失 Program".to_owned()),
    };
    let active = state.active_sequence_id() == Some(sequence.id);
    let readiness = match sequence.dynamic_hdr.delivery_intent() {
        DynamicHdrDeliveryIntent::Omit => {
            "已明确省略动态元数据；静态 HDR 仍由序列交付策略独立控制".to_owned()
        }
        DynamicHdrDeliveryIntent::PreserveSourceExact { family } => format!(
            "仅允许完整源文件逐字节复制并复探测 {}；剪辑、重封装或渲染均不回退",
            family.diagnostic_label()
        ),
        DynamicHdrDeliveryIntent::Remake { program_id } => sequence
            .dynamic_hdr
            .program(*program_id)
            .map(|program| {
                format!(
                    "{} / {} 个 shot；需合格且已授权的生成、独立验证和人工 HDR/SDR QC Adapter",
                    program.standard.diagnostic_label(),
                    program.shots.len()
                )
            })
            .unwrap_or_else(|| "引用的 Dynamic HDR Program 已缺失，交付将阻断".to_owned()),
    };
    let mut alternatives = vec![
        (
            "省略 Dynamic HDR".to_owned(),
            DynamicHdrDeliveryIntent::Omit,
        ),
        (
            "逐字节保留 ST 2094-40 App #4".to_owned(),
            DynamicHdrDeliveryIntent::PreserveSourceExact {
                family: DynamicHdrMetadataFamily::St2094_40Application4,
            },
        ),
        (
            "逐字节保留 Dolby Vision 元数据".to_owned(),
            DynamicHdrDeliveryIntent::PreserveSourceExact {
                family: DynamicHdrMetadataFamily::DolbyVision,
            },
        ),
    ];
    alternatives.extend(sequence.dynamic_hdr.programs().iter().map(|program| {
        (
            format!("重制：{}", program.name),
            DynamicHdrDeliveryIntent::Remake { program_id: program.id },
        )
    }));
    let intent_actions = if active {
        alternatives
            .into_iter()
            .filter_map(|(label, intent)| {
                let action =
                    ProductAction::DynamicHdr(DynamicHdrAuthorEdit::SetDeliveryIntent { intent });
                state.product_action_availability().allows(&action).then(|| {
                    ExportDynamicHdrIntentActionModel {
                        label,
                        action: action.into_external_action(),
                    }
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let readiness = if active {
        readiness
    } else {
        format!("{readiness}；先打开此序列才能修改交付意图")
    };
    ExportDynamicHdrModel {
        intent_label,
        readiness,
        program_count: sequence.dynamic_hdr.programs().len(),
        intent_actions,
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
    let surface = VideoScopesSurface::new()
        .with_settings(model.settings, model.signal_color_space)
        .on_settings_changed(app_shell_scopes_settings_changed_action);
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
        .with_sample_aspect_ratio(model.sample_aspect_ratio)
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
    let surface = match model.frame_content.clone() {
        Some(frame_content) => surface.with_frame_content(frame_content),
        None => surface,
    };
    let surface = match model.comparison_reference.clone() {
        Some(reference) => surface.with_comparison_reference(reference),
        None => surface,
    };
    if let Some(window) = model.power_window.clone() {
        let clip_id = window.clip_id;
        let mask_id = window.mask_id;
        surface.with_power_window(window.overlay).on_power_window_edit(move |shape| {
            visual_mask_write_shape_action(VisualMaskWriteShapePayload {
                clip_id,
                mask_id,
                shape: mask_shape_from_viewer(shape),
                interpolation: MaskShapeInterpolation::Hold,
            })
        })
    } else {
        surface
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
    handle_states: Option<&HashMap<VideoTransitionId, VideoTransitionHandleState>>,
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
            if let Some(handle_states) = handle_states {
                let issue = match handle_states.get(&transition.id) {
                    Some(VideoTransitionHandleState::Available) => None,
                    Some(VideoTransitionHandleState::Insufficient) => {
                        Some("当前源素材句柄不足，预览和导出将失败关闭".to_owned())
                    }
                    Some(VideoTransitionHandleState::Unresolved { reason }) => {
                        Some(format!("无法解析当前源素材句柄：{reason}"))
                    }
                    Some(VideoTransitionHandleState::InvalidAuthorState { reason }) => {
                        Some(format!("视频转场作者状态无效：{reason}"))
                    }
                    None => Some("视频转场缺少句柄诊断快照".to_owned()),
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
    if kind == TimelineClipKind::Audio
        && let Some(lib) = library
        && let Some(asset_id) = clip.media_asset_id()
        && let Ok(Some(record)) = lib.get_asset(asset_id)
        && let Some(selection) =
            record.admitted_audio_source_selection(AudioSourceComponentId::primary())
    {
        view = view.with_source_identity(
            record.id,
            selection,
            clip.source_origin().to_f64(),
            clip.source_terminal_boundary().ok()?.to_f64(),
        );
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

fn clip_rotation_degrees(clip: &Clip, time: TimelineTime) -> f32 {
    let author_time = clip.clamped_visual_author_time(time).unwrap_or(clip.clip_time_in);
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
                            camera_raw: video.camera_raw.as_deref().cloned(),
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
                            source: AudioComponentSource::Media { component_id: component.id },
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
                                source: AudioComponentSource::NestedOutput { output_id: output.id },
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
        ChannelLayout::Exact(layout) => layout.to_string(),
        ChannelLayout::Unspecified(channels) => format!("未指定 {channels}ch"),
        ChannelLayout::Unsupported(channels) => format!("不支持的命名布局 {channels}ch"),
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
    let frame_rate = model.timeline_display.frame_rate();
    let in_out_time_base = Rational::new(frame_rate.den, frame_rate.num);
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
                    .map(video_transition_select_action)
            }
        })
        .on_transition_resize({
            let action_model = action_model.clone();
            move |resize, _transition| {
                action_model
                    .transition_resize_payload(resize)
                    .map(video_transition_set_range_action)
            }
        })
        .on_cut_transition_create({
            let action_model = action_model.clone();
            move |cut_ref| {
                action_model
                    .cut_transition_payload(cut_ref)
                    .map(video_transition_create_cross_dissolve_action)
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
            move |movement, _track| action_model.track_move_payload(movement).map(track_move_action)
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
                        .map(track_set_edit_policy_action)
                } else {
                    action_model
                        .track_control_payload(control, track_ref, track)
                        .map(track_set_author_control_action)
                }
            }
        })
        .on_track_add(|kind| {
            let kind = match kind {
                mondrian_ui_widgets::TimelineTrackKind::Video => TrackAddKind::Video,
                mondrian_ui_widgets::TimelineTrackKind::Audio => TrackAddKind::Audio,
            };
            track_add_action(TrackAddPayload { kind })
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
        .on_in_out_point(move |point, frame| {
            timeline_set_in_out_point_action(TimelineSetInOutPointPayload {
                point: timeline_in_out_point_payload_kind(point),
                position: FramePosition::new(frame.max(0), in_out_time_base),
            })
        })
        .on_seek(move |seek| timeline_seek_action_from_widget(seek, in_out_time_base))
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

fn timeline_seek_action_from_widget(seek: TimelineSeek, time_base: Rational) -> Action {
    timeline_seek_with_source_action(
        FramePosition::new(seek.frame, time_base),
        timeline_seek_source_from_widget(seek.source),
    )
}

fn timeline_seek_source_from_widget(source: WidgetTimelineSeekSource) -> AppTimelineSeekSource {
    match source {
        WidgetTimelineSeekSource::PointerDrag => AppTimelineSeekSource::PointerDrag,
        WidgetTimelineSeekSource::Settled => AppTimelineSeekSource::Settled,
    }
}

fn timeline_in_out_point_payload_kind(point: TimelineInOutPoint) -> TimelineInOutPointKind {
    match point {
        TimelineInOutPoint::In => TimelineInOutPointKind::In,
        TimelineInOutPoint::Out => TimelineInOutPointKind::Out,
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
        TimelineEditCommand::TrimSelectionInToPlayhead => Some(
            timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::In),
        ),
        TimelineEditCommand::TrimSelectionOutToPlayhead => Some(
            timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::Out),
        ),
        TimelineEditCommand::RollSelectedCutToPlayhead => {
            Some(timeline_roll_selected_cut_to_playhead_action())
        }
        TimelineEditCommand::EnableSelection => {
            Some(timeline_set_selected_clips_enabled_action(true))
        }
        TimelineEditCommand::DisableSelection => {
            Some(timeline_set_selected_clips_enabled_action(false))
        }
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
            timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::In)
        }
        TimelineEditCommand::TrimSelectionOutToPlayhead => {
            timeline_trim_selected_clips_to_playhead_action(TimelineTrimPayloadEdge::Out)
        }
        TimelineEditCommand::RollSelectedCutToPlayhead => {
            timeline_roll_selected_cut_to_playhead_action()
        }
        TimelineEditCommand::EnableSelection => timeline_set_selected_clips_enabled_action(true),
        TimelineEditCommand::DisableSelection => timeline_set_selected_clips_enabled_action(false),
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
    export_edit_draft_action(ExportDraftEdit::Preset(preset))
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
    if preset.media_file().is_none() {
        return Vec::new();
    }
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
        if let Some(media) = updated.media_file_mut() {
            media.container = container;
        }
        MenuItem::new(label, export_preset_update_action(updated))
    })
    .collect()
}

fn export_video_codec_label(video: &VideoCodecConfig) -> &'static str {
    if let Some(label) = mondrian_export::mezzanine::professional_mezzanine_label(video) {
        return label;
    }
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
        VideoCodecConfig::DnxHr { .. }
        | VideoCodecConfig::AvcIntra { .. }
        | VideoCodecConfig::Uncompressed { .. } => unreachable!("handled above"),
    }
}

fn export_image_sequence_format_label(format: ImageSequenceFormat) -> &'static str {
    match format {
        ImageSequenceFormat::Png8 => "PNG 8-bit（无损）",
        ImageSequenceFormat::Png16 => "PNG 16-bit（无损）",
        ImageSequenceFormat::OpenExrHalf => "OpenEXR Half（ZIP16）",
        ImageSequenceFormat::OpenExrFloat => "OpenEXR Float32（ZIP16）",
        ImageSequenceFormat::Dpx16 => "DPX 16-bit RGB",
        ImageSequenceFormat::Tiff16 => "TIFF 16-bit（Deflate）",
        ImageSequenceFormat::TiffFloat => "TIFF Float32（无损）",
    }
}

fn export_video_rate_control(video: &VideoCodecConfig) -> Option<(VideoRateControl, u8)> {
    match video {
        VideoCodecConfig::H264 { rate_control, .. }
        | VideoCodecConfig::Hevc { rate_control, .. } => Some((*rate_control, 51)),
        VideoCodecConfig::Av1 { rate_control, .. } => Some((*rate_control, 63)),
        VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::DnxHr { .. }
        | VideoCodecConfig::AvcIntra { .. }
        | VideoCodecConfig::Uncompressed { .. }
        | VideoCodecConfig::Gif { .. } => None,
    }
}

fn export_video_codec_items(preset: &ExportPreset) -> Vec<MenuItem> {
    let Some(media) = preset.media_file() else {
        return Vec::new();
    };
    let rate_control = export_video_rate_control(&media.video)
        .map(|(rate_control, _)| rate_control)
        .unwrap_or_else(|| VideoRateControl::constant_quality(20));
    let (gif_colors, gif_dither) = match media.video {
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
        VideoCodecConfig::DnxHr { profile: DnxHrProfile::Lb },
        VideoCodecConfig::DnxHr { profile: DnxHrProfile::Sq },
        VideoCodecConfig::DnxHr { profile: DnxHrProfile::Hq },
        VideoCodecConfig::DnxHr { profile: DnxHrProfile::Hqx },
        VideoCodecConfig::DnxHr { profile: DnxHrProfile::FourFourFour },
        VideoCodecConfig::AvcIntra { class: AvcIntraClass::Class100 },
        VideoCodecConfig::AvcIntra { class: AvcIntraClass::Class200 },
        VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::Yuv422Eight },
        VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::Yuv422Ten },
        VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::RgbEight },
        VideoCodecConfig::Uncompressed { format: UncompressedVideoFormat::RgbTen },
        VideoCodecConfig::Gif { colors: gif_colors, dither: gif_dither },
    ];
    choices
        .into_iter()
        .map(|video| {
            let label = export_video_codec_label(&video);
            let mut updated = preset.clone();
            let video_coding = match video {
                VideoCodecConfig::H264 { .. } | VideoCodecConfig::Hevc { .. } => {
                    VideoCodingStructure::h26x_delivery()
                }
                VideoCodecConfig::Av1 { .. } => VideoCodingStructure::av1_delivery(),
                VideoCodecConfig::ProRes { .. }
                | VideoCodecConfig::DnxHr { .. }
                | VideoCodecConfig::AvcIntra { .. }
                | VideoCodecConfig::Uncompressed { .. }
                | VideoCodecConfig::Gif { .. } => VideoCodingStructure::IntraOnly,
            };
            if let Some(defaults) =
                mondrian_export::mezzanine::professional_mezzanine_authoring_defaults(&video)
            {
                updated.video_signal.bit_depth = ExportParameter::Explicit(defaults.bit_depth);
                updated.video_signal.range = ExportParameter::Explicit(defaults.video_range);
                updated.video_signal.chroma_sampling = defaults.chroma_sampling;
                updated.alpha_mode = ExportAlphaMode::FlattenBlack;
                if let Some(resolution) = defaults.resolution {
                    updated.resolution = Some(ExportResolution {
                        width: resolution.width,
                        height: resolution.height,
                    });
                }
                if let Some(frame_rate) = defaults.frame_rate {
                    updated.frame_rate = ExportParameter::Explicit(frame_rate);
                }
            }
            if let Some(media) = updated.media_file_mut() {
                media.video_coding = video_coding;
                media.video = video.clone();
                if let Some(defaults) =
                    mondrian_export::mezzanine::professional_mezzanine_authoring_defaults(&video)
                {
                    media.container = defaults.container;
                    if defaults.video_only {
                        media.audio = AudioCodecConfig::Disabled;
                    }
                }
            }
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
    let Some(media) = preset.media_file() else {
        return Vec::new();
    };
    let aac_bitrate = match media.audio {
        AudioCodecConfig::Aac { bitrate_kbps } => bitrate_kbps,
        _ => 192,
    };
    let pcm_bit_depth = match media.audio {
        AudioCodecConfig::Pcm { bit_depth } => bit_depth,
        _ => 24,
    };
    let mp3_bitrate = match media.audio {
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
        if let Some(media) = updated.media_file_mut() {
            media.audio = audio;
        }
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

fn export_frame_rate_label(frame_rate: ExportParameter<Rational>) -> String {
    match frame_rate {
        ExportParameter::FollowSequence => "跟随序列".to_owned(),
        ExportParameter::Explicit(frame_rate) => frame_rate_label(frame_rate),
    }
}

fn export_frame_rate_items(preset: &ExportPreset) -> Vec<MenuItem> {
    std::iter::once(ExportParameter::FollowSequence)
        .chain(Rational::SEQUENCE_FRAME_RATES.into_iter().map(ExportParameter::Explicit))
        .map(|frame_rate| {
            let mut updated = preset.clone();
            updated.frame_rate = frame_rate;
            MenuItem::new(
                export_frame_rate_label(frame_rate),
                export_preset_update_action(updated),
            )
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

fn export_color_target_spaces(
    mode: ExportColorTargetMode,
    preset: &ExportPreset,
) -> Vec<ColorSpace> {
    ColorSpace::ALL
        .into_iter()
        .filter(|color_space| match mode {
            ExportColorTargetMode::FollowSequence => false,
            ExportColorTargetMode::RenderingView => color_space.is_display_referred(),
            ExportColorTargetMode::Colorimetric => {
                is_explicit_export_color_space(*color_space)
                    || (preset.image_sequence_format().is_some_and(|format| {
                        !matches!(
                            format,
                            ImageSequenceFormat::Png8 | ImageSequenceFormat::Png16
                        )
                    }) && color_space.is_scene_linear())
            }
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
    export_color_target_spaces(mode, preset)
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
    let Some(media) = preset.media_file_mut() else {
        return preset;
    };
    match &mut media.video {
        VideoCodecConfig::H264 { rate_control: current, .. }
        | VideoCodecConfig::Hevc { rate_control: current, .. }
        | VideoCodecConfig::Av1 { rate_control: current, .. } => *current = rate_control,
        VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::DnxHr { .. }
        | VideoCodecConfig::AvcIntra { .. }
        | VideoCodecConfig::Uncompressed { .. }
        | VideoCodecConfig::Gif { .. } => {}
    }
    preset
}

#[derive(Clone, Copy)]
enum ProfessionalMetadataField {
    Title,
    Issuer,
    Creator,
    Language,
}

fn export_professional_metadata_action(
    mut preset: ExportPreset,
    field: ProfessionalMetadataField,
    value: &str,
) -> Action {
    if let ExportArtifactEncoding::ProfessionalDelivery(delivery) = &mut preset.artifact {
        match field {
            ProfessionalMetadataField::Title => delivery.metadata.title = value.to_owned(),
            ProfessionalMetadataField::Issuer => delivery.metadata.issuer = value.to_owned(),
            ProfessionalMetadataField::Creator => delivery.metadata.creator = value.to_owned(),
            ProfessionalMetadataField::Language => delivery.metadata.language = value.to_owned(),
        }
    }
    export_preset_update_action(preset)
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
                export_edit_draft_action(ExportDraftEdit::BuiltinPreset(option.id)),
            )
        })
        .collect::<Vec<_>>();
    let preset_dropdown = Dropdown::new(preset_label, preset_items).with_max_visible_items(6);
    let media = model.preset.media_file();
    let professional = model.preset.professional_delivery();
    let professional_container_label = professional.map(|delivery| match delivery.profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => "IMF package",
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => "AS-11 X9 OP1a MXF",
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => "SMPTE DCP package",
    });
    let professional_video_label = professional.map(|delivery| match delivery.profile {
        ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => "ProRes 422 HQ / RDD 45",
        ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => "AVC High 4:2:2 / AS-11 X9",
        ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => "JPEG 2000 / ST 428-1 XYZ 12-bit",
    });
    let container_dropdown = Dropdown::new(
        media
            .map(|media| export_container_label(&media.container))
            .or(professional_container_label)
            .unwrap_or("图像序列目录"),
        export_container_items(&model.preset),
    )
    .with_max_visible_items(6)
    .enabled(media.is_some() && professional.is_none());
    let video_codec_dropdown = Dropdown::new(
        media
            .map(|media| export_video_codec_label(&media.video))
            .or(professional_video_label)
            .unwrap_or_else(|| {
                model
                    .preset
                    .image_sequence_format()
                    .map(export_image_sequence_format_label)
                    .unwrap_or("无视频")
            }),
        export_video_codec_items(&model.preset),
    )
    .with_max_visible_items(8)
    .enabled(media.is_some() && professional.is_none());
    let resolution_dropdown = Dropdown::new(
        export_resolution_label(model.preset.resolution),
        export_resolution_items(&model.preset),
    )
    .with_max_visible_items(5)
    .enabled(professional.is_none());
    let frame_rate_dropdown = Dropdown::new(
        export_frame_rate_label(model.preset.frame_rate),
        export_frame_rate_items(&model.preset),
    )
    .with_max_visible_items(8)
    .enabled(professional.is_none());
    let bit_depth_dropdown = Dropdown::new(
        export_bit_depth_label(model.preset.video_signal.bit_depth),
        export_bit_depth_items(&model.preset),
    )
    .with_max_visible_items(4)
    .enabled(media.is_some() && professional.is_none());
    let color_target_mode = export_color_target_mode(model.preset.color_target);
    let color_target_mode_dropdown = Dropdown::new(
        export_color_target_mode_label(color_target_mode),
        export_color_target_mode_items(&model.preset),
    )
    .with_max_visible_items(3)
    .enabled(professional.is_none());
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
    .enabled(professional.is_none() && color_target_mode != ExportColorTargetMode::FollowSequence);
    let video_range_dropdown = Dropdown::new(
        export_video_range_label(model.preset.video_signal.range),
        export_video_range_items(&model.preset),
    )
    .with_max_visible_items(3)
    .enabled(media.is_some() && professional.is_none());
    let chroma_dropdown = Dropdown::new(
        export_chroma_label(model.preset.video_signal.chroma_sampling),
        export_chroma_items(&model.preset),
    )
    .with_max_visible_items(4)
    .enabled(media.is_some() && professional.is_none());
    let alpha_dropdown = Dropdown::new(
        export_alpha_mode_label(model.preset.alpha_mode),
        export_alpha_mode_items(&model.preset),
    )
    .with_max_visible_items(2)
    .enabled(professional.is_none());
    let legalizer_preset = model.preset.clone();
    let legalizer_checkbox = Checkbox::new(
        "限制到合法 RGB 信号范围",
        model.preset.legalizer == SignalLegalizer::ClampRgb,
    )
    .enabled(
        professional.is_none()
            && !matches!(
                model.preset.artifact,
                mondrian_export::preset::ExportArtifactEncoding::AudioStems { .. }
            ),
    )
    .on_change(move |enabled| {
        let mut updated = legalizer_preset.clone();
        updated.legalizer = if enabled {
            SignalLegalizer::ClampRgb
        } else {
            SignalLegalizer::Off
        };
        export_preset_update_action(updated)
    });
    let audio_codec_dropdown = Dropdown::new(
        media
            .map(|media| export_audio_codec_label(&media.audio))
            .or(professional.map(|_| "PCM 24-bit / 48 kHz / stereo"))
            .unwrap_or("无音频（图像序列）"),
        export_audio_codec_items(&model.preset),
    )
    .with_max_visible_items(4)
    .enabled(media.is_some() && professional.is_none());
    let metadata = professional.map(|delivery| delivery.metadata.clone());
    let metadata_enabled = metadata.is_some();
    let title_preset = model.preset.clone();
    let title_input = TextInput::new("交付标题")
        .with_text(metadata.as_ref().map(|value| value.title.clone()).unwrap_or_default())
        .enabled(metadata_enabled)
        .on_change(move |value| {
            export_professional_metadata_action(
                title_preset.clone(),
                ProfessionalMetadataField::Title,
                value,
            )
        });
    let issuer_preset = model.preset.clone();
    let issuer_input = TextInput::new("发行方")
        .with_text(metadata.as_ref().map(|value| value.issuer.clone()).unwrap_or_default())
        .enabled(metadata_enabled)
        .on_change(move |value| {
            export_professional_metadata_action(
                issuer_preset.clone(),
                ProfessionalMetadataField::Issuer,
                value,
            )
        });
    let creator_preset = model.preset.clone();
    let creator_input = TextInput::new("创建系统")
        .with_text(metadata.as_ref().map(|value| value.creator.clone()).unwrap_or_default())
        .enabled(metadata_enabled)
        .on_change(move |value| {
            export_professional_metadata_action(
                creator_preset.clone(),
                ProfessionalMetadataField::Creator,
                value,
            )
        });
    let language_preset = model.preset.clone();
    let language_input = TextInput::new("RFC 5646 语言")
        .with_text(metadata.as_ref().map(|value| value.language.clone()).unwrap_or_default())
        .enabled(metadata_enabled)
        .on_change(move |value| {
            export_professional_metadata_action(
                language_preset.clone(),
                ProfessionalMetadataField::Language,
                value,
            )
        });

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
                export_edit_draft_action(ExportDraftEdit::Sequence(Some(sequence.id))),
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
                export_edit_draft_action(ExportDraftEdit::Range(
                    TimelineExportRange::SequenceInOut,
                )),
            ),
            MenuItem::new(
                export_range_label(TimelineExportRange::EntireSequence),
                export_edit_draft_action(ExportDraftEdit::Range(
                    TimelineExportRange::EntireSequence,
                )),
            ),
        ],
    )
    .enabled(model.can_select_range());
    let dynamic_hdr_dropdown = Dropdown::new(
        model.dynamic_hdr.intent_label.clone(),
        model
            .dynamic_hdr
            .intent_actions
            .iter()
            .map(|item| MenuItem::new(item.label.clone(), item.action.clone()))
            .collect(),
    )
    .with_max_visible_items(8)
    .enabled(!model.dynamic_hdr.intent_actions.is_empty());

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
        .on_change(|text| export_edit_draft_action(ExportDraftEdit::OutputPath(text.to_owned())));
    let output_row = FlexContainer::row(vec![
        FlexChild::flex(Box::new(output_input), 1.0),
        FlexChild::fixed(Box::new(output_browse)),
    ])
    .with_gap(8.0);
    let enqueue_action = model.enqueue_request().map(export_enqueue_action);
    let enqueue_button = AppIcon::Export
        .text_button_or_label("Add to queue")
        .enabled(model.can_enqueue())
        .on_click(enqueue_action);
    let clear_terminal_button = AppIcon::Trash
        .text_button_or_label("Clear finished")
        .enabled(model.can_clear_terminal_history)
        .on_click(export_clear_terminal_history_action());

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
        .with_row(PropertyRow::new("画幅", Box::new(resolution_dropdown)))
        .with_row(PropertyRow::new("帧率", Box::new(frame_rate_dropdown)));
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
        ))
        .with_row(PropertyRow::new("Legalizer", Box::new(legalizer_checkbox)));

    let mut encoding_section = PropertySection::new("编码参数");
    if let Some((rate_control, max_crf)) =
        media.and_then(|media| export_video_rate_control(&media.video))
    {
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

        match media.map(|media| media.video_coding) {
            Some(VideoCodingStructure::H26xLongGop {
                keyframe_interval_seconds,
                max_b_frames,
                closed_gop,
                scene_cut,
            }) => {
                let keyframe_preset = model.preset.clone();
                let keyframe_input = NumberInput::new(keyframe_interval_seconds as f64, 1.0, 10.0)
                    .with_step(1.0)
                    .on_change(move |seconds| {
                        let mut updated = keyframe_preset.clone();
                        if let Some(media) = updated.media_file_mut()
                            && let VideoCodingStructure::H26xLongGop {
                                keyframe_interval_seconds,
                                ..
                            } = &mut media.video_coding
                        {
                            *keyframe_interval_seconds = seconds.round() as u16;
                        }
                        export_preset_update_action(updated)
                    });
                let b_frame_preset = model.preset.clone();
                let b_frame_input = NumberInput::new(max_b_frames as f64, 0.0, 4.0)
                    .with_step(1.0)
                    .on_change(move |frames| {
                        let mut updated = b_frame_preset.clone();
                        if let Some(media) = updated.media_file_mut()
                            && let VideoCodingStructure::H26xLongGop { max_b_frames, .. } =
                                &mut media.video_coding
                        {
                            *max_b_frames = frames.round() as u8;
                        }
                        export_preset_update_action(updated)
                    });
                let closed_preset = model.preset.clone();
                let closed_checkbox =
                    Checkbox::new("封闭 GOP", closed_gop).on_change(move |enabled| {
                        let mut updated = closed_preset.clone();
                        if let Some(media) = updated.media_file_mut()
                            && let VideoCodingStructure::H26xLongGop { closed_gop, .. } =
                                &mut media.video_coding
                        {
                            *closed_gop = enabled;
                        }
                        export_preset_update_action(updated)
                    });
                let scene_cut_preset = model.preset.clone();
                let scene_cut_checkbox = Checkbox::new(
                    "场景切换插入关键帧",
                    scene_cut == VideoSceneCutPolicy::Adaptive,
                )
                .on_change(move |enabled| {
                    let mut updated = scene_cut_preset.clone();
                    if let Some(media) = updated.media_file_mut()
                        && let VideoCodingStructure::H26xLongGop { scene_cut, .. } =
                            &mut media.video_coding
                    {
                        *scene_cut = if enabled {
                            VideoSceneCutPolicy::Adaptive
                        } else {
                            VideoSceneCutPolicy::Disabled
                        };
                    }
                    export_preset_update_action(updated)
                });
                encoding_section = encoding_section
                    .with_row(PropertyRow::new(
                        "关键帧间隔（秒）",
                        Box::new(keyframe_input),
                    ))
                    .with_row(PropertyRow::new("最大连续 B 帧", Box::new(b_frame_input)))
                    .with_row(PropertyRow::new("GOP", Box::new(closed_checkbox)))
                    .with_row(PropertyRow::new("场景切换", Box::new(scene_cut_checkbox)));
            }
            Some(VideoCodingStructure::Av1RandomAccess {
                keyframe_interval_seconds,
                lookahead_frames,
            }) => {
                let keyframe_preset = model.preset.clone();
                let keyframe_input = NumberInput::new(keyframe_interval_seconds as f64, 1.0, 10.0)
                    .with_step(1.0)
                    .on_change(move |seconds| {
                        let mut updated = keyframe_preset.clone();
                        if let Some(media) = updated.media_file_mut()
                            && let VideoCodingStructure::Av1RandomAccess {
                                keyframe_interval_seconds,
                                ..
                            } = &mut media.video_coding
                        {
                            *keyframe_interval_seconds = seconds.round() as u16;
                        }
                        export_preset_update_action(updated)
                    });
                let lookahead_preset = model.preset.clone();
                let lookahead_input = NumberInput::new(lookahead_frames as f64, 0.0, 120.0)
                    .with_step(1.0)
                    .on_change(move |frames| {
                        let mut updated = lookahead_preset.clone();
                        if let Some(media) = updated.media_file_mut()
                            && let VideoCodingStructure::Av1RandomAccess {
                                lookahead_frames, ..
                            } = &mut media.video_coding
                        {
                            *lookahead_frames = frames.round() as u16;
                        }
                        export_preset_update_action(updated)
                    });
                encoding_section = encoding_section
                    .with_row(PropertyRow::new(
                        "关键帧间隔（秒）",
                        Box::new(keyframe_input),
                    ))
                    .with_row(PropertyRow::new("Lookahead 帧", Box::new(lookahead_input)));
            }
            Some(VideoCodingStructure::IntraOnly) | None => {}
        }
    } else if let Some(VideoCodecConfig::Gif { colors, dither }) =
        media.map(|media| media.video.clone())
    {
        let colors_preset = model.preset.clone();
        let colors_input =
            NumberInput::new(colors as f64, 2.0, 256.0)
                .with_step(1.0)
                .on_change(move |colors| {
                    let mut updated = colors_preset.clone();
                    if let Some(media) = updated.media_file_mut()
                        && let VideoCodecConfig::Gif { colors: current, .. } = &mut media.video
                    {
                        *current = colors.round() as u16;
                    }
                    export_preset_update_action(updated)
                });
        let dither_preset = model.preset.clone();
        let dither_checkbox = Checkbox::new("允许调色板抖动", dither).on_change(move |enabled| {
            let mut updated = dither_preset.clone();
            if let Some(media) = updated.media_file_mut()
                && let VideoCodecConfig::Gif { dither, .. } = &mut media.video
            {
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
    match media.map(|media| media.audio.clone()) {
        None | Some(AudioCodecConfig::Disabled) => {}
        Some(AudioCodecConfig::Aac { bitrate_kbps })
        | Some(AudioCodecConfig::Mp3 { bitrate_kbps }) => {
            let bitrate_preset = model.preset.clone();
            let bitrate_input = NumberInput::new(bitrate_kbps as f64, 1.0, 1_536.0)
                .with_step(8.0)
                .on_change(move |bitrate| {
                    let mut updated = bitrate_preset.clone();
                    if let Some(media) = updated.media_file_mut() {
                        match &mut media.audio {
                            AudioCodecConfig::Aac { bitrate_kbps }
                            | AudioCodecConfig::Mp3 { bitrate_kbps } => {
                                *bitrate_kbps = bitrate.round() as u32;
                            }
                            AudioCodecConfig::Disabled | AudioCodecConfig::Pcm { .. } => {}
                        }
                    }
                    export_preset_update_action(updated)
                });
            audio_section =
                audio_section.with_row(PropertyRow::new("码率 kbps", Box::new(bitrate_input)));
        }
        Some(AudioCodecConfig::Pcm { bit_depth }) => {
            let items = [16u8, 24, 32]
                .into_iter()
                .map(|candidate| {
                    let mut updated = model.preset.clone();
                    if let Some(media) = updated.media_file_mut() {
                        media.audio = AudioCodecConfig::Pcm { bit_depth: candidate };
                    }
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
            queue_section.with_row(PropertyRow::new("", Box::new(clear_terminal_button)));
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
        .with_section(
            PropertySection::new("交付元数据")
                .with_row(PropertyRow::new("标题", Box::new(title_input)))
                .with_row(PropertyRow::new("发行方", Box::new(issuer_input)))
                .with_row(PropertyRow::new("创建者", Box::new(creator_input)))
                .with_row(PropertyRow::new("语言", Box::new(language_input))),
        )
        .with_section(color_section)
        .with_section(signal_section)
        .with_section(encoding_section)
        .with_section(audio_section)
        .with_section(
            PropertySection::new("Dynamic HDR 交付")
                .with_row(PropertyRow::new("意图", Box::new(dynamic_hdr_dropdown)))
                .with_row(PropertyRow::new(
                    "Program",
                    Box::new(
                        Label::new(format!(
                            "{} 个已分析 Program",
                            model.dynamic_hdr.program_count
                        ))
                        .muted(),
                    ),
                ))
                .with_row(
                    PropertyRow::new(
                        "资格状态",
                        Box::new(Label::new(model.dynamic_hdr.readiness.clone()).muted().wrapped()),
                    )
                    .with_height(58.0),
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
        let cancel = AppIcon::Trash
            .text_button_or_label("取消")
            .on_click(export_cancel_action(job.id));
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
    if let Some(delivery) = preset.professional_delivery() {
        let (profile, layout, essence) = match delivery.profile {
            ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => (
                "IMF RDD 45",
                "directory package",
                "ProRes 422 HQ 10-bit 4:2:2",
            ),
            ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => (
                "AMWA AS-11 X9",
                "OP1a MXF file",
                "AVC High 4:2:2 10-bit intra",
            ),
            ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => (
                "SMPTE DCP 2K Flat",
                "directory package",
                "JPEG 2000 XYZ 12-bit",
            ),
        };
        return format!(
            "{resolution} / {profile} / {essence} / {} / PCM 24-bit 48 kHz stereo / {layout} / .{}",
            export_frame_rate_label(preset.frame_rate),
            export_preset_extension(preset),
        );
    }
    let Some(media) = preset.media_file() else {
        return format!(
            "{resolution} / PNG 无损序列 / sRGB / {} / {} / .{} 目录",
            export_alpha_mode_label(preset.alpha_mode),
            export_frame_rate_label(preset.frame_rate),
            export_preset_extension(preset)
        );
    };
    let bitrate = match &media.video {
        VideoCodecConfig::H264 { rate_control, .. } => export_rate_control_label(*rate_control),
        VideoCodecConfig::Hevc { rate_control, .. } => export_rate_control_label(*rate_control),
        VideoCodecConfig::Av1 { rate_control, .. } => export_rate_control_label(*rate_control),
        VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::DnxHr { .. }
        | VideoCodecConfig::AvcIntra { .. }
        | VideoCodecConfig::Uncompressed { .. } => "固定 Profile/Class 表示".to_owned(),
        VideoCodecConfig::Gif { colors, dither } => {
            format!(
                "{colors} 色 / dither {}",
                if *dither { "on" } else { "off" }
            )
        }
    };
    let audio = match media.audio {
        AudioCodecConfig::Disabled => "无音频".to_owned(),
        AudioCodecConfig::Aac { bitrate_kbps } => format!("AAC {bitrate_kbps} kbps"),
        AudioCodecConfig::Pcm { bit_depth } => format!("PCM {bit_depth}-bit"),
        AudioCodecConfig::Mp3 { bitrate_kbps } => format!("MP3 {bitrate_kbps} kbps"),
    };
    format!(
        "{resolution} / {} / {bitrate} / {} / {} / {} / {} / {} / {audio} / .{}",
        export_video_codec_label(&media.video),
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
        ExportProgressPhase::Packaging => "Packaging",
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

mod property_panels;
use property_panels::*;

#[cfg(test)]
mod tests;
