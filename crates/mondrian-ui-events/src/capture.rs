//! Pointer capture state for event routing.
//!
//! Widgets request pointer capture through `EventRequests`; the router owns the
//! resulting capture state so routing rules stay centralized and testable.

use mondrian_ui_core::types::WidgetId;
use mondrian_ui_core::widget::PointerCaptureRequest;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PointerCaptureState {
    owner: Option<WidgetId>,
}

impl PointerCaptureState {
    pub(crate) fn owner(self) -> Option<WidgetId> {
        self.owner
    }

    pub(crate) fn is_idle(self) -> bool {
        self.owner.is_none()
    }

    pub(crate) fn set_owner(&mut self, owner: Option<WidgetId>) {
        self.owner = owner;
    }

    pub(crate) fn clear(&mut self) {
        self.owner = None;
    }

    pub(crate) fn apply_request(&mut self, request: PointerCaptureRequest) {
        match request {
            PointerCaptureRequest::Capture(id) => self.owner = Some(id),
            PointerCaptureRequest::Release(id) if self.owner == Some(id) => {
                self.owner = None;
            }
            PointerCaptureRequest::Clear => self.owner = None,
            PointerCaptureRequest::Release(_) => {}
        }
    }

    pub(crate) fn clear_if_stale(&mut self, mut exists: impl FnMut(WidgetId) -> bool) -> bool {
        if self.owner.is_some_and(|id| !exists(id)) {
            self.owner = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_request_sets_owner() {
        let mut capture = PointerCaptureState::default();
        let widget = WidgetId::new();

        capture.apply_request(PointerCaptureRequest::Capture(widget));

        assert_eq!(capture.owner(), Some(widget));
        assert!(!capture.is_idle());
    }

    #[test]
    fn matching_release_clears_owner() {
        let mut capture = PointerCaptureState::default();
        let widget = WidgetId::new();
        capture.apply_request(PointerCaptureRequest::Capture(widget));

        capture.apply_request(PointerCaptureRequest::Release(widget));

        assert_eq!(capture.owner(), None);
        assert!(capture.is_idle());
    }

    #[test]
    fn non_owner_release_is_ignored() {
        let mut capture = PointerCaptureState::default();
        let owner = WidgetId::new();
        capture.apply_request(PointerCaptureRequest::Capture(owner));

        capture.apply_request(PointerCaptureRequest::Release(WidgetId::new()));

        assert_eq!(capture.owner(), Some(owner));
    }

    #[test]
    fn clear_request_always_clears_owner() {
        let mut capture = PointerCaptureState::default();
        capture.apply_request(PointerCaptureRequest::Capture(WidgetId::new()));

        capture.apply_request(PointerCaptureRequest::Clear);

        assert_eq!(capture.owner(), None);
    }

    #[test]
    fn clear_if_stale_reports_whether_owner_was_removed() {
        let mut capture = PointerCaptureState::default();
        let owner = WidgetId::new();
        capture.set_owner(Some(owner));

        assert!(!capture.clear_if_stale(|id| id == owner));
        assert_eq!(capture.owner(), Some(owner));

        assert!(capture.clear_if_stale(|_| false));
        assert_eq!(capture.owner(), None);
    }
}
