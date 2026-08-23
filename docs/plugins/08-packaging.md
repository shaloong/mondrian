# 打包与分发

> **状态：规划中** —— 以下描述的是目标形态，当前插件以源码 crate 形式集成。

本文介绍如何将插件打包为可分发的产物，以及相关的依赖管理和版本策略。

## 1. 当前方式：源码级集成

目前 Mondrian 插件以 Rust crate 形式存在于仓库中，通过 Cargo workspace 集成：

```
crates/
├── mondrian-plugin-hello/
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
```

应用在初始化时显式调用插件的注册函数：

```rust
// 在 mondrian-app 初始化代码中
mondrian_plugin_hello::register();
```

这种方式的优点是可以直接利用 Rust 的类型系统和编译期检查，缺点是需要重新编译整个应用来添加或更新插件。

## 2. 目标形态：独立打包

后续版本将支持以下分发方式：

### 2.1 插件包结构（规划）

```
my_plugin.mdp-plugin          # ZIP 容器
├── manifest.json             # 插件元数据
├── libmy_plugin.so           # 编译产物（平台相关）
│   ├── linux-x86_64/
│   ├── macos-arm64/
│   └── windows-x86_64/
├── assets/                   # 插件自带资源
│   ├── icons/
│   ├── luts/
│   └── fonts/
└── docs/                     # 插件自带文档
```

### 2.2 Manifest（规划）

```json
{
  "key": "plugin.acme.color_toolkit",
  "name": "ACME Color Toolkit",
  "version": "1.2.0",
  "api_version": "1.0",
  "author": "ACME Corp",
  "description": "Professional color grading tools",
  "capabilities": ["effect", "panel"],
  "effects": [
    {
      "key": "plugin.acme.color_toolkit.film_emulation",
      "display_name": "Film Emulation",
      "category": "color"
    }
  ],
  "panels": [
    {
      "id": "acme_color_scopes",
      "title": "Color Scopes",
      "location": "right_sidebar"
    }
  ],
  "dependencies": {},
  "min_app_version": "0.8.0"
}
```

## 3. 依赖管理

### 3.1 当前约束

- 插件依赖 `mondrian-core` 和 `mondrian-effects`，跟随仓库版本
- 插件不能引入与 Mondrian 冲突的依赖版本
- 所有依赖必须是纯 Rust 或已有 FFI 绑定的库

### 3.2 后续方向

- 插件可以声明对特定 Mondrian API 版本的依赖
- 运行时在加载时验证版本兼容性
- 允许插件自带平台相关的本地库（通过 FFI）

## 4. 版本策略建议

无论采用什么分发方式，建议遵循语义化版本：

- **主版本**（major）：不兼容的 API 变更
- **次版本**（minor）：向后兼容的新功能
- **修订版本**（patch）：向后兼容的 bug 修复

与 Mondrian API 的兼容性通过 `api_version` 字段独立管理。

## 5. 当前推荐做法

在独立打包可用之前：

1. 将插件维护在 Mondrian 仓库的 `crates/` 目录下
2. 使用 `plugin.<author>.<name>` 作为唯一标识
3. 在 `Cargo.toml` 中使用 path dependency 引用 core 和 effects
4. 通过 `register()` 函数作为插件入口
5. 在应用初始化时显式注册

## 下一步

- [快速入门](./01-getting-started.md) —— 当前可用的开发流程
- [特效开发](./03-effect-development.md) —— 核心特效能力
