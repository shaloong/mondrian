//! Public GPU closure protocol; these cases do not claim physical fault injection.
#![cfg(feature = "validation")]

use mondrian_app::app::endurance_campaign::{
    EnduranceGpuShutdownEvidence, ViewerGpuDeviceGenerationTerminalKind,
};
use mondrian_renderer::{ViewerCpuYuvUploadWorkerExit, ViewerGpuRetirementReceipt};

fn normal_receipt() -> EnduranceGpuShutdownEvidence {
    EnduranceGpuShutdownEvidence {
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
