//! UI action payloads consumed by [`AppState`](crate::app::AppState).
//!
//! Reusable widget crates remain domain-light. UI adapters attach stable app ids
//! to `Action::Custom` payloads before actions reach the app state layer.

use mondrian_core::automation::AnimationParameterAddress;
use mondrian_core::effect_data::EffectType;
use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::types::{
    AssetId, AudioComponentEditId, AudioSourceComponentId, ClipId, EffectId, JobId, KeyframeId,
    ProgramOutputId, SequenceId, TrackId, VideoTransitionId,
};
use mondrian_core::{
    ColorEngine, ColorSpace, DisplayToneMapPolicy, ProjectColorEnvironment, ProjectSettings,
    Rational, Resolution, TimelineDisplayFormat, WorkingColorSpace,
};
use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_export::preset::{ExportPreset, TimelineExportRange};
use mondrian_media::{
    DecodedVideoRange, DetectedColorInterpretation, ProvenVideoSampling, VideoColorMetadata,
    VideoColorMetadataHint,
};
use mondrian_timeline::{
    sequence::{ColorWorkflow, DeliveryBitDepth, MissingColorMetadataPolicy, VideoRange},
    AudioChannelLayout, AudioChannelStripEditRequest, AudioDisplayFormat,
    AudioProcessorRackEditRequest, AudioRoutingEditRequest, EditingMode, FieldOrder,
    PixelAspectRatio, PreviewRenderFormat, SequenceSettings,
};
use mondrian_ui_theme::ThemePreference;
use mondrian_ui_widgets::{ViewerCanvasBackground, WaveformDisplay};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::product_action::{AudioProductAction, ProductAction, TimelineProductAction};
pub use super::product_action::{
    TimelineClipSelectionModePayload, TimelineMoveClipPayload, TimelineSeekPayload,
    TimelineSeekSource, TimelineSelectClipPayload, TimelineTrimClipsPayload,
    TimelineTrimPayloadEdge, AUDIO_EDIT_COMPONENT, AUDIO_EDIT_PROCESSOR_RACK, AUDIO_NAMESPACE,
    TIMELINE_MOVE_CLIP, TIMELINE_NAMESPACE, TIMELINE_SEEK, TIMELINE_SELECT_CLIP,
    TIMELINE_TRIM_CLIPS,
};
use super::CrashRecoveryCandidate;

/// Action name for linking the current Clip selection.
pub const TIMELINE_LINK_SELECTED_CLIPS: &str = "link_selected_clips";
/// Action name for unlinking the current Clip selection.
pub const TIMELINE_UNLINK_SELECTED_CLIPS: &str = "unlink_selected_clips";
/// Action name for selecting a visual Transition.
pub const TIMELINE_SELECT_VIDEO_TRANSITION: &str = "select_video_transition";
/// Action name for creating the product-default Cross Dissolve at an edit.
pub const TIMELINE_CREATE_CROSS_DISSOLVE: &str = "create_cross_dissolve";
/// Action name for creating a generated Basic Title at the current edit range.
pub const TIMELINE_CREATE_BASIC_TITLE: &str = "create_basic_title";
/// Action name for changing one visual Transition range on the frame grid.
pub const TIMELINE_SET_VIDEO_TRANSITION_RANGE: &str = "set_video_transition_range";
/// Action name for trimming the current clip selection to the playhead.
pub const TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD: &str = "trim_selected_clips_to_playhead";
/// Action name for rolling the selected timeline cut to the playhead.
pub const TIMELINE_ROLL_SELECTED_CUT_TO_PLAYHEAD: &str = "roll_selected_cut_to_playhead";
/// Action name for setting one timeline in/out point to an explicit frame.
pub const TIMELINE_SET_IN_OUT_POINT: &str = "set_in_out_point";
/// Action name for clearing active-sequence in/out points.
pub const TIMELINE_CLEAR_IN_OUT_POINTS: &str = "clear_in_out_points";
/// Action name for lifting the active In/Out range from targeted Tracks.
pub const TIMELINE_LIFT_RANGE: &str = "lift_range";
/// Action name for extracting the active In/Out range from targeted Tracks.
pub const TIMELINE_EXTRACT_RANGE: &str = "extract_range";
/// Action name for toggling the current timeline clip selection.
pub const TIMELINE_SET_SELECTED_CLIPS_ENABLED: &str = "set_selected_clips_enabled";
/// Action name for changing one timeline track header control.
pub const TIMELINE_SET_TRACK_CONTROL: &str = "set_track_control";
/// Action name for changing Track Targeting or Sync-Lock session policy.
pub const TIMELINE_SET_TRACK_TARGETING: &str = "set_track_targeting";
/// Action name for adding a video or audio timeline track.
pub const TIMELINE_ADD_TRACK: &str = "add_track";
/// Action name for reordering one timeline track.
pub const TIMELINE_MOVE_TRACK: &str = "move_track";
/// Action name for dropping one prepared asset onto a timeline track.
pub const TIMELINE_DROP_ASSET: &str = "drop_asset";
/// Action name for inserting one Asset through explicit target/ripple scope.
pub const TIMELINE_INSERT_ASSET: &str = "insert_asset";
/// Action name for replacing the current clip selection with a nested Sequence.
pub const TIMELINE_PRECOMPOSE_SELECTION: &str = "precompose_selection";
/// Action name for opening a nested sequence clip.
pub const TIMELINE_OPEN_NESTED_SEQUENCE: &str = "open_nested_sequence";

/// Custom action namespace for inspector UI operations.
pub const INSPECTOR_NAMESPACE: &str = "ui.inspector";

/// Action name for toggling a selected clip's enabled state.
pub const INSPECTOR_SET_CLIP_ENABLED: &str = "set_clip_enabled";
/// Action name for changing a selected clip's opacity percentage.
pub const INSPECTOR_SET_CLIP_OPACITY: &str = "set_clip_opacity";
/// Action name for changing a selected clip's solid/tint color.
pub const INSPECTOR_SET_CLIP_TINT: &str = "set_clip_tint";
/// Action name for changing one selected clip transform field.
pub const INSPECTOR_SET_CLIP_TRANSFORM_FIELD: &str = "set_clip_transform_field";
/// Action name for committing one stable-identity Clip curve edit.
pub const INSPECTOR_EDIT_CLIP_CURVE: &str = "edit_clip_curve";
/// Action name for changing one definition-backed Clip content property.
pub const INSPECTOR_SET_CLIP_PROPERTY: &str = "set_clip_property";
/// Action name for selecting the logical source of one Clip audio Component Edit.
pub const INSPECTOR_SET_AUDIO_COMPONENT_SOURCE: &str = "set_audio_component_source";
/// Action name for selecting one effect inside the selected clip.
pub const INSPECTOR_SELECT_EFFECT: &str = "select_effect";
/// Action name for toggling one effect on a selected clip.
pub const INSPECTOR_SET_EFFECT_ENABLED: &str = "set_effect_enabled";
/// Action name for removing one effect from a selected clip.
pub const INSPECTOR_REMOVE_EFFECT: &str = "remove_effect";
/// Action name for changing one effect property value.
pub const INSPECTOR_SET_EFFECT_PROPERTY: &str = "set_effect_property";

/// Custom action namespace for effect browser operations.
pub const EFFECTS_NAMESPACE: &str = "ui.effects";

/// Action name for adding an effect to a selected clip.
pub const EFFECTS_ADD_TO_CLIP: &str = "add_to_clip";

/// Custom action namespace for asset-browser operations.
pub const ASSETS_NAMESPACE: &str = "ui.assets";

/// Action name for preparing an asset for timeline drag/drop.
pub const ASSETS_PREPARE_DRAG: &str = "prepare_drag";
/// Action name for re-probing one Asset's audio Component candidates.
pub const ASSETS_REFRESH_AUDIO_COMPONENTS: &str = "refresh_audio_components";
/// Action name for explicitly rebinding one stable Asset audio Component.
pub const ASSETS_REBIND_AUDIO_COMPONENT: &str = "rebind_audio_component";
/// Action name for creating an adjustment-layer asset in the library.
pub const ASSETS_CREATE_ADJUSTMENT_LAYER: &str = "create_adjustment_layer";
/// Action name for creating a solid-color asset in the library.
pub const ASSETS_CREATE_SOLID_COLOR: &str = "create_solid_color";
/// Action name for creating a folder in the library.
pub const ASSETS_CREATE_FOLDER: &str = "create_folder";
/// Action name for opening an asset-browser folder in the app UI shell.
pub const ASSETS_OPEN_FOLDER: &str = "open_folder";
/// Action name for importing files into an asset-browser folder.
pub const ASSETS_IMPORT_FILES: &str = "import_files";
/// Action name for relinking one asset-library record to a new media path.
pub const ASSETS_RELINK_ASSET: &str = "relink_asset";
/// Action name for setting one asset-library record's media interpretation.
pub const ASSETS_SET_INTERPRETATION: &str = "set_interpretation";
/// Action name for renaming one asset-library record.
pub const ASSETS_RENAME_ASSET: &str = "rename_asset";
/// Action name for renaming one asset-library folder/bin.
pub const ASSETS_RENAME_FOLDER: &str = "rename_folder";
/// Action name for toggling proxy playback for one video asset.
pub const ASSETS_SET_PROXY_MODE: &str = "set_proxy_mode";
/// Action name for deleting one asset from the library.
pub const ASSETS_DELETE_ASSET: &str = "delete_asset";
/// Action name for deleting one folder/bin from the library.
pub const ASSETS_DELETE_FOLDER: &str = "delete_folder";
/// Action name for deleting multiple asset-browser items together.
pub const ASSETS_DELETE_SELECTION: &str = "delete_selection";
/// Action name for moving one asset between folders.
pub const ASSETS_MOVE_ASSET: &str = "move_asset";
/// Action name for moving one folder/bin between parents.
pub const ASSETS_MOVE_FOLDER: &str = "move_folder";
/// Action name for moving multiple asset-browser items together.
pub const ASSETS_MOVE_SELECTION: &str = "move_selection";

/// Custom action namespace for export operations.
pub const EXPORT_NAMESPACE: &str = "ui.export";

/// Action name for enqueueing a timeline export job.
pub const EXPORT_ENQUEUE: &str = "enqueue";
/// Action name for updating the app UI export draft.
pub const EXPORT_SET_DRAFT: &str = "set_draft";
/// Action name for cancelling one export queue job.
pub const EXPORT_CANCEL_JOB: &str = "cancel_job";
/// Action name for clearing completed export queue jobs.
pub const EXPORT_CLEAR_COMPLETED: &str = "clear_completed";

/// Custom action namespace for viewer operations.
pub const VIEWER_NAMESPACE: &str = "ui.viewer";

/// Action name for changing the active sequence preview resolution scale.
pub const VIEWER_SET_PREVIEW_RESOLUTION_SCALE: &str = "set_preview_resolution_scale";
/// Action name for changing one selected clip transform from monitor editing.
pub const VIEWER_SET_CLIP_TRANSFORM: &str = "set_clip_transform";
/// Shell-local action name for cycling viewer canvas zoom.
pub const VIEWER_CYCLE_ZOOM: &str = "cycle_zoom";
/// Shell-local action name for setting viewer canvas zoom.
pub const VIEWER_SET_ZOOM_SCALE: &str = "set_zoom_scale";

/// Custom action namespace for project lifecycle operations supplied by shell UI.
pub const PROJECT_NAMESPACE: &str = "ui.project";

/// Action name for creating a project with explicit settings.
pub const PROJECT_CREATE_WITH_SETTINGS: &str = "create_with_settings";
/// Action name for replacing the template copied into future Sequences.
pub const PROJECT_UPDATE_NEW_SEQUENCE_DEFAULTS: &str = "update_new_sequence_defaults";
/// Action name for atomically replacing the Project-wide color engine.
pub const PROJECT_UPDATE_COLOR_ENVIRONMENT: &str = "update_color_environment";
/// Action name for recovering a project from an autosave snapshot.
pub const PROJECT_RECOVER_FROM_AUTOSAVE: &str = "recover_from_autosave";

/// Custom action namespace for sequence management operations.
pub const SEQUENCE_NAMESPACE: &str = "ui.sequence";

/// Action name for returning from a nested sequence to its parent sequence.
pub const SEQUENCE_RETURN_TO_PARENT: &str = "return_to_parent";
/// Action name for making the active sequence the project default sequence.
pub const SEQUENCE_SET_ACTIVE_DEFAULT: &str = "set_active_default";
/// Action name for creating a new sequence with product defaults.
pub const SEQUENCE_NEW: &str = "new";
/// Action name for switching the active sequence.
pub const SEQUENCE_SWITCH_ACTIVE: &str = "switch_active";
/// Action name for duplicating a sequence.
pub const SEQUENCE_DUPLICATE: &str = "duplicate";
/// Action name for deleting a sequence.
pub const SEQUENCE_DELETE: &str = "delete";
/// Action name for updating sequence identity/settings from app UI.
pub const SEQUENCE_UPDATE_SETTINGS: &str = "update_settings";

/// Custom action namespace for app-shell operations resolved by native adapters.
pub const APP_SHELL_NAMESPACE: &str = "app.shell";

/// App-shell request to create a new project through a platform save dialog.
pub const APP_SHELL_NEW_PROJECT_DIALOG: &str = "new_project_dialog";
/// App-shell request to update one app UI new-project draft setting.
pub const APP_SHELL_NEW_PROJECT_DRAFT_CHANGED: &str = "new_project_draft_changed";
/// App-shell request to open project-level color settings.
pub const APP_SHELL_PROJECT_SETTINGS: &str = "project_settings";
/// App-shell request to update the project-settings color draft.
pub const APP_SHELL_PROJECT_SETTINGS_DRAFT_CHANGED: &str = "project_settings_draft_changed";
/// App-shell request to commit project-level color settings.
pub const APP_SHELL_CONFIRM_PROJECT_SETTINGS: &str = "confirm_project_settings";
/// App-shell request to choose and validate a Custom OCIO project config.
pub const APP_SHELL_SELECT_CUSTOM_OCIO_CONFIG: &str = "select_custom_ocio_config";
/// App-shell request to confirm the app UI new-project dialog.
pub const APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG: &str = "confirm_new_project_dialog";
/// App-shell request to cancel the app UI new-project dialog.
pub const APP_SHELL_CANCEL_NEW_PROJECT_DIALOG: &str = "cancel_new_project_dialog";
/// App-shell request to open a platform project file dialog.
pub const APP_SHELL_OPEN_PROJECT_DIALOG: &str = "open_project_dialog";
/// App-shell request to open one project from the app UI recent list.
pub const APP_SHELL_OPEN_RECENT_PROJECT: &str = "open_recent_project";
/// App-shell request to recover a project from a startup autosave candidate.
pub const APP_SHELL_RECOVER_PROJECT: &str = "recover_project";
/// App-shell request to open a platform media import dialog.
pub const APP_SHELL_IMPORT_MEDIA_DIALOG: &str = "import_media_dialog";
/// App-shell request to reveal one real file in the platform file manager.
pub const APP_SHELL_REVEAL_IN_FILE_MANAGER: &str = "reveal_in_file_manager";
/// App-shell request to choose a replacement media file for one asset.
pub const APP_SHELL_RELINK_ASSET_DIALOG: &str = "relink_asset_dialog";
/// App-shell request to open the Interpret Footage dialog for one asset.
pub const APP_SHELL_INTERPRET_ASSET_DIALOG: &str = "interpret_asset_dialog";
/// App-shell request to update the Interpret Footage dialog draft.
pub const APP_SHELL_INTERPRET_ASSET_DRAFT_CHANGED: &str = "interpret_asset_draft_changed";
/// App-shell request to apply the Interpret Footage dialog.
pub const APP_SHELL_CONFIRM_INTERPRET_ASSET_DIALOG: &str = "confirm_interpret_asset_dialog";
/// App-shell request to open a platform project save-as dialog.
pub const APP_SHELL_SAVE_PROJECT_AS_DIALOG: &str = "save_project_as_dialog";
/// App-shell request to choose a timeline export output file.
pub const APP_SHELL_EXPORT_OUTPUT_DIALOG: &str = "export_output_dialog";
/// App-shell request to show product about information.
pub const APP_SHELL_ABOUT: &str = "about";
/// App-shell request to show app UI preferences.
pub const APP_SHELL_PREFERENCES: &str = "preferences";
/// App-shell request to show the active sequence settings dialog.
pub const APP_SHELL_SEQUENCE_SETTINGS: &str = "sequence_settings";
/// App-shell request to update one app UI sequence-settings draft field.
pub const APP_SHELL_SEQUENCE_SETTINGS_DRAFT_CHANGED: &str = "sequence_settings_draft_changed";
/// App-shell request to apply the active sequence settings dialog.
pub const APP_SHELL_CONFIRM_SEQUENCE_SETTINGS: &str = "confirm_sequence_settings";
/// App-shell request to switch the active sequence-settings tab.
pub const APP_SHELL_SEQUENCE_SETTINGS_TAB_CHANGED: &str = "sequence_settings_tab_changed";
/// App-shell request to switch the active app UI preferences tab.
pub const APP_SHELL_PREFERENCES_TAB_CHANGED: &str = "preferences_tab_changed";
/// App-shell request to switch the active app UI theme preset.
pub const APP_SHELL_PREFERENCES_THEME_CHANGED: &str = "preferences_theme_changed";
/// App-shell request to switch the waveform display mode.
pub const APP_SHELL_PREFERENCES_WAVEFORM_DISPLAY_CHANGED: &str =
    "preferences_waveform_display_changed";
/// App-shell request to switch the presentation-only Viewer canvas background.
pub const APP_SHELL_PREFERENCES_VIEWER_BACKGROUND_CHANGED: &str =
    "preferences_viewer_background_changed";
/// App-shell request to disable one app UI shortcut descriptor.
pub const APP_SHELL_PREFERENCES_SHORTCUT_DISABLED: &str = "preferences_shortcut_disabled";
/// App-shell request to restore one app UI shortcut descriptor to default.
pub const APP_SHELL_PREFERENCES_SHORTCUT_RESET: &str = "preferences_shortcut_reset";
/// App-shell request to bind one app UI shortcut descriptor to a new key chord.
pub const APP_SHELL_PREFERENCES_SHORTCUT_REBOUND: &str = "preferences_shortcut_rebound";
/// App-shell request to close the current shell-local modal.
pub const APP_SHELL_CLOSE_MODAL: &str = "close_modal";
/// App-shell request to copy system info to the native clipboard.
pub const APP_SHELL_COPY_SYSTEM_INFO: &str = "copy_system_info";
/// App-shell request to save before continuing a pending close/quit flow.
pub const APP_SHELL_PENDING_CLOSE_SAVE_CONTINUE: &str = "pending_close_save_continue";
/// App-shell request to continue a pending close/quit flow without saving.
pub const APP_SHELL_PENDING_CLOSE_DISCARD: &str = "pending_close_discard";
/// App-shell request to cancel a pending close/quit flow.
pub const APP_SHELL_PENDING_CLOSE_CANCEL: &str = "pending_close_cancel";
/// App-shell request to quit the native application window.
pub const APP_SHELL_QUIT: &str = "quit";
/// App-shell request to minimize the native application window.
pub const APP_SHELL_WINDOW_MINIMIZE: &str = "window_minimize";
/// App-shell request to toggle the native application window maximized state.
pub const APP_SHELL_WINDOW_TOGGLE_MAXIMIZE: &str = "window_toggle_maximize";
/// App-shell request to begin native window dragging from custom chrome.
pub const APP_SHELL_WINDOW_DRAG: &str = "window_drag";
/// App-shell request to relocate one dock panel tab in the workspace layout.
pub const APP_SHELL_RELOCATE_PANEL: &str = "relocate_panel";

/// App UI preferences section selected by the shell-local preferences UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreferencesTabPayload {
    General,
    Media,
    Shortcuts,
    Developer,
}

/// Shell request to copy system info text to the native clipboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppShellCopySystemInfoPayload {
    pub text: String,
}

/// Theme preference selected by the app UI preferences UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesThemePayload {
    /// Theme preference to apply and persist.
    pub preference: ThemePreference,
}

/// Waveform display mode selected by the app UI preferences UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesWaveformDisplayPayload {
    pub mode: WaveformDisplay,
}

/// Viewer canvas background selected by the app UI preferences UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesViewerBackgroundPayload {
    pub background: ViewerCanvasBackground,
}

/// Stable shortcut descriptor selected in the app UI preferences UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesShortcutPayload {
    /// Stable shortcut descriptor id.
    pub id: String,
}

/// New key chord captured by the app UI shortcut preferences UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesShortcutReboundPayload {
    /// Stable shortcut descriptor id.
    pub id: String,
    /// Stable key name matching `AppUiShortcutKey` serialization.
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

/// Project path selected from the app UI recent-project startup list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppShellOpenRecentProjectPayload {
    /// Mondrian project file to open.
    pub project_file: PathBuf,
}

/// Project autosave candidate selected from the app UI startup surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecoverFromAutosavePayload {
    /// Exact discovery evidence selected by the user.
    pub candidate: CrashRecoveryCandidate,
}

/// File-system path to reveal through the native file manager.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppShellRevealInFileManagerPayload {
    /// File or directory to reveal.
    pub path: PathBuf,
}

/// Asset selected for a native relink file dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppShellRelinkAssetDialogPayload {
    /// Asset whose media path should be replaced.
    pub asset_id: AssetId,
}

/// Dock drop region selected by the app UI workspace shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockDropAreaPayload {
    /// Add as a tab in the target panel group.
    Center,
    /// Split to the left of the target panel group.
    Left,
    /// Split to the right of the target panel group.
    Right,
    /// Split above the target panel group.
    Top,
    /// Split below the target panel group.
    Bottom,
}

/// Move an existing dock panel relative to another panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppShellRelocatePanelPayload {
    /// Panel tab being moved.
    pub panel: PanelKind,
    /// Existing panel whose group receives or anchors the drop.
    pub target: PanelKind,
    /// Region selected inside the target panel group.
    pub area: DockDropAreaPayload,
    /// Optional tab insertion index when the drop target is a tab bar.
    ///
    /// `None` keeps the target-panel default, usually after the active tab.
    #[serde(default)]
    pub tab_index: Option<usize>,
}

/// Select one visual Transition in the active Sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSelectVideoTransitionPayload {
    /// Stable Transition identity.
    pub transition_id: VideoTransitionId,
}

/// Create the product-default Cross Dissolve between an adjacent edit pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCreateCrossDissolvePayload {
    /// Clip ending at the edit.
    pub left_clip_id: ClipId,
    /// Clip beginning at the edit.
    pub right_clip_id: ClipId,
}

/// Change one visual Transition range on the Sequence video grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSetVideoTransitionRangePayload {
    /// Stable Transition identity.
    pub transition_id: VideoTransitionId,
    /// Inclusive range start in Sequence evaluation frames.
    pub start_frame: i64,
    /// Exclusive range end in Sequence evaluation frames.
    pub end_frame: i64,
}

/// Trim the current timeline clip selection to the playhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineTrimSelectedClipsToPlayheadPayload {
    /// Edge that should be trimmed.
    pub edge: TimelineTrimPayloadEdge,
}

/// Timeline range point edited by a UI surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineInOutPointPayloadKind {
    In,
    Out,
}

/// Set one active-sequence in/out point to a target timeline frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSetInOutPointPayload {
    /// In or out point being changed.
    pub point: TimelineInOutPointPayloadKind,
    /// Target timeline frame.
    pub frame: i64,
}

/// Toggle the enabled state for the current timeline clip selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSetSelectedClipsEnabledPayload {
    /// `true` when selected clips should participate in rendering/playback.
    pub enabled: bool,
}

/// Replace the current clip selection with one nested Sequence placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelinePrecomposeSelectionPayload {
    /// User-facing name assigned to the new nested Sequence.
    pub name: String,
}

/// Open one nested sequence from a timeline clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineOpenNestedSequencePayload {
    /// Nested sequence to make active.
    pub sequence_id: SequenceId,
}

/// Target one project sequence from an app UI sequence menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceTargetPayload {
    /// Sequence to operate on.
    pub sequence_id: SequenceId,
}

/// Apply edited settings to one sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceUpdateSettingsPayload {
    /// Sequence whose name/settings should be replaced.
    pub sequence_id: SequenceId,
    /// User-facing sequence name.
    pub name: String,
    /// Full sequence settings after applying shell-local edits.
    pub settings: SequenceSettings,
}

/// Change the active viewer preview resolution scale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewerSetPreviewResolutionScalePayload {
    /// Preview resolution scale requested by the UI.
    pub scale: f32,
}

/// Change the shell-local viewer canvas zoom.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewerSetZoomScalePayload {
    /// Fixed canvas scale. `None` means fit to available viewer space.
    pub scale: Option<f32>,
}

/// Sequence-space position emitted by monitor direct manipulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewerTransformPositionPayload {
    /// Horizontal position in sequence pixels.
    pub x: f32,
    /// Vertical position in sequence pixels.
    pub y: f32,
}

/// Change one clip transform from the viewer/monitor surface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewerSetClipTransformPayload {
    /// Clip targeted by the monitor interaction.
    pub clip: InspectorClipRefPayload,
    /// Optional absolute sequence-space position.
    pub position: Option<ViewerTransformPositionPayload>,
    /// Optional uniform scale in UI percent units.
    pub scale_percent: Option<f32>,
    /// Optional rotation in degrees.
    pub rotation_degrees: Option<f32>,
}

/// Track control targeted by the timeline header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineTrackControlPayloadKind {
    /// Track visibility in video/compositing output.
    Visibility,
    /// Track muted state.
    Mute,
    /// Track locked state.
    Lock,
}

/// Set one track-level control from the timeline header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSetTrackControlPayload {
    /// Track targeted by the timeline header.
    pub track_id: TrackId,
    /// Whether `track_id` is a video track rather than an audio track.
    pub is_video_track: bool,
    /// Control being changed.
    pub control: TimelineTrackControlPayloadKind,
    /// New value for that control.
    pub enabled: bool,
}

/// Editor-session Track control targeted by the timeline header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineTrackTargetingControl {
    /// Whether content edits directly affect this Track.
    Target,
    /// Whether downstream placements follow ripple edits.
    SyncLock,
}

/// Change Track Targeting or Sync-Lock without an author transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSetTrackTargetingPayload {
    /// Stable Track identity in the active Sequence.
    pub track_id: TrackId,
    /// Session policy being changed.
    pub control: TimelineTrackTargetingControl,
    /// New enabled state.
    pub enabled: bool,
}

/// Track category for adding tracks from timeline UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineAddTrackKind {
    /// Add a video track.
    Video,
    /// Add an audio track.
    Audio,
}

/// Add a track to the active timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineAddTrackPayload {
    /// Track category to add.
    pub kind: TimelineAddTrackKind,
}

/// Move a track within its video or audio track list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMoveTrackPayload {
    /// Track being moved.
    pub track_id: TrackId,
    /// Whether `track_id` is a video track rather than an audio track.
    pub is_video_track: bool,
    /// Target index within the matching video/audio track list.
    pub target_index: usize,
}

/// Drop an asset onto one timeline track at a target frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineDropAssetPayload {
    /// Asset being dropped.
    pub asset_id: AssetId,
    /// Track that should receive the created clip.
    pub target_track_id: TrackId,
    /// Whether `target_track_id` is a video track rather than an audio track.
    pub is_video_track: bool,
    /// Target timeline frame for the new clip start.
    pub frame: i64,
}

/// Insert one Asset through an explicit professional edit scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineInsertAssetPayload {
    /// Asset whose selected source interval should be inserted.
    pub asset_id: AssetId,
    /// Insert boundary on the active Sequence video evaluation grid.
    pub insert_frame: i64,
    /// Source selection start on the active Sequence video evaluation grid.
    pub source_in_frame: i64,
    /// Positive selected source duration on the active Sequence video grid.
    pub duration_frames: i64,
    /// Target video Track, when this insertion contains picture.
    pub video_target_track_id: Option<TrackId>,
    /// Target audio Track, when this insertion contains audio.
    pub audio_target_track_id: Option<TrackId>,
    /// Exact Track set resolved from targeting and Sync-Lock UI state.
    pub ripple_track_ids: Vec<TrackId>,
    /// Sequence-time automation behavior.
    pub automation_policy: mondrian_timeline::InsertAutomationPolicy,
    /// Disposition for Transitions intersected by the edit.
    pub transition_policy: mondrian_timeline::InsertTransitionPolicy,
    /// Playhead and In/Out behavior.
    pub timeline_state_policy: mondrian_timeline::InsertTimelineStatePolicy,
}

/// Application-level identity for an inspector-selected clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorClipRefPayload {
    /// Track that owned the selected clip when the panel snapshot was built.
    pub track_id: TrackId,
    /// Whether `track_id` was a video track rather than an audio track.
    pub is_video_track: bool,
    /// Clip targeted by the inspector mutation.
    pub clip_id: ClipId,
}

/// Toggle the enabled state for a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorSetClipEnabledPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// `true` when the clip should participate in rendering/playback.
    pub enabled: bool,
}

/// Change a clip opacity value in UI percentage units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetClipOpacityPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Opacity in the same `0.0..=100.0` percentage range used by the slider.
    pub opacity_percent: f32,
}

/// Change a clip solid/tint color.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetClipTintPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// New color value.
    pub color: mondrian_core::Color,
}

/// Transform field exposed by the app UI inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InspectorClipTransformField {
    /// Horizontal position in sequence pixels.
    PositionX,
    /// Vertical position in sequence pixels.
    PositionY,
    /// Uniform scale displayed in percent units.
    ScalePercent,
    /// Rotation in degrees.
    RotationDegrees,
}

/// Change a single transform field on a selected clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetClipTransformFieldPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Transform field being changed.
    pub field: InspectorClipTransformField,
    /// New UI-space value for the field.
    pub value: f32,
}

/// One normalized point from the app UI curve editor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InspectorCurvePointPayload {
    /// Normalized x coordinate in the curve editor.
    pub x: f32,
    /// Normalized y coordinate in the curve editor.
    pub y: f32,
}

/// One incremental stable-key curve edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InspectorCurveEditPayload {
    /// Insert a key, or move/update the addressed key.
    Upsert {
        /// Existing stable key identity. `None` inserts or updates by time.
        keyframe_id: Option<KeyframeId>,
        /// New normalized time and value.
        point: InspectorCurvePointPayload,
    },
    /// Remove one existing key by stable identity.
    Remove {
        /// Stable key identity captured by the panel snapshot.
        keyframe_id: KeyframeId,
    },
}

/// Commit one selected Clip curve edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectorEditClipCurvePayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Stable property-instance and Parameter Schema identity.
    pub property: AnimationParameterAddress,
    /// One incremental point intent.
    pub edit: InspectorCurveEditPayload,
}

/// Change one definition-backed Clip property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetClipPropertyPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Current authoring address of the definition-backed parameter.
    pub path: String,
    /// New typed value.
    pub value: mondrian_core::automation::PropertyValue,
}

/// Logical source selected for one placement-local audio Component Edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InspectorAudioComponentSourcePayload {
    /// Stable audio Component owned by the media Clip's Asset.
    Media {
        /// Asset-owned logical Component identity.
        component_id: AudioSourceComponentId,
    },
    /// Stable public output owned by the nested Sequence.
    NestedOutput {
        /// Child Sequence output identity.
        output_id: ProgramOutputId,
    },
}

/// Change the logical source of one placement-local audio Component Edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorSetAudioComponentSourcePayload {
    /// Clip that owns the edit.
    pub clip: InspectorClipRefPayload,
    /// Stable edit being changed; source identity is not edit identity.
    pub edit_id: AudioComponentEditId,
    /// New logical source in the owning Clip's source domain.
    pub source: InspectorAudioComponentSourcePayload,
}

/// Select one effect instance inside a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorSelectEffectPayload {
    /// Clip that owns the effect.
    pub clip: InspectorClipRefPayload,
    /// Effect instance to make active in the inspector scope.
    pub effect_id: EffectId,
}

/// Toggle a clip effect enabled state from an inspector panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorSetEffectEnabledPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Effect instance being toggled.
    pub effect_id: EffectId,
    /// Whether the effect should participate in rendering.
    pub enabled: bool,
}

/// Remove an effect instance from a selected clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorRemoveEffectPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Effect instance to remove.
    pub effect_id: EffectId,
}

/// Change one effect property value from an inspector panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetEffectPropertyPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Effect instance being edited.
    pub effect_id: EffectId,
    /// Namespaced property path on the effect.
    pub path: String,
    /// New property value to set.
    pub value: mondrian_core::automation::PropertyValue,
}

/// Add an effect from the effect browser to a clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectsAddToClipPayload {
    /// Clip targeted by the effect insertion.
    pub clip: InspectorClipRefPayload,
    /// Effect type to instantiate with defaults.
    pub effect_type: EffectType,
}

/// Prepare one asset for the existing timeline drag/drop path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsPrepareDragPayload {
    /// Asset selected from the app UI asset browser.
    pub asset_id: AssetId,
}

/// Re-probe one Asset without retargeting any existing logical Component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsRefreshAudioComponentsPayload {
    /// Asset whose current file supplies fresh stream evidence.
    pub asset_id: AssetId,
}

/// Explicitly repair one Asset audio Component's physical stream binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsRebindAudioComponentPayload {
    /// Asset that owns the stable logical Component.
    pub asset_id: AssetId,
    /// Logical Component identity preserved by the operation.
    pub component_id: AudioSourceComponentId,
    /// Absolute stream index selected from current probe evidence.
    pub stream_index: u32,
}

/// Delete one asset-library record and any timeline clips that reference it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsDeleteAssetPayload {
    /// Asset to remove from the project library.
    pub asset_id: AssetId,
}

/// Delete one asset-library folder/bin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsDeleteFolderPayload {
    /// Folder id to remove.
    pub folder_id: String,
}

/// Delete multiple asset-library items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsDeleteSelectionPayload {
    /// Asset ids to delete.
    pub asset_ids: Vec<AssetId>,
    /// Folder ids to delete.
    pub folder_ids: Vec<String>,
}

/// Move one asset-library item into a folder, or to the root view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsMoveAssetPayload {
    /// Asset id to move.
    pub asset_id: AssetId,
    /// Destination folder. `None` moves to the root/unfiled view.
    pub folder_id: Option<String>,
}

/// Move one asset-library folder/bin under another folder, or to the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsMoveFolderPayload {
    /// Folder id to move.
    pub folder_id: String,
    /// Destination parent folder. `None` moves to the root level.
    pub parent_folder_id: Option<String>,
}

/// Move multiple asset-library items into a folder, or to the root view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsMoveSelectionPayload {
    /// Asset ids to move.
    pub asset_ids: Vec<AssetId>,
    /// Folder ids to reparent.
    pub folder_ids: Vec<String>,
    /// Destination folder/parent. `None` moves to the root level.
    pub target_folder_id: Option<String>,
}

/// Create a synthetic reusable asset in the selected folder, or at root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsCreateAssetPayload {
    /// Target folder for the new asset. `None` creates it in the root/unfiled view.
    pub folder_id: Option<String>,
}

/// Open one folder in the app UI asset browser, or the root view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsOpenFolderPayload {
    /// Folder to show. `None` returns to the root/unfiled asset view.
    pub folder_id: Option<String>,
}

/// Create an asset-library folder in the selected parent, or at root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsCreateFolderPayload {
    /// Parent folder for the new folder. `None` creates a root-level folder.
    pub parent_folder_id: Option<String>,
}

/// Import media files into an asset-library folder, or into the root view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsImportFilesPayload {
    /// Media file paths selected by the user or dropped onto the asset browser.
    pub paths: Vec<PathBuf>,
    /// Target folder for imported assets. `None` imports into the root/unfiled view.
    pub folder_id: Option<String>,
}

/// Relink one asset-library record to a replacement file selected by the shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsRelinkAssetPayload {
    /// Asset to relink.
    pub asset_id: AssetId,
    /// Replacement media path.
    pub path: PathBuf,
}

/// Persist one asset-library record's media interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsSetInterpretationPayload {
    /// Asset to update.
    pub asset_id: AssetId,
    /// Persistent user intent to store on the asset.
    pub interpretation: AssetMediaInterpretation,
}

/// Rename one asset-library record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsRenameAssetPayload {
    /// Asset to rename.
    pub asset_id: AssetId,
    /// New user-facing asset name.
    pub name: String,
}

/// Rename one asset-library folder/bin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsRenameFolderPayload {
    /// Folder to rename.
    pub folder_id: String,
    /// New user-facing folder name.
    pub name: String,
}

/// Enable or disable proxy playback for one video asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsSetProxyModePayload {
    /// Video asset to update.
    pub asset_id: AssetId,
    /// Whether timeline playback should prefer a generated proxy.
    pub enabled: bool,
}

/// Platform file-dialog target for importing media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportMediaDialogPayload {
    /// Target folder for selected media. `None` imports into the root/unfiled view.
    pub folder_id: Option<String>,
}

/// Shell-local target for the Interpret Footage dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppShellInterpretAssetDialogPayload {
    /// Asset being interpreted.
    pub asset_id: AssetId,
    /// User-facing asset name captured from the asset grid model.
    pub asset_name: String,
    /// Current persistent interpretation to seed the dialog draft.
    pub interpretation: AssetMediaInterpretation,
    /// Current structured automatic color interpretation from media metadata.
    pub auto_interpretation: Option<DetectedColorInterpretation>,
    /// Raw primary-video signal metadata retained for user-visible diagnostics.
    pub video_signal: Option<AppShellVideoSignalDiagnostics>,
    /// Effective project/sequence input pipeline used to identify the OCIO processor.
    pub input_pipeline: Option<AppShellInputColorPipelineDiagnostics>,
}

/// Raw encoded-signal facts shown by Interpret Footage without re-probing media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppShellVideoSignalDiagnostics {
    /// Decoder-proven full/limited range, or explicit unknown.
    pub range: DecodedVideoRange,
    /// Probe-proven physical sampling family, bit depth, and Alpha presence.
    pub sampling: Option<ProvenVideoSampling>,
    /// Raw CICP primaries/transfer/matrix triplet when FFmpeg exposed it.
    pub color_metadata: Option<VideoColorMetadata>,
    /// Raw container/stream/file-name hints captured by the same media probe.
    #[serde(default)]
    pub color_metadata_hints: Vec<VideoColorMetadataHint>,
}

/// Effective input-to-working identities resolved before opening Interpret Footage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppShellInputColorPipelineDiagnostics {
    /// Exact immutable/config-pinned OCIO engine selected by the project or sequence.
    pub engine: ColorEngine,
    /// Scene-linear working identity receiving this asset.
    pub working_color_space: WorkingColorSpace,
}

/// Draft update emitted by the Interpret Footage dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterpretAssetDraftUpdatePayload {
    /// New draft interpretation.
    pub interpretation: AssetMediaInterpretation,
}

/// Enqueue a timeline export job from a UI frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEnqueuePayload {
    /// Preset used for codec/container defaults.
    pub preset: ExportPreset,
    /// Sequence to export; absent means the active sequence.
    pub sequence_id: Option<SequenceId>,
    /// Timeline range to render.
    pub range: TimelineExportRange,
    /// Output media file path.
    pub output_path: PathBuf,
    /// Final namespace policy. Missing legacy payloads default to create-only.
    #[serde(default)]
    pub output_policy: mondrian_export::preset::ExportOutputPolicy,
}

/// Target one export queue job from a UI frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportJobTargetPayload {
    /// Render queue job id.
    pub job_id: JobId,
}

/// Update one field of the app UI export draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExportDraftUpdatePayload {
    /// Reset the materialized draft from a stable built-in preset.
    BuiltinPreset(mondrian_export::preset::BuiltinExportPreset),
    /// Replace the complete typed delivery draft after one product form edit.
    Preset(ExportPreset),
    /// Select the sequence to export.
    Sequence(Option<SequenceId>),
    /// Select the timeline range to render.
    Range(TimelineExportRange),
    /// Replace the output path text.
    OutputPath(String),
}

/// Platform save-dialog defaults for choosing an export output file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportOutputDialogPayload {
    /// Suggested file name shown by the native save dialog.
    pub default_file_name: String,
    /// Preferred container extension without a leading dot.
    pub extension: String,
}

/// Create a project at a user-selected path with explicit initial settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCreateWithSettingsPayload {
    /// Target `.mdp` project container path.
    pub project_file: PathBuf,
    /// Initial project and sequence display name.
    pub name: String,
    /// Initial sequence settings.
    pub sequence_settings: SequenceSettings,
    /// Project-wide color engine shared by every Sequence.
    pub color_environment: ProjectColorEnvironment,
    /// Initial project-level settings.
    pub project_settings: ProjectSettings,
}

/// Replace the complete template copied into future Sequences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectUpdateNewSequenceDefaultsPayload {
    /// Complete validated Sequence settings template.
    pub settings: SequenceSettings,
}

/// Atomically replace the Project-wide color engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectUpdateColorEnvironmentPayload {
    /// Complete version-pinned engine environment.
    pub color_environment: ProjectColorEnvironment,
}

/// One mutation to the shell-local new-project draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NewProjectDraftUpdatePayload {
    /// Project and initial sequence display name.
    Name(String),
    /// Initial sequence frame size.
    Resolution(Resolution),
    /// Initial sequence frame rate.
    FrameRate(Rational),
    /// Initial sequence audio sample rate in Hz.
    AudioSampleRate(u32),
    /// Complete project color engine; Custom OCIO values are already pinned.
    ColorEngine(ColorEngine),
    /// Whether project proxy generation is enabled.
    ProxyEnabled(bool),
    /// Whether preview rendering cache is enabled.
    PreviewCacheEnabled(bool),
}

/// One mutation to the shell-local project color-settings draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectSettingsDraftUpdatePayload {
    /// Complete project color engine; Custom OCIO values are already pinned.
    ColorEngine(ColorEngine),
}

/// One mutation to the shell-local sequence-settings draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SequenceSettingsDraftUpdatePayload {
    /// User-facing sequence display name.
    Name(String),
    /// Sequence editing preset/mode.
    EditingMode(EditingMode),
    /// Active sequence frame size.
    Resolution(Resolution),
    /// Active sequence frame width in pixels.
    ResolutionWidth(u32),
    /// Active sequence frame height in pixels.
    ResolutionHeight(u32),
    /// Active sequence frame rate.
    FrameRate(Rational),
    /// Active sequence pixel aspect ratio.
    PixelAspectRatio(PixelAspectRatio),
    /// Active sequence field order.
    FieldOrder(FieldOrder),
    /// Sequence position-display format.
    TimelineDisplayFormat(TimelineDisplayFormat),
    /// Signed actual-frame offset used as the Sequence timecode origin.
    StartTimecodeFrame(i64),
    /// Sequence working color space.
    WorkingColorSpace(WorkingColorSpace),
    /// Whether source media is auto tone-mapped into the sequence.
    AutoToneMapMedia(bool),
    /// Sequence color workflow.
    ColorWorkflow(ColorWorkflow),
    /// Policy for media with missing color metadata.
    MissingColorMetadataPolicy(MissingColorMetadataPolicy),
    /// Program-output tone-map policy.
    OutputToneMapPolicy(DisplayToneMapPolicy),
    /// Output color space for sequence rendering/export.
    OutputColorSpace(ColorSpace),
    /// Video range used by the sequence output.
    VideoRange(VideoRange),
    /// Export bit depth preference.
    DeliveryBitDepth(DeliveryBitDepth),
    /// Whether explicitly authored static HDR metadata should be written.
    WriteStaticHdrMetadata(bool),
    /// Active sequence audio sample rate in Hz.
    AudioSampleRate(u32),
    /// Active sequence audio channel layout.
    AudioChannelLayout(AudioChannelLayout),
    /// Active sequence audio display format.
    AudioDisplayFormat(AudioDisplayFormat),
    /// Active sequence preview render format.
    PreviewRenderFormat(PreviewRenderFormat),
    /// Preview resolution scale, from 0.125 to 1.0.
    PreviewResolutionScale(f32),
    /// Whether preview rendering cache is enabled.
    PreviewCacheEnabled(bool),
}

/// Section selected in the app UI sequence settings dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceSettingsTabPayload {
    /// Format, frame timing, and audio settings.
    Format,
    /// Color management settings.
    Color,
    /// Preview render/cache settings.
    Preview,
}

/// Build an action that selects a clip in the active timeline.
pub fn timeline_select_clip_action(payload: TimelineSelectClipPayload) -> Action {
    ProductAction::Timeline(TimelineProductAction::SelectClip(payload)).into_external_action()
}

/// Build one atomic Sequence Audio Processor Rack authoring action.
pub fn audio_processor_rack_edit_action(request: AudioProcessorRackEditRequest) -> Action {
    ProductAction::Audio(AudioProductAction::EditProcessorRack(request)).into_external_action()
}

/// Encode one stable-address exact audio automation edit for Widget dispatch.
pub fn audio_automation_edit_action(
    request: mondrian_timeline::AudioAutomationEditRequest,
) -> Action {
    ProductAction::Audio(AudioProductAction::EditAutomation(request)).into_external_action()
}

/// Build one atomic placement-local Audio Component authoring action.
pub fn audio_component_edit_action(
    request: mondrian_timeline::AudioComponentEditRequest,
) -> Action {
    ProductAction::Audio(AudioProductAction::EditComponent(request)).into_external_action()
}

/// Build an action that inserts one canonical product-visible built-in Processor.
pub fn audio_processor_insert_built_in_action(
    payload: super::product_action::AudioProcessorInsertBuiltInPayload,
) -> Action {
    ProductAction::Audio(AudioProductAction::InsertBuiltInProcessor(payload)).into_external_action()
}

/// Build one atomic normative Audio Channel Strip authoring action.
pub fn audio_channel_strip_edit_action(request: AudioChannelStripEditRequest) -> Action {
    ProductAction::Audio(AudioProductAction::EditChannelStrip(request)).into_external_action()
}

/// Build one atomic Sequence Audio Routing graph authoring action.
pub fn audio_routing_edit_action(request: AudioRoutingEditRequest) -> Action {
    ProductAction::Audio(AudioProductAction::EditRouting(request)).into_external_action()
}

/// Build one open-Session Track audition action.
pub fn audio_track_solo_action(payload: super::product_action::AudioTrackSoloPayload) -> Action {
    ProductAction::Audio(AudioProductAction::SetTrackSolo(payload)).into_external_action()
}

/// Build an action that links the current Clip selection.
pub fn timeline_link_selected_clips_action() -> Action {
    custom_timeline_action(TIMELINE_LINK_SELECTED_CLIPS, ())
}

/// Build an action that unlinks the current Clip selection.
pub fn timeline_unlink_selected_clips_action() -> Action {
    custom_timeline_action(TIMELINE_UNLINK_SELECTED_CLIPS, ())
}

/// Build an action that selects a visual Transition.
pub fn timeline_select_video_transition_action(
    payload: TimelineSelectVideoTransitionPayload,
) -> Action {
    custom_timeline_action(TIMELINE_SELECT_VIDEO_TRANSITION, payload)
}

/// Build an action that creates the product-default Cross Dissolve at an edit.
pub fn timeline_create_cross_dissolve_action(
    payload: TimelineCreateCrossDissolvePayload,
) -> Action {
    custom_timeline_action(TIMELINE_CREATE_CROSS_DISSOLVE, payload)
}

/// Build an action that creates a generated Basic Title.
pub fn timeline_create_basic_title_action() -> Action {
    custom_timeline_action(TIMELINE_CREATE_BASIC_TITLE, ())
}

/// Build an action that changes one visual Transition range.
pub fn timeline_set_video_transition_range_action(
    payload: TimelineSetVideoTransitionRangePayload,
) -> Action {
    custom_timeline_action(TIMELINE_SET_VIDEO_TRANSITION_RANGE, payload)
}

/// Build an action that moves a clip in the active timeline.
pub fn timeline_move_clip_action(payload: TimelineMoveClipPayload) -> Action {
    ProductAction::Timeline(TimelineProductAction::MoveClip(payload)).into_external_action()
}

/// Build an action that trims clip edges in the active timeline.
pub fn timeline_trim_clips_action(payload: TimelineTrimClipsPayload) -> Action {
    ProductAction::Timeline(TimelineProductAction::TrimClips(payload)).into_external_action()
}

/// Build an action that trims the current clip selection to the playhead.
pub fn timeline_trim_selected_clips_to_playhead_action(
    payload: TimelineTrimSelectedClipsToPlayheadPayload,
) -> Action {
    custom_timeline_action(TIMELINE_TRIM_SELECTED_CLIPS_TO_PLAYHEAD, payload)
}

/// Build an action that rolls the selected cut to the playhead.
pub fn timeline_roll_selected_cut_to_playhead_action() -> Action {
    custom_timeline_action(TIMELINE_ROLL_SELECTED_CUT_TO_PLAYHEAD, ())
}

/// Build an action that sets one active-sequence in/out point.
pub fn timeline_set_in_out_point_action(payload: TimelineSetInOutPointPayload) -> Action {
    custom_timeline_action(TIMELINE_SET_IN_OUT_POINT, payload)
}

/// Build an action that clears the active sequence in/out points.
pub fn timeline_clear_in_out_points_action() -> Action {
    custom_timeline_action(TIMELINE_CLEAR_IN_OUT_POINTS, ())
}

/// Build an action that lifts the active In/Out range.
pub fn timeline_lift_range_action() -> Action {
    custom_timeline_action(TIMELINE_LIFT_RANGE, ())
}

/// Build an action that extracts the active In/Out range.
pub fn timeline_extract_range_action() -> Action {
    custom_timeline_action(TIMELINE_EXTRACT_RANGE, ())
}

/// Build an action that toggles the current timeline clip selection.
pub fn timeline_set_selected_clips_enabled_action(
    payload: TimelineSetSelectedClipsEnabledPayload,
) -> Action {
    custom_timeline_action(TIMELINE_SET_SELECTED_CLIPS_ENABLED, payload)
}

/// Build an action that seeks the active timeline.
pub fn timeline_seek_action(frame: i64) -> Action {
    timeline_seek_with_source_action(frame, TimelineSeekSource::Settled)
}

/// Build an action that seeks the active timeline with explicit interaction source.
pub fn timeline_seek_with_source_action(frame: i64, source: TimelineSeekSource) -> Action {
    ProductAction::Timeline(TimelineProductAction::Seek(TimelineSeekPayload {
        frame,
        source,
    }))
    .into_external_action()
}

/// Build an action that changes a timeline track header control.
pub fn timeline_set_track_control_action(payload: TimelineSetTrackControlPayload) -> Action {
    custom_timeline_action(TIMELINE_SET_TRACK_CONTROL, payload)
}

/// Build an action that changes Track Targeting or Sync-Lock session policy.
pub fn timeline_set_track_targeting_action(payload: TimelineSetTrackTargetingPayload) -> Action {
    custom_timeline_action(TIMELINE_SET_TRACK_TARGETING, payload)
}

/// Build an action that adds a timeline track.
pub fn timeline_add_track_action(payload: TimelineAddTrackPayload) -> Action {
    custom_timeline_action(TIMELINE_ADD_TRACK, payload)
}

/// Build an action that moves a timeline track.
pub fn timeline_move_track_action(payload: TimelineMoveTrackPayload) -> Action {
    custom_timeline_action(TIMELINE_MOVE_TRACK, payload)
}

/// Build an action that drops an asset onto a timeline track.
pub fn timeline_drop_asset_action(payload: TimelineDropAssetPayload) -> Action {
    custom_timeline_action(TIMELINE_DROP_ASSET, payload)
}

/// Build an action that performs one professional Insert Edit from an Asset.
pub fn timeline_insert_asset_action(payload: TimelineInsertAssetPayload) -> Action {
    custom_timeline_action(TIMELINE_INSERT_ASSET, payload)
}

/// Build an action that precomposes the current clip selection.
pub fn timeline_precompose_selection_action(payload: TimelinePrecomposeSelectionPayload) -> Action {
    custom_timeline_action(TIMELINE_PRECOMPOSE_SELECTION, payload)
}

/// Build an action that opens one nested sequence from the timeline.
pub fn timeline_open_nested_sequence_action(payload: TimelineOpenNestedSequencePayload) -> Action {
    custom_timeline_action(TIMELINE_OPEN_NESTED_SEQUENCE, payload)
}

/// Build an action that toggles a clip enabled state from an inspector panel.
pub fn inspector_set_clip_enabled_action(payload: InspectorSetClipEnabledPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_ENABLED, payload)
}

/// Build an action that changes clip opacity from an inspector panel.
pub fn inspector_set_clip_opacity_action(payload: InspectorSetClipOpacityPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_OPACITY, payload)
}

/// Build an action that changes clip solid/tint color from an inspector panel.
pub fn inspector_set_clip_tint_action(payload: InspectorSetClipTintPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_TINT, payload)
}

/// Build an action that changes a clip transform field from an inspector panel.
pub fn inspector_set_clip_transform_field_action(
    payload: InspectorSetClipTransformFieldPayload,
) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_TRANSFORM_FIELD, payload)
}

/// Build an action that commits one Clip curve edit from an inspector panel.
pub fn inspector_edit_clip_curve_action(payload: InspectorEditClipCurvePayload) -> Action {
    custom_inspector_action(INSPECTOR_EDIT_CLIP_CURVE, payload)
}

/// Build an action that changes one definition-backed Clip property.
pub fn inspector_set_clip_property_action(payload: InspectorSetClipPropertyPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_PROPERTY, payload)
}

/// Build an action that changes one Clip audio Component Edit's logical source.
pub fn inspector_set_audio_component_source_action(
    payload: InspectorSetAudioComponentSourcePayload,
) -> Action {
    custom_inspector_action(INSPECTOR_SET_AUDIO_COMPONENT_SOURCE, payload)
}

/// Build an action that selects an effect in the inspector scope.
pub fn inspector_select_effect_action(payload: InspectorSelectEffectPayload) -> Action {
    custom_inspector_action(INSPECTOR_SELECT_EFFECT, payload)
}

/// Build an action that toggles an effect on a selected clip.
pub fn inspector_set_effect_enabled_action(payload: InspectorSetEffectEnabledPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_EFFECT_ENABLED, payload)
}

/// Build an action that removes an effect from a selected clip.
pub fn inspector_remove_effect_action(payload: InspectorRemoveEffectPayload) -> Action {
    custom_inspector_action(INSPECTOR_REMOVE_EFFECT, payload)
}

/// Build an action that changes one effect property value.
pub fn inspector_set_effect_property_action(payload: InspectorSetEffectPropertyPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_EFFECT_PROPERTY, payload)
}

/// Build an action that adds an effect to a selected clip.
pub fn effects_add_to_clip_action(payload: EffectsAddToClipPayload) -> Action {
    custom_effects_action(EFFECTS_ADD_TO_CLIP, payload)
}

/// Build an action that prepares an asset for timeline drag/drop.
pub fn assets_prepare_drag_action(payload: AssetsPrepareDragPayload) -> Action {
    custom_assets_action(ASSETS_PREPARE_DRAG, payload)
}

/// Build an action that refreshes one Asset's audio stream candidates.
pub fn assets_refresh_audio_components_action(
    payload: AssetsRefreshAudioComponentsPayload,
) -> Action {
    custom_assets_action(ASSETS_REFRESH_AUDIO_COMPONENTS, payload)
}

/// Build an action that explicitly repairs an Asset audio Component binding.
pub fn assets_rebind_audio_component_action(payload: AssetsRebindAudioComponentPayload) -> Action {
    custom_assets_action(ASSETS_REBIND_AUDIO_COMPONENT, payload)
}

/// Build an action that deletes one asset from the library.
pub fn assets_delete_asset_action(payload: AssetsDeleteAssetPayload) -> Action {
    custom_assets_action(ASSETS_DELETE_ASSET, payload)
}

/// Build an action that deletes one folder from the library.
pub fn assets_delete_folder_action(payload: AssetsDeleteFolderPayload) -> Action {
    custom_assets_action(ASSETS_DELETE_FOLDER, payload)
}

pub fn assets_delete_selection_action(payload: AssetsDeleteSelectionPayload) -> Action {
    custom_assets_action(ASSETS_DELETE_SELECTION, payload)
}

/// Build an action that moves one asset to another folder.
pub fn assets_move_asset_action(payload: AssetsMoveAssetPayload) -> Action {
    custom_assets_action(ASSETS_MOVE_ASSET, payload)
}

/// Build an action that moves one folder to another parent.
pub fn assets_move_folder_action(payload: AssetsMoveFolderPayload) -> Action {
    custom_assets_action(ASSETS_MOVE_FOLDER, payload)
}

pub fn assets_move_selection_action(payload: AssetsMoveSelectionPayload) -> Action {
    custom_assets_action(ASSETS_MOVE_SELECTION, payload)
}

/// Build an action that creates an adjustment-layer asset in the library.
pub fn assets_create_adjustment_layer_action(payload: AssetsCreateAssetPayload) -> Action {
    custom_assets_action(ASSETS_CREATE_ADJUSTMENT_LAYER, payload)
}

/// Build an action that creates a solid-color asset in the library.
pub fn assets_create_solid_color_action(payload: AssetsCreateAssetPayload) -> Action {
    custom_assets_action(ASSETS_CREATE_SOLID_COLOR, payload)
}

/// Build an action that creates a folder in the library.
pub fn assets_create_folder_action(payload: AssetsCreateFolderPayload) -> Action {
    custom_assets_action(ASSETS_CREATE_FOLDER, payload)
}

/// Build an action that imports media files into an asset-library folder.
pub fn assets_import_files_action(payload: AssetsImportFilesPayload) -> Action {
    custom_assets_action(ASSETS_IMPORT_FILES, payload)
}

/// Build an action that relinks one asset to a replacement file.
pub fn assets_relink_asset_action(payload: AssetsRelinkAssetPayload) -> Action {
    custom_assets_action(ASSETS_RELINK_ASSET, payload)
}

/// Build an action that persists one asset's media interpretation.
pub fn assets_set_interpretation_action(payload: AssetsSetInterpretationPayload) -> Action {
    custom_assets_action(ASSETS_SET_INTERPRETATION, payload)
}

/// Build an action that renames one asset.
pub fn assets_rename_asset_action(payload: AssetsRenameAssetPayload) -> Action {
    custom_assets_action(ASSETS_RENAME_ASSET, payload)
}

/// Build an action that renames one folder.
pub fn assets_rename_folder_action(payload: AssetsRenameFolderPayload) -> Action {
    custom_assets_action(ASSETS_RENAME_FOLDER, payload)
}

/// Build an action that toggles proxy playback for one video asset.
pub fn assets_set_proxy_mode_action(payload: AssetsSetProxyModePayload) -> Action {
    custom_assets_action(ASSETS_SET_PROXY_MODE, payload)
}

/// Build a shell-local action that opens an asset-browser folder.
pub fn assets_open_folder_action(payload: AssetsOpenFolderPayload) -> Action {
    custom_assets_action(ASSETS_OPEN_FOLDER, payload)
}

/// Build an action that enqueues a timeline export.
pub fn export_enqueue_action(payload: ExportEnqueuePayload) -> Action {
    custom_export_action(EXPORT_ENQUEUE, payload)
}

/// Build an action that updates one export draft field.
pub fn export_set_draft_action(payload: ExportDraftUpdatePayload) -> Action {
    custom_export_action(EXPORT_SET_DRAFT, payload)
}

/// Build an action that cancels one export queue job.
pub fn export_cancel_job_action(payload: ExportJobTargetPayload) -> Action {
    custom_export_action(EXPORT_CANCEL_JOB, payload)
}

/// Build an action that clears completed export queue jobs.
pub fn export_clear_completed_action() -> Action {
    custom_export_action(EXPORT_CLEAR_COMPLETED, ())
}

/// Build a viewer request for changing preview resolution scale.
pub fn viewer_set_preview_resolution_scale_action(
    payload: ViewerSetPreviewResolutionScalePayload,
) -> Action {
    custom_viewer_action(VIEWER_SET_PREVIEW_RESOLUTION_SCALE, payload)
}

/// Build a viewer request for changing one selected clip transform.
pub fn viewer_set_clip_transform_action(payload: ViewerSetClipTransformPayload) -> Action {
    custom_viewer_action(VIEWER_SET_CLIP_TRANSFORM, payload)
}

/// Build a shell-local viewer request for cycling canvas zoom.
pub fn viewer_cycle_zoom_action() -> Action {
    custom_viewer_action(VIEWER_CYCLE_ZOOM, ())
}

/// Build a shell-local viewer request for setting canvas zoom.
pub fn viewer_set_zoom_scale_action(payload: ViewerSetZoomScalePayload) -> Action {
    custom_viewer_action(VIEWER_SET_ZOOM_SCALE, payload)
}

/// Build an action that creates a project from shell UI.
pub fn project_create_with_settings_action(payload: ProjectCreateWithSettingsPayload) -> Action {
    custom_project_action(PROJECT_CREATE_WITH_SETTINGS, payload)
}

/// Build a project action that atomically replaces new-Sequence defaults.
pub fn project_update_new_sequence_defaults_action(
    payload: ProjectUpdateNewSequenceDefaultsPayload,
) -> Action {
    custom_project_action(PROJECT_UPDATE_NEW_SEQUENCE_DEFAULTS, payload)
}

/// Build a project action that atomically replaces the shared color engine.
pub fn project_update_color_environment_action(
    payload: ProjectUpdateColorEnvironmentPayload,
) -> Action {
    custom_project_action(PROJECT_UPDATE_COLOR_ENVIRONMENT, payload)
}

/// Build an action that recovers a project from an autosave snapshot.
pub fn project_recover_from_autosave_action(payload: ProjectRecoverFromAutosavePayload) -> Action {
    custom_project_action(PROJECT_RECOVER_FROM_AUTOSAVE, payload)
}

/// Build an action that returns from a nested sequence to its parent.
pub fn sequence_return_to_parent_action() -> Action {
    custom_sequence_action(SEQUENCE_RETURN_TO_PARENT)
}

/// Build an action that makes the current active sequence the project default.
pub fn sequence_set_active_default_action() -> Action {
    custom_sequence_action(SEQUENCE_SET_ACTIVE_DEFAULT)
}

/// Build an action that creates a new sequence with product defaults.
pub fn sequence_new_action() -> Action {
    custom_sequence_action(SEQUENCE_NEW)
}

/// Build an action that switches the active sequence.
pub fn sequence_switch_active_action(payload: SequenceTargetPayload) -> Action {
    custom_sequence_action_with_payload(SEQUENCE_SWITCH_ACTIVE, payload)
}

/// Build an action that duplicates one sequence.
pub fn sequence_duplicate_action(payload: SequenceTargetPayload) -> Action {
    custom_sequence_action_with_payload(SEQUENCE_DUPLICATE, payload)
}

/// Build an action that deletes one sequence.
pub fn sequence_delete_action(payload: SequenceTargetPayload) -> Action {
    custom_sequence_action_with_payload(SEQUENCE_DELETE, payload)
}

/// Build an action that applies edited settings to one sequence.
pub fn sequence_update_settings_action(payload: SequenceUpdateSettingsPayload) -> Action {
    custom_sequence_action_with_payload(SEQUENCE_UPDATE_SETTINGS, payload)
}

/// Build an app-shell request for creating a new project.
pub fn app_shell_new_project_dialog_action() -> Action {
    custom_app_shell_action(APP_SHELL_NEW_PROJECT_DIALOG)
}

/// Build an app-shell request for changing one new-project draft setting.
pub fn app_shell_new_project_draft_changed_action(payload: NewProjectDraftUpdatePayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_NEW_PROJECT_DRAFT_CHANGED, payload)
}

/// Build an app-shell request for selecting a Custom OCIO config.
pub fn app_shell_select_custom_ocio_config_action() -> Action {
    custom_app_shell_action(APP_SHELL_SELECT_CUSTOM_OCIO_CONFIG)
}

/// Build an app-shell request for project-level color settings.
pub fn app_shell_project_settings_action() -> Action {
    custom_app_shell_action(APP_SHELL_PROJECT_SETTINGS)
}

/// Build an app-shell request for changing the project-settings draft.
pub fn app_shell_project_settings_draft_changed_action(
    payload: ProjectSettingsDraftUpdatePayload,
) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_PROJECT_SETTINGS_DRAFT_CHANGED, payload)
}

/// Build an app-shell request for committing project-level color settings.
pub fn app_shell_confirm_project_settings_action() -> Action {
    custom_app_shell_action(APP_SHELL_CONFIRM_PROJECT_SETTINGS)
}

/// Build an app-shell request for confirming the new-project dialog.
pub fn app_shell_confirm_new_project_dialog_action() -> Action {
    custom_app_shell_action(APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG)
}

/// Build an app-shell request for canceling the new-project dialog.
pub fn app_shell_cancel_new_project_dialog_action() -> Action {
    custom_app_shell_action(APP_SHELL_CANCEL_NEW_PROJECT_DIALOG)
}

/// Build an app-shell request for opening a project dialog.
pub fn app_shell_open_project_dialog_action() -> Action {
    custom_app_shell_action(APP_SHELL_OPEN_PROJECT_DIALOG)
}

/// Build an app-shell request for opening one recent project path.
pub fn app_shell_open_recent_project_action(payload: AppShellOpenRecentProjectPayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_OPEN_RECENT_PROJECT, payload)
}

/// Build an app-shell request for recovering one startup autosave candidate.
pub fn app_shell_recover_project_action(payload: ProjectRecoverFromAutosavePayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_RECOVER_PROJECT, payload)
}

/// Build an app-shell request for importing media files.
pub fn app_shell_import_media_dialog_action() -> Action {
    app_shell_import_media_dialog_action_with_target(ImportMediaDialogPayload { folder_id: None })
}

/// Build an app-shell request for importing media files into a target folder.
pub fn app_shell_import_media_dialog_action_with_target(
    payload: ImportMediaDialogPayload,
) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_IMPORT_MEDIA_DIALOG, payload)
}

/// Build an app-shell request for revealing a file in the native file manager.
pub fn app_shell_reveal_in_file_manager_action(
    payload: AppShellRevealInFileManagerPayload,
) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_REVEAL_IN_FILE_MANAGER, payload)
}

/// Build an app-shell request for choosing a replacement file for one asset.
pub fn app_shell_relink_asset_dialog_action(payload: AppShellRelinkAssetDialogPayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_RELINK_ASSET_DIALOG, payload)
}

/// Build an app-shell request for editing one asset's media interpretation.
pub fn app_shell_interpret_asset_dialog_action(
    payload: AppShellInterpretAssetDialogPayload,
) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_INTERPRET_ASSET_DIALOG, payload)
}

/// Build an app-shell request for changing the Interpret Footage draft.
pub fn app_shell_interpret_asset_draft_changed_action(
    payload: InterpretAssetDraftUpdatePayload,
) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_INTERPRET_ASSET_DRAFT_CHANGED, payload)
}

/// Build an app-shell request for applying the Interpret Footage draft.
pub fn app_shell_confirm_interpret_asset_dialog_action() -> Action {
    custom_app_shell_action(APP_SHELL_CONFIRM_INTERPRET_ASSET_DIALOG)
}

/// Build an app-shell request for saving the current project to a chosen path.
pub fn app_shell_save_project_as_dialog_action() -> Action {
    custom_app_shell_action(APP_SHELL_SAVE_PROJECT_AS_DIALOG)
}

/// Build an app-shell request for choosing an export output file.
pub fn app_shell_export_output_dialog_action(payload: ExportOutputDialogPayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_EXPORT_OUTPUT_DIALOG, payload)
}

/// Build an app-shell request for showing product about information.
pub fn app_shell_about_action() -> Action {
    custom_app_shell_action(APP_SHELL_ABOUT)
}

/// Build an app-shell request for showing preferences.
pub fn app_shell_preferences_action() -> Action {
    custom_app_shell_action(APP_SHELL_PREFERENCES)
}

/// Build an app-shell request for editing the active sequence settings.
pub fn app_shell_sequence_settings_action() -> Action {
    custom_app_shell_action(APP_SHELL_SEQUENCE_SETTINGS)
}

/// Build an app-shell request for changing one sequence-settings draft field.
pub fn app_shell_sequence_settings_draft_changed_action(
    payload: SequenceSettingsDraftUpdatePayload,
) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_SEQUENCE_SETTINGS_DRAFT_CHANGED, payload)
}

/// Build an app-shell request for selecting one sequence-settings tab.
pub fn app_shell_sequence_settings_tab_changed_action(
    payload: SequenceSettingsTabPayload,
) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_SEQUENCE_SETTINGS_TAB_CHANGED, payload)
}

/// Build an app-shell request for applying sequence settings.
pub fn app_shell_confirm_sequence_settings_action() -> Action {
    custom_app_shell_action(APP_SHELL_CONFIRM_SEQUENCE_SETTINGS)
}

/// Build an app-shell request for selecting one preferences tab.
pub fn app_shell_preferences_tab_changed_action(payload: PreferencesTabPayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_PREFERENCES_TAB_CHANGED, payload)
}

/// Build an app-shell request for switching the app UI theme preference.
pub fn app_shell_preferences_theme_changed_action(preference: ThemePreference) -> Action {
    custom_app_shell_action_with_payload(
        APP_SHELL_PREFERENCES_THEME_CHANGED,
        PreferencesThemePayload { preference },
    )
}

/// Build an app-shell request for switching the waveform display mode.
pub fn app_shell_preferences_waveform_display_changed_action(mode: WaveformDisplay) -> Action {
    custom_app_shell_action_with_payload(
        APP_SHELL_PREFERENCES_WAVEFORM_DISPLAY_CHANGED,
        PreferencesWaveformDisplayPayload { mode },
    )
}

/// Build an app-shell request for switching the Viewer transparency background.
pub fn app_shell_preferences_viewer_background_changed_action(
    background: ViewerCanvasBackground,
) -> Action {
    custom_app_shell_action_with_payload(
        APP_SHELL_PREFERENCES_VIEWER_BACKGROUND_CHANGED,
        PreferencesViewerBackgroundPayload { background },
    )
}

/// Build an app-shell request for disabling one shortcut descriptor.
pub fn app_shell_preferences_shortcut_disabled_action(id: impl Into<String>) -> Action {
    custom_app_shell_action_with_payload(
        APP_SHELL_PREFERENCES_SHORTCUT_DISABLED,
        PreferencesShortcutPayload { id: id.into() },
    )
}

/// Build an app-shell request for restoring one shortcut descriptor to default.
pub fn app_shell_preferences_shortcut_reset_action(id: impl Into<String>) -> Action {
    custom_app_shell_action_with_payload(
        APP_SHELL_PREFERENCES_SHORTCUT_RESET,
        PreferencesShortcutPayload { id: id.into() },
    )
}

/// Build an app-shell request for rebinding one shortcut descriptor.
pub fn app_shell_preferences_shortcut_rebound_action(
    payload: PreferencesShortcutReboundPayload,
) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_PREFERENCES_SHORTCUT_REBOUND, payload)
}

/// Build an app-shell request for closing the current shell-local modal.
pub fn app_shell_close_modal_action() -> Action {
    custom_app_shell_action(APP_SHELL_CLOSE_MODAL)
}

/// Build an app-shell request to copy system info text to the clipboard.
pub fn app_shell_copy_system_info_action(text: impl Into<String>) -> Action {
    custom_app_shell_action_with_payload(
        APP_SHELL_COPY_SYSTEM_INFO,
        AppShellCopySystemInfoPayload { text: text.into() },
    )
}

/// Build an app-shell request to save before continuing a pending close/quit flow.
pub fn app_shell_pending_close_save_continue_action() -> Action {
    custom_app_shell_action(APP_SHELL_PENDING_CLOSE_SAVE_CONTINUE)
}

/// Build an app-shell request to continue a pending close/quit flow without saving.
pub fn app_shell_pending_close_discard_action() -> Action {
    custom_app_shell_action(APP_SHELL_PENDING_CLOSE_DISCARD)
}

/// Build an app-shell request to cancel a pending close/quit flow.
pub fn app_shell_pending_close_cancel_action() -> Action {
    custom_app_shell_action(APP_SHELL_PENDING_CLOSE_CANCEL)
}

/// Build an app-shell request for quitting the native application window.
pub fn app_shell_quit_action() -> Action {
    custom_app_shell_action(APP_SHELL_QUIT)
}

/// Build an app-shell request for minimizing the native application window.
pub fn app_shell_window_minimize_action() -> Action {
    custom_app_shell_action(APP_SHELL_WINDOW_MINIMIZE)
}

/// Build an app-shell request for toggling the native application window maximized state.
pub fn app_shell_window_toggle_maximize_action() -> Action {
    custom_app_shell_action(APP_SHELL_WINDOW_TOGGLE_MAXIMIZE)
}

/// Build an app-shell request for beginning native window drag from custom chrome.
pub fn app_shell_window_drag_action() -> Action {
    custom_app_shell_action(APP_SHELL_WINDOW_DRAG)
}

/// Build an app-shell request for relocating one dock panel tab.
pub fn app_shell_relocate_panel_action(payload: AppShellRelocatePanelPayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_RELOCATE_PANEL, payload)
}

fn custom_timeline_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: TIMELINE_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_inspector_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: INSPECTOR_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_effects_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: EFFECTS_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_assets_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: ASSETS_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_export_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: EXPORT_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_viewer_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: VIEWER_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_project_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: PROJECT_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_sequence_action(name: &'static str) -> Action {
    Action::Custom {
        namespace: SEQUENCE_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::Value::Null,
    }
}

fn custom_sequence_action_with_payload<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: SEQUENCE_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_app_shell_action(name: &'static str) -> Action {
    Action::Custom {
        namespace: APP_SHELL_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::Value::Null,
    }
}

fn custom_app_shell_action_with_payload<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: APP_SHELL_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}
