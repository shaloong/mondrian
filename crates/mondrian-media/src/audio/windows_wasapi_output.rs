//! Event-driven WASAPI output for exact named-speaker layouts.
//!
//! CPAL's WASAPI backend deliberately opens `WAVEFORMATEXTENSIBLE` streams
//! with `KSAUDIO_SPEAKER_DIRECTOUT`. That is correct for ordinal channels but
//! cannot carry Mondrian's named speaker semantics. This narrow Adapter owns
//! only the Windows stream opening and render-buffer pump; queue authority,
//! callback activation, and transport evidence remain in the parent module.

use super::{render_f32_output_block, RealtimeAudioCallbackControl, RealtimeAudioOutputTelemetry};
use crossbeam_queue::ArrayQueue;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{sync_channel, SyncSender},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::{Audio, KernelStreaming, Multimedia};
use windows::Win32::System::{
    Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED},
    Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
        WaitForSingleObject, INFINITE,
    },
};

const SHARED_BUFFER_DURATION_100NS: i64 = 1_000_000;

/// One running event-driven WASAPI stream whose format carries an exact mask.
pub(super) struct WindowsWasapiNamedOutputStream {
    stop: Arc<AtomicBool>,
    event_raw: usize,
    thread: Option<JoinHandle<()>>,
}

enum Startup {
    Ready { event_raw: usize },
    Failed(WindowsWasapiNamedOutputOpenError),
}

pub(super) enum WindowsWasapiNamedOutputOpenError {
    Build(String),
    Start(String),
}

impl WindowsWasapiNamedOutputOpenError {
    pub(super) fn into_stage_and_detail(self) -> (bool, String) {
        match self {
            Self::Build(detail) => (false, detail),
            Self::Start(detail) => (true, detail),
        }
    }
}

impl From<String> for WindowsWasapiNamedOutputOpenError {
    fn from(detail: String) -> Self {
        Self::Build(detail)
    }
}

struct RenderContext {
    endpoint_id: String,
    sample_rate: u32,
    channels: usize,
    channel_mask: u32,
    queue: Arc<ArrayQueue<f32>>,
    callback_control: Arc<RealtimeAudioCallbackControl>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    stop: Arc<AtomicBool>,
}

impl WindowsWasapiNamedOutputStream {
    pub(super) fn start(
        endpoint_id: String,
        sample_rate: u32,
        channels: usize,
        channel_mask: u32,
        queue: Arc<ArrayQueue<f32>>,
        callback_control: Arc<RealtimeAudioCallbackControl>,
        telemetry: Arc<RealtimeAudioOutputTelemetry>,
    ) -> Result<Self, WindowsWasapiNamedOutputOpenError> {
        let stop = Arc::new(AtomicBool::new(false));
        let context = RenderContext {
            endpoint_id,
            sample_rate,
            channels,
            channel_mask,
            queue,
            callback_control,
            telemetry,
            stop: Arc::clone(&stop),
        };
        let (startup_tx, startup_rx) = sync_channel(1);
        let thread = thread::Builder::new()
            .name("mondrian-wasapi-output".to_owned())
            .spawn(move || run_stream(context, startup_tx))
            .map_err(|error| {
                WindowsWasapiNamedOutputOpenError::Build(format!(
                    "failed to spawn WASAPI render thread: {error}"
                ))
            })?;

        match startup_rx.recv() {
            Ok(Startup::Ready { event_raw }) => Ok(Self { stop, event_raw, thread: Some(thread) }),
            Ok(Startup::Failed(detail)) => {
                let _ = thread.join();
                Err(detail)
            }
            Err(error) => {
                let _ = thread.join();
                Err(WindowsWasapiNamedOutputOpenError::Build(format!(
                    "WASAPI render thread ended before publishing startup evidence: {error}"
                )))
            }
        }
    }
}

impl Drop for WindowsWasapiNamedOutputStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let event = HANDLE(self.event_raw as *mut core::ffi::c_void);
        // SAFETY: `event_raw` is published only after CreateEventW succeeds and
        // remains owned by this object until the render thread has joined.
        unsafe {
            let _ = SetEvent(event);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: the render thread no longer waits on or accesses the event.
        unsafe {
            let _ = CloseHandle(event);
        }
    }
}

fn run_stream(context: RenderContext, startup_tx: SyncSender<Startup>) {
    if let Err(error) = run_stream_inner(&context, &startup_tx) {
        context.telemetry.stream_failed.store(true, Ordering::Release);
        let _ = startup_tx.try_send(Startup::Failed(error));
    }
}

fn run_stream_inner(
    context: &RenderContext,
    startup_tx: &SyncSender<Startup>,
) -> Result<(), WindowsWasapiNamedOutputOpenError> {
    // SAFETY: this dedicated worker initializes and uninitializes its own COM apartment.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|error| format!("failed to initialize WASAPI COM apartment: {error}"))?;
    }
    let _com = ComApartment;

    let mut task_index = 0_u32;
    // SAFETY: the static wide string is null terminated and the returned handle
    // is reverted on this same thread after the stream stops.
    let mmcss = unsafe {
        AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task_index)
            .map_err(|error| format!("failed to enter the Pro Audio MMCSS class: {error}"))?
    };
    let _mmcss = MmcssRegistration(mmcss);

    // SAFETY: COM is initialized above and every interface remains on this worker.
    let enumerator: Audio::IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&Audio::MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|error| format!("failed to create MMDeviceEnumerator: {error}"))?
    };
    let endpoint_id = HSTRING::from(context.endpoint_id.as_str());
    // SAFETY: `endpoint_id` remains alive for the duration of the call.
    let endpoint = unsafe {
        enumerator
            .GetDevice(PCWSTR(endpoint_id.as_ptr()))
            .map_err(|error| format!("selected WASAPI endpoint is unavailable: {error}"))?
    };
    // SAFETY: the selected endpoint was obtained from the render-device catalog.
    let audio_client: Audio::IAudioClient = unsafe {
        endpoint
            .Activate(CLSCTX_ALL, None)
            .map_err(|error| format!("failed to activate selected WASAPI endpoint: {error}"))?
    };

    let format = named_float_format(context.sample_rate, context.channels, context.channel_mask)?;
    let flags = Audio::AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | Audio::AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | Audio::AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    // Success is the decisive evidence that the exact WAVEFORMATEXTENSIBLE
    // speaker mask and scalar format are bound to this concrete stream.
    unsafe {
        audio_client
            .Initialize(
                Audio::AUDCLNT_SHAREMODE_SHARED,
                flags,
                SHARED_BUFFER_DURATION_100NS,
                0,
                &format.Format,
                None,
            )
            .map_err(|error| {
                format!(
                    "WASAPI rejected {} Hz / {} channel mask 0x{:08x}: {error}",
                    context.sample_rate, context.channels, context.channel_mask
                )
            })?;
    }

    // SAFETY: an unnamed auto-reset event has no external lifetime dependency.
    let event = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|error| format!("failed to create WASAPI render event: {error}"))?;
    let event_raw = event.0 as usize;
    let mut event_owner = EventOwner(Some(event));
    // SAFETY: the event stays open until the worker has stopped and the owner is disarmed.
    unsafe {
        audio_client
            .SetEventHandle(event)
            .map_err(|error| format!("failed to bind WASAPI render event: {error}"))?;
    }
    let buffer_frames = unsafe { audio_client.GetBufferSize() }
        .map_err(|error| format!("failed to query WASAPI buffer size: {error}"))?;
    let stream_latency = unsafe { audio_client.GetStreamLatency() }
        .map(|value| Duration::from_nanos(value.max(0) as u64 * 100))
        .map_err(|error| format!("failed to query WASAPI stream latency: {error}"))?;
    let render_client: Audio::IAudioRenderClient = unsafe { audio_client.GetService() }
        .map_err(|error| format!("failed to obtain WASAPI render client: {error}"))?;

    write_frames(
        context,
        &audio_client,
        &render_client,
        buffer_frames,
        stream_latency,
    )?;
    unsafe {
        audio_client.Start().map_err(|error| {
            WindowsWasapiNamedOutputOpenError::Start(format!(
                "failed to start WASAPI output stream: {error}"
            ))
        })?;
    }
    startup_tx
        .send(Startup::Ready { event_raw })
        .map_err(|error| format!("WASAPI owner disappeared during startup: {error}"))?;
    event_owner.0 = None;

    while !context.stop.load(Ordering::Acquire) {
        // SAFETY: the event remains owned by the parent stream until this worker joins.
        let wait = unsafe { WaitForSingleObject(event, INFINITE) };
        if wait != WAIT_OBJECT_0 {
            return Err(format!("WASAPI render-event wait failed with {wait:?}").into());
        }
        if context.stop.load(Ordering::Acquire) {
            break;
        }
        let padding = unsafe { audio_client.GetCurrentPadding() }
            .map_err(|error| format!("failed to query WASAPI render padding: {error}"))?;
        let available = buffer_frames.saturating_sub(padding);
        if available > 0 {
            write_frames(
                context,
                &audio_client,
                &render_client,
                available,
                stream_latency,
            )?;
        }
    }
    unsafe {
        audio_client
            .Stop()
            .map_err(|error| format!("failed to stop WASAPI output stream: {error}"))?;
    }
    Ok(())
}

fn write_frames(
    context: &RenderContext,
    audio_client: &Audio::IAudioClient,
    render_client: &Audio::IAudioRenderClient,
    frames: u32,
    stream_latency: Duration,
) -> Result<(), String> {
    let padding = unsafe { audio_client.GetCurrentPadding() }
        .map_err(|error| format!("failed to sample WASAPI playback padding: {error}"))?;
    let queued_delay = Duration::from_secs_f64(f64::from(padding) / f64::from(context.sample_rate));
    let playback_delay = stream_latency.saturating_add(queued_delay);
    let samples = usize::try_from(frames)
        .ok()
        .and_then(|frames| frames.checked_mul(context.channels))
        .ok_or_else(|| "WASAPI render-buffer sample extent overflowed".to_owned())?;
    // SAFETY: GetBuffer grants exactly `frames * block_align` writable bytes
    // until the matching ReleaseBuffer call. The initialized format is f32.
    let data = unsafe {
        render_client
            .GetBuffer(frames)
            .map_err(|error| format!("failed to acquire WASAPI render buffer: {error}"))?
    };
    // SAFETY: the pointer and exact sample extent are proven above.
    let output = unsafe { std::slice::from_raw_parts_mut(data.cast::<f32>(), samples) };
    render_f32_output_block(
        output,
        context.channels,
        &context.queue,
        &context.callback_control,
        &context.telemetry,
        playback_delay,
    );
    // SAFETY: this releases the exact frame extent acquired by GetBuffer.
    unsafe {
        render_client
            .ReleaseBuffer(frames, 0)
            .map_err(|error| format!("failed to release WASAPI render buffer: {error}"))?;
    }
    Ok(())
}

fn named_float_format(
    sample_rate: u32,
    channels: usize,
    channel_mask: u32,
) -> Result<Audio::WAVEFORMATEXTENSIBLE, String> {
    let channels = u16::try_from(channels)
        .map_err(|_| "WASAPI channel count exceeds WAVEFORMATEXTENSIBLE".to_owned())?;
    let block_align = channels
        .checked_mul(4)
        .ok_or_else(|| "WASAPI frame byte extent overflowed".to_owned())?;
    let average_bytes = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| "WASAPI average-byte rate overflowed".to_owned())?;
    Ok(Audio::WAVEFORMATEXTENSIBLE {
        Format: Audio::WAVEFORMATEX {
            wFormatTag: KernelStreaming::WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: average_bytes,
            nBlockAlign: block_align,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: Audio::WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
        dwChannelMask: channel_mask,
        SubFormat: Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    })
}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: paired with this worker's successful CoInitializeEx call.
        unsafe { CoUninitialize() };
    }
}

struct MmcssRegistration(HANDLE);

impl Drop for MmcssRegistration {
    fn drop(&mut self) {
        // SAFETY: the registration is reverted from its originating worker.
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.0);
        }
    }
}

struct EventOwner(Option<HANDLE>);

impl Drop for EventOwner {
    fn drop(&mut self) {
        if let Some(event) = self.0 {
            // SAFETY: this guard owns the event only on startup failure.
            unsafe {
                let _ = CloseHandle(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_float_format_carries_exact_mask_and_frame_extent() {
        let format = named_float_format(48_000, 6, 0x60f).expect("5.1(side) format");
        let channels = format.Format.nChannels;
        let block_align = format.Format.nBlockAlign;
        let average_bytes = format.Format.nAvgBytesPerSec;
        let format_tag = format.Format.wFormatTag;
        let channel_mask = format.dwChannelMask;
        let sub_format = format.SubFormat;
        assert_eq!(channels, 6);
        assert_eq!(block_align, 24);
        assert_eq!(average_bytes, 1_152_000);
        assert_eq!(format_tag, KernelStreaming::WAVE_FORMAT_EXTENSIBLE as u16);
        assert_eq!(channel_mask, 0x60f);
        assert_eq!(sub_format, Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
    }
}
