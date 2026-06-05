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
