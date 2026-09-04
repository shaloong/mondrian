//! Deterministic channel/JoinHandle protocol tests; real GPU coverage is separate.

use super::*;
use crate::ViewerCpuYuvUploadWorkerExit;
use std::time::{Duration, Instant};

fn protocol_owner(
    work: impl FnOnce(
            mpsc::Receiver<CpuYuvUploadWorkerCommand>,
            mpsc::SyncSender<CpuYuvUploadWorkerResult>,
            mpsc::Receiver<wgpu::Buffer>,
        ) + Send
        + 'static,
) -> CpuYuvUploadRuntime {
    let (request_sender, requests) = mpsc::sync_channel(CPU_YUV_UPLOAD_WORKER_CAPACITY);
    let (results, result_receiver) = mpsc::sync_channel(CPU_YUV_UPLOAD_WORKER_CAPACITY);
    let (returned_sender, returns) = mpsc::channel();
    let worker = std::thread::spawn(move || work(requests, results, returns));
    CpuYuvUploadRuntime {
        request_sender,
        trim_requested: Arc::new(AtomicBool::new(false)),
        state: Mutex::new(CpuYuvUploadState {
            slots: vec![],
            next_slot: 0,
            generation: 1,
            pending: vec![],
            prepared: VecDeque::new(),
            result_receiver,
            returned_sender,
            used_buffers: vec![],
            completion_waker: None,
        }),
        worker,
    }
}

fn finish(owner: &mut CpuYuvUploadRetirement) -> ViewerCpuYuvUploadWorkerExit {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(outcome) = owner.poll() {
            return outcome;
        }
        assert!(Instant::now() < deadline, "upload worker did not terminate");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn idle_worker_exits_even_while_late_map_return_sender_survives() {
    let owner = protocol_owner(|requests, _results, _returns| {
        assert!(requests.recv().is_err());
    });
    let late_return = owner.state.lock().returned_sender.clone();
    let mut retiring = owner.into_retirement();
    assert_eq!(
        finish(&mut retiring),
        ViewerCpuYuvUploadWorkerExit::Returned
    );
    for _ in 0..8 {
        assert_eq!(
            retiring.poll(),
            Some(ViewerCpuYuvUploadWorkerExit::Returned)
        );
    }
    drop(late_return);
}

#[test]
fn full_result_channel_cannot_deadlock_consuming_retirement() {
    let (attempting_send, attempt) = mpsc::channel();
    let owner = protocol_owner(move |_requests, results, _returns| {
        for index in 0..=CPU_YUV_UPLOAD_WORKER_CAPACITY {
            if index == CPU_YUV_UPLOAD_WORKER_CAPACITY {
                attempting_send.send(()).expect("report full-channel send");
            }
            let sent = results.send(CpuYuvUploadWorkerResult {
                key: CpuYuvFrameUploadKey { generation: 1, frame_identity: index },
                outcome: Err("protocol-only result".to_owned()),
            });
            if index == CPU_YUV_UPLOAD_WORKER_CAPACITY {
                assert!(
                    sent.is_err(),
                    "retirement must disconnect the full result queue"
                );
            } else {
                assert!(sent.is_ok());
            }
        }
    });
    attempt
        .recv_timeout(Duration::from_secs(5))
        .expect("full queue before retirement");
    let mut retiring = owner.into_retirement();
    assert_eq!(
        finish(&mut retiring),
        ViewerCpuYuvUploadWorkerExit::Returned
    );
}

#[test]
fn pending_worker_is_not_joined_or_reported_terminal_early() {
    let (release, gate) = mpsc::channel();
    let owner = protocol_owner(move |_requests, _results, _returns| {
        gate.recv_timeout(Duration::from_secs(5)).expect("release pending work");
    });
    let mut retiring = owner.into_retirement();
    assert_eq!(retiring.poll(), None);
    release.send(()).expect("release worker");
    assert_eq!(
        finish(&mut retiring),
        ViewerCpuYuvUploadWorkerExit::Returned
    );
}

#[test]
fn joined_panic_is_terminal_but_never_healthy() {
    let owner = protocol_owner(|_requests, _results, _returns| panic!("injected upload panic"));
    let mut retiring = owner.into_retirement();
    assert_eq!(
        finish(&mut retiring),
        ViewerCpuYuvUploadWorkerExit::Panicked
    );
    assert_eq!(
        retiring.poll(),
        Some(ViewerCpuYuvUploadWorkerExit::Panicked)
    );
    assert!(!crate::ViewerGpuRetirementReceipt {
        cpu_yuv_upload: ViewerCpuYuvUploadWorkerExit::Panicked,
        native_device_removed: false,
    }
    .is_healthy());
}
