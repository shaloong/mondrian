//! Same-child configuration seed ownership; no plan or playback publication.

use super::{NativeConfigurationCandidate, NativeConfigurationFormat, NativeRequestCohort};
use crate::AudioProcessorExecutionContract;
use sha2::{Digest, Sha256};
use std::num::NonZeroU64;

/// Exact configuration issued by one retained child control attempt.
///
/// A ticket has authority only when compared with that same child's private ready
/// record. It is not an occurrence, plan, session, epoch or install lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeConfigurationSeedTicket {
    /// Serviced native callback generation.
    pub serviced_cohort: NativeRequestCohort,
    /// Exact original non-RT control attempt.
    pub control_token: NonZeroU64,
    /// Actual activated scheduling facts observed during that attempt.
    pub observed_contract: AudioProcessorExecutionContract,
    /// Canonical native state encoding.
    pub format: NativeConfigurationFormat,
    /// Initialized configuration extent, independently of allocation capacity.
    pub configuration_bytes: usize,
    /// SHA256 of the child-issued configuration bytes, never private DSP history.
    pub configuration_sha256: [u8; 32],
}

impl NativeConfigurationSeedTicket {
    /// Project immutable state facts without allocating or taking child authority.
    pub fn from_candidate(candidate: &NativeConfigurationCandidate) -> Self {
        Self {
            serviced_cohort: candidate.serviced_cohort,
            control_token: candidate.control_token,
            observed_contract: candidate.observed_contract,
            format: candidate.format,
            configuration_bytes: candidate.configuration.len(),
            configuration_sha256: Sha256::digest(&candidate.configuration).into(),
        }
    }
}

/// Borrowed parent command; the original candidate allocation remains owned.
pub struct NativeConfigurationSeedRequest<'a> {
    /// Child-issued candidate identity to stage on that same exclusive object.
    pub ticket: NativeConfigurationSeedTicket,
    /// Exact candidate bytes; no controller-only approximation is accepted.
    pub configuration: &'a [u8],
    /// Additional actual child seed allocation capacity grant.
    ///
    /// This excludes old/parent configuration owners, mapping, IPC, SDK and plan
    /// storage; a complete peak ledger must account for them separately.
    pub seed_capacity_grant_bytes: usize,
}

/// Owned child decoder result, adopted without a second seed allocation.
pub struct NativeConfigurationOwnedSeed {
    /// Exact ticket verified against the retained child's private ready record.
    pub ticket: NativeConfigurationSeedTicket,
    /// Actual decoded configuration owner.
    pub configuration: Vec<u8>,
    /// Maximum permitted actual capacity of this one additional owner.
    pub seed_capacity_grant_bytes: usize,
}

/// Safe refusal leaves this child stopped and the configuration phase blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeConfigurationSeedRefusal {
    /// The concrete adapter lacks retained seed ownership.
    UnsupportedAdapter,
    /// Another service/cohort/configuration was requested or is now pending.
    StaleTicket,
    /// Actual bytes do not match the configuration issued by this child.
    ConfigurationMismatch,
    /// The one seed allocation could not fit its explicit capacity grant.
    BufferGrantExceeded,
    /// No matching successful StageSeed was retained before CommitSeed.
    SeedNotStaged,
}

/// Actual one-owner child seed allocation facts, not complete peak admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeConfigurationSeedReceipt {
    /// Exact same-child configuration identity.
    pub ticket: NativeConfigurationSeedTicket,
    /// Actual Vec capacity in bytes; no assumption about reserve equality.
    pub child_seed_capacity_bytes: usize,
}

/// Stage or commit result from an explicitly named child control command.
///
/// Stage acceptance only owns stopped configuration. Commit acceptance installs
/// this child's admitted contract/seed only. Neither result publishes a prepared
/// root plan, resumes audio, commits Undo, or proves private DSP continuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeConfigurationSeedOutcome {
    /// The named command accepted its exact child seed receipt.
    Accepted(NativeConfigurationSeedReceipt),
    /// A safe bounded refusal; no automatic retry follows.
    Refused(NativeConfigurationSeedRefusal),
}

pub(super) struct ReadyNativeConfiguration {
    ticket: NativeConfigurationSeedTicket,
    staged: Option<Vec<u8>>,
}

impl ReadyNativeConfiguration {
    pub(super) fn issued(candidate: &NativeConfigurationCandidate) -> Self {
        Self {
            ticket: NativeConfigurationSeedTicket::from_candidate(candidate),
            staged: None,
        }
    }

    pub(super) const fn ticket(&self) -> NativeConfigurationSeedTicket {
        self.ticket
    }

    pub(super) fn stage(
        &mut self,
        seed: NativeConfigurationOwnedSeed,
    ) -> NativeConfigurationSeedOutcome {
        if seed.ticket != self.ticket {
            return NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::StaleTicket,
            );
        }
        if seed.configuration.len() != self.ticket.configuration_bytes
            || <[u8; 32]>::from(Sha256::digest(&seed.configuration))
                != self.ticket.configuration_sha256
        {
            return NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::ConfigurationMismatch,
            );
        }
        if seed.configuration.capacity() > seed.seed_capacity_grant_bytes {
            return NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::BufferGrantExceeded,
            );
        }
        if let Some(staged) = &self.staged {
            // Idempotent same-ticket staging retains the original owner. Both
            // incoming and retained owners still overlap in the caller's peak.
            if staged.capacity() > seed.seed_capacity_grant_bytes {
                return NativeConfigurationSeedOutcome::Refused(
                    NativeConfigurationSeedRefusal::BufferGrantExceeded,
                );
            }
            return NativeConfigurationSeedOutcome::Accepted(NativeConfigurationSeedReceipt {
                ticket: self.ticket,
                child_seed_capacity_bytes: staged.capacity(),
            });
        }
        let receipt = NativeConfigurationSeedReceipt {
            ticket: self.ticket,
            child_seed_capacity_bytes: seed.configuration.capacity(),
        };
        self.staged = Some(seed.configuration);
        NativeConfigurationSeedOutcome::Accepted(receipt)
    }

    /// Move an already accounted owner after the adapter's atomic serial claim.
    pub(super) fn take_seed(
        &mut self,
        ticket: NativeConfigurationSeedTicket,
    ) -> Result<(Vec<u8>, NativeConfigurationSeedReceipt), NativeConfigurationSeedRefusal> {
        if ticket != self.ticket {
            return Err(NativeConfigurationSeedRefusal::StaleTicket);
        }
        let seed = self.staged.take().ok_or(NativeConfigurationSeedRefusal::SeedNotStaged)?;
        let receipt =
            NativeConfigurationSeedReceipt { ticket, child_seed_capacity_bytes: seed.capacity() };
        Ok((seed, receipt))
    }

    pub(super) fn staged_receipt(&self) -> Option<NativeConfigurationSeedReceipt> {
        self.staged.as_ref().map(|seed| NativeConfigurationSeedReceipt {
            ticket: self.ticket,
            child_seed_capacity_bytes: seed.capacity(),
        })
    }
}

#[cfg(test)]
mod seed_tests {
    use super::super::{NativeConfigurationFormat, NativeControlBufferReceipt};
    use super::*;
    use crate::{AudioProcessorExecutionContract, AudioProcessorTail};
    use mondrian_core::NativeAudioReprepareRequest;

    fn candidate() -> NativeConfigurationCandidate {
        NativeConfigurationCandidate {
            serviced_cohort: NativeRequestCohort::new(1, NativeAudioReprepareRequest::ClapRestart)
                .expect("recorded cohort"),
            control_token: NonZeroU64::new(17).expect("token"),
            observed_contract: AudioProcessorExecutionContract::new(
                7,
                AudioProcessorTail::Finite(7),
                true,
                true,
                true,
                0,
            )
            .expect("observed facts"),
            format: NativeConfigurationFormat::ClapState,
            configuration: vec![7, 1, 2],
            buffers: NativeControlBufferReceipt {
                request_value_capacity_bytes: 0,
                capture_payload_bytes: 0,
                configuration_capacity_bytes: 3,
            },
            parent_configuration_capacity_bytes: 0,
        }
    }
    fn owned(
        ticket: NativeConfigurationSeedTicket,
        bytes: Vec<u8>,
        grant: usize,
    ) -> NativeConfigurationOwnedSeed {
        NativeConfigurationOwnedSeed {
            ticket,
            configuration: bytes,
            seed_capacity_grant_bytes: grant,
        }
    }

    #[test]
    fn native_configuration_seed_exact_owner_moves_without_a_second_seed_allocation() {
        let candidate = candidate();
        let ticket = NativeConfigurationSeedTicket::from_candidate(&candidate);
        let mut ready = ReadyNativeConfiguration::issued(&candidate);
        let mut bytes = Vec::with_capacity(19);
        bytes.extend_from_slice(&candidate.configuration);
        let pointer = bytes.as_ptr();
        let capacity = bytes.capacity();
        assert_eq!(
            ready.stage(owned(ticket, bytes, capacity)),
            NativeConfigurationSeedOutcome::Accepted(NativeConfigurationSeedReceipt {
                ticket,
                child_seed_capacity_bytes: capacity,
            })
        );
        let (seed, receipt) = ready.take_seed(ticket).expect("preowned exact seed");
        assert_eq!(seed.as_ptr(), pointer);
        assert_eq!(seed.capacity(), capacity);
        assert_eq!(receipt.child_seed_capacity_bytes, capacity);
        assert_eq!(seed, candidate.configuration);
        assert_eq!(
            ready.take_seed(ticket),
            Err(NativeConfigurationSeedRefusal::SeedNotStaged)
        );
    }

    #[test]
    fn native_configuration_seed_ticket_does_not_reduce_identity_to_class_or_token() {
        let candidate = candidate();
        let ticket = NativeConfigurationSeedTicket::from_candidate(&candidate);
        for index in 0..7 {
            let mut wrong = ticket;
            match index {
                0 => wrong.control_token = NonZeroU64::new(18).expect("different token"),
                1 => wrong.serviced_cohort.serial = 2,
                2 => {
                    wrong.serviced_cohort.request =
                        NativeAudioReprepareRequest::Vst3Restart { flags: 8 }
                }
                3 => {
                    wrong.observed_contract = AudioProcessorExecutionContract::new(
                        3,
                        AudioProcessorTail::Finite(3),
                        true,
                        true,
                        true,
                        0,
                    )
                    .expect("old facts")
                }
                4 => wrong.format = NativeConfigurationFormat::Vst3ComponentAndController,
                5 => wrong.configuration_bytes += 1,
                _ => wrong.configuration_sha256[0] ^= 1,
            }
            let mut ready = ReadyNativeConfiguration::issued(&candidate);
            assert_eq!(
                ready.stage(owned(wrong, candidate.configuration.clone(), 64)),
                NativeConfigurationSeedOutcome::Refused(
                    NativeConfigurationSeedRefusal::StaleTicket
                )
            );
            assert_eq!(
                ready.take_seed(wrong),
                Err(NativeConfigurationSeedRefusal::StaleTicket)
            );
            assert!(ready.staged_receipt().is_none());
        }
    }

    #[test]
    fn native_configuration_seed_actual_capacity_and_body_are_independently_checked() {
        let candidate = candidate();
        let ticket = NativeConfigurationSeedTicket::from_candidate(&candidate);
        let mut ready = ReadyNativeConfiguration::issued(&candidate);
        let mut wrong = candidate.configuration.clone();
        wrong[0] ^= 1;
        assert_eq!(
            ready.stage(owned(ticket, wrong, 64)),
            NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::ConfigurationMismatch
            )
        );
        let mut excessive = Vec::with_capacity(19);
        excessive.extend_from_slice(&candidate.configuration);
        let capacity = excessive.capacity();
        assert!(capacity > 3);
        assert_eq!(
            ready.stage(owned(ticket, excessive, 3)),
            NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::BufferGrantExceeded
            )
        );
        assert!(ready.staged_receipt().is_none());
        assert!(matches!(
            ready.stage(owned(ticket, candidate.configuration.clone(), capacity)),
            NativeConfigurationSeedOutcome::Accepted(_)
        ));
    }

    #[test]
    fn native_configuration_seed_idempotent_stage_keeps_first_owner_and_its_capacity() {
        let candidate = candidate();
        let ticket = NativeConfigurationSeedTicket::from_candidate(&candidate);
        let mut ready = ReadyNativeConfiguration::issued(&candidate);
        let mut first = Vec::with_capacity(19);
        first.extend_from_slice(&candidate.configuration);
        let first_pointer = first.as_ptr();
        let first_capacity = first.capacity();
        let accepted = ready.stage(owned(ticket, first, first_capacity));
        assert_eq!(
            ready.stage(owned(ticket, candidate.configuration.clone(), 3)),
            NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::BufferGrantExceeded
            )
        );
        assert_eq!(
            ready.stage(owned(
                ticket,
                candidate.configuration.clone(),
                first_capacity
            )),
            accepted
        );
        let (seed, _) = ready.take_seed(ticket).expect("original child owner");
        assert_eq!(seed.as_ptr(), first_pointer);
        assert_eq!(seed.capacity(), first_capacity);
    }

    #[test]
    fn native_configuration_seed_serial_claim_preserves_before_and_after_same_bit_callbacks() {
        use super::super::native_request_cohort::NativeRequestMarker;
        let marker = NativeRequestMarker::new();
        assert!(!marker.claim_retired_serial(0));
        assert!(!marker.claim_retired_serial(u32::MAX));
        marker.record(1);
        let serviced = marker.snapshot();
        assert!(!marker.claim_retired_serial(serviced.serial()));
        assert!(marker.retire(serviced));
        assert!(marker.claim_retired_serial(serviced.serial()));
        marker.record(1);
        assert!(!marker.claim_retired_serial(serviced.serial()));
        let newer = marker.snapshot();
        assert_eq!(newer.serial(), serviced.serial() + 1);
        assert_eq!(newer.flags(), 1);
        assert!(marker.retire(newer));
        assert!(marker.claim_retired_serial(newer.serial()));
    }
}
