//! This extension provides a way for plugins to log messages through the host,
//! which can be useful for debugging and error reporting. The host can choose how to handle these log messages,
//! such as writing them to a dedicated log file.

use clack_common::extensions::{Extension, HostExtensionSide, RawExtension};
use clap_sys::ext::log::{CLAP_EXT_LOG, clap_host_log, clap_log_severity};
use std::ffi::CStr;
use std::fmt::{Display, Formatter};

mod error;
pub use error::LogError;

#[cfg(feature = "clack-host")]
mod host;

#[cfg(feature = "clack-host")]
pub use host::*;

/// Maximum copied prefix of one native log message, excluding a C terminator.
///
/// The callback reads at most this many bytes plus one terminator lookahead.
/// Truncation is explicit; no promise about omitted UTF-8 bytes is made.
pub const HOST_LOG_MESSAGE_CAPACITY: usize = 512;

/// Bounded borrowed immutable facts from one CLAP Host Log callback.
///
/// Raw bytes preserve malformed UTF-8 and an unknown signed native severity;
/// neither is silently replaced or reclassified as an ordinary warning.
#[derive(Debug, Clone, Copy)]
pub struct HostLogMessage<'a> {
    pub(crate) raw_severity: clap_sys::ext::log::clap_log_severity,
    pub(crate) severity: Option<LogSeverity>,
    pub(crate) prefix: &'a [u8],
    pub(crate) terminated: bool,
    pub(crate) null_message: bool,
}

impl<'a> HostLogMessage<'a> {
    /// The exact signed native value, including an unknown severity.
    pub const fn raw_severity(self) -> clap_sys::ext::log::clap_log_severity {
        self.raw_severity
    }
    /// One of the seven official severities, or explicitly unknown.
    pub const fn severity(self) -> Option<LogSeverity> {
        self.severity
    }
    /// At most the bounded copied prefix; valid only during this callback.
    pub const fn prefix(self) -> &'a [u8] {
        self.prefix
    }
    /// Whether this prefix ends at the actual C-string terminator.
    pub const fn terminated(self) -> bool {
        self.terminated
    }
    /// Whether the plugin supplied a null message instead of a valid C string.
    pub const fn null_message(self) -> bool {
        self.null_message
    }
    /// Validate exactly the retained prefix without allocation or replacement.
    ///
    /// A truncated prefix can end inside an otherwise valid UTF-8 character;
    /// the caller must distinguish an incomplete suffix from malformed bytes.
    pub fn text(self) -> Result<&'a str, std::str::Utf8Error> {
        std::str::from_utf8(self.prefix)
    }
}

/// A severity level for a log message.
#[repr(i32)]
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub enum LogSeverity {
    /// Debug level
    Debug = 0,
    /// Info level
    Info = 1,
    /// Warning level
    Warning = 2,
    /// Error level
    Error = 3,
    /// Fatal level
    Fatal = 4,

    /// This severity could be used for reporting host misbehavior
    HostMisbehaving = 5,
    /// This severity could be used for reporting plugin misbehavior, usually by a layer between the host and the plugin.
    PluginMisbehaving = 6,
}

impl LogSeverity {
    /// Creates a [`LogSeverity`] from the raw CLAP integer.
    pub fn from_raw(raw: clap_log_severity) -> Option<Self> {
        use LogSeverity::*;
        use clap_sys::ext::log::*;

        match raw {
            CLAP_LOG_DEBUG => Some(Debug),
            CLAP_LOG_INFO => Some(Info),
            CLAP_LOG_WARNING => Some(Warning),
            CLAP_LOG_ERROR => Some(Error),
            CLAP_LOG_FATAL => Some(Fatal),
            CLAP_LOG_HOST_MISBEHAVING => Some(HostMisbehaving),
            CLAP_LOG_PLUGIN_MISBEHAVING => Some(PluginMisbehaving),
            _ => None,
        }
    }

    /// Converts this [`LogSeverity`] into the raw CLAP integer.
    #[inline]
    pub fn to_raw(self) -> clap_log_severity {
        self as _
    }
}

impl Display for LogSeverity {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let displayed = match self {
            LogSeverity::Debug => "DEBUG",
            LogSeverity::Info => "INFO",
            LogSeverity::Warning => "WARN",
            LogSeverity::Error => "ERROR",
            LogSeverity::Fatal => "FATAL",
            LogSeverity::HostMisbehaving => "HOST_MISBEHAVING",
            LogSeverity::PluginMisbehaving => "PLUGIN_MISBEHAVING",
        };

        f.write_str(displayed)
    }
}

/// The Host-side of the Log extension.
#[derive(Copy, Clone)]
#[allow(dead_code)]
pub struct HostLog(RawExtension<HostExtensionSide, clap_host_log>);

// SAFETY: This type is repr(C) and ABI-compatible with the matching extension type.
unsafe impl Extension for HostLog {
    const IDENTIFIERS: &[&CStr] = &[CLAP_EXT_LOG];
    type ExtensionSide = HostExtensionSide;

    #[inline]
    unsafe fn from_raw(raw: RawExtension<Self::ExtensionSide>) -> Self {
        // SAFETY: the guarantee that this pointer is of the correct type is upheld by the caller.
        Self(unsafe { raw.cast() })
    }
}

#[cfg(feature = "clack-plugin")]
mod plugin {
    use super::*;
    use clack_plugin::host::HostSharedHandle;

    impl HostLog {
        /// Logs a message through the host with the given severity.
        #[inline]
        pub fn log(&self, host: &HostSharedHandle, log_severity: LogSeverity, message: &CStr) {
            if let Some(log) = host.use_extension(&self.0).log {
                // SAFETY: This type ensures the function pointer is valid.
                unsafe { log(host.as_raw(), log_severity.to_raw(), message.as_ptr()) }
            }
        }
    }
}
