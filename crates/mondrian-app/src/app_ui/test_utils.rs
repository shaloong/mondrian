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

/// Build one native gain key through the localized shared route curve projection.
pub(crate) fn native_sidechain_gain_key_action(
    state: &crate::app::AppState,
    processor_id: mondrian_core::AudioProcessorInstanceId,
    route_id: mondrian_core::AudioRouteId,
    x: f32,
    value_db: f64,
) -> mondrian_editor_state::Action {
    use super::audio_automation::{prepare_audio_curve_edit, AudioAutomationTimeContext};
    use super::localization::{AppUiLocale, Localizer};
    let sequence = state.active_sequence().expect("native Sequence");
    let keyframe_id = mondrian_core::KeyframeId::new();
    let mut action = None;
    for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
        let localizer = Localizer::new(locale).expect("gain locale");
        let routes = super::audio_sidechain::retained_routes(
            sequence,
            processor_id,
            &localizer,
            AudioAutomationTimeContext::at_sequence_time(sequence, sequence.playhead),
        );
        let model = routes
            .iter()
            .find(|route| route.route_id == route_id)
            .expect("retained native route")
            .gain_automation
            .as_ref()
            .expect("shared gain curve");
        assert_eq!(
            model.viewport.range.start,
            mondrian_core::TimelineTime::ZERO
        );
        assert_eq!(
            model.viewport.range.duration,
            mondrian_core::TimelineTime::ONE
        );
        action = Some(
            prepare_audio_curve_edit(
                model,
                mondrian_ui_widgets::CurveEdit::Insert {
                    keyframe_id,
                    index: 0,
                    point: mondrian_ui_widgets::CurvePoint { x, y: value_db },
                },
            )
            .expect("real gain curve author action")
            .action,
        );
    }
    action.expect("localized curve action")
}

/// Choose Linear through the shared curve selection context-menu action.
pub(crate) fn native_sidechain_gain_linear_action(
    state: &crate::app::AppState,
    processor_id: mondrian_core::AudioProcessorInstanceId,
    route_id: mondrian_core::AudioRouteId,
) -> mondrian_editor_state::Action {
    let sequence = state.active_sequence().expect("Sequence");
    let localizer = super::localization::Localizer::new(super::localization::AppUiLocale::EnUs)
        .expect("catalog");
    let routes = super::audio_sidechain::retained_routes(
        sequence,
        processor_id,
        &localizer,
        super::audio_automation::AudioAutomationTimeContext::at_sequence_time(
            sequence,
            sequence.playhead,
        ),
    );
    let model = routes
        .iter()
        .find(|route| route.route_id == route_id)
        .expect("route")
        .gain_automation
        .as_ref()
        .expect("curve");
    let ids = model.source_curve.keyframes.iter().map(|key| key.id).collect::<Vec<_>>();
    super::audio_automation::audio_automation_interpolations_action(
        model,
        &ids,
        mondrian_core::InterpolationType::Linear,
    )
    .expect("Linear menu action")
}

/// Check the real shared Rack and return an observed source creation or rewire action.
pub(crate) fn native_sidechain_source_action(
    state: &crate::app::AppState,
    query: crate::app::product_action::AudioProcessorInputQueryPayload,
    source: mondrian_timeline::audio::AudioRouteSource,
    bus_key: &str,
    retained_route: Option<mondrian_core::AudioRouteId>,
) -> mondrian_editor_state::Action {
    use super::audio_automation::AudioAutomationTimeContext;
    use super::audio_processor_rack::{bind_processor_input_queries, project_audio_processor_rack};
    use super::localization::{AppUiLocale, Localizer};
    use crate::app::audio_input_observation::AudioInputObservation;
    use mondrian_timeline::AudioProcessorRackAddress;
    let (address, occurrence) = match query.target {
        mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack } => (
            AudioProcessorRackAddress::ChannelStrip { owner, rack },
            None,
        ),
        mondrian_audio::AudioProcessorInputInspectionTarget::ProcessingScope {
            scope_id,
            occurrence,
        } => (
            AudioProcessorRackAddress::ProcessingScope { scope_id },
            Some(occurrence),
        ),
    };
    let AudioInputObservation::Ready(observed) = state.audio_processor_input_observation(query)
    else {
        panic!("Native UI projection requires completed real inspection");
    };
    let mut connection = None;
    for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
        let localizer = Localizer::new(locale).expect("native Rack locale");
        let current = state.active_sequence().expect("native author");
        let mut time_context =
            AudioAutomationTimeContext::at_sequence_time(current, current.playhead);
        time_context.scope_occurrence = occurrence;
        let mut racks = vec![project_audio_processor_rack(
            current,
            address,
            &localizer,
            time_context,
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
        connection = Some(if let Some(route_id) = retained_route {
            let route = racks[0].processors[0]
                .sidechain_routes
                .iter()
                .find(|route| route.route_id == route_id)
                .expect("retained native route");
            let option = route
                .rewire_options
                .iter()
                .find(|option| option.payload.connection.source == source)
                .expect("causal native replacement admitted by shared Rack");
            assert_eq!(option.payload.connection.binding, observed.binding);
            assert_eq!(option.payload.connection.bus_key, bus_key);
            super::audio_sidechain::rewire_action(option)
        } else {
            super::audio_sidechain::connection_action(option)
        });
    }

    connection.expect("shared named connection action")
}

/// Select the exact manual query payload projected by the real visible Rack.
/// Keep private UI projection modules behind this existing test-only seam.
#[cfg(windows)]
pub(crate) fn native_parameter_query_payload(
    state: &crate::app::AppState,
    address: mondrian_timeline::AudioProcessorRackAddress,
    processor_id: mondrian_core::AudioProcessorInstanceId,
    parameter: &mondrian_core::ParameterId,
) -> Option<crate::app::product_action::AudioNativeParameterQueryPayload> {
    native_parameter_row(state, address, processor_id, parameter)?.native_query
}

/// Select the existing Adopt action from the same actual visible Rack row.
#[cfg(windows)]
pub(crate) fn native_parameter_adoption_payload(
    state: &crate::app::AppState,
    address: mondrian_timeline::AudioProcessorRackAddress,
    processor_id: mondrian_core::AudioProcessorInstanceId,
    parameter: &mondrian_core::ParameterId,
) -> Option<crate::app::parameter_gesture::AdoptDisplayedNativeParameterEdit> {
    native_parameter_row(state, address, processor_id, parameter)?.native_adoption
}

#[cfg(windows)]
fn native_parameter_row(
    state: &crate::app::AppState,
    address: mondrian_timeline::AudioProcessorRackAddress,
    processor_id: mondrian_core::AudioProcessorInstanceId,
    parameter: &mondrian_core::ParameterId,
) -> Option<super::audio_processor_rack::AudioProcessorParameterModel> {
    use super::audio_automation::AudioAutomationTimeContext;
    use super::audio_processor_rack::{
        bind_audio_scalar_author_lease, project_audio_processor_rack,
    };
    use super::localization::{AppUiLocale, Localizer};
    let sequence = state.active_sequence()?;
    let localizer = Localizer::new(AppUiLocale::EnUs).expect("native Rack locale");
    let mut rack = project_audio_processor_rack(
        sequence,
        address,
        &localizer,
        AudioAutomationTimeContext::at_sequence_time(sequence, mondrian_core::TimelineTime::ZERO),
    );
    bind_audio_scalar_author_lease(std::slice::from_mut(&mut rack), state);
    rack.processors
        .into_iter()
        .find(|processor| processor.processor_id == processor_id)?
        .parameters
        .into_iter()
        .find(|row| &row.parameter_id == parameter)
}

/// Native primary modifier for actual text/NumberInput Host fixture events.
/// Explicit curve, timeline and classifier shortcut vectors keep their declared modifiers.
pub(crate) fn primary_text_modifiers() -> Modifiers {
    Modifiers {
        ctrl: !cfg!(target_os = "macos"),
        meta: cfg!(target_os = "macos"),
        ..Modifiers::none()
    }
}
