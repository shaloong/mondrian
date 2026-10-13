//! Actual restored-instance Note input binding and one raw callback source.
//! No event scheduler, voice ledger, author state or DSP qualification lives here.

use super::clap_discovery::{invalid, unavailable};
use super::clap_note_input_metadata::{inspect_inputs, validate_factory_plugin_id};
use super::clap_worker::{configure_render_mode, ClapHost, ClapHostShared, ClapNativeHostShared};
use super::native_request_cohort::{NativeRequestMarker, RecordedRequestCohort};
use crate::{
    AudioNoteExecutionLimits, AudioProcessorHostError, AudioProcessorNoteInputContract,
    AudioProcessorNoteInputPort, AudioRenderContract,
};
use clack_extensions::state::PluginState;
use clack_extensions::thread_check::HostThreadCheckImpl;
use clack_host::prelude::{HostInfo, PluginEntry, PluginInstance};
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::sync::atomic::{AtomicU32, Ordering};

pub(super) const NATIVE_CLAP_DIALECT: u32 = 1;
const KNOWN_DIALECTS: u32 = 15;
const RESCAN_ALL: u32 = 1;
const RESCAN_NAMES: u32 = 2;
const KEY_CAPACITY: usize = 26;

#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum NotePortFailure {
    WrongThread,
    DisabledOperation,
    UnknownFlags,
    ActiveAll,
    ExhaustedCohort,
}

/// Inline in the actual SDK Shared owner. Raw callback flags are monotonic
/// evidence; only the exact successfully scanned legal cohort may be retired.
pub(super) struct NotePortRequests {
    cohort: NativeRequestMarker,
    observed_flags: AtomicU32,
    failures: AtomicU32,
}
impl NotePortRequests {
    pub(super) const fn new() -> Self {
        Self {
            cohort: NativeRequestMarker::new(),
            observed_flags: AtomicU32::new(0),
            failures: AtomicU32::new(0),
        }
    }
    pub(super) fn fail(&self, failure: NotePortFailure) {
        self.failures.fetch_or(1 << failure as u8, Ordering::Release);
    }
    pub(super) fn record(&self, flags: u32, actual_main: bool, enabled: bool, active: bool) {
        self.observed_flags.fetch_or(flags, Ordering::AcqRel);
        if !actual_main {
            self.fail(NotePortFailure::WrongThread);
            return;
        }
        if !enabled {
            self.fail(NotePortFailure::DisabledOperation);
            return;
        }
        // Only legitimate actual-main callbacks mutate the existing cohort.
        // Foreign callback refusal touches only immutable shared atomic fields.
        self.cohort.record(flags);
        if flags & !(RESCAN_ALL | RESCAN_NAMES) != 0 {
            self.fail(NotePortFailure::UnknownFlags);
        }
        if flags & RESCAN_ALL != 0 && active {
            self.fail(NotePortFailure::ActiveAll);
        }
        if self.cohort.snapshot().exhausted() {
            self.fail(NotePortFailure::ExhaustedCohort);
        }
    }
    pub(super) fn snapshot(&self) -> RecordedRequestCohort {
        self.cohort.snapshot()
    }
    pub(super) fn check(&self) -> Result<(), AudioProcessorHostError> {
        let failures = self.failures.load(Ordering::Acquire);
        if failures != 0 {
            return Err(unavailable(format!(
                "CLAP Note ports callback unavailable: failure_bits={failures:#x} raw_flags={:#x}",
                self.observed_flags.load(Ordering::Acquire)
            )));
        }
        Ok(())
    }
    fn retire_scanned(
        &self,
        captured: RecordedRequestCohort,
    ) -> Result<(), AudioProcessorHostError> {
        self.check()?;
        if self.snapshot() != captured || captured.pending() && !self.cohort.retire(captured) {
            return Err(unavailable("CLAP Note ports changed during actual scan"));
        }
        self.check()?;
        if self.snapshot().pending() {
            return Err(unavailable("CLAP Note ports has a newer unscanned cohort"));
        }
        Ok(())
    }
}

/// A host offer bounded by the existing shared finite limits, independent of
/// plugin names, claimed voice counts or instrument classification.
pub(super) const fn offered_limits() -> AudioNoteExecutionLimits {
    AudioNoteExecutionLimits {
        input_events: crate::AUDIO_NOTE_EVENTS_PER_BLOCK,
        output_ends: crate::AUDIO_NOTE_ENDS_PER_BLOCK,
        retained_records: crate::AUDIO_NOTE_RECORDS,
        live_voices: crate::AUDIO_NOTE_RECORDS,
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PortFingerprint {
    id: u32,
    supported: u32,
    preferred: u32,
}

pub(super) struct RestoredNoteInputs {
    pub(super) contract: AudioProcessorNoteInputContract,
    fingerprints: [PortFingerprint; crate::AUDIO_NOTE_INPUT_PORTS],
}

impl RestoredNoteInputs {
    pub(super) fn allocated_bytes(&self) -> Result<usize, AudioProcessorHostError> {
        let ports = std::alloc::Layout::array::<AudioProcessorNoteInputPort>(
            self.contract.ports.capacity(),
        )
        .map_err(|_| invalid("CLAP Note binding allocation overflowed"))?
        .size();
        self.contract.ports.iter().try_fold(ports, |bytes, port| {
            bytes
                .checked_add(port.port_key.capacity())
                .ok_or_else(|| invalid("CLAP Note key allocation overflowed"))
        })
    }
    pub(super) fn reservation_bytes(count: usize) -> Result<usize, AudioProcessorHostError> {
        if count > crate::AUDIO_NOTE_INPUT_PORTS {
            return Err(invalid("CLAP Note port count exceeds bound"));
        }
        std::alloc::Layout::array::<AudioProcessorNoteInputPort>(count)
            .map_err(|_| invalid("CLAP Note binding reservation overflowed"))?
            .size()
            .checked_add(
                count
                    .checked_mul(KEY_CAPACITY)
                    .ok_or_else(|| invalid("CLAP Note keys overflowed"))?,
            )
            .ok_or_else(|| invalid("CLAP Note binding reservation overflowed"))
    }
}

fn check_role(
    shared: &ClapNativeHostShared,
    require_inactive: bool,
) -> Result<(), AudioProcessorHostError> {
    if !shared.control.is_main_thread() || !shared.control.note_input_enabled {
        return Err(unavailable(
            "CLAP Note negotiation requires its explicit actual-main operation",
        ));
    }
    if require_inactive && shared.services.active.load(Ordering::Acquire) {
        return Err(unavailable(
            "CLAP full Note scan requires the actual inactive instance",
        ));
    }
    shared.services.note_ports.check()?;
    if shared.control.restart_requested() {
        return Err(unavailable("CLAP Note negotiation has pending restart"));
    }
    Ok(())
}

fn scan_inner(
    instance: &mut PluginInstance<ClapHost>,
    limits: AudioNoteExecutionLimits,
    expected: Option<&RestoredNoteInputs>,
    names_only: bool,
    retire: bool,
) -> Result<RestoredNoteInputs, AudioProcessorHostError> {
    if !names_only && instance.is_active() {
        return Err(unavailable(
            "CLAP full Note scan requires the actual deactivated SDK instance",
        ));
    }
    instance.access_shared_handler(|shared| check_role(shared, !names_only))?;
    let captured = instance.access_shared_handler(|shared| shared.services.note_ports.snapshot());
    if names_only && (captured.flags() & !RESCAN_NAMES != 0 || expected.is_none()) {
        return Err(unavailable(
            "CLAP Note names service cannot retire a full-port request",
        ));
    }
    let control = instance.access_shared_handler(|shared| shared.control.clone());
    let (present, raw) = {
        let handle = instance.plugin_handle();
        inspect_inputs(&handle, &control)?
    };
    instance.access_shared_handler(|shared| check_role(shared, !names_only))?;
    if !present || raw.is_empty() {
        return Err(unavailable("CLAP native Note input ports are absent"));
    }
    if raw.len() > crate::AUDIO_NOTE_INPUT_PORTS {
        return Err(invalid("CLAP Note input scan exceeds shared index bound"));
    }
    let mut ports = Vec::new();
    ports
        .try_reserve_exact(raw.len())
        .map_err(|_| invalid("CLAP Note binding storage unavailable"))?;
    if ports.capacity() > raw.len() {
        return Err(invalid("CLAP Note binding capacity exceeds reservation"));
    }
    let mut fingerprints = [PortFingerprint::default(); crate::AUDIO_NOTE_INPUT_PORTS];
    for (index, port) in raw.iter().enumerate() {
        if port.port_index != index as u32 || port.supported_dialects & NATIVE_CLAP_DIALECT == 0 {
            return Err(unavailable(format!(
                "CLAP native Note dialect unavailable at unchanged input index {index}"
            )));
        }
        let preference = port.preferred_dialect;
        if preference == 0
            || preference & !KNOWN_DIALECTS != 0
            || !preference.is_power_of_two()
            || port.supported_dialects & preference == 0
        {
            return Err(unavailable(format!("CLAP native Note preferred dialect unavailable at input index {index}: raw={preference:#x}")));
        }
        // Full raw supported masks survive future bits; selection is CLAP only.
        fingerprints[index] = PortFingerprint {
            id: port.id,
            supported: port.supported_dialects,
            preferred: preference,
        };
        let mut key = String::new();
        key.try_reserve_exact(KEY_CAPACITY)
            .map_err(|_| invalid("CLAP Note stable key storage unavailable"))?;
        if key.capacity() > KEY_CAPACITY {
            return Err(invalid("CLAP Note key capacity exceeds reservation"));
        }
        write!(&mut key, "clap.note-input.{}", port.id)
            .map_err(|_| invalid("CLAP Note stable key formatting failed"))?;
        ports.push(AudioProcessorNoteInputPort {
            port_key: key,
            native_id: port.id,
            native_index: index as u16,
        });
    }
    let binding = RestoredNoteInputs {
        contract: AudioProcessorNoteInputContract { ports, limits },
        fingerprints,
    };
    binding.contract.validate()?;
    if binding.allocated_bytes()? > RestoredNoteInputs::reservation_bytes(raw.len())? {
        return Err(invalid(
            "CLAP actual Note binding storage exceeds its reservation",
        ));
    }
    if let Some(expected) = expected
        && (binding.contract != expected.contract || binding.fingerprints != expected.fingerprints)
    {
        return Err(unavailable(
            "CLAP actual Note input IDs/order/dialects changed; new exact contract required",
        ));
    }
    instance.access_shared_handler(|shared| {
        shared.services.note_ports.check()?;
        if retire {
            shared.services.note_ports.retire_scanned(captured)
        } else if shared.services.note_ports.snapshot() != captured {
            Err(unavailable("CLAP Note ports changed during actual scan"))
        } else {
            Ok(())
        }
    })?;
    Ok(binding)
}

fn scan(
    instance: &mut PluginInstance<ClapHost>,
    limits: AudioNoteExecutionLimits,
    expected: Option<&RestoredNoteInputs>,
    names_only: bool,
) -> Result<RestoredNoteInputs, AudioProcessorHostError> {
    scan_inner(instance, limits, expected, names_only, true)
}

pub(super) fn negotiate_restored(
    instance: &mut PluginInstance<ClapHost>,
    expected: &AudioProcessorNoteInputContract,
) -> Result<RestoredNoteInputs, AudioProcessorHostError> {
    expected.validate()?;
    let captured = instance.access_shared_handler(|shared| shared.services.note_ports.snapshot());
    // Do not retire before comparing the exact requested native indices/keys.
    let binding = scan_unretired(instance, expected.limits)?;
    if &binding.contract != expected {
        return Err(unavailable(
            "CLAP actual restored Note inputs differ from the prepared contract",
        ));
    }
    instance.access_shared_handler(|shared| shared.services.note_ports.retire_scanned(captured))?;
    Ok(binding)
}

// Full offer capture shares scan validation but retirement is postponed until
// the caller has compared its exact external contract. No blind marker clear.
fn scan_unretired(
    instance: &mut PluginInstance<ClapHost>,
    limits: AudioNoteExecutionLimits,
) -> Result<RestoredNoteInputs, AudioProcessorHostError> {
    scan_inner(instance, limits, None, false, false)
}

pub(super) fn service_names(
    instance: &mut PluginInstance<ClapHost>,
    expected: &RestoredNoteInputs,
) -> Result<(), AudioProcessorHostError> {
    let cohort = instance.access_shared_handler(|shared| shared.services.note_ports.snapshot());
    instance.access_shared_handler(|shared| shared.services.note_ports.check())?;
    if cohort.pending() {
        scan(instance, expected.contract.limits, Some(expected), true)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Render/state-bound offer from the separate actual native ProbeNotes operation.
/// This cannot stand in for concrete-instance revalidation or DSP qualification.
pub struct ClapNoteInputContractProbe {
    /// Additive response version, explicitly validated by the parent.
    pub schema_version: u32,
    /// Exact selected native definition.
    pub plugin_id: String,
    /// Fingerprint checked before and after the isolated operation.
    pub binary_sha256: [u8; 32],
    /// Supplied opaque state digest; None means no state was supplied.
    pub state_sha256: Option<[u8; 32]>,
    /// Exact selected render sample rate.
    pub sample_rate: u32,
    /// Exact selected render layout.
    pub channel_layout: mondrian_core::AudioChannelLayout,
    /// Exact selected maximum operation extent.
    pub max_block_frames: usize,
    /// Whether the actual inactive render-mode negotiation selected offline.
    pub offline: bool,
    /// Negotiated port/storage offer, not Note DSP/Session qualification.
    pub contract: AudioProcessorNoteInputContract,
}

pub(super) fn probe_loaded(
    entry: &PluginEntry,
    plugin_id: &str,
    binary_sha256: [u8; 32],
    state: Option<&[u8]>,
    render: AudioRenderContract,
) -> Result<ClapNoteInputContractProbe, AudioProcessorHostError> {
    super::clap_note_input_metadata::validate_request(plugin_id, state)?;
    if render.sample_rate == 0
        || u32::try_from(render.max_block_frames).ok().is_none_or(|frames| frames == 0)
    {
        return Err(invalid("CLAP Note contract probe render extent is invalid"));
    }
    let id = std::ffi::CString::new(plugin_id)
        .map_err(|_| invalid("CLAP Note plugin ID contains NUL"))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    validate_factory_plugin_id(&factory, &id)?;
    let info = HostInfo::new(
        "Mondrian",
        "Mondrian",
        "https://github.com/shaloong/mondrian",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| invalid(format!("CLAP host identity is invalid: {error}")))?;
    let shared = ClapHostShared::new_with_note_input();
    let native_shared = shared.clone().with_native_logs()?;
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| native_shared,
        |shared| super::clap_main_services::ClapHostMainThread::new(shared),
        entry,
        &id,
        &info,
    )
    .map_err(|error| unavailable(format!("CLAP Note probe instance failed: {error}")))?;
    shared.service_callbacks(&mut instance)?;
    if let Some(state) = state {
        let handle = instance.plugin_handle();
        let extension = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| unavailable("CLAP Note plugin lacks state restoration"))?;
        let mut reader = state;
        extension
            .load(&handle, &mut reader)
            .map_err(|error| unavailable(format!("CLAP Note state restore failed: {error}")))?;
    }
    shared.service_callbacks(&mut instance)?;
    configure_render_mode(&instance.plugin_handle(), render.processing_mode)?;
    shared.service_callbacks(&mut instance)?;
    let binding = scan(&mut instance, offered_limits(), None, false)?;
    let result = ClapNoteInputContractProbe {
        schema_version: 1,
        plugin_id: plugin_id.to_owned(),
        binary_sha256,
        state_sha256: super::clap_note_input_metadata::state_digest(state),
        sample_rate: render.sample_rate,
        channel_layout: render.channel_layout,
        max_block_frames: render.max_block_frames,
        offline: render.processing_mode == crate::AudioProcessingMode::Offline,
        contract: binding.contract,
    };
    drop(instance); // Inactive actual instance is destroyed before publishing.
    Ok(result)
}

#[cfg(test)]
#[path = "clap_note_input_contract_tests.rs"]
mod tests;
