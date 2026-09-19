//! Event-driven WASAPI output for exact named-speaker layouts.
//!
//! CPAL's WASAPI backend deliberately opens `WAVEFORMATEXTENSIBLE` streams
//! with `KSAUDIO_SPEAKER_DIRECTOUT`. That is correct for ordinal channels but
//! cannot carry Mondrian's named speaker semantics. This narrow Adapter owns
//! only the Windows stream opening and render-buffer pump; queue authority,
//! callback activation, and transport evidence remain in the parent module.

use super::{render_f32_output_block, RealtimeAudioCallbackControl, RealtimeAudioOutputTelemetry};
use crate::{RealtimeAudioOutputAccessPolicy, RealtimeAudioOutputShareMode};
use crossbeam_queue::ArrayQueue;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{sync_channel, SyncSender},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::{Audio, KernelStreaming, Multimedia};
use windows::Win32::System::{
    Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED},
    Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
        WaitForSingleObject, INFINITE,
    },
};

const SHARED_BUFFER_DURATION_100NS: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WasapiSampleEncoding {
    F32,
    Pcm16,
    Pcm24Packed,
    Pcm24In32,
    Pcm32,
}

impl WasapiSampleEncoding {
    const fn sample_format(self) -> crate::RealtimeAudioSampleFormat {
        match self {
            Self::F32 => crate::RealtimeAudioSampleFormat::F32,
            Self::Pcm16 => crate::RealtimeAudioSampleFormat::I16,
            Self::Pcm24Packed | Self::Pcm24In32 => crate::RealtimeAudioSampleFormat::I24,
            Self::Pcm32 => crate::RealtimeAudioSampleFormat::I32,
        }
    }

    const fn container_bits(self) -> u16 {
        match self {
            Self::F32 | Self::Pcm24In32 | Self::Pcm32 => 32,
            Self::Pcm24Packed => 24,
            Self::Pcm16 => 16,
        }
    }

    const fn valid_bits(self) -> u16 {
        match self {
            Self::Pcm24Packed | Self::Pcm24In32 => 24,
            other => other.container_bits(),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::Pcm16 => "pcm16",
            Self::Pcm24Packed => "pcm24-packed",
            Self::Pcm24In32 => "pcm32-container-24-valid",
            Self::Pcm32 => "pcm32",
        }
    }
}

struct WasapiFormatCandidate {
    encoding: WasapiSampleEncoding,
    format: Audio::WAVEFORMATEXTENSIBLE,
}

/// Exact stream facts returned by the render worker after WASAPI initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowsWasapiOutputNegotiation {
    pub(super) share_mode: RealtimeAudioOutputShareMode,
    pub(super) sample_format: crate::RealtimeAudioSampleFormat,
    pub(super) container_bits: u16,
    pub(super) valid_bits: u16,
    pub(super) buffer_frames: u32,
    pub(super) period_100ns: i64,
    pub(super) exclusive_fallback_reason: Option<String>,
}

/// One running event-driven WASAPI stream whose format carries an exact mask.
pub(super) struct WindowsWasapiNamedOutputStream {
    stop: Arc<AtomicBool>,
    event_raw: usize,
    thread: Option<JoinHandle<()>>,
}

enum Startup {
    Ready {
        event_raw: usize,
        negotiation: WindowsWasapiOutputNegotiation,
    },
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
    access_policy: RealtimeAudioOutputAccessPolicy,
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
        access_policy: RealtimeAudioOutputAccessPolicy,
        queue: Arc<ArrayQueue<f32>>,
        callback_control: Arc<RealtimeAudioCallbackControl>,
        telemetry: Arc<RealtimeAudioOutputTelemetry>,
    ) -> Result<(Self, WindowsWasapiOutputNegotiation), WindowsWasapiNamedOutputOpenError> {
        let stop = Arc::new(AtomicBool::new(false));
        let context = RenderContext {
            endpoint_id,
            sample_rate,
            channels,
            channel_mask,
            access_policy,
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
            Ok(Startup::Ready { event_raw, negotiation }) => {
                Ok((Self { stop, event_raw, thread: Some(thread) }, negotiation))
            }
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
    let opened = open_audio_client(&endpoint, context)?;
    let audio_client = opened.client;
    let encoding = opened.encoding;
    let share_mode = opened.share_mode;
    let period_100ns = opened.period_100ns;
    let exclusive_fallback_reason = opened.exclusive_fallback_reason;

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
    let audio_clock: Audio::IAudioClock = unsafe { audio_client.GetService() }
        .map_err(|error| format!("failed to obtain WASAPI audio clock: {error}"))?;
    let audio_clock_frequency = unsafe { audio_clock.GetFrequency() }
        .map_err(|error| format!("failed to query WASAPI audio-clock frequency: {error}"))?;
    if audio_clock_frequency == 0 {
        return Err("WASAPI audio clock reported a zero frequency".to_owned().into());
    }
    let mut qpc_frequency = 0_i64;
    // SAFETY: the output pointer remains valid for the complete call.
    unsafe { QueryPerformanceFrequency(&mut qpc_frequency) }
        .map_err(|error| format!("failed to query QPC frequency: {error}"))?;
    let qpc_frequency = u64::try_from(qpc_frequency)
        .ok()
        .filter(|frequency| *frequency > 0)
        .ok_or_else(|| "QPC reported a non-positive frequency".to_owned())?;
    let scratch_samples = usize::try_from(buffer_frames)
        .ok()
        .and_then(|frames| frames.checked_mul(context.channels))
        .ok_or_else(|| "WASAPI render scratch extent overflowed".to_owned())?;
    let mut scratch = if encoding == WasapiSampleEncoding::F32 {
        Vec::new()
    } else {
        vec![0.0_f32; scratch_samples]
    };

    let initial_observed_at = Instant::now();
    let initial_playback_delay = super::callback_tail_playback_delay(
        stream_latency,
        buffer_frames as usize,
        context.sample_rate,
    )
    .map_err(|error| format!("invalid initial WASAPI endpoint delay: {error}"))?;
    let initial_uncertainty = super::callback_tail_playback_delay(
        Duration::ZERO,
        buffer_frames as usize,
        context.sample_rate,
    )
    .map_err(|error| format!("invalid initial WASAPI buffer span: {error}"))?;
    write_frames(
        context,
        &render_client,
        RenderPacket {
            observed_at: initial_observed_at,
            frames: buffer_frames,
            queued_before: 0,
        },
        initial_playback_delay,
        initial_uncertainty,
        encoding,
        &mut scratch,
    )?;
    unsafe {
        audio_client.Start().map_err(|error| {
            WindowsWasapiNamedOutputOpenError::Start(format!(
                "failed to start WASAPI output stream: {error}"
            ))
        })?;
    }
    startup_tx
        .send(Startup::Ready {
            event_raw,
            negotiation: WindowsWasapiOutputNegotiation {
                share_mode,
                sample_format: encoding.sample_format(),
                container_bits: encoding.container_bits(),
                valid_bits: encoding.valid_bits(),
                buffer_frames,
                period_100ns,
                exclusive_fallback_reason,
            },
        })
        .map_err(|error| format!("WASAPI owner disappeared during startup: {error}"))?;
    event_owner.0 = None;
    let mut last_audio_clock_correlation = None;

    while !context.stop.load(Ordering::Acquire) {
        // SAFETY: the event remains owned by the parent stream until this worker joins.
        let wait = unsafe { WaitForSingleObject(event, INFINITE) };
        if wait != WAIT_OBJECT_0 {
            return Err(format!("WASAPI render-event wait failed with {wait:?}").into());
        }
        if context.stop.load(Ordering::Acquire) {
            break;
        }
        let packet = sample_render_packet(share_mode, buffer_frames, || {
            unsafe { audio_client.GetCurrentPadding() }
                .map_err(|error| format!("failed to query WASAPI render padding: {error}"))
        })?;
        if packet.frames > 0 {
            let sampled = sample_wasapi_audio_clock(
                &audio_clock,
                audio_clock_frequency,
                qpc_frequency,
                context.telemetry.callback_consumed_frames.load(Ordering::Acquire),
                packet.frames,
                context.sample_rate,
                last_audio_clock_correlation,
            )?;
            last_audio_clock_correlation = Some(sampled.correlation);

            write_frames(
                context,
                &render_client,
                RenderPacket { observed_at: sampled.observed_at, ..packet },
                sampled.playback_delay,
                sampled.uncertainty,
                encoding,
                &mut scratch,
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

struct OpenedAudioClient {
    client: Audio::IAudioClient,
    encoding: WasapiSampleEncoding,
    share_mode: RealtimeAudioOutputShareMode,
    period_100ns: i64,
    exclusive_fallback_reason: Option<String>,
}

fn open_audio_client(
    endpoint: &Audio::IMMDevice,
    context: &RenderContext,
) -> Result<OpenedAudioClient, WindowsWasapiNamedOutputOpenError> {
    if context.access_policy == RealtimeAudioOutputAccessPolicy::Shared {
        return open_shared_audio_client(endpoint, context, None);
    }

    let probe_client = activate_audio_client(endpoint)?;
    let selected = select_exclusive_format(&probe_client, context);
    match selected {
        Ok(candidate) => match open_exclusive_audio_client(endpoint, context, candidate) {
            Ok(opened) => Ok(opened),
            Err(reason)
                if context.access_policy == RealtimeAudioOutputAccessPolicy::PreferExclusive =>
            {
                open_shared_audio_client(endpoint, context, Some(reason))
            }
            Err(reason) => Err(reason.into()),
        },
        Err(reason)
            if context.access_policy == RealtimeAudioOutputAccessPolicy::PreferExclusive =>
        {
            open_shared_audio_client(endpoint, context, Some(reason))
        }
        Err(reason) => Err(reason.into()),
    }
}

fn open_shared_audio_client(
    endpoint: &Audio::IMMDevice,
    context: &RenderContext,
    exclusive_fallback_reason: Option<String>,
) -> Result<OpenedAudioClient, WindowsWasapiNamedOutputOpenError> {
    let format = named_float_format(context.sample_rate, context.channels, context.channel_mask)?;
    let client = activate_audio_client(endpoint)?;
    let mut engine_period = 0_i64;
    unsafe {
        client
            .GetDevicePeriod(Some(&mut engine_period), None)
            .map_err(|error| format!("failed to query WASAPI shared engine period: {error}"))?;
        client
            .Initialize(
                Audio::AUDCLNT_SHAREMODE_SHARED,
                Audio::AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | Audio::AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | Audio::AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                SHARED_BUFFER_DURATION_100NS,
                0,
                &format.Format,
                None,
            )
            .map_err(|error| {
                format!(
                    "WASAPI shared mode rejected {} Hz / {} channel f32 mask 0x{:08x}: {error}",
                    context.sample_rate, context.channels, context.channel_mask
                )
            })?;
    }
    Ok(OpenedAudioClient {
        client,
        encoding: WasapiSampleEncoding::F32,
        share_mode: RealtimeAudioOutputShareMode::Shared,
        period_100ns: engine_period,
        exclusive_fallback_reason,
    })
}

fn select_exclusive_format(
    client: &Audio::IAudioClient,
    context: &RenderContext,
) -> Result<WasapiFormatCandidate, String> {
    let candidates = [
        WasapiSampleEncoding::F32,
        WasapiSampleEncoding::Pcm24In32,
        WasapiSampleEncoding::Pcm24Packed,
        WasapiSampleEncoding::Pcm32,
        WasapiSampleEncoding::Pcm16,
    ];
    let mut rejected = Vec::with_capacity(candidates.len());
    for encoding in candidates {
        let format = match encoding {
            WasapiSampleEncoding::F32 => {
                named_float_format(context.sample_rate, context.channels, context.channel_mask)?
            }
            _ => named_pcm_format(
                context.sample_rate,
                context.channels,
                context.channel_mask,
                encoding.container_bits(),
                encoding.valid_bits(),
            )?,
        };
        let result = unsafe {
            client.IsFormatSupported(Audio::AUDCLNT_SHAREMODE_EXCLUSIVE, &format.Format, None)
        };
        if result.is_ok() {
            return Ok(WasapiFormatCandidate { encoding, format });
        }
        rejected.push(format!("{}={result:?}", encoding.label()));
    }
    Err(format!(
        "WASAPI exclusive mode supports none of the allowed exact {} Hz / {} channel mask 0x{:08x} formats; {}",
        context.sample_rate,
        context.channels,
        context.channel_mask,
        rejected.join(", ")
    ))
}

fn open_exclusive_audio_client(
    endpoint: &Audio::IMMDevice,
    context: &RenderContext,
    candidate: WasapiFormatCandidate,
) -> Result<OpenedAudioClient, String> {
    let mut probe_client = activate_audio_client(endpoint).map_err(|error| match error {
        WindowsWasapiNamedOutputOpenError::Build(detail)
        | WindowsWasapiNamedOutputOpenError::Start(detail) => detail,
    })?;
    let mut minimum_period = 0_i64;
    unsafe {
        probe_client
            .GetDevicePeriod(None, Some(&mut minimum_period))
            .map_err(|error| format!("failed to query WASAPI exclusive device period: {error}"))?;
    }
    if minimum_period <= 0 {
        return Err("WASAPI reported a non-positive exclusive device period".to_owned());
    }

    let initialize = |client: &Audio::IAudioClient, duration: i64| unsafe {
        client.Initialize(
            Audio::AUDCLNT_SHAREMODE_EXCLUSIVE,
            Audio::AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            duration,
            duration,
            &candidate.format.Format,
            None,
        )
    };
    let mut period = minimum_period;
    if let Err(error) = initialize(&probe_client, period) {
        if error.code() != Audio::AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED {
            return Err(format!(
                "WASAPI exclusive {} initialization failed at period {}: {error}",
                candidate.encoding.label(),
                period
            ));
        }
        let aligned_frames = unsafe { probe_client.GetBufferSize() }.map_err(|query_error| {
            format!(
                "WASAPI exclusive buffer alignment failed and aligned size was unavailable: {query_error}"
            )
        })?;
        period = aligned_buffer_duration_100ns(aligned_frames, context.sample_rate)?;
        probe_client =
            activate_audio_client(endpoint).map_err(|activate_error| match activate_error {
                WindowsWasapiNamedOutputOpenError::Build(detail)
                | WindowsWasapiNamedOutputOpenError::Start(detail) => detail,
            })?;
        initialize(&probe_client, period).map_err(|retry_error| {
            format!(
                "WASAPI exclusive aligned retry rejected {} frames / {} 100ns units for {}: {retry_error}",
                aligned_frames,
                period,
                candidate.encoding.label()
            )
        })?;
    }

    Ok(OpenedAudioClient {
        client: probe_client,
        encoding: candidate.encoding,
        share_mode: RealtimeAudioOutputShareMode::Exclusive,
        period_100ns: period,
        exclusive_fallback_reason: None,
    })
}
fn activate_audio_client(
    endpoint: &Audio::IMMDevice,
) -> Result<Audio::IAudioClient, WindowsWasapiNamedOutputOpenError> {
    // SAFETY: the selected endpoint was obtained from the render-device catalog.
    unsafe {
        endpoint.Activate(CLSCTX_ALL, None).map_err(|error| {
            WindowsWasapiNamedOutputOpenError::Build(format!(
                "failed to activate selected WASAPI endpoint: {error}"
            ))
        })
    }
}

fn aligned_buffer_duration_100ns(frames: u32, sample_rate: u32) -> Result<i64, String> {
    if frames == 0 || sample_rate == 0 {
        return Err("WASAPI exclusive alignment returned an empty extent".to_owned());
    }
    let numerator = u64::from(frames)
        .checked_mul(10_000_000)
        .and_then(|value| value.checked_add(u64::from(sample_rate) - 1))
        .ok_or_else(|| "WASAPI exclusive aligned duration overflowed".to_owned())?;
    i64::try_from(numerator / u64::from(sample_rate))
        .map_err(|_| "WASAPI exclusive aligned duration exceeded i64".to_owned())
}
struct WasapiAudioClockSample {
    correlation: WasapiAudioClockCorrelation,
    observed_at: Instant,
    playback_delay: Duration,
    uncertainty: Duration,
}

#[derive(Clone, Copy)]
struct WasapiAudioClockCorrelation {
    raw_position: u64,
    qpc_position_100ns: u64,
}

fn sample_wasapi_audio_clock(
    audio_clock: &Audio::IAudioClock,
    frequency: u64,
    qpc_frequency: u64,
    submitted_before: u64,
    packet_frames: u32,
    sample_rate: u32,
    previous_correlation: Option<WasapiAudioClockCorrelation>,
) -> Result<WasapiAudioClockSample, String> {
    let query_started = Instant::now();
    let mut raw_position = 0_u64;
    let mut position_qpc_100ns = 0_u64;
    // SAFETY: the clock belongs to this COM worker and both output pointers
    // remain valid for the complete call.
    unsafe { audio_clock.GetPosition(&mut raw_position, Some(&mut position_qpc_100ns)) }
        .map_err(|error| format!("failed to query WASAPI audio-clock position: {error}"))?;
    let qpc_now_100ns = qpc_now_100ns(qpc_frequency)?;
    let observed_at = Instant::now();
    if previous_correlation.is_some_and(|previous| raw_position < previous.raw_position) {
        return Err(format!(
            "WASAPI audio clock regressed from {} to {raw_position}",
            previous_correlation.map(|sample| sample.raw_position).unwrap_or_default()
        ));
    }
    let projected_position = project_wasapi_audio_clock_position(
        raw_position,
        frequency,
        position_qpc_100ns,
        qpc_now_100ns,
        submitted_before,
        sample_rate,
    )?;
    let (playback_delay, quantization_uncertainty) = wasapi_audio_clock_tail_timing(
        projected_position,
        frequency,
        submitted_before,
        packet_frames,
        sample_rate,
    )?;
    let cross_clock_residual = wasapi_cross_clock_residual_uncertainty(
        previous_correlation,
        WasapiAudioClockCorrelation {
            raw_position,
            qpc_position_100ns: position_qpc_100ns,
        },
        frequency,
    )?;
    let uncertainty = observed_at
        .saturating_duration_since(query_started)
        .checked_add(quantization_uncertainty)
        .and_then(|value| value.checked_add(cross_clock_residual))
        .ok_or_else(|| "WASAPI audio-clock uncertainty overflowed Duration".to_owned())?;
    Ok(WasapiAudioClockSample {
        correlation: WasapiAudioClockCorrelation {
            raw_position,
            qpc_position_100ns: position_qpc_100ns,
        },
        observed_at,
        playback_delay,
        uncertainty,
    })
}

fn wasapi_cross_clock_residual_uncertainty(
    previous: Option<WasapiAudioClockCorrelation>,
    current: WasapiAudioClockCorrelation,
    frequency: u64,
) -> Result<Duration, String> {
    let Some(previous) = previous else {
        return Ok(Duration::ZERO);
    };
    if frequency == 0 {
        return Err("WASAPI cross-clock residual requires a positive frequency".to_owned());
    }
    let device_ticks = current
        .raw_position
        .checked_sub(previous.raw_position)
        .ok_or_else(|| "WASAPI audio-clock position regressed".to_owned())?;
    let qpc_100ns = current
        .qpc_position_100ns
        .checked_sub(previous.qpc_position_100ns)
        .ok_or_else(|| "WASAPI audio-clock QPC position regressed".to_owned())?;
    let device_100ns = u128::from(device_ticks)
        .checked_mul(10_000_000)
        .and_then(|value| value.checked_div(u128::from(frequency)))
        .ok_or_else(|| "WASAPI cross-clock residual conversion overflowed".to_owned())?;
    let residual_100ns = device_100ns.abs_diff(u128::from(qpc_100ns));
    let residual_ns = residual_100ns
        .checked_mul(100)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| "WASAPI cross-clock residual exceeded Duration".to_owned())?;
    Ok(Duration::from_nanos(residual_ns))
}

fn qpc_now_100ns(frequency: u64) -> Result<u64, String> {
    if frequency == 0 {
        return Err("QPC conversion requires a positive frequency".to_owned());
    }
    let mut counter = 0_i64;
    // SAFETY: the output pointer remains valid for the complete call.
    unsafe { QueryPerformanceCounter(&mut counter) }
        .map_err(|error| format!("failed to query QPC position: {error}"))?;
    let counter =
        u64::try_from(counter).map_err(|_| "QPC reported a negative position".to_owned())?;
    let units_100ns = u128::from(counter)
        .checked_mul(10_000_000)
        .and_then(|value| value.checked_div(u128::from(frequency)))
        .ok_or_else(|| "QPC 100ns conversion overflowed".to_owned())?;
    u64::try_from(units_100ns).map_err(|_| "QPC 100ns position exceeded u64".to_owned())
}

fn project_wasapi_audio_clock_position(
    raw_position: u64,
    frequency: u64,
    position_qpc_100ns: u64,
    qpc_now_100ns: u64,
    submitted_before: u64,
    sample_rate: u32,
) -> Result<u64, String> {
    if frequency == 0 || sample_rate == 0 {
        return Err("WASAPI audio-clock projection requires positive rates".to_owned());
    }
    if position_qpc_100ns > qpc_now_100ns.saturating_add(1) {
        return Err(format!(
            "WASAPI audio-clock QPC position {position_qpc_100ns} is newer than local QPC {qpc_now_100ns}"
        ));
    }
    let elapsed_100ns = qpc_now_100ns.saturating_sub(position_qpc_100ns);
    let projected_ticks = u128::from(elapsed_100ns)
        .checked_mul(u128::from(frequency))
        .and_then(|value| value.checked_div(10_000_000))
        .ok_or_else(|| "WASAPI audio-clock QPC projection overflowed".to_owned())?;
    let submitted_tail_position = u128::from(submitted_before)
        .checked_mul(u128::from(frequency))
        .and_then(|value| value.checked_div(u128::from(sample_rate)))
        .ok_or_else(|| "WASAPI submitted-tail clock conversion overflowed".to_owned())?;
    let projected_position = u128::from(raw_position)
        .checked_add(projected_ticks)
        .ok_or_else(|| "WASAPI audio-clock projected position overflowed".to_owned())?
        .min(submitted_tail_position);
    u64::try_from(projected_position)
        .map_err(|_| "WASAPI audio-clock projected position exceeded u64".to_owned())
}

fn wasapi_audio_clock_tail_timing(
    raw_position: u64,
    frequency: u64,
    submitted_before: u64,
    packet_frames: u32,
    sample_rate: u32,
) -> Result<(Duration, Duration), String> {
    if frequency == 0 || sample_rate == 0 {
        return Err("WASAPI audio-clock timing requires positive rates".to_owned());
    }
    let device_frames = u128::from(raw_position)
        .checked_mul(u128::from(sample_rate))
        .and_then(|value| value.checked_div(u128::from(frequency)))
        .ok_or_else(|| "WASAPI audio-clock position conversion overflowed".to_owned())?;
    let device_frames = u64::try_from(device_frames)
        .map_err(|_| "WASAPI audio-clock position exceeded u64 frames".to_owned())?;
    if device_frames > submitted_before {
        return Err(format!(
            "WASAPI audio clock advanced to {device_frames} frames beyond {submitted_before} submitted frames"
        ));
    }
    let submitted_after = submitted_before
        .checked_add(u64::from(packet_frames))
        .ok_or_else(|| "WASAPI submitted-frame position overflowed".to_owned())?;
    let queued_to_tail = submitted_after
        .checked_sub(device_frames)
        .ok_or_else(|| "WASAPI device position exceeded the callback tail".to_owned())?;
    let queued_to_tail = usize::try_from(queued_to_tail)
        .map_err(|_| "WASAPI callback-tail delay exceeded usize frames".to_owned())?;
    let playback_delay =
        super::callback_tail_playback_delay(Duration::ZERO, queued_to_tail, sample_rate)
            .map_err(|error| format!("invalid WASAPI audio-clock tail delay: {error}"))?;
    let quantization_uncertainty =
        super::callback_tail_playback_delay(Duration::ZERO, 1, sample_rate)
            .map_err(|error| format!("invalid WASAPI audio-clock quantization: {error}"))?;
    Ok((playback_delay, quantization_uncertainty))
}

struct RenderPacket {
    observed_at: Instant,
    frames: u32,
    queued_before: u32,
}

fn sample_render_packet(
    share_mode: RealtimeAudioOutputShareMode,
    buffer_frames: u32,
    read_padding: impl FnOnce() -> Result<u32, String>,
) -> Result<RenderPacket, String> {
    let observed_at = Instant::now();
    // Exclusive event-driven streams transfer one complete buffer per event.
    // Shared streams bind writable capacity to one coherent padding observation.
    let queued_before = if share_mode == RealtimeAudioOutputShareMode::Exclusive {
        0
    } else {
        read_padding()?
    };
    let frames = buffer_frames
        .checked_sub(queued_before)
        .ok_or_else(|| "WASAPI padding exceeds the negotiated buffer extent".to_owned())?;
    Ok(RenderPacket { observed_at, frames, queued_before })
}

fn write_frames(
    context: &RenderContext,
    render_client: &Audio::IAudioRenderClient,
    packet: RenderPacket,
    playback_delay: Duration,
    playback_delay_uncertainty: Duration,
    encoding: WasapiSampleEncoding,
    scratch: &mut [f32],
) -> Result<(), String> {
    let RenderPacket { observed_at, frames, queued_before } = packet;
    let _ = queued_before;
    let samples = usize::try_from(frames)
        .ok()
        .and_then(|frames| frames.checked_mul(context.channels))
        .ok_or_else(|| "WASAPI render-buffer sample extent overflowed".to_owned())?;
    // SAFETY: GetBuffer grants the negotiated writable frame extent until ReleaseBuffer.
    let data = unsafe {
        render_client
            .GetBuffer(frames)
            .map_err(|error| format!("failed to acquire WASAPI render buffer: {error}"))?
    };
    if encoding == WasapiSampleEncoding::F32 {
        // SAFETY: the negotiated format is f32 and the pointer covers all samples.
        let output = unsafe { std::slice::from_raw_parts_mut(data.cast::<f32>(), samples) };
        render_f32_output_block(
            observed_at,
            output,
            context.channels,
            &context.queue,
            &context.callback_control,
            &context.telemetry,
            playback_delay,
            playback_delay_uncertainty,
        );
    } else {
        let staging = scratch.get_mut(..samples).ok_or_else(|| {
            "WASAPI render scratch buffer is smaller than the callback".to_owned()
        })?;
        render_f32_output_block(
            observed_at,
            staging,
            context.channels,
            &context.queue,
            &context.callback_control,
            &context.telemetry,
            playback_delay,
            playback_delay_uncertainty,
        );
        // SAFETY: storage matches the negotiated container and remains writable.
        unsafe { encode_pcm_samples(data, staging, encoding) };
    }
    // SAFETY: this releases the exact frame extent acquired by GetBuffer.
    unsafe {
        render_client
            .ReleaseBuffer(frames, 0)
            .map_err(|error| format!("failed to release WASAPI render buffer: {error}"))?;
    }
    Ok(())
}

unsafe fn encode_pcm_samples(
    destination: *mut u8,
    samples: &[f32],
    encoding: WasapiSampleEncoding,
) {
    let bytes_per_sample = usize::from(encoding.container_bits() / 8);
    let output = unsafe {
        std::slice::from_raw_parts_mut(destination, samples.len().saturating_mul(bytes_per_sample))
    };
    for (sample, chunk) in samples.iter().copied().zip(output.chunks_exact_mut(bytes_per_sample)) {
        match encoding {
            WasapiSampleEncoding::F32 => unreachable!("f32 output bypasses PCM conversion"),
            WasapiSampleEncoding::Pcm16 => {
                chunk.copy_from_slice(&(quantize_signed(sample, 16) as i16).to_le_bytes());
            }
            WasapiSampleEncoding::Pcm24Packed => {
                let value = quantize_signed(sample, 24).to_le_bytes();
                chunk.copy_from_slice(&value[..3]);
            }
            WasapiSampleEncoding::Pcm24In32 => {
                let value = quantize_signed(sample, 24) << 8;
                chunk.copy_from_slice(&value.to_le_bytes());
            }
            WasapiSampleEncoding::Pcm32 => {
                chunk.copy_from_slice(&quantize_signed(sample, 32).to_le_bytes());
            }
        }
    }
}

fn quantize_signed(sample: f32, bits: u32) -> i32 {
    if !sample.is_finite() {
        return 0;
    }
    let minimum = -(1_i64 << (bits - 1));
    let maximum = (1_i64 << (bits - 1)) - 1;
    if sample <= -1.0 {
        return minimum as i32;
    }
    if sample >= 1.0 {
        return maximum as i32;
    }
    (f64::from(sample) * maximum as f64).round() as i32
}

fn named_pcm_format(
    sample_rate: u32,
    channels: usize,
    channel_mask: u32,
    container_bits: u16,
    valid_bits: u16,
) -> Result<Audio::WAVEFORMATEXTENSIBLE, String> {
    if !matches!(container_bits, 16 | 24 | 32) || valid_bits == 0 || valid_bits > container_bits {
        return Err(format!(
            "unsupported WASAPI PCM container/valid-bit pair {container_bits}/{valid_bits}"
        ));
    }
    let channels = u16::try_from(channels)
        .map_err(|_| "WASAPI channel count exceeds WAVEFORMATEXTENSIBLE".to_owned())?;
    let bytes_per_sample = container_bits / 8;
    let block_align = channels
        .checked_mul(bytes_per_sample)
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
            wBitsPerSample: container_bits,
            cbSize: 22,
        },
        Samples: Audio::WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: valid_bits },
        dwChannelMask: channel_mask,
        SubFormat: KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM,
    })
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
    fn shared_packet_binds_capacity_to_one_padding_sample() {
        let samples = std::cell::Cell::new(0);
        let before = Instant::now();
        let packet = sample_render_packet(RealtimeAudioOutputShareMode::Shared, 4_800, || {
            samples.set(samples.get() + 1);
            Ok(4_320)
        })
        .expect("shared packet");
        assert_eq!(samples.get(), 1);
        assert_eq!(packet.frames, 480);
        assert_eq!(packet.queued_before + packet.frames, 4_800);
        assert!(packet.observed_at >= before && packet.observed_at <= Instant::now());
        let full = sample_render_packet(RealtimeAudioOutputShareMode::Shared, 480, || Ok(480))
            .expect("full endpoint buffer");
        assert_eq!(full.frames, 0);
        assert!(
            sample_render_packet(RealtimeAudioOutputShareMode::Shared, 480, || Ok(481)).is_err()
        );
        assert!(
            sample_render_packet(RealtimeAudioOutputShareMode::Shared, 480, || Err(
                "device lost".into()
            ))
            .is_err()
        );
    }

    #[test]
    fn audio_clock_qpc_projection_advances_a_stale_device_position() {
        let projected = project_wasapi_audio_clock_position(
            48_000, 384_000, 1_000_000, 1_100_000, 60_000, 48_000,
        )
        .expect("project correlated audio clock");
        assert_eq!(projected, 51_840);
    }

    #[test]
    fn audio_clock_qpc_projection_stops_at_the_submitted_tail() {
        let projected = project_wasapi_audio_clock_position(
            390_000, 384_000, 1_000_000, 2_000_000, 50_000, 48_000,
        )
        .expect("bound projection by submitted samples");
        assert_eq!(projected, 400_000);
    }

    #[test]
    fn audio_clock_qpc_projection_rejects_an_uncorrelated_future_timestamp() {
        let error = project_wasapi_audio_clock_position(
            48_000, 384_000, 1_000_002, 1_000_000, 50_000, 48_000,
        )
        .expect_err("future clock timestamp must fail closed");
        assert!(error.contains("newer than local QPC"));
    }

    #[test]
    fn audio_clock_maps_device_position_to_the_submitted_callback_tail() {
        let (delay, uncertainty) = wasapi_audio_clock_tail_timing(480, 48_000, 960, 480, 48_000)
            .expect("client-frame clock");
        assert_eq!(delay, Duration::from_millis(20));
        assert_eq!(uncertainty, Duration::from_nanos(20_834));

        let (scaled_delay, scaled_uncertainty) =
            wasapi_audio_clock_tail_timing(100_000, 10_000_000, 960, 480, 48_000)
                .expect("100ns-unit clock");
        assert_eq!(scaled_delay, delay);
        assert_eq!(scaled_uncertainty, uncertainty);
    }

    #[test]
    fn audio_clock_rejects_position_beyond_submitted_frames() {
        let error = wasapi_audio_clock_tail_timing(961, 48_000, 960, 480, 48_000)
            .expect_err("future device position must fail closed");
        assert!(error.contains("beyond 960 submitted frames"));
    }

    #[test]
    fn audio_clock_uncertainty_includes_measured_device_to_qpc_residual() {
        let previous = WasapiAudioClockCorrelation {
            raw_position: 384_000,
            qpc_position_100ns: 1_000_000,
        };
        let current = WasapiAudioClockCorrelation {
            raw_position: 387_840,
            qpc_position_100ns: 1_100_500,
        };
        let uncertainty = wasapi_cross_clock_residual_uncertainty(Some(previous), current, 384_000)
            .expect("derive measured cross-clock residual");
        assert_eq!(uncertainty, Duration::from_micros(50));
        assert_eq!(
            wasapi_cross_clock_residual_uncertainty(None, current, 384_000)
                .expect("first correlated sample"),
            Duration::ZERO
        );
    }

    #[test]
    fn exclusive_event_packet_transfers_full_buffer_without_padding_query() {
        let packet = sample_render_packet(RealtimeAudioOutputShareMode::Exclusive, 480, || {
            panic!("exclusive event mode must not depend on padding")
        })
        .expect("exclusive event packet");
        assert_eq!(packet.frames, 480);
        assert_eq!(packet.queued_before, 0);
    }

    #[test]
    fn professional_pcm_formats_preserve_container_and_valid_bit_contracts() {
        let packed = named_pcm_format(48_000, 2, 0x3, 24, 24).expect("packed 24-bit");
        let packed_block_align =
            unsafe { std::ptr::addr_of!(packed.Format.nBlockAlign).read_unaligned() };
        let packed_container_bits =
            unsafe { std::ptr::addr_of!(packed.Format.wBitsPerSample).read_unaligned() };
        let packed_valid_bits =
            unsafe { std::ptr::addr_of!(packed.Samples.wValidBitsPerSample).read_unaligned() };
        let packed_subformat = unsafe { std::ptr::addr_of!(packed.SubFormat).read_unaligned() };
        assert_eq!(packed_block_align, 6);
        assert_eq!(packed_container_bits, 24);
        assert_eq!(packed_valid_bits, 24);
        assert_eq!(packed_subformat, KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM);

        let padded = named_pcm_format(48_000, 2, 0x3, 32, 24).expect("24-in-32");
        let padded_block_align =
            unsafe { std::ptr::addr_of!(padded.Format.nBlockAlign).read_unaligned() };
        let padded_container_bits =
            unsafe { std::ptr::addr_of!(padded.Format.wBitsPerSample).read_unaligned() };
        let padded_valid_bits =
            unsafe { std::ptr::addr_of!(padded.Samples.wValidBitsPerSample).read_unaligned() };
        assert_eq!(padded_block_align, 8);
        assert_eq!(padded_container_bits, 32);
        assert_eq!(padded_valid_bits, 24);
    }

    #[test]
    fn pcm_conversion_is_saturating_little_endian_and_silences_non_finite_values() {
        let samples = [-1.0, -0.5, 0.0, 0.5, 1.0, f32::NAN];
        let mut packed = vec![0_u8; samples.len() * 3];
        unsafe {
            encode_pcm_samples(
                packed.as_mut_ptr(),
                &samples,
                WasapiSampleEncoding::Pcm24Packed,
            );
        }
        assert_eq!(&packed[0..3], &[0x00, 0x00, 0x80]);
        assert_eq!(&packed[6..9], &[0x00, 0x00, 0x00]);
        assert_eq!(&packed[12..15], &[0xff, 0xff, 0x7f]);
        assert_eq!(&packed[15..18], &[0x00, 0x00, 0x00]);

        let mut padded = vec![0_u8; 4];
        unsafe {
            encode_pcm_samples(padded.as_mut_ptr(), &[1.0], WasapiSampleEncoding::Pcm24In32);
        }
        assert_eq!(padded, vec![0x00, 0xff, 0xff, 0x7f]);
    }
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
