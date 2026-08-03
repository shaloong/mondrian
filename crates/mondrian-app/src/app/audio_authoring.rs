//! App composition seam for Sequence audio authoring.
//!
//! UI Adapters submit the closed product action; Timeline Modules own Channel
//! Strip/Rack mutation and validation; App owns transaction and execution refresh.

use mondrian_core::{MondrianError, Result};
use mondrian_timeline::audio::{
    AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID, BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID,
};
use mondrian_timeline::{
    apply_audio_channel_strip_edit, apply_audio_processor_rack_edit, AudioChannelStripEditRequest,
    AudioProcessorRackEdit, AudioProcessorRackEditRequest,
};

use super::product_action::{AudioProcessorBuiltInPreset, AudioProductAction};
use super::AppState;

impl AppState {
    pub(super) fn dispatch_audio_product_action(
        &mut self,
        action: AudioProductAction,
    ) -> Result<()> {
        match action {
            AudioProductAction::EditProcessorRack(request) => {
                self.edit_audio_processor_rack(request)
            }
            AudioProductAction::InsertBuiltInProcessor(payload) => {
                let definition_id = match payload.preset {
                    AudioProcessorBuiltInPreset::Gain => BUILTIN_GAIN_DEFINITION_ID,
                    AudioProcessorBuiltInPreset::LookaheadLimiter => {
                        BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID
                    }
                };
                self.edit_audio_processor_rack(AudioProcessorRackEditRequest {
                    address: payload.address,
                    edit: AudioProcessorRackEdit::Insert {
                        processor: AudioProcessorInstance::built_in(definition_id, 1),
                        placement: payload.placement,
                    },
                })
            }
            AudioProductAction::EditChannelStrip(request) => self.edit_audio_channel_strip(request),
        }
    }

    fn edit_audio_channel_strip(&mut self, request: AudioChannelStripEditRequest) -> Result<()> {
        let sequence_id =
            self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "audio_edit_channel_strip".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            })?;
        let outcome =
            self.commit_sequence_edit(sequence_id, "编辑音频通道条", |sequence| {
                apply_audio_channel_strip_edit(sequence, &request).map_err(|error| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "audio_edit_channel_strip".to_owned(),
                        reason: error.to_string(),
                    }
                })
            })?;
        if outcome.changed {
            self.reconcile_audio_after_committed_authoring_change("audio_edit_channel_strip");
        }
        Ok(())
    }

    fn edit_audio_processor_rack(&mut self, request: AudioProcessorRackEditRequest) -> Result<()> {
        let sequence_id =
            self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "audio_processor_edit_rack".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            })?;
        let outcome =
            self.commit_sequence_edit(sequence_id, "编辑音频处理器", |sequence| {
                apply_audio_processor_rack_edit(sequence, &request).map_err(|error| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "audio_processor_edit_rack".to_owned(),
                        reason: error.to_string(),
                    }
                })
            })?;
        if outcome.changed {
            self.reconcile_audio_after_committed_authoring_change("audio_processor_edit_rack");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{
        AudioProcessorBuiltInPreset, AudioProcessorInsertBuiltInPayload,
    };
    use crate::app::ui_actions::{
        audio_channel_strip_edit_action, audio_processor_insert_built_in_action,
        audio_processor_rack_edit_action,
    };
    use mondrian_core::ParameterId;
    use mondrian_timeline::audio::{
        AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
    };
    use mondrian_timeline::{
        audio_channel_strip, audio_processor_rack, sequence::Sequence, AudioChannelStripEdit,
        AudioChannelStripEditRequest, AudioChannelStripOwner, AudioChannelStripRack,
        AudioProcessorParameterEdit, AudioProcessorRackAddress, AudioProcessorRackEdit,
        AudioProcessorRackPlacement,
    };

    fn request(
        address: AudioProcessorRackAddress,
        edit: AudioProcessorRackEdit,
    ) -> AudioProcessorRackEditRequest {
        AudioProcessorRackEditRequest { address, edit }
    }

    #[test]
    fn typed_rack_actions_commit_once_and_round_trip_through_undo_redo() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Processor authoring");
        let track_id = sequence.audio_tracks[0].id;
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PreFader,
        };
        state.test_set_sequence(Some(sequence));
        let initial_generation = state.project_author_generation();

        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let processor_id = processor.id;
        state
            .dispatch_action(audio_processor_rack_edit_action(request(
                address,
                AudioProcessorRackEdit::Insert {
                    processor,
                    placement: AudioProcessorRackPlacement::End,
                },
            )))
            .expect("insert through typed product action");

        assert_eq!(state.project_author_generation(), initial_generation + 1);
        assert!(state.can_undo_action());
        assert_eq!(
            audio_processor_rack(state.active_sequence().expect("active Sequence"), &address)
                .expect("Track Rack")
                .processors[0]
                .id,
            processor_id
        );

        state
            .dispatch_action(audio_processor_rack_edit_action(request(
                address,
                AudioProcessorRackEdit::EditParameter {
                    processor_id,
                    parameter_id: ParameterId::new_static(GAIN_DB_PARAMETER_ID),
                    edit: AudioProcessorParameterEdit::SetStaticValue { value: -6.0 },
                },
            )))
            .expect("parameter edit through same Interface");
        assert_eq!(state.project_author_generation(), initial_generation + 2);

        assert!(state.undo_timeline().expect("undo parameter edit"));
        let gain =
            &audio_processor_rack(state.active_sequence().expect("active Sequence"), &address)
                .expect("Track Rack")
                .processors[0];
        assert_eq!(
            gain.parameters[&ParameterId::new_static(GAIN_DB_PARAMETER_ID)]
                .automation
                .default_value,
            0.0
        );
        assert!(state.undo_timeline().expect("undo insert"));
        assert!(
            audio_processor_rack(state.active_sequence().expect("active Sequence"), &address)
                .expect("Track Rack")
                .processors
                .is_empty()
        );

        assert!(state.redo_timeline().expect("redo insert"));
        assert!(state.redo_timeline().expect("redo parameter edit"));
        let gain =
            &audio_processor_rack(state.active_sequence().expect("active Sequence"), &address)
                .expect("Track Rack")
                .processors[0];
        assert_eq!(
            gain.parameters[&ParameterId::new_static(GAIN_DB_PARAMETER_ID)]
                .automation
                .default_value,
            -6.0
        );
    }

    #[test]
    fn typed_channel_strip_action_commits_once_and_round_trips_through_undo_redo() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Channel Strip authoring");
        let owner = AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id };
        state.test_set_sequence(Some(sequence));
        let initial_generation = state.project_author_generation();

        state
            .dispatch_action(audio_channel_strip_edit_action(
                AudioChannelStripEditRequest {
                    owner,
                    edit: AudioChannelStripEdit::SetFaderDb { value: -6.0 },
                },
            ))
            .expect("fader through typed product action");
        assert_eq!(state.project_author_generation(), initial_generation + 1);
        assert_eq!(
            audio_channel_strip(state.active_sequence().expect("Sequence"), owner)
                .expect("Track strip")
                .fader_db,
            -6.0
        );

        assert!(state.undo_timeline().expect("undo fader"));
        assert_eq!(
            audio_channel_strip(state.active_sequence().expect("Sequence"), owner)
                .expect("Track strip")
                .fader_db,
            0.0
        );
        assert!(state.redo_timeline().expect("redo fader"));
        assert_eq!(
            audio_channel_strip(state.active_sequence().expect("Sequence"), owner)
                .expect("Track strip")
                .fader_db,
            -6.0
        );
    }

    #[test]
    fn product_builtin_insertion_resolves_canonical_instance_at_dispatch() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Product Processor insertion");
        let track_id = sequence.audio_tracks[0].id;
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PreFader,
        };
        state.test_set_sequence(Some(sequence));
        let generation = state.project_author_generation();

        state
            .dispatch_action(audio_processor_insert_built_in_action(
                AudioProcessorInsertBuiltInPayload {
                    address,
                    preset: AudioProcessorBuiltInPreset::LookaheadLimiter,
                    placement: AudioProcessorRackPlacement::End,
                },
            ))
            .expect("insert canonical built-in");

        let processor =
            &audio_processor_rack(state.active_sequence().expect("active Sequence"), &address)
                .expect("Rack")
                .processors[0];
        assert!(matches!(
            &processor.definition,
            mondrian_timeline::audio::AudioProcessorDefinitionRef::BuiltIn {
                definition_id,
                schema_version: 1,
            } if definition_id == BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID
        ));
        assert_eq!(processor.parameters.len(), 3);
        assert_eq!(state.project_author_generation(), generation + 1);
    }

    #[test]
    fn no_op_rack_action_does_not_advance_author_generation() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Processor no-op");
        let track_id = sequence.audio_tracks[0].id;
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PostFader,
        };
        state.test_set_sequence(Some(sequence));
        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let processor_id = processor.id;
        state
            .dispatch_action(audio_processor_rack_edit_action(request(
                address,
                AudioProcessorRackEdit::Insert {
                    processor,
                    placement: AudioProcessorRackPlacement::End,
                },
            )))
            .expect("insert Processor");
        let generation = state.project_author_generation();

        state
            .dispatch_action(audio_processor_rack_edit_action(request(
                address,
                AudioProcessorRackEdit::SetBypassed { processor_id, bypassed: false },
            )))
            .expect("idempotent bypass edit");

        assert_eq!(state.project_author_generation(), generation);
    }

    #[test]
    fn rejected_locked_track_action_does_not_commit_partial_author_state() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Locked Processor Rack");
        let track_id = sequence.audio_tracks[0].id;
        sequence.audio_tracks[0].is_locked = true;
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PreFader,
        };
        state.test_set_sequence(Some(sequence));
        let generation = state.project_author_generation();

        let error = state
            .dispatch_action(audio_processor_rack_edit_action(request(
                address,
                AudioProcessorRackEdit::Insert {
                    processor: AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
                    placement: AudioProcessorRackPlacement::End,
                },
            )))
            .expect_err("locked Track rejects Rack mutation");

        assert!(error.to_string().contains("locked Track"));
        assert_eq!(state.project_author_generation(), generation);
        assert!(!state.can_undo_action());
        assert!(
            audio_processor_rack(state.active_sequence().expect("active Sequence"), &address)
                .expect("Track Rack")
                .processors
                .is_empty()
        );
    }
}
