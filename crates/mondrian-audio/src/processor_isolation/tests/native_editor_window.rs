//! Actual current product helper/owned VST3 window fixture qualification.
//! Direct isolated processor tests prove the child control/main/DSP transport,
//! not Runtime-installed author authority, persistence, physical GUI or RT timing.

use super::*;
use crate::{
    NativeAudioEditorCloseStage, NativeAudioEditorOpenRequest, NativeAudioEditorOutcome,
    NativeAudioObjectBindOutcome, NativeAudioObjectBindRequest, NativeAudioObservationConsumption,
};
use std::ffi::c_void;
use std::{thread, time::Instant};

#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "FindWindowW"]
    fn find_window_w(class: *const u16, title: *const u16) -> *mut c_void;
    #[link_name = "FindWindowExW"]
    fn find_window_ex_w(
        parent: *mut c_void,
        after: *mut c_void,
        class: *const u16,
        title: *const u16,
    ) -> *mut c_void;
    #[link_name = "PostMessageW"]
    fn post_message_w(window: *mut c_void, message: u32, word: usize, long: isize) -> i32;
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn editor_snapshot(mode: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(b"MEV1");
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&0.5_f64.to_le_bytes());
    bytes.extend_from_slice(&0.5_f64.to_le_bytes());
    bytes.extend_from_slice(&mode.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    assert_eq!(bytes.len(), 32, "independent owned fixture state extent");
    let mut snapshot = Vec::with_capacity(92);
    snapshot.extend_from_slice(b"VST3HOST_STATE\0\0");
    snapshot.extend_from_slice(&1_u32.to_le_bytes());
    snapshot.extend_from_slice(&32_u32.to_le_bytes());
    snapshot.extend_from_slice(&32_u32.to_le_bytes());
    snapshot.extend_from_slice(&bytes);
    snapshot.extend_from_slice(&bytes);
    snapshot
}

struct NativeEditorProductFixture {
    processor: Box<dyn AudioProcessor>,
    binding: NativeAudioObjectBindRequest,
    title: String,
}
impl NativeEditorProductFixture {
    fn load(mode: u32) -> Self {
        Self::load_with_entry(mode, true)
    }
    fn load_idle(mode: u32) -> Self {
        Self::load_with_entry(mode, false)
    }
    fn load_with_entry(mode: u32, entered: bool) -> Self {
        let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .map(PathBuf::from)
            .expect("current V7 product helper");
        let library = std::env::var_os("MONDRIAN_VST3_EDITOR_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("rebuilt owned IPlugView fixture");
        let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [library])
            .expect("actual native descriptor");
        assert_eq!(catalog.descriptors().len(), 1, "exact owned fixture class");
        let render = AudioRenderContract {
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 64 * 1024 * 1024,
            ..test_render_contract()
        };
        let mut instance = catalog
            .create_instance(
                &catalog.descriptors()[0].class_id,
                render,
                Some(editor_snapshot(mode)),
            )
            .expect("one actual component/controller snapshot");
        // This transport qualification intentionally has no authored parameter
        // lanes: it observes actual native GUI/controller→DSP behavior without
        // pretending that product author automation has already been installed.
        instance.parameters.clear();
        let request = AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: instance.id,
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &instance.definition,
            &instance.parameters,
            instance.opaque_state.as_ref().map(AsRef::as_ref),
            render,
        );
        let factory = IsolatedAudioProcessorFactory::prepare(
            catalog.resolve(request).expect("exact authenticated native spec"),
        )
        .expect("fresh actual child preparation");
        let mut processor = factory.create(None).expect("same exclusive native object");
        let before = processor
            .native_audio_object_observation()
            .expect("actual prepared observation")
            .expect("VST3 native object");
        if entered {
            processor.enter_state(0).expect("actual first signal on the prepared object");
        }
        let actual = processor
            .native_audio_object_observation()
            .expect("actual entered observation")
            .expect("actual current load");
        assert_eq!(
            before.process_id(),
            actual.process_id(),
            "Enter must retain the isolated child"
        );
        if entered {
            assert_eq!(
                actual, before,
                "first signal uses actual accepted native initialization"
            );
        } else {
            assert_eq!(
                actual, before,
                "idle native inspection does not fabricate a fresh state entry"
            );
        }
        let binding = NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: actual,
        };
        assert!(
            matches!(processor.bind_native_audio_object(binding).expect("same actual child acknowledges"),NativeAudioObjectBindOutcome::Bound(receipt)
            if receipt.binding()==binding.binding && receipt.object()==actual)
        );
        let title = format!("Mondrian product native editor {}", binding.binding);
        Self { processor, binding, title }
    }
    fn open(&mut self) -> *mut c_void {
        let request = NativeAudioEditorOpenRequest::new(
            self.binding,
            mondrian_core::NativeAudioEditorTitle::new(self.title.clone())
                .expect("bounded actual title"),
        );
        assert_eq!(
            self.processor
                .open_native_audio_editor(request)
                .expect("actual same bound child Open11"),
            NativeAudioEditorOutcome::Opened { binding: self.binding, width: 320, height: 160 }
        );
        let title = wide(&self.title);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let parent = unsafe { find_window_w(std::ptr::null(), title.as_ptr()) };
            if !parent.is_null() {
                return parent;
            }
            thread::yield_now();
        }
        panic!("real product-owned parent window was never created");
    }
    fn block(&mut self, start: i64) -> Result<Vec<f32>, AudioProcessorHostError> {
        let mut audio = TestAudioIo { main: vec![1.0; 8], auxiliary: Vec::new() };
        let context = AudioProcessorProcessContext::new(
            AudioRenderRequest { transport_activity: None, start_sample: start, frames: 4 },
            AudioSampleRate::new(48_000).expect("exact rate"),
            AudioChannelLayout::Stereo,
            AudioProcessingMode::Offline,
            AudioKernelBackend::ScalarReference,
        );
        self.processor.process(
            context,
            &mut audio,
            AudioParameterEventBatch::new(start, 4, &[], &[], &[]),
        )?;
        Ok(audio.main)
    }
}

#[test]
#[ignore = "rebuilt current V7 product helper + owned real Windows IPlugView fixture"]
fn native_editor_product_real_window_messages_and_scoped_dsp_share_the_retained_child_object() {
    let mut fixture = NativeEditorProductFixture::load(0);
    let parent = fixture.open();
    assert_eq!(
        fixture.block(0).expect("finite native positive control"),
        vec![0.5; 8]
    );
    let class = wide("MondrianOwnedVst3EditorFixture");
    let view = unsafe {
        find_window_ex_w(
            parent,
            std::ptr::null_mut(),
            class.as_ptr(),
            std::ptr::null(),
        )
    };
    assert!(
        !view.is_null(),
        "actual owned child view must belong to the real product parent"
    );
    assert_ne!(
        unsafe { post_message_w(view, 0x0100, 0x47, 0) },
        0,
        "actual native G group request"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut start = 4;
    let mut changed = None;
    while Instant::now() < deadline {
        let observed = fixture.block(start).expect("same DSP continuity while native main pumps");
        start += 4;
        if observed == [1.125, 0.375, 1.125, 0.375, 1.125, 0.375, 1.125, 0.375] {
            changed = Some(observed);
            break;
        }
        thread::yield_now();
    }
    assert!(
        changed.is_some(),
        "actual controller group must reach the same native DSP instance"
    );
    assert_eq!(
        fixture
            .processor
            .native_audio_object_observation()
            .expect("Preserve9 source facts"),
        Some(fixture.binding.expected_object),
        "message service and DSP joins cannot replace the native owner"
    );
    assert_eq!(
        fixture
            .processor
            .close_native_audio_editor(fixture.binding)
            .expect("same actual attachment closes"),
        NativeAudioEditorOutcome::Closed { binding: fixture.binding }
    );
    assert_eq!(
        fixture
            .processor
            .native_audio_object_observation()
            .expect("closed view retained processor"),
        Some(fixture.binding.expected_object)
    );
}

#[test]
#[ignore = "rebuilt current V7 product helper + owned real Windows IPlugView fixture"]
fn native_editor_product_native_user_close_frame_clear_pending_is_retryable_without_repeated_removal(
) {
    let mut fixture = NativeEditorProductFixture::load(2);
    let parent = fixture.open();
    assert_ne!(
        unsafe { post_message_w(parent, 0x0010, 0, 0) },
        0,
        "actual parent WM_CLOSE source intent"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut pending = false;
    while Instant::now() < deadline {
        let observations = fixture
            .processor
            .poll_native_observations(NativeAudioObservationConsumption::Preserve)
            .expect("same source observation while control-only");
        assert_eq!(observations.object, Some(fixture.binding.expected_object));
        if observations.editor_cleanup == Some(NativeAudioEditorCloseStage::ClearHostFrame) {
            pending = true;
            break;
        }
        thread::yield_now();
    }
    assert!(
        pending,
        "native source Close must keep retryable frame cleanup rather than kill the child"
    );
    assert_eq!(
        fixture
            .processor
            .close_native_audio_editor(fixture.binding)
            .expect("Close12 retries only canonical remaining frame clear"),
        NativeAudioEditorOutcome::Closed { binding: fixture.binding }
    );
    assert_eq!(
        fixture
            .block(0)
            .expect("same still-entered processor resumes only after complete cleanup"),
        vec![0.5; 8]
    );
    assert_eq!(
        fixture
            .processor
            .native_audio_object_observation()
            .expect("retained original child and load"),
        Some(fixture.binding.expected_object)
    );
}

#[test]
#[ignore = "rebuilt current V7 product helper + owned real Windows IPlugView fixture"]
fn native_editor_product_removal_refusal_retires_the_isolated_child_without_claiming_retry_or_close(
) {
    let mut fixture = NativeEditorProductFixture::load(1);
    fixture.open();
    assert!(
        fixture.processor.close_native_audio_editor(fixture.binding).is_err(),
        "removed refusal is terminal, not retryable frame cleanup"
    );
    assert!(
        fixture.processor.native_audio_object_observation().is_err(),
        "retired native attachment cannot authorize further operations"
    );
    assert!(
        fixture.processor.close_native_audio_editor(fixture.binding).is_err(),
        "host cannot call removed a second time on the unresolved old child"
    );
}

fn request_fixture_main_restart(fixture: &mut NativeEditorProductFixture, parent: *mut c_void) {
    let class = wide("MondrianOwnedVst3EditorFixture");
    let view = unsafe {
        find_window_ex_w(
            parent,
            std::ptr::null_mut(),
            class.as_ptr(),
            std::ptr::null(),
        )
    };
    assert!(!view.is_null(), "same real attached child view");
    assert_ne!(
        unsafe { post_message_w(view, 0x0100, b'R' as usize, 0) },
        0,
        "legal restart originates in actual native-main WM_KEYDOWN"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let snapshot = fixture
            .processor
            .preflight_native_observations(fixture.binding.expected_object)
            .expect("one actual process-deadline Preserve9 snapshot");
        assert_eq!(snapshot.object, Some(fixture.binding.expected_object));
        if snapshot.pending_reprepare.map(crate::NativeObservedRequest::request)
            == Some(mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 })
        {
            return;
        }
        thread::yield_now();
    }
    panic!("actual native UI restart never appeared in the same bounded snapshot");
}

#[test]
#[ignore = "fresh matching V7 product helper, owned R-capable VST3 editor fixture and Windows main/DSP lanes"]
fn native_preflight_snapshot_product_legal_main_request_is_nonconsuming_and_same_actual_object() {
    let mut fixture = NativeEditorProductFixture::load(0);
    let parent = fixture.open();
    request_fixture_main_restart(&mut fixture, parent);
    let cohort = fixture
        .processor
        .native_configuration_cohort()
        .expect("same actual owner cohort")
        .expect("actual callback cohort");
    assert_eq!(
        cohort.request,
        mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 }
    );
    for _ in 0..3 {
        let snapshot = fixture
            .processor
            .preflight_native_observations(fixture.binding.expected_object)
            .expect("single Preserve9");
        assert_eq!(
            snapshot.pending_reprepare.map(crate::NativeObservedRequest::request),
            Some(cohort.request)
        );
        assert_eq!(snapshot.object, Some(fixture.binding.expected_object));
        assert_eq!(snapshot.editor_cleanup, None);
    }
    assert_eq!(
        fixture
            .processor
            .native_configuration_cohort()
            .expect("non-consuming observation"),
        Some(cohort)
    );
    assert_eq!(
        fixture
            .processor
            .close_native_audio_editor(fixture.binding)
            .expect("same actual attachment close"),
        NativeAudioEditorOutcome::Closed { binding: fixture.binding }
    );
}

#[test]
#[ignore = "fresh matching V7 product helper, owned R-capable VST3 editor fixture and Windows main/DSP lanes"]
fn native_preflight_snapshot_product_new_same_bit_ui_request_preserves_new_cohort_serial() {
    let mut fixture = NativeEditorProductFixture::load(0);
    let parent = fixture.open();
    request_fixture_main_restart(&mut fixture, parent);
    let first = fixture
        .processor
        .native_configuration_cohort()
        .expect("actual first cohort")
        .expect("recorded callback");
    let class = wide("MondrianOwnedVst3EditorFixture");
    let view = unsafe {
        find_window_ex_w(
            parent,
            std::ptr::null_mut(),
            class.as_ptr(),
            std::ptr::null(),
        )
    };
    assert!(!view.is_null());
    assert_ne!(unsafe { post_message_w(view, 0x0100, b'R' as usize, 0) }, 0);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut next = None;
    while Instant::now() < deadline {
        let snapshot = fixture
            .processor
            .preflight_native_observations(fixture.binding.expected_object)
            .expect("same main snapshot after queued R");
        assert_eq!(snapshot.object, Some(fixture.binding.expected_object));
        assert_eq!(
            snapshot.pending_reprepare.map(crate::NativeObservedRequest::request),
            Some(first.request)
        );
        let cohort = fixture
            .processor
            .native_configuration_cohort()
            .expect("non-consuming serial check")
            .expect("pending remains");
        if cohort.serial > first.serial {
            next = Some(cohort);
            break;
        }
        thread::yield_now();
    }
    let next = next.expect("same-bit native callback must advance exact serial");
    assert_eq!(next.request, first.request);
    assert_eq!(
        fixture
            .processor
            .native_configuration_cohort()
            .expect("retained exact newer cohort"),
        Some(next)
    );
    assert_eq!(
        fixture
            .processor
            .close_native_audio_editor(fixture.binding)
            .expect("same actual attachment close"),
        NativeAudioEditorOutcome::Closed { binding: fixture.binding }
    );
}

#[test]
#[ignore = "fresh matching V7 product helper, owned R-capable VST3 editor fixture and Windows main/DSP lanes"]
fn native_preflight_snapshot_product_frame_clear_and_pending_share_one_reply_without_retiring_owner(
) {
    let mut fixture = NativeEditorProductFixture::load(2);
    let parent = fixture.open();
    request_fixture_main_restart(&mut fixture, parent);
    assert_ne!(unsafe { post_message_w(parent, 0x0010, 0, 0) }, 0);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut cleanup = None;
    while Instant::now() < deadline {
        let snapshot = fixture
            .processor
            .preflight_native_observations(fixture.binding.expected_object)
            .expect("same actual source while control-only");
        assert_eq!(snapshot.object, Some(fixture.binding.expected_object));
        assert_eq!(
            snapshot.pending_reprepare.map(crate::NativeObservedRequest::request),
            Some(mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 })
        );
        if snapshot.editor_cleanup == Some(NativeAudioEditorCloseStage::ClearHostFrame) {
            cleanup = Some(snapshot);
            break;
        }
        thread::yield_now();
    }
    cleanup.expect("FrameClear must be projected without losing pending request");
    assert_eq!(
        fixture
            .processor
            .close_native_audio_editor(fixture.binding)
            .expect("canonical cleanup retry"),
        NativeAudioEditorOutcome::Closed { binding: fixture.binding }
    );
    let snapshot = fixture
        .processor
        .preflight_native_observations(fixture.binding.expected_object)
        .expect("same object after successful remaining cleanup");
    assert_eq!(snapshot.object, Some(fixture.binding.expected_object));
    assert_eq!(snapshot.editor_cleanup, None);
    assert_eq!(
        snapshot.pending_reprepare.map(crate::NativeObservedRequest::request),
        Some(mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 }),
        "closing a view is not configuration installation"
    );
}

fn native_author_delivered_after_group(
    fixture: &mut NativeEditorProductFixture,
) -> crate::NativeAudioAuthorDelivery {
    let parent = fixture.open();
    let class = wide("MondrianOwnedVst3EditorFixture");
    let view = unsafe {
        find_window_ex_w(
            parent,
            std::ptr::null_mut(),
            class.as_ptr(),
            std::ptr::null(),
        )
    };
    assert!(!view.is_null(), "actual owned child window");
    assert_ne!(
        unsafe { post_message_w(view, 0x0100, 0x47, 0) },
        0,
        "real G group message"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let observation = fixture
            .processor
            .poll_native_observations(NativeAudioObservationConsumption::DrainAuthor(
                fixture.binding,
            ))
            .expect("same-child V7 native Poll9");
        assert_eq!(observation.object, Some(fixture.binding.expected_object));
        if let Some(author) = observation.author {
            match author.delivery {
                crate::NativeAudioAuthorDeliveryState::Delivered(delivery) => return delivery,
                crate::NativeAudioAuthorDeliveryState::Preserved => {}
                crate::NativeAudioAuthorDeliveryState::Refused(
                    crate::NativeAudioAuthorRefusal::SourceChanged,
                ) => {}
                other => panic!("native group must yield coherent complete delivery: {other:?}"),
            }
        }
        thread::yield_now();
    }
    panic!("real native GUI callbacks were not delivered");
}

fn assert_native_author_state_matches_real_processor_and_controller(
    delivery: &crate::NativeAudioAuthorDelivery,
) {
    use crate::{NativeAudioAuthorEvent as Event, NativeAudioAuthorParameterPhase as Phase};
    assert_eq!(delivery.events().first(), Some(&Event::GroupStarted));
    assert!(delivery.events().contains(&Event::GroupFinished));
    assert!(delivery.events().contains(&Event::DirtyChanged(true)));
    assert_eq!(
        delivery.events().last(),
        Some(&Event::DirtyChanged(false)),
        "actual getState dirty=false suffix is retained"
    );
    let values: Vec<_> = delivery
        .events()
        .iter()
        .filter_map(|event| match event {
            Event::Parameter(parameter) if parameter.phase() == Phase::Value => Some((
                parameter.parameter().as_str(),
                parameter.value().expect("actual native Value").value().to_bits(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        values.len(),
        2,
        "native multi-parameter group must be complete"
    );
    assert_eq!(
        values.iter().map(|(_, bits)| *bits).collect::<Vec<_>>(),
        [0.75_f64.to_bits(), 0.25_f64.to_bits()]
    );
    let bytes = delivery.configuration().expect("coherent actual terminal state");
    assert_eq!(
        bytes.len(),
        92,
        "owned fixture's independent canonical envelope extent"
    );
    assert_eq!(&bytes[..16], b"VST3HOST_STATE\0\0");
    assert_eq!(
        u32::from_le_bytes(bytes[16..20].try_into().expect("version")),
        1
    );
    assert_eq!(
        u32::from_le_bytes(bytes[20..24].try_into().expect("component extent")),
        32
    );
    assert_eq!(
        u32::from_le_bytes(bytes[24..28].try_into().expect("controller extent")),
        32
    );
    for state in [&bytes[28..60], &bytes[60..92]] {
        assert_eq!(&state[..4], b"MEV1");
        assert_eq!(
            f64::from_le_bytes(state[8..16].try_into().expect("real gain")).to_bits(),
            0.75_f64.to_bits()
        );
        assert_eq!(
            f64::from_le_bytes(state[16..24].try_into().expect("real balance")).to_bits(),
            0.25_f64.to_bits()
        );
    }
}

#[test]
#[ignore = "fresh current V7 helper + rebuilt owned Windows IPlugView; no physical GUI or author Session claim"]
fn native_author_product_idle_capture_then_explicit_first_entry_uses_accepted_seed() {
    let mut fixture = NativeEditorProductFixture::load_idle(0);
    let delivery = native_author_delivered_after_group(&mut fixture);
    assert_native_author_state_matches_real_processor_and_controller(&delivery);
    assert_eq!(
        fixture.processor.native_audio_object_observation().expect("same actual object"),
        Some(fixture.binding.expected_object)
    );
    let ack = crate::NativeAudioAuthorAcknowledgement {
        source_binding: fixture.binding,
        installed_binding: fixture.binding,
        revision: delivery.revision(),
    };
    let observed = fixture
        .processor
        .poll_native_observations(NativeAudioObservationConsumption::AcknowledgeAuthor(ack))
        .expect("same native delivery receipt");
    assert!(
        matches!(observed.author,Some(crate::NativeAudioAuthorObservation{delivery:crate::NativeAudioAuthorDeliveryState::Acknowledged { acknowledgement: actual, request_cohort: None },..}) if actual==ack)
    );
    let before = fixture
        .processor
        .poll_native_observations(NativeAudioObservationConsumption::Preserve)
        .expect("real post-ack source");
    assert!(before.author.expect("normative VST source").source.closed_empty());
    // Ready/idle inspection has never entered a signal continuity. The
    // explicit first entry supplies its coordinate; zero-sample catchup cannot
    // invent zero or authorize an unentered raw Process. Actual accepted native
    // initialization keeps the same object and view for this first signal.
    let prior = fixture.binding;
    fixture
        .processor
        .enter_state(-37)
        .expect("mandatory real first signal entry after accepted state");
    let entered = fixture
        .processor
        .native_audio_object_observation()
        .expect("actual fresh first-entry object")
        .expect("real native object");
    assert_eq!(
        entered.process_id(),
        prior.expected_object.process_id(),
        "same retained child"
    );
    assert_eq!(
        entered, prior.expected_object,
        "first signal retains accepted actual initialization"
    );
    assert!(matches!(
        fixture
            .processor
            .close_native_audio_editor(prior)
            .expect("old binding is a typed refusal, never a close permission"),
        NativeAudioEditorOutcome::Refused(crate::NativeAudioEditorRefusal::OwnerChanged)
    ));
    let binding = NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: entered,
    };
    assert!(matches!(fixture.processor.bind_native_audio_object(binding)
        .expect("actual first-entry object acknowledges its fresh binding"),
        NativeAudioObjectBindOutcome::Bound(receipt) if receipt.binding()==binding.binding && receipt.object()==entered));
    fixture.binding = binding;
    for sample in [-37, -33] {
        assert_eq!(
            fixture.block(sample).expect(
                "first and continued PCM use the accepted actual component/controller seed"
            ),
            [1.125, 0.375, 1.125, 0.375, 1.125, 0.375, 1.125, 0.375]
        );
    }
    assert!(
        !fixture.open().is_null(),
        "the fresh actual binding can reopen its real view"
    );
    assert!(
        matches!(fixture.processor.close_native_audio_editor(fixture.binding).expect("same actual view close"),NativeAudioEditorOutcome::Closed{binding} if binding==fixture.binding)
    );
}

#[test]
#[ignore = "fresh current V7 helper + rebuilt owned Windows IPlugView; direct transport, not installed author authorization"]
fn native_author_product_preserve_and_wrong_ack_cannot_erase_a_delivered_real_native_group() {
    let mut fixture = NativeEditorProductFixture::load(0);
    assert_eq!(
        fixture.block(0).expect("finite native positive control"),
        [0.5; 8]
    );
    let delivery = native_author_delivered_after_group(&mut fixture);
    assert_native_author_state_matches_real_processor_and_controller(&delivery);
    for consumption in [
        NativeAudioObservationConsumption::Preserve,
        NativeAudioObservationConsumption::Drain,
    ] {
        let observed = fixture
            .processor
            .poll_native_observations(consumption)
            .expect("same sole observation stream");
        assert!(
            matches!(observed.author,Some(crate::NativeAudioAuthorObservation{delivery:crate::NativeAudioAuthorDeliveryState::AwaitingAcknowledgement{binding,revision,request_cohort},..}) if binding==fixture.binding && revision==delivery.revision() && request_cohort==delivery.request_cohort())
        );
    }
    let bad = crate::NativeAudioAuthorAcknowledgement {
        source_binding: fixture.binding,
        installed_binding: fixture.binding,
        revision: delivery.revision() + 1,
    };
    let refused = fixture
        .processor
        .poll_native_observations(NativeAudioObservationConsumption::AcknowledgeAuthor(bad))
        .expect("typed stale receipt refusal");
    assert!(matches!(
        refused.author,
        Some(crate::NativeAudioAuthorObservation {
            delivery: crate::NativeAudioAuthorDeliveryState::Refused(
                crate::NativeAudioAuthorRefusal::StaleDelivery
            ),
            ..
        })
    ));
    let retained = fixture
        .processor
        .poll_native_observations(NativeAudioObservationConsumption::Preserve)
        .expect("same pending record");
    assert!(
        matches!(retained.author,Some(crate::NativeAudioAuthorObservation{delivery:crate::NativeAudioAuthorDeliveryState::AwaitingAcknowledgement{revision,..},..}) if revision==delivery.revision())
    );
    assert_eq!(
        fixture
            .block(4)
            .expect("zero-sample native catchup leaves continuous actual sample start unchanged"),
        [1.125, 0.375, 1.125, 0.375, 1.125, 0.375, 1.125, 0.375]
    );
    assert!(
        matches!(fixture.processor.close_native_audio_editor(fixture.binding).expect("same view close"),NativeAudioEditorOutcome::Closed{binding} if binding==fixture.binding)
    );
}

#[test]
#[ignore = "fresh current V7 product helper + rebuilt owned Windows IPlugView; direct transport, not App history/persistence or physical presentation"]
fn native_author_product_terminal_ack_seeds_real_future_entry_on_same_child() {
    let mut fixture = NativeEditorProductFixture::load(0);
    assert_eq!(fixture.block(0).expect("native positive control"), [0.5; 8]);
    let delivery = native_author_delivered_after_group(&mut fixture);
    assert_native_author_state_matches_real_processor_and_controller(&delivery);
    let original_object = fixture.binding.expected_object;
    let ack = crate::NativeAudioAuthorAcknowledgement {
        source_binding: fixture.binding,
        installed_binding: fixture.binding,
        revision: delivery.revision(),
    };
    let observed = fixture
        .processor
        .poll_native_observations(NativeAudioObservationConsumption::AcknowledgeAuthor(ack))
        .expect("one exact accepted actual state receipt");
    assert!(matches!(observed.author,
        Some(crate::NativeAudioAuthorObservation { delivery: crate::NativeAudioAuthorDeliveryState::Acknowledged { acknowledgement: actual, request_cohort: None }, .. })
        if actual == ack));
    assert_eq!(
        fixture.block(4).expect("no Enter/Stop during actual accepted group"),
        [1.125, 0.375, 1.125, 0.375, 1.125, 0.375, 1.125, 0.375]
    );
    assert!(
        matches!(fixture.processor.close_native_audio_editor(fixture.binding)
        .expect("actual view detached before future native object transition"),
        NativeAudioEditorOutcome::Closed { binding } if binding == fixture.binding)
    );
    // This is an explicitly new real entry, not a parameter handoff reset.
    // Its accepted private seed must recreate the actual component/controller
    // values; the original factory seed contains gain/balance 0.5/0.5.
    fixture.processor.enter_state(20).expect("real future native state entry");
    let reloaded = fixture
        .processor
        .native_audio_object_observation()
        .expect("fresh actual load observation")
        .expect("same native adapter");
    assert_eq!(reloaded.process_id(), original_object.process_id());
    assert!(reloaded.object_serial() > original_object.object_serial());
    let previous_binding = fixture.binding;
    fixture.binding = NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: reloaded,
    };
    assert!(
        matches!(fixture.processor.bind_native_audio_object(fixture.binding)
        .expect("fresh current object binding"), NativeAudioObjectBindOutcome::Bound(receipt)
        if receipt.binding() == fixture.binding.binding && receipt.object() == reloaded)
    );
    assert_eq!(
        fixture.block(20).expect("accepted coherent seed must survive real reload"),
        [1.125, 0.375, 1.125, 0.375, 1.125, 0.375, 1.125, 0.375]
    );
    assert!(matches!(
        fixture
            .processor
            .close_native_audio_editor(previous_binding)
            .expect("old actual object close is refused"),
        NativeAudioEditorOutcome::Refused(_)
    ));
    let _parent = fixture.open();
    assert!(
        matches!(fixture.processor.close_native_audio_editor(fixture.binding)
        .expect("fresh actual view close"), NativeAudioEditorOutcome::Closed { binding }
        if binding == fixture.binding)
    );
}

#[test]
#[ignore = "fresh product helper and owned Windows IPlugView fixture"]
fn native_entry_binding_product_first_signal_retains_accepted_native_state_and_same_hwnd_old_api() {
    let mut fixture = NativeEditorProductFixture::load_idle(0);
    let delivery = native_author_delivered_after_group(&mut fixture);
    assert_native_author_state_matches_real_processor_and_controller(&delivery);
    let title = wide(&fixture.title);
    let parent = unsafe { find_window_w(std::ptr::null(), title.as_ptr()) };
    assert!(!parent.is_null(), "real GUI was opened before any signal");
    let class = wide("MondrianOwnedVst3EditorFixture");
    let view = unsafe {
        find_window_ex_w(
            parent,
            std::ptr::null_mut(),
            class.as_ptr(),
            std::ptr::null(),
        )
    };
    assert!(!view.is_null(), "real IPlugView is attached");
    let prior = fixture.binding;
    let ack = crate::NativeAudioAuthorAcknowledgement {
        source_binding: prior,
        installed_binding: prior,
        revision: delivery.revision(),
    };
    assert!(
        matches!(fixture.processor.poll_native_observations(NativeAudioObservationConsumption::AcknowledgeAuthor(ack)).expect("accepted canonical seed receipt").author,
        Some(crate::NativeAudioAuthorObservation {delivery:crate::NativeAudioAuthorDeliveryState::Acknowledged { acknowledgement: actual, request_cohort: None },..}) if actual==ack)
    );
    fixture
        .processor
        .enter_state(-37)
        .expect("actual first signed signal coordinate");
    let object = fixture
        .processor
        .native_audio_object_observation()
        .expect("same retained child")
        .expect("native object");
    assert_eq!(object,prior.expected_object,"FIRST_NATIVE_OBJECT_PRIMARY: accepted initial component/controller must not reload before first signal");
    assert_eq!(
        unsafe { find_window_w(std::ptr::null(), title.as_ptr()) },
        parent,
        "same actual parent HWND"
    );
    assert_eq!(
        unsafe {
            find_window_ex_w(
                parent,
                std::ptr::null_mut(),
                class.as_ptr(),
                std::ptr::null(),
            )
        },
        view,
        "same actual plugin view HWND"
    );
    let binding = NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: object,
    };
    assert!(
        matches!(fixture.processor.bind_native_audio_object(binding).expect("same object acknowledges current binding"),NativeAudioObjectBindOutcome::Bound(receipt) if receipt.binding()==binding.binding)
    );
    fixture.binding = binding;
    assert_eq!(
        fixture.block(-37).expect("independent first actual PCM"),
        [1.125, 0.375, 1.125, 0.375, 1.125, 0.375, 1.125, 0.375]
    );
    let later = native_author_delivered_after_group(&mut fixture);
    assert_native_author_state_matches_real_processor_and_controller(&later);
    assert_eq!(
        unsafe { find_window_w(std::ptr::null(), title.as_ptr()) },
        parent
    );
    assert!(
        matches!(fixture.processor.close_native_audio_editor(binding).expect("same current owner closes once"),NativeAudioEditorOutcome::Closed {binding:actual} if actual==binding)
    );
}
