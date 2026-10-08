//! Test helpers for app UI shell widgets.

use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_platform::NoopPlatformService;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, EventRequests};
use mondrian_ui_core::{
    FocusManager, ShortcutBinding, ShortcutContext, ShortcutManager, ShortcutScope, TooltipManager,
    TooltipState,
};
use mondrian_ui_theme::{set_theme_preset, ThemePreset};
use std::sync::{Mutex, MutexGuard};

static THEME_TEST_LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct ThemeTestGuard {
    _lock: MutexGuard<'static, ()>,
}

impl Drop for ThemeTestGuard {
    fn drop(&mut self) {
        set_theme_preset(ThemePreset::Dark);
    }
}

pub(crate) fn theme_test_guard() -> ThemeTestGuard {
    let lock = THEME_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    set_theme_preset(ThemePreset::Dark);
    ThemeTestGuard { _lock: lock }
}

pub(crate) struct DummyFocus;

impl FocusManager for DummyFocus {
    fn focused_widget(&self) -> Option<WidgetId> {
        None
    }

    fn focused_panel(&self) -> Option<PanelKind> {
        None
    }

    fn request_focus(&mut self, _widget: WidgetId) {}

    fn release_focus(&mut self, _widget: WidgetId) {}

    fn clear_focus(&mut self) {}
}

pub(crate) struct DummyShortcut;

impl ShortcutManager for DummyShortcut {
    fn register(&mut self, _scope: ShortcutScope, _binding: ShortcutBinding, _action: Action) {}

    fn unregister(&mut self, _scope: ShortcutScope, _binding: &ShortcutBinding) {}

    fn resolve(
        &self,
        _key: KeyCode,
        _modifiers: Modifiers,
        _context: ShortcutContext,
    ) -> Option<Action> {
        None
    }

    fn clear_scope(&mut self, _scope: ShortcutScope) {}

    fn clear_all(&mut self) {}
}

pub(crate) struct DummyTooltip;

impl TooltipManager for DummyTooltip {
    fn show(&mut self, _text: String, _position: Point) {}

    fn hide(&mut self) {}

    fn current(&self) -> Option<&TooltipState> {
        None
    }

    fn update(&mut self, _delta_ms: u64) {}
}

pub(crate) fn event_ctx<'a>(
    focus: &'a mut DummyFocus,
    shortcut: &'a mut DummyShortcut,
    tooltip: &'a mut DummyTooltip,
    requests: &'a mut EventRequests,
    dispatch: &'a dyn Fn(Action),
) -> EventContext<'a> {
    EventContext {
        focus,
        shortcut,
        tooltip,
        dispatch,
        platform: &NoopPlatformService,
        requests,
    }
}

/// Platform-localized shortcut hint text for tests that assert on the
/// user-facing hint (Ctrl on other platforms, symbol glyphs on macOS).
pub(crate) fn expected_shortcut(hint: &str) -> String {
    if cfg!(target_os = "macos") {
        hint.replace("Ctrl", "\u{2303}")
            .replace("Alt", "\u{2325}")
            .replace("Shift", "\u{21e7}")
            .replace("Esc", "\u{238b}")
            .replace("Tab", "\u{21e5}")
            .replace("Enter", "\u{23ce}")
            .replace("Backspace", "\u{232b}")
            .replace("Left", "\u{25c0}")
            .replace("Right", "\u{25b6}")
            .replace("Up", "\u{25b2}")
            .replace("Down", "\u{25bc}")
            .replace('+', "")
    } else {
        hint.to_owned()
    }
}

/// Check the real shared Rack presentation and return its observed connection action.
pub(crate) fn native_sidechain_connection_action(
    state: &crate::app::AppState,
    query: crate::app::product_action::AudioProcessorInputQueryPayload,
    source: mondrian_timeline::audio::AudioRouteSource,
    bus_key: &str,
) -> mondrian_editor_state::Action {
    use super::audio_automation::AudioAutomationTimeContext;
    use super::audio_processor_rack::{bind_processor_input_queries, project_audio_processor_rack};
    use super::localization::{AppUiLocale, Localizer};
    use crate::app::audio_input_observation::AudioInputObservation;
    use mondrian_timeline::AudioProcessorRackAddress;
    let mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack } =
        query.target
    else {
        panic!("Native history fixture uses a channel strip");
    };
    let AudioInputObservation::Ready(observed) = state.audio_processor_input_observation(query)
    else {
        panic!("Native UI projection requires completed real inspection");
    };
    let mut connection = None;
    for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
        let localizer = Localizer::new(locale).expect("native Rack locale");
        let current = state.active_sequence().expect("native author");
        let mut racks = vec![project_audio_processor_rack(
            current,
            AudioProcessorRackAddress::ChannelStrip { owner, rack },
            &localizer,
            AudioAutomationTimeContext::at_sequence_time(current, current.playhead),
        )];
        bind_processor_input_queries(&mut racks, state, &localizer, None);
        let input = racks[0].processors[0]
            .sidechain_inputs
            .iter()
            .find(|input| input.options.iter().any(|option| option.payload.bus_key == bus_key))
            .expect("real named input control");
        assert_eq!(
            input.label,
            localizer.format_text("audio-sidechain-input", "identity", "压缩探测 🙂")
        );
        let option = input
            .options
            .iter()
            .find(|option| option.payload.source == source)
            .expect("causal native source admitted by shared Rack");
        assert_eq!(option.payload.binding, observed.binding);
        connection = Some(super::audio_sidechain::connection_action(option));
    }

    connection.expect("shared named connection action")
}
