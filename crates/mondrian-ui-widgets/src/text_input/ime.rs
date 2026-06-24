use mondrian_ui_core::types::{KeyCode, Modifiers, Rect};
use mondrian_ui_core::widget::EventRequests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ImeKeyDisposition {
    ClearComposition,
    ConsumeDuringComposition,
    RouteNormally,
}

pub(super) fn request_enabled_ime(requests: &mut EventRequests, focused: bool, cursor_area: Rect) {
    if focused {
        requests.set_ime_enabled(true, Some(cursor_area));
    }
}

pub(super) fn request_disabled_ime(requests: &mut EventRequests) {
    requests.set_ime_enabled(false, None);
}

pub(super) fn classify_ime_key(
    focused: bool,
    composition_active: bool,
    key: KeyCode,
    modifiers: Modifiers,
) -> ImeKeyDisposition {
    if !focused || !composition_active {
        return ImeKeyDisposition::RouteNormally;
    }

    if key == KeyCode::Escape && modifiers == Modifiers::none() {
        ImeKeyDisposition::ClearComposition
    } else {
        ImeKeyDisposition::ConsumeDuringComposition
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_request_only_enables_when_focused() {
        let caret = Rect::new(1.0, 2.0, 3.0, 4.0);
        let mut requests = EventRequests::default();

        request_enabled_ime(&mut requests, false, caret);
        assert!(requests.ime.is_none());

        request_enabled_ime(&mut requests, true, caret);
        let request = requests.ime.expect("focused text input should request IME");
        assert!(request.enabled);
        assert_eq!(request.cursor_area, Some(caret));
    }

    #[test]
    fn disabled_ime_request_clears_cursor_area() {
        let mut requests = EventRequests::default();
        request_disabled_ime(&mut requests);

        let request = requests.ime.expect("disable request should be recorded");
        assert!(!request.enabled);
        assert_eq!(request.cursor_area, None);
    }

    #[test]
    fn active_composition_escape_without_modifiers_clears_composition() {
        assert_eq!(
            classify_ime_key(true, true, KeyCode::Escape, Modifiers::none()),
            ImeKeyDisposition::ClearComposition
        );
    }

    #[test]
    fn active_composition_consumes_non_escape_or_modified_escape() {
        assert_eq!(
            classify_ime_key(true, true, KeyCode::A, Modifiers::none()),
            ImeKeyDisposition::ConsumeDuringComposition
        );
        assert_eq!(
            classify_ime_key(true, true, KeyCode::Escape, Modifiers::ctrl()),
            ImeKeyDisposition::ConsumeDuringComposition
        );
    }

    #[test]
    fn inactive_or_unfocused_composition_routes_normally() {
        assert_eq!(
            classify_ime_key(false, true, KeyCode::Escape, Modifiers::none()),
            ImeKeyDisposition::RouteNormally
        );
        assert_eq!(
            classify_ime_key(true, false, KeyCode::A, Modifiers::none()),
            ImeKeyDisposition::RouteNormally
        );
    }
}
