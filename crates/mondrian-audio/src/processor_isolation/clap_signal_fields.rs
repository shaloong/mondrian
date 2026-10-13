//! Borrowed signal storage of the same retained CLAP object.
//! Inline and scoped DSP paths use the one production parameter/PCM algorithm.

use super::clap_output_events::{ClapOutputEvents, ClapOutputOrigin};
use super::clap_parameter_events::ClapInputEvents;
use super::clap_worker::{clap_transport, invalid, ClapHost, ClapHostShared};
use super::native_configuration_control::NativeConfigurationPhase;
use super::IsolatedAudioProcessorWorkerBlock;
use crate::{AudioProcessorAuxiliaryInputContract, AudioProcessorHostError};
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, ClapId, InputChannel, Pckn,
    PluginAudioProcessor,
};

pub(super) struct ClapSignalFields<'a> {
    pub(super) configuration_phase: &'a NativeConfigurationPhase,
    pub(super) input: &'a mut Vec<Vec<Vec<f32>>>,
    pub(super) output: &'a mut Vec<Vec<f32>>,
    pub(super) auxiliary_inputs: &'a AudioProcessorAuxiliaryInputContract,
    pub(super) input_ports: &'a mut AudioPorts,
    pub(super) output_ports: &'a mut AudioPorts,
    pub(super) parameter_lanes: &'a [(ClapId, f64, f64, bool)],
    pub(super) input_events: &'a mut ClapInputEvents,
    pub(super) output_events: &'a mut ClapOutputEvents,
    pub(super) last_end: &'a mut Option<i64>,
    pub(super) shared: &'a ClapHostShared,
}

impl ClapSignalFields<'_> {
    /// The active CLAP parameter operation runs on this same exclusive DSP
    /// role. It cannot publish PCM, advance the transport or concurrent process.
    pub(super) fn flush_parameters(
        &mut self,
        native: &mut PluginAudioProcessor<ClapHost>,
        force: bool,
    ) -> Result<crate::NativeAudioAuthorParameterFlushOutcome, AudioProcessorHostError> {
        use crate::{
            NativeAudioAuthorParameterFlushOutcome as Outcome, NativeAudioAuthorRefusal as Refusal,
        };
        use clack_extensions::params::PluginParams;
        use std::sync::atomic::Ordering;
        if !force
            && !native
                .access_shared_handler(|host| host.services.flush_requested.load(Ordering::Acquire))
        {
            return Ok(Outcome::Flushed);
        }
        if self.configuration_phase.blocked_request().is_some() {
            // Preserve the request for its actual owner lifecycle service.
            return Ok(Outcome::Refused(Refusal::AwaitingAcceptance));
        }
        let _audio_role = self.shared.enter_audio_thread();
        let Some(params) = native.plugin_handle().get_extension::<PluginParams>() else {
            return Ok(Outcome::Refused(Refusal::UnsupportedAdapter));
        };
        // Begin publication before claiming the request; exhaustion/busy cannot
        // consume a request without its actual exclusive native operation.
        let mut output = self
            .output_events
            .writer(ClapOutputOrigin::ActiveFlush)
            .map_err(|_| AudioProcessorHostError::NativeParameterSourceUnavailable)?;
        let claimed = native.access_shared_handler(|host| {
            host.services.flush_requested.swap(false, Ordering::AcqRel)
        });
        if !force && !claimed {
            return Ok(Outcome::Flushed);
        }
        let mut handle = native.plugin_handle();
        let empty = clack_host::events::io::InputEvents::empty();
        params.flush_active(&mut handle, &empty, &mut output.as_output());
        // Requests made after the claim survive. The native void flush has now
        // completed; each output callback records its own success/refusal fact.
        Ok(Outcome::Flushed)
    }

    pub(super) fn process(
        &mut self,
        native: &mut PluginAudioProcessor<ClapHost>,
        mut block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        if let Some(request) = self.configuration_phase.blocked_request() {
            return Err(AudioProcessorHostError::ReprepareRequired(request));
        }
        let frames = block.frames();
        let end_sample = block.start_sample().checked_add(frames as i64).ok_or_else(|| {
            AudioProcessorHostError::Process("CLAP sample position overflowed".to_owned())
        })?;
        let channels = self.output.len();
        if *self.last_end != Some(block.start_sample())
            || frames == 0
            || frames > self.input[0][0].len()
            || block.main_interleaved().len() != frames * channels
        {
            return Err(AudioProcessorHostError::Process(
                "CLAP block violates prepared continuity or extent".to_owned(),
            ));
        }
        if block.parameter_lane_count() != self.parameter_lanes.len() {
            return Err(AudioProcessorHostError::Process(
                "CLAP parameter lane count changed during processing".to_owned(),
            ));
        }
        self.input_events.clear();
        for (lane, &(clap_id, min, max, stepped)) in self.parameter_lanes.iter().enumerate() {
            let events = block.parameter_events(lane).ok_or_else(|| {
                AudioProcessorHostError::Process("CLAP parameter lane is missing".to_owned())
            })?;
            for event in events {
                if event.sample_offset as usize >= frames
                    || !event.value.is_finite()
                    || event.value < min
                    || event.value > max
                    || (stepped && event.value.fract() != 0.0)
                {
                    return Err(AudioProcessorHostError::Process(
                        "CLAP parameter event violates its admitted contract".to_owned(),
                    ));
                }
                if !self.input_events.try_push(ParamValueEvent::new(
                    event.sample_offset,
                    clap_id,
                    Pckn::match_all(),
                    event.value,
                )) {
                    return Err(AudioProcessorHostError::Process(
                        "CLAP parameter events exceed the prepared storage".to_owned(),
                    ));
                }
            }
        }
        let note_port_count = block.note_input_contract().ports.len();
        let note_end_capacity = block.note_input_contract().limits.output_ends;
        for event in block.note_events() {
            if !self.input_events.try_push_note(event, frames, note_port_count) {
                return Err(AudioProcessorHostError::Process(
                    "CLAP Note events violate prepared capacity or input contract".to_owned(),
                ));
            }
        }
        self.input_events.sort();
        for (frame, samples) in block.main_interleaved().chunks_exact(channels).enumerate() {
            for (channel, sample) in samples.iter().copied().enumerate() {
                self.input[0][channel][frame] = sample;
            }
        }
        for (index, bus) in self.auxiliary_inputs.buses.iter().enumerate() {
            let samples = block.auxiliary_input(&bus.bus_key).ok_or_else(|| {
                AudioProcessorHostError::Process("CLAP auxiliary PCM is missing".to_owned())
            })?;
            if samples.len() != frames * channels {
                return Err(AudioProcessorHostError::Process(
                    "CLAP auxiliary extent changed".to_owned(),
                ));
            }
            for (frame, samples) in samples.chunks_exact(channels).enumerate() {
                for (channel, sample) in samples.iter().copied().enumerate() {
                    self.input[index + 1][channel][frame] = sample;
                }
            }
        }
        for channel in self.output.iter_mut() {
            channel[..frames].fill(0.0);
        }
        let transport = clap_transport(block.start_sample(), block.sample_rate())?;
        let output_origin = ClapOutputOrigin::Process {
            start_sample: block.start_sample(),
            sample_rate: block.sample_rate(),
            frames: u32::try_from(frames)
                .map_err(|_| invalid("CLAP output process extent exceeds u32"))?,
        };
        let input_audio = self.input_ports.with_input_buffers(self.input.iter_mut().map(|port| {
            AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    port.iter_mut().map(|channel| InputChannel::variable(&mut channel[..frames])),
                ),
            }
        }));
        let mut output_audio = self.output_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(
                self.output.iter_mut().map(|channel| &mut channel[..frames]),
            ),
        }]);
        {
            let _audio_role = self.shared.enter_audio_thread();
            let mut output = if note_port_count == 0 {
                self.output_events
                    .writer(output_origin)
                    .map_err(|_| AudioProcessorHostError::NativeParameterSourceUnavailable)?
            } else {
                self.output_events.writer_with_notes(
                    output_origin,
                    note_port_count,
                    note_end_capacity,
                )?
            };
            let processor = native.ensure_processing_started().map_err(|error| {
                AudioProcessorHostError::Process(format!("CLAP start failed: {error}"))
            })?;
            // Only this exclusive process/flush role may claim the request.
            // New requests during process remain set for a later actual service.
            let flush_claimed = processor.access_shared_handler(|host| {
                host.services.flush_requested.swap(false, std::sync::atomic::Ordering::AcqRel)
            });
            processor
                .process(
                    &input_audio,
                    &mut output_audio,
                    &self.input_events.as_input(),
                    &mut output.as_output(),
                    None,
                    Some(&transport),
                )
                .map_err(|error| {
                    if flush_claimed {
                        processor.access_shared_handler(|host| {
                            host.services
                                .flush_requested
                                .store(true, std::sync::atomic::Ordering::Release);
                        });
                    }
                    AudioProcessorHostError::Process(format!("CLAP process failed: {error}"))
                })?;
        }
        if note_port_count != 0 {
            if self.output_events.note_failed() {
                return Err(AudioProcessorHostError::Process(
                    "CLAP rejected a NoteEnd callback in the native operation".to_owned(),
                ));
            }
            for &end in self.output_events.note_ends() {
                if !block.try_push_note_end(end) {
                    return Err(AudioProcessorHostError::Process(
                        "CLAP NoteEnd observations exceed admitted IPC storage".to_owned(),
                    ));
                }
            }
        }
        for (frame, samples) in block.main_interleaved().chunks_exact_mut(channels).enumerate() {
            for (channel, sample) in samples.iter_mut().enumerate() {
                let value = self.output[channel][frame];
                if !value.is_finite() {
                    return Err(AudioProcessorHostError::Process(
                        "CLAP produced nonfinite PCM".to_owned(),
                    ));
                }
                *sample = value;
            }
        }
        *self.last_end = Some(end_sample);
        Ok(())
    }
}
