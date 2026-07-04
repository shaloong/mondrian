use std::env;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context};
use mondrian_app::app_ui::viewer_gpu_output_budget::{evaluate_jsonl, ViewerGpuOutputBudget};

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
                "usage: viewer_gpu_output_budget <jsonl-path> [--min-records N] [--min-ready N] [--max-failed N] [--max-blocked N] [--max-rejected N] [--max-degraded N] [--max-waiting N] [--max-display-issues N] [--max-display-payload-blockers N]"
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
                "--min-records" => budget.min_records = parsed,
                "--min-ready" => budget.min_ready = parsed,
                "--max-failed" => budget.max_failed = parsed,
                "--max-blocked" => budget.max_blocked = parsed,
                "--max-rejected" => budget.max_rejected = parsed,
                "--max-degraded" => budget.max_degraded = parsed,
                "--max-waiting" => budget.max_waiting = parsed,
                "--max-display-issues" => budget.max_display_issues = parsed,
                "--max-display-payload-blockers" => {
                    budget.max_display_payload_blockers = parsed;
                }
                _ => bail!("unknown argument: {flag}"),
            }
            index += 2;
        }
        Ok(Self { path: Path::new(first).to_path_buf(), budget })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_args_parse_threshold_overrides() {
        let args = vec![
            "target/perf/viewer.jsonl".to_owned(),
            "--min-ready".to_owned(),
            "4".to_owned(),
            "--min-records".to_owned(),
            "3".to_owned(),
            "--max-degraded".to_owned(),
            "2".to_owned(),
            "--max-waiting".to_owned(),
            "8".to_owned(),
            "--max-display-issues".to_owned(),
            "1".to_owned(),
            "--max-display-payload-blockers".to_owned(),
            "0".to_owned(),
        ];

        let parsed = BudgetArgs::parse(&args).expect("parse budget args");

        assert_eq!(parsed.path, Path::new("target/perf/viewer.jsonl"));
        assert_eq!(
            parsed.budget,
            ViewerGpuOutputBudget {
                min_records: 3,
                min_ready: 4,
                max_degraded: 2,
                max_waiting: 8,
                max_display_issues: 1,
                max_display_payload_blockers: 0,
                ..ViewerGpuOutputBudget::default()
            }
        );
    }
}
