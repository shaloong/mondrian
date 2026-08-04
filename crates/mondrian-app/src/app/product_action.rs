//! Closed product Actions and the external custom-action Adapter.
//!
//! Product code dispatches and admits the typed algebra in this Module.
//! `Action::Custom` remains a compatibility transport for reusable Widgets,
//! scripting, and future plugin Adapters; it is decoded only at this seam.

use std::path::PathBuf;

use mondrian_core::types::{ClipId, FramePosition, JobId, SequenceId, TrackId};
use mondrian_core::{ProjectColorEnvironment, ProjectSettings, TimelineTime};
use mondrian_editor_state::Action;
use mondrian_export::preset::{BuiltinExportPreset, ExportPreset, TimelineExportRange};
use mondrian_timeline::{
    sequence::{Sequence, SequenceSettings},
    AudioAutomationEditRequest, AudioChannelStripEditRequest, AudioComponentEditRequest,
    AudioProcessorRackEditRequest, AudioRoutingEditRequest,
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

/// External custom-action namespace for Viewer product operations.
pub const VIEWER_NAMESPACE: &str = "ui.viewer";

/// External action name for changing the active Sequence preview scale.
pub const VIEWER_SET_PREVIEW_RESOLUTION_SCALE: &str = "set_preview_resolution_scale";
/// External action name for changing one Clip transform from monitor editing.
pub const VIEWER_SET_CLIP_TRANSFORM: &str = "set_clip_transform";

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

/// One closed product operation accepted by the App composition root.
///
/// Additional product domains may extend this algebra without making their
/// internal authoring or execution state part of the UI Interface.
#[derive(Debug, Clone, PartialEq)]
pub enum ProductAction {
    /// An operation owned by the active Timeline Interface.
    Timeline(TimelineProductAction),
    /// An operation owned by Sequence audio authoring.
    Audio(AudioProductAction),
    /// An operation owned by the Viewer product Interface.
    Viewer(ViewerProductAction),
    /// An operation owned by the Project lifecycle or authoring Interface.
    Project(ProjectProductAction),
    /// An operation owned by the Sequence management Interface.
    Sequence(SequenceProductAction),
    /// An operation owned by Export draft or execution orchestration.
    Export(ExportProductAction),
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

/// Closed Viewer operations that mutate product state.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewerProductAction {
    /// Change the active Sequence's authored preview resolution scale.
    SetPreviewResolutionScale(ViewerSetPreviewResolutionScalePayload),
    /// Change one Clip transform at the active Sequence playhead.
    SetClipTransform(ViewerSetClipTransformPayload),
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
            AUDIO_NAMESPACE => "audio_action",
            VIEWER_NAMESPACE => "viewer_action",
            PROJECT_NAMESPACE => "project_action",
            SEQUENCE_NAMESPACE => "sequence_action",
            EXPORT_NAMESPACE => "export_action",
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
            VIEWER_NAMESPACE => match name.as_str() {
                VIEWER_SET_PREVIEW_RESOLUTION_SCALE => Ok(Some(Self::Viewer(
                    ViewerProductAction::SetPreviewResolutionScale(decode_payload(
                        namespace, name, payload,
                    )?),
                ))),
                VIEWER_SET_CLIP_TRANSFORM => {
                    Ok(Some(Self::Viewer(ViewerProductAction::SetClipTransform(
                        decode_payload(namespace, name, payload)?,
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
            Self::Viewer(ViewerProductAction::SetClipTransform(payload)) => (
                VIEWER_NAMESPACE,
                VIEWER_SET_CLIP_TRANSFORM,
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
        };
        Action::Custom {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            payload,
        }
    }
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

/// Change the active Viewer preview resolution scale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerSetPreviewResolutionScalePayload {
    /// Preview resolution scale requested by the UI.
    pub scale: f32,
}

/// Sequence-space position emitted by monitor direct manipulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerTransformPositionPayload {
    /// Horizontal position in Sequence pixels.
    pub x: f32,
    /// Vertical position in Sequence pixels.
    pub y: f32,
}

/// Change one Clip transform from the Viewer/monitor surface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerSetClipTransformPayload {
    /// Stable Clip identity targeted by the monitor interaction.
    pub clip_id: ClipId,
    /// Optional absolute Sequence-space position.
    pub position: Option<ViewerTransformPositionPayload>,
    /// Optional uniform scale in UI percent units.
    pub scale_percent: Option<f32>,
    /// Optional rotation in degrees.
    pub rotation_degrees: Option<f32>,
}

impl ViewerSetClipTransformPayload {
    pub(crate) const fn has_mutation(self) -> bool {
        self.position.is_some() || self.scale_percent.is_some() || self.rotation_degrees.is_some()
    }

    pub(crate) fn values_are_finite(self) -> bool {
        self.position
            .is_none_or(|position| position.x.is_finite() && position.y.is_finite())
            && self.scale_percent.is_none_or(f32::is_finite)
            && self.rotation_degrees.is_none_or(f32::is_finite)
    }
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
            ProductAction::Audio(_) => self.state.active_sequence().is_some(),
            ProductAction::Viewer(action) => self.allows_viewer(action),
            ProductAction::Project(action) => self.allows_project(action),
            ProductAction::Sequence(action) => self.allows_sequence(action),
            ProductAction::Export(action) => self.allows_export(action),
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

    fn allows_viewer(&self, action: &ViewerProductAction) -> bool {
        let Some(sequence) = self.state.active_sequence() else {
            return false;
        };
        match action {
            ViewerProductAction::SetPreviewResolutionScale(_) => true,
            ViewerProductAction::SetClipTransform(payload) => {
                payload.has_mutation()
                    && payload.values_are_finite()
                    && clip_admission_facts(sequence, payload.clip_id)
                        .is_some_and(|clip| clip.source_track_unlocked && clip.is_video_track)
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
        clip::Clip,
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
    fn external_codec_round_trips_every_viewer_product_action() {
        let clip_id = ClipId::new();
        let actions = [
            ProductAction::Viewer(ViewerProductAction::SetPreviewResolutionScale(
                ViewerSetPreviewResolutionScalePayload { scale: 0.25 },
            )),
            ProductAction::Viewer(ViewerProductAction::SetClipTransform(
                ViewerSetClipTransformPayload {
                    clip_id,
                    position: Some(ViewerTransformPositionPayload { x: 320.0, y: 180.0 }),
                    scale_percent: Some(125.0),
                    rotation_degrees: Some(8.5),
                },
            )),
        ];

        for expected in actions {
            let external = expected.clone().into_external_action();
            let decoded = ProductAction::decode_external(&external)
                .expect("valid external payload")
                .expect("recognized Viewer product action");
            assert_eq!(decoded, expected);
        }
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

        let malformed_viewer = Action::Custom {
            namespace: VIEWER_NAMESPACE.to_owned(),
            name: VIEWER_SET_CLIP_TRANSFORM.to_owned(),
            payload: serde_json::json!({"clip_id": ClipId::new(), "position": {"x": 1.0}}),
        };
        let error = ProductAction::decode_external(&malformed_viewer)
            .expect_err("recognized malformed Viewer payload fails closed");
        assert_eq!(error.dispatch_step_id(), "viewer_action.set_clip_transform");

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
        assert!(projection.allows(&ProductAction::Viewer(
            ViewerProductAction::SetClipTransform(ViewerSetClipTransformPayload {
                clip_id,
                position: Some(ViewerTransformPositionPayload { x: 10.0, y: 20.0 }),
                scale_percent: None,
                rotation_degrees: None,
            })
        )));
        assert!(!projection.allows(&ProductAction::Viewer(
            ViewerProductAction::SetClipTransform(ViewerSetClipTransformPayload {
                clip_id,
                position: None,
                scale_percent: None,
                rotation_degrees: None,
            })
        )));
        assert!(!projection.allows(&ProductAction::Viewer(
            ViewerProductAction::SetClipTransform(ViewerSetClipTransformPayload {
                clip_id: ClipId::new(),
                position: Some(ViewerTransformPositionPayload { x: 10.0, y: 20.0 }),
                scale_percent: None,
                rotation_degrees: None,
            })
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
        assert!(!locked.allows(&ProductAction::Viewer(
            ViewerProductAction::SetClipTransform(ViewerSetClipTransformPayload {
                clip_id,
                position: Some(ViewerTransformPositionPayload { x: 10.0, y: 20.0 }),
                scale_percent: None,
                rotation_degrees: None,
            })
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
