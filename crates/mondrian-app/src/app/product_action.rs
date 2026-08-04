//! Closed product Actions and the external custom-action Adapter.
//!
//! Product code dispatches and admits the typed algebra in this Module.
//! `Action::Custom` remains an external transport for reusable Widgets,
//! scripting, and future plugin Adapters; it is decoded only at this seam.

use std::path::PathBuf;

use mondrian_core::automation::{AnimationParameterAddress, PropertyValue};
use mondrian_core::effect_data::EffectType;
use mondrian_core::timeline_data::AssetMediaInterpretation;
use mondrian_core::types::{
    AssetId, AudioSourceComponentId, ClipId, EffectId, FramePosition, GeneratedAssetKind, JobId,
    KeyframeId, SequenceId, TrackId, VideoTransitionId,
};
use mondrian_core::{
    Color, ProjectColorEnvironment, ProjectSettings, TimelineTime, TimelineTimeRange,
};
use mondrian_editor_state::Action;
use mondrian_export::preset::{BuiltinExportPreset, ExportPreset, TimelineExportRange};
use mondrian_timeline::{
    clip::Clip,
    sequence::{Sequence, SequenceSettings},
    AudioAutomationEditRequest, AudioChannelStripEditRequest, AudioComponentEditRequest,
    AudioProcessorRackEditRequest, AudioRoutingEditRequest, EffectRelativePlacement,
};
use serde::{Deserialize, Serialize};

use super::exporting::TimelineExportRequest;
use super::{AppState, CrashRecoveryCandidate};

/// External custom-action namespace for Timeline product operations.
pub const TIMELINE_NAMESPACE: &str = "ui.timeline";

/// External action name for selecting one Timeline Clip.
pub const TIMELINE_SELECT_CLIP: &str = "select_clip";
/// External action name for moving one Timeline Clip.
pub const TIMELINE_MOVE_CLIP: &str = "move_clip";
/// External action name for trimming one or more Timeline Clips.
pub const TIMELINE_TRIM_CLIPS: &str = "trim_clips";
/// External action name for seeking the active Timeline.
pub const TIMELINE_SEEK: &str = "seek";

/// External custom-action namespace for Sequence-owned visual Transitions.
pub const VIDEO_TRANSITION_NAMESPACE: &str = "ui.video_transition";

/// External action name for selecting one visual Transition.
pub const VIDEO_TRANSITION_SELECT: &str = "select";
/// External action name for creating a product-default Cross Dissolve.
pub const VIDEO_TRANSITION_CREATE_CROSS_DISSOLVE: &str = "create_cross_dissolve";
/// External action name for changing one visual Transition's exact author range.
pub const VIDEO_TRANSITION_SET_RANGE: &str = "set_range";
/// External action name for removing one visual Transition.
pub const VIDEO_TRANSITION_REMOVE: &str = "remove";

/// External custom-action namespace for Viewer product operations.
pub const VIEWER_NAMESPACE: &str = "ui.viewer";

/// External action name for changing the active Sequence preview scale.
pub const VIEWER_SET_PREVIEW_RESOLUTION_SCALE: &str = "set_preview_resolution_scale";
/// External custom-action namespace for Clip authoring operations.
pub const CLIP_NAMESPACE: &str = "ui.clip";

/// External action name for changing one Clip enabled state.
pub const CLIP_SET_ENABLED: &str = "set_enabled";
/// External action name for changing one Solid Color Clip's source color.
pub const CLIP_SET_SOLID_COLOR: &str = "set_solid_color";
/// External action name for atomically writing stable-address Clip parameters.
pub const CLIP_WRITE_PARAMETER_VALUES: &str = "write_parameter_values";
/// External action name for editing one numeric Clip curve by stable identity.
pub const CLIP_EDIT_NUMERIC_CURVE: &str = "edit_numeric_curve";

/// External custom-action namespace for Project product operations.
pub const PROJECT_NAMESPACE: &str = "ui.project";

/// External action name for creating a Project with explicit settings.
pub const PROJECT_CREATE_WITH_SETTINGS: &str = "create_with_settings";
/// External action name for replacing the template copied into future Sequences.
pub const PROJECT_UPDATE_NEW_SEQUENCE_DEFAULTS: &str = "update_new_sequence_defaults";
/// External action name for atomically replacing the Project-wide color engine.
pub const PROJECT_UPDATE_COLOR_ENVIRONMENT: &str = "update_color_environment";
/// External action name for recovering a Project from an autosave snapshot.
pub const PROJECT_RECOVER_FROM_AUTOSAVE: &str = "recover_from_autosave";

/// External custom-action namespace for Sequence product operations.
pub const SEQUENCE_NAMESPACE: &str = "ui.sequence";

/// External action name for returning from a nested Sequence to its parent.
pub const SEQUENCE_RETURN_TO_PARENT: &str = "return_to_parent";
/// External action name for making the active Sequence the Project default.
pub const SEQUENCE_SET_ACTIVE_DEFAULT: &str = "set_active_default";
/// External action name for creating a new Sequence from Project defaults.
pub const SEQUENCE_NEW: &str = "new";
/// External action name for switching the active Sequence.
pub const SEQUENCE_SWITCH_ACTIVE: &str = "switch_active";
/// External action name for duplicating a Sequence.
pub const SEQUENCE_DUPLICATE: &str = "duplicate";
/// External action name for deleting a Sequence.
pub const SEQUENCE_DELETE: &str = "delete";
/// External action name for updating Sequence identity and settings.
pub const SEQUENCE_UPDATE_SETTINGS: &str = "update_settings";

/// External custom-action namespace for Export product operations.
pub const EXPORT_NAMESPACE: &str = "ui.export";

/// External action name for updating the app-session Export draft.
pub const EXPORT_EDIT_DRAFT: &str = "edit_draft";
/// External action name for admitting an immutable Timeline Export Snapshot.
pub const EXPORT_ENQUEUE: &str = "enqueue";
/// External action name for requesting cancellation of one Export attempt.
pub const EXPORT_CANCEL: &str = "cancel";
/// External action name for clearing bounded terminal Export evidence.
pub const EXPORT_CLEAR_TERMINAL_HISTORY: &str = "clear_terminal_history";

/// External custom-action namespace for visual Effect authoring operations.
pub const VISUAL_EFFECT_NAMESPACE: &str = "ui.visual_effect";

/// External action name for inserting one registered visual Effect on a Clip.
pub const VISUAL_EFFECT_ADD_TO_CLIP: &str = "add_to_clip";
/// External action name for selecting one visual Effect instance.
pub const VISUAL_EFFECT_SELECT: &str = "select";
/// External action name for changing one visual Effect enabled state.
pub const VISUAL_EFFECT_SET_ENABLED: &str = "set_enabled";
/// External action name for removing one visual Effect instance.
pub const VISUAL_EFFECT_REMOVE: &str = "remove";
/// External action name for moving one visual Effect relative to another instance.
pub const VISUAL_EFFECT_REORDER: &str = "reorder";
/// External action name for writing one stable-address visual Effect parameter.
pub const VISUAL_EFFECT_SET_PARAMETER_VALUE: &str = "set_parameter_value";

/// External custom-action namespace for Sequence audio authoring operations.
pub const AUDIO_NAMESPACE: &str = "ui.audio";

/// External action name for one atomic Audio Processor Rack edit.
pub const AUDIO_EDIT_PROCESSOR_RACK: &str = "edit_processor_rack";
/// External action name for inserting one product-visible built-in Processor.
pub const AUDIO_INSERT_BUILT_IN_PROCESSOR: &str = "insert_built_in_processor";
/// External action name for one normative Channel Strip edit.
pub const AUDIO_EDIT_CHANNEL_STRIP: &str = "edit_channel_strip";
/// External action name for one atomic Bus/Route graph edit.
pub const AUDIO_EDIT_ROUTING: &str = "edit_routing";
/// External action name for one stable-address audio automation edit.
pub const AUDIO_EDIT_AUTOMATION: &str = "edit_automation";
/// External action name for one placement-local Audio Component edit.
pub const AUDIO_EDIT_COMPONENT: &str = "edit_component";
/// External action name for one open-Session Track solo change.
pub const AUDIO_SET_TRACK_SOLO: &str = "set_track_solo";

/// External custom-action namespace for Project Asset Library operations.
pub const ASSET_NAMESPACE: &str = "ui.asset";

/// External action name for preparing one Asset for Timeline drag/drop.
pub const ASSET_PREPARE_DRAG: &str = "prepare_drag";
/// External action name for re-probing one Asset's audio Component evidence.
pub const ASSET_REFRESH_AUDIO_COMPONENTS: &str = "refresh_audio_components";
/// External action name for repairing one stable audio Component binding.
pub const ASSET_REBIND_AUDIO_COMPONENT: &str = "rebind_audio_component";
/// External action name for creating one generated Asset.
pub const ASSET_CREATE_GENERATED: &str = "create_generated";
/// External action name for creating one Asset Library folder.
pub const ASSET_CREATE_FOLDER: &str = "create_folder";
/// External action name for importing files into the Asset Library.
pub const ASSET_IMPORT_FILES: &str = "import_files";
/// External action name for relinking one Asset to a replacement file.
pub const ASSET_RELINK: &str = "relink";
/// External action name for changing persistent media interpretation.
pub const ASSET_SET_INTERPRETATION: &str = "set_interpretation";
/// External action name for renaming one Asset.
pub const ASSET_RENAME: &str = "rename";
/// External action name for renaming one Asset Library folder.
pub const ASSET_RENAME_FOLDER: &str = "rename_folder";
/// External action name for changing one Asset's proxy preference.
pub const ASSET_SET_PROXY_MODE: &str = "set_proxy_mode";
/// External action name for retiring Assets and deleting Library folders atomically.
pub const ASSET_REMOVE_ENTRIES: &str = "remove_entries";
/// External action name for moving Assets and folders atomically.
pub const ASSET_MOVE_ENTRIES: &str = "move_entries";

/// One closed product operation accepted by the App composition root.
///
/// Additional product domains may extend this algebra without making their
/// internal authoring or execution state part of the UI Interface.
#[derive(Debug, Clone, PartialEq)]
pub enum ProductAction {
    /// An operation owned by the active Timeline Interface.
    Timeline(TimelineProductAction),
    /// An operation owned by a Sequence-local visual Transition.
    VideoTransition(VideoTransitionProductAction),
    /// An operation owned by Sequence audio authoring.
    Audio(AudioProductAction),
    /// An operation owned by the Project Asset Library Interface.
    Asset(AssetProductAction),
    /// An operation owned by the Viewer product Interface.
    Viewer(ViewerProductAction),
    /// An operation owned by one Timeline Clip's authoring Interface.
    Clip(ClipProductAction),
    /// An operation owned by the Project lifecycle or authoring Interface.
    Project(ProjectProductAction),
    /// An operation owned by the Sequence management Interface.
    Sequence(SequenceProductAction),
    /// An operation owned by Export draft or execution orchestration.
    Export(ExportProductAction),
    /// An operation owned by Clip-local visual Effect authoring or selection.
    VisualEffect(VisualEffectProductAction),
}

/// Closed Project Asset Library operations.
#[derive(Debug, Clone, PartialEq)]
pub enum AssetProductAction {
    /// Prepare one validated Asset for a Timeline placement gesture.
    PrepareDrag(AssetTargetPayload),
    /// Re-probe physical audio stream evidence without changing logical identity.
    RefreshAudioComponents(AssetTargetPayload),
    /// Repair one stable logical audio Component's physical binding.
    RebindAudioComponent(AssetAudioComponentRebindPayload),
    /// Create one generated Asset in a Library folder or at root.
    CreateGenerated(AssetCreateGeneratedPayload),
    /// Create one Library folder under an optional parent.
    CreateFolder(AssetCreateFolderPayload),
    /// Import one or more files into a Library folder or at root.
    ImportFiles(Box<AssetImportFilesPayload>),
    /// Relink one stable Asset identity to a replacement file.
    Relink(AssetRelinkPayload),
    /// Replace one Asset's persistent media interpretation.
    SetInterpretation(AssetSetInterpretationPayload),
    /// Rename one Asset.
    Rename(AssetRenamePayload),
    /// Rename one Library folder.
    RenameFolder(AssetRenameFolderPayload),
    /// Change one video Asset's proxy preference.
    SetProxyMode(AssetSetProxyModePayload),
    /// Atomically retire visible Asset memberships and delete Library folders.
    RemoveEntries(Box<AssetLibrarySelectionPayload>),
    /// Atomically move Asset memberships and folders to one destination.
    MoveEntries(Box<AssetLibraryMovePayload>),
}

/// Closed Project lifecycle and authoring operations.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectProductAction {
    /// Create one Project at an explicitly selected publication path.
    CreateWithSettings(ProjectCreateWithSettingsPayload),
    /// Replace the complete template copied into future Sequences.
    UpdateNewSequenceDefaults(ProjectUpdateNewSequenceDefaultsPayload),
    /// Atomically replace the Project-wide color engine.
    UpdateColorEnvironment(ProjectUpdateColorEnvironmentPayload),
    /// Recover one exact durable autosave candidate.
    RecoverFromAutosave(ProjectRecoverFromAutosavePayload),
}

/// Closed Sequence management operations.
#[derive(Debug, Clone, PartialEq)]
pub enum SequenceProductAction {
    /// Return from a nested Sequence to its parent.
    ReturnToParent,
    /// Make the active Sequence the Project default.
    SetActiveDefault,
    /// Create a new Sequence from Project defaults.
    New,
    /// Switch the active Sequence.
    SwitchActive(SequenceTargetPayload),
    /// Duplicate one Sequence with fresh author identities.
    Duplicate(SequenceTargetPayload),
    /// Delete one Sequence when author references permit it.
    Delete(SequenceTargetPayload),
    /// Replace one Sequence's name and complete settings atomically.
    UpdateSettings(Box<SequenceUpdateSettingsPayload>),
}

/// Closed Export draft and execution operations.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportProductAction {
    /// Apply one exact edit to the app-session Export draft.
    EditDraft(Box<ExportDraftEdit>),
    /// Admit one immutable Timeline Export request.
    Enqueue(Box<TimelineExportRequest>),
    /// Request cancellation for one retained Export attempt.
    Cancel(JobId),
    /// Remove all retained terminal Export evidence.
    ClearTerminalHistory,
}

/// Closed Clip-local visual Effect operations.
#[derive(Debug, Clone, PartialEq)]
pub enum VisualEffectProductAction {
    /// Instantiate one currently registered definition and append it to a Clip.
    AddToClip(VisualEffectAddToClipPayload),
    /// Select one existing Effect instance in the App selection scope.
    Select(VisualEffectTargetPayload),
    /// Change whether one Effect instance participates in rendering.
    SetEnabled(VisualEffectSetEnabledPayload),
    /// Remove one Effect instance from its Clip.
    Remove(VisualEffectTargetPayload),
    /// Move one Effect relative to another stable instance identity.
    Reorder(VisualEffectReorderPayload),
    /// Write one parameter value through its stable author instance address.
    SetParameterValue(Box<VisualEffectSetParameterValuePayload>),
}

/// Closed authoring operations owned by one Timeline Clip.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipProductAction {
    /// Change whether one Clip participates in picture or sound execution.
    SetEnabled(ClipSetEnabledPayload),
    /// Change the generated source color of one Solid Color Clip.
    SetSolidColor(ClipSetSolidColorPayload),
    /// Atomically write one or more persistent Clip-owned parameters.
    WriteParameterValues(Box<ClipWriteParameterValuesPayload>),
    /// Insert, edit, or remove one complete numeric key by stable identity.
    EditNumericCurve(Box<ClipEditNumericCurvePayload>),
}

/// Closed Viewer operations that mutate product state.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewerProductAction {
    /// Change the active Sequence's authored preview resolution scale.
    SetPreviewResolutionScale(ViewerSetPreviewResolutionScalePayload),
}

/// Closed Sequence audio authoring operations.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioProductAction {
    /// Change one transient Track audition flag without mutating author state.
    SetTrackSolo(AudioTrackSoloPayload),
    /// Apply one exact owner-time curve mutation.
    EditAutomation(AudioAutomationEditRequest),
    /// Apply one stable-address placement-local Component mutation.
    EditComponent(AudioComponentEditRequest),
    /// Apply one stable-address Rack mutation in one author transaction.
    EditProcessorRack(AudioProcessorRackEditRequest),
    /// Resolve and insert one canonical product-visible built-in Processor.
    InsertBuiltInProcessor(AudioProcessorInsertBuiltInPayload),
    /// Apply one Track, Bus, or Program Output Channel Strip mutation.
    EditChannelStrip(AudioChannelStripEditRequest),
    /// Apply one Bus/Route graph mutation.
    EditRouting(AudioRoutingEditRequest),
}

/// Closed high-frequency Timeline interaction operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineProductAction {
    /// Select one Clip through the App-owned selection policy.
    SelectClip(TimelineSelectClipPayload),
    /// Move one Clip through the App-owned edit transaction.
    MoveClip(TimelineMoveClipPayload),
    /// Trim one or more Clip edges through one App-owned edit transaction.
    TrimClips(TimelineTrimClipsPayload),
    /// Seek the active Timeline through the transport Interface.
    Seek(TimelineSeekPayload),
}

/// Closed operations owned by Sequence-local visual Transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoTransitionProductAction {
    /// Select one existing Transition in the App selection scope.
    Select(VideoTransitionTargetPayload),
    /// Create the product-default Cross Dissolve on an adjacent Clip pair.
    CreateCrossDissolve(VideoTransitionCreateCrossDissolvePayload),
    /// Change one Transition's exact Sequence-local author range.
    SetRange(VideoTransitionSetRangePayload),
    /// Remove one Transition through a single author transaction.
    Remove(VideoTransitionTargetPayload),
}

/// Failure to decode a recognized external custom Action.
///
/// Unknown namespaces and names are not errors: they return `None` from
/// [`ProductAction::decode_external`] so an owning legacy or plugin Adapter may
/// inspect them. A recognized product name with a malformed payload fails
/// closed and must never fall through to another interpretation.
#[derive(Debug, thiserror::Error)]
#[error("invalid payload for recognized product action {namespace}.{name}: {source}")]
pub struct ProductActionDecodeError {
    namespace: String,
    name: String,
    #[source]
    source: serde_json::Error,
}

impl ProductActionDecodeError {
    pub(crate) fn dispatch_step_id(&self) -> String {
        let domain = match self.namespace.as_str() {
            TIMELINE_NAMESPACE => "timeline_ui_action",
            VIDEO_TRANSITION_NAMESPACE => "video_transition_action",
            AUDIO_NAMESPACE => "audio_action",
            ASSET_NAMESPACE => "asset_action",
            VIEWER_NAMESPACE => "viewer_action",
            CLIP_NAMESPACE => "clip_action",
            PROJECT_NAMESPACE => "project_action",
            SEQUENCE_NAMESPACE => "sequence_action",
            EXPORT_NAMESPACE => "export_action",
            VISUAL_EFFECT_NAMESPACE => "visual_effect_action",
            _ => "product_action",
        };
        format!("{domain}.{}", self.name)
    }
}

impl ProductAction {
    /// Decode one recognized product Action from the external intent envelope.
    ///
    /// This is the sole namespace/name/payload interpretation for the migrated
    /// product slice. Non-custom and unrecognized custom Actions return
    /// `Ok(None)`.
    pub fn decode_external(action: &Action) -> Result<Option<Self>, ProductActionDecodeError> {
        let Action::Custom { namespace, name, payload } = action else {
            return Ok(None);
        };
        match namespace.as_str() {
            TIMELINE_NAMESPACE => {
                let timeline_action = match name.as_str() {
                    TIMELINE_SELECT_CLIP => {
                        TimelineProductAction::SelectClip(decode_payload(namespace, name, payload)?)
                    }
                    TIMELINE_MOVE_CLIP => {
                        TimelineProductAction::MoveClip(decode_payload(namespace, name, payload)?)
                    }
                    TIMELINE_TRIM_CLIPS => {
                        TimelineProductAction::TrimClips(decode_payload(namespace, name, payload)?)
                    }
                    TIMELINE_SEEK => {
                        TimelineProductAction::Seek(decode_payload(namespace, name, payload)?)
                    }
                    _ => return Ok(None),
                };
                Ok(Some(Self::Timeline(timeline_action)))
            }
            VIDEO_TRANSITION_NAMESPACE => match name.as_str() {
                VIDEO_TRANSITION_SELECT => Ok(Some(Self::VideoTransition(
                    VideoTransitionProductAction::Select(decode_payload(namespace, name, payload)?),
                ))),
                VIDEO_TRANSITION_CREATE_CROSS_DISSOLVE => Ok(Some(Self::VideoTransition(
                    VideoTransitionProductAction::CreateCrossDissolve(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                VIDEO_TRANSITION_SET_RANGE => Ok(Some(Self::VideoTransition(
                    VideoTransitionProductAction::SetRange(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                VIDEO_TRANSITION_REMOVE => Ok(Some(Self::VideoTransition(
                    VideoTransitionProductAction::Remove(decode_payload(namespace, name, payload)?),
                ))),
                _ => Ok(None),
            },
            AUDIO_NAMESPACE => match name.as_str() {
                AUDIO_EDIT_AUTOMATION => Ok(Some(Self::Audio(AudioProductAction::EditAutomation(
                    decode_payload(namespace, name, payload)?,
                )))),
                AUDIO_EDIT_COMPONENT => Ok(Some(Self::Audio(AudioProductAction::EditComponent(
                    decode_payload(namespace, name, payload)?,
                )))),
                AUDIO_SET_TRACK_SOLO => Ok(Some(Self::Audio(AudioProductAction::SetTrackSolo(
                    decode_payload(namespace, name, payload)?,
                )))),
                AUDIO_EDIT_PROCESSOR_RACK => {
                    Ok(Some(Self::Audio(AudioProductAction::EditProcessorRack(
                        decode_payload(namespace, name, payload)?,
                    ))))
                }
                AUDIO_INSERT_BUILT_IN_PROCESSOR => Ok(Some(Self::Audio(
                    AudioProductAction::InsertBuiltInProcessor(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                AUDIO_EDIT_CHANNEL_STRIP => Ok(Some(Self::Audio(
                    AudioProductAction::EditChannelStrip(decode_payload(namespace, name, payload)?),
                ))),
                AUDIO_EDIT_ROUTING => Ok(Some(Self::Audio(AudioProductAction::EditRouting(
                    decode_payload(namespace, name, payload)?,
                )))),
                _ => Ok(None),
            },
            ASSET_NAMESPACE => match name.as_str() {
                ASSET_PREPARE_DRAG => Ok(Some(Self::Asset(AssetProductAction::PrepareDrag(
                    decode_payload(namespace, name, payload)?,
                )))),
                ASSET_REFRESH_AUDIO_COMPONENTS => Ok(Some(Self::Asset(
                    AssetProductAction::RefreshAudioComponents(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                ASSET_REBIND_AUDIO_COMPONENT => {
                    Ok(Some(Self::Asset(AssetProductAction::RebindAudioComponent(
                        decode_payload(namespace, name, payload)?,
                    ))))
                }
                ASSET_CREATE_GENERATED => Ok(Some(Self::Asset(
                    AssetProductAction::CreateGenerated(decode_payload(namespace, name, payload)?),
                ))),
                ASSET_CREATE_FOLDER => Ok(Some(Self::Asset(AssetProductAction::CreateFolder(
                    decode_payload(namespace, name, payload)?,
                )))),
                ASSET_IMPORT_FILES => Ok(Some(Self::Asset(AssetProductAction::ImportFiles(
                    Box::new(decode_payload(namespace, name, payload)?),
                )))),
                ASSET_RELINK => Ok(Some(Self::Asset(AssetProductAction::Relink(
                    decode_payload(namespace, name, payload)?,
                )))),
                ASSET_SET_INTERPRETATION => {
                    Ok(Some(Self::Asset(AssetProductAction::SetInterpretation(
                        decode_payload(namespace, name, payload)?,
                    ))))
                }
                ASSET_RENAME => Ok(Some(Self::Asset(AssetProductAction::Rename(
                    decode_payload(namespace, name, payload)?,
                )))),
                ASSET_RENAME_FOLDER => Ok(Some(Self::Asset(AssetProductAction::RenameFolder(
                    decode_payload(namespace, name, payload)?,
                )))),
                ASSET_SET_PROXY_MODE => Ok(Some(Self::Asset(AssetProductAction::SetProxyMode(
                    decode_payload(namespace, name, payload)?,
                )))),
                ASSET_REMOVE_ENTRIES => Ok(Some(Self::Asset(AssetProductAction::RemoveEntries(
                    Box::new(decode_payload(namespace, name, payload)?),
                )))),
                ASSET_MOVE_ENTRIES => Ok(Some(Self::Asset(AssetProductAction::MoveEntries(
                    Box::new(decode_payload(namespace, name, payload)?),
                )))),
                _ => Ok(None),
            },
            VIEWER_NAMESPACE => match name.as_str() {
                VIEWER_SET_PREVIEW_RESOLUTION_SCALE => Ok(Some(Self::Viewer(
                    ViewerProductAction::SetPreviewResolutionScale(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                _ => Ok(None),
            },
            CLIP_NAMESPACE => match name.as_str() {
                CLIP_SET_ENABLED => Ok(Some(Self::Clip(ClipProductAction::SetEnabled(
                    decode_payload(namespace, name, payload)?,
                )))),
                CLIP_SET_SOLID_COLOR => Ok(Some(Self::Clip(ClipProductAction::SetSolidColor(
                    decode_payload(namespace, name, payload)?,
                )))),
                CLIP_WRITE_PARAMETER_VALUES => {
                    Ok(Some(Self::Clip(ClipProductAction::WriteParameterValues(
                        Box::new(decode_payload(namespace, name, payload)?),
                    ))))
                }
                CLIP_EDIT_NUMERIC_CURVE => {
                    Ok(Some(Self::Clip(ClipProductAction::EditNumericCurve(
                        Box::new(decode_payload(namespace, name, payload)?),
                    ))))
                }
                _ => Ok(None),
            },
            PROJECT_NAMESPACE => match name.as_str() {
                PROJECT_CREATE_WITH_SETTINGS => Ok(Some(Self::Project(
                    ProjectProductAction::CreateWithSettings(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                PROJECT_UPDATE_NEW_SEQUENCE_DEFAULTS => Ok(Some(Self::Project(
                    ProjectProductAction::UpdateNewSequenceDefaults(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                PROJECT_UPDATE_COLOR_ENVIRONMENT => Ok(Some(Self::Project(
                    ProjectProductAction::UpdateColorEnvironment(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                PROJECT_RECOVER_FROM_AUTOSAVE => Ok(Some(Self::Project(
                    ProjectProductAction::RecoverFromAutosave(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                _ => Ok(None),
            },
            SEQUENCE_NAMESPACE => match name.as_str() {
                SEQUENCE_RETURN_TO_PARENT => {
                    decode_payload::<()>(namespace, name, payload)?;
                    Ok(Some(Self::Sequence(SequenceProductAction::ReturnToParent)))
                }
                SEQUENCE_SET_ACTIVE_DEFAULT => {
                    decode_payload::<()>(namespace, name, payload)?;
                    Ok(Some(Self::Sequence(
                        SequenceProductAction::SetActiveDefault,
                    )))
                }
                SEQUENCE_NEW => {
                    decode_payload::<()>(namespace, name, payload)?;
                    Ok(Some(Self::Sequence(SequenceProductAction::New)))
                }
                SEQUENCE_SWITCH_ACTIVE => Ok(Some(Self::Sequence(
                    SequenceProductAction::SwitchActive(decode_payload(namespace, name, payload)?),
                ))),
                SEQUENCE_DUPLICATE => Ok(Some(Self::Sequence(SequenceProductAction::Duplicate(
                    decode_payload(namespace, name, payload)?,
                )))),
                SEQUENCE_DELETE => Ok(Some(Self::Sequence(SequenceProductAction::Delete(
                    decode_payload(namespace, name, payload)?,
                )))),
                SEQUENCE_UPDATE_SETTINGS => {
                    Ok(Some(Self::Sequence(SequenceProductAction::UpdateSettings(
                        Box::new(decode_payload(namespace, name, payload)?),
                    ))))
                }
                _ => Ok(None),
            },
            EXPORT_NAMESPACE => match name.as_str() {
                EXPORT_EDIT_DRAFT => Ok(Some(Self::Export(ExportProductAction::EditDraft(
                    Box::new(decode_payload(namespace, name, payload)?),
                )))),
                EXPORT_ENQUEUE => Ok(Some(Self::Export(ExportProductAction::Enqueue(Box::new(
                    decode_payload(namespace, name, payload)?,
                ))))),
                EXPORT_CANCEL => {
                    let target: ExportCancelWirePayload = decode_payload(namespace, name, payload)?;
                    Ok(Some(Self::Export(ExportProductAction::Cancel(
                        target.job_id,
                    ))))
                }
                EXPORT_CLEAR_TERMINAL_HISTORY => {
                    decode_payload::<()>(namespace, name, payload)?;
                    Ok(Some(Self::Export(
                        ExportProductAction::ClearTerminalHistory,
                    )))
                }
                _ => Ok(None),
            },
            VISUAL_EFFECT_NAMESPACE => match name.as_str() {
                VISUAL_EFFECT_ADD_TO_CLIP => Ok(Some(Self::VisualEffect(
                    VisualEffectProductAction::AddToClip(decode_payload(namespace, name, payload)?),
                ))),
                VISUAL_EFFECT_SELECT => Ok(Some(Self::VisualEffect(
                    VisualEffectProductAction::Select(decode_payload(namespace, name, payload)?),
                ))),
                VISUAL_EFFECT_SET_ENABLED => Ok(Some(Self::VisualEffect(
                    VisualEffectProductAction::SetEnabled(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                VISUAL_EFFECT_REMOVE => Ok(Some(Self::VisualEffect(
                    VisualEffectProductAction::Remove(decode_payload(namespace, name, payload)?),
                ))),
                VISUAL_EFFECT_REORDER => Ok(Some(Self::VisualEffect(
                    VisualEffectProductAction::Reorder(decode_payload(namespace, name, payload)?),
                ))),
                VISUAL_EFFECT_SET_PARAMETER_VALUE => Ok(Some(Self::VisualEffect(
                    VisualEffectProductAction::SetParameterValue(Box::new(decode_payload(
                        namespace, name, payload,
                    )?)),
                ))),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// Encode a typed product Action for an external Widget or plugin envelope.
    ///
    /// Product dispatch immediately lowers this envelope back through
    /// [`Self::decode_external`]; no other App Module may reinterpret these wire
    /// names or payloads.
    pub fn into_external_action(self) -> Action {
        let (namespace, name, payload) = match self {
            Self::Audio(AudioProductAction::SetTrackSolo(payload)) => (
                AUDIO_NAMESPACE,
                AUDIO_SET_TRACK_SOLO,
                serde_json::json!(payload),
            ),
            Self::Audio(AudioProductAction::EditAutomation(request)) => (
                AUDIO_NAMESPACE,
                AUDIO_EDIT_AUTOMATION,
                serde_json::json!(request),
            ),
            Self::Audio(AudioProductAction::EditComponent(request)) => (
                AUDIO_NAMESPACE,
                AUDIO_EDIT_COMPONENT,
                serde_json::json!(request),
            ),
            Self::Asset(AssetProductAction::PrepareDrag(payload)) => (
                ASSET_NAMESPACE,
                ASSET_PREPARE_DRAG,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::RefreshAudioComponents(payload)) => (
                ASSET_NAMESPACE,
                ASSET_REFRESH_AUDIO_COMPONENTS,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::RebindAudioComponent(payload)) => (
                ASSET_NAMESPACE,
                ASSET_REBIND_AUDIO_COMPONENT,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::CreateGenerated(payload)) => (
                ASSET_NAMESPACE,
                ASSET_CREATE_GENERATED,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::CreateFolder(payload)) => (
                ASSET_NAMESPACE,
                ASSET_CREATE_FOLDER,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::ImportFiles(payload)) => (
                ASSET_NAMESPACE,
                ASSET_IMPORT_FILES,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::Relink(payload)) => {
                (ASSET_NAMESPACE, ASSET_RELINK, serde_json::json!(payload))
            }
            Self::Asset(AssetProductAction::SetInterpretation(payload)) => (
                ASSET_NAMESPACE,
                ASSET_SET_INTERPRETATION,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::Rename(payload)) => {
                (ASSET_NAMESPACE, ASSET_RENAME, serde_json::json!(payload))
            }
            Self::Asset(AssetProductAction::RenameFolder(payload)) => (
                ASSET_NAMESPACE,
                ASSET_RENAME_FOLDER,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::SetProxyMode(payload)) => (
                ASSET_NAMESPACE,
                ASSET_SET_PROXY_MODE,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::RemoveEntries(payload)) => (
                ASSET_NAMESPACE,
                ASSET_REMOVE_ENTRIES,
                serde_json::json!(payload),
            ),
            Self::Asset(AssetProductAction::MoveEntries(payload)) => (
                ASSET_NAMESPACE,
                ASSET_MOVE_ENTRIES,
                serde_json::json!(payload),
            ),
            Self::Timeline(TimelineProductAction::SelectClip(payload)) => (
                TIMELINE_NAMESPACE,
                TIMELINE_SELECT_CLIP,
                serde_json::json!({
                    "clip_id": payload.clip_id,
                    "mode": payload.mode,
                }),
            ),
            Self::Timeline(TimelineProductAction::MoveClip(payload)) => (
                TIMELINE_NAMESPACE,
                TIMELINE_MOVE_CLIP,
                serde_json::json!({
                    "target_track_id": payload.target_track_id,
                    "is_video_track": payload.is_video_track,
                    "clip_id": payload.clip_id,
                    "frame": payload.frame,
                }),
            ),
            Self::Timeline(TimelineProductAction::TrimClips(payload)) => (
                TIMELINE_NAMESPACE,
                TIMELINE_TRIM_CLIPS,
                serde_json::json!({
                    "clip_ids": payload.clip_ids,
                    "edge": payload.edge,
                    "frame": payload.frame,
                }),
            ),
            Self::Timeline(TimelineProductAction::Seek(payload)) => (
                TIMELINE_NAMESPACE,
                TIMELINE_SEEK,
                serde_json::json!({
                    "frame": payload.frame,
                    "source": payload.source,
                }),
            ),
            Self::VideoTransition(VideoTransitionProductAction::Select(payload)) => (
                VIDEO_TRANSITION_NAMESPACE,
                VIDEO_TRANSITION_SELECT,
                serde_json::json!(payload),
            ),
            Self::VideoTransition(VideoTransitionProductAction::CreateCrossDissolve(payload)) => (
                VIDEO_TRANSITION_NAMESPACE,
                VIDEO_TRANSITION_CREATE_CROSS_DISSOLVE,
                serde_json::json!(payload),
            ),
            Self::VideoTransition(VideoTransitionProductAction::SetRange(payload)) => (
                VIDEO_TRANSITION_NAMESPACE,
                VIDEO_TRANSITION_SET_RANGE,
                serde_json::json!(payload),
            ),
            Self::VideoTransition(VideoTransitionProductAction::Remove(payload)) => (
                VIDEO_TRANSITION_NAMESPACE,
                VIDEO_TRANSITION_REMOVE,
                serde_json::json!(payload),
            ),
            Self::Audio(AudioProductAction::EditProcessorRack(request)) => (
                AUDIO_NAMESPACE,
                AUDIO_EDIT_PROCESSOR_RACK,
                serde_json::json!(request),
            ),
            Self::Audio(AudioProductAction::InsertBuiltInProcessor(payload)) => (
                AUDIO_NAMESPACE,
                AUDIO_INSERT_BUILT_IN_PROCESSOR,
                serde_json::json!(payload),
            ),
            Self::Audio(AudioProductAction::EditChannelStrip(request)) => (
                AUDIO_NAMESPACE,
                AUDIO_EDIT_CHANNEL_STRIP,
                serde_json::json!(request),
            ),
            Self::Audio(AudioProductAction::EditRouting(request)) => (
                AUDIO_NAMESPACE,
                AUDIO_EDIT_ROUTING,
                serde_json::json!(request),
            ),
            Self::Viewer(ViewerProductAction::SetPreviewResolutionScale(payload)) => (
                VIEWER_NAMESPACE,
                VIEWER_SET_PREVIEW_RESOLUTION_SCALE,
                serde_json::json!(payload),
            ),
            Self::Clip(ClipProductAction::SetEnabled(payload)) => {
                (CLIP_NAMESPACE, CLIP_SET_ENABLED, serde_json::json!(payload))
            }
            Self::Clip(ClipProductAction::SetSolidColor(payload)) => (
                CLIP_NAMESPACE,
                CLIP_SET_SOLID_COLOR,
                serde_json::json!(payload),
            ),
            Self::Clip(ClipProductAction::WriteParameterValues(payload)) => (
                CLIP_NAMESPACE,
                CLIP_WRITE_PARAMETER_VALUES,
                serde_json::json!(payload),
            ),
            Self::Clip(ClipProductAction::EditNumericCurve(payload)) => (
                CLIP_NAMESPACE,
                CLIP_EDIT_NUMERIC_CURVE,
                serde_json::json!(payload),
            ),
            Self::Project(ProjectProductAction::CreateWithSettings(payload)) => (
                PROJECT_NAMESPACE,
                PROJECT_CREATE_WITH_SETTINGS,
                serde_json::json!(payload),
            ),
            Self::Project(ProjectProductAction::UpdateNewSequenceDefaults(payload)) => (
                PROJECT_NAMESPACE,
                PROJECT_UPDATE_NEW_SEQUENCE_DEFAULTS,
                serde_json::json!(payload),
            ),
            Self::Project(ProjectProductAction::UpdateColorEnvironment(payload)) => (
                PROJECT_NAMESPACE,
                PROJECT_UPDATE_COLOR_ENVIRONMENT,
                serde_json::json!(payload),
            ),
            Self::Project(ProjectProductAction::RecoverFromAutosave(payload)) => (
                PROJECT_NAMESPACE,
                PROJECT_RECOVER_FROM_AUTOSAVE,
                serde_json::json!(payload),
            ),
            Self::Sequence(SequenceProductAction::ReturnToParent) => (
                SEQUENCE_NAMESPACE,
                SEQUENCE_RETURN_TO_PARENT,
                serde_json::Value::Null,
            ),
            Self::Sequence(SequenceProductAction::SetActiveDefault) => (
                SEQUENCE_NAMESPACE,
                SEQUENCE_SET_ACTIVE_DEFAULT,
                serde_json::Value::Null,
            ),
            Self::Sequence(SequenceProductAction::New) => {
                (SEQUENCE_NAMESPACE, SEQUENCE_NEW, serde_json::Value::Null)
            }
            Self::Sequence(SequenceProductAction::SwitchActive(payload)) => (
                SEQUENCE_NAMESPACE,
                SEQUENCE_SWITCH_ACTIVE,
                serde_json::json!(payload),
            ),
            Self::Sequence(SequenceProductAction::Duplicate(payload)) => (
                SEQUENCE_NAMESPACE,
                SEQUENCE_DUPLICATE,
                serde_json::json!(payload),
            ),
            Self::Sequence(SequenceProductAction::Delete(payload)) => (
                SEQUENCE_NAMESPACE,
                SEQUENCE_DELETE,
                serde_json::json!(payload),
            ),
            Self::Sequence(SequenceProductAction::UpdateSettings(payload)) => (
                SEQUENCE_NAMESPACE,
                SEQUENCE_UPDATE_SETTINGS,
                serde_json::json!(payload),
            ),
            Self::Export(ExportProductAction::EditDraft(edit)) => {
                (EXPORT_NAMESPACE, EXPORT_EDIT_DRAFT, serde_json::json!(edit))
            }
            Self::Export(ExportProductAction::Enqueue(request)) => {
                (EXPORT_NAMESPACE, EXPORT_ENQUEUE, serde_json::json!(request))
            }
            Self::Export(ExportProductAction::Cancel(job_id)) => (
                EXPORT_NAMESPACE,
                EXPORT_CANCEL,
                serde_json::json!(ExportCancelWirePayload { job_id }),
            ),
            Self::Export(ExportProductAction::ClearTerminalHistory) => (
                EXPORT_NAMESPACE,
                EXPORT_CLEAR_TERMINAL_HISTORY,
                serde_json::Value::Null,
            ),
            Self::VisualEffect(VisualEffectProductAction::AddToClip(payload)) => (
                VISUAL_EFFECT_NAMESPACE,
                VISUAL_EFFECT_ADD_TO_CLIP,
                serde_json::json!(payload),
            ),
            Self::VisualEffect(VisualEffectProductAction::Select(payload)) => (
                VISUAL_EFFECT_NAMESPACE,
                VISUAL_EFFECT_SELECT,
                serde_json::json!(payload),
            ),
            Self::VisualEffect(VisualEffectProductAction::SetEnabled(payload)) => (
                VISUAL_EFFECT_NAMESPACE,
                VISUAL_EFFECT_SET_ENABLED,
                serde_json::json!(payload),
            ),
            Self::VisualEffect(VisualEffectProductAction::Remove(payload)) => (
                VISUAL_EFFECT_NAMESPACE,
                VISUAL_EFFECT_REMOVE,
                serde_json::json!(payload),
            ),
            Self::VisualEffect(VisualEffectProductAction::Reorder(payload)) => (
                VISUAL_EFFECT_NAMESPACE,
                VISUAL_EFFECT_REORDER,
                serde_json::json!(payload),
            ),
            Self::VisualEffect(VisualEffectProductAction::SetParameterValue(payload)) => (
                VISUAL_EFFECT_NAMESPACE,
                VISUAL_EFFECT_SET_PARAMETER_VALUE,
                serde_json::json!(payload),
            ),
        };
        Action::Custom {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            payload,
        }
    }
}

/// Stable target of a single-Asset product operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetTargetPayload {
    /// Canonical Asset identity.
    pub asset_id: AssetId,
}

/// Repair one logical audio Component's physical stream binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetAudioComponentRebindPayload {
    /// Asset that owns the stable logical Component.
    pub asset_id: AssetId,
    /// Logical Component identity preserved by the operation.
    pub component_id: AudioSourceComponentId,
    /// Absolute stream index selected from current probe evidence.
    pub stream_index: u32,
}

/// Create one generated Asset in an optional Library folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetCreateGeneratedPayload {
    /// Exact generated-content kind.
    pub kind: GeneratedAssetKind,
    /// Target folder, or `None` for the root/unfiled view.
    pub folder_id: Option<String>,
}

/// Create one Asset Library folder under an optional parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetCreateFolderPayload {
    /// Parent folder, or `None` for a root-level folder.
    pub parent_folder_id: Option<String>,
}

/// Import files into one Asset Library location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetImportFilesPayload {
    /// Media file paths selected by the user or dropped onto the Asset browser.
    pub paths: Vec<PathBuf>,
    /// Target folder, or `None` for the root/unfiled view.
    pub folder_id: Option<String>,
}

/// Relink one stable Asset identity to a replacement media file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRelinkPayload {
    /// Asset to relink.
    pub asset_id: AssetId,
    /// Replacement media path.
    pub path: PathBuf,
}

/// Replace one Asset's persistent media interpretation intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSetInterpretationPayload {
    /// Asset to update.
    pub asset_id: AssetId,
    /// Persistent user intent stored independently of transient probe evidence.
    pub interpretation: AssetMediaInterpretation,
}

/// Rename one Asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRenamePayload {
    /// Asset to rename.
    pub asset_id: AssetId,
    /// New user-facing name; dispatch trims surrounding whitespace.
    pub name: String,
}

/// Rename one Asset Library folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRenameFolderPayload {
    /// Folder to rename.
    pub folder_id: String,
    /// New user-facing name; dispatch trims surrounding whitespace.
    pub name: String,
}

/// Change one video Asset's proxy preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSetProxyModePayload {
    /// Video Asset to update.
    pub asset_id: AssetId,
    /// Whether playback should prefer a qualified proxy.
    pub enabled: bool,
}

/// One atomic Asset Library removal selection.
///
/// Asset identities are retired from visible Library membership; strong
/// Sequence, history, proxy, and recovery references remain addressable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetLibrarySelectionPayload {
    /// Asset memberships to retire.
    pub asset_ids: Vec<AssetId>,
    /// Library folders to delete.
    pub folder_ids: Vec<String>,
}

/// One atomic Asset Library organization edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetLibraryMovePayload {
    /// Asset memberships to move.
    pub asset_ids: Vec<AssetId>,
    /// Library folders to reparent.
    pub folder_ids: Vec<String>,
    /// Destination folder/parent, or `None` for the root level.
    pub target_folder_id: Option<String>,
}

/// Product-visible built-in Processor choice.
///
/// This closed presentation catalog is deliberately separate from persistent
/// [`mondrian_timeline::audio::AudioProcessorDefinitionRef`]. Projects retain
/// definition identity and schema snapshots; the menu only exposes built-ins
/// whose production resolver and editor contract are currently complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioProcessorBuiltInPreset {
    /// Stateless decibel gain.
    Gain,
    /// Linked-channel sample-peak limiter with compensated lookahead.
    LookaheadLimiter,
}

/// Insert one canonical built-in at a stable Rack-relative placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProcessorInsertBuiltInPayload {
    /// Rack receiving the new Processor.
    pub address: mondrian_timeline::AudioProcessorRackAddress,
    /// Product-visible canonical built-in.
    pub preset: AudioProcessorBuiltInPreset,
    /// Stable position inside the Rack.
    pub placement: mondrian_timeline::AudioProcessorRackPlacement,
}

/// Transient Track audition intent owned by the open App Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioTrackSoloPayload {
    /// Audio Track whose program contribution changes audition state.
    pub track_id: TrackId,
    /// Whether the Track participates in the current solo set.
    pub soloed: bool,
}

fn decode_payload<T: serde::de::DeserializeOwned>(
    namespace: &str,
    name: &str,
    payload: &serde_json::Value,
) -> Result<T, ProductActionDecodeError> {
    serde_json::from_value(payload.clone()).map_err(|source| ProductActionDecodeError {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        source,
    })
}

/// Select a Clip in the active Sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineClipSelectionModePayload {
    /// Replace the current Clip selection.
    Replace,
    /// Toggle the complete Clip selection unit.
    Toggle,
    /// Preserve an existing multi-selection for a context command.
    Preserve,
}

/// Select one Clip in the active Sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSelectClipPayload {
    /// Clip selected by the input Adapter.
    pub clip_id: ClipId,
    /// Selection-set operation requested by the input Adapter.
    pub mode: TimelineClipSelectionModePayload,
}

/// Move one Clip to a target Track and frame in the active Sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMoveClipPayload {
    /// Track that should own the Clip after the move.
    pub target_track_id: TrackId,
    /// Whether `target_track_id` is a video Track rather than an audio Track.
    pub is_video_track: bool,
    /// Clip being moved.
    pub clip_id: ClipId,
    /// Target Sequence evaluation frame for the Clip start.
    pub frame: i64,
}

/// Clip edge addressed by a Timeline trim interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineTrimPayloadEdge {
    /// Trim the inclusive Clip start.
    In,
    /// Trim the exclusive Clip end.
    Out,
}

/// Trim one or more Clip edges to one Sequence frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineTrimClipsPayload {
    /// Clips being trimmed in one author transaction.
    pub clip_ids: Vec<ClipId>,
    /// Edge that should be trimmed.
    pub edge: TimelineTrimPayloadEdge,
    /// Target Sequence evaluation frame for every selected edge.
    pub frame: i64,
}

/// User interaction source for a Timeline seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineSeekSource {
    /// Continuous playhead or ruler pointer drag.
    PointerDrag,
    /// Stable click, keyboard command, programmatic seek, or drag release.
    Settled,
}

/// Seek the active Timeline to one Sequence frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSeekPayload {
    /// Target Sequence evaluation frame.
    pub frame: i64,
    /// User interaction source for this seek.
    pub source: TimelineSeekSource,
}

/// Create a Project at a user-selected path with explicit initial settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCreateWithSettingsPayload {
    /// Target `.mdp` Project container path.
    pub project_file: PathBuf,
    /// Initial Project and Sequence display name.
    pub name: String,
    /// Initial Sequence settings.
    pub sequence_settings: SequenceSettings,
    /// Project-wide color engine shared by every Sequence.
    pub color_environment: ProjectColorEnvironment,
    /// Initial Project-level settings.
    pub project_settings: ProjectSettings,
}

/// Replace the complete template copied into future Sequences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdateNewSequenceDefaultsPayload {
    /// Complete validated Sequence settings template.
    pub settings: SequenceSettings,
}

/// Atomically replace the Project-wide color engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdateColorEnvironmentPayload {
    /// Complete version-pinned engine environment.
    pub color_environment: ProjectColorEnvironment,
}

/// Recover a Project from one exact durable autosave candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRecoverFromAutosavePayload {
    /// Exact discovery evidence selected by the user.
    pub candidate: CrashRecoveryCandidate,
}

/// Target one Project Sequence from a product operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceTargetPayload {
    /// Sequence to operate on.
    pub sequence_id: SequenceId,
}

/// Apply edited identity and settings to one Sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceUpdateSettingsPayload {
    /// Sequence whose name and settings should be replaced.
    pub sequence_id: SequenceId,
    /// User-facing Sequence name.
    pub name: String,
    /// Complete Sequence settings after applying shell-local edits.
    pub settings: SequenceSettings,
}

/// One exact edit to the app-session Export draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportDraftEdit {
    /// Reset the materialized draft from a stable built-in preset.
    BuiltinPreset(BuiltinExportPreset),
    /// Replace the complete typed delivery draft after one form edit.
    Preset(ExportPreset),
    /// Select the Sequence to export; `None` follows the active Sequence.
    Sequence(Option<SequenceId>),
    /// Select the Timeline range to export.
    Range(TimelineExportRange),
    /// Replace the user-entered output path without syntactic correction.
    OutputPath(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportCancelWirePayload {
    job_id: JobId,
}

/// Clip and registered definition selected for one visual Effect insertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualEffectAddToClipPayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Definition identity to instantiate from the current Effect registry.
    pub effect_type: EffectType,
}

/// Stable identity of one visual Effect instance on one Clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualEffectTargetPayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Stable Effect instance identity owned by the Clip.
    pub effect_id: EffectId,
}

/// Change one visual Effect's enabled state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualEffectSetEnabledPayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Stable Effect instance identity owned by the Clip.
    pub effect_id: EffectId,
    /// Whether the Effect participates in prepared visual execution.
    pub enabled: bool,
}

/// Move one visual Effect relative to another stable instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualEffectReorderPayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Stable Effect instance being moved.
    pub effect_id: EffectId,
    /// Stable relative placement requested inside the same Effect chain.
    pub placement: EffectRelativePlacement,
}

/// Write one visual Effect parameter through stable author identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualEffectSetParameterValuePayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Stable Effect instance that owns the parameter.
    pub effect_id: EffectId,
    /// Stable parameter instance plus definition identity; never a property-path alias.
    pub parameter: AnimationParameterAddress,
    /// Value to write statically or as a key at the current Clip-local author time.
    pub value: PropertyValue,
}

/// Explicit author policy when real endpoint handles cannot satisfy a visual
/// Transition range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoTransitionHandlePolicy {
    /// Preserve the requested range or reject the complete operation.
    Reject,
    /// Intersect with proven endpoint extents while retaining the edit cut.
    ShortenToAvailable,
}

/// Address one Sequence-local visual Transition by stable identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTransitionTargetPayload {
    /// Stable Transition identity; Track membership is derived at dispatch.
    pub transition_id: VideoTransitionId,
}

/// Create the product-default Cross Dissolve between an ordered adjacent edit
/// pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTransitionCreateCrossDissolvePayload {
    /// Clip ending at the shared edit.
    pub left_clip_id: ClipId,
    /// Clip beginning at the shared edit.
    pub right_clip_id: ClipId,
    /// Explicit behavior when current media/nested handles are insufficient.
    pub handle_policy: VideoTransitionHandlePolicy,
}

/// Change one visual Transition through exact Sequence-local author time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTransitionSetRangePayload {
    /// Stable Transition identity; endpoint Track membership is derived.
    pub transition_id: VideoTransitionId,
    /// Exact requested half-open Sequence-local range.
    pub requested_range: TimelineTimeRange,
    /// Explicit behavior when current media/nested handles are insufficient.
    pub handle_policy: VideoTransitionHandlePolicy,
}

/// Change one Clip's enabled state through its canonical identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipSetEnabledPayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Whether the Clip participates in picture or sound execution.
    pub enabled: bool,
}

/// Change the generated source color of one Solid Color Clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipSetSolidColorPayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Straight-alpha source color authored by the generated Clip.
    pub color: Color,
}

/// One stable-address parameter value in an atomic Clip write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipParameterValueWrite {
    /// Stable owner-local parameter instance and Definition identity.
    pub parameter: AnimationParameterAddress,
    /// Typed value to write statically or at the current Clip-local author time.
    pub value: PropertyValue,
}

/// Atomically write one or more persistent parameters owned directly by a Clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipWriteParameterValuesPayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Non-empty, duplicate-free parameter writes committed as one gesture.
    pub writes: Vec<ClipParameterValueWrite>,
}

/// One finite normalized point in a Clip curve editor viewport.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipNormalizedCurvePointPayload {
    /// Ratio across the visible Clip-local author span, in `0..=1`.
    pub time_ratio: f64,
    /// Ratio across the Parameter Schema soft range, in `0..=1`.
    pub value_ratio: f64,
}

/// One stable-key numeric curve edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClipCurveEditPayload {
    /// Insert a key, or edit one existing complete key.
    Upsert {
        /// Existing stable identity; `None` inserts or resolves by exact time.
        keyframe_id: Option<KeyframeId>,
        /// New normalized editor point.
        point: ClipNormalizedCurvePointPayload,
    },
    /// Remove one complete key by stable identity.
    Remove {
        /// Stable key identity captured by the current projection.
        keyframe_id: KeyframeId,
    },
}

/// Edit one Clip-owned numeric curve through stable author identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipEditNumericCurvePayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Stable owner-local parameter instance and Definition identity.
    pub parameter: AnimationParameterAddress,
    /// Incremental key edit; never a replacement curve or point index.
    pub edit: ClipCurveEditPayload,
}

/// Change the active Viewer preview resolution scale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerSetPreviewResolutionScalePayload {
    /// Preview resolution scale requested by the UI.
    pub scale: f32,
}

/// Stable read-only admission projection for migrated product Actions.
///
/// Its facts are deliberately private. UI Adapters can ask whether a typed
/// operation is currently useful, but cannot observe or reinterpret Sequence,
/// Track, Clip, authoring-session, or execution internals.
pub struct ProductActionAvailability<'a> {
    state: &'a AppState,
}

#[derive(Debug, Clone, Copy)]
struct ClipAdmissionFacts {
    source_track_unlocked: bool,
    is_video_track: bool,
    position: TimelineTime,
    end: Option<TimelineTime>,
}

impl<'a> ProductActionAvailability<'a> {
    fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    /// Return whether a typed product operation has a useful current target.
    ///
    /// This is an early, read-only UI projection. The owning mutation or
    /// transport Interface still revalidates authoritative state at dispatch.
    pub fn allows(&self, action: &ProductAction) -> bool {
        match action {
            ProductAction::Timeline(action) => self.allows_timeline(action),
            ProductAction::VideoTransition(action) => self.allows_video_transition(action),
            ProductAction::Audio(_) => self.state.active_sequence().is_some(),
            ProductAction::Asset(action) => self.allows_asset(action),
            ProductAction::Viewer(action) => self.allows_viewer(action),
            ProductAction::Clip(action) => self.allows_clip(action),
            ProductAction::Project(action) => self.allows_project(action),
            ProductAction::Sequence(action) => self.allows_sequence(action),
            ProductAction::Export(action) => self.allows_export(action),
            ProductAction::VisualEffect(action) => self.allows_visual_effect(action),
        }
    }

    fn allows_asset(&self, action: &AssetProductAction) -> bool {
        let Some(library) = self.state.asset_library_handle() else {
            return false;
        };
        let asset_record = |asset_id| {
            library
                .get_asset(asset_id)
                .ok()
                .flatten()
                .filter(|asset| !asset.membership.is_retired())
        };
        let asset_exists = |asset_id| asset_record(asset_id).is_some();
        let folder_id_valid = |folder_id: &Option<String>| {
            folder_id.as_ref().is_none_or(|folder_id| !folder_id.trim().is_empty())
        };
        match action {
            AssetProductAction::PrepareDrag(payload)
            | AssetProductAction::RefreshAudioComponents(payload) => asset_exists(payload.asset_id),
            AssetProductAction::RebindAudioComponent(payload) => asset_exists(payload.asset_id),
            AssetProductAction::CreateGenerated(payload) => folder_id_valid(&payload.folder_id),
            AssetProductAction::CreateFolder(payload) => folder_id_valid(&payload.parent_folder_id),
            AssetProductAction::ImportFiles(payload) => {
                !payload.paths.is_empty()
                    && payload.paths.iter().all(|path| !path.as_os_str().is_empty())
                    && folder_id_valid(&payload.folder_id)
            }
            AssetProductAction::Relink(payload) => {
                asset_exists(payload.asset_id) && !payload.path.as_os_str().is_empty()
            }
            AssetProductAction::SetInterpretation(payload) => asset_record(payload.asset_id)
                .is_some_and(|asset| asset.interpretation != payload.interpretation),
            AssetProductAction::Rename(payload) => {
                asset_record(payload.asset_id).is_some_and(|asset| {
                    !payload.name.trim().is_empty() && asset.name != payload.name.trim()
                })
            }
            AssetProductAction::RenameFolder(payload) => {
                !payload.folder_id.trim().is_empty()
                    && !payload.name.trim().is_empty()
                    && library.list_folders().ok().is_some_and(|folders| {
                        folders.iter().any(|folder| {
                            folder.id == payload.folder_id && folder.name != payload.name.trim()
                        })
                    })
            }
            AssetProductAction::SetProxyMode(payload) => asset_record(payload.asset_id)
                .is_some_and(|asset| {
                    matches!(asset.kind, mondrian_assets::AssetKind::Video)
                        && self.state.proxy_mode_assets().contains(&payload.asset_id)
                            != payload.enabled
                }),
            AssetProductAction::RemoveEntries(payload) => {
                !payload.asset_ids.is_empty() || !payload.folder_ids.is_empty()
            }
            AssetProductAction::MoveEntries(payload) => {
                (!payload.asset_ids.is_empty() || !payload.folder_ids.is_empty())
                    && folder_id_valid(&payload.target_folder_id)
            }
        }
    }

    fn allows_timeline(&self, action: &TimelineProductAction) -> bool {
        let Some(sequence) = self.state.active_sequence() else {
            return false;
        };
        match action {
            TimelineProductAction::SelectClip(payload) => {
                clip_admission_facts(sequence, payload.clip_id).is_some()
            }
            TimelineProductAction::MoveClip(payload) => {
                clip_admission_facts(sequence, payload.clip_id).is_some_and(|clip| {
                    clip.source_track_unlocked && clip.is_video_track == payload.is_video_track
                }) && track_is_unlocked(sequence, payload.target_track_id, payload.is_video_track)
            }
            TimelineProductAction::TrimClips(payload) => allows_trim(sequence, payload),
            TimelineProductAction::Seek(payload) => payload.frame >= 0,
        }
    }

    fn allows_video_transition(&self, action: &VideoTransitionProductAction) -> bool {
        match action {
            VideoTransitionProductAction::Select(payload) => {
                self.state.video_transition_selection_available(payload.transition_id)
            }
            VideoTransitionProductAction::CreateCrossDissolve(payload) => self
                .state
                .video_transition_creation_available(payload.left_clip_id, payload.right_clip_id),
            VideoTransitionProductAction::SetRange(payload) => {
                self.state.video_transition_range_edit_available(
                    payload.transition_id,
                    payload.requested_range,
                )
            }
            VideoTransitionProductAction::Remove(payload) => {
                self.state.video_transition_removal_available(payload.transition_id)
            }
        }
    }

    fn allows_viewer(&self, action: &ViewerProductAction) -> bool {
        let Some(_sequence) = self.state.active_sequence() else {
            return false;
        };
        match action {
            ViewerProductAction::SetPreviewResolutionScale(_) => true,
        }
    }

    fn allows_clip(&self, action: &ClipProductAction) -> bool {
        let Some(_sequence) = self.state.active_sequence() else {
            return false;
        };
        match action {
            ClipProductAction::SetEnabled(payload) => self
                .state
                .clip_enabled_write_would_change(payload.clip_id, payload.enabled)
                .unwrap_or(false),
            ClipProductAction::SetSolidColor(payload) => self
                .state
                .clip_solid_color_write_would_change(payload.clip_id, payload.color)
                .unwrap_or(false),
            ClipProductAction::WriteParameterValues(payload) => {
                self.state.clip_parameter_writes_would_change(payload).unwrap_or(false)
            }
            ClipProductAction::EditNumericCurve(payload) => {
                self.state.clip_numeric_curve_payload_would_change(payload).unwrap_or(false)
            }
        }
    }

    fn allows_project(&self, action: &ProjectProductAction) -> bool {
        match action {
            ProjectProductAction::CreateWithSettings(payload) => {
                !payload.project_file.as_os_str().is_empty()
            }
            ProjectProductAction::RecoverFromAutosave(payload) => {
                recovery_candidate_is_addressable(&payload.candidate)
            }
            ProjectProductAction::UpdateNewSequenceDefaults(payload) => {
                self.state.authoring.as_ref().is_some_and(|session| {
                    session.document().new_sequence_defaults != payload.settings
                })
            }
            ProjectProductAction::UpdateColorEnvironment(payload) => {
                self.state.authoring.as_ref().is_some_and(|session| {
                    session.document().color_environment != payload.color_environment
                })
            }
        }
    }

    fn allows_sequence(&self, action: &SequenceProductAction) -> bool {
        let Some(session) = self.state.authoring.as_ref() else {
            return false;
        };
        let document = session.document();
        let active = document.sequences.active_sequence_id;
        match action {
            SequenceProductAction::ReturnToParent => !session.navigation_stack().is_empty(),
            SequenceProductAction::SetActiveDefault => {
                document.sequences.default_sequence_id != active
            }
            SequenceProductAction::New => true,
            SequenceProductAction::SwitchActive(payload) => {
                active != payload.sequence_id
                    && document.sequences.sequence(payload.sequence_id).is_some()
            }
            SequenceProductAction::Duplicate(payload) => {
                document.sequences.sequence(payload.sequence_id).is_some()
            }
            SequenceProductAction::Delete(payload) => {
                document.sequences.sequences.len() > 1
                    && document.sequences.sequence(payload.sequence_id).is_some()
            }
            SequenceProductAction::UpdateSettings(payload) => {
                document.sequences.sequence(payload.sequence_id).is_some_and(|sequence| {
                    let name = payload.name.trim();
                    !name.is_empty()
                        && (sequence.name != name || sequence.settings != payload.settings)
                })
            }
        }
    }

    fn allows_export(&self, action: &ExportProductAction) -> bool {
        match action {
            ExportProductAction::EditDraft(edit) => match edit.as_ref() {
                ExportDraftEdit::BuiltinPreset(preset) => {
                    self.state.export_draft.selected_builtin_preset != *preset
                        || self.state.export_draft.preset != preset.preset()
                }
                ExportDraftEdit::Preset(preset) => self.state.export_draft.preset != *preset,
                ExportDraftEdit::Sequence(sequence_id) => {
                    sequence_id
                        .is_none_or(|sequence_id| self.state.sequence_by_id(sequence_id).is_some())
                        && self.state.export_draft.selected_sequence_id != *sequence_id
                }
                ExportDraftEdit::Range(range) => self.state.export_draft.range != *range,
                ExportDraftEdit::OutputPath(output_path) => {
                    self.state.export_draft.output_path != *output_path
                }
            },
            ExportProductAction::Enqueue(request) => {
                !request.output_path.as_os_str().is_empty()
                    && request.sequence_id.map_or_else(
                        || self.state.active_sequence().is_some(),
                        |sequence_id| self.state.sequence_by_id(sequence_id).is_some(),
                    )
            }
            ExportProductAction::Cancel(job_id) => self.state.render_queue.can_cancel(*job_id),
            ExportProductAction::ClearTerminalHistory => {
                self.state.render_queue.has_terminal_history()
            }
        }
    }

    fn allows_visual_effect(&self, action: &VisualEffectProductAction) -> bool {
        let Some(sequence) = self.state.active_sequence() else {
            return false;
        };
        match action {
            VisualEffectProductAction::AddToClip(payload) => {
                visual_effect_clip(sequence, payload.clip_id).is_some_and(|target| {
                    target.track_unlocked
                        && target.is_video_track
                        && mondrian_effects::effect_definition(&payload.effect_type)
                            .is_some_and(|definition| definition.supports_visual_evaluation())
                })
            }
            VisualEffectProductAction::Select(payload) => {
                visual_effect_target(sequence, payload.clip_id, payload.effect_id).is_some()
                    && self.state.primary_selected_effect().is_none_or(|selected| {
                        selected.clip.clip_id != payload.clip_id
                            || selected.effect_id != payload.effect_id
                    })
            }
            VisualEffectProductAction::SetEnabled(payload) => {
                visual_effect_target(sequence, payload.clip_id, payload.effect_id).is_some_and(
                    |target| target.track_unlocked && target.effect.is_enabled != payload.enabled,
                )
            }
            VisualEffectProductAction::Remove(payload) => {
                visual_effect_target(sequence, payload.clip_id, payload.effect_id)
                    .is_some_and(|target| target.track_unlocked)
            }
            VisualEffectProductAction::Reorder(payload) => {
                visual_effect_target(sequence, payload.clip_id, payload.effect_id).is_some_and(
                    |target| {
                        target.track_unlocked
                            && target
                                .clip
                                .effect_relative_placement_would_change(
                                    payload.effect_id,
                                    payload.placement,
                                )
                                .unwrap_or(false)
                    },
                )
            }
            VisualEffectProductAction::SetParameterValue(payload) => {
                visual_effect_target(sequence, payload.clip_id, payload.effect_id).is_some_and(
                    |target| {
                        if !target.track_unlocked {
                            return false;
                        }
                        visual_effect_author_time(self.state, sequence, target.clip)
                            .and_then(|time| {
                                target
                                    .effect
                                    .properties
                                    .prepare_value_write_by_address(
                                        &payload.parameter,
                                        time,
                                        payload.value.clone(),
                                    )
                                    .ok()
                                    .flatten()
                            })
                            .is_some()
                    },
                )
            }
        }
    }
}

#[derive(Clone, Copy)]
struct VisualEffectClip<'a> {
    clip: &'a Clip,
    track_unlocked: bool,
    is_video_track: bool,
}

#[derive(Clone, Copy)]
struct VisualEffectTarget<'a> {
    clip: &'a Clip,
    effect: &'a mondrian_core::effect_data::EffectNode,
    track_unlocked: bool,
}

fn visual_effect_clip(sequence: &Sequence, clip_id: ClipId) -> Option<VisualEffectClip<'_>> {
    sequence
        .video_tracks
        .iter()
        .map(|track| (track, true))
        .chain(sequence.audio_tracks.iter().map(|track| (track, false)))
        .find_map(|(track, is_video_track)| {
            track.clips.iter().find(|clip| clip.id == clip_id).map(|clip| VisualEffectClip {
                clip,
                track_unlocked: !track.is_locked,
                is_video_track,
            })
        })
}

fn visual_effect_target(
    sequence: &Sequence,
    clip_id: ClipId,
    effect_id: EffectId,
) -> Option<VisualEffectTarget<'_>> {
    let target = visual_effect_clip(sequence, clip_id)?;
    if !target.is_video_track {
        return None;
    }
    let effect = target.clip.effects.iter().find(|effect| effect.id == effect_id)?;
    Some(VisualEffectTarget {
        clip: target.clip,
        effect,
        track_unlocked: target.track_unlocked,
    })
}

fn visual_effect_author_time(
    state: &AppState,
    sequence: &Sequence,
    clip: &Clip,
) -> Option<TimelineTime> {
    let sequence_time = state.current_timeline_time().ok().flatten().unwrap_or(sequence.playhead);
    let end = clip.end_position().ok()?;
    clip.timeline_to_clip_time(sequence_time.clamp(clip.position, end)).ok()
}

fn clip_admission_facts(sequence: &Sequence, clip_id: ClipId) -> Option<ClipAdmissionFacts> {
    sequence
        .video_tracks
        .iter()
        .map(|track| (track, true))
        .chain(sequence.audio_tracks.iter().map(|track| (track, false)))
        .find_map(|(track, is_video_track)| {
            track
                .clips
                .iter()
                .find(|clip| clip.id == clip_id)
                .map(|clip| ClipAdmissionFacts {
                    source_track_unlocked: !track.is_locked,
                    is_video_track,
                    position: clip.position,
                    end: clip.end_position().ok(),
                })
        })
}

fn track_is_unlocked(sequence: &Sequence, track_id: TrackId, is_video_track: bool) -> bool {
    let tracks = if is_video_track {
        &sequence.video_tracks
    } else {
        &sequence.audio_tracks
    };
    tracks
        .iter()
        .find(|track| track.id == track_id)
        .is_some_and(|track| !track.is_locked)
}

fn allows_trim(sequence: &Sequence, payload: &TimelineTrimClipsPayload) -> bool {
    if payload.clip_ids.is_empty() {
        return false;
    }
    let Ok(target) =
        TimelineTime::from_frame_position(FramePosition::new(payload.frame, sequence.time_base()))
    else {
        return false;
    };
    payload.clip_ids.iter().all(|clip_id| {
        clip_admission_facts(sequence, *clip_id).is_some_and(|clip| {
            clip.source_track_unlocked
                && clip.end.is_some_and(|end| target > clip.position && target < end)
        })
    })
}

fn recovery_candidate_is_addressable(candidate: &CrashRecoveryCandidate) -> bool {
    !candidate.runtime_root.as_os_str().is_empty()
        && !candidate.project_file.as_os_str().is_empty()
        && !candidate.autosave_file.as_os_str().is_empty()
        && candidate.archive_sha256.len() == 64
        && candidate.archive_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        && candidate.total_snapshots > 0
}

impl AppState {
    /// Borrow the read-only admission Interface for migrated product Actions.
    pub fn product_action_availability(&self) -> ProductActionAvailability<'_> {
        ProductActionAvailability::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::AssetId;
    use mondrian_core::Rational;
    use mondrian_timeline::{
        audio::{AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID},
        clip::{Clip, Transform2D},
        AudioAutomationEdit, AudioAutomationEditRequest, AudioAutomationTarget,
        AudioChannelStripOwner, AudioChannelStripRack, AudioProcessorRackAddress,
        AudioProcessorRackEdit, AudioProcessorRackPlacement, AudioRouteDestination,
        AudioRoutingEdit, AudioRoutingEditRequest,
    };

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        let numerator = frame.checked_mul(time_base.num).expect("test time fits");
        TimelineTime::new(numerator, time_base.den).expect("valid test time")
    }

    #[test]
    fn external_codec_round_trips_every_timeline_product_action() {
        let clip_id = ClipId::new();
        let track_id = TrackId::new();
        let actions = [
            ProductAction::Timeline(TimelineProductAction::SelectClip(
                TimelineSelectClipPayload {
                    clip_id,
                    mode: TimelineClipSelectionModePayload::Toggle,
                },
            )),
            ProductAction::Timeline(TimelineProductAction::MoveClip(TimelineMoveClipPayload {
                target_track_id: track_id,
                is_video_track: true,
                clip_id,
                frame: 17,
            })),
            ProductAction::Timeline(TimelineProductAction::TrimClips(TimelineTrimClipsPayload {
                clip_ids: vec![clip_id],
                edge: TimelineTrimPayloadEdge::Out,
                frame: 29,
            })),
            ProductAction::Timeline(TimelineProductAction::Seek(TimelineSeekPayload {
                frame: 21,
                source: TimelineSeekSource::PointerDrag,
            })),
        ];

        for expected in actions {
            let external = expected.clone().into_external_action();
            let decoded = ProductAction::decode_external(&external)
                .expect("valid external payload")
                .expect("recognized product action");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn external_codec_round_trips_every_asset_product_action() {
        let asset_id = AssetId::new();
        let folder_id = "folder-a".to_owned();
        let actions = vec![
            ProductAction::Asset(AssetProductAction::PrepareDrag(AssetTargetPayload {
                asset_id,
            })),
            ProductAction::Asset(AssetProductAction::RefreshAudioComponents(
                AssetTargetPayload { asset_id },
            )),
            ProductAction::Asset(AssetProductAction::RebindAudioComponent(
                AssetAudioComponentRebindPayload {
                    asset_id,
                    component_id: AudioSourceComponentId::new(),
                    stream_index: 3,
                },
            )),
            ProductAction::Asset(AssetProductAction::CreateGenerated(
                AssetCreateGeneratedPayload {
                    kind: GeneratedAssetKind::SolidColor,
                    folder_id: Some(folder_id.clone()),
                },
            )),
            ProductAction::Asset(AssetProductAction::CreateFolder(AssetCreateFolderPayload {
                parent_folder_id: Some(folder_id.clone()),
            })),
            ProductAction::Asset(AssetProductAction::ImportFiles(Box::new(
                AssetImportFilesPayload {
                    paths: vec![PathBuf::from("media.mov")],
                    folder_id: Some(folder_id.clone()),
                },
            ))),
            ProductAction::Asset(AssetProductAction::Relink(AssetRelinkPayload {
                asset_id,
                path: PathBuf::from("replacement.mov"),
            })),
            ProductAction::Asset(AssetProductAction::SetInterpretation(
                AssetSetInterpretationPayload {
                    asset_id,
                    interpretation: AssetMediaInterpretation::default(),
                },
            )),
            ProductAction::Asset(AssetProductAction::Rename(AssetRenamePayload {
                asset_id,
                name: "Interview A".to_owned(),
            })),
            ProductAction::Asset(AssetProductAction::RenameFolder(AssetRenameFolderPayload {
                folder_id: folder_id.clone(),
                name: "Interviews".to_owned(),
            })),
            ProductAction::Asset(AssetProductAction::SetProxyMode(AssetSetProxyModePayload {
                asset_id,
                enabled: true,
            })),
            ProductAction::Asset(AssetProductAction::RemoveEntries(Box::new(
                AssetLibrarySelectionPayload {
                    asset_ids: vec![asset_id],
                    folder_ids: vec![folder_id.clone()],
                },
            ))),
            ProductAction::Asset(AssetProductAction::MoveEntries(Box::new(
                AssetLibraryMovePayload {
                    asset_ids: vec![asset_id],
                    folder_ids: vec![folder_id],
                    target_folder_id: Some("folder-b".to_owned()),
                },
            ))),
        ];

        for expected in actions {
            let external = expected.clone().into_external_action();
            let decoded = ProductAction::decode_external(&external)
                .expect("valid external payload")
                .expect("recognized Asset product action");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn external_codec_rejects_unknown_asset_payload_fields() {
        let action = Action::Custom {
            namespace: ASSET_NAMESPACE.to_owned(),
            name: ASSET_PREPARE_DRAG.to_owned(),
            payload: serde_json::json!({
                "asset_id": AssetId::new(),
                "legacy_folder_hint": "bin-a",
            }),
        };

        let error = ProductAction::decode_external(&action)
            .expect_err("recognized Asset payload must fail closed");
        assert_eq!(error.dispatch_step_id(), "asset_action.prepare_drag");
    }

    #[test]
    fn external_codec_round_trips_every_video_transition_product_action() {
        let transition_id = VideoTransitionId::new();
        let left_clip_id = ClipId::new();
        let right_clip_id = ClipId::new();
        let range = TimelineTimeRange::new(
            TimelineTime::new(7, 20).expect("range start"),
            TimelineTime::new(1, 10).expect("range duration"),
        )
        .expect("exact range");
        let actions = [
            ProductAction::VideoTransition(VideoTransitionProductAction::Select(
                VideoTransitionTargetPayload { transition_id },
            )),
            ProductAction::VideoTransition(VideoTransitionProductAction::CreateCrossDissolve(
                VideoTransitionCreateCrossDissolvePayload {
                    left_clip_id,
                    right_clip_id,
                    handle_policy: VideoTransitionHandlePolicy::Reject,
                },
            )),
            ProductAction::VideoTransition(VideoTransitionProductAction::SetRange(
                VideoTransitionSetRangePayload {
                    transition_id,
                    requested_range: range,
                    handle_policy: VideoTransitionHandlePolicy::ShortenToAvailable,
                },
            )),
            ProductAction::VideoTransition(VideoTransitionProductAction::Remove(
                VideoTransitionTargetPayload { transition_id },
            )),
        ];

        for expected in actions {
            let external = expected.clone().into_external_action();
            let decoded = ProductAction::decode_external(&external)
                .expect("valid external payload")
                .expect("recognized visual Transition product action");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn external_codec_round_trips_every_viewer_product_action() {
        let actions = [ProductAction::Viewer(
            ViewerProductAction::SetPreviewResolutionScale(
                ViewerSetPreviewResolutionScalePayload { scale: 0.25 },
            ),
        )];

        for expected in actions {
            let external = expected.clone().into_external_action();
            let decoded = ProductAction::decode_external(&external)
                .expect("valid external payload")
                .expect("recognized Viewer product action");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn external_codec_round_trips_atomic_clip_parameter_writes() {
        let expected = ProductAction::Clip(ClipProductAction::WriteParameterValues(Box::new(
            ClipWriteParameterValuesPayload {
                clip_id: ClipId::new(),
                writes: vec![ClipParameterValueWrite {
                    parameter: AnimationParameterAddress {
                        animation_track_id: mondrian_core::AnimationTrackId::new(),
                        parameter_id: mondrian_core::ParameterId::new_static("transform.position"),
                    },
                    value: PropertyValue::Vec2(glam::Vec2::new(320.0, 180.0)),
                }],
            },
        )));

        let decoded = ProductAction::decode_external(&expected.clone().into_external_action())
            .expect("valid external payload")
            .expect("recognized Clip product action");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn external_codec_round_trips_every_project_product_action() {
        let actions = [
            ProductAction::Project(ProjectProductAction::CreateWithSettings(
                ProjectCreateWithSettingsPayload {
                    project_file: PathBuf::from("edit.mdp"),
                    name: "Edit".to_owned(),
                    sequence_settings: SequenceSettings::default(),
                    color_environment: ProjectColorEnvironment::default(),
                    project_settings: ProjectSettings::default(),
                },
            )),
            ProductAction::Project(ProjectProductAction::UpdateNewSequenceDefaults(
                ProjectUpdateNewSequenceDefaultsPayload { settings: SequenceSettings::default() },
            )),
            ProductAction::Project(ProjectProductAction::UpdateColorEnvironment(
                ProjectUpdateColorEnvironmentPayload {
                    color_environment: ProjectColorEnvironment::default(),
                },
            )),
            ProductAction::Project(ProjectProductAction::RecoverFromAutosave(
                ProjectRecoverFromAutosavePayload {
                    candidate: CrashRecoveryCandidate {
                        project_id: mondrian_core::ProjectId::new(),
                        runtime_root: PathBuf::from("runtime"),
                        project_file: PathBuf::from("edit.mdp"),
                        autosave_file: PathBuf::from("autosave.mdp"),
                        author_generation: 4,
                        asset_library_revision: 3,
                        document_revision: 2,
                        archive_sha256: "a".repeat(64),
                        saved_at_unix_ms: 1,
                        total_snapshots: 1,
                    },
                },
            )),
        ];

        for expected in actions {
            let decoded = ProductAction::decode_external(&expected.clone().into_external_action())
                .expect("valid external payload")
                .expect("recognized Project product action");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn external_codec_round_trips_every_sequence_product_action() {
        let sequence_id = SequenceId::new();
        let actions = [
            ProductAction::Sequence(SequenceProductAction::ReturnToParent),
            ProductAction::Sequence(SequenceProductAction::SetActiveDefault),
            ProductAction::Sequence(SequenceProductAction::New),
            ProductAction::Sequence(SequenceProductAction::SwitchActive(SequenceTargetPayload {
                sequence_id,
            })),
            ProductAction::Sequence(SequenceProductAction::Duplicate(SequenceTargetPayload {
                sequence_id,
            })),
            ProductAction::Sequence(SequenceProductAction::Delete(SequenceTargetPayload {
                sequence_id,
            })),
            ProductAction::Sequence(SequenceProductAction::UpdateSettings(Box::new(
                SequenceUpdateSettingsPayload {
                    sequence_id,
                    name: "Updated".to_owned(),
                    settings: SequenceSettings::default(),
                },
            ))),
        ];

        for expected in actions {
            let decoded = ProductAction::decode_external(&expected.clone().into_external_action())
                .expect("valid external payload")
                .expect("recognized Sequence product action");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn external_codec_round_trips_every_export_product_action() {
        let sequence_id = SequenceId::new();
        let job_id = JobId::new();
        let preset = ExportPreset::h264_aac_sdr_1080p();
        let actions = [
            ProductAction::Export(ExportProductAction::EditDraft(Box::new(
                ExportDraftEdit::BuiltinPreset(BuiltinExportPreset::H264AacSdr1080p),
            ))),
            ProductAction::Export(ExportProductAction::EditDraft(Box::new(
                ExportDraftEdit::Preset(preset.clone()),
            ))),
            ProductAction::Export(ExportProductAction::EditDraft(Box::new(
                ExportDraftEdit::Sequence(Some(sequence_id)),
            ))),
            ProductAction::Export(ExportProductAction::EditDraft(Box::new(
                ExportDraftEdit::Range(TimelineExportRange::EntireSequence),
            ))),
            ProductAction::Export(ExportProductAction::EditDraft(Box::new(
                ExportDraftEdit::OutputPath("delivery.mp4".to_owned()),
            ))),
            ProductAction::Export(ExportProductAction::Enqueue(Box::new(
                TimelineExportRequest {
                    preset,
                    sequence_id: Some(sequence_id),
                    range: TimelineExportRange::EntireSequence,
                    output_path: PathBuf::from("delivery.mp4"),
                    output_policy: mondrian_export::preset::ExportOutputPolicy::CreateNew,
                },
            ))),
            ProductAction::Export(ExportProductAction::Cancel(job_id)),
            ProductAction::Export(ExportProductAction::ClearTerminalHistory),
        ];

        for expected in actions {
            let decoded = ProductAction::decode_external(&expected.clone().into_external_action())
                .expect("valid external payload")
                .expect("recognized Export product action");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn external_codec_round_trips_every_visual_effect_product_action() {
        let clip_id = ClipId::new();
        let effect_id = EffectId::new();
        let anchor_id = EffectId::new();
        let parameter = AnimationParameterAddress {
            animation_track_id: mondrian_core::AnimationTrackId::new(),
            parameter_id: mondrian_core::ParameterId::new_static("mondrian.effect.test.amount"),
        };
        let actions = [
            ProductAction::VisualEffect(VisualEffectProductAction::AddToClip(
                VisualEffectAddToClipPayload { clip_id, effect_type: EffectType::GaussianBlur },
            )),
            ProductAction::VisualEffect(VisualEffectProductAction::Select(
                VisualEffectTargetPayload { clip_id, effect_id },
            )),
            ProductAction::VisualEffect(VisualEffectProductAction::SetEnabled(
                VisualEffectSetEnabledPayload { clip_id, effect_id, enabled: false },
            )),
            ProductAction::VisualEffect(VisualEffectProductAction::Remove(
                VisualEffectTargetPayload { clip_id, effect_id },
            )),
            ProductAction::VisualEffect(VisualEffectProductAction::Reorder(
                VisualEffectReorderPayload {
                    clip_id,
                    effect_id,
                    placement: EffectRelativePlacement::After(anchor_id),
                },
            )),
            ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(Box::new(
                VisualEffectSetParameterValuePayload {
                    clip_id,
                    effect_id,
                    parameter,
                    value: PropertyValue::Float(0.75),
                },
            ))),
        ];

        for expected in actions {
            let decoded = ProductAction::decode_external(&expected.clone().into_external_action())
                .expect("valid external payload")
                .expect("recognized visual Effect product action");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn external_codec_round_trips_audio_processor_rack_edits() {
        let request = AudioProcessorRackEditRequest {
            address: AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Track { track_id: TrackId::new() },
                rack: AudioChannelStripRack::PreFader,
            },
            edit: AudioProcessorRackEdit::Insert {
                processor: AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
                placement: AudioProcessorRackPlacement::End,
            },
        };
        let expected = ProductAction::Audio(AudioProductAction::EditProcessorRack(request));

        let external = expected.clone().into_external_action();
        let decoded = ProductAction::decode_external(&external)
            .expect("valid external payload")
            .expect("recognized product action");

        assert_eq!(decoded, expected);
    }

    #[test]
    fn external_codec_round_trips_transient_track_solo_intent() {
        let expected =
            ProductAction::Audio(AudioProductAction::SetTrackSolo(AudioTrackSoloPayload {
                track_id: TrackId::new(),
                soloed: true,
            }));

        let external = expected.clone().into_external_action();
        let decoded = ProductAction::decode_external(&external)
            .expect("valid external payload")
            .expect("recognized product action");

        assert_eq!(decoded, expected);
    }

    #[test]
    fn external_codec_round_trips_stable_audio_automation_edits() {
        let expected = ProductAction::Audio(AudioProductAction::EditAutomation(
            AudioAutomationEditRequest {
                target: AudioAutomationTarget::ChannelFader {
                    owner: AudioChannelStripOwner::Track { track_id: TrackId::new() },
                },
                edit: AudioAutomationEdit::UpsertKeyframe {
                    keyframe: mondrian_core::ExactAutomationKeyframe::linear(
                        TimelineTime::new(1, 48_000).expect("sample time"),
                        -6.0,
                    ),
                },
            },
        ));

        let external = expected.clone().into_external_action();
        let decoded = ProductAction::decode_external(&external)
            .expect("valid external payload")
            .expect("recognized product action");

        assert_eq!(decoded, expected);
    }

    #[test]
    fn external_codec_round_trips_audio_component_edits() {
        let expected = ProductAction::Audio(AudioProductAction::EditComponent(
            AudioComponentEditRequest {
                address: mondrian_timeline::AudioComponentAddress {
                    track_id: TrackId::new(),
                    clip_id: ClipId::new(),
                    edit_id: mondrian_core::AudioComponentEditId::new(),
                },
                mutation: mondrian_timeline::AudioComponentMutation::SetChannelMapping {
                    value: mondrian_timeline::audio::AudioComponentChannelMapping::Explicit(
                        mondrian_core::AudioChannelMixMatrix::standard(
                            mondrian_core::AudioChannelLayout::Mono,
                            mondrian_core::AudioChannelLayout::Stereo,
                        )
                        .expect("standard matrix"),
                    ),
                },
            },
        ));

        let external = expected.clone().into_external_action();
        let decoded = ProductAction::decode_external(&external)
            .expect("valid external payload")
            .expect("recognized product action");

        assert_eq!(decoded, expected);
    }

    #[test]
    fn external_codec_round_trips_audio_processor_builtin_insertion_intent() {
        let expected = ProductAction::Audio(AudioProductAction::InsertBuiltInProcessor(
            AudioProcessorInsertBuiltInPayload {
                address: AudioProcessorRackAddress::ProcessingScope {
                    scope_id: mondrian_core::AudioProcessingScopeId::new(),
                },
                preset: AudioProcessorBuiltInPreset::LookaheadLimiter,
                placement: AudioProcessorRackPlacement::End,
            },
        ));

        let external = expected.clone().into_external_action();
        let decoded = ProductAction::decode_external(&external)
            .expect("valid external payload")
            .expect("recognized product action");

        assert_eq!(decoded, expected);
    }

    #[test]
    fn external_codec_round_trips_channel_strip_edits() {
        let expected = ProductAction::Audio(AudioProductAction::EditChannelStrip(
            AudioChannelStripEditRequest {
                owner: AudioChannelStripOwner::Bus { bus_id: mondrian_core::MixBusId::new() },
                edit: mondrian_timeline::AudioChannelStripEdit::SetFaderDb { value: -3.0 },
            },
        ));

        let external = expected.clone().into_external_action();
        let decoded = ProductAction::decode_external(&external)
            .expect("valid external payload")
            .expect("recognized product action");

        assert_eq!(decoded, expected);
    }

    #[test]
    fn external_codec_round_trips_audio_routing_edits() {
        let expected =
            ProductAction::Audio(AudioProductAction::EditRouting(AudioRoutingEditRequest {
                edit: AudioRoutingEdit::CreateBus {
                    name: "Dialogue".to_owned(),
                    route_to: Some(AudioRouteDestination::Output(
                        mondrian_core::ProgramOutputId::new(),
                    )),
                },
            }));

        let external = expected.clone().into_external_action();
        let decoded = ProductAction::decode_external(&external)
            .expect("valid external payload")
            .expect("recognized product action");

        assert_eq!(decoded, expected);
    }

    #[test]
    fn external_codec_distinguishes_unknown_and_malformed_known_actions() {
        let unknown = Action::Custom {
            namespace: TIMELINE_NAMESPACE.to_owned(),
            name: "plugin_extension".to_owned(),
            payload: serde_json::json!({"anything": true}),
        };
        assert!(ProductAction::decode_external(&unknown)
            .expect("unknown names are not decode failures")
            .is_none());

        let malformed = Action::Custom {
            namespace: TIMELINE_NAMESPACE.to_owned(),
            name: TIMELINE_MOVE_CLIP.to_owned(),
            payload: serde_json::json!({"clip_id": ClipId::new()}),
        };
        assert!(ProductAction::decode_external(&malformed).is_err());

        let unknown_audio = Action::Custom {
            namespace: AUDIO_NAMESPACE.to_owned(),
            name: "plugin_extension".to_owned(),
            payload: serde_json::json!({"anything": true}),
        };
        assert!(ProductAction::decode_external(&unknown_audio)
            .expect("unknown names are not decode failures")
            .is_none());

        let malformed_audio = Action::Custom {
            namespace: AUDIO_NAMESPACE.to_owned(),
            name: AUDIO_EDIT_PROCESSOR_RACK.to_owned(),
            payload: serde_json::json!({"address": {"kind": "processing_scope"}}),
        };
        let error = ProductAction::decode_external(&malformed_audio)
            .expect_err("recognized malformed payload fails closed");
        assert_eq!(error.dispatch_step_id(), "audio_action.edit_processor_rack");

        let shell_zoom = Action::Custom {
            namespace: VIEWER_NAMESPACE.to_owned(),
            name: "cycle_zoom".to_owned(),
            payload: serde_json::json!(null),
        };
        assert!(ProductAction::decode_external(&shell_zoom)
            .expect("shell-local Viewer names are not decode failures")
            .is_none());

        let malformed_clip = Action::Custom {
            namespace: CLIP_NAMESPACE.to_owned(),
            name: CLIP_WRITE_PARAMETER_VALUES.to_owned(),
            payload: serde_json::json!({"clip_id": ClipId::new()}),
        };
        let error = ProductAction::decode_external(&malformed_clip)
            .expect_err("recognized malformed Clip payload fails closed");
        assert_eq!(
            error.dispatch_step_id(),
            "clip_action.write_parameter_values"
        );

        let malformed_transition = Action::Custom {
            namespace: VIDEO_TRANSITION_NAMESPACE.to_owned(),
            name: VIDEO_TRANSITION_SET_RANGE.to_owned(),
            payload: serde_json::json!({"transition_id": VideoTransitionId::new()}),
        };
        let error = ProductAction::decode_external(&malformed_transition)
            .expect_err("recognized malformed visual Transition payload fails closed");
        assert_eq!(
            error.dispatch_step_id(),
            "video_transition_action.set_range"
        );

        for (namespace, name, step_id) in [
            (
                PROJECT_NAMESPACE,
                PROJECT_CREATE_WITH_SETTINGS,
                "project_action.create_with_settings",
            ),
            (SEQUENCE_NAMESPACE, SEQUENCE_NEW, "sequence_action.new"),
            (
                EXPORT_NAMESPACE,
                EXPORT_CLEAR_TERMINAL_HISTORY,
                "export_action.clear_terminal_history",
            ),
        ] {
            let malformed = Action::Custom {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                payload: serde_json::json!({}),
            };
            let error = ProductAction::decode_external(&malformed)
                .expect_err("recognized malformed product payload fails closed");
            assert_eq!(error.dispatch_step_id(), step_id);
        }

        for namespace in [PROJECT_NAMESPACE, SEQUENCE_NAMESPACE, EXPORT_NAMESPACE] {
            let unknown = Action::Custom {
                namespace: namespace.to_owned(),
                name: "plugin_extension".to_owned(),
                payload: serde_json::Value::Null,
            };
            assert!(ProductAction::decode_external(&unknown)
                .expect("unknown product name remains available to another Adapter")
                .is_none());
        }
    }

    #[test]
    fn interaction_projection_hides_state_but_preserves_lock_and_range_admission() {
        let mut sequence = Sequence::new("Edit");
        let time_base = sequence.time_base();
        let clip =
            Clip::new(AssetId::new(), tt(10, time_base), tt(20, time_base)).expect("valid clip");
        let clip_id = clip.id;
        let position_parameter = clip
            .intrinsic_parameter_bag()
            .address_for_path(Transform2D::POSITION_PATH)
            .expect("position parameter");
        let track_id = sequence.video_tracks[0].id;
        let audio_track_id = sequence.audio_tracks[0].id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));

        let projection = state.product_action_availability();
        assert!(
            projection.allows(&ProductAction::Timeline(TimelineProductAction::SelectClip(
                TimelineSelectClipPayload {
                    clip_id,
                    mode: TimelineClipSelectionModePayload::Replace,
                },
            )))
        );
        assert!(
            projection.allows(&ProductAction::Timeline(TimelineProductAction::MoveClip(
                TimelineMoveClipPayload {
                    target_track_id: track_id,
                    is_video_track: true,
                    clip_id,
                    frame: 12,
                }
            )))
        );
        assert!(
            !projection.allows(&ProductAction::Timeline(TimelineProductAction::MoveClip(
                TimelineMoveClipPayload {
                    target_track_id: audio_track_id,
                    is_video_track: false,
                    clip_id,
                    frame: 12,
                }
            )))
        );
        assert!(
            projection.allows(&ProductAction::Timeline(TimelineProductAction::TrimClips(
                TimelineTrimClipsPayload {
                    clip_ids: vec![clip_id],
                    edge: TimelineTrimPayloadEdge::In,
                    frame: 15,
                },
            )))
        );
        assert!(
            !projection.allows(&ProductAction::Timeline(TimelineProductAction::TrimClips(
                TimelineTrimClipsPayload {
                    clip_ids: vec![clip_id],
                    edge: TimelineTrimPayloadEdge::In,
                    frame: 10,
                },
            )))
        );
        assert!(projection.allows(&ProductAction::Viewer(
            ViewerProductAction::SetPreviewResolutionScale(
                ViewerSetPreviewResolutionScalePayload { scale: 0.25 }
            )
        )));
        assert!(projection.allows(&ProductAction::Clip(
            ClipProductAction::WriteParameterValues(Box::new(ClipWriteParameterValuesPayload {
                clip_id,
                writes: vec![ClipParameterValueWrite {
                    parameter: position_parameter.clone(),
                    value: PropertyValue::Vec2(glam::Vec2::new(10.0, 20.0)),
                }],
            }))
        )));
        assert!(!projection.allows(&ProductAction::Clip(
            ClipProductAction::WriteParameterValues(Box::new(ClipWriteParameterValuesPayload {
                clip_id,
                writes: Vec::new(),
            }))
        )));
        assert!(!projection.allows(&ProductAction::Clip(
            ClipProductAction::WriteParameterValues(Box::new(ClipWriteParameterValuesPayload {
                clip_id: ClipId::new(),
                writes: vec![ClipParameterValueWrite {
                    parameter: position_parameter.clone(),
                    value: PropertyValue::Vec2(glam::Vec2::new(10.0, 20.0)),
                }],
            }))
        )));

        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
        let locked = state.product_action_availability();
        assert!(
            !locked.allows(&ProductAction::Timeline(TimelineProductAction::MoveClip(
                TimelineMoveClipPayload {
                    target_track_id: track_id,
                    is_video_track: true,
                    clip_id,
                    frame: 12,
                }
            )))
        );
        assert!(
            !locked.allows(&ProductAction::Timeline(TimelineProductAction::TrimClips(
                TimelineTrimClipsPayload {
                    clip_ids: vec![clip_id],
                    edge: TimelineTrimPayloadEdge::Out,
                    frame: 20,
                },
            )))
        );
        assert!(!locked.allows(&ProductAction::Clip(
            ClipProductAction::WriteParameterValues(Box::new(ClipWriteParameterValuesPayload {
                clip_id,
                writes: vec![ClipParameterValueWrite {
                    parameter: position_parameter,
                    value: PropertyValue::Vec2(glam::Vec2::new(10.0, 20.0)),
                }],
            }))
        )));
        assert!(
            locked.allows(&ProductAction::Timeline(TimelineProductAction::Seek(
                TimelineSeekPayload { frame: 0, source: TimelineSeekSource::Settled }
            )))
        );
        assert!(
            !locked.allows(&ProductAction::Timeline(TimelineProductAction::Seek(
                TimelineSeekPayload { frame: -1, source: TimelineSeekSource::Settled }
            )))
        );

        let empty = AppState::new();
        assert!(
            !empty.product_action_availability().allows(&ProductAction::Timeline(
                TimelineProductAction::Seek(TimelineSeekPayload {
                    frame: 0,
                    source: TimelineSeekSource::Settled
                })
            ))
        );
        assert!(
            !empty.product_action_availability().allows(&ProductAction::Viewer(
                ViewerProductAction::SetPreviewResolutionScale(
                    ViewerSetPreviewResolutionScalePayload { scale: 0.25 }
                )
            ))
        );
    }

    #[test]
    fn visual_effect_availability_uses_authoritative_targets_and_exact_noop_semantics() {
        let mut sequence = Sequence::new("Visual Effects");
        let time_base = sequence.time_base();
        let mut clip =
            Clip::new(AssetId::new(), tt(0, time_base), tt(30, time_base)).expect("valid Clip");
        let first = mondrian_effects::instantiate_effect_node(EffectType::GaussianBlur)
            .expect("registered Effect");
        let first_id = first.id;
        let (_, first_property) = first.properties.iter().next().expect("Effect property");
        let parameter = AnimationParameterAddress {
            animation_track_id: first_property.track_id,
            parameter_id: first_property.descriptor.parameter_id().clone(),
        };
        let initial_value = first_property.static_value().clone();
        let second = mondrian_effects::instantiate_effect_node(EffectType::Sharpen)
            .expect("registered anchor Effect");
        let second_id = second.id;
        clip.add_effect_node(first);
        clip.add_effect_node(second);
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add Clip");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));

        let add = ProductAction::VisualEffect(VisualEffectProductAction::AddToClip(
            VisualEffectAddToClipPayload { clip_id, effect_type: EffectType::BasicCorrection },
        ));
        let missing_add = ProductAction::VisualEffect(VisualEffectProductAction::AddToClip(
            VisualEffectAddToClipPayload {
                clip_id,
                effect_type: EffectType::Plugin("plugin.test.missing".to_owned()),
            },
        ));
        let select = ProductAction::VisualEffect(VisualEffectProductAction::Select(
            VisualEffectTargetPayload { clip_id, effect_id: first_id },
        ));
        let set_same = ProductAction::VisualEffect(VisualEffectProductAction::SetEnabled(
            VisualEffectSetEnabledPayload { clip_id, effect_id: first_id, enabled: true },
        ));
        let set_changed = ProductAction::VisualEffect(VisualEffectProductAction::SetEnabled(
            VisualEffectSetEnabledPayload { clip_id, effect_id: first_id, enabled: false },
        ));
        let already_before = ProductAction::VisualEffect(VisualEffectProductAction::Reorder(
            VisualEffectReorderPayload {
                clip_id,
                effect_id: first_id,
                placement: EffectRelativePlacement::Before(second_id),
            },
        ));
        let move_after = ProductAction::VisualEffect(VisualEffectProductAction::Reorder(
            VisualEffectReorderPayload {
                clip_id,
                effect_id: first_id,
                placement: EffectRelativePlacement::After(second_id),
            },
        ));
        let same_parameter =
            ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(Box::new(
                VisualEffectSetParameterValuePayload {
                    clip_id,
                    effect_id: first_id,
                    parameter: parameter.clone(),
                    value: initial_value,
                },
            )));
        let changed_parameter =
            ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(Box::new(
                VisualEffectSetParameterValuePayload {
                    clip_id,
                    effect_id: first_id,
                    parameter,
                    value: PropertyValue::Float(3.0),
                },
            )));

        let availability = state.product_action_availability();
        assert!(availability.allows(&add));
        assert!(!availability.allows(&missing_add));
        assert!(availability.allows(&select));
        assert!(!availability.allows(&set_same));
        assert!(availability.allows(&set_changed));
        assert!(!availability.allows(&already_before));
        assert!(availability.allows(&move_after));
        assert!(!availability.allows(&same_parameter));
        assert!(availability.allows(&changed_parameter));

        state.active_sequence_mut_uncommitted().expect("Sequence").video_tracks[0].is_locked = true;
        assert!(!state.product_action_availability().allows(&add));
        assert!(!state.product_action_availability().allows(&set_changed));
        assert!(!state.product_action_availability().allows(&move_after));
        assert!(state.product_action_availability().allows(&select));
    }

    #[test]
    fn product_availability_uses_authoritative_project_and_sequence_targets() {
        let mut state = AppState::new();
        let first = Sequence::new("First");
        let first_id = first.id;
        state.test_set_sequence(Some(first));

        let create = ProductAction::Project(ProjectProductAction::CreateWithSettings(
            ProjectCreateWithSettingsPayload {
                project_file: PathBuf::from("edit.mdp"),
                name: "Edit".to_owned(),
                sequence_settings: SequenceSettings::default(),
                color_environment: ProjectColorEnvironment::default(),
                project_settings: ProjectSettings::default(),
            },
        ));
        assert!(state.product_action_availability().allows(&create));
        let empty_create = ProductAction::Project(ProjectProductAction::CreateWithSettings(
            ProjectCreateWithSettingsPayload {
                project_file: PathBuf::new(),
                name: "Edit".to_owned(),
                sequence_settings: SequenceSettings::default(),
                color_environment: ProjectColorEnvironment::default(),
                project_settings: ProjectSettings::default(),
            },
        ));
        assert!(!state.product_action_availability().allows(&empty_create));

        let unchanged_defaults =
            ProductAction::Project(ProjectProductAction::UpdateNewSequenceDefaults(
                ProjectUpdateNewSequenceDefaultsPayload { settings: SequenceSettings::default() },
            ));
        assert!(!state.product_action_availability().allows(&unchanged_defaults));
        let mut changed_settings = SequenceSettings::default();
        changed_settings.preview.resolution_scale = 0.25;
        let changed_defaults =
            ProductAction::Project(ProjectProductAction::UpdateNewSequenceDefaults(
                ProjectUpdateNewSequenceDefaultsPayload { settings: changed_settings },
            ));
        assert!(state.product_action_availability().allows(&changed_defaults));

        assert!(state
            .product_action_availability()
            .allows(&ProductAction::Sequence(SequenceProductAction::New)));
        let second_id = state.new_sequence("Second").expect("new sequence");
        let first_target = SequenceTargetPayload { sequence_id: first_id };
        let second_target = SequenceTargetPayload { sequence_id: second_id };
        assert!(
            state.product_action_availability().allows(&ProductAction::Sequence(
                SequenceProductAction::SwitchActive(first_target)
            ))
        );
        assert!(
            !state.product_action_availability().allows(&ProductAction::Sequence(
                SequenceProductAction::SwitchActive(second_target)
            ))
        );
        assert!(
            !state.product_action_availability().allows(&ProductAction::Sequence(
                SequenceProductAction::SwitchActive(SequenceTargetPayload {
                    sequence_id: SequenceId::new(),
                })
            ))
        );
        assert!(
            state.product_action_availability().allows(&ProductAction::Sequence(
                SequenceProductAction::Duplicate(first_target)
            ))
        );
        assert!(
            state.product_action_availability().allows(&ProductAction::Sequence(
                SequenceProductAction::Delete(first_target)
            ))
        );
        assert!(
            state.product_action_availability().allows(&ProductAction::Sequence(
                SequenceProductAction::SetActiveDefault
            ))
        );
        assert!(
            !state.product_action_availability().allows(&ProductAction::Sequence(
                SequenceProductAction::ReturnToParent
            ))
        );

        let second = state.sequence_by_id(second_id).expect("second sequence");
        let unchanged = ProductAction::Sequence(SequenceProductAction::UpdateSettings(Box::new(
            SequenceUpdateSettingsPayload {
                sequence_id: second_id,
                name: second.name.clone(),
                settings: second.settings.clone(),
            },
        )));
        assert!(!state.product_action_availability().allows(&unchanged));
        let changed = ProductAction::Sequence(SequenceProductAction::UpdateSettings(Box::new(
            SequenceUpdateSettingsPayload {
                sequence_id: second_id,
                name: "Renamed".to_owned(),
                settings: second.settings.clone(),
            },
        )));
        assert!(state.product_action_availability().allows(&changed));
    }

    #[test]
    fn export_availability_rejects_noops_and_stale_targets_without_cloning_jobs() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Export");
        let sequence_id = sequence.id;
        state.test_set_sequence(Some(sequence));

        let unchanged_builtin = ProductAction::Export(ExportProductAction::EditDraft(Box::new(
            ExportDraftEdit::BuiltinPreset(state.export_draft.selected_builtin_preset),
        )));
        assert!(!state.product_action_availability().allows(&unchanged_builtin));
        let output_edit = ProductAction::Export(ExportProductAction::EditDraft(Box::new(
            ExportDraftEdit::OutputPath("delivery.mp4".to_owned()),
        )));
        assert!(state.product_action_availability().allows(&output_edit));
        let select_sequence = ProductAction::Export(ExportProductAction::EditDraft(Box::new(
            ExportDraftEdit::Sequence(Some(sequence_id)),
        )));
        assert!(state.product_action_availability().allows(&select_sequence));
        let stale_sequence = ProductAction::Export(ExportProductAction::EditDraft(Box::new(
            ExportDraftEdit::Sequence(Some(SequenceId::new())),
        )));
        assert!(!state.product_action_availability().allows(&stale_sequence));

        let valid_request = TimelineExportRequest {
            preset: ExportPreset::h264_aac_sdr_1080p(),
            sequence_id: Some(sequence_id),
            range: TimelineExportRange::EntireSequence,
            output_path: PathBuf::from("delivery.mp4"),
            output_policy: mondrian_export::preset::ExportOutputPolicy::CreateNew,
        };
        let valid_enqueue = ProductAction::Export(ExportProductAction::Enqueue(Box::new(
            valid_request.clone(),
        )));
        assert!(state.product_action_availability().allows(&valid_enqueue));
        let stale_enqueue = ProductAction::Export(ExportProductAction::Enqueue(Box::new(
            TimelineExportRequest {
                sequence_id: Some(SequenceId::new()),
                ..valid_request
            },
        )));
        assert!(!state.product_action_availability().allows(&stale_enqueue));
        assert!(
            !state.product_action_availability().allows(&ProductAction::Export(
                ExportProductAction::Cancel(JobId::new())
            ))
        );
        assert!(
            !state.product_action_availability().allows(&ProductAction::Export(
                ExportProductAction::ClearTerminalHistory
            ))
        );
    }
}
