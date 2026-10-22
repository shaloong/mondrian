//! Real Surge manual query, closed Host-origin Adopt and durable author state.
//! Host-origin Adopt adds no native state rewrite/Ack; reload qualifies no fresh native preparation or DSP recovery.
use super::native_parameter_adoption::assert_one_host_value;
use super::*;
use crate::app::parameter_gesture::{DisplayedNativeParameterIdentity, ParameterGestureAction};
use crate::app::product_action::AudioNativeEditorPayload;

#[test]
#[ignore = "current normal Product and pinned Surge XT 1.3.4 VST3; actual single-ID query/Closed Adopt/public author-state reload, no fresh native prepare/device/PCM"]
fn app_surge_xt_vst3_query_adoption_after_close_persists_one_host_value() {
    run_surge_xt_vst3_app_editor_control(SurgeVst3AppControl::QueryAdopt);
}

pub(super) fn adopt_closed_parameter_and_reload(
    state: &mut AppState,
    payload: &AudioNativeEditorPayload,
    directory: &mondrian_timeline::audio::AudioProcessorInstance,
    parameter: &mondrian_core::ParameterId,
    observed: &mondrian_core::NativeAudioParameterObservation,
    deadline: Instant,
) -> u64 {
    let files = tempfile::tempdir().expect("bounded public Surge author archive");
    let archive = files.path().join("Surge采用后重开🙂.mdp");
    let mut reopened: Option<AppState> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(
            state.native_audio_editor_surface_state(payload),
            (false, false)
        );
        assert!(state.active_parameter_gesture.is_none());
        assert!(state.audio_playback.native_control_hold_generation().is_none());
        let baseline = state.active_sequence().unwrap().audio_program.outputs[0]
            .strip
            .pre_fader
            .processors[0]
            .clone();
        assert_eq!(baseline.id, directory.id);
        assert_eq!(baseline.definition, directory.definition);
        assert_eq!(baseline.bypassed, directory.bypassed);
        assert_eq!(baseline.parameters.len(), 2855);
        assert_eq!(
            baseline.parameters, directory.parameters,
            "settled initialization retains every original schema/UI/None lane"
        );
        assert!(baseline.parameters.values().all(|row| row.automation.is_none()));
        // Legal initialization may have installed a new opaque state. This is
        // the independent pre-Adopt baseline, never the post-edit result.
        assert!(baseline.opaque_state.is_some());
        assert_eq!(observed.facts().len(), 1);
        let fact = &observed.facts()[0];
        assert_eq!(&fact.parameter, parameter);
        assert_eq!(fact.revision, observed.source_revision());
        assert!(matches!(
            fact.origin,
            mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { rescan_flags: 0 }
        ));
        let author_bits = fact.author_value_bits.expect("actual queried author-domain scalar");
        assert!(f64::from_bits(author_bits).is_finite());
        assert_eq!(
            state.native_audio_parameter_presentation(payload, parameter),
            Some(fact),
            "Close retains the exact ticketed query Fact in the sole presentation cache"
        );
        let target = observed.target();
        assert_eq!(target.instance, directory.id);
        let edit = crate::app_ui::test_utils::native_parameter_adoption_payload(
            state,
            payload.address,
            directory.id,
            parameter,
        )
        .expect("actual visible closed Rack row offers Adopt");
        assert_eq!(edit.target.lease.session_id, payload.session_id);
        assert_eq!(edit.target.lease.sequence_id, payload.sequence_id);
        assert_eq!(
            edit.expected,
            DisplayedNativeParameterIdentity {
                root_plan: target.root_plan,
                root_session: target.root_session,
                local_plan: target.local_plan,
                local_session: target.local_session,
                continuity_epoch: target.continuity_epoch,
                binding: target.binding,
                installed_generation: target.installed_generation,
                process_id: observed.process_id(),
                object_serial: observed.object_serial(),
                source_revision: observed.source_revision(),
                fact_revision: fact.revision,
                author_value_bits: author_bits,
            },
            "Rack authority is tied to the complete actual query witness"
        );
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        assert_eq!(history.pending_entries, 0);
        let generation = state.project_author_generation();
        assert_eq!(edit.expected_author_generation, generation);
        let query_serial = state.native_parameter_query_serial;
        state
            .dispatch_action(
                ParameterGestureAction::AdoptDisplayedNativeParameter { edit: edit.clone() }
                    .action(),
            )
            .expect("one ordinary Host-origin Adopt after actual Closed");
        assert_one_host_value(state, &baseline, parameter, author_bits);
        let adopted_history = state.authoring.as_ref().unwrap().history().diagnostics();
        assert_eq!(adopted_history.undo_entries, history.undo_entries + 1);
        assert_eq!(adopted_history.pending_entries, 0);
        assert_eq!(adopted_history.redo_entries, 0);
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(state.native_parameter_query_serial, query_serial);
        assert!(state.native_audio_renderer.is_none());
        assert!(!state.audio_playback.native_editor_source_available());
        assert!(state.native_audio_parameter_presentation(payload, parameter).is_none());
        let adopted = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        assert!(
            state
                .dispatch_action(
                    ParameterGestureAction::AdoptDisplayedNativeParameter { edit: edit.clone() }
                        .action()
                )
                .is_err(),
            "the old Fact capability cannot be adopted twice"
        );
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            adopted
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            adopted_history
        );
        let project = state.authoring.as_ref().unwrap().project_id();
        let old_session = state.authoring_session_id().unwrap();
        let clap = state.clap_catalog.as_ref().unwrap().clone();
        let vst3 = state.vst3_catalog.as_ref().unwrap().clone();
        state
            .save_project_file_as(archive.clone())
            .expect("public Surge author Save As");
        assert!(archive.is_file());
        state.close_project().expect("public ProjectClose after closed adoption");
        // The original Surge driver's outer owner alone consumes old Audio
        // cleanup. This child owns only the new author-state reload App.
        let mut fresh = AppState::with_native_audio_catalogs(clap, vst3);
        assert!(fresh.shutdown_audio_playback_and_wait().all_workers_terminated());
        reopened = Some(fresh);
        let fresh = reopened.as_mut().unwrap();
        fresh
            .open_project_file(archive.clone())
            .expect("public new-App Surge author reopen");
        assert_eq!(fresh.authoring.as_ref().unwrap().project_id(), project);
        assert_ne!(fresh.authoring_session_id(), Some(old_session));
        assert_one_host_value(fresh, &baseline, parameter, author_bits);
        assert!(!fresh.can_undo_action() && !fresh.can_redo_action());
        assert!(fresh.native_audio_editor.is_none());
        assert!(fresh.native_audio_renderer.is_none());
        assert_eq!(fresh.native_parameter_query_serial, 0);
        let reloaded = serde_json::to_value(fresh.authoring.as_ref().unwrap().document()).unwrap();
        let reloaded_history = fresh.authoring.as_ref().unwrap().history().diagnostics();
        assert!(
            fresh
                .dispatch_action(
                    ParameterGestureAction::AdoptDisplayedNativeParameter { edit }.action()
                )
                .is_err(),
            "old Session/transient Fact is not reopened authority"
        );
        assert_eq!(
            serde_json::to_value(fresh.authoring.as_ref().unwrap().document()).unwrap(),
            reloaded
        );
        assert_eq!(
            fresh.authoring.as_ref().unwrap().history().diagnostics(),
            reloaded_history
        );
        assert!(
            Instant::now() < deadline,
            "finite Surge workflow exceeded 30s control budget"
        );
        author_bits
    }));
    let cleanup = reopened.as_mut().map(AppState::shutdown_audio_playback_and_wait);
    let clean = cleanup.as_ref().is_none_or(|e| {
        e.all_workers_terminated()
            && e.shutdown_resource_facts_complete_at_deadline
            && !e.shutdown_owner_lifetime_unresolved_at_deadline
            && e.foreign_owner_panics == 0
            && e.foreign_owner_abandonments == 0
    });
    let author_bits = match result {
        Ok(bits) => bits,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    assert!(clean, "new author-state App cleanup: {cleanup:?}");
    author_bits
}
