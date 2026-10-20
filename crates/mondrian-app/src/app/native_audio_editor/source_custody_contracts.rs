//! Actual builtin Media-slot custody controls. The idle Work latch and Core
//! proposal are App metadata models, never native installation/ACK evidence.

use super::attachment_continuity_contracts::opened_author;
use super::*;
use crate::app::{audio_rendering::TimelineAudioPcmRenderer, playback::AppAudioPlayback};
use mondrian_core::{AudioSamplePosition, AudioSampleRate};
use mondrian_media::AudioPcmRenderer;
use std::sync::{Arc, Weak};

fn anchor(state: &AppState) -> AudioSamplePosition {
    AudioSamplePosition::new(0, AudioSampleRate::new(state.audio_sample_rate).unwrap())
}

fn install(state: &mut AppState, renderer: &Arc<TimelineAudioPcmRenderer>) {
    let anchor = anchor(state);
    let AppAudioPlayback::Available(playback) = &mut state.audio_playback else {
        panic!("actual Media execution is required")
    };
    let owner: Arc<dyn AudioPcmRenderer> = renderer.clone();
    playback.prepare(anchor, owner, renderer.continuity_model()).unwrap();
    let concrete = Arc::downgrade(renderer);
    let identity: Weak<dyn AudioPcmRenderer> = concrete;
    assert!(state.audio_playback.matches_source_weak(&identity));
}

fn clear(state: &mut AppState) {
    let anchor = anchor(state);
    let AppAudioPlayback::Available(playback) = &mut state.audio_playback else {
        panic!("actual Media execution is required")
    };
    playback.clear_source(anchor).unwrap();
}

fn idle_state_entry_work(
    state: &AppState,
    renderer: &Arc<TimelineAudioPcmRenderer>,
) -> NativeAudioAuthorWork {
    // Model the already settled App latch only. The real Media slot below is
    // installed/cleared by its public API; this creates no native permission.
    NativeAudioAuthorWork::Single(NativeAudioSingleAuthorWork {
        session_id: state.authoring_session_id().unwrap(),
        sequence_id: state.active_sequence().unwrap().id,
        renderer: Arc::downgrade(renderer),
        gesture: None,
        outstanding: None,
        state_entry_accepted: true,
    })
}

#[test]
fn native_author_retained_source_requires_exact_current_media_slot() {
    let (_directory, mut state, proposal, renderer_a) = opened_author();
    let (_other_directory, mut other, _, renderer_b) = opened_author();
    let work = idle_state_entry_work(&state, &renderer_a);
    let session = state.authoring_session_id().unwrap();
    install(&mut state, &renderer_a);
    let selected = work
        .retained_state_entry_source(None, session, proposal.sequence(), &state.audio_playback)
        .expect("the exact actual slot keeps this source alive");
    assert!(Arc::ptr_eq(&selected, &renderer_a));
    drop(selected);
    assert!(!work.blocks_source_replacement());

    install(&mut state, &renderer_b);
    assert!(renderer_a.native_author_snapshot_current());
    assert!(
        work.single()
            .unwrap()
            .retained_state_entry_source(None, session, proposal.sequence())
            .is_some(),
        "the old Weak is live and its metadata still qualifies"
    );
    assert!(
        work.retained_state_entry_source(None, session, proposal.sequence(), &state.audio_playback)
            .is_none(),
        "Media B must never return the retired A"
    );
    let concrete = Arc::downgrade(&renderer_a);
    let old: Weak<dyn AudioPcmRenderer> = concrete;
    assert!(!state.audio_playback.matches_source_weak(&old));
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
    assert!(other.shutdown_audio_playback_and_wait().all_workers_terminated());
}

#[test]
fn native_author_snapshot_rejects_live_global_weak_without_media_slot() {
    let (_directory, mut state, _, renderer) = opened_author();
    let work = idle_state_entry_work(&state, &renderer);
    let global = state.native_audio_renderer.as_ref().unwrap().clone();
    install(&mut state, &renderer);
    assert!(work.snapshot_current_for_source(&global, &state.audio_playback));

    clear(&mut state);
    assert!(Weak::ptr_eq(
        state.native_audio_renderer.as_ref().unwrap(),
        &global
    ));
    assert!(renderer.native_author_snapshot_current());
    assert!(
        work.single().unwrap().snapshot_current_for_source(&global),
        "historical identity and metadata alone still pass the old helper"
    );
    assert!(!work.snapshot_current_for_source(&global, &state.audio_playback));
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
}

#[test]
fn native_author_queue_without_media_slot_retains_uncompiled_original_prefix() {
    let (_directory, mut state, proposal, renderer) = opened_author();
    state.observe_native_audio_author_proposal(proposal.clone()).unwrap();
    let work = state
        .native_audio_author_work
        .as_ref()
        .and_then(NativeAudioAuthorWork::single)
        .unwrap();
    let gesture = work.gesture.as_ref().unwrap().clone();
    let pending = work.outstanding.as_ref().unwrap();
    let snapshot = Arc::clone(&pending.snapshot);
    assert!(pending.programs.is_none());
    assert!(!pending.queued);
    let author = state.authoring.as_ref().unwrap();
    let document = serde_json::to_value(author.document()).unwrap();
    let generation = author.author_generation();
    let history = author.history().diagnostics();

    install(&mut state, &renderer);
    clear(&mut state);
    assert!(renderer.native_author_snapshot_current());
    state.queue_native_audio_author_handoff().unwrap();
    let work = state
        .native_audio_author_work
        .as_ref()
        .and_then(NativeAudioAuthorWork::single)
        .unwrap();
    let retained = work.gesture.as_ref().unwrap();
    assert_eq!(retained.gesture_id(), gesture.gesture_id());
    assert_eq!(retained.owner(), gesture.owner());
    assert_eq!(retained.processor_id(), gesture.processor_id());
    assert_eq!(retained.session_id(), gesture.session_id());
    assert_eq!(retained.sequence_id(), gesture.sequence_id());
    assert!(work.blocks_source_replacement());
    let pending = work.outstanding.as_ref().unwrap();
    assert!(pending.proposal.same_retained_proposal(&proposal));
    assert!(Arc::ptr_eq(&pending.snapshot, &snapshot));
    assert!(
        pending.programs.is_none(),
        "do not compile or construct a provider for a retired slot"
    );
    assert!(pending.installed.is_none());
    assert!(!pending.queued);
    assert_eq!(
        state.status_hint,
        Some((AppMessage::new("native-editor-control-pending"), true))
    );
    let author = state.authoring.as_ref().unwrap();
    assert_eq!(serde_json::to_value(author.document()).unwrap(), document);
    assert_eq!(author.author_generation(), generation);
    assert_eq!(author.history().diagnostics(), history);
    assert!(snapshot.enter_commit().is_ok());
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
}
