//! Content identity shared by directory-based export artifacts.

use mondrian_core::ExecutionCancellationToken;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

pub(crate) fn sha256_file(
    path: &Path,
    cancel: &ExecutionCancellationToken,
    artifact_label: &str,
) -> Result<String, String> {
    let file = File::open(path)
        .map_err(|error| format!("cannot hash {artifact_label} {}: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancel.is_canceled() {
            return Err(format!("{artifact_label} hashing cancelled"));
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash {artifact_label} {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
