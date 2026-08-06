//! ShortcutManager 实现
//!
//! 支持 Global / Workspace / Panel / Widget 四级作用域，
//! 高优先级的绑定覆盖低优先级。

use std::collections::HashMap;

use mondrian_editor_state::Action;
use mondrian_ui_core::shortcut::{
    ShortcutBinding, ShortcutContext, ShortcutManager, ShortcutScope,
};
use mondrian_ui_core::types::{KeyCode, Modifiers};

/// ShortcutManager 的具体实现
///
/// 按作用域分组存储绑定。`resolve()` 按优先级从高到低搜索：
/// Widget > Panel > Workspace > Global
#[derive(Debug, Default)]
pub struct ShortcutManagerImpl {
    /// 每个作用域下的绑定列表
    bindings: HashMap<ShortcutScope, Vec<(ShortcutBinding, Action)>>,
}

impl ShortcutManagerImpl {
    pub fn new() -> Self {
        Self { bindings: HashMap::new() }
    }

    /// 注册全局快捷键的便捷方法
    pub fn register_global(&mut self, binding: ShortcutBinding, action: Action) {
        self.register(ShortcutScope::Global, binding, action);
    }
}

impl ShortcutManager for ShortcutManagerImpl {
    fn register(&mut self, scope: ShortcutScope, binding: ShortcutBinding, action: Action) {
        let list = self.bindings.entry(scope).or_default();
        list.retain(|(registered, _)| registered != &binding);
        list.push((binding, action));
    }

    fn unregister(&mut self, scope: ShortcutScope, binding: &ShortcutBinding) {
        if let Some(list) = self.bindings.get_mut(&scope) {
            list.retain(|(b, _)| b != binding);
        }
    }

    fn resolve(
        &self,
        key: KeyCode,
        modifiers: Modifiers,
        context: ShortcutContext,
    ) -> Option<Action> {
        if let Some(widget) = context.widget
            && let Some(action) =
                self.resolve_in_scope(ShortcutScope::Widget(widget), key, modifiers)
        {
            return Some(action);
        }
        if let Some(panel) = context.panel
            && let Some(action) = self.resolve_in_scope(ShortcutScope::Panel(panel), key, modifiers)
        {
            return Some(action);
        }
        self.resolve_in_scope(ShortcutScope::Workspace, key, modifiers)
            .or_else(|| self.resolve_in_scope(ShortcutScope::Global, key, modifiers))
    }

    fn clear_scope(&mut self, scope: ShortcutScope) {
        self.bindings.remove(&scope);
    }

    fn clear_all(&mut self) {
        self.bindings.clear();
    }
}

impl ShortcutManagerImpl {
    fn resolve_in_scope(
        &self,
        scope: ShortcutScope,
        key: KeyCode,
        modifiers: Modifiers,
    ) -> Option<Action> {
        self.bindings.get(&scope).and_then(|list| {
            list.iter()
                .find(|(binding, _)| binding.key == key && binding.modifiers == modifiers)
                .map(|(_, action)| action.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_editor_state::state::PanelKind;

    #[test]
    fn register_and_resolve() {
        let mut mgr = ShortcutManagerImpl::new();
        mgr.register_global(ShortcutBinding::ctrl(KeyCode::S), Action::SaveProject);

        let found = mgr.resolve(KeyCode::S, Modifiers::ctrl(), ShortcutContext::default());
        assert_eq!(found, Some(Action::SaveProject));
    }

    #[test]
    fn resolve_returns_none_for_unknown() {
        let mgr = ShortcutManagerImpl::new();
        assert_eq!(
            mgr.resolve(KeyCode::X, Modifiers::none(), ShortcutContext::default()),
            None
        );
    }

    #[test]
    fn unregister_removes_binding() {
        let mut mgr = ShortcutManagerImpl::new();
        let binding = ShortcutBinding::ctrl(KeyCode::Z);
        mgr.register_global(binding.clone(), Action::Undo);
        mgr.unregister(ShortcutScope::Global, &binding);
        assert_eq!(
            mgr.resolve(KeyCode::Z, Modifiers::ctrl(), ShortcutContext::default()),
            None
        );
    }

    #[test]
    fn clear_scope_removes_all_in_scope() {
        let mut mgr = ShortcutManagerImpl::new();
        mgr.register_global(ShortcutBinding::ctrl(KeyCode::S), Action::SaveProject);
        mgr.register_global(ShortcutBinding::ctrl(KeyCode::Z), Action::Undo);
        mgr.clear_scope(ShortcutScope::Global);
        assert_eq!(
            mgr.resolve(KeyCode::S, Modifiers::ctrl(), ShortcutContext::default()),
            None
        );
        assert_eq!(
            mgr.resolve(KeyCode::Z, Modifiers::ctrl(), ShortcutContext::default()),
            None
        );
    }

    #[test]
    fn clear_all_removes_everything() {
        let mut mgr = ShortcutManagerImpl::new();
        mgr.register(
            ShortcutScope::Global,
            ShortcutBinding::ctrl(KeyCode::S),
            Action::SaveProject,
        );
        mgr.register(
            ShortcutScope::Panel(PanelKind::Timeline),
            ShortcutBinding::key_only(KeyCode::Delete),
            Action::DeleteSelection,
        );
        mgr.clear_all();
        assert!(mgr.resolve(KeyCode::S, Modifiers::ctrl(), ShortcutContext::default()).is_none());
        assert!(mgr
            .resolve(
                KeyCode::Delete,
                Modifiers::none(),
                ShortcutContext::default()
            )
            .is_none());
    }

    #[test]
    fn resolve_uses_focused_scope_priority_deterministically() {
        let mut mgr = ShortcutManagerImpl::new();
        let widget = mondrian_ui_core::types::WidgetId::new();
        mgr.register_global(ShortcutBinding::ctrl(KeyCode::S), Action::SaveProject);
        mgr.register(
            ShortcutScope::Workspace,
            ShortcutBinding::ctrl(KeyCode::S),
            Action::SaveProjectAs("workspace.mdp".into()),
        );
        mgr.register(
            ShortcutScope::Panel(PanelKind::Timeline),
            ShortcutBinding::ctrl(KeyCode::S),
            Action::SplitClipAtPlayhead,
        );
        mgr.register(
            ShortcutScope::Widget(widget),
            ShortcutBinding::ctrl(KeyCode::S),
            Action::Pause,
        );

        assert_eq!(
            mgr.resolve(
                KeyCode::S,
                Modifiers::ctrl(),
                ShortcutContext::new(Some(widget), Some(PanelKind::Timeline)),
            ),
            Some(Action::Pause)
        );
        assert_eq!(
            mgr.resolve(
                KeyCode::S,
                Modifiers::ctrl(),
                ShortcutContext::new(None, Some(PanelKind::Timeline)),
            ),
            Some(Action::SplitClipAtPlayhead)
        );
        assert_eq!(
            mgr.resolve(KeyCode::S, Modifiers::ctrl(), ShortcutContext::default()),
            Some(Action::SaveProjectAs("workspace.mdp".into()))
        );
    }

    #[test]
    fn register_replaces_existing_binding_in_same_scope() {
        let mut mgr = ShortcutManagerImpl::new();
        let binding = ShortcutBinding::ctrl(KeyCode::S);
        mgr.register_global(binding.clone(), Action::SaveProject);
        mgr.register_global(binding, Action::SaveProjectAs("next.mdp".into()));

        assert_eq!(
            mgr.resolve(KeyCode::S, Modifiers::ctrl(), ShortcutContext::default()),
            Some(Action::SaveProjectAs("next.mdp".into()))
        );
    }
}
