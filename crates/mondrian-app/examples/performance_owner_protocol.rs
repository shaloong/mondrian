//! Bounded protocol checks linked to the actual validation App library.

#[cfg(not(test))]
fn main() {
    eprintln!("Run with cargo test -p mondrian-app --features validation --example performance_owner_protocol -- --test-threads=1");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use mondrian_app::app::performance_owner_closure::{
        run_performance_owner_protocol_case, PerformanceOwnerProtocolCase,
    };

    #[test]
    fn actual_app_preview_and_cache_owners_close_across_success_error_panic_and_missing_inventory()
    {
        for case in [
            PerformanceOwnerProtocolCase::Clean,
            PerformanceOwnerProtocolCase::OperationError,
            PerformanceOwnerProtocolCase::OperationPanic,
            PerformanceOwnerProtocolCase::MissingRequiredCache,
        ] {
            let cache = tempfile::tempdir().expect("bounded protocol cache");
            let receipt =
                run_performance_owner_protocol_case(case, cache.path()).expect("raw owner receipt");
            assert_eq!(
                receipt.accepted,
                case == PerformanceOwnerProtocolCase::Clean,
                "{case:?}: {receipt:?}"
            );
            assert_eq!(
                receipt.closure_qualified,
                case != PerformanceOwnerProtocolCase::MissingRequiredCache,
                "{case:?}: {receipt:?}"
            );
            let raw: serde_json::Value =
                serde_json::from_str(&receipt.owner_closure_json).expect("canonical closure");
            assert_eq!(raw["app"]["app_owner_consumed"], true);
            assert_eq!(raw["preview_owner_count"], 1);
            assert_eq!(raw["previews"][0]["worker_panics"], 0);
            assert_eq!(raw["previews"][0]["render_cache_required"], true);
            assert_eq!(
                raw["previews"][0]["render_cache_start_failed"],
                case == PerformanceOwnerProtocolCase::MissingRequiredCache
            );
            if case == PerformanceOwnerProtocolCase::MissingRequiredCache {
                assert!(raw["previews"][0]["render_cache_worker"].is_null());
            } else {
                assert_eq!(
                    raw["previews"][0]["render_cache_worker"]["worker_terminated"],
                    true
                );
            }
            assert_eq!(
                raw["previews"][0]["work_callbacks"]["admission_closed"],
                true
            );
        }
    }
}
