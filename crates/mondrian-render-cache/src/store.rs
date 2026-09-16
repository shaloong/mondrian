use crate::artifact::{decode_artifact, encode_artifact};
use crate::{TimelineRenderCacheFrame, TimelineRenderCacheIdentity};
use mondrian_core::WorkingColorSpace;
use mondrian_storage::{write_durable_file_atomically, FilePublicationFailure};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const CACHE_NAMESPACE: &str = "timeline-render-v2";
const ARTIFACT_EXTENSION: &str = "mrc";

#[derive(Debug, thiserror::Error)]
pub(crate) enum TimelineRenderCacheStoreError {
    #[error("Timeline render-cache root must be absolute: {0}")]
    RelativeRoot(PathBuf),
    #[error("Timeline render-cache filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Artifact(#[from] crate::TimelineRenderCacheArtifactError),
    #[error("Timeline render-cache publication failed: {0}")]
    Publication(#[from] FilePublicationFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreLookupDisposition {
    Hit,
    Miss,
    CorruptRemoved,
}

pub(crate) struct StoreLookup {
    pub(crate) disposition: StoreLookupDisposition,
    pub(crate) frame: Option<TimelineRenderCacheFrame>,
    pub(crate) detail: Option<String>,
}

pub(crate) struct StorePublication {
    pub(crate) artifact_bytes: u64,
    pub(crate) evicted_entries: u64,
    pub(crate) evicted_bytes: u64,
}

struct StoreEntry {
    path: PathBuf,
    bytes: u64,
    last_access: u64,
}

pub(crate) struct TimelineRenderCacheStore {
    namespace_root: PathBuf,
    max_disk_bytes: u64,
    max_artifact_bytes: u64,
    entries: HashMap<TimelineRenderCacheIdentity, StoreEntry>,
    resident_bytes: u64,
    access_clock: u64,
}

impl TimelineRenderCacheStore {
    pub(crate) fn open(
        root: &Path,
        max_disk_bytes: u64,
        max_artifact_bytes: u64,
    ) -> Result<Self, TimelineRenderCacheStoreError> {
        if !root.is_absolute() {
            return Err(TimelineRenderCacheStoreError::RelativeRoot(
                root.to_path_buf(),
            ));
        }
        let namespace_root = root.join(CACHE_NAMESPACE);
        fs::create_dir_all(&namespace_root)?;
        let mut store = Self {
            namespace_root,
            max_disk_bytes,
            max_artifact_bytes,
            entries: HashMap::new(),
            resident_bytes: 0,
            access_clock: 0,
        };
        store.scan_existing()?;
        let _ = store.evict_to_budget(None);
        Ok(store)
    }

    pub(crate) fn lookup(
        &mut self,
        identity: TimelineRenderCacheIdentity,
        color_space: WorkingColorSpace,
    ) -> Result<StoreLookup, TimelineRenderCacheStoreError> {
        let path = self.artifact_path(identity);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                return Ok(StoreLookup {
                    disposition: StoreLookupDisposition::Miss,
                    frame: None,
                    detail: Some("cache identity path is not a regular file".to_owned()),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.remove_index_entry(identity);
                return Ok(StoreLookup {
                    disposition: StoreLookupDisposition::Miss,
                    frame: None,
                    detail: None,
                });
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.len() > self.max_artifact_bytes {
            return Ok(self.remove_corrupt(
                identity,
                &path,
                format!(
                    "artifact has {} bytes; limit is {}",
                    metadata.len(),
                    self.max_artifact_bytes
                ),
            ));
        }
        let bytes = fs::read(&path)?;
        match decode_artifact(identity, color_space, &bytes, self.max_artifact_bytes) {
            Ok(frame) => {
                self.touch(identity, path, metadata.len());
                Ok(StoreLookup {
                    disposition: StoreLookupDisposition::Hit,
                    frame: Some(frame),
                    detail: None,
                })
            }
            Err(error) => Ok(self.remove_corrupt(identity, &path, error.to_string())),
        }
    }

    pub(crate) fn publish(
        &mut self,
        frame: &TimelineRenderCacheFrame,
    ) -> Result<StorePublication, TimelineRenderCacheStoreError> {
        let bytes = encode_artifact(frame, self.max_artifact_bytes)?;
        let artifact_bytes = u64::try_from(bytes.len())
            .map_err(|_| crate::TimelineRenderCacheArtifactError::ExtentOverflow)?;
        let path = self.artifact_path(frame.identity());
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cache artifact path has no parent",
            )
        })?;
        fs::create_dir_all(parent)?;
        write_durable_file_atomically(&path, &bytes)?;
        self.touch(frame.identity(), path, artifact_bytes);
        let (evicted_entries, evicted_bytes) = self.evict_to_budget(Some(frame.identity()));
        Ok(StorePublication { artifact_bytes, evicted_entries, evicted_bytes })
    }

    pub(crate) fn resident_entries(&self) -> u64 {
        self.entries.len() as u64
    }

    pub(crate) const fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    fn artifact_path(&self, identity: TimelineRenderCacheIdentity) -> PathBuf {
        let hex = identity.hex();
        self.namespace_root
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(format!("{hex}.{ARTIFACT_EXTENSION}"))
    }

    fn touch(&mut self, identity: TimelineRenderCacheIdentity, path: PathBuf, bytes: u64) {
        self.access_clock = self.access_clock.saturating_add(1);
        let previous = self.entries.insert(
            identity,
            StoreEntry { path, bytes, last_access: self.access_clock },
        );
        if let Some(previous) = previous {
            self.resident_bytes = self.resident_bytes.saturating_sub(previous.bytes);
        }
        self.resident_bytes = self.resident_bytes.saturating_add(bytes);
    }

    fn remove_corrupt(
        &mut self,
        identity: TimelineRenderCacheIdentity,
        path: &Path,
        detail: String,
    ) -> StoreLookup {
        self.remove_index_entry(identity);
        match fs::remove_file(path) {
            Ok(()) => StoreLookup {
                disposition: StoreLookupDisposition::CorruptRemoved,
                frame: None,
                detail: Some(detail),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoreLookup {
                disposition: StoreLookupDisposition::CorruptRemoved,
                frame: None,
                detail: Some(detail),
            },
            Err(error) => StoreLookup {
                disposition: StoreLookupDisposition::Miss,
                frame: None,
                detail: Some(format!(
                    "{detail}; corrupt artifact removal failed: {error}"
                )),
            },
        }
    }

    fn remove_index_entry(&mut self, identity: TimelineRenderCacheIdentity) {
        if let Some(entry) = self.entries.remove(&identity) {
            self.resident_bytes = self.resident_bytes.saturating_sub(entry.bytes);
        }
    }

    fn evict_to_budget(&mut self, protected: Option<TimelineRenderCacheIdentity>) -> (u64, u64) {
        let mut evicted_entries = 0_u64;
        let mut evicted_bytes = 0_u64;
        while self.resident_bytes > self.max_disk_bytes {
            let Some(identity) = self
                .entries
                .iter()
                .filter(|(identity, _)| Some(**identity) != protected)
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(identity, _)| *identity)
            else {
                break;
            };
            let Some(entry) = self.entries.remove(&identity) else {
                break;
            };
            match fs::remove_file(&entry.path) {
                Ok(()) => {
                    self.resident_bytes = self.resident_bytes.saturating_sub(entry.bytes);
                    evicted_entries = evicted_entries.saturating_add(1);
                    evicted_bytes = evicted_bytes.saturating_add(entry.bytes);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.resident_bytes = self.resident_bytes.saturating_sub(entry.bytes);
                }
                Err(error) => {
                    tracing::warn!(
                        path = %entry.path.display(),
                        %error,
                        "failed to evict Timeline render-cache artifact"
                    );
                    self.entries.insert(identity, entry);
                    break;
                }
            }
        }
        (evicted_entries, evicted_bytes)
    }

    fn scan_existing(&mut self) -> Result<(), TimelineRenderCacheStoreError> {
        let mut files = Vec::new();
        collect_files(&self.namespace_root, 3, &mut files)?;
        files.sort_by_key(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });
        for path in files {
            let Some(identity) = identity_from_artifact_path(&path) else {
                continue;
            };
            let metadata = match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if metadata.len() > self.max_artifact_bytes {
                let _ = fs::remove_file(&path);
                continue;
            }
            self.touch(identity, path, metadata.len());
        }
        Ok(())
    }
}

fn collect_files(
    directory: &Path,
    remaining_depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if remaining_depth == 0 {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&entry.path(), remaining_depth - 1, output)?;
        } else if file_type.is_file() {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn identity_from_artifact_path(path: &Path) -> Option<TimelineRenderCacheIdentity> {
    if path.extension()?.to_str()? != ARTIFACT_EXTENSION {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    if stem.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&stem[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(TimelineRenderCacheIdentity::from_digest(digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::WorkingRgbaF32Frame;

    fn frame(identity_byte: u8, value: f32) -> TimelineRenderCacheFrame {
        TimelineRenderCacheFrame::new(
            TimelineRenderCacheIdentity::from_digest([identity_byte; 32]),
            WorkingRgbaF32Frame {
                width: 8,
                height: 8,
                data: vec![[value; 4]; 64],
                color_space: WorkingColorSpace::LinearRec709,
            },
        )
        .expect("valid frame")
    }

    #[test]
    fn publication_roundtrips_and_corruption_is_local() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut store =
            TimelineRenderCacheStore::open(temp.path(), 1_048_576, 1_048_576).expect("store");
        let frame = frame(1, 0.25);
        store.publish(&frame).expect("publish");
        let hit = store.lookup(frame.identity(), WorkingColorSpace::LinearRec709).expect("lookup");
        assert_eq!(hit.disposition, StoreLookupDisposition::Hit);
        let path = store.artifact_path(frame.identity());
        let mut bytes = fs::read(&path).expect("artifact");
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        fs::write(&path, bytes).expect("corrupt test artifact");
        let corrupt = store
            .lookup(frame.identity(), WorkingColorSpace::LinearRec709)
            .expect("lookup corruption");
        assert_eq!(corrupt.disposition, StoreLookupDisposition::CorruptRemoved);
        assert!(!path.exists());
    }

    #[test]
    fn disk_budget_evicts_oldest_identity_without_global_clear() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut store = TimelineRenderCacheStore::open(temp.path(), 300, 16_384).expect("store");
        let first = frame(1, 0.1);
        let second = frame(2, 0.2);
        store.publish(&first).expect("first");
        store.publish(&second).expect("second");
        assert!(store.resident_bytes() <= 300 || store.resident_entries() == 1);
        let second_lookup = store
            .lookup(second.identity(), WorkingColorSpace::LinearRec709)
            .expect("second lookup");
        assert_eq!(second_lookup.disposition, StoreLookupDisposition::Hit);
    }
}
