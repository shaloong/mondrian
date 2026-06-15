//! 快捷键管理器 trait
//!
//! 支持全局/工作区/Panel/Widget 四级作用域，高层级的绑定优先。

use crate::types::WidgetId;
use crate::types::{KeyCode, Modifiers};
use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;

/// 快捷键作用域（优先级从高到低）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutScope {
    /// 全局快捷键（如 Ctrl+S 保存）
    Global,
    /// 工作区级快捷键
    Workspace,
    /// 面板级快捷键（如 Timeline 的 Delete）
    Panel(PanelKind),
    /// 特定 Widget 的快捷键
    Widget(WidgetId),
}

/// 快捷键绑定
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShortcutBinding {
    pub key: KeyCode,
    pub modifiers: Modifiers,
}

impl ShortcutBinding {
    pub fn new(key: KeyCode, modifiers: Modifiers) -> Self {
        Self { key, modifiers }
    }

    pub fn key_only(key: KeyCode) -> Self {
        Self { key, modifiers: Modifiers::none() }
    }

    pub fn ctrl(key: KeyCode) -> Self {
        Self { key, modifiers: Modifiers::ctrl() }
    }

    pub fn ctrl_shift(key: KeyCode) -> Self {
        Self {
            key,
            modifiers: Modifiers { ctrl: true, shift: true, ..Default::default() },
        }
    }
}

/// Shortcut resolution context captured from the current focus state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShortcutContext {
    /// Currently focused widget, if any.
    pub widget: Option<WidgetId>,
    /// Currently focused panel, if any.
    pub panel: Option<PanelKind>,
}

impl ShortcutContext {
    pub fn new(widget: Option<WidgetId>, panel: Option<PanelKind>) -> Self {
        Self { widget, panel }
    }
}

/// 快捷键管理器
///
/// ## 职责
///
/// * 注册/注销快捷键绑定
/// * 根据当前焦点上下文分发快捷键
/// * 高优先级作用域覆盖低优先级（Global < Workspace < Panel < Widget）
pub trait ShortcutManager {
    /// 注册一个快捷键绑定
    fn register(&mut self, scope: ShortcutScope, binding: ShortcutBinding, action: Action);

    /// 注销一个快捷键绑定
    fn unregister(&mut self, scope: ShortcutScope, binding: &ShortcutBinding);

    /// 根据按键和当前焦点上下文，查找对应的 Action
    fn resolve(
        &self,
        key: KeyCode,
        modifiers: Modifiers,
        context: ShortcutContext,
    ) -> Option<Action>;

    /// 清除某个作用域的所有绑定
    fn clear_scope(&mut self, scope: ShortcutScope);

    /// 清除所有绑定
    fn clear_all(&mut self);
}
