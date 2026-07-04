use std::env;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context};
use mondrian_app::app_ui::viewer_gpu_output_budget::{
    build_health_report, evaluate_jsonl, ViewerGpuOutputBudget,
};

const DISPLAY_BASELINE_PRESET: &str = "display-baseline";
const CUSTOM_PROFILE: &str = "custom";

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
    let summary_passed = summary.passed;
    let output_json = serde_json::to_string(&build_health_report(
        summary,
        args.profile,
        Some(args.path.display().to_string()),
    ))?;
    println!("{output_json}");
    if !summary_passed {
        bail!("viewer GPU output budget failed: {output_json}");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BudgetArgs {
    path: std::path::PathBuf,
    budget: ViewerGpuOutputBudget,
    profile: String,
}

impl BudgetArgs {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let Some(first) = args.first() else {
            bail!(
                "usage: viewer_gpu_output_budget <jsonl-path> [--profile NAME] [--preset display-baseline] [--min-records N] [--min-ready N] [--max-failed N] [--max-blocked N] [--max-rejected N] [--max-degraded N] [--max-waiting N] [--max-display-issues N] [--max-hdr-output-requires-hdr-surface N] [--max-output-color-space-requires-surface-color-space N] [--max-reconfigure-blocked-by-payload N] [--max-unsupported-presentation-intent N] [--max-unsupported-surface-contract N] [--max-unknown-display-issues N] [--max-display-contract-refreshes N] [--max-display-issue-refresh-correlations N] [--max-display-tone-map-headroom-changes N] [--max-available-surface-format-changes N] [--max-format-color-space-changes N] [--max-present-mode-changes N] [--max-alpha-mode-changes N] [--max-display-payload-blockers N] [--max-color-rejections N]"
            );
        };
        let mut budget = ViewerGpuOutputBudget::default();
        let mut profile = CUSTOM_PROFILE.to_owned();
        let mut index = 1usize;
        while index < args.len() {
            let flag = args[index].as_str();
            if flag == "--profile" {
                profile = args
                    .get(index + 1)
                    .with_context(|| format!("missing value for {flag}"))?
                    .to_owned();
                index += 2;
                continue;
            }
            if flag == "--preset" {
                let preset =
                    args.get(index + 1).with_context(|| format!("missing value for {flag}"))?;
                budget = parse_budget_preset(preset)?;
                profile = preset.to_owned();
                index += 2;
                continue;
            }
            if !is_budget_threshold_flag(flag) {
                bail!("unknown argument: {flag}");
            }
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
                "--max-hdr-output-requires-hdr-surface" => {
                    budget.max_hdr_output_requires_hdr_surface = parsed;
                }
                "--max-output-color-space-requires-surface-color-space" => {
                    budget.max_output_color_space_requires_surface_color_space = parsed;
                }
                "--max-reconfigure-blocked-by-payload" => {
                    budget.max_reconfigure_blocked_by_payload = parsed;
                }
                "--max-unsupported-presentation-intent" => {
                    budget.max_unsupported_presentation_intent = parsed;
                }
                "--max-unsupported-surface-contract" => {
                    budget.max_unsupported_surface_contract = parsed;
                }
                "--max-unknown-display-issues" => budget.max_unknown_display_issues = parsed,
                "--max-display-contract-refreshes" => {
                    budget.max_display_contract_refreshes = parsed;
                }
                "--max-display-issue-refresh-correlations" => {
                    budget.max_display_issue_refresh_correlations = parsed;
                }
                "--max-display-tone-map-headroom-changes" => {
                    budget.max_display_tone_map_headroom_changes = parsed;
                }
                "--max-available-surface-format-changes" => {
                    budget.max_available_surface_format_changes = parsed;
                }
                "--max-format-color-space-changes" => {
                    budget.max_format_color_space_changes = parsed;
                }
                "--max-present-mode-changes" => budget.max_present_mode_changes = parsed,
                "--max-alpha-mode-changes" => budget.max_alpha_mode_changes = parsed,
                "--max-display-payload-blockers" => {
                    budget.max_display_payload_blockers = parsed;
                }
                "--max-color-rejections" => budget.max_color_rejections = parsed,
                _ => unreachable!("budget threshold flag prevalidated"),
            }
            index += 2;
        }
        Ok(Self {
            path: Path::new(first).to_path_buf(),
            budget,
            profile,
        })
    }
}

fn is_budget_threshold_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--min-records"
            | "--min-ready"
            | "--max-failed"
            | "--max-blocked"
            | "--max-rejected"
            | "--max-degraded"
            | "--max-waiting"
            | "--max-display-issues"
            | "--max-hdr-output-requires-hdr-surface"
            | "--max-output-color-space-requires-surface-color-space"
            | "--max-reconfigure-blocked-by-payload"
            | "--max-unsupported-presentation-intent"
            | "--max-unsupported-surface-contract"
            | "--max-unknown-display-issues"
            | "--max-display-contract-refreshes"
            | "--max-display-issue-refresh-correlations"
            | "--max-display-tone-map-headroom-changes"
            | "--max-available-surface-format-changes"
            | "--max-format-color-space-changes"
            | "--max-present-mode-changes"
            | "--max-alpha-mode-changes"
            | "--max-display-payload-blockers"
            | "--max-color-rejections"
    )
}

fn parse_budget_preset(name: &str) -> anyhow::Result<ViewerGpuOutputBudget> {
    match name {
        DISPLAY_BASELINE_PRESET => Ok(ViewerGpuOutputBudget::display_baseline()),
        _ => bail!("unknown budget preset: {name}"),
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
            "--max-hdr-output-requires-hdr-surface".to_owned(),
            "0".to_owned(),
            "--max-output-color-space-requires-surface-color-space".to_owned(),
            "3".to_owned(),
            "--max-reconfigure-blocked-by-payload".to_owned(),
            "1".to_owned(),
            "--max-unsupported-presentation-intent".to_owned(),
            "2".to_owned(),
            "--max-unsupported-surface-contract".to_owned(),
            "4".to_owned(),
            "--max-unknown-display-issues".to_owned(),
            "0".to_owned(),
            "--max-display-contract-refreshes".to_owned(),
            "9".to_owned(),
            "--max-display-issue-refresh-correlations".to_owned(),
            "1".to_owned(),
            "--max-display-tone-map-headroom-changes".to_owned(),
            "2".to_owned(),
            "--max-available-surface-format-changes".to_owned(),
            "3".to_owned(),
            "--max-format-color-space-changes".to_owned(),
            "4".to_owned(),
            "--max-present-mode-changes".to_owned(),
            "5".to_owned(),
            "--max-alpha-mode-changes".to_owned(),
            "6".to_owned(),
            "--max-display-payload-blockers".to_owned(),
            "0".to_owned(),
            "--max-color-rejections".to_owned(),
            "2".to_owned(),
        ];

        let parsed = BudgetArgs::parse(&args).expect("parse budget args");

        assert_eq!(parsed.path, Path::new("target/perf/viewer.jsonl"));
        assert_eq!(parsed.profile, CUSTOM_PROFILE);
        assert_eq!(
            parsed.budget,
            ViewerGpuOutputBudget {
                min_records: 3,
                min_ready: 4,
                max_degraded: 2,
                max_waiting: 8,
                max_display_issues: 1,
                max_hdr_output_requires_hdr_surface: 0,
                max_output_color_space_requires_surface_color_space: 3,
                max_reconfigure_blocked_by_payload: 1,
                max_unsupported_presentation_intent: 2,
                max_unsupported_surface_contract: 4,
                max_unknown_display_issues: 0,
                max_display_contract_refreshes: 9,
                max_display_issue_refresh_correlations: 1,
                max_display_tone_map_headroom_changes: 2,
                max_available_surface_format_changes: 3,
                max_format_color_space_changes: 4,
                max_present_mode_changes: 5,
                max_alpha_mode_changes: 6,
                max_display_payload_blockers: 0,
                max_color_rejections: 2,
                ..ViewerGpuOutputBudget::default()
            }
        );
    }

    #[test]
    fn budget_args_parse_display_baseline_preset() {
        let args = vec![
            "target/perf/viewer.jsonl".to_owned(),
            "--preset".to_owned(),
            DISPLAY_BASELINE_PRESET.to_owned(),
        ];

        let parsed = BudgetArgs::parse(&args).expect("parse preset args");

        assert_eq!(parsed.path, Path::new("target/perf/viewer.jsonl"));
        assert_eq!(parsed.profile, DISPLAY_BASELINE_PRESET);
        assert_eq!(parsed.budget, ViewerGpuOutputBudget::display_baseline());
    }

    #[test]
    fn budget_args_allow_overrides_after_display_baseline_preset() {
        let args = vec![
            "target/perf/viewer.jsonl".to_owned(),
            "--preset".to_owned(),
            DISPLAY_BASELINE_PRESET.to_owned(),
            "--max-display-contract-refreshes".to_owned(),
            "4".to_owned(),
            "--max-display-issues".to_owned(),
            "1".to_owned(),
        ];

        let parsed = BudgetArgs::parse(&args).expect("parse preset override args");

        assert_eq!(parsed.path, Path::new("target/perf/viewer.jsonl"));
        assert_eq!(parsed.profile, DISPLAY_BASELINE_PRESET);
        assert_eq!(
            parsed.budget,
            ViewerGpuOutputBudget {
                max_display_contract_refreshes: 4,
                max_display_issues: 1,
                ..ViewerGpuOutputBudget::display_baseline()
            }
        );
    }

    #[test]
    fn budget_args_parse_profile() {
        let args = vec![
            "target/perf/viewer.jsonl".to_owned(),
            "--profile".to_owned(),
            "ci-display-wall".to_owned(),
        ];

        let parsed = BudgetArgs::parse(&args).expect("parse profile args");

        assert_eq!(parsed.path, Path::new("target/perf/viewer.jsonl"));
        assert_eq!(parsed.profile, "ci-display-wall");
        assert_eq!(parsed.budget, ViewerGpuOutputBudget::default());
    }

    #[test]
    fn budget_args_reject_legacy_format_switch() {
        let args = vec![
            "target/perf/viewer.jsonl".to_owned(),
            "--format".to_owned(),
            "summary".to_owned(),
        ];

        let err = BudgetArgs::parse(&args).expect_err("legacy format switch should fail");

        assert!(err.to_string().contains("unknown argument: --format"));
    }
}
