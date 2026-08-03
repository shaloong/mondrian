//! App composition seam for Sequence Audio Processor authoring.
//!
//! UI Adapters submit the closed product action; the Timeline Module owns Rack
//! mutation and validation; the App owns transaction and execution refresh.

use mondrian_core::{MondrianError, Result};
use mondrian_timeline::audio::{
    AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID, BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID,
};
use mondrian_timeline::{
    apply_audio_processor_rack_edit, AudioProcessorRackEdit, AudioProcessorRackEditRequest,
};

use super::product_action::{AudioProcessorBuiltInPreset, AudioProcessorProductAction};
use super::AppState;

impl AppState {
    pub(super) fn dispatch_audio_processor_product_action(
        &mut self,
        action: AudioProcessorProductAction,
    ) -> Result<()> {
        match action {
            AudioProcessorProductAction::EditRack(request) => {
                self.edit_audio_processor_rack(request)
            }
            AudioProcessorProductAction::InsertBuiltIn(payload) => {
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
        }
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
        audio_processor_insert_built_in_action, audio_processor_rack_edit_action,
    };
    use mondrian_core::ParameterId;
    use mondrian_timeline::audio::{
        AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
    };
    use mondrian_timeline::{
        audio_processor_rack, sequence::Sequence, AudioChannelStripOwner, AudioChannelStripRack,
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
