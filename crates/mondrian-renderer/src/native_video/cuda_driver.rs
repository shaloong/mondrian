//! Linux CUDA driver ABI used only by the Vulkan native-memory Adapter.
//! Layouts follow CUDA Driver API / FFmpeg nv-codec-headers dynlink_cuda.h.
use std::ffi::c_void;
use std::sync::Arc;

type Handle = *mut c_void;

#[derive(Debug, thiserror::Error)]
pub(super) enum CudaError {
    #[error("CUDA driver library/symbol unavailable: {0}")]
    Library(#[from] libloading::Error),
    #[error("CUDA {operation} failed with code {code}")]
    Call { operation: &'static str, code: i32 },
    #[error("CUDA has no device with the renderer's exact UUID")]
    DeviceIdentity,
}
pub(super) fn check(operation: &'static str, code: i32) -> Result<(), CudaError> {
    if code == 0 {
        Ok(())
    } else {
        Err(CudaError::Call { operation, code })
    }
}

// Publish foreign output ownership only under the acquisition result contract.
pub(super) fn acquire<T: Default>(
    operation: &'static str,
    owned: &mut T,
    call: impl FnOnce(*mut T) -> i32,
) -> Result<(), CudaError> {
    let mut pending = T::default();
    check(operation, call(&mut pending))?;
    *owned = pending;
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) union ExternalHandle {
    pub fd: i32,
    _abi: [usize; 2],
}
#[repr(C)]
pub(super) struct MemoryHandle {
    pub kind: i32,
    pub handle: ExternalHandle,
    pub size: u64,
    pub flags: u32,
    pub reserved: [u32; 16],
}
#[repr(C)]
pub(super) struct BufferDesc {
    pub offset: u64,
    pub size: u64,
    pub flags: u32,
    pub reserved: [u32; 16],
}
#[repr(C)]
pub(super) struct SemaphoreHandle {
    pub kind: i32,
    pub handle: ExternalHandle,
    pub flags: u32,
    pub reserved: [u32; 16],
}
#[repr(C)]
#[derive(Default)]
pub(super) struct SignalParams {
    pub value: u64,
    pub inner_reserved: [u32; 16],
    pub flags: u32,
    pub reserved: [u32; 16],
}
#[repr(C)]
pub(super) struct Copy2d {
    pub src_x: usize,
    pub src_y: usize,
    pub src_kind: i32,
    pub src_host: *const c_void,
    pub src_device: usize,
    pub src_array: Handle,
    pub src_pitch: usize,
    pub dst_x: usize,
    pub dst_y: usize,
    pub dst_kind: i32,
    pub dst_host: Handle,
    pub dst_device: usize,
    pub dst_array: Handle,
    pub dst_pitch: usize,
    pub width_bytes: usize,
    pub height: usize,
}

macro_rules! driver {
    ($($field:ident : $name:literal ($($arg:ty),*) ),* $(,)?) => {
        pub(super) struct CudaDriver {
            $(pub $field: unsafe extern "C" fn($($arg),*) -> i32,)*
            _library: libloading::Library,
        }
        impl CudaDriver {
            pub fn load() -> Result<Arc<Self>, CudaError> {
                // SAFETY: fixed system driver name; its lifetime is retained by
                // every function-table owner and every outstanding transfer.
                let library = unsafe { libloading::Library::new("libcuda.so.1") }?;
                let api = Self {
                    $($field: unsafe { *library.get::<unsafe extern "C" fn($($arg),*) -> i32>(concat!($name, "\0").as_bytes())? },)*
                    _library: library,
                };
                check("cuInit", unsafe { (api.init)(0) })?;
                Ok(Arc::new(api))
            }
        }
    }
}
driver! {
    init: "cuInit" (u32),
    device_count: "cuDeviceGetCount" (*mut i32),
    device_get: "cuDeviceGet" (*mut i32, i32),
    device_uuid: "cuDeviceGetUuid" (*mut [u8;16], i32),
    context_device: "cuCtxGetDevice" (*mut i32),
    push: "cuCtxPushCurrent_v2" (Handle),
    pop: "cuCtxPopCurrent_v2" (*mut Handle),
    stream_create: "cuStreamCreate" (*mut Handle, u32),
    stream_sync: "cuStreamSynchronize" (Handle),
    stream_destroy: "cuStreamDestroy_v2" (Handle),
    event_create: "cuEventCreate" (*mut Handle, u32),
    event_record: "cuEventRecord" (Handle, Handle),
    event_destroy: "cuEventDestroy_v2" (Handle),
    stream_wait_event: "cuStreamWaitEvent" (Handle, Handle, u32),
    import_memory: "cuImportExternalMemory" (*mut Handle, *const MemoryHandle),
    map_buffer: "cuExternalMemoryGetMappedBuffer" (*mut usize, Handle, *const BufferDesc),
    free: "cuMemFree_v2" (usize),
    destroy_memory: "cuDestroyExternalMemory" (Handle),
    memset: "cuMemsetD8Async" (usize, u8, usize, Handle),
    copy_2d: "cuMemcpy2DAsync_v2" (*const Copy2d, Handle),
    import_semaphore: "cuImportExternalSemaphore" (*mut Handle, *const SemaphoreHandle),
    signal: "cuSignalExternalSemaphoresAsync" (*const Handle, *const SignalParams, u32, Handle),
    destroy_semaphore: "cuDestroyExternalSemaphore" (Handle),
}
impl CudaDriver {
    pub fn matching_ordinal(&self, uuid: [u8; 16]) -> Result<u32, CudaError> {
        let mut count = 0;
        check("cuDeviceGetCount", unsafe {
            (self.device_count)(&mut count)
        })?;
        for ordinal in 0..count {
            let mut device = 0;
            let mut actual = [0; 16];
            check("cuDeviceGet", unsafe {
                (self.device_get)(&mut device, ordinal)
            })?;
            check("cuDeviceGetUuid", unsafe {
                (self.device_uuid)(&mut actual, device)
            })?;
            if actual == uuid {
                return Ok(ordinal as u32);
            }
        }
        Err(CudaError::DeviceIdentity)
    }

    // Caller retains the FFmpeg context owner for this entire scope.
    pub unsafe fn enter(&self, context: Handle) -> Result<ContextGuard<'_>, CudaError> {
        check("cuCtxPushCurrent", unsafe { (self.push)(context) })?;
        Ok(ContextGuard(Some(self)))
    }

    pub fn require_current_uuid(&self, expected: [u8; 16]) -> Result<(), CudaError> {
        let mut device = 0;
        let mut uuid = [0; 16];
        check("cuCtxGetDevice", unsafe {
            (self.context_device)(&mut device)
        })?;
        check("cuDeviceGetUuid", unsafe {
            (self.device_uuid)(&mut uuid, device)
        })?;
        if uuid == expected {
            Ok(())
        } else {
            Err(CudaError::DeviceIdentity)
        }
    }
}
pub(super) struct ContextGuard<'a>(Option<&'a CudaDriver>);
impl ContextGuard<'_> {
    pub fn finish(mut self) -> Result<(), CudaError> {
        if let Some(api) = self.0.take() {
            let mut previous = std::ptr::null_mut();
            check("cuCtxPopCurrent", unsafe { (api.pop)(&mut previous) })?;
        }
        Ok(())
    }
}
impl Drop for ContextGuard<'_> {
    fn drop(&mut self) {
        let Some(api) = self.0.take() else {
            return;
        };
        let mut previous = std::ptr::null_mut();
        let code = unsafe { (api.pop)(&mut previous) };
        if code != 0 {
            tracing::error!(code, "CUDA context restoration failed");
        }
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn acquisition_failure_does_not_publish_foreign_output() {
        let mut mapped = 0usize;
        let result = acquire("fault-injected map", &mut mapped, |output| {
            // A failed foreign call provides no valid ownership of this value.
            unsafe { *output = 0xdead };
            999
        });
        assert!(matches!(result, Err(CudaError::Call { code: 999, .. })));
        assert_eq!(
            mapped, 0,
            "failure must not arm cleanup with an unowned address"
        );
        let mut handle = std::ptr::null_mut::<c_void>();
        assert!(acquire("fault-injected create", &mut handle, |output| {
            unsafe { *output = std::ptr::dangling_mut() };
            999
        })
        .is_err());
        assert!(
            handle.is_null(),
            "failure must not arm a foreign destructor"
        );
        acquire("successful map", &mut mapped, |output| {
            unsafe { *output = 42 };
            0
        })
        .expect("successful ownership transfer");
        assert_eq!(mapped, 42);
    }

    #[test]
    fn driver_abi_matches_linux_64_bit_cuda_headers() {
        // Independently compiled C sizeof/_Alignof/offsetof results from
        // FFmpeg nv-codec-headers dynlink_cuda.h, not Rust-derived values.
        assert_eq!(
            (
                size_of::<MemoryHandle>(),
                align_of::<MemoryHandle>(),
                offset_of!(MemoryHandle, size)
            ),
            (104, 8, 24)
        );
        assert_eq!(
            (
                size_of::<BufferDesc>(),
                align_of::<BufferDesc>(),
                offset_of!(BufferDesc, flags)
            ),
            (88, 8, 16)
        );
        assert_eq!(
            (
                size_of::<SemaphoreHandle>(),
                align_of::<SemaphoreHandle>(),
                offset_of!(SemaphoreHandle, flags)
            ),
            (96, 8, 24)
        );
        assert_eq!(
            (
                size_of::<SignalParams>(),
                align_of::<SignalParams>(),
                offset_of!(SignalParams, flags)
            ),
            (144, 8, 72)
        );
        assert_eq!(
            (
                size_of::<Copy2d>(),
                align_of::<Copy2d>(),
                offset_of!(Copy2d, width_bytes)
            ),
            (128, 8, 112)
        );
    }
}
