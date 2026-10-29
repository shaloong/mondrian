//! Actual native callback controls through the official generated CLAP ABI.
//! Invalid input controls are defensive policy checks, not compliant vendor PASS.

use clack_extensions::log::{HostLog, HostLogImpl, HostLogMessage};
use clack_host::prelude::*;
use clack_plugin::prelude::{
    Audio, DefaultPluginFactory, Events, HostAudioProcessorHandle, HostMainThreadHandle,
    HostSharedHandle, Plugin, PluginAudioConfiguration, PluginDescriptor, PluginError,
    PluginMainThread, PluginShared, Process, ProcessStatus, SinglePluginEntry,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

#[derive(Default)]
struct Facts {
    calls: AtomicU32,
    severities: AtomicU32,
    unknown: AtomicI32,
    special: AtomicU32,
}
struct Observer(Arc<Facts>);
impl SharedHandler<'_> for Observer {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for Observer {
    fn log(&self, message: HostLogMessage<'_>) {
        self.0.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(severity) = message.severity() {
            self.0
                .severities
                .fetch_or(1 << severity.to_raw(), Ordering::Relaxed);
        } else {
            self.0
                .unknown
                .store(message.raw_severity(), Ordering::Relaxed);
        }
        if !message.terminated() && message.prefix() == [b'x'; 512] {
            self.0.special.fetch_or(1, Ordering::Relaxed);
        }
        if message.prefix() == [255] && message.text().is_err() {
            self.0.special.fetch_or(2, Ordering::Relaxed);
        }
        if message.null_message() && message.prefix().is_empty() && !message.terminated() {
            self.0.special.fetch_or(4, Ordering::Relaxed);
        }
        if message.raw_severity() == -17 && message.prefix() == b"unknown" {
            self.0.special.fetch_or(8, Ordering::Relaxed);
        }
    }
}
struct ObservationHost;
impl HostHandlers for ObservationHost {
    type Shared<'a> = Observer;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(builder: &mut HostExtensions<Self>, _: &Observer) {
        builder.register::<HostLog>();
    }
}

struct NativeEmitter;
struct EmitterShared<'a>(HostSharedHandle<'a>);
impl<'a> PluginShared<'a> for EmitterShared<'a> {}
struct EmitterMain<'a>(&'a EmitterShared<'a>);
struct NeverActivated;
impl Plugin for NativeEmitter {
    type Shared<'a> = EmitterShared<'a>;
    type MainThread<'a> = EmitterMain<'a>;
    type AudioProcessor<'a> = NeverActivated;
}
impl DefaultPluginFactory for NativeEmitter {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("org.mondrian.test.sdk-log", "SDK native logger")
    }
    fn new_shared(host: HostSharedHandle<'_>) -> Result<EmitterShared<'_>, PluginError> {
        Ok(EmitterShared(host))
    }
    fn new_main_thread<'a>(
        _: HostMainThreadHandle<'a>,
        shared: &'a EmitterShared<'a>,
    ) -> Result<EmitterMain<'a>, PluginError> {
        Ok(EmitterMain(shared))
    }
}
impl<'a> PluginMainThread<'a, EmitterShared<'a>> for EmitterMain<'a> {
    fn on_main_thread(&self) {
        let raw = self.0.0.as_raw();
        // SAFETY: This is the real live matching host supplied by Clack; the
        // returned table is interpreted using the canonical generated SDK ABI.
        let table = unsafe {
            raw.get_extension.expect("native extension callback")(
                raw,
                clap_sys::ext::log::CLAP_EXT_LOG.as_ptr(),
            )
        }
        .cast::<clap_sys::ext::log::clap_host_log>();
        assert!(!table.is_null());
        let callback = unsafe { (*table).log }.expect("official log callback");
        for severity in 0..7 {
            // SAFETY: Both owner and exact nul-terminated static string are live.
            unsafe {
                callback(raw, severity, c"seven severities".as_ptr());
            }
        }
        let mut long = [b'x'; 514];
        long[513] = 0;
        let invalid = [255_u8, 0];
        // SAFETY: Defensive controls retain legal extents/lifetimes. Invalid
        // UTF-8/unknown severity/null message intentionally violate plugin input
        // requirements, and do not qualify conforming third-party behavior.
        unsafe {
            callback(raw, 1, long.as_ptr().cast());
            callback(raw, 2, invalid.as_ptr().cast());
            callback(raw, -17, c"unknown".as_ptr());
            callback(raw, 3, std::ptr::null());
        }
    }
}
impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, EmitterShared<'a>, EmitterMain<'a>>
    for NeverActivated
{
    fn activate(
        _: HostAudioProcessorHandle<'a>,
        _: &EmitterMain<'a>,
        _: &'a EmitterShared<'a>,
        _: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Err(PluginError::Message(
            "no audio activation in callback-only control",
        ))
    }
    fn process(&mut self, _: Process, _: Audio, _: Events) -> Result<ProcessStatus, PluginError> {
        unreachable!("not activated")
    }
}

#[test]
fn native_sdk_log_observation_keeps_seven_severities_and_exact_defensive_byte_facts() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<NativeEmitter>>(c"/test/sdk-log")
        .expect("native owned entry");
    let facts = Arc::new(Facts::default());
    let info = HostInfo::new("Mondrian test", "Mondrian", "https://example.invalid", "0")
        .expect("identity");
    let mut instance = PluginInstance::<ObservationHost>::new(
        |_| Observer(Arc::clone(&facts)),
        |_| (),
        &entry,
        c"org.mondrian.test.sdk-log",
        &info,
    )
    .expect("real matching wrapper");
    instance.call_on_main_thread_callback();
    assert_eq!(facts.calls.load(Ordering::Relaxed), 11);
    assert_eq!(facts.severities.load(Ordering::Relaxed), 127);
    assert_eq!(facts.unknown.load(Ordering::Relaxed), -17);
    assert_eq!(
        facts.special.load(Ordering::Relaxed),
        15,
        "no lossy bytes, warning reclassification or unbounded C strlen"
    );
}
