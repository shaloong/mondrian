# UI 扩展

> **状态：规划中** —— 以下描述的是旧 egui UI 的目标形态，具体 API 尚未开放。
> self-hosted UI 的插件扩展应基于未来的 widget / panel adapter 接口另行设计。

本文介绍插件如何扩展 Mondrian 的用户界面：注册面板、菜单项和工具栏按钮。

## 1. 设计理念

UI 扩展遵循以下原则：

- **低侵入** —— 插件 UI 运行在隔离上下文中，不能破坏主界面布局
- **声明式** —— 通过 manifest 声明需要什么 UI 区域，运行时负责放置
- **一致性** —— 插件面板遵循全局主题令牌，自动适配暗色/亮色模式
- **按需加载** —— 面板在首次可见时才创建，不可见时可休眠

## 2. 扩展点概览

```
┌─────────────────────────────────────────────────────────┐
│  菜单栏                     [插件注册的菜单项]           │
├──────────┬────────────────────────────────┬─────────────┤
│          │                                │             │
│  侧边栏   │        主视图区域               │  侧边栏      │
│  (可注册  │                                │  (可注册     │
│   面板)   │                                │   面板)      │
│          │                                │             │
├──────────┴────────────────────────────────┴─────────────┤
│  底部面板区域 (可注册面板)                                │
│  状态栏                        [插件注册的状态指示器]     │
└─────────────────────────────────────────────────────────┘
```

## 3. 面板

插件可以注册自定义面板，出现在侧边栏或底部面板区域。

### 3.1 预期使用方式

```rust
// 规划中的 API 示意 —— 尚未可用
use mondrian_app::egui_ui::{
    PanelDefinition, PanelLocation, PanelContext,
};

struct MyPluginPanel {
    // 面板状态
}

impl PanelDefinition for MyPluginPanel {
    fn title(&self) -> &str { "My Tool" }
    fn location(&self) -> PanelLocation { PanelLocation::RightSidebar }
    fn show(&mut self, ctx: &PanelContext, ui: &mut egui::Ui) {
        // 使用 egui 绘制面板内容
        ui.label("Hello from plugin!");
    }
}
```

### 3.2 面板位置

| 位置 | 说明 |
|------|------|
| `LeftSidebar` | 左侧边栏 |
| `RightSidebar` | 右侧边栏 |
| `BottomPanel` | 底部面板区域 |
| `Floating` | 浮动窗口 |

## 4. 菜单项

插件可以向主菜单或右键菜单注册自定义项。

### 4.1 预期使用方式

```rust
// 规划中的 API 示意 —— 尚未可用
use mondrian_app::egui_ui::MenuExtension;

fn register_menu_items() -> Vec<MenuExtension> {
    vec![
        MenuExtension::new("Tools > My Plugin > Do Thing")
            .with_shortcut("Ctrl+Shift+T")
            .with_action(|| { /* ... */ }),
    ]
}
```

## 5. 工具栏按钮

```rust
// 规划中的 API 示意 —— 尚未可用
use mondrian_app::egui_ui::ToolbarButton;

fn register_toolbar_buttons() -> Vec<ToolbarButton> {
    vec![
        ToolbarButton::new("my_tool")
            .with_icon("plugin://my_plugin/icons/tool.png")
            .with_tooltip("My Tool")
            .with_action(|| { /* ... */ }),
    ]
}
```

## 6. 主题一致性

所有插件 UI 组件应该使用 Mondrian 的全局主题令牌，而不是硬编码颜色值：

- 颜色：从 `MondrianTheme` 获取语义色（`text_primary`、`bg_panel`、`accent` 等）
- 间距：使用主题定义的 spacing scale
- 圆角：使用主题定义的 rounding tokens
- 字体：使用主题定义的 typography scale

这确保插件 UI 在暗色/亮色模式下都能自然融入主界面。

## 7. 当前替代方案

在 UI 扩展 API 正式开放之前，插件目前可以通过以下方式提供有限的用户交互：

- **特效参数**：通过 `PropertyDescriptor` 定义的参数会自动出现在 Inspector 面板中，用户可以在那里调整
- **特效库**：注册的特效会自动出现在特效库面板中

这些是当前唯一可用的"UI 入口"。

## 下一步

- [打包与分发](./08-packaging.md) —— 发布插件
- [特效开发](./03-effect-development.md) —— 当前可用的核心能力
