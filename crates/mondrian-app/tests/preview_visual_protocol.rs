//! Bounded tests over the actual Preview worker, notification and visual-task
//! source Modules. This is not full App or physical GPU lifecycle qualification.

#[path = "../src/app/preview_worker_lifecycle.rs"]
mod preview_worker_lifecycle;

// Preserve the existing import path with the exact production Module/type.
// This alias implements no Runtime, join policy, or substitute execution state.
use preview_worker_lifecycle as preview_runtime;

#[allow(dead_code)] // Full Runtime consumer methods remain compiled but unused here.
#[path = "../src/app/preview_work_notification.rs"]
mod preview_work_notification;

#[allow(dead_code)] // This target deliberately tests only the visual protocol Interface.
#[path = "../src/app/preview_visual_execution_task.rs"]
mod preview_visual_execution_task;
