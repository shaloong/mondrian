//! Bounded services owned by the one actual CLAP main thread.
//!
//! Plugin callbacks publish intents; the owning main thread services them.

use super::clap_gui_requests::GuiRequests;
use super::clap_parameter_source::{
    ClapParameterControlKind, ClapParameterControlSource, ClapParameterSourceFailure,
};
use super::clap_worker::{ClapHost, ClapHostShared, ClapNativeHostShared};
use crate::AudioProcessorHostError;
use clack_extensions::gui::{GuiSize, HostGuiImpl};
use clack_extensions::params::{
    HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::state::HostStateImpl;
use clack_extensions::timer::{HostTimerImpl, PluginTimer, TimerId};
use clack_host::prelude::{HostError, MainThreadHandler, PluginInstance};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(super) const MAX_TIMERS: usize = 32;

/// Fixed shared observations; callbacks never allocate, wait or invoke the GUI.
/// Main's request consumption is separate from Root/Session author admission.
pub(super) struct ClapNativeServicesShared {
    pub(super) note_ports: super::clap_note_input_contract::NotePortRequests,
    pub(super) timer_available: AtomicBool,
    pub(super) active: AtomicBool,
    pub(super) flush_requested: AtomicBool,
    pub(super) gui: GuiRequests,
    pub(super) wrong_thread: AtomicBool,
}

/// Default instances retain services inline. Only the explicit v2 metadata
/// operation shares this SAME services owner through native destroy. Deref
/// reaches the actual callback source; neither variant mirrors its observations.
pub(super) enum ClapNativeServicesOwner {
    Inline(ClapNativeServicesShared),
    Retained(std::sync::Arc<ClapNativeServicesShared>),
}

impl std::ops::Deref for ClapNativeServicesOwner {
    type Target = ClapNativeServicesShared;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Inline(services) => services,
            Self::Retained(services) => services,
        }
    }
}

impl ClapNativeServicesShared {
    pub(super) const fn new() -> Self {
        Self {
            note_ports: super::clap_note_input_contract::NotePortRequests::new(),
            timer_available: AtomicBool::new(false),
            active: AtomicBool::new(false),
            flush_requested: AtomicBool::new(false),
            gui: GuiRequests::new(),
            wrong_thread: AtomicBool::new(false),
        }
    }

    #[cfg(windows)]
    pub(super) fn close_requested(&self) -> bool {
        self.gui.close_requested()
    }
}

impl HostGuiImpl for ClapNativeHostShared {
    fn resize_hints_changed(&self) {
        self.services.gui.request_hints();
    }

    fn request_resize(&self, size: GuiSize) -> Result<(), HostError> {
        if !self.services.gui.request_resize(size) {
            return Err(HostError::Message(
                "CLAP embedded GUI resize is unavailable",
            ));
        }
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        if !self.services.gui.request_visibility(true) {
            return Err(HostError::Message("CLAP GUI is not created"));
        }
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        if !self.services.gui.request_visibility(false) {
            return Err(HostError::Message("CLAP GUI is not created"));
        }
        Ok(())
    }

    fn closed(&self, destroyed: bool) {
        self.services.gui.report_closed(destroyed);
    }
}

#[derive(Clone, Copy)]
struct RegisteredTimer {
    id: TimerId,
    period: Duration,
    next: Instant,
}

/// Inline main-handler storage; no timer or host-request Vec is allocated.
/// The long-lived shared borrow is supplied by the actual Clack HostWrapper.
pub(super) struct ClapHostMainThread<'a> {
    shared: &'a ClapNativeHostShared,
    timers: Cell<[Option<RegisteredTimer>; MAX_TIMERS]>,
    next_timer_id: Cell<u32>,
    parameter_controls: ClapParameterControlSource<'a>,
}

impl<'a> ClapHostMainThread<'a> {
    pub(super) fn new(shared: &'a ClapNativeHostShared) -> Self {
        Self {
            shared,
            timers: Cell::new([None; MAX_TIMERS]),
            next_timer_id: Cell::new(0),
            parameter_controls: ClapParameterControlSource::new(&shared.control.parameter_source),
        }
    }

    fn ensure_main(&self) -> Result<(), HostError> {
        use clack_extensions::thread_check::HostThreadCheckImpl;
        // [main-thread] checks fixed identity. CLAP explicitly permits this OS
        // thread to be the symbolic audio owner too; only request_flush has
        // the additional !audio-thread restriction.
        if self.shared.control.is_main_thread() {
            Ok(())
        } else {
            self.shared.services.wrong_thread.store(true, Ordering::Release);
            Err(HostError::Message(
                "CLAP main service called from a different role",
            ))
        }
    }

    fn due_timers(&self, now: Instant) -> Result<[Option<TimerId>; MAX_TIMERS], HostError> {
        let mut due = [None; MAX_TIMERS];
        let mut timers = self.timers.get();
        for (slot, timer) in timers.iter_mut().enumerate() {
            let Some(entry) = timer else { continue };
            if entry.next > now {
                continue;
            }
            due[slot] = Some(entry.id);
            // Do not run an unbounded catch-up loop. Skip elapsed periods while
            // retaining the timer's original cadence whenever representable.
            let elapsed = now.duration_since(entry.next).as_nanos();
            let periods = elapsed / entry.period.as_nanos() + 1;
            let advance = u32::try_from(periods).ok().and_then(|n| entry.period.checked_mul(n));
            let next = advance
                .and_then(|advance| entry.next.checked_add(advance))
                .or_else(|| now.checked_add(entry.period));
            entry.next = next.ok_or(HostError::Message(
                "CLAP periodic timer deadline overflowed",
            ))?;
        }
        self.timers.set(timers);
        Ok(due)
    }

    fn contains_timer(&self, id: TimerId) -> bool {
        self.timers.get().iter().flatten().any(|entry| entry.id == id)
    }

    pub(super) fn prepare_parameter_prefix(
        &self,
        source: std::sync::Arc<super::clap_parameter_source::ClapParameterSource>,
        output: &super::clap_output_events::ClapOutputEvents,
    ) -> Result<
        Option<super::clap_parameter_prefix::PreparedClapParameterPrefix>,
        crate::NativeAudioAuthorRefusal,
    > {
        super::clap_parameter_prefix::PreparedClapParameterPrefix::prepare(
            source,
            output,
            &self.parameter_controls,
        )
    }

    pub(super) fn acknowledge_prepared_parameter_prefix(
        &self,
        prefix: super::clap_parameter_prefix::PreparedClapParameterPrefix,
        output: &mut super::clap_output_events::ClapOutputEvents,
    ) -> Result<(), super::clap_parameter_prefix::PreparedClapParameterPrefix> {
        prefix.acknowledge(output, &self.parameter_controls)
    }

    /// Cold, unexposed construction only: the caller has admitted the complete
    /// post-restore snapshot and exact writable values under `observed`.
    /// Retire only rescan invalidations already satisfied by that snapshot;
    /// editor/state edits, outputs, loss and later notifications stay retained.
    pub(super) fn acknowledge_initial_parameter_rescans(
        &self,
        observed: super::clap_parameter_source::ClapParameterSourceSnapshot,
        output: &mut super::clap_output_events::ClapOutputEvents,
    ) -> bool {
        use super::clap_parameter_source::ClapParameterControlKind;
        if self.ensure_main().is_err()
            || self.shared.services.active.load(Ordering::Acquire)
            || self.shared.services.flush_requested.load(Ordering::Acquire)
            || observed.publishing_output
            || observed.incomplete != 0
            || observed.pending_output != 0
            || observed.pending_author_output != 0
            || observed.open_parameter_gestures != 0
            || self.shared.control.parameter_source.snapshot() != observed
        {
            return false;
        }
        let (records, count) = self.parameter_controls.snapshot();
        if count == 0 {
            return observed.pending_controls == 0;
        }
        if observed.pending_controls as usize != count {
            return false;
        }
        let Some(through) = records[..count].iter().try_fold(0_u64, |previous, record| {
            let record = record.as_ref()?;
            match record.kind {
                ClapParameterControlKind::Rescan { flags }
                    if flags & !15 == 0
                        && record.sequence > previous
                        && record.sequence <= observed.revision =>
                {
                    Some(record.sequence)
                }
                _ => None,
            }
        }) else {
            return false;
        };
        if through != observed.revision {
            return false;
        }
        // The existing atomic two-owner retirement checks exact identity and
        // prefixes. No native call or new dispatcher is introduced here.
        super::clap_output_events::acknowledge_parameter_prefix(
            output,
            &self.parameter_controls,
            None,
            0,
            Some(through),
            count,
        )
    }

    pub(super) fn has_timers(&self) -> bool {
        self.timers.get().iter().any(Option::is_some)
    }
}

impl<'a> MainThreadHandler<'a> for ClapHostMainThread<'a> {}

impl HostTimerImpl for ClapHostMainThread<'_> {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        self.ensure_main()?;
        if !self.shared.services.timer_available.load(Ordering::Acquire) {
            return Err(HostError::Message(
                "CLAP plugin timer support is unavailable",
            ));
        }
        let mut timers = self.timers.get();
        let slot = timers
            .iter()
            .position(Option::is_none)
            .ok_or(HostError::Message("CLAP timer capacity exhausted"))?;
        let raw = self.next_timer_id.get();
        let next_id = raw
            .checked_add(1)
            .filter(|id| *id != u32::MAX)
            .ok_or(HostError::Message("CLAP timer ID exhausted"))?;
        // CLAP explicitly permits a host to adjust very short periods.
        let period = Duration::from_millis(u64::from(period_ms.max(1)));
        let next = Instant::now()
            .checked_add(period)
            .ok_or(HostError::Message("CLAP timer deadline overflowed"))?;
        let id = TimerId(raw);
        timers[slot] = Some(RegisteredTimer { id, period, next });
        self.timers.set(timers);
        self.next_timer_id.set(next_id);
        Ok(id)
    }

    fn unregister_timer(&self, id: TimerId) -> Result<(), HostError> {
        self.ensure_main()?;
        let mut timers = self.timers.get();
        let slot = timers
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.id == id))
            .ok_or(HostError::Message("CLAP timer ID is not registered"))?;
        timers[slot] = None;
        self.timers.set(timers);
        Ok(())
    }
}

/// One bounded real main tick; timer callbacks run after handler borrows end.
/// Timers unregistered by an earlier callback in this batch are not called.
pub(super) fn service_main_tick(
    control: &ClapHostShared,
    instance: &mut PluginInstance<ClapHost>,
) -> Result<(), AudioProcessorHostError> {
    control.service_callbacks(instance)?;
    let due =
        instance
            .access_handler(|handler| handler.due_timers(Instant::now()))
            .map_err(|error| AudioProcessorHostError::WorkerFailed {
                operation: "CLAP main timer service",
                detail: error.to_string(),
            })?;
    let has_timers = instance.access_handler(|handler| handler.has_timers());
    let timer = instance.plugin_handle().get_extension::<PluginTimer>();
    if timer.is_none() && has_timers {
        return Err(AudioProcessorHostError::WorkerFailed {
            operation: "CLAP main timer service",
            detail: "plugin registered a timer without clap.timer-support".to_owned(),
        });
    }
    for id in due.into_iter().flatten() {
        if !instance.access_handler(|handler| handler.contains_timer(id)) {
            continue;
        }
        if let Some(timer) = &timer {
            // The previous timer callback may unregister another due timer.
            // Reborrow only after checking the actual main-handler slot above.
            let handle = instance.plugin_handle();
            timer.on_timer(&handle, id);
        }
    }
    Ok(())
}

impl HostParamsImplShared for ClapNativeHostShared {
    fn request_flush(&self) {
        use clack_extensions::thread_check::HostThreadCheckImpl;
        if self.control.is_audio_thread() {
            self.control.parameter_source.refuse(ClapParameterSourceFailure::WrongThread);
            return;
        }
        // One scheduling intent, not an author event or a request to create PCM.
        // The same exclusive DSP role claims it before process or idle flush.
        self.services.flush_requested.store(true, Ordering::Release);
    }
}

impl clack_extensions::note_ports::HostNotePortsRawImpl for ClapNativeHostShared {
    fn supported_dialects_raw(&self) -> u32 {
        use clack_extensions::thread_check::HostThreadCheckImpl;
        if !self.control.is_main_thread() {
            self.services
                .note_ports
                .fail(super::clap_note_input_contract::NotePortFailure::WrongThread);
            return 0;
        }
        if !self.control.note_input_enabled {
            return 0;
        }
        super::clap_note_input_contract::NATIVE_CLAP_DIALECT
    }

    fn rescan_raw(&self, flags: u32) {
        use clack_extensions::thread_check::HostThreadCheckImpl;
        self.services.note_ports.record(
            flags,
            self.control.is_main_thread(),
            self.control.note_input_enabled,
            self.services.active.load(Ordering::Acquire),
        );
    }
}

impl ClapHostMainThread<'_> {
    fn record_parameter_control(&self, kind: ClapParameterControlKind) {
        if self.ensure_main().is_err() {
            self.shared
                .control
                .parameter_source
                .refuse(ClapParameterSourceFailure::WrongThread);
            return;
        }
        self.parameter_controls.record(kind);
    }
}
impl HostParamsImplMainThread for ClapHostMainThread<'_> {
    fn rescan(&self, flags: ParamRescanFlags) {
        let raw = flags.bits();
        if self.ensure_main().is_err() {
            self.shared
                .control
                .parameter_source
                .refuse(ClapParameterSourceFailure::WrongThread);
            return;
        }
        if raw & !ParamRescanFlags::all().bits() != 0 {
            self.shared
                .control
                .parameter_source
                .refuse(ClapParameterSourceFailure::UnknownFlags);
        }
        if raw & ParamRescanFlags::ALL.bits() != 0
            && self.shared.services.active.load(Ordering::Acquire)
        {
            self.shared
                .control
                .parameter_source
                .refuse(ClapParameterSourceFailure::ActiveRescanAll);
        }
        // Retain ALL and unknown evidence too. The exact raw callback cannot be
        // acknowledged as complete by merely filtering its unsupported flags.
        self.record_parameter_control(ClapParameterControlKind::Rescan { flags: raw });
    }
    fn clear(&self, id: clack_host::prelude::ClapId, flags: ParamClearFlags) {
        if self.ensure_main().is_err() {
            self.shared
                .control
                .parameter_source
                .refuse(ClapParameterSourceFailure::WrongThread);
            return;
        }
        let raw = flags.bits();
        if raw & !ParamClearFlags::all().bits() != 0 {
            self.shared
                .control
                .parameter_source
                .refuse(ClapParameterSourceFailure::UnknownFlags);
        }
        // A removed/reused native ID is explicitly legal; do not require it to
        // be in the new parameter descriptor list before retaining its clear.
        self.record_parameter_control(ClapParameterControlKind::Clear {
            parameter: id.get(),
            flags: raw,
        });
    }
}
impl HostStateImpl for ClapHostMainThread<'_> {
    fn mark_dirty(&self) {
        self.record_parameter_control(ClapParameterControlKind::StateDirty);
    }
}
