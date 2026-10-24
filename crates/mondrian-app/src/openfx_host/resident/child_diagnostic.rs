//! Fixed default-off child/native timing. Neither report is execution evidence.
use super::*;

/// A disjoint segment on the existing child dispatch thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum OpenFxChildRenderPhase {
    /// Timing, schema, curve and current-frame query admission.
    RequestAdmission,
    /// Existing complete selected-module fingerprint.
    BinaryIdentity,
    /// Existing C-string, query and parameter-array preparation.
    ParameterArena,
    /// Cold native construction only; missing on actual warm reuse.
    NativeCreate,
    /// Native ABI call and bounded report decode, inclusive of native subspans.
    NativeDispatch,
    /// Exact native feedback, query ownership and terminal value preparation.
    TerminalPreparation,
}
impl OpenFxChildRenderPhase {
    /// Stable disjoint order; native phases nest inside NativeDispatch.
    pub const ALL: [Self; 6] = [
        Self::RequestAdmission,
        Self::BinaryIdentity,
        Self::ParameterArena,
        Self::NativeCreate,
        Self::NativeDispatch,
        Self::TerminalPreparation,
    ];
    /// Stable diagnostic label, not an action or owner identifier.
    pub const fn name(self) -> &'static str {
        match self {
            Self::RequestAdmission => "request_admission",
            Self::BinaryIdentity => "binary_identity",
            Self::ParameterArena => "parameter_arena",
            Self::NativeCreate => "native_create_cold",
            Self::NativeDispatch => "native_dispatch_inclusive",
            Self::TerminalPreparation => "terminal_preparation",
        }
    }
}
/// Nonoverlapping native-body segments; Output clear is a nested observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum OpenFxNativeRenderPhase {
    /// Session metadata, complete raw input validation and image binding/UIDs.
    SessionAdmission,
    /// Bounded suite invocation setup.
    InvocationSetup,
    /// Parameters and their actual InstanceChanged actions.
    ParameterActions,
    /// Actual clip-preference action and negotiated format admission.
    ClipPreferences,
    /// Existing Output fetch reset and canonical/pixel render geometry.
    FrameSetup,
    /// Plugin BeginSequenceRender action inclusive.
    BeginSequence,
    /// Plugin ROI action inclusive.
    RegionOfInterest,
    /// Plugin Render inclusive of host calls, clear and callback computations.
    PluginRender,
    /// Existing complete raw physical-lane finite validation.
    OutputValidation,
    /// Plugin EndSequenceRender action inclusive.
    EndSequence,
    /// Existing fatal/blocked/frame disposition, without changing semantics.
    Disposition,
    /// Actual bounded auxiliary report copy.
    AuxiliaryReport,
}
impl OpenFxNativeRenderPhase {
    /// Stable fixed measurement order, with no per-frame map allocation.
    pub const ALL: [Self; 12] = [
        Self::SessionAdmission,
        Self::InvocationSetup,
        Self::ParameterActions,
        Self::ClipPreferences,
        Self::FrameSetup,
        Self::BeginSequence,
        Self::RegionOfInterest,
        Self::PluginRender,
        Self::OutputValidation,
        Self::EndSequence,
        Self::Disposition,
        Self::AuxiliaryReport,
    ];
    /// Stable diagnostic label. PluginRender is not pure algorithm/DSP time.
    pub const fn name(self) -> &'static str {
        match self {
            Self::SessionAdmission => "session_admission",
            Self::InvocationSetup => "invocation_setup",
            Self::ParameterActions => "parameter_actions",
            Self::ClipPreferences => "clip_preferences",
            Self::FrameSetup => "frame_setup",
            Self::BeginSequence => "begin_sequence",
            Self::RegionOfInterest => "region_of_interest",
            Self::PluginRender => "plugin_render_inclusive",
            Self::OutputValidation => "output_validation",
            Self::EndSequence => "end_sequence",
            Self::Disposition => "disposition",
            Self::AuxiliaryReport => "auxiliary_report",
        }
    }
}

// Mirrors render_diagnostic.h exactly. Integer fields have valid all-zero bits.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeRenderDiagnostic {
    present_mask: u32,
    counter_overflowed: u32,
    total_ns: u64,
    output_clear_ns: u64,
    output_clear_bytes: u64,
    output_clear_calls: u64,
    phase_ns: [u64; 12],
}
impl NativeRenderDiagnostic {
    fn validate(&self, _frame_bytes: usize) -> Result<(), OpenFxHostError> {
        let all = (1_u32 << 12) - 1;
        if self.present_mask & !all != 0
            || self.counter_overflowed > 1
            || self.output_clear_calls > 1
            || (self.output_clear_calls == 0
                && (self.output_clear_bytes != 0 || self.output_clear_ns != 0))
            || (self.output_clear_calls == 1
                && (self.output_clear_bytes == 0
                    || self.output_clear_bytes % 16 != 0
                    || self.output_clear_bytes > super::super::MAX_FRAME_BYTES as u64))
        {
            return Err(unavailable(
                "native diagnostic violated bounded image storage contract",
            ));
        }
        if self.counter_overflowed == 0 {
            let sum = self
                .phase_ns
                .iter()
                .enumerate()
                .try_fold(0_u64, |sum, (index, value)| {
                    if self.present_mask & (1 << index) == 0 && *value != 0 {
                        return None;
                    }
                    sum.checked_add(*value)
                })
                .ok_or_else(|| unavailable("native diagnostic phase sum is invalid"))?;
            if sum > self.total_ns
                || self.output_clear_ns
                    > self.phase_ns[OpenFxNativeRenderPhase::PluginRender as usize]
            {
                return Err(unavailable("native diagnostic nesting is invalid"));
            }
        }
        Ok(())
    }
}

/// One fixed report from the current child request. It owns no raster or worker.
/// Total ends before pump completion/serialization/write; those costs are not
/// retrospectively measured by the packet which carries this report.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenFxChildRenderDiagnostic {
    owner: mondrian_core::EffectProcessorOwnerId,
    sequence: u64,
    native_owner: u64,
    invocation: u64,
    input_bytes: u64,
    output_alpha: Option<mondrian_core::ColorFrameAlpha>,
    counter_overflowed: bool,
    dispatch_elapsed_ns: Option<u64>,
    phase_ns: [Option<u64>; 6],
    native: Option<NativeRenderDiagnostic>,
}
impl OpenFxChildRenderDiagnostic {
    pub(super) fn new(
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
        bytes: usize,
    ) -> Self {
        Self {
            owner,
            sequence,
            native_owner: 0,
            invocation: 0,
            input_bytes: bytes as u64,
            output_alpha: None,
            counter_overflowed: false,
            dispatch_elapsed_ns: None,
            phase_ns: [None; 6],
            native: None,
        }
    }
    /// Actual parent-issued instance namespace, not an authored effect ID.
    pub const fn owner(&self) -> mondrian_core::EffectProcessorOwnerId {
        self.owner
    }
    /// Original Render wire/action serial; separate from native Render count.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Actual native SDK invocation observed in this response's feedback.
    pub const fn invocation(&self) -> u64 {
        self.invocation
    }
    /// Completed disjoint child span, missing when that phase did not complete.
    pub const fn elapsed_ns(&self, phase: OpenFxChildRenderPhase) -> Option<u64> {
        self.phase_ns[phase as usize]
    }
    /// Native subspan within child NativeDispatch. Missing is not zero elapsed.
    pub fn native_elapsed_ns(&self, phase: OpenFxNativeRenderPhase) -> Option<u64> {
        self.native
            .as_ref()
            .filter(|v| v.present_mask & (1 << (phase as usize)) != 0)
            .map(|v| v.phase_ns[phase as usize])
    }
    /// Existing Output clear nested inside PluginRender, or zero if no clear ran.
    pub fn output_clear_ns(&self) -> Option<u64> {
        self.native.as_ref().map(|v| v.output_clear_ns)
    }
    /// Actual bytes initialized on the first legal Output fetch.
    pub fn output_clear_bytes(&self) -> Option<u64> {
        self.native.as_ref().map(|v| v.output_clear_bytes)
    }
    /// Native CABI body including its RAII cleanup; nested inside NativeDispatch.
    pub fn native_total_ns(&self) -> Option<u64> {
        self.native.as_ref().map(|v| v.total_ns)
    }
    /// Total from accepted child dispatch through terminal value preparation.
    pub const fn dispatch_elapsed_ns(&self) -> Option<u64> {
        self.dispatch_elapsed_ns
    }
    /// Diagnostic overflow is reported, never converted to a plugin DSP fault.
    pub fn counter_overflowed(&self) -> bool {
        self.counter_overflowed || self.native.as_ref().is_some_and(|v| v.counter_overflowed != 0)
    }
    pub(super) fn validate(
        &self,
        owner: mondrian_core::EffectProcessorOwnerId,
        sequence: u64,
        feedback: &mondrian_effects::EffectProcessorFeedbackSnapshot,
        alpha: Option<mondrian_core::ColorFrameAlpha>,
        bytes: usize,
    ) -> Result<(), OpenFxHostError> {
        if self.owner != owner
            || self.sequence != sequence
            || self.native_owner != feedback.native_owner()
            || self.invocation != feedback.invocation()
            || self.input_bytes != bytes as u64
            || self.output_alpha != alpha
            || self.native.is_none()
            || (!self.counter_overflowed && self.dispatch_elapsed_ns.is_none())
        {
            return Err(unavailable(
                "child diagnostic lost its exact Render correlation",
            ));
        }
        if !self.counter_overflowed {
            let sum = self
                .phase_ns
                .iter()
                .flatten()
                .try_fold(0_u64, |sum, value| sum.checked_add(*value))
                .ok_or_else(|| unavailable("child diagnostic phase sum overflowed"))?;
            if Some(sum) > self.dispatch_elapsed_ns {
                return Err(unavailable("child diagnostic spans overlap"));
            }
        }
        if let Some(native) = &self.native {
            native.validate(bytes)?;
        }
        Ok(())
    }
    pub(super) fn attach(mut self, response: Response, start: Instant) -> Response {
        self.dispatch_elapsed_ns = duration_ns(start.elapsed());
        self.counter_overflowed |= self.dispatch_elapsed_ns.is_none();
        match response {
            Response::Rendered { sequence, alpha, feedback, .. } => {
                self.native_owner = feedback.native_owner();
                self.invocation = feedback.invocation();
                self.output_alpha = Some(alpha);
                Response::Rendered { sequence, alpha, feedback, diagnostic: Some(self) }
            }
            Response::UserBlocked { sequence, feedback, .. } => {
                self.native_owner = feedback.native_owner();
                self.invocation = feedback.invocation();
                Response::UserBlocked { sequence, feedback, diagnostic: Some(self) }
            }
            Response::PreflightBudgetRefused { sequence, required_bytes, feedback, .. } => {
                self.native_owner = feedback.native_owner();
                self.invocation = feedback.invocation();
                self.output_alpha = None;
                Response::PreflightBudgetRefused {
                    sequence,
                    required_bytes,
                    feedback,
                    diagnostic: Some(self),
                }
            }
            other => other,
        }
    }
}
fn duration_ns(value: Duration) -> Option<u64> {
    u64::try_from(value.as_nanos()).ok()
}
pub(super) trait ChildProbe {
    type Stamp;
    fn start(&mut self) -> Self::Stamp;
    fn finish(&mut self, phase: OpenFxChildRenderPhase, start: Self::Stamp);
    fn native_target(&mut self) -> Option<&mut NativeRenderDiagnostic>;
}
impl ChildProbe for () {
    type Stamp = ();
    #[inline(always)]
    fn start(&mut self) {}
    #[inline(always)]
    fn finish(&mut self, _phase: OpenFxChildRenderPhase, _start: ()) {}
    #[inline(always)]
    fn native_target(&mut self) -> Option<&mut NativeRenderDiagnostic> {
        None
    }
}
impl ChildProbe for OpenFxChildRenderDiagnostic {
    type Stamp = Instant;
    fn start(&mut self) -> Instant {
        Instant::now()
    }
    fn finish(&mut self, phase: OpenFxChildRenderPhase, start: Instant) {
        let value = duration_ns(start.elapsed());
        self.counter_overflowed |= value.is_none();
        self.phase_ns[phase as usize] = value;
    }
    fn native_target(&mut self) -> Option<&mut NativeRenderDiagnostic> {
        self.native = Some(NativeRenderDiagnostic::default());
        self.native.as_mut()
    }
}
pub(super) fn required_optional<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<OpenFxChildRenderDiagnostic>, D::Error> {
    Deserialize::deserialize(d)
}
// Explicit fixed local owners not already included by sizeof(Shared/Response):
// two dispatch reports, parent receive/result reports, native callback POD, and
// the single borrowed FrameState diagnostic pointer. This is logical capacity,
// not allocator/serde/OS/CRT/SDK/plugin whole RSS.
pub(crate) fn storage_bytes() -> Option<usize> {
    std::mem::size_of::<OpenFxChildRenderDiagnostic>()
        .checked_mul(4)?
        .checked_add(std::mem::size_of::<NativeRenderDiagnostic>())?
        .checked_add(std::mem::size_of::<*const NativeRenderDiagnostic>())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn feedback() -> mondrian_effects::EffectProcessorFeedbackSnapshot {
        mondrian_effects::EffectProcessorFeedbackSnapshot::new(13, 5, 0, None, None, Vec::new())
            .expect("exact native facts")
    }
    fn report() -> OpenFxChildRenderDiagnostic {
        let mut value =
            OpenFxChildRenderDiagnostic::new(mondrian_core::EffectProcessorOwnerId::new(), 7, 16);
        value.native_owner = 13;
        value.invocation = 5;
        value.output_alpha = Some(mondrian_core::ColorFrameAlpha::StraightFloat);
        value.dispatch_elapsed_ns = Some(30);
        value.phase_ns[OpenFxChildRenderPhase::NativeDispatch as usize] = Some(20);
        value.native = Some(NativeRenderDiagnostic {
            present_mask: 1 << 7,
            total_ns: 15,
            output_clear_ns: 3,
            output_clear_bytes: 16,
            output_clear_calls: 1,
            phase_ns: {
                let mut v = [0; 12];
                v[7] = 10;
                v
            },
            counter_overflowed: 0,
        });
        value
    }
    #[test]
    fn fixed_native_layout_and_default_probe_have_no_clock_or_raster_owner() {
        assert_eq!(std::mem::size_of::<NativeRenderDiagnostic>(), 136);
        assert_eq!(std::mem::offset_of!(NativeRenderDiagnostic, phase_ns), 40);
        assert_eq!(
            std::mem::align_of::<NativeRenderDiagnostic>(),
            std::mem::align_of::<u64>()
        );
        let mut probe = ();
        let _: () = ChildProbe::start(&mut probe);
        assert!(ChildProbe::native_target(&mut probe).is_none());
        assert!(!ParentProbe::measure_child(&probe));
        assert_eq!(std::mem::size_of::<<() as ChildProbe>::Stamp>(), 0);
    }
    #[test]
    fn exact_owner_action_native_invocation_and_bytes_are_not_interchangeable() {
        let value = report();
        let feedback = feedback();
        let alpha = value.output_alpha;
        value
            .validate(value.owner, 7, &feedback, alpha, 16)
            .expect("actual same coordinates despite wire7/native5");
        for wrong in [0, 1, 2, 3, 4] {
            let mut bad = value;
            match wrong {
                0 => bad.owner = mondrian_core::EffectProcessorOwnerId::new(),
                1 => bad.sequence = 5,
                2 => bad.invocation = 7,
                3 => bad.native_owner = 14,
                4 => bad.input_bytes = 32,
                _ => unreachable!(),
            }
            assert!(bad.validate(value.owner, 7, &feedback, alpha, 16).is_err());
        }
        assert!(value
            .validate(
                value.owner,
                7,
                &feedback,
                Some(mondrian_core::ColorFrameAlpha::Opaque),
                16
            )
            .is_err());
        assert!(
            admit_child_diagnostic(Some(value), value.owner, 7, &feedback, alpha, 16, &mut ())
                .is_err(),
            "ordinary request cannot accept unsolicited child probe"
        );
        let mut parent = OpenFxRenderDiagnostic::default();
        assert!(
            admit_child_diagnostic(None, value.owner, 7, &feedback, alpha, 16, &mut parent)
                .is_err()
        );
        admit_child_diagnostic(
            Some(value),
            value.owner,
            7,
            &feedback,
            alpha,
            16,
            &mut parent,
        )
        .expect("original selected diagnostic");
        assert_eq!(parent.child.expect("fixed report").sequence(), 7);
    }
    #[test]
    fn invalid_nested_spans_and_clear_bytes_refuse_but_clock_overflow_is_not_a_plugin_fault() {
        let value = report();
        let feedback = feedback();
        for wrong in [0, 1, 2, 3, 4, 5] {
            let mut bad = value;
            let native = bad.native.as_mut().expect("fixed native report");
            match wrong {
                0 => native.present_mask |= 1 << 12,
                1 => native.total_ns = 9,
                2 => native.output_clear_ns = 11,
                3 => native.output_clear_bytes = super::super::super::MAX_FRAME_BYTES as u64 + 16,
                4 => native.output_clear_calls = 2,
                5 => native.counter_overflowed = 2,
                _ => unreachable!(),
            }
            assert!(bad.validate(value.owner, 7, &feedback, value.output_alpha, 16).is_err());
        }
        let mut expanded_domain = value;
        expanded_domain.native.as_mut().unwrap().output_clear_bytes = 32;
        expanded_domain
            .validate(value.owner, 7, &feedback, value.output_alpha, 16)
            .expect("native full RoD can exceed the publication viewport within its active grant");
        let mut overflow = value;
        overflow.counter_overflowed = true;
        overflow.dispatch_elapsed_ns = None;
        overflow.native.as_mut().expect("native").counter_overflowed = 1;
        overflow
            .validate(value.owner, 7, &feedback, value.output_alpha, 16)
            .expect("diagnostic counter refusal does not alter valid DSP");
        assert!(overflow.counter_overflowed());
    }
    #[test]
    fn current_wire_requires_explicit_probe_selection_and_terminal_optional_field() {
        let base = serde_json::json!({"kind":"render","owner":mondrian_core::EffectProcessorOwnerId::new(),"sequence":1,
            "gui_open":false,"interactive_render":false,"question_work":false,"timing":{"frame":0.0,"frame_rate":24.0,"first_frame":0.0,"last_frame":9.0},"sampling":mondrian_core::ImageSampling::UNIT_SQUARE,"parameters":{}});
        assert!(serde_json::from_value::<Request>(base.clone()).is_err());
        let mut current = base;
        current["diagnostic"] = serde_json::json!(false);
        assert!(serde_json::from_value::<Request>(current).is_ok());
        let base = serde_json::json!({"kind":"rendered","sequence":1,"alpha":"straight_float","feedback":feedback()});
        assert!(serde_json::from_value::<Response>(base.clone()).is_err());
        let mut current = base;
        current["diagnostic"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Response>(current).is_ok());
    }

    unsafe extern "C" fn count_abort(context: *const c_void) -> c_int {
        if context.is_null() {
            return 0;
        }
        // SAFETY: this exact atomic is borrowed until the synchronous action and
        // all legal callback threads complete; no borrowed FnMut is invoked.
        let counter = unsafe { &*context.cast::<std::sync::atomic::AtomicUsize>() };
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        0
    }
    #[test]
    #[ignore = "requires pinned official Basic binary; supervised native qualification"]
    fn native_cpp_child_attribution_preserves_bits_and_control_calls() {
        const TEST:&str="openfx_host::resident::child_diagnostic::tests::native_cpp_child_attribution_preserves_bits_and_control_calls";
        if !shared_frame_tests::qualification_child(TEST, &[]) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic"),
        ))
        .expect("selected binary");
        let bundle = path
            .ancestors()
            .find(|v| v.extension().is_some_and(|e| e == "bundle"))
            .expect("bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle).expect("bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let name = CString::new("scale").expect("actual parameter");
        let parameters = [NativeParameter {
            name: name.as_ptr(),
            query_mode: 0,
            kind: 1,
            value: [0.5, 0.0, 0.0, 0.0],
            string_value: std::ptr::null(),
            string_length: 0,
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
        }];
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 2,
            frame: 1.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 9.0,
            pixel_aspect_ratio: 1.0,

            project_width: 1,
            project_height: 2,
            project_pixel_aspect_ratio: 1.0,
        };
        let samples = [
            [-0.25_f32, 2.0, -16.0, 1.5],
            [f32::MAX, -f32::MAX, f32::from_bits(2), -0.5],
        ];
        let input = bytemuck::cast_slice(&samples);
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            input,
            &info,
            &parameters,
        )
        .expect("actual sole instance");
        let counter = std::sync::atomic::AtomicUsize::new(0);
        let control = NativeInvocationControl {
            context: std::ptr::from_ref(&counter).cast(),
            abort: Some(count_abort),
            progress: None,
            gui_open: 0,
            interactive_render: 0,
            question_work: 0,
            question: None,
            acquire_memory: None,
        };
        let mut ordinary = vec![[0_f32; 4]; samples.len()];
        let mut measured = ordinary.clone();
        assert_eq!(
            session.render_controlled(
                input,
                bytemuck::cast_slice_mut(&mut ordinary),
                &info,
                &parameters,
                Some(&control)
            ),
            Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
        );
        let first = session.take_feedback().expect("first actual render");
        let calls = counter.swap(0, std::sync::atomic::Ordering::Relaxed);
        let mut native = NativeRenderDiagnostic::default();
        assert_eq!(
            session.render_with_native_diagnostic(
                input,
                bytemuck::cast_slice_mut(&mut measured),
                &info,
                &parameters,
                Some(&control),
                Some(&mut native)
            ),
            Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
        );
        let second = session.take_feedback().expect("second actual render");
        assert_eq!(
            ordinary, measured,
            "bit-equivalent sole native law including negative/HDR/subnormal/signed-alpha"
        );
        let expected = samples.map(|v| v.map(|x| x * 0.5));
        assert!(
            ordinary
                .iter()
                .zip(expected)
                .all(|(a, b)| a.map(f32::to_bits) == b.map(f32::to_bits)),
            "independent gain oracle"
        );
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            calls,
            "no added SDK/owner callbacks"
        );
        assert_eq!(first.native_owner(), second.native_owner());
        assert_eq!(second.invocation(), first.invocation() + 1);
        assert_eq!(native.present_mask, (1 << 12) - 1);
        assert_eq!(native.counter_overflowed, 0);
        assert_eq!(native.output_clear_calls, 1);
        native
            .validate(input.len())
            .expect("actual truthful nested durations and exact clear bytes");
    }
}
