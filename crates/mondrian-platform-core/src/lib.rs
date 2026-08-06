//! Platform boundary interfaces shared by UI and desktop adapters.
//!
//! This crate intentionally contains no operating-system implementation. UI
//! crates depend on these contracts, while desktop shells provide concrete
//! adapters from `mondrian-platform`. Domain modules keep unrelated platform
//! facts from growing into one universal service interface.

mod desktop;
mod display;
mod memory;
mod user_state_directory;

pub use desktop::{
    ClipboardError, FileDialogError, FileDialogOutcome, FileFilter, FileRevealError,
    PlatformService,
};
pub use display::{
    DisplayHdrProbe, DisplayHdrProbeDetails, DisplayHdrProbeResult, DisplayIccProfileProbeResult,
    DisplayProbeBackend, DisplayProfileProbe, DisplayProfileProbeTarget,
};
pub use memory::{
    ExecutionMemoryProbe, PhysicalMemoryCapacityProbe, PhysicalMemoryCapacityProbeBackend,
    PhysicalMemoryCapacityProbeResult, ProcessMemoryProbe, ProcessMemoryProbeBackend,
    ProcessMemoryProbeResult, ProcessMemoryScope, ProcessPrivateMemoryMetric, SystemMemoryProbe,
    SystemMemoryProbeBackend, SystemMemoryProbeResult,
};
pub use user_state_directory::{UserStateDirectory, UserStateDirectoryError};

/// Empty platform implementation for tests and headless UI execution.
///
/// Every capability either returns its typed unavailable result or a benign
/// cancelled/no-op shell outcome. It never manufactures native evidence.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopPlatformService;
