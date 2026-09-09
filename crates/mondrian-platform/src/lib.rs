//! Desktop platform adapters.
//!
//! Pure platform interfaces live in `mondrian-platform-core`. This crate keeps
//! OS-backed desktop implementations behind those interfaces.

use std::path::{Path, PathBuf};
use std::process::Command;

mod display;
mod eyedropper;
mod global_pointer;
mod memory;
mod playback_scheduling;
mod process_memory;
mod user_state_directory;
pub use mondrian_platform_core::{
    ClipboardError, DisplayHdrProbe, DisplayHdrProbeDetails, DisplayHdrProbeResult,
    DisplayIccProfileProbeResult, DisplayProbeBackend, DisplayProfileProbe,
    DisplayProfileProbeTarget, EnduranceAncillaryExportArtifact, EnduranceAncillaryPhaseEvidence,
    EnduranceAncillaryWireJournal, EnduranceCounterRequirement, EnduranceCounters, EnduranceGauges,
    EnduranceMemoryRequirement, EndurancePhaseChunkReceipt, EndurancePhaseKind,
    EndurancePhaseManifest, EndurancePhaseMeasurementTiming, EndurancePhaseOwnerReceipt,
    EndurancePhaseProducerEvidence, EndurancePhaseReport, EndurancePhaseRequirement,
    EndurancePhaseTerminalEvidence, EndurancePhaseTerminalStatus, EnduranceProcessMemorySample,
    EnduranceQualificationError, EnduranceQualificationProfile, EnduranceQualificationReport,
    EnduranceQualificationStatus, EnduranceRunManifest, EnduranceRunOwnerClosureEvidence,
    EnduranceSample, EnduranceSampleChunk, ExecutionMemoryProbe, FileDialogError,
    FileDialogOutcome, FileFilter, FileRevealError, NoopPlatformService,
    PhysicalMemoryCapacityProbe, PhysicalMemoryCapacityProbeBackend,
    PhysicalMemoryCapacityProbeResult, PlatformService, PreparedEnduranceQualification,
    ProcessEventLoopOwnerClosureEvidence, ProcessMemoryProbe, ProcessMemoryProbeBackend,
    ProcessMemoryProbeResult, ProcessMemoryScope, ProcessPrivateMemoryMetric, SystemMemoryProbe,
    SystemMemoryProbeBackend, SystemMemoryProbeResult, UserStateDirectory, UserStateDirectoryError,
};

/// Default desktop platform implementation.
///
/// Clipboard, native dialogs, and file reveal use cross-platform desktop
/// adapters. Speculative operations are not part of the platform-neutral
/// Interface until a product caller and a typed failure contract exist.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPlatformService;

pub use eyedropper::{DesktopEyedropper, DesktopPoint, DesktopRgba8};
pub use playback_scheduling::{
    PlaybackThreadScheduling, PlaybackThreadSchedulingError, PlaybackThreadSchedulingStatus,
};

impl PlatformService for SystemPlatformService {
    fn clipboard_copy(&self, text: &str) -> Result<(), ClipboardError> {
        let mut clipboard = arboard::Clipboard::new().map_err(|_| ClipboardError::Unavailable)?;
        clipboard.set_text(text).map_err(|_| ClipboardError::WriteFailed)
    }

    fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
        let mut clipboard = arboard::Clipboard::new().map_err(|_| ClipboardError::Unavailable)?;
        match clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(_) => Err(ClipboardError::ReadFailed),
        }
    }

    fn open_file_dialog(
        &self,
        title: &str,
        filters: &[FileFilter],
    ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
        Ok(match configured_file_dialog(title, filters).pick_files() {
            Some(paths) => FileDialogOutcome::Selected(paths),
            None => FileDialogOutcome::Cancelled,
        })
    }

    fn save_file_dialog(
        &self,
        title: &str,
        default_name: &str,
        filters: &[FileFilter],
    ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
        Ok(
            match configured_file_dialog(title, filters).set_file_name(default_name).save_file() {
                Some(path) => FileDialogOutcome::Selected(path),
                None => FileDialogOutcome::Cancelled,
            },
        )
    }

    fn reveal_in_file_manager(&self, path: &Path) -> Result<(), FileRevealError> {
        reveal_path_in_file_manager(path)
    }
}

impl DisplayProfileProbe for SystemPlatformService {
    fn display_icc_profile(
        &self,
        target: DisplayProfileProbeTarget,
    ) -> DisplayIccProfileProbeResult {
        system_display_icc_profile(target)
    }
}

impl DisplayHdrProbe for SystemPlatformService {
    fn display_hdr_state(&self, target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
        system_display_hdr_state(target)
    }
}

impl ProcessMemoryProbe for SystemPlatformService {
    fn process_memory(&self, scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
        process_memory::system_process_memory(scope)
    }
}

impl PhysicalMemoryCapacityProbe for SystemPlatformService {
    fn physical_memory_capacity(&self) -> PhysicalMemoryCapacityProbeResult {
        system_physical_memory_capacity()
    }
}

impl SystemMemoryProbe for SystemPlatformService {
    fn current_system_memory(&self) -> SystemMemoryProbeResult {
        system_memory()
    }
}

impl UserStateDirectory for SystemPlatformService {
    fn user_state_directory(&self) -> Result<PathBuf, UserStateDirectoryError> {
        user_state_directory::system_user_state_directory()
    }
}

fn system_physical_memory_capacity() -> PhysicalMemoryCapacityProbeResult {
    memory::physical_memory_capacity()
}

fn system_memory() -> SystemMemoryProbeResult {
    memory::system_memory()
}

#[cfg(target_os = "windows")]
fn system_display_icc_profile(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    display::windows::display_icc_profile(target)
}

#[cfg(target_os = "macos")]
fn system_display_icc_profile(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    display::macos::display_icc_profile(target)
}

#[cfg(target_os = "linux")]
fn system_display_icc_profile(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    display::linux::display_icc_profile(target)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn system_display_icc_profile(_target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    DisplayIccProfileProbeResult::unsupported(
        "OS ICC profile discovery is not implemented for this platform",
    )
}

#[cfg(target_os = "windows")]
fn system_display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    display::windows::display_hdr_state(target)
}

#[cfg(target_os = "macos")]
fn system_display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    display::macos::display_hdr_state(target)
}

#[cfg(target_os = "linux")]
fn system_display_hdr_state(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    display::linux::display_hdr_state(target)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn system_display_hdr_state(_target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    DisplayHdrProbeResult::unsupported(
        "OS HDR / Advanced Color discovery is not implemented for this platform",
    )
}

fn reveal_path_in_file_manager(path: &Path) -> Result<(), FileRevealError> {
    #[cfg(target_os = "windows")]
    {
        let target = if path.exists() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        Command::new("explorer")
            .arg(format!("/select,{}", target.display()))
            .spawn()
            .map_err(|error| FileRevealError::LaunchFailed { reason: error.to_string() })?;
    }

    #[cfg(target_os = "macos")]
    {
        let target = if path.exists() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        Command::new("open")
            .arg("-R")
            .arg(target)
            .spawn()
            .map_err(|error| FileRevealError::LaunchFailed { reason: error.to_string() })?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        Command::new("xdg-open")
            .arg(target)
            .spawn()
            .map_err(|error| FileRevealError::LaunchFailed { reason: error.to_string() })?;
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    return Err(FileRevealError::Unavailable);

    #[cfg(any(target_os = "windows", target_os = "macos", unix))]
    Ok(())
}

fn configured_file_dialog(title: &str, filters: &[FileFilter]) -> rfd::FileDialog {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    for filter in filters {
        if filter.extensions.is_empty() {
            continue;
        }
        let extensions = filter.extensions.iter().map(String::as_str).collect::<Vec<_>>();
        dialog = dialog.add_filter(&filter.name, &extensions);
    }
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════════
    // FileFilter
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn file_filter_new() {
        let filter = FileFilter::new("Video Files", vec!["mp4", "mov", "avi"]);
        assert_eq!(filter.name, "Video Files");
        assert_eq!(filter.extensions, vec!["mp4", "mov", "avi"]);
    }

    #[test]
    fn file_filter_from_string_types() {
        let filter = FileFilter::new(
            String::from("Images"),
            vec![String::from("png"), String::from("jpg")],
        );
        assert_eq!(filter.extensions, vec!["png", "jpg"]);
    }

    #[test]
    fn file_filter_empty_extensions() {
        let filter = FileFilter::new("All Files", Vec::<&str>::new());
        assert!(filter.extensions.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // NoopPlatformService
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn noop_clipboard_paste_reports_unavailable() {
        let svc = NoopPlatformService;
        assert_eq!(svc.clipboard_paste(), Err(ClipboardError::Unavailable));
    }

    #[test]
    fn noop_clipboard_copy_reports_unavailable() {
        let svc = NoopPlatformService;
        assert_eq!(
            svc.clipboard_copy("any text"),
            Err(ClipboardError::Unavailable)
        );
    }

    #[test]
    fn noop_open_file_dialog_reports_unavailable() {
        let svc = NoopPlatformService;
        assert_eq!(
            svc.open_file_dialog("Open", &[]),
            Err(FileDialogError::Unavailable)
        );
    }

    #[test]
    fn noop_save_file_dialog_reports_unavailable() {
        let svc = NoopPlatformService;
        assert_eq!(
            svc.save_file_dialog("Save", "test.txt", &[]),
            Err(FileDialogError::Unavailable)
        );
    }

    #[test]
    fn noop_file_reveal_reports_unavailable() {
        let svc = NoopPlatformService;
        assert_eq!(
            svc.reveal_in_file_manager(Path::new("/tmp/test.txt")),
            Err(FileRevealError::Unavailable)
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // trait object safety
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn platform_service_is_object_safe() {
        let svc: &dyn PlatformService = &NoopPlatformService;
        assert_eq!(svc.clipboard_paste(), Err(ClipboardError::Unavailable));
    }

    #[test]
    fn platform_service_can_be_boxed() {
        let _boxed: Box<dyn PlatformService> = Box::new(NoopPlatformService);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_installed_memory_probe_reports_physical_capacity() {
        let result = SystemPlatformService.physical_memory_capacity();

        assert!(result.discovery_available, "{:?}", result.error);
        assert_eq!(
            result.backend,
            Some(PhysicalMemoryCapacityProbeBackend::WindowsInstalledSystemMemory)
        );
        assert!(result.installed_physical_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.error.is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_process_memory_probe_reports_private_commit() {
        let result = SystemPlatformService.current_process_memory();

        assert!(result.discovery_available, "{:?}", result.error);
        assert_eq!(result.scope, ProcessMemoryScope::CurrentProcess);
        assert_eq!(
            result.backend,
            Some(ProcessMemoryProbeBackend::WindowsCurrentProcessStatus)
        );
        assert_eq!(result.observed_process_count, 1);
        assert!(result.inventory_complete);
        assert!(result.private_memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.peak_resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.error.is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_product_process_tree_probe_reports_complete_inventory() {
        let result = SystemPlatformService.product_process_tree_memory();

        assert!(result.discovery_available, "{:?}", result.error);
        assert_eq!(result.scope, ProcessMemoryScope::ProductProcessTree);
        assert_eq!(
            result.backend,
            Some(ProcessMemoryProbeBackend::WindowsToolhelpProcessTree)
        );
        assert!(result.observed_process_count >= 1);
        assert!(result.inventory_complete);
        assert!(result.inventory_attempts >= 1);
        assert!(result.private_memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.peak_resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(result.error.is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_system_memory_probe_reports_available_capacity() {
        let result = SystemPlatformService.current_system_memory();

        assert!(result.discovery_available, "{:?}", result.error);
        assert_eq!(
            result.backend,
            Some(SystemMemoryProbeBackend::WindowsGlobalMemoryStatus)
        );
        let total = result.total_physical_bytes.expect("Windows reports total physical memory");
        let available = result
            .available_physical_bytes
            .expect("Windows reports available physical memory");
        assert!(total > 0);
        assert!(available <= total);
        assert!(result.memory_load_percent.is_some_and(|load| load <= 100));
        assert!(result.error.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // mock for testing downstream consumers
    // ═══════════════════════════════════════════════════════════════════════════

    /// A test mock that returns controlled values.
    struct MockPlatformService {
        clipboard_content: Result<Option<String>, ClipboardError>,
        file_dialog_result: Option<Vec<PathBuf>>,
        save_dialog_result: Option<PathBuf>,
    }

    impl PlatformService for MockPlatformService {
        fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
            Ok(())
        }

        fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
            self.clipboard_content.clone()
        }

        fn open_file_dialog(
            &self,
            _title: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
            Ok(match self.file_dialog_result.clone() {
                Some(paths) => FileDialogOutcome::Selected(paths),
                None => FileDialogOutcome::Cancelled,
            })
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
            Ok(match self.save_dialog_result.clone() {
                Some(path) => FileDialogOutcome::Selected(path),
                None => FileDialogOutcome::Cancelled,
            })
        }

        fn reveal_in_file_manager(&self, _path: &Path) -> Result<(), FileRevealError> {
            Ok(())
        }
    }

    #[test]
    fn mock_platform_service_returns_configured_values() {
        let mock = MockPlatformService {
            clipboard_content: Ok(Some("copied text".into())),
            file_dialog_result: Some(vec![PathBuf::from("/test/file.mp4")]),
            save_dialog_result: Some(PathBuf::from("/test/output.mp4")),
        };

        assert_eq!(mock.clipboard_paste(), Ok(Some("copied text".into())));
        assert_eq!(
            mock.open_file_dialog("", &[]),
            Ok(FileDialogOutcome::Selected(vec![PathBuf::from(
                "/test/file.mp4"
            )]))
        );
        assert_eq!(
            mock.save_file_dialog("", "", &[]),
            Ok(FileDialogOutcome::Selected(PathBuf::from(
                "/test/output.mp4"
            )))
        );
    }
}
