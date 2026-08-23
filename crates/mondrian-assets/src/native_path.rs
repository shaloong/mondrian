//! Stable native paths for Asset Library identity and SQLite I/O.
//!
//! Persisted media identity uses one ordinary absolute/canonical native path.
//! SQLite's Win32 VFS separately receives an extended-length path so its
//! rollback journal, WAL, and shared-memory sidecars can cross the legacy
//! `MAX_PATH` boundary. Device namespaces are never accepted as file identity.

use std::path::{Path, PathBuf};

/// Resolve one path once into the ordinary absolute namespace used by the
/// Asset Library Interface and its in-memory identity.
#[cfg(windows)]
pub(crate) fn ordinary_absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Prefix};

    const SEPARATOR: u16 = b'\\' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const VERBATIM_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, QUESTION, SEPARATOR];
    const VERBATIM_UNC_PREFIX: &[u16] = &[
        SEPARATOR,
        SEPARATOR,
        QUESTION,
        SEPARATOR,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEPARATOR,
    ];
    const DEVICE_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, DOT, SEPARATOR];
    const NT_DEVICE_PREFIX: &[u16] = &[SEPARATOR, QUESTION, QUESTION, SEPARATOR];

    let native = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let ordinary = if starts_with_ascii_case(&native, VERBATIM_UNC_PREFIX) {
        let mut units = Vec::with_capacity(native.len().saturating_sub(6));
        units.extend_from_slice(&[SEPARATOR, SEPARATOR]);
        units.extend_from_slice(&native[VERBATIM_UNC_PREFIX.len()..]);
        PathBuf::from(OsString::from_wide(&units))
    } else if native.starts_with(VERBATIM_PREFIX) {
        let tail = &native[VERBATIM_PREFIX.len()..];
        let candidate = PathBuf::from(OsString::from_wide(tail));
        if !candidate.is_absolute()
            || !matches!(
                candidate.components().next(),
                Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))
            )
        {
            return Err(invalid_namespace(
                "Windows verbatim path is not an ordinary drive or UNC path",
            ));
        }
        candidate
    } else {
        if native.starts_with(DEVICE_PREFIX) || native.starts_with(NT_DEVICE_PREFIX) {
            return Err(invalid_namespace(
                "Windows device namespace is not an Asset Library file path",
            ));
        }
        path.to_path_buf()
    };

    let absolute = std::path::absolute(ordinary)?;
    match absolute.components().next() {
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(_, _)) =>
        {
            Ok(absolute)
        }
        _ => Err(invalid_namespace(
            "Windows Asset Library path is not an absolute drive or UNC path",
        )),
    }
}

#[cfg(not(windows))]
pub(crate) fn ordinary_absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    std::path::absolute(path)
}

/// Canonicalize one existing file while keeping ordinary persisted namespace
/// spelling rather than Windows' `\\?\` physical-I/O prefix.
pub(crate) fn ordinary_canonical_path(path: &Path) -> std::io::Result<PathBuf> {
    let admitted = ordinary_absolute_path(path)?;
    let canonical = std::fs::canonicalize(admitted)?;
    ordinary_absolute_path(&canonical)
}

/// Freeze the existing parent of one sibling anchor while leaving its leaf
/// uninterpreted. This prevents a later `..` or ancestor reparse from sending
/// an opaque staging object to a different directory.
pub(crate) fn ordinary_sibling_anchor(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = ordinary_absolute_path(path)?;
    let leaf = absolute.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sibling anchor has no direct file name",
        )
    })?;
    let parent = absolute.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sibling anchor has no parent directory",
        )
    })?;
    Ok(ordinary_canonical_path(parent)?.join(leaf))
}

/// Admit one persisted UTF-8 path without silently repairing a non-canonical
/// or non-ordinary identity from SQLite.
pub(crate) fn persisted_file_path(path_text: &str) -> std::io::Result<PathBuf> {
    use std::path::Component;

    let path = PathBuf::from(path_text);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "persisted Asset Library path is not a normalized absolute path",
        ));
    }
    let ordinary = ordinary_absolute_path(&path)?;
    if ordinary != path || persisted_path_text(&ordinary)? != path_text {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "persisted Asset Library path is not in the ordinary native namespace",
        ));
    }
    Ok(ordinary)
}

/// Encode a canonical path losslessly for the current SQLite TEXT schema.
pub(crate) fn persisted_path_text(path: &Path) -> std::io::Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Asset Library path cannot be represented losslessly by the current UTF-8 schema",
        )
    })
}

/// Adapt an ordinary absolute path only at the SQLite physical-open boundary.
#[cfg(windows)]
pub(crate) fn sqlite_open_path(path: &Path) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Prefix};

    const SEPARATOR: u16 = b'\\' as u16;
    const VERBATIM_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, b'?' as u16, SEPARATOR];
    const VERBATIM_UNC_PREFIX: &[u16] = &[
        SEPARATOR,
        SEPARATOR,
        b'?' as u16,
        SEPARATOR,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEPARATOR,
    ];

    let ordinary = ordinary_absolute_path(path)?;
    let native = ordinary.as_os_str().encode_wide().collect::<Vec<_>>();
    let mut extended = Vec::with_capacity(native.len().saturating_add(8));
    match ordinary.components().next() {
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(_, _)) => {
            extended.extend_from_slice(VERBATIM_UNC_PREFIX);
            extended.extend_from_slice(&native[2..]);
        }
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)) => {
            extended.extend_from_slice(VERBATIM_PREFIX);
            extended.extend_from_slice(&native);
        }
        _ => {
            return Err(invalid_namespace(
                "SQLite path is not an ordinary absolute drive or UNC path",
            ));
        }
    }
    Ok(PathBuf::from(OsString::from_wide(&extended)))
}

#[cfg(not(windows))]
pub(crate) fn sqlite_open_path(path: &Path) -> std::io::Result<PathBuf> {
    ordinary_absolute_path(path)
}

#[cfg(windows)]
fn starts_with_ascii_case(value: &[u16], prefix: &[u16]) -> bool {
    value.len() >= prefix.len()
        && value
            .iter()
            .zip(prefix)
            .all(|(value, expected)| ascii_lowercase(*value) == ascii_lowercase(*expected))
}

#[cfg(windows)]
const fn ascii_lowercase(value: u16) -> u16 {
    if value >= b'A' as u16 && value <= b'Z' as u16 {
        value + (b'a' - b'A') as u16
    } else {
        value
    }
}

#[cfg(windows)]
fn invalid_namespace(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_persisted_identity_is_ordinary_but_sqlite_uses_extended_length_paths() {
        let ordinary = ordinary_absolute_path(Path::new(r"\\?\C:\projects\library\index.db"))
            .expect("ordinary database identity");
        assert_eq!(ordinary, Path::new(r"C:\projects\library\index.db"));

        let sqlite = sqlite_open_path(&ordinary).expect("SQLite database path");
        assert!(sqlite.as_os_str().to_string_lossy().starts_with(r"\\?\C:\"));

        let unc = ordinary_absolute_path(Path::new(r"\\?\UNC\server\share\library\index.db"))
            .expect("ordinary UNC identity");
        assert_eq!(unc, Path::new(r"\\server\share\library\index.db"));
        let sqlite_unc = sqlite_open_path(&unc).expect("SQLite UNC path");
        assert!(sqlite_unc.as_os_str().to_string_lossy().starts_with(r"\\?\UNC\server\share\"));
    }

    #[test]
    fn windows_device_and_non_file_verbatim_namespaces_fail_before_io() {
        for path in [
            Path::new(r"\\.\PhysicalDrive0"),
            Path::new(r"\\?\GLOBALROOT\Device\HarddiskVolume1"),
            Path::new(r"\\?\PIPE\mondrian"),
            Path::new(r"\??\C:\private"),
        ] {
            let error =
                ordinary_absolute_path(path).expect_err("device namespace must be rejected");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }
}
