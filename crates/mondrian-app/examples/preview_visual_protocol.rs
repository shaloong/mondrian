//! Bounded tests over the actual Preview worker, notification, dependency and visual-task
//! source Modules. This is not full App or physical GPU lifecycle qualification.

#[cfg(test)]
#[path = "../src/app/preview_worker_lifecycle.rs"]
mod preview_worker_lifecycle;

#[cfg(test)]
#[allow(dead_code)] // Full Runtime consumer methods remain compiled but unused here.
#[path = "../src/app/preview_work_notification.rs"]
mod preview_work_notification;

#[cfg(test)]
#[allow(dead_code)] // This target deliberately tests only the visual protocol Interface.
#[path = "../src/app/preview_visual_execution_task.rs"]
mod preview_visual_execution_task;

#[cfg(test)]
#[allow(dead_code)] // Full Runtime consumer methods remain compiled but unused here.
#[path = "../src/app/preview_visual_dependencies.rs"]
mod preview_visual_dependencies;

#[cfg(not(test))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "This example is a protocol-test target, not a qualification runner.\n\
         Run: cargo test --release -p mondrian-app --features validation \
         --example preview_visual_protocol -j 1 -- --test-threads=1"
    );
    std::process::ExitCode::FAILURE
}
