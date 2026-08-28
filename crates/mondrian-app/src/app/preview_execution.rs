//! UI-independent lifecycle coordination for production preview execution.
//!
//! Media decoding, renderer recording, and Window texture registration are
//! concrete Adapters. This deep Module owns the state that must remain coherent
//! across those Adapters: generation binding, pending state, executed quality,
//! candidate identity, and the exact currently registered output.

use super::preview_unavailability::PreviewUnavailability;
use mondrian_core::types::SequenceId;
use mondrian_core::{ExecutionCancellationToken, WorkingColorSpace};
use mondrian_media::{DecodedVideoSurfaceFormat, PreviewDecodeExecutionPath};
use mondrian_playback::{FramePresentationQuality, PlaybackEpoch};
use mondrian_renderer::{
    HeterogeneousGpuContinuationBinding, RenderMonitorAdaptation, RenderOutputColorBoundary,
    ViewerGpuExecutionLayer, ViewerHeterogeneousGpuCompletedBatch, ViewerHeterogeneousGpuInput,
};
use sha2::{Digest, Sha256};
use std::hash::{Hash, Hasher};

/// Maximum number of CPU-complete speculative Viewer frames retained without
/// consuming GPU submission or presentation-output capacity.
///
/// Three lookahead coordinates cover a bounded 50 ms scheduler displacement
/// at 60 fps; the fourth slot lets the exact immediate successor wait for GPU
/// admission without evicting that horizon.
pub(crate) const PREVIEW_GPU_CPU_STAGING_CAPACITY: usize = 4;

/// Strong process-local semantic identity used by Preview cache and
/// presentation-registration keys.
///
/// Construction is domain-separated and hashes every canonical field with
/// SHA-256. The complete 32-byte value is equality authority; a compact
/// `Hasher::finish()` projection is diagnostic only.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PreviewSemanticIdentity([u8; 32]);

impl std::fmt::Debug for PreviewSemanticIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::fmt::Display for PreviewSemanticIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl PreviewSemanticIdentity {
    /// Construct an identity from an already domain-separated complete
    /// semantic fingerprint.
    pub(crate) const fn from_complete_fingerprint(fingerprint: [u8; 32]) -> Self {
        Self(fingerprint)
    }

    /// Complete authoritative fingerprint.
    pub(crate) const fn semantic_fingerprint(self) -> [u8; 32] {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn from_test_fingerprint(fingerprint: [u8; 32]) -> Self {
        Self::from_complete_fingerprint(fingerprint)
    }

    #[cfg(test)]
    pub(crate) const fn compact_diagnostic_hash(self) -> u64 {
        u64::from_le_bytes([
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7],
        ])
    }
}

/// Canonical field writer shared by Preview media and Viewer-plan identities.
pub(crate) struct PreviewSemanticIdentityBuilder {
    hasher: Sha256,
}

impl PreviewSemanticIdentityBuilder {
    /// Start a domain-separated identity.
    pub(crate) fn new(domain: &'static [u8]) -> Self {
        let mut builder = Self { hasher: Sha256::new() };
        builder.write(domain);
        builder
    }

    /// Finish the complete authoritative identity.
    pub(crate) fn finish_identity(self) -> PreviewSemanticIdentity {
        PreviewSemanticIdentity(self.hasher.finalize().into())
    }
}

impl Hasher for PreviewSemanticIdentityBuilder {
    fn finish(&self) -> u64 {
        let fingerprint: [u8; 32] = self.hasher.clone().finalize().into();
        u64::from_le_bytes([
            fingerprint[0],
            fingerprint[1],
            fingerprint[2],
            fingerprint[3],
            fingerprint[4],
            fingerprint[5],
            fingerprint[6],
            fingerprint[7],
        ])
    }

    fn write(&mut self, bytes: &[u8]) {
        self.hasher.update((bytes.len() as u64).to_le_bytes());
        self.hasher.update(bytes);
    }
}

/// Complete identity of one resolved Viewer output.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PreviewOutputKey {
    pub(crate) sequence_id: SequenceId,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) plan_identity: PreviewSemanticIdentity,
}

/// Exact running-transport coordinate proved by one Viewer execution turn.
///
/// This identity is deliberately separate from [`PreviewOutputKey`]. Adjacent
/// Timeline frames may resolve to the same pixels, but proving that reusable
/// output for frame N does not by itself prove that frame N+1 was evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewPlaybackIntent {
    pub(crate) epoch: PlaybackEpoch,
    pub(crate) quality_revision: u64,
    pub(crate) frame: i64,
}

/// Semantic output promoted from an exact prepared transport coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreviewPreparedPromotion<K> {
    /// Reuse or publish this exact GPU output identity.
    Gpu {
        /// Complete semantic output identity.
        key: K,
        /// Whether the same physical artifact was already current before the
        /// transport crossed into this coordinate.
        already_visible: bool,
    },
    /// The coordinate was evaluated as the transparent canvas.
    Transparent,
}

enum PreparedPreviewOutput<K, O> {
    Gpu {
        key: K,
        output: O,
        already_visible: bool,
    },
    Transparent,
}

impl PreviewPlaybackIntent {
    /// Bind the active Playback generation, quality policy and Timeline frame.
    pub(crate) const fn new(epoch: PlaybackEpoch, quality_revision: u64, frame: i64) -> Self {
        Self { epoch, quality_revision, frame }
    }
}

impl std::fmt::Display for PreviewOutputKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}:{}x{}:{}",
            self.sequence_id, self.width, self.height, self.plan_identity
        )
    }
}

impl PreviewOutputKey {
    /// Construct one output identity from its complete resolved-plan identity.
    pub(crate) const fn new(
        sequence_id: SequenceId,
        width: u32,
        height: u32,
        plan_identity: PreviewSemanticIdentity,
    ) -> Self {
        Self { sequence_id, width, height, plan_identity }
    }

    /// Derive the monitor-adapted identity used by final Viewer presentation.
    pub(crate) fn with_monitor_adaptation(&self, adaptation: &RenderMonitorAdaptation) -> Self {
        self.with_monitor_adaptation_opt(Some(adaptation))
    }

    /// Derive the monitor-adapted identity when a proven adaptation exists.
    ///
    /// A missing adaptation leaves the plan identity unchanged, matching the
    /// optional-consumer fallback of best-effort presentation adapters.
    pub(crate) fn with_monitor_adaptation_opt(
        &self,
        adaptation: Option<&RenderMonitorAdaptation>,
    ) -> Self {
        let Some(adaptation) = adaptation else {
            return self.clone();
        };
        let mut builder =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.monitor-output.v1");
        self.plan_identity.hash(&mut builder);
        adaptation.hash(&mut builder);
        Self::new(
            self.sequence_id,
            self.width,
            self.height,
            builder.finish_identity(),
        )
    }

    /// Derive the final Viewer presentation identity for active signal warnings.
    ///
    /// Disabled monitoring preserves the existing identity, while the tap and
    /// complete warning settings distinguish every presentation-only raster.
    pub(crate) fn with_signal_monitoring(
        &self,
        tap: mondrian_core::ProgramScopesTap,
        settings: mondrian_core::SignalMonitoringSettings,
    ) -> Self {
        if !settings.is_active() {
            return self.clone();
        }
        let mut builder =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.signal-monitoring.v1");
        self.plan_identity.hash(&mut builder);
        tap.hash(&mut builder);
        settings.hash(&mut builder);
        Self::new(
            self.sequence_id,
            self.width,
            self.height,
            builder.finish_identity(),
        )
    }

    /// Bind a semantic plan identity to one concrete non-reusable execution.
    pub(crate) fn with_execution_nonce(&self, candidate_id: u64) -> Self {
        let mut builder =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.execution-output.v1");
        self.plan_identity.hash(&mut builder);
        candidate_id.hash(&mut builder);
        Self::new(
            self.sequence_id,
            self.width,
            self.height,
            builder.finish_identity(),
        )
    }
}

/// UI-neutral result of asking for a GPU Viewer execution candidate.
pub(crate) enum PreviewGpuFrameState {
    /// The exact output is already registered by the active presentation Adapter.
    Current(super::preview_runtime::PreviewPresentationCandidate<()>),
    /// The exact ticketless successor is already retained but remains invisible.
    Prepared,
    /// The exact current output is the semantic transparent canvas and needs no texture.
    Transparent(super::preview_runtime::PreviewPresentationCandidate<()>),
    /// A working-space frame is ready for Viewer GPU execution.
    Ready(Box<PreviewGpuFrame>),
    /// Required media is still decoding or rendering.
    Loading,
    /// No Viewer output can be produced for the current intent.
    Unavailable(PreviewUnavailability),
}

/// Working-space input consumed by either Window or Headless Viewer GPU execution.
pub(crate) enum PreviewGpuWorkingInput {
    /// Layer stack to composite directly before the output transform.
    GpuComposite {
        layers: Vec<ViewerGpuExecutionLayer>,
    },
}

/// UI-independent Viewer GPU execution contract for one resolved frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewGpuFramePurpose {
    /// Exact visible output carrying current presentation authority.
    Current,
    /// Ticketless immediate successor retained for later atomic promotion.
    SuccessorPreparation,
}

/// UI-independent Viewer GPU execution contract for one resolved frame.
pub(crate) struct PreviewGpuFrame {
    pub(crate) output_key: PreviewOutputKey,
    pub(crate) sequence_id: SequenceId,
    pub(crate) frame: i64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) working_color_space: WorkingColorSpace,
    pub(crate) working_input: PreviewGpuWorkingInput,
    pub(crate) program_output_boundary: RenderOutputColorBoundary,
    pub(crate) monitor_adaptation: RenderMonitorAdaptation,
    candidate_id: u64,
    generation: u64,
    purpose: PreviewGpuFramePurpose,
    playback_intent: PreviewPlaybackIntent,
    presentation_quality: FramePresentationQuality,
    presentation_ticket: Option<mondrian_playback::FramePresentationTicket>,
    heterogeneous_execution: Option<PreviewGpuHeterogeneousExecution>,
    // Intentionally unread: dropping the complete submitted frame releases
    // these demand-scoped Frame Store guards only after exact GPU completion.
    _media_residency_protections: Vec<mondrian_playback::MediaFrameProtectionLease>,
    #[cfg_attr(not(test), allow(dead_code))]
    decode_execution: PreviewDecodeExecutionSummary,
}

impl PreviewGpuFrame {
    /// Construct a closed Viewer GPU execution contract.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        output_key: PreviewOutputKey,
        sequence_id: SequenceId,
        frame: i64,
        width: u32,
        height: u32,
        working_color_space: WorkingColorSpace,
        working_input: PreviewGpuWorkingInput,
        program_output_boundary: RenderOutputColorBoundary,
        monitor_adaptation: RenderMonitorAdaptation,
        candidate_id: u64,
        generation: u64,
        purpose: PreviewGpuFramePurpose,
        playback_intent: PreviewPlaybackIntent,
        presentation_quality: FramePresentationQuality,
        presentation_ticket: Option<mondrian_playback::FramePresentationTicket>,
        decode_execution: PreviewDecodeExecutionSummary,
        heterogeneous_execution: Option<PreviewGpuHeterogeneousExecution>,
        media_residency_protections: Vec<mondrian_playback::MediaFrameProtectionLease>,
    ) -> Self {
        Self {
            output_key,
            sequence_id,
            frame,
            width,
            height,
            working_color_space,
            working_input,
            program_output_boundary,
            monitor_adaptation,
            candidate_id,
            generation,
            purpose,
            playback_intent,
            presentation_quality,
            presentation_ticket,
            heterogeneous_execution,
            _media_residency_protections: media_residency_protections,
            decode_execution,
        }
    }

    /// Stable renderer registration key for this resolved output.
    pub(crate) fn external_texture_key(&self) -> String {
        format!("viewer.gpu:{}", self.output_key)
    }

    /// Candidate identity used to correlate one execution attempt.
    pub(crate) const fn candidate_id(&self) -> u64 {
        self.candidate_id
    }

    /// Preview execution generation that proved this complete CPU candidate.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether this output is ticketless immediate-successor preparation.
    pub(crate) const fn is_successor_preparation(&self) -> bool {
        matches!(self.purpose, PreviewGpuFramePurpose::SuccessorPreparation)
    }

    /// Exact running coordinate evaluated by this candidate.
    pub(crate) const fn playback_intent(&self) -> PreviewPlaybackIntent {
        self.playback_intent
    }

    /// Exact terminal authority completed only after usable presentation.
    pub(crate) const fn presentation_ticket(
        &self,
    ) -> Option<mondrian_playback::FramePresentationTicket> {
        self.presentation_ticket
    }

    /// Convert an exact CPU-complete speculative candidate into the current
    /// presentation candidate under a freshly captured Frame Demand.
    ///
    /// Callers must first prove generation and playback-intent equality. This
    /// transition preserves the frame-local quality result and cannot invent
    /// authority when the current request carries no demand.
    pub(crate) fn bind_current_presentation(
        &mut self,
        presentation_ticket: mondrian_playback::FramePresentationTicket,
    ) {
        debug_assert!(self.is_successor_preparation());
        debug_assert_eq!(
            presentation_ticket.identity().epoch,
            self.playback_intent.epoch
        );
        debug_assert_eq!(
            presentation_ticket.identity().quality_revision,
            self.playback_intent.quality_revision
        );
        debug_assert_eq!(
            presentation_ticket.identity().target_frame,
            self.playback_intent.frame
        );
        self.purpose = PreviewGpuFramePurpose::Current;
        self.presentation_ticket = Some(presentation_ticket);
    }

    /// Frame-local quality used when a speculative candidate later acquires
    /// the exact current presentation authority.
    pub(crate) const fn presentation_quality(&self) -> FramePresentationQuality {
        self.presentation_quality
    }

    /// Move the exact CPU-prefix completions into Viewer command recording.
    ///
    /// The Broker lease remains attached to this frame until recording and
    /// submission succeed. Calling this twice returns an empty input table and
    /// causes renderer validation to fail closed.
    pub(crate) fn take_heterogeneous_gpu_inputs(&mut self) -> Vec<ViewerHeterogeneousGpuInput> {
        self.heterogeneous_execution
            .as_mut()
            .map_or_else(Vec::new, PreviewGpuHeterogeneousExecution::take_inputs)
    }

    /// Whether this candidate owns a Broker lease that must cross actual GPU
    /// completion before publication.
    pub(crate) fn has_heterogeneous_gpu_execution(&self) -> bool {
        self.heterogeneous_execution.is_some()
    }

    /// Move terminal authority into the Adapter's in-flight submission state.
    pub(crate) fn take_heterogeneous_gpu_execution(
        &mut self,
    ) -> Option<PreviewGpuHeterogeneousExecution> {
        self.heterogeneous_execution.take()
    }

    /// Decode provenance for the exact media layers entering this candidate.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn decode_execution(&self) -> PreviewDecodeExecutionSummary {
        self.decode_execution
    }
}

/// Bounded Adapter-owned queue of CPU-complete speculative Viewer frames.
///
/// Entries are keyed by the complete playback intent. No entry is semantically
/// prepared or physically published until an Adapter submits it. Replacing an
/// exact intent and evicting the oldest excess entry both retire resources by
/// ordinary RAII.
pub(crate) struct PreviewGpuFrameStaging {
    frames: Vec<PreviewGpuFrame>,
}

impl Default for PreviewGpuFrameStaging {
    fn default() -> Self {
        Self {
            frames: Vec::with_capacity(PREVIEW_GPU_CPU_STAGING_CAPACITY),
        }
    }
}

impl PreviewGpuFrameStaging {
    /// Retain one CPU-complete ticketless candidate inside the bounded horizon.
    pub(crate) fn stage(&mut self, frame: Box<PreviewGpuFrame>) {
        debug_assert!(frame.is_successor_preparation());
        debug_assert!(frame.presentation_ticket().is_none());
        let intent = frame.playback_intent();
        if let Some(existing) =
            self.frames.iter_mut().find(|existing| existing.playback_intent() == intent)
        {
            *existing = *frame;
            return;
        }
        if self.frames.len() == PREVIEW_GPU_CPU_STAGING_CAPACITY {
            self.frames.remove(0);
        }
        self.frames.push(*frame);
    }

    /// Take only the frame proved for this exact playback intent.
    pub(crate) fn take_exact(
        &mut self,
        intent: PreviewPlaybackIntent,
    ) -> Option<Box<PreviewGpuFrame>> {
        let index = self.frames.iter().position(|frame| frame.playback_intent() == intent)?;
        Some(Box::new(self.frames.remove(index)))
    }

    /// Whether the queue already retains this complete intent.
    pub(crate) fn contains(&self, intent: PreviewPlaybackIntent) -> bool {
        self.frames.iter().any(|frame| frame.playback_intent() == intent)
    }

    /// Retire entries outside the current bounded playback horizon.
    pub(crate) fn retain_only(&mut self, expected: &[PreviewPlaybackIntent]) {
        self.frames.retain(|frame| expected.contains(&frame.playback_intent()));
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.frames.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewGpuHeterogeneousExpectation {
    graph_fingerprint: [u8; 32],
    generation: u64,
    frame_extent: mondrian_effects::EffectFrameExtent,
    frame_seed: i64,
}

/// Move-only terminal authority for a Broker-owned heterogeneous Viewer
/// candidate.
pub(crate) struct PreviewGpuHeterogeneousExecution {
    inputs: Vec<ViewerHeterogeneousGpuInput>,
    expectations: Vec<PreviewGpuHeterogeneousExpectation>,
    lease: Option<super::preview_visual_execution_task::VisualExecutionLease>,
    reusable: bool,
}

impl PreviewGpuHeterogeneousExecution {
    /// Bind exact renderer inputs to the visual Broker lease that produced
    /// their CPU prefixes.
    pub(crate) fn new(
        inputs: Vec<ViewerHeterogeneousGpuInput>,
        lease: super::preview_visual_execution_task::VisualExecutionLease,
        reusable: bool,
    ) -> Result<
        Self,
        (
            PreviewGpuHeterogeneousCompletionError,
            super::preview_visual_execution_task::VisualExecutionLease,
        ),
    > {
        if inputs.is_empty() {
            return Err((
                PreviewGpuHeterogeneousCompletionError::EmptyInputBatch,
                lease,
            ));
        }
        let lease_generation = lease.generation();
        let expectations = inputs
            .iter()
            .map(|input| expectation(input.request.binding()))
            .collect::<Vec<_>>();
        if expectations.iter().any(|expected| expected.generation != lease_generation) {
            return Err((
                PreviewGpuHeterogeneousCompletionError::LeaseGenerationMismatch {
                    lease_generation,
                },
                lease,
            ));
        }
        Ok(Self { inputs, expectations, lease: Some(lease), reusable })
    }

    fn take_inputs(&mut self) -> Vec<ViewerHeterogeneousGpuInput> {
        std::mem::take(&mut self.inputs)
    }

    /// Validate renderer completion evidence without consuming the still-active
    /// Broker lease. No freshness or deadline decision is made here.
    pub(crate) fn validate_completed(
        &self,
        completed: &ViewerHeterogeneousGpuCompletedBatch,
    ) -> Result<(), PreviewGpuHeterogeneousCompletionError> {
        if !self.inputs.is_empty() {
            return Err(PreviewGpuHeterogeneousCompletionError::InputsNotRecorded);
        }
        if completed.len() != self.expectations.len() {
            return Err(
                PreviewGpuHeterogeneousCompletionError::CompletionCountMismatch {
                    expected: self.expectations.len(),
                    actual: completed.len(),
                },
            );
        }
        for (expected, actual) in self.expectations.iter().zip(completed.continuations().iter()) {
            let recorded = actual.evidence().recorded();
            if recorded.graph_fingerprint() != expected.graph_fingerprint
                || recorded.generation() != expected.generation
                || recorded.frame_extent() != expected.frame_extent
                || recorded.frame_seed() != expected.frame_seed
            {
                return Err(PreviewGpuHeterogeneousCompletionError::CompletionIdentityMismatch);
            }
        }
        if self.lease.is_none() {
            return Err(PreviewGpuHeterogeneousCompletionError::LeaseMissing);
        }
        Ok(())
    }

    /// Whether this exact effect output admits compatible in-flight rebound
    /// and post-completion cache use.
    pub(crate) const fn reusable(&self) -> bool {
        self.reusable
    }

    /// Consume the validated terminal and return its active Broker lease.
    pub(crate) fn into_lease(
        mut self,
    ) -> Result<
        super::preview_visual_execution_task::VisualExecutionLease,
        PreviewGpuHeterogeneousCompletionError,
    > {
        self.lease.take().ok_or(PreviewGpuHeterogeneousCompletionError::LeaseMissing)
    }

    /// Fail this candidate before actual GPU completion. The Broker atomically
    /// decides whether a latest compatible binding still owns terminal
    /// authority.
    pub(crate) fn fail(
        mut self,
    ) -> Option<super::preview_visual_execution_task::VisualExecutionGpuFailure> {
        self.lease.take().map(|lease| lease.fail_gpu())
    }
}

fn expectation(binding: HeterogeneousGpuContinuationBinding) -> PreviewGpuHeterogeneousExpectation {
    PreviewGpuHeterogeneousExpectation {
        graph_fingerprint: binding.graph_fingerprint(),
        generation: binding.generation(),
        frame_extent: binding.frame_extent(),
        frame_seed: binding.frame_seed(),
    }
}

/// Invalid ownership or evidence at the heterogeneous Viewer terminal seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PreviewGpuHeterogeneousCompletionError {
    #[error("heterogeneous Viewer execution has no CPU-prefix inputs")]
    EmptyInputBatch,
    #[error("heterogeneous Viewer input generation does not match lease {lease_generation}")]
    LeaseGenerationMismatch { lease_generation: u64 },
    #[error("heterogeneous Viewer inputs were not consumed by command recording")]
    InputsNotRecorded,
    #[error("heterogeneous Viewer completion count is {actual}, expected {expected}")]
    CompletionCountMismatch { expected: usize, actual: usize },
    #[error("heterogeneous Viewer completion identity differs from its recorded request")]
    CompletionIdentityMismatch,
    #[error("heterogeneous Viewer Broker lease is missing")]
    LeaseMissing,
}

/// Decode provenance aggregated across one resolved Viewer candidate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PreviewDecodeExecutionSummary {
    pub(crate) media_layers: u32,
    pub(crate) software_cpu_layers: u32,
    pub(crate) hardware_cpu_transfer_layers: u32,
    pub(crate) hardware_native_layers: u32,
    pub(crate) p010_10_bit_hardware_layers: u32,
}

impl PreviewDecodeExecutionSummary {
    /// Project one decoder path into stable candidate provenance.
    pub(crate) fn from_path(path: PreviewDecodeExecutionPath) -> Self {
        let mut summary = Self { media_layers: 1, ..Self::default() };
        match path {
            PreviewDecodeExecutionPath::SoftwareCpu => summary.software_cpu_layers = 1,
            PreviewDecodeExecutionPath::HardwareCpuTransfer { surface, sampling, .. } => {
                summary.hardware_cpu_transfer_layers = 1;
                if surface == DecodedVideoSurfaceFormat::P010 && sampling.bit_depth == 10 {
                    summary.p010_10_bit_hardware_layers = 1;
                }
            }
            PreviewDecodeExecutionPath::HardwareNative { surface, sampling, .. } => {
                summary.hardware_native_layers = 1;
                if surface == DecodedVideoSurfaceFormat::P010 && sampling.bit_depth == 10 {
                    summary.p010_10_bit_hardware_layers = 1;
                }
            }
        }
        summary
    }

    /// Merge another layer summary without losing whole-candidate provenance.
    pub(crate) fn accumulate(&mut self, other: Self) {
        self.media_layers = self.media_layers.saturating_add(other.media_layers);
        self.software_cpu_layers =
            self.software_cpu_layers.saturating_add(other.software_cpu_layers);
        self.hardware_cpu_transfer_layers = self
            .hardware_cpu_transfer_layers
            .saturating_add(other.hardware_cpu_transfer_layers);
        self.hardware_native_layers =
            self.hardware_native_layers.saturating_add(other.hardware_native_layers);
        self.p010_10_bit_hardware_layers = self
            .p010_10_bit_hardware_layers
            .saturating_add(other.p010_10_bit_hardware_layers);
    }
}

/// Result of binding one preview intent to execution generation authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewGenerationBinding {
    /// The intent is already represented by the current generation.
    Current(u64),
    /// A new generation was opened for a materially different intent.
    Rotated(u64),
}

/// UI-neutral disposition of one resolved Viewer candidate request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewCandidateDecision {
    /// The exact usable output is already registered.
    Current,
    /// Execute and register a new output under this correlation identity.
    Execute(u64),
    /// Required media work is still pending.
    Loading,
    /// The intent cannot currently produce a Viewer output.
    Unavailable,
}

/// Coherent execution lifecycle shared by Window and Headless Preview Adapters.
///
/// `G` is the complete generation identity, `K` is the resolved Viewer output
/// identity, and `O` is an opaque Adapter-owned usable output payload. The
/// Module never interprets UI state, codec payloads, or GPU resources.
pub(crate) struct PreviewExecutionCoordinator<G, K, O> {
    generation_key: Option<G>,
    generation: u64,
    generation_cancellation: ExecutionCancellationToken,
    pending: bool,
    presentation_quality: FramePresentationQuality,
    next_candidate_id: u64,
    current_output: Option<(K, O)>,
    current_output_generation: Option<u64>,
    prepared_successor: Option<(PreviewPlaybackIntent, PreparedPreviewOutput<K, O>)>,
    prepared_successor_generation: Option<u64>,
}

impl<G, K, O> Default for PreviewExecutionCoordinator<G, K, O> {
    fn default() -> Self {
        Self {
            generation_key: None,
            generation: 0,
            generation_cancellation: ExecutionCancellationToken::new(),
            pending: false,
            presentation_quality: FramePresentationQuality::Ready,
            next_candidate_id: 0,
            current_output: None,
            current_output_generation: None,
            prepared_successor: None,
            prepared_successor_generation: None,
        }
    }
}

impl<G: PartialEq, K, O> PreviewExecutionCoordinator<G, K, O> {
    /// Whether binding `key` would preserve the active execution generation.
    ///
    /// Adapters use this observation before rotation to finish lifecycle work
    /// whose safety proof belongs to the current generation. Once rotation
    /// occurs, a retained output is intentionally stale and can no longer
    /// authorize release of its decoded source residency.
    pub(crate) fn is_current_generation_key(&self, key: &G) -> bool {
        self.generation_key.as_ref() == Some(key)
    }

    /// Borrow the exact active generation identity for transition
    /// classification without transferring mutation authority.
    pub(crate) fn current_generation_key(&self) -> Option<&G> {
        self.generation_key.as_ref()
    }

    /// Reuse the exact current generation or rotate through the sole generation
    /// authority supplied by the execution Adapter.
    pub(crate) fn bind_generation(
        &mut self,
        key: G,
        begin_generation: impl FnOnce() -> u64,
    ) -> PreviewGenerationBinding {
        if self.generation_key.as_ref() == Some(&key) {
            return PreviewGenerationBinding::Current(self.generation);
        }
        self.generation_cancellation.cancel();
        self.generation_key = Some(key);
        self.generation = begin_generation();
        self.generation_cancellation = ExecutionCancellationToken::new();
        self.pending = false;
        self.prepared_successor = None;
        self.prepared_successor_generation = None;
        PreviewGenerationBinding::Rotated(self.generation)
    }

    /// Invalidate every intent and bind the resulting empty lifecycle to a new
    /// execution generation.
    pub(crate) fn invalidate(&mut self, begin_generation: impl FnOnce() -> u64) -> u64 {
        self.generation_cancellation.cancel();
        self.generation_key = None;
        self.generation = begin_generation();
        self.generation_cancellation = ExecutionCancellationToken::new();
        self.pending = false;
        self.presentation_quality = FramePresentationQuality::Ready;
        self.current_output = None;
        self.current_output_generation = None;
        self.prepared_successor = None;
        self.prepared_successor_generation = None;
        self.generation
    }

    /// Current execution generation.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Cooperative cancellation authority for the current immutable
    /// generation. A rotation cancels every previously issued clone.
    pub(crate) fn generation_cancellation(&self) -> ExecutionCancellationToken {
        self.generation_cancellation.clone()
    }

    /// Mark whether the current intent is waiting for required media work.
    pub(crate) fn set_pending(&mut self, pending: bool) {
        self.pending = pending;
    }

    /// Whether required work for the current intent remains pending.
    pub(crate) const fn is_pending(&self) -> bool {
        self.pending
    }

    /// Record the aggregate quality of the exact resolved candidate.
    pub(crate) fn set_presentation_quality(&mut self, quality: FramePresentationQuality) {
        self.presentation_quality = quality;
    }

    /// Aggregate quality carried into a Playback presentation ticket.
    pub(crate) const fn presentation_quality(&self) -> FramePresentationQuality {
        self.presentation_quality
    }

    /// Issue one non-repeating candidate identity for execution correlation.
    pub(crate) fn issue_candidate_id(&mut self) -> u64 {
        self.next_candidate_id = self.next_candidate_id.saturating_add(1);
        self.next_candidate_id
    }

    /// Select the one legal action for a resolved or pending Viewer intent.
    ///
    /// Candidate identity is issued only for actual execution; cache hits,
    /// loading observations, and unavailable states cannot consume IDs.
    pub(crate) fn plan_candidate(&mut self, resolved_key: Option<&K>) -> PreviewCandidateDecision
    where
        K: PartialEq,
    {
        self.plan_candidate_with_reuse(resolved_key, true)
    }

    /// Select one action while keeping semantic identity separate from cache
    /// admission. A non-reusable plan always receives a fresh candidate even
    /// when its semantic key equals the retained output.
    pub(crate) fn plan_candidate_with_reuse(
        &mut self,
        resolved_key: Option<&K>,
        allow_cross_call_reuse: bool,
    ) -> PreviewCandidateDecision
    where
        K: PartialEq,
    {
        let Some(key) = resolved_key else {
            return if self.pending {
                PreviewCandidateDecision::Loading
            } else {
                PreviewCandidateDecision::Unavailable
            };
        };
        if allow_cross_call_reuse
            && self.current_output.as_ref().is_some_and(|(current, _)| current == key)
        {
            // Re-resolving the complete output identity proves that a retained
            // stale output is exact under the active generation again.
            self.current_output_generation = Some(self.generation);
            PreviewCandidateDecision::Current
        } else {
            PreviewCandidateDecision::Execute(self.issue_candidate_id())
        }
    }

    /// Register the exact output that a presentation Adapter made usable.
    pub(crate) fn register_output(&mut self, key: K, output: O) {
        self.current_output = Some((key, output));
        self.current_output_generation = Some(self.generation);
    }

    /// Retain one ticketless immediate successor without replacing visibility.
    pub(crate) fn register_prepared_successor(
        &mut self,
        intent: PreviewPlaybackIntent,
        key: K,
        output: O,
    ) {
        self.prepared_successor = Some((
            intent,
            PreparedPreviewOutput::Gpu { key, output, already_visible: false },
        ));
        self.prepared_successor_generation = Some(self.generation);
    }

    /// Retain a proved transparent immediate successor.
    pub(crate) fn register_prepared_transparent_successor(
        &mut self,
        intent: PreviewPlaybackIntent,
    ) {
        self.prepared_successor = Some((intent, PreparedPreviewOutput::Transparent));
        self.prepared_successor_generation = Some(self.generation);
    }

    /// Whether the active generation already retains this exact successor intent.
    pub(crate) fn has_prepared_successor_for_intent(&self, intent: PreviewPlaybackIntent) -> bool {
        self.prepared_successor_generation == Some(self.generation)
            && self
                .prepared_successor
                .as_ref()
                .is_some_and(|(prepared, _)| *prepared == intent)
    }

    /// GPU output identity of the prepared successor proved for the exact
    /// current transport intent.
    pub(crate) fn exact_prepared_successor_gpu_output_key_for_intent(
        &self,
        intent: PreviewPlaybackIntent,
    ) -> Option<&K> {
        if !self.has_prepared_successor_for_intent(intent) {
            return None;
        }
        self.prepared_successor.as_ref().and_then(|(_, prepared)| match prepared {
            PreparedPreviewOutput::Gpu { key, .. } => Some(key),
            PreparedPreviewOutput::Transparent => None,
        })
    }

    /// Promote only the successor proved for the exact current transport intent.
    pub(crate) fn promote_prepared_successor_for_intent(
        &mut self,
        intent: PreviewPlaybackIntent,
    ) -> Option<PreviewPreparedPromotion<K>>
    where
        K: Clone,
    {
        let matches = self.prepared_successor_generation == Some(self.generation)
            && self
                .prepared_successor
                .as_ref()
                .is_some_and(|(prepared, _)| *prepared == intent);
        if !matches {
            return None;
        }
        let (_, prepared) = self.prepared_successor.take()?;
        self.prepared_successor_generation = None;
        match prepared {
            PreparedPreviewOutput::Gpu { key, output, already_visible } => {
                self.current_output = Some((key, output));
                self.current_output_generation = Some(self.generation);
                self.current_output.as_ref().map(|(key, _)| PreviewPreparedPromotion::Gpu {
                    key: key.clone(),
                    already_visible,
                })
            }
            PreparedPreviewOutput::Transparent => {
                self.current_output = None;
                self.current_output_generation = None;
                Some(PreviewPreparedPromotion::Transparent)
            }
        }
    }

    /// Prove an immediate successor by aliasing the exact retained artifact.
    ///
    /// The opaque payload is cloneable metadata only; the presentation Adapter
    /// continues to own the single move-only physical lease.
    pub(crate) fn prepare_successor_from_current(
        &mut self,
        intent: PreviewPlaybackIntent,
        key: &K,
    ) -> bool
    where
        K: Clone + PartialEq,
        O: Clone,
    {
        let Some((current_key, output)) = self
            .current_output
            .as_ref()
            .filter(|(current_key, _)| current_key == key)
            .cloned()
        else {
            return false;
        };
        self.prepared_successor = Some((
            intent,
            PreparedPreviewOutput::Gpu { key: current_key, output, already_visible: true },
        ));
        self.prepared_successor_generation = Some(self.generation);
        true
    }

    /// Exact prepared alias that is already represented by the visible slot.
    #[cfg(test)]
    pub(crate) fn already_visible_successor_key(
        &self,
        intent: PreviewPlaybackIntent,
    ) -> Option<&K> {
        if self.prepared_successor_generation != Some(self.generation) {
            return None;
        }
        self.prepared_successor.as_ref().and_then(|(prepared_intent, prepared)| {
            if *prepared_intent != intent {
                return None;
            }
            match prepared {
                PreparedPreviewOutput::Gpu { key, already_visible: true, .. } => Some(key),
                PreparedPreviewOutput::Gpu { already_visible: false, .. }
                | PreparedPreviewOutput::Transparent => None,
            }
        })
    }

    /// Whether the registered output was proven under the active generation.
    pub(crate) fn has_exact_current_output(&self) -> bool {
        self.current_output.is_some() && self.current_output_generation == Some(self.generation)
    }

    /// Return the usable output only when its complete identity matches.
    pub(crate) fn output_for(&mut self, key: &K) -> Option<O>
    where
        K: PartialEq,
        O: Clone,
    {
        let output = self
            .current_output
            .as_ref()
            .filter(|(current, _)| current == key)
            .map(|(_, output)| output.clone());
        if output.is_some() {
            self.current_output_generation = Some(self.generation);
        }
        output
    }

    /// Inspect the one registered output for explicitly scoped stale reuse.
    pub(crate) fn current_output(&self) -> Option<(&K, &O)> {
        self.current_output.as_ref().map(|(key, output)| (key, output))
    }

    /// Inspect the registered output only when it remains proved under the
    /// active execution generation.
    ///
    /// This is a read-only observation of an artifact that was already made
    /// usable. It does not re-resolve a semantic key, refresh stale authority,
    /// or publish an output.
    pub(crate) fn exact_current_output(&self) -> Option<(&K, &O)> {
        self.has_exact_current_output().then(|| self.current_output()).flatten()
    }

    /// Retire the currently registered output without rotating media work.
    pub(crate) fn clear_output(&mut self) -> bool {
        self.current_output_generation = None;
        self.prepared_successor_generation = None;
        let current = self.current_output.take().is_some();
        let prepared = self.prepared_successor.take().is_some();
        current || prepared
    }

    /// Revoke the registered output only when its exact semantic key matches.
    ///
    /// A force-retired quarantined submission lost its completion callback
    /// after a queue-order publication; the artifact must not stay current,
    /// but a newer output with a different key is untouched.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn clear_current_output_for_key(&mut self, key: &K)
    where
        K: PartialEq,
    {
        if self.current_output.as_ref().is_some_and(|(current, _)| current == key) {
            self.current_output_generation = None;
            self.current_output = None;
        }
    }

    /// Revoke one prepared successor for an exact playback intent.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn clear_prepared_successor_for_intent(&mut self, intent: PreviewPlaybackIntent) {
        if self
            .prepared_successor
            .as_ref()
            .is_some_and(|(prepared, _)| *prepared == intent)
        {
            self.prepared_successor_generation = None;
            self.prepared_successor = None;
        }
    }

    /// Retire the output only when both its semantic key and Adapter-owned
    /// physical artifact identity still match.
    ///
    /// The predicate must be a bounded, non-reentrant identity comparison. It
    /// executes while the Coordinator exclusively owns the check-and-clear
    /// transition so a same-semantic replacement cannot be cleared between
    /// authority validation and commit.
    pub(crate) fn clear_output_if(&mut self, key: &K, matches_artifact: impl Fn(&O) -> bool) -> bool
    where
        K: PartialEq,
    {
        let current_matches = self
            .current_output
            .as_ref()
            .is_some_and(|(current, output)| current == key && matches_artifact(output));
        if current_matches {
            self.current_output_generation = None;
            self.current_output = None;
        }
        let prepared_matches =
            self.prepared_successor.as_ref().is_some_and(|(_, prepared)| match prepared {
                PreparedPreviewOutput::Gpu { key: prepared_key, output, .. } => {
                    prepared_key == key && matches_artifact(output)
                }
                PreparedPreviewOutput::Transparent => false,
            });
        if prepared_matches {
            self.prepared_successor_generation = None;
            self.prepared_successor = None;
        }
        current_matches || prepared_matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_signal_monitoring_is_part_of_final_presentation_identity_only() {
        let base = PreviewOutputKey::new(
            SequenceId::new(),
            1920,
            1080,
            PreviewSemanticIdentity::from_test_fingerprint([7; 32]),
        );
        assert_eq!(
            base.with_signal_monitoring(
                mondrian_core::ProgramScopesTap::MonitorOutput,
                mondrian_core::SignalMonitoringSettings::default(),
            ),
            base
        );
        let active =
            mondrian_core::SignalMonitoringSettings { false_color: true, ..Default::default() };
        assert_ne!(
            base.with_signal_monitoring(mondrian_core::ProgramScopesTap::ProgramOutput, active),
            base
        );
        assert_ne!(
            base.with_signal_monitoring(mondrian_core::ProgramScopesTap::ProgramOutput, active),
            base.with_signal_monitoring(mondrian_core::ProgramScopesTap::MonitorOutput, active)
        );
    }

    #[test]
    fn generation_output_and_candidate_lifecycle_stay_atomic() {
        let mut coordinator = PreviewExecutionCoordinator::<u8, u8, &'static str>::default();
        assert_eq!(
            coordinator.bind_generation(1, || 41),
            PreviewGenerationBinding::Rotated(41)
        );
        coordinator.set_pending(true);
        coordinator.set_presentation_quality(FramePresentationQuality::Degraded);
        coordinator.register_output(9, "texture");
        assert!(coordinator.has_exact_current_output());
        assert_eq!(coordinator.exact_current_output(), Some((&9, &"texture")));
        assert_eq!(
            coordinator.plan_candidate(Some(&9)),
            PreviewCandidateDecision::Current
        );
        assert_eq!(
            coordinator.plan_candidate(Some(&10)),
            PreviewCandidateDecision::Execute(1)
        );
        assert_eq!(coordinator.output_for(&9), Some("texture"));

        assert_eq!(
            coordinator.bind_generation(1, || panic!("same key cannot rotate")),
            PreviewGenerationBinding::Current(41)
        );
        assert!(coordinator.is_pending());
        assert_eq!(
            coordinator.presentation_quality(),
            FramePresentationQuality::Degraded
        );

        assert_eq!(coordinator.invalidate(|| 42), 42);
        assert!(!coordinator.is_pending());
        assert_eq!(
            coordinator.presentation_quality(),
            FramePresentationQuality::Ready
        );
        assert_eq!(coordinator.output_for(&9), None);
        assert!(!coordinator.has_exact_current_output());
        assert_eq!(coordinator.exact_current_output(), None);
    }

    #[test]
    fn retained_output_is_stale_until_new_generation_resolves_its_identity() {
        let mut coordinator = PreviewExecutionCoordinator::<u8, u8, &'static str>::default();
        coordinator.bind_generation(1, || 41);
        coordinator.register_output(9, "texture");

        assert!(coordinator.is_current_generation_key(&1));
        assert!(!coordinator.is_current_generation_key(&2));

        assert_eq!(
            coordinator.bind_generation(2, || 42),
            PreviewGenerationBinding::Rotated(42)
        );
        assert!(!coordinator.has_exact_current_output());
        assert_eq!(coordinator.exact_current_output(), None);
        assert_eq!(
            coordinator.plan_candidate(Some(&9)),
            PreviewCandidateDecision::Current
        );
        assert!(coordinator.has_exact_current_output());
    }

    #[test]
    fn seek_and_invalidation_cancel_only_the_retired_generation() {
        let mut coordinator = PreviewExecutionCoordinator::<u8, u8, ()>::default();
        coordinator.bind_generation(1, || 41);
        let first = coordinator.generation_cancellation();
        assert!(!first.is_canceled());

        assert_eq!(
            coordinator.bind_generation(1, || panic!("same intent must not rotate")),
            PreviewGenerationBinding::Current(41)
        );
        assert!(!first.is_canceled());

        assert_eq!(
            coordinator.bind_generation(2, || 42),
            PreviewGenerationBinding::Rotated(42)
        );
        assert!(
            first.is_canceled(),
            "seek rotation must retire the old token"
        );
        let second = coordinator.generation_cancellation();
        assert!(!second.is_canceled());

        coordinator.invalidate(|| 43);
        assert!(second.is_canceled());
        assert!(!coordinator.generation_cancellation().is_canceled());
    }

    #[test]
    fn non_reusable_semantic_identity_always_issues_a_fresh_candidate() {
        let mut coordinator = PreviewExecutionCoordinator::<u8, u8, &'static str>::default();
        coordinator.bind_generation(1, || 41);
        coordinator.register_output(9, "texture");

        assert_eq!(
            coordinator.plan_candidate_with_reuse(Some(&9), false),
            PreviewCandidateDecision::Execute(1)
        );
        assert_eq!(
            coordinator.plan_candidate_with_reuse(Some(&9), false),
            PreviewCandidateDecision::Execute(2)
        );
    }

    #[test]
    fn conditional_clear_is_atomic_across_semantic_and_physical_identity() {
        let mut coordinator = PreviewExecutionCoordinator::<u8, u8, &'static str>::default();
        coordinator.bind_generation(1, || 41);
        coordinator.register_output(9, "physical:new");

        assert!(!coordinator.clear_output_if(&9, |output| *output == "physical:old"));
        assert_eq!(coordinator.current_output(), Some((&9, &"physical:new")));
        assert!(!coordinator.clear_output_if(&10, |output| *output == "physical:new"));
        assert!(coordinator.clear_output_if(&9, |output| *output == "physical:new"));
        assert!(coordinator.current_output().is_none());
    }

    #[test]
    fn prepared_successor_is_invisible_until_exact_key_promotion() {
        let mut coordinator = PreviewExecutionCoordinator::<u8, u8, &'static str>::default();
        coordinator.bind_generation(1, || 41);
        coordinator.register_output(9, "physical:current");
        let successor = PreviewPlaybackIntent::new(
            mondrian_playback::PlaybackEngine::default().snapshot().epoch,
            3,
            10,
        );
        let skipped_successor = PreviewPlaybackIntent::new(successor.epoch, 3, 11);
        coordinator.register_prepared_successor(successor, 10, "physical:successor");

        assert_eq!(
            coordinator.current_output(),
            Some((&9, &"physical:current"))
        );
        assert!(coordinator.has_prepared_successor_for_intent(successor));
        assert_eq!(
            coordinator.exact_prepared_successor_gpu_output_key_for_intent(successor),
            Some(&10)
        );
        assert_eq!(
            coordinator.exact_prepared_successor_gpu_output_key_for_intent(skipped_successor),
            None
        );
        assert_eq!(
            coordinator.plan_candidate(Some(&9)),
            PreviewCandidateDecision::Current
        );
        assert_eq!(
            coordinator.current_output(),
            Some((&9, &"physical:current"))
        );

        assert_eq!(
            coordinator.promote_prepared_successor_for_intent(successor),
            Some(PreviewPreparedPromotion::Gpu { key: 10, already_visible: false })
        );
        assert_eq!(
            coordinator.current_output(),
            Some((&10, &"physical:successor"))
        );
        assert!(!coordinator.has_prepared_successor_for_intent(successor));
    }

    #[test]
    fn rotation_and_artifact_scoped_clear_cannot_revive_a_prepared_successor() {
        let mut coordinator = PreviewExecutionCoordinator::<u8, u8, &'static str>::default();
        coordinator.bind_generation(1, || 41);
        coordinator.register_output(9, "physical:current");
        let successor = PreviewPlaybackIntent::new(
            mondrian_playback::PlaybackEngine::default().snapshot().epoch,
            3,
            10,
        );
        coordinator.register_prepared_successor(successor, 10, "physical:successor");

        assert!(coordinator.clear_output_if(&10, |output| *output == "physical:successor"));
        assert_eq!(
            coordinator.current_output(),
            Some((&9, &"physical:current"))
        );
        coordinator.register_prepared_successor(successor, 10, "physical:successor-2");
        coordinator.bind_generation(2, || 42);
        assert!(!coordinator.has_prepared_successor_for_intent(successor));
        assert_eq!(
            coordinator.plan_candidate(Some(&10)),
            PreviewCandidateDecision::Execute(1)
        );
    }

    #[test]
    fn transparent_successor_is_bound_to_exact_transport_intent() {
        let mut coordinator = PreviewExecutionCoordinator::<u8, u8, &'static str>::default();
        coordinator.bind_generation(1, || 41);
        coordinator.register_output(9, "physical:stale");
        let epoch = mondrian_playback::PlaybackEngine::default().snapshot().epoch;
        let successor = PreviewPlaybackIntent::new(epoch, 3, 10);
        let wrong_frame = PreviewPlaybackIntent::new(epoch, 3, 11);
        coordinator.register_prepared_transparent_successor(successor);

        assert_eq!(
            coordinator.promote_prepared_successor_for_intent(wrong_frame),
            None
        );
        assert_eq!(coordinator.current_output(), Some((&9, &"physical:stale")));
        assert_eq!(
            coordinator.promote_prepared_successor_for_intent(successor),
            Some(PreviewPreparedPromotion::Transparent)
        );
        assert!(coordinator.current_output().is_none());
    }
}
