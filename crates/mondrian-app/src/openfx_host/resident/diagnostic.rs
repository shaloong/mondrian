//! Default-off parent attribution over the sole native render path.
//!
//! Fixed caller-owned diagnostic storage is never a frame/owner authority.
//! Native actions and scheduling stay unchanged. Explicit measured requests
//! carry a bounded correlated child report on the current strict wire schema.
use std::time::Duration;
#[cfg(any(test, feature = "validation"))]
use std::time::Instant;

/// A nonoverlapping parent segment of the complete public native render call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum OpenFxRenderPhase {
    /// First owner check, extent and complete controlled input admission.
    InputAdmission,
    /// Acquisition and admission of the single output owner before native DSP.
    OutputAllocation,
    /// Complete selected binary fingerprint before request submission.
    IdentityBefore,
    /// Complete admitted input copy to the exclusive shared source range.
    InputCopy,
    /// Authored parameter/timing clone, bounded encoding and socket write.
    RequestSubmission,
    /// Exact completion wait, including child processing, both pumps and scheduling.
    AwaitCompletion,
    /// Complete selected binary fingerprint after native completion.
    IdentityAfter,
    /// Terminal envelope, feedback identity/revision and product log admission.
    FeedbackAdmission,
    /// Copy to the preacquired output owner's initially uninitialized storage.
    OutputCopy,
    /// Canonical opaque alpha sweep; absent for other representations.
    OpaqueCanonicalization,
    /// Sole Effects controlled numeric, extent and actual-alpha admission.
    OutputAdmission,
    /// Final owner checkpoint immediately before returning the admitted output.
    FinalPublication,
}

impl OpenFxRenderPhase {
    /// Stable order for fixed diagnostic storage; these spans do not overlap.
    #[cfg(any(test, feature = "validation"))]
    pub const ALL: [Self; 12] = [
        Self::InputAdmission,
        Self::OutputAllocation,
        Self::IdentityBefore,
        Self::InputCopy,
        Self::RequestSubmission,
        Self::AwaitCompletion,
        Self::IdentityAfter,
        Self::FeedbackAdmission,
        Self::OutputCopy,
        Self::OpaqueCanonicalization,
        Self::OutputAdmission,
        Self::FinalPublication,
    ];

    /// Stable diagnostic name; this is not a production protocol field.
    #[cfg(any(test, feature = "validation"))]
    pub const fn name(self) -> &'static str {
        match self {
            Self::InputAdmission => "input_admission",
            Self::OutputAllocation => "output_allocation",
            Self::IdentityBefore => "identity_before",
            Self::InputCopy => "input_copy",
            Self::RequestSubmission => "request_submission",
            Self::AwaitCompletion => "await_completion_inclusive",
            Self::IdentityAfter => "identity_after",
            Self::FeedbackAdmission => "feedback_admission",
            Self::OutputCopy => "output_copy",
            Self::OpaqueCanonicalization => "opaque_canonicalization",
            Self::OutputAdmission => "output_admission",
            Self::FinalPublication => "final_publication",
        }
    }
}

pub(super) enum ProbeEvent {
    Sequence(u64),
    RequestCommitted,
    InputCopied(usize),
    OutputCopied(usize),
    OpaquePixels(usize),
    PollStep,
    ReadMadeProgress,
    ProgressPacket,
    OutputAlpha(mondrian_core::ColorFrameAlpha),
}

// Static dispatch keeps the normal route zero-sized. Its implementation has
// no clocks, allocations, branches on a global mode, log or foreign callbacks.
pub(super) trait ParentProbe {
    type Stamp;
    fn start(&mut self) -> Self::Stamp;
    fn measure_child(&self) -> bool;
    fn record_child(&mut self, value: super::child_diagnostic::OpenFxChildRenderDiagnostic);
    fn finish(&mut self, phase: OpenFxRenderPhase, start: Self::Stamp);
    fn event(&mut self, event: ProbeEvent);
    fn slept(&mut self, requested: Duration, start: Self::Stamp);
    fn output_admission_checkpoint(
        &mut self,
        checkpoint: &mut dyn FnMut() -> Result<(), super::OpenFxHostError>,
    ) -> Result<(), super::OpenFxHostError>;
}

impl ParentProbe for () {
    type Stamp = ();
    #[inline(always)]
    fn measure_child(&self) -> bool {
        false
    }
    #[inline(always)]
    fn record_child(&mut self, _value: super::child_diagnostic::OpenFxChildRenderDiagnostic) {}
    #[inline(always)]
    fn start(&mut self) {}
    #[inline(always)]
    fn finish(&mut self, _phase: OpenFxRenderPhase, _start: ()) {}
    #[inline(always)]
    fn event(&mut self, _event: ProbeEvent) {}
    #[inline(always)]
    fn slept(&mut self, _requested: Duration, _start: ()) {}
    #[inline(always)]
    fn output_admission_checkpoint(
        &mut self,
        checkpoint: &mut dyn FnMut() -> Result<(), super::OpenFxHostError>,
    ) -> Result<(), super::OpenFxHostError> {
        checkpoint()
    }
}

/// Fixed caller-owned parent measurement for one actual public render attempt.
///
/// This report is available only for validation/test builds. It owns no raster,
/// strings, worker, request queue, lease, deadline or cancellation authority.
/// Its timestamps perturb the instrumented route; it cannot establish a PASS.
/// A missing phase means it did not complete, rather than zero work/zero cost.
#[cfg(any(test, feature = "validation"))]
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct OpenFxRenderDiagnostic {
    /// Actual immutable parent-issued native instance identity, never an author ID.
    pub owner: Option<mondrian_core::EffectProcessorOwnerId>,
    /// Actual supervised child process identity observed by the session.
    pub worker_process_id: Option<std::num::NonZeroU32>,
    /// Original Render TCP action serial, absent before request admission.
    /// Idle GUI actions may make it differ from native feedback invocation count.
    pub sequence: Option<u64>,
    /// Fixed same-owner/action child report. Native phases nest inside child
    /// NativeDispatch and parent Await; they are never additive outer spans.
    pub child: Option<super::child_diagnostic::OpenFxChildRenderDiagnostic>,
    /// Whether all bytes of this actual request committed to the original socket.
    pub request_committed: bool,
    /// Whether this call returned an admitted output; errors preserve their own type.
    pub accepted_output: bool,
    /// Actual accepted native output representation, not a requested label.
    pub output_alpha: Option<mondrian_core::ColorFrameAlpha>,
    /// Completed input transport bytes; a refused or partial input is not counted.
    pub input_copy_bytes: usize,
    /// Bytes actually copied to this output owner, including a stopped partial copy.
    pub output_copy_bytes: usize,
    /// Original checkpoint calls inside OutputAdmission, including a refused call.
    pub output_admission_checkpoint_calls: usize,
    /// Time inside those callbacks, nested in OutputAdmission, never additive.
    /// Includes diagnostic clock overhead; normal rendering performs no clocks.
    pub output_admission_checkpoint_elapsed: Duration,
    /// Opaque output pixels whose A lane was canonicalized to one.
    pub opaque_pixels: usize,
    /// Calls to the sole parent's packet-reader step while awaiting this response.
    pub await_poll_steps: usize,
    /// Incomplete reader steps which made I/O progress and did not idle-sleep.
    pub await_reads_made_progress: usize,
    /// Actual complete correlated progress packets read during the wait.
    pub progress_packets: usize,
    /// Actual idle sleep calls in this parent's wait only, not child pump sleeps.
    pub idle_sleep_calls: usize,
    /// Sum of durations requested from the parent's existing idle sleep policy.
    pub idle_sleep_requested: Duration,
    /// Observed elapsed time around those parent sleeps, including timer overhead.
    pub idle_sleep_elapsed: Duration,
    /// Sole owned invocation interval after report reset, including failure retirement.
    pub owned_invocation_elapsed: Option<Duration>,
    /// Checked diagnostic counters overflowed; never affects native work/error types.
    pub counter_overflowed: bool,
    phases: [Option<Duration>; 12],
}

#[cfg(any(test, feature = "validation"))]
impl OpenFxRenderDiagnostic {
    /// Completed duration of this nonoverlapping phase; feedback pieces accumulate.
    pub const fn elapsed(&self, phase: OpenFxRenderPhase) -> Option<Duration> {
        self.phases[phase as usize]
    }

    fn add_count(&mut self, get: impl FnOnce(&mut Self) -> &mut usize, amount: usize) {
        let destination = get(self);
        if let Some(value) = destination.checked_add(amount) {
            *destination = value;
        } else {
            self.counter_overflowed = true;
        }
    }
}

#[cfg(any(test, feature = "validation"))]
impl ParentProbe for OpenFxRenderDiagnostic {
    type Stamp = Instant;
    fn measure_child(&self) -> bool {
        true
    }
    fn record_child(&mut self, value: super::child_diagnostic::OpenFxChildRenderDiagnostic) {
        self.child = Some(value);
    }
    #[inline]
    fn start(&mut self) -> Instant {
        Instant::now()
    }
    #[inline]
    fn finish(&mut self, phase: OpenFxRenderPhase, start: Instant) {
        let elapsed = start.elapsed();
        let slot = &mut self.phases[phase as usize];
        match slot.unwrap_or_default().checked_add(elapsed) {
            Some(value) => *slot = Some(value),
            None => self.counter_overflowed = true,
        }
    }
    #[inline]
    fn event(&mut self, event: ProbeEvent) {
        match event {
            ProbeEvent::Sequence(value) => self.sequence = Some(value),
            ProbeEvent::RequestCommitted => self.request_committed = true,
            ProbeEvent::InputCopied(bytes) => self.add_count(|p| &mut p.input_copy_bytes, bytes),
            ProbeEvent::OutputCopied(bytes) => self.add_count(|p| &mut p.output_copy_bytes, bytes),
            ProbeEvent::OpaquePixels(count) => self.add_count(|p| &mut p.opaque_pixels, count),
            ProbeEvent::PollStep => self.add_count(|p| &mut p.await_poll_steps, 1),
            ProbeEvent::ReadMadeProgress => self.add_count(|p| &mut p.await_reads_made_progress, 1),
            ProbeEvent::ProgressPacket => self.add_count(|p| &mut p.progress_packets, 1),
            ProbeEvent::OutputAlpha(value) => self.output_alpha = Some(value),
        }
    }
    #[inline]
    fn slept(&mut self, requested: Duration, start: Instant) {
        self.add_count(|p| &mut p.idle_sleep_calls, 1);
        if let Some(value) = self.idle_sleep_requested.checked_add(requested) {
            self.idle_sleep_requested = value;
        } else {
            self.counter_overflowed = true;
        }
        if let Some(value) = self.idle_sleep_elapsed.checked_add(start.elapsed()) {
            self.idle_sleep_elapsed = value;
        } else {
            self.counter_overflowed = true;
        }
    }
    #[inline]
    fn output_admission_checkpoint(
        &mut self,
        checkpoint: &mut dyn FnMut() -> Result<(), super::OpenFxHostError>,
    ) -> Result<(), super::OpenFxHostError> {
        let start = Instant::now();
        let result = checkpoint();
        let elapsed = start.elapsed();
        self.add_count(|p| &mut p.output_admission_checkpoint_calls, 1);
        if let Some(value) = self.output_admission_checkpoint_elapsed.checked_add(elapsed) {
            self.output_admission_checkpoint_elapsed = value;
        } else {
            self.counter_overflowed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        materialize_native_output, materialize_native_output_profiled, reserve_native_output,
        NativeOutputAllocation, OpenFxHostError, FRAME_TRANSFER_PIXELS,
    };
    use super::*;
    use mondrian_core::ColorFrameAlpha;
    use mondrian_effects::{
        EffectColorDomain, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
    };

    fn frame_format(alpha: ColorFrameAlpha) -> EffectValueFormat {
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            alpha,
        )
    }

    #[test]
    fn attribution_materialization_preserves_raw_signal_and_the_same_control_calls() {
        let count = FRAME_TRANSFER_PIXELS + 256;
        let extent = EffectFrameExtent::new(256, 257);
        for alpha in [
            ColorFrameAlpha::StraightFloat,
            ColorFrameAlpha::PremultipliedFloat,
            ColorFrameAlpha::Opaque,
        ] {
            let mut source = vec![[f32::MAX, -2.0, f32::from_bits(1), -0.5]; count];
            source[count - 1] = [-0.0, -f32::MAX, 16.0, 1.5];
            let make_owner = || {
                NativeOutputAllocation::acquire(extent, &mut || Ok(()), reserve_native_output)
                    .expect("pre-DSP output owner")
            };
            let mut ordinary_calls = 0;
            let ordinary = materialize_native_output(
                bytemuck::cast_slice(&source),
                make_owner(),
                frame_format(alpha),
                &mut || {
                    ordinary_calls += 1;
                    Ok(())
                },
            )
            .expect("ordinary sole frame admission");
            let mut measured_calls = 0;
            let mut report = OpenFxRenderDiagnostic::default();
            let measured = materialize_native_output_profiled(
                bytemuck::cast_slice(&source),
                make_owner(),
                frame_format(alpha),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                &mut || {
                    measured_calls += 1;
                    Ok(())
                },
                &mut report,
            )
            .expect("measured same frame admission");
            assert_eq!(measured.extent(), ordinary.extent());
            assert_eq!(measured.format(), ordinary.format());
            assert_eq!(
                measured_calls, ordinary_calls,
                "no additional owner callback"
            );
            assert!(measured
                .pixels()
                .iter()
                .zip(ordinary.pixels())
                .all(|(a, b)| a.map(f32::to_bits) == b.map(f32::to_bits)));
            assert_eq!(
                report.output_copy_bytes,
                count * std::mem::size_of::<[f32; 4]>()
            );
            assert_eq!(
                report.opaque_pixels,
                if alpha == ColorFrameAlpha::Opaque {
                    count
                } else {
                    0
                }
            );
            assert!(report.elapsed(OpenFxRenderPhase::OutputCopy).is_some());
            assert!(report.elapsed(OpenFxRenderPhase::OutputAdmission).is_some());
            assert_eq!(report.output_admission_checkpoint_calls, 3);
            assert!(
                report.output_admission_checkpoint_elapsed
                    <= report.elapsed(OpenFxRenderPhase::OutputAdmission).unwrap()
            );
            assert_eq!(
                report.elapsed(OpenFxRenderPhase::OpaqueCanonicalization).is_some(),
                alpha == ColorFrameAlpha::Opaque
            );
            assert!(!report.counter_overflowed);
        }
    }

    #[test]
    fn attribution_preserves_every_existing_materialization_stop_frontier() {
        let count = FRAME_TRANSFER_PIXELS + 256;
        let extent = EffectFrameExtent::new(256, 257);
        let source = vec![[2.0_f32, -3.0, 4.0, 0.0]; count];
        for alpha in [ColorFrameAlpha::StraightFloat, ColorFrameAlpha::Opaque] {
            for stop_at in 1..=12 {
                let make_owner = || {
                    NativeOutputAllocation::acquire(extent, &mut || Ok(()), reserve_native_output)
                        .expect("pre-DSP output owner")
                };
                let mut ordinary_calls = 0;
                let ordinary = materialize_native_output(
                    bytemuck::cast_slice(&source),
                    make_owner(),
                    frame_format(alpha),
                    &mut || {
                        ordinary_calls += 1;
                        if ordinary_calls == stop_at {
                            Err(OpenFxHostError::Canceled)
                        } else {
                            Ok(())
                        }
                    },
                );
                let mut measured_calls = 0;
                let mut report = OpenFxRenderDiagnostic::default();
                let measured = materialize_native_output_profiled(
                    bytemuck::cast_slice(&source),
                    make_owner(),
                    frame_format(alpha),
                    mondrian_core::ImageSampling::UNIT_SQUARE,
                    &mut || {
                        measured_calls += 1;
                        if measured_calls == stop_at {
                            Err(OpenFxHostError::Canceled)
                        } else {
                            Ok(())
                        }
                    },
                    &mut report,
                );
                assert_eq!(
                    ordinary_calls, measured_calls,
                    "same original stop frontier {stop_at}"
                );
                match (ordinary, measured) {
                    (Err(OpenFxHostError::Canceled), Err(OpenFxHostError::Canceled)) => {}
                    (Ok(a), Ok(b)) => assert!(a
                        .pixels()
                        .iter()
                        .zip(b.pixels())
                        .all(|(a, b)| a.map(f32::to_bits) == b.map(f32::to_bits))),
                    (a, b) => panic!("instrumentation changed original stop/result: {a:?} / {b:?}"),
                }
            }
        }
    }

    #[test]
    fn diagnostic_counter_exhaustion_is_explicit_and_never_wraps() {
        let mut report = OpenFxRenderDiagnostic {
            output_copy_bytes: usize::MAX,
            ..Default::default()
        };
        report.event(ProbeEvent::OutputCopied(1));
        assert!(report.counter_overflowed);
        assert_eq!(report.output_copy_bytes, usize::MAX);
        assert_eq!(report.sequence, None);
        assert!(!report.accepted_output);
        assert_eq!(report.elapsed(OpenFxRenderPhase::AwaitCompletion), None);
    }
}
