//! Agent 编排引擎

use crate::workflow::{WorkflowContext, WorkflowDef, WorkflowInputs, WorkflowResult};
use mondrian_core::{
    events::{AppEvent, EventBus},
    Result,
};
use std::sync::Arc;
use tracing::{info, warn};

pub struct AgentOrchestrator {
    pub event_bus: Arc<EventBus>,
}

impl AgentOrchestrator {
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self { event_bus }
    }

    /// 执行工作流（异步，通过 EventBus 广播进度）
    pub async fn run_workflow(
        &self,
        workflow: WorkflowDef,
        inputs: WorkflowInputs,
    ) -> Result<WorkflowResult> {
        info!("Starting workflow: {}", workflow.name);
        self.event_bus
            .publish(AppEvent::WorkflowStarted { workflow_name: workflow.name.clone() });

        let ctx = WorkflowContext::new(inputs);
        let step_count = workflow.steps.len();

        for step in &workflow.steps {
            info!("Executing step: {} ({})", step.id, step.action);
            self.event_bus.publish(AppEvent::WorkflowStepStarted {
                step_id: step.id.clone(),
                step_name: step.name.clone(),
            });

            // TODO: 根据 step.action 分发到对应 Provider
            match step.action.as_str() {
                "generate_image" => self.action_generate_image(&ctx, step).await?,
                "generate_video" => self.action_generate_video(&ctx, step).await?,
                "transcribe" => self.action_transcribe(&ctx, step).await?,
                "generate_music" => self.action_generate_music(&ctx, step).await?,
                "llm_chat" => self.action_llm_chat(&ctx, step).await?,
                "place_on_timeline" => { /* 通过 EventBus 通知 App 层 */ }
                other => warn!("Unknown workflow action: {other}"),
            }

            self.event_bus
                .publish(AppEvent::WorkflowStepCompleted { step_id: step.id.clone() });
        }

        self.event_bus
            .publish(AppEvent::WorkflowCompleted { workflow_name: workflow.name.clone() });

        Ok(WorkflowResult { context: ctx, step_count })
    }

    async fn action_generate_image(
        &self,
        _ctx: &WorkflowContext,
        _step: &crate::workflow::StepDef,
    ) -> Result<()> {
        // TODO: 调用 ImageGenerationProvider
        tracing::info!("[stub] generate_image");
        Ok(())
    }

    async fn action_generate_video(
        &self,
        _ctx: &WorkflowContext,
        _step: &crate::workflow::StepDef,
    ) -> Result<()> {
        tracing::info!("[stub] generate_video");
        Ok(())
    }

    async fn action_transcribe(
        &self,
        _ctx: &WorkflowContext,
        _step: &crate::workflow::StepDef,
    ) -> Result<()> {
        tracing::info!("[stub] transcribe");
        Ok(())
    }

    async fn action_generate_music(
        &self,
        _ctx: &WorkflowContext,
        _step: &crate::workflow::StepDef,
    ) -> Result<()> {
        tracing::info!("[stub] generate_music");
        Ok(())
    }

    async fn action_llm_chat(
        &self,
        _ctx: &WorkflowContext,
        _step: &crate::workflow::StepDef,
    ) -> Result<()> {
        tracing::info!("[stub] llm_chat");
        Ok(())
    }
}
