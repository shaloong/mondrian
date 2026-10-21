# UI Extension

> **状态：已确定方向，尚未开放 API。** 本文是效果 Extension 手册中的
> 导航与边界说明，正式平台设计见 [Extension 架构](../../architecture/extension-system.md)。
> 旧 egui UI 不作为兼容目标。

## 1. 真实 Web UI

Web Extension 使用真正的 HTML、CSS、DOM 和 JS，以及构建后的 TS。开发者可以
使用浏览器兼容的前端框架和组件。自建 JS/TS 到原生 Widget 的映射不是前置路线。
主应用继续使用原生 winit/wgpu 工作台，宿主负责 Web 表面的停靠、窗口与生命周期。

浏览器引擎、系统 Webview 或随包引擎、合成和部署方式尚未确定。需要验证安装与
下载体积、冷启动、多面板内存/GPU、播放影响，以及输入法、快捷键、焦点、缩放、
停靠和浮动窗口。Web 技术兼容不意味着完整 Node.js 或任意原生 npm 模块可用。

## 2. 贡献与替换

初期支持的目标包括面板、菜单、工具栏、命令和设置入口，以及指定的 Inspector
扩展位置。宿主拥有放置、焦点、注册与用户选择；Extension 提供内容和行为。
菜单与快捷键通过统一命令入口，项目编辑进入 Authoring Session。

平台必须支持新增、组合和替换指定实现。Viewer Overlay、Timeline 工具和更深的
交互需要各自的公开接口；完整 Viewer、Timeline 编辑器和工作台替换是长期目标。
不能按加载顺序任意覆盖其他实现；禁用扩展后要能恢复有效的默认界面。

Rust Extension 同样需要受限执行。此前示意的跨二进制
`Box<dyn mondrian_ui_core::Widget>` 不是接受的公共 UI ABI，也没有已实现的
`PanelDefinition`/`PanelBuildContext` 扩展 API。原生 UI 或 Overlay 若需要开放，
必须另行定义明确协议。

## 3. 统一授权

Web/Rust Extension 通过公共 SDK 和宿主代理访问受保护能力。已有范围内授权
无感执行；拒绝、撤销或越界返回结构化错误；尚未决定时按用户策略和交互上下文
处理。开发者处理正常结果与失败，无需自行实现授权界面、权限记录或 IPC。

浏览器直接联网、资源加载、导航、Worker 和其他启用的入口也必须接受策略。
仅包装 fetch、声明 manifest 权限或配置 CSP 不能独立证明完整限制。扩展之间
的 DOM、存储、通信与权限也需要隔离。当前效果 SDK 不提供这些保证。

## 4. 主题、语言和交互

宿主从 mondrian-ui-theme 令牌向 Web 提供颜色、间距、字体、圆角、动画与缩放
等样式值；具体 CSS 变量名称尚未确定。扩展面板应使用这些值融入全局主题。

贡献声明与界面文本使用稳定的资源身份；语言变化不改变命令、面板或项目数据
身份。键盘导航、可访问名称、焦点恢复与输入法是宿主集成验收的一部分。

## 5. 当前实际入口

现在没有通用 Extension 面板安装或执行能力。现有效果 SDK 的
PropertyDescriptor 可由 Inspector 投影为参数控件，注册且准入的定义可出现在
Effect Browser。这是宿主生成的现有界面，不是独立扩展的 Web 面板。

原生格式插件的厂商编辑器另有窗口、线程和生命周期契约，不能用 Web 面板
规划冒充已支持。

## 相关文档

- [Extension 平台](../README.md)
- [Extension 架构](../../architecture/extension-system.md)
- [现有效果开发](03-effect-development.md)
- [打包与分发](08-packaging.md)
