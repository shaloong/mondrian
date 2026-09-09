//! Resolution policy for FFmpeg command-line tools.
//!
//! Packaged applications place `ffmpeg` and `ffprobe` beside the Mondrian
//! executable. Development builds may fall back to the process search path.
//! Keeping this policy here prevents media, proxy, and export call sites from
//! acquiring different runtime implementations accidentally.

use crate::FfmpegCommand;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

/// Command admission failed before a child process could be created.
///
/// An installed qualification toolchain may never fall back to another tool.
/// Ordinary builds currently have no fallible command-admission policy.
#[derive(Debug, thiserror::Error)]
pub enum FfmpegCommandError {
    /// The installed exact-runtime toolchain no longer authorizes execution.
    #[cfg(feature = "validation")]
    #[error("qualified FFmpeg command admission rejected: {0}")]
    QualifiedToolchain(#[from] crate::qualified_ffmpeg::QualifiedFfmpegToolchainError),
}

impl FfmpegCommandError {
    /// Find typed command admission through native I/O and domain error wrappers.
    pub fn is_error_cause(mut error: &(dyn std::error::Error + 'static)) -> bool {
        loop {
            if error.is::<Self>() {
                return true;
            }
            if let Some(inner) =
                error.downcast_ref::<std::io::Error>().and_then(std::io::Error::get_ref)
                && Self::is_error_cause(inner)
            {
                return true;
            }
            match error.source() {
                Some(source) => error = source,
                None => return false,
            }
        }
    }
    /// Test whether a domain failure retains rejected execution authority.
    pub fn is_cause_of(error: &mondrian_core::MondrianError) -> bool {
        match error {
            mondrian_core::MondrianError::Other(source) => source.chain().any(Self::is_error_cause),
            _ => Self::is_error_cause(error),
        }
    }
}

impl From<FfmpegCommandError> for mondrian_core::MondrianError {
    fn from(error: FfmpegCommandError) -> Self {
        Self::Other(anyhow::Error::new(error))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FfmpegTool {
    Ffmpeg,
    Ffprobe,
}

impl FfmpegTool {
    pub(crate) const fn command_name(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
        }
    }

    fn packaged_file_name(self) -> String {
        format!("{}{}", self.command_name(), std::env::consts::EXE_SUFFIX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FfmpegToolSource {
    Packaged,
    SearchPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedFfmpegTool {
    pub(crate) path: PathBuf,
    pub(crate) source: FfmpegToolSource,
}

/// Construct an admitted FFmpeg command, using packaged/PATH resolution only
/// when no qualification toolchain is installed. Rejection never yields a command.
pub fn ffmpeg_command() -> Result<FfmpegCommand, FfmpegCommandError> {
    command(FfmpegTool::Ffmpeg)
}

/// Construct an admitted ffprobe command under the same policy as [`ffmpeg_command`].
pub fn ffprobe_command() -> Result<FfmpegCommand, FfmpegCommandError> {
    command(FfmpegTool::Ffprobe)
}

fn command(tool: FfmpegTool) -> Result<FfmpegCommand, FfmpegCommandError> {
    #[cfg(feature = "validation")]
    let qualified = match tool {
        FfmpegTool::Ffmpeg => crate::qualified_ffmpeg::process_ffmpeg_command(),
        FfmpegTool::Ffprobe => crate::qualified_ffmpeg::process_ffprobe_command(),
    }
    .map_err(FfmpegCommandError::from);
    #[cfg(not(feature = "validation"))]
    let qualified = Ok(None);
    resolve_command(qualified, || resolve_ffmpeg_tool(tool))
}

fn resolve_command(
    qualified: Result<Option<FfmpegCommand>, FfmpegCommandError>,
    fallback: impl FnOnce() -> ResolvedFfmpegTool,
) -> Result<FfmpegCommand, FfmpegCommandError> {
    match qualified? {
        Some(command) => Ok(command),
        None => Ok(FfmpegCommand::new(fallback().path)),
    }
}

pub(crate) fn resolve_ffmpeg_tool(tool: FfmpegTool) -> ResolvedFfmpegTool {
    resolve_ffmpeg_tool_from(std::env::current_exe().ok().as_deref(), tool)
}

fn resolve_ffmpeg_tool_from(
    current_executable: Option<&Path>,
    tool: FfmpegTool,
) -> ResolvedFfmpegTool {
    if let Some(directory) = current_executable.and_then(Path::parent) {
        let candidate = directory.join(tool.packaged_file_name());
        if candidate.is_file() {
            return ResolvedFfmpegTool {
                path: candidate,
                source: FfmpegToolSource::Packaged,
            };
        }
    }
    ResolvedFfmpegTool {
        path: PathBuf::from(tool.command_name()),
        source: FfmpegToolSource::SearchPath,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_toolchain_alone_resolves_the_development_command() {
        let resolutions = std::cell::Cell::new(0);
        let command = resolve_command(Ok(None), || {
            resolutions.set(resolutions.get() + 1);
            ResolvedFfmpegTool {
                path: PathBuf::from("ffprobe"),
                source: FfmpegToolSource::SearchPath,
            }
        })
        .expect("absent toolchain may resolve development tool");
        assert_eq!(resolutions.get(), 1);
        assert_eq!(command.get_program(), "ffprobe");
    }

    #[test]
    fn admitted_command_preserves_exact_program_arguments_and_environment() {
        let root = tempfile::tempdir().expect("private command directory");
        let path = root.path().join("ffmpeg.exe");
        let mut prepared = Command::new(&path);
        prepared.current_dir(root.path()).env("PATH", root.path()).arg("-nostdin");
        let command = resolve_command(Ok(Some(FfmpegCommand::ordinary(prepared))), || {
            panic!("must not resolve another tool")
        })
        .expect("already admitted command");
        assert_eq!(command.get_program(), path.as_os_str());
        assert_eq!(command.get_current_dir(), Some(root.path()));
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["-nostdin"]);
        assert_eq!(
            command.get_envs().collect::<Vec<_>>(),
            [(std::ffi::OsStr::new("PATH"), Some(root.path().as_os_str()))]
        );
    }

    #[cfg(feature = "validation")]
    #[test]
    fn rejected_toolchain_neither_resolves_nor_emits_a_command_and_retains_source() {
        let error = resolve_command(
            Err(
                crate::qualified_ffmpeg::QualifiedFfmpegToolchainError::CapsuleNamespaceChanged
                    .into(),
            ),
            || panic!("denied authority must never fall back to packaged or PATH tools"),
        )
        .expect_err("no command may cross the admission seam");
        assert!(matches!(
            std::error::Error::source(&error).and_then(|source| source.downcast_ref()),
            Some(crate::qualified_ffmpeg::QualifiedFfmpegToolchainError::CapsuleNamespaceChanged)
        ));
        let domain_error: mondrian_core::MondrianError = error.into();
        assert!(FfmpegCommandError::is_cause_of(&domain_error));
        let mondrian_core::MondrianError::Other(source) = domain_error else {
            panic!("typed source carrier");
        };
        assert!(matches!(
            source.downcast_ref::<FfmpegCommandError>(),
            Some(FfmpegCommandError::QualifiedToolchain(
                crate::qualified_ffmpeg::QualifiedFfmpegToolchainError::CapsuleNamespaceChanged
            ))
        ));
    }

    #[test]
    fn packaged_tool_beside_executable_wins_over_search_path() {
        let root = tempfile::tempdir().expect("temporary runtime");
        let executable = root.path().join(format!("mondrian{}", std::env::consts::EXE_SUFFIX));
        let tool = root.path().join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&executable, []).expect("fake executable");
        std::fs::write(&tool, []).expect("fake packaged tool");

        let resolved = resolve_ffmpeg_tool_from(Some(&executable), FfmpegTool::Ffmpeg);

        assert_eq!(resolved.path, tool);
        assert_eq!(resolved.source, FfmpegToolSource::Packaged);
    }

    #[test]
    fn development_resolution_falls_back_to_search_path() {
        let root = tempfile::tempdir().expect("temporary runtime");
        let executable = root.path().join(format!("mondrian{}", std::env::consts::EXE_SUFFIX));

        let resolved = resolve_ffmpeg_tool_from(Some(&executable), FfmpegTool::Ffprobe);

        assert_eq!(resolved.path, PathBuf::from("ffprobe"));
        assert_eq!(resolved.source, FfmpegToolSource::SearchPath);
    }
}
