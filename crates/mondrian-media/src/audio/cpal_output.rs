//! Portable CPAL physical-output Adapter.

use super::{RealtimeAudioCallbackControl, RealtimeAudioOutputTelemetry};
use crate::audio_device::{
    RealtimeAudioOutputContract, RealtimeAudioOutputOpenFailure, RealtimeAudioOutputOpenFailureCode,
};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use crossbeam_queue::ArrayQueue;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

pub(super) fn build_and_start(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    queue: Arc<ArrayQueue<f32>>,
    callback_control: Arc<RealtimeAudioCallbackControl>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    contract: RealtimeAudioOutputContract,
) -> Result<cpal::Stream, RealtimeAudioOutputOpenFailure> {
    let telemetry_for_error = Arc::clone(&telemetry);
    let err_fn = move |_error| {
        telemetry_for_error.stream_failed.store(true, Ordering::Release);
    };
    let build = |error: cpal::Error| {
        RealtimeAudioOutputOpenFailure::after_selection(
            RealtimeAudioOutputOpenFailureCode::StreamBuildFailed,
            contract,
            error.to_string(),
        )
    };
    let stream = match sample_format {
        cpal::SampleFormat::I8 => build_sample_stream::<i8>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::I16 => build_sample_stream::<i16>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::I32 => build_sample_stream::<i32>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::I64 => build_sample_stream::<i64>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::U8 => build_sample_stream::<u8>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::U16 => build_sample_stream::<u16>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::U32 => build_sample_stream::<u32>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::U64 => build_sample_stream::<u64>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::F32 => build_sample_stream::<f32>(
            device,
            config,
            Arc::clone(&queue),
            Arc::clone(&callback_control),
            Arc::clone(&telemetry),
            err_fn,
        )
        .map_err(build)?,
        cpal::SampleFormat::F64 => {
            build_sample_stream::<f64>(device, config, queue, callback_control, telemetry, err_fn)
                .map_err(build)?
        }
        _ => {
            return Err(RealtimeAudioOutputOpenFailure::after_selection(
                RealtimeAudioOutputOpenFailureCode::SampleFormatUnsupported,
                contract,
                "selected CPAL sample format has no callback implementation",
            ));
        }
    };
    stream.play().map_err(|error| {
        RealtimeAudioOutputOpenFailure::after_selection(
            RealtimeAudioOutputOpenFailureCode::StreamStartFailed,
            contract,
            error.to_string(),
        )
    })?;
    Ok(stream)
}

fn build_sample_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<ArrayQueue<f32>>,
    callback_control: Arc<RealtimeAudioCallbackControl>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    device.build_output_stream(
        *config,
        move |data: &mut [T], info| {
            let frames = data.len() / channels;
            let playback_delay = callback_playback_delay(info);
            let active_block = callback_control.begin_callback_block(&telemetry);
            if !active_block {
                data.fill(T::EQUILIBRIUM);
                telemetry.record_callback(false, frames, 0, playback_delay);
                callback_control.finish_callback_block(false, &telemetry);
                return;
            }
            let mut missing_samples = 0usize;
            for sample in data {
                let value = queue
                    .pop()
                    .unwrap_or_else(|| {
                        missing_samples = missing_samples.saturating_add(1);
                        0.0
                    })
                    .clamp(-1.0, 1.0);
                *sample = T::from_sample(value);
            }
            telemetry.record_callback(true, frames, missing_samples / channels, playback_delay);
            callback_control.finish_callback_block(true, &telemetry);
        },
        err_fn,
        None,
    )
}

fn callback_playback_delay(info: &cpal::OutputCallbackInfo) -> Duration {
    let timestamp = info.timestamp();
    timestamp.playback.duration_since(timestamp.callback)
}
