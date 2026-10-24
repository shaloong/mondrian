//! Actual Runtime boundaries for the unchanged prepared logical policy.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn contract() -> AudioRenderContract {
    AudioRenderContract {
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
    }
}

#[derive(Default)]
struct CountingProcessorResolver {
    calls: AtomicUsize,
}

impl AudioProcessorResolver for CountingProcessorResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        TestDelayProcessorResolver::default().prepare(request)
    }
}

#[test]
fn root_logical_refusal_precedes_media_and_factory_but_exact_grant_keeps_output() {
    let sequence = sequence_with_parallel_hosted_delay();
    let contract = contract();
    let baseline = AudioProgramRuntime::build_with_processor_resolver(
        &sequence,
        &[],
        &RampResolver,
        &TestDelayProcessorResolver::default(),
        contract,
        None,
    )
    .unwrap();
    let footprint = baseline.resource_footprint();
    let program = compile_audio_program(
        &sequence,
        AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
    )
    .unwrap();
    let requested = PreparedAudioPlan::requested_base_logical_bytes(&program).unwrap();
    assert_eq!(requested, footprint.prepared_logical_bytes);
    let media = RecordingResolver::default();
    let processors = CountingProcessorResolver::default();
    let refused = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &media,
        &processors,
        contract,
        None,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, requested - 1),
    );
    assert!(matches!(
        refused,
        Err(AudioRuntimeBuildError::ResourceGrantExceeded {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
            required,
            granted,
            ..
        }) if required == requested && granted == requested - 1
    ));
    assert!(media.resolved_assets.lock().unwrap().is_empty());
    assert_eq!(processors.calls.load(Ordering::SeqCst), 0);

    let mut exact = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &RampResolver,
        &TestDelayProcessorResolver::default(),
        contract,
        None,
        AudioRuntimeResourceGrant::new(
            footprint.runtime_occurrences,
            footprint.fixed_resident_bytes,
            footprint.prepared_logical_bytes,
        ),
    )
    .unwrap();
    assert_eq!(exact.resource_footprint(), footprint);
    let mut expected = baseline;
    let mut actual_pcm = [0.0; 4];
    let mut expected_pcm = [0.0; 4];
    let request = AudioRenderRequest { transport_activity: None, start_sample: 0, frames: 4 };
    expected.render_into(request, &mut expected_pcm).unwrap();
    exact.render_into(request, &mut actual_pcm).unwrap();
    assert_eq!(actual_pcm, expected_pcm);
    assert!(actual_pcm.iter().any(|sample| *sample != 0.0));
}

#[test]
fn child_admission_includes_the_pending_parent_before_descendant_media() {
    let child = sequence_with_audio_clip();
    let child_output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("prospective parent charge");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track = root.audio_tracks[0].id;
    root.add_nested_audio_clip(
        track,
        Clip::new_nested_sequence(child.id, TimelineTime::ZERO, tt(4, 1), None).unwrap(),
        child_output,
    )
    .unwrap();
    let root_program = compile_audio_program(
        &root,
        AudioCompileRequest::program(root.audio_program.outputs[0].id),
    )
    .unwrap();
    let child_program =
        compile_audio_program(&child, AudioCompileRequest::program(child_output)).unwrap();
    let parent_bytes = PreparedAudioPlan::requested_base_logical_bytes(&root_program).unwrap();
    let child_bytes = PreparedAudioPlan::requested_base_logical_bytes(&child_program).unwrap();
    let ceiling = parent_bytes + child_bytes - 1;
    assert!(ceiling >= parent_bytes && ceiling >= child_bytes);
    let resolver = RecordingResolver::default();
    let refused = AudioProgramRuntime::build_with_resource_grant(
        &root,
        std::slice::from_ref(&child),
        &resolver,
        contract(),
        None,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, ceiling),
    );
    assert!(matches!(
        refused,
        Err(AudioRuntimeBuildError::ResourceGrantExceeded {
            category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
            sequence_id,
            required,
            granted,
            ..
        }) if sequence_id == child.id && required == parent_bytes + child_bytes && granted == ceiling
    ));
    assert!(resolver.resolved_assets.lock().unwrap().is_empty());
}
