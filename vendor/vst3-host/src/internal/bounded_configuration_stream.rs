//! Fixed state-capture storage; no growth from an untrusted IBStream write.

use crate::configuration_capture::ConfigurationCaptureError;
use std::sync::Mutex;
use vst3::{
    Class,
    Steinberg::{kOutOfMemory, kResultFalse, kResultOk, tresult, IBStream, IBStreamTrait},
};

struct CaptureBuffer {
    bytes: Box<[u8]>,
    position: usize,
    used: usize,
    write_limit: usize,
    failed: bool,
    sealed: bool,
}

/// A purpose-specific capture stream, exposing the standard IBStream interface.
///
/// Optional stream attributes are unavailable on this bounded capture path.
/// Used state bytes and granted payload allocation bytes are separate facts.
pub(crate) struct ConfigurationCaptureStream {
    buffer: Mutex<CaptureBuffer>,
}

impl ConfigurationCaptureStream {
    pub(crate) fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::{HostStorageError, HostStorageRef};
        let buffer = self
            .buffer
            .lock()
            .map_err(|_| HostStorageError::PoisonedOwner)?;
        visitor.visit(HostStorageRef::boxed(&buffer.bytes))
    }
    pub(crate) fn new(limit: usize) -> Result<Self, ConfigurationCaptureError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(limit)
            .map_err(|_| ConfigurationCaptureError::AllocationFailed)?;
        if bytes.capacity() > limit {
            return Err(ConfigurationCaptureError::AllocationExtent {
                requested: limit,
                actual: bytes.capacity(),
            });
        }
        bytes.resize(limit, 0);
        Ok(Self {
            buffer: Mutex::new(CaptureBuffer {
                bytes: bytes.into_boxed_slice(),
                position: 0,
                used: 0,
                write_limit: limit,
                failed: false,
                sealed: false,
            }),
        })
    }

    pub(crate) fn payload_allocation_bytes(&self) -> Result<usize, ConfigurationCaptureError> {
        Ok(self
            .buffer
            .lock()
            .map_err(|_| ConfigurationCaptureError::PoisonedBuffer)?
            .bytes
            .len())
    }

    /// Reduce the writer extent while retaining its already granted fixed buffer.
    pub(crate) fn set_write_limit(&self, limit: usize) -> Result<(), ConfigurationCaptureError> {
        let mut buffer = self
            .buffer
            .lock()
            .map_err(|_| ConfigurationCaptureError::PoisonedBuffer)?;
        if buffer.sealed || buffer.used != 0 || buffer.position != 0 || limit > buffer.bytes.len() {
            return Err(ConfigurationCaptureError::InvalidCaptureState);
        }
        buffer.write_limit = limit;
        Ok(())
    }

    /// Freeze bytes after getState returns; later retained-stream writes are refused.
    pub(crate) fn seal(&self) -> Result<usize, ConfigurationCaptureError> {
        let mut buffer = self
            .buffer
            .lock()
            .map_err(|_| ConfigurationCaptureError::PoisonedBuffer)?;
        buffer.sealed = true;
        if buffer.failed {
            return Err(ConfigurationCaptureError::StateLimitExceeded {
                limit: buffer.write_limit,
            });
        }
        Ok(buffer.used)
    }

    /// Copy only into already granted envelope capacity; never grows the target.
    pub(crate) fn append_to(&self, target: &mut Vec<u8>) -> Result<(), ConfigurationCaptureError> {
        let buffer = self
            .buffer
            .lock()
            .map_err(|_| ConfigurationCaptureError::PoisonedBuffer)?;
        if !buffer.sealed || buffer.failed {
            return Err(ConfigurationCaptureError::InvalidCaptureState);
        }
        let end = target
            .len()
            .checked_add(buffer.used)
            .ok_or(ConfigurationCaptureError::ExtentOverflow)?;
        if end > target.capacity() {
            return Err(ConfigurationCaptureError::StateLimitExceeded {
                limit: target.capacity(),
            });
        }
        target.extend_from_slice(&buffer.bytes[..buffer.used]);
        Ok(())
    }

    fn write_bytes(&self, source: &[u8]) -> Result<usize, ()> {
        let mut buffer = self.buffer.lock().map_err(|_| ())?;
        if buffer.sealed {
            return Err(());
        }
        let end = buffer.position.checked_add(source.len());
        let Some(end) = end.filter(|end| *end <= buffer.write_limit) else {
            buffer.failed = true;
            return Err(());
        };
        let position = buffer.position;
        buffer.bytes[position..end].copy_from_slice(source);
        buffer.used = buffer.used.max(end);
        buffer.position = end;
        Ok(source.len())
    }

    fn seek_position(&self, offset: i64, mode: i32) -> Result<i64, ()> {
        let mut buffer = self.buffer.lock().map_err(|_| ())?;
        if buffer.sealed {
            return Err(());
        }
        let base = match mode {
            0 => 0,
            1 => i64::try_from(buffer.position).map_err(|_| ())?,
            2 => i64::try_from(buffer.used).map_err(|_| ())?,
            _ => return Err(()),
        };
        let next = base
            .checked_add(offset)
            .and_then(|value| usize::try_from(value).ok());
        let Some(next) = next.filter(|next| *next <= buffer.write_limit) else {
            buffer.failed = true;
            return Err(());
        };
        buffer.position = next;
        i64::try_from(next).map_err(|_| ())
    }
}

impl Class for ConfigurationCaptureStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for ConfigurationCaptureStream {
    unsafe fn read(&self, target: *mut std::ffi::c_void, count: i32, read: *mut i32) -> tresult {
        if !read.is_null() {
            unsafe {
                *read = 0;
            }
        }
        if count < 0 || (count > 0 && target.is_null()) {
            return kResultFalse;
        }
        let Ok(mut buffer) = self.buffer.lock() else {
            return kResultFalse;
        };
        let start = buffer.position.min(buffer.used);
        let end = start.saturating_add(count as usize).min(buffer.used);
        let actual = end - start;
        if actual > 0 {
            // SAFETY: the native caller owns count writable bytes. Our source is
            // within the fixed captured extent, and actual never exceeds count.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    buffer.bytes[start..end].as_ptr(),
                    target.cast::<u8>(),
                    actual,
                );
            }
        }
        buffer.position = end;
        if !read.is_null() {
            unsafe {
                *read = actual as i32;
            }
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        source: *mut std::ffi::c_void,
        count: i32,
        written: *mut i32,
    ) -> tresult {
        if !written.is_null() {
            unsafe {
                *written = 0;
            }
        }
        if count < 0 || (count > 0 && source.is_null()) {
            return kResultFalse;
        }
        let source = if count == 0 {
            &[]
        } else {
            // SAFETY: the native caller provides count readable bytes. Null and
            // negative extents were rejected before constructing the borrowed view.
            unsafe { std::slice::from_raw_parts(source.cast::<u8>(), count as usize) }
        };
        let Ok(actual) = self.write_bytes(source) else {
            return kOutOfMemory;
        };
        if !written.is_null() {
            unsafe {
                *written = actual as i32;
            }
        }
        kResultOk
    }

    unsafe fn seek(&self, offset: i64, mode: i32, result: *mut i64) -> tresult {
        let Ok(position) = self.seek_position(offset, mode) else {
            return kResultFalse;
        };
        if !result.is_null() {
            unsafe {
                *result = position;
            }
        }
        kResultOk
    }

    unsafe fn tell(&self, result: *mut i64) -> tresult {
        if result.is_null() {
            return kResultFalse;
        }
        let Ok(buffer) = self.buffer.lock() else {
            return kResultFalse;
        };
        let Ok(position) = i64::try_from(buffer.position) else {
            return kResultFalse;
        };
        unsafe {
            *result = position;
        }
        kResultOk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_configuration_capture_sparse_and_ignored_overflow_are_bounded() {
        let stream = ConfigurationCaptureStream::new(8).expect("fixed stream");
        assert_eq!(stream.payload_allocation_bytes().expect("extent"), 8);
        assert_eq!(stream.write_bytes(&[1, 2]), Ok(2));
        assert_eq!(stream.seek_position(7, 0), Ok(7));
        assert_eq!(stream.write_bytes(&[9]), Ok(1));
        assert_eq!(stream.write_bytes(&[10]), Err(()));
        assert!(matches!(
            stream.seal(),
            Err(ConfigurationCaptureError::StateLimitExceeded { limit: 8 })
        ));
        assert_eq!(
            stream.payload_allocation_bytes().expect("unchanged extent"),
            8
        );
        let mut output = Vec::with_capacity(8);
        assert!(
            stream.append_to(&mut output).is_err(),
            "ignored native write failure cannot publish truncated state"
        );
        assert!(output.is_empty());
    }

    #[test]
    fn native_configuration_capture_copies_sparse_zeroes_and_seals_without_growth() {
        let stream = ConfigurationCaptureStream::new(8).expect("fixed stream");
        assert_eq!(stream.write_bytes(&[1, 2]), Ok(2));
        assert_eq!(stream.seek_position(7, 0), Ok(7));
        assert_eq!(stream.write_bytes(&[9]), Ok(1));
        assert_eq!(stream.seal().expect("valid capture"), 8);
        assert_eq!(
            stream.write_bytes(&[3]),
            Err(()),
            "state bytes remain sealed"
        );
        let mut output = Vec::with_capacity(8);
        let capacity = output.capacity();
        stream.append_to(&mut output).expect("granted output");
        assert_eq!(output, [1, 2, 0, 0, 0, 0, 0, 9]);
        assert_eq!(output.capacity(), capacity);
        let mut insufficient = Vec::with_capacity(7);
        assert!(stream.append_to(&mut insufficient).is_err());
        assert!(insufficient.is_empty());
    }

    #[test]
    fn native_configuration_capture_sdk_zero_length_pointer_operations_are_valid() {
        let stream = ConfigurationCaptureStream::new(0).expect("empty state stream");
        let mut written = -1;
        let mut read = -1;
        unsafe {
            assert_eq!(
                stream.write(std::ptr::null_mut(), 0, &mut written),
                kResultOk
            );
            assert_eq!(stream.read(std::ptr::null_mut(), 0, &mut read), kResultOk);
        }
        assert_eq!((written, read), (0, 0));
        assert_eq!(stream.seal().expect("empty capture"), 0);
        assert_eq!(stream.payload_allocation_bytes().expect("empty extent"), 0);
    }

    #[test]
    fn native_configuration_capture_combined_controller_limit_precedes_write() {
        let stream = ConfigurationCaptureStream::new(8).expect("preallocated controller stream");
        stream
            .set_write_limit(2)
            .expect("combined component/controller remainder");
        assert_eq!(
            stream
                .payload_allocation_bytes()
                .expect("retained fixed bytes"),
            8
        );
        assert_eq!(stream.write_bytes(&[1, 2]), Ok(2));
        assert_eq!(stream.write_bytes(&[3]), Err(()));
        assert!(matches!(
            stream.seal(),
            Err(ConfigurationCaptureError::StateLimitExceeded { limit: 2 })
        ));
    }
}
