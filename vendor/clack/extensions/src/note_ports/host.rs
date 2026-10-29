use super::*;
use clack_host::extensions::prelude::*;
use std::mem::MaybeUninit;

/// A scratch buffer for the plugin to write note port metadata to.
#[derive(Clone)]
pub struct NotePortInfoBuffer {
    inner: MaybeUninit<clap_note_port_info>,
}

impl Default for NotePortInfoBuffer {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl NotePortInfoBuffer {
    /// Get an empty buffer for the plugin to write note port metadata into.
    #[inline]
    pub const fn new() -> Self {
        Self {
            inner: MaybeUninit::zeroed(),
        }
    }
}

impl PluginNotePorts {
    /// Whether the native metadata getter exists, including for zero ports.
    /// Missing functions are malformed metadata rather than an empty result.
    pub fn has_get(&self, plugin: &PluginMainThreadHandle) -> bool {
        plugin.use_extension(&self.0).get.is_some()
    }

    /// Query the native count without treating a missing function as zero ports.
    ///
    /// Call on the plugin main thread while deactivated. This factual query does
    /// not advertise host event support or alter the existing typed API.
    pub fn count_raw(&self, plugin: &PluginMainThreadHandle, is_input: bool) -> Option<u32> {
        let count = plugin.use_extension(&self.0).count?;
        // SAFETY: the extension and main-thread handle belong to this instance.
        Some(unsafe { count(plugin.as_raw(), is_input) })
    }

    /// Query an owned native POD, preserving all numeric dialect bits.
    ///
    /// A new zeroed buffer is used on every call, so a partially written native
    /// result cannot retain bytes from another port. Missing `get` or native
    /// failure returns `None`. IDs and dialect policy remain the caller's job.
    /// No pointer into plugin storage is returned. Call while deactivated.
    pub fn get_raw(
        &self,
        plugin: &PluginMainThreadHandle,
        index: u32,
        is_input: bool,
    ) -> Option<clap_note_port_info> {
        let get = plugin.use_extension(&self.0).get?;
        // SAFETY: official clap_note_port_info is only integers and a fixed
        // character array. All-zero is valid storage for each of its fields.
        let mut buffer: clap_note_port_info = unsafe { std::mem::zeroed() };
        // SAFETY: extension/handle lifetime and function type are valid; the
        // local writable POD remains live for this synchronous native call.
        if unsafe { get(plugin.as_raw(), index, is_input, &mut buffer) } {
            Some(buffer)
        } else {
            None
        }
    }

    /// Returns number of note ports, for either input or output.
    pub fn count(&self, plugin: &PluginMainThreadHandle, is_input: bool) -> u32 {
        match plugin.use_extension(&self.0).count {
            None => 0,
            // SAFETY: This type ensures the function pointer is valid.
            Some(count) => unsafe { count(plugin.as_raw(), is_input) },
        }
    }

    /// Get information about a note port by its index, for either input or output.
    pub fn get<'b>(
        &self,
        plugin: &PluginMainThreadHandle,
        index: u32,
        is_input: bool,
        buffer: &'b mut NotePortInfoBuffer,
    ) -> Option<NotePortInfo<'b>> {
        let success =
            // SAFETY: This type ensures the function pointer is valid.
            unsafe { plugin.use_extension(&self.0).get?(plugin.as_raw(), index, is_input, buffer.inner.as_mut_ptr()) };

        if success {
            // SAFETY: we just checked the buffer was successfully written to
            Some(unsafe { NotePortInfo::from_raw(buffer.inner.assume_init_ref())? })
        } else {
            None
        }
    }
}

/// Implementation of the Host-side of the Note Ports extension.
pub trait HostNotePortsImpl {
    /// Query which note dialects are supported by the host.
    fn supported_dialects(&self) -> NoteDialects;

    /// Rescan the full list of note ports according to the flags.
    /// See [`NotePortRescanFlags`] for more details.
    fn rescan(&self, flags: NotePortRescanFlags);
}

/// Numeric raw callback facts on a thread-safe shared owner. The native ABI
/// remains main-thread only; implementations check actual thread/lifecycle
/// first and retain illegal callback observations without touching main state.
pub trait HostNotePortsRawImpl {
    /// Raw dialect mask, with native role rejection represented as zero.
    fn supported_dialects_raw(&self) -> u32;
    /// Original untruncated native rescan flags, including unknown bits.
    fn rescan_raw(&self, flags: u32);
}

// SAFETY: this is the same official extension ABI; registration selects this
// shared-only callback table instead of, never alongside, the legacy table.
unsafe impl<H> ExtensionImplementation<H> for HostNotePortsRaw
where
    for<'h> H: HostHandlers<Shared<'h>: HostNotePortsRawImpl>,
{
    const IMPLEMENTATION: RawExtensionImplementation =
        RawExtensionImplementation::new(&clap_host_note_ports {
            supported_dialects: Some(supported_dialects_raw::<H>),
            rescan: Some(rescan_raw::<H>),
        });
}

unsafe extern "C" fn supported_dialects_raw<H>(host: *const clap_host) -> u32
where
    for<'h> H: HostHandlers<Shared<'h>: HostNotePortsRawImpl>,
{
    // SAFETY: ABI caller supplies the matching live H wrapper. The observer
    // borrows only immutable Shared; no lazy initialization/main borrow/log I/O.
    unsafe {
        HostWrapper::<H>::handle_observation(host, |wrapper| {
            Ok(wrapper.shared().supported_dialects_raw())
        })
    }
    .unwrap_or(0)
}

unsafe extern "C" fn rescan_raw<H>(host: *const clap_host, flags: u32)
where
    for<'h> H: HostHandlers<Shared<'h>: HostNotePortsRawImpl>,
{
    // SAFETY: identical lifetime/type precondition. Raw bits reach the one
    // shared observation owner even when native role or semantics are refused.
    let _ = unsafe {
        HostWrapper::<H>::handle_observation(host, |wrapper| {
            wrapper.shared().rescan_raw(flags);
            Ok(())
        })
    };
}

// SAFETY: The given struct is the CLAP extension struct for the matching side of this extension.
unsafe impl<H> ExtensionImplementation<H> for HostNotePorts
where
    for<'h> H: HostHandlers<MainThread<'h>: HostNotePortsImpl>,
{
    const IMPLEMENTATION: RawExtensionImplementation =
        RawExtensionImplementation::new(&clap_host_note_ports {
            supported_dialects: Some(supported_dialects::<H>),
            rescan: Some(rescan::<H>),
        });
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn supported_dialects<H>(host: *const clap_host) -> u32
where
    for<'h> H: HostHandlers<MainThread<'h>: HostNotePortsImpl>,
{
    HostWrapper::<H>::handle_main_thread(host, |host| Ok(host.supported_dialects().bits()))
        .unwrap_or(0)
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn rescan<H>(host: *const clap_host, flags: u32)
where
    for<'h> H: HostHandlers<MainThread<'h>: HostNotePortsImpl>,
{
    HostWrapper::<H>::handle_main_thread(host, |host| {
        host.rescan(NotePortRescanFlags::from_bits_truncate(flags));

        Ok(())
    });
}
