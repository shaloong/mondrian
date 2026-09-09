//! Application media adaptation for production Preview.
//!
//! This production Adapter owns asset-library lookup, cache observation, proxy-job
//! dispatch, and diagnostics projection. Canonical media-source interpretation
//! lives in `app::preview_media_source`.

use super::request_scheduler::MediaPreviewRequestAdmission;
use super::*;
use crate::app::preview_access_mode::MediaPreviewRequestIntent;
use crate::app::preview_media_source::{
    resolve_preview_media_source, PreviewMediaDecodePathResolution, PreviewMediaSourceOutcome,
    PreviewMediaSourceRequest, PreviewProxyGenerationIntent,
};
use crate::app::preview_quality::preview_representation_quality;
use crate::app::preview_timeline_execution::{
    PreviewTimelineMediaFrame, PreviewTimelineMediaRequest, PreviewTimelineMediaWait,
};

/// Classify nonterminal admission outcomes without projecting expected
/// generation races as Viewer failures.
pub(super) const fn media_wait_for_admission(
    admission: MediaPreviewRequestAdmission,
) -> Option<PreviewTimelineMediaWait> {
    match admission {
        MediaPreviewRequestAdmission::Scheduled | MediaPreviewRequestAdmission::ExistingWork => {
            Some(PreviewTimelineMediaWait::Producer)
        }
        MediaPreviewRequestAdmission::DeferredResidencyTransition
        | MediaPreviewRequestAdmission::DeferredAggregateCapacity
        | MediaPreviewRequestAdmission::DeferredExecutionPressure
        | MediaPreviewRequestAdmission::ObsoleteGeneration => {
            Some(PreviewTimelineMediaWait::RetryAdmission)
        }
        MediaPreviewRequestAdmission::AlreadyResident
        | MediaPreviewRequestAdmission::BlockedCurrentDemand
        | MediaPreviewRequestAdmission::BlockedAggregateCapacity
        | MediaPreviewRequestAdmission::InvalidMediaIdentity
        | MediaPreviewRequestAdmission::InvalidScheduling
        | MediaPreviewRequestAdmission::ExpiredPrerollDeadline
        | MediaPreviewRequestAdmission::WorkerUnavailable => None,
    }
}

impl<O: Clone> PreviewProductionRuntime<O> {
    #[cfg(test)]
    pub(super) fn media_frame_for_plan(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        request: PreviewTimelineMediaRequest,
    ) -> PreviewTimelineMediaFrame {
        self.media_frame_for_plan_observing_key(snapshot, proxy_demands, request, |_| {})
    }

    pub(super) fn media_frame_for_plan_observing_key(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        request: PreviewTimelineMediaRequest,
        mut observe_key: impl FnMut(&MediaPreviewKey),
    ) -> PreviewTimelineMediaFrame {
        let transport = snapshot.transport();
        let access_mode = media_preview_access_mode_for_intent(media_preview_viewer_access_intent(
            transport.is_playing(),
            transport.seek_source(),
        ));
        let representation_quality = preview_representation_quality(transport.runtime_scale());
        let key = match self.media_preview_key_for_timeline_request(
            snapshot,
            proxy_demands,
            &request,
            representation_quality,
            true,
            access_mode == PreviewDecodeAccessMode::PlaybackCursor,
        ) {
            Ok(key) => key,
            Err(reason) => return PreviewTimelineMediaFrame::Unavailable { reason },
        };
        observe_key(&key);
        let generation = self.execution.borrow().generation();
        let demand_identity = (access_mode == PreviewDecodeAccessMode::PlaybackCursor)
            .then(|| transport.demand().map(PreviewFrameDemandSnapshot::identity))
            .flatten();
        let current_demand_id = demand_identity.map_or_else(
            || {
                mondrian_playback::MediaWorkDemandId::for_preview_generation(
                    transport.epoch(),
                    generation,
                    transport.current_frame(),
                )
            },
            mondrian_playback::MediaWorkDemandId::for_playback,
        );
        let intent = if transport.is_speculative_preparation() {
            MediaPreviewRequestIntent::Prefetch
        } else {
            MediaPreviewRequestIntent::Current(current_demand_id)
        };
        match self.cached_media_frame_for_intent(&key, intent) {
            Ok(Some(frame)) => return PreviewTimelineMediaFrame::Ready(frame),
            Ok(None) => {}
            Err(mondrian_playback::MediaFrameProtectionError::CurrentWorkingSetCapacity) => {
                return PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::MediaDecode,
                        "current Viewer media closure exceeds the machine-class per-demand working-set grant",
                    ),
                };
            }
        }
        if let Some(reason) = self.failed_media_key(&key) {
            return PreviewTimelineMediaFrame::Unavailable { reason };
        }
        let mut adaptive_hints = self.preview_decode_adaptive_hints(access_mode, &key);
        adaptive_hints.playback_direction = transport.playback_direction();
        let deadline = (access_mode == PreviewDecodeAccessMode::PlaybackCursor)
            .then(|| {
                if transport.is_speculative_preparation() {
                    transport.priming_work_deadline()
                } else {
                    transport.demand().and_then(PreviewFrameDemandSnapshot::adapter_deadline)
                }
            })
            .flatten();
        let admission = if request.cpu_working_required || self.viewer_cpu_fallback_active.get() {
            self.request_cpu_working_media_preview(
                key.clone(),
                intent,
                access_mode,
                deadline,
                demand_identity,
                adaptive_hints,
            )
        } else {
            self.request_media_preview(
                key.clone(),
                intent,
                access_mode,
                deadline,
                demand_identity,
                adaptive_hints,
            )
        };
        self.last_current_media_admission.set(Some(admission.as_str()));
        if let Some(wait) = media_wait_for_admission(admission) {
            // An obsolete generation has no physical producer and must be
            // retried from a fresh execution snapshot. Other pending outcomes
            // retain an observable work/residency owner that will publish a
            // retry edge.
            if admission != MediaPreviewRequestAdmission::ObsoleteGeneration {
                self.execution.borrow_mut().set_pending(true);
            }
            return PreviewTimelineMediaFrame::Pending { wait };
        }
        match admission {
            MediaPreviewRequestAdmission::AlreadyResident => {
                match self.cached_media_frame_for_intent(&key, intent) {
                    Ok(Some(frame)) => PreviewTimelineMediaFrame::Ready(frame),
                    Err(
                        mondrian_playback::MediaFrameProtectionError::CurrentWorkingSetCapacity,
                    ) => PreviewTimelineMediaFrame::Unavailable {
                        reason: PreviewUnavailability::blocked(
                            PreviewOutputStage::MediaDecode,
                            "current Viewer media closure exceeds the machine-class per-demand working-set grant",
                        ),
                    },
                    Ok(None) => PreviewTimelineMediaFrame::Unavailable {
                        reason: PreviewUnavailability::failed(
                            PreviewOutputStage::MediaDecode,
                            "Frame Store reported resident media but could not return the exact frame",
                        ),
                    },
                }
            }
            MediaPreviewRequestAdmission::BlockedCurrentDemand => {
                PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::MediaDecode,
                        "current Viewer media closure exceeds the machine-class per-demand working-set grant",
                    ),
                }
            }
            MediaPreviewRequestAdmission::BlockedAggregateCapacity => {
                PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::MediaDecode,
                        "Preview media aggregate capacity is occupied without an observable retry owner",
                    ),
                }
            }
            MediaPreviewRequestAdmission::InvalidMediaIdentity => {
                PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::blocked(
                        PreviewOutputStage::MediaResolution,
                        "media source lacks immutable object identity required for safe Preview reuse",
                    ),
                }
            }
            MediaPreviewRequestAdmission::InvalidScheduling => {
                PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::failed(
                        PreviewOutputStage::MediaDecode,
                        "media request violated the Preview priority/access scheduling contract",
                    ),
                }
            }
            MediaPreviewRequestAdmission::ExpiredPrerollDeadline => {
                PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::failed(
                        PreviewOutputStage::MediaDecode,
                        "playback preroll deadline expired before media producer admission",
                    ),
                }
            }
            MediaPreviewRequestAdmission::WorkerUnavailable => {
                PreviewTimelineMediaFrame::Unavailable {
                    reason: PreviewUnavailability::failed(
                        PreviewOutputStage::MediaDecode,
                        self.media_worker_start_failure.as_deref().unwrap_or(
                            "no Preview media worker is available",
                        ),
                    ),
                }
            }
            MediaPreviewRequestAdmission::Scheduled
            | MediaPreviewRequestAdmission::ExistingWork => PreviewTimelineMediaFrame::Pending {
                wait: PreviewTimelineMediaWait::Producer,
            },
            MediaPreviewRequestAdmission::DeferredResidencyTransition
            | MediaPreviewRequestAdmission::DeferredAggregateCapacity
            | MediaPreviewRequestAdmission::DeferredExecutionPressure
            | MediaPreviewRequestAdmission::ObsoleteGeneration => PreviewTimelineMediaFrame::Pending {
                wait: PreviewTimelineMediaWait::RetryAdmission,
            },
        }
    }

    pub(super) fn cached_media_frame_for_intent(
        &self,
        key: &MediaPreviewKey,
        intent: MediaPreviewRequestIntent,
    ) -> Result<Option<MediaPreviewFrame>, mondrian_playback::MediaFrameProtectionError> {
        // Ticketless successor and lookahead preparation retain the physical
        // payload they actually consume, but cannot acquire Current's protected
        // working-set grant or preempt nearer speculative work under that role.
        let frame = match intent {
            MediaPreviewRequestIntent::Current(demand_id) => {
                self.frame_store.borrow_mut().protected_media_frame(key, demand_id)
            }
            MediaPreviewRequestIntent::Prefetch => {
                Ok(self.frame_store.borrow_mut().media_frame(key))
            }
        };
        match &frame {
            Ok(Some(_)) | Err(_) => bump(&self.metrics.media_cache_hits),
            Ok(None) => bump(&self.metrics.media_cache_misses),
        }
        frame
    }

    pub(super) fn failed_media_key(&self, key: &MediaPreviewKey) -> Option<PreviewUnavailability> {
        let failure = self
            .media_execution_failure(key)
            .map(media_execution_failure_unavailability)
            .or_else(|| {
                self.frame_store.borrow_mut().contains_failure(key).then(|| {
                    PreviewUnavailability::failed(
                        PreviewOutputStage::MediaDecode,
                        format!(
                            "media {} is in persistent terminal source-decode failure memory",
                            key.asset_id
                        ),
                    )
                })
            });
        if failure.is_some() {
            bump(&self.metrics.media_failure_hits);
        }
        failure
    }

    fn media_execution_failure(&self, key: &MediaPreviewKey) -> Option<MediaPreviewFailureReason> {
        let generation = self.execution.borrow().generation();
        let mut failures = self.media_execution_failures.borrow_mut();
        failures.retain(|_, (failure_generation, _)| *failure_generation == generation);
        failures.get(key).map(|(_, reason)| *reason)
    }

    pub(super) fn media_preview_key_for_timeline_request(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        request: &PreviewTimelineMediaRequest,
        representation_quality: mondrian_media::PreviewRepresentationQuality,
        record_color_rejection: bool,
        request_missing_proxy_generation: bool,
    ) -> Result<MediaPreviewKey, PreviewUnavailability> {
        self.media_preview_key_for_asset_with_hardware_admission(
            snapshot,
            proxy_demands,
            &request.asset_id,
            request.color_space_override,
            request.alpha_interpretation,
            request.picture_overrides,
            request.source_sample,
            request.target_resolution.width,
            request.target_resolution.height,
            &request.input_color,
            record_color_rejection,
            request_missing_proxy_generation,
            self.hardware_decode_admission.get(),
            request.cpu_working_required || self.viewer_cpu_fallback_active.get(),
            representation_quality,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub(super) fn media_preview_key_for_asset(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        asset_id: &AssetId,
        color_space_override: Option<ColorSpace>,
        alpha_interpretation: AlphaInterpretation,
        source_time: mondrian_core::TimelineTime,
        target_width: u32,
        target_height: u32,
        runtime_scale: mondrian_playback::PreviewResolutionScale,
        input_color: &MediaInputColorContext,
        record_color_rejection: bool,
        request_missing_proxy_generation: bool,
    ) -> Result<MediaPreviewKey, PreviewUnavailability> {
        self.media_preview_key_for_asset_with_hardware_admission(
            snapshot,
            proxy_demands,
            asset_id,
            color_space_override,
            alpha_interpretation,
            mondrian_core::PictureInterpretationOverrides::default(),
            mondrian_core::SourceSampleTarget::covering(source_time),
            target_width,
            target_height,
            input_color,
            record_color_rejection,
            request_missing_proxy_generation,
            self.hardware_decode_admission.get(),
            false,
            preview_representation_quality(runtime_scale),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn media_preview_key_for_asset_with_hardware_admission(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        asset_id: &AssetId,
        color_space_override: Option<ColorSpace>,
        alpha_interpretation: AlphaInterpretation,
        picture_overrides: mondrian_core::PictureInterpretationOverrides,
        source_sample: mondrian_core::SourceSampleTarget,
        _target_width: u32,
        _target_height: u32,
        input_color: &MediaInputColorContext,
        record_color_rejection: bool,
        request_missing_proxy_generation: bool,
        hardware_admission: PreviewHardwareDecodeAdmissionState,
        cpu_working_required: bool,
        representation_quality: mondrian_media::PreviewRepresentationQuality,
    ) -> Result<MediaPreviewKey, PreviewUnavailability> {
        let authoring = snapshot.authoring().ok_or_else(|| {
            PreviewUnavailability::blocked(
                PreviewOutputStage::MediaResolution,
                "project asset library is unavailable",
            )
        })?;
        let library = authoring.asset_library();
        let asset = match library.get_asset(*asset_id) {
            Ok(Some(asset)) if matches!(asset.kind, AssetKind::Video | AssetKind::StillImage) => {
                asset
            }
            Ok(Some(_)) => {
                return Err(PreviewUnavailability::blocked(
                    PreviewOutputStage::MediaResolution,
                    format!("asset {asset_id} is not a video source"),
                ));
            }
            Ok(None) => {
                return Err(PreviewUnavailability::blocked(
                    PreviewOutputStage::MediaResolution,
                    format!("asset {asset_id} does not exist in the project library"),
                ));
            }
            Err(err) => {
                return Err(PreviewUnavailability::failed(
                    PreviewOutputStage::MediaResolution,
                    format!("asset {asset_id} lookup failed: {err}"),
                ));
            }
        };
        let proxy_config = authoring.proxy().config();
        let proxy_color = resolve_asset_proxy_color_contract(&asset, input_color).ok();
        let prefer_proxy = authoring.proxy().prefers_proxy(*asset_id);
        match resolve_preview_media_source(PreviewMediaSourceRequest {
            asset: &asset,
            color_space_override,
            alpha_interpretation,
            picture_overrides,
            source_sample,
            input_color,
            prefer_proxy,
            request_missing_proxy_generation,
            proxy_config,
            proxy_color,
            hardware_admission,
            cpu_working_required,
            representation_quality,
        }) {
            PreviewMediaSourceOutcome::Ready(resolved) => {
                match resolved.path_resolution {
                    PreviewMediaDecodePathResolution::Proxy => {
                        bump(&self.metrics.media_proxy_path_hits);
                    }
                    PreviewMediaDecodePathResolution::ProxyMissing => {
                        bump(&self.metrics.media_proxy_path_misses);
                    }
                    PreviewMediaDecodePathResolution::ProxyStale => {
                        bump(&self.metrics.media_proxy_path_stale);
                    }
                    PreviewMediaDecodePathResolution::Source
                    | PreviewMediaDecodePathResolution::ProxyColorIncompatible => {
                        bump(&self.metrics.media_proxy_path_bypasses);
                    }
                }
                self.record_input_color_resolution(resolved.input_color_resolution.source);
                self.maybe_request_preview_proxy_generation(
                    proxy_demands,
                    resolved.proxy_generation,
                );
                Ok(resolved.key)
            }
            PreviewMediaSourceOutcome::ColorRejected(rejection) => {
                let resolution = rejection.input_color_resolution;
                self.record_input_color_resolution(resolution.source);
                let diagnostic_summary = rejection.diagnostic.summary();
                let diagnostic_issue_summary = rejection.diagnostic.issue_summary();
                if record_color_rejection {
                    self.record_color_rejection(PreviewColorRejection::new(
                        rejection.asset_id,
                        rejection.path.clone(),
                        resolution,
                        diagnostic_summary.clone(),
                        diagnostic_issue_summary,
                    ));
                }
                tracing::warn!(
                    asset_id = %rejection.asset_id,
                    path = %rejection.path.display(),
                    missing_metadata_policy = ?input_color.missing_metadata_policy,
                    color_resolution_source = ?resolution.source,
                    override_color_space = ?resolution.override_color_space,
                    executable_color_space = ?resolution.executable_color_space,
                    working_color_space = ?resolution.working_color_space,
                    color_diagnostic = %diagnostic_summary,
                    color_diagnostic_issue_summary = ?diagnostic_issue_summary,
                    "viewer preview rejected media with missing color metadata"
                );
                Err(PreviewUnavailability::blocked(
                    PreviewOutputStage::InputColor,
                    format!(
                        "asset {} input color interpretation was rejected: {}",
                        rejection.asset_id, diagnostic_summary
                    ),
                ))
            }
            PreviewMediaSourceOutcome::Unavailable(unavailable) => {
                let source = unavailable
                    .path
                    .as_deref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<non-file asset>".to_owned());
                tracing::debug!(
                    asset_id = %unavailable.asset_id,
                    path = %source,
                    reason = %unavailable.reason,
                    "viewer preview media source is unavailable"
                );
                Err(PreviewUnavailability::blocked(
                    PreviewOutputStage::MediaResolution,
                    format!(
                        "asset {} source {} is unavailable: {}",
                        unavailable.asset_id, source, unavailable.reason
                    ),
                ))
            }
        }
    }

    fn maybe_request_preview_proxy_generation(
        &self,
        proxy_demands: &dyn PreviewProxyDemandSink,
        intent: Option<PreviewProxyGenerationIntent>,
    ) {
        let Some(intent) = intent else {
            return;
        };
        let asset_id = intent.key.asset_id;
        let outcome = proxy_demands.request_preview_proxy(intent);
        match outcome {
            ProxyGenerationRequestOutcome::Admitted { .. } => {
                bump(&self.metrics.media_proxy_generation_requests);
            }
            ProxyGenerationRequestOutcome::AlreadyFresh
            | ProxyGenerationRequestOutcome::Deduplicated { .. }
            | ProxyGenerationRequestOutcome::RetainedFailure(_) => {
                bump(&self.metrics.media_proxy_generation_request_dedupes);
            }
            ProxyGenerationRequestOutcome::Failed(failure) => {
                tracing::warn!(
                    target: "mondrian::proxy",
                    asset_id = %asset_id,
                    reason = failure.reason.code(),
                    detail = %failure.detail,
                    "preview proxy generation request failed"
                );
            }
        }
    }
}

fn media_execution_failure_unavailability(
    reason: MediaPreviewFailureReason,
) -> PreviewUnavailability {
    match reason {
        MediaPreviewFailureReason::ResidencyCapacityRejected => PreviewUnavailability::blocked(
            PreviewOutputStage::MediaDecode,
            "decoded media allocation exceeds the active Preview generation's exact-demand or aggregate physical residency grant",
        ),
        MediaPreviewFailureReason::ResidencyContractViolation => PreviewUnavailability::failed(
            PreviewOutputStage::MediaDecode,
            "decoded media result violated the Preview Frame Store ownership contract",
        ),
        MediaPreviewFailureReason::WorkerPanicked => PreviewUnavailability::failed(
            PreviewOutputStage::MediaDecode,
            "Preview media worker panicked while executing this generation",
        ),
        reason => PreviewUnavailability::failed(
            PreviewOutputStage::MediaDecode,
            format!("Preview media execution failed for this generation: {reason:?}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::preview_unavailability::PreviewUnavailabilityDisposition;

    #[test]
    fn execution_failures_preserve_capacity_and_internal_failure_classification() {
        let capacity = media_execution_failure_unavailability(
            MediaPreviewFailureReason::ResidencyCapacityRejected,
        );
        assert_eq!(
            capacity.disposition(),
            PreviewUnavailabilityDisposition::Blocked
        );
        assert_eq!(capacity.stage(), PreviewOutputStage::MediaDecode);

        for reason in [
            MediaPreviewFailureReason::ResidencyContractViolation,
            MediaPreviewFailureReason::WorkerPanicked,
        ] {
            let unavailable = media_execution_failure_unavailability(reason);
            assert_eq!(
                unavailable.disposition(),
                PreviewUnavailabilityDisposition::Failed
            );
            assert_eq!(unavailable.stage(), PreviewOutputStage::MediaDecode);
        }
    }
}
