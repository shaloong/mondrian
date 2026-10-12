//! Interpret one fully reaped verifier result under the baseline SMPTE DCP contract.

use mondrian_media::SupervisedProcessOutput;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DcpVerificationRejection {
    #[error("verifier did not exit successfully")]
    UnsuccessfulExit,
    #[error("verifier report is incomplete")]
    IncompleteReport,
    #[error("verifier reported a SMPTE interoperability error")]
    InteroperabilityError,
}

pub(crate) fn validate_dcp_verifier_output(
    output: &SupervisedProcessOutput,
) -> Result<String, DcpVerificationRejection> {
    if !output.status.success() {
        return Err(DcpVerificationRejection::UnsuccessfulExit);
    }
    if output.stdout_truncated || output.stderr_truncated {
        return Err(DcpVerificationRejection::IncompleteReport);
    }
    let mut combined = output.stdout.clone();
    combined.extend_from_slice(&output.stderr);
    let report = String::from_utf8_lossy(&combined).into_owned();
    if [output.stdout.as_slice(), output.stderr.as_slice()].iter().any(|stream| {
        String::from_utf8_lossy(stream)
            .lines()
            .any(|line| line.trim_start().starts_with("Error:"))
    }) {
        return Err(DcpVerificationRejection::InteroperabilityError);
    }
    Ok(report)
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use mondrian_core::ExecutionCancellationToken;

    use crate::queue::{run_dcp_package_validator, JobExecutionResult};

    fn validate(script: &str) -> Result<String, JobExecutionResult> {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        run_dcp_package_validator(
            &mut command,
            &ExecutionCancellationToken::new(),
            Instant::now() + Duration::from_secs(10),
        )
    }

    #[test]
    fn warning_cannot_excuse_abnormal_verifier_termination() {
        for script in [
            "printf 'Warning: diagnostic\\n'; exit 9",
            "printf 'Warning: diagnostic\\n'; kill -TERM $$",
            "printf 'Bv2.1 error: diagnostic\\n'; exit 1",
            "exit 9",
        ] {
            let result = validate(script);
            assert!(
                matches!(result, Err(JobExecutionResult::Failed(_))),
                "abnormal verifier result was accepted: {result:?}"
            );
        }
    }

    #[test]
    fn successful_baseline_verification_retains_bv21_and_warning_findings() {
        let report =
            validate("printf 'Bv2.1 error: baseline distinction\\nWarning: diagnostic\\n'; exit 0")
                .expect("successful baseline report");
        assert!(report.contains("Bv2.1 error: baseline distinction"));
        assert!(report.contains("Warning: diagnostic"));
        assert_eq!(validate("exit 0").expect("quiet successful verifier"), "");
    }

    #[test]
    fn successful_exit_cannot_excuse_an_interoperability_error() {
        let result =
            validate("printf 'Warning: diagnostic'; printf 'Error: bad asset hash\\n' >&2; exit 0");
        assert!(matches!(result, Err(JobExecutionResult::Failed(_))));
    }

    #[test]
    fn stderr_truncation_cannot_hide_an_early_interoperability_error() {
        let result = validate(
            "printf 'Error: bad asset hash\\n' >&2; printf '%02097152d' 0 >&2; \
             printf '\\nWarning: tail\\n' >&2; exit 0",
        );
        assert!(matches!(result, Err(JobExecutionResult::Failed(_))));
    }
}
