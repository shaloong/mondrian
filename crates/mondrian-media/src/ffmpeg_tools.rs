//! Resolution policy for FFmpeg command-line tools.
//!
//! Packaged applications place `ffmpeg` and `ffprobe` beside the Mondrian
//! executable. Development builds may fall back to the process search path.
//! Keeping this policy here prevents media, proxy, and export call sites from
//! acquiring different runtime implementations accidentally.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Construct an FFmpeg command using the packaged tool when present.
pub fn ffmpeg_command() -> Command {
    Command::new(resolve_ffmpeg_tool(FfmpegTool::Ffmpeg).path)
}

/// Construct an ffprobe command using the packaged tool when present.
pub fn ffprobe_command() -> Command {
    Command::new(resolve_ffmpeg_tool(FfmpegTool::Ffprobe).path)
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
