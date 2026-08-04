//! UI action payloads consumed by [`AppState`](crate::app::AppState).
//!
//! Reusable widget crates remain domain-light. UI adapters attach stable app ids
//! to `Action::Custom` payloads before actions reach the app state layer.

use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::types::{
    AssetId, AudioComponentEditId, AudioSourceComponentId, ClipId, JobId, ProgramOutputId,
};
use mondrian_core::{
    ColorEngine, ColorSpace, DisplayToneMapPolicy, FramePosition, Rational, Resolution,
    TimelineDisplayFormat, WorkingColorSpace,
};
use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_media::{
    DecodedVideoRange, DetectedColorInterpretation, ProvenVideoSampling, VideoColorMetadata,
    VideoColorMetadataHint,
};
use mondrian_timeline::{
    sequence::{ColorWorkflow, DeliveryBitDepth, MissingColorMetadataPolicy, VideoRange},
    AudioChannelLayout, AudioChannelStripEditRequest, AudioDisplayFormat,
    AudioProcessorRackEditRequest, AudioRoutingEditRequest, EditingMode, FieldOrder,
    PixelAspectRatio, PreviewRenderFormat,
};
use mondrian_ui_theme::ThemePreference;
use mondrian_ui_widgets::{ViewerCanvasBackground, WaveformDisplay};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use super::exporting::TimelineExportRequest;
pub use super::product_action::{
    AssetAudioComponentRebindPayload, AssetCreateFolderPayload, AssetCreateGeneratedPayload,
    AssetImportFilesPayload, AssetLibraryMovePayload, AssetLibrarySelectionPayload,
    AssetRelinkPayload, AssetRenameFolderPayload, AssetRenamePayload,
    AssetSetInterpretationPayload, AssetSetProxyModePayload, AssetTargetPayload,
    ClipCurveEditPayload, ClipEditNumericCurvePayload, ClipNormalizedCurvePointPayload,
    ClipParameterValueWrite, ClipSetEnabledPayload, ClipSetSolidColorPayload,
    ClipWriteParameterValuesPayload, ExportDraftEdit, ProjectCreateWithSettingsPayload,
    ProjectRecoverFromAutosavePayload, ProjectUpdateColorEnvironmentPayload,
    ProjectUpdateNewSequenceDefaultsPayload, SequenceTargetPayload, SequenceUpdateSettingsPayload,
    TimelineClipSelectionModePayload, TimelineDropAssetPayload, TimelineInOutPointKind,
    TimelineInsertAssetPayload, TimelineMoveClipPayload, TimelinePrecomposeSelectionPayload,
    TimelineSeekPayload, TimelineSeekSource, TimelineSelectClipPayload, TimelineSelectionEdit,
    TimelineSetInOutPointPayload, TimelineTrimClipsPayload, TimelineTrimPayloadEdge, TrackAddKind,
    TrackAddPayload, TrackAuthorControl, TrackEditPolicyControl, TrackMovePayload,
    TrackSetAuthorControlPayload, TrackSetEditPolicyPayload,
    VideoTransitionCreateCrossDissolvePayload, VideoTransitionHandlePolicy,
    VideoTransitionSetRangePayload, VideoTransitionTargetPayload,
    ViewerSetPreviewResolutionScalePayload, VisualEffectAddToClipPayload,
    VisualEffectReorderPayload, VisualEffectSetEnabledPayload,
    VisualEffectSetParameterValuePayload, VisualEffectTargetPayload, ASSET_CREATE_FOLDER,
    ASSET_CREATE_GENERATED, ASSET_IMPORT_FILES, ASSET_MOVE_ENTRIES, ASSET_NAMESPACE,
    ASSET_PREPARE_DRAG, ASSET_REBIND_AUDIO_COMPONENT, ASSET_REFRESH_AUDIO_COMPONENTS, ASSET_RELINK,
    ASSET_REMOVE_ENTRIES, ASSET_RENAME, ASSET_RENAME_FOLDER, ASSET_SET_INTERPRETATION,
    ASSET_SET_PROXY_MODE, AUDIO_EDIT_COMPONENT, AUDIO_EDIT_PROCESSOR_RACK, AUDIO_NAMESPACE,
    CLIP_EDIT_NUMERIC_CURVE, CLIP_NAMESPACE, CLIP_SET_ENABLED, CLIP_SET_SOLID_COLOR,
    CLIP_WRITE_PARAMETER_VALUES, EXPORT_CANCEL, EXPORT_CLEAR_TERMINAL_HISTORY, EXPORT_EDIT_DRAFT,
    EXPORT_ENQUEUE, EXPORT_NAMESPACE, PROJECT_CREATE_WITH_SETTINGS, PROJECT_NAMESPACE,
    PROJECT_RECOVER_FROM_AUTOSAVE, PROJECT_UPDATE_COLOR_ENVIRONMENT,
    PROJECT_UPDATE_NEW_SEQUENCE_DEFAULTS, SEQUENCE_DELETE, SEQUENCE_DUPLICATE, SEQUENCE_NAMESPACE,
    SEQUENCE_NEW, SEQUENCE_OPEN_NESTED, SEQUENCE_RETURN_TO_PARENT, SEQUENCE_SET_ACTIVE_DEFAULT,
    SEQUENCE_SWITCH_ACTIVE, SEQUENCE_UPDATE_SETTINGS, TIMELINE_APPLY_RANGE_EDIT,
    TIMELINE_CLEAR_IN_OUT_POINTS, TIMELINE_CREATE_BASIC_TITLE, TIMELINE_EDIT_SELECTION,
    TIMELINE_INSERT_ASSET, TIMELINE_MOVE_CLIP, TIMELINE_NAMESPACE, TIMELINE_PLACE_ASSET,
    TIMELINE_PRECOMPOSE_SELECTION, TIMELINE_SEEK, TIMELINE_SELECT_CLIP, TIMELINE_SET_IN_OUT_POINT,
    TIMELINE_TRIM_CLIPS, TRACK_ADD, TRACK_MOVE, TRACK_NAMESPACE, TRACK_SET_AUTHOR_CONTROL,
    TRACK_SET_EDIT_POLICY, VIDEO_TRANSITION_CREATE_CROSS_DISSOLVE, VIDEO_TRANSITION_NAMESPACE,
    VIDEO_TRANSITION_REMOVE, VIDEO_TRANSITION_SELECT, VIDEO_TRANSITION_SET_RANGE, VIEWER_NAMESPACE,
    VISUAL_EFFECT_ADD_TO_CLIP, VISUAL_EFFECT_NAMESPACE, VISUAL_EFFECT_REMOVE,
    VISUAL_EFFECT_REORDER, VISUAL_EFFECT_SELECT, VISUAL_EFFECT_SET_ENABLED,
    VISUAL_EFFECT_SET_PARAMETER_VALUE,
};
use super::product_action::{
    AssetProductAction, AudioProductAction, ClipProductAction, ExportProductAction, ProductAction,
    ProjectProductAction, SequenceProductAction, TimelineProductAction, TrackProductAction,
    VideoTransitionProductAction, ViewerProductAction, VisualEffectProductAction,
};

/// Custom action namespace for inspector UI operations.
pub const INSPECTOR_NAMESPACE: &str = "ui.inspector";

/// Action name for selecting the logical source of one Clip audio Component Edit.
pub const INSPECTOR_SET_AUDIO_COMPONENT_SOURCE: &str = "set_audio_component_source";
/// Shell-local action name for cycling viewer canvas zoom.
pub const VIEWER_CYCLE_ZOOM: &str = "cycle_zoom";
/// Shell-local action name for setting viewer canvas zoom.
pub const VIEWER_SET_ZOOM_SCALE: &str = "set_zoom_scale";

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
/// App-shell request to navigate the Asset browser to one folder.
pub const APP_SHELL_ASSET_BROWSER_OPEN_FOLDER: &str = "asset_browser_open_folder";

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

/// Change the shell-local viewer canvas zoom.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewerSetZoomScalePayload {
    /// Fixed canvas scale. `None` means fit to available viewer space.
    pub scale: Option<f32>,
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
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Stable edit being changed; source identity is not edit identity.
    pub edit_id: AudioComponentEditId,
    /// New logical source in the owning Clip's source domain.
    pub source: InspectorAudioComponentSourcePayload,
}

/// Open one folder in the app UI asset browser, or the root view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBrowserOpenFolderPayload {
    /// Folder to show. `None` returns to the root/unfiled asset view.
    pub folder_id: Option<String>,
}

/// UI Adapter target for one Asset operation.
pub type AssetsPrepareDragPayload = AssetTargetPayload;
/// UI Adapter target for refreshing one Asset's audio evidence.
pub type AssetsRefreshAudioComponentsPayload = AssetTargetPayload;
/// UI Adapter input for repairing one audio Component binding.
pub type AssetsRebindAudioComponentPayload = AssetAudioComponentRebindPayload;
/// UI Adapter input for creating generated content in one folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsCreateAssetPayload {
    /// Target folder, or `None` for root.
    pub folder_id: Option<String>,
}
/// UI Adapter input for retiring one Asset membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsDeleteAssetPayload {
    /// Asset membership to retire.
    pub asset_id: AssetId,
}
/// UI Adapter input for deleting one Library folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsDeleteFolderPayload {
    /// Folder to delete.
    pub folder_id: String,
}
/// UI Adapter input for removing a multi-selection atomically.
pub type AssetsDeleteSelectionPayload = AssetLibrarySelectionPayload;
/// UI Adapter input for moving one Asset membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsMoveAssetPayload {
    /// Asset to move.
    pub asset_id: AssetId,
    /// Destination folder, or `None` for root.
    pub folder_id: Option<String>,
}
/// UI Adapter input for reparenting one Library folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsMoveFolderPayload {
    /// Folder to move.
    pub folder_id: String,
    /// Destination parent, or `None` for root.
    pub parent_folder_id: Option<String>,
}
/// UI Adapter input for moving a multi-selection atomically.
pub type AssetsMoveSelectionPayload = AssetLibraryMovePayload;
/// UI Adapter input for creating one Library folder.
pub type AssetsCreateFolderPayload = AssetCreateFolderPayload;
/// UI Adapter input for importing files.
pub type AssetsImportFilesPayload = AssetImportFilesPayload;
/// UI Adapter input for relinking one Asset.
pub type AssetsRelinkAssetPayload = AssetRelinkPayload;
/// UI Adapter input for changing media interpretation.
pub type AssetsSetInterpretationPayload = AssetSetInterpretationPayload;
/// UI Adapter input for renaming one Asset.
pub type AssetsRenameAssetPayload = AssetRenamePayload;
/// UI Adapter input for renaming one Library folder.
pub type AssetsRenameFolderPayload = AssetRenameFolderPayload;
/// UI Adapter input for changing proxy preference.
pub type AssetsSetProxyModePayload = AssetSetProxyModePayload;
/// Shell Adapter input for navigating the Asset browser.
pub type AssetsOpenFolderPayload = AssetBrowserOpenFolderPayload;

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

/// Platform save-dialog defaults for choosing an export output file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportOutputDialogPayload {
    /// Suggested file name shown by the native save dialog.
    pub default_file_name: String,
    /// Preferred container extension without a leading dot.
    pub extension: String,
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
    ProductAction::Timeline(TimelineProductAction::EditSelection(
        TimelineSelectionEdit::LinkClips,
    ))
    .into_external_action()
}

/// Build an action that unlinks the current Clip selection.
pub fn timeline_unlink_selected_clips_action() -> Action {
    ProductAction::Timeline(TimelineProductAction::EditSelection(
        TimelineSelectionEdit::UnlinkClips,
    ))
    .into_external_action()
}

/// Build an action that selects a visual Transition.
pub fn video_transition_select_action(payload: VideoTransitionTargetPayload) -> Action {
    ProductAction::VideoTransition(VideoTransitionProductAction::Select(payload))
        .into_external_action()
}

/// Build an action that creates the product-default Cross Dissolve at an edit.
pub fn video_transition_create_cross_dissolve_action(
    payload: VideoTransitionCreateCrossDissolvePayload,
) -> Action {
    ProductAction::VideoTransition(VideoTransitionProductAction::CreateCrossDissolve(payload))
        .into_external_action()
}

/// Build an action that creates a generated Basic Title.
pub fn timeline_create_basic_title_action() -> Action {
    ProductAction::Timeline(TimelineProductAction::CreateBasicTitle).into_external_action()
}

/// Build an action that changes one visual Transition range.
pub fn video_transition_set_range_action(payload: VideoTransitionSetRangePayload) -> Action {
    ProductAction::VideoTransition(VideoTransitionProductAction::SetRange(payload))
        .into_external_action()
}

/// Build an action that removes one visual Transition.
pub fn video_transition_remove_action(payload: VideoTransitionTargetPayload) -> Action {
    ProductAction::VideoTransition(VideoTransitionProductAction::Remove(payload))
        .into_external_action()
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
pub fn timeline_trim_selected_clips_to_playhead_action(edge: TimelineTrimPayloadEdge) -> Action {
    ProductAction::Timeline(TimelineProductAction::EditSelection(
        TimelineSelectionEdit::TrimClipsToPlayhead { edge },
    ))
    .into_external_action()
}

/// Build an action that rolls the selected cut to the playhead.
pub fn timeline_roll_selected_cut_to_playhead_action() -> Action {
    ProductAction::Timeline(TimelineProductAction::EditSelection(
        TimelineSelectionEdit::RollCutToPlayhead,
    ))
    .into_external_action()
}

/// Build an action that sets one active-sequence in/out point.
pub fn timeline_set_in_out_point_action(payload: TimelineSetInOutPointPayload) -> Action {
    ProductAction::Timeline(TimelineProductAction::SetInOutPoint(payload)).into_external_action()
}

/// Build an action that clears the active sequence in/out points.
pub fn timeline_clear_in_out_points_action() -> Action {
    ProductAction::Timeline(TimelineProductAction::ClearInOutPoints).into_external_action()
}

/// Build an action that lifts the active In/Out range.
pub fn timeline_lift_range_action() -> Action {
    ProductAction::Timeline(TimelineProductAction::ApplyRangeEdit(
        mondrian_timeline::RangeEditKind::Lift,
    ))
    .into_external_action()
}

/// Build an action that extracts the active In/Out range.
pub fn timeline_extract_range_action() -> Action {
    ProductAction::Timeline(TimelineProductAction::ApplyRangeEdit(
        mondrian_timeline::RangeEditKind::Extract,
    ))
    .into_external_action()
}

/// Build an action that toggles the current timeline clip selection.
pub fn timeline_set_selected_clips_enabled_action(enabled: bool) -> Action {
    ProductAction::Timeline(TimelineProductAction::EditSelection(
        TimelineSelectionEdit::SetClipsEnabled { enabled },
    ))
    .into_external_action()
}

/// Build an action that seeks the active timeline.
pub fn timeline_seek_action(position: FramePosition) -> Action {
    timeline_seek_with_source_action(position, TimelineSeekSource::Settled)
}

/// Build an action that seeks the active timeline with explicit interaction source.
pub fn timeline_seek_with_source_action(
    position: FramePosition,
    source: TimelineSeekSource,
) -> Action {
    ProductAction::Timeline(TimelineProductAction::Seek(TimelineSeekPayload {
        position,
        source,
    }))
    .into_external_action()
}

/// Build an action that changes one persistent Track control.
pub fn track_set_author_control_action(payload: TrackSetAuthorControlPayload) -> Action {
    ProductAction::Track(TrackProductAction::SetAuthorControl(payload)).into_external_action()
}

/// Build an action that changes Track Targeting or Sync-Lock session policy.
pub fn track_set_edit_policy_action(payload: TrackSetEditPolicyPayload) -> Action {
    ProductAction::Track(TrackProductAction::SetEditPolicy(payload)).into_external_action()
}

/// Build an action that adds a Timeline Track.
pub fn track_add_action(payload: TrackAddPayload) -> Action {
    ProductAction::Track(TrackProductAction::Add(payload)).into_external_action()
}

/// Build an action that moves a Timeline Track by stable relative placement.
pub fn track_move_action(payload: TrackMovePayload) -> Action {
    ProductAction::Track(TrackProductAction::Move(payload)).into_external_action()
}

/// Build an action that drops an asset onto a timeline track.
pub fn timeline_drop_asset_action(payload: TimelineDropAssetPayload) -> Action {
    ProductAction::Timeline(TimelineProductAction::PlaceAsset(payload)).into_external_action()
}

/// Build an action that performs one professional Insert Edit from an Asset.
pub fn timeline_insert_asset_action(payload: TimelineInsertAssetPayload) -> Action {
    ProductAction::Timeline(TimelineProductAction::InsertAsset(Box::new(payload)))
        .into_external_action()
}

/// Build an action that precomposes the current clip selection.
pub fn timeline_precompose_selection_action(payload: TimelinePrecomposeSelectionPayload) -> Action {
    ProductAction::Timeline(TimelineProductAction::PrecomposeSelection(payload))
        .into_external_action()
}

/// Build an action that opens one nested sequence from the timeline.
pub fn timeline_open_nested_sequence_action(payload: SequenceTargetPayload) -> Action {
    ProductAction::Sequence(SequenceProductAction::OpenNested(payload)).into_external_action()
}

/// Build an action that changes one Clip enabled state.
pub fn clip_set_enabled_action(payload: ClipSetEnabledPayload) -> Action {
    ProductAction::Clip(ClipProductAction::SetEnabled(payload)).into_external_action()
}

/// Build an action that changes one Solid Color Clip source color.
pub fn clip_set_solid_color_action(payload: ClipSetSolidColorPayload) -> Action {
    ProductAction::Clip(ClipProductAction::SetSolidColor(payload)).into_external_action()
}

/// Build an action that atomically writes persistent Clip-owned parameters.
pub fn clip_write_parameter_values_action(payload: ClipWriteParameterValuesPayload) -> Action {
    ProductAction::Clip(ClipProductAction::WriteParameterValues(Box::new(payload)))
        .into_external_action()
}

/// Build an action that edits one numeric Clip curve by stable identity.
pub fn clip_edit_numeric_curve_action(payload: ClipEditNumericCurvePayload) -> Action {
    ProductAction::Clip(ClipProductAction::EditNumericCurve(Box::new(payload)))
        .into_external_action()
}

/// Build an action that changes one Clip audio Component Edit's logical source.
pub fn inspector_set_audio_component_source_action(
    payload: InspectorSetAudioComponentSourcePayload,
) -> Action {
    custom_inspector_action(INSPECTOR_SET_AUDIO_COMPONENT_SOURCE, payload)
}

/// Build an action that inserts one registered visual Effect on a Clip.
pub fn visual_effect_add_to_clip_action(payload: VisualEffectAddToClipPayload) -> Action {
    ProductAction::VisualEffect(VisualEffectProductAction::AddToClip(payload))
        .into_external_action()
}

/// Build an action that selects one visual Effect instance.
pub fn visual_effect_select_action(payload: VisualEffectTargetPayload) -> Action {
    ProductAction::VisualEffect(VisualEffectProductAction::Select(payload)).into_external_action()
}

/// Build an action that changes one visual Effect enabled state.
pub fn visual_effect_set_enabled_action(payload: VisualEffectSetEnabledPayload) -> Action {
    ProductAction::VisualEffect(VisualEffectProductAction::SetEnabled(payload))
        .into_external_action()
}

/// Build an action that removes one visual Effect instance.
pub fn visual_effect_remove_action(payload: VisualEffectTargetPayload) -> Action {
    ProductAction::VisualEffect(VisualEffectProductAction::Remove(payload)).into_external_action()
}

/// Build an action that moves one visual Effect relative to another instance.
pub fn visual_effect_reorder_action(payload: VisualEffectReorderPayload) -> Action {
    ProductAction::VisualEffect(VisualEffectProductAction::Reorder(payload)).into_external_action()
}

/// Build an action that writes one stable-address visual Effect parameter.
pub fn visual_effect_set_parameter_value_action(
    payload: VisualEffectSetParameterValuePayload,
) -> Action {
    ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(Box::new(
        payload,
    )))
    .into_external_action()
}

/// Build an action that prepares one Asset for Timeline drag/drop.
pub fn asset_prepare_drag_action(payload: AssetTargetPayload) -> Action {
    ProductAction::Asset(AssetProductAction::PrepareDrag(payload)).into_external_action()
}

/// Build an action that refreshes one Asset's audio stream candidates.
pub fn asset_refresh_audio_components_action(payload: AssetTargetPayload) -> Action {
    ProductAction::Asset(AssetProductAction::RefreshAudioComponents(payload)).into_external_action()
}

/// Build an action that explicitly repairs an Asset audio Component binding.
pub fn asset_rebind_audio_component_action(payload: AssetAudioComponentRebindPayload) -> Action {
    ProductAction::Asset(AssetProductAction::RebindAudioComponent(payload)).into_external_action()
}

/// Build one atomic Asset Library removal action.
pub fn asset_remove_entries_action(payload: AssetLibrarySelectionPayload) -> Action {
    ProductAction::Asset(AssetProductAction::RemoveEntries(Box::new(payload)))
        .into_external_action()
}

/// Build one atomic Asset Library move action.
pub fn asset_move_entries_action(payload: AssetLibraryMovePayload) -> Action {
    ProductAction::Asset(AssetProductAction::MoveEntries(Box::new(payload))).into_external_action()
}

/// Build an action that creates an adjustment-layer Asset in the Library.
pub fn asset_create_adjustment_layer_action(folder_id: Option<String>) -> Action {
    ProductAction::Asset(AssetProductAction::CreateGenerated(
        AssetCreateGeneratedPayload {
            kind: mondrian_core::types::GeneratedAssetKind::AdjustmentLayer,
            folder_id,
        },
    ))
    .into_external_action()
}

/// Build an action that creates a solid-color Asset in the Library.
pub fn asset_create_solid_color_action(folder_id: Option<String>) -> Action {
    ProductAction::Asset(AssetProductAction::CreateGenerated(
        AssetCreateGeneratedPayload {
            kind: mondrian_core::types::GeneratedAssetKind::SolidColor,
            folder_id,
        },
    ))
    .into_external_action()
}

/// Build an action that creates a Library folder.
pub fn asset_create_folder_action(payload: AssetCreateFolderPayload) -> Action {
    ProductAction::Asset(AssetProductAction::CreateFolder(payload)).into_external_action()
}

/// Build an action that imports media files into an Asset Library folder.
pub fn asset_import_files_action(payload: AssetImportFilesPayload) -> Action {
    ProductAction::Asset(AssetProductAction::ImportFiles(Box::new(payload))).into_external_action()
}

/// Build an action that relinks one Asset to a replacement file.
pub fn asset_relink_action(payload: AssetRelinkPayload) -> Action {
    ProductAction::Asset(AssetProductAction::Relink(payload)).into_external_action()
}

/// Build an action that persists one Asset's media interpretation.
pub fn asset_set_interpretation_action(payload: AssetSetInterpretationPayload) -> Action {
    ProductAction::Asset(AssetProductAction::SetInterpretation(payload)).into_external_action()
}

/// Build an action that renames one Asset.
pub fn asset_rename_action(payload: AssetRenamePayload) -> Action {
    ProductAction::Asset(AssetProductAction::Rename(payload)).into_external_action()
}

/// Build an action that renames one Library folder.
pub fn asset_rename_folder_action(payload: AssetRenameFolderPayload) -> Action {
    ProductAction::Asset(AssetProductAction::RenameFolder(payload)).into_external_action()
}

/// Build an action that toggles proxy playback for one video Asset.
pub fn asset_set_proxy_mode_action(payload: AssetSetProxyModePayload) -> Action {
    ProductAction::Asset(AssetProductAction::SetProxyMode(payload)).into_external_action()
}

/// Build a shell-local action that opens an Asset-browser folder.
pub fn asset_browser_open_folder_action(payload: AssetBrowserOpenFolderPayload) -> Action {
    custom_app_shell_action_with_payload(APP_SHELL_ASSET_BROWSER_OPEN_FOLDER, payload)
}

/// Build the Asset-panel drag Adapter action.
pub fn assets_prepare_drag_action(payload: AssetsPrepareDragPayload) -> Action {
    asset_prepare_drag_action(payload)
}

/// Build the Asset-panel audio-evidence refresh Adapter action.
pub fn assets_refresh_audio_components_action(
    payload: AssetsRefreshAudioComponentsPayload,
) -> Action {
    asset_refresh_audio_components_action(payload)
}

/// Build the Asset-panel audio Component rebind Adapter action.
pub fn assets_rebind_audio_component_action(payload: AssetsRebindAudioComponentPayload) -> Action {
    asset_rebind_audio_component_action(payload)
}

/// Build the Asset-panel single-membership retirement Adapter action.
pub fn assets_delete_asset_action(payload: AssetsDeleteAssetPayload) -> Action {
    asset_remove_entries_action(AssetLibrarySelectionPayload {
        asset_ids: vec![payload.asset_id],
        folder_ids: Vec::new(),
    })
}

/// Build the Asset-panel single-folder removal Adapter action.
pub fn assets_delete_folder_action(payload: AssetsDeleteFolderPayload) -> Action {
    asset_remove_entries_action(AssetLibrarySelectionPayload {
        asset_ids: Vec::new(),
        folder_ids: vec![payload.folder_id],
    })
}

/// Build the Asset-panel multi-selection removal Adapter action.
pub fn assets_delete_selection_action(payload: AssetsDeleteSelectionPayload) -> Action {
    asset_remove_entries_action(payload)
}

/// Build the Asset-panel single-membership move Adapter action.
pub fn assets_move_asset_action(payload: AssetsMoveAssetPayload) -> Action {
    asset_move_entries_action(AssetLibraryMovePayload {
        asset_ids: vec![payload.asset_id],
        folder_ids: Vec::new(),
        target_folder_id: payload.folder_id,
    })
}

/// Build the Asset-panel single-folder move Adapter action.
pub fn assets_move_folder_action(payload: AssetsMoveFolderPayload) -> Action {
    asset_move_entries_action(AssetLibraryMovePayload {
        asset_ids: Vec::new(),
        folder_ids: vec![payload.folder_id],
        target_folder_id: payload.parent_folder_id,
    })
}

/// Build the Asset-panel multi-selection move Adapter action.
pub fn assets_move_selection_action(payload: AssetsMoveSelectionPayload) -> Action {
    asset_move_entries_action(payload)
}

/// Build the Asset-panel generated adjustment-layer Adapter action.
pub fn assets_create_adjustment_layer_action(payload: AssetsCreateAssetPayload) -> Action {
    asset_create_adjustment_layer_action(payload.folder_id)
}

/// Build the Asset-panel generated solid-color Adapter action.
pub fn assets_create_solid_color_action(payload: AssetsCreateAssetPayload) -> Action {
    asset_create_solid_color_action(payload.folder_id)
}

/// Build the Asset-panel folder-creation Adapter action.
pub fn assets_create_folder_action(payload: AssetsCreateFolderPayload) -> Action {
    asset_create_folder_action(payload)
}

/// Build the Asset-panel file-import Adapter action.
pub fn assets_import_files_action(payload: AssetsImportFilesPayload) -> Action {
    asset_import_files_action(payload)
}

/// Build the native-shell relink Adapter action.
pub fn assets_relink_asset_action(payload: AssetsRelinkAssetPayload) -> Action {
    asset_relink_action(payload)
}

/// Build the Interpret Footage Adapter action.
pub fn assets_set_interpretation_action(payload: AssetsSetInterpretationPayload) -> Action {
    asset_set_interpretation_action(payload)
}

/// Build the Asset-panel rename Adapter action.
pub fn assets_rename_asset_action(payload: AssetsRenameAssetPayload) -> Action {
    asset_rename_action(payload)
}

/// Build the Asset-panel folder rename Adapter action.
pub fn assets_rename_folder_action(payload: AssetsRenameFolderPayload) -> Action {
    asset_rename_folder_action(payload)
}

/// Build the Asset-panel proxy-preference Adapter action.
pub fn assets_set_proxy_mode_action(payload: AssetsSetProxyModePayload) -> Action {
    asset_set_proxy_mode_action(payload)
}

/// Build the shell-local Asset-browser navigation Adapter action.
pub fn assets_open_folder_action(payload: AssetsOpenFolderPayload) -> Action {
    asset_browser_open_folder_action(payload)
}

/// Build an action that enqueues a timeline export.
pub fn export_enqueue_action(request: TimelineExportRequest) -> Action {
    ProductAction::Export(ExportProductAction::Enqueue(Box::new(request))).into_external_action()
}

/// Build an action that updates one export draft field.
pub fn export_edit_draft_action(edit: ExportDraftEdit) -> Action {
    ProductAction::Export(ExportProductAction::EditDraft(Box::new(edit))).into_external_action()
}

/// Build an action that cancels one export queue job.
pub fn export_cancel_action(job_id: JobId) -> Action {
    ProductAction::Export(ExportProductAction::Cancel(job_id)).into_external_action()
}

/// Build an action that clears retained terminal export evidence.
pub fn export_clear_terminal_history_action() -> Action {
    ProductAction::Export(ExportProductAction::ClearTerminalHistory).into_external_action()
}

/// Build a viewer request for changing preview resolution scale.
pub fn viewer_set_preview_resolution_scale_action(
    payload: ViewerSetPreviewResolutionScalePayload,
) -> Action {
    ProductAction::Viewer(ViewerProductAction::SetPreviewResolutionScale(payload))
        .into_external_action()
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
    ProductAction::Project(ProjectProductAction::CreateWithSettings(payload)).into_external_action()
}

/// Build a project action that atomically replaces new-Sequence defaults.
pub fn project_update_new_sequence_defaults_action(
    payload: ProjectUpdateNewSequenceDefaultsPayload,
) -> Action {
    ProductAction::Project(ProjectProductAction::UpdateNewSequenceDefaults(payload))
        .into_external_action()
}

/// Build a project action that atomically replaces the shared color engine.
pub fn project_update_color_environment_action(
    payload: ProjectUpdateColorEnvironmentPayload,
) -> Action {
    ProductAction::Project(ProjectProductAction::UpdateColorEnvironment(payload))
        .into_external_action()
}

/// Build an action that recovers a project from an autosave snapshot.
pub fn project_recover_from_autosave_action(payload: ProjectRecoverFromAutosavePayload) -> Action {
    ProductAction::Project(ProjectProductAction::RecoverFromAutosave(payload))
        .into_external_action()
}

/// Build an action that returns from a nested sequence to its parent.
pub fn sequence_return_to_parent_action() -> Action {
    ProductAction::Sequence(SequenceProductAction::ReturnToParent).into_external_action()
}

/// Build an action that makes the current active sequence the project default.
pub fn sequence_set_active_default_action() -> Action {
    ProductAction::Sequence(SequenceProductAction::SetActiveDefault).into_external_action()
}

/// Build an action that creates a new sequence with product defaults.
pub fn sequence_new_action() -> Action {
    ProductAction::Sequence(SequenceProductAction::New).into_external_action()
}

/// Build an action that switches the active sequence.
pub fn sequence_switch_active_action(payload: SequenceTargetPayload) -> Action {
    ProductAction::Sequence(SequenceProductAction::SwitchActive(payload)).into_external_action()
}

/// Build an action that duplicates one sequence.
pub fn sequence_duplicate_action(payload: SequenceTargetPayload) -> Action {
    ProductAction::Sequence(SequenceProductAction::Duplicate(payload)).into_external_action()
}

/// Build an action that deletes one sequence.
pub fn sequence_delete_action(payload: SequenceTargetPayload) -> Action {
    ProductAction::Sequence(SequenceProductAction::Delete(payload)).into_external_action()
}

/// Build an action that applies edited settings to one sequence.
pub fn sequence_update_settings_action(payload: SequenceUpdateSettingsPayload) -> Action {
    ProductAction::Sequence(SequenceProductAction::UpdateSettings(Box::new(payload)))
        .into_external_action()
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

fn custom_inspector_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: INSPECTOR_NAMESPACE.into(),
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
