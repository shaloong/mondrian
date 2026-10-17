use super::protocol::{
    auxiliary_from_wire, auxiliary_to_wire, read_prepare_frame, write_json_frame, CommandKind,
    ReadyResultWire, ReadyWire, SharedBlockLayout, SharedNoteEnd, SharedNoteEvent,
    SharedParameterEvent, WorkerCommand, WorkerResponse, PROTOCOL_VERSION,
};
use super::{
    IsolatedAudioProcessorWorker, IsolatedAudioProcessorWorkerBlock,
    IsolatedAudioProcessorWorkerFactory, IsolatedAudioProcessorWorkerPrepareRequest,
    ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV, ISOLATED_AUDIO_PROCESSOR_NONCE_ENV,
};
use crate::{AudioProcessingMode, AudioProcessorHostError, AudioRenderContract};
use memmap2::MmapOptions;
use std::fs::OpenOptions;
use std::net::TcpStream;
use std::panic::{catch_unwind, AssertUnwindSafe};

pub(super) fn run_worker(
    factory: &dyn IsolatedAudioProcessorWorkerFactory,
) -> Result<(), AudioProcessorHostError> {
    let endpoint = std::env::var(ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV)
        .map_err(|error| worker_failed("startup", error))?;
    let nonce = std::env::var(ISOLATED_AUDIO_PROCESSOR_NONCE_ENV)
        .map_err(|error| worker_failed("startup", error))?;
    let mut control =
        TcpStream::connect(&endpoint).map_err(|error| worker_failed("startup", error))?;
    control.set_nodelay(true).map_err(|error| worker_failed("startup", error))?;
    let (prepare, payload) = read_prepare_frame(&mut control, &nonce)
        .map_err(|error| worker_failed("startup", error))?;
    let native_lane = prepare.native_lane.clone().realize()?;
    let max_block_frames = usize::try_from(prepare.max_block_frames)
        .map_err(|_| invalid_contract("maximum block extent exceeds Worker address space"))?;
    let auxiliary_inputs = auxiliary_from_wire(prepare.auxiliary_buses.clone());
    auxiliary_inputs.validate(prepare.channel_layout)?;
    let auxiliary_outputs = super::protocol::outputs_from_wire(prepare.auxiliary_outputs.clone());
    auxiliary_outputs.validate(prepare.channel_layout)?;
    prepare.note_input_contract.validate()?;
    let plugin_execution_contract = prepare.execution_contract.realize()?;
    let render_contract = AudioRenderContract {
        sample_rate: prepare.sample_rate,
        channel_layout: prepare.channel_layout,
        max_block_frames,
        processing_mode: if prepare.realtime {
            AudioProcessingMode::Realtime
        } else {
            AudioProcessingMode::Offline
        },
        processor_session_scratch_budget_bytes: usize::MAX,
        public_output_lookahead_budget_frames: usize::MAX,
        compensation_delay_scratch_budget_bytes: usize::MAX,
    };
    let layout = SharedBlockLayout::new(
        max_block_frames,
        prepare.channel_layout.channel_count(),
        auxiliary_inputs.buses.len(),
        prepare.parameter_ids.len(),
    )?
    .with_auxiliary_outputs(auxiliary_outputs.buses.len())?
    .with_notes(
        prepare.note_input_contract.limits.input_events,
        prepare.note_input_contract.limits.output_ends,
    )?;
    if u64::try_from(layout.total_bytes).ok() != Some(prepare.shared_bytes) {
        return Err(invalid_contract(
            "parent and Worker computed different shared-storage extents",
        ));
    }
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
        musical_time_reference: prepare.musical_time_reference,
        native_lane,
        payload,
        plugin_execution_contract,
        auxiliary_inputs: auxiliary_inputs.clone(),
        auxiliary_outputs: auxiliary_outputs.clone(),
        note_input_contract: prepare.note_input_contract.clone(),
        parameter_ids: prepare.parameter_ids.clone(),
        render_contract,
    };
    let mut processor = match prepare_worker(factory, request) {
        Ok(processor) => processor,
        Err(detail) => {
            write_ready_rejected(&mut control, detail)?;
            return Ok(());
        }
    };
    if processor.native_worker_lane_resources() != native_lane
        || processor.uses_native_processing_split() != native_lane.is_some()
        || processor.execution_contract() != plugin_execution_contract
        || processor.auxiliary_input_contract() != auxiliary_inputs
        || processor.auxiliary_output_contract() != auxiliary_outputs
        || processor.note_input_contract() != prepare.note_input_contract
    {
        write_ready_rejected(
            &mut control,
            "Worker factory returned a contract different from its admitted request".to_owned(),
        )?;
        return Ok(());
    }

    let input_names = processor.auxiliary_input_names();
    if let Err(error) =
        crate::AudioProcessorInputName::validate_all(&input_names, &auxiliary_inputs)
    {
        write_ready_rejected(&mut control, error.to_string())?;
        return Ok(());
    }
    let shared_path = std::path::PathBuf::from(prepare.shared_path);
    let shared_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&shared_path)
        .map_err(|error| worker_failed("startup", error))?;
    let metadata_len =
        shared_file.metadata().map_err(|error| worker_failed("startup", error))?.len();
    if metadata_len != prepare.shared_bytes {
        return Err(invalid_contract(
            "shared-storage file extent changed before Worker mapping",
        ));
    }
    // SAFETY: the parent created and retains this exact fixed-size file. The
    // Worker validates the independently computed layout and file extent before
    // creating any typed view, and never resizes the mapping.
    let mut shared = unsafe { MmapOptions::new().len(layout.total_bytes).map_mut(&shared_file) }
        .map_err(|error| worker_failed("startup", error))?;
    let ready = ReadyWire {
        schema_version: PROTOCOL_VERSION,
        result: ReadyResultWire::Ready {
            musical_time_reference: prepare.musical_time_reference,
            execution_contract: prepare.execution_contract,
            auxiliary_buses: auxiliary_to_wire(&auxiliary_inputs),
            auxiliary_outputs: super::protocol::outputs_to_wire(&auxiliary_outputs),
            note_input_contract: prepare.note_input_contract.clone(),
            input_names,
        },
    };
    let context = WorkerProcessContext {
        musical_time_reference: prepare.musical_time_reference,
        layout,
        auxiliary_inputs: &auxiliary_inputs,
        auxiliary_outputs: &auxiliary_outputs,
        note_input_contract: &prepare.note_input_contract,
        parameter_ids: &prepare.parameter_ids,
        render_contract,
    };
    let result = if processor.uses_native_processing_split() {
        run_native_command_service(
            processor.as_mut(),
            &mut control,
            &mut shared,
            context,
            &ready,
        )
    } else {
        (|| {
            write_json_frame(&mut control, &ready)
                .map_err(|error| worker_failed("startup", error))?;
            loop {
                let command = super::native_worker_command::decode(
                    &mut control,
                    prepare.parameter_ids.len(),
                )?;
                if !dispatch_native_owner_command(
                    processor.as_mut(),
                    command,
                    &mut control,
                    &mut shared,
                    context,
                )? {
                    break Ok(());
                }
            }
        })()
    };
    // No native module may unwind/drop while its actual view remains attached.
    let closed = catch_unwind(AssertUnwindSafe(|| processor.close_native_editor_owner()))
        .unwrap_or(Err(AudioProcessorHostError::AdapterPanicked(
            "native editor owner removal",
        )));
    if let Err(error) = closed {
        let _ = WorkerResponse::Failed(error.to_string()).write_to(&mut control);
        std::process::exit(1);
    }
    result
}

/// The single admitted shared-frame interpretation, reused by inline and
/// a future native scoped consumer. It carries borrowed schedule facts, no native
/// object or alternate author/runtime owner.
#[derive(Clone, Copy)]
pub(super) struct WorkerProcessContext<'a> {
    pub(super) musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    pub(super) layout: SharedBlockLayout,
    pub(super) auxiliary_inputs: &'a crate::AudioProcessorAuxiliaryInputContract,
    pub(super) auxiliary_outputs: &'a crate::AudioProcessorAuxiliaryOutputContract,
    pub(super) note_input_contract: &'a crate::AudioProcessorNoteInputContract,
    pub(super) parameter_ids: &'a [mondrian_core::ParameterId],
    pub(super) render_contract: AudioRenderContract,
}

/// Successful actual Process completion carried through the existing DSP slot.
/// None retains parameter-only response semantics; Some includes zero Ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkerProcessCompletion {
    pub(super) note_end_count: Option<u32>,
}

impl WorkerProcessContext<'_> {
    pub(super) fn process(
        &self,
        mut process: impl FnMut(
            IsolatedAudioProcessorWorkerBlock<'_>,
        ) -> Result<(), AudioProcessorHostError>,
        shared: &mut [u8],
        command: WorkerCommand,
    ) -> Result<WorkerProcessCompletion, AudioProcessorHostError> {
        if command.kind != CommandKind::Process {
            return Err(invalid_contract("non-Process command reached shared PCM"));
        }
        let frames = usize::try_from(command.frames)
            .map_err(|_| invalid_contract("block frame extent exceeds Worker address space"))?;
        let event_count = usize::try_from(command.event_count)
            .map_err(|_| invalid_contract("event count exceeds Worker address space"))?;
        let note_count = usize::try_from(command.note_count)
            .map_err(|_| invalid_contract("Note count exceeds Worker address space"))?;
        if frames > self.render_contract.max_block_frames
            || event_count > self.layout.event_capacity
            || note_count > self.layout.note_event_capacity
            || note_count > crate::AUDIO_NOTE_EVENTS_PER_BLOCK
            || self.layout.note_event_capacity != self.note_input_contract.limits.input_events
            || self.layout.note_end_capacity != self.note_input_contract.limits.output_ends
        {
            return Err(invalid_contract(
                "Worker command exceeds prepared shared capacity",
            ));
        }
        if self.layout.total_bytes != shared.len() {
            return Err(invalid_contract(
                "Worker mapped extent differs from its prepared layout",
            ));
        }
        let samples = frames
            .checked_mul(self.render_contract.channel_count())
            .ok_or_else(|| invalid_contract("Worker block sample extent overflowed"))?;
        let auxiliary_samples = samples
            .checked_mul(self.auxiliary_inputs.buses.len())
            .ok_or_else(|| invalid_contract("Worker auxiliary sample extent overflowed"))?;
        let main_bytes = samples
            .checked_mul(size_of::<f32>())
            .ok_or_else(|| invalid_contract("Worker main-bus bytes overflowed"))?;
        let auxiliary_bytes = auxiliary_samples
            .checked_mul(size_of::<f32>())
            .ok_or_else(|| invalid_contract("Worker auxiliary bytes overflowed"))?;
        let event_bytes = event_count
            .checked_mul(size_of::<SharedParameterEvent>())
            .ok_or_else(|| invalid_contract("Worker event bytes overflowed"))?;
        let main_end = self
            .layout
            .main_offset
            .checked_add(main_bytes)
            .ok_or_else(|| invalid_contract("Worker main range overflowed"))?;
        let auxiliary_end = self
            .layout
            .auxiliary_offset
            .checked_add(auxiliary_bytes)
            .ok_or_else(|| invalid_contract("Worker auxiliary range overflowed"))?;
        let event_end = self
            .layout
            .events_offset
            .checked_add(event_bytes)
            .ok_or_else(|| invalid_contract("Worker event range overflowed"))?;
        if main_end > shared.len() || auxiliary_end > shared.len() || event_end > shared.len() {
            return Err(invalid_contract(
                "Worker shared ranges exceed the mapped extent",
            ));
        }
        let note_bytes = note_count
            .checked_mul(size_of::<SharedNoteEvent>())
            .ok_or_else(|| invalid_contract("Worker Note input bytes overflowed"))?;
        let end_bytes = self
            .layout
            .note_end_capacity
            .checked_mul(size_of::<SharedNoteEnd>())
            .ok_or_else(|| invalid_contract("Worker Note End bytes overflowed"))?;
        let note_end = self
            .layout
            .note_events_offset
            .checked_add(note_bytes)
            .ok_or_else(|| invalid_contract("Worker Note input range overflowed"))?;
        let end_end = self
            .layout
            .note_ends_offset
            .checked_add(end_bytes)
            .ok_or_else(|| invalid_contract("Worker Note End range overflowed"))?;
        if note_end > self.layout.note_ends_offset
            || end_end > shared.len()
            || self.layout.note_events_offset < event_end
            || self.layout.note_ends_offset < self.layout.note_events_offset
        {
            return Err(invalid_contract(
                "Worker Note ranges exceed the mapped extent",
            ));
        }
        let (before_ends, ends_and_after) = shared.split_at_mut(self.layout.note_ends_offset);
        let ends: &mut [SharedNoteEnd] = bytemuck::try_cast_slice_mut(
            ends_and_after
                .get_mut(..end_bytes)
                .ok_or_else(|| invalid_contract("Worker Note End range is unavailable"))?,
        )
        .map_err(|_| invalid_contract("Worker Note End storage is not aligned"))?;
        let (before_notes, notes_and_gap) =
            before_ends.split_at_mut(self.layout.note_events_offset);
        let notes: &[SharedNoteEvent] = bytemuck::try_cast_slice(
            notes_and_gap
                .get(..note_bytes)
                .ok_or_else(|| invalid_contract("Worker Note input range is unavailable"))?,
        )
        .map_err(|_| invalid_contract("Worker Note input storage is not aligned"))?;
        let mut previous_note = None;
        for note in notes {
            let event = note.realize(frames, self.note_input_contract.ports.len())?;
            if !self.note_input_contract.accepts_event(event, frames) {
                return Err(invalid_contract(
                    "Worker Note channel exceeds admitted input channels",
                ));
            }
            let order = (note.sample_offset, note.kind);
            if previous_note.is_some_and(|previous| previous > order) {
                return Err(invalid_contract("Worker observed unordered input Notes"));
            }
            previous_note = Some(order);
        }
        let (before_events, events_and_after) =
            before_notes.split_at_mut(self.layout.events_offset);
        let event_storage = events_and_after
            .get_mut(..event_bytes)
            .ok_or_else(|| invalid_contract("Worker event range is unavailable"))?;
        let events: &[SharedParameterEvent] = bytemuck::try_cast_slice(event_storage)
            .map_err(|_| invalid_contract("Worker event storage is not aligned"))?;
        if events.iter().any(|event| {
            usize::try_from(event.lane).map_or(true, |lane| lane >= self.parameter_ids.len())
                || usize::try_from(event.sample_offset).map_or(true, |offset| offset >= frames)
                || !event.value.is_finite()
        }) {
            return Err(invalid_contract(
                "Worker observed an invalid parameter event",
            ));
        }
        let (inputs_and_gap, outputs_and_gap) =
            before_events.split_at_mut(self.layout.auxiliary_outputs_offset);
        let output_bytes = samples
            .checked_mul(self.auxiliary_outputs.buses.len())
            .and_then(|samples| samples.checked_mul(size_of::<f32>()))
            .ok_or_else(|| invalid_contract("Worker output extent overflowed"))?;
        let output_storage = outputs_and_gap
            .get_mut(..output_bytes)
            .ok_or_else(|| invalid_contract("Worker output range is unavailable"))?;
        let output: &mut [f32] = bytemuck::try_cast_slice_mut(output_storage)
            .map_err(|_| invalid_contract("Worker output storage is not aligned"))?;
        output.fill(0.0);
        let (main_and_gap, auxiliary_and_gap) =
            inputs_and_gap.split_at_mut(self.layout.auxiliary_offset);
        let main_storage = main_and_gap
            .get_mut(self.layout.main_offset..main_end)
            .ok_or_else(|| invalid_contract("Worker main range is unavailable"))?;
        let auxiliary_storage = auxiliary_and_gap
            .get_mut(..auxiliary_bytes)
            .ok_or_else(|| invalid_contract("Worker auxiliary range is unavailable"))?;
        let main: &mut [f32] = bytemuck::try_cast_slice_mut(main_storage)
            .map_err(|_| invalid_contract("Worker main storage is not aligned"))?;
        let auxiliary: &[f32] = bytemuck::try_cast_slice(auxiliary_storage)
            .map_err(|_| invalid_contract("Worker auxiliary storage is not aligned"))?;
        let note_end_count = std::cell::Cell::new(0_usize);
        let note_output_refused = std::cell::Cell::new(false);
        let block = IsolatedAudioProcessorWorkerBlock::new(
            command.start_sample,
            frames,
            self.render_contract,
            main,
            self.auxiliary_inputs,
            auxiliary,
            self.parameter_ids,
            events,
        )
        .with_musical_time_reference(self.musical_time_reference)
        .with_auxiliary_outputs(self.auxiliary_outputs, output)
        .with_note_regions(
            self.note_input_contract,
            notes,
            ends,
            &note_end_count,
            &note_output_refused,
        );
        let result = catch_unwind(AssertUnwindSafe(|| process(block)));
        // A native adapter cannot hide output refusal by ignoring false or by
        // returning Ok/ReprepareRequired later in the same actual operation.
        if note_output_refused.get() {
            return Err(invalid_contract(
                "Worker Note End output was refused during native processing",
            ));
        }
        match result {
            Ok(result) => result?,
            Err(_) => {
                return Err(AudioProcessorHostError::AdapterPanicked(
                    "isolated Worker block processing",
                ))
            }
        }
        let count = u32::try_from(note_end_count.get())
            .map_err(|_| invalid_contract("Worker Note End count is not protocol-representable"))?;
        Ok(WorkerProcessCompletion {
            note_end_count: if self.note_input_contract.ports.is_empty() {
                None
            } else {
                Some(count)
            },
        })
    }
}

fn write_process_operation_result(
    control: &mut TcpStream,
    result: Result<WorkerProcessCompletion, AudioProcessorHostError>,
    pending: Option<crate::NativeObservedRequest>,
) -> Result<bool, AudioProcessorHostError> {
    let response = match result {
        Ok(WorkerProcessCompletion { note_end_count: Some(note_end_count) }) => {
            WorkerResponse::NotesCompleted { note_end_count, pending }
        }
        Ok(WorkerProcessCompletion { note_end_count: None }) => pending.map_or(
            WorkerResponse::Completed,
            WorkerResponse::CompletedWithPending,
        ),
        Err(AudioProcessorHostError::ReprepareRequired(request)) => {
            WorkerResponse::DeferredForConfiguration(request)
        }
        Err(error) => WorkerResponse::Failed(error.to_string()),
    };
    let success = !matches!(response, WorkerResponse::Failed(_));
    response.write_to(control).map_err(|error| worker_failed("response", error))?;
    Ok(success)
}

fn write_operation_result(
    control: &mut TcpStream,
    result: Result<Result<(), AudioProcessorHostError>, Box<dyn std::any::Any + Send>>,
    pending: Option<crate::NativeObservedRequest>,
) -> Result<bool, AudioProcessorHostError> {
    let response = match result {
        Ok(Ok(())) => pending.map_or(
            WorkerResponse::Completed,
            WorkerResponse::CompletedWithPending,
        ),
        Ok(Err(AudioProcessorHostError::ReprepareRequired(request))) => {
            WorkerResponse::DeferredForConfiguration(request)
        }
        Ok(Err(error)) => WorkerResponse::Failed(error.to_string()),
        Err(_) => WorkerResponse::Failed("isolated Worker processor panicked".to_owned()),
    };
    let success = !matches!(response, WorkerResponse::Failed(_));
    response.write_to(control).map_err(|error| worker_failed("response", error))?;
    Ok(success)
}

fn prepare_worker(
    factory: &dyn IsolatedAudioProcessorWorkerFactory,
    request: IsolatedAudioProcessorWorkerPrepareRequest,
) -> Result<Box<dyn IsolatedAudioProcessorWorker>, String> {
    let native_lane = request.native_worker_lane_resources();
    let requested_lane = factory
        .native_worker_lane_resources_for_preparation(&request)
        .map_err(|error| error.to_string())?;
    if native_lane != requested_lane {
        return Err(
            "parent and child preparation-specific native storage requests differ".to_owned(),
        );
    }
    let processor = catch_unwind(AssertUnwindSafe(|| factory.prepare(request)))
        .map_err(|_| "Worker factory panicked".to_owned())?
        .map_err(|error| error.to_string())?;
    if let Some(profile) = native_lane {
        let layout = std::alloc::Layout::for_value(processor.as_ref());
        profile
            .admit_adapter_resident(crate::NativeHostBufferLayout {
                bytes: layout.size(),
                alignment: layout.align(),
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(processor)
}

fn write_ready_rejected(
    control: &mut TcpStream,
    detail: String,
) -> Result<(), AudioProcessorHostError> {
    write_json_frame(
        control,
        &ReadyWire {
            schema_version: PROTOCOL_VERSION,
            result: ReadyResultWire::Rejected { detail },
        },
    )
    .map_err(|error| worker_failed("startup", error))
}

fn invalid_contract(detail: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.to_owned())
}

fn worker_failed(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> AudioProcessorHostError {
    worker_failed_detail(operation, &error.to_string())
}

fn worker_failed_detail(operation: &'static str, detail: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::WorkerFailed { operation, detail: detail.to_owned() }
}

fn write_native_object_observation_result(
    control: &mut TcpStream,
    result: Result<
        Result<crate::NativeAudioObservations, AudioProcessorHostError>,
        Box<dyn std::any::Any + Send>,
    >,
) -> Result<bool, AudioProcessorHostError> {
    use super::native_audio_object_wire::{write_reply, ObjectReply};
    let success = matches!(&result, Ok(Ok(_)));
    match result {
        Ok(Ok(mut observations)) => {
            observations.owner_process_id = std::process::id();
            write_reply(control, ObjectReply::Observation(&observations))
        }
        Ok(Err(error)) => write_reply(control, ObjectReply::Failed(error.to_string())),
        Err(_) => write_reply(
            control,
            ObjectReply::Failed("native observation panicked".into()),
        ),
    }
    .map_err(|error| worker_failed("native observation reply", error))?;
    Ok(success)
}

/// The sole lifecycle/control dispatcher of this authenticated native owner.
/// Inline workers run it directly. A future scoped lane must end and join its
/// borrowed DSP role before using this same dispatcher for native mutation.
fn dispatch_native_owner_command(
    processor: &mut dyn IsolatedAudioProcessorWorker,
    command: super::native_worker_command::NativeCommand,
    control: &mut TcpStream,
    shared: &mut [u8],
    context: WorkerProcessContext<'_>,
) -> Result<bool, AudioProcessorHostError> {
    use super::native_worker_command::NativeCommand;
    let input_layout = command.owned_input_layout()?;
    admit_native_control_storage(processor, input_layout, 0)?;
    match command {
        NativeCommand::Plain(command) => {
            if command.kind == CommandKind::PollConfigurationCohort {
                write_native_observation_reply(
                    control,
                    command.kind,
                    processor.native_configuration_cohort(),
                    None,
                )?;
                return Ok(true);
            }
            if command.kind == CommandKind::PollPending {
                write_native_observation_reply(
                    control,
                    command.kind,
                    None,
                    processor.pending_reprepare(),
                )?;
                return Ok(true);
            }
            match command.kind {
                CommandKind::Shutdown => {
                    WorkerResponse::Completed
                        .write_to(control)
                        .map_err(|error| worker_failed("shutdown", error))?;
                    Ok(false)
                }
                CommandKind::EnterState => {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        processor.enter_state(command.start_sample)
                    }));
                    write_operation_result(control, result, processor.pending_reprepare())
                }
                CommandKind::Process => {
                    let result = context.process(|block| processor.process(block), shared, command);
                    write_process_operation_result(control, result, processor.pending_reprepare())
                }
                CommandKind::PollPending
                | CommandKind::PollConfigurationCohort
                | CommandKind::ServiceConfiguration
                | CommandKind::StageConfigurationSeed
                | CommandKind::CommitConfigurationSeed
                | CommandKind::PollNativeObservations
                | CommandKind::BindNativeObject
                | CommandKind::EditorOpen
                | CommandKind::EditorClose
                | CommandKind::PrepareAuthorStateEntry => Err(invalid_contract(
                    "decoded control kind does not match its sole payload variant",
                )),
            }
        }
        NativeCommand::PollObservations(consumption) => {
            let result = catch_unwind(AssertUnwindSafe(|| {
                let mut observations = processor.poll_native_observations(consumption)?;
                observations.pending_reprepare = processor.pending_reprepare();
                Ok(observations)
            }));
            write_native_object_observation_result(control, result)
        }
        NativeCommand::BindObject(request) => {
            let result = catch_unwind(AssertUnwindSafe(|| {
                processor.bind_native_audio_object(request)
            }));
            let reply = match result {
                Ok(Ok(outcome)) => super::native_audio_object_wire::ObjectReply::Binding(outcome),
                Ok(Err(error)) => {
                    super::native_audio_object_wire::ObjectReply::Failed(error.to_string())
                }
                Err(_) => super::native_audio_object_wire::ObjectReply::Failed(
                    "native object binding panicked".into(),
                ),
            };
            let success = !matches!(
                reply,
                super::native_audio_object_wire::ObjectReply::Failed(_)
            );
            super::native_audio_object_wire::write_reply(control, reply)
                .map_err(|error| worker_failed("native bind reply", error))?;
            Ok(success)
        }
        NativeCommand::EditorOpen(request) => write_editor_operation_result(
            control,
            catch_unwind(AssertUnwindSafe(|| {
                processor.open_native_audio_editor(request)
            })),
        ),
        NativeCommand::EditorClose(binding) => write_editor_operation_result(
            control,
            catch_unwind(AssertUnwindSafe(|| {
                processor.close_native_audio_editor(binding)
            })),
        ),
        NativeCommand::AuthorStateEntry(request) => {
            let outcome = match catch_unwind(AssertUnwindSafe(|| {
                processor.prepare_native_author_state_entry(request.borrow())
            })) {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(error)) => {
                    super::native_audio_author_state_entry_wire::write_failed_reply(control)
                        .map_err(|error| worker_failed("native state-entry failed reply", error))?;
                    return Err(error);
                }
                Err(_) => {
                    super::native_audio_author_state_entry_wire::write_failed_reply(control)
                        .map_err(|error| worker_failed("native state-entry failed reply", error))?;
                    return Err(worker_failed(
                        "native state-entry preparation",
                        "native adapter panicked",
                    ));
                }
            };
            super::native_audio_author_state_entry_wire::write_reply(control, outcome)
                .map_err(|error| worker_failed("native state-entry reply", error))?;
            Ok(true)
        }
        NativeCommand::Configuration(request) => {
            use super::native_configuration_wire::{self as wire, ControlReply};
            let value_bytes = request
                .value_bytes()
                .map_err(|error| worker_failed("configuration request capacity", error))?;
            let result = catch_unwind(AssertUnwindSafe(|| {
                processor.service_native_configuration(
                    request
                        .borrow()
                        .map_err(|error| worker_failed("configuration request grant", error))?,
                )
            }));
            let reply = match result {
                Ok(Ok(mut outcome)) => {
                    if let super::NativeConfigurationControlOutcome::ReadyForInstall(candidate) =
                        &mut outcome
                    {
                        candidate.buffers.request_value_capacity_bytes = value_bytes;
                        admit_native_control_storage(
                            processor,
                            input_layout,
                            candidate.configuration.capacity(),
                        )?;
                    }
                    ControlReply::Outcome(outcome)
                }
                Ok(Err(error)) => ControlReply::Failed(error.to_string()),
                Err(_) => ControlReply::Failed("native configuration control panicked".into()),
            };
            let success = !matches!(reply, ControlReply::Failed(_));
            wire::write_reply(control, reply)
                .map_err(|error| worker_failed("configuration reply", error))?;
            Ok(success)
        }
        NativeCommand::StageSeed(stage) => {
            use super::native_configuration_seed_wire::DecodedStage;
            let result = match stage {
                DecodedStage::Owned(seed) => catch_unwind(AssertUnwindSafe(|| {
                    processor.stage_native_configuration_seed(seed)
                })),
                DecodedStage::Refused(refusal) => {
                    Ok(Ok(super::NativeConfigurationSeedOutcome::Refused(refusal)))
                }
            };
            admit_native_control_storage(
                processor,
                crate::NativeHostBufferLayout { bytes: 0, alignment: 1 },
                0,
            )?;
            write_seed_operation_result(control, result)
        }
        NativeCommand::CommitSeed(ticket) => {
            let result = catch_unwind(AssertUnwindSafe(|| {
                processor.commit_native_configuration_seed(ticket)
            }));
            write_seed_operation_result(control, result)
        }
    }
}

/// Admit actual simultaneous host-owned payloads against the one known lane request.
/// Native/private plugin memory and error Strings remain separate declared frontiers.
fn admit_native_control_storage(
    processor: &dyn IsolatedAudioProcessorWorker,
    decoded_input: crate::NativeHostBufferLayout,
    reply_bytes: usize,
) -> Result<(), AudioProcessorHostError> {
    let Some(profile) = processor.native_worker_lane_resources() else {
        return Ok(());
    };
    profile
        .admit_observed_lane_owners(
            profile.owning_layouts(),
            crate::NativeWorkerControlOverlapLayout {
                decoded_input,
                reply_storage: crate::NativeHostBufferLayout {
                    bytes: reply_bytes,
                    alignment: align_of::<u8>(),
                },
                staged_seed: processor.native_staged_seed_layout(),
            },
        )
        .map_err(AudioProcessorHostError::from)
}

/// Pure observations use the same reply encoder in inline and scoped lanes.
/// Pending data never installs configuration or truncates an admitted root
/// audio window; the sole parent Runtime chooses its next pre-DSP boundary.
fn write_native_observation_reply(
    control: &mut TcpStream,
    kind: CommandKind,
    cohort: Option<super::NativeRequestCohort>,
    pending: Option<crate::NativeObservedRequest>,
) -> Result<bool, AudioProcessorHostError> {
    match kind {
        CommandKind::PollConfigurationCohort => {
            super::native_configuration_wire::write_cohort_reply(control, cohort)
                .map_err(|error| worker_failed("configuration cohort query", error))?;
        }
        CommandKind::PollPending => {
            pending
                .map_or(
                    WorkerResponse::Completed,
                    WorkerResponse::CompletedWithPending,
                )
                .write_to(control)
                .map_err(|error| worker_failed("pending query", error))?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn write_editor_operation_result(
    control: &mut TcpStream,
    result: Result<
        Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError>,
        Box<dyn std::any::Any + Send>,
    >,
) -> Result<bool, AudioProcessorHostError> {
    use super::native_audio_editor_wire::{self as wire, EditorReply};
    let reply = match result {
        Ok(Ok(outcome)) => EditorReply::Outcome(outcome),
        Ok(Err(error)) => EditorReply::Failed(error.to_string()),
        Err(_) => EditorReply::Failed("native editor control panicked".into()),
    };
    let success = !matches!(reply, EditorReply::Failed(_));
    wire::write_reply(control, reply)
        .map_err(|error| worker_failed("native editor reply", error))?;
    Ok(success)
}

fn write_seed_operation_result(
    control: &mut TcpStream,
    result: Result<
        Result<super::NativeConfigurationSeedOutcome, AudioProcessorHostError>,
        Box<dyn std::any::Any + Send>,
    >,
) -> Result<bool, AudioProcessorHostError> {
    use super::native_configuration_seed_wire::{self as wire, SeedReply};
    let reply = match result {
        Ok(Ok(outcome)) => SeedReply::Outcome(outcome),
        Ok(Err(error)) => SeedReply::Failed(error.to_string()),
        Err(_) => SeedReply::Failed("native configuration seed control panicked".into()),
    };
    let success = !matches!(reply, SeedReply::Failed(_));
    wire::write_reply(control, reply)
        .map_err(|error| worker_failed("seed control reply", error))?;
    Ok(success)
}

/// Keep the one admitted DSP scope alive while main services the same native
/// owner and this existing connection. A lifecycle command is returned intact;
/// the caller joins the scope before dispatching it, then acknowledges only
/// after its correlated control response has actually been written.
enum NativeScopeExit {
    OwnerCommand(super::native_worker_command::NativeCommand),
    UserCloseRequested,
    Completed,
}

fn service_native_main_scope(
    main: &mut dyn super::IsolatedAudioProcessorMainRole,
    dsp: Option<&super::native_worker_processing::NativeDspDispatch<'_>>,
    ingress: &super::native_worker_ingress::NativeCommandIngress<'_>,
    control: &mut TcpStream,
) -> Result<NativeScopeExit, AudioProcessorHostError> {
    use super::native_worker_command::NativeCommand;
    loop {
        main.service_native_main_thread()?;
        if main.native_editor_close_requested()? {
            return Ok(NativeScopeExit::UserCloseRequested);
        }
        let Some(command) = ingress.receive()? else {
            continue;
        };
        if let NativeCommand::Plain(header) = &command {
            if write_native_observation_reply(
                control,
                header.kind,
                main.native_configuration_cohort()?,
                main.pending_reprepare()?,
            )? {
                ingress.acknowledge()?;
                continue;
            }
            if header.kind == CommandKind::Process {
                let result = if let Some(dsp) = dsp {
                    dsp.submit(*header)?;
                    dsp.complete(main)
                } else {
                    Err(main.pending_reprepare()?.map_or_else(
                        || {
                            AudioProcessorHostError::StateEntry(
                                "stopped native owner requires explicit state entry".into(),
                            )
                        },
                        AudioProcessorHostError::ReprepareRequired,
                    ))
                };
                if !write_process_operation_result(control, result, main.pending_reprepare()?)? {
                    // No further native mutation or command is admitted after a
                    // failed DSP response. Scope teardown still explicitly joins.
                    ingress.acknowledge()?;
                    return Ok(NativeScopeExit::Completed);
                }
                ingress.acknowledge()?;
                continue;
            }
        }
        if matches!(
            &command,
            NativeCommand::PollObservations(crate::NativeAudioObservationConsumption::Preserve)
        ) {
            let result = catch_unwind(AssertUnwindSafe(|| {
                // The main role serviced prior serial control writes before
                // this command. No DSP join, flag retirement or log drain occurs.
                let mut observations = main.preserve_native_observations()?;
                observations.pending_reprepare = main.pending_reprepare()?;
                Ok(observations)
            }));
            let success = write_native_object_observation_result(control, result)?;
            ingress.acknowledge()?;
            if !success {
                return Ok(NativeScopeExit::Completed);
            }
            continue;
        }
        if let NativeCommand::PollObservations(
            crate::NativeAudioObservationConsumption::DrainAuthor(binding),
        ) = &command
            && main.native_author_binding_matches(*binding)?
            && let Some(dsp) = dsp
        {
            // The sole slot settles this exact object's control DSP operation
            // before its borrowed processor joins and main captures state.
            // Source refusal stays in the actual source and full-owner reply;
            // it never fabricates PCM or a successful author delivery.
            dsp.submit_author_flush()?;
            let _source_outcome = dsp.complete_author_flush(main)?;
        }
        return Ok(NativeScopeExit::OwnerCommand(command));
    }
}

fn admit_native_main_role(
    profile: crate::NativeWorkerLaneResourceProfile,
    main: &dyn super::IsolatedAudioProcessorMainRole,
) -> Result<(), AudioProcessorHostError> {
    let [runtime, sdk] = main.allocation_layouts().ok_or_else(|| {
        invalid_contract("native control role cannot report its complete Box layouts")
    })?;
    let prepared = profile.owning_layouts();
    if runtime != prepared.runtime_main_role || sdk != prepared.sdk_main_role {
        return Err(invalid_contract(
            "native control main-role allocation layout changed",
        ));
    }
    // The original lane grant retains its conservative DSP/slot reservation.
    // Only these two main allocations exist in this borrow; no DSP/slot owner
    // or thread is manufactured to satisfy an allocation observation.
    Ok(())
}

fn run_native_command_service(
    processor: &mut dyn IsolatedAudioProcessorWorker,
    control: &mut TcpStream,
    shared: &mut [u8],
    context: WorkerProcessContext<'_>,
    ready: &ReadyWire,
) -> Result<(), AudioProcessorHostError> {
    use super::native_worker_ingress::with_native_ingress;
    use super::native_worker_processing::with_processing_scope;
    let profile = processor.native_worker_lane_resources().ok_or_else(|| {
        invalid_contract("scoped native adapter lacks its admitted owning-layout request")
    })?;
    with_native_ingress(
        control,
        context.parameter_ids.len(),
        Some(profile),
        |control, ingress| {
            let mut ready_sent = false;
            loop {
                if processor.native_editor_detach_stage().is_some() {
                    // Canonical attachment failure admits control-only recovery.
                    // Do not dispatch its still-live HWND or start a DSP borrow.
                    // A typed state-entry retry performs the same exact Close
                    // before any restore. Keep its named reply codec and peer
                    // cohort intact while frame-clear cleanup is still pending.
                    let Some(command) = ingress.receive()? else {
                        continue;
                    };
                    let cleanup_control = matches!(
                        &command,
                        super::native_worker_command::NativeCommand::EditorClose(_)
                            | super::native_worker_command::NativeCommand::AuthorStateEntry(_)
                            | super::native_worker_command::NativeCommand::PollObservations(_)
                            | super::native_worker_command::NativeCommand::Plain(WorkerCommand {
                                kind: CommandKind::Shutdown,
                                ..
                            })
                    );
                    if !cleanup_control {
                        WorkerResponse::Failed("native attachment cleanup admits only source observation, Close, state-entry retry or Shutdown".to_owned())
                        .write_to(control).map_err(|error| worker_failed("native cleanup Process refusal", error))?;
                        ingress.acknowledge()?;
                        return Ok(());
                    }
                    let success = dispatch_native_owner_command(
                        processor, command, control, shared, context,
                    )?;
                    ingress.acknowledge()?;
                    if !success {
                        return Ok(());
                    }
                    continue;
                }
                let control_exit = {
                    let role = processor.native_control_only_role()?;
                    if let Some(mut main) = role {
                        if !ready_sent {
                            write_ready_rejected(
                                control,
                                "native owner is control-only before initial DSP readiness".into(),
                            )?;
                            return Ok(());
                        }
                        admit_native_main_role(profile, &*main)?;
                        Some(service_native_main_scope(
                            &mut *main, None, ingress, control,
                        )?)
                    } else {
                        None
                    }
                };
                let exit = if let Some(exit) = control_exit {
                    exit
                } else {
                    let split = processor.split_native_processing()?;
                    let outcome = with_processing_scope(
                        split,
                        profile,
                        shared,
                        context,
                        |main, dsp| {
                            match (context.render_contract.processing_mode, dsp.scheduling_entry()) {
                    (AudioProcessingMode::Offline, None) => {}
                    (AudioProcessingMode::Realtime, Some(evidence))
                        if evidence.role() == mondrian_audio_scheduling::RealtimeAudioThreadRole::NativeDsp => {}
                    _ => return Err(invalid_contract("native DSP scope readiness does not match its actual processing role")),
                }
                            if !ready_sent {
                                // The parent may not observe Ready until this actual DSP
                                // thread acquired its required scheduling role. No caller
                                // manufactures native entry from the main thread's mode.
                                write_json_frame(control, ready).map_err(|error| {
                                    worker_failed("native scope startup", error)
                                })?;
                                ready_sent = true;
                            }
                            service_native_main_scope(main, Some(dsp), ingress, control)
                        },
                    )?;
                    // Actual explicit scheduling restoration is retained in `outcome`.
                    // A later readiness/exit wire contract must carry these facts to
                    // the parent before claiming its native RT qualification.
                    let _scheduling_exit = outcome.scheduling_exit;
                    outcome.value
                };
                match exit {
                    NativeScopeExit::Completed => return Ok(()),
                    NativeScopeExit::UserCloseRequested => {
                        // This is an observed native window event, not a second
                        // command queue or a synthetic author End gesture. The
                        // same actual view/parent are removed only after DSP join.
                        match processor.service_native_editor_close_request()? {
                            crate::NativeAudioEditorOutcome::Closed { .. }
                            | crate::NativeAudioEditorOutcome::ClosePending {
                                stage: crate::NativeAudioEditorCloseStage::ClearHostFrame,
                                ..
                            } => {}
                            _ => return Err(invalid_contract(
                                "native Close source intent did not settle this actual attachment",
                            )),
                        }
                    }
                    NativeScopeExit::OwnerCommand(command) => {
                        let success = dispatch_native_owner_command(
                            processor, command, control, shared, context,
                        )?;
                        ingress.acknowledge()?;
                        if !success {
                            return Ok(());
                        }
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod native_control_scope_tests;

#[cfg(test)]
mod native_resident_owner_tests;

#[cfg(test)]
mod inline_dispatch_contracts {
    use super::super::native_configuration_control::{
        NativeConfigurationControlRequest, NativeRequestCohort,
    };
    use super::super::native_configuration_seed::{
        NativeConfigurationSeedRequest, NativeConfigurationSeedTicket,
    };
    use super::super::{NativeConfigurationFormat, NativeConfigurationSeedRefusal};
    use super::*;
    use sha2::{Digest, Sha256};
    use std::net::TcpListener;
    use std::num::NonZeroU64;
    use std::time::Duration;

    #[derive(Default)]
    struct RecordingWorker {
        entered: Vec<i64>,
        processed: Vec<i64>,
        staged: usize,
        committed: usize,
    }

    impl IsolatedAudioProcessorWorker for RecordingWorker {
        fn execution_contract(&self) -> crate::AudioProcessorExecutionContract {
            crate::AudioProcessorExecutionContract::new(
                0,
                crate::AudioProcessorTail::None,
                true,
                true,
                true,
                0,
            )
            .expect("contract")
        }
        fn auxiliary_input_contract(&self) -> crate::AudioProcessorAuxiliaryInputContract {
            crate::AudioProcessorAuxiliaryInputContract::default()
        }
        fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError> {
            self.entered.push(start_sample);
            Ok(())
        }
        fn process(
            &mut self,
            mut block: IsolatedAudioProcessorWorkerBlock<'_>,
        ) -> Result<(), AudioProcessorHostError> {
            self.processed.push(block.start_sample());
            block.main_interleaved().fill(0.375);
            Ok(())
        }
        fn stage_native_configuration_seed(
            &mut self,
            _: super::super::NativeConfigurationOwnedSeed,
        ) -> Result<super::super::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
            self.staged += 1;
            Ok(super::super::NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::UnsupportedAdapter,
            ))
        }
        fn commit_native_configuration_seed(
            &mut self,
            _: NativeConfigurationSeedTicket,
        ) -> Result<super::super::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
            self.committed += 1;
            Ok(super::super::NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::UnsupportedAdapter,
            ))
        }
    }

    #[test]
    fn native_inline_dispatch_correlates_real_socket_controls_and_preserves_same_processor() {
        use super::super::native_configuration_seed_wire::{read_reply, SeedReply};
        use super::super::native_worker_command::decode;
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener");
        let mut parent = TcpStream::connect(listener.local_addr().expect("bound address"))
            .expect("one control connection");
        let (mut child, _) = listener.accept().expect("accepted control connection");
        for socket in [&parent, &child] {
            socket.set_read_timeout(Some(Duration::from_secs(2))).expect("test deadline");
        }
        let contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: mondrian_core::AudioChannelLayout::Mono,
            max_block_frames: 2,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 0,
            public_output_lookahead_budget_frames: 0,
            compensation_delay_scratch_budget_bytes: 0,
        };
        let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
        let note_input_contract = crate::AudioProcessorNoteInputContract::default();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).expect("exact shared extent");
        let context = WorkerProcessContext {
            musical_time_reference: None,
            auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
            layout,
            auxiliary_inputs: &auxiliary,
            note_input_contract: &note_input_contract,
            parameter_ids: &[],
            render_contract: contract,
        };
        let mut storage = vec![0_u64; layout.total_bytes.div_ceil(size_of::<u64>())];
        let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut storage);
        let mut worker = RecordingWorker::default();
        for (kind, start_sample, frames) in [
            (CommandKind::EnterState, -11, 0),
            (CommandKind::Process, -11, 2),
            (CommandKind::PollPending, 0, 0),
        ] {
            WorkerCommand {
                kind,
                start_sample,
                frames,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut parent)
            .expect("real request");
            let command = decode(&mut child, 0).expect("single decoder");
            assert!(dispatch_native_owner_command(
                &mut worker,
                command,
                &mut child,
                shared,
                context
            )
            .expect("inline consumer"));
            assert_eq!(
                WorkerResponse::read_from(&mut parent).expect("exact correlated response"),
                WorkerResponse::Completed
            );
        }
        assert_eq!(worker.entered, [-11]);
        assert_eq!(worker.processed, [-11]);
        assert_eq!(
            bytemuck::cast_slice::<u8, f32>(&shared[..8]),
            &[0.375, 0.375]
        );
        WorkerCommand {
            kind: CommandKind::PollConfigurationCohort,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut parent)
        .expect("pure query");
        let command = decode(&mut child, 0).expect("pure query decoder");
        assert!(
            dispatch_native_owner_command(&mut worker, command, &mut child, shared, context)
                .expect("pure query consumer")
        );
        assert_eq!(
            super::super::native_configuration_wire::read_cohort_reply(&mut parent)
                .expect("same query codec"),
            None
        );

        let bytes = [7, 3, 1];
        let ticket = NativeConfigurationSeedTicket {
            serviced_cohort: NativeRequestCohort::new(
                1,
                mondrian_core::NativeAudioReprepareRequest::ClapRestart,
            )
            .expect("cohort"),
            control_token: NonZeroU64::new(17).expect("token"),
            observed_contract: worker.execution_contract(),
            format: NativeConfigurationFormat::ClapState,
            configuration_bytes: bytes.len(),
            configuration_sha256: Sha256::digest(bytes).into(),
        };
        let control = NativeConfigurationControlRequest {
            expected_cohort: ticket.serviced_cohort,
            control_token: ticket.control_token,
            canonical_constant_values: &[],
            configuration_limit_bytes: 32,
            control_buffer_grant_bytes: 64,
        };
        WorkerCommand {
            kind: CommandKind::ServiceConfiguration,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut parent)
        .expect("control request header");
        super::super::native_configuration_wire::write_request(&mut parent, &control)
            .expect("control request body");
        let command = decode(&mut child, 0).expect("owned exact control");
        assert!(
            dispatch_native_owner_command(&mut worker, command, &mut child, shared, context)
                .expect("safe capability refusal keeps owner")
        );
        assert!(matches!(
            super::super::native_configuration_wire::read_reply(&mut parent, &control)
                .expect("control correlated reply"),
            super::super::native_configuration_wire::ControlReply::Outcome(
                super::super::NativeConfigurationControlOutcome::CapabilityRefused(
                    super::super::NativeConfigurationRefusal::UnsupportedAdapter
                )
            )
        ));
        WorkerCommand {
            kind: CommandKind::StageConfigurationSeed,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut parent)
        .expect("stage header");
        super::super::native_configuration_seed_wire::write_stage(
            &mut parent,
            &NativeConfigurationSeedRequest {
                ticket,
                configuration: &bytes,
                seed_capacity_grant_bytes: 1,
            },
        )
        .expect("below-size grant remains a typed resource refusal");
        let command = decode(&mut child, 0).expect("consumed refused stage");
        assert!(
            dispatch_native_owner_command(&mut worker, command, &mut child, shared, context)
                .expect("refusal retains native worker")
        );
        assert!(matches!(
            read_reply(&mut parent, ticket, 1).expect("exact refusal reply"),
            SeedReply::Outcome(super::super::NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::BufferGrantExceeded
            ))
        ));
        assert_eq!(
            worker.staged, 0,
            "decoder refusal cannot dispatch a phantom native stage"
        );
        WorkerCommand {
            kind: CommandKind::CommitConfigurationSeed,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut parent)
        .expect("next exact commit header");
        super::super::native_configuration_seed_wire::write_commit(&mut parent, ticket)
            .expect("next exact commit body");
        let command = decode(&mut child, 0).expect("framing survives consumed refusal");
        assert!(
            dispatch_native_owner_command(&mut worker, command, &mut child, shared, context)
                .expect("safe unsupported commit")
        );
        assert!(matches!(
            read_reply(&mut parent, ticket, 32).expect("commit correlated reply"),
            SeedReply::Outcome(super::super::NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::UnsupportedAdapter
            ))
        ));
        assert_eq!(worker.committed, 1);
        WorkerCommand {
            kind: CommandKind::Shutdown,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut parent)
        .expect("shutdown request");
        let command = decode(&mut child, 0).expect("shutdown decoder");
        assert!(
            !dispatch_native_owner_command(&mut worker, command, &mut child, shared, context)
                .expect("matched shutdown closes sole loop")
        );
        assert_eq!(
            WorkerResponse::read_from(&mut parent)
                .expect("shutdown completed before owner release"),
            WorkerResponse::Completed
        );
        assert_eq!(worker.entered, [-11]);
        assert_eq!(worker.processed, [-11]);
    }
}

#[cfg(test)]
mod note_process_contracts {
    use super::*;
    use crate::{
        AudioNoteEndEvent, AudioNoteEvent, AudioNoteEventKind, AudioNoteExecutionLimits,
        AudioProcessorNoteInputContract, AudioProcessorNoteInputPort,
    };

    fn render() -> AudioRenderContract {
        AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: mondrian_core::AudioChannelLayout::Mono,
            max_block_frames: 2,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 0,
            public_output_lookahead_budget_frames: 0,
            compensation_delay_scratch_budget_bytes: 0,
        }
    }

    fn notes(input: usize, output: usize) -> AudioProcessorNoteInputContract {
        let contract = AudioProcessorNoteInputContract {
            completion: crate::AudioNoteCompletionPolicy::NativeEnd,
            ports: vec![AudioProcessorNoteInputPort {
                channel_count: 16,
                port_key: "main-notes".to_owned(),
                native_id: 27,
                native_index: 0,
            }],
            limits: AudioNoteExecutionLimits {
                input_events: input,
                output_ends: output,
                retained_records: 2,
                live_voices: 2,
            },
        };
        contract.validate().unwrap();
        contract
    }

    fn command(count: u32) -> WorkerCommand {
        WorkerCommand {
            kind: CommandKind::Process,
            start_sample: -3,
            frames: 2,
            event_count: 0,
            note_count: count,
        }
    }

    fn packed() -> SharedNoteEvent {
        SharedNoteEvent::capture(
            AudioNoteEvent {
                sample_offset: 0,
                kind: AudioNoteEventKind::On,
                port_index: 0,
                channel: 4,
                key: 60,
                native_note_id: 7,
                velocity: -0.0,
            },
            2,
            1,
        )
        .unwrap()
    }

    fn put_notes(shared: &mut [u8], layout: SharedBlockLayout, events: &[SharedNoteEvent]) {
        let end = layout.note_events_offset + std::mem::size_of_val(events);
        let target: &mut [SharedNoteEvent] =
            bytemuck::cast_slice_mut(&mut shared[layout.note_events_offset..end]);
        target.copy_from_slice(events);
    }

    #[test]
    fn worker_notes_preserve_exact_input_bits_and_copy_end_after_actual_return() {
        let contract = notes(2, 2);
        let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap().with_notes(2, 2).unwrap();
        let context = WorkerProcessContext {
            musical_time_reference: None,
            auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
            layout,
            auxiliary_inputs: &auxiliary,
            note_input_contract: &contract,
            parameter_ids: &[],
            render_contract: render(),
        };
        let mut storage = vec![0_u64; layout.total_bytes / 8];
        let shared = bytemuck::cast_slice_mut(&mut storage);
        let on = packed();
        let off = SharedNoteEvent {
            kind: 0,
            velocity: f64::from_bits(0x3fd3_3333_3333_3335),
            ..on
        };
        put_notes(shared, layout, &[off, on]);
        let ended = AudioNoteEndEvent {
            native_note_id: 7,
            port_index: -1,
            channel: -1,
            key: -1,
            flags: 3,
        };
        let mut calls = 0;
        let result = context
            .process(
                |mut block| {
                    calls += 1;
                    assert_eq!(block.note_input_contract(), &contract);
                    let events: Vec<_> = block.note_events().collect();
                    assert_eq!(events.len(), 2);
                    assert_eq!(events[0].kind, AudioNoteEventKind::Off);
                    assert_eq!(events[0].velocity.to_bits(), 0x3fd3_3333_3333_3335);
                    assert_eq!(events[1].kind, AudioNoteEventKind::On);
                    assert_eq!(events[1].velocity.to_bits(), (-0.0_f64).to_bits());
                    assert_eq!(
                        (
                            events[1].port_index,
                            events[1].channel,
                            events[1].key,
                            events[1].native_note_id
                        ),
                        (0, 4, 60, 7)
                    );
                    assert!(block.try_push_note_end(ended));
                    block.main_interleaved().fill(0.75);
                    Ok(())
                },
                shared,
                command(2),
            )
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result, WorkerProcessCompletion { note_end_count: Some(1) });
        let copied: &[SharedNoteEnd] =
            bytemuck::cast_slice(&shared[layout.note_ends_offset..layout.note_ends_offset + 16]);
        assert_eq!(copied[0].reserved, 0);
        assert_eq!(copied[0].realize(1).unwrap(), ended);
        let main: &[f32] = bytemuck::cast_slice(&shared[..8]);
        assert_eq!(main, &[0.75, 0.75]);
    }

    #[test]
    fn admitted_channel_width_and_zero_end_contract_guard_the_actual_process_context() {
        let mut contract = notes(2, 1);
        contract.completion = crate::AudioNoteCompletionPolicy::SuccessfulOffBlock;
        contract.ports[0].channel_count = 4;
        contract.limits.output_ends = 0;
        contract.limits.live_voices = 0;
        contract.validate().unwrap();
        let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
        let outputs = crate::AudioProcessorAuxiliaryOutputContract::default();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap().with_notes(2, 0).unwrap();
        let context = WorkerProcessContext {
            musical_time_reference: None,
            auxiliary_outputs: &outputs,
            layout,
            auxiliary_inputs: &auxiliary,
            note_input_contract: &contract,
            parameter_ids: &[],
            render_contract: render(),
        };
        let mut storage = vec![0_u64; layout.total_bytes / 8];
        let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut storage);
        bytemuck::cast_slice_mut::<u8, f32>(&mut shared[..8]).copy_from_slice(&[5.0, 6.0]);
        put_notes(shared, layout, &[packed()]); // channel 4 is scalar-valid but not admitted.
        let mut calls = 0;
        assert!(context
            .process(
                |_| {
                    calls += 1;
                    Ok(())
                },
                shared,
                command(1)
            )
            .is_err());
        assert_eq!(calls, 0);
        assert_eq!(bytemuck::cast_slice::<u8, f32>(&shared[..8]), &[5.0, 6.0]);
        put_notes(
            shared,
            layout,
            &[SharedNoteEvent { channel: 3, ..packed() }],
        );
        assert_eq!(
            context
                .process(
                    |block| {
                        calls += 1;
                        assert_eq!(block.note_events().next().unwrap().channel, 3);
                        Ok(())
                    },
                    shared,
                    command(1)
                )
                .unwrap(),
            WorkerProcessCompletion { note_end_count: Some(0) }
        );
        assert_eq!(calls, 1);
        assert!(context
            .process(
                |mut block| {
                    calls += 1;
                    assert!(!block.try_push_note_end(AudioNoteEndEvent {
                        native_note_id: 7,
                        port_index: 0,
                        channel: 3,
                        key: 60,
                        flags: 0,
                    }));
                    Ok(()) // Ignoring the false callback cannot produce success/Off credit.
                },
                shared,
                command(1)
            )
            .is_err());
        assert_eq!(calls, 2);
    }

    #[test]
    fn invalid_input_notes_never_call_processor_or_change_pcm() {
        let contract = notes(2, 1);
        let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap().with_notes(2, 1).unwrap();
        let context = WorkerProcessContext {
            musical_time_reference: None,
            auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
            layout,
            auxiliary_inputs: &auxiliary,
            note_input_contract: &contract,
            parameter_ids: &[],
            render_contract: render(),
        };
        let mut storage = vec![0_u64; layout.total_bytes / 8];
        let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut storage);
        let canonical = packed();
        let invalid = [
            SharedNoteEvent { kind: 2, ..canonical },
            SharedNoteEvent { sample_offset: 2, ..canonical },
            SharedNoteEvent { native_id: -1, ..canonical },
            SharedNoteEvent { port_index: -1, ..canonical },
            SharedNoteEvent { channel: 16, ..canonical },
            SharedNoteEvent { key: 128, ..canonical },
            SharedNoteEvent { velocity: f64::NAN, ..canonical },
        ];
        for event in invalid {
            bytemuck::cast_slice_mut::<u8, f32>(&mut shared[..8]).copy_from_slice(&[5.0, 6.0]);
            put_notes(shared, layout, &[event]);
            let mut calls = 0;
            assert!(context
                .process(
                    |_| {
                        calls += 1;
                        Ok(())
                    },
                    shared,
                    command(1)
                )
                .is_err());
            assert_eq!(calls, 0);
            assert_eq!(bytemuck::cast_slice::<u8, f32>(&shared[..8]), &[5.0, 6.0]);
        }
        put_notes(
            shared,
            layout,
            &[canonical, SharedNoteEvent { kind: 0, ..canonical }],
        );
        let mut calls = 0;
        assert!(context
            .process(
                |_| {
                    calls += 1;
                    Ok(())
                },
                shared,
                command(2)
            )
            .is_err());
        assert!(context
            .process(
                |_| {
                    calls += 1;
                    Ok(())
                },
                shared,
                command(3)
            )
            .is_err());
        assert_eq!(calls, 0);
        assert_eq!(bytemuck::cast_slice::<u8, f32>(&shared[..8]), &[5.0, 6.0]);
    }

    #[test]
    fn ignored_note_end_overflow_or_invalid_push_remains_hard_failure() {
        let contract = notes(1, 1);
        let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap().with_notes(1, 1).unwrap();
        let context = WorkerProcessContext {
            musical_time_reference: None,
            auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
            layout,
            auxiliary_inputs: &auxiliary,
            note_input_contract: &contract,
            parameter_ids: &[],
            render_contract: render(),
        };
        let mut storage = vec![0_u64; layout.total_bytes / 8];
        let shared: &mut [u8] = bytemuck::cast_slice_mut(&mut storage);
        let end = AudioNoteEndEvent {
            native_note_id: 7,
            port_index: 0,
            channel: 4,
            key: 60,
            flags: 0,
        };
        let mut calls = 0;
        assert!(context
            .process(
                |mut block| {
                    calls += 1;
                    assert!(block.try_push_note_end(end));
                    assert!(!block.try_push_note_end(end));
                    assert!(!block.try_push_note_end(end)); // refusal cannot be cleared
                    Ok(()) // hostile/buggy adapter ignores the failed callback
                },
                shared,
                command(0)
            )
            .is_err());
        assert_eq!(calls, 1);
        assert!(context
            .process(
                |mut block| {
                    assert!(
                        !block.try_push_note_end(AudioNoteEndEvent { native_note_id: -1, ..end })
                    );
                    assert!(!block.try_push_note_end(end));
                    Err(AudioProcessorHostError::ReprepareRequired(
                        crate::NativeObservedRequest::RequestOnly(
                            mondrian_core::NativeAudioReprepareRequest::ClapRestart,
                        ),
                    ))
                },
                shared,
                command(0)
            )
            .is_err_and(|error| !matches!(error, AudioProcessorHostError::ReprepareRequired(_))));
        // A subsequent actual operation starts with its own zero count/latch;
        // this pure Context is not a running/poisoned parent processor instance.
        assert_eq!(
            context
                .process(
                    |mut block| {
                        assert!(block.try_push_note_end(end));
                        Ok(())
                    },
                    shared,
                    command(0)
                )
                .unwrap()
                .note_end_count,
            Some(1)
        );
    }

    #[test]
    fn note_region_extent_or_alignment_refuses_before_native_call() {
        let contract = notes(1, 1);
        let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap().with_notes(1, 1).unwrap();
        let context = WorkerProcessContext {
            musical_time_reference: None,
            auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
            layout,
            auxiliary_inputs: &auxiliary,
            note_input_contract: &contract,
            parameter_ids: &[],
            render_contract: render(),
        };
        let mut storage = vec![0_u64; layout.total_bytes / 8 + 1];
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut storage);
        let mut calls = 0;
        assert!(context
            .process(
                |_| {
                    calls += 1;
                    Ok(())
                },
                &mut bytes[..layout.total_bytes - 1],
                command(0)
            )
            .is_err());
        let misaligned = &mut bytes[1..layout.total_bytes + 1];
        let before = misaligned.to_vec();
        assert!(context
            .process(
                |_| {
                    calls += 1;
                    Ok(())
                },
                misaligned,
                command(0)
            )
            .is_err());
        assert_eq!(calls, 0);
        assert_eq!(misaligned, before.as_slice());
    }

    #[test]
    fn empty_note_contract_keeps_legacy_completion_and_rejects_any_note_count() {
        let contract = AudioProcessorNoteInputContract::default();
        let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap().with_notes(0, 0).unwrap();
        assert_eq!(layout.total_bytes, 8);
        let context = WorkerProcessContext {
            musical_time_reference: None,
            auxiliary_outputs: &crate::AudioProcessorAuxiliaryOutputContract::default(),
            layout,
            auxiliary_inputs: &auxiliary,
            note_input_contract: &contract,
            parameter_ids: &[],
            render_contract: render(),
        };
        let mut storage = vec![0_u64; 1];
        let shared = bytemuck::cast_slice_mut(&mut storage);
        assert_eq!(
            context
                .process(
                    |mut block| {
                        assert!(block.note_input_contract().ports.is_empty());
                        assert_eq!(block.note_events().count(), 0);
                        block.main_interleaved().fill(0.375);
                        Ok(())
                    },
                    shared,
                    command(0)
                )
                .unwrap(),
            WorkerProcessCompletion { note_end_count: None }
        );
        assert_eq!(
            bytemuck::cast_slice::<u8, f32>(&shared[..8]),
            &[0.375, 0.375]
        );
        let mut calls = 0;
        assert!(context
            .process(
                |_| {
                    calls += 1;
                    Ok(())
                },
                shared,
                command(1)
            )
            .is_err());
        assert_eq!(calls, 0);
        assert!(context
            .process(
                |mut block| {
                    assert!(!block.try_push_note_end(AudioNoteEndEvent {
                        native_note_id: 0,
                        port_index: -1,
                        channel: -1,
                        key: -1,
                        flags: 0
                    }));
                    Ok(())
                },
                shared,
                command(0)
            )
            .is_err());
    }
}
