//! Stable per-user state-directory discovery.
//!
//! This Module only resolves an absolute native path. It never creates the
//! directory and owns no product namespace, permission, locking, symlink, or
//! durability policy.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::ffi::OsStr;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;

use mondrian_platform_core::UserStateDirectoryError;

pub(crate) fn system_user_state_directory() -> Result<PathBuf, UserStateDirectoryError> {
    let path = platform_user_state_directory()?;
    if !path.is_absolute() {
        return Err(discovery_failed(format!(
            "native state directory is not absolute: {}",
            path.display()
        )));
    }
    Ok(path)
}

fn discovery_failed(reason: impl Into<String>) -> UserStateDirectoryError {
    UserStateDirectoryError::DiscoveryFailed { reason: reason.into() }
}

#[cfg(target_os = "windows")]
fn platform_user_state_directory() -> Result<PathBuf, UserStateDirectoryError> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};

    let mut raw_path: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: the API initializes `raw_path` with a COM-task allocation on
    // success. A null token selects the current user and the allocation is
    // released exactly once below.
    let result = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_LocalAppData,
            0,
            std::ptr::null_mut(),
            &mut raw_path,
        )
    };
    if result < 0 {
        return Err(discovery_failed(format!(
            "SHGetKnownFolderPath(FOLDERID_LocalAppData) failed with HRESULT 0x{:08x}",
            result as u32
        )));
    }
    if raw_path.is_null() {
        return Err(discovery_failed(
            "SHGetKnownFolderPath returned a null LocalAppData path",
        ));
    }
    let mut length = 0_usize;
    // SAFETY: the successful call returned one NUL-terminated UTF-16 string.
    while unsafe { *raw_path.add(length) } != 0 {
        if length >= 32_768 {
            // SAFETY: `raw_path` is the exact COM-task allocation above.
            unsafe { CoTaskMemFree(raw_path.cast()) };
            return Err(discovery_failed(
                "LocalAppData path exceeds the 32,768 UTF-16 unit safety bound",
            ));
        }
        length += 1;
    }
    // SAFETY: the scan proved the first `length` units precede the NUL.
    let units = unsafe { std::slice::from_raw_parts(raw_path, length) };
    let path = PathBuf::from(std::ffi::OsString::from_wide(units));
    // SAFETY: balances the successful native allocation exactly once.
    unsafe { CoTaskMemFree(raw_path.cast()) };
    Ok(path)
}

#[cfg(target_os = "macos")]
fn platform_user_state_directory() -> Result<PathBuf, UserStateDirectoryError> {
    macos_application_support_directory(std::env::var_os("HOME").as_deref())
}

#[cfg(target_os = "linux")]
fn platform_user_state_directory() -> Result<PathBuf, UserStateDirectoryError> {
    linux_state_home(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn platform_user_state_directory() -> Result<PathBuf, UserStateDirectoryError> {
    Err(UserStateDirectoryError::Unavailable)
}

#[cfg(target_os = "macos")]
fn macos_application_support_directory(
    home: Option<&OsStr>,
) -> Result<PathBuf, UserStateDirectoryError> {
    let home = absolute_path(home).ok_or_else(|| {
        discovery_failed("macOS state discovery requires an absolute HOME directory")
    })?;
    Ok(home.join("Library").join("Application Support"))
}

#[cfg(target_os = "linux")]
fn linux_state_home(
    xdg_state_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, UserStateDirectoryError> {
    if let Some(path) = absolute_path(xdg_state_home) {
        return Ok(path.to_path_buf());
    }
    let home = absolute_path(home).ok_or_else(|| {
        discovery_failed("Linux state discovery requires absolute XDG_STATE_HOME or HOME")
    })?;
    Ok(home.join(".local").join("state"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn absolute_path(value: Option<&OsStr>) -> Option<&Path> {
    value.map(Path::new).filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_state_home_prefers_only_an_absolute_xdg_path() {
        let home = PathBuf::from("/home/editor");
        assert_eq!(
            linux_state_home(Some(OsStr::new("/state/editor")), Some(home.as_os_str()))
                .expect("absolute XDG state root"),
            PathBuf::from("/state/editor")
        );
        assert_eq!(
            linux_state_home(Some(OsStr::new("relative")), Some(home.as_os_str()))
                .expect("absolute HOME fallback"),
            home.join(".local").join("state")
        );
        assert!(linux_state_home(None, Some(OsStr::new("relative"))).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_application_support_requires_an_absolute_home() {
        assert_eq!(
            macos_application_support_directory(Some(OsStr::new("/Users/editor")))
                .expect("absolute HOME"),
            PathBuf::from("/Users/editor/Library/Application Support")
        );
        assert!(macos_application_support_directory(Some(OsStr::new("relative"))).is_err());
        assert!(macos_application_support_directory(None).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_system_state_directory_is_absolute() {
        let path = system_user_state_directory().expect("Windows LocalAppData");
        assert!(path.is_absolute());
    }
}
