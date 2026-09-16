//! Cancellable canonical ANC carriage and independent final MXF reimport.

use super::JobExecutionResult;
use mondrian_broadcast::{write_st436_klv_frame, FrozenAncillaryProgram};
use mondrian_core::ExecutionCancellationToken;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(super) fn prepare(
    program: &FrozenAncillaryProgram,
    work: &Path,
    cancel: &ExecutionCancellationToken,
) -> Result<PathBuf, JobExecutionResult> {
    let path = work.join("canonical-ancillary.klv");
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        program.validate()?;
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        let mut writer = std::io::BufWriter::new(file);
        for index in 0..program.frame_count() {
            check_cancel(cancel)?;
            write_st436_klv_frame(&mut writer, &program.frame(index)?)?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        check_cancel(cancel)?;
        Ok(())
    })();
    result.map_err(|error| failure(cancel, "canonical ST436 input", error))?;
    Ok(path)
}

pub(super) fn verify(
    program: &FrozenAncillaryProgram,
    path: &Path,
    cancel: &ExecutionCancellationToken,
) -> Result<(), JobExecutionResult> {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        check_cancel(cancel)?;
        let file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::other("AS-11 output is not a regular file").into());
        }
        let mut reader = CancellableReader { inner: std::io::BufReader::new(file), cancel };
        program.verify_mxf(&mut reader, metadata.len())?;
        check_cancel(cancel)?;
        Ok(())
    })();
    result.map_err(|error| failure(cancel, "AS-11 ST436 final MXF reimport", error))
}

fn failure(
    cancel: &ExecutionCancellationToken,
    context: &str,
    error: impl std::fmt::Display,
) -> JobExecutionResult {
    if cancel.is_canceled() {
        JobExecutionResult::Cancelled
    } else {
        JobExecutionResult::Failed(format!("{context}: {error}"))
    }
}

fn check_cancel(cancel: &ExecutionCancellationToken) -> std::io::Result<()> {
    // Interrupted is retried by Read::read_exact and would spin after cancel.
    if cancel.is_canceled() {
        Err(std::io::Error::other("ST436 operation cancelled"))
    } else {
        Ok(())
    }
}

struct CancellableReader<'a, R> {
    inner: R,
    cancel: &'a ExecutionCancellationToken,
}
impl<R: Read> Read for CancellableReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        check_cancel(self.cancel)?;
        self.inner.read(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_is_terminal_instead_of_read_exact_retryable() {
        let cancel = ExecutionCancellationToken::new();
        cancel.cancel();
        let mut reader = CancellableReader { inner: &[1, 2, 3][..], cancel: &cancel };
        let mut bytes = [0; 3];
        assert_eq!(
            reader.read_exact(&mut bytes).expect_err("cancel").kind(),
            std::io::ErrorKind::Other
        );
        let work = tempfile::tempdir().expect("work");
        let program = FrozenAncillaryProgram::new(
            mondrian_core::TimelineTime::ZERO,
            mondrian_core::Rational::new(25, 1),
            1,
            vec![],
        )
        .expect("program");
        assert!(matches!(
            prepare(&program, work.path(), &cancel),
            Err(JobExecutionResult::Cancelled)
        ));
    }
}
