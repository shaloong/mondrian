use super::protocol::{
    auxiliary_from_wire, auxiliary_to_wire, read_json_frame, write_json_frame, CommandKind,
    PrepareWire, ReadyResultWire, ReadyWire, SharedBlockLayout, SharedParameterEvent,
    WorkerCommand, WorkerResponse, PROTOCOL_VERSION,
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
    let prepare: PrepareWire =
        read_json_frame(&mut control).map_err(|error| worker_failed("startup", error))?;
    if prepare.schema_version != PROTOCOL_VERSION || prepare.nonce != nonce {
        return Err(worker_failed_detail(
            "startup",
            "parent preparation envelope failed version or nonce validation",
        ));
    }
    let max_block_frames = usize::try_from(prepare.max_block_frames)
        .map_err(|_| invalid_contract("maximum block extent exceeds Worker address space"))?;
    let auxiliary_inputs = auxiliary_from_wire(prepare.auxiliary_buses.clone());
    auxiliary_inputs.validate(prepare.channel_layout)?;
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
    )?;
    if u64::try_from(layout.total_bytes).ok() != Some(prepare.shared_bytes) {
        return Err(invalid_contract(
            "parent and Worker computed different shared-storage extents",
        ));
    }
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
        payload: prepare.payload,
        plugin_execution_contract,
        auxiliary_inputs: auxiliary_inputs.clone(),
        parameter_ids: prepare.parameter_ids.clone(),
        render_contract,
    };
    let prepared = catch_unwind(AssertUnwindSafe(|| factory.prepare(request)));
    let mut processor = match prepared {
        Ok(Ok(processor)) => processor,
        Ok(Err(error)) => {
            write_ready_rejected(&mut control, error.to_string())?;
            return Ok(());
        }
        Err(_) => {
            write_ready_rejected(&mut control, "Worker factory panicked".to_owned())?;
            return Ok(());
        }
    };
    if processor.execution_contract() != plugin_execution_contract
        || processor.auxiliary_input_contract() != auxiliary_inputs
    {
        write_ready_rejected(
            &mut control,
            "Worker factory returned a contract different from its admitted request".to_owned(),
        )?;
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
    write_json_frame(
        &mut control,
        &ReadyWire {
            schema_version: PROTOCOL_VERSION,
            result: ReadyResultWire::Ready {
                execution_contract: prepare.execution_contract,
                auxiliary_buses: auxiliary_to_wire(&auxiliary_inputs),
            },
        },
    )
    .map_err(|error| worker_failed("startup", error))?;

    loop {
        let command = WorkerCommand::read_from(&mut control)
            .map_err(|error| worker_failed("command", error))?;
        match command.kind {
            CommandKind::Shutdown => {
                WorkerResponse::Completed
                    .write_to(&mut control)
                    .map_err(|error| worker_failed("shutdown", error))?;
                return Ok(());
            }
            CommandKind::EnterState => {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    processor.enter_state(command.start_sample)
                }));
                if !write_operation_result(&mut control, result)? {
                    return Ok(());
                }
            }
            CommandKind::Process => {
                let result = process_shared_block(
                    processor.as_mut(),
                    &mut shared,
                    layout,
                    command,
                    &auxiliary_inputs,
                    &prepare.parameter_ids,
                    render_contract,
                );
                if !write_operation_result(&mut control, Ok(result))? {
                    return Ok(());
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_shared_block(
    processor: &mut dyn IsolatedAudioProcessorWorker,
    shared: &mut [u8],
    layout: SharedBlockLayout,
    command: WorkerCommand,
    auxiliary_inputs: &crate::AudioProcessorAuxiliaryInputContract,
    parameter_ids: &[mondrian_core::ParameterId],
    render_contract: AudioRenderContract,
) -> Result<(), AudioProcessorHostError> {
    let frames = usize::try_from(command.frames)
        .map_err(|_| invalid_contract("block frame extent exceeds Worker address space"))?;
    let event_count = usize::try_from(command.event_count)
        .map_err(|_| invalid_contract("event count exceeds Worker address space"))?;
    if frames > render_contract.max_block_frames || event_count > layout.event_capacity {
        return Err(invalid_contract(
            "Worker command exceeds prepared shared capacity",
        ));
    }
    let samples = frames
        .checked_mul(render_contract.channel_count())
        .ok_or_else(|| invalid_contract("Worker block sample extent overflowed"))?;
    let auxiliary_samples = samples
        .checked_mul(auxiliary_inputs.buses.len())
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
    let main_end = layout
        .main_offset
        .checked_add(main_bytes)
        .ok_or_else(|| invalid_contract("Worker main range overflowed"))?;
    let auxiliary_end = layout
        .auxiliary_offset
        .checked_add(auxiliary_bytes)
        .ok_or_else(|| invalid_contract("Worker auxiliary range overflowed"))?;
    let event_end = layout
        .events_offset
        .checked_add(event_bytes)
        .ok_or_else(|| invalid_contract("Worker event range overflowed"))?;
    if main_end > shared.len() || auxiliary_end > shared.len() || event_end > shared.len() {
        return Err(invalid_contract(
            "Worker shared ranges exceed the mapped extent",
        ));
    }
    let (before_events, events_and_after) = shared.split_at_mut(layout.events_offset);
    let event_storage = events_and_after
        .get_mut(..event_bytes)
        .ok_or_else(|| invalid_contract("Worker event range is unavailable"))?;
    let events: &[SharedParameterEvent] = bytemuck::try_cast_slice(event_storage)
        .map_err(|_| invalid_contract("Worker event storage is not aligned"))?;
    if events.iter().any(|event| {
        usize::try_from(event.lane).map_or(true, |lane| lane >= parameter_ids.len())
            || usize::try_from(event.sample_offset).map_or(true, |offset| offset >= frames)
            || !event.value.is_finite()
    }) {
        return Err(invalid_contract(
            "Worker observed an invalid parameter event",
        ));
    }
    let (main_and_gap, auxiliary_and_gap) = before_events.split_at_mut(layout.auxiliary_offset);
    let main_storage = main_and_gap
        .get_mut(layout.main_offset..main_end)
        .ok_or_else(|| invalid_contract("Worker main range is unavailable"))?;
    let auxiliary_storage = auxiliary_and_gap
        .get_mut(..auxiliary_bytes)
        .ok_or_else(|| invalid_contract("Worker auxiliary range is unavailable"))?;
    let main: &mut [f32] = bytemuck::try_cast_slice_mut(main_storage)
        .map_err(|_| invalid_contract("Worker main storage is not aligned"))?;
    let auxiliary: &[f32] = bytemuck::try_cast_slice(auxiliary_storage)
        .map_err(|_| invalid_contract("Worker auxiliary storage is not aligned"))?;
    let block = IsolatedAudioProcessorWorkerBlock::new(
        command.start_sample,
        frames,
        render_contract,
        main,
        auxiliary_inputs,
        auxiliary,
        parameter_ids,
        events,
    );
    match catch_unwind(AssertUnwindSafe(|| processor.process(block))) {
        Ok(result) => result,
        Err(_) => Err(AudioProcessorHostError::AdapterPanicked(
            "isolated Worker block processing",
        )),
    }
}

fn write_operation_result(
    control: &mut TcpStream,
    result: Result<Result<(), AudioProcessorHostError>, Box<dyn std::any::Any + Send>>,
) -> Result<bool, AudioProcessorHostError> {
    let response = match result {
        Ok(Ok(())) => WorkerResponse::Completed,
        Ok(Err(error)) => WorkerResponse::Failed(error.to_string()),
        Err(_) => WorkerResponse::Failed("isolated Worker processor panicked".to_owned()),
    };
    let success = response == WorkerResponse::Completed;
    response.write_to(control).map_err(|error| worker_failed("response", error))?;
    Ok(success)
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
