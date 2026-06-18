//! UI action payloads consumed by [`AppState`](crate::app::AppState).
//!
//! Reusable widget crates remain domain-light. UI adapters attach stable app ids
//! to `Action::Custom` payloads before actions reach the app state layer.

use mondrian_core::effect_data::EffectType;
use mondrian_core::types::{AssetId, ClipId, EffectId, SequenceId, TrackId};
use mondrian_core::{ProjectSettings, Rational, Resolution};
use mondrian_editor_state::Action;
use mondrian_export::preset::{ExportPreset, TimelineExportRange};
use mondrian_timeline::SequenceSettings;
use mondrian_ui_theme::ThemePreset;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Custom action namespace for timeline UI operations.
pub const TIMELINE_NAMESPACE: &str = "ui.timeline";

/// Action name for selecting a timeline clip.
pub const TIMELINE_SELECT_CLIP: &str = "select_clip";
/// Action name for moving a timeline clip.
pub const TIMELINE_MOVE_CLIP: &str = "move_clip";
/// Action name for trimming a timeline clip.
pub const TIMELINE_TRIM_CLIP: &str = "trim_clip";
/// Action name for seeking the active timeline.
pub const TIMELINE_SEEK: &str = "seek";
/// Action name for changing one timeline track header control.
pub const TIMELINE_SET_TRACK_CONTROL: &str = "set_track_control";
/// Action name for adding a video or audio timeline track.
pub const TIMELINE_ADD_TRACK: &str = "add_track";
/// Action name for reordering one timeline track.
pub const TIMELINE_MOVE_TRACK: &str = "move_track";
/// Action name for dropping one prepared asset onto a timeline track.
pub const TIMELINE_DROP_ASSET: &str = "drop_asset";

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
/// Action name for changing a selected clip's animation curve draft.
pub const INSPECTOR_SET_CLIP_CURVE: &str = "set_clip_curve";
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
/// Action name for creating an adjustment-layer asset in the library.
pub const ASSETS_CREATE_ADJUSTMENT_LAYER: &str = "create_adjustment_layer";
/// Action name for creating a solid-color asset in the library.
pub const ASSETS_CREATE_SOLID_COLOR: &str = "create_solid_color";
/// Action name for creating a folder in the library.
pub const ASSETS_CREATE_FOLDER: &str = "create_folder";
/// Action name for opening an asset-browser folder in the self-hosted shell.
pub const ASSETS_OPEN_FOLDER: &str = "open_folder";
/// Action name for importing files into an asset-browser folder.
pub const ASSETS_IMPORT_FILES: &str = "import_files";
/// Action name for deleting one asset from the library.
pub const ASSETS_DELETE_ASSET: &str = "delete_asset";
/// Action name for deleting one folder/bin from the library.
pub const ASSETS_DELETE_FOLDER: &str = "delete_folder";
/// Action name for moving one asset between folders.
pub const ASSETS_MOVE_ASSET: &str = "move_asset";
/// Action name for moving one folder/bin between parents.
pub const ASSETS_MOVE_FOLDER: &str = "move_folder";

/// Custom action namespace for export operations.
pub const EXPORT_NAMESPACE: &str = "ui.export";

/// Action name for enqueueing a timeline export job.
pub const EXPORT_ENQUEUE: &str = "enqueue";
/// Action name for updating the self-hosted export draft.
pub const EXPORT_SET_DRAFT: &str = "set_draft";

/// Custom action namespace for project lifecycle operations supplied by shell UI.
pub const PROJECT_NAMESPACE: &str = "ui.project";

/// Action name for creating a project with explicit settings.
pub const PROJECT_CREATE_WITH_SETTINGS: &str = "create_with_settings";

/// Custom action namespace for app-shell operations resolved by native adapters.
pub const APP_SHELL_NAMESPACE: &str = "app.shell";

/// App-shell request to create a new project through a platform save dialog.
pub const APP_SHELL_NEW_PROJECT_DIALOG: &str = "new_project_dialog";
/// App-shell request to update one self-hosted new-project draft setting.
pub const APP_SHELL_NEW_PROJECT_DRAFT_CHANGED: &str = "new_project_draft_changed";
/// App-shell request to confirm the self-hosted new-project dialog.
pub const APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG: &str = "confirm_new_project_dialog";
/// App-shell request to cancel the self-hosted new-project dialog.
pub const APP_SHELL_CANCEL_NEW_PROJECT_DIALOG: &str = "cancel_new_project_dialog";
/// App-shell request to open a platform project file dialog.
pub const APP_SHELL_OPEN_PROJECT_DIALOG: &str = "open_project_dialog";
/// App-shell request to open a platform media import dialog.
pub const APP_SHELL_IMPORT_MEDIA_DIALOG: &str = "import_media_dialog";
/// App-shell request to open a platform project save-as dialog.
pub const APP_SHELL_SAVE_PROJECT_AS_DIALOG: &str = "save_project_as_dialog";
/// App-shell request to choose a timeline export output file.
pub const APP_SHELL_EXPORT_OUTPUT_DIALOG: &str = "export_output_dialog";
/// App-shell request to show product about information.
pub const APP_SHELL_ABOUT: &str = "about";
/// App-shell request to show self-hosted preferences.
pub const APP_SHELL_PREFERENCES: &str = "preferences";
/// App-shell request to switch the active self-hosted preferences tab.
pub const APP_SHELL_PREFERENCES_TAB_CHANGED: &str = "preferences_tab_changed";
/// App-shell request to switch the active self-hosted theme preset.
pub const APP_SHELL_PREFERENCES_THEME_CHANGED: &str = "preferences_theme_changed";
/// App-shell request to close the current shell-local modal.
pub const APP_SHELL_CLOSE_MODAL: &str = "close_modal";
/// App-shell request to quit the native application window.
pub const APP_SHELL_QUIT: &str = "quit";
/// App-shell request to minimize the native application window.
pub const APP_SHELL_WINDOW_MINIMIZE: &str = "window_minimize";
/// App-shell request to toggle the native application window maximized state.
pub const APP_SHELL_WINDOW_TOGGLE_MAXIMIZE: &str = "window_toggle_maximize";
/// App-shell request to begin native window dragging from custom chrome.
pub const APP_SHELL_WINDOW_DRAG: &str = "window_drag";

/// Self-hosted preferences section selected by the shell-local preferences UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreferencesTabPayload {
    General,
    Media,
    Shortcuts,
    Developer,
}

/// Theme preset selected by the self-hosted preferences UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesThemePayload {
    /// Theme preset to apply and persist.
    pub preset: ThemePreset,
}

/// Clip edge being trimmed by a timeline UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineTrimPayloadEdge {
    In,
    Out,
}

/// Select a clip in the active sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSelectClipPayload {
    /// Track that owns the selected clip.
    pub track_id: TrackId,
    /// Whether `track_id` is a video track rather than an audio track.
    pub is_video_track: bool,
    /// Clip selected by the UI.
    pub clip_id: ClipId,
}

/// Move a clip to a target track and frame in the active sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMoveClipPayload {
    /// Track that should own the clip after the move.
    pub target_track_id: TrackId,
    /// Whether `target_track_id` is a video track rather than an audio track.
    pub is_video_track: bool,
    /// Clip being moved.
    pub clip_id: ClipId,
    /// Target timeline frame for the clip start.
    pub frame: i64,
}

/// Trim one clip edge to a target timeline frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineTrimClipPayload {
    /// Clip being trimmed.
    pub clip_id: ClipId,
    /// Edge that should be trimmed.
    pub edge: TimelineTrimPayloadEdge,
    /// Target timeline frame for the selected edge.
    pub frame: i64,
}

/// Seek the active timeline to a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSeekPayload {
    /// Target timeline frame.
    pub frame: i64,
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

/// Transform field exposed by the self-hosted inspector.
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

/// One normalized point from the self-hosted curve editor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InspectorCurvePointPayload {
    /// Normalized x coordinate in the curve editor.
    pub x: f32,
    /// Normalized y coordinate in the curve editor.
    pub y: f32,
}

/// Change a selected clip's animation curve draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetClipCurvePayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Ordered normalized curve points.
    pub points: Vec<InspectorCurvePointPayload>,
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
    /// Asset selected from the self-hosted asset browser.
    pub asset_id: AssetId,
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

/// Create a synthetic reusable asset in the selected folder, or at root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsCreateAssetPayload {
    /// Target folder for the new asset. `None` creates it in the root/unfiled view.
    pub folder_id: Option<String>,
}

/// Open one folder in the self-hosted asset browser, or the root view.
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

/// Platform file-dialog target for importing media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportMediaDialogPayload {
    /// Target folder for selected media. `None` imports into the root/unfiled view.
    pub folder_id: Option<String>,
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
}

/// Update one field of the self-hosted export draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExportDraftUpdatePayload {
    /// Select a built-in preset by index.
    PresetIndex(usize),
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
    /// Initial project-level settings.
    pub project_settings: ProjectSettings,
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
    /// Whether project proxy generation is enabled.
    ProxyEnabled(bool),
    /// Whether preview rendering cache is enabled.
    PreviewCacheEnabled(bool),
}

/// Build an action that selects a clip in the active timeline.
pub fn timeline_select_clip_action(payload: TimelineSelectClipPayload) -> Action {
    custom_timeline_action(TIMELINE_SELECT_CLIP, payload)
}

/// Build an action that moves a clip in the active timeline.
pub fn timeline_move_clip_action(payload: TimelineMoveClipPayload) -> Action {
    custom_timeline_action(TIMELINE_MOVE_CLIP, payload)
}

/// Build an action that trims a clip edge in the active timeline.
pub fn timeline_trim_clip_action(payload: TimelineTrimClipPayload) -> Action {
    custom_timeline_action(TIMELINE_TRIM_CLIP, payload)
}

/// Build an action that seeks the active timeline.
pub fn timeline_seek_action(frame: i64) -> Action {
    custom_timeline_action(TIMELINE_SEEK, TimelineSeekPayload { frame: frame.max(0) })
}

/// Build an action that changes a timeline track header control.
pub fn timeline_set_track_control_action(payload: TimelineSetTrackControlPayload) -> Action {
    custom_timeline_action(TIMELINE_SET_TRACK_CONTROL, payload)
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

/// Build an action that changes a clip curve from an inspector panel.
pub fn inspector_set_clip_curve_action(payload: InspectorSetClipCurvePayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_CURVE, payload)
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

/// Build an action that deletes one asset from the library.
pub fn assets_delete_asset_action(payload: AssetsDeleteAssetPayload) -> Action {
    custom_assets_action(ASSETS_DELETE_ASSET, payload)
}

/// Build an action that deletes one folder from the library.
pub fn assets_delete_folder_action(payload: AssetsDeleteFolderPayload) -> Action {
    custom_assets_action(ASSETS_DELETE_FOLDER, payload)
}

/// Build an action that moves one asset to another folder.
pub fn assets_move_asset_action(payload: AssetsMoveAssetPayload) -> Action {
    custom_assets_action(ASSETS_MOVE_ASSET, payload)
}

/// Build an action that moves one folder to another parent.
pub fn assets_move_folder_action(payload: AssetsMoveFolderPayload) -> Action {
    custom_assets_action(ASSETS_MOVE_FOLDER, payload)
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

/// Build an action that creates a project from shell UI.
pub fn project_create_with_settings_action(payload: ProjectCreateWithSettingsPayload) -> Action {
    custom_project_action(PROJECT_CREATE_WITH_SETTINGS, payload)
}

/// Build an app-shell request for creating a new project.
pub fn app_shell_new_project_dialog_action() -> Action {
    custom_app_shell_action(APP_SHELL_NEW_PROJECT_DIALOG)
}

/// Build an app-shell request for changing one new-project draft setting.
pub fn app_shell_new_project_draft_changed_action(payload: NewProjectDraftUpdatePayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_NEW_PROJECT_DRAFT_CHANGED, payload)
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

/// Build an app-shell request for selecting one preferences tab.
pub fn app_shell_preferences_tab_changed_action(payload: PreferencesTabPayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_PREFERENCES_TAB_CHANGED, payload)
}

/// Build an app-shell request for switching the self-hosted theme preset.
pub fn app_shell_preferences_theme_changed_action(preset: ThemePreset) -> Action {
    custom_app_shell_action_with_payload(
        APP_SHELL_PREFERENCES_THEME_CHANGED,
        PreferencesThemePayload { preset },
    )
}

/// Build an app-shell request for closing the current shell-local modal.
pub fn app_shell_close_modal_action() -> Action {
    custom_app_shell_action(APP_SHELL_CLOSE_MODAL)
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

fn custom_project_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: PROJECT_NAMESPACE.into(),
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
