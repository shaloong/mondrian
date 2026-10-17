//! Existing-production-API behavioral oracle. No new collector API is required.

use super::*;
use crate::{AudioProcessingMode, AudioProcessorTail, AudioRenderContract};
use clack_plugin::prelude::SinglePluginEntry;
use mondrian_core::AudioChannelLayout;

#[path = "../../tests/fixtures/clap-output-probe/src/plugin.rs"]
mod owned_fixture;

fn render(mode: AudioProcessingMode) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: mode,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}

fn assert_callback_refusal(mode: AudioProcessingMode) {
    let entry =
        PluginEntry::load_from_clack::<SinglePluginEntry<owned_fixture::OutputProbePlugin>>(
            c"/test/owned-output-probe",
        )
        .expect("owned CLAP entry");
    let render = render(mode);
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
        musical_time_reference: None,
        auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
        note_input_contract: crate::AudioProcessorNoteInputContract::default(),
        native_lane: None,
        payload: Vec::new(),
        plugin_execution_contract: AudioProcessorExecutionContract::new(
            0,
            AudioProcessorTail::None,
            true,
            true,
            true,
            0,
        )
        .expect("native probe contract"),
        auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
        parameter_ids: Vec::new(),
        render_contract: render,
    };
    let mut worker = prepare_loaded(
        request,
        entry,
        c"org.mondrian.fixture.output-probe",
        None,
        &[],
        &[],
    )
    .expect("actual production native worker");
    worker.enter_state(-3).expect("fresh native epoch");
    let auxiliary = AudioProcessorAuxiliaryInputContract::default();
    for origin in [-3, 1] {
        let mut samples = [0.25_f32; 8];
        worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                origin,
                4,
                render,
                &mut samples,
                &auxiliary,
                &[],
                &[],
                &[],
            ))
            .expect("actual native process callback");
        assert_eq!(samples, [0.0; 8],
            "native callback must return false for declared note/MIDI outputs with no Core consumer; true-and-discard is false support");
    }
}

#[test]
fn clap_output_owning_old_api_realtime_does_not_accept_and_discard_unsupported_events() {
    assert_callback_refusal(AudioProcessingMode::Realtime);
}
#[test]
fn clap_output_owning_old_api_offline_does_not_accept_and_discard_unsupported_events() {
    assert_callback_refusal(AudioProcessingMode::Offline);
}

#[test]
#[ignore = "fresh matching product executable and owned output-probe DLL required"]
fn clap_output_owning_actual_isolated_child_does_not_accept_and_discard_unsupported_events() {
    use crate::{
        AudioKernelBackend, AudioParameterEventBatch, AudioProcessorAudioIo,
        AudioProcessorInputBus, AudioProcessorMainAndInputBuses, AudioProcessorProcessContext,
        AudioRenderRequest,
    };
    use mondrian_core::AudioSampleRate;
    struct Io([f32; 8]);
    impl AudioProcessorAudioIo for Io {
        fn main_layout(&self) -> AudioChannelLayout {
            AudioChannelLayout::Stereo
        }
        fn frames(&self) -> usize {
            4
        }
        fn main_interleaved(&mut self) -> &mut [f32] {
            &mut self.0
        }
        fn auxiliary_input(&self, _key: &str) -> Option<AudioProcessorInputBus<'_>> {
            None
        }
        fn main_and_auxiliary_input(
            &mut self,
            _key: &str,
        ) -> Option<AudioProcessorMainAndInputBuses<'_>> {
            None
        }
    }
    let helper = std::env::var_os("MONDRIAN_CLAP_OUTPUT_HELPER")
        .map(PathBuf::from)
        .expect("real product helper");
    let binary = std::env::var_os("MONDRIAN_CLAP_OUTPUT_FIXTURE")
        .map(PathBuf::from)
        .expect("fresh owned fixture");
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let render = render(mode);
        let registration = probe_clap_plugin_registration(
            &helper,
            &binary,
            "org.mondrian.fixture.output-probe",
            render,
            None,
        )
        .expect("fresh actual plugin probe");
        let payload = serde_json::to_vec(&ClapPayload {
            main_audio_output_id: registration.main_audio_output_id,
            auxiliary_outputs: super::super::protocol::outputs_to_wire(
                &registration.auxiliary_outputs,
            ),
            main_audio_input: ClapMainAudioInput::Present,
            library_path: registration.library_path,
            plugin_id: registration.plugin_id,
            binary_sha256: registration.binary_sha256,
            state: None,
            parameters: registration.parameters,
            parameter_ids: Vec::new(),
        })
        .expect("actual codec");
        let spec = super::super::IsolatedAudioProcessorWorkerSpec::new(
            helper.clone(),
            payload,
            registration.execution_contract,
            AudioProcessorAuxiliaryInputContract::default(),
            Vec::new(),
            render,
        )
        .expect("actual isolated spec");
        let factory = super::super::IsolatedAudioProcessorFactory::prepare(spec)
            .expect("actual validation child");
        let mut processor =
            crate::AudioProcessorFactory::create(factory.as_ref()).expect("sole actual DSP child");
        processor.enter_state(-3).expect("exact native continuity");
        for origin in [-3, 1] {
            let context = AudioProcessorProcessContext::new(
                AudioRenderRequest { start_sample: origin, frames: 4 },
                AudioSampleRate::new(48_000).expect("rate"),
                AudioChannelLayout::Stereo,
                mode,
                AudioKernelBackend::ScalarReference,
            );
            let mut io = Io([0.25; 8]);
            processor
                .process(
                    context,
                    &mut io,
                    AudioParameterEventBatch::new(origin, 4, &[], &[], &[]),
                )
                .expect("real shared-buffer native process");
            assert_eq!(
                io.0, [0.0; 8],
                "real child's native output callback must not report discarded events as accepted"
            );
        }
    }
}
