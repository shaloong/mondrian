//! 面板注册表
//!
//! 动态创建 Panel 实例的工厂系统。
//! 所有 Panel 类型通过注册表注册，运行时按需创建。

use std::collections::HashMap;
use std::sync::Arc;

use crate::panel::{Panel, PanelContext, PanelKind};

/// Panel 工厂函数类型
type PanelFactory = Arc<dyn Fn(PanelContext) -> Box<dyn Panel> + Send + Sync>;

/// 面板注册表 —— 按 PanelKind 注册工厂函数
///
/// ## 使用示例
///
/// ```ignore
/// let mut registry = PanelRegistry::new();
/// registry.register(PanelKind::Timeline, Arc::new(|ctx| {
///     Box::new(TimelinePanel::new(ctx))
/// }));
/// let panel = registry.create(PanelKind::Timeline, context);
/// ```
pub struct PanelRegistry {
    factories: HashMap<PanelKind, PanelFactory>,
}

impl PanelRegistry {
    pub fn new() -> Self {
        Self { factories: HashMap::new() }
    }

    /// 注册一个 Panel 工厂
    pub fn register(&mut self, kind: PanelKind, factory: PanelFactory) {
        self.factories.insert(kind, factory);
    }

    /// 创建指定类型的 Panel 实例
    ///
    /// 如果该类型未注册，返回 `None`。
    pub fn create(&self, kind: PanelKind, context: PanelContext) -> Option<Box<dyn Panel>> {
        self.factories.get(&kind).map(|factory| factory(context))
    }

    /// 检查某个 Panel 类型是否已注册
    pub fn is_registered(&self, kind: PanelKind) -> bool {
        self.factories.contains_key(&kind)
    }

    /// 已注册的所有 Panel 类型
    pub fn registered_kinds(&self) -> Vec<PanelKind> {
        self.factories.keys().copied().collect()
    }
}

impl Default for PanelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct TestPanel {
        kind: PanelKind,
    }

    impl Panel for TestPanel {
        fn kind(&self) -> PanelKind {
            self.kind
        }

        fn title(&self) -> std::borrow::Cow<'static, str> {
            "Test".into()
        }

        fn build_widget_tree(&mut self) -> Box<dyn mondrian_ui_core::Widget> {
            Box::new(mondrian_ui_core::widgets::Spacer::new(10.0, 10.0))
        }
    }

    fn test_context() -> PanelContext {
        use mondrian_core::events::EventBus;
        PanelContext {
            event_bus: EventBus::new(),
        }
    }

    #[test]
    fn registry_new_is_empty() {
        let registry = PanelRegistry::new();
        assert!(registry.registered_kinds().is_empty());
    }

    #[test]
    fn registry_register_adds_factory() {
        let mut registry = PanelRegistry::new();
        registry.register(PanelKind::Timeline, Arc::new(|_ctx| {
            Box::new(TestPanel { kind: PanelKind::Timeline })
        }));
        assert!(registry.is_registered(PanelKind::Timeline));
        assert!(!registry.is_registered(PanelKind::Viewer));
    }

    #[test]
    fn registry_create_returns_panel() {
        let mut registry = PanelRegistry::new();
        registry.register(PanelKind::Console, Arc::new(|_ctx| {
            Box::new(TestPanel { kind: PanelKind::Console })
        }));

        let panel = registry.create(PanelKind::Console, test_context()).unwrap();
        assert_eq!(panel.kind(), PanelKind::Console);
    }

    #[test]
    fn registry_create_unknown_returns_none() {
        let registry = PanelRegistry::new();
        assert!(registry.create(PanelKind::Viewer, test_context()).is_none());
    }

    #[test]
    fn registry_register_overwrites_existing() {
        let mut registry = PanelRegistry::new();
        registry.register(PanelKind::Timeline, Arc::new(|_ctx| {
            Box::new(TestPanel { kind: PanelKind::Timeline })
        }));
        registry.register(PanelKind::Timeline, Arc::new(|_ctx| {
            Box::new(TestPanel { kind: PanelKind::Viewer }) // wrong kind on purpose
        }));

        // Second registration overwrites
        let panel = registry.create(PanelKind::Timeline, test_context()).unwrap();
        assert_eq!(panel.kind(), PanelKind::Viewer); // from second factory
    }

    #[test]
    fn registry_registered_kinds_returns_all() {
        let mut registry = PanelRegistry::new();
        registry.register(PanelKind::Viewer, Arc::new(|_ctx| {
            Box::new(TestPanel { kind: PanelKind::Viewer })
        }));
        registry.register(PanelKind::Timeline, Arc::new(|_ctx| {
            Box::new(TestPanel { kind: PanelKind::Timeline })
        }));

        let kinds = registry.registered_kinds();
        assert_eq!(kinds.len(), 2);
        assert!(kinds.contains(&PanelKind::Viewer));
        assert!(kinds.contains(&PanelKind::Timeline));
    }

    #[test]
    fn registry_default_is_empty() {
        let registry = PanelRegistry::default();
        assert!(registry.registered_kinds().is_empty());
    }
}
