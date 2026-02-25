//! 工作流 DSL 数据结构

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 工作流定义（从 YAML 文件加载）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDef {
    pub name:        String,
    pub version:     String,
    pub description: Option<String>,
    pub inputs:      HashMap<String, InputDef>,
    pub steps:       Vec<StepDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputDef {
    #[serde(rename = "type")]
    pub input_type: String,     // "string" / "number" / "enum"
    pub label:      Option<String>,
    pub default:    Option<serde_json::Value>,
    pub options:    Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepDef {
    pub id:        String,
    pub name:      String,
    pub action:    String,      // "generate_image" / "generate_video" / ...
    pub provider:  Option<String>,
    pub condition: Option<String>,  // Tera 模板表达式
    pub params:    HashMap<String, serde_json::Value>,
    pub output:    Option<String>,  // 输出变量名
    pub on_error:  Option<ErrorPolicy>,
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
        serde_yaml::from_str(yaml).map_err(|e| {
            mondrian_core::MondrianError::WorkflowParseFailed { reason: e.to_string() }
        })
    }
}

/// 工作流输入值
pub type WorkflowInputs = HashMap<String, serde_json::Value>;

/// 工作流执行上下文（步骤间传递数据）
#[derive(Debug, Default)]
pub struct WorkflowContext {
    pub inputs:  WorkflowInputs,
    outputs:     HashMap<String, serde_json::Value>,
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
    pub context:   WorkflowContext,
    pub step_count: usize,
}
