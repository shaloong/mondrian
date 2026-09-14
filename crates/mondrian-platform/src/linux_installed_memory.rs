//! Installed-memory evidence from the system's unprivileged udev DMI inventory.

use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr::NonNull;

use libloading::Library;

#[derive(Debug, thiserror::Error)]
pub(super) enum InstalledMemoryError {
    #[error("Linux installed-memory provider is unavailable: {0}")]
    Provider(#[from] libloading::Error),
    #[error("Linux installed-memory evidence is unavailable: {0}")]
    Unavailable(&'static str),
    #[error("Linux installed-memory evidence is invalid: {0}")]
    Invalid(&'static str),
}

type Release = unsafe extern "C" fn(*mut c_void) -> *mut c_void;

struct UdevObject<'a> {
    pointer: NonNull<c_void>,
    release: Release,
    _library: &'a Library,
}

impl Drop for UdevObject<'_> {
    fn drop(&mut self) {
        // SAFETY: this object owns one live udev reference; the borrowed
        // library outlives the release function and this destructor.
        unsafe { (self.release)(self.pointer.as_ptr()) };
    }
}

/// Query installed bytes without executing a helper or reading privileged DMI.
pub(super) fn installed_bytes() -> Result<u64, InstalledMemoryError> {
    // SAFETY: the system libudev SONAME and functions below have stable C ABI.
    // Every function/reference is consumed before the library is dropped.
    let library = unsafe { Library::new("libudev.so.1") }?;
    // SAFETY: each symbol is loaded using its documented libudev C signature.
    let (new, unref, from_path, device_unref, property) = unsafe {
        (
            *library.get::<unsafe extern "C" fn() -> *mut c_void>(b"udev_new\0")?,
            *library.get::<Release>(b"udev_unref\0")?,
            *library.get::<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>(
                b"udev_device_new_from_syspath\0",
            )?,
            *library.get::<Release>(b"udev_device_unref\0")?,
            *library.get::<unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char>(
                b"udev_device_get_property_value\0",
            )?,
        )
    };
    // SAFETY: udev_new takes no arguments and transfers one reference.
    let context = UdevObject {
        pointer: NonNull::new(unsafe { new() }).ok_or(InstalledMemoryError::Unavailable(
            "udev context creation failed",
        ))?,
        release: unref,
        _library: &library,
    };
    // SAFETY: context is live and the static syspath is NUL terminated. The
    // returned reference is independently owned and released before context.
    let device = UdevObject {
        pointer: NonNull::new(unsafe {
            from_path(
                context.pointer.as_ptr(),
                c"/sys/devices/virtual/dmi/id".as_ptr(),
            )
        })
        .ok_or(InstalledMemoryError::Unavailable("DMI device is absent"))?,
        release: device_unref,
        _library: &library,
    };
    capacity_from_properties(|key| {
        let key = CString::new(key)
            .map_err(|_| InstalledMemoryError::Invalid("property name contains NUL"))?;
        // SAFETY: device and key are live. libudev owns a NUL-terminated value
        // until device release; copy only requested capacity fields here.
        let value = unsafe { property(device.pointer.as_ptr(), key.as_ptr()) };
        if value.is_null() {
            return Ok(None);
        }
        // SAFETY: the non-null value has the lifetime and termination above.
        unsafe { CStr::from_ptr(value) }
            .to_str()
            .map(|value| Some(value.to_owned()))
            .map_err(|_| InstalledMemoryError::Invalid("property is not UTF-8"))
    })
}

fn capacity_from_properties(
    mut property: impl FnMut(&str) -> Result<Option<String>, InstalledMemoryError>,
) -> Result<u64, InstalledMemoryError> {
    let count = property("MEMORY_ARRAY_NUM_DEVICES")?.ok_or(InstalledMemoryError::Unavailable(
        "memory device inventory is absent",
    ))?;
    let count = count
        .parse::<usize>()
        .map_err(|_| InstalledMemoryError::Invalid("invalid memory device count"))?;
    if !(1..=4096).contains(&count) {
        return Err(InstalledMemoryError::Invalid(
            "memory device count exceeds bounds",
        ));
    }
    let mut total = 0_u64;
    for index in 0..count {
        let size = property(&format!("MEMORY_DEVICE_{index}_SIZE"))?;
        let present = property(&format!("MEMORY_DEVICE_{index}_PRESENT"))?;
        if present.as_deref() == Some("0") {
            if size.is_some() {
                return Err(InstalledMemoryError::Invalid("empty slot has a capacity"));
            }
            continue;
        }
        if present.as_deref().is_some_and(|value| value != "1") {
            return Err(InstalledMemoryError::Invalid("invalid presence value"));
        }
        let size = size.ok_or(InstalledMemoryError::Unavailable(
            "a slot has unknown capacity",
        ))?;
        let size = size
            .parse::<u64>()
            .map_err(|_| InstalledMemoryError::Invalid("invalid slot capacity"))?;
        if size == 0 {
            return Err(InstalledMemoryError::Invalid(
                "populated slot has zero capacity",
            ));
        }
        // DMI persistent memory must not inflate the ordinary RAM grant.
        if let Some(nonvolatile) = property(&format!("MEMORY_DEVICE_{index}_NON_VOLATILE_SIZE"))?
            && nonvolatile != "0"
        {
            return Err(InstalledMemoryError::Unavailable(
                "non-volatile memory inventory",
            ));
        }
        total = total.checked_add(size).ok_or(InstalledMemoryError::Invalid(
            "installed capacity overflowed",
        ))?;
    }
    if total == 0 {
        return Err(InstalledMemoryError::Unavailable(
            "no populated memory slots",
        ));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity(properties: &[(&str, &str)]) -> Result<u64, InstalledMemoryError> {
        capacity_from_properties(|key| {
            Ok(properties
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned()))
        })
    }

    #[test]
    fn sums_installed_dimms_and_explicitly_empty_slots() {
        assert_eq!(
            capacity(&[
                ("MEMORY_ARRAY_NUM_DEVICES", "4"),
                ("MEMORY_DEVICE_0_SIZE", "8589934592"),
                ("MEMORY_DEVICE_1_PRESENT", "0"),
                ("MEMORY_DEVICE_2_SIZE", "8589934592"),
                ("MEMORY_DEVICE_3_PRESENT", "0"),
            ])
            .expect("complete DMI inventory"),
            16 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn rejects_partial_unknown_conflicting_and_overflowing_inventory() {
        for properties in [
            vec![],
            vec![("MEMORY_ARRAY_NUM_DEVICES", "4097")],
            vec![("MEMORY_ARRAY_NUM_DEVICES", "1")],
            vec![
                ("MEMORY_ARRAY_NUM_DEVICES", "1"),
                ("MEMORY_DEVICE_0_SIZE", "unknown"),
            ],
            vec![
                ("MEMORY_ARRAY_NUM_DEVICES", "1"),
                ("MEMORY_DEVICE_0_SIZE", "0"),
            ],
            vec![
                ("MEMORY_ARRAY_NUM_DEVICES", "1"),
                ("MEMORY_DEVICE_0_SIZE", "8"),
                ("MEMORY_DEVICE_0_PRESENT", "0"),
            ],
            vec![
                ("MEMORY_ARRAY_NUM_DEVICES", "1"),
                ("MEMORY_DEVICE_0_SIZE", "8"),
                ("MEMORY_DEVICE_0_NON_VOLATILE_SIZE", "4"),
            ],
            vec![
                ("MEMORY_ARRAY_NUM_DEVICES", "2"),
                ("MEMORY_DEVICE_0_SIZE", "18446744073709551615"),
                ("MEMORY_DEVICE_1_SIZE", "1"),
            ],
        ] {
            assert!(capacity(&properties).is_err(), "{properties:?}");
        }
    }
}
