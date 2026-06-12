//! Tooltip 管理器实现
//!
//! 管理 hover 计时器、延迟显示、位置跟踪。

use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::Point;

/// Tooltip 管理器的具体实现
///
/// ## 行为
///
/// * `show()` 记录请求但不立即显示，启动延迟计时器
/// * `hide()` 清除请求和当前 tooltip
/// * `update(delta_ms)` 推进计时器，达到延迟后显示
/// * 同一时间只跟踪一个 tooltip 请求
pub struct TooltipManagerImpl {
    /// 当前显示的 tooltip（已过延迟期）
    state: Option<TooltipState>,
    /// 等待显示的 tooltip 文本
    pending_text: Option<String>,
    /// 等待显示的位置
    pending_position: Point,
    /// 已 hover 的累计毫秒数
    hover_ms: u64,
    /// 显示前的延迟（毫秒）
    delay_ms: u64,
}

impl TooltipManagerImpl {
    pub fn new(delay_ms: u64) -> Self {
        Self {
            state: None,
            pending_text: None,
            pending_position: Point::ZERO,
            hover_ms: 0,
            delay_ms,
        }
    }
}

impl TooltipManager for TooltipManagerImpl {
    fn show(&mut self, text: String, position: Point) {
        if self.pending_text.as_ref() == Some(&text) {
            return;
        }
        if self.state.as_ref().is_some_and(|state| state.text == text) {
            return;
        }
        self.pending_text = Some(text);
        self.pending_position = position;
        self.hover_ms = 0;
        self.state = None;
    }

    fn hide(&mut self) {
        self.pending_text = None;
        self.hover_ms = 0;
        self.state = None;
    }

    fn current(&self) -> Option<&TooltipState> {
        self.state.as_ref()
    }

    fn update(&mut self, delta_ms: u64) {
        if self.pending_text.is_none() {
            return;
        }

        self.hover_ms += delta_ms;
        if self.hover_ms >= self.delay_ms {
            self.state = Some(TooltipState {
                text: self.pending_text.take().unwrap_or_default(),
                position: self.pending_position,
                visible: true,
            });
        }
    }

    fn next_update_in_ms(&self) -> Option<u64> {
        self.pending_text.as_ref().map(|_| self.delay_ms.saturating_sub(self.hover_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_not_visible_before_delay() {
        let mut mgr = TooltipManagerImpl::new(500);
        mgr.show("hello".into(), Point::new(10.0, 20.0));
        mgr.update(200);
        assert!(mgr.current().is_none());
    }

    #[test]
    fn tooltip_visible_after_delay() {
        let mut mgr = TooltipManagerImpl::new(500);
        mgr.show("hello".into(), Point::new(10.0, 20.0));
        mgr.update(500);
        let state = mgr.current().unwrap();
        assert!(state.visible);
        assert_eq!(state.text, "hello");
    }

    #[test]
    fn tooltip_hide_clears_pending() {
        let mut mgr = TooltipManagerImpl::new(500);
        mgr.show("hello".into(), Point::ZERO);
        mgr.update(200);
        mgr.hide();
        mgr.update(400);
        assert!(mgr.current().is_none());
    }

    #[test]
    fn tooltip_second_show_resets_timer() {
        let mut mgr = TooltipManagerImpl::new(500);
        mgr.show("first".into(), Point::ZERO);
        mgr.update(400);
        // Second show resets the timer
        mgr.show("second".into(), Point::new(5.0, 5.0));
        mgr.update(200);
        assert!(mgr.current().is_none()); // not enough time since second show
        mgr.update(400);
        let state = mgr.current().unwrap();
        assert_eq!(state.text, "second");
    }

    #[test]
    fn tooltip_same_show_preserves_timer_and_position() {
        let mut mgr = TooltipManagerImpl::new(500);
        mgr.show("same".into(), Point::new(1.0, 1.0));
        mgr.update(300);
        mgr.show("same".into(), Point::new(8.0, 9.0));
        mgr.update(200);

        let state = mgr.current().unwrap();
        assert_eq!(state.text, "same");
        assert_eq!(state.position, Point::new(1.0, 1.0));
    }

    #[test]
    fn tooltip_same_visible_show_preserves_current_position() {
        let mut mgr = TooltipManagerImpl::new(0);
        mgr.show("same".into(), Point::new(1.0, 1.0));
        mgr.update(0);

        mgr.show("same".into(), Point::new(3.0, 4.0));

        let state = mgr.current().unwrap();
        assert_eq!(state.position, Point::new(1.0, 1.0));
    }

    #[test]
    fn tooltip_update_without_pending_is_noop() {
        let mut mgr = TooltipManagerImpl::new(500);
        mgr.update(1000);
        assert!(mgr.current().is_none());
    }

    #[test]
    fn tooltip_reports_next_update_while_pending() {
        let mut mgr = TooltipManagerImpl::new(500);
        assert_eq!(mgr.next_update_in_ms(), None);

        mgr.show("hello".into(), Point::ZERO);
        assert_eq!(mgr.next_update_in_ms(), Some(500));

        mgr.update(125);
        assert_eq!(mgr.next_update_in_ms(), Some(375));

        mgr.update(375);
        assert_eq!(mgr.next_update_in_ms(), None);
        assert!(mgr.current().is_some());
    }
}
