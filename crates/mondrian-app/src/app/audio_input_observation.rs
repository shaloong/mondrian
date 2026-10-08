//! Bounded, read-only processor input observations for product controls.

use std::collections::VecDeque;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use mondrian_audio::{
    inspect_audio_processor_input_author, inspect_audio_processor_inputs,
    AudioProcessorInputInspection, AudioProcessorInputInspectionRequest, AudioProcessorResolver,
};
use mondrian_core::{MondrianError, Result};
use mondrian_timeline::{audio::AudioProcessorInstance, Sequence};

use super::product_action::{AudioProcessorInputQueryOperation, AudioProcessorInputQueryPayload};
use super::AppState;

const INPUT_OBSERVATION_CACHE_CAPACITY: usize = 16;
const INPUT_OBSERVATION_QUEUE_CAPACITY: usize = 16;

#[derive(Debug, Clone, PartialEq)]
struct ObservationKey {
    payload: AudioProcessorInputQueryPayload,
    request: AudioProcessorInputInspectionRequest,
    processor: AudioProcessorInstance,
}

struct Work {
    key: ObservationKey,
    sequence: Sequence,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum AudioInputObservationFailure {
    #[error(transparent)]
    Inspection(#[from] mondrian_audio::AudioProcessorInputInspectionError),
    #[error("Input observation worker could not start: {0}")]
    Spawn(String),
    #[error("Input observation worker disconnected")]
    Disconnected,
    #[error("Input observation worker panicked")]
    Panicked,
    #[error("Input observation worker produced no result")]
    MissingResult,
}

struct CachedObservation {
    result: std::result::Result<Arc<ObservedProcessorInputs>, AudioInputObservationFailure>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ObservedProcessorInputs {
    pub(crate) binding: super::product_action::AudioProcessorInputBinding,
    pub(crate) inputs: AudioProcessorInputInspection,
}

type QueryResult = std::result::Result<AudioProcessorInputInspection, AudioInputObservationFailure>;

struct Job {
    key: ObservationKey,
    receiver: mpsc::Receiver<QueryResult>,
    worker: JoinHandle<()>,
    reply: Option<QueryResult>,
    canceled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AudioInputObservation {
    Unknown,
    Pending,
    Ready(Arc<ObservedProcessorInputs>),
    Failed(AudioInputObservationFailure),
}

/// One physical job, a bounded coalescing FIFO and a bounded transient cache.
#[derive(Default)]
pub(super) struct AudioInputObservationService {
    active: Option<Job>,
    queued: VecDeque<Work>,
    cache: VecDeque<(ObservationKey, CachedObservation)>,
}

impl AudioInputObservationService {
    fn observation(&self, key: &ObservationKey) -> AudioInputObservation {
        if self.active.as_ref().is_some_and(|job| !job.canceled && job.key == *key)
            || self.queued.iter().any(|work| work.key == *key)
        {
            return AudioInputObservation::Pending;
        }
        match self
            .cache
            .iter()
            .rev()
            .find(|(cached, _)| cached == key)
            .map(|(_, cached)| cached)
        {
            Some(CachedObservation { result: Ok(observed) }) => {
                AudioInputObservation::Ready(Arc::clone(observed))
            }
            Some(CachedObservation { result: Err(error) }) => {
                AudioInputObservation::Failed(error.clone())
            }
            None => AudioInputObservation::Unknown,
        }
    }

    fn remember(&mut self, key: ObservationKey, result: QueryResult) {
        self.cache.retain(|(cached, _)| cached != &key);
        let result = result.map(|inputs| {
            Arc::new(ObservedProcessorInputs {
                binding: super::product_action::AudioProcessorInputBinding {
                    session_id: key.payload.session_id,
                    sequence_id: key.payload.sequence_id,
                    target: key.payload.target,
                    processor_id: key.payload.processor_id,
                    observation_id: mondrian_core::AudioProcessorInputObservationId::new(),
                },
                inputs,
            })
        });
        self.cache.push_back((key, CachedObservation { result }));
        while self.cache.len() > INPUT_OBSERVATION_CACHE_CAPACITY {
            self.cache.pop_front();
        }
    }

    fn request(
        &mut self,
        work: Work,
        resolver: Arc<dyn AudioProcessorResolver>,
        current: impl Fn(&ObservationKey) -> bool,
    ) -> Result<()> {
        self.queued.retain(|queued| current(&queued.key));
        if self.active.as_ref().is_some_and(|job| !job.canceled && job.key == work.key) {
            return Ok(());
        }
        if let Some(queued) = self
            .queued
            .iter_mut()
            .find(|queued| same_target(&queued.key.payload, &work.key.payload))
        {
            *queued = work;
            return Ok(());
        }
        if self.active.is_some() && self.queued.len() == INPUT_OBSERVATION_QUEUE_CAPACITY {
            return Err(observation_error("Too many pending processor input inspections; cancel a query or wait for completion"));
        }
        self.cache.retain(|(cached, _)| cached != &work.key);
        if self.active.is_some() {
            self.queued.push_back(work);
        } else {
            self.start(work, resolver);
        }
        Ok(())
    }

    fn start(&mut self, work: Work, resolver: Arc<dyn AudioProcessorResolver>) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let key = work.key;
        let request = key.request;
        let worker = thread::Builder::new()
            .name("mondrian-audio-input-observation".to_owned())
            .spawn(move || {
                let result =
                    inspect_audio_processor_inputs(&work.sequence, request, resolver.as_ref())
                        .map_err(AudioInputObservationFailure::Inspection);
                let _ = sender.send(result);
            });
        match worker {
            Ok(worker) => {
                self.active = Some(Job {
                    key,
                    receiver,
                    worker,
                    reply: None,
                    canceled: false,
                })
            }
            Err(error) => self.remember(
                key,
                Err(AudioInputObservationFailure::Spawn(error.to_string())),
            ),
        }
    }

    fn cancel(&mut self, payload: &AudioProcessorInputQueryPayload) {
        if let Some(job) = &mut self.active
            && same_target(&job.key.payload, payload)
        {
            job.canceled = true;
        }
        self.queued.retain(|work| !same_target(&work.key.payload, payload));
        self.cache.retain(|(key, _)| !same_target(&key.payload, payload));
    }

    /// Catalog changes invalidate observations without detaching a running job.
    pub(super) fn invalidate(&mut self) {
        if let Some(job) = &mut self.active {
            job.canceled = true;
        }
        self.queued.clear();
        self.cache.clear();
    }

    fn retain_session(
        &mut self,
        session_id: Option<mondrian_editor_state::AuthoringSessionId>,
    ) -> bool {
        let mut changed = false;
        if let Some(job) = &mut self.active
            && !job.canceled
            && Some(job.key.payload.session_id) != session_id
        {
            job.canceled = true;
            changed = true;
        }
        let before = (self.queued.len(), self.cache.len());
        self.queued.retain(|work| Some(work.key.payload.session_id) == session_id);
        self.cache.retain(|(key, _)| Some(key.payload.session_id) == session_id);
        changed || before != (self.queued.len(), self.cache.len())
    }

    fn poll(
        &mut self,
        resolver: Arc<dyn AudioProcessorResolver>,
        current: impl Fn(&ObservationKey) -> bool,
    ) -> bool {
        let mut changed = false;
        if let Some(job) = &mut self.active {
            if job.reply.is_none() {
                match job.receiver.try_recv() {
                    Ok(reply) => job.reply = Some(reply),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        job.reply = Some(Err(AudioInputObservationFailure::Disconnected));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            // A reply does not prove thread termination. Never join running work on UI.
            if !job.worker.is_finished() {
                return false;
            }
        }
        if let Some(job) = self.active.take() {
            let reply = job.reply.or_else(|| job.receiver.try_recv().ok());
            let result = if job.worker.join().is_err() {
                Err(AudioInputObservationFailure::Panicked)
            } else {
                reply.unwrap_or(Err(AudioInputObservationFailure::MissingResult))
            };
            if !job.canceled && current(&job.key) {
                self.remember(job.key, result);
            }
            changed = true;
        }
        while let Some(work) = self.queued.pop_front() {
            changed = true;
            if current(&work.key) {
                self.start(work, Arc::clone(&resolver));
                break;
            }
        }
        changed
    }
}

fn same_target(
    first: &AudioProcessorInputQueryPayload,
    second: &AudioProcessorInputQueryPayload,
) -> bool {
    first.session_id == second.session_id
        && first.sequence_id == second.sequence_id
        && first.target == second.target
        && first.processor_id == second.processor_id
}

fn observation_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "audio_processor_input_observation".into(),
        reason: reason.into(),
    }
}

impl AppState {
    fn input_observation_key(
        &self,
        mut payload: AudioProcessorInputQueryPayload,
    ) -> Result<ObservationKey> {
        if self.project_close_blocks_actions()
            || self.authoring_session_id() != Some(payload.session_id)
        {
            return Err(observation_error(
                "Processor input request belongs to an unavailable editing session",
            ));
        }
        let sequence = self
            .active_sequence()
            .filter(|sequence| sequence.id == payload.sequence_id)
            .ok_or_else(|| {
                observation_error("Processor input request belongs to another Sequence")
            })?;
        let processor =
            inspect_audio_processor_input_author(sequence, payload.target, payload.processor_id)
                .map_err(|error| observation_error(error.to_string()))?
                .clone();
        payload.operation = AudioProcessorInputQueryOperation::Query;
        Ok(ObservationKey {
            request: AudioProcessorInputInspectionRequest {
                target: payload.target,
                processor_id: payload.processor_id,
                render_contract: self
                    .plugin_render_contract(sequence.settings.audio_channel_layout),
            },
            payload,
            processor,
        })
    }

    pub(super) fn query_audio_processor_inputs(
        &mut self,
        payload: AudioProcessorInputQueryPayload,
    ) -> Result<()> {
        if payload.operation == AudioProcessorInputQueryOperation::Cancel {
            if self.authoring_session_id() != Some(payload.session_id)
                || self.active_sequence_id() != Some(payload.sequence_id)
            {
                return Err(observation_error(
                    "Processor input cancellation belongs to another editing session",
                ));
            }
            self.audio_input_observations.cancel(&payload);
            return Ok(());
        }
        let key = self.input_observation_key(payload)?;
        let sequence = self
            .active_sequence()
            .cloned()
            .ok_or_else(|| observation_error("No active Sequence"))?;
        let mut service = std::mem::take(&mut self.audio_input_observations);
        service.retain_session(self.authoring_session_id());
        let result = service.request(
            Work { key, sequence },
            Arc::clone(&self.audio_processor_resolver),
            |key| self.input_observation_key(key.payload).is_ok_and(|current| current == *key),
        );
        self.audio_input_observations = service;
        result
    }

    pub(crate) fn audio_processor_input_observation(
        &self,
        payload: AudioProcessorInputQueryPayload,
    ) -> AudioInputObservation {
        match self.input_observation_key(payload) {
            Ok(key) => self.audio_input_observations.observation(&key),
            Err(_) => AudioInputObservation::Unknown,
        }
    }

    pub(crate) fn has_native_audio_control_work(&self) -> bool {
        self.native_audio_restore.is_some()
            || self.audio_input_observations.active.is_some()
            || !self.audio_input_observations.queued.is_empty()
    }

    pub(crate) fn poll_audio_processor_input_observations(&mut self) -> bool {
        let mut service = std::mem::take(&mut self.audio_input_observations);
        let session_changed = service.retain_session(self.authoring_session_id());
        let changed = service.poll(Arc::clone(&self.audio_processor_resolver), |key| {
            self.input_observation_key(key.payload).is_ok_and(|current| current == *key)
        });
        self.audio_input_observations = service;
        changed || session_changed
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::product_action::{AudioProductAction, ProductAction};
    use mondrian_audio::{
        AudioProcessorFactory, AudioProcessorHostError, AudioProcessorInputInspectionTarget,
        AudioProcessorPrepareRequest, BuiltInAudioProcessorResolver,
    };
    use mondrian_timeline::audio::BUILTIN_GAIN_DEFINITION_ID;
    use mondrian_timeline::{AudioChannelStripOwner, AudioChannelStripRack};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };
    use std::time::{Duration, Instant};

    struct ControlledResolver {
        calls: Arc<AtomicUsize>,
        started: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl AudioProcessorResolver for ControlledResolver {
        fn prepare(
            &self,
            request: AudioProcessorPrepareRequest<'_>,
        ) -> std::result::Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.send(()).expect("Notify controlled call");
            self.release
                .lock()
                .expect("Control lock")
                .recv_timeout(Duration::from_secs(5))
                .expect("Release controlled call within test deadline");
            match request.definition() {
                mondrian_timeline::audio::AudioProcessorDefinitionRef::BuiltIn { .. } => {
                    BuiltInAudioProcessorResolver.prepare(request)
                }
                mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
                    plugin_id, ..
                } if plugin_id == "fixture.missing" => Err(AudioProcessorHostError::Unavailable(
                    "fixture missing".to_owned(),
                )),
                _ => Ok(Arc::new(ControlInputFactory(
                    request.render_contract().channel_layout,
                ))),
            }
        }
    }

    // A source-level adapter fixture; this does not claim real CLAP/VST discovery.
    struct ControlInputFactory(mondrian_core::AudioChannelLayout);

    impl AudioProcessorFactory for ControlInputFactory {
        fn auxiliary_input_names(&self) -> Vec<mondrian_audio::AudioProcessorInputName> {
            [("detector.11", "压缩探测 🙂"), ("detector.7", "备用探测")]
                .into_iter()
                .map(|(key, name)| mondrian_audio::AudioProcessorInputName {
                    bus_key: key.to_owned(),
                    name: name.to_owned(),
                })
                .collect()
        }

        fn execution_contract(&self) -> mondrian_audio::AudioProcessorExecutionContract {
            mondrian_audio::AudioProcessorExecutionContract::stateless()
        }
        fn auxiliary_input_contract(&self) -> mondrian_audio::AudioProcessorAuxiliaryInputContract {
            mondrian_audio::AudioProcessorAuxiliaryInputContract {
                buses: ["detector.11", "detector.7"]
                    .into_iter()
                    .map(
                        |key| mondrian_audio::AudioProcessorAuxiliaryInputBusContract {
                            bus_key: key.to_owned(),
                            channel_layout: self.0,
                        },
                    )
                    .collect(),
            }
        }
        fn create(
            &self,
        ) -> std::result::Result<Box<dyn mondrian_audio::AudioProcessor>, AudioProcessorHostError>
        {
            Err(AudioProcessorHostError::InstanceCreation(
                "Control query must not create DSP".to_owned(),
            ))
        }
    }

    pub(crate) type Fixture = (
        AppState,
        Vec<AudioProcessorInputQueryPayload>,
        Arc<AtomicUsize>,
        mpsc::Receiver<()>,
        mpsc::SyncSender<()>,
    );

    pub(crate) fn fixture(count: usize) -> Fixture {
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started) = mpsc::sync_channel(4);
        let (release, release_rx) = mpsc::sync_channel(4);
        let resolver = ControlledResolver {
            calls: Arc::clone(&calls),
            started: started_tx,
            release: Mutex::new(release_rx),
        };
        let mut state = AppState::with_audio_processor_resolver(Arc::new(resolver));
        let mut sequence = Sequence::new("Input observation fixture");
        let output_id = sequence.audio_program.outputs[0].id;
        let processors = (0..count)
            .map(|_| AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1))
            .collect::<Vec<_>>();
        let ids = processors.iter().map(|processor| processor.id).collect::<Vec<_>>();
        sequence.audio_program.outputs[0].strip.pre_fader.processors.extend(processors);
        let sequence_id = sequence.id;
        state.test_set_sequence(Some(sequence));
        let session_id = state.authoring_session_id().expect("Fixture Session");
        let payloads = ids
            .into_iter()
            .map(|processor_id| AudioProcessorInputQueryPayload {
                session_id,
                sequence_id,
                target: AudioProcessorInputInspectionTarget::ChannelStrip {
                    owner: AudioChannelStripOwner::ProgramOutput { output_id },
                    rack: AudioChannelStripRack::PreFader,
                },
                processor_id,
                operation: AudioProcessorInputQueryOperation::Query,
            })
            .collect();
        (state, payloads, calls, started, release)
    }

    pub(crate) fn finish(state: &mut AppState) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.audio_input_observations.active.is_some() {
            state.poll_audio_processor_input_observations();
            assert!(Instant::now() < deadline, "Observation must terminate");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn input_observation_cancellation_retains_one_job_and_other_accepted_requests() {
        let (mut state, payloads, calls, started, release) = fixture(4);
        let generation = state.project_author_generation();
        let before = serde_json::to_vec(state.active_sequence().expect("Active fixture Sequence"))
            .expect("Author snapshot");
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::QueryProcessorInputs(payloads[0]))
                    .into_external_action(),
            )
            .expect("Start first typed control query");
        assert!(!state.can_undo_action());
        started.recv_timeout(Duration::from_secs(2)).expect("First worker started");
        assert_eq!(
            state.audio_processor_input_observation(payloads[0]),
            AudioInputObservation::Pending
        );
        // Coalesce the same active request; preserve distinct accepted FIFO requests.
        state.query_audio_processor_inputs(payloads[0]).expect("Coalesce");
        state.query_audio_processor_inputs(payloads[1]).expect("Queue second");
        state
            .query_audio_processor_inputs(payloads[2])
            .expect("Preserve distinct queued request");
        assert_eq!(
            state.audio_processor_input_observation(payloads[1]),
            AudioInputObservation::Pending
        );
        assert_eq!(
            state.audio_processor_input_observation(payloads[2]),
            AudioInputObservation::Pending
        );
        let mut cancel = payloads[0];
        cancel.operation = AudioProcessorInputQueryOperation::Cancel;
        state.query_audio_processor_inputs(cancel).expect("Cancel active observation");
        assert_eq!(
            state.audio_processor_input_observation(payloads[0]),
            AudioInputObservation::Unknown
        );
        for payload in &payloads[1..3] {
            let mut cancel = *payload;
            cancel.operation = AudioProcessorInputQueryOperation::Cancel;
            state
                .query_audio_processor_inputs(cancel)
                .expect("Explicitly cancel queued query");
            assert_eq!(
                state.audio_processor_input_observation(*payload),
                AudioInputObservation::Unknown
            );
        }
        state
            .query_audio_processor_inputs(payloads[3])
            .expect("Latest replacement after cancellation");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "Cancellation must not detach and spawn replacements"
        );
        assert!(
            state
                .audio_input_observations
                .active
                .as_ref()
                .expect("Owned active job")
                .canceled
        );
        assert!(
            state.has_native_audio_control_work(),
            "Canceled physical work still needs native wakeups"
        );
        release.send(()).expect("Release canceled worker");
        // The queued worker may start only after the old physical worker has finished.
        let deadline = Instant::now() + Duration::from_secs(3);
        while calls.load(Ordering::SeqCst) < 2 {
            state.poll_audio_processor_input_observations();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        started
            .recv_timeout(Duration::from_secs(2))
            .expect("Latest queued worker starts");
        assert_eq!(
            state.audio_processor_input_observation(payloads[2]),
            AudioInputObservation::Unknown
        );
        release.send(()).expect("Release latest worker");
        finish(&mut state);
        assert!(matches!(
            state.audio_processor_input_observation(payloads[3]),
            AudioInputObservation::Ready(_)
        ));
        assert_eq!(state.audio_input_observations.cache.len(), 1);
        assert!(!state.has_native_audio_control_work());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(state.project_author_generation(), generation);
        assert!(!state.can_undo_action());
        assert_eq!(
            serde_json::to_vec(state.active_sequence().expect("Active fixture Sequence"))
                .expect("Author snapshot"),
            before
        );
    }

    #[test]
    fn input_observation_rejects_late_author_render_catalog_and_reopened_session_results() {
        for scenario in 0..5 {
            let (mut state, payloads, calls, started, release) = fixture(1);
            let payload = payloads[0];
            state.query_audio_processor_inputs(payload).expect("Inspect snapshot");
            started.recv_timeout(Duration::from_secs(2)).expect("Worker entered resolver");
            match scenario {
                0 => {
                    state
                        .active_sequence_mut_uncommitted()
                        .expect("Sequence")
                        .audio_program
                        .outputs[0]
                        .strip
                        .pre_fader
                        .processors[0]
                        .bypassed = true
                }
                1 => state.audio_sample_rate += 1,
                2 => state.audio_input_observations.invalidate(),
                3 => {
                    let processor = &mut state
                        .active_sequence_mut_uncommitted()
                        .expect("Sequence")
                        .audio_program
                        .outputs[0]
                        .strip
                        .pre_fader
                        .processors[0];
                    processor.opaque_state = Some(vec![7, 11].into());
                }
                _ => {
                    let same_document =
                        state.active_sequence().cloned().expect("Same archive state");
                    state.test_set_sequence(None);
                    state.test_set_sequence(Some(same_document));
                    assert_ne!(state.authoring_session_id(), Some(payload.session_id));
                    assert!(
                        state.query_audio_processor_inputs(payload).is_err(),
                        "Old UI action must fail before probing"
                    );
                }
            }
            release.send(()).expect("Release stale worker");
            finish(&mut state);
            assert!(
                state.audio_input_observations.cache.is_empty(),
                "Scenario {scenario} refuses stale facts"
            );
            assert_eq!(
                state.audio_processor_input_observation(payload),
                AudioInputObservation::Unknown
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn input_observation_queue_pressure_refuses_new_work_without_losing_accepted_queries() {
        let (mut state, payloads, calls, started, release) =
            fixture(INPUT_OBSERVATION_QUEUE_CAPACITY + 2);
        state.query_audio_processor_inputs(payloads[0]).expect("Active query");
        started.recv_timeout(Duration::from_secs(2)).expect("Controlled active worker");
        for payload in &payloads[1..=INPUT_OBSERVATION_QUEUE_CAPACITY] {
            state.query_audio_processor_inputs(*payload).expect("Accepted queued query");
        }
        let keys = state
            .audio_input_observations
            .queued
            .iter()
            .map(|work| work.key.clone())
            .collect::<Vec<_>>();
        state
            .query_audio_processor_inputs(payloads[1])
            .expect("Coalesce at full capacity");
        assert!(state
            .query_audio_processor_inputs(*payloads.last().expect("Overflow query"))
            .is_err());
        assert_eq!(
            state
                .audio_input_observations
                .queued
                .iter()
                .map(|work| work.key.clone())
                .collect::<Vec<_>>(),
            keys
        );
        for payload in &payloads[..=INPUT_OBSERVATION_QUEUE_CAPACITY] {
            assert_eq!(
                state.audio_processor_input_observation(*payload),
                AudioInputObservation::Pending
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        state.audio_input_observations.invalidate();
        release.send(()).expect("Release invalidated active job");
        finish(&mut state);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "Invalidated queue never starts native work"
        );
        assert!(state.audio_input_observations.queued.is_empty());
    }

    #[test]
    fn input_observation_cache_is_bounded_reads_are_quiet_and_control_intents_round_trip() {
        let (mut state, payloads, calls, started, release) =
            fixture(INPUT_OBSERVATION_CACHE_CAPACITY + 2);
        let generation = state.project_author_generation();
        for payload in &payloads {
            let action = ProductAction::Audio(AudioProductAction::QueryProcessorInputs(*payload))
                .into_external_action();
            assert!(
                matches!(ProductAction::decode_external(&action).expect("Decode control intent"),
                Some(ProductAction::Audio(AudioProductAction::QueryProcessorInputs(decoded))) if decoded == *payload)
            );
            state.query_audio_processor_inputs(*payload).expect("Inspect next processor");
            started.recv_timeout(Duration::from_secs(2)).expect("Worker started");
            release.send(()).expect("Finish query");
            finish(&mut state);
        }
        assert_eq!(
            state.audio_input_observations.cache.len(),
            INPUT_OBSERVATION_CACHE_CAPACITY
        );
        assert_eq!(
            state.audio_processor_input_observation(payloads[0]),
            AudioInputObservation::Unknown
        );
        let last = *payloads.last().expect("Last payload");
        let call_count = calls.load(Ordering::SeqCst);
        for _ in 0..100 {
            assert!(matches!(
                state.audio_processor_input_observation(last),
                AudioInputObservation::Ready(_)
            ));
            assert!(!state.poll_audio_processor_input_observations());
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            call_count,
            "Projection never probes again"
        );
        assert_eq!(state.project_author_generation(), generation);
        let mut cancel = last;
        cancel.operation = AudioProcessorInputQueryOperation::Cancel;
        state.query_audio_processor_inputs(cancel).expect("Clear retained observation");
        assert_eq!(
            state.audio_processor_input_observation(last),
            AudioInputObservation::Unknown
        );
        state.test_set_sequence(None);
        assert!(state.poll_audio_processor_input_observations());
        assert!(state.audio_input_observations.cache.is_empty());
        assert!(!state.has_native_audio_control_work());
    }
    fn observed_native_fixture() -> Fixture {
        let (mut state, payloads, calls, started, release) = fixture(1);
        state.active_sequence_mut_uncommitted().expect("Sequence").audio_program.outputs[0]
            .strip
            .pre_fader
            .processors[0]
            .definition = mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
            plugin_id: "fixture.inputs".to_owned(),
            schema_version: 1,
            binary_sha256: Some([7; 32]),
        };
        state.query_audio_processor_inputs(payloads[0]).expect("Inspect native author");
        started.recv_timeout(Duration::from_secs(2)).expect("Resolver entered");
        release.send(()).expect("Complete inspection");
        finish(&mut state);
        (state, payloads, calls, started, release)
    }

    fn connection_payload(
        state: &AppState,
        query: AudioProcessorInputQueryPayload,
    ) -> super::super::product_action::AudioProcessorInputConnectionPayload {
        let AudioInputObservation::Ready(observed) = state.audio_processor_input_observation(query)
        else {
            panic!("Observed fixture inputs")
        };
        super::super::product_action::AudioProcessorInputConnectionPayload {
            binding: observed.binding,
            source: mondrian_timeline::audio::AudioRouteSource::Track {
                track_id: state.active_sequence().expect("Sequence").audio_tracks[0].id,
                port: mondrian_timeline::audio::AudioChannelStripOutputPort::PostMute,
            },
            bus_key: "detector.7".to_owned(),
        }
    }

    #[test]
    fn observed_input_connection_dispatch_preserves_receipt_multiple_routes_and_undo() {
        let (mut state, queries, calls, _, _) = observed_native_fixture();
        let payload = connection_payload(&state, queries[0]);
        for _ in 0..100 {
            assert_eq!(
                connection_payload(&state, queries[0]).binding,
                payload.binding
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let action =
            ProductAction::Audio(AudioProductAction::ConnectProcessorInput(payload.clone()))
                .into_external_action();
        assert!(
            matches!(ProductAction::decode_external(&action).expect("Codec"), Some(ProductAction::Audio(AudioProductAction::ConnectProcessorInput(decoded))) if decoded == payload)
        );
        let mut before = state.active_sequence().expect("Sequence").clone();
        state.dispatch_action(action).expect("Observed connect");
        let first_id =
            state.active_sequence().expect("Sequence").audio_program.sidechain_routes[0].id;
        let committed_revision = state.active_sequence().expect("Sequence").revision;
        assert!(state.undo_timeline().expect("Undo one connect"));
        let undone = state.active_sequence().expect("Sequence");
        assert_eq!(
            undone.revision,
            committed_revision.checked_next().expect("Revision")
        );
        // Undo restores every author field while publishing a new revision.
        before.revision = undone.revision;
        assert_eq!(undone, &before);
        assert!(!state.undo_timeline().expect("No second history item"));
        assert!(state.redo_timeline().expect("Redo"));
        assert_eq!(
            state.active_sequence().expect("Sequence").audio_program.sidechain_routes[0].id,
            first_id
        );
        let mut second = payload;
        second.source = mondrian_timeline::audio::AudioRouteSource::Track {
            track_id: state.active_sequence().expect("Sequence").audio_tracks[1].id,
            port: mondrian_timeline::audio::AudioChannelStripOutputPort::PreFader,
        };
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::ConnectProcessorInput(second))
                    .into_external_action(),
            )
            .expect("Second source same receipt");
        let routes = &state.active_sequence().expect("Sequence").audio_program.sidechain_routes;
        assert_eq!(routes.len(), 2);
        assert_ne!(routes[0].id, routes[1].id);
        assert_eq!(routes[0].bus_key, routes[1].bus_key);
        assert!(state.undo_timeline().expect("Undo second source"));
        assert_eq!(
            state.active_sequence().expect("Sequence").audio_program.sidechain_routes[0].id,
            first_id
        );
    }

    #[test]
    fn observed_input_connection_refuses_stale_unavailable_and_locked_intents_atomically() {
        for scenario in 0..14 {
            let (mut state, queries, calls, started, release) = observed_native_fixture();
            let mut payload = connection_payload(&state, queries[0]);
            match scenario {
                0 => {
                    state
                        .active_sequence_mut_uncommitted()
                        .expect("Sequence")
                        .audio_program
                        .outputs[0]
                        .strip
                        .pre_fader
                        .processors[0]
                        .opaque_state = Some(vec![31, 7].into())
                }
                1 => state.audio_sample_rate += 1,
                2 => state.audio_input_observations.invalidate(),
                3 => {
                    let sequence = state.active_sequence().expect("Sequence").clone();
                    state.test_set_sequence(None);
                    state.test_set_sequence(Some(sequence));
                }
                4 => {
                    payload.binding.observation_id =
                        mondrian_core::AudioProcessorInputObservationId::new()
                }
                5 => payload.bus_key = "missing.port".to_owned(),
                6 => {
                    payload.source = mondrian_timeline::audio::AudioRouteSource::Track {
                        track_id: mondrian_core::TrackId::new(),
                        port: mondrian_timeline::audio::AudioChannelStripOutputPort::PostMute,
                    }
                }
                7 => {
                    state.active_sequence_mut_uncommitted().expect("Sequence").audio_tracks[0]
                        .is_locked = true
                }
                8 | 9 => {
                    state.query_audio_processor_inputs(queries[0]).expect("Refresh");
                    started.recv_timeout(Duration::from_secs(2)).expect("Refresh entered");
                    if scenario == 9 {
                        release.send(()).expect("Refresh complete");
                        finish(&mut state);
                        assert_ne!(
                            connection_payload(&state, queries[0]).binding.observation_id,
                            payload.binding.observation_id
                        );
                    }
                }
                10 => {
                    state
                        .active_sequence_mut_uncommitted()
                        .expect("Sequence")
                        .audio_program
                        .outputs[0]
                        .strip
                        .pre_fader
                        .processors[0]
                        .definition = mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
                        plugin_id: "fixture.missing".to_owned(),
                        schema_version: 1,
                        binary_sha256: Some([7; 32]),
                    };
                    state.query_audio_processor_inputs(queries[0]).expect("Inspect unavailable");
                    started.recv_timeout(Duration::from_secs(2)).expect("Missing resolver entered");
                    release.send(()).expect("Failure complete");
                    finish(&mut state);
                    assert!(matches!(
                        state.audio_processor_input_observation(queries[0]),
                        AudioInputObservation::Failed(_)
                    ));
                }
                11 => state.audio_input_observations.cancel(&queries[0]),
                12 => payload.binding.sequence_id = mondrian_core::SequenceId::new(),
                13 => {
                    payload.binding.target = AudioProcessorInputInspectionTarget::ChannelStrip {
                        owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                            output_id: state
                                .active_sequence()
                                .expect("Sequence")
                                .audio_program
                                .outputs[0]
                                .id,
                        },
                        rack: mondrian_timeline::AudioChannelStripRack::PostFader,
                    }
                }
                _ => unreachable!("Closed scenario matrix"),
            }
            let before =
                serde_json::to_vec(state.active_sequence().expect("Sequence")).expect("Snapshot");
            let generation = state.project_author_generation();
            let calls_before = calls.load(Ordering::SeqCst);
            let action = ProductAction::Audio(AudioProductAction::ConnectProcessorInput(payload))
                .into_external_action();
            assert!(
                state.dispatch_action(action).is_err(),
                "scenario {scenario}"
            );
            assert_eq!(
                serde_json::to_vec(state.active_sequence().expect("Sequence")).expect("Snapshot"),
                before,
                "scenario {scenario}"
            );
            assert_eq!(
                state.project_author_generation(),
                generation,
                "scenario {scenario}"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                calls_before,
                "No implicit native probe"
            );
            assert!(!state.undo_timeline().expect("No rejected history item"));
            if scenario == 8 {
                release.send(()).expect("Pending refresh complete");
                finish(&mut state);
            }
        }
    }
    #[test]
    fn observed_input_connection_current_receipt_still_requires_author_cycle_admission() {
        let (mut state, mut queries, _, started, release) = fixture(1);
        let sequence = state.active_sequence_mut_uncommitted().expect("Sequence");
        let track_id = sequence.audio_tracks[0].id;
        let mut processor = sequence.audio_program.outputs[0].strip.pre_fader.processors.remove(0);
        processor.definition = mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
            plugin_id: "fixture.inputs".to_owned(),
            schema_version: 1,
            binary_sha256: Some([7; 32]),
        };
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("Track strip")
            .strip
            .pre_fader
            .processors
            .push(processor);
        queries[0].target = AudioProcessorInputInspectionTarget::ChannelStrip {
            owner: mondrian_timeline::AudioChannelStripOwner::Track { track_id },
            rack: mondrian_timeline::AudioChannelStripRack::PreFader,
        };
        state
            .query_audio_processor_inputs(queries[0])
            .expect("Inspect exact Track processor");
        started.recv_timeout(Duration::from_secs(2)).expect("Resolver entered");
        release.send(()).expect("Complete inspection");
        finish(&mut state);
        let payload = connection_payload(&state, queries[0]);
        let before =
            serde_json::to_vec(state.active_sequence().expect("Sequence")).expect("Snapshot");
        let generation = state.project_author_generation();
        assert!(
            state
                .dispatch_action(
                    ProductAction::Audio(AudioProductAction::ConnectProcessorInput(payload))
                        .into_external_action()
                )
                .is_err(),
            "A valid receipt cannot authorize self-feedback"
        );
        assert_eq!(
            serde_json::to_vec(state.active_sequence().expect("Sequence")).expect("Snapshot"),
            before
        );
        assert_eq!(state.project_author_generation(), generation);
        assert!(!state.undo_timeline().expect("No cycle history"));
    }
}
