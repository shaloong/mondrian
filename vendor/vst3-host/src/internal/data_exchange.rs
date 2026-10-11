//! Bounded host implementation of VST3's realtime data-exchange API.

use crossbeam_queue::ArrayQueue;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle, Thread, ThreadId};
use vst3::{ComPtr, Steinberg::Vst::*, Steinberg::*};

use crate::plugin::DataExchangeBlock as OwnedDataExchangeBlock;

const MAX_QUEUES: usize = 32;
const DISPATCHER_STACK_BYTES: usize = 2 * 1024 * 1024;
const QUEUE_INDEX_BITS: u32 = 5;
const MAX_BLOCK_SIZE: usize = 16 * 1024 * 1024;
const MAX_BLOCKS_PER_QUEUE: usize = 256;
const MAX_TOTAL_QUEUE_BYTES: usize = 256 * 1024 * 1024;
const MAX_SNAPSHOT_BLOCKS: usize = 1024;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

const BLOCK_AVAILABLE: u8 = 0;
const BLOCK_LOCKED: u8 = 1;
const BLOCK_PENDING: u8 = 2;

struct AlignedBlock {
    ptr: NonNull<u8>,
    layout: Layout,
    state: AtomicU8,
}

unsafe impl Send for AlignedBlock {}
unsafe impl Sync for AlignedBlock {}

impl AlignedBlock {
    fn new(size: usize, alignment: usize) -> Option<Self> {
        if size == 0 {
            return None;
        }
        let layout = Layout::from_size_align(size, alignment).ok()?;
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(Self {
            ptr,
            layout,
            state: AtomicU8::new(BLOCK_AVAILABLE),
        })
    }
}

impl Drop for AlignedBlock {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

struct ExchangeQueue {
    id: u32,
    user_context_id: u32,
    block_size: usize,
    total_bytes: usize,
    background: bool,
    blocks: Box<[AlignedBlock]>,
    available: ArrayQueue<u32>,
    pending: ArrayQueue<u32>,
}

impl ExchangeQueue {
    fn new(
        id: u32,
        user_context_id: u32,
        block_size: usize,
        num_blocks: usize,
        alignment: usize,
        background: bool,
    ) -> Option<Self> {
        let mut blocks = Vec::with_capacity(num_blocks);
        for _ in 0..num_blocks {
            blocks.push(AlignedBlock::new(block_size, alignment)?);
        }
        let available = ArrayQueue::new(num_blocks);
        for id in 0..num_blocks as u32 {
            available.push(id).ok()?;
        }
        Some(Self {
            id,
            user_context_id,
            block_size,
            total_bytes: block_size.checked_mul(num_blocks)?,
            background,
            blocks: blocks.into_boxed_slice(),
            available,
            pending: ArrayQueue::new(num_blocks),
        })
    }

    fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::HostStorageRef;
        visitor.visit(HostStorageRef::boxed(&self.blocks))?;
        for block in &self.blocks {
            // This Layout is the actual retained allocation/deallocation contract.
            visitor.visit(HostStorageRef::owned_layout(
                block,
                block.ptr.as_ptr() as usize,
                block.layout,
            ))?;
        }
        visitor.visit(HostStorageRef::queue(&self.available))?;
        visitor.visit(HostStorageRef::queue(&self.pending))?;
        Ok(())
    }

    fn lock(&self, out: *mut vst3::Steinberg::Vst::DataExchangeBlock) -> tresult {
        let Some(block_id) = self.available.pop() else {
            return kOutOfMemory;
        };
        let Some(block) = self.blocks.get(block_id as usize) else {
            return kInternalError;
        };
        if block
            .state
            .compare_exchange(
                BLOCK_AVAILABLE,
                BLOCK_LOCKED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return kInternalError;
        }
        unsafe {
            (*out).data = block.ptr.as_ptr().cast();
            (*out).size = self.block_size as u32;
            (*out).blockID = block_id;
        }
        kResultTrue
    }

    fn free(&self, block_id: u32, send: bool) -> tresult {
        let Some(block) = self.blocks.get(block_id as usize) else {
            return kInvalidArgument;
        };
        if block
            .state
            .compare_exchange(
                BLOCK_LOCKED,
                if send { BLOCK_PENDING } else { BLOCK_AVAILABLE },
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return kInvalidArgument;
        }
        let pushed = if send {
            self.pending.push(block_id)
        } else {
            self.available.push(block_id)
        };
        if pushed.is_err() {
            block.state.store(BLOCK_LOCKED, Ordering::Release);
            return kInternalError;
        }
        kResultTrue
    }

    fn recycle(&self, block_id: u32) {
        if let Some(block) = self.blocks.get(block_id as usize) {
            block.state.store(BLOCK_AVAILABLE, Ordering::Release);
            let _ = self.available.push(block_id);
        }
    }
}

/// Sole owning ring for copied diagnostics; it never resizes after construction.
struct SnapshotSink {
    blocks: Vec<Option<OwnedDataExchangeBlock>>,
    head: usize,
    len: usize,
    bytes: usize,
}

impl Default for SnapshotSink {
    fn default() -> Self {
        let mut blocks = Vec::with_capacity(MAX_SNAPSHOT_BLOCKS);
        blocks.resize_with(MAX_SNAPSHOT_BLOCKS, || None);
        Self {
            blocks,
            head: 0,
            len: 0,
            bytes: 0,
        }
    }
}

impl SnapshotSink {
    fn pop_front(&mut self) -> Option<OwnedDataExchangeBlock> {
        if self.len == 0 {
            return None;
        }
        let block = self.blocks[self.head].take()?;
        self.head = (self.head + 1) % self.blocks.len();
        self.len -= 1;
        self.bytes -= block.data.capacity();
        Some(block)
    }

    fn push(&mut self, block: OwnedDataExchangeBlock) {
        // The diagnostic cap concerns owned allocator storage, not its used length.
        let charge = block.data.capacity();
        if charge > MAX_SNAPSHOT_BYTES {
            return;
        }
        while self.len >= self.blocks.len()
            || self
                .bytes
                .checked_add(charge)
                .is_none_or(|bytes| bytes > MAX_SNAPSHOT_BYTES)
        {
            if self.pop_front().is_none() {
                return;
            }
        }
        let tail = (self.head + self.len) % self.blocks.len();
        debug_assert!(self.blocks[tail].is_none());
        self.blocks[tail] = Some(block);
        self.len += 1;
        self.bytes += charge;
    }

    fn take_blocks(&mut self) -> Vec<OwnedDataExchangeBlock> {
        let mut out = Vec::with_capacity(self.len);
        while let Some(block) = self.pop_front() {
            out.push(block);
        }
        out
    }

    fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::HostStorageRef;
        visitor.visit(HostStorageRef::vector(&self.blocks)?)?;
        for block in self.blocks.iter().flatten() {
            visitor.visit(HostStorageRef::vector(&block.data)?)?;
        }
        Ok(())
    }
}

/// State shared by the host context, the audio callback, and one background dispatcher.
pub struct DataExchangeState {
    core: Arc<DataExchangeCore>,
    worker_join: Mutex<Option<JoinHandle<()>>>,
}

struct DataExchangeCore {
    queues: [AtomicPtr<ExchangeQueue>; MAX_QUEUES],
    generations: [AtomicU32; MAX_QUEUES],
    lifecycle: Mutex<()>,
    receiver: Mutex<Option<ComPtr<IDataExchangeReceiver>>>,
    processor: AtomicPtr<IAudioProcessor>,
    active: AtomicBool,
    in_process: AtomicBool,
    allocated_bytes: AtomicUsize,
    snapshots: Mutex<SnapshotSink>,
    control_thread: ThreadId,
    worker_stop: AtomicBool,
    worker_thread: OnceLock<Thread>,
    worker_progress: (Mutex<u64>, Condvar),
}

unsafe impl Send for DataExchangeState {}
unsafe impl Sync for DataExchangeState {}

unsafe impl Send for DataExchangeCore {}
unsafe impl Sync for DataExchangeCore {}

impl DataExchangeState {
    pub fn new() -> Arc<Self> {
        let core = Arc::new(DataExchangeCore {
            queues: std::array::from_fn(|_| AtomicPtr::new(ptr::null_mut())),
            generations: std::array::from_fn(|_| AtomicU32::new(0)),
            lifecycle: Mutex::new(()),
            receiver: Mutex::new(None),
            processor: AtomicPtr::new(ptr::null_mut()),
            active: AtomicBool::new(false),
            in_process: AtomicBool::new(false),
            allocated_bytes: AtomicUsize::new(0),
            snapshots: Mutex::new(SnapshotSink::default()),
            control_thread: thread::current().id(),
            worker_stop: AtomicBool::new(false),
            worker_thread: OnceLock::new(),
            worker_progress: (Mutex::new(0), Condvar::new()),
        });
        let dispatcher_core = Arc::clone(&core);
        let join = thread::Builder::new()
            .name("vst3-data-exchange".to_string())
            .stack_size(DISPATCHER_STACK_BYTES)
            .spawn(move || loop {
                thread::park();
                if dispatcher_core.worker_stop.load(Ordering::Acquire) {
                    break;
                }
                dispatcher_core.dispatch_matching(true);
                let (sequence, wake) = &dispatcher_core.worker_progress;
                let mut sequence = sequence.lock().unwrap_or_else(|p| p.into_inner());
                *sequence = sequence.wrapping_add(1);
                wake.notify_all();
            })
            .expect("failed to spawn VST3 data-exchange dispatcher");
        let _ = core.worker_thread.set(join.thread().clone());
        Arc::new(Self {
            core,
            worker_join: Mutex::new(Some(join)),
        })
    }

    /// Borrow every actual exchange allocation at the component control boundary.
    /// The lifecycle lock pins raw queue owners against close/background dispatch.
    /// No receiver callback, queue drain or plugin-owned payload is inspected.
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::{HostStorageError, HostStorageRef, HostThreadStorageRef};
        if thread::current().id() != self.core.control_thread {
            return Err(HostStorageError::WrongThread);
        }
        visitor.visit(HostStorageRef::arc(&self.core))?;
        let lifecycle = self
            .core
            .lifecycle
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        for cell in &self.core.queues {
            let ptr = cell.load(Ordering::Acquire);
            // All queue destruction and pointer changes require this same lifecycle lock.
            if let Some(queue) = unsafe { ptr.as_ref() } {
                visitor.visit(HostStorageRef::owned_layout(
                    queue,
                    ptr as usize,
                    Layout::new::<ExchangeQueue>(),
                ))?;
                queue.visit_owned_storage(visitor)?;
            }
        }
        let snapshots = self
            .core
            .snapshots
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        snapshots.visit_owned_storage(visitor)?;
        drop(snapshots);
        drop(lifecycle);
        let join = self
            .worker_join
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        if let Some(join) = join.as_ref() {
            visitor.visit_thread(HostThreadStorageRef::new(
                join.thread(),
                DISPATCHER_STACK_BYTES,
                true,
                join.is_finished(),
            ))?;
        } else if let Some(worker) = self.core.worker_thread.get() {
            visitor.visit_thread(HostThreadStorageRef::new(
                worker,
                DISPATCHER_STACK_BYTES,
                false,
                true,
            ))?;
        }
        Ok(())
    }

    pub fn configure(
        &self,
        processor: *mut IAudioProcessor,
        receiver: Option<ComPtr<IDataExchangeReceiver>>,
    ) {
        self.core.processor.store(processor, Ordering::Release);
        *self.core.receiver.lock().unwrap_or_else(|p| p.into_inner()) = receiver;
    }

    pub fn set_active(&self, active: bool) {
        self.core.active.store(active, Ordering::Release);
    }

    pub fn enter_process(&self) {
        self.core.in_process.store(true, Ordering::Release);
    }

    pub fn leave_process(&self) {
        self.core.in_process.store(false, Ordering::Release);
    }

    fn queue(&self, id: u32) -> Option<&ExchangeQueue> {
        let slot = (id & ((1 << QUEUE_INDEX_BITS) - 1)) as usize;
        let ptr = self.core.queues.get(slot)?.load(Ordering::Acquire);
        let queue = unsafe { ptr.as_ref()? };
        (queue.id == id).then_some(queue)
    }

    fn reserve_bytes(&self, bytes: usize) -> bool {
        self.core
            .allocated_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= MAX_TOTAL_QUEUE_BYTES)
            })
            .is_ok()
    }

    pub unsafe fn open_queue(
        &self,
        processor: *mut IAudioProcessor,
        block_size: u32,
        num_blocks: u32,
        alignment: u32,
        user_context_id: u32,
        out_id: *mut u32,
    ) -> tresult {
        if out_id.is_null() {
            return kInvalidArgument;
        }
        *out_id = InvalidDataExchangeQueueID;
        if thread::current().id() != self.core.control_thread
            || self.core.active.load(Ordering::Acquire)
            || processor.is_null()
            || processor != self.core.processor.load(Ordering::Acquire)
        {
            return kInvalidArgument;
        }
        let (block_size, num_blocks) = (block_size as usize, num_blocks as usize);
        if block_size == 0
            || block_size > MAX_BLOCK_SIZE
            || num_blocks == 0
            || num_blocks > MAX_BLOCKS_PER_QUEUE
        {
            return kInvalidArgument;
        }
        let alignment = if alignment == 0 {
            std::mem::align_of::<u128>()
        } else {
            alignment as usize
        };
        if !alignment.is_power_of_two() {
            return kInvalidArgument;
        }
        let Some(total_bytes) = block_size.checked_mul(num_blocks) else {
            return kInvalidArgument;
        };
        if !self.reserve_bytes(total_bytes) {
            return kOutOfMemory;
        }

        let _lifecycle = self
            .core
            .lifecycle
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(slot) = self
            .core
            .queues
            .iter()
            .position(|slot| slot.load(Ordering::Acquire).is_null())
        else {
            self.core
                .allocated_bytes
                .fetch_sub(total_bytes, Ordering::AcqRel);
            return kOutOfMemory;
        };
        let generation = self.core.generations[slot]
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            & ((1 << (31 - QUEUE_INDEX_BITS)) - 1);
        let id = (generation << QUEUE_INDEX_BITS) | slot as u32;

        let receiver = self
            .core
            .receiver
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let Some(receiver) = receiver else {
            self.core
                .allocated_bytes
                .fetch_sub(total_bytes, Ordering::AcqRel);
            return kNoInterface;
        };
        let Some(mut queue) = ExchangeQueue::new(
            id,
            user_context_id,
            block_size,
            num_blocks,
            alignment,
            false,
        ) else {
            self.core
                .allocated_bytes
                .fetch_sub(total_bytes, Ordering::AcqRel);
            return kOutOfMemory;
        };
        let mut background = 0;
        receiver.queueOpened(user_context_id, block_size as u32, &mut background);
        queue.background = background != 0;
        self.core.queues[slot].store(Box::into_raw(Box::new(queue)), Ordering::Release);
        *out_id = id;
        kResultTrue
    }

    pub unsafe fn close_queue(&self, id: u32) -> tresult {
        if thread::current().id() != self.core.control_thread
            || self.core.active.load(Ordering::Acquire)
        {
            return kInvalidArgument;
        }
        let _lifecycle = self
            .core
            .lifecycle
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let slot = (id & ((1 << QUEUE_INDEX_BITS) - 1)) as usize;
        let Some(cell) = self.core.queues.get(slot) else {
            return kInvalidArgument;
        };
        let ptr = cell.load(Ordering::Acquire);
        let Some(queue) = ptr.as_ref() else {
            return kInvalidArgument;
        };
        if queue.id != id {
            return kInvalidArgument;
        }
        self.core.dispatch_queue(queue, queue.background);
        let ptr = cell.swap(ptr::null_mut(), Ordering::AcqRel);
        let queue = Box::from_raw(ptr);
        if let Some(receiver) = self
            .core
            .receiver
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            receiver.queueClosed(queue.user_context_id);
        }
        self.core
            .allocated_bytes
            .fetch_sub(queue.total_bytes, Ordering::AcqRel);
        drop(queue);
        kResultTrue
    }

    pub unsafe fn lock_block(
        &self,
        id: u32,
        out: *mut vst3::Steinberg::Vst::DataExchangeBlock,
    ) -> tresult {
        if out.is_null() {
            return kInvalidArgument;
        }
        (*out).data = ptr::null_mut();
        (*out).size = 0;
        (*out).blockID = InvalidDataExchangeBlockID;
        if !self.core.active.load(Ordering::Acquire)
            || !self.core.in_process.load(Ordering::Acquire)
        {
            return kInvalidArgument;
        }
        self.queue(id)
            .map_or(kInvalidArgument, |queue| queue.lock(out))
    }

    pub fn free_block(&self, id: u32, block_id: u32, send: bool) -> tresult {
        if !self.core.active.load(Ordering::Acquire)
            || !self.core.in_process.load(Ordering::Acquire)
        {
            return kInvalidArgument;
        }
        let Some(queue) = self.queue(id) else {
            return kInvalidArgument;
        };
        let result = queue.free(block_id, send);
        if result == kResultTrue && send && queue.background {
            if let Some(worker) = self.core.worker_thread.get() {
                worker.unpark();
            }
        }
        result
    }

    fn has_background_pending(&self) -> bool {
        let _lifecycle = self
            .core
            .lifecycle
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        self.core.queues.iter().any(|cell| {
            let ptr = cell.load(Ordering::Acquire);
            unsafe { ptr.as_ref() }
                .is_some_and(|queue| queue.background && !queue.pending.is_empty())
        })
    }

    /// Dispatch ready non-background blocks on the actual host control thread.
    /// This does not wait for background queues or drain diagnostic snapshots.
    pub fn service_main_thread(&self) {
        self.core.dispatch_matching(false);
    }

    /// Deliver all blocks before deactivation, as required by the VST3 contract.
    pub fn flush(&self) {
        self.core.dispatch_matching(false);
        while self.has_background_pending() {
            let Some(worker) = self.core.worker_thread.get() else {
                break;
            };
            let (sequence, wake) = &self.core.worker_progress;
            let sequence = sequence.lock().unwrap_or_else(|p| p.into_inner());
            worker.unpark();
            let _ = wake
                .wait_timeout(sequence, std::time::Duration::from_millis(10))
                .unwrap_or_else(|p| p.into_inner());
        }
    }

    pub fn take_blocks(&self) -> Vec<OwnedDataExchangeBlock> {
        self.core.dispatch_matching(false);
        let mut sink = self
            .core
            .snapshots
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        sink.take_blocks()
    }

    /// Close every open queue and drop the plugin-side references, undoing [`Self::configure`].
    ///
    /// Releasing the receiver here is what makes the teardown sound. The receiver is the
    /// plugin's own edit controller, held as an owning COM reference, but this state lives in
    /// the `HostApplication` — which `PluginImpl` deliberately drops *after* the module (see the
    /// `_host_app` field). Leaving the reference in place defers its `release` until after
    /// `dlclose`, dispatching through a vtable in unmapped memory. The last reference the host
    /// holds into the plugin has to go while the module is still loaded.
    pub fn shutdown(&self) {
        self.flush();
        let ids: Vec<u32> = {
            let _lifecycle = self
                .core
                .lifecycle
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            self.core
                .queues
                .iter()
                .filter_map(|cell| unsafe { cell.load(Ordering::Acquire).as_ref() }.map(|q| q.id))
                .collect()
        };
        for id in ids {
            unsafe {
                let _ = self.close_queue(id);
            }
        }
        self.core
            .processor
            .store(ptr::null_mut(), Ordering::Release);
        // Taken out of the mutex first: `release` runs plugin code, which is free to re-enter
        // the host, and must not do so while this lock is held.
        let receiver = self
            .core
            .receiver
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        drop(receiver);
        // This synchronous control-owner frontier precedes component/controller
        // termination and module destruction in PluginImpl::drop. No dispatcher
        // callback or plugin receiver release can remain beyond this join.
        self.settle_dispatcher();
    }

    fn settle_dispatcher(&self) {
        self.core.worker_stop.store(true, Ordering::Release);
        if let Some(worker) = self.core.worker_thread.get() {
            worker.unpark();
        }
        let join = self
            .worker_join
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        // Never hold a storage lock across joining or final captured Arc retirement.
        if let Some(join) = join {
            let _ = join.join();
        }
    }
}

impl Drop for DataExchangeState {
    fn drop(&mut self) {
        self.settle_dispatcher();
    }
}

impl DataExchangeCore {
    fn dispatch_queue(&self, queue: &ExchangeQueue, on_background_thread: bool) {
        while let Some(block_id) = queue.pending.pop() {
            let Some(block) = queue.blocks.get(block_id as usize) else {
                continue;
            };
            let bytes = unsafe { std::slice::from_raw_parts(block.ptr.as_ptr(), queue.block_size) };
            self.push_snapshot(OwnedDataExchangeBlock {
                queue_id: queue.id,
                user_context_id: queue.user_context_id,
                block_id,
                data: bytes.to_vec(),
            });
            if let Some(receiver) = self
                .receiver
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
            {
                let mut raw = vst3::Steinberg::Vst::DataExchangeBlock {
                    data: block.ptr.as_ptr().cast(),
                    size: queue.block_size as u32,
                    blockID: block_id,
                };
                unsafe {
                    receiver.onDataExchangeBlocksReceived(
                        queue.user_context_id,
                        1,
                        &mut raw,
                        u8::from(on_background_thread),
                    );
                }
            }
            queue.recycle(block_id);
        }
    }

    fn push_snapshot(&self, block: OwnedDataExchangeBlock) {
        let mut sink = self.snapshots.lock().unwrap_or_else(|p| p.into_inner());
        sink.push(block);
    }

    fn dispatch_matching(&self, background: bool) {
        let _lifecycle = self.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        for cell in &self.queues {
            let ptr = cell.load(Ordering::Acquire);
            if let Some(queue) = unsafe { ptr.as_ref() } {
                if queue.background == background {
                    self.dispatch_queue(queue, background);
                }
            }
        }
    }
}

impl Drop for DataExchangeCore {
    fn drop(&mut self) {
        // The parent control owner has joined the dispatcher before its final
        // core reference is retired. This core never owns the JoinHandle.
        for cell in &self.queues {
            let ptr = cell.swap(ptr::null_mut(), Ordering::AcqRel);
            if !ptr.is_null() {
                unsafe { drop(Box::from_raw(ptr)) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_queue_exhausts_and_recycles_without_allocating() {
        let queue = ExchangeQueue::new(32, 7, 64, 2, 32, false).unwrap();
        let mut first = vst3::Steinberg::Vst::DataExchangeBlock {
            data: ptr::null_mut(),
            size: 0,
            blockID: InvalidDataExchangeBlockID,
        };
        let mut second = first;
        let mut exhausted = first;
        assert_eq!(queue.lock(&mut first), kResultTrue);
        assert_eq!(queue.lock(&mut second), kResultTrue);
        assert_eq!(queue.lock(&mut exhausted), kOutOfMemory);
        assert_eq!((first.data as usize) % 32, 0);
        assert_eq!(first.size, 64);

        assert_eq!(queue.free(first.blockID, true), kResultTrue);
        assert_eq!(queue.free(first.blockID, true), kInvalidArgument);
        let pending = queue.pending.pop().unwrap();
        assert_eq!(pending, first.blockID);
        queue.recycle(pending);

        let mut recycled = exhausted;
        assert_eq!(queue.lock(&mut recycled), kResultTrue);
        assert_eq!(recycled.blockID, first.blockID);
        assert_eq!(queue.free(recycled.blockID, false), kResultTrue);
        assert_eq!(queue.free(second.blockID, false), kResultTrue);
    }

    #[test]
    fn exchange_queue_rejects_invalid_layouts() {
        assert!(ExchangeQueue::new(1, 0, 0, 1, 8, false).is_none());
        assert!(ExchangeQueue::new(1, 0, 8, 1, 3, false).is_none());
    }
}

#[cfg(test)]
mod dispatcher_owner_teardown_contracts {
    use super::*;
    use std::time::{Duration, Instant};
    use vst3::{Class, ComWrapper};

    #[derive(Default)]
    struct CallbackGate {
        state: Mutex<(bool, bool)>,
        changed: Condvar,
        receiver_retired: AtomicBool,
        module_alive: AtomicBool,
        late_native_release: AtomicBool,
    }

    struct BlockingReceiver(Arc<CallbackGate>);
    impl Class for BlockingReceiver {
        type Interfaces = (IDataExchangeReceiver,);
    }
    impl Drop for BlockingReceiver {
        fn drop(&mut self) {
            if !self.0.module_alive.load(Ordering::Acquire) {
                self.0.late_native_release.store(true, Ordering::Release);
            }
            self.0.receiver_retired.store(true, Ordering::Release);
            self.0.changed.notify_all();
        }
    }
    impl IDataExchangeReceiverTrait for BlockingReceiver {
        unsafe fn queueOpened(&self, _: u32, _: u32, background: *mut TBool) {
            if !background.is_null() {
                unsafe { *background = 1 };
            }
        }
        unsafe fn queueClosed(&self, _: u32) {}
        unsafe fn onDataExchangeBlocksReceived(
            &self,
            _: u32,
            count: u32,
            blocks: *mut vst3::Steinberg::Vst::DataExchangeBlock,
            background: TBool,
        ) {
            assert_eq!(count, 1);
            assert!(!blocks.is_null());
            assert_ne!(background, 0);
            let mut gate = self.0.state.lock().expect("callback gate");
            gate.0 = true;
            self.0.changed.notify_all();
            while !gate.1 {
                gate = self.0.changed.wait(gate).expect("callback release");
            }
        }
    }

    fn start_blocked_dispatch() -> (Arc<DataExchangeState>, Arc<CallbackGate>) {
        let state = DataExchangeState::new();
        let gate = Arc::new(CallbackGate::default());
        gate.module_alive.store(true, Ordering::Release);
        let receiver = ComWrapper::new(BlockingReceiver(Arc::clone(&gate)));
        let pointer = receiver
            .to_com_ptr::<IDataExchangeReceiver>()
            .expect("receiver interface");
        state.configure(ptr::null_mut(), Some(pointer));
        drop(receiver);
        // Host-internal queue/lifetime control, not a claimed plugin/standards fixture.
        let queue = ExchangeQueue::new(32, 19, 16, 1, 16, true).expect("exchange queue");
        let mut block = vst3::Steinberg::Vst::DataExchangeBlock {
            data: ptr::null_mut(),
            size: 0,
            blockID: InvalidDataExchangeBlockID,
        };
        assert_eq!(queue.lock(&mut block), kResultTrue);
        assert_eq!(queue.free(block.blockID, true), kResultTrue);
        state
            .core
            .allocated_bytes
            .store(queue.total_bytes, Ordering::Release);
        state.core.queues[0].store(Box::into_raw(Box::new(queue)), Ordering::Release);
        state
            .core
            .worker_thread
            .get()
            .expect("actual worker")
            .unpark();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut locked = gate.state.lock().expect("start gate");
        while !locked.0 {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "actual dispatcher did not enter receiver");
            let (next, timeout) = gate.changed.wait_timeout(locked, left).expect("start wait");
            locked = next;
            assert!(
                !timeout.timed_out() || locked.0,
                "actual dispatcher did not enter receiver"
            );
        }
        drop(locked);
        (state, gate)
    }

    #[test]
    fn dispatcher_owner_shutdown_joins_before_native_module_retirement() {
        let (state, gate) = start_blocked_dispatch();
        let release_gate = Arc::clone(&gate);
        let release = thread::spawn(move || {
            let mut locked = release_gate.state.lock().expect("release gate");
            locked.1 = true;
            release_gate.changed.notify_all();
        });
        state.shutdown();
        release.join().expect("callback release thread");
        assert!(
            gate.receiver_retired.load(Ordering::Acquire),
            "native receiver released in module lifetime"
        );
        assert!(state.worker_join.lock().expect("join owner").is_none(),
            "shutdown must consume and settle the real JoinHandle before the native module can unload");
        gate.module_alive.store(false, Ordering::Release);
        drop(state);
        assert!(!gate.late_native_release.load(Ordering::Acquire));
    }

    #[test]
    fn dispatcher_owner_last_parent_drop_cannot_join_its_own_dispatcher() {
        let (state, gate) = start_blocked_dispatch();
        let weak_parent = Arc::downgrade(&state);
        let release_gate = Arc::clone(&gate);
        let release = thread::spawn(move || {
            // The old implementation holds its parent's last Arc inside the callback.
            // Wait until that parent reference has been retired. The repaired worker
            // owns only a separate shared core, so it never retains this parent.
            let deadline = Instant::now() + Duration::from_secs(2);
            while weak_parent.strong_count() > 1 {
                assert!(Instant::now() < deadline, "parent teardown did not begin");
                thread::yield_now();
            }
            let mut locked = release_gate.state.lock().expect("release gate");
            locked.1 = true;
            release_gate.changed.notify_all();
        });
        drop(state);
        release.join().expect("callback release thread");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut locked = gate.state.lock().expect("retirement gate");
        while !gate.receiver_retired.load(Ordering::Acquire) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let (next, _) = gate
                .changed
                .wait_timeout(locked, left)
                .expect("retirement wait");
            locked = next;
        }
        drop(locked);
        assert!(gate.receiver_retired.load(Ordering::Acquire),
            "last parent Drop must finish the callback, join from its control owner, and retire the native receiver");
        gate.module_alive.store(false, Ordering::Release);
        assert!(!gate.late_native_release.load(Ordering::Acquire));
    }
}

#[cfg(test)]
mod data_exchange_storage_contracts {
    use super::*;
    use crate::host_storage::{
        HostStorageError, HostStorageKind, HostStorageRef, HostStorageVisitor,
    };
    use std::alloc::{GlobalAlloc, System};
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct Allocation {
        operation: u8,
        address: usize,
        bytes: usize,
        alignment: usize,
    }
    #[derive(Clone, Copy)]
    struct Trace {
        enabled: bool,
        overflow: bool,
        len: usize,
        entries: [Allocation; 64],
    }
    thread_local! {
        static TRACE: Cell<Trace> = const { Cell::new(Trace {
            enabled: false, overflow: false, len: 0,
            entries: [Allocation { operation: 0, address: 0, bytes: 0, alignment: 0 }; 64],
        }) };
    }
    fn record(operation: u8, pointer: *mut u8, layout: Layout) {
        let _ = TRACE.try_with(|slot| {
            let mut trace = slot.get();
            if !trace.enabled {
                return;
            }
            if trace.len == trace.entries.len() {
                trace.overflow = true;
            } else {
                trace.entries[trace.len] = Allocation {
                    operation,
                    address: pointer as usize,
                    bytes: layout.size(),
                    alignment: layout.align(),
                };
                trace.len += 1;
            }
            slot.set(trace);
        });
    }
    struct ObserveAllocator;
    // Test binary only; original pointer/Layout pass through unchanged. The
    // warmed fixed journal never allocates and does not inspect allocation bytes.
    unsafe impl GlobalAlloc for ObserveAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc(layout) };
            record(1, pointer, layout);
            pointer
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc_zeroed(layout) };
            record(1, pointer, layout);
            pointer
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            record(2, pointer, layout);
            unsafe { System.dealloc(pointer, layout) };
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
            record(3, pointer, layout);
            let next = unsafe { System.realloc(pointer, layout, bytes) };
            if let Ok(layout) = Layout::from_size_align(bytes, layout.align()) {
                record(4, next, layout);
            }
            next
        }
    }
    #[global_allocator]
    static ALLOCATOR: ObserveAllocator = ObserveAllocator;
    struct Recording;
    impl Drop for Recording {
        fn drop(&mut self) {
            TRACE.with(|slot| {
                let mut trace = slot.get();
                trace.enabled = false;
                slot.set(trace);
            });
        }
    }
    fn capture<T>(operation: impl FnOnce() -> T) -> (T, Trace) {
        TRACE.with(|slot| {
            let mut trace = slot.get();
            trace.enabled = true;
            trace.overflow = false;
            trace.len = 0;
            slot.set(trace);
        });
        let guard = Recording;
        let result = operation();
        let trace = TRACE.with(Cell::get);
        drop(guard);
        assert!(!trace.overflow, "actual allocator journal overflowed");
        (result, trace)
    }
    #[derive(Default)]
    struct Inventory {
        entries: [Allocation; 16],
        len: usize,
    }
    impl HostStorageVisitor for Inventory {
        fn visit(&mut self, storage: HostStorageRef<'_>) -> Result<(), HostStorageError> {
            if self.len == self.entries.len() {
                return Err(HostStorageError::ObserverRefused);
            }
            assert_eq!(storage.kind(), HostStorageKind::CompleteAllocation);
            self.entries[self.len] = Allocation {
                operation: 1,
                address: storage.address(),
                bytes: storage.layout().size(),
                alignment: storage.layout().align(),
            };
            self.len += 1;
            Ok(())
        }
    }
    fn block(id: u32, data: Vec<u8>) -> OwnedDataExchangeBlock {
        OwnedDataExchangeBlock {
            queue_id: 32,
            user_context_id: 19,
            block_id: id,
            data,
        }
    }

    #[test]
    fn data_exchange_queue_actual_allocator_views_cover_every_live_owner() {
        let (queue, construction) = capture(|| {
            Box::new(ExchangeQueue::new(32, 19, 4097, 2, 128, false).expect("actual aligned queue"))
        });
        let mut inventory = Inventory::default();
        let (result, observation) = capture(|| {
            inventory.visit(HostStorageRef::boxed(&queue))?;
            queue.visit_owned_storage(&mut inventory)
        });
        result.expect("complete real queue observation");
        assert_eq!(observation.len, 0, "owning observation allocates nothing");
        assert_eq!(
            construction.len, inventory.len,
            "no live host allocation is omitted"
        );
        for actual in &construction.entries[..construction.len] {
            assert_eq!(
                actual.operation, 1,
                "this pinned constructor has no hidden temporary/reallocation"
            );
            assert_ne!(actual.address, 0);
            assert_eq!(
                inventory.entries[..inventory.len]
                    .iter()
                    .filter(|row| *row == actual)
                    .count(),
                1,
                "actual allocator pointer and complete request layout must match once"
            );
        }
        let (_, destruction) = capture(|| drop(queue));
        assert_eq!(destruction.len, construction.len);
        for actual in &destruction.entries[..destruction.len] {
            assert_eq!(actual.operation, 2);
            let mut allocation = *actual;
            allocation.operation = 1;
            assert!(
                construction.entries[..construction.len].contains(&allocation),
                "final real owner must deallocate its original pointer/Layout"
            );
        }
    }

    #[test]
    fn data_exchange_snapshot_ring_actual_capacity_and_transfer_owner() {
        let (mut sink, ring_creation) = capture(SnapshotSink::default);
        assert_eq!(
            ring_creation.len, 1,
            "fixed owning ring has one backing request"
        );
        let (mut payload, payload_creation) = capture(|| Vec::<u8>::with_capacity(257));
        payload.resize(3, 7);
        assert_eq!(payload_creation.len, 1);
        let capacity = payload.capacity();
        let address = payload.as_ptr() as usize;
        let (_, publication) = capture(|| sink.push(block(7, payload)));
        assert_eq!(
            publication.len, 0,
            "ring never resizes when a snapshot is transferred"
        );
        assert_eq!(
            sink.bytes, capacity,
            "allocator storage capacity, not used payload length"
        );
        let mut inventory = Inventory::default();
        let (result, observation) = capture(|| sink.visit_owned_storage(&mut inventory));
        result.expect("snapshot owner observation");
        assert_eq!(observation.len, 0);
        assert_eq!(inventory.len, 2);
        assert!(inventory.entries[..inventory.len].contains(&ring_creation.entries[0]));
        assert!(inventory.entries[..inventory.len].contains(&payload_creation.entries[0]));
        let (taken, transfer) = capture(|| sink.take_blocks());
        assert_eq!(
            transfer.len, 1,
            "only returned outer Vec is newly allocated"
        );
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].block_id, 7);
        assert_eq!(taken[0].data, [7, 7, 7]);
        assert_eq!(
            taken[0].data.as_ptr() as usize,
            address,
            "payload moves without cloning"
        );
        assert_eq!(taken[0].data.capacity(), capacity);
        assert_eq!((sink.len, sink.bytes), (0, 0));
        let mut empty = Inventory::default();
        sink.visit_owned_storage(&mut empty)
            .expect("empty ring retains its actual backing");
        assert_eq!(empty.len, 1);
        assert_eq!(empty.entries[0], ring_creation.entries[0]);
    }

    #[test]
    fn data_exchange_snapshot_ring_wrap_keeps_exact_fifo_and_fixed_backing() {
        let mut sink = SnapshotSink::default();
        let pointer = sink.blocks.as_ptr();
        let capacity = sink.blocks.capacity();
        for id in 0..(MAX_SNAPSHOT_BLOCKS as u32 + 7) {
            sink.push(block(id, Vec::new()));
        }
        assert_eq!((sink.len, sink.bytes), (MAX_SNAPSHOT_BLOCKS, 0));
        assert_eq!(sink.blocks.as_ptr(), pointer);
        assert_eq!(sink.blocks.capacity(), capacity);
        let taken = sink.take_blocks();
        assert_eq!(taken.len(), MAX_SNAPSHOT_BLOCKS);
        for (index, block) in taken.iter().enumerate() {
            assert_eq!(block.block_id, index as u32 + 7);
        }
        assert!(sink.take_blocks().is_empty());
        assert_eq!(sink.blocks.as_ptr(), pointer);
        assert_eq!(sink.blocks.capacity(), capacity);
    }

    #[test]
    fn data_exchange_thread_profile_missing_is_not_a_zero_charge_success() {
        // A visitor with no qualified std-thread profile must explicitly refuse
        // this family even after every visible exchange allocation was observed.
        let state = DataExchangeState::new();
        struct ObserveVisible;
        impl HostStorageVisitor for ObserveVisible {
            fn visit(&mut self, _: HostStorageRef<'_>) -> Result<(), HostStorageError> {
                Ok(())
            }
        }
        assert_eq!(
            state.visit_owned_storage(&mut ObserveVisible),
            Err(HostStorageError::UnqualifiedStdThreadOwner)
        );
        state.shutdown();
    }
    #[test]
    fn data_exchange_snapshot_oversize_capacity_preserves_prior_accepted_diagnostics() {
        let mut sink = SnapshotSink::default();
        let mut accepted = Vec::with_capacity(257);
        accepted.extend_from_slice(&[3, 5, 7]);
        let original_address = accepted.as_ptr() as usize;
        let original_capacity = accepted.capacity();
        sink.push(block(11, accepted));
        // The incoming used length is zero, but its actual owned capacity
        // exceeds the public diagnostic byte frontier. It cannot evict a
        // previously accepted payload and then disappear silently.
        let oversize = Vec::with_capacity(MAX_SNAPSHOT_BYTES + 1);
        assert!(oversize.capacity() > MAX_SNAPSHOT_BYTES);
        sink.push(block(12, oversize));
        assert_eq!((sink.len, sink.bytes), (1, original_capacity));
        let taken = sink.take_blocks();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].block_id, 11);
        assert_eq!(taken[0].data, [3, 5, 7]);
        assert_eq!(taken[0].data.as_ptr() as usize, original_address);
    }

    #[test]
    fn data_exchange_snapshot_byte_frontier_evicts_fifo_by_real_capacity() {
        let mut sink = SnapshotSink::default();
        let mut first = Vec::with_capacity(MAX_SNAPSHOT_BYTES / 2);
        first.push(1);
        let mut second = Vec::with_capacity(MAX_SNAPSHOT_BYTES / 2);
        second.push(2);
        let first_capacity = first.capacity();
        let second_capacity = second.capacity();
        let second_address = second.as_ptr() as usize;
        assert_eq!(
            first_capacity + second_capacity,
            MAX_SNAPSHOT_BYTES,
            "this actual constructor must reach the precise byte boundary"
        );
        sink.push(block(21, first));
        sink.push(block(22, second));
        assert_eq!((sink.len, sink.bytes), (2, MAX_SNAPSHOT_BYTES));
        let mut incoming = Vec::with_capacity(1);
        incoming.push(3);
        let incoming_capacity = incoming.capacity();
        sink.push(block(23, incoming));
        assert_eq!(
            (sink.len, sink.bytes),
            (2, second_capacity + incoming_capacity)
        );
        let taken = sink.take_blocks();
        assert_eq!(
            taken.iter().map(|row| row.block_id).collect::<Vec<_>>(),
            [22, 23]
        );
        assert_eq!(taken[0].data, [2]);
        assert_eq!(taken[0].data.as_ptr() as usize, second_address);
        assert_eq!(taken[1].data, [3]);
        assert_eq!((sink.len, sink.bytes), (0, 0));
    }
}
