//! Solve signal arrival on the sole prepared causal stage graph.

use crate::schedule::{
    PreparedContribution, PreparedNode, PreparedNodeOrigin, PreparedProcessor,
    PreparedProcessorAuxiliaryBus, PreparedRack, PreparedRoute, PreparedSidechainBinding,
    PreparedSidechainSource,
};
use crate::AudioProcessorTail;
use mondrian_timeline::audio::{AudioChannelSignalStage, AudioChannelStripOutputPort};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreparedNodeLatency {
    pub(crate) input_frames: usize,
    pub(crate) pre_fader_frames: usize,
    pub(crate) post_fader_frames: usize,
}
impl PreparedNodeLatency {
    pub(crate) fn port(self, port: AudioChannelStripOutputPort) -> usize {
        match port {
            AudioChannelStripOutputPort::PreFader => self.pre_fader_frames,
            AudioChannelStripOutputPort::PostFaderPreMute
            | AudioChannelStripOutputPort::PostMute => self.post_fader_frames,
        }
    }
}

pub(crate) struct PreparedLatencyGraph<'a> {
    pub(crate) nodes: &'a mut [PreparedNode],
    pub(crate) routes: &'a [PreparedRoute],
    pub(crate) contributions: &'a mut [PreparedContribution],
    pub(crate) processors: &'a mut [PreparedProcessor],
    pub(crate) auxiliary_buses: &'a [PreparedProcessorAuxiliaryBus],
    pub(crate) bindings: &'a mut [PreparedSidechainBinding],
    pub(crate) sources: &'a [PreparedSidechainSource],
    pub(crate) execution_order: &'a [(usize, AudioChannelSignalStage)],
    pub(crate) output_slot: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedLatencyPlan {
    pub(crate) contribution_compensation_frames: Vec<usize>,
    pub(crate) route_compensation_frames: Vec<usize>,
    pub(crate) node_latencies: Vec<PreparedNodeLatency>,
    pub(crate) output_algorithmic_latency_frames: usize,
    pub(crate) maximum_compensation_frames: usize,
}

pub(crate) fn solve_prepared_latency(
    graph: PreparedLatencyGraph<'_>,
) -> Result<PreparedLatencyPlan, &'static str> {
    let mut node_latencies = vec![PreparedNodeLatency::default(); graph.nodes.len()];
    let mut published = vec![[false; 2]; graph.nodes.len()];
    let mut contribution_compensation_frames = vec![0; graph.contributions.len()];
    let mut route_compensation_frames = vec![0; graph.routes.len()];
    let mut maximum_compensation_frames = 0;
    for &(slot, stage) in graph.execution_order {
        let node = graph.nodes.get_mut(slot).ok_or("latency stage node is unavailable")?;
        let stage_index = usize::from(stage == AudioChannelSignalStage::PostFader);
        if published[slot][stage_index] || (stage_index == 1 && !published[slot][0]) {
            return Err("latency stages are duplicated or non-causal");
        }
        let input = if stage_index == 0 {
            match node.origin {
                PreparedNodeOrigin::Track(_) => {
                    let mut arrivals = Vec::with_capacity(node.contributions.len());
                    for contribution in graph
                        .contributions
                        .get_mut(node.contributions.clone())
                        .ok_or("latency contribution range is invalid")?
                    {
                        let arrival = solve_rack(
                            &mut contribution.scope_rack,
                            contribution.source_algorithmic_latency_frames,
                            graph.processors,
                            graph.auxiliary_buses,
                            graph.bindings,
                            graph.sources,
                            &node_latencies,
                            &published,
                        )?;
                        let extension = match contribution.scope_rack.tail {
                            AudioProcessorTail::None => Some(arrival),
                            AudioProcessorTail::Finite(frames) => Some(
                                arrival
                                    .checked_add(frames)
                                    .ok_or("Scope execution extent overflowed")?,
                            ),
                            AudioProcessorTail::Infinite => None,
                        };
                        contribution.execution_end_sample = extension
                            .map(|frames| {
                                let frames = i64::try_from(frames)
                                    .map_err(|_| "Scope extent exceeds the sample grid")?;
                                contribution
                                    .sequence_end_sample
                                    .checked_add(frames)
                                    .ok_or("Scope execution end overflowed")
                            })
                            .transpose()?;
                        arrivals.push(arrival);
                    }
                    let maximum = arrivals.iter().copied().max().unwrap_or(0);
                    for (index, arrival) in node.contributions.clone().zip(arrivals) {
                        contribution_compensation_frames[index] = maximum - arrival;
                    }
                    maximum
                }
                PreparedNodeOrigin::Bus(_) | PreparedNodeOrigin::Output(_) => {
                    let incoming = graph
                        .routes
                        .get(node.incoming.clone())
                        .ok_or("latency route range is invalid")?;
                    let mut maximum = 0;
                    for route in incoming {
                        if route.destination_slot != slot {
                            return Err("latency route destination is invalid");
                        }
                        maximum = maximum.max(source_arrival(
                            PreparedSidechainSource {
                                node_slot: route.source_slot,
                                port: route.source_port,
                                processor_output: route.processor_output,
                            },
                            &node_latencies,
                            &published,
                            graph.processors,
                        )?);
                    }
                    for (index, route) in node.incoming.clone().zip(incoming) {
                        route_compensation_frames[index] = maximum
                            - source_arrival(
                                PreparedSidechainSource {
                                    node_slot: route.source_slot,
                                    port: route.source_port,
                                    processor_output: route.processor_output,
                                },
                                &node_latencies,
                                &published,
                                graph.processors,
                            )?;
                    }
                    maximum
                }
            }
        } else {
            node_latencies[slot].pre_fader_frames
        };
        let output = solve_rack(
            if stage_index == 0 {
                &mut node.pre_rack
            } else {
                &mut node.post_rack
            },
            input,
            graph.processors,
            graph.auxiliary_buses,
            graph.bindings,
            graph.sources,
            &node_latencies,
            &published,
        )?;
        if stage_index == 0 {
            node_latencies[slot].input_frames = input;
            node_latencies[slot].pre_fader_frames = output;
        } else {
            node_latencies[slot].post_fader_frames = output;
        }
        published[slot][stage_index] = true;
    }
    if published.iter().any(|stages| *stages != [true, true]) {
        return Err("latency graph omits channel stages");
    }
    for frames in contribution_compensation_frames
        .iter()
        .chain(&route_compensation_frames)
        .chain(
            graph
                .processors
                .iter()
                .map(|processor| &processor.main_compensation_delay_frames),
        )
        .chain(graph.bindings.iter().map(|binding| &binding.compensation_delay_frames))
    {
        maximum_compensation_frames = maximum_compensation_frames.max(*frames);
    }
    let output_algorithmic_latency_frames = node_latencies
        .get(graph.output_slot)
        .ok_or("latency output is unavailable")?
        .post_fader_frames;
    Ok(PreparedLatencyPlan {
        contribution_compensation_frames,
        route_compensation_frames,
        node_latencies,
        output_algorithmic_latency_frames,
        maximum_compensation_frames,
    })
}

fn source_arrival(
    source: PreparedSidechainSource,
    latencies: &[PreparedNodeLatency],
    published: &[[bool; 2]],
    processors: &[PreparedProcessor],
) -> Result<usize, &'static str> {
    let stage = usize::from(
        AudioChannelSignalStage::producing(source.port) == AudioChannelSignalStage::PostFader,
    );
    if !published.get(source.node_slot).ok_or("latency source is unavailable")?[stage] {
        return Err("latency source stage has not been produced");
    }
    if let Some(output) = source.processor_output {
        let processor =
            processors.get(output.processor_slot).ok_or("output processor is unavailable")?;
        return processor
            .input_signal_delay_frames
            .checked_add(processor.factory.contract().algorithmic_latency_frames())
            .ok_or("processor output latency overflowed");
    }
    Ok(latencies
        .get(source.node_slot)
        .ok_or("latency source is unavailable")?
        .port(source.port))
}

#[allow(clippy::too_many_arguments)]
fn solve_rack(
    rack: &mut PreparedRack,
    input: usize,
    processors: &mut [PreparedProcessor],
    auxiliary_buses: &[PreparedProcessorAuxiliaryBus],
    bindings: &mut [PreparedSidechainBinding],
    sources: &[PreparedSidechainSource],
    latencies: &[PreparedNodeLatency],
    published: &[[bool; 2]],
) -> Result<usize, &'static str> {
    let mut arrival = input;
    if rack.processors.end > processors.len() {
        return Err("latency processor range is invalid");
    }
    for processor_index in rack.processors.clone() {
        let processor = &processors[processor_index];
        let buses = auxiliary_buses
            .get(processor.auxiliary_buses.clone())
            .ok_or("latency auxiliary range is invalid")?;
        let mut aligned = arrival;
        for bus in buses {
            for binding in
                bindings.get(bus.bindings.clone()).ok_or("latency binding range is invalid")?
            {
                aligned = aligned.max(source_arrival(
                    *sources
                        .get(binding.source_buffer_slot)
                        .ok_or("latency source buffer is unavailable")?,
                    latencies,
                    published,
                    processors,
                )?);
            }
        }
        let latency = processor.factory.contract().algorithmic_latency_frames();
        for bus in buses {
            for binding in bindings
                .get_mut(bus.bindings.clone())
                .ok_or("latency binding range is invalid")?
            {
                binding.compensation_delay_frames = aligned
                    - source_arrival(
                        *sources
                            .get(binding.source_buffer_slot)
                            .ok_or("latency source buffer is unavailable")?,
                        latencies,
                        published,
                        processors,
                    )?;
            }
        }
        let processor = &mut processors[processor_index];
        processor.main_compensation_delay_frames = aligned - arrival;
        processor.input_signal_delay_frames = aligned;
        arrival = aligned.checked_add(latency).ok_or("processor signal latency overflowed")?;
    }
    rack.algorithmic_latency_frames = arrival - input;
    Ok(arrival)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compile_audio_program, AudioCompileRequest, AudioKernelBackend, AudioProcessingMode,
        AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorHostError,
        AudioProcessorPrepareRequest, AudioProcessorResolver, AudioRenderContract,
        PreparedAudioPlan,
    };
    use mondrian_core::{
        AssetId, AudioChannelLayout, AudioProcessorInstanceId, AudioSourceComponentId, MixBusId,
        TimelineTime,
    };
    use mondrian_timeline::audio::{
        AudioProcessorDefinitionRef, AudioProcessorInstance, AudioRoute, AudioRouteDestination,
        AudioRouteSource,
    };
    use mondrian_timeline::{AudioMixBus, Clip, Sequence};
    use std::sync::Arc;

    struct Resolver;
    struct Factory(usize);
    impl AudioProcessorResolver for Resolver {
        fn prepare(
            &self,
            request: AudioProcessorPrepareRequest<'_>,
        ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
            let AudioProcessorDefinitionRef::Clap { plugin_id, .. } = request.definition() else {
                panic!("fixture native metadata");
            };
            Ok(Arc::new(Factory(plugin_id.parse().expect("latency tag"))))
        }
    }
    impl AudioProcessorFactory for Factory {
        fn execution_contract(&self) -> AudioProcessorExecutionContract {
            AudioProcessorExecutionContract::new(
                self.0,
                AudioProcessorTail::None,
                self.0 > 0,
                true,
                true,
                0,
            )
            .expect("latency metadata")
        }
        fn create(
            &self,
            _native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
        ) -> Result<Box<dyn crate::AudioProcessor>, AudioProcessorHostError> {
            panic!("latency preparation must not create DSP");
        }
    }
    fn processor(latency: usize) -> AudioProcessorInstance {
        AudioProcessorInstance {
            id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::Clap {
                plugin_id: latency.to_string(),
                schema_version: 1,
                binary_sha256: None,
            },
            bypassed: false,
            parameters: Default::default(),
            opaque_state: None,
        }
    }
    fn fixture() -> (PreparedAudioPlan, [PreparedNodeOrigin; 3]) {
        let mut sequence = Sequence::new("latency sum reference");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let tracks = [sequence.audio_tracks[0].id, sequence.audio_tracks[1].id];
        for (track, position) in [(tracks[0], 0), (tracks[0], 1), (tracks[1], 0)] {
            sequence
                .add_media_audio_clip(
                    track,
                    Clip::new(
                        AssetId::new(),
                        TimelineTime::new(position, 1).expect("position"),
                        TimelineTime::ONE,
                    )
                    .expect("clip"),
                    AudioSourceComponentId::primary(),
                )
                .expect("source");
        }
        let a = &mut sequence.audio_program.track_channels.get_mut(&tracks[0]).expect("A").strip;
        a.pre_fader.processors.push(processor(3));
        a.post_fader.processors.push(processor(5));
        sequence
            .audio_program
            .track_channels
            .get_mut(&tracks[1])
            .expect("B")
            .strip
            .post_fader
            .processors
            .push(processor(2));
        let bus = MixBusId::new();
        let mut strip = mondrian_timeline::AudioChannelStrip::default();
        strip.pre_fader.processors.push(processor(7));
        sequence
            .audio_program
            .buses
            .push(AudioMixBus { id: bus, name: "sum".into(), strip });
        let output = sequence.audio_program.outputs[0].id;
        sequence.audio_program.outputs[0]
            .strip
            .post_fader
            .processors
            .push(processor(11));
        sequence.audio_program.routes.clear();
        sequence.audio_program.routes.extend([
            AudioRoute::new(
                AudioRouteSource::Track {
                    track_id: tracks[0],
                    port: AudioChannelStripOutputPort::PreFader,
                },
                AudioRouteDestination::Bus(bus),
            ),
            AudioRoute::new(
                AudioRouteSource::Track {
                    track_id: tracks[1],
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Bus(bus),
            ),
            AudioRoute::new(
                AudioRouteSource::Track {
                    track_id: tracks[0],
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Output(output),
            ),
            AudioRoute::new(
                AudioRouteSource::Bus {
                    bus_id: bus,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Output(output),
            ),
        ]);
        let mut plan = PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::new(
                compile_audio_program(&sequence, AudioCompileRequest::program(output))
                    .expect("graph"),
            ),
            AudioRenderContract {
                sample_rate: 2,
                channel_layout: AudioChannelLayout::Mono,
                max_block_frames: 4,
                processing_mode: AudioProcessingMode::Realtime,
                processor_session_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
                public_output_lookahead_budget_frames:
                    AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
                compensation_delay_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
            },
            AudioKernelBackend::ScalarReference,
            &Resolver,
        )
        .expect("metadata-only preparation");
        let mut a_arrivals = [4, 9].into_iter();
        for contribution in &mut plan.schedule.contributions {
            contribution.source_algorithmic_latency_frames =
                match plan.schedule.nodes[contribution.track_slot].origin {
                    PreparedNodeOrigin::Track(id) if id == tracks[0] => {
                        a_arrivals.next().expect("two A inputs")
                    }
                    PreparedNodeOrigin::Track(id) if id == tracks[1] => 1,
                    _ => panic!("unexpected fixture contribution owner"),
                };
        }
        (
            plan,
            [
                PreparedNodeOrigin::Track(tracks[0]),
                PreparedNodeOrigin::Track(tracks[1]),
                PreparedNodeOrigin::Bus(bus),
            ],
        )
    }
    fn solve(plan: &mut PreparedAudioPlan) -> Result<PreparedLatencyPlan, &'static str> {
        let s = &mut plan.schedule;
        solve_prepared_latency(PreparedLatencyGraph {
            nodes: &mut s.nodes,
            routes: &s.routes,
            contributions: &mut s.contributions,
            processors: &mut s.processors,
            auxiliary_buses: &s.processor_auxiliary_buses,
            bindings: &mut s.sidechain_bindings,
            sources: &s.sidechain_sources,
            execution_order: &s.execution_order,
            output_slot: s.output_slot,
        })
    }
    #[test]
    fn aligns_contributions_and_port_specific_routes_at_every_sum() {
        let (mut plan, origins) = fixture();
        let slots = origins.map(|origin| {
            plan.schedule
                .nodes
                .iter()
                .position(|node| node.origin == origin)
                .expect("typed owner slot")
        });
        let result = solve(&mut plan).expect("stage latency plan");
        for (index, contribution) in plan.schedule.contributions.iter().enumerate() {
            assert_eq!(
                result.contribution_compensation_frames[index],
                if contribution.source_algorithmic_latency_frames == 4 {
                    5
                } else {
                    0
                }
            );
        }
        assert_eq!(result.node_latencies[slots[0]].pre_fader_frames, 12);
        assert_eq!(result.node_latencies[slots[0]].post_fader_frames, 17);
        assert_eq!(result.node_latencies[slots[1]].post_fader_frames, 3);
        assert_eq!(result.node_latencies[slots[2]].post_fader_frames, 19);
        for (index, route) in plan.schedule.routes.iter().enumerate() {
            let expected = if route.destination_slot == slots[2] {
                if route.source_slot == slots[0] {
                    0
                } else {
                    9
                }
            } else if route.source_slot == slots[0] {
                2
            } else {
                0
            };
            assert_eq!(result.route_compensation_frames[index], expected);
        }
        assert_eq!(result.output_algorithmic_latency_frames, 30);
        assert_eq!(result.maximum_compensation_frames, 9);
    }
    #[test]
    fn rejects_a_non_topological_route_instead_of_guessing_latency() {
        let (mut plan, _) = fixture();
        let output = plan.schedule.output_slot;
        let start = plan.schedule.routes.len();
        plan.schedule.routes.push(PreparedRoute {
            source_slot: output,
            destination_slot: output,
            source_port: AudioChannelStripOutputPort::PostMute,
            processor_output: None,
            constant_gain: Some(1.0),
            gain_automation: None,
            compensation_delay_frames: 0,
        });
        plan.schedule.nodes[output].incoming = start..start + 1;
        assert!(solve(&mut plan).is_err());
    }
}
