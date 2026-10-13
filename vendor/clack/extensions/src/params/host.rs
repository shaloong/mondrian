use super::*;
use clack_common::events::io::{InputEvents, OutputEvents};
use clack_host::extensions::prelude::*;
use std::mem::MaybeUninit;

/// A buffer in which plugins may write parameter information into.
#[derive(Clone)]
pub struct ParamInfoBuffer {
    inner: MaybeUninit<clap_param_info>,
}

impl Default for ParamInfoBuffer {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl ParamInfoBuffer {
    /// Creates a new, empty [`ParamInfoBuffer`]  for the plugin to write parameter information into.
    #[inline]
    pub const fn new() -> Self {
        Self {
            inner: MaybeUninit::zeroed(),
        }
    }
}

/// Outcome of the existing single native parameter query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamQueryReturn {
    /// The extension has no callback for this query.
    MissingCallback,
    /// The native callback returned false; output fields are unavailable.
    NativeFalse,
    /// The native callback returned true; original conversion still determines usability.
    NativeTrue,
}

/// Owned numeric metadata only; no cookie, native pointer or buffer lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamInfoScalars {
    /// Native parameter identity, including a reserved identity in raw evidence.
    pub id: u32,
    /// All parameter flag bits, including unknown bits retained by conversion.
    pub flags: u32,
    /// Exact floating-point bits of the minimum plain value.
    pub min_bits: u64,
    /// Exact floating-point bits of the maximum plain value.
    pub max_bits: u64,
    /// Exact floating-point bits of the default plain value.
    pub default_bits: u64,
}

impl ParamInfoScalars {
    fn raw(info: &clap_param_info) -> Self {
        Self {
            id: info.id,
            flags: info.flags,
            min_bits: info.min_value.to_bits(),
            max_bits: info.max_value.to_bits(),
            default_bits: info.default_value.to_bits(),
        }
    }
    fn converted(info: &ParamInfo<'_>) -> Self {
        Self {
            id: info.id.get(),
            flags: info.flags.bits(),
            min_bits: info.min_value.to_bits(),
            max_bits: info.max_value.to_bits(),
            default_bits: info.default_value.to_bits(),
        }
    }
}

/// Fixed-size same-call evidence. False/missing output is unavailable.
/// This intentionally has no generic raw-before snapshot: a buffer may be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamInfoQueryObservation {
    /// Whether the callback was absent or returned false or true.
    pub native_return: ParamQueryReturn,
    /// Successful native output before conversion; unavailable otherwise.
    pub raw_after: Option<ParamInfoScalars>,
    /// Original converted output; absent when conversion rejects the native identity.
    pub converted: Option<ParamInfoScalars>,
}

/// Current value is separate from default metadata. No false-output read occurs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamValueQueryObservation {
    /// Whether the callback was absent or returned false or true.
    pub native_return: ParamQueryReturn,
    /// Bits of the same initialized zero local used by the ordinary query.
    pub initialized_before_bits: u64,
    /// Bits returned by a successful native callback; unavailable otherwise.
    pub raw_after_bits: Option<u64>,
    /// Bits in the original successful return value, separate from metadata defaults.
    pub converted_bits: Option<u64>,
}

type GetInfoCallback = unsafe extern "C" fn(*const clap_plugin, u32, *mut clap_param_info) -> bool;
type GetValueCallback = unsafe extern "C" fn(*const clap_plugin, u32, *mut f64) -> bool;

/// Perform the original single metadata query and optional owned observation.
///
/// # Safety
/// The plugin and matching callback must remain live on the main thread. The
/// buffer must be exclusively borrowed, and native true must provide initialized
/// output valid for the original conversion. Native writes must finish on return.
unsafe fn query_info<'b, F: FnOnce(ParamInfoQueryObservation)>(
    plugin: *const clap_plugin,
    index: u32,
    buffer: &'b mut ParamInfoBuffer,
    callback: Option<GetInfoCallback>,
    observer: Option<F>,
) -> Option<ParamInfo<'b>> {
    let Some(callback) = callback else {
        if let Some(observer) = observer {
            observer(ParamInfoQueryObservation {
                native_return: ParamQueryReturn::MissingCallback,
                raw_after: None,
                converted: None,
            });
        }
        return None;
    };
    // SAFETY: matching live plugin/callback and exclusive buffer; invoke exactly once.
    let success = unsafe { callback(plugin, index, buffer.inner.as_mut_ptr()) };
    if !success {
        if let Some(observer) = observer {
            observer(ParamInfoQueryObservation {
                native_return: ParamQueryReturn::NativeFalse,
                raw_after: None,
                converted: None,
            });
        }
        return None;
    }
    // Original get_info already relies on native true guaranteeing valid output.
    // Disabled observation does not read/copy diagnostic fields.
    let raw_after = observer.as_ref().map(|_| {
        // SAFETY: native true initialized the same exclusively borrowed output buffer.
        ParamInfoScalars::raw(unsafe { buffer.inner.assume_init_ref() })
    });
    // SAFETY: preserve the original success-only conversion of initialized output.
    let result = unsafe { ParamInfo::from_raw(buffer.inner.assume_init_mut()) };
    if let Some(observer) = observer {
        observer(ParamInfoQueryObservation {
            native_return: ParamQueryReturn::NativeTrue,
            raw_after,
            converted: result.as_ref().map(ParamInfoScalars::converted),
        });
    }
    result
}

/// Perform the original single value query and optional owned observation.
///
/// # Safety
/// The plugin and matching callback must remain live on the main thread. The
/// callback must respect the exclusive initialized output local and finish every
/// native write before returning.
unsafe fn query_value<F: FnOnce(ParamValueQueryObservation)>(
    plugin: *const clap_plugin,
    id: u32,
    callback: Option<GetValueCallback>,
    observer: Option<F>,
) -> Option<f64> {
    let mut value = 0.0;
    let Some(callback) = callback else {
        if let Some(observer) = observer {
            observer(ParamValueQueryObservation {
                native_return: ParamQueryReturn::MissingCallback,
                initialized_before_bits: 0,
                raw_after_bits: None,
                converted_bits: None,
            });
        }
        return None;
    };
    // SAFETY: the caller provides the matching live plugin and callback; value is exclusive.
    let valid = unsafe { callback(plugin, id, &mut value) };
    let result = if valid { Some(value) } else { None };
    if let Some(observer) = observer {
        let bits = if valid { Some(value.to_bits()) } else { None };
        observer(ParamValueQueryObservation {
            native_return: if valid {
                ParamQueryReturn::NativeTrue
            } else {
                ParamQueryReturn::NativeFalse
            },
            initialized_before_bits: 0,
            raw_after_bits: bits,
            converted_bits: result.map(f64::to_bits),
        });
    }
    result
}

impl PluginParams {
    /// Returns the total number of parameters the plugin exposes.
    pub fn count(&self, plugin: &PluginMainThreadHandle) -> u32 {
        match plugin.use_extension(&self.0).count {
            None => 0,
            // SAFETY: This type ensures the function pointer is valid.
            Some(count) => unsafe { count(plugin.as_raw()) },
        }
    }

    /// Gets the metadata for a parameter by its index.
    ///
    /// The host calls this to learn about a parameter’s identity, range, name,
    /// and other properties. The implementation should write the parameter’s
    /// metadata into the provided `info` writer.
    ///
    /// # Arguments
    ///
    /// * `index`: The index of the parameter to query. Must be less than
    ///   the value returned by `count()`.
    /// * `info`: A writer to populate with the parameter’s metadata.
    ///
    /// # Return
    ///
    /// Returns `true` on success, or `false` if `index` is out of bounds.
    pub fn get_info<'b>(
        &self,
        plugin: &PluginMainThreadHandle,
        index: u32,
        buffer: &'b mut ParamInfoBuffer,
    ) -> Option<ParamInfo<'b>> {
        // SAFETY: original typed handle/extension and exclusive output buffer.
        unsafe {
            query_info(
                plugin.as_raw(),
                index,
                buffer,
                plugin.use_extension(&self.0).get_info,
                None::<fn(ParamInfoQueryObservation)>,
            )
        }
    }

    /// Diagnostic-only, main-thread and non-realtime variant of the same query.
    /// The observer receives one fixed owned scalar outcome after the original
    /// callback/conversion. It must not query the plugin, service callbacks or
    /// change admission. No observer/global sink is installed by default.
    /// Output is unavailable on missing/false; no generic raw-before read occurs.
    /// Keep the observer bounded; publish/format/file I/O at an outer owner only.
    /// Observer panics are caller policy, not an FFI/unwind recovery guarantee.
    pub fn get_info_observed<'b>(
        &self,
        plugin: &PluginMainThreadHandle,
        index: u32,
        buffer: &'b mut ParamInfoBuffer,
        observer: impl FnOnce(ParamInfoQueryObservation),
    ) -> Option<ParamInfo<'b>> {
        // SAFETY: same typed handle/extension and output buffer as get_info.
        unsafe {
            query_info(
                plugin.as_raw(),
                index,
                buffer,
                plugin.use_extension(&self.0).get_info,
                Some(observer),
            )
        }
    }

    /// Gets the current value of a parameter by its ID.
    ///
    /// The host calls this to read a parameter’s state. The implementation
    /// should return the parameter’s current plain value.
    ///
    /// # Arguments
    ///
    /// * `param_id`: The ID of the parameter to query.
    ///
    /// # Return
    ///
    /// Returns the current value of the parameter, or `None` if the ID is invalid.
    pub fn get_value(&self, plugin: &PluginMainThreadHandle, param_id: ClapId) -> Option<f64> {
        // SAFETY: original typed handle/extension and initialized local value.
        unsafe {
            query_value(
                plugin.as_raw(),
                param_id.get(),
                plugin.use_extension(&self.0).get_value,
                None::<fn(ParamValueQueryObservation)>,
            )
        }
    }

    /// Diagnostic-only main-thread/non-realtime single current-value query.
    /// Its before value is the same initialized 0.0 local used by get_value.
    /// Missing/false output stays unavailable and the original Option is returned.
    /// The fixed owned outcome is never a parameter default or admission input.
    /// Observer rules are the same as get_info_observed; default get_value has none.
    pub fn get_value_observed(
        &self,
        plugin: &PluginMainThreadHandle,
        param_id: ClapId,
        observer: impl FnOnce(ParamValueQueryObservation),
    ) -> Option<f64> {
        // SAFETY: same typed handle/extension as get_value.
        unsafe {
            query_value(
                plugin.as_raw(),
                param_id.get(),
                plugin.use_extension(&self.0).get_value,
                Some(observer),
            )
        }
    }

    /// Converts a parameter’s plain value to a human-readable string.
    ///
    /// The host uses this to display parameter values in a user-friendly format,
    /// such as "440.0 Hz" instead of just "440.0".
    ///
    /// Note that this method will always reset the buffer to all-zeros before each use, for safety
    /// reasons in case the plugin implementation misbehaves.
    /// Clearing the buffer before passing it to this method is therefore redundant.
    ///
    /// # Arguments
    ///
    /// * `param_id`: The ID of the parameter.
    /// * `value`: The plain value to format.
    /// * `buffer`: A buffer to write the formatted string into.
    ///
    /// # Return
    ///
    /// Returns `Ok(())` on success, or `Err` if formatting fails.
    pub fn value_to_text<'b>(
        &self,
        plugin: &PluginMainThreadHandle,
        param_id: ClapId,
        value: f64,
        buffer: &'b mut [u8],
    ) -> Result<&'b mut [u8], core::fmt::Error> {
        let Some(value_to_text) = plugin.use_extension(&self.0).value_to_text else {
            return Err(core::fmt::Error);
        };
        let len = u32::try_from(buffer.len()).unwrap_or(u32::MAX);

        buffer.fill(0);

        // SAFETY: This type ensures the function pointer is valid.
        let valid = unsafe {
            value_to_text(
                plugin.as_raw(),
                param_id.get(),
                value,
                buffer.as_mut_ptr() as *mut _,
                len,
            )
        };

        if !valid {
            return Err(core::fmt::Error);
        }

        // If no nul byte found, we take the entire buffer
        let buffer_total_len = buffer.iter().position(|b| *b == 0).unwrap_or(len as usize);
        Ok(&mut buffer[..buffer_total_len])
    }

    /// Converts a human-readable string back to a parameter’s plain value.
    ///
    /// The host uses this to handle user text input for parameter values.
    ///
    /// # Arguments
    ///
    /// * `param_id`: The ID of the parameter.
    /// * `text`: The text to parse.
    ///
    /// # Return
    ///
    /// Returns the parsed value, or `None` if parsing fails or the ID is invalid.
    pub fn text_to_value(
        &self,
        plugin: &PluginMainThreadHandle,
        param_id: ClapId,
        text: &CStr,
    ) -> Option<f64> {
        let mut value = 0.0;

        // SAFETY: This type ensures the function pointer is valid.
        let valid = unsafe {
            plugin.use_extension(&self.0).text_to_value?(
                plugin.as_raw(),
                param_id.get(),
                text.as_ptr(),
                &mut value,
            )
        };

        if valid { Some(value) } else { None }
    }

    /// Flushes pending parameter changes between the host and plugin.
    ///
    /// This method is called by the host to synchronize parameter values in
    /// either direction. It receives incoming changes via `input_parameter_changes`
    /// and allows the plugin to send outgoing changes via `output_parameter_changes`.
    ///
    /// This is typically called when the plugin is not actively processing audio,
    /// but can also be used for parameter automation without audio playback.
    ///
    /// Note that if the plugin is processing, then the `process()` call will already achieve the
    /// parameter update (bidirectional), so a call to flush isn't required.
    /// Also be aware that the plugin may use the sample offset in `process()`, while this information would be
    /// lost within `flush()`.
    ///
    /// # Arguments
    ///
    /// * `input_parameter_changes`: A reader for incoming parameter change events.
    /// * `output_parameter_changes`: A writer for outgoing parameter change events.
    ///
    ///
    pub fn flush(
        &self,
        plugin: &mut InactivePluginMainThreadHandle,
        input_parameter_changes: &InputEvents,
        output_parameter_changes: &mut OutputEvents,
    ) {
        if let Some(flush) = plugin.use_extension(&self.0).flush {
            // SAFETY: This type ensures the function pointer is valid.
            unsafe {
                flush(
                    plugin.as_raw(),
                    input_parameter_changes.as_raw(),
                    output_parameter_changes.as_raw_mut(),
                )
            }
        }
    }

    /// Flushes a set of parameter changes when the plugin is active.
    /// This method cannot be called concurrently with `process`, which is statically guaranteed by the [`PluginAudioProcessorHandle`] type.
    ///
    /// Note: if the plugin is processing, then the process() call will already
    /// achieve the parameter update (bidirectional), so a call to flush isn't
    /// required, also be aware that the plugin may use the sample offset in
    /// process(), while this information would be lost within flush().
    pub fn flush_active(
        &self,
        plugin: &mut PluginAudioProcessorHandle,
        input_parameter_changes: &InputEvents,
        output_parameter_changes: &mut OutputEvents,
    ) {
        if let Some(flush) = plugin.use_extension(&self.0).flush {
            // SAFETY: This type ensures the function pointer is valid.
            unsafe {
                flush(
                    plugin.as_raw(),
                    input_parameter_changes.as_raw(),
                    output_parameter_changes.as_raw_mut(),
                )
            }
        }
    }
}

/// Implementation of the thread-safe Host Params extension operations.
pub trait HostParamsImplShared {
    /// Plugin requested a parameter flush.
    ///
    /// The host must then schedule a call to either:
    /// - [`PluginParams::flush`] or [`PluginParams::flush_active`]
    /// - the process callback
    ///
    /// This function is always safe to use, but should not be called from the plugin's audio thread,
    /// as it would already be within `process()` or `flush()`.
    fn request_flush(&self);
}

/// Implementation of the main-thread Host Params extension operations.
pub trait HostParamsImplMainThread {
    /// Rescan the full list of parameters, according to the given `flags`.
    /// See [`ParamRescanFlags`] for more details.
    fn rescan(&self, flags: ParamRescanFlags);
    /// Clears references (such as automation or modulation) to a parameter (identified by `param_id`), according to the given `flags`.
    /// See [`ParamClearFlags`] for more details.
    fn clear(&self, param_id: ClapId, flags: ParamClearFlags);
}

// SAFETY: The given struct is the CLAP extension struct for the matching side of this extension.
unsafe impl<H> ExtensionImplementation<H> for HostParams
where
    for<'a> H:
        HostHandlers<Shared<'a>: HostParamsImplShared, MainThread<'a>: HostParamsImplMainThread>,
{
    const IMPLEMENTATION: RawExtensionImplementation =
        RawExtensionImplementation::new(&clap_host_params {
            rescan: Some(rescan::<H>),
            clear: Some(clear::<H>),
            request_flush: Some(request_flush::<H>),
        });
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn rescan<H>(host: *const clap_host, flags: clap_param_rescan_flags)
where
    for<'a> H: HostHandlers<MainThread<'a>: HostParamsImplMainThread>,
{
    HostWrapper::<H>::handle_main_thread(host, |host| {
        host.rescan(ParamRescanFlags::from_bits_retain(flags));

        Ok(())
    });
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn clear<H>(host: *const clap_host, param_id: u32, flags: clap_param_clear_flags)
where
    for<'a> H: HostHandlers<MainThread<'a>: HostParamsImplMainThread>,
{
    HostWrapper::<H>::handle_main_thread(host, |host| {
        let param_id = ClapId::from_raw(param_id)
            .ok_or(HostWrapperError::InvalidParameter("Invalid param_id"))?;

        host.clear(param_id, ParamClearFlags::from_bits_retain(flags));

        Ok(())
    });
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn request_flush<H>(host: *const clap_host)
where
    for<'a> H: HostHandlers<Shared<'a>: HostParamsImplShared>,
{
    HostWrapper::<H>::handle(host, |host| {
        host.shared().request_flush();

        Ok(())
    });
}

#[cfg(test)]
mod param_query_observation_contracts {
    use super::*;
    use std::cell::Cell;

    const TRUE: u8 = 0;
    const FALSE: u8 = 1;
    const RESERVED_ID: u8 = 2;
    const TINY_BITS: u64 = 0x378F_FE40_0000_0000;
    struct Audit {
        mode: u8,
        info_calls: Cell<u32>,
        value_calls: Cell<u32>,
        value_bits: u64,
    }

    /// Supply owned metadata to the synchronous raw-C-ABI test query.
    ///
    /// # Safety
    /// The caller must provide the test's live plugin/Audit and exclusive aligned
    /// output storage. Neither pointer may escape this synchronous callback.
    unsafe extern "C" fn info_callback(
        plugin: *const clap_plugin,
        _index: u32,
        output: *mut clap_param_info,
    ) -> bool {
        // SAFETY: the test owns the live plugin and its Audit for this synchronous call.
        let audit = unsafe { &*((*plugin).plugin_data.cast::<Audit>()) };
        audit.info_calls.set(audit.info_calls.get() + 1);
        // The C info contains only pointers, numeric scalars and char arrays:
        // zero is a valid initialized representation, not an uninitialized read.
        // SAFETY: every scalar, nullable pointer and character array accepts zero bits.
        let mut info: clap_param_info = unsafe { MaybeUninit::zeroed().assume_init() };
        info.id = if audit.mode == RESERVED_ID {
            u32::MAX
        } else {
            7
        };
        info.flags = 0x8000_0000; // Existing conversion must retain unknown bits.
        info.min_value = -0.0;
        info.max_value = 1.0;
        info.default_value = f64::from_bits(TINY_BITS);
        // SAFETY: query_info supplied its exclusive valid output storage.
        unsafe { output.write(info) };
        // False deliberately writes first: consumer must still mark output unavailable.
        audit.mode != FALSE
    }
    /// Supply an owned value to the synchronous raw-C-ABI test query.
    ///
    /// # Safety
    /// The caller must provide the test's live plugin/Audit and exclusive aligned
    /// output storage. Neither pointer may escape this synchronous callback.
    unsafe extern "C" fn value_callback(
        plugin: *const clap_plugin,
        _id: u32,
        output: *mut f64,
    ) -> bool {
        // SAFETY: the test owns the live plugin and its Audit for this synchronous call.
        let audit = unsafe { &*((*plugin).plugin_data.cast::<Audit>()) };
        audit.value_calls.set(audit.value_calls.get() + 1);
        // SAFETY: query_value supplied its exclusive initialized local output storage.
        unsafe { output.write(f64::from_bits(audit.value_bits)) };
        audit.mode != FALSE
    }
    fn plugin(audit: &Audit) -> clap_plugin {
        // Tests use only plugin_data; nullable C pointers/optional functions stay null.
        // SAFETY: all fields in this C record accept zero; only plugin_data is consumed.
        let mut plugin: clap_plugin = unsafe { MaybeUninit::zeroed().assume_init() };
        plugin.plugin_data = (audit as *const Audit).cast_mut().cast();
        plugin
    }

    #[test]
    fn info_missing_false_true_and_reserved_id_preserve_option_and_one_call() {
        for (mode, present) in [
            (TRUE, false),
            (FALSE, true),
            (TRUE, true),
            (RESERVED_ID, true),
        ] {
            let audit = Audit {
                mode,
                info_calls: Cell::new(0),
                value_calls: Cell::new(0),
                value_bits: TINY_BITS,
            };
            let plugin = plugin(&audit);
            let callback = present.then_some(info_callback as GetInfoCallback);
            let mut plain_buffer = ParamInfoBuffer::new();
            // SAFETY: plugin/Audit are live, callback matches and buffer is exclusive.
            let plain = unsafe {
                query_info(
                    &plugin,
                    5,
                    &mut plain_buffer,
                    callback,
                    None::<fn(ParamInfoQueryObservation)>,
                )
            }
            .as_ref()
            .map(ParamInfoScalars::converted);
            let expected = if present && mode == TRUE {
                Some(ParamInfoScalars {
                    id: 7,
                    flags: 0x8000_0000,
                    min_bits: (-0.0_f64).to_bits(),
                    max_bits: 1.0_f64.to_bits(),
                    default_bits: TINY_BITS,
                })
            } else {
                None
            };
            assert_eq!(plain, expected);
            assert_eq!(audit.info_calls.get(), u32::from(present));
            audit.info_calls.set(0);
            let mut observed_buffer = ParamInfoBuffer::new();
            let mut seen = None;
            let mut emissions = 0;
            // SAFETY: same owned plugin/Audit, matching callback and exclusive fresh buffer.
            let observed = unsafe {
                query_info(
                    &plugin,
                    5,
                    &mut observed_buffer,
                    callback,
                    Some(|event| {
                        emissions += 1;
                        seen = Some(event);
                    }),
                )
            }
            .as_ref()
            .map(ParamInfoScalars::converted);
            assert_eq!(audit.info_calls.get(), u32::from(present));
            assert_eq!(audit.value_calls.get(), 0);
            assert_eq!(emissions, 1);
            assert_eq!(observed, plain);
            assert_eq!(observed, expected);
            let event = seen.unwrap();
            if !present {
                assert_eq!(event.native_return, ParamQueryReturn::MissingCallback);
                assert_eq!((event.raw_after, event.converted), (None, None));
            } else if mode == FALSE {
                assert_eq!(event.native_return, ParamQueryReturn::NativeFalse);
                assert_eq!((event.raw_after, event.converted), (None, None));
            } else {
                assert_eq!(event.native_return, ParamQueryReturn::NativeTrue);
                let raw = event.raw_after.unwrap();
                assert_eq!(raw.min_bits, (-0.0_f64).to_bits());
                assert_eq!(raw.default_bits, TINY_BITS);
                assert_eq!(raw.flags, 0x8000_0000);
                if mode == RESERVED_ID {
                    assert_eq!(raw.id, u32::MAX);
                    assert_eq!(event.converted, None);
                } else {
                    assert_eq!(event.converted, Some(raw));
                }
            }
        }
    }

    #[test]
    fn value_missing_false_true_preserves_bits_and_one_call() {
        for bits in [
            0_u64,
            (-0.0_f64).to_bits(),
            TINY_BITS,
            0x7FF8_0000_0000_0042,
        ] {
            for (mode, present) in [(TRUE, false), (FALSE, true), (TRUE, true)] {
                let audit = Audit {
                    mode,
                    info_calls: Cell::new(0),
                    value_calls: Cell::new(0),
                    value_bits: bits,
                };
                let plugin = plugin(&audit);
                let callback = present.then_some(value_callback as GetValueCallback);
                // SAFETY: plugin/Audit are live and the optional callback matches that plugin.
                let plain = unsafe {
                    query_value(&plugin, 7, callback, None::<fn(ParamValueQueryObservation)>)
                }
                .map(f64::to_bits);
                let expected = if present && mode == TRUE {
                    Some(bits)
                } else {
                    None
                };
                assert_eq!(plain, expected);
                assert_eq!(audit.value_calls.get(), u32::from(present));
                audit.value_calls.set(0);
                let mut seen = None;
                let mut emissions = 0;
                // SAFETY: same owned live plugin/Audit and matching optional callback.
                let observed = unsafe {
                    query_value(
                        &plugin,
                        7,
                        callback,
                        Some(|event| {
                            emissions += 1;
                            seen = Some(event);
                        }),
                    )
                }
                .map(f64::to_bits);
                assert_eq!(audit.value_calls.get(), u32::from(present));
                assert_eq!(audit.info_calls.get(), 0);
                assert_eq!(emissions, 1);
                assert_eq!(observed, plain);
                assert_eq!(observed, expected);
                let event = seen.unwrap();
                assert_eq!(event.initialized_before_bits, 0);
                assert_eq!(event.converted_bits, plain);
                if !present {
                    assert_eq!(event.native_return, ParamQueryReturn::MissingCallback);
                    assert_eq!(event.raw_after_bits, None);
                } else if mode == FALSE {
                    assert_eq!(event.native_return, ParamQueryReturn::NativeFalse);
                    assert_eq!(event.raw_after_bits, None);
                } else {
                    assert_eq!(event.native_return, ParamQueryReturn::NativeTrue);
                    assert_eq!(event.raw_after_bits, Some(bits));
                }
            }
        }
    }
}
