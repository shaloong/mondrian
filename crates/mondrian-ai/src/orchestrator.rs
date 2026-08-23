//! Fail-closed AI workflow orchestration.

use crate::workflow::{StepDef, WorkflowContext, WorkflowDef, WorkflowInputs, WorkflowResult};
use mondrian_core::{
    events::{AppEvent, EventBus},
    MondrianError, Result,
};
use std::sync::Arc;
use tracing::info;

/// Coordinates workflow attempts and publishes observation-only lifecycle events.
///
/// Provider and editor-action execution are deliberately unavailable until a
/// concrete typed Adapter is installed. The [`EventBus`] reports facts after
/// they occur; it is never used to request an editor mutation.
pub struct AgentOrchestrator {
    event_bus: Arc<EventBus>,
}

impl AgentOrchestrator {
    /// Create an orchestrator that publishes workflow lifecycle notifications.
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self { event_bus }
    }

    /// Attempt to execute a workflow.
    ///
    /// Every unavailable, unknown, or unbound step fails with a structured
    /// error. A failed step publishes `WorkflowStepFailed` and can never publish
    /// `WorkflowStepCompleted` or `WorkflowCompleted`.
    pub async fn run_workflow(
        &self,
        workflow: WorkflowDef,
        inputs: WorkflowInputs,
    ) -> Result<WorkflowResult> {
        if workflow.steps.is_empty() {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "<workflow>".to_owned(),
                reason: "workflow contains no executable steps".to_owned(),
            });
        }

        info!(workflow = %workflow.name, "starting AI workflow attempt");
        self.event_bus
            .publish(AppEvent::WorkflowStarted { workflow_name: workflow.name.clone() });

        let context = WorkflowContext::new(inputs);
        let step_count = workflow.steps.len();

        for step in &workflow.steps {
            info!(step_id = %step.id, action = %step.action, "attempting AI workflow step");
            self.event_bus.publish(AppEvent::WorkflowStepStarted {
                step_id: step.id.clone(),
                step_name: step.name.clone(),
            });

            if let Err(error) = self.execute_step(step) {
                self.event_bus.publish(AppEvent::WorkflowStepFailed {
                    step_id: step.id.clone(),
                    error: error.to_string(),
                });
                return Err(error);
            }

            self.event_bus
                .publish(AppEvent::WorkflowStepCompleted { step_id: step.id.clone() });
        }

        self.event_bus
            .publish(AppEvent::WorkflowCompleted { workflow_name: workflow.name });

        Ok(WorkflowResult { context, step_count })
    }

    fn execute_step(&self, step: &StepDef) -> Result<()> {
        let reason = match step.action.as_str() {
            "generate_image" | "generate_video" | "transcribe" | "generate_music"
            | "llm_chat" => format!(
                "workflow action '{}' has no bound typed Provider Adapter",
                step.action
            ),
            "place_on_timeline" => format!(
                "workflow action '{}' has no UI-independent typed editor Action Adapter; EventBus is notification-only",
                step.action
            ),
            other => format!("unknown workflow action '{other}'"),
        };

        Err(MondrianError::WorkflowStepFailed { step_id: step.id.clone(), reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn workflow(action: &str) -> WorkflowDef {
        WorkflowDef {
            name: "fail-closed workflow".to_owned(),
            version: "1".to_owned(),
            description: None,
            inputs: HashMap::new(),
            steps: vec![StepDef {
                id: "step-1".to_owned(),
                name: "Attempt one step".to_owned(),
                action: action.to_owned(),
                provider: None,
                condition: None,
                params: HashMap::new(),
                output: None,
                on_error: None,
            }],
        }
    }

    async fn assert_step_fails_without_completion(action: &str, expected_reason: &str) {
        let event_bus = EventBus::new();
        let events = event_bus.subscribe();
        let orchestrator = AgentOrchestrator::new(event_bus);

        let error = match orchestrator.run_workflow(workflow(action), WorkflowInputs::new()).await {
            Ok(_) => panic!("an unavailable workflow step must fail closed"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { ref step_id, ref reason }
                if step_id == "step-1" && reason.contains(expected_reason)
        ));

        let events = events.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(event, AppEvent::WorkflowStarted { .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::WorkflowStepStarted { step_id, .. } if step_id == "step-1"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::WorkflowStepFailed { step_id, error }
                if step_id == "step-1" && error.contains(expected_reason)
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            AppEvent::WorkflowStepCompleted { .. } | AppEvent::WorkflowCompleted { .. }
        )));
    }

    #[tokio::test]
    async fn unbound_provider_action_fails_without_completion_evidence() {
        assert_step_fails_without_completion("generate_image", "no bound typed Provider Adapter")
            .await;
    }

    #[tokio::test]
    async fn timeline_action_requires_a_typed_editor_adapter() {
        assert_step_fails_without_completion(
            "place_on_timeline",
            "no UI-independent typed editor Action Adapter",
        )
        .await;
    }

    #[tokio::test]
    async fn unknown_action_fails_without_completion_evidence() {
        assert_step_fails_without_completion("future_magic", "unknown workflow action").await;
    }

    #[tokio::test]
    async fn empty_workflow_is_not_reported_as_success() {
        let event_bus = EventBus::new();
        let events = event_bus.subscribe();
        let orchestrator = AgentOrchestrator::new(event_bus);
        let mut empty = workflow("generate_image");
        empty.steps.clear();

        let error = match orchestrator.run_workflow(empty, WorkflowInputs::new()).await {
            Ok(_) => panic!("an empty workflow must not report success"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { ref step_id, ref reason }
                if step_id == "<workflow>" && reason.contains("no executable steps")
        ));
        assert!(events.try_iter().next().is_none());
    }
}
