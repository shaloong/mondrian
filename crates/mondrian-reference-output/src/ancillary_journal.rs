//! Consuming native-owner publication of existing raw wire journals.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Exact immutable file bytes written by one closed native wire owner.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeAncillaryJournalReceipt {
    /// Absolute actual owner-created journal path.
    pub path: PathBuf,
    /// Hash of successful owner writes under a deny-write/delete lease, synchronized on close.
    pub sha256: String,
}

/// Bounded shared owner channel; it does not scan a directory or infer filenames.
#[derive(Debug, Clone, Default)]
pub struct NativeAncillaryJournalInventory {
    inner: Arc<Mutex<Vec<(String, NativeAncillaryJournalReceipt)>>>,
}
impl NativeAncillaryJournalInventory {
    /// Read only records published by the matching phase's consumed native owners.
    pub fn closed_for_phase(
        &self,
        phase_id: &str,
    ) -> Result<Vec<NativeAncillaryJournalReceipt>, String> {
        let values = self.inner.lock().map_err(|_| "wire journal owner lock poisoned")?;
        Ok(values
            .iter()
            .filter(|(phase, _)| phase == phase_id)
            .map(|(_, receipt)| receipt.clone())
            .collect())
    }
    #[cfg(all(windows, any(feature = "native-aja", feature = "native-decklink")))]
    pub(crate) fn publish(
        &self,
        phase_id: String,
        receipt: NativeAncillaryJournalReceipt,
    ) -> Result<(), String> {
        let mut values = self.inner.lock().map_err(|_| "wire journal owner lock poisoned")?;
        if values.len() >= 64 || values.iter().any(|(_, item)| item.path == receipt.path) {
            return Err("wire journal inventory repeated or exceeds 64 sessions".to_owned());
        }
        values.push((phase_id, receipt));
        Ok(())
    }
}

/// Optional program-bound publication channel carried into the native owner.
#[derive(Debug, Clone)]
pub struct NativeAncillaryJournalBinding {
    /// Actual immutable program source hash, retained by the application phase owner.
    pub ancillary_program_sha256: String,
    /// Exact phase owning this native session.
    pub phase_id: String,
    /// Channel receiving only successfully consumed native journals.
    pub inventory: NativeAncillaryJournalInventory,
}

impl NativeAncillaryJournalBinding {
    /// Validate the optional immutable source/phase binding before opening devices.
    pub fn validate(&self) -> Result<(), String> {
        if self.phase_id.is_empty()
            || self.phase_id.len() > 128
            || !self
                .phase_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            || self.ancillary_program_sha256.len() != 64
            || !self
                .ancillary_program_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid native journal program/phase identity".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn program_identity_is_exact_and_phase_bounded() {
        let mut binding = NativeAncillaryJournalBinding {
            ancillary_program_sha256: "a".repeat(64),
            phase_id: "03-concurrent-recovery-24h".to_owned(),
            inventory: NativeAncillaryJournalInventory::default(),
        };
        assert!(binding.validate().is_ok());
        binding.ancillary_program_sha256 = "A".repeat(64);
        assert!(binding.validate().is_err());
        binding.ancillary_program_sha256 = "a".repeat(64);
        binding.phase_id = "../another-phase".to_owned();
        assert!(binding.validate().is_err());
    }
    #[cfg(all(windows, any(feature = "native-aja", feature = "native-decklink")))]
    #[test]
    fn consuming_inventory_is_phase_scoped_and_rejects_duplicate_paths() {
        let inventory = NativeAncillaryJournalInventory::default();
        let path = std::env::temp_dir().join("owner-journal.jsonl");
        let receipt = NativeAncillaryJournalReceipt { path, sha256: "a".repeat(64) };
        inventory
            .publish("phase-a".to_owned(), receipt.clone())
            .expect("owner publication");
        assert_eq!(
            inventory.closed_for_phase("phase-a").expect("phase"),
            vec![receipt.clone()]
        );
        assert!(inventory.closed_for_phase("phase-b").expect("different phase").is_empty());
        assert!(inventory.publish("phase-b".to_owned(), receipt).is_err());
    }
}

/// Incremental digest over successful complete writes under the native exclusive writer.
#[cfg(any(
    test,
    all(windows, any(feature = "native-aja", feature = "native-decklink"))
))]
#[derive(Default)]
pub(crate) struct NativeJournalDigest {
    digest: sha2::Sha256,
    written: u64,
    failed: bool,
}
#[cfg(any(
    test,
    all(windows, any(feature = "native-aja", feature = "native-decklink"))
))]
impl NativeJournalDigest {
    pub(crate) fn invalidate(&mut self) {
        self.failed = true;
    }
    pub(crate) fn write(
        &mut self,
        writer: &mut impl std::io::Write,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        use sha2::Digest;
        if self.failed {
            return Err(std::io::Error::other(
                "wire journal writer previously failed",
            ));
        }
        let Some(next) = self.written.checked_add(bytes.len() as u64) else {
            self.failed = true;
            return Err(std::io::Error::other("wire journal digest length overflow"));
        };
        if let Err(error) = writer.write_all(bytes) {
            self.failed = true;
            return Err(error);
        }
        self.digest.update(bytes);
        self.written = next;
        Ok(())
    }
    pub(crate) fn finish(
        &self,
        observed_length: u64,
        clean_native_close: bool,
    ) -> std::io::Result<String> {
        use sha2::Digest;
        if self.failed
            || self.written == 0
            || self.written != observed_length
            || !clean_native_close
        {
            return Err(std::io::Error::other(
                "wire journal is partial, changed or its native owner is not closed",
            ));
        }
        Ok(format!("{:x}", self.digest.clone().finalize()))
    }
}
#[cfg(test)]
mod digest_tests {
    use super::NativeJournalDigest;
    use sha2::{Digest, Sha256};
    #[test]
    fn incremental_owner_hash_rejects_length_changes_and_dirty_close() {
        let mut digest = NativeJournalDigest::default();
        let mut bytes = Vec::new();
        digest.write(&mut bytes, b"identity\n").expect("first write");
        digest.write(&mut bytes, b"captured-native-words\n").expect("second write");
        assert_eq!(
            digest.finish(bytes.len() as u64, true).expect("complete"),
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert!(digest.finish(bytes.len() as u64 - 1, true).is_err());
        assert!(digest.finish(bytes.len() as u64 + 1, true).is_err());
        assert!(digest.finish(bytes.len() as u64, false).is_err());
        digest.invalidate();
        assert!(digest.finish(bytes.len() as u64, true).is_err());
    }
    #[test]
    fn partial_write_permanently_prevents_prefix_receipt() {
        struct Partial {
            written: bool,
        }
        impl std::io::Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.written {
                    return Err(std::io::Error::other("disk exhausted"));
                }
                self.written = true;
                Ok(bytes.len().min(1))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut digest = NativeJournalDigest::default();
        assert!(digest.write(&mut Partial { written: false }, b"not-complete").is_err());
        assert!(digest.finish(1, true).is_err());
        assert!(digest.write(&mut Vec::new(), b"retry-cannot-hide-failure").is_err());
    }
}
