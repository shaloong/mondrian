//! One bounded current command interpretation on the authenticated worker stream.
//! Inline native adapters use it directly; native objects stay with the sole
//! owner dispatcher. This module adds no reader thread or editor authority.

use super::native_configuration_seed_wire::{self as seed_wire, DecodedStage};
use super::native_configuration_wire::{self as control_wire, OwnedControlRequest};
use super::protocol::{CommandKind, WorkerCommand};
use super::NativeConfigurationSeedTicket;
use crate::AudioProcessorHostError;
use std::io::Read;

pub(super) enum NativeCommand {
    Plain(WorkerCommand),
    Configuration(OwnedControlRequest),
    StageSeed(DecodedStage),
    CommitSeed(NativeConfigurationSeedTicket),
    BindObject(crate::NativeAudioObjectBindRequest),
    PollObservations(crate::NativeAudioObservationConsumption),
    EditorOpen(crate::NativeAudioEditorOpenRequest),
    EditorClose(crate::NativeAudioObjectBindRequest),
    AuthorStateEntry(super::native_audio_author_state_entry_wire::OwnedStateEntry),
}

impl NativeCommand {
    /// The actual decoded payload owner; union ceilings retain its real alignment.
    /// Fixed command/header storage is already charged to the concrete slot.
    pub(super) fn owned_input_layout(
        &self,
    ) -> Result<crate::NativeHostBufferLayout, AudioProcessorHostError> {
        let (bytes, alignment) = match self {
            Self::Configuration(request) => (
                request
                    .value_bytes()
                    .map_err(|error| failed("configuration request capacity", error))?,
                align_of::<f64>(),
            ),
            Self::StageSeed(DecodedStage::Owned(seed)) => {
                (seed.configuration.capacity(), align_of::<u8>())
            }
            Self::EditorOpen(request) => (request.title().len(), align_of::<u8>()),
            Self::AuthorStateEntry(request) => {
                let layout = request.owned_layout();
                (layout.bytes, layout.alignment)
            }
            _ => (0, 1),
        };
        Ok(crate::NativeHostBufferLayout { bytes, alignment })
    }
}

/// The single named-control decoder, shared by inline and serviced native lanes.
pub(super) fn decode(
    stream: &mut impl Read,
    parameter_lanes: usize,
) -> Result<NativeCommand, AudioProcessorHostError> {
    let command = WorkerCommand::read_from(stream).map_err(|error| failed("command", error))?;
    match command.kind {
        CommandKind::ServiceConfiguration => control_wire::read_request(stream, parameter_lanes)
            .map(NativeCommand::Configuration)
            .map_err(|error| failed("configuration request", error)),
        CommandKind::StageConfigurationSeed => seed_wire::read_stage(stream)
            .map(NativeCommand::StageSeed)
            .map_err(|error| failed("seed stage request", error)),
        CommandKind::CommitConfigurationSeed => seed_wire::read_commit(stream)
            .map(NativeCommand::CommitSeed)
            .map_err(|error| failed("seed commit request", error)),
        CommandKind::PollNativeObservations => {
            super::native_audio_object_wire::read_observation_request(stream)
                .map(NativeCommand::PollObservations)
                .map_err(|error| failed("native observation request", error))
        }
        CommandKind::BindNativeObject => super::native_audio_object_wire::read_bind_request(stream)
            .map(NativeCommand::BindObject)
            .map_err(|error| failed("native bind request", error)),
        CommandKind::EditorOpen => super::native_audio_editor_wire::read_open_request(stream)
            .map(NativeCommand::EditorOpen)
            .map_err(|error| failed("native editor open request", error)),
        CommandKind::EditorClose => super::native_audio_object_wire::read_bind_request(stream)
            .map(NativeCommand::EditorClose)
            .map_err(|error| failed("native editor close request", error)),
        CommandKind::PrepareAuthorStateEntry => {
            super::native_audio_author_state_entry_wire::read_request(stream)
                .map(NativeCommand::AuthorStateEntry)
                .map_err(|error| failed("native author state-entry request", error))
        }
        _ => Ok(NativeCommand::Plain(command)),
    }
}

fn failed(operation: &'static str, detail: impl std::fmt::Display) -> AudioProcessorHostError {
    AudioProcessorHostError::WorkerFailed { operation, detail: detail.to_string() }
}

#[cfg(test)]
mod inline_command_contracts {
    use super::super::native_configuration_control::{
        NativeConfigurationControlRequest, NativeRequestCohort,
    };
    use super::super::native_configuration_seed::{
        NativeConfigurationSeedRequest, NativeConfigurationSeedTicket,
    };
    use super::super::protocol::WorkerResponse;
    use super::super::{NativeConfigurationFormat, NativeConfigurationSeedRefusal};
    use super::*;
    use sha2::{Digest, Sha256};
    use std::net::{TcpListener, TcpStream};
    use std::num::NonZeroU64;
    use std::time::Duration;

    fn header(tag: u32) -> WorkerCommand {
        WorkerCommand {
            kind: match tag {
                1 => CommandKind::EnterState,
                2 => CommandKind::Process,
                3 => CommandKind::Shutdown,
                4 => CommandKind::PollPending,
                5 => CommandKind::ServiceConfiguration,
                6 => CommandKind::PollConfigurationCohort,
                7 => CommandKind::StageConfigurationSeed,
                8 => CommandKind::CommitConfigurationSeed,
                _ => panic!("exact current original tag"),
            },
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
    }

    fn ticket(bytes: &[u8]) -> NativeConfigurationSeedTicket {
        NativeConfigurationSeedTicket {
            serviced_cohort: NativeRequestCohort::new(
                7,
                mondrian_core::NativeAudioReprepareRequest::ClapRestart,
            )
            .expect("cohort"),
            control_token: NonZeroU64::new(31).expect("token"),
            observed_contract: crate::AudioProcessorExecutionContract::new(
                0,
                crate::AudioProcessorTail::None,
                true,
                true,
                true,
                0,
            )
            .expect("contract"),
            format: NativeConfigurationFormat::ClapState,
            configuration_bytes: bytes.len(),
            configuration_sha256: Sha256::digest(bytes).into(),
        }
    }

    fn paired_socket(test: impl FnOnce(&mut TcpStream, &mut TcpStream)) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("portable endpoint");
        let mut parent = TcpStream::connect(listener.local_addr().expect("bound endpoint"))
            .expect("same connection");
        let (mut child, _) = listener.accept().expect("accepted connection");
        parent.set_read_timeout(Some(Duration::from_secs(2))).expect("parent deadline");
        child.set_read_timeout(Some(Duration::from_secs(2))).expect("child deadline");
        test(&mut parent, &mut child);
    }

    #[test]
    fn native_inline_decoder_preserves_all_original_tags_and_each_exact_body() {
        use super::super::native_worker_command::{decode, NativeCommand};
        paired_socket(|parent, child| {
            let bytes = [3, 5, 7];
            let seed = ticket(&bytes);
            for version in [8_u32, 9, 10, 11, 12] {
                let mut old = Vec::new();
                header(1).write_to(&mut old).expect("one complete old command");
                old[4..8].copy_from_slice(&version.to_le_bytes());
                use std::io::Write;
                parent.write_all(&old).expect("same authenticated transport");
                assert!(
                    decode(child, 2).is_err(),
                    "old outer schema cannot dispatch"
                );
            }
            for tag in 1..=8 {
                let command = header(tag);
                let mut wire = Vec::new();
                command.write_to(&mut wire).expect("existing command codec");
                assert_eq!(
                    &wire[4..12],
                    &[13, 0, 0, 0, tag as u8, 0, 0, 0],
                    "independent version/tag contract"
                );
                use std::io::Write;
                parent.write_all(&wire).expect("same authenticated transport");
                match tag {
                    5 => super::super::native_configuration_wire::write_request(
                        parent,
                        &NativeConfigurationControlRequest {
                            expected_cohort: seed.serviced_cohort,
                            control_token: seed.control_token,
                            canonical_constant_values: &[-9.25, 0.75],
                            configuration_limit_bytes: 32,
                            control_buffer_grant_bytes: 64,
                        },
                    )
                    .expect("control body"),
                    7 => super::super::native_configuration_seed_wire::write_stage(
                        parent,
                        &NativeConfigurationSeedRequest {
                            ticket: seed,
                            configuration: &bytes,
                            seed_capacity_grant_bytes: 16,
                        },
                    )
                    .expect("stage body"),
                    8 => super::super::native_configuration_seed_wire::write_commit(parent, seed)
                        .expect("commit body"),
                    _ => {}
                }
                match decode(child, 2).expect("exact command plus its body") {
                    NativeCommand::Plain(observed) => assert_eq!(observed, command),
                    NativeCommand::Configuration(request) => {
                        assert_eq!(tag, 5);
                        let observed = request.borrow().expect("bounded decoded request");
                        assert_eq!(observed.expected_cohort, seed.serviced_cohort);
                        assert_eq!(observed.control_token, seed.control_token);
                        assert_eq!(observed.canonical_constant_values, &[-9.25, 0.75]);
                    }
                    NativeCommand::StageSeed(
                        super::super::native_configuration_seed_wire::DecodedStage::Owned(observed),
                    ) => {
                        assert_eq!(tag, 7);
                        assert_eq!(observed.ticket, seed);
                        assert_eq!(observed.configuration, bytes);
                    }
                    NativeCommand::CommitSeed(observed) => {
                        assert_eq!(tag, 8);
                        assert_eq!(observed, seed);
                    }
                    NativeCommand::StageSeed(_) => panic!("admitted body cannot become refusal"),
                    NativeCommand::BindObject(_) => panic!("old tag cannot decode as bind object"),
                    NativeCommand::AuthorStateEntry(_) => {
                        panic!("old command cannot become state-entry")
                    }
                    NativeCommand::EditorOpen(_) | NativeCommand::EditorClose(_) => {
                        panic!("old tag cannot decode as editor control")
                    }
                    NativeCommand::PollObservations(_) => {
                        panic!("old tag cannot decode as native observation")
                    }
                }
                WorkerResponse::Completed.write_to(child).expect("correlated sole reply");
                assert_eq!(
                    WorkerResponse::read_from(parent).expect("parent reply"),
                    WorkerResponse::Completed
                );
            }
        });
    }

    #[test]
    fn native_inline_decoder_refused_seed_consumes_body_before_next_exact_command() {
        use super::super::native_worker_command::{decode, NativeCommand};
        paired_socket(|parent, child| {
            let bytes = [1, 9, 3, 7];
            let seed = ticket(&bytes);
            header(7).write_to(parent).expect("stage command");
            super::super::native_configuration_seed_wire::write_stage(
                parent,
                &NativeConfigurationSeedRequest {
                    ticket: seed,
                    configuration: &bytes,
                    seed_capacity_grant_bytes: 1,
                },
            )
            .expect("bounded refused body");
            let next = WorkerCommand {
                kind: CommandKind::EnterState,
                start_sample: -93,
                frames: 0,
                event_count: 0,
                note_count: 0,
            };
            next.write_to(parent).expect("next command on same connection");
            assert!(matches!(
                decode(child, 0).expect("safe typed refusal"),
                NativeCommand::StageSeed(
                    super::super::native_configuration_seed_wire::DecodedStage::Refused(
                        NativeConfigurationSeedRefusal::BufferGrantExceeded
                    )
                )
            ));
            assert!(
                matches!(decode(child, 0).expect("no body bytes mistaken as command"), NativeCommand::Plain(observed) if observed == next)
            );
        });
    }

    #[test]
    fn native_inline_decoder_incomplete_body_is_failure_without_native_dispatch() {
        use super::super::native_worker_command::decode;
        paired_socket(|parent, child| {
            header(8).write_to(parent).expect("commit header");
            use std::io::Write;
            parent.write_all(&[1, 2, 3, 4]).expect("actual incomplete body");
            parent.shutdown(std::net::Shutdown::Write).expect("owner ends exact stream");
            let error = decode(child, 0).err().expect("incomplete control cannot be plain success");
            assert!(matches!(
                error,
                AudioProcessorHostError::WorkerFailed { operation: "seed commit request", .. }
            ));
        });
    }
}
