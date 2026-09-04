//! Production-linked qualification of owning App UI Host startup.

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "explicitly starts the packaged product worker and every Host service"]
    fn product_host_retains_app_and_all_partial_service_owners() {
        let report = mondrian_app::app_ui::host::qualify_app_ui_host_startup_ownership()
            .expect("production-linked Host startup qualification");
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.successful_routes, 2);
        assert_eq!(report.failed_start_cases.len(), 26);
        assert!(report.opaque_payload_fail_closed);
        assert!(report
            .failed_start_cases
            .iter()
            .all(|case| case.shutdown.all_created_resources_released()));
    }
}

#[cfg(not(test))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "Run cargo test --release -p mondrian-app --features validation --example host_startup_qualification -j 1 -- --ignored --test-threads=1"
    );
    std::process::ExitCode::FAILURE
}
