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
        std::mem::offset_of!(NativeInvocationControl, acquire_memory),
        callback + pointer
    );
    assert_eq!(
        std::mem::size_of::<NativeInvocationControl>(),
        callback + 2 * pointer
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
            render_scale: [1.0; 2],

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 1.0,
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
            acquire_memory: None,
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
        mondrian_effects::EffectProjectGeometry::new(
            1,
            1,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
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
        mondrian_core::ImageSampling::UNIT_SQUARE,
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

/// Internal read-only generic-GUI contract through the real native CABI; this
/// does not qualify plugin-origin author transactions or a physical App view.
#[test]
#[ignore = "requires owned string_gui_permission SessionProbe"]
fn native_generic_gui_string_writes_respect_existing_author_permission() {
    const TEST: &str = "openfx_host::native_gui_qualification::native_generic_gui_string_writes_respect_existing_author_permission";
    if !shared_frame_tests::qualification_child(
        TEST,
        &[(
            "MONDRIAN_OPENFX_SESSION_PROBE_MODE",
            "string_gui_permission",
        )],
    ) {
        return;
    }
    run_parameter_permission_session(true);
}

/// Ordinary fixed CPU Filter Render, without GUI. This does not qualify the C2
/// author bridge or an action-wide parameter permission matrix.
#[test]
#[ignore = "requires owned render_parameter_permission SessionProbe"]
fn native_render_rejects_local_parameter_writes() {
    const TEST: &str =
        "openfx_host::native_gui_qualification::native_render_rejects_local_parameter_writes";
    if !shared_frame_tests::qualification_child(
        TEST,
        &[(
            "MONDRIAN_OPENFX_SESSION_PROBE_MODE",
            "render_parameter_permission",
        )],
    ) {
        return;
    }
    run_parameter_permission_session(false);
}

fn run_parameter_permission_session(gui_open: bool) {
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct Phase {
        action: c_int,
        interactive: c_int,
        status: [c_int; 2],
        before: [[u8; 128]; 2],
        after: [[u8; 128]; 2],
        double_status: [c_int; 2],
        double_before: [f64; 2],
        double_after: [f64; 2],
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct Receipt {
        created: c_int,
        destroyed: c_int,
        create_status: [c_int; 2],
        invalid_status: [c_int; 4],
        create_double_status: [c_int; 2],
        phase_count: c_int,
        phases: [Phase; 4],
    }
    assert_eq!(std::mem::size_of::<Phase>(), 568);
    assert_eq!(std::mem::size_of::<Receipt>(), 2320);
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
    let names = ["scale", "single", "multi", "writePlain", "writeTimed"]
        .map(|value| CString::new(value).unwrap());
    const INITIAL: [&str; 2] = ["create/plain", "create/timed"];
    const AUTHOR_A: [&str; 2] = ["host-A/plain🙂", "host-A/timed\nline"];
    const AUTHOR_B: [&str; 2] = ["host-B/plain另", "host-B/timed\r\nend"];
    let initial = INITIAL.map(|value| CString::new(value).unwrap());
    let author_a = AUTHOR_A.map(|value| CString::new(value).unwrap());
    let author_b = AUTHOR_B.map(|value| CString::new(value).unwrap());
    const DOUBLE_INITIAL: [f64; 2] = [0.125, -0.25];
    const DOUBLE_A: [f64; 2] = [0.25, -0.5];
    const DOUBLE_B: [f64; 2] = [0.75, 1.5];
    let parameters = |values: &[CString; 2], numbers: [f64; 2]| {
        let mut parameters = Vec::from(std::array::from_fn::<_, 3, _>(|index| NativeParameter {
            name: names[index].as_ptr(),
            query_mode: 0,
            kind: if index == 0 { 1 } else { 6 },
            value: [if index == 0 { 2.0 } else { 0.0 }, 0.0, 0.0, 0.0],
            string_value: if index == 0 {
                std::ptr::null()
            } else {
                values[index - 1].as_ptr()
            },
            string_length: if index == 0 {
                0
            } else {
                values[index - 1].as_bytes().len()
            },
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
        }));
        if !gui_open {
            parameters.extend(std::array::from_fn::<_, 2, _>(|index| NativeParameter {
                name: names[index + 3].as_ptr(),
                query_mode: 0,
                kind: 1,
                value: [numbers[index], 0.0, 0.0, 0.0],
                string_value: std::ptr::null(),
                string_length: 0,
                query: None,
                query_context: std::ptr::null(),
                query_revision: 0,
            }));
        }
        parameters
    };
    let (initial_parameters, parameters_a, parameters_b) = (
        parameters(&initial, DOUBLE_INITIAL),
        parameters(&author_a, DOUBLE_A),
        parameters(&author_b, DOUBLE_B),
    );
    let info = NativeFrameInfo {
        source_alpha: 0,
        width: 1,
        height: 1,
        frame: 0.0,
        frame_rate: 24.0,
        first_frame: 0.0,
        last_frame: 100.0,
        pixel_aspect_ratio: 1.0,
        render_scale: [1.0; 2],

        project_width: 1,
        project_height: 1,
        project_pixel_aspect_ratio: 1.0,
    };
    let input = [0.125_f32, -2.0, 16.0, 0.5]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect::<Vec<_>>();
    let original_input = input.clone();
    let library = unsafe { libloading::Library::new(&path) }.expect("retain fixture receipts");
    type ReadReceipt = unsafe extern "C" fn(*mut Receipt, usize) -> c_int;
    let read_receipt: libloading::Symbol<'_, ReadReceipt> =
        unsafe { library.get(b"SessionProbeStringGuiPermissionReceipt\0") }
            .expect("bounded C-linked callback facts");
    let mut session = NativeSession::create(
        mondrian_core::EffectProcessorOwnerId::new(),
        &binary,
        &bundle,
        &identifier,
        &input,
        &info,
        &initial_parameters,
    )
    .expect("same native instance");
    let replies = Replies { status: 1, calls: AtomicUsize::new(0) };
    let control = NativeInvocationControl {
        context: std::ptr::from_ref(&replies).cast(),
        abort: None,
        progress: None,
        gui_open: c_int::from(gui_open),
        interactive_render: 0,
        question_work: 0,
        question: Some(answer),
        acquire_memory: None,
    };
    let mut output_a = vec![0_u8; input.len()];
    let mut output_b = vec![0_u8; input.len()];
    let rendered_a =
        session.render_controlled(&input, &mut output_a, &info, &parameters_a, Some(&control));
    let feedback_a = session.take_feedback();
    let rendered_b =
        session.render_controlled(&input, &mut output_b, &info, &parameters_b, Some(&control));
    let feedback_b = session.take_feedback();
    // Collect original cleanup before any target assertion, including the RED
    // path: a plugin permission violation must not mask setup/close failure.
    let closed = session.close_gui();
    let closed_again = session.close_gui();
    drop(session);
    // SAFETY: all fields are C integers/floats/byte arrays. The pinned fixture copies
    // its bounded POD only after checking this exact size; no native owner leaks.
    let mut receipt: Receipt = unsafe { std::mem::zeroed() };
    let receipt_status = unsafe { read_receipt(&mut receipt, std::mem::size_of::<Receipt>()) };
    let end = closed.expect("original native Close must succeed");
    assert!(closed_again.expect("idempotent native Close").is_none());
    assert_eq!(receipt_status, 0, "fixture receipt layout");
    assert_eq!(
        (receipt.created, receipt.destroyed),
        (1, 1),
        "same instance settled"
    );
    assert_eq!(
        receipt.create_status,
        [0, 0],
        "noninteractive initialization remains legal"
    );
    assert_eq!(
        rendered_a,
        Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
    );
    assert_eq!(
        rendered_b,
        Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
    );
    assert_eq!(receipt.phase_count, if gui_open { 4 } else { 2 });
    let first = feedback_a.expect("actual first Render feedback");
    let second = feedback_b.expect("actual second Render feedback");
    assert_eq!((first.invocation(), second.invocation()), (1, 2));
    assert_eq!(first.native_owner(), second.native_owner());
    if gui_open {
        let end = end.expect("actual End action");
        assert_eq!(end.invocation(), 2);
        assert_eq!(first.native_owner(), end.native_owner());
    } else {
        assert!(end.is_none(), "noninteractive session never opened a GUI");
    }
    assert_eq!(input, original_input);
    assert_eq!(
        replies.calls.load(Ordering::Relaxed),
        0,
        "no Question work was requested"
    );
    let expected_pixels = [0.25_f32, -4.0, 32.0, 1.0]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect::<Vec<_>>();
    assert_eq!(output_a, expected_pixels);
    assert_eq!(output_b, expected_pixels);
    for (actual, expected) in [
        (&initial, INITIAL),
        (&author_a, AUTHOR_A),
        (&author_b, AUTHOR_B),
    ] {
        assert_eq!(
            actual.each_ref().map(|value| value.as_bytes()),
            expected.map(str::as_bytes)
        );
    }
    fn text(bytes: &[u8; 128]) -> &str {
        std::ffi::CStr::from_bytes_until_nul(bytes)
            .expect("bounded receipt text")
            .to_str()
            .expect("actual UTF-8")
    }
    if gui_open {
        assert_eq!(
            receipt.invalid_status,
            [11, 11, 11, 4],
            "original input-error precedence"
        );
        assert_eq!(receipt.phases.map(|phase| phase.action), [1, 2, 2, 3]);
        assert_eq!(receipt.phases.map(|phase| phase.interactive), [1, 1, 1, 0]);
        // Begin precedes this frame's host assignment. Both Render snapshots must
        // show the real A/B ParameterAssignment while the same GUI remains open.
        for (phase, expected) in receipt.phases[1..3].iter().zip([AUTHOR_A, AUTHOR_B]) {
            assert_eq!(
                phase.before.each_ref().map(text),
                expected,
                "host author assignment"
            );
        }
        for (index, (phase, expected)) in
            receipt.phases.iter().zip([INITIAL, AUTHOR_A, AUTHOR_B, AUTHOR_B]).enumerate()
        {
            assert_eq!(
                phase.status,
                [5, 5],
                "STRING_GUI_PERMISSION phase={index} must refuse local writes"
            );
            assert_eq!(phase.before.each_ref().map(text), expected);
            assert_eq!(
                phase.after, phase.before,
                "STRING_GUI_PERMISSION phase={index} changed local text"
            );
        }
    } else {
        assert_eq!(
            receipt.create_double_status,
            [0, 0],
            "legal Create double writes"
        );
        assert_eq!(
            receipt.phases[..2].iter().map(|phase| phase.action).collect::<Vec<_>>(),
            [2, 2]
        );
        assert_eq!(
            receipt.phases[..2].iter().map(|phase| phase.interactive).collect::<Vec<_>>(),
            [0, 0]
        );
        // Observe both actual host assignments before any permission assertion.
        // The independent targets never affect the official Basic gain output.
        for ((phase, strings), numbers) in
            receipt.phases[..2].iter().zip([AUTHOR_A, AUTHOR_B]).zip([DOUBLE_A, DOUBLE_B])
        {
            assert_eq!(
                phase.before.each_ref().map(text),
                strings,
                "host String assignment"
            );
            assert_eq!(phase.double_before, numbers, "host Double assignment");
        }
        for (parameters, numbers) in [
            (&initial_parameters, DOUBLE_INITIAL),
            (&parameters_a, DOUBLE_A),
            (&parameters_b, DOUBLE_B),
        ] {
            assert_eq!(
                [parameters[3].value[0], parameters[4].value[0]],
                numbers,
                "immutable author numbers"
            );
        }
        for (index, phase) in receipt.phases[..2].iter().enumerate() {
            assert_eq!(
                [
                    phase.double_status[0],
                    phase.double_status[1],
                    phase.status[0],
                    phase.status[1]
                ],
                [5, 5, 5, 5],
                "RENDER_PARAMETER_PERMISSION phase={index} must refuse local writes"
            );
            assert_eq!(
                phase.after, phase.before,
                "RENDER_PARAMETER_PERMISSION phase={index} changed local text"
            );
            assert_eq!(
                phase.double_after, phase.double_before,
                "RENDER_PARAMETER_PERMISSION phase={index} changed local number"
            );
        }
    }
}
