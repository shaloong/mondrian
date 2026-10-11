use super::*;
use crate::{
    NativeAudioObjectBindOutcome, NativeAudioObjectBindRefusal, NativeAudioObjectBindRequest,
};

#[test]
#[ignore = "current V6 product helper and rebuilt owned VST3 fixture"]
fn installed_vst3_native_object_binding_changes_on_real_enter_and_retains_refused_child() {
    let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
        .map(PathBuf::from)
        .expect("current V6 product helper");
    let library = std::env::var_os("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")
        .map(PathBuf::from)
        .expect("rebuilt owned VST3 fixture");
    let catalog = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [library])
        .expect("discover actual pinned VST3 class");
    let render = AudioRenderContract {
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    };
    let mut instance = catalog
        .create_instance(
            &catalog.descriptors()[0].class_id,
            render,
            Some(vst3_sidechain_snapshot(0.5, 0)),
        )
        .expect("same native authored instance");
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
    let factory =
        IsolatedAudioProcessorFactory::prepare(catalog.resolve(request).expect("native spec"))
            .expect("prepare authenticated actual child");
    let mut processor = factory.create().expect("exclusive current native processor");
    let before = processor
        .native_audio_object_observation()
        .expect("actual before-entry observation")
        .expect("VST3 current object capability");
    assert_ne!(
        before.process_id().get(),
        std::process::id(),
        "ABI object is isolated in the actual child"
    );
    let initial = NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: before,
    };
    let NativeAudioObjectBindOutcome::Bound(initial_receipt) = processor
        .bind_native_audio_object(initial)
        .expect("correlated child acknowledgement")
    else {
        panic!("initial actual object must bind");
    };
    assert_eq!(initial_receipt.binding(), initial.binding);
    assert_eq!(initial_receipt.object(), before);
    processor.enter_state(-4).expect("first Enter anchors initialized native object");
    let anchored = processor
        .native_audio_object_observation()
        .expect("actual anchored object")
        .expect("current VST3 object after first Enter");
    assert_eq!(
        anchored, before,
        "first Enter preserves initialized native object and child"
    );
    let anchored_binding = NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: anchored,
    };
    let NativeAudioObjectBindOutcome::Bound(anchored_receipt) = processor
        .bind_native_audio_object(anchored_binding)
        .expect("correlated rebind after first Enter")
    else {
        panic!("anchored current object must bind");
    };
    assert_eq!(anchored_receipt.binding(), anchored_binding.binding);
    assert_eq!(anchored_receipt.object(), anchored);
    processor.enter_state(9).expect("second Enter stops/destroys/reloads in same child");
    let after = processor
        .native_audio_object_observation()
        .expect("actual reloaded object")
        .expect("current VST3 object");
    assert_eq!(
        before.process_id(),
        after.process_id(),
        "retain the one exclusive child"
    );
    assert!(
        after.object_serial() > before.object_serial(),
        "every actual reload has a new nonwrapping load serial"
    );
    assert_eq!(
        processor
            .bind_native_audio_object(initial)
            .expect("stale request has typed response"),
        NativeAudioObjectBindOutcome::Refused(NativeAudioObjectBindRefusal::ObjectChanged)
    );
    let fresh = NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: after,
    };
    let NativeAudioObjectBindOutcome::Bound(receipt) = processor
        .bind_native_audio_object(fresh)
        .expect("recovery after refused stale object")
    else {
        panic!("current same child must remain bindable");
    };
    assert_eq!(receipt.object(), after);
    assert_eq!(receipt.binding(), fresh.binding);
    assert_eq!(
        processor.native_audio_object_observation().expect("child retained"),
        Some(after)
    );
    // This gate proves actual object/binding IPC, not editor attachment, physical
    // GUI behavior, installed Runtime authority, author Undo or scheduling.
}
