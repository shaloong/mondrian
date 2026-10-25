//! The sole TCP reader/writer for one supervised native worker.
//!
//! Native actions stay on the dispatch thread. This owner keeps one request,
//! one completion, one coalesced POD progress value and two packet cursors.
use super::*;
use std::net::Shutdown;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub(super) const PUMP_STACK_BYTES: usize = 8 * 1024 * 1024;
const PUMP_GUARD_BYTES: usize = 64 * 1024;
const IO_QUANTUM_BYTES: usize = 64 * 1024;
const IDLE_POLL: Duration = Duration::from_millis(1);

/// Additional known logical storage, not a claim about OS/CRT or physical RSS.
pub(crate) fn storage_bytes() -> Option<usize> {
    PUMP_STACK_BYTES
        .checked_add(PUMP_GUARD_BYTES)?
        .checked_add(std::mem::size_of::<Shared>())?
        .checked_add(std::mem::size_of::<WorkerPump>())?
        .checked_add(std::mem::size_of::<PacketReader>().checked_mul(2)?)?
        .checked_add(std::mem::size_of::<PacketWriter>())?
        .checked_add(std::mem::size_of::<PendingWrite>())?
        // Declared logical capacities: one reader and writer in each process.
        // Existing bounded terminal/request metadata policy remains separate.
        .checked_add(MAX_WORKER_REQUEST_BYTES.checked_mul(4)?.checked_add(8)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Queued,
    Executing,
    Completed,
}

struct Slot {
    phase: Phase,
    serial: u64,
    last_serial: u64,
    render_invocation: u64,
    last_render_invocation: u64,
    request: Option<Request>,
    response: Option<Response>,
    progress: Option<NativeLiveProgress>,
    progress_revision: u64,
    sent_revision: u64,
    fault: Option<String>,
    completion_generation: u64,
    acknowledged_generation: u64,

    questions: question::QuestionSlot,
    memory: Option<MemoryRequest>,
    memory_requested: bool,
}

#[derive(Clone, Copy)]
struct MemoryRequest {
    bytes: usize,
    sent: bool,
    reply: Option<bool>,
}

pub(super) struct Shared {
    owner: mondrian_core::EffectProcessorOwnerId,
    gate: Mutex<Slot>,
    ready: Condvar,
    canceled: AtomicBool,
    stopping: AtomicBool,
    gui_open: AtomicBool,
    interactive_render: AtomicBool,
    question_work: AtomicBool,
    #[cfg(test)]
    outbound_blocked: AtomicBool,
}

impl Shared {
    fn new(owner: mondrian_core::EffectProcessorOwnerId) -> Self {
        Self {
            owner,
            gate: Mutex::new(Slot {
                phase: Phase::Idle,
                serial: 0,
                last_serial: 0,
                render_invocation: 0,
                last_render_invocation: 0,
                request: None,
                response: None,
                progress: None,
                progress_revision: 0,
                sent_revision: 0,
                fault: None,
                completion_generation: 0,
                acknowledged_generation: 0,

                questions: question::QuestionSlot::default(),
                memory: None,
                memory_requested: false,
            }),
            ready: Condvar::new(),
            canceled: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            gui_open: AtomicBool::new(false),
            interactive_render: AtomicBool::new(false),
            question_work: AtomicBool::new(false),
            #[cfg(test)]
            outbound_blocked: AtomicBool::new(false),
        }
    }

    /// No native pointers, rasters or borrowed parent closures cross this seam.
    pub(super) fn native_control(&self) -> NativeInvocationControl {
        NativeInvocationControl {
            context: std::ptr::from_ref(self).cast(),
            abort: Some(native_abort),
            progress: Some(native_progress),
            gui_open: c_int::from(self.gui_open.load(Ordering::Acquire)),
            interactive_render: c_int::from(self.interactive_render.load(Ordering::Acquire)),
            question_work: c_int::from(self.question_work.load(Ordering::Acquire)),
            question: Some(native_question),
            acquire_memory: Some(native_acquire_memory),
        }
    }

    fn admit(&self, request: Request) -> Result<(), OpenFxHostError> {
        let mut slot = self.gate.lock().map_err(|_| unavailable("native pump gate poisoned"))?;
        if let Request::Cancel { owner, sequence } = request {
            if owner != self.owner
                || sequence == 0
                || sequence != slot.serial
                || !matches!(
                    slot.phase,
                    Phase::Queued | Phase::Executing | Phase::Completed
                )
            {
                return Err(invalid("cancel does not identify the current native work"));
            }
            self.canceled.store(true, Ordering::Release);
            self.ready.notify_all();
            return Ok(());
        }
        if let Request::QuestionReply { owner, sequence, identity, reply } = request {
            if owner != self.owner
                || sequence == 0
                || sequence != slot.serial
                || slot.phase != Phase::Executing
                || identity.invocation != slot.render_invocation
                || !slot.questions.answer(identity, reply)
            {
                return Err(invalid("Question reply lost its published current service"));
            }
            self.ready.notify_all();
            return Ok(());
        }
        if let Request::MemoryReply { owner, sequence, required_bytes, granted } = request {
            if owner != self.owner
                || sequence == 0
                || sequence != slot.serial
                || slot.phase != Phase::Executing
                || slot.memory.is_none_or(|value| {
                    !value.sent || value.bytes != required_bytes || value.reply.is_some()
                })
            {
                return Err(invalid("memory reply lost its current allocation request"));
            }
            slot.memory.as_mut().expect("checked memory request").reply = Some(granted);
            self.ready.notify_all();
            return Ok(());
        }
        if slot.phase != Phase::Idle {
            return Err(invalid("native work already owns the slot"));
        }
        if slot.completion_generation == u64::MAX {
            return Err(unavailable(
                "native completion generation exhausted before work admission",
            ));
        }
        if let Request::Render {
            owner,
            sequence,
            gui_open,
            interactive_render,
            question_work,
            ..
        } = &request
        {
            if *owner != self.owner
                || slot.last_serial.checked_add(1) != Some(*sequence)
                || ((*question_work || *interactive_render) && !*gui_open)
            {
                return Err(invalid("native request owner or sequence does not match"));
            }
            // TCP action serial and native Render invocation are distinct.
            // Idle GUI actions advance only the former. This sole accepted
            // Render binds both coordinates before any native callback runs.
            let invocation = slot.last_render_invocation.checked_add(1).ok_or_else(|| {
                unavailable("native Render invocation exhausted before admission")
            })?;
            slot.render_invocation = invocation;
            slot.last_render_invocation = invocation;
            slot.serial = *sequence;
            slot.last_serial = *sequence;
            self.gui_open.store(*gui_open, Ordering::Release);
            self.interactive_render.store(*interactive_render, Ordering::Release);
            self.question_work.store(*question_work, Ordering::Release);
        }
        if let Request::CloseGui { owner, sequence } = &request {
            if *owner != self.owner || slot.last_serial.checked_add(1) != Some(*sequence) {
                return Err(invalid(
                    "GUI action does not identify the current owner/serial",
                ));
            }
            slot.render_invocation = 0;
            slot.serial = *sequence;
            slot.last_serial = *sequence;
            self.gui_open.store(false, Ordering::Release);
            self.interactive_render.store(false, Ordering::Release);
            self.question_work.store(false, Ordering::Release);
        }
        self.canceled.store(false, Ordering::Release);
        slot.memory = None;
        slot.memory_requested = false;
        slot.progress = None;
        slot.sent_revision = slot.progress_revision;
        slot.phase = Phase::Queued;
        slot.request = Some(request);
        self.ready.notify_all();
        Ok(())
    }

    pub(super) fn take_request(&self) -> Result<Request, OpenFxHostError> {
        let mut slot = self.gate.lock().map_err(|_| unavailable("native pump gate poisoned"))?;
        loop {
            if let Some(fault) = &slot.fault {
                return Err(unavailable(fault.clone()));
            }
            if self.stopping.load(Ordering::Acquire) {
                return Err(unavailable("native pump stopped"));
            }
            if slot.phase == Phase::Queued {
                // Activation never resets a cancel already accepted while queued.
                slot.phase = Phase::Executing;
                return slot
                    .request
                    .take()
                    .ok_or_else(|| unavailable("queued native request absent"));
            }
            slot = self.ready.wait(slot).map_err(|_| unavailable("native pump wait poisoned"))?;
        }
    }

    pub(super) fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Acquire)
    }

    pub(super) fn complete(&self, response: Response) -> Result<u64, OpenFxHostError> {
        let mut slot = self.gate.lock().map_err(|_| unavailable("native pump gate poisoned"))?;
        if slot.phase != Phase::Executing
            || slot.response.is_some()
            || slot.questions.is_active()
            || slot.memory.is_some()
        {
            return Err(unavailable("native completion lost its executing owner"));
        }
        let response = if self.is_canceled() && !matches!(response, Response::Closed {}) {
            Response::Canceled { owner: self.owner, sequence: slot.serial }
        } else {
            response
        };
        let generation = slot
            .completion_generation
            .checked_add(1)
            .ok_or_else(|| unavailable("native completion generation exhausted"))?;
        slot.completion_generation = generation;
        slot.phase = Phase::Completed;
        slot.response = Some(response);
        self.ready.notify_all();
        Ok(generation)
    }

    /// Result ownership persists until all bytes of its original response commit.
    fn acknowledged(&self) -> Result<(), OpenFxHostError> {
        let mut slot = self.gate.lock().map_err(|_| unavailable("native pump gate poisoned"))?;
        if slot.phase != Phase::Completed {
            return Err(unavailable("native acknowledgment lost its slot"));
        }
        slot.acknowledged_generation = slot.completion_generation;
        slot.phase = Phase::Idle;
        self.gui_open.store(false, Ordering::Release);
        self.interactive_render.store(false, Ordering::Release);
        self.question_work.store(false, Ordering::Release);
        slot.progress = None;
        self.ready.notify_all();
        Ok(())
    }

    pub(super) fn wait_acknowledged(&self, generation: u64) -> Result<(), OpenFxHostError> {
        let mut slot = self.gate.lock().map_err(|_| unavailable("native pump gate poisoned"))?;
        if generation == 0 || generation > slot.completion_generation {
            return Err(invalid("native acknowledgement token was never completed"));
        }
        loop {
            if let Some(fault) = &slot.fault {
                return Err(unavailable(fault.clone()));
            }
            // A later queued request cannot erase this original completion.
            // The token is only a local response acknowledgement, not another
            // native work/owner authority or a permit to execute another frame.
            if generation != 0 && slot.acknowledged_generation >= generation {
                return Ok(());
            }
            if self.stopping.load(Ordering::Acquire) {
                return Err(unavailable("native pump stopped before acknowledgment"));
            }
            slot = self.ready.wait(slot).map_err(|_| unavailable("native pump wait poisoned"))?;
        }
    }

    fn fail(&self, error: &OpenFxHostError) {
        self.canceled.store(true, Ordering::Release);
        self.stopping.store(true, Ordering::Release);
        if let Ok(mut slot) = self.gate.lock() {
            // Fault descriptions are host-authored, not arbitrary plugin strings.
            slot.fault = Some(error.to_string());
        }
        self.ready.notify_all();
    }
}

unsafe extern "C" fn native_abort(context: *const c_void) -> c_int {
    if context.is_null() {
        return 0;
    }
    // SAFETY: NativeSession::render_controlled borrows Shared for the entire
    // invocation. The native RAII guard clears this pointer before return;
    // legal spawned callbacks complete before the action returns.
    let shared = unsafe { &*context.cast::<Shared>() };
    c_int::from(shared.is_canceled() || shared.stopping.load(Ordering::Acquire))
}

unsafe extern "C" fn native_acquire_memory(context: *const c_void, bytes: usize) -> c_int {
    if context.is_null() || bytes == 0 || bytes > MAX_FRAME_BYTES || bytes % 16 != 0 {
        return -1;
    }
    // SAFETY: the same action-scoped Shared borrow as native_abort. This is a
    // host allocation barrier before Render, never a plugin/UI thread closure.
    let shared = unsafe { &*context.cast::<Shared>() };
    let Ok(mut slot) = shared.gate.lock() else {
        return -1;
    };
    if slot.phase != Phase::Executing
        || slot.render_invocation == 0
        || slot.memory_requested
        || slot.questions.is_active()
    {
        return -1;
    }
    slot.memory_requested = true;
    slot.memory = Some(MemoryRequest { bytes, sent: false, reply: None });
    shared.ready.notify_all();
    loop {
        if shared.is_canceled()
            || shared.stopping.load(Ordering::Acquire)
            || slot.fault.is_some()
            || slot.phase != Phase::Executing
        {
            slot.memory = None;
            return -1;
        }
        if let Some(granted) = slot.memory.and_then(|value| value.reply) {
            slot.memory = None;
            return c_int::from(granted);
        }
        slot = match shared.ready.wait(slot) {
            Ok(slot) => slot,
            Err(poison) => {
                poison.into_inner().memory = None;
                return -1;
            }
        };
    }
}

unsafe extern "C" fn native_progress(
    context: *const c_void,
    value: *const NativeLiveProgress,
) -> c_int {
    if context.is_null() || value.is_null() {
        return 2;
    }
    // SAFETY: same invocation borrow as native_abort; C++ lends a complete POD
    // only during this callback. Copy it before returning, never retain it.
    let (shared, value) = unsafe { (&*context.cast::<Shared>(), *value) };
    if value.owner == 0
        || value.generation == 0
        || !matches!(value.active, 0 | 1)
        || !value.fraction.is_finite()
        || !(0.0..=1.0).contains(&value.fraction)
        || value.label.kind != 1
        || value.label.text_length > NATIVE_NOTICE_TEXT_BYTES
        || value.label.id_length > NATIVE_NOTICE_ID_BYTES
    {
        return 6;
    }
    let Ok(mut slot) = shared.gate.lock() else {
        return 6;
    };
    if slot.phase != Phase::Executing || value.invocation != slot.render_invocation {
        return 2;
    }
    let Some(revision) = slot.progress_revision.checked_add(1) else {
        return 6;
    };
    // One fixed POD assignment; no callback-side heap/string/frame allocation.
    slot.progress = Some(value);
    slot.progress_revision = revision;
    shared.ready.notify_all();
    0
}

unsafe extern "C" fn native_question(
    context: *const c_void,
    value: *const NativeLiveQuestion,
) -> c_int {
    if context.is_null() || value.is_null() {
        return 1;
    }
    // SAFETY: Aux owns one in-flight Question barrier. ControlBorrow cannot
    // return/drop this context until this callback finishes, including a legal
    // foreign-thread Question. The caller lends this complete immutable POD.
    let (shared, value) = unsafe { (&*context.cast::<Shared>(), *value) };
    let Ok(mut slot) = shared.gate.lock() else {
        return 1;
    };
    if !shared.gui_open.load(Ordering::Acquire)
        || !shared.question_work.load(Ordering::Acquire)
        || slot.phase != Phase::Executing
        || value.invocation != slot.render_invocation
        || shared.is_canceled()
        || shared.stopping.load(Ordering::Acquire)
    {
        return 1;
    }
    let Some(identity) = slot.questions.reserve(value) else {
        return 1;
    };
    shared.ready.notify_all();
    loop {
        if shared.is_canceled()
            || shared.stopping.load(Ordering::Acquire)
            || slot.phase != Phase::Executing
            || slot.render_invocation != identity.invocation
        {
            slot.questions.abandon(identity);
            return 1;
        }
        if let Some(reply) = slot.questions.take_answer(identity) {
            return reply.native_status();
        }
        if !slot.questions.contains(identity) {
            return 1;
        }
        slot = match shared.ready.wait(slot) {
            Ok(slot) => slot,
            Err(poison) => {
                // Return no answer; the sole pump reports its poisoned owner.
                // Do not leave a callback reservation after returning the borrow.
                poison.into_inner().questions.abandon(identity);
                return 1;
            }
        };
    }
}

/// Preserves partial header/body cursors across poll, progress and cancellation.
#[derive(Debug)]
pub(super) struct PacketReader {
    header: [u8; 4],
    header_count: usize,
    body: Vec<u8>,
    body_count: usize,
    expected: usize,
    started: Option<Instant>,
    did_io: bool,
}
impl PacketReader {
    pub(super) fn acquire() -> Result<Self, OpenFxHostError> {
        let body = packet_owner(MAX_WORKER_REQUEST_BYTES)?;
        Ok(Self {
            header: [0; 4],
            header_count: 0,
            body,
            body_count: 0,
            expected: 0,
            started: None,
            did_io: false,
        })
    }
    pub(super) const fn made_io_progress(&self) -> bool {
        self.did_io
    }
    fn reserve_body(&mut self, expected: usize) -> Result<(), OpenFxHostError> {
        self.body.clear();
        if self.body.capacity() < expected {
            return Err(unavailable("native packet lost its pre-admitted owner"));
        }
        if self.body.capacity() > MAX_WORKER_REQUEST_BYTES {
            return Err(unavailable("native packet capacity exceeds admission"));
        }
        self.body.resize(expected, 0);
        self.expected = expected;
        Ok(())
    }
    pub(super) fn step<T: DeserializeOwned>(
        &mut self,
        stream: &mut TcpStream,
    ) -> Result<Option<T>, OpenFxHostError> {
        self.did_io = false;
        if self.started.is_some_and(|started| started.elapsed() >= DEADLINE) {
            return Err(OpenFxHostError::DeadlineExceeded);
        }
        let target = if self.header_count < 4 {
            &mut self.header[self.header_count..]
        } else {
            let end = self.body_count.saturating_add(IO_QUANTUM_BYTES).min(self.expected);
            &mut self.body[self.body_count..end]
        };
        let count = match stream.read(target) {
            Ok(0) => return Err(unavailable("native control closed during work")),
            Ok(count) => count,
            Err(error) if retry(&error) => return Ok(None),
            Err(error) => return Err(control_error(error)),
        };
        self.did_io = true;
        self.started.get_or_insert_with(Instant::now);
        if self.header_count < 4 {
            self.header_count += count;
            if self.header_count == 4 {
                let expected = u32::from_be_bytes(self.header) as usize;
                if expected == 0 || expected > MAX_WORKER_REQUEST_BYTES {
                    return Err(unavailable("native packet exceeds admission"));
                }
                self.reserve_body(expected)?;
            }
            return Ok(None);
        }
        self.body_count += count;
        if self.body_count != self.expected {
            return Ok(None);
        }
        let value = serde_json::from_slice(&self.body)
            .map_err(|error| unavailable(format!("native packet malformed: {error}")))?;
        self.header_count = 0;
        self.body_count = 0;
        self.expected = 0;
        self.started = None;
        self.body.clear();
        Ok(Some(value))
    }
}

fn packet_owner(bytes: usize) -> Result<Vec<u8>, OpenFxHostError> {
    let mut owner = Vec::new();
    owner
        .try_reserve_exact(bytes)
        .map_err(|_| unavailable("native packet reservation refused before processing"))?;
    if owner.capacity() > bytes {
        return Err(unavailable("native packet capacity exceeds admission"));
    }
    Ok(owner)
}
struct BoundedEncoder<'a> {
    bytes: &'a mut Vec<u8>,
    limit: usize,
}
impl Write for BoundedEncoder<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native packet exceeds admission",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
    completion: bool,
    deadline: Instant,

    question: Option<question::QuestionIdentity>,
    #[cfg(test)]
    blocked: bool,
}
fn encode_packet<T: Serialize>(bytes: &mut Vec<u8>, value: &T) -> Result<(), OpenFxHostError> {
    bytes.clear();
    if bytes.capacity() != MAX_WORKER_REQUEST_BYTES + 4 {
        return Err(unavailable(
            "native outbound packet lost its admitted capacity",
        ));
    }
    bytes.extend_from_slice(&[0; 4]);
    serde_json::to_writer(
        BoundedEncoder {
            bytes: &mut *bytes,
            limit: MAX_WORKER_REQUEST_BYTES + 4,
        },
        value,
    )
    .map_err(|error| unavailable(error.to_string()))?;
    let length = bytes.len() - 4;
    if length == 0 || length > MAX_WORKER_REQUEST_BYTES {
        return Err(unavailable("native outbound packet exceeds admission"));
    }
    bytes[..4].copy_from_slice(&(length as u32).to_be_bytes());
    Ok(())
}

/// Parent dispatch reuses its pre-admitted owner for Render, Cancel and Shutdown.
#[derive(Debug)]
pub(super) struct PacketWriter {
    bytes: Vec<u8>,
}
impl PacketWriter {
    pub(super) fn acquire() -> Result<Self, OpenFxHostError> {
        Ok(Self { bytes: packet_owner(MAX_WORKER_REQUEST_BYTES + 4)? })
    }
    pub(super) fn write<T: Serialize>(
        &mut self,
        stream: &mut TcpStream,
        value: &T,
        deadline: Instant,
        checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
        committed: &mut bool,
    ) -> Result<(), OpenFxHostError> {
        check_owner(checkpoint, deadline)?;
        encode_packet(&mut self.bytes, value)?;
        stream.set_nonblocking(true).map_err(control_error)?;
        let mut offset = 0;
        while offset < self.bytes.len() {
            check_owner(checkpoint, deadline)?;
            let end = offset.saturating_add(IO_QUANTUM_BYTES).min(self.bytes.len());
            match stream.write(&self.bytes[offset..end]) {
                Ok(0) => return Err(unavailable("native control closed mid-packet")),
                Ok(count) => offset += count,
                Err(error) if retry(&error) => {
                    check_owner(checkpoint, deadline)?;
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if !remaining.is_zero() {
                        thread::sleep(remaining.min(IDLE_POLL));
                    }
                }
                Err(error) => return Err(control_error(error)),
            }
        }
        *committed = true;
        check_owner(checkpoint, deadline)
    }
}

impl PendingWrite {
    fn new(
        response: &Response,
        completion: bool,
        mut bytes: Vec<u8>,
    ) -> Result<Self, OpenFxHostError> {
        encode_packet(&mut bytes, response)?;

        let question = match response {
            Response::Question { identity, .. } => Some(*identity),
            _ => None,
        };
        Ok(Self {
            bytes,
            offset: 0,
            completion,
            question,
            deadline: Instant::now() + DEADLINE,
            #[cfg(test)]
            blocked: false,
        })
    }
    fn step(&mut self, stream: &mut TcpStream) -> Result<bool, OpenFxHostError> {
        if Instant::now() >= self.deadline {
            return Err(OpenFxHostError::DeadlineExceeded);
        }
        let end = self.offset.saturating_add(IO_QUANTUM_BYTES).min(self.bytes.len());
        match stream.write(&self.bytes[self.offset..end]) {
            Ok(0) => return Err(unavailable("native control closed mid-response")),
            Ok(count) => self.offset += count,
            Err(error) if retry(&error) => {
                #[cfg(test)]
                {
                    self.blocked = error.kind() == io::ErrorKind::WouldBlock;
                }
            }
            Err(error) => return Err(control_error(error)),
        }
        Ok(self.offset == self.bytes.len())
    }
}

pub(super) struct WorkerPump {
    pub(super) shared: Arc<Shared>,
    shutdown: TcpStream,
    worker: Option<JoinHandle<()>>,
}
impl WorkerPump {
    #[cfg(test)]
    pub(super) fn start(
        stream: TcpStream,
        owner: mondrian_core::EffectProcessorOwnerId,
        ready: Response,
    ) -> Result<Self, OpenFxHostError> {
        Self::start_observed(stream, owner, ready, close_observation::Trace::default())
    }
    pub(super) fn start_observed(
        mut stream: TcpStream,
        owner: mondrian_core::EffectProcessorOwnerId,
        ready: Response,
        close_trace: close_observation::Trace,
    ) -> Result<Self, OpenFxHostError> {
        stream.set_nonblocking(true).map_err(control_error)?;
        // This clone is ONLY a shutdown capability, never another reader/writer.
        let shutdown = stream.try_clone().map_err(control_error)?;
        // Acquire both controlled packet owners before the thread/native
        // instance exists. Response encoding reuses this owner after every ack.
        let reader = PacketReader::acquire()?;
        let writer = packet_owner(MAX_WORKER_REQUEST_BYTES + 4)?;
        let shared = Arc::new(Shared::new(owner));
        // Ready is encoded and committed only by the successfully started sole
        // pump after all declared packet owners and the thread are acquired.
        shared
            .gate
            .lock()
            .map_err(|_| unavailable("native startup gate poisoned"))?
            .phase = Phase::Executing;
        shared.complete(ready)?;
        let state = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("mondrian-ofx-control".to_owned())
            .stack_size(PUMP_STACK_BYTES)
            .spawn(move || {
                if let Err(error) = pump_loop(&mut stream, &state, reader, writer, &close_trace) {
                    state.fail(&error);
                }
            })
            .map_err(control_error)?;
        Ok(Self { shared, shutdown, worker: Some(worker) })
    }
    pub(super) fn stop_and_join(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.canceled.store(true, Ordering::Release);
        self.shared.ready.notify_all();
        let _ = self.shutdown.shutdown(Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            // Unwinding with an active invocation/borrow is forbidden. A pump
            // panic retires this supervised process rather than detach a reader.
            if worker.join().is_err() {
                std::process::abort();
            }
        }
    }
}
impl Drop for WorkerPump {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn pump_loop(
    stream: &mut TcpStream,
    shared: &Shared,
    mut reader: PacketReader,
    writer: Vec<u8>,
    close_trace: &close_observation::Trace,
) -> Result<(), OpenFxHostError> {
    let mut available_writer = Some(writer);
    let mut pending: Option<PendingWrite> = None;
    let mut pending_is_closed = false;
    while !shared.stopping.load(Ordering::Acquire) {
        // Reading is always attempted before any blocked progress write. Each
        // direction has a bounded quantum; a full socket never masks Cancel.
        if let Some(request) = reader.step::<Request>(stream)? {
            shared.admit(request)?;
        }
        let mut did_io = reader.did_io;
        if pending.is_none() {
            let (response, completion) = {
                let mut slot =
                    shared.gate.lock().map_err(|_| unavailable("native pump gate poisoned"))?;
                if let Some(response) = slot.response.take() {
                    (Some(response), true)
                } else if slot.phase == Phase::Executing
                    && slot.memory.is_some_and(|value| !value.sent)
                {
                    let sequence = slot.serial;
                    let memory = slot.memory.as_mut().expect("pending memory request");
                    memory.sent = true;
                    (
                        Some(Response::MemoryNeeded {
                            owner: shared.owner,
                            sequence,
                            required_bytes: memory.bytes,
                        }),
                        false,
                    )
                } else if slot.phase == Phase::Executing && slot.questions.unpublished().is_some() {
                    let pending = slot
                        .questions
                        .unpublished()
                        .ok_or_else(|| unavailable("Question lost its fixed owner"))?;
                    let text = pending
                        .value
                        .text
                        .decode()
                        .map_err(|_| unavailable("Question text malformed"))?
                        .ok_or_else(|| unavailable("Question text absent"))?;
                    (
                        Some(Response::Question {
                            owner: shared.owner,
                            sequence: slot.serial,
                            identity: pending.identity,
                            text,
                        }),
                        false,
                    )
                } else if slot.phase == Phase::Executing
                    && slot.sent_revision < slot.progress_revision
                {
                    let progress =
                        slot.progress.ok_or_else(|| unavailable("native progress lost its POD"))?;
                    slot.sent_revision = slot.progress_revision;
                    let progress_value = progress
                        .decode()
                        .map_err(|_| unavailable("native progress metadata malformed"))?;
                    (
                        Some(Response::Progress {
                            owner: shared.owner,
                            sequence: slot.serial,
                            revision: slot.sent_revision,
                            native_owner: progress.owner,
                            progress: progress_value,
                        }),
                        false,
                    )
                } else {
                    (None, false)
                }
            };
            if let Some(response) = response {
                pending_is_closed = matches!(response, Response::Closed {});
                pending = Some(PendingWrite::new(
                    &response,
                    completion,
                    available_writer
                        .take()
                        .ok_or_else(|| unavailable("native writer lost its packet owner"))?,
                )?);
            }
        }
        if let Some(packet) = &mut pending {
            let before = packet.offset;
            let complete = packet.step(stream)?;
            #[cfg(test)]
            if packet.blocked {
                shared.outbound_blocked.store(true, Ordering::Release);
            }
            did_io |= before != packet.offset;
            let completion = packet.completion;
            if complete {
                let packet =
                    pending.take().ok_or_else(|| unavailable("committed native packet absent"))?;
                if let Some(identity) = packet.question {
                    let mut slot = shared
                        .gate
                        .lock()
                        .map_err(|_| unavailable("Question commit gate poisoned"))?;
                    if !slot.questions.mark_published(identity)
                        && !shared.is_canceled()
                        && !shared.stopping.load(Ordering::Acquire)
                    {
                        return Err(unavailable(
                            "Question publication lost its original service",
                        ));
                    }
                    shared.ready.notify_all();
                }
                available_writer = Some(packet.bytes);
                if pending_is_closed {
                    // Actual full packet bytes committed; the original local
                    // acknowledgement flag is still unpublished at this point.
                    close_trace.record(close_observation::Stage::ClosedCommitted, 0);
                }
                pending_is_closed = false;
                if completion {
                    shared.acknowledged()?;
                }
            }
        }
        // No per-fragment sleep: large valid packets consume bounded quanta
        // immediately while both directions remain serviced. Idle wake latency
        // is the declared small poll policy, not hardware CPU permits.
        if !did_io {
            thread::sleep(IDLE_POLL);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_memory_request(shared: &Shared) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if shared.gate.lock().unwrap().memory.is_some() {
                return true;
            }
            thread::yield_now();
        }
        false
    }

    #[test]
    fn native_memory_barrier_binds_one_exact_reply_and_rejects_stale_or_duplicate_grants() {
        for granted in [false, true] {
            let owner = mondrian_core::EffectProcessorOwnerId::new();
            let shared = Arc::new(Shared::new(owner));
            shared.admit(render(owner, 1)).unwrap();
            shared.take_request().unwrap();
            let foreign = Arc::clone(&shared);
            let callback = thread::spawn(move || unsafe {
                native_acquire_memory(std::ptr::from_ref(&*foreign).cast(), 320)
            });
            let waiting = wait_memory_request(&shared);
            if !waiting {
                shared.admit(Request::Cancel { owner, sequence: 1 }).unwrap();
                let _ = callback.join();
                panic!("memory callback did not reach its fixed slot");
            }
            shared.gate.lock().unwrap().memory.as_mut().unwrap().sent = true;
            let wrong = [
                Request::MemoryReply {
                    owner: mondrian_core::EffectProcessorOwnerId::new(),
                    sequence: 1,
                    required_bytes: 320,
                    granted,
                },
                Request::MemoryReply { owner, sequence: 2, required_bytes: 320, granted },
                Request::MemoryReply { owner, sequence: 1, required_bytes: 304, granted },
            ]
            .into_iter()
            .all(|request| shared.admit(request).is_err());
            let reply = shared.admit(Request::MemoryReply {
                owner,
                sequence: 1,
                required_bytes: 320,
                granted,
            });
            if reply.is_err() {
                shared.admit(Request::Cancel { owner, sequence: 1 }).unwrap();
            }
            let result = callback.join().unwrap();
            assert!(wrong);
            assert!(reply.is_ok());
            assert_eq!(result, c_int::from(granted));
            assert!(shared
                .admit(Request::MemoryReply { owner, sequence: 1, required_bytes: 320, granted })
                .is_err());
            assert_eq!(
                unsafe { native_acquire_memory(std::ptr::from_ref(&*shared).cast(), 320) },
                -1,
                "one host allocation request per invocation"
            );
            assert!(shared.gate.lock().unwrap().memory.is_none());
        }
    }

    #[test]
    fn current_cancel_releases_the_waiting_memory_barrier_without_a_grant() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Arc::new(Shared::new(owner));
        shared.admit(render(owner, 1)).unwrap();
        shared.take_request().unwrap();
        let foreign = Arc::clone(&shared);
        let callback = thread::spawn(move || unsafe {
            native_acquire_memory(std::ptr::from_ref(&*foreign).cast(), 16)
        });
        let waiting = wait_memory_request(&shared);
        shared.admit(Request::Cancel { owner, sequence: 1 }).unwrap();
        let result = callback.join().unwrap();
        assert!(waiting);
        assert_eq!(result, -1);
        assert!(shared.gate.lock().unwrap().memory.is_none());
        assert!(shared.is_canceled());
    }

    fn render(owner: mondrian_core::EffectProcessorOwnerId, sequence: u64) -> Request {
        Request::Render {
            owner,
            sequence,
            gui_open: false,
            interactive_render: false,
            question_work: false,
            timing: OpenFxRenderTiming {
                frame: 0.0,
                frame_rate: 24.0,
                first_frame: 0.0,
                last_frame: 100.0,
            },
            sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
            parameters: BTreeMap::new(),
            diagnostic: false,
            sources: None,
        }
    }

    #[test]
    fn current_gui_wire_requires_all_admission_and_terminal_fields() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let canonical = serde_json::to_value(render(owner, 1)).expect("current request");
        for field in [
            "gui_open",
            "interactive_render",
            "question_work",
            "diagnostic",
            "sampling",
            "sources",
        ] {
            let mut missing = canonical.clone();
            missing.as_object_mut().expect("request object").remove(field);
            assert!(
                serde_json::from_value::<Request>(missing).is_err(),
                "missing {field}"
            );
        }
        let response = serde_json::json!({"kind":"gui_closed","owner":owner,"sequence":2});
        assert!(serde_json::from_value::<Response>(response.clone()).is_err());
        let mut null = response;
        null["feedback"] = serde_json::Value::Null;
        assert!(matches!(
            serde_json::from_value::<Response>(null),
            Ok(Response::GuiClosed { feedback: None, .. })
        ));
        assert_eq!(VERSION, 16);
    }

    #[test]
    fn current_render_wire_has_one_required_checked_sampling_authority() {
        let value =
            serde_json::to_value(render(mondrian_core::EffectProcessorOwnerId::new(), 1)).unwrap();
        for sampling in [
            serde_json::Value::Null,
            serde_json::json!({"sample_aspect_ratio":{"numerator":0,"denominator":1},
                "render_scale":[{"num":1,"den":1},{"num":1,"den":1}]}),
            serde_json::json!({"sample_aspect_ratio":{"numerator":1,"denominator":1},
                "render_scale":[{"num":0,"den":1},{"num":1,"den":1}]}),
        ] {
            let mut invalid = value.clone();
            invalid["sampling"] = sampling;
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }
        let mut old_timing = value;
        old_timing["timing"]["pixel_aspect_ratio"] = serde_json::json!(2.0);
        assert!(
            serde_json::from_value::<Request>(old_timing).is_err(),
            "no second PAR authority"
        );
        let mut packet = packet_owner(MAX_WORKER_REQUEST_BYTES + 4).unwrap();
        let capacity = packet.capacity();
        encode_packet(
            &mut packet,
            &render(mondrian_core::EffectProcessorOwnerId::new(), 1),
        )
        .unwrap();
        assert_eq!(
            packet.capacity(),
            capacity,
            "required sampling fits the existing charged packet owner"
        );
    }

    #[test]
    fn render_close_render_uses_distinct_monotonic_coordinates_and_refuses_late_original_controls()
    {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.admit(render(owner, 1)).expect("Render wire 1");
        shared.take_request().expect("Render invocation 1");
        assert_eq!(shared.gate.lock().expect("slot").render_invocation, 1);
        let first = shared
            .complete(Response::Rejected { sequence: 1, status: 5 })
            .expect("first completion");
        shared.gate.lock().expect("slot").response.take();
        shared.acknowledged().expect("original response committed");
        shared
            .admit(Request::CloseGui { owner, sequence: 2 })
            .expect("idle action wire 2");
        shared.take_request().expect("same action owner");
        {
            let slot = shared.gate.lock().expect("slot");
            assert_eq!(slot.render_invocation, 0);
            assert_eq!(slot.last_render_invocation, 1);
        }
        shared
            .complete(Response::GuiClosed { owner, sequence: 2, feedback: None })
            .expect("End completion");
        shared.gate.lock().expect("slot").response.take();
        shared.acknowledged().expect("End response committed");
        shared.admit(render(owner, 3)).expect("next Render wire 3");
        shared
            .wait_acknowledged(first)
            .expect("prior ack unaffected by next accepted work");
        shared.take_request().expect("next native Render invocation 2");
        {
            let slot = shared.gate.lock().expect("slot");
            assert_eq!(slot.serial, 3);
            assert_eq!(slot.render_invocation, 2);
        }
        assert!(shared.admit(Request::Cancel { owner, sequence: 1 }).is_err());
        assert!(shared.admit(Request::Cancel { owner, sequence: 2 }).is_err());
        assert!(!shared.is_canceled());
        let mut value: NativeLiveQuestion = unsafe { std::mem::zeroed() };
        value.owner = 17;
        value.invocation = 2;
        value.text.kind = 1;
        let identity = {
            let mut slot = shared.gate.lock().expect("slot");
            let id = slot.questions.reserve(value).expect("second Render question");
            assert!(slot.questions.mark_published(id));
            id
        };
        let old = question::QuestionIdentity {
            native_owner: 17,
            invocation: 1,
            service: identity.service,
        };
        assert!(shared
            .admit(Request::QuestionReply {
                owner,
                sequence: 3,
                identity: old,
                reply: question::QuestionReply::No
            })
            .is_err());
        assert!(shared
            .admit(Request::QuestionReply {
                owner,
                sequence: 1,
                identity,
                reply: question::QuestionReply::No
            })
            .is_err());
        shared
            .admit(Request::QuestionReply {
                owner,
                sequence: 3,
                identity,
                reply: question::QuestionReply::Yes,
            })
            .expect("only actual current action and native invocation can answer");
        assert_eq!(
            shared.gate.lock().expect("slot").questions.take_answer(identity),
            Some(question::QuestionReply::Yes)
        );
        assert!(!shared.is_canceled());
    }

    #[test]
    fn queued_cancel_remains_sticky_when_native_dispatch_activates() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.admit(render(owner, 1)).expect("owned request");
        shared.admit(Request::Cancel { owner, sequence: 1 }).expect("queued cancel");
        assert!(matches!(
            shared.take_request(),
            Ok(Request::Render { sequence: 1, .. })
        ));
        let control = shared.native_control();
        assert_eq!(unsafe { native_abort(control.context) }, 1);
        shared
            .complete(Response::Rejected { sequence: 1, status: 5 })
            .expect("canceled native result");
        let slot = shared.gate.lock().expect("slot");
        assert!(
            matches!(slot.response, Some(Response::Canceled { sequence: 1, owner: value }) if value == owner)
        );
    }

    #[test]
    fn obsolete_future_and_foreign_cancel_never_cancel_current_work() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.admit(render(owner, 1)).expect("request");
        for request in [
            Request::Cancel { owner, sequence: 0 },
            Request::Cancel { owner, sequence: 2 },
            Request::Cancel {
                owner: mondrian_core::EffectProcessorOwnerId::new(),
                sequence: 1,
            },
        ] {
            assert!(shared.admit(request).is_err());
            assert!(!shared.is_canceled());
        }
        shared.take_request().expect("dispatch");
        assert!(
            shared.admit(render(owner, 2)).is_err(),
            "no second queued main request"
        );
        assert!(!shared.is_canceled());
    }

    #[test]
    fn original_completion_owns_slot_until_its_bytes_are_acknowledged() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.admit(render(owner, 1)).expect("request");
        shared.take_request().expect("dispatch");
        shared
            .complete(Response::Rejected { sequence: 1, status: 5 })
            .expect("completion");
        assert!(
            shared.admit(render(owner, 2)).is_err(),
            "result cannot be stolen before its ack"
        );
        let original = shared.gate.lock().expect("slot").response.take();
        assert!(matches!(
            original,
            Some(Response::Rejected { sequence: 1, status: 5 })
        ));
        assert!(
            shared.admit(render(owner, 2)).is_err(),
            "encoding/taking does not acknowledge"
        );
        shared.acknowledged().expect("actual complete write");
        shared.admit(render(owner, 2)).expect("next request only after ack");
        assert!(!shared.is_canceled());
    }

    #[test]
    fn foreign_native_abort_reads_only_actual_shared_work_and_progress_is_one_pod() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Arc::new(Shared::new(owner));
        shared.admit(render(owner, 1)).expect("request");
        shared.take_request().expect("dispatch");
        let native_state = Arc::clone(&shared);
        let foreign = thread::spawn(move || {
            let control = native_state.native_control();
            assert_eq!(unsafe { native_abort(control.context) }, 0);
            let mut value: NativeLiveProgress = unsafe { std::mem::zeroed() };
            value.owner = 17;
            value.invocation = 1;
            value.generation = 3;
            value.active = 1;
            value.fraction = 0.375;
            value.label.kind = 1;
            assert_eq!(unsafe { native_progress(control.context, &value) }, 0);
            value.fraction = 0.625;
            assert_eq!(unsafe { native_progress(control.context, &value) }, 0);
            native_state
                .admit(Request::Cancel { owner, sequence: 1 })
                .expect("actual cancel");
            assert_eq!(unsafe { native_abort(control.context) }, 1);
        });
        foreign.join().expect("joined callback lifetime");
        let slot = shared.gate.lock().expect("slot");
        assert_eq!(slot.progress_revision, 2);
        assert_eq!(slot.progress.expect("one latest POD").fraction, 0.625);
    }

    #[test]
    fn malformed_callback_metadata_refuses_without_mutating_current_progress() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.admit(render(owner, 1)).expect("request");
        shared.take_request().expect("dispatch");
        let control = shared.native_control();
        let mut value: NativeLiveProgress = unsafe { std::mem::zeroed() };
        value.owner = 1;
        value.invocation = 1;
        value.generation = 1;
        value.active = 1;
        value.label.kind = 1;
        for fraction in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            value.fraction = fraction;
            assert_eq!(unsafe { native_progress(control.context, &value) }, 6);
        }
        value.fraction = 0.5;
        value.invocation = 2;
        assert_eq!(unsafe { native_progress(control.context, &value) }, 2);
        assert!(shared.gate.lock().expect("slot").progress.is_none());
    }

    #[test]
    fn pump_shutdown_joins_its_reader_without_stdin_or_stdout_dependency() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let _parent = TcpStream::connect(listener.local_addr().expect("address")).expect("parent");
        let (worker, _) = listener.accept().expect("worker");
        let mut pump = WorkerPump::start(
            worker,
            mondrian_core::EffectProcessorOwnerId::new(),
            Response::Ready { version: VERSION, nonce: "unit-ready".to_owned() },
        )
        .expect("bounded pump");
        pump.stop_and_join();
        assert!(pump.worker.is_none(), "reader joined, not detached");
        assert!(pump.shared.stopping.load(Ordering::Acquire));
    }

    #[test]
    fn fixed_packet_encoder_refuses_before_capacity_growth() {
        let mut bytes = packet_owner(8).expect("owner");
        let capacity = bytes.capacity();
        let mut writer = BoundedEncoder { bytes: &mut bytes, limit: 8 };
        writer.write_all(b"12345678").expect("bound");
        assert!(writer.write_all(b"9").is_err());
        assert_eq!(bytes, b"12345678");
        assert_eq!(bytes.capacity(), capacity);
    }
    #[test]
    fn partial_packet_cursors_survive_poll_and_reuse_the_single_admitted_owner() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let mut parent =
            TcpStream::connect(listener.local_addr().expect("address")).expect("parent");
        let (mut stream, _) = listener.accept().expect("worker");
        stream.set_nonblocking(true).expect("nonblocking reader");
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let bytes =
            serde_json::to_vec(&Response::Canceled { owner, sequence: 7 }).expect("bounded packet");
        let header = (bytes.len() as u32).to_be_bytes();
        let mut reader = PacketReader::acquire().expect("one admitted reader");
        let allocation = reader.body.as_ptr();
        let capacity = reader.body.capacity();
        let deadline = Instant::now() + Duration::from_secs(5);
        parent.write_all(&header[..3]).expect("partial header");
        while reader.header_count != 3 {
            assert!(Instant::now() < deadline);
            assert!(reader.step::<Response>(&mut stream).expect("header fragment").is_none());
            thread::yield_now();
        }
        assert_eq!(reader.header_count, 3);
        assert!(reader.step::<Response>(&mut stream).expect("idle poll").is_none());
        assert_eq!(
            reader.header_count, 3,
            "idle poll never resets an owned packet"
        );
        parent.write_all(&header[3..]).expect("header end");
        while reader.header_count != 4 {
            assert!(Instant::now() < deadline);
            assert!(reader.step::<Response>(&mut stream).expect("header complete").is_none());
            thread::yield_now();
        }
        let split = bytes.len() / 2;
        parent.write_all(&bytes[..split]).expect("partial body");
        while reader.body_count != split {
            assert!(Instant::now() < deadline);
            assert!(reader.step::<Response>(&mut stream).expect("body fragment").is_none());
            thread::yield_now();
        }
        assert_eq!(reader.body_count, split);
        assert!(reader.step::<Response>(&mut stream).expect("idle body poll").is_none());
        assert_eq!(reader.body_count, split);
        parent.write_all(&bytes[split..]).expect("body end");
        let response = loop {
            assert!(Instant::now() < deadline);
            if let Some(response) = reader.step::<Response>(&mut stream).expect("complete packet") {
                break response;
            }
            thread::yield_now();
        };
        assert!(
            matches!(response, Response::Canceled { owner: actual, sequence: 7 } if actual == owner)
        );
        assert_eq!(reader.body.as_ptr(), allocation);
        assert_eq!(reader.body.capacity(), capacity);
        assert_eq!(
            (reader.header_count, reader.body_count, reader.expected),
            (0, 0, 0)
        );
    }

    #[test]
    fn outbound_backpressure_cannot_block_reading_actual_current_cancel() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let mut parent =
            TcpStream::connect(listener.local_addr().expect("address")).expect("parent");
        let (worker, _) = listener.accept().expect("worker");
        let deadline = Instant::now() + Duration::from_secs(5);
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let mut pump = WorkerPump::start(
            worker,
            owner,
            Response::Ready {
                version: VERSION,
                nonce: "live-work-ready".to_owned(),
            },
        )
        .expect("pump");
        parent.set_nonblocking(true).expect("parent poll");
        let mut reader = PacketReader::acquire().expect("startup reader");
        loop {
            assert!(
                Instant::now() < deadline,
                "actual pump startup did not finish"
            );
            if let Some(response) = reader.step::<Response>(&mut parent).expect("ready packet") {
                assert!(matches!(response, Response::Ready { .. }));
                break;
            }
            thread::yield_now();
        }
        let mut writer = PacketWriter::acquire().expect("bounded parent owner");
        writer
            .write(
                &mut parent,
                &render(owner, 1),
                deadline,
                &mut || Ok(()),
                &mut false,
            )
            .expect("one actual main request");
        assert!(
            matches!(pump.shared.take_request().expect("actual executing work"),
            Request::Render { owner: actual, sequence: 1, .. } if actual == owner)
        );
        let mut value: NativeLiveProgress = unsafe { std::mem::zeroed() };
        value.owner = 17;
        value.invocation = 1;
        value.generation = 1;
        value.active = 1;
        value.fraction = 0.375;
        value.label.kind = 1;
        value.label.text_length = NATIVE_NOTICE_TEXT_BYTES;
        value.label.text[..NATIVE_NOTICE_TEXT_BYTES].fill(b'P' as c_char);
        let control = pump.shared.native_control();
        // The parent deliberately stops reading. Only the real pump writes;
        // its cfg(test) witness records an actual WouldBlock, not elapsed time.
        while !pump.shared.outbound_blocked.load(Ordering::Acquire) && Instant::now() < deadline {
            assert_eq!(unsafe { native_progress(control.context, &value) }, 0);
            thread::yield_now();
        }
        assert!(
            pump.shared.outbound_blocked.load(Ordering::Acquire),
            "requires actual outbound backpressure"
        );
        writer
            .write(
                &mut parent,
                &Request::Cancel { owner, sequence: 1 },
                deadline,
                &mut || Ok(()),
                &mut false,
            )
            .expect("independent input direction");
        while !pump.shared.is_canceled() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            pump.shared.is_canceled(),
            "full outbound socket cannot mask current cancellation input"
        );
        assert_eq!(unsafe { native_abort(control.context) }, 1);
        pump.stop_and_join();
        assert!(
            pump.worker.is_none(),
            "no blocked or detached reader after stop"
        );
    }

    #[test]
    fn writer_expired_deadline_never_submits_or_claims_a_packet() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let mut sender =
            TcpStream::connect(listener.local_addr().expect("address")).expect("sender");
        let (mut receiver, _) = listener.accept().expect("receiver");
        receiver.set_nonblocking(true).expect("receiver");
        let mut writer = PacketWriter::acquire().expect("owner");
        let mut committed = false;
        assert!(matches!(
            writer.write(
                &mut sender,
                &Request::Shutdown {},
                Instant::now(),
                &mut || Ok(()),
                &mut committed
            ),
            Err(OpenFxHostError::DeadlineExceeded)
        ));
        assert!(!committed);
        assert_eq!(
            receiver.read(&mut [0_u8; 1]).expect_err("no output bytes").kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn original_completion_ack_survives_the_next_queued_request() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.admit(render(owner, 1)).expect("first accepted owner");
        shared.take_request().expect("first executing owner");
        let first = shared
            .complete(Response::Rejected { sequence: 1, status: 5 })
            .expect("first completed response");
        // Model the sole pump taking the response, fully committing its bytes,
        // and accepting the next request before dispatch reacquires the gate.
        shared.gate.lock().expect("slot").response.take();
        shared.acknowledged().expect("first original response committed");
        shared.admit(render(owner, 2)).expect("second legitimately queued");
        assert!(
            shared.admit(Request::Cancel { owner, sequence: 1 }).is_err(),
            "late original cancel cannot target the replacement work"
        );
        assert!(
            !shared.is_canceled(),
            "rejected old correlation did not mutate new work"
        );
        shared.wait_acknowledged(first).expect("first token remains acknowledged");
        assert!(matches!(
            shared.take_request().expect("second is executable"),
            Request::Render { sequence: 2, .. }
        ));
        let second = shared
            .complete(Response::Rejected { sequence: 2, status: 6 })
            .expect("second original completion");
        assert!(second > first);
        let slot = shared.gate.lock().expect("slot");
        assert!(
            slot.acknowledged_generation < second,
            "first acknowledgement is not second completion evidence"
        );
    }

    #[test]
    fn completion_or_request_sequence_exhaustion_refuses_before_work_admission() {
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.gate.lock().expect("slot").completion_generation = u64::MAX;
        assert!(shared.admit(render(owner, 1)).is_err());
        assert!(shared.gate.lock().expect("slot").request.is_none());
        assert!(!shared.is_canceled());
        let shared = Shared::new(owner);
        shared.gate.lock().expect("slot").last_serial = u64::MAX;
        assert!(shared.admit(render(owner, 0)).is_err());
        assert!(shared.gate.lock().expect("slot").request.is_none());
        assert_eq!(shared.gate.lock().expect("slot").phase, Phase::Idle);
    }

    #[test]
    #[ignore = "requires the owned cancel_after_create SessionProbe native bundle"]
    fn actual_constructor_complete_then_current_cancel_never_enters_render() {
        const TEST: &str = "openfx_host::resident::pump::tests::actual_constructor_complete_then_current_cancel_never_enters_render";
        let directory = tempfile::tempdir().expect("owned exact action receipts");
        let constructed = directory.path().join("constructed.tsv");
        let rendered = directory.path().join("rendered.tsv");
        if !crate::openfx_host::shared_frame_tests::qualification_child(
            TEST,
            &[
                ("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "cancel_after_create"),
                (
                    "MONDRIAN_OPENFX_ACTUAL_CONSTRUCTOR_COMPLETE",
                    constructed.to_str().expect("constructor path"),
                ),
                (
                    "MONDRIAN_OPENFX_ACTUAL_RENDER_ENTERED",
                    rendered.to_str().expect("render path"),
                ),
            ],
        ) {
            return;
        }
        let constructed = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_ACTUAL_CONSTRUCTOR_COMPLETE")
                .expect("exact constructor receipt"),
        );
        let rendered = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_ACTUAL_RENDER_ENTERED")
                .expect("exact render receipt"),
        );
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned native fixture"),
        ))
        .expect("actual binary");
        let bundle_path = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("actual bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle_path).expect("bundle");
        let identifier =
            CString::new("uk.co.thefoundry.BasicGainPlugin").expect("actual selected identifier");
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 1.0,
        };
        let input = [[0.25_f32, -0.5, 1.5, 0.25]];
        let input_bytes: &[u8] = bytemuck::cast_slice(&input);
        let mut output = [[7.0_f32, -3.0, 11.0, 0.5]];
        let original = output;
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.admit(render(owner, 1)).expect("same accepted request");
        shared.take_request().expect("same executing owner");
        let mut instance = NativeSession::create(
            owner,
            &binary,
            &bundle,
            &identifier,
            input_bytes,
            &info,
            &[],
        )
        .expect("actual native constructor returned");
        let receipt = std::fs::read_to_string(&constructed).expect("real completed CreateInstance");
        assert_eq!(receipt.lines().count(), 1);
        assert_eq!(
            receipt.trim().parse::<u32>().expect("actual native PID"),
            std::process::id()
        );
        assert!(
            !rendered.exists(),
            "actual construction performed no Render"
        );
        // Exact deterministic action frontier: no delay, sleep, or fake native
        // Create-period abort. The real constructor has already returned.
        shared
            .admit(Request::Cancel { owner, sequence: 1 })
            .expect("same actual current stop");
        let outcome = dispatch_prepared_native_render(
            &shared,
            &mut instance,
            input_bytes,
            bytemuck::cast_slice_mut(&mut output),
            &info,
            &[],
        );
        assert!(
            !rendered.exists(),
            "accepted stop after real construction must precede DSP entry"
        );
        assert!(matches!(outcome, NativeRenderDispatch::Canceled));
        assert_eq!(output, original, "no canceled DSP/output publication");
        drop(instance);
        // Independent positive control: this same extracted production action
        // path must still invoke a real uncanceled native instance once.
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let shared = Shared::new(owner);
        shared.admit(render(owner, 1)).expect("new valid owner");
        shared.take_request().expect("new actual execution");
        let mut instance = NativeSession::create(
            owner,
            &binary,
            &bundle,
            &identifier,
            input_bytes,
            &info,
            &[],
        )
        .expect("actual uncanceled constructor");
        assert!(matches!(
            dispatch_prepared_native_render(
                &shared,
                &mut instance,
                input_bytes,
                bytemuck::cast_slice_mut(&mut output),
                &info,
                &[]
            ),
            NativeRenderDispatch::Invoked(Ok(mondrian_core::ColorFrameAlpha::StraightFloat))
        ));
        assert_eq!(output, input, "independent Basic unit-gain pixels");
        assert_eq!(
            std::fs::read_to_string(&rendered)
                .expect("actual positive Render receipt")
                .lines()
                .count(),
            1
        );
        drop(instance);
    }
}
