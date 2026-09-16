//! Synchronous test-only retirement. Production Adapters use their progress owner.

use anyhow::{anyhow, Result};
use mondrian_renderer::{ViewerGpuExecutionRuntime, ViewerGpuRetirementReceipt};
use std::time::{Duration, Instant};

pub fn retire_runtime(
    device: &wgpu::Device,
    runtime: ViewerGpuExecutionRuntime,
) -> Result<ViewerGpuRetirementReceipt> {
    let mut owner = runtime.into_retirement();
    let deadline = Instant::now() + Duration::from_secs(30);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut queue_completed = false;
        loop {
            if !queue_completed {
                queue_completed = match device.poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_millis(10)),
                }) {
                    Ok(wgpu::PollStatus::QueueEmpty | wgpu::PollStatus::WaitSucceeded) => true,
                    Ok(wgpu::PollStatus::Poll) | Err(wgpu::PollError::Timeout) => false,
                    Err(error) => return Err(anyhow!("retirement queue barrier: {error}")),
                };
            }
            if let Some(receipt) = owner.poll()?
                && queue_completed
            {
                return Ok(receipt);
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("Viewer retirement timed out"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }))
    .unwrap_or_else(|_| {
        Err(anyhow!(
            "Viewer retirement panicked before release was proved"
        ))
    });
    if result.is_err() {
        // A failing qualification may not destroy unproved native resources.
        std::mem::forget((owner, device.clone()));
    }
    let receipt = result?;
    if !receipt.is_healthy() {
        return Err(anyhow!("unhealthy Renderer retirement: {receipt:?}"));
    }
    Ok(receipt)
}
