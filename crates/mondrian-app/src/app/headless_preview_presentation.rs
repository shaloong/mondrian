//! UI-independent Headless presentation over the production Preview Runtime.
//!
//! Performance and Golden validation Adapters share this coordinator so GPU
//! completion, output registration, Frame Presentation Ticket consumption, and
//! video preroll cannot acquire separate test-only meanings.

use std::time::Instant;

use super::headless_viewer_gpu::{
    HeadlessGpuCompletionDeadline, HeadlessViewerGpuAdapter, HeadlessViewerGpuCompletionPoll,
    HeadlessViewerGpuError, HeadlessViewerGpuExecution, HeadlessViewerGpuOutput,
};
use super::playback_preview::{observe_playback_video_preroll, pump_playback_preview};
use super::preview_execution::{PreviewGpuFrameState, PreviewOutputKey};
use super::preview_raster_frame::PreviewRasterFrame;
use super::preview_runtime::{
    PreviewPresentationContent, PreviewPresentationState, PreviewProductionRuntime,
    PreviewVisualGpuCompletionDisposition,
};
use super::preview_unavailability::PreviewUnavailability;
use super::{
    AppState, FramePresentationDisposition, FramePresentationPreflight,
    FramePresentationPublication,
};
use anyhow::{ensure, Context};

/// Production Preview Runtime specialized for the no-Surface GPU Adapter.
pub(crate) type HeadlessPreviewRuntime = PreviewProductionRuntime<HeadlessViewerGpuOutput>;

fn headless_gpu_output_is_exact_current(
    preview: &HeadlessPreviewRuntime,
    gpu: &HeadlessViewerGpuAdapter,
    output_key: &PreviewOutputKey,
) -> bool {
    preview.has_gpu_output_artifact(output_key, |registered| {
        gpu.has_current_physical_output_artifact(output_key, registered)
    })
}

fn clear_mismatched_headless_gpu_output(
    preview: &HeadlessPreviewRuntime,
    gpu: &HeadlessViewerGpuAdapter,
    output_key: &PreviewOutputKey,
) -> bool {
    preview.clear_external_viewer_frame_for_artifact(output_key, |registered| {
        !gpu.has_current_physical_output_artifact(output_key, registered)
    })
}

fn clear_revoked_headless_gpu_output(
    preview: &HeadlessPreviewRuntime,
    output_key: &PreviewOutputKey,
    revoked_output: &HeadlessViewerGpuOutput,
) -> bool {
    preview.clear_external_viewer_frame_for_artifact(output_key, |registered| {
        registered.resource_key == revoked_output.resource_key
    })
}

/// Usable output made current by one bounded Headless presentation attempt.
#[derive(Debug)]
pub(crate) enum HeadlessPresentedOutput {
    /// A GPU output reached exact callback completion.
    Gpu {
        execution: Box<HeadlessViewerGpuExecution>,
    },
    /// Ordinary queue ordering authorized publication before exact callback
    /// completion. The Adapter retains exact submission correlation.
    QueuedGpu,
    /// The Preview Runtime already retained the exact current GPU output.
    CurrentGpu,
    /// The exact current output is the semantic transparent canvas.
    Transparent,
    /// A validated final CPU raster made usable by the Headless consumer.
    Raster(PreviewRasterFrame),
}

/// Result of one bounded Headless presentation attempt.
#[derive(Debug)]
pub(crate) enum HeadlessPreviewCandidate {
    /// An exact current output is now usable.
    Ready {
        /// Concrete output path used for this presentation.
        output: HeadlessPresentedOutput,
        /// Exact Playback demand consumed by this publication, when any.
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    },
    /// Required Preview work remains pending.
    Loading,
    /// GPU work completed without satisfying the current consumer intent.
    /// The artifact can be released or terminal, or may have been published
    /// earlier under queue ordering and become stale before callback evidence
    /// arrived. The execution remains evidence but requires reconciliation.
    CompletedGpu {
        /// Concrete completed Adapter execution.
        execution: Box<HeadlessViewerGpuExecution>,
        /// Why the completed artifact was not published.
        disposition: HeadlessCompletedGpuDisposition,
    },
    /// The GPU Adapter could not accept more work in this bounded turn.
    Backpressured,
    /// The exact Playback demand was consumed as Late, so the completed
    /// artifact must not become current.
    DroppedLate,
    /// The current intent cannot produce a valid output.
    Unavailable(PreviewUnavailability),
}

/// Publication result for completed Headless GPU work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadlessCompletedGpuDisposition {
    /// Queue ordering published the ordinary artifact earlier; callback
    /// completion supplies physical execution evidence without rewriting that
    /// historical publication classification.
    PublishedCurrent {
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    },
    /// The visual lifecycle no longer permitted this artifact to publish.
    Released,
    /// The artifact could not publish because the exact demand accepted this
    /// terminal delivery. The concrete kind is preserved rather than
    /// reinterpreted by the validation Adapter.
    TerminalDelivery(mondrian_playback::FrameDeliveryKind),
}

/// Execute one current Preview candidate through the real Headless GPU Adapter.
///
/// A successful demand completion is proven by consuming the exact pending
/// identity as a presentable Ready or explicitly allowed Degraded delivery.
/// `gpu_completion_deadline` bounds only proof of submitted GPU completion.
/// Frame timeliness remains on the carried presentation ticket.
pub(crate) fn present_headless_preview_candidate(
    preview: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu: &mut HeadlessViewerGpuAdapter,
    gpu_completion_deadline: HeadlessGpuCompletionDeadline,
) -> anyhow::Result<HeadlessPreviewCandidate> {
    if let Some(candidate) = drive_headless_gpu_submission(preview, state, gpu)? {
        return Ok(candidate);
    }
    // A presentable publication consumes its exact Frame Demand before the
    // GPU callback and later validation probes necessarily finish. Observing
    // the same semantic + physical artifact is not another publication and
    // must not require a replacement ticket. Generation rotation clears this
    // proof, so a seek or other changed intent cannot enter this branch.
    if state.pending_playback_frame_demand_identity().is_none()
        && let Some(output_key) = preview.registered_exact_current_gpu_output_key()
    {
        if headless_gpu_output_is_exact_current(preview, gpu, &output_key) {
            return Ok(HeadlessPreviewCandidate::Ready {
                output: HeadlessPresentedOutput::CurrentGpu,
                completed_demand: None,
            });
        }
        clear_mismatched_headless_gpu_output(preview, gpu, &output_key);
    }
    match state.preflight_pending_frame_presentation(Instant::now()) {
        FramePresentationPreflight::MaySubmit => {}
        FramePresentationPreflight::DroppedLate(_) => {
            return Ok(HeadlessPreviewCandidate::DroppedLate);
        }
        FramePresentationPreflight::LostAuthority => {
            return Ok(HeadlessPreviewCandidate::Loading);
        }
    }
    // Headless validation is a production consumer, not a policy bypass.
    // It has no Window-owned thumbnail/waveform demand, but it must still
    // publish current transport demand and apply the same Preview trim
    // decision before asking the Runtime for work.
    let resource_decision = state.refresh_execution_resource_decision(Default::default());
    preview.apply_resource_decision(&resource_decision.preview);
    gpu.apply_resource_decision(&resource_decision.preview.viewer_gpu)?;
    match preview.gpu_preview_frame(state.preview_frame_execution_request(Instant::now())) {
        PreviewGpuFrameState::Ready(frame) => {
            match state.preflight_frame_presentation(frame.presentation_ticket(), Instant::now()) {
                FramePresentationPreflight::MaySubmit => {}
                FramePresentationPreflight::DroppedLate(_) => {
                    return Ok(HeadlessPreviewCandidate::DroppedLate);
                }
                FramePresentationPreflight::LostAuthority => {
                    return Ok(HeadlessPreviewCandidate::Loading);
                }
            }
            let submitted = match gpu.submit(*frame, gpu_completion_deadline) {
                Ok(submitted) => submitted,
                Err(HeadlessViewerGpuError::Backpressure(_)) => {
                    return Ok(HeadlessPreviewCandidate::Backpressured);
                }
                Err(error) => return Err(error).context("submit Headless Viewer GPU candidate"),
            };
            if submitted.heterogeneous {
                return Ok(HeadlessPreviewCandidate::Loading);
            }
            let submitted_output_key = gpu
                .output_key(submitted.submission_id)
                .context("ordinary Headless submission omitted its exact output key")?;
            let presentation =
                gpu.publish_ordinary_submission(submitted.submission_id, |frame, output| {
                    // Prepare the pointer-only clone before entering the
                    // authoritative commit seam. The commit itself only moves
                    // the payload into the coordinator slot.
                    let prepared_output = output.clone();
                    let output_key = frame.output_key.clone();
                    state.finalize_frame_presentation(
                        frame.presentation_ticket(),
                        FramePresentationPublication::prepared(move || {
                            preview.register_gpu_output(output_key, prepared_output);
                        }),
                    )
                })?;
            match presentation {
                FramePresentationDisposition::Presented(_)
                | FramePresentationDisposition::NoDemand => {
                    ensure!(
                        headless_gpu_output_is_exact_current(
                            preview,
                            gpu,
                            &submitted_output_key,
                        ),
                        "ordinary Headless publication committed semantic metadata without its physical output lease"
                    );
                    let completed_demand = match presentation {
                        FramePresentationDisposition::Presented(completion) => {
                            Some(completion.delivery().identity())
                        }
                        FramePresentationDisposition::NoDemand => None,
                        _ => unreachable!("matched presentable disposition"),
                    };
                    preview.try_release_settled_transport_media_residency();
                    observe_playback_video_preroll(state, preview);
                    Ok(HeadlessPreviewCandidate::Ready {
                        output: HeadlessPresentedOutput::QueuedGpu,
                        completed_demand,
                    })
                }
                FramePresentationDisposition::DroppedLate(_) => {
                    quarantine_headless_submission(
                        preview,
                        state,
                        gpu,
                        submitted.submission_id,
                        "ordinary queue-ordered publication missed its presentation deadline",
                    );
                    Ok(HeadlessPreviewCandidate::DroppedLate)
                }
                FramePresentationDisposition::OutputRejected
                | FramePresentationDisposition::LostAuthority => {
                    quarantine_headless_submission(
                        preview,
                        state,
                        gpu,
                        submitted.submission_id,
                        "ordinary queue-ordered publication lost semantic authority",
                    );
                    Ok(HeadlessPreviewCandidate::Loading)
                }
            }
        }
        PreviewGpuFrameState::Current(candidate) => {
            let output_key = preview
                .registered_gpu_output_key()
                .context("exact-current Headless GPU candidate omitted its output key")?;
            if !headless_gpu_output_is_exact_current(preview, gpu, &output_key) {
                clear_mismatched_headless_gpu_output(preview, gpu, &output_key);
                return Ok(HeadlessPreviewCandidate::Loading);
            }
            let presentation = state.finalize_frame_presentation(
                candidate.presentation_ticket(),
                FramePresentationPublication::prepared(|| {}),
            );
            let Some(completed_demand) =
                headless_published_demand_completion(preview, presentation)?
            else {
                return Ok(HeadlessPreviewCandidate::DroppedLate);
            };
            observe_playback_video_preroll(state, preview);
            Ok(HeadlessPreviewCandidate::Ready {
                output: HeadlessPresentedOutput::CurrentGpu,
                completed_demand,
            })
        }
        PreviewGpuFrameState::Transparent(candidate) => {
            let presentation = state.finalize_frame_presentation(
                candidate.presentation_ticket(),
                FramePresentationPublication::prepared(|| {
                    gpu.clear_current_physical_output();
                    preview.clear_external_viewer_frame();
                }),
            );
            let Some(completed_demand) =
                headless_published_demand_completion(preview, presentation)?
            else {
                return Ok(HeadlessPreviewCandidate::DroppedLate);
            };
            observe_playback_video_preroll(state, preview);
            Ok(HeadlessPreviewCandidate::Ready {
                output: HeadlessPresentedOutput::Transparent,
                completed_demand,
            })
        }
        PreviewGpuFrameState::Loading => Ok(HeadlessPreviewCandidate::Loading),
        PreviewGpuFrameState::Unavailable(reason) => {
            Ok(HeadlessPreviewCandidate::Unavailable(reason))
        }
    }
}

fn drive_headless_gpu_submission(
    preview: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu: &mut HeadlessViewerGpuAdapter,
) -> anyhow::Result<Option<HeadlessPreviewCandidate>> {
    let now = Instant::now();
    let mut completion = gpu.poll_completion(now);
    if let Some(terminal) = gpu.device_generation_terminal() {
        let failure_context = terminal.submission_id.map_or_else(
            || "outside an active Viewer submission".to_owned(),
            |submission_id| format!("after submission attempt {}", submission_id.get()),
        );
        let (active_submission, revoked_current_output) =
            gpu.enter_device_generation_retirement(format!(
                "device generation terminal {failure_context}: {}",
                terminal.reason
            ));
        if let Some((output_key, output)) = revoked_current_output.as_ref() {
            clear_revoked_headless_gpu_output(preview, output_key, output);
        }
        match &mut completion {
            HeadlessViewerGpuCompletionPoll::Completed(completed) => {
                if let Some(revoked_output) = completed.revoked_current_physical_output.as_ref() {
                    clear_revoked_headless_gpu_output(
                        preview,
                        &completed.frame.output_key,
                        revoked_output,
                    );
                }
                fail_headless_heterogeneous_candidate(preview, state, &mut completed.frame);
            }
            HeadlessViewerGpuCompletionPoll::QuarantineStarted {
                quarantine,
                revoked_current_physical_output,
            } => {
                if let (Some(output_key), Some(revoked_output)) = (
                    gpu.output_key(quarantine.submission_id),
                    revoked_current_physical_output.as_ref(),
                ) {
                    clear_revoked_headless_gpu_output(preview, &output_key, revoked_output);
                }
            }
            HeadlessViewerGpuCompletionPoll::Idle
            | HeadlessViewerGpuCompletionPoll::Pending { .. } => {}
        }
        if let Some(submission_id) = active_submission
            && let Some(execution) = gpu.take_heterogeneous_terminal(submission_id)
        {
            observe_headless_visual_disposition(
                state,
                preview.fail_heterogeneous_gpu_execution(execution),
            );
        }
        return Err(HeadlessViewerGpuError::DeviceGenerationTerminal(terminal).into());
    }
    match completion {
        HeadlessViewerGpuCompletionPoll::Idle => Ok(None),
        HeadlessViewerGpuCompletionPoll::Pending { submission_id, quarantined } => {
            if quarantined {
                return Ok(Some(HeadlessPreviewCandidate::Loading));
            }
            if gpu.has_queued_publication(submission_id) {
                let output_key = gpu
                    .output_key(submission_id)
                    .filter(|key| headless_gpu_output_is_exact_current(preview, gpu, key));
                let exact_intent = gpu.matches_consumer_intent(
                    submission_id,
                    state.current_frame(),
                    state.pending_playback_frame_demand_identity(),
                );
                let candidate = if output_key.filter(|_| exact_intent).is_some() {
                    HeadlessPreviewCandidate::Ready {
                        output: HeadlessPresentedOutput::CurrentGpu,
                        completed_demand: None,
                    }
                } else {
                    HeadlessPreviewCandidate::Loading
                };
                return Ok(Some(candidate));
            }
            match state.preflight_frame_presentation(gpu.presentation_ticket(submission_id), now) {
                FramePresentationPreflight::MaySubmit => {
                    Ok(Some(HeadlessPreviewCandidate::Loading))
                }
                FramePresentationPreflight::DroppedLate(_) => {
                    quarantine_headless_submission(
                        preview,
                        state,
                        gpu,
                        submission_id,
                        "heterogeneous Viewer completion missed its presentation deadline",
                    );
                    Ok(Some(HeadlessPreviewCandidate::DroppedLate))
                }
                FramePresentationPreflight::LostAuthority => {
                    quarantine_headless_submission(
                        preview,
                        state,
                        gpu,
                        submission_id,
                        "heterogeneous Viewer candidate lost presentation authority",
                    );
                    Ok(Some(HeadlessPreviewCandidate::Loading))
                }
            }
        }
        HeadlessViewerGpuCompletionPoll::QuarantineStarted {
            quarantine,
            revoked_current_physical_output,
        } => {
            if let Some(revoked_output) = revoked_current_physical_output.as_ref()
                && let Some(key) = gpu.output_key(quarantine.submission_id)
            {
                clear_revoked_headless_gpu_output(preview, &key, revoked_output);
            }
            if let Some(terminal) = gpu.take_heterogeneous_terminal(quarantine.submission_id) {
                observe_headless_visual_disposition(
                    state,
                    preview.fail_heterogeneous_gpu_execution(terminal),
                );
            }
            Ok(Some(HeadlessPreviewCandidate::Loading))
        }
        HeadlessViewerGpuCompletionPoll::Completed(mut completed) => {
            if let Some(error) = completed.completion_error.take() {
                let revoked_current_physical_output = gpu
                    .clear_current_physical_output_for_submission(completed.submission_id)
                    .or_else(|| completed.revoked_current_physical_output.take());
                if let Some(revoked_output) = revoked_current_physical_output.as_ref() {
                    clear_revoked_headless_gpu_output(
                        preview,
                        &completed.frame.output_key,
                        revoked_output,
                    );
                }
                fail_headless_heterogeneous_candidate(preview, state, &mut completed.frame);
                anyhow::bail!(
                    "Headless Viewer GPU submission {} completion cleanup failed: {error}",
                    completed.execution.submission_id
                );
            }
            ensure!(
                completed.execution.gpu_completion_observed,
                "Headless Viewer GPU completion omitted exact callback evidence"
            );
            if completed.quarantine_reason.is_some() {
                // Queue-ordered ordinary publication may already have made
                // this exact output current. A callback observed at/after the
                // safety deadline is retirement-only and must revoke that
                // semantic pointer before the owner is released.
                if let Some(revoked_output) = completed.revoked_current_physical_output.as_ref() {
                    clear_revoked_headless_gpu_output(
                        preview,
                        &completed.frame.output_key,
                        revoked_output,
                    );
                }
                fail_headless_heterogeneous_candidate(preview, state, &mut completed.frame);
                return Ok(Some(HeadlessPreviewCandidate::CompletedGpu {
                    execution: Box::new(completed.execution),
                    disposition: HeadlessCompletedGpuDisposition::Released,
                }));
            }
            if let Some(publication) = completed.queued_publication {
                ensure!(
                    completed.heterogeneous_completion.is_none()
                        && !completed.frame.has_heterogeneous_gpu_execution(),
                    "ordinary Headless publication retained heterogeneous terminal evidence"
                );
                let candidate = match publication {
                    FramePresentationDisposition::Presented(_)
                    | FramePresentationDisposition::NoDemand => {
                        let remains_current = headless_gpu_output_is_exact_current(
                            preview,
                            gpu,
                            &completed.frame.output_key,
                        ) && completed.frame.frame == state.current_frame()
                            && state.pending_playback_frame_demand_identity().is_none_or(
                                |pending| {
                                    completed
                                        .frame
                                        .presentation_ticket()
                                        .is_some_and(|ticket| ticket.identity() == pending)
                                },
                            );
                        if remains_current {
                            let completed_demand = match publication {
                                FramePresentationDisposition::Presented(completion) => {
                                    Some(completion.delivery().identity())
                                }
                                FramePresentationDisposition::NoDemand => None,
                                _ => unreachable!("matched published disposition"),
                            };
                            HeadlessPreviewCandidate::Ready {
                                output: HeadlessPresentedOutput::Gpu {
                                    execution: Box::new(completed.execution),
                                },
                                completed_demand,
                            }
                        } else {
                            let completed_demand = match publication {
                                FramePresentationDisposition::Presented(completion) => {
                                    Some(completion.delivery().identity())
                                }
                                FramePresentationDisposition::NoDemand => None,
                                _ => unreachable!("matched published disposition"),
                            };
                            HeadlessPreviewCandidate::CompletedGpu {
                                execution: Box::new(completed.execution),
                                disposition: HeadlessCompletedGpuDisposition::PublishedCurrent {
                                    completed_demand,
                                },
                            }
                        }
                    }
                    FramePresentationDisposition::DroppedLate(completion) => {
                        HeadlessPreviewCandidate::CompletedGpu {
                            execution: Box::new(completed.execution),
                            disposition: HeadlessCompletedGpuDisposition::TerminalDelivery(
                                completion.delivery().kind(),
                            ),
                        }
                    }
                    FramePresentationDisposition::OutputRejected
                    | FramePresentationDisposition::LostAuthority => {
                        HeadlessPreviewCandidate::CompletedGpu {
                            execution: Box::new(completed.execution),
                            disposition: HeadlessCompletedGpuDisposition::Released,
                        }
                    }
                };
                return Ok(Some(candidate));
            }

            let Some(terminal) = completed.frame.take_heterogeneous_gpu_execution() else {
                anyhow::bail!(
                    "Headless Viewer GPU submission {} completed without publication or heterogeneous terminal authority",
                    completed.execution.submission_id
                );
            };
            let Some(gpu_completion) = completed.heterogeneous_completion.as_ref() else {
                observe_headless_visual_disposition(
                    state,
                    preview.fail_heterogeneous_gpu_execution(terminal),
                );
                anyhow::bail!(
                    "Headless heterogeneous Viewer submission {} omitted GPU completion evidence",
                    completed.execution.submission_id
                );
            };
            let disposition = match preview
                .finalize_heterogeneous_gpu_completion(terminal, gpu_completion)
            {
                Ok(disposition) => disposition,
                Err(error) => {
                    let _ = pump_playback_preview(state, preview);
                    return Err(error).context("finalize Headless heterogeneous Viewer execution");
                }
            };
            match disposition {
                PreviewVisualGpuCompletionDisposition::PublishCurrent => {
                    let presentation =
                        gpu.publish_completed_heterogeneous(&mut completed, |frame, output| {
                            let prepared_output = output.clone();
                            let output_key = frame.output_key.clone();
                            state.finalize_frame_presentation(
                                frame.presentation_ticket(),
                                FramePresentationPublication::prepared(move || {
                                    preview.register_gpu_output(output_key, prepared_output);
                                }),
                            )
                        })?;
                    let Some(completed_demand) =
                        headless_published_demand_completion(preview, presentation)?
                    else {
                        return Ok(Some(HeadlessPreviewCandidate::CompletedGpu {
                            execution: Box::new(completed.execution),
                            disposition: HeadlessCompletedGpuDisposition::TerminalDelivery(
                                mondrian_playback::FrameDeliveryKind::Late,
                            ),
                        }));
                    };
                    ensure!(
                        headless_gpu_output_is_exact_current(
                            preview,
                            gpu,
                            &completed.frame.output_key,
                        ),
                        "heterogeneous Headless publication committed semantic metadata without its physical output lease"
                    );
                    observe_playback_video_preroll(state, preview);
                    Ok(Some(HeadlessPreviewCandidate::Ready {
                        output: HeadlessPresentedOutput::Gpu {
                            execution: Box::new(completed.execution),
                        },
                        completed_demand,
                    }))
                }
                PreviewVisualGpuCompletionDisposition::Release => {
                    Ok(Some(HeadlessPreviewCandidate::CompletedGpu {
                        execution: Box::new(completed.execution),
                        disposition: HeadlessCompletedGpuDisposition::Released,
                    }))
                }
                PreviewVisualGpuCompletionDisposition::TerminalCandidate(candidate) => {
                    let delivery_kind = candidate.kind();
                    observe_headless_visual_disposition(
                        state,
                        PreviewVisualGpuCompletionDisposition::TerminalCandidate(candidate),
                    );
                    Ok(Some(HeadlessPreviewCandidate::CompletedGpu {
                        execution: Box::new(completed.execution),
                        disposition: HeadlessCompletedGpuDisposition::TerminalDelivery(
                            delivery_kind,
                        ),
                    }))
                }
            }
        }
    }
}

fn quarantine_headless_submission(
    preview: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu: &mut HeadlessViewerGpuAdapter,
    submission_id: super::viewer_gpu_submission::ViewerGpuSubmissionId,
    reason: &'static str,
) {
    let output_key = gpu.output_key(submission_id);
    let (_, revoked_current_physical_output) =
        gpu.quarantine_after_authority_revocation(submission_id, reason);
    if let Some(revoked_output) = revoked_current_physical_output.as_ref()
        && let Some(key) = output_key.as_ref()
    {
        clear_revoked_headless_gpu_output(preview, key, revoked_output);
    }
    if let Some(terminal) = gpu.take_heterogeneous_terminal(submission_id) {
        observe_headless_visual_disposition(
            state,
            preview.fail_heterogeneous_gpu_execution(terminal),
        );
    }
}

fn fail_headless_heterogeneous_candidate(
    preview: &HeadlessPreviewRuntime,
    state: &mut AppState,
    frame: &mut super::preview_execution::PreviewGpuFrame,
) {
    if let Some(execution) = frame.take_heterogeneous_gpu_execution() {
        observe_headless_visual_disposition(
            state,
            preview.fail_heterogeneous_gpu_execution(execution),
        );
    }
}

fn observe_headless_visual_disposition(
    state: &mut AppState,
    disposition: PreviewVisualGpuCompletionDisposition,
) {
    if let PreviewVisualGpuCompletionDisposition::TerminalCandidate(candidate) = disposition
        && state.pending_playback_frame_demand_identity() == Some(candidate.identity())
    {
        state.observe_frame_delivery_candidate(candidate, Instant::now());
    }
}

/// Arbitrate the complete production Headless presentation path.
///
/// GPU execution is attempted first. An explicit GPU blocker then falls
/// through to the same final CPU Raster path used by the Window Preview
/// Adapter; Loading and terminal unavailability retain their exact meanings.
/// The caller's one GPU-completion safety deadline is shared by both
/// arbitration steps; presentation timeliness remains independently ticketed.
pub(crate) fn present_headless_preview_output(
    preview: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu: &mut HeadlessViewerGpuAdapter,
    gpu_completion_deadline: HeadlessGpuCompletionDeadline,
) -> anyhow::Result<HeadlessPreviewCandidate> {
    match present_headless_preview_candidate(preview, state, gpu, gpu_completion_deadline)? {
        HeadlessPreviewCandidate::Unavailable(_) => {
            match preview.presentation(state.preview_frame_execution_request(Instant::now())) {
                PreviewPresentationState::Ready(candidate) => {
                    let ticket = candidate.presentation_ticket();
                    match candidate.into_value() {
                        PreviewPresentationContent::Raster(frame) => {
                            let presentation = state.finalize_frame_presentation(
                                ticket,
                                FramePresentationPublication::prepared(|| {
                                    gpu.clear_current_physical_output();
                                    preview.clear_external_viewer_frame();
                                }),
                            );
                            let Some(completed_demand) =
                                headless_published_demand_completion(preview, presentation)?
                            else {
                                return Ok(HeadlessPreviewCandidate::DroppedLate);
                            };
                            observe_playback_video_preroll(state, preview);
                            Ok(HeadlessPreviewCandidate::Ready {
                                output: HeadlessPresentedOutput::Raster(frame),
                                completed_demand,
                            })
                        }
                        PreviewPresentationContent::Gpu(_) => {
                            let output_key = preview.registered_gpu_output_key().context(
                                "presented Headless GPU output omitted its exact output key",
                            )?;
                            if !headless_gpu_output_is_exact_current(preview, gpu, &output_key) {
                                clear_mismatched_headless_gpu_output(preview, gpu, &output_key);
                                return Ok(HeadlessPreviewCandidate::Loading);
                            }
                            let presentation = state.finalize_frame_presentation(
                                ticket,
                                FramePresentationPublication::prepared(|| {}),
                            );
                            let Some(completed_demand) =
                                headless_published_demand_completion(preview, presentation)?
                            else {
                                return Ok(HeadlessPreviewCandidate::DroppedLate);
                            };
                            observe_playback_video_preroll(state, preview);
                            Ok(HeadlessPreviewCandidate::Ready {
                                output: HeadlessPresentedOutput::CurrentGpu,
                                completed_demand,
                            })
                        }
                    }
                }
                PreviewPresentationState::Transparent(candidate) => {
                    let presentation = state.finalize_frame_presentation(
                        candidate.presentation_ticket(),
                        FramePresentationPublication::prepared(|| {
                            gpu.clear_current_physical_output();
                            preview.clear_external_viewer_frame();
                        }),
                    );
                    let Some(completed_demand) =
                        headless_published_demand_completion(preview, presentation)?
                    else {
                        return Ok(HeadlessPreviewCandidate::DroppedLate);
                    };
                    observe_playback_video_preroll(state, preview);
                    Ok(HeadlessPreviewCandidate::Ready {
                        output: HeadlessPresentedOutput::Transparent,
                        completed_demand,
                    })
                }
                PreviewPresentationState::Loading | PreviewPresentationState::Stale(_) => {
                    Ok(HeadlessPreviewCandidate::Loading)
                }
                PreviewPresentationState::Unavailable(reason) => {
                    Ok(HeadlessPreviewCandidate::Unavailable(reason))
                }
            }
        }
        candidate => Ok(candidate),
    }
}

fn headless_published_demand_completion(
    preview: &HeadlessPreviewRuntime,
    disposition: FramePresentationDisposition,
) -> anyhow::Result<Option<Option<mondrian_playback::FrameDemandIdentity>>> {
    match disposition {
        FramePresentationDisposition::Presented(completion) => {
            preview.try_release_settled_transport_media_residency();
            Ok(Some(Some(completion.delivery().identity())))
        }
        FramePresentationDisposition::NoDemand => {
            preview.try_release_settled_transport_media_residency();
            Ok(Some(None))
        }
        FramePresentationDisposition::DroppedLate(_) => Ok(None),
        FramePresentationDisposition::OutputRejected => {
            anyhow::bail!("Headless output registration rejected a presentable Frame Demand")
        }
        FramePresentationDisposition::LostAuthority => {
            anyhow::bail!("Headless presentation lost its exact Frame Demand authority")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    use mondrian_playback::{FrameDeliveryKind, FramePresentationQuality};

    #[test]
    fn explicitly_degraded_output_completes_exact_headless_demand() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let identity =
            state.pending_playback_frame_demand_identity().expect("current frame demand");
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Degraded)
            .expect("degraded presentation ticket");

        assert!(matches!(
            state.finalize_frame_presentation(
                Some(ticket),
                FramePresentationPublication::prepared(|| {})
            ),
            FramePresentationDisposition::Presented(completion)
                if completion.delivery().kind() == FrameDeliveryKind::Degraded
        ));
        assert_ne!(
            state.pending_playback_frame_demand_identity(),
            Some(identity)
        );
        let evidence = state.playback_evidence_report();
        assert_eq!(evidence.deliveries.ready, 0);
        assert_eq!(evidence.deliveries.degraded, 1);
        assert_eq!(evidence.deliveries.late, 0);
        assert!(evidence.events.iter().any(|event| {
            matches!(
                event.kind,
                mondrian_playback::PlaybackEvidenceEventKind::Delivery {
                    kind: FrameDeliveryKind::Degraded,
                    accepted: true,
                    ..
                }
            )
        }));
    }

    #[test]
    fn late_output_consumes_its_demand_without_running_publication() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let identity =
            state.pending_playback_frame_demand_identity().expect("current frame demand");
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("timed presentation ticket");
        assert!(ticket.deadline().is_some());
        let published = Cell::new(false);
        state.playback_observation_instant_anchor = Instant::now() - Duration::from_secs(10);

        let disposition = state.finalize_frame_presentation(
            Some(ticket),
            FramePresentationPublication::prepared(|| published.set(true)),
        );

        assert!(matches!(
            disposition,
            FramePresentationDisposition::DroppedLate(completion)
                if completion.delivery().kind() == FrameDeliveryKind::Late
        ));
        assert!(
            !published.get(),
            "Late output must not run its publication callback"
        );
        assert_ne!(
            state.pending_playback_frame_demand_identity(),
            Some(identity)
        );
        let evidence = state.playback_evidence_report();
        assert_eq!(evidence.deliveries.late, 1);
        assert_eq!(evidence.deliveries.rejected, 0);
    }

    #[test]
    fn rejected_output_does_not_consume_a_presentable_demand() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let identity =
            state.pending_playback_frame_demand_identity().expect("current frame demand");
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("presentation ticket");

        assert_eq!(
            state.finalize_frame_presentation(
                Some(ticket),
                FramePresentationPublication::rejected()
            ),
            FramePresentationDisposition::OutputRejected
        );
        assert_eq!(
            state.pending_playback_frame_demand_identity(),
            Some(identity)
        );
    }
}
