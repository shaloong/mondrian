//! Bounded inactive CLAP observations, not a second routing or instance model.
use super::NativeRequestCohort;
use mondrian_core::{AudioChannelLayout, NativeAudioReprepareRequest};
use std::num::NonZeroU64;

pub(super) const MAX_PORTS: usize = 32;

/// One actual CLAP input in native index order. IDs are direction-local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapAudioInputPort {
    /// Native CLAP port ID, not the index used for process buffers.
    pub native_id: u32,
    /// Host-facing layout. An arbitrary native port uses the adapter's explicit
    /// ordinal lane mapping; this does not claim native speaker semantics.
    pub channel_layout: AudioChannelLayout,
    /// A Main input, when present, is at native index zero.
    pub is_main: bool,
}

/// Same-instance inactive observation awaiting coordinated resource admission.
///
/// This inline bounded value owns no heap. It neither consumes the request nor
/// authorizes activation, mapping replacement, seed commit or old-shape DSP.
/// Main/output compatibility is checked by the adapter before issuing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClapAudioInputProposal {
    /// Still-pending native generation; this has not been retired.
    pub observed_cohort: NativeRequestCohort,
    /// Original stopped service attempt which observed these native ports.
    pub control_token: NonZeroU64,
    ports: [Option<ClapAudioInputPort>; MAX_PORTS],
    len: u8,
}

impl ClapAudioInputProposal {
    /// First installed migration keeps native IDs/order and authored route keys.
    /// Only explicit Mono/Stereo widths may differ; port additions need a wider
    /// metadata transaction and are never silently rebound to another input.
    pub(crate) fn preserves_auxiliary_keys(
        &self,
        before: &crate::AudioProcessorAuxiliaryInputContract,
    ) -> bool {
        let mut ports = self.ports().filter(|p| !p.is_main);
        before.buses.iter().all(|bus| {
            ports.next().is_some_and(|p| {
                bus.bus_key
                    .strip_prefix("clap.audio-input.")
                    .and_then(|id| id.parse::<u32>().ok())
                    == Some(p.native_id)
                    && matches!(
                        bus.channel_layout,
                        AudioChannelLayout::Mono | AudioChannelLayout::Stereo
                    )
            })
        }) && ports.next().is_none()
    }

    pub(crate) fn matches_reconfigured_inputs(
        &self,
        before: &crate::AudioProcessorAuxiliaryInputContract,
        after: &crate::AudioProcessorAuxiliaryInputContract,
    ) -> bool {
        self.preserves_auxiliary_keys(before)
            && before.buses.len() == after.buses.len()
            && before
                .buses
                .iter()
                .zip(&after.buses)
                .zip(self.ports().filter(|p| !p.is_main))
                .all(|((a, b), p)| a.bus_key == b.bus_key && b.channel_layout == p.channel_layout)
    }

    fn adjusted_scratch(
        &self,
        original: usize,
        frames: usize,
        before: &crate::AudioProcessorAuxiliaryInputContract,
        shared: bool,
    ) -> Option<usize> {
        if !self.preserves_auxiliary_keys(before) || frames == 0 {
            return None;
        }
        let old = before.buses.iter().try_fold(0_usize, |n, b| {
            n.checked_add(b.channel_layout.channel_count())
        })?;
        let new = self.ports().filter(|p| !p.is_main).try_fold(0_usize, |n, p| {
            n.checked_add(p.channel_layout.channel_count())
        })?;
        // IDs/counts stay fixed: only channel Vec headers, f32 planes, SDK
        // pointers and (for the isolated model) one shared PCM region differ.
        let pcm = frames.checked_mul(std::mem::size_of::<f32>())?;
        let unit = std::mem::size_of::<Vec<f32>>()
            .checked_add(std::mem::size_of::<*mut f32>())?
            .checked_add(pcm)?
            .checked_add(if shared { pcm } else { 0 })?;
        original
            .checked_sub(old.checked_mul(unit)?)?
            .checked_add(new.checked_mul(unit)?)
    }

    pub(crate) fn native_scratch_bytes(
        &self,
        original: usize,
        frames: usize,
        before: &crate::AudioProcessorAuxiliaryInputContract,
    ) -> Option<usize> {
        self.adjusted_scratch(original, frames, before, false)
    }

    pub(crate) fn isolated_scratch_bytes(
        &self,
        original: usize,
        frames: usize,
        before: &crate::AudioProcessorAuxiliaryInputContract,
    ) -> Option<usize> {
        self.adjusted_scratch(original, frames, before, true)
    }

    pub(super) fn new(
        observed_cohort: NativeRequestCohort,
        control_token: NonZeroU64,
    ) -> Result<Self, &'static str> {
        if observed_cohort.request != NativeAudioReprepareRequest::ClapRestart
            || observed_cohort.serial == 0
            || observed_cohort.exhausted()
        {
            return Err("invalid CLAP input proposal cohort");
        }
        Ok(Self {
            observed_cohort,
            control_token,
            ports: [None; MAX_PORTS],
            len: 0,
        })
    }

    /// Complete ordered native input list, including Main when present.
    pub fn ports(&self) -> impl ExactSizeIterator<Item = &ClapAudioInputPort> {
        // Private construction and push preserve a dense initialized prefix.
        self.ports[..usize::from(self.len)].iter().map(|port| port.as_ref().unwrap())
    }

    pub(super) fn push(&mut self, port: ClapAudioInputPort) -> Result<(), &'static str> {
        let index = usize::from(self.len);
        let main_present = self.ports().any(|port| port.is_main);
        let auxiliary_count = index - usize::from(main_present);
        if index >= MAX_PORTS
            || port.native_id == u32::MAX
            || !matches!(
                port.channel_layout,
                AudioChannelLayout::Mono | AudioChannelLayout::Stereo
            )
            || (port.is_main && index != 0)
            || (!port.is_main && auxiliary_count >= MAX_PORTS - 1)
            || self.ports().any(|other| other.native_id == port.native_id)
        {
            return Err("invalid CLAP input proposal port");
        }
        self.ports[index] = Some(port);
        self.len += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_input_width_fee_matches_actual_plane_owners_in_both_directions() {
        use super::super::clap_audio_input_buffers::{reservation, StagedClapAudioInputs};
        use super::super::ClapAudioInputBufferRequest;
        use crate::{
            AudioProcessorAuxiliaryInputBusContract, AudioProcessorAuxiliaryInputContract,
        };
        let before = AudioProcessorAuxiliaryInputContract {
            buses: vec![
                AudioProcessorAuxiliaryInputBusContract {
                    bus_key: "clap.audio-input.11".into(),
                    channel_layout: AudioChannelLayout::Mono,
                },
                AudioProcessorAuxiliaryInputBusContract {
                    bus_key: "clap.audio-input.7".into(),
                    channel_layout: AudioChannelLayout::Stereo,
                },
            ],
        };
        let cohort = NativeRequestCohort::new(3, NativeAudioReprepareRequest::ClapRestart).unwrap();
        for frames in [1, 4, 64] {
            for next in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
                let mut old =
                    ClapAudioInputProposal::new(cohort, NonZeroU64::new(1).unwrap()).unwrap();
                let mut new = old;
                for p in [&mut old, &mut new] {
                    p.push(ClapAudioInputPort {
                        native_id: 0,
                        channel_layout: AudioChannelLayout::Stereo,
                        is_main: true,
                    })
                    .unwrap();
                }
                for (id, layout) in [
                    (11, AudioChannelLayout::Mono),
                    (7, AudioChannelLayout::Stereo),
                ] {
                    old.push(ClapAudioInputPort {
                        native_id: id,
                        channel_layout: layout,
                        is_main: false,
                    })
                    .unwrap();
                    new.push(ClapAudioInputPort {
                        native_id: id,
                        channel_layout: next,
                        is_main: false,
                    })
                    .unwrap();
                }
                let prepare = |proposal| {
                    StagedClapAudioInputs::prepare(ClapAudioInputBufferRequest {
                        proposal,
                        max_block_frames: frames,
                        buffer_grant_bytes: reservation(&proposal, frames).unwrap(),
                    })
                    .unwrap()
                };
                let a = prepare(old);
                let b = prepare(new);
                assert_eq!(
                    new.native_scratch_bytes(10000, frames, &before),
                    10000usize
                        .checked_sub(a.receipt.allocated_bytes)
                        .and_then(|n| n.checked_add(b.receipt.allocated_bytes))
                );
                assert!(new.native_scratch_bytes(0, frames, &before).is_none());
                assert!(new.native_scratch_bytes(10000, usize::MAX, &before).is_none());
                let mut changed = before.clone();
                changed.buses[0].bus_key = "clap.audio-input.12".into();
                assert!(!new.preserves_auxiliary_keys(&changed));
                let mut reordered = before.clone();
                reordered.buses.reverse();
                assert!(!new.preserves_auxiliary_keys(&reordered));
            }
        }
    }

    fn proposal() -> ClapAudioInputProposal {
        ClapAudioInputProposal::new(
            NativeRequestCohort::new(1, NativeAudioReprepareRequest::ClapRestart).unwrap(),
            NonZeroU64::new(17).unwrap(),
        )
        .unwrap()
    }

    fn port(id: u32, main: bool) -> ClapAudioInputPort {
        ClapAudioInputPort {
            native_id: id,
            channel_layout: AudioChannelLayout::Stereo,
            is_main: main,
        }
    }

    #[test]
    fn native_ids_roles_and_bounded_prefix_are_preserved() {
        for main in [false, true] {
            let mut proposal = proposal();
            if main {
                proposal.push(port(0, true)).unwrap();
            }
            for id in 1..=31 {
                proposal.push(port(id, false)).unwrap();
            }
            let before = proposal;
            assert!(proposal.push(port(32, false)).is_err());
            assert_eq!(proposal, before);
            assert_eq!(proposal.ports().len(), 31 + usize::from(main));
            assert_eq!(
                proposal.ports().filter(|port| port.is_main).count(),
                usize::from(main)
            );
        }
    }

    #[test]
    fn invalid_rows_and_non_clap_or_exhausted_cohorts_cannot_be_issued() {
        let mut proposal = proposal();
        proposal.push(port(7, false)).unwrap();
        let before = proposal;
        for invalid in [
            port(7, false),
            port(11, true),
            port(u32::MAX, false),
            ClapAudioInputPort {
                channel_layout: AudioChannelLayout::discrete(2).unwrap(),
                ..port(12, false)
            },
        ] {
            assert!(proposal.push(invalid).is_err());
            assert_eq!(proposal, before);
        }
        for (serial, request) in [
            (1, NativeAudioReprepareRequest::Vst3Restart { flags: 8 }),
            (u32::MAX, NativeAudioReprepareRequest::ClapRestart),
        ] {
            let cohort = NativeRequestCohort::new(serial, request).unwrap();
            assert!(ClapAudioInputProposal::new(cohort, NonZeroU64::new(1).unwrap()).is_err());
        }
    }

    #[test]
    fn an_input_observation_cannot_issue_a_configuration_seed_or_unblock_dsp() {
        use super::super::native_configuration_control::NativeConfigurationPhase;
        let proposal = proposal();
        let phase = NativeConfigurationPhase::AudioInputsProposed(proposal);
        assert_eq!(
            phase.blocked_request(),
            Some(proposal.observed_cohort.request)
        );
        assert_eq!(phase.ready_token(), None);
        assert_eq!(phase.seed_ticket(), None);
    }
}
