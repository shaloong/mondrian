use super::*;
use mondrian_broadcast::{
    AncillaryField, AncillaryPlacement, AncillarySpace, AncillaryWireCorrelation,
    CapturedAncillaryPacket,
};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn independent_capture_devices(output: &NativeDevice, input: &NativeDevice, mode: u32) -> bool {
    mode < MODES.len() as u32
        && output.serial != input.serial
        && output.physical_group != 0
        && input.physical_group != 0
        && output.physical_group != input.physical_group
        && input.input_mode_mask & (1 << mode) != 0
}

/// Explicit independent-card SDI readback configuration for validation only.
#[derive(Debug, Clone)]
pub struct DeckLinkWireReadbackConfiguration {
    /// Receiver card's exact discovered stable identity, different from output.
    pub device_id: ReferenceOutputDeviceId,
    /// Receiver discovery generation that must still match.
    pub device_generation: u64,
    /// Exact SDI signal physically wired from output SDI endpoint to receiver SDI endpoint.
    pub signal: ReferenceOutputSignal,
    /// Owner of a real canonical correlation packet in every output frame.
    pub correlation: AncillaryWireCorrelation,
    /// Existing directory for create-only full output/capture JSONL receipts.
    pub receipt_directory: PathBuf,
    /// Explicit per-session journal bound; exceeding it fails the session.
    pub maximum_receipt_bytes: u64,
    /// Optional immutable program and consumed journal publication owner.
    pub program_journal: Option<crate::NativeAncillaryJournalBinding>,
}
impl DeckLinkWireReadbackConfiguration {
    pub(super) fn serial(&self) -> Result<u64, ReferenceOutputAdapterError> {
        let raw = self
            .device_id
            .as_str()
            .strip_prefix("decklink:")
            .and_then(|id| id.strip_suffix(":sdi"))
            .ok_or_else(|| {
                vendor(
                    "wire-plan",
                    "receiver must be an exact DeckLink SDI endpoint identity",
                )
            })?;
        u64::from_str_radix(raw, 16).map_err(|error| vendor("wire-plan", error.to_string()))
    }
    pub(super) fn mode(&self) -> Result<u32, ReferenceOutputAdapterError> {
        MODES
            .iter()
            .position(|(w, h, n, d)| {
                *w == self.signal.width
                    && *h == self.signal.height
                    && Rational::new(*n, *d) == self.signal.frame_rate
            })
            .map(|mode| mode as u32)
            .ok_or(ReferenceOutputAdapterError::ModeUnsupported)
    }
    pub(super) fn validate(&self) -> Result<(), ReferenceOutputAdapterError> {
        self.signal
            .validate()
            .map_err(|error| vendor("wire-signal", error.to_string()))?;
        let _ = self.mode()?;
        if let Some(binding) = &self.program_journal {
            binding.validate().map_err(|error| vendor("wire-program", error))?;
        }

        AncillaryWireCorrelation::new(*self.correlation.nonce(), self.correlation.placement())
            .map_err(|error| vendor("wire-plan", error.to_string()))?;
        if self.serial()? == 0
            || self.device_generation == 0
            || !self.receipt_directory.is_absolute()
            || self.maximum_receipt_bytes < 4096
            || self.maximum_receipt_bytes > (1u64 << 40)
            || self.correlation.placement().horizontal_offset as u32 + 39 > self.signal.width
        {
            return Err(vendor(
                "wire-plan",
                "invalid receiver, marker or journal bound",
            ));
        }
        let metadata = std::fs::symlink_metadata(&self.receipt_directory)
            .map_err(|error| vendor("wire-journal", error.to_string()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(vendor(
                "wire-journal",
                "receipt directory must be an existing direct directory",
            ));
        }
        Ok(())
    }
    pub(super) fn preflight(
        &self,
        runtime: &NativeRuntime,
        devices: &HashMap<ReferenceOutputDeviceId, NativeDevice>,
        output_serial: u64,
    ) -> Result<(), ReferenceOutputAdapterError> {
        self.validate()?;
        let device = devices
            .get(&self.device_id)
            .ok_or(ReferenceOutputAdapterError::DeviceUnavailable)?;
        let output = devices
            .values()
            .find(|item| item.serial == output_serial)
            .ok_or(ReferenceOutputAdapterError::DeviceUnavailable)?;
        if !independent_capture_devices(output, device, self.mode()?)
            || device.serial != self.serial()?
            || device.generation != self.device_generation
        {
            return Err(vendor(
                "wire-preflight",
                "receiver must be a distinct, unchanged physical card",
            ));
        }
        let mut error = [0u8; ERROR_BYTES];
        if unsafe {
            (runtime.capture_preflight)(
                device.serial,
                device.generation,
                self.mode()?,
                u32::from(self.correlation.placement().line),
                u32::from(self.correlation.placement().horizontal_offset),
                error.as_mut_ptr(),
                ERROR_BYTES as u32,
            )
        } != 0
        {
            return Err(vendor("wire-preflight", native_text(&error)));
        }
        Ok(())
    }
}
#[repr(C)]
pub(super) struct NativeWirePacket {
    line: u32,
    offset: u32,
    count: u32,
    reserved: u32,
    words: [u16; 262],
}
#[repr(C)]
pub(super) struct NativeWireFrame {
    capture_ticks: u64,
    count: u32,
    reserved: u32,
    packets: [NativeWirePacket; 64],
}
pub(super) type ScheduleVancFn = unsafe extern "C" fn(
    *mut c_void,
    u64,
    *const u8,
    u32,
    u32,
    *const i32,
    u32,
    *const NativeWirePacket,
    u32,
    *mut u8,
    u32,
) -> i32;
pub(super) type CapturePreflightFn =
    unsafe extern "C" fn(u64, u64, u32, u32, u32, *mut u8, u32) -> i32;
pub(super) type CaptureOpenFn =
    unsafe extern "C" fn(u64, u64, u32, u32, *mut *mut c_void, *mut u8, u32) -> i32;
pub(super) type CapturePollFn =
    unsafe extern "C" fn(*mut c_void, *mut NativeWireFrame, *mut u8, u32) -> i32;

pub(super) struct WireSession {
    runtime: Arc<NativeRuntime>,
    raw: Option<usize>,
    configuration: DeckLinkWireReadbackConfiguration,
    journal: File,
    journal_path: PathBuf,
    journal_bytes: u64,
    journal_digest: crate::ancillary_journal::NativeJournalDigest,
    limit: usize,
    unmatched: usize,
    marker_seen: bool,
    last_received_index: Option<u64>,
    expected: BTreeMap<u64, AncillaryFrame>,
    received: BTreeMap<u64, (u64, Vec<CapturedAncillaryPacket>)>,
    completed: VecDeque<ReferenceOutputAdapterEvent>,
    open_error: Option<String>,
}
impl WireSession {
    pub(super) fn open(
        runtime: Arc<NativeRuntime>,
        configuration: DeckLinkWireReadbackConfiguration,
        devices: &HashMap<ReferenceOutputDeviceId, NativeDevice>,
        output: u64,
        mode: u32,
        limit: u32,
    ) -> Result<Self, ReferenceOutputAdapterError> {
        configuration.preflight(&runtime, devices, output)?;
        if configuration.mode()? != mode {
            return Err(ReferenceOutputAdapterError::ModeUnsupported);
        }
        let (output_id, output_device) = devices
            .iter()
            .find(|(_, device)| device.serial == output)
            .ok_or_else(|| vendor("wire-open", "output discovery identity missing"))?;
        let mut identity = serde_json::json!({"schema_version":1,"event":"session_identity",
            "output_device":output_id,"output_generation":output_device.generation,
            "output_driver":native_text(&output_device.driver),
            "receiver_device":configuration.device_id,"receiver_generation":configuration.device_generation,
            "signal":configuration.signal,"correlation":configuration.correlation,
            "native_image_sha256":runtime.sha256,"sdk_version":runtime.version});
        if let Some(binding) = &configuration.program_journal {
            identity["ancillary_program_sha256"] =
                serde_json::json!(binding.ancillary_program_sha256);
            identity["phase_id"] = serde_json::json!(binding.phase_id);
        }
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let nonce = configuration
            .correlation
            .nonce()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut journal = None;
        let mut journal_path = PathBuf::new();
        for _ in 0..1024 {
            let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
            let path = configuration
                .receipt_directory
                .join(format!("decklink-wire-{nonce}-{serial:08}.jsonl"));
            use std::os::windows::fs::OpenOptionsExt;
            match OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(1)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    journal_path = path;
                    journal = Some(file);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(vendor("wire-journal", error.to_string())),
            }
        }
        let journal = journal
            .ok_or_else(|| vendor("wire-journal", "create-only journal namespace exhausted"))?;
        let mut raw = std::ptr::null_mut();
        let mut error = [0u8; ERROR_BYTES];
        let result = unsafe {
            (runtime.capture_open)(
                configuration.serial()?,
                configuration.device_generation,
                mode,
                limit,
                &mut raw,
                error.as_mut_ptr(),
                ERROR_BYTES as u32,
            )
        };
        if raw.is_null() {
            return Err(vendor("wire-open", native_text(&error)));
        }
        let mut owner = Self {
            runtime,
            raw: Some(raw as usize),
            configuration,
            journal,
            journal_path,
            journal_bytes: 0,
            journal_digest: Default::default(),
            limit: limit as usize,
            unmatched: 0,
            marker_seen: false,
            last_received_index: None,
            expected: BTreeMap::new(),
            received: BTreeMap::new(),
            completed: VecDeque::new(),
            open_error: None,
        };
        if result != 0 {
            owner.open_error = Some(native_text(&error));
        }
        if let Err(error) = owner.write_receipt(&identity) {
            owner.open_error = Some(format!("wire session identity journal failed: {error}"));
        }
        Ok(owner)
    }
    pub(super) fn open_error(&self) -> Option<&str> {
        self.open_error.as_deref()
    }
    pub(super) fn start(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        let raw = self.raw.ok_or(ReferenceOutputAdapterError::SessionStopped)?;
        let mut error = [0u8; ERROR_BYTES];
        if unsafe {
            (self.runtime.capture_start)(raw as *mut c_void, error.as_mut_ptr(), ERROR_BYTES as u32)
        } != 0
        {
            return Err(vendor("wire-start", native_text(&error)));
        }
        Ok(())
    }
    pub(super) fn prepare(
        &self,
        frame: &AncillaryFrame,
    ) -> Result<Vec<NativeWirePacket>, ReferenceOutputAdapterError> {
        if self.expected.len() >= self.limit {
            return Err(ReferenceOutputAdapterError::Backpressure);
        }
        let captured = wire_words(frame);
        if self
            .configuration
            .correlation
            .captured_frame_index(&captured)
            .map_err(|error| vendor("wire-marker", error.to_string()))?
            != frame.frame_index()
        {
            return Err(vendor(
                "wire-marker",
                "canonical frame does not own its marker",
            ));
        }
        captured
            .iter()
            .map(|packet| {
                if packet.placement.space != AncillarySpace::Vanc
                    || packet.placement.field != AncillaryField::Progressive
                    || packet.component_words.len() > 262
                {
                    return Err(vendor(
                        "wire-schedule",
                        "only exact progressive luma VANC is admitted",
                    ));
                }
                let mut words = [0; 262];
                words[..packet.component_words.len()].copy_from_slice(&packet.component_words);
                Ok(NativeWirePacket {
                    line: u32::from(packet.placement.line),
                    offset: u32::from(packet.placement.horizontal_offset),
                    count: packet.component_words.len() as u32,
                    reserved: 0,
                    words,
                })
            })
            .collect()
    }
    pub(super) fn scheduled(
        &mut self,
        frame: AncillaryFrame,
    ) -> Result<(), ReferenceOutputAdapterError> {
        let value = serde_json::json!({"schema_version":1,"event":"output_scheduled","frame_index":frame.frame_index(),
            "output_words":wire_words(&frame),"output_sha256":frame.sha256()});
        self.write_receipt(&value)?;
        if self.expected.insert(frame.frame_index(), frame).is_some() {
            return Err(vendor("wire-schedule", "duplicate canonical frame"));
        }
        Ok(())
    }
    pub(super) fn completed(
        &mut self,
        event: ReferenceOutputAdapterEvent,
    ) -> Result<(), ReferenceOutputAdapterError> {
        if self.completed.len() >= self.limit {
            return Err(vendor(
                "wire-completion",
                "independent readback did not drain",
            ));
        }
        self.completed.push_back(event);
        Ok(())
    }
    pub(super) fn poll_capture(&mut self) -> Result<(), ReferenceOutputAdapterError> {
        // Four reads bound one product poll even when the capture ring is full.
        for _ in 0..4 {
            let mut frame: NativeWireFrame = unsafe { std::mem::zeroed() };
            let mut error = [0u8; ERROR_BYTES];
            let raw = self.raw.ok_or(ReferenceOutputAdapterError::SessionStopped)?;
            let result = unsafe {
                (self.runtime.capture_poll)(
                    raw as *mut c_void,
                    &mut frame,
                    error.as_mut_ptr(),
                    ERROR_BYTES as u32,
                )
            };
            if result == 0 {
                break;
            }
            if result != 1 || frame.count > 64 || frame.capture_ticks == 0 || frame.reserved != 0 {
                return Err(vendor("wire-capture", native_text(&error)));
            }
            let mut packets = Vec::new();
            for packet in &frame.packets[..frame.count as usize] {
                if packet.count < 7 || packet.count > 262 || packet.reserved != 0 {
                    return Err(vendor("wire-capture", "invalid captured packet extent"));
                }
                let line = u16::try_from(packet.line)
                    .map_err(|_| vendor("wire-capture", "line overflow"))?;
                let offset = u16::try_from(packet.offset)
                    .map_err(|_| vendor("wire-capture", "offset overflow"))?;
                packets.push(CapturedAncillaryPacket {
                    placement: AncillaryPlacement::new(
                        AncillarySpace::Vanc,
                        AncillaryField::Progressive,
                        line,
                        offset,
                    )
                    .map_err(|error| vendor("wire-capture", error.to_string()))?,
                    component_words: packet.words[..packet.count as usize].to_vec(),
                });
            }
            let index = match self.configuration.correlation.captured_frame_index(&packets) {
                Ok(index) => index,
                Err(
                    mondrian_broadcast::AncillaryWireError::MissingMarker
                    | mondrian_broadcast::AncillaryWireError::InvalidCorrelation,
                ) if !self.marker_seen && self.unmatched < self.limit * 2 => {
                    self.unmatched += 1;
                    continue;
                }
                Err(error) => return Err(vendor("wire-correlation", error.to_string())),
            };
            self.marker_seen = true;
            if self.last_received_index.is_some_and(|previous| index <= previous) {
                return Err(vendor(
                    "wire-correlation",
                    "physical capture repeated or reordered a canonical frame",
                ));
            }
            self.last_received_index = Some(index);
            if !self.expected.contains_key(&index)
                || self.received.len() >= self.limit
                || self.received.insert(index, (frame.capture_ticks, packets)).is_some()
            {
                return Err(vendor(
                    "wire-correlation",
                    "unexpected, repeated or unbounded captured frame",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn ready(
        &mut self,
    ) -> Result<Option<ReferenceOutputAdapterEvent>, ReferenceOutputAdapterError> {
        let Some(ReferenceOutputAdapterEvent::FrameCompleted {
            frame_index, hardware_time, ..
        }) = self.completed.front()
        else {
            return Ok(None);
        };
        let frame_index = *frame_index;
        let hardware_time = *hardware_time;
        let Some((capture_ticks, captured)) = self.received.remove(&frame_index) else {
            return Ok(None);
        };
        let expected = self
            .expected
            .remove(&frame_index)
            .ok_or_else(|| vendor("wire-correlation", "output owner missing"))?;
        let actual = self
            .configuration
            .correlation
            .verify_capture(&expected, &captured)
            .map_err(|error| vendor("wire-verify", error.to_string()))?;
        let receipt = serde_json::json!({"schema_version":1,"event":"independent_capture_verified","frame_index":frame_index,
            "receiver_device":self.configuration.device_id,"receiver_generation":self.configuration.device_generation,
            "receiver_hardware_ticks":capture_ticks,"receiver_ticks_per_second":48000,"output_hardware_time":hardware_time,
            "output_words":wire_words(&expected),"captured_words":captured,"output_sha256":expected.sha256(),"captured_sha256":actual.sha256()});
        self.write_receipt(&receipt)?;
        self.completed.pop_front();
        Ok(Some(ReferenceOutputAdapterEvent::FrameCompleted {
            frame_index,
            hardware_time,
            ancillary_readback_sha256: Some(actual.sha256()),
        }))
    }
    fn write_receipt(
        &mut self,
        value: &serde_json::Value,
    ) -> Result<(), ReferenceOutputAdapterError> {
        let mut bytes =
            serde_json::to_vec(value).map_err(|error| vendor("wire-journal", error.to_string()))?;
        bytes.push(b'\n');
        let Some(next) = self
            .journal_bytes
            .checked_add(bytes.len() as u64)
            .filter(|bytes| *bytes <= self.configuration.maximum_receipt_bytes)
        else {
            self.journal_digest.invalidate();
            return Err(vendor(
                "wire-journal",
                "full wire receipt byte budget exceeded",
            ));
        };
        self.journal_digest
            .write(&mut self.journal, &bytes)
            .map_err(|error| vendor("wire-journal", error.to_string()))?;
        self.journal_bytes = next;
        Ok(())
    }
    pub(super) fn consume_into(&mut self, output: &mut NativeShutdown) {
        if let Some(raw) = self.raw.take() {
            let mut capture = empty_shutdown();
            let result =
                unsafe { (self.runtime.capture_shutdown)(raw as *mut c_void, &mut capture) };
            output.stopped &= capture.stopped;
            output.requested &= capture.requested;
            output.released &= capture.released;
            output.worker_joined &= capture.worker_joined;
            output.outstanding_resources = output
                .outstanding_resources
                .saturating_add(capture.outstanding_resources)
                .saturating_add(u64::from(result != 0));
            if result != 0 || capture.outstanding_resources != 0 {
                output.error = capture.error;
            }
        }
        let published = self.journal.sync_all().and_then(|()| {
            if output.requested != 1
                || output.stopped != 1
                || output.worker_joined != 1
                || output.released != 1
                || output.outstanding_frames != 0
                || output.outstanding_resources != 0
                || output.error[0] != 0
            {
                return Ok(());
            }
            if let Some(binding) = &self.configuration.program_journal {
                let sha256 = self.journal_digest.finish(self.journal.metadata()?.len(), true)?;
                binding
                    .inventory
                    .publish(
                        binding.phase_id.clone(),
                        crate::NativeAncillaryJournalReceipt {
                            path: self.journal_path.clone(),
                            sha256,
                        },
                    )
                    .map_err(std::io::Error::other)?;
            }
            Ok(())
        });
        if let Err(error) = published {
            output.outstanding_resources = output.outstanding_resources.saturating_add(1);
            let bytes = error.to_string();
            let length = bytes.len().min(output.error.len() - 1);
            output.error[..length].copy_from_slice(&bytes.as_bytes()[..length]);
            output.error[length] = 0;
        }
    }
}
fn empty_shutdown() -> NativeShutdown {
    NativeShutdown {
        requested: 1,
        stopped: 1,
        worker_joined: 1,
        released: 1,
        outstanding_frames: 0,
        outstanding_resources: 0,
        error: [0; 512],
    }
}
fn wire_words(frame: &AncillaryFrame) -> Vec<CapturedAncillaryPacket> {
    frame
        .packets()
        .iter()
        .map(|packet| CapturedAncillaryPacket {
            placement: packet.placement,
            component_words: packet.packet.component_words(),
        })
        .collect()
}
impl Drop for WireSession {
    fn drop(&mut self) {
        let Some(raw) = self.raw.take() else {
            return;
        };
        // A consuming parent handles normal closure. Abandonment is reaped away
        // from the product caller while retaining the executable mapping.
        let owner = Box::into_raw(Box::new((Arc::clone(&self.runtime), raw))) as usize;
        let spawn = std::thread::Builder::new()
            .name("decklink-abandoned-capture".to_owned())
            .spawn(move || {
                let owner = unsafe { Box::from_raw(owner as *mut (Arc<NativeRuntime>, usize)) };
                let mut facts = empty_shutdown();
                unsafe {
                    (owner.0.capture_shutdown)(owner.1 as *mut c_void, &mut facts);
                }
                if facts.worker_joined != 1
                    || facts.released != 1
                    || facts.outstanding_resources != 0
                {
                    std::mem::forget(Arc::clone(&owner.0));
                }
            });
        drop(spawn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_receiver_rejects_same_card_subdevice_and_output_only_port() {
        let output = NativeDevice {
            serial: 1,
            generation: 1,
            physical_group: 10,
            mode_mask: 1,
            input_mode_mask: 1,
            flags: 3,
            maximum_audio_channels: 8,
            driver: [0; 96],
            name: [0; 128],
        };
        let same_card_port = NativeDevice { serial: 2, ..output };
        assert!(!independent_capture_devices(&output, &same_card_port, 0));
        let independent = NativeDevice { serial: 3, physical_group: 11, ..output };
        assert!(independent_capture_devices(&output, &independent, 0));
        assert!(!independent_capture_devices(
            &output,
            &independent,
            u32::MAX
        ));
        assert!(!independent_capture_devices(
            &output,
            &NativeDevice { input_mode_mask: 0, ..independent },
            0
        ));
        assert!(!independent_capture_devices(
            &output,
            &NativeDevice { physical_group: 0, ..independent },
            0
        ));
    }
}
