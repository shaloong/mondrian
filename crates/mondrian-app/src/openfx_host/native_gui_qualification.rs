//! Native ABI post-contracts. They require C1; they are not old-API RED gates.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn native_gui_control_has_exact_cabi_field_alignment() {
    let pointer = std::mem::size_of::<*const c_void>();
    let alignment = std::mem::align_of::<*const c_void>();
    let fields = 3 * pointer + 3 * std::mem::size_of::<c_int>();
    let callback = fields.div_ceil(alignment) * alignment;
    assert_eq!(
        std::mem::offset_of!(NativeInvocationControl, gui_open),
        3 * pointer
    );
    assert_eq!(
        std::mem::offset_of!(NativeInvocationControl, interactive_render),
        3 * pointer + 4
    );
    assert_eq!(
        std::mem::offset_of!(NativeInvocationControl, question_work),
        3 * pointer + 8
    );
    assert_eq!(
        std::mem::offset_of!(NativeInvocationControl, question),
        callback
    );
    assert_eq!(
        std::mem::size_of::<NativeInvocationControl>(),
        callback + pointer
    );
    assert_eq!(std::mem::align_of::<NativeInvocationControl>(), alignment);
}

struct Replies {
    status: c_int,
    calls: AtomicUsize,
}
unsafe extern "C" fn answer(context: *const c_void, question: *const NativeLiveQuestion) -> c_int {
    if context.is_null() || question.is_null() {
        return 1;
    }
    // SAFETY: this exact immutable Replies remains live until synchronous native
    // dispatch returns. Its only mutated field is atomic, including foreign calls.
    let (replies, question) = unsafe { (&*context.cast::<Replies>(), &*question) };
    if question.owner == 0 || question.invocation == 0 {
        return 1;
    }
    replies.calls.fetch_add(1, Ordering::Relaxed);
    replies.status
}

#[test]
#[ignore = "requires owned C1 GUI/Question SessionProbe modes"]
fn native_generic_gui_pairs_failed_begin_end_and_honest_question_replies() {
    const TEST: &str = "openfx_host::native_gui_qualification::native_generic_gui_pairs_failed_begin_end_and_honest_question_replies";
    for (mode, reply) in [
        ("gui_failed_begin", 1),
        ("gui_question_unavailable", 1),
        ("gui_question_yes", 12),
        ("gui_question_no", 13),
        ("gui_question_foreign", 12),
        ("gui_cycle", 1),
    ] {
        if !shared_frame_tests::qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", mode)],
        ) {
            continue;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned fixture"),
        ))
        .expect("binary");
        let bundle = path
            .ancestors()
            .find(|value| value.extension().is_some_and(|extension| extension == "bundle"))
            .expect("bundle");
        let binary = native_string(&path).expect("binary path");
        let bundle = native_string(bundle).expect("bundle path");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let name = CString::new("scale").expect("scale");
        let parameters = [NativeParameter {
            name: name.as_ptr(),
            query_mode: 0,
            kind: 1,
            value: [2.0, 0.0, 0.0, 0.0],
            string_value: std::ptr::null(),
            string_length: 0,
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
        }];
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,
        };
        let input = [0.125_f32, -2.0, 16.0, 0.5]
            .iter()
            .flat_map(|sample| sample.to_ne_bytes())
            .collect::<Vec<_>>();
        let library =
            unsafe { libloading::Library::new(&path) }.expect("retain test counter module");
        type Receipt = unsafe extern "C" fn(c_int) -> c_int;
        let receipt: libloading::Symbol<'_, Receipt> =
            unsafe { library.get(b"SessionProbeGuiReceipt\0") }
                .expect("C-linked independent fixture receipt");
        let counts = || [0, 1, 2, 3].map(|index| unsafe { receipt(index) });
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &parameters,
        )
        .expect("same native instance");
        let replies = Replies { status: reply, calls: AtomicUsize::new(0) };
        let control = NativeInvocationControl {
            context: std::ptr::from_ref(&replies).cast(),
            abort: None,
            progress: None,
            gui_open: 1,
            interactive_render: 0,
            question_work: 1,
            question: Some(answer),
        };
        let mut output = vec![0_u8; input.len()];
        assert_eq!(
            session.render_controlled(&input, &mut output, &info, &parameters, Some(&control)),
            Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
        );
        assert_eq!(
            counts(),
            [1, 0, 1, 1],
            "called Begin remains open even after legal Failed"
        );
        assert_eq!(replies.calls.load(Ordering::Relaxed), 1);
        let actual = output
            .chunks_exact(4)
            .map(|bytes| f32::from_ne_bytes(bytes.try_into().expect("f32")))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [0.25, -4.0, 32.0, 1.0],
            "Failed Question permits legal Basic output"
        );
        let first = session.take_feedback().expect("actual first Render report");
        assert_eq!(first.invocation(), 1);
        let end = session
            .close_gui()
            .expect("legal Failed End closes the same instance")
            .expect("actual called End has separate action feedback");
        assert_eq!(end.native_owner(), first.native_owner());
        assert_eq!(end.invocation(), 1, "End did not invent another Render");
        assert_eq!(
            counts(),
            [1, 1, 1, 0],
            "no DSP replay during idle GUI closure"
        );
        assert!(session.close_gui().expect("idempotent closed GUI").is_none());
        assert_eq!(counts(), [1, 1, 1, 0]);
        let expected = if mode == "gui_cycle" {
            assert_eq!(end.notices().len(), 1);
            assert_eq!(end.notices()[0].text(), "Actual End 1");
            assert_eq!(
                session.render_controlled(&input, &mut output, &info, &parameters, Some(&control)),
                Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
            );
            let next = session.take_feedback().expect("actual second Render");
            assert_eq!(next.native_owner(), first.native_owner());
            assert_eq!(next.invocation(), 2);
            assert!(
                next.notices().is_empty(),
                "prior End notices cannot be relabeled as this frame"
            );
            assert_eq!(counts(), [2, 1, 2, 1]);
            let end = session.close_gui().expect("second End").expect("actual second action");
            assert_eq!(end.invocation(), 2);
            assert_eq!(end.notices()[0].text(), "Actual End 2");
            [2, 2, 2, 0]
        } else {
            assert!(end.notices().is_empty());
            [1, 1, 1, 0]
        };
        drop(session);
        assert_eq!(counts(), expected, "destruction never duplicates End");
    }
}

/// Actual compiled graph/factory/residency qualification. The test service is
/// explicitly a native-host contract fixture, not physical App GUI evidence.
#[test]
#[ignore = "requires current C1 product and owned gui_cycle SessionProbe"]
fn actual_bank_render_idle_end_render_preserves_native_owner_and_distinct_action_identity() {
    use mondrian_effects::{EffectProcessorGuiPresence, EffectProcessorQuestionService};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    #[derive(Debug)]
    struct Presence {
        open: AtomicBool,
        action: Mutex<Option<mondrian_effects::EffectProcessorGuiActionReceipt>>,
    }
    impl EffectProcessorGuiPresence for Presence {
        fn is_open(&self) -> bool {
            self.open.load(Ordering::Acquire)
        }
        fn observe_action(
            &self,
            receipt: mondrian_effects::EffectProcessorGuiActionReceipt,
        ) -> bool {
            let Ok(mut slot) = self.action.lock() else {
                return false;
            };
            if slot.is_some() {
                return false;
            }
            *slot = Some(receipt);
            true
        }
    }
    #[derive(Debug)]
    struct Service {
        presence: Arc<Presence>,
    }
    impl EffectProcessorQuestionService for Service {
        fn maximum_storage_bytes(&self) -> usize {
            std::mem::size_of::<Self>()
                + std::mem::size_of::<Presence>()
                + mondrian_effects::EffectProcessorFeedbackSnapshot::maximum_retained_bytes()
        }
        fn gui_state(
            &self,
            _: mondrian_effects::EffectFloatProcessorPreparation,
            _: u64,
        ) -> mondrian_effects::EffectProcessorGuiState {
            if self.presence.is_open() {
                mondrian_effects::EffectProcessorGuiState::Open
            } else {
                mondrian_effects::EffectProcessorGuiState::Closed
            }
        }
        fn gui_presence(
            &self,
            _: mondrian_effects::EffectFloatProcessorPreparation,
            _: u64,
            _: mondrian_core::EffectProcessorOwnerId,
        ) -> Option<std::sync::Weak<dyn EffectProcessorGuiPresence>> {
            let presence: Arc<dyn EffectProcessorGuiPresence> = self.presence.clone();
            Some(Arc::downgrade(&presence))
        }
        fn question(
            &self,
            _: mondrian_effects::EffectProcessorQuestionView<'_>,
        ) -> mondrian_effects::EffectProcessorQuestionState {
            mondrian_effects::EffectProcessorQuestionState::Unavailable
        }
        fn finish_question(&self, _: mondrian_effects::EffectProcessorQuestionView<'_>) {}
    }
    #[derive(Debug)]
    struct Observer(Service);
    impl mondrian_effects::EffectProcessorObserver for Observer {
        fn maximum_storage_bytes(&self) -> usize {
            self.0.maximum_storage_bytes()
        }
        fn observe(&self, _: &mondrian_effects::EffectProcessorProgressReceipt) {}
        fn question_service(&self) -> Option<&dyn EffectProcessorQuestionService> {
            Some(&self.0)
        }
    }
    mondrian_core::ensure_mondrian_default_ocio_loaded().expect("canonical stock OCIO");
    assert_eq!(
        std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("owned mode"),
        "gui_cycle"
    );
    let helper = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_GUI_TEST_HELPER").expect("fresh current C1 product"),
    );
    let binary = PathBuf::from(
        std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned reference"),
    );
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary)
        .expect("actual selected binary");
    let kind = crate::openfx_effect::register_selected_openfx_filter(
        &helper,
        &inspection,
        "uk.co.thefoundry.BasicGainPlugin",
    )
    .expect("actual native recipe");
    let effect =
        mondrian_effects::instantiate_effect_node(kind).expect("actual authored occurrence");
    let context = mondrian_effects::EffectFrameContext::new(
        mondrian_core::Rational::FPS_24,
        mondrian_core::SampleAspectRatio::SQUARE,
        mondrian_core::TimelineTimeRange::new(
            mondrian_core::TimelineTime::ZERO,
            mondrian_core::TimelineTime::new(10, 24).expect("duration"),
        )
        .expect("nonempty sequence range"),
    )
    .expect("exact frame context");
    let program = mondrian_effects::PreparedEffectProgram::prepare_hierarchical_with_frame_context(
        std::slice::from_ref(&effect),
        &[],
        &[],
        &[],
        mondrian_core::WorkingColorSpace::LinearRec709,
        context,
        &mondrian_effects::LutPreparationCache::uncached(),
    )
    .expect("sole program");
    let input = mondrian_effects::EffectFloatFrame::from_pixels(
        mondrian_effects::EffectFrameExtent::new(1, 1),
        mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        ),
        vec![[0.125, -2.0, 16.0, 0.5]],
    )
    .expect("finite independent HDR/negative input");
    let presence = Arc::new(Presence {
        open: AtomicBool::new(true),
        action: Mutex::new(None),
    });
    let observer: Arc<dyn mondrian_effects::EffectProcessorObserver> =
        Arc::new(Observer(Service { presence: presence.clone() }));
    let mut session = mondrian_effects::EffectExecutionSession::new(
        mondrian_effects::EffectExecutionSessionConfig {
            max_processor_instances: 1,
            max_processor_bytes: 64 * 1024 * 1024,
            max_working_bytes: 128 * 1024 * 1024,
            max_cache_bytes: 0,
            max_cache_entries: 0,
            ..Default::default()
        },
    );
    session.bind_generation(7);
    session.bind_processor_observation(mondrian_effects::EffectProcessorObservation {
        sink: Some(Arc::clone(&observer)),
        deadline: None,
    });
    let mut previous: Option<mondrian_effects::EffectProcessorFeedbackReceipt> = None;
    for invocation in [1_u64, 2] {
        presence.open.store(true, Ordering::Release);
        let graph = program
            .evaluate(
                mondrian_core::TimelineTime::new((invocation - 1) as i64, 24).expect("exact time"),
            )
            .expect("same occurrence graph");
        let mut control = mondrian_effects::EffectProcessorControl::uncancelled();
        control.bind_observation(mondrian_effects::EffectProcessorObservation {
            sink: Some(Arc::clone(&observer)),
            deadline: None,
        });
        let output = session
            .apply_compiled_rgba_f32(input.view(), &graph, 0, &mut control)
            .expect("legal Basic after Failed Question");
        assert_eq!(
            output.pixels(),
            input.pixels(),
            "independent unit-gain analytic pixels"
        );
        let reports = control.take_feedback().expect("actual bank feedback");
        assert_eq!(reports.len(), 1);
        let receipt = reports.iter().next().expect("one executed native occurrence").clone();
        assert_eq!(
            receipt.feedback.snapshot.invocation(),
            invocation,
            "native invocation differs from wire3 on second render"
        );
        assert!(
            receipt.feedback.snapshot.notices().is_empty(),
            "prior End not relabeled as this Render"
        );
        assert_eq!(
            receipt.retention,
            mondrian_effects::EffectProcessorRetention::Retained
        );
        if let Some(previous) = &previous {
            assert_eq!(receipt.feedback.owner, previous.feedback.owner);
            assert_eq!(
                receipt.feedback.origin, previous.feedback.origin,
                "same actual child PID"
            );
            assert_eq!(
                receipt.feedback.snapshot.native_owner(),
                previous.feedback.snapshot.native_owner()
            );
            assert_eq!(receipt.preparation, previous.preparation);
            assert_eq!(receipt.provider_revision, previous.provider_revision);
        }
        presence.open.store(false, Ordering::Release);
        assert_eq!(
            session.reconcile_processor_guis(),
            0,
            "valid owner remains in canonical bank"
        );
        assert_eq!(session.diagnostics().processor_instances, 1);
        let action = presence
            .action
            .lock()
            .expect("same existing bounded result")
            .take()
            .expect("actual End delivery");
        assert_eq!(action.action_serial, invocation * 2);
        assert_eq!(action.receipt.feedback.owner, receipt.feedback.owner);
        assert_eq!(action.receipt.feedback.origin, receipt.feedback.origin);
        assert_eq!(
            action.receipt.feedback.snapshot.invocation(),
            invocation,
            "idle End did not invent Render"
        );
        assert_eq!(action.receipt.feedback.snapshot.notices().len(), 1);
        assert_eq!(
            action.receipt.feedback.snapshot.notices()[0].text(),
            format!("Actual End {invocation}")
        );
        assert_eq!(
            action.receipt.retention,
            mondrian_effects::EffectProcessorRetention::Retained
        );
        assert_eq!(session.reconcile_processor_guis(), 0);
        assert!(
            presence.action.lock().expect("slot").is_none(),
            "no duplicate End/DSP on repeated idle reconcile"
        );
        previous = Some(receipt);
    }
    session.retire_processor_resources();
    assert_eq!(session.diagnostics().processor_instances, 0);
    assert!(
        presence.action.lock().expect("slot").is_none(),
        "destruction cannot duplicate End notices"
    );
}
