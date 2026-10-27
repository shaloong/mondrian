//! Owned native DLL + canonical DAG controls; no third-party or RT grant.
use super::*;
use crate::{
    compile_audio_program, AudioCompileRequest, AudioContinuityEpoch, AudioPcmSource,
    AudioRenderSession, AudioStateEntry, PreparedAudioPlan,
};
use mondrian_core::{AssetId, AudioComponentEditId, AudioSourceComponentId, TimelineTime};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioProcessorSidechainRoute, AudioRouteSource,
};
use mondrian_timeline::{Clip, Sequence};

struct Source(AudioComponentEditId);
impl AudioPcmSource for Source {
    fn read_indexed_interleaved(
        &mut self,
        edit: AudioComponentEditId,
        positions: &[i64],
        layout: AudioChannelLayout,
        pcm: &mut [f32],
    ) -> Result<(), crate::AudioExecutionError> {
        for (frame, position) in pcm.chunks_exact_mut(layout.channel_count()).zip(positions) {
            for (channel, sample) in frame.iter_mut().enumerate() {
                let value = if edit == self.0 {
                    0.125 * (channel as f32 + 1.0)
                } else {
                    0.5 * (2 * channel + 1) as f32
                };
                *sample = if *position < 0 {
                    0.0
                } else {
                    value * (*position as f32 + 1.0)
                };
            }
        }
        Ok(())
    }
}

#[test]
#[ignore = "requires matching Product and rebuilt CLAP sidechain modes17/18/23/24 under native Job"]
fn installed_clap_mixed_inputs_preserve_native_state_layout_and_dag_pcm() {
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").unwrap());
    let plugin = PathBuf::from(std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN").unwrap());
    let catalog =
        Arc::new(DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin]).unwrap());
    assert_eq!(catalog.descriptors().len(), 1);
    for (port_mode, layout, input_layouts, output_weights) in [
        (
            17,
            AudioChannelLayout::Stereo,
            [AudioChannelLayout::Mono, AudioChannelLayout::Stereo],
            [2.875, 4.0],
        ),
        (
            18,
            AudioChannelLayout::Mono,
            [AudioChannelLayout::Stereo, AudioChannelLayout::Mono],
            [2.125, 0.0],
        ),
        // Same native planes and independent oracle, now arbitrary NULL/empty
        // declarations on every Main/Aux port. Opaque mode bytes must survive.
        (
            23,
            AudioChannelLayout::Stereo,
            [AudioChannelLayout::Mono, AudioChannelLayout::Stereo],
            [2.875, 4.0],
        ),
        (
            24,
            AudioChannelLayout::Mono,
            [AudioChannelLayout::Stereo, AudioChannelLayout::Mono],
            [2.125, 0.0],
        ),
    ] {
        for rate in [44_100, 48_000] {
            for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
                let render = AudioRenderContract {
                    sample_rate: rate,
                    channel_layout: layout,
                    max_block_frames: 4,
                    processing_mode: mode,
                    processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
                    public_output_lookahead_budget_frames: 4096,
                    compensation_delay_scratch_budget_bytes: 1024 * 1024,
                };
                let mut state = vec![port_mode];
                state.extend(0.5_f32.to_le_bytes());
                let instance = catalog
                    .create_instance(
                        "org.mondrian.fixture.sidechain",
                        render,
                        Some(state.clone()),
                    )
                    .expect("restore/capture actual mixed native state");
                assert!(instance.parameters.is_empty());
                assert_eq!(
                    instance.opaque_state.as_ref().map(AsRef::as_ref),
                    Some(state.as_slice())
                );
                let spec = catalog
                    .resolve(AudioProcessorPrepareRequest::new(
                        AudioProcessorOccurrence {
                            instance_id: instance.id,
                            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                            insertion: AudioProcessorInsertionPoint::PreFader,
                        },
                        &instance.definition,
                        &instance.parameters,
                        instance.opaque_state.as_ref().map(AsRef::as_ref),
                        render,
                    ))
                    .unwrap();
                assert_eq!(
                    spec.auxiliary_inputs
                        .buses
                        .iter()
                        .map(|bus| (bus.bus_key.as_str(), bus.channel_layout))
                        .collect::<Vec<_>>(),
                    vec![
                        ("clap.audio-input.11", input_layouts[0]),
                        ("clap.audio-input.7", input_layouts[1])
                    ]
                );

                let mut sequence = Sequence::new("native CLAP mixed sidechain");
                sequence.settings.audio_channel_layout = layout;
                let main = sequence.audio_tracks[0].id;
                let detector = sequence.add_audio_track();
                for track in [main, detector] {
                    sequence
                        .add_media_audio_clip(
                            track,
                            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE)
                                .unwrap(),
                            AudioSourceComponentId::primary(),
                        )
                        .unwrap();
                }
                let main_edit = sequence.audio_tracks.iter().find(|t| t.id == main).unwrap().clips
                    [0]
                .audio_components[0]
                    .id;
                let processor_id = instance.id;
                sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
                sequence.audio_program.routes.retain(|route|
                    matches!(route.source, AudioRouteSource::Track { track_id, .. } if track_id == main));
                for key in ["clap.audio-input.11", "clap.audio-input.7"] {
                    sequence.audio_program.sidechain_routes.push(
                        AudioProcessorSidechainRoute::new(
                            AudioRouteSource::Track {
                                track_id: detector,
                                port: AudioChannelStripOutputPort::PostMute,
                            },
                            processor_id,
                            key,
                        ),
                    );
                }
                let before = serde_json::to_value(&sequence).unwrap();
                let program = Arc::new(
                    compile_audio_program(
                        &sequence,
                        AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
                    )
                    .unwrap(),
                );
                let resolver = IsolatedAudioProcessorResolver::new(catalog.clone());
                let plan = Arc::new(
                    PreparedAudioPlan::prepare_with_processor_resolver(
                        program,
                        render,
                        AudioKernelBackend::ScalarReference,
                        &resolver,
                    )
                    .unwrap(),
                );
                assert_eq!(plan.public_output_lookahead_frames(), 3);
                let mut session = AudioRenderSession::new(plan).unwrap();
                for (epoch, start, parts) in [
                    (1, 0, vec![4, 4, 4]),
                    (2, 0, vec![1, 3, 2, 4, 2]),
                    (3, 5, vec![3, 1, 4, 4]),
                ] {
                    session
                        .enter_state(AudioStateEntry {
                            epoch: AudioContinuityEpoch::new(epoch),
                            start_sample: start,
                        })
                        .unwrap();
                    let mut source = Source(main_edit);
                    let mut position = start;
                    for frames in parts {
                        let mut pcm = vec![f32::NAN; frames * layout.channel_count()];
                        session
                            .render_into(
                                &mut source,
                                AudioRenderRequest {
                                    transport_activity: None,
                                    start_sample: position,
                                    frames,
                                },
                                &mut pcm,
                            )
                            .unwrap();
                        for (offset, frame) in pcm.chunks_exact(layout.channel_count()).enumerate()
                        {
                            for (channel, sample) in frame.iter().enumerate() {
                                let expected =
                                    output_weights[channel] * (position + offset as i64 + 1) as f32;
                                assert_eq!(
                                    *sample, expected,
                                    "mode{port_mode} {rate} {mode:?} epoch{epoch}"
                                );
                            }
                        }
                        position += frames as i64;
                    }
                }
                assert_eq!(serde_json::to_value(&sequence).unwrap(), before);
            }
        }
    }
}
