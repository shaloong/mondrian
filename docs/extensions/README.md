# Mondrian Extension

Extension 是 Mondrian 通用扩展平台的正式名称。它覆盖语言包、主题、命令、
工作流、面板、编辑工具、素材与服务提供者，以及与领域处理能力的集成。
一个 Extension 可以包含资源、Web 代码、Rust 代码或它们的组合。

> **状态：已确定方向，平台尚未实现。** 当前先完成 OpenFX/VST3/CLAP 音视频
> 插件宿主，再实施通用 Extension。本文不是可直接调用的 SDK 说明。

## 已确定的方向

- 支持新增、组合现有能力和替换指定实现；完整工作台与编辑器替换是长期目标。
- Web 扩展采用真正的 HTML/CSS/DOM，支持构建后的 JS/TS 前端生态；不以自建的
  JS/TS 到原生 Widget 映射作为前置路线。主应用仍是原生 winit/wgpu 工作台。
- Web 与 Rust 扩展使用受限容器和统一权限语义；已授权调用无感执行，拒绝、撤销
  或越界调用返回结构化错误。网络、外部文件、本机程序等访问由宿主检查并代理。
- Rust 可以采用 WASM 或 OS 沙箱原生 Worker；具体覆盖范围和浏览器引擎需要验证。
  普通子进程隔离、SDK 包装或权限声明都不等于已经实现安全沙箱。
- 初期复用现有创作对象；为未来第三方生成器和新内容类型保留领域接入路径。
- Alpha 阶段允许 API 调整，成熟后按能力逐步提供稳定兼容承诺。

## 文档导航

| 文档 | 用途 |
| --- | --- |
| [Extension 架构](../architecture/extension-system.md) | 范围、运行容器、权限、项目语义、交付顺序和技术验证 |
| [实施准备与验收计划](implementation-plan.md) | 当前代码入口、后续工作包、选型实验与退出条件 |
| [术语与边界](../domain/extensions.md) | Extension、Contribution、Permission Grant、Companion 等概念 |
| [ADR 0011](../adr/0011-use-managed-extension-platform.md) | 已接受的架构决策与原因 |
| [现有效果 SDK](effects/README.md) | 当前源码级 Rust EffectDefinition 开发接口 |
| [原生格式插件宿主](../architecture/native-plugin-hosts.md) | OpenFX/VST3/CLAP 当前实现与验证范围 |

## 命名与数据边界

通用平台使用 **Extension**。现有效果 SDK 的公开类型、模块、方法、示例、测试目标与
文档路径同样使用 Extension 命名；不因旧称已经存在而保留旧源码别名或目录。
泛指外部处理定义的 EffectType、视觉转场变体和颜色令牌使用 External，避免把原生格式
插件与 Extension 包混为一谈。

| 范围 | 当前规则 |
| --- | --- |
| 效果 SDK | EffectExtension*、extension_sdk、extension_contract；手册在 effects/ |
| 外部效果与转场 | 源码使用 EffectType::External、VideoTransitionType::External 与相应扁平类型 |
| 原生格式 | OpenFX/VST3/CLAP 的 plugin、厂商 SDK、扫描目录和格式标签保持专业含义 |
| 已保存数据 | serde 显式保留现有项目的 Plugin/plugin 判别值；已有定义 key 与参数身份按原值解析 |
| 新示例与自有定义 | 使用 extension.* 示例命名空间；既有效果 key 不自动变换字符串 |
| 许可材料 | Extension Additional Permission；SDK 与示例路径已同步 |

读取已有项目是需要测试的实际功能；它不要求保留旧源码 API。源码名称调整属于 Alpha
阶段允许的接口调整，不增加旧名称兼容壳。后续如要改变项目表示，需要独立的数据格式决策。

本地开发加载、打包和诊断先于商店建设；最终 manifest、包后缀、运行时与权限配置格式
待实现验证后确定。效果 SDK 的更名不表示通用 Extension 沙箱已经实现。
