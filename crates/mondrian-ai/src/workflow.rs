//! 工作流 DSL 数据结构

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 工作流定义（从 YAML 文件加载）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDef {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub inputs: HashMap<String, InputDef>,
    pub steps: Vec<StepDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputDef {
    #[serde(rename = "type")]
    pub input_type: String, // "string" / "number" / "enum"
    pub label: Option<String>,
    pub default: Option<serde_json::Value>,
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepDef {
    pub id: String,
    pub name: String,
    pub action: String, // "generate_image" / "generate_video" / ...
    pub provider: Option<String>,
    pub condition: Option<String>, // Tera 模板表达式
    pub params: HashMap<String, serde_json::Value>,
    pub output: Option<String>, // 输出变量名
    pub on_error: Option<ErrorPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorPolicy {
    Fail,
    Skip,
    Retry { max_attempts: u32, delay_secs: f32 },
}

impl WorkflowDef {
    /// 从 YAML 字符串解析工作流定义
    pub fn from_yaml(yaml: &str) -> mondrian_core::Result<Self> {
        serde_yaml::from_str(yaml).map_err(|e| mondrian_core::MondrianError::WorkflowParseFailed {
            reason: e.to_string(),
        })
    }
}

/// 工作流输入值
pub type WorkflowInputs = HashMap<String, serde_json::Value>;

/// 工作流执行上下文（步骤间传递数据）
#[derive(Debug, Default)]
pub struct WorkflowContext {
    pub inputs: WorkflowInputs,
    outputs: HashMap<String, serde_json::Value>,
}

impl WorkflowContext {
    pub fn new(inputs: WorkflowInputs) -> Self {
        Self { inputs, outputs: HashMap::new() }
    }

    pub fn set_output(&mut self, step_id: &str, value: serde_json::Value) {
        self.outputs.insert(step_id.to_string(), value);
    }

    pub fn get_output(&self, step_id: &str) -> Option<&serde_json::Value> {
        self.outputs.get(step_id)
    }
}

/// 工作流执行结果
pub struct WorkflowResult {
    pub context: WorkflowContext,
    pub step_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_yaml() -> &'static str {
        r#"
name: Test Workflow
version: "1.0"
description: A minimal test workflow
inputs:
  prompt:
    type: string
    label: "Main Prompt"
    default: "default text"
steps:
  - id: step1
    name: Generate Image
    action: generate_image
    provider: openai
    params:
      prompt: "{{ inputs.prompt }}"
      width: 1024
      height: 1024
    output: image_result
"#
    }

    #[test]
    fn parse_valid_workflow() {
        let wf = WorkflowDef::from_yaml(minimal_yaml()).expect("parse valid yaml");
        assert_eq!(wf.name, "Test Workflow");
        assert_eq!(wf.version, "1.0");
        assert_eq!(wf.description.as_deref(), Some("A minimal test workflow"));
        assert_eq!(wf.steps.len(), 1);
        assert_eq!(wf.steps[0].id, "step1");
        assert_eq!(wf.steps[0].action, "generate_image");
        assert_eq!(wf.steps[0].provider.as_deref(), Some("openai"));
        assert_eq!(wf.steps[0].output.as_deref(), Some("image_result"));
    }

    #[test]
    fn parse_workflow_with_inputs() {
        let wf = WorkflowDef::from_yaml(minimal_yaml()).expect("parse");
        let prompt_input = wf.inputs.get("prompt").expect("prompt input");
        assert_eq!(prompt_input.input_type, "string");
        assert_eq!(prompt_input.label.as_deref(), Some("Main Prompt"));
        assert_eq!(
            prompt_input.default.as_ref().and_then(|v| v.as_str()),
            Some("default text")
        );
    }

    #[test]
    fn parse_invalid_yaml_fails() {
        let result = WorkflowDef::from_yaml("!!! not valid yaml : }");
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_yaml_fails() {
        let result = WorkflowDef::from_yaml("");
        assert!(result.is_err());
    }

    #[test]
    fn error_policy_yaml_roundtrip() {
        // serde_yaml 0.9 uses !Tag representation for enums by default
        let yaml = r#"
name: Test
version: "1"
inputs: {}
steps:
  - id: s1
    name: Step
    action: test
    params: {}
    on_error: !Retry
      max_attempts: 3
      delay_secs: 1.5
"#;
        let wf = WorkflowDef::from_yaml(yaml).expect("parse");
        let parsed = wf.steps[0].on_error.as_ref().expect("on_error");
        assert!(matches!(
            parsed,
            ErrorPolicy::Retry { max_attempts: 3, delay_secs }
            if (delay_secs - 1.5).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn workflow_context_set_and_get() {
        let inputs = {
            let mut m = WorkflowInputs::new();
            m.insert("key".into(), serde_json::json!("value"));
            m
        };
        let mut ctx = WorkflowContext::new(inputs);
        assert_eq!(ctx.inputs.get("key").and_then(|v| v.as_str()), Some("value"));
        assert!(ctx.get_output("step1").is_none());

        ctx.set_output("step1", serde_json::json!({"url": "https://example.com/img.png"}));
        let out = ctx.get_output("step1").expect("exists");
        assert_eq!(out["url"].as_str(), Some("https://example.com/img.png"));
    }

    #[test]
    fn workflow_with_condition() {
        let yaml = r#"
name: Conditional
version: "1"
inputs:
  mode:
    type: enum
    options: ["fast", "quality"]
steps:
  - id: upscale
    name: Upscale
    action: upscale
    condition: '{{ inputs.mode == "quality" }}'
    params:
      factor: 2
"#;
        let wf = WorkflowDef::from_yaml(yaml).expect("parse");
        assert_eq!(wf.steps[0].condition.as_deref(), Some("{{ inputs.mode == \"quality\" }}"));
    }

    #[test]
    fn workflow_with_enum_input() {
        let yaml = r#"
name: Enum Test
version: "1"
inputs:
  style:
    type: enum
    options: ["realistic", "anime", "3d"]
steps: []
"#;
        let wf = WorkflowDef::from_yaml(yaml).expect("parse");
        let style = wf.inputs.get("style").expect("style input");
        assert_eq!(style.input_type, "enum");
        assert_eq!(
            style.options.as_ref().map(|o| o.as_slice()),
            Some(&["realistic".to_string(), "anime".to_string(), "3d".to_string()][..])
        );
    }

    #[test]
    fn workflow_with_multiple_steps() {
        let yaml = r#"
name: Multi Step
version: "1"
inputs:
  topic:
    type: string
steps:
  - id: write_script
    name: Write Script
    action: llm_chat
    params:
      prompt: "Write about {{ inputs.topic }}"
    output: script
  - id: generate_video
    name: Generate Video
    action: generate_video
    params:
      prompt: "{{ outputs.write_script }}"
"#;
        let wf = WorkflowDef::from_yaml(yaml).expect("parse");
        assert_eq!(wf.steps.len(), 2);
        assert_eq!(wf.steps[0].id, "write_script");
        assert_eq!(wf.steps[1].id, "generate_video");
        assert_eq!(
            wf.steps[1].params.get("prompt").and_then(|v| v.as_str()),
            Some("{{ outputs.write_script }}")
        );
    }
}
