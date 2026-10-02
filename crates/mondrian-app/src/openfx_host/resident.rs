//! Exclusive supervised native session, bounded control packets and shared frames.
use super::*;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::Child;

/// Hidden executable mode for a resident native Filter session.
pub const OPENFX_RESIDENT_WORKER_ARGUMENT: &str = "--internal-openfx-session-v1";
const ENDPOINT_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_ENDPOINT";
const NONCE_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_NONCE";
const VERSION: u16 = 1;
const POLL: Duration = Duration::from_millis(20);
const RETIRE: Duration = Duration::from_millis(100);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    version: u16,
    nonce: String,
    binary_path: PathBuf,
    bundle_path: PathBuf,
    binary_sha256: String,
    plugin_identifier: String,
    width: u32,
    height: u32,
    shared_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Render {
        sequence: u64,
        timing: OpenFxRenderTiming,
        parameters: BTreeMap<String, OpenFxParameterValue>,
    },
    Shutdown {},
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Connected { version: u16, nonce: String },
    Ready { version: u16, nonce: String },
    Rendered { sequence: u64, status: i32 },
    Closed {},
}

fn check_owner(
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    deadline: Instant,
) -> Result<(), OpenFxHostError> {
    checkpoint()?;
    if Instant::now() >= deadline {
        return Err(OpenFxHostError::DeadlineExceeded);
    }
    Ok(())
}

#[derive(Debug)]
struct ProcessOwner {
    child: Child,
    active: bool,
}
impl ProcessOwner {
    fn check(
        &mut self,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
        deadline: Instant,
    ) -> Result<(), OpenFxHostError> {
        check_owner(owner_checkpoint, deadline)?;
        if !self.active {
            return Err(unavailable("native session has been retired"));
        }
        match self.child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => {
                self.active = false;
                Err(OpenFxHostError::WorkerFailed(format!(
                    "native session exited ({status})"
                )))
            }
            Err(error) => Err(unavailable(format!(
                "native session observation failed: {error}"
            ))),
        }
    }
    fn terminate(&mut self) -> Result<(), OpenFxHostError> {
        if !self.active {
            return Ok(());
        }
        let _ = self.child.kill();
        self.child
            .wait()
            .map_err(|error| unavailable(format!("native session reap failed: {error}")))?;
        self.active = false;
        Ok(())
    }
}
impl Drop for ProcessOwner {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

/// One exclusive child and immutable pair of shared Float32 frame ranges.
///
/// Calls are serialized through mutable ownership. Only this owner writes input
/// before a request or reads output after its matching response. Drop retires and
/// reaps the child before releasing the mapping and its file. Native code never
/// runs in the application process.
#[derive(Debug)]
pub struct OpenFxFilterSession {
    process: ProcessOwner,
    control: TcpStream,
    shared: memmap2::MmapMut,
    _file: tempfile::NamedTempFile,
    inspection: OpenFxBinaryInspection,
    width: u32,
    height: u32,
    frame_bytes: usize,
    sequence: u64,
}

impl OpenFxFilterSession {
    /// Peak controlled frame storage for this session: two shared rasters,
    /// and the returned Float32 raster. Native image handles borrow mappings.
    ///
    /// This is a logical frame charge, counted once for shared physical pages.
    /// It excludes executable mappings, control metadata and arbitrary private
    /// allocations made by the installed plugin; it is not a process RSS cap.
    pub fn required_frame_storage(width: u32, height: u32) -> Result<usize, OpenFxHostError> {
        let count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| invalid("frame dimensions overflow"))?;
        validate_frame(width, height, count)?
            .checked_mul(3)
            .ok_or_else(|| invalid("native frame storage overflows"))
    }

    /// Prepare a pinned filter and fixed frame extent without loading it in the
    /// application. The child creates its native instance on the first render.
    /// The borrowed owner checkpoint is polled during preparation and every
    /// bounded transport wait; its cancellation/deadline authority is preserved.
    pub fn prepare(
        helper: &Path,
        inspection: &OpenFxBinaryInspection,
        identifier: &str,
        width: u32,
        height: u32,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<Self, OpenFxHostError> {
        owner_checkpoint()?;
        if !helper.is_absolute() || !helper.is_file() {
            return Err(invalid("helper must be an absolute regular file"));
        }
        let count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| invalid("frame dimensions overflow"))?;
        let frame_bytes = validate_frame(width, height, count)?;
        let bundle = verified_selection(inspection, identifier)?;
        let file = tempfile::NamedTempFile::new()
            .map_err(|error| unavailable(format!("shared frame creation failed: {error}")))?;
        file.as_file()
            .set_len((frame_bytes * 2) as u64)
            .map_err(|error| unavailable(format!("shared frame allocation failed: {error}")))?;
        // SAFETY: this exact-sized file is retained by the owner until child reap.
        // Disjoint ranges are accessed exclusively under request/response ownership.
        let shared = unsafe { MmapOptions::new().len(frame_bytes * 2).map_mut(file.as_file()) }
            .map_err(|error| unavailable(format!("shared frame mapping failed: {error}")))?;
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(control_error)?;
        listener.set_nonblocking(true).map_err(control_error)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let mut command = Command::new(helper);
        command
            .arg(OPENFX_RESIDENT_WORKER_ARGUMENT)
            .env(
                ENDPOINT_ENV,
                listener.local_addr().map_err(control_error)?.to_string(),
            )
            .env(NONCE_ENV, &nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut process = ProcessOwner {
            child: command.spawn().map_err(control_error)?,
            active: true,
        };
        let deadline = Instant::now() + DEADLINE;
        let mut control = loop {
            process.check(owner_checkpoint, deadline)?;
            match listener.accept() {
                Ok((stream, address)) if address.ip().is_loopback() => break stream,
                Ok(_) => return Err(unavailable("native session peer is not local")),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                Err(error) => return Err(control_error(error)),
            }
        };
        configure(&control, POLL)?;
        let prepare = Prepare {
            version: VERSION,
            nonce: nonce.clone(),
            binary_path: inspection.binary_path.clone(),
            bundle_path: bundle,
            binary_sha256: inspection.binary_sha256.clone(),
            plugin_identifier: identifier.to_owned(),
            width,
            height,
            shared_path: file.path().to_path_buf(),
        };
        let mut checkpoint = || process.check(owner_checkpoint, deadline);
        let connected: Response = read_packet(&mut control, &mut checkpoint)?;
        if !matches!(connected, Response::Connected { version: VERSION, nonce: ref value } if value == &nonce)
        {
            return Err(unavailable("native session peer did not authenticate"));
        }
        write_packet(&mut control, &prepare, &mut checkpoint)?;
        let ready: Response = read_packet(&mut control, &mut checkpoint)?;
        if !matches!(ready, Response::Ready { version: VERSION, nonce: ref value } if value == &nonce)
        {
            return Err(unavailable(
                "native session readiness envelope does not match",
            ));
        }
        checkpoint()?;
        Ok(Self {
            process,
            control,
            shared,
            _file: file,
            inspection: inspection.clone(),
            width,
            height,
            frame_bytes,
            sequence: 0,
        })
    }

    /// Child identity for lifecycle diagnostics; it remains stable across frames.
    pub fn worker_process_id(&self) -> u32 {
        self.process.child.id()
    }

    /// Execute one complete authored frame. Cancellation, deadline, native or
    /// protocol failure retires the worker before an output can be published.
    /// The owner checkpoint can return Canceled or DeadlineExceeded without
    /// creating an adapter-owned token or cancellation thread.
    pub fn render(
        &mut self,
        timing: &OpenFxRenderTiming,
        parameters: &BTreeMap<String, OpenFxParameterValue>,
        frame: &OpenFxFloatFrame,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<OpenFxFloatFrame, OpenFxHostError> {
        if frame.width != self.width || frame.height != self.height {
            return Err(invalid("frame extent differs from the native session"));
        }
        validate_render_request(timing, parameters, frame)?;
        let result = self.render_inner(timing, parameters, frame, owner_checkpoint);
        if result.is_err() {
            self.process.terminate()?;
        }
        result
    }

    fn render_inner(
        &mut self,
        timing: &OpenFxRenderTiming,
        parameters: &BTreeMap<String, OpenFxParameterValue>,
        frame: &OpenFxFloatFrame,
        owner_checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<OpenFxFloatFrame, OpenFxHostError> {
        let deadline = Instant::now() + DEADLINE;
        self.process.check(owner_checkpoint, deadline)?;
        if sha256_file(&self.inspection.binary_path)? != self.inspection.binary_sha256 {
            return Err(invalid("OpenFX binary changed before render"));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| unavailable("native session sequence exhausted"))?;
        // Check the owner at bounded copy intervals without a process syscall
        // for every pixel block. The child is idle until the request is sent.
        for (destination, source) in self.shared[..self.frame_bytes]
            .chunks_mut(1024 * 1024)
            .zip(frame.pixels.chunks(65536))
        {
            check_owner(owner_checkpoint, deadline)?;
            for (bytes, channel) in destination.chunks_exact_mut(4).zip(source.iter().flatten()) {
                bytes.copy_from_slice(&channel.to_ne_bytes());
            }
        }
        let request = Request::Render {
            sequence: self.sequence,
            timing: timing.clone(),
            parameters: parameters.clone(),
        };
        let mut checkpoint = || self.process.check(owner_checkpoint, deadline);
        write_packet(&mut self.control, &request, &mut checkpoint)?;
        let response: Response = read_packet(&mut self.control, &mut checkpoint)?;
        match response {
            Response::Rendered { sequence, status: 0 } if sequence == self.sequence => {}
            Response::Rendered { sequence, status } if sequence == self.sequence => {
                return Err(OpenFxHostError::WorkerFailed(format!(
                    "native status {status}"
                )))
            }
            _ => {
                return Err(unavailable(
                    "native session response sequence does not match",
                ))
            }
        }
        checkpoint()?;
        if sha256_file(&self.inspection.binary_path)? != self.inspection.binary_sha256 {
            return Err(invalid("OpenFX binary changed during render"));
        }
        let mut pixels = Vec::with_capacity(frame.pixels.len());
        for block in self.shared[self.frame_bytes..].chunks(1024 * 1024) {
            checkpoint()?;
            pixels.extend(block.chunks_exact(16).map(|pixel| {
                let channel = |index| {
                    f32::from_ne_bytes([
                        pixel[index],
                        pixel[index + 1],
                        pixel[index + 2],
                        pixel[index + 3],
                    ])
                };
                [channel(0), channel(4), channel(8), channel(12)]
            }));
        }
        if pixels.iter().flatten().any(|channel| !channel.is_finite()) {
            return Err(unavailable("render output contains a nonfinite channel"));
        }
        checkpoint()?;
        Ok(OpenFxFloatFrame { width: self.width, height: self.height, pixels })
    }

    /// Retire the native instance gracefully under a short deadline, then verify
    /// child exit. A failed or stalled shutdown is terminated and reaped.
    pub fn shutdown(&mut self) -> Result<(), OpenFxHostError> {
        if !self.process.active {
            return Ok(());
        }
        let deadline = Instant::now() + RETIRE;
        let graceful = (|| {
            let mut checkpoint = || {
                if Instant::now() >= deadline {
                    return Err(OpenFxHostError::DeadlineExceeded);
                }
                Ok(())
            };
            write_packet(&mut self.control, &Request::Shutdown {}, &mut checkpoint)?;
            let response: Response = read_packet(&mut self.control, &mut checkpoint)?;
            if !matches!(response, Response::Closed {}) {
                return Err(unavailable("native session did not close"));
            }
            loop {
                if let Some(status) = self.process.child.try_wait().map_err(control_error)? {
                    self.process.active = false;
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(OpenFxHostError::WorkerFailed(status.to_string()))
                    };
                }
                if Instant::now() >= deadline {
                    return Err(OpenFxHostError::DeadlineExceeded);
                }
                thread::sleep(Duration::from_millis(1));
            }
        })();
        self.process.terminate()?;
        graceful
    }
}
impl Drop for OpenFxFilterSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn configure(stream: &TcpStream, timeout: Duration) -> Result<(), OpenFxHostError> {
    stream.set_nodelay(true).map_err(control_error)?;
    stream.set_read_timeout(Some(timeout)).map_err(control_error)?;
    stream.set_write_timeout(Some(timeout)).map_err(control_error)
}
fn control_error(error: io::Error) -> OpenFxHostError {
    unavailable(format!("native session control failed: {error}"))
}
fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn read_exact_controlled(
    stream: &mut TcpStream,
    mut bytes: &mut [u8],
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<(), OpenFxHostError> {
    while !bytes.is_empty() {
        checkpoint()?;
        match stream.read(bytes) {
            Ok(0) => return Err(unavailable("native session control closed mid-packet")),
            Ok(count) => {
                bytes = &mut bytes[count..];
            }
            Err(error) if retry(&error) => {}
            Err(error) => return Err(control_error(error)),
        }
    }
    checkpoint()
}
fn read_packet<T: DeserializeOwned>(
    stream: &mut TcpStream,
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<T, OpenFxHostError> {
    let mut header = [0; 4];
    read_exact_controlled(stream, &mut header, checkpoint)?;
    read_body(stream, header, checkpoint)
}
fn read_body<T: DeserializeOwned>(
    stream: &mut TcpStream,
    header: [u8; 4],
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<T, OpenFxHostError> {
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_WORKER_REQUEST_BYTES {
        return Err(unavailable("native session packet exceeds its bound"));
    }
    let mut bytes = vec![0; length];
    read_exact_controlled(stream, &mut bytes, checkpoint)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| unavailable(format!("native session packet is malformed: {error}")))
}
fn read_idle_request(stream: &mut TcpStream) -> Result<Request, OpenFxHostError> {
    stream.set_read_timeout(None).map_err(control_error)?;
    let mut header = [0; 4];
    // Only the first byte may wait indefinitely. Once a packet starts, the
    // remaining header/body must arrive under the bounded operation deadline.
    stream.read_exact(&mut header[..1]).map_err(control_error)?;
    configure(stream, POLL)?;
    let deadline = Instant::now() + DEADLINE;
    let mut checkpoint = || {
        if Instant::now() >= deadline {
            Err(OpenFxHostError::DeadlineExceeded)
        } else {
            Ok(())
        }
    };
    read_exact_controlled(stream, &mut header[1..], &mut checkpoint)?;
    read_body(stream, header, &mut checkpoint)
}

fn write_packet<T: Serialize>(
    stream: &mut TcpStream,
    value: &T,
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<(), OpenFxHostError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| invalid(format!("native session packet encoding failed: {error}")))?;
    if bytes.is_empty() || bytes.len() > MAX_WORKER_REQUEST_BYTES {
        return Err(invalid("native session packet exceeds its bound"));
    }
    let mut packet = Vec::with_capacity(bytes.len() + 4);
    packet.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    packet.extend_from_slice(&bytes);
    let mut remaining = packet.as_slice();
    while !remaining.is_empty() {
        checkpoint()?;
        match stream.write(remaining) {
            Ok(0) => return Err(unavailable("native session control closed mid-packet")),
            Ok(count) => remaining = &remaining[count..],
            Err(error) if retry(&error) => {}
            Err(error) => return Err(control_error(error)),
        }
    }
    checkpoint()
}

fn write_response(stream: &mut TcpStream, response: &Response) -> Result<(), OpenFxHostError> {
    let deadline = Instant::now() + DEADLINE;
    write_packet(stream, response, &mut || {
        if Instant::now() >= deadline {
            Err(OpenFxHostError::DeadlineExceeded)
        } else {
            Ok(())
        }
    })
}

fn map_frames(
    prepare: &Prepare,
) -> Result<(fs::File, memmap2::Mmap, memmap2::MmapMut), OpenFxHostError> {
    let count = (prepare.width as usize)
        .checked_mul(prepare.height as usize)
        .ok_or_else(|| invalid("frame dimensions overflow"))?;
    let bytes = validate_frame(prepare.width, prepare.height, count)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&prepare.shared_path)
        .map_err(control_error)?;
    let metadata = file.metadata().map_err(control_error)?;
    if !metadata.is_file() || metadata.len() != (bytes * 2) as u64 {
        return Err(invalid("shared frame has an invalid size"));
    }
    // SAFETY: the parent keeps this exact file/extent and transfers exclusive
    // access through sequenced control messages. Native pointers never escape a call.
    // The parent's immutable published input is copied before each request.
    // Child Source pages are read-only; a violating native plugin is isolated
    // by the supervised process without a per-frame hash or extra raster.
    let source = unsafe { MmapOptions::new().len(bytes).map(&file) }.map_err(control_error)?;
    // SAFETY: this validated disjoint Output range has the same sequenced owner
    // and fixed file lifetime as Source; no parent access occurs during render.
    let output = unsafe { MmapOptions::new().offset(bytes as u64).len(bytes).map_mut(&file) }
        .map_err(control_error)?;
    Ok((file, source, output))
}

/// Run a resident native session before media, graphics or application startup.
pub fn run_openfx_resident_worker() -> Result<(), OpenFxHostError> {
    let endpoint: SocketAddr = std::env::var(ENDPOINT_ENV)
        .map_err(|_| invalid("native session endpoint is missing"))?
        .parse()
        .map_err(|_| invalid("native session endpoint is invalid"))?;
    if !endpoint.ip().is_loopback() {
        return Err(invalid("native session endpoint is not local"));
    }
    let nonce = std::env::var(NONCE_ENV).map_err(|_| invalid("native session nonce is missing"))?;
    if uuid::Uuid::parse_str(&nonce).is_err() {
        return Err(invalid("native session nonce is invalid"));
    }
    let mut control = TcpStream::connect_timeout(&endpoint, DEADLINE).map_err(control_error)?;
    configure(&control, POLL)?;
    let deadline = Instant::now() + DEADLINE;
    write_packet(
        &mut control,
        &Response::Connected { version: VERSION, nonce: nonce.clone() },
        &mut || {
            if Instant::now() >= deadline {
                Err(OpenFxHostError::DeadlineExceeded)
            } else {
                Ok(())
            }
        },
    )?;
    let prepare: Prepare = read_packet(&mut control, &mut || {
        if Instant::now() >= deadline {
            Err(OpenFxHostError::DeadlineExceeded)
        } else {
            Ok(())
        }
    })?;
    if prepare.version != VERSION || prepare.nonce != nonce {
        return Err(invalid(
            "native session preparation envelope does not match",
        ));
    }
    let (_file, input, mut output) = map_frames(&prepare)?;
    verify_worker_binary(
        &prepare.binary_path,
        &prepare.bundle_path,
        &prepare.binary_sha256,
    )?;
    let binary = native_string(&prepare.binary_path)?;
    let bundle = native_string(&prepare.bundle_path)?;
    let identifier = CString::new(prepare.plugin_identifier.as_str())
        .map_err(|_| invalid("plugin identifier contains a null byte"))?;
    write_response(&mut control, &Response::Ready { version: VERSION, nonce })?;
    let mut session: Option<NativeSession> = None;
    let mut sequence = 0_u64;
    loop {
        // Idle read is interrupted by parent EOF. Processing is supervised by
        // the parent, whose cancellation/deadline kills and reaps this process.
        let request = read_idle_request(&mut control)?;
        match request {
            Request::Shutdown {} => {
                drop(session.take());
                write_response(&mut control, &Response::Closed {})?;
                return Ok(());
            }
            Request::Render { sequence: received, timing, parameters } => {
                sequence = sequence
                    .checked_add(1)
                    .ok_or_else(|| invalid("native session sequence exhausted"))?;
                if received != sequence {
                    return Err(invalid("native session request sequence does not match"));
                }
                validate_timing(&timing)?;
                validate_parameters(&parameters)?;
                verify_worker_binary(
                    &prepare.binary_path,
                    &prepare.bundle_path,
                    &prepare.binary_sha256,
                )?;
                let strings = parameters
                    .iter()
                    .map(|(name, value)| {
                        let name = CString::new(name.as_str())
                            .map_err(|_| invalid("parameter name contains null"))?;
                        let text = if let OpenFxParameterValue::StrChoice(key) = value {
                            Some(
                                CString::new(key.as_str())
                                    .map_err(|_| invalid("StrChoice key contains null"))?,
                            )
                        } else {
                            None
                        };
                        Ok((name, text))
                    })
                    .collect::<Result<Vec<_>, OpenFxHostError>>()?;
                let native = parameters
                    .values()
                    .zip(&strings)
                    .map(|(value, (name, text))| {
                        let (kind, number) = match value {
                            OpenFxParameterValue::Double(value) => (1, *value),
                            OpenFxParameterValue::Boolean(value) => (2, f64::from(*value as u8)),
                            OpenFxParameterValue::Integer(value) => (3, f64::from(*value)),
                            OpenFxParameterValue::Choice(value) => (4, f64::from(*value)),
                            OpenFxParameterValue::StrChoice(_) => (5, 0.0),
                        };
                        NativeParameter {
                            name: name.as_ptr(),
                            kind,
                            value: number,
                            string_value: text
                                .as_ref()
                                .map_or(std::ptr::null(), |text| text.as_ptr()),
                            string_length: text.as_ref().map_or(0, |text| text.as_bytes().len()),
                        }
                    })
                    .collect::<Vec<_>>();
                let info = NativeFrameInfo {
                    width: prepare.width as c_int,
                    height: prepare.height as c_int,
                    frame: timing.frame,
                    frame_rate: timing.frame_rate,
                    first_frame: timing.first_frame,
                    last_frame: timing.last_frame,
                    pixel_aspect_ratio: timing.pixel_aspect_ratio,
                };
                let status = if let Some(instance) = session.as_mut() {
                    instance.render(&input, &mut output, &info, &native)
                } else {
                    match NativeSession::create(
                        &binary,
                        &bundle,
                        &identifier,
                        &input,
                        &info,
                        &native,
                    ) {
                        Ok(mut instance) => {
                            let status = instance.render(&input, &mut output, &info, &native);
                            session = Some(instance);
                            status
                        }
                        Err(status) => status,
                    }
                };
                write_response(&mut control, &Response::Rendered { sequence, status })?;
                if status != 0 {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let sender = TcpStream::connect(listener.local_addr().expect("address")).expect("sender");
        let (receiver, _) = listener.accept().expect("receiver");
        configure(&receiver, POLL).expect("bounded receiver");
        (sender, receiver)
    }

    #[test]
    fn partial_packets_survive_poll_timeouts_without_losing_bytes() {
        let (mut sender, mut receiver) = pair();
        let writer = thread::spawn(move || {
            let body =
                serde_json::to_vec(&Response::Rendered { sequence: 17, status: 0 }).expect("body");
            let header = (body.len() as u32).to_be_bytes();
            sender.write_all(&header[..2]).expect("partial header");
            thread::sleep(POLL * 2);
            sender.write_all(&header[2..]).expect("remaining header");
            for bytes in body.chunks(3) {
                sender.write_all(bytes).expect("fragment");
                thread::sleep(Duration::from_millis(1));
            }
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let response: Response = read_packet(&mut receiver, &mut || {
            if Instant::now() >= deadline {
                Err(OpenFxHostError::DeadlineExceeded)
            } else {
                Ok(())
            }
        })
        .expect("complete fragmented response");
        assert!(matches!(
            response,
            Response::Rendered { sequence: 17, status: 0 }
        ));
        writer.join().expect("writer");
    }

    #[test]
    fn packet_bounds_truncation_and_unknown_fields_fail_closed() {
        for length in [0_u32, MAX_WORKER_REQUEST_BYTES as u32 + 1] {
            let (mut sender, mut receiver) = pair();
            sender.write_all(&length.to_be_bytes()).expect("header");
            assert!(
                matches!(read_packet::<Response>(&mut receiver, &mut || Ok(())), Err(OpenFxHostError::Unavailable(reason)) if reason.contains("bound"))
            );
        }
        let (mut sender, mut receiver) = pair();
        sender.write_all(&[0, 0]).expect("truncated header");
        drop(sender);
        assert!(
            matches!(read_packet::<Response>(&mut receiver, &mut || Ok(())), Err(OpenFxHostError::Unavailable(reason)) if reason.contains("mid-packet"))
        );
        let (mut sender, mut receiver) = pair();
        let body = br#"{"kind":"closed","unexpected":1}"#;
        sender.write_all(&(body.len() as u32).to_be_bytes()).expect("header");
        sender.write_all(body).expect("body");
        assert!(
            matches!(read_packet::<Response>(&mut receiver, &mut || Ok(())), Err(OpenFxHostError::Unavailable(reason)) if reason.contains("malformed"))
        );
    }

    #[test]
    fn cancellation_interrupts_a_stalled_partial_packet() {
        let (mut sender, mut receiver) = pair();
        sender.write_all(&100_u32.to_be_bytes()).expect("header");
        sender.write_all(b"partial").expect("partial body");
        let cancellation = mondrian_core::ExecutionCancellationToken::new();
        let worker_token = cancellation.clone();
        let canceler = thread::spawn(move || {
            thread::sleep(POLL * 3);
            worker_token.cancel();
        });
        let started = Instant::now();
        let result = read_packet::<Response>(&mut receiver, &mut || {
            if cancellation.is_canceled() {
                Err(OpenFxHostError::Canceled)
            } else if started.elapsed() > Duration::from_secs(2) {
                Err(OpenFxHostError::DeadlineExceeded)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(OpenFxHostError::Canceled)));
        canceler.join().expect("canceler");
    }

    #[test]
    fn worker_rejects_truncated_and_oversized_shared_frames_before_native_loading() {
        for size in [0, 16, 31, 33, 64] {
            let file = tempfile::NamedTempFile::new().expect("shared file");
            file.as_file().set_len(size).expect("malformed extent");
            let prepare = Prepare {
                version: VERSION,
                nonce: String::new(),
                binary_path: PathBuf::from("never-loaded.ofx"),
                bundle_path: PathBuf::from("never-loaded.ofx.bundle"),
                binary_sha256: String::new(),
                plugin_identifier: "test".to_owned(),
                width: 1,
                height: 1,
                shared_path: file.path().to_path_buf(),
            };
            assert!(
                matches!(map_frames(&prepare), Err(OpenFxHostError::Invalid(reason)) if reason == "shared frame has an invalid size")
            );
        }
    }
}
