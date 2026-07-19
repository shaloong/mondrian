//! Concrete media Adapter behind the proxy execution service.

use mondrian_core::ExecutionCancellationToken;
use mondrian_media::proxy::ProxyProgress;
use mondrian_media::{ProxyGenerationOutcome, ProxyGenerator, ProxyStatus};

use super::state::ProxyGenerationRequest;
use super::{ProxyGenerationFailure, ProxyGenerationFailureReason};

pub(super) trait ProxyGenerationBackend: Send + Sync {
    fn status(
        &self,
        request: &ProxyGenerationRequest,
    ) -> Result<ProxyStatus, ProxyGenerationFailure>;

    fn execute(
        &self,
        runtime: &tokio::runtime::Runtime,
        request: &ProxyGenerationRequest,
        cancellation: ExecutionCancellationToken,
    ) -> Result<ProxyGenerationOutcome, ProxyGenerationFailure>;
}

pub(super) struct MediaProxyGenerationBackend;

impl ProxyGenerationBackend for MediaProxyGenerationBackend {
    fn status(
        &self,
        request: &ProxyGenerationRequest,
    ) -> Result<ProxyStatus, ProxyGenerationFailure> {
        let generator = ProxyGenerator::new(request.config.clone());
        generator.encoding_profile(request.color).map_err(|error| {
            ProxyGenerationFailure::new(
                ProxyGenerationFailureReason::InvalidProxyContract,
                error.to_string(),
            )
        })?;
        generator.proxy_path(&request.key.source_path, request.color).map_err(|error| {
            ProxyGenerationFailure::new(
                ProxyGenerationFailureReason::InvalidProxyContract,
                error.to_string(),
            )
        })?;
        generator
            .proxy_status_for_source_fingerprint(
                &request.key.source_path,
                request.key.source_fingerprint,
                request.color,
            )
            .map_err(|error| {
                ProxyGenerationFailure::new(
                    ProxyGenerationFailureReason::GenerationFailed,
                    error.to_string(),
                )
            })
    }

    fn execute(
        &self,
        runtime: &tokio::runtime::Runtime,
        request: &ProxyGenerationRequest,
        cancellation: ExecutionCancellationToken,
    ) -> Result<ProxyGenerationOutcome, ProxyGenerationFailure> {
        let generator = ProxyGenerator::new(request.config.clone());
        let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel::<ProxyProgress>(8);
        runtime
            .block_on(generator.generate_cancellable(
                request.key.asset_id,
                request.key.source_path.clone(),
                request.key.source_fingerprint,
                request.color,
                progress_tx,
                cancellation,
            ))
            .map_err(|error| {
                ProxyGenerationFailure::new(
                    ProxyGenerationFailureReason::GenerationFailed,
                    error.to_string(),
                )
            })
    }
}
