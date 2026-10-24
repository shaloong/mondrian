use super::*;
use mondrian_core::TimelineTransportActivity as Activity;

type Calls = Arc<Mutex<Vec<(i64, usize, Option<Activity>)>>>;

struct Observer(Calls, usize);
struct Factory(Calls, usize);
struct Processor(Calls);

impl AudioProcessorResolver for Observer {
    fn prepare(
        &self,
        _: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        Ok(Arc::new(Factory(self.0.clone(), self.1)))
    }
}
impl AudioProcessorFactory for Factory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(self.1, AudioProcessorTail::None, self.1 != 0, true, true, 0)
            .unwrap()
    }
    fn create(
        &self,
        _: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        Ok(Box::new(Processor(self.0.clone())))
    }
}
impl AudioProcessor for Processor {
    fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }
    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        io: &mut dyn AudioProcessorAudioIo,
        _: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let activity = context.transport_activity();
        self.0.lock().unwrap().push((
            context.request().start_sample,
            context.request().frames,
            activity,
        ));
        io.main_interleaved().fill(match activity {
            None => 1.0,
            Some(Activity::Stopped) => 2.0,
            Some(Activity::Playing) => 3.0,
        });
        Ok(())
    }
}

fn contract() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 8,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 4,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}
fn child() -> Sequence {
    let mut sequence = Sequence::new("transport observer");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
        AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
    );
    sequence
}
fn request(start_sample: i64, transport_activity: Option<Activity>) -> AudioRenderRequest {
    AudioRenderRequest { start_sample, frames: 2, transport_activity }
}

#[test]
fn entered_transport_refusal_preserves_pcm_callbacks_and_same_epoch_retry() {
    let sequence = child();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &RampResolver,
        &Observer(calls.clone(), 0),
        contract(),
        None,
    )
    .unwrap();
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .unwrap();
    let mut pcm = [0.0; 2];
    runtime.render_into(request(0, Some(Activity::Playing)), &mut pcm).unwrap();
    assert_eq!(pcm, [3.0; 2]);
    for activity in [Some(Activity::Stopped), None] {
        pcm.fill(99.0);
        let count = calls.lock().unwrap().len();
        assert!(matches!(
            runtime.render_into(request(2, activity), &mut pcm),
            Err(AudioExecutionError::TransportActivityChanged)
        ));
        assert_eq!(pcm, [99.0; 2]);
        assert_eq!(
            calls.lock().unwrap().len(),
            count,
            "refusal must not consume callbacks"
        );
    }
    runtime.render_into(request(2, Some(Activity::Playing)), &mut pcm).unwrap();
    assert_eq!(pcm, [3.0; 2]);
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(2),
            start_sample: 0,
        })
        .unwrap();
    runtime.render_into(request(0, Some(Activity::Stopped)), &mut pcm).unwrap();
    assert_eq!(pcm, [2.0; 2]);
    assert_eq!(
        calls.lock().unwrap().iter().map(|c| c.2).collect::<Vec<_>>(),
        [
            Some(Activity::Playing),
            Some(Activity::Playing),
            Some(Activity::Stopped)
        ]
    );
}

#[test]
fn unentered_stateless_nested_cache_is_keyed_by_transport_and_reuses_equal_context() {
    let child = child();
    let mut root = Sequence::new("nested transport");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track = root.audio_tracks[0].id;
    let clip = Clip::new_nested_sequence(child.id, TimelineTime::ZERO, tt(4, 1), None).unwrap();
    root.add_nested_audio_clip(track, clip, child.audio_program.outputs[0].id)
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
        &root,
        &[child],
        &RampResolver,
        &Observer(calls.clone(), 0),
        contract(),
        None,
    )
    .unwrap();
    let mut pcm = [0.0; 2];
    runtime.render_into(request(0, Some(Activity::Playing)), &mut pcm).unwrap();
    assert_eq!(pcm, [3.0; 2]);
    let first = calls.lock().unwrap().len();
    // A malformed changed-context request must not invalidate valid prefetched PCM.
    let mut wrong_shape = [99.0; 1];
    assert!(matches!(runtime.render_into(request(0, Some(Activity::Stopped)), &mut wrong_shape),
        Err(AudioExecutionError::OutputSizeMismatch)));
    assert_eq!(wrong_shape, [99.0]);
    assert_eq!(calls.lock().unwrap().len(), first);
    runtime.render_into(request(0, Some(Activity::Playing)), &mut pcm).unwrap();
    assert_eq!(
        calls.lock().unwrap().len(),
        first,
        "equal nested context must hit its cache"
    );
    runtime.render_into(request(0, Some(Activity::Stopped)), &mut pcm).unwrap();
    assert_eq!(pcm, [2.0; 2]);
    assert!(
        calls.lock().unwrap().len() > first,
        "old prefetched Playing PCM must not be reused"
    );
    runtime.render_into(request(0, None), &mut pcm).unwrap();
    assert_eq!(
        pcm, [1.0; 2],
        "absence remains distinct from an explicit Stopped context"
    );
}

#[test]
fn pdc_priming_and_execution_keep_one_activity_and_canceled_change_is_pure() {
    let sequence = child();
    let calls = Arc::new(Mutex::new(Vec::new()));
    // The observer's declared latency exercises PDC scheduling, not a DSP delay oracle.
    let mut runtime = AudioProgramRuntime::build_with_processor_resolver(
        &sequence, &[], &RampResolver, &Observer(calls.clone(), 5), contract(), None,
    ).unwrap();
    runtime.enter_state(AudioStateEntry { epoch: AudioContinuityEpoch::new(1), start_sample: -2 }).unwrap();
    let mut pcm = [0.0; 2];
    runtime.render_into(request(-2, Some(Activity::Playing)), &mut pcm).unwrap();
    assert_eq!(*calls.lock().unwrap(), [(-2, 4, Some(Activity::Playing)),
        (2, 1, Some(Activity::Playing)), (3, 2, Some(Activity::Playing))]);
    let cancellation = mondrian_core::ExecutionCancellationToken::new();
    cancellation.cancel();
    pcm.fill(99.0);
    assert!(runtime.render_into_cancellable(request(0, Some(Activity::Stopped)), &mut pcm, &cancellation).is_err());
    assert_eq!(pcm, [99.0; 2]);
    assert_eq!(calls.lock().unwrap().len(), 3);
    runtime.render_into(request(0, Some(Activity::Playing)), &mut pcm).unwrap();
    assert_eq!(calls.lock().unwrap().last(), Some(&(5, 2, Some(Activity::Playing))));
}
