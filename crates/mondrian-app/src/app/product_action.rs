//! Closed product Actions and the external custom-action Adapter.
//!
//! Product code dispatches and admits the typed algebra in this Module.
//! `Action::Custom` remains a compatibility transport for reusable Widgets,
//! scripting, and future plugin Adapters; it is decoded only at this seam.

use std::collections::HashMap;

use mondrian_core::types::{ClipId, FramePosition, TrackId};
use mondrian_core::{Rational, TimelineTime};
use mondrian_editor_state::Action;
use mondrian_timeline::{
    sequence::Sequence, AudioChannelStripEditRequest, AudioProcessorRackEditRequest,
    AudioRoutingEditRequest,
};
use serde::{Deserialize, Serialize};

use super::AppState;

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
}

/// Closed Sequence audio authoring operations.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioProductAction {
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

/// Stable read-only admission projection for high-frequency Timeline actions.
///
/// Its facts are deliberately private. UI Adapters can ask whether a typed
/// operation is currently useful, but cannot observe or reinterpret Sequence,
/// Track, Clip, authoring-session, or execution internals.
#[derive(Debug, Clone, Default)]
pub struct TimelineInteractionProjection {
    sequence: Option<TimelineInteractionSequenceFacts>,
}

#[derive(Debug, Clone)]
struct TimelineInteractionSequenceFacts {
    time_base: Rational,
    tracks: HashMap<(TrackId, bool), TimelineInteractionTrackFacts>,
    clips: HashMap<ClipId, TimelineInteractionClipFacts>,
}

#[derive(Debug, Clone, Copy)]
struct TimelineInteractionTrackFacts {
    unlocked: bool,
}

#[derive(Debug, Clone, Copy)]
struct TimelineInteractionClipFacts {
    source_track_unlocked: bool,
    is_video_track: bool,
    position: TimelineTime,
    end: Option<TimelineTime>,
}

impl TimelineInteractionProjection {
    fn from_sequence(sequence: Option<&Sequence>) -> Self {
        let Some(sequence) = sequence else {
            return Self::default();
        };

        let mut facts = TimelineInteractionSequenceFacts {
            time_base: sequence.time_base(),
            tracks: HashMap::new(),
            clips: HashMap::new(),
        };
        for track in &sequence.video_tracks {
            facts.insert_track(track, true);
        }
        for track in &sequence.audio_tracks {
            facts.insert_track(track, false);
        }
        Self { sequence: Some(facts) }
    }

    /// Return whether a typed Timeline operation has a valid current target.
    ///
    /// This is an early, read-only UI projection. The owning mutation or
    /// transport Interface still revalidates authoritative state at dispatch.
    pub fn allows(&self, action: &TimelineProductAction) -> bool {
        let Some(sequence) = &self.sequence else {
            return false;
        };
        match action {
            TimelineProductAction::SelectClip(payload) => {
                sequence.clips.contains_key(&payload.clip_id)
            }
            TimelineProductAction::MoveClip(payload) => {
                sequence.clips.get(&payload.clip_id).is_some_and(|clip| {
                    clip.source_track_unlocked && clip.is_video_track == payload.is_video_track
                }) && sequence
                    .tracks
                    .get(&(payload.target_track_id, payload.is_video_track))
                    .is_some_and(|track| track.unlocked)
            }
            TimelineProductAction::TrimClips(payload) => sequence.allows_trim(payload),
            TimelineProductAction::Seek(payload) => payload.frame >= 0,
        }
    }
}

impl TimelineInteractionSequenceFacts {
    fn insert_track(&mut self, track: &mondrian_timeline::track::Track, is_video_track: bool) {
        let unlocked = !track.is_locked;
        self.tracks.insert(
            (track.id, is_video_track),
            TimelineInteractionTrackFacts { unlocked },
        );
        for clip in &track.clips {
            self.clips.insert(
                clip.id,
                TimelineInteractionClipFacts {
                    source_track_unlocked: unlocked,
                    is_video_track,
                    position: clip.position,
                    end: clip.end_position().ok(),
                },
            );
        }
    }

    fn allows_trim(&self, payload: &TimelineTrimClipsPayload) -> bool {
        if payload.clip_ids.is_empty() {
            return false;
        }
        let Ok(target) =
            TimelineTime::from_frame_position(FramePosition::new(payload.frame, self.time_base))
        else {
            return false;
        };
        payload.clip_ids.iter().all(|clip_id| {
            self.clips.get(clip_id).is_some_and(|clip| {
                clip.source_track_unlocked
                    && clip.end.is_some_and(|end| target > clip.position && target < end)
            })
        })
    }
}

impl AppState {
    /// Project the minimal App-owned facts needed to admit Timeline interactions.
    pub fn timeline_interaction_projection(&self) -> TimelineInteractionProjection {
        TimelineInteractionProjection::from_sequence(self.active_sequence())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::AssetId;
    use mondrian_timeline::{
        audio::{AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID},
        clip::Clip,
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
    }

    #[test]
    fn interaction_projection_hides_state_but_preserves_lock_and_range_admission() {
        let mut sequence = Sequence::new("Edit");
        let time_base = sequence.time_base();
        let clip =
            Clip::new(AssetId::new(), tt(10, time_base), tt(20, time_base)).expect("valid clip");
        let clip_id = clip.id;
        let track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");

        let projection = TimelineInteractionProjection::from_sequence(Some(&sequence));
        assert!(projection.allows(&TimelineProductAction::SelectClip(
            TimelineSelectClipPayload {
                clip_id,
                mode: TimelineClipSelectionModePayload::Replace,
            },
        )));
        assert!(
            projection.allows(&TimelineProductAction::MoveClip(TimelineMoveClipPayload {
                target_track_id: track_id,
                is_video_track: true,
                clip_id,
                frame: 12,
            }))
        );
        assert!(
            !projection.allows(&TimelineProductAction::MoveClip(TimelineMoveClipPayload {
                target_track_id: sequence.audio_tracks[0].id,
                is_video_track: false,
                clip_id,
                frame: 12,
            }))
        );
        assert!(projection.allows(&TimelineProductAction::TrimClips(
            TimelineTrimClipsPayload {
                clip_ids: vec![clip_id],
                edge: TimelineTrimPayloadEdge::In,
                frame: 15,
            },
        )));
        assert!(!projection.allows(&TimelineProductAction::TrimClips(
            TimelineTrimClipsPayload {
                clip_ids: vec![clip_id],
                edge: TimelineTrimPayloadEdge::In,
                frame: 10,
            },
        )));

        sequence.video_tracks[0].is_locked = true;
        let locked = TimelineInteractionProjection::from_sequence(Some(&sequence));
        assert!(
            !locked.allows(&TimelineProductAction::MoveClip(TimelineMoveClipPayload {
                target_track_id: track_id,
                is_video_track: true,
                clip_id,
                frame: 12,
            }))
        );
        assert!(!locked.allows(&TimelineProductAction::TrimClips(
            TimelineTrimClipsPayload {
                clip_ids: vec![clip_id],
                edge: TimelineTrimPayloadEdge::Out,
                frame: 20,
            },
        )));
        assert!(
            locked.allows(&TimelineProductAction::Seek(TimelineSeekPayload {
                frame: 0,
                source: TimelineSeekSource::Settled,
            }))
        );
        assert!(
            !locked.allows(&TimelineProductAction::Seek(TimelineSeekPayload {
                frame: -1,
                source: TimelineSeekSource::Settled
            }))
        );

        assert!(
            !TimelineInteractionProjection::default().allows(&TimelineProductAction::Seek(
                TimelineSeekPayload { frame: 0, source: TimelineSeekSource::Settled }
            ),)
        );
    }
}
