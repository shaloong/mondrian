//! Existing Session/processor APIs only; pending callback disappearance is not installation.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct OncePendingResolver {
    pending: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
    observed: crate::NativeObservedRequest,
}
struct OncePendingFactory {
    pending: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
    observed: crate::NativeObservedRequest,
}
struct OncePendingProcessor {
    pending: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
    observed: crate::NativeObservedRequest,
}
impl AudioProcessorResolver for OncePendingResolver {
    fn prepare(
        &self,
        _request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        Ok(Arc::new(OncePendingFactory {
            pending: Arc::clone(&self.pending),
            calls: Arc::clone(&self.calls),
            queries: Arc::clone(&self.queries),
            observed: self.observed,
        }))
    }
}
impl AudioProcessorFactory for OncePendingFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, false, true, true, 0)
            .expect("actual stateless test processor")
    }
    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        Ok(Box::new(OncePendingProcessor {
            pending: Arc::clone(&self.pending),
            calls: Arc::clone(&self.calls),
            queries: Arc::clone(&self.queries),
            observed: self.observed,
        }))
    }
}
impl AudioProcessor for OncePendingProcessor {
    fn enter_state(&mut self, _start_sample: i64) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }
    fn preflight_reprepare(
        &mut self,
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(self.pending.swap(false, Ordering::SeqCst).then_some(self.observed))
    }
    fn process(
        &mut self,
        _context: AudioProcessorProcessContext,
        _audio: &mut dyn AudioProcessorAudioIo,
        _parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn fixture() -> (
    AudioRenderSession,
    Arc<PreparedAudioPlan>,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    fixture_with_request(crate::NativeObservedRequest::RequestOnly(
        mondrian_core::NativeAudioReprepareRequest::ClapRestart,
    ))
}

fn fixture_with_request(
    observed: crate::NativeObservedRequest,
) -> (
    AudioRenderSession,
    Arc<PreparedAudioPlan>,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let sequence = sequence_with_parallel_hosted_delay();
    let compiled = Arc::new(
        compile_audio_program(
            &sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
        )
        .expect("sole actual compiler"),
    );
    let pending = Arc::new(AtomicBool::new(true));
    let calls = Arc::new(AtomicUsize::new(0));
    let queries = Arc::new(AtomicUsize::new(0));
    let resolver = OncePendingResolver {
        pending: Arc::clone(&pending),
        calls: Arc::clone(&calls),
        queries: Arc::clone(&queries),
        observed,
    };
    let contract = prepared(&sequence_with_audio_clip(), 16).contract();
    let plan = Arc::new(
        PreparedAudioPlan::prepare_with_processor_resolver(
            compiled,
            contract,
            AudioKernelBackend::ScalarReference,
            &resolver,
        )
        .expect("actual prepared plan"),
    );
    let session = AudioRenderSession::new(Arc::clone(&plan)).expect("actual exclusive Session");
    (session, plan, pending, calls, queries)
}

#[test]
fn native_pending_ticket_old_session_some_then_none_keeps_zero_source_and_dsp() {
    let (mut session, _plan, pending, calls, queries) = fixture();
    let mut source = RampSource::default();
    let request = AudioRenderRequest { start_sample: 0, frames: 8 };
    let mut output = vec![73.0; 8];
    let first = match session.render_into(&mut source, request, &mut output) {
        Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
        other => panic!("expected exact initial root refusal, got {other:?}"),
    };
    assert!(
        !pending.load(Ordering::SeqCst),
        "the observed callback snapshot has genuinely become empty"
    );
    assert_eq!(queries.load(Ordering::SeqCst), 1);
    assert_eq!(source.block_reads, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(output, vec![73.0; 8]);
    let next = session.render_into(&mut source, request, &mut output);
    assert!(
        matches!(&next,Err(AudioExecutionError::ReprepareRequired(ticket)) if *ticket==first),
        "an empty later callback is not a new graph/PDC installation: {next:?}"
    );
    assert_eq!(session.pending_reprepare_ticket(), Some(first));
    assert_eq!(source.block_reads, 0, "root source admission stays at zero");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "old prepared contract must not reach DSP"
    );
    assert_eq!(
        output,
        vec![73.0; 8],
        "no silence/partial/old-contract PCM may replace typed refusal"
    );
}

#[test]
fn native_pending_ticket_old_session_fresh_enter_cannot_clear_uninstalled_configuration() {
    let (mut session, _plan, _pending, calls, _queries) = fixture();
    let mut source = RampSource::default();
    let request = AudioRenderRequest { start_sample: 0, frames: 8 };
    let mut output = vec![91.0; 8];
    let ticket = match session.render_into(&mut source, request, &mut output) {
        Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
        other => panic!("initial refusal: {other:?}"),
    };
    let entered = session.enter_state(AudioStateEntry {
        epoch: AudioContinuityEpoch::new(1),
        start_sample: 0,
    });
    assert!(
        matches!(&entered,Err(AudioExecutionError::ReprepareRequired(current)) if *current==ticket),
        "fresh continuity alone cannot install changed execution contracts: {entered:?}"
    );
    assert_eq!(source.block_reads, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(session.pending_reprepare_ticket(), Some(ticket));
}

#[test]
fn native_pending_ticket_actual_new_session_replacement_starts_clear_and_has_new_identity() {
    let (mut current, plan, pending, calls, _queries) = fixture();
    let mut source = RampSource::default();
    let request = AudioRenderRequest { start_sample: 0, frames: 8 };
    let mut output = vec![11.0; 8];
    let old_ticket = match current.render_into(&mut source, request, &mut output) {
        Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
        other => panic!("initial refusal: {other:?}"),
    };
    assert!(!pending.load(Ordering::SeqCst));
    let replacement =
        AudioRenderSession::new(plan).expect("actual independently allocated exclusive Session");
    assert!(replacement.pending_reprepare_ticket().is_none());
    let retired = std::mem::replace(&mut current, replacement);
    assert_eq!(
        retired.pending_reprepare_ticket(),
        Some(old_ticket),
        "retired continuity is never relabeled or reset"
    );
    current
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .expect("explicit fresh entry on new Session");
    current
        .render_into(&mut source, request, &mut output)
        .expect("actual new Session accepts its unchanged declared contract");
    assert!(source.block_reads > 0);
    assert!(calls.load(Ordering::SeqCst) > 0);
    pending.store(true, Ordering::SeqCst);
    let next = match current.render_into(&mut source, request, &mut output) {
        Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
        other => panic!("new exact Session pending: {other:?}"),
    };
    assert_ne!(
        next.session_id, old_ticket.session_id,
        "same immutable Plan is not the old exclusive Session"
    );
    assert_eq!(retired.pending_reprepare_ticket(), Some(old_ticket));
}

struct CancelCountingMediaResolver(Arc<AtomicUsize>);
struct CancelCountingDecodedSource(Arc<AtomicUsize>);
impl AudioMediaResolver for CancelCountingMediaResolver {
    fn resolve(
        &self,
        _asset: AssetId,
        _component: AudioSourceComponentId,
        _rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        Ok(ResolvedAudioSource::new(
            AudioChannelLayout::Mono,
            Arc::new(CancelCountingDecodedSource(Arc::clone(&self.0))),
        ))
    }
}
impl AudioDecodedSource for CancelCountingDecodedSource {
    fn read_interleaved(
        &self,
        _start: i64,
        _frames: usize,
        destination: &mut [f32],
        _cancel: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        destination.fill(1.0);
        Ok(())
    }
}

fn assert_public_canceled_preflight(nested: bool, has_pending: bool) {
    let child = sequence_with_parallel_hosted_delay();
    let mut root = if nested {
        Sequence::new("canceled nested root")
    } else {
        child.clone()
    };
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    if nested {
        let clip = Clip::new_nested_sequence(child.id, TimelineTime::ZERO, tt(4, 1), None)
            .expect("canonical nested clip");
        root.add_nested_audio_clip(
            root.audio_tracks[0].id,
            clip,
            child.audio_program.outputs[0].id,
        )
        .expect("canonical nested audio authoring");
    }
    let children = if nested { vec![child] } else { Vec::new() };
    let pending = Arc::new(AtomicBool::new(has_pending));
    let calls = Arc::new(AtomicUsize::new(0));
    let queries = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let processors = OncePendingResolver {
        pending: Arc::clone(&pending),
        calls: Arc::clone(&calls),
        queries: Arc::clone(&queries),
    };
    let contract = prepared(&sequence_with_audio_clip(), 16).contract();
    let mut runtime =
        AudioProgramRuntime::build_with_compile_request_processor_resolver_and_resource_grant(
            &root,
            &children,
            &CancelCountingMediaResolver(Arc::clone(&reads)),
            &processors,
            contract,
            AudioCompileRequest::program(root.audio_program.outputs[0].id),
            AudioRuntimeResourceGrant::new(64, 512 * 1024 * 1024, 128 * 1024 * 1024),
        )
        .expect("actual recursively prepared Runtime");
    let cancel = ExecutionCancellationToken::new();
    cancel.cancel();
    let mut pcm = [73.0; 8];
    let result = runtime.render_into_cancellable(
        AudioRenderRequest { start_sample: 0, frames: 8 },
        &mut pcm,
        &cancel,
    );
    assert!(
        matches!(&result,Err(AudioExecutionError::SourceUnavailable(detail)) if detail.contains("canceled")),
        "a pre-canceled request must retain cancellation priority: {result:?}"
    );
    assert_eq!(
        queries.load(Ordering::SeqCst),
        0,
        "no foreign root or nested pending query"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no DSP advance");
    assert_eq!(reads.load(Ordering::SeqCst), 0, "no decoded source read");
    assert_eq!(
        pending.load(Ordering::SeqCst),
        has_pending,
        "cancellation does not consume callback facts"
    );
    assert_eq!(pcm, [73.0; 8], "caller PCM remains untouched");
}
#[test]
fn native_preflight_canceled_direct_with_pending() {
    assert_public_canceled_preflight(false, true);
}
#[test]
fn native_preflight_canceled_direct_without_pending() {
    assert_public_canceled_preflight(false, false);
}
#[test]
fn native_preflight_canceled_nested_with_pending() {
    assert_public_canceled_preflight(true, true);
}
#[test]
fn native_preflight_canceled_nested_without_pending() {
    assert_public_canceled_preflight(true, false);
}

#[test]
fn native_pending_ticket_preserves_first_cohort_without_querying_a_later_identity() {
    let observed = crate::NativeObservedRequest::Cohort(
        crate::NativeRequestCohort::new(
            37,
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 },
        )
        .unwrap(),
    );
    let (mut session, _plan, _pending, calls, queries) = fixture_with_request(observed);
    let mut source = RampSource::default();
    let request = AudioRenderRequest { start_sample: 0, frames: 8 };
    let mut output = vec![61.0; 8];
    let first = match session.render_into(&mut source, request, &mut output) {
        Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
        other => panic!("expected exact observed request ticket: {other:?}"),
    };
    assert_eq!(first.request, observed);
    assert_eq!(
        first.product_notice().request,
        observed.request(),
        "notice is only a raw diagnostic projection"
    );
    assert!(
        matches!(session.render_into(&mut source, request, &mut output), Err(AudioExecutionError::ReprepareRequired(ticket)) if ticket == first)
    );
    assert_eq!(
        queries.load(Ordering::SeqCst),
        1,
        "stored ticket cannot acquire a serial from a later snapshot"
    );
    assert_eq!(source.block_reads, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(output, vec![61.0; 8]);
}
