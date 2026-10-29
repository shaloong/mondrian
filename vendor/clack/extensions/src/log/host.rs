use super::{HOST_LOG_MESSAGE_CAPACITY, HostLog, HostLogMessage, LogSeverity};
use clack_host::extensions::prelude::*;
use clap_sys::ext::log::{clap_host_log, clap_log_severity};
use std::os::raw::c_char;

/// Implementation of the thread-safe Host-side Log extension.
pub trait HostLogImpl {
    /// Observe one bounded immutable callback message on its calling thread.
    ///
    /// A realtime-capable implementation must copy into already admitted
    /// storage without allocation, locks, waits, formatting or terminal I/O.
    /// The borrowed prefix is valid only for this call. Unknown severities,
    /// null messages and truncation remain explicit message facts.
    fn log(&self, message: HostLogMessage<'_>);
}

// SAFETY: This is the official generated Host Log ABI, implemented by H's
// corresponding shared handler. Only the native callback owns raw pointers.
unsafe impl<H> ExtensionImplementation<H> for HostLog
where
    for<'a> H: HostHandlers<Shared<'a>: HostLogImpl>,
{
    const IMPLEMENTATION: RawExtensionImplementation =
        RawExtensionImplementation::new(&clap_host_log {
            log: Some(log::<H>),
        });
}

// SAFETY: CLAP supplies the valid matching H host pointer and either a valid
// nul-terminated message pointer or the explicitly handled null violation.
// A nonnull invalid/unmapped pointer remains a plugin C-ABI lifetime violation;
// the host cannot validate arbitrary foreign address ranges by dereferencing.
unsafe extern "C" fn log<H>(host: *const clap_host, severity: clap_log_severity, msg: *const c_char)
where
    for<'a> H: HostHandlers<Shared<'a>: HostLogImpl>,
{
    // The observer wrapper neither recursively logs failures nor writes stderr.
    // There is no correct native owner to attribute a null/mismatched host to.
    let observer = |host: &HostWrapper<H>| {
        let mut prefix = [0u8; HOST_LOG_MESSAGE_CAPACITY];
        let mut used = 0usize;
        let mut terminated = false;
        if !msg.is_null() {
            while used < prefix.len() {
                // SAFETY: SDK requires a valid C string. Stop at its first nul
                // or at the fixed prefix bound; never invoke unbounded strlen.
                let byte = unsafe { msg.add(used).read() } as u8;
                if byte == 0 {
                    terminated = true;
                    break;
                }
                prefix[used] = byte;
                used += 1;
            }
            if used == prefix.len() {
                // SAFETY: for a legal C string this byte exists (possibly its
                // terminator). One bounded lookahead distinguishes exact fit.
                terminated = unsafe { msg.add(used).read() } == 0;
            }
        }
        host.shared().log(HostLogMessage {
            raw_severity: severity,
            severity: LogSeverity::from_raw(severity),
            prefix: &prefix[..used],
            terminated,
            null_message: msg.is_null(),
        });
        Ok(())
    };
    // SAFETY: The native callback contract supplies this matching live owner.
    let _ = unsafe { HostWrapper::<H>::handle_observation(host, observer) };
}
