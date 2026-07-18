//! Prepared-schedule latency propagation and delay-compensation solving.
//!
//! The solver operates only on dense prepared topology. It never scans author
//! Routes or infers latency from display names. Every summing point aligns its
//! incoming branches to the maximum declared arrival latency, then propagates
//! the resulting port latency downstream.

use crate::schedule::{PreparedNodeOrigin, PreparedRoute};
use mondrian_timeline::audio::AudioChannelStripOutputPort;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreparedNodeLatency {
    pub(crate) input_frames: usize,
    pub(crate) pre_fader_frames: usize,
    pub(crate) post_fader_frames: usize,
}

impl PreparedNodeLatency {
    fn port(self, port: AudioChannelStripOutputPort) -> usize {
        match port {
            AudioChannelStripOutputPort::PreFader => self.pre_fader_frames,
            AudioChannelStripOutputPort::PostFaderPreMute
            | AudioChannelStripOutputPort::PostMute => self.post_fader_frames,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedLatencyPlan {
    pub(crate) contribution_compensation_frames: Vec<usize>,
    pub(crate) route_compensation_frames: Vec<usize>,
    pub(crate) node_latencies: Vec<PreparedNodeLatency>,
    pub(crate) output_latency_frames: usize,
    pub(crate) maximum_compensation_frames: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedLatencyNodeInput {
    pub(crate) origin: PreparedNodeOrigin,
    pub(crate) contribution_start: usize,
    pub(crate) contribution_end: usize,
    pub(crate) route_start: usize,
    pub(crate) route_end: usize,
    pub(crate) pre_rack_latency_frames: usize,
    pub(crate) post_rack_latency_frames: usize,
}

pub(crate) fn solve_prepared_latency(
    nodes: &[PreparedLatencyNodeInput],
    routes: &[PreparedRoute],
    contribution_intrinsic_latency_frames: &[usize],
    output_slot: usize,
) -> Result<PreparedLatencyPlan, &'static str> {
    if output_slot >= nodes.len() {
        return Err("audio latency output slot is outside the prepared topology");
    }
    let mut contribution_compensation_frames = vec![0; contribution_intrinsic_latency_frames.len()];
    let mut route_compensation_frames = vec![0; routes.len()];
    let mut node_latencies = vec![PreparedNodeLatency::default(); nodes.len()];
    let mut maximum_compensation_frames = 0;

    for (node_slot, node) in nodes.iter().copied().enumerate() {
        let input_frames = match node.origin {
            PreparedNodeOrigin::Track(_) => {
                let contributions = contribution_intrinsic_latency_frames
                    .get(node.contribution_start..node.contribution_end)
                    .ok_or("audio latency contribution range is invalid")?;
                let maximum = contributions.iter().copied().max().unwrap_or(0);
                for (offset, latency) in contributions.iter().copied().enumerate() {
                    let compensation = maximum.saturating_sub(latency);
                    contribution_compensation_frames[node.contribution_start + offset] =
                        compensation;
                    maximum_compensation_frames = maximum_compensation_frames.max(compensation);
                }
                maximum
            }
            PreparedNodeOrigin::Bus(_) | PreparedNodeOrigin::Output(_) => {
                let incoming = routes
                    .get(node.route_start..node.route_end)
                    .ok_or("audio latency route range is invalid")?;
                let mut maximum = 0;
                for route in incoming {
                    if route.destination_slot != node_slot || route.source_slot >= node_slot {
                        return Err("audio latency route violates prepared topological order");
                    }
                    maximum =
                        maximum.max(node_latencies[route.source_slot].port(route.source_port));
                }
                for (offset, route) in incoming.iter().enumerate() {
                    let arrival = node_latencies[route.source_slot].port(route.source_port);
                    let compensation = maximum.saturating_sub(arrival);
                    route_compensation_frames[node.route_start + offset] = compensation;
                    maximum_compensation_frames = maximum_compensation_frames.max(compensation);
                }
                maximum
            }
        };
        let pre_fader_frames = input_frames
            .checked_add(node.pre_rack_latency_frames)
            .ok_or("audio latency exceeds the prepared frame range")?;
        let post_fader_frames = pre_fader_frames
            .checked_add(node.post_rack_latency_frames)
            .ok_or("audio latency exceeds the prepared frame range")?;
        node_latencies[node_slot] =
            PreparedNodeLatency { input_frames, pre_fader_frames, post_fader_frames };
    }

    Ok(PreparedLatencyPlan {
        contribution_compensation_frames,
        route_compensation_frames,
        node_latencies: node_latencies.clone(),
        output_latency_frames: node_latencies[output_slot].post_fader_frames,
        maximum_compensation_frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{MixBusId, ProgramOutputId, TrackId};

    fn node(
        origin: PreparedNodeOrigin,
        contributions: std::ops::Range<usize>,
        routes: std::ops::Range<usize>,
        pre: usize,
        post: usize,
    ) -> PreparedLatencyNodeInput {
        PreparedLatencyNodeInput {
            origin,
            contribution_start: contributions.start,
            contribution_end: contributions.end,
            route_start: routes.start,
            route_end: routes.end,
            pre_rack_latency_frames: pre,
            post_rack_latency_frames: post,
        }
    }

    #[test]
    fn aligns_contributions_and_port_specific_routes_at_every_sum() {
        let track_a = TrackId::new();
        let track_b = TrackId::new();
        let bus = MixBusId::new();
        let output = ProgramOutputId::new();
        let nodes = [
            node(PreparedNodeOrigin::Track(track_a), 0..2, 0..0, 3, 5),
            node(PreparedNodeOrigin::Track(track_b), 2..3, 0..0, 0, 2),
            node(PreparedNodeOrigin::Bus(bus), 3..3, 0..2, 7, 0),
            node(PreparedNodeOrigin::Output(output), 3..3, 2..4, 0, 11),
        ];
        let routes = [
            PreparedRoute {
                source_slot: 0,
                destination_slot: 2,
                source_port: AudioChannelStripOutputPort::PreFader,
                compensation_delay_frames: 0,
            },
            PreparedRoute {
                source_slot: 1,
                destination_slot: 2,
                source_port: AudioChannelStripOutputPort::PostMute,
                compensation_delay_frames: 0,
            },
            PreparedRoute {
                source_slot: 0,
                destination_slot: 3,
                source_port: AudioChannelStripOutputPort::PostMute,
                compensation_delay_frames: 0,
            },
            PreparedRoute {
                source_slot: 2,
                destination_slot: 3,
                source_port: AudioChannelStripOutputPort::PostMute,
                compensation_delay_frames: 0,
            },
        ];

        let plan = solve_prepared_latency(&nodes, &routes, &[4, 9, 1], 3).expect("latency plan");

        assert_eq!(plan.contribution_compensation_frames, [5, 0, 0]);
        assert_eq!(plan.node_latencies[0].pre_fader_frames, 12);
        assert_eq!(plan.node_latencies[0].post_fader_frames, 17);
        assert_eq!(plan.node_latencies[1].post_fader_frames, 3);
        assert_eq!(plan.route_compensation_frames[0..2], [0, 9]);
        assert_eq!(plan.node_latencies[2].post_fader_frames, 19);
        assert_eq!(plan.route_compensation_frames[2..4], [2, 0]);
        assert_eq!(plan.output_latency_frames, 30);
        assert_eq!(plan.maximum_compensation_frames, 9);
    }

    #[test]
    fn rejects_a_non_topological_route_instead_of_guessing_latency() {
        let output = ProgramOutputId::new();
        let nodes = [node(PreparedNodeOrigin::Output(output), 0..0, 0..1, 0, 0)];
        let routes = [PreparedRoute {
            source_slot: 0,
            destination_slot: 0,
            source_port: AudioChannelStripOutputPort::PostMute,
            compensation_delay_frames: 0,
        }];
        assert!(solve_prepared_latency(&nodes, &routes, &[], 0).is_err());
    }
}
