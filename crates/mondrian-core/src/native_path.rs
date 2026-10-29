//! Lossless platform-owned paths, including recoverable foreign-platform intent.

use crate::{AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    borrow::Cow,
    fmt,
    path::{Path, PathBuf},
};

/// The platform whose filename and root rules own a persisted path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePathPlatform {
    /// Windows native UTF-16 filename units.
    Windows,
    /// Unix native filename bytes, including macOS.
    Unix,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
enum EncodedPath {
    Windows { units: Vec<u16> },
    Unix { bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum PathStorage {
    Local(std::ffi::OsString),
    Foreign(EncodedPath),
}

/// An exact native path with explicit platform provenance.
///
/// Local paths retain their exact native filename allocation. Equality,
/// ordering, and hashing preserve spelling rather than normalizing components.
/// Foreign paths retain their original units and cannot be used for local
/// filesystem access.
/// Serialization never converts a filename through a lossy Unicode string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NativePath(PathStorage);

/// A path belongs to another platform and requires author relinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("resource path belongs to {platform:?}; relink it for this platform")]
pub struct ForeignNativePathError {
    /// Platform which owns the unavailable native path.
    pub platform: NativePathPlatform,
}

impl From<PathBuf> for NativePath {
    fn from(path: PathBuf) -> Self {
        Self(PathStorage::Local(path.into_os_string()))
    }
}

impl From<&Path> for NativePath {
    fn from(path: &Path) -> Self {
        path.to_path_buf().into()
    }
}

impl NativePath {
    /// Retain exact Windows filename units, without a Unicode conversion.
    pub fn from_windows_units(units: Vec<u16>) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_wide(&units)).into()
        }
        #[cfg(not(windows))]
        {
            Self(PathStorage::Foreign(EncodedPath::Windows { units }))
        }
    }

    /// Retain exact Unix filename bytes, without a Unicode conversion.
    pub fn from_unix_bytes(bytes: Vec<u8>) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(bytes)).into()
        }
        #[cfg(not(unix))]
        {
            Self(PathStorage::Foreign(EncodedPath::Unix { bytes }))
        }
    }

    /// Platform which owns the path's syntax and native filename units.
    pub fn platform(&self) -> NativePathPlatform {
        match &self.0 {
            PathStorage::Foreign(EncodedPath::Windows { .. }) => NativePathPlatform::Windows,
            PathStorage::Foreign(EncodedPath::Unix { .. }) => NativePathPlatform::Unix,
            PathStorage::Local(_) => {
                #[cfg(windows)]
                {
                    NativePathPlatform::Windows
                }
                #[cfg(not(windows))]
                {
                    NativePathPlatform::Unix
                }
            }
        }
    }

    /// Borrow a local path without allocation; foreign paths fail explicitly.
    pub fn native_path(&self) -> Result<&Path, ForeignNativePathError> {
        match &self.0 {
            PathStorage::Local(path) => Ok(Path::new(path)),
            PathStorage::Foreign(_) => Err(ForeignNativePathError { platform: self.platform() }),
        }
    }

    /// Consume an owned local path without copying its filename allocation.
    pub fn into_native_path(self) -> Result<PathBuf, ForeignNativePathError> {
        let platform = self.platform();
        match self.0 {
            PathStorage::Local(path) => Ok(PathBuf::from(path)),
            PathStorage::Foreign(_) => Err(ForeignNativePathError { platform }),
        }
    }

    /// Whether the originating platform supplies an absolute path owner.
    pub fn is_absolute(&self) -> bool {
        match &self.0 {
            PathStorage::Local(path) => Path::new(path).is_absolute(),
            PathStorage::Foreign(EncodedPath::Windows { units }) => {
                let syntax = windows_path_syntax(units);
                let components = typed_path::WindowsPath::new(&syntax).components();
                // Windows std paths accept implicit roots on UNC and device
                // prefixes, including a share with no trailing separator.
                components.has_prefix()
                    && (components.has_physical_root() || components.has_implicit_root())
            }
            PathStorage::Foreign(EncodedPath::Unix { bytes }) => {
                typed_path::UnixPath::new(bytes).is_absolute()
            }
        }
    }

    /// Whether the exact filename representation is empty.
    pub fn is_empty(&self) -> bool {
        match &self.0 {
            PathStorage::Local(path) => path.as_os_str().is_empty(),
            PathStorage::Foreign(EncodedPath::Windows { units }) => units.is_empty(),
            PathStorage::Foreign(EncodedPath::Unix { bytes }) => bytes.is_empty(),
        }
    }

    /// Whether any native filename unit is NUL.
    pub fn contains_nul(&self) -> bool {
        match &self.0 {
            PathStorage::Local(path) => path.as_os_str().as_encoded_bytes().contains(&0),
            PathStorage::Foreign(EncodedPath::Windows { units }) => units.contains(&0),
            PathStorage::Foreign(EncodedPath::Unix { bytes }) => bytes.contains(&0),
        }
    }

    /// Exact UTF-8 text when the original units admit it; never replaces invalid units.
    pub fn utf8_text(&self) -> Option<Cow<'_, str>> {
        match &self.0 {
            PathStorage::Local(path) => path.to_str().map(Cow::Borrowed),
            PathStorage::Foreign(EncodedPath::Windows { units }) => {
                String::from_utf16(units).ok().map(Cow::Owned)
            }
            PathStorage::Foreign(EncodedPath::Unix { bytes }) => {
                std::str::from_utf8(bytes).ok().map(Cow::Borrowed)
            }
        }
    }

    /// UTF-8 extension under the originating platform's filename separator rules.
    /// This validates author metadata without opening or rebinding a foreign path.
    pub fn extension_utf8(&self) -> Option<Cow<'_, str>> {
        if let PathStorage::Local(path) = &self.0 {
            return Path::new(path).extension().and_then(|value| value.to_str()).map(Cow::Borrowed);
        }
        match &self.0 {
            PathStorage::Foreign(EncodedPath::Windows { units }) => {
                // Windows path syntax uses ASCII delimiters. One byte per
                // UTF-16 unit retains exact offsets without decoding filenames;
                // this projection is only for the foreign metadata parser.
                let syntax = windows_path_syntax(units);
                let path = typed_path::WindowsPath::new(&syntax);
                let extension = path.extension()?;
                let offset = extension.as_ptr().addr().checked_sub(syntax.as_ptr().addr())?;
                // std recognizes a verbatim drive only when its colon ends
                // the prefix or is followed by a separator. Otherwise the
                // whole first verbatim component is a namespace prefix, not
                // a filename (e.g. \\?\C:look.cube).
                if syntax.starts_with(br"\\?\")
                    && path.components().has_verbatim_disk_prefix()
                    && syntax.get(6).is_some_and(|byte| !matches!(byte, b'\\' | b'/'))
                {
                    let prefix_end = syntax[4..]
                        .iter()
                        .position(|byte| *byte == b'\\')
                        .map_or(syntax.len(), |index| index + 4);
                    if offset < prefix_end {
                        return None;
                    }
                }
                String::from_utf16(units.get(offset..offset + extension.len())?)
                    .ok()
                    .map(Cow::Owned)
            }
            PathStorage::Foreign(EncodedPath::Unix { bytes }) => {
                let extension = typed_path::UnixPath::new(bytes).extension()?;
                std::str::from_utf8(extension).ok().map(Cow::Borrowed)
            }
            PathStorage::Local(_) => None,
        }
    }

    /// Display-only Unicode projection. Never use it for persistence or filesystem access.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        match &self.0 {
            PathStorage::Local(path) => path.to_string_lossy(),
            PathStorage::Foreign(EncodedPath::Windows { units }) => {
                Cow::Owned(String::from_utf16_lossy(units))
            }
            PathStorage::Foreign(EncodedPath::Unix { bytes }) => String::from_utf8_lossy(bytes),
        }
    }

    /// Display-only formatter retaining ordinary local path presentation.
    pub fn display(&self) -> impl fmt::Display + '_ {
        self.to_string_lossy()
    }
}

fn windows_path_syntax(units: &[u16]) -> Vec<u8> {
    units
        .iter()
        .map(|unit| u8::try_from(*unit).ok().filter(|byte| byte.is_ascii()).unwrap_or(0xff))
        .collect()
}

impl Serialize for NativePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            PathStorage::Foreign(encoded) => encoded.serialize(serializer),
            PathStorage::Local(path) => {
                #[cfg(windows)]
                {
                    use std::os::windows::ffi::OsStrExt;
                    EncodedPath::Windows { units: path.as_os_str().encode_wide().collect() }
                        .serialize(serializer)
                }
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    EncodedPath::Unix { bytes: path.as_os_str().as_bytes().to_vec() }
                        .serialize(serializer)
                }
                #[cfg(not(any(windows, unix)))]
                {
                    let _ = path;
                    Err(serde::ser::Error::custom(
                        "unsupported native path platform",
                    ))
                }
            }
        }
    }
}

impl<'de> Deserialize<'de> for NativePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match EncodedPath::deserialize(deserializer)? {
            EncodedPath::Windows { units } => Self::from_windows_units(units),
            EncodedPath::Unix { bytes } => Self::from_unix_bytes(bytes),
        })
    }
}

impl AuthoringFootprint for NativePath {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match &self.0 {
            PathStorage::Local(path) => collector.collect(path),
            PathStorage::Foreign(EncodedPath::Windows { units }) => collector.collect(units),
            PathStorage::Foreign(EncodedPath::Unix { bytes }) => collector.collect(bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_reference_identity_retains_exact_spelling_for_foreign_rebinding() {
        let plain: NativePath = PathBuf::from("directory/look.cube").into();
        let dotted: NativePath = PathBuf::from("directory/./look.cube").into();
        assert_ne!(
            serde_json::to_value(&plain).expect("plain units"),
            serde_json::to_value(&dotted).expect("dotted units")
        );
        assert_ne!(
            plain, dotted,
            "distinct persisted coordinates must not collapse before cross-platform rebinding"
        );
        let paths = std::collections::BTreeSet::from([plain, dotted]);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn both_platform_representations_round_trip_exactly() {
        for path in [
            NativePath::from_windows_units(vec![67, 58, 92, 0xd800, 46, 99]),
            NativePath::from_unix_bytes(vec![47, 0xff, 46, 99]),
        ] {
            let json = serde_json::to_vec(&path).expect("encode exact native units");
            let restored: NativePath = serde_json::from_slice(&json).expect("restore units");
            assert_eq!(restored, path);
            assert!(restored.utf8_text().is_none());
            assert!(restored.is_absolute());
        }
    }

    #[test]
    fn foreign_paths_do_not_become_local_relative_paths() {
        #[cfg(windows)]
        let path = NativePath::from_unix_bytes(b"/studio/look.cube".to_vec());
        #[cfg(not(windows))]
        let path = NativePath::from_windows_units("C:\\studio\\look.cube".encode_utf16().collect());
        assert!(path.native_path().is_err());
        assert!(path.is_absolute());
        assert_eq!(path.extension_utf8().as_deref(), Some("cube"));
        assert!(serde_json::from_str::<NativePath>("\"legacy-path\"").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn foreign_windows_metadata_matches_native_root_and_extension_rules() {
        for text in [
            r"C:.cube",
            r"\\?\C:",
            r"\\?\C:look.cube",
            r"C:\look.cube\",
            r"C:\look.cube\.",
            r"C:\look.cube\..",
            r"\\server\share.cube",
            r"\\server\share\look.cube",
            r"\\?\C:\look.cube\.",
            r"\\?\UNC\server\share.cube",
            r"\\?\UNC\server\share\look.cube",
            r"\\.\COM1",
            r"\\.\look.cube",
            r"C:\.cube",
            r"C:\.hidden.cube",
            r"relative\look.CUBE",
            r"C:\调色🙂\look.cube",
        ] {
            let foreign = NativePath(PathStorage::Foreign(EncodedPath::Windows {
                units: text.encode_utf16().collect(),
            }));
            let local = Path::new(text);
            assert_eq!(foreign.is_absolute(), local.is_absolute(), "owner: {text}");
            assert_eq!(
                foreign.extension_utf8().as_deref(),
                local.extension().and_then(|extension| extension.to_str()),
                "extension: {text}"
            );
        }
    }

    #[test]
    fn invalid_filename_units_do_not_destroy_valid_extension_metadata() {
        for encoded in [
            EncodedPath::Windows {
                units: vec![67, 58, 92, 0xd800, 46, 99, 117, 98, 101],
            },
            EncodedPath::Unix { bytes: vec![47, 0xff, 46, 99, 117, 98, 101] },
        ] {
            let path = NativePath(PathStorage::Foreign(encoded));
            assert!(path.utf8_text().is_none());
            assert_eq!(path.extension_utf8().as_deref(), Some("cube"));
        }
    }
}
