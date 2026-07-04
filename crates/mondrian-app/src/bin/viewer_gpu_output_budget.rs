use std::env;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let args = BudgetArgs::parse(&args)?;
    let contents = fs::read_to_string(&args.path).with_context(|| {
        format!(
            "failed to read viewer GPU output JSONL: {}",
            args.path.display()
        )
    })?;
    let summary = evaluate_jsonl(&contents, &args.budget)?;
    let summary_json = serde_json::to_string(&summary)?;
    println!("{summary_json}");
    if !summary.passed {
        bail!("viewer GPU output budget failed: {summary_json}");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BudgetArgs {
    path: std::path::PathBuf,
    budget: ViewerGpuOutputBudget,
}

impl BudgetArgs {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let Some(first) = args.first() else {
            bail!(
                "usage: viewer_gpu_output_budget <jsonl-path> [--min-ready N] [--max-failed N] [--max-blocked N] [--max-rejected N] [--max-degraded N] [--max-waiting N]"
            );
        };
        let mut budget = ViewerGpuOutputBudget::default();
        let mut index = 1usize;
        while index < args.len() {
            let flag = args[index].as_str();
            let value = args.get(index + 1).with_context(|| format!("missing value for {flag}"))?;
            let parsed = value
                .parse::<u64>()
                .with_context(|| format!("invalid numeric value for {flag}: {value}"))?;
            match flag {
                "--min-ready" => budget.min_ready = parsed,
                "--max-failed" => budget.max_failed = parsed,
                "--max-blocked" => budget.max_blocked = parsed,
                "--max-rejected" => budget.max_rejected = parsed,
                "--max-degraded" => budget.max_degraded = parsed,
                "--max-waiting" => budget.max_waiting = parsed,
                _ => bail!("unknown argument: {flag}"),
            }
            index += 2;
        }
        Ok(Self { path: Path::new(first).to_path_buf(), budget })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewerGpuOutputBudget {
    min_ready: u64,
    max_failed: u64,
    max_blocked: u64,
    max_rejected: u64,
    max_degraded: u64,
    max_waiting: u64,
}

impl Default for ViewerGpuOutputBudget {
    fn default() -> Self {
        Self {
            min_ready: 1,
            max_failed: 0,
            max_blocked: 0,
            max_rejected: 0,
            max_degraded: 0,
            max_waiting: u64::MAX,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ViewerGpuOutputBudgetSummary {
    records: u64,
    counts: ViewerGpuOutputHealthCounts,
    budget: ViewerGpuOutputBudgetReport,
    passed: bool,
    failures: Vec<ViewerGpuOutputBudgetFailure>,
    last_status: Option<ViewerGpuOutputHealthStatus>,
    last_frame_context: Option<ViewerGpuOutputFrameContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct ViewerGpuOutputBudgetReport {
    min_ready: u64,
    max_failed: u64,
    max_blocked: u64,
    max_rejected: u64,
    max_degraded: u64,
    max_waiting: u64,
}

impl From<ViewerGpuOutputBudget> for ViewerGpuOutputBudgetReport {
    fn from(budget: ViewerGpuOutputBudget) -> Self {
        Self {
            min_ready: budget.min_ready,
            max_failed: budget.max_failed,
            max_blocked: budget.max_blocked,
            max_rejected: budget.max_rejected,
            max_degraded: budget.max_degraded,
            max_waiting: budget.max_waiting,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ViewerGpuOutputBudgetFailure {
    metric: &'static str,
    actual: u64,
    limit: u64,
}

fn evaluate_jsonl(
    contents: &str,
    budget: &ViewerGpuOutputBudget,
) -> anyhow::Result<ViewerGpuOutputBudgetSummary> {
    let mut counts = ViewerGpuOutputHealthCounts::default();
    let mut records = 0u64;
    let mut last_status = None;
    let mut last_frame_context = None;

    for (line_index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: ViewerGpuOutputDiagnosticRecord = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSONL record at line {}", line_index + 1))?;
        records = records.saturating_add(1);
        counts.record(record.health.status);
        last_status = Some(record.health.status);
        last_frame_context = record.last_frame_context;
    }

    let mut failures = Vec::new();
    if counts.ready < budget.min_ready {
        failures.push(ViewerGpuOutputBudgetFailure {
            metric: "ready",
            actual: counts.ready,
            limit: budget.min_ready,
        });
    }
    push_max_failure(&mut failures, "failed", counts.failed, budget.max_failed);
    push_max_failure(&mut failures, "blocked", counts.blocked, budget.max_blocked);
    push_max_failure(
        &mut failures,
        "rejected",
        counts.rejected,
        budget.max_rejected,
    );
    push_max_failure(
        &mut failures,
        "degraded",
        counts.degraded,
        budget.max_degraded,
    );
    push_max_failure(&mut failures, "waiting", counts.waiting, budget.max_waiting);

    Ok(ViewerGpuOutputBudgetSummary {
        records,
        counts,
        budget: (*budget).into(),
        passed: failures.is_empty(),
        failures,
        last_status,
        last_frame_context,
    })
}

fn push_max_failure(
    failures: &mut Vec<ViewerGpuOutputBudgetFailure>,
    metric: &'static str,
    actual: u64,
    limit: u64,
) {
    if actual > limit {
        failures.push(ViewerGpuOutputBudgetFailure { metric, actual, limit });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ViewerGpuOutputDiagnosticRecord {
    health: ViewerGpuOutputHealthSummary,
    last_frame_context: Option<ViewerGpuOutputFrameContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
struct ViewerGpuOutputHealthSummary {
    status: ViewerGpuOutputHealthStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct ViewerGpuOutputHealthCounts {
    no_invocation: u64,
    waiting: u64,
    blocked: u64,
    failed: u64,
    rejected: u64,
    degraded: u64,
    ready: u64,
}

impl ViewerGpuOutputHealthCounts {
    fn record(&mut self, status: ViewerGpuOutputHealthStatus) {
        match status {
            ViewerGpuOutputHealthStatus::NoInvocation => {
                self.no_invocation = self.no_invocation.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Waiting => {
                self.waiting = self.waiting.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Blocked => {
                self.blocked = self.blocked.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Failed => {
                self.failed = self.failed.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Rejected => {
                self.rejected = self.rejected.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Degraded => {
                self.degraded = self.degraded.saturating_add(1);
            }
            ViewerGpuOutputHealthStatus::Ready => {
                self.ready = self.ready.saturating_add(1);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
enum ViewerGpuOutputHealthStatus {
    NoInvocation,
    Waiting,
    Blocked,
    Failed,
    Rejected,
    Degraded,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct ViewerGpuOutputFrameContext {
    sequence_id: String,
    frame: i64,
    width: u32,
    height: u32,
    external_texture_key: String,
    output_target: String,
    output_color_space: String,
    tone_map: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_passes_clean_ready_stream() {
        let jsonl = r#"
{"health":{"status":"Waiting"}}
{"health":{"status":"Ready"},"last_frame_context":{"sequence_id":"seq","frame":7,"width":1920,"height":1080,"external_texture_key":"key","output_target":"Display","output_color_space":"Srgb","tone_map":false}}
"#;

        let summary =
            evaluate_jsonl(jsonl, &ViewerGpuOutputBudget::default()).expect("budget summary");

        assert!(summary.passed);
        assert_eq!(summary.records, 2);
        assert_eq!(summary.counts.waiting, 1);
        assert_eq!(summary.counts.ready, 1);
        assert_eq!(
            summary.last_status,
            Some(ViewerGpuOutputHealthStatus::Ready)
        );
        assert_eq!(summary.last_frame_context.expect("frame context").frame, 7);
    }

    #[test]
    fn budget_fails_failed_blocked_and_missing_ready_stream() {
        let jsonl = r#"
{"health":{"status":"Failed"}}
{"health":{"status":"Blocked"}}
"#;

        let summary =
            evaluate_jsonl(jsonl, &ViewerGpuOutputBudget::default()).expect("budget summary");

        assert!(!summary.passed);
        assert_eq!(summary.counts.failed, 1);
        assert_eq!(summary.counts.blocked, 1);
        assert_eq!(
            summary.failures,
            vec![
                ViewerGpuOutputBudgetFailure { metric: "ready", actual: 0, limit: 1 },
                ViewerGpuOutputBudgetFailure { metric: "failed", actual: 1, limit: 0 },
                ViewerGpuOutputBudgetFailure { metric: "blocked", actual: 1, limit: 0 },
            ]
        );
    }

    #[test]
    fn budget_args_parse_threshold_overrides() {
        let args = vec![
            "target/perf/viewer.jsonl".to_owned(),
            "--min-ready".to_owned(),
            "4".to_owned(),
            "--max-degraded".to_owned(),
            "2".to_owned(),
            "--max-waiting".to_owned(),
            "8".to_owned(),
        ];

        let parsed = BudgetArgs::parse(&args).expect("parse budget args");

        assert_eq!(parsed.path, Path::new("target/perf/viewer.jsonl"));
        assert_eq!(
            parsed.budget,
            ViewerGpuOutputBudget {
                min_ready: 4,
                max_degraded: 2,
                max_waiting: 8,
                ..ViewerGpuOutputBudget::default()
            }
        );
    }
}
