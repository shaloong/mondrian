//! App composition seam for Sequence audio authoring.
//!
//! UI Adapters submit the closed product action; Timeline Modules own Channel
//! Strip/Rack mutation and validation; App owns transaction and execution refresh.

#[cfg(test)]
mod musical_reference_tests;

use crate::app::AppMessage;
use mondrian_audio::{
    AudioProcessingMode, AudioRenderContract, ClapPluginDescriptor, Vst3PluginDescriptor,
};
use mondrian_core::{MondrianError, Result};
use mondrian_timeline::audio::{
    AudioComponentSource, AudioProcessorDefinitionRef, AudioProcessorInstance,
    BUILTIN_GAIN_DEFINITION_ID, BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID,
};
use mondrian_timeline::{
    apply_audio_automation_edit, apply_audio_channel_strip_edit, apply_audio_component_edit,
    apply_audio_processor_rack_edit, apply_audio_routing_edit, inspect_audio_component,
    inspect_audio_processor_rack, AudioAutomationEditRequest, AudioChannelStripEditRequest,
    AudioComponentEditBlocker, AudioComponentEditRequest, AudioComponentMutation,
    AudioProcessorRackEdit, AudioProcessorRackEditRequest, AudioRoutingEditRequest,
};

use super::product_action::{
    AudioProcessorBuiltInPreset, AudioProcessorRebindClapPayload, AudioProcessorRebindVst3Payload,
    AudioProductAction,
};
use super::AppState;

impl AppState {
    fn select_audio_scope_occurrence(
        &mut self,
        payload: super::product_action::AudioScopeOccurrencePayload,
    ) -> Result<()> {
        let failure = |reason: String| MondrianError::WorkflowStepFailed {
            step_id: "audio_select_scope_occurrence".into(),
            reason,
        };
        let sequence =
            self.active_sequence().ok_or_else(|| failure("No active Sequence".into()))?;
        if sequence.id != payload.sequence_id
            || self
                .primary_selected_clip()
                .is_none_or(|clip| clip.clip_id != payload.address.clip_id)
        {
            return Err(failure(
                "Scope occurrence no longer belongs to the inspected Clip".into(),
            ));
        }
        let occurrence = inspect_audio_component(sequence, payload.address)
            .map_err(|error| failure(error.to_string()))?;
        if occurrence.component().processing.scope_id != payload.scope_id {
            return Err(failure(
                "Component occurrence belongs to another Scope".into(),
            ));
        }
        self.selection.audio_scope_occurrences.retain(|(sequence_id, _), address| {
            *sequence_id == payload.sequence_id && address.clip_id == payload.address.clip_id
        });
        self.selection
            .audio_scope_occurrences
            .insert((payload.sequence_id, payload.scope_id), payload.address);
        Ok(())
    }

    pub(super) fn dispatch_audio_product_action(
        &mut self,
        action: AudioProductAction,
    ) -> Result<()> {
        match action {
            AudioProductAction::QueryNativeParameter(payload) => {
                self.query_native_audio_parameter_product(payload)
            }
            AudioProductAction::NativeEditor(payload) => {
                self.control_native_audio_editor_product(payload)
            }
            AudioProductAction::RewireProcessorInput(payload) => {
                self.rewire_observed_processor_input(payload)
            }
            AudioProductAction::ConnectProcessorInput(payload) => {
                self.connect_observed_processor_input(payload)
            }
            AudioProductAction::QueryProcessorInputs(payload) => {
                self.query_audio_processor_inputs(payload)
            }
            AudioProductAction::SelectScopeOccurrence(payload) => {
                self.select_audio_scope_occurrence(payload)
            }
            AudioProductAction::SetTrackSolo(payload) => self.set_audio_track_solo(payload),
            AudioProductAction::EditAutomation(request) => self.edit_audio_automation(request),
            AudioProductAction::CurveClipboard(payload) => {
                self.dispatch_audio_curve_clipboard(*payload)
            }
            AudioProductAction::EditComponent(request) => self.edit_audio_component(request),
            AudioProductAction::EditProcessorRack(request) => {
                if matches!(request.edit, AudioProcessorRackEdit::RebindNative { .. }) {
                    return Err(clap_unavailable(
                        "原生插件重新绑定必须通过已安装插件探测入口完成",
                    ));
                }
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
            AudioProductAction::InstallClapLibrary(payload) => {
                let catalog = self
                    .clap_catalog
                    .as_ref()
                    .ok_or_else(|| clap_unavailable("native CLAP helper is unavailable"))?;
                let descriptors = catalog
                    .install_library(payload.path)
                    .map_err(|error| clap_unavailable(error.to_string()))?;
                self.audio_input_observations.invalidate();
                self.set_status_hint(
                    AppMessage::new("status-clap-installed").with_count("count", descriptors.len()),
                    false,
                );
                if !descriptors.is_empty() && self.active_sequence().is_some() {
                    self.reconcile_audio_after_committed_authoring_change("clap_install_library");
                }
                Ok(())
            }
            AudioProductAction::InsertClapProcessor(payload) => {
                let channel_layout = self
                    .active_sequence()
                    .ok_or_else(|| clap_unavailable("当前没有活动序列"))?
                    .settings
                    .audio_channel_layout;
                let catalog = self
                    .clap_catalog
                    .as_ref()
                    .ok_or_else(|| clap_unavailable("native CLAP helper is unavailable"))?;
                let processor = catalog
                    .create_instance(
                        &payload.plugin_id,
                        AudioRenderContract {
                            sample_rate: self.audio_sample_rate,
                            channel_layout,
                            max_block_frames: super::audio_rendering::MAX_AUDIO_RENDER_BLOCK_FRAMES,
                            processing_mode: AudioProcessingMode::Realtime,
                            processor_session_scratch_budget_bytes: AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
                            public_output_lookahead_budget_frames: AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
                            compensation_delay_scratch_budget_bytes: AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
                        },
                        None,
                    )
                    .map_err(|error| clap_unavailable(error.to_string()))?;
                self.edit_audio_processor_rack(AudioProcessorRackEditRequest {
                    address: payload.address,
                    edit: AudioProcessorRackEdit::Insert {
                        processor,
                        placement: payload.placement,
                    },
                })
            }
            AudioProductAction::RebindClapProcessor(payload) => self.rebind_clap_processor(payload),
            AudioProductAction::InstallVst3Plugin(payload) => {
                let catalog = self
                    .vst3_catalog
                    .as_ref()
                    .ok_or_else(|| vst3_unavailable("native VST3 helper is unavailable"))?;
                let descriptors = catalog
                    .install_plugin(payload.path)
                    .map_err(|error| vst3_unavailable(error.to_string()))?;
                self.audio_input_observations.invalidate();
                self.set_status_hint(
                    AppMessage::new("status-vst3-installed").with_count("count", descriptors.len()),
                    false,
                );
                if !descriptors.is_empty() && self.active_sequence().is_some() {
                    self.reconcile_audio_after_committed_authoring_change("vst3_install_plugin");
                }
                Ok(())
            }
            AudioProductAction::InsertVst3Processor(payload) => {
                let layout = self
                    .active_sequence()
                    .ok_or_else(|| vst3_unavailable("当前没有活动序列"))?
                    .settings
                    .audio_channel_layout;
                let catalog = self
                    .vst3_catalog
                    .as_ref()
                    .ok_or_else(|| vst3_unavailable("native VST3 helper is unavailable"))?;
                let processor = catalog
                    .create_instance(&payload.class_id, self.plugin_render_contract(layout), None)
                    .map_err(|error| vst3_unavailable(error.to_string()))?;
                self.edit_audio_processor_rack(AudioProcessorRackEditRequest {
                    address: payload.address,
                    edit: AudioProcessorRackEdit::Insert {
                        processor,
                        placement: payload.placement,
                    },
                })
            }
            AudioProductAction::RebindVst3Processor(payload) => self.rebind_vst3_processor(payload),
            AudioProductAction::EditChannelStrip(request) => self.edit_audio_channel_strip(request),
            AudioProductAction::EditRouting(request) => self.edit_audio_routing(request),
        }
    }

    /// Session-visible CLAP definitions for Inspector and Mixer insertion menus.
    pub fn installed_clap_processors(&self) -> Result<Vec<ClapPluginDescriptor>> {
        let catalog = self
            .clap_catalog
            .as_ref()
            .ok_or_else(|| clap_unavailable("native CLAP helper is unavailable"))?;
        catalog.descriptors().map_err(|error| clap_unavailable(error.to_string()))
    }

    /// Session-visible VST3 effect classes for Inspector and Mixer insertion menus.
    pub fn installed_vst3_processors(&self) -> Result<Vec<Vst3PluginDescriptor>> {
        let catalog = self
            .vst3_catalog
            .as_ref()
            .ok_or_else(|| vst3_unavailable("native VST3 helper is unavailable"))?;
        catalog.descriptors().map_err(|error| vst3_unavailable(error.to_string()))
    }

    pub(super) fn plugin_render_contract(
        &self,
        channel_layout: mondrian_core::AudioChannelLayout,
    ) -> AudioRenderContract {
        AudioRenderContract {
            sample_rate: self.audio_sample_rate,
            channel_layout,
            max_block_frames: super::audio_rendering::MAX_AUDIO_RENDER_BLOCK_FRAMES,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            public_output_lookahead_budget_frames:
                AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        }
    }

    fn rebind_vst3_processor(&mut self, payload: AudioProcessorRebindVst3Payload) -> Result<()> {
        let sequence =
            self.active_sequence().ok_or_else(|| vst3_unavailable("当前没有活动序列"))?;
        let inspection = inspect_audio_processor_rack(sequence, &payload.address)
            .map_err(|error| vst3_unavailable(error.to_string()))?;
        if let Some(blocker) = inspection.edit_blocker() {
            return Err(vst3_unavailable(blocker.to_string()));
        }
        let old = inspection
            .rack()
            .processors
            .iter()
            .find(|processor| processor.id == payload.processor_id)
            .ok_or_else(|| vst3_unavailable("目标 VST3 处理器不存在"))?
            .clone();
        let AudioProcessorDefinitionRef::Vst3 { class_id, .. } = &old.definition else {
            return Err(vst3_unavailable("只能重新绑定 VST3 处理器"));
        };
        let layout = sequence.settings.audio_channel_layout;
        let catalog = self
            .vst3_catalog
            .as_ref()
            .ok_or_else(|| vst3_unavailable("native VST3 helper is unavailable"))?;
        let candidate = catalog
            .create_instance(
                class_id,
                self.plugin_render_contract(layout),
                old.opaque_state.as_ref().map(|state| state.iter().copied().collect()),
            )
            .map_err(|error| vst3_unavailable(error.to_string()))?;
        if old.parameters.len() != candidate.parameters.len()
            || old.parameters.iter().any(|(id, parameter)| {
                candidate.parameters.get(id).is_none_or(|new| new.schema != parameter.schema)
            })
        {
            return Err(vst3_unavailable(
                "插件参数架构与项目中保存的架构不匹配；原自动化已保留",
            ));
        }
        self.edit_audio_processor_rack(AudioProcessorRackEditRequest {
            address: payload.address,
            edit: AudioProcessorRackEdit::RebindNative {
                processor_id: payload.processor_id,
                expected_definition: old.definition,
                new_definition: candidate.definition,
            },
        })
    }

    fn rebind_clap_processor(&mut self, payload: AudioProcessorRebindClapPayload) -> Result<()> {
        let sequence =
            self.active_sequence().ok_or_else(|| clap_unavailable("当前没有活动序列"))?;
        let inspection = inspect_audio_processor_rack(sequence, &payload.address)
            .map_err(|error| clap_unavailable(error.to_string()))?;
        if let Some(blocker) = inspection.edit_blocker() {
            return Err(clap_unavailable(blocker.to_string()));
        }
        let old = inspection
            .rack()
            .processors
            .iter()
            .find(|processor| processor.id == payload.processor_id)
            .ok_or_else(|| clap_unavailable("目标 CLAP 处理器不存在"))?
            .clone();
        let AudioProcessorDefinitionRef::Clap { plugin_id, .. } = &old.definition else {
            return Err(clap_unavailable("只能重新绑定 CLAP 处理器"));
        };
        let channel_layout = sequence.settings.audio_channel_layout;
        let catalog = self
            .clap_catalog
            .as_ref()
            .ok_or_else(|| clap_unavailable("native CLAP helper is unavailable"))?;
        let candidate = catalog
            .create_instance(
                plugin_id,
                AudioRenderContract {
                    sample_rate: self.audio_sample_rate,
                    channel_layout,
                    max_block_frames: super::audio_rendering::MAX_AUDIO_RENDER_BLOCK_FRAMES,
                    processing_mode: AudioProcessingMode::Realtime,
                    processor_session_scratch_budget_bytes:
                        AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
                    public_output_lookahead_budget_frames:
                        AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
                    compensation_delay_scratch_budget_bytes:
                        AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
                },
                old.opaque_state.as_ref().map(|state| state.iter().copied().collect()),
            )
            .map_err(|error| clap_unavailable(error.to_string()))?;
        if old.parameters.len() != candidate.parameters.len()
            || old.parameters.iter().any(|(id, parameter)| {
                candidate.parameters.get(id).is_none_or(|new| new.schema != parameter.schema)
            })
        {
            return Err(clap_unavailable(
                "插件参数架构与项目中保存的架构不匹配；原自动化已保留",
            ));
        }
        self.edit_audio_processor_rack(AudioProcessorRackEditRequest {
            address: payload.address,
            edit: AudioProcessorRackEdit::RebindNative {
                processor_id: payload.processor_id,
                expected_definition: old.definition,
                new_definition: candidate.definition,
            },
        })
    }

    pub(super) fn edit_audio_automation(
        &mut self,
        request: AudioAutomationEditRequest,
    ) -> Result<()> {
        let sequence_id =
            self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "audio_edit_automation".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            })?;
        self.commit_sequence_edit(sequence_id, "编辑音频自动化", |sequence| {
            apply_audio_automation_edit(sequence, &request).map_err(|error| {
                MondrianError::WorkflowStepFailed {
                    step_id: "audio_edit_automation".to_owned(),
                    reason: error.to_string(),
                }
            })
        })?;
        Ok(())
    }

    fn edit_audio_component(&mut self, request: AudioComponentEditRequest) -> Result<()> {
        self.validate_audio_component_source_dependency(&request)?;
        let sequence_id =
            self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "audio_edit_component".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            })?;
        let outcome =
            self.commit_sequence_edit(sequence_id, "调整片段音频 Component", |sequence| {
                apply_audio_component_edit(sequence, &request).map_err(|error| {
                    MondrianError::WorkflowStepFailed {
                        step_id: "audio_edit_component".to_owned(),
                        reason: error.to_string(),
                    }
                })
            })?;
        if !outcome.changed {
            return Err(MondrianError::ActionNotExecuted {
                action: "audio_edit_component".to_owned(),
                reason: "Audio Component already has the requested value".to_owned(),
            });
        }
        Ok(())
    }

    fn validate_audio_component_source_dependency(
        &self,
        request: &AudioComponentEditRequest,
    ) -> Result<()> {
        const STEP_ID: &str = "audio_edit_component";
        let AudioComponentMutation::SetSource { value } = &request.mutation else {
            return Ok(());
        };
        let sequence = self.active_sequence().ok_or_else(|| MondrianError::WorkflowStepFailed {
            step_id: STEP_ID.to_owned(),
            reason: "当前没有活动序列".to_owned(),
        })?;
        let inspection = inspect_audio_component(sequence, request.address).map_err(|error| {
            MondrianError::WorkflowStepFailed {
                step_id: STEP_ID.to_owned(),
                reason: error.to_string(),
            }
        })?;
        if let Some(AudioComponentEditBlocker::LockedTrack(track_id)) = inspection.edit_blocker() {
            return Err(MondrianError::TrackLocked { track_id: track_id.to_string() });
        }
        if &inspection.component().source == value {
            return Ok(());
        }

        let clip = inspection.owning_clip();
        match value {
            AudioComponentSource::Media { component_id } => {
                let asset_id =
                    clip.media_asset_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                        step_id: STEP_ID.to_owned(),
                        reason: "only a media Clip can select an Asset audio Component".to_owned(),
                    })?;
                let library =
                    self.asset_library().ok_or_else(|| MondrianError::WorkflowStepFailed {
                        step_id: STEP_ID.to_owned(),
                        reason: "asset library is unavailable".to_owned(),
                    })?;
                let asset = library.get_asset(asset_id)?.ok_or_else(|| {
                    MondrianError::AssetNotFound { asset_id: asset_id.to_string() }
                })?;
                asset.audio_components.validate().map_err(|error| {
                    MondrianError::WorkflowStepFailed {
                        step_id: STEP_ID.to_owned(),
                        reason: format!("invalid Asset audio Component catalog: {error}"),
                    }
                })?;
                if !asset
                    .audio_components
                    .components
                    .iter()
                    .any(|component| component.id == *component_id)
                {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: STEP_ID.to_owned(),
                        reason: format!(
                            "Asset {asset_id} does not expose audio Component {component_id}"
                        ),
                    });
                }
            }
            AudioComponentSource::NestedOutput { output_id } => {
                let child_id =
                    clip.nested_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                        step_id: STEP_ID.to_owned(),
                        reason: "only a nested Sequence Clip can select a child Program Output"
                            .to_owned(),
                    })?;
                let child =
                    self.sequences().iter().find(|candidate| candidate.id == child_id).ok_or_else(
                        || MondrianError::WorkflowStepFailed {
                            step_id: STEP_ID.to_owned(),
                            reason: format!("nested Sequence {child_id} is unavailable"),
                        },
                    )?;
                if !child.audio_program.outputs.iter().any(|output| output.id == *output_id) {
                    return Err(MondrianError::WorkflowStepFailed {
                        step_id: STEP_ID.to_owned(),
                        reason: format!(
                            "nested Sequence {child_id} does not expose output {output_id}"
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    fn admit_observed_processor_input(
        &self,
        payload: &super::product_action::AudioProcessorInputConnectionPayload,
        step_id: &str,
    ) -> Result<()> {
        let failure = |reason: &str| MondrianError::WorkflowStepFailed {
            step_id: step_id.to_owned(),
            reason: reason.to_owned(),
        };
        let super::audio_input_observation::AudioInputObservation::Ready(observed) =
            self.audio_processor_input_observation(payload.binding.query_payload())
        else {
            return Err(failure(
                "Processor input observation is unavailable; inspect inputs again",
            ));
        };
        if observed.binding != payload.binding {
            return Err(failure(
                "Processor input observation has changed; refresh the connection menu",
            ));
        }
        if !observed
            .inputs
            .auxiliary_inputs
            .buses
            .iter()
            .any(|bus| bus.bus_key == payload.bus_key)
        {
            return Err(failure(
                "Requested auxiliary input is absent from the observed contract",
            ));
        }
        Ok(())
    }

    fn connect_observed_processor_input(
        &mut self,
        payload: super::product_action::AudioProcessorInputConnectionPayload,
    ) -> Result<()> {
        self.admit_observed_processor_input(&payload, "audio_connect_processor_input")?;
        self.edit_audio_routing(AudioRoutingEditRequest {
            edit: mondrian_timeline::AudioRoutingEdit::CreateProcessorSidechain {
                source: payload.source,
                processor_id: payload.binding.processor_id,
                bus_key: payload.bus_key,
            },
        })
    }

    fn rewire_observed_processor_input(
        &mut self,
        payload: super::product_action::AudioProcessorInputRewirePayload,
    ) -> Result<()> {
        let failure = |reason: &str| MondrianError::WorkflowStepFailed {
            step_id: "audio_rewire_processor_input".to_owned(),
            reason: reason.to_owned(),
        };
        self.admit_observed_processor_input(&payload.connection, "audio_rewire_processor_input")?;
        let sequence = self.active_sequence().ok_or_else(|| failure("No active Sequence"))?;
        let inspected =
            mondrian_timeline::inspect_audio_processor_sidechain(sequence, payload.route_id)
                .map_err(|error| failure(&error.to_string()))?;
        let route = inspected.route();
        if route.source != payload.expected_source
            || route.processor_id != payload.connection.binding.processor_id
            || route.bus_key != payload.connection.bus_key
        {
            return Err(failure(
                "Processor input Route has changed; refresh the source menu",
            ));
        }
        self.edit_audio_routing(AudioRoutingEditRequest {
            edit: mondrian_timeline::AudioRoutingEdit::SetProcessorSidechainEndpoints {
                route_id: payload.route_id,
                source: payload.connection.source,
                processor_id: payload.connection.binding.processor_id,
                bus_key: payload.connection.bus_key,
            },
        })
    }

    fn edit_audio_routing(&mut self, request: AudioRoutingEditRequest) -> Result<()> {
        let sequence_id =
            self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "audio_edit_routing".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            })?;
        self.commit_sequence_edit(sequence_id, "编辑音频路由", |sequence| {
            apply_audio_routing_edit(sequence, &request).map_err(|error| {
                MondrianError::WorkflowStepFailed {
                    step_id: "audio_edit_routing".to_owned(),
                    reason: error.to_string(),
                }
            })
        })?;
        Ok(())
    }

    fn edit_audio_channel_strip(&mut self, request: AudioChannelStripEditRequest) -> Result<()> {
        let sequence_id =
            self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "audio_edit_channel_strip".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            })?;
        self.commit_sequence_edit(sequence_id, "编辑音频通道条", |sequence| {
            apply_audio_channel_strip_edit(sequence, &request).map_err(|error| {
                MondrianError::WorkflowStepFailed {
                    step_id: "audio_edit_channel_strip".to_owned(),
                    reason: error.to_string(),
                }
            })
        })?;
        Ok(())
    }

    fn edit_audio_processor_rack(&mut self, request: AudioProcessorRackEditRequest) -> Result<()> {
        let sequence_id =
            self.active_sequence_id().ok_or_else(|| MondrianError::WorkflowStepFailed {
                step_id: "audio_processor_edit_rack".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            })?;
        self.commit_sequence_edit(sequence_id, "编辑音频处理器", |sequence| {
            apply_audio_processor_rack_edit(sequence, &request).map_err(|error| {
                MondrianError::WorkflowStepFailed {
                    step_id: "audio_processor_edit_rack".to_owned(),
                    reason: error.to_string(),
                }
            })
        })?;
        Ok(())
    }
}

fn clap_unavailable(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "audio_clap_plugin".to_owned(),
        reason: reason.into(),
    }
}

fn vst3_unavailable(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "audio_vst3_plugin".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{
        AudioInstallClapLibraryPayload, AudioInstallVst3PluginPayload, AudioProcessorBuiltInPreset,
        AudioProcessorInsertBuiltInPayload, AudioProcessorInsertClapPayload,
        AudioProcessorInsertVst3Payload, AudioProcessorRebindClapPayload,
        AudioProcessorRebindVst3Payload, ProductAction,
    };
    use crate::app::ui_actions::{
        audio_automation_edit_action, audio_channel_strip_edit_action,
        audio_processor_insert_built_in_action, audio_processor_rack_edit_action,
        audio_routing_edit_action,
    };
    use mondrian_core::ParameterId;
    use mondrian_timeline::audio::{
        AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
    };
    use mondrian_timeline::{
        audio_channel_strip, audio_processor_rack, sequence::Sequence, AudioAutomationEdit,
        AudioAutomationEditRequest, AudioAutomationTarget, AudioChannelStripEdit,
        AudioChannelStripEditRequest, AudioChannelStripOwner, AudioChannelStripRack,
        AudioProcessorRackAddress, AudioProcessorRackEdit, AudioProcessorRackPlacement,
        AudioRouteDestination, AudioRoutingEdit, AudioRoutingEditRequest,
    };

    fn request(
        address: AudioProcessorRackAddress,
        edit: AudioProcessorRackEdit,
    ) -> AudioProcessorRackEditRequest {
        AudioProcessorRackEditRequest { address, edit }
    }

    #[test]
    fn parameter_reset_audio_roundtrips_constrained_history_and_unicode_archives() {
        use mondrian_core::{ExactAutomationKeyframe, InterpolationType, TimelineTime};
        let mut sequence = Sequence::new("Audio reset");
        let track_id = sequence.audio_tracks[0].id;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        for (time, value) in [(0, -12.0), (1, -6.0), (2, -3.0)] {
            mondrian_timeline::apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe {
                        keyframe: ExactAutomationKeyframe::linear(
                            TimelineTime::new(time, 1).expect("time"),
                            value,
                        ),
                    },
                },
            )
            .expect("seed curve");
        }
        let key = mondrian_timeline::inspect_audio_automation(&sequence, &target)
            .expect("target")
            .curve()
            .expect("curve")
            .keyframes[1]
            .id;
        mondrian_timeline::apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::SetInterpolation {
                    keyframe_id: key,
                    interpolation: InterpolationType::AutoBezier,
                },
            },
        )
        .expect("constrained curve");
        let original = sequence.clone();
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        let action = || {
            audio_automation_edit_action(AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::ResetToDefault,
            })
        };
        let generation = state.project_author_generation();
        state.active_sequence_mut_uncommitted().expect("lock").audio_tracks[0].is_locked = true;
        let locked = state.active_sequence().expect("locked").clone();
        state.dispatch_action(action()).expect_err("locked reset");
        assert!(state
            .active_sequence()
            .expect("unchanged")
            .author_state_eq_ignoring_revision(&locked));
        assert_eq!(state.project_author_generation(), generation);
        state.active_sequence_mut_uncommitted().expect("unlock").audio_tracks[0].is_locked = false;
        state.dispatch_action(action()).expect("reset");
        let edited = state.active_sequence().expect("reset").clone();
        let inspection =
            mondrian_timeline::inspect_audio_automation(&edited, &target).expect("target");
        assert!(inspection.curve().is_none());
        assert_eq!(
            inspection.default_value(),
            0.0,
            "definition default, not last sampled value"
        );
        assert_eq!(state.project_author_generation(), generation + 1);
        let history = state.authoring_history().expect("history").diagnostics();
        let _ = state.dispatch_action(action());
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        assert!(state.undo_timeline().expect("one Undo"));
        assert!(!state.can_undo_action());
        assert!(state
            .active_sequence()
            .expect("undone")
            .author_state_eq_ignoring_revision(&original));
        let directory = tempfile::tempdir().expect("archives");
        let undone = directory.path().join("音频参数重置撤销🙂.mdp");
        state.save_project_file_as(undone.clone()).expect("save undone");
        assert!(state.can_redo_action());
        let mut reopened = AppState::new();
        reopened.open_project_file(undone).expect("reopen original");
        assert!(reopened
            .active_sequence()
            .expect("original archive")
            .author_state_eq_ignoring_revision(&original));
        assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
        drop(reopened);
        assert!(state.redo_timeline().expect("Redo after save"));
        let redone = directory.path().join("音频参数重置重做🙂.mdp");
        state.save_project_file_as(redone.clone()).expect("save reset");
        drop(state);
        let mut reopened = AppState::new();
        reopened.open_project_file(redone).expect("reopen reset");
        assert!(reopened
            .active_sequence()
            .expect("reset archive")
            .author_state_eq_ignoring_revision(&edited));
        assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
    }

    #[test]
    fn batch_key_removal_audio_action_preserves_atomic_history_and_track_locks() {
        use mondrian_core::{ExactAutomationKeyframe, InterpolationType, KeyframeId, TimelineTime};
        for preset in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let mut sequence = Sequence::new("Batch automation");
            let track_id = sequence.audio_tracks[0].id;
            let target = AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id },
            };
            let mut ids = Vec::new();
            for (time, value) in [(0, -12.0), (1, -6.0), (2, 0.0)] {
                let key = ExactAutomationKeyframe::linear(
                    TimelineTime::new(time, 1).expect("time"),
                    value,
                );
                ids.push(key.id);
                mondrian_timeline::apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe { keyframe: key },
                    },
                )
                .expect("seed");
            }
            mondrian_timeline::apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::SetInterpolation {
                        keyframe_id: ids[1],
                        interpolation: preset,
                    },
                },
            )
            .expect("mode");
            let mut state = AppState::new();
            state.test_set_sequence(Some(sequence));
            let original = state.active_sequence().expect("origin").clone();
            let retain_time = TimelineTime::new(3, 2).expect("retention");
            let retained = mondrian_timeline::inspect_audio_automation(&original, &target)
                .expect("inspection")
                .curve()
                .expect("curve")
                .evaluate(retain_time)
                .expect("retained");
            let action = |ids| {
                audio_automation_edit_action(AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::RemoveKeyframes { keyframe_ids: ids, retain_time },
                })
            };
            let generation = state.project_author_generation();
            let history = state.authoring_history().expect("history").diagnostics();
            for invalid in [
                vec![],
                vec![ids[1], ids[1]],
                vec![ids[1], KeyframeId::new()],
            ] {
                state.dispatch_action(action(invalid)).expect_err("invalid selection");
                assert!(state
                    .active_sequence()
                    .expect("unchanged")
                    .author_state_eq_ignoring_revision(&original));
                assert_eq!(state.project_author_generation(), generation);
                assert_eq!(
                    state.authoring_history().expect("history").diagnostics(),
                    history
                );
            }
            state.dispatch_action(action(ids.clone())).expect("remove all");
            let changed = state.active_sequence().expect("changed").clone();
            let inspection =
                mondrian_timeline::inspect_audio_automation(&changed, &target).expect("inspection");
            assert!(inspection.curve().is_none());
            assert_eq!(inspection.default_value(), retained);
            assert!(state.undo_timeline().expect("one undo"));
            assert!(state
                .active_sequence()
                .expect("undone")
                .author_state_eq_ignoring_revision(&original));
            assert!(state.redo_timeline().expect("one redo"));
            assert!(state
                .active_sequence()
                .expect("redone")
                .author_state_eq_ignoring_revision(&changed));
            let mut locked = original.clone();
            locked.audio_tracks[0].is_locked = true;
            let before = locked.clone();
            assert!(mondrian_timeline::apply_audio_automation_edit(
                &mut locked,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::RemoveKeyframes { keyframe_ids: ids, retain_time },
                }
            )
            .is_err());
            assert_eq!(locked, before);
        }
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
                AudioProcessorRackEdit::SetParameterStaticValue {
                    processor_id,
                    parameter_id: ParameterId::new_static(GAIN_DB_PARAMETER_ID),
                    value: -6.0,
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
                .as_ref()
                .expect("bound host curve")
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
                .as_ref()
                .expect("bound host curve")
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
    fn typed_handle_edit_commits_once_and_preserves_curve_after_save_reopen() {
        use mondrian_core::{
            BezierHandleSide, ExactAutomationKeyframe, ExactAutomationTangentMode,
            ExactBezierHandleEdit, InterpolationType, TimelineTime,
        };
        let mut sequence = Sequence::new("Handle history");
        let owner = AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id };
        let target = AudioAutomationTarget::ChannelFader { owner };
        let keys = [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, -12.0),
            ExactAutomationKeyframe::linear(TimelineTime::ONE, -6.0),
            ExactAutomationKeyframe::linear(TimelineTime::new(2, 1).expect("last time"), 0.0),
        ];
        for keyframe in keys.iter().cloned() {
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                },
            )
            .expect("fixture key");
        }
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::SetInterpolation {
                    keyframe_id: keys[1].id,
                    interpolation: InterpolationType::AutoBezier,
                },
            },
        )
        .expect("fixture auto");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        let curve = |state: &AppState| {
            audio_channel_strip(state.active_sequence().expect("sequence"), owner)
                .expect("strip")
                .fader_automation
                .clone()
                .expect("curve")
        };
        let before = curve(&state);
        let generation = state.project_author_generation();
        let request = AudioAutomationEditRequest {
            target: target.clone(),
            edit: AudioAutomationEdit::EditHandle {
                keyframe_id: keys[1].id,
                side: BezierHandleSide::Outgoing,
                edit: ExactBezierHandleEdit { time_offset: None, value_offset: Some(1.5) },
            },
        };
        state
            .dispatch_action(audio_automation_edit_action(request.clone()))
            .expect("typed handle action");
        let after = curve(&state);
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            after.keyframes[1].tangent_mode,
            ExactAutomationTangentMode::Continuous
        );
        assert_eq!(
            after.keyframes[1].out_handle.expect("handle").time_offset,
            TimelineTime::new(1, 3).expect("untouched exact axis")
        );
        assert_eq!(
            after.keyframes[1].out_handle.expect("handle").value_offset,
            1.5
        );
        assert!(state.undo_timeline().expect("undo"));
        assert_eq!(curve(&state), before);
        assert!(
            !state.can_undo_action(),
            "one handle edit owns one history entry"
        );
        assert!(state.redo_timeline().expect("redo"));
        assert_eq!(curve(&state), after);
        let generation = state.project_author_generation();
        state
            .dispatch_action(audio_automation_edit_action(request))
            .expect("noop action");
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(curve(&state), after);
        assert!(
            state.dispatch_action(audio_automation_edit_action(AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::EditHandle {
                    keyframe_id: keys[1].id,
                    side: BezierHandleSide::Outgoing,
                    edit: ExactBezierHandleEdit {
                        time_offset: Some(TimelineTime::new(1, 2).expect("changed time")),
                        value_offset: Some(f64::INFINITY),
                    },
                },
            })).is_err(),
            "An invalid coordinate must not become an omitted axis and publish the other half of an edit"
        );
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(curve(&state), after);
        assert!(state
            .dispatch_action(audio_automation_edit_action(AudioAutomationEditRequest {
                target,
                edit: AudioAutomationEdit::EditHandle {
                    keyframe_id: mondrian_core::KeyframeId::new(),
                    side: BezierHandleSide::Incoming,
                    edit: ExactBezierHandleEdit { time_offset: None, value_offset: Some(0.5) },
                },
            }))
            .is_err());
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(curve(&state), after);
        let directory =
            std::env::temp_dir().join(format!("mondrian-handle-history-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).expect("directory");
        let project = directory.join("手柄 曲线.mdp");
        state.save_project_file_as(project.clone()).expect("durable save");
        drop(state);
        let mut reopened = AppState::new();
        reopened.open_project_file(project).expect("actual reopen");
        let restored = curve(&reopened);
        assert_eq!(restored, after);
        for numerator in 0..=40 {
            let time = TimelineTime::new(numerator, 20).expect("sample");
            assert_eq!(restored.evaluate(time), after.evaluate(time));
        }
        drop(reopened);
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn typed_automation_action_commits_once_and_round_trips_through_undo_redo() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Automation authoring");
        let owner = AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id };
        state.test_set_sequence(Some(sequence));
        let generation = state.project_author_generation();
        let keyframe =
            mondrian_core::ExactAutomationKeyframe::linear(mondrian_core::TimelineTime::ONE, -6.0);
        let keyframe_id = keyframe.id;

        state
            .dispatch_action(audio_automation_edit_action(AudioAutomationEditRequest {
                target: AudioAutomationTarget::ChannelFader { owner },
                edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
            }))
            .expect("automation through typed product action");
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            audio_channel_strip(state.active_sequence().expect("Sequence"), owner)
                .expect("strip")
                .fader_automation
                .as_ref()
                .expect("curve")
                .keyframes[0]
                .id,
            keyframe_id
        );

        assert!(state.undo_timeline().expect("undo automation"));
        assert!(
            audio_channel_strip(state.active_sequence().expect("Sequence"), owner)
                .expect("strip")
                .fader_automation
                .is_none()
        );
        assert!(state.redo_timeline().expect("redo automation"));
        assert_eq!(
            audio_channel_strip(state.active_sequence().expect("Sequence"), owner)
                .expect("strip")
                .fader_automation
                .as_ref()
                .expect("curve")
                .keyframes[0]
                .id,
            keyframe_id
        );
    }

    #[test]
    fn typed_routing_action_commits_bus_and_route_once_and_round_trips_undo_redo() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Routing authoring");
        let output_id = sequence.audio_program.outputs[0].id;
        state.test_set_sequence(Some(sequence));
        let initial_generation = state.project_author_generation();

        state
            .dispatch_action(audio_routing_edit_action(AudioRoutingEditRequest {
                edit: AudioRoutingEdit::CreateBus {
                    name: "Dialogue".to_owned(),
                    route_to: Some(AudioRouteDestination::Output(output_id)),
                },
            }))
            .expect("create Bus through typed product action");
        assert_eq!(state.project_author_generation(), initial_generation + 1);
        let sequence = state.active_sequence().expect("Sequence");
        assert_eq!(sequence.audio_program.buses.len(), 1);
        assert_eq!(sequence.audio_program.buses[0].name, "Dialogue");
        assert!(sequence.audio_program.routes.iter().any(|route| {
            matches!(route.source, mondrian_timeline::AudioRouteSource::Bus { bus_id, .. }
                if bus_id == sequence.audio_program.buses[0].id)
                && route.destination == AudioRouteDestination::Output(output_id)
        }));

        assert!(state.undo_timeline().expect("undo Bus creation"));
        assert!(state.active_sequence().expect("Sequence").audio_program.buses.is_empty());
        assert!(state.redo_timeline().expect("redo Bus creation"));
        assert_eq!(
            state.active_sequence().expect("Sequence").audio_program.buses.len(),
            1
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
    fn failed_clap_install_and_missing_definition_leave_author_state_untouched() {
        let mut state = AppState::new();
        let sequence = Sequence::new("CLAP admission");
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            rack: AudioChannelStripRack::PreFader,
        };
        state.test_set_sequence(Some(sequence));
        let generation = state.project_author_generation();
        let missing_path = std::env::temp_dir().join(format!(
            "mondrian-missing-clap-{}.dll",
            mondrian_core::ProjectId::new()
        ));
        let install = ProductAction::Audio(AudioProductAction::InstallClapLibrary(
            AudioInstallClapLibraryPayload { path: missing_path },
        ));
        assert!(state.dispatch_action(install.into_external_action()).is_err());
        let insert = ProductAction::Audio(AudioProductAction::InsertClapProcessor(
            AudioProcessorInsertClapPayload {
                address,
                plugin_id: "invalid.plugin".to_owned(),
                placement: AudioProcessorRackPlacement::End,
            },
        ));
        assert!(state.dispatch_action(insert.into_external_action()).is_err());
        assert_eq!(state.project_author_generation(), generation);
        assert!(state.installed_clap_processors().expect("catalog").is_empty());
        assert!(
            audio_processor_rack(state.active_sequence().expect("Sequence"), &address)
                .expect("Rack")
                .processors
                .is_empty()
        );
    }

    #[test]
    fn generic_rack_action_cannot_bypass_clap_rebind_probe() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("CLAP rebind admission");
        let track_id = sequence.audio_tracks[0].id;
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PreFader,
        };
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        processor.definition = AudioProcessorDefinitionRef::Clap {
            plugin_id: "org.example.gain".to_owned(),
            schema_version: 1,
            binary_sha256: None,
        };
        let processor_id = processor.id;
        let expected_definition = processor.definition.clone();
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("channel")
            .strip
            .pre_fader
            .processors
            .push(processor);
        state.test_set_sequence(Some(sequence));
        let before = state.active_sequence().expect("sequence").clone();
        let generation = state.project_author_generation();
        let action = audio_processor_rack_edit_action(request(
            address,
            AudioProcessorRackEdit::RebindNative {
                processor_id,
                expected_definition,
                new_definition: AudioProcessorDefinitionRef::Clap {
                    plugin_id: "org.example.gain".to_owned(),
                    schema_version: 1,
                    binary_sha256: Some([7; 32]),
                },
            },
        ));
        assert!(state.dispatch_action(action).is_err());
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(state.active_sequence().expect("sequence"), &before);
    }

    #[test]
    #[ignore = "requires MONDRIAN_CLAP_TEST_HELPER and MONDRIAN_CLAP_TEST_PLUGIN"]
    fn selected_clap_library_inserts_canonical_processor_and_round_trips_undo() {
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(std::path::PathBuf::from)
            .expect("built app executable");
        let library = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(std::path::PathBuf::from)
            .expect("Clack reference library");
        let catalog = std::sync::Arc::new(
            mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper).expect("catalog"),
        );
        let mut state = AppState::with_clap_catalog(catalog);
        let sequence = Sequence::new("CLAP insertion");
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            rack: AudioChannelStripRack::PreFader,
        };
        state.test_set_sequence(Some(sequence));
        let generation = state.project_author_generation();
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::InstallClapLibrary(
                    AudioInstallClapLibraryPayload { path: library },
                ))
                .into_external_action(),
            )
            .expect("install reference library");
        let plugins = state.installed_clap_processors().expect("catalog descriptors");
        assert_eq!(plugins.len(), 1);
        assert_eq!(state.project_author_generation(), generation);
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::InsertClapProcessor(
                    AudioProcessorInsertClapPayload {
                        address,
                        plugin_id: plugins[0].plugin_id.clone(),
                        placement: AudioProcessorRackPlacement::End,
                    },
                ))
                .into_external_action(),
            )
            .expect("insert reference processor");
        let rack = audio_processor_rack(state.active_sequence().expect("Sequence"), &address)
            .expect("Rack");
        assert_eq!(rack.processors.len(), 1);
        assert!(matches!(
            &rack.processors[0].definition,
            mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
                plugin_id,
                binary_sha256: Some(hash),
                ..
            } if plugin_id == &plugins[0].plugin_id && *hash != [0; 32]
        ));
        assert_eq!(state.project_author_generation(), generation + 1);
        let mut legacy = state.active_sequence().expect("Sequence").clone();
        let legacy_processor = &mut legacy
            .audio_program
            .track_channels
            .get_mut(&legacy.audio_tracks[0].id)
            .expect("Track channel")
            .strip
            .pre_fader
            .processors[0];
        let legacy_id = legacy_processor.id;
        if let AudioProcessorDefinitionRef::Clap { binary_sha256, .. } =
            &mut legacy_processor.definition
        {
            *binary_sha256 = None;
        }
        let old_parameters = legacy_processor.parameters.clone();
        let mut restored = AppState::with_clap_catalog(
            state.clap_catalog.as_ref().expect("installed catalog").clone(),
        );
        restored.test_set_sequence(Some(legacy));
        let restore_generation = restored.project_author_generation();
        restored
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::RebindClapProcessor(
                    AudioProcessorRebindClapPayload { address, processor_id: legacy_id },
                ))
                .into_external_action(),
            )
            .expect("rebind legacy instance");
        let rebound =
            &audio_processor_rack(restored.active_sequence().expect("Sequence"), &address)
                .expect("Rack")
                .processors[0];
        assert!(matches!(
            rebound.definition,
            AudioProcessorDefinitionRef::Clap { binary_sha256: Some(_), .. }
        ));
        assert_eq!(rebound.parameters, old_parameters);
        assert_eq!(restored.project_author_generation(), restore_generation + 1);
        assert!(restored.undo_timeline().expect("undo rebind"));
        assert!(matches!(
            audio_processor_rack(restored.active_sequence().expect("Sequence"), &address)
                .expect("Rack")
                .processors[0]
                .definition,
            AudioProcessorDefinitionRef::Clap { binary_sha256: None, .. }
        ));
        assert!(state.undo_timeline().expect("undo insertion"));
        assert!(
            audio_processor_rack(state.active_sequence().expect("Sequence"), &address)
                .expect("Rack")
                .processors
                .is_empty()
        );
    }

    #[test]
    #[ignore = "requires MONDRIAN_VST3_TEST_HELPER and MONDRIAN_VST3_TEST_PLUGIN"]
    fn selected_vst3_plugin_inserts_canonical_processor_and_round_trips_undo() {
        let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .map(std::path::PathBuf::from)
            .expect("built app executable");
        let selection = std::env::var_os("MONDRIAN_VST3_TEST_PLUGIN")
            .map(std::path::PathBuf::from)
            .expect("VST3 reference file or bundle");
        let clap = std::sync::Arc::new(
            mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone())
                .expect("CLAP catalog"),
        );
        let vst3 = std::sync::Arc::new(
            mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper)
                .expect("VST3 catalog"),
        );
        let mut state = AppState::with_native_audio_catalogs(clap, vst3);
        let sequence = Sequence::new("VST3 insertion");
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            rack: AudioChannelStripRack::PreFader,
        };
        state.test_set_sequence(Some(sequence));
        let generation = state.project_author_generation();
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::InstallVst3Plugin(
                    AudioInstallVst3PluginPayload { path: selection },
                ))
                .into_external_action(),
            )
            .expect("install reference VST3 plugin");
        let classes = state.installed_vst3_processors().expect("VST3 catalog descriptors");
        assert_eq!(classes.len(), 1);
        assert_eq!(state.project_author_generation(), generation);
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::InsertVst3Processor(
                    AudioProcessorInsertVst3Payload {
                        address,
                        class_id: classes[0].class_id.clone(),
                        placement: AudioProcessorRackPlacement::End,
                    },
                ))
                .into_external_action(),
            )
            .expect("insert reference VST3 processor");
        let rack = audio_processor_rack(state.active_sequence().expect("Sequence"), &address)
            .expect("Rack");
        assert_eq!(rack.processors.len(), 1);
        assert!(matches!(
            &rack.processors[0].definition,
            AudioProcessorDefinitionRef::Vst3 { class_id, binary_sha256: Some(hash), .. }
                if class_id == &classes[0].class_id && *hash != [0; 32]
        ));
        assert_eq!(state.project_author_generation(), generation + 1);
        let processor_id = rack.processors[0].id;
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::RebindVst3Processor(
                    AudioProcessorRebindVst3Payload { address, processor_id },
                ))
                .into_external_action(),
            )
            .expect("rebind same VST3 revision");
        assert_eq!(state.project_author_generation(), generation + 1);
        assert!(state.undo_timeline().expect("undo insertion"));
        assert!(
            audio_processor_rack(state.active_sequence().expect("Sequence"), &address)
                .expect("Rack")
                .processors
                .is_empty()
        );
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
