//! UI-independent lifecycle coordination for production preview execution.
//!
//! Media decoding, renderer recording, and Window texture registration are
//! concrete Adapters. This deep Module owns the state that must remain coherent
//! across those Adapters: generation binding, pending state, executed quality,
//! candidate identity, and the exact currently registered output.

use super::preview_unavailability::PreviewUnavailability;
use mondrian_core::types::SequenceId;
use mondrian_core::WorkingColorSpace;
use mondrian_media::{DecodedVideoSurfaceFormat, PreviewDecodeExecutionPath};
use mondrian_playback::FramePresentationQuality;
use mondrian_renderer::{
    RenderMonitorAdaptation, RenderOutputColorBoundary, ViewerGpuExecutionLayer,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Complete identity of one resolved Viewer output.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PreviewOutputKey {
    pub(crate) sequence_id: SequenceId,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) plan_signature: u64,
}

impl PreviewOutputKey {
    /// Construct one output identity from its complete resolved plan signature.
    pub(crate) const fn new(
        sequence_id: SequenceId,
        width: u32,
        height: u32,
        plan_signature: u64,
    ) -> Self {
        Self { sequence_id, width, height, plan_signature }
    }

    /// Derive the monitor-adapted identity used by final Viewer presentation.
    pub(crate) fn with_monitor_adaptation(&self, adaptation: &RenderMonitorAdaptation) -> Self {
        let mut hasher = DefaultHasher::new();
        self.plan_signature.hash(&mut hasher);
        adaptation.hash(&mut hasher);
        Self::new(self.sequence_id, self.width, self.height, hasher.finish())
    }
}

/// UI-neutral result of asking for a GPU Viewer execution candidate.
pub(crate) enum PreviewGpuFrameState {
    /// The exact output is already registered by the active presentation Adapter.
    Current,
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
    presentation_ticket: Option<mondrian_playback::FramePresentationTicket>,
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
        presentation_ticket: Option<mondrian_playback::FramePresentationTicket>,
        decode_execution: PreviewDecodeExecutionSummary,
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
            presentation_ticket,
            decode_execution,
        }
    }

    /// Stable renderer registration key for this resolved output.
    pub(crate) fn external_texture_key(&self) -> String {
        format!(
            "viewer.gpu:{}:{}x{}:{:016x}",
            self.sequence_id, self.width, self.height, self.output_key.plan_signature
        )
    }

    /// Candidate identity used to correlate one execution attempt.
    pub(crate) const fn candidate_id(&self) -> u64 {
        self.candidate_id
    }

    /// Exact terminal authority completed only after usable presentation.
    pub(crate) const fn presentation_ticket(
        &self,
    ) -> Option<mondrian_playback::FramePresentationTicket> {
        self.presentation_ticket
    }

    /// Decode provenance for the exact media layers entering this candidate.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn decode_execution(&self) -> PreviewDecodeExecutionSummary {
        self.decode_execution
    }
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

impl PreviewGenerationBinding {
    /// Bound generation in either lifecycle case.
    pub(crate) const fn generation(self) -> u64 {
        match self {
            Self::Current(generation) | Self::Rotated(generation) => generation,
        }
    }
}

/// Coherent execution lifecycle shared by Window and Headless Preview Adapters.
///
/// `G` is the complete generation identity, `K` is the resolved Viewer output
/// identity, and `O` is an opaque Adapter-owned usable output payload. The
/// Module never interprets UI state, codec payloads, or GPU resources.
pub(crate) struct PreviewExecutionCoordinator<G, K, O> {
    generation_key: Option<G>,
    generation: u64,
    pending: bool,
    presentation_quality: FramePresentationQuality,
    next_candidate_id: u64,
    current_output: Option<(K, O)>,
}

impl<G, K, O> Default for PreviewExecutionCoordinator<G, K, O> {
    fn default() -> Self {
        Self {
            generation_key: None,
            generation: 0,
            pending: false,
            presentation_quality: FramePresentationQuality::Ready,
            next_candidate_id: 0,
            current_output: None,
        }
    }
}

impl<G: PartialEq, K, O> PreviewExecutionCoordinator<G, K, O> {
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
        self.generation_key = Some(key);
        self.generation = begin_generation();
        self.pending = false;
        PreviewGenerationBinding::Rotated(self.generation)
    }

    /// Invalidate every intent and bind the resulting empty lifecycle to a new
    /// execution generation.
    pub(crate) fn invalidate(&mut self, begin_generation: impl FnOnce() -> u64) -> u64 {
        self.generation_key = None;
        self.generation = begin_generation();
        self.pending = false;
        self.presentation_quality = FramePresentationQuality::Ready;
        self.current_output = None;
        self.generation
    }

    /// Current execution generation.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
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
        let Some(key) = resolved_key else {
            return if self.pending {
                PreviewCandidateDecision::Loading
            } else {
                PreviewCandidateDecision::Unavailable
            };
        };
        if self.current_output.as_ref().is_some_and(|(current, _)| current == key) {
            PreviewCandidateDecision::Current
        } else {
            PreviewCandidateDecision::Execute(self.issue_candidate_id())
        }
    }

    /// Register the exact output that a presentation Adapter made usable.
    pub(crate) fn register_output(&mut self, key: K, output: O) {
        self.current_output = Some((key, output));
    }

    /// Return the usable output only when its complete identity matches.
    pub(crate) fn output_for(&self, key: &K) -> Option<O>
    where
        K: PartialEq,
        O: Clone,
    {
        self.current_output
            .as_ref()
            .filter(|(current, _)| current == key)
            .map(|(_, output)| output.clone())
    }

    /// Inspect the one registered output for explicitly scoped stale reuse.
    pub(crate) fn current_output(&self) -> Option<(&K, &O)> {
        self.current_output.as_ref().map(|(key, output)| (key, output))
    }

    /// Retire the currently registered output without rotating media work.
    pub(crate) fn clear_output(&mut self) -> bool {
        self.current_output.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
