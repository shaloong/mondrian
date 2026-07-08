//! App-layer proxy generation dispatcher shared by import actions and preview pressure.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use mondrian_core::types::AssetId;

/// Queue proxy generation for a video asset on the app proxy worker pool.
pub(crate) fn request_proxy_generation(
    asset_id: AssetId,
    source_path: PathBuf,
    proxy_config: mondrian_media::ProxyConfig,
) {
    proxy_generation_dispatcher().enqueue(ProxyGenerationJob {
        asset_id,
        source_path,
        proxy_config,
    });
}

struct ProxyGenerationJob {
    asset_id: AssetId,
    source_path: PathBuf,
    proxy_config: mondrian_media::ProxyConfig,
}

struct ProxyGenerationDispatcher {
    sender: mpsc::Sender<ProxyGenerationJob>,
}

impl ProxyGenerationDispatcher {
    fn start(worker_count: usize) -> Self {
        let (sender, receiver) = mpsc::channel::<ProxyGenerationJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..worker_count.max(1) {
            let receiver = Arc::clone(&receiver);
            let spawn_result = std::thread::Builder::new()
                .name(format!("mondrian-proxy-generator-{index}"))
                .spawn(move || proxy_generation_worker_loop(receiver));
            if let Err(err) = spawn_result {
                tracing::error!(
                    target: "mondrian::proxy",
                    worker_index = index,
                    "failed to start proxy generation worker: {err}"
                );
            }
        }
        Self { sender }
    }

    fn enqueue(&self, job: ProxyGenerationJob) {
        if let Err(err) = self.sender.send(job) {
            tracing::warn!(
                target: "mondrian::proxy",
                asset_id = %err.0.asset_id,
                path = %err.0.source_path.display(),
                "proxy generation dispatcher is unavailable"
            );
        }
    }
}

fn proxy_generation_dispatcher() -> &'static ProxyGenerationDispatcher {
    static DISPATCHER: OnceLock<ProxyGenerationDispatcher> = OnceLock::new();
    DISPATCHER.get_or_init(|| ProxyGenerationDispatcher::start(proxy_generation_worker_count()))
}

fn proxy_generation_worker_loop(receiver: Arc<Mutex<mpsc::Receiver<ProxyGenerationJob>>>) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(
                target: "mondrian::proxy",
                "failed to build proxy generation worker runtime: {err}"
            );
            return;
        }
    };

    loop {
        let job = {
            let receiver = match receiver.lock() {
                Ok(receiver) => receiver,
                Err(poisoned) => poisoned.into_inner(),
            };
            receiver.recv()
        };
        let Ok(job) = job else {
            break;
        };

        runtime.block_on(async move {
            let generator = mondrian_media::ProxyGenerator::new(job.proxy_config);
            let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(8);
            if let Err(err) =
                generator.generate(job.asset_id, job.source_path.clone(), progress_tx).await
            {
                tracing::warn!(
                    target: "mondrian::proxy",
                    asset_id = %job.asset_id,
                    path = %job.source_path.display(),
                    "proxy generation failed: {err}"
                );
            }
        });
    }
}

const MAX_PROXY_GENERATION_WORKERS: usize = 8;

fn proxy_generation_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| proxy_generation_worker_count_for(parallelism.get()))
        .unwrap_or(1)
}

fn proxy_generation_worker_count_for(parallelism: usize) -> usize {
    parallelism.saturating_sub(2).clamp(1, MAX_PROXY_GENERATION_WORKERS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_generation_worker_count_reserves_capacity_for_preview() {
        assert_eq!(proxy_generation_worker_count_for(0), 1);
        assert_eq!(proxy_generation_worker_count_for(1), 1);
        assert_eq!(proxy_generation_worker_count_for(2), 1);
        assert_eq!(proxy_generation_worker_count_for(4), 2);
        assert_eq!(
            proxy_generation_worker_count_for(16),
            MAX_PROXY_GENERATION_WORKERS
        );
    }
}
