//! Public GPU closure protocol; these cases do not claim physical fault injection.
#![cfg(feature = "validation")]

use mondrian_app::app::endurance_campaign::{
    EnduranceGpuShutdownEvidence, ViewerGpuDeviceGenerationTerminalKind,
};
use mondrian_renderer::{ViewerCpuYuvUploadWorkerExit, ViewerGpuRetirementReceipt};

fn normal_receipt() -> EnduranceGpuShutdownEvidence {
    let owners: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/validation/fixtures/window-owner-closure.json"
    ))
    .expect("owner replay fixture");
    EnduranceGpuShutdownEvidence {
        worker_shutdown: serde_json::from_str(r#""terminated""#).expect("worker outcome"),
        wake_callbacks: serde_json::from_value(
            owners["host_shutdown"]["preview"]["work_callbacks"].clone(),
        )
        .expect("callback receipt"),
        native_wake_failures: 0,
        wake_registration_rejections: 0,
        worker_started: true,
        worker_terminated: true,
        worker_panicked: false,
        timed_out: false,
        retirement_requested: true,
        retirement_handoff_accepted: true,
        retirement_completed: true,
        renderer_retirement: Some(ViewerGpuRetirementReceipt {
            cpu_yuv_upload: ViewerCpuYuvUploadWorkerExit::Returned,
            native_device_removed: false,
        }),
        generation_terminal_kind: None,
    }
}

#[test]
fn physical_release_does_not_qualify_a_terminal_gpu_generation() {
    use ViewerGpuDeviceGenerationTerminalKind::*;
    for (terminal, losses, fatal) in [
        (None, 0, 0),
        (Some(DeviceLost), 1, 0),
        (Some(DeviceDestroyed), 0, 1),
        (Some(ProgressFailure), 0, 1),
    ] {
        let evidence = EnduranceGpuShutdownEvidence {
            generation_terminal_kind: terminal,
            ..normal_receipt()
        };
        assert!(evidence.retirement_completed);
        assert_eq!(evidence.generation_terminal_kind, terminal);
        assert_eq!(evidence.qualifies_normal_runtime(), terminal.is_none());
        assert_eq!(evidence.device_loss_count(), losses);
        assert_eq!(evidence.fatal_error_count(), fatal);
    }
}

#[test]
fn every_normal_shutdown_barrier_is_required() {
    let normal = normal_receipt();
    assert!(normal.qualifies_normal_runtime());
    for evidence in [
        EnduranceGpuShutdownEvidence { native_wake_failures: 1, ..normal },
        EnduranceGpuShutdownEvidence { wake_registration_rejections: 1, ..normal },
        EnduranceGpuShutdownEvidence { wake_callbacks: Default::default(), ..normal },
        EnduranceGpuShutdownEvidence {
            worker_shutdown: serde_json::from_str(r#""timed_out_detached""#)
                .expect("worker outcome"),
            ..normal
        },
        EnduranceGpuShutdownEvidence { worker_started: false, ..normal },
        EnduranceGpuShutdownEvidence { worker_terminated: false, ..normal },
        EnduranceGpuShutdownEvidence { worker_panicked: true, ..normal },
        EnduranceGpuShutdownEvidence { timed_out: true, ..normal },
        EnduranceGpuShutdownEvidence { retirement_requested: false, ..normal },
        EnduranceGpuShutdownEvidence { retirement_handoff_accepted: false, ..normal },
        EnduranceGpuShutdownEvidence { retirement_completed: false, ..normal },
        EnduranceGpuShutdownEvidence { renderer_retirement: None, ..normal },
        EnduranceGpuShutdownEvidence {
            renderer_retirement: Some(ViewerGpuRetirementReceipt {
                cpu_yuv_upload: ViewerCpuYuvUploadWorkerExit::Panicked,
                native_device_removed: false,
            }),
            ..normal
        },
        EnduranceGpuShutdownEvidence {
            renderer_retirement: Some(ViewerGpuRetirementReceipt {
                cpu_yuv_upload: ViewerCpuYuvUploadWorkerExit::Returned,
                native_device_removed: true,
            }),
            ..normal
        },
    ] {
        assert!(!evidence.qualifies_normal_runtime(), "{evidence:?}");
    }
}
