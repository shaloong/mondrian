use super::*;
use mondrian_core::AudioSamplePosition;

#[derive(Default)]
struct CursorCollector {
    sessions: [Option<AudioSessionCursorEvidence>; 8],
    processors: [Option<AudioProcessorCursorEvidence>; 8],
    session_count: usize,
    processor_count: usize,
}

impl AudioContinuityEvidenceVisitor for CursorCollector {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        let slot = self
            .sessions
            .get_mut(self.session_count)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        *slot = Some(evidence);
        self.session_count += 1;
        Ok(())
    }

    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        let slot = self
            .processors
            .get_mut(self.processor_count)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        *slot = Some(evidence);
        self.processor_count += 1;
        Ok(())
    }
}

fn cursor_delay_plan() -> Arc<PreparedAudioPlan> {
    prepared_parallel_delay_plan(&TestDelayProcessorResolver {
        latency_frames: 2,
        realtime_capable: true,
        ..Default::default()
    })
}

#[test]
fn continuity_evidence_separates_public_head_from_generated_lookahead() {
    let plan = cursor_delay_plan();
    let mut session = AudioRenderSession::new(plan).expect("prepared Session");
    let mut initial = CursorCollector::default();
    session.visit_continuity_evidence(&mut initial).expect("unentered facts");
    assert_eq!(
        initial.sessions[0].expect("Session").state,
        AudioSessionCursorState::Unentered
    );
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(91),
            start_sample: 0,
        })
        .expect("entry");
    let mut source = RampSource::default();
    let mut pcm = [0.0; 4];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { transport_activity: None, start_sample: 0, frames: 4 },
            &mut pcm,
        )
        .expect("actual DSP");
    // Two independent paths sum the mathematical ramp. The 2-sample latency
    // causes six internal samples to be evaluated for four public samples.
    assert_eq!(pcm, [2.0, 4.0, 6.0, 8.0]);
    let mut facts = CursorCollector::default();
    session.visit_continuity_evidence(&mut facts).expect("exclusive facts");
    let evidence = facts.sessions[0].expect("Session");
    let AudioSessionCursorState::Active {
        epoch,
        next_public_sample,
        next_execution_sample,
        alignment_primed,
    } = evidence.state
    else {
        panic!("active Session");
    };
    assert_eq!(epoch, AudioContinuityEpoch::new(91));
    assert_eq!(next_public_sample.sample(), 4);
    assert_eq!(next_execution_sample.sample(), 6);
    assert!(alignment_primed);
    assert_eq!(evidence.public_output_lookahead_frames, 2);
    let processor = facts
        .processors
        .iter()
        .flatten()
        .find(|value| matches!(value.state, AudioProcessorCursorState::Active { .. }))
        .expect("actual hosted history");
    let AudioProcessorCursorState::Active { next_input_signal_sample, .. } = processor.state else {
        unreachable!();
    };
    assert_eq!(next_input_signal_sample.sample(), 6);
    assert_eq!(processor.identity, evidence.identity);
    // A consumer position inside returned PCM is a different coordinate;
    // this unit gate is not physical-output or callback qualification.
    let callback_anchor = AudioSamplePosition::new(1, evidence.sample_rate);
    assert!(callback_anchor.sample() < next_public_sample.sample());
    assert!(callback_anchor.sample() < next_input_signal_sample.sample());
    // Visiting causes no reset or replay; the next ordinary window continues
    // the same delay state and returns the independent mathematical suffix.
    let mut suffix = [0.0; 2];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { transport_activity: None, start_sample: 4, frames: 2 },
            &mut suffix,
        )
        .expect("unchanged continuation");
    assert_eq!(suffix, [10.0, 12.0]);
}

#[test]
fn continuity_evidence_keeps_signed_negative_signal_coordinates() {
    let mut session = AudioRenderSession::new(cursor_delay_plan()).expect("Session");
    let epoch = AudioContinuityEpoch::new(95);
    session
        .enter_state(AudioStateEntry { epoch, start_sample: -10 })
        .expect("negative entry");
    let mut source = RampSource::default();
    let mut pcm = [1.0; 1];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { transport_activity: None, start_sample: -10, frames: 1 },
            &mut pcm,
        )
        .expect("actual negative-grid DSP");
    assert_eq!(pcm, [0.0]);
    let mut facts = CursorCollector::default();
    session.visit_continuity_evidence(&mut facts).expect("signed facts");
    let AudioSessionCursorState::Active { next_public_sample, next_execution_sample, .. } =
        facts.sessions[0].expect("Session").state
    else {
        panic!("active Session");
    };
    assert_eq!(next_public_sample.sample(), -9);
    assert_eq!(next_execution_sample.sample(), -7);
    let processor = facts
        .processors
        .iter()
        .flatten()
        .find(|value| matches!(value.state, AudioProcessorCursorState::Active { .. }))
        .expect("actual negative hosted history");
    let AudioProcessorCursorState::Active { next_input_signal_sample, .. } = processor.state else {
        unreachable!();
    };
    assert_eq!(next_input_signal_sample.sample(), -7);
}

#[test]
fn continuity_evidence_visits_actual_nested_materializations_with_distinct_identities() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("cursor root");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let root_track = root.audio_tracks[0].id;
    let nested_clip = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(4, 1),
        Some("cursor child".to_owned()),
    )
    .expect("nested Clip");
    root.add_nested_audio_clip(root_track, nested_clip, child_output)
        .expect("nested authoring");
    let contract = AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let mut runtime = AudioProgramRuntime::build(&root, &[child], &RampResolver, contract, None)
        .expect("nested Runtime");
    let mut pcm = [0.0; 4];
    runtime
        .render_into(AudioRenderRequest { transport_activity: None, start_sample: 0, frames: 4 }, &mut pcm)
        .expect("actual nested DSP");
    assert_eq!(pcm, [1.0, 2.0, 3.0, 4.0]);
    let mut facts = CursorCollector::default();
    runtime.visit_continuity_evidence(&mut facts).expect("actual closure facts");
    assert_eq!(facts.session_count, 2);
    let parent = facts.sessions[0].expect("root");
    let nested = facts.sessions[1].expect("nested");
    assert_ne!(parent.identity.session_id, nested.identity.session_id);
    assert_ne!(
        parent.identity.prepared_plan_id,
        nested.identity.prepared_plan_id
    );
    // Stateless root/child Sessions store no contiguous DSP cursor. Observing
    // the actual completed PCM must not manufacture a private execution head.
    assert_eq!(parent.state, AudioSessionCursorState::Stateless);
    assert_eq!(nested.state, AudioSessionCursorState::Stateless);
}

#[test]
fn continuity_evidence_plan_aliases_still_name_exclusive_session_owners() {
    let plan = cursor_delay_plan();
    let first = AudioRenderSession::new(Arc::clone(&plan)).expect("first");
    let second = AudioRenderSession::new(plan).expect("second");
    let mut a = CursorCollector::default();
    let mut b = CursorCollector::default();
    first.visit_continuity_evidence(&mut a).expect("first facts");
    second.visit_continuity_evidence(&mut b).expect("second facts");
    let a = a.sessions[0].expect("first Session").identity;
    let b = b.sessions[0].expect("second Session").identity;
    assert_eq!(a.prepared_plan_id, b.prepared_plan_id);
    assert_ne!(a.session_id, b.session_id);
}

#[test]
fn continuity_evidence_partial_failure_has_no_invented_restore_cursor() {
    let mut session = AudioRenderSession::new(cursor_delay_plan()).expect("Session");
    let epoch = AudioContinuityEpoch::new(93);
    session.enter_state(AudioStateEntry { epoch, start_sample: 0 }).expect("entry");
    let mut pcm = [0.0; 2];
    assert!(session
        .render_into(
            &mut FailingSource,
            AudioRenderRequest { transport_activity: None, start_sample: 0, frames: 2 },
            &mut pcm
        )
        .is_err());
    let mut facts = CursorCollector::default();
    session.visit_continuity_evidence(&mut facts).expect("failure facts");
    assert_eq!(
        facts.sessions[0].expect("Session").state,
        AudioSessionCursorState::Poisoned { epoch }
    );
}

#[test]
fn continuity_evidence_bounded_visitor_rejection_changes_no_signal_history() {
    struct Reject;
    impl AudioContinuityEvidenceVisitor for Reject {
        fn session(
            &mut self,
            _evidence: AudioSessionCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            Err(AudioExecutionError::BufferTooLarge)
        }
        fn processor(
            &mut self,
            _evidence: AudioProcessorCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            panic!("no callback after refusal");
        }
    }
    let mut session = AudioRenderSession::new(cursor_delay_plan()).expect("Session");
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(94),
            start_sample: 0,
        })
        .expect("entry");
    assert!(matches!(
        session.visit_continuity_evidence(&mut Reject),
        Err(AudioExecutionError::BufferTooLarge)
    ));
    let mut source = RampSource::default();
    let mut pcm = [0.0; 2];
    session
        .render_into(
            &mut source,
            AudioRenderRequest { transport_activity: None, start_sample: 0, frames: 2 },
            &mut pcm,
        )
        .expect("first actual DSP");
    assert_eq!(pcm, [2.0, 4.0]);
}
