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
    /// Explicitly declared channel meaning; never inferred from count alone.
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
