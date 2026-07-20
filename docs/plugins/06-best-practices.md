# 最佳实践与反模式

本文汇总了核心开发团队在实践中总结的设计建议和常见陷阱。

## 1. 设计原则

### 1.1 先分类，再编码

在写任何代码之前，先确定你的特效属于哪一类：

- 线性 unary effect → `with_graph`
- 需要分支/混合/蒙版 → `with_branching_graph`
- 内置算子无法表达的像素处理 → `with_custom_render_backend`

不要上来就选最底层接口。能用高层 DSL 实现的效果，底层实现不仅代码量大，而且失去了编译图优化和子树缓存的机会。

### 1.2 参数模型保持稳定

参数 key 会被序列化到项目文件、关联到关键帧系统、在 Inspector 中分组展示。参数变动的影响范围很大：

- 改名 → 旧项目打开后参数值丢失
- 改变类型 → 关键帧数据不可用
- 改变默认值 → 旧项目重新打开后行为差异

在插件设计阶段就确定参数命名和类型，后续避免不兼容的变更。

### 1.3 让图语义直观

好的 graph authoring 代码应该让人一眼看出：

- 从哪里分支
- 在哪条支路上处理
- 最后如何合成

如果实现需要大量手搓 node id，说明应该往 DSL 层再提一层。

### 1.4 不要推测预览/导出的差异

插件代码只描述特效语义，不要在里面写 `if is_preview { ... } else { ... }` 之类的逻辑。运行时负责调度和缓存策略的差异化，插件的责任是正确描述效果。

## 2. 最佳实践清单

### 特效定义

- [ ] 使用 `Plugin("plugin.<author>.<name>")` 作为 `EffectType`
- [ ] 显式声明 `EffectPluginContract`
- [ ] 显式声明 `EffectCapabilities`（通过 builder 方法自动推导）
- [ ] 参数 key 遵循 `{plugin_key}.{param}` 命名惯例
- [ ] 参数有合理的默认值和范围
- [ ] 图逻辑在参数不足以产生可见效果时提前 return（退化为 identity）

### 图构建

- [ ] 优先用 `apply` / `blend_current` / `mask_current` 等 DSL 方法
- [ ] 分支效果用 `blend_current` 而非手写 blend node
- [ ] 不在 graph builder 中执行耗时操作（文件 I/O、网络请求等应在 processor 中）
- [ ] 不在 graph builder 中依赖全局可变状态

### 自定义渲染

- [ ] 在 `buffer` 上做原地处理，不分配新的超大缓冲区
- [ ] `Ok(())` 才表示成功，半成品不写回
- [ ] 依赖外部资源时提供稳定 `cache_key`
- [ ] 正确处理 width/height 和 RGBA 步长（`buffer.len() == width * height * 4`）

### 缓存与性能

- [ ] deterministic 效果声明 `Deterministic`
- [ ] frame-dependent 效果明确标为 `FrameDependent`
- [ ] 外部资源相关特效提供 `cache_key`
- [ ] 大型自定义效果尽量把稳定子步骤拆出来

### 版本与容错

- [ ] 开发期用 `KeepDefinitionAvailable` + `KeepVisible`，但仍处理并显示每次求值错误
- [ ] 发布前切换到 `DisableDefinition` + `HideWhenUnavailable`
- [ ] 不在处理器中 panic（运行时已有隔离，但 panic 仍是应该避免的）

## 3. 反模式

### 3.1 在插件中直接依赖时间线或导出队列实现

```rust
// 反模式：插件直接访问时间线内部
let track = get_current_track(); // 不要这样做
let export_settings = get_export_queue_config(); // 不要这样做
```

插件只知道 Effect 语义。时间线和导出是上层关注的事情。

### 3.2 把 frame-dependent 效果声明成 deterministic

```rust
// 反模式：包含随机噪声却声明 Deterministic
EffectCachePolicy::Deterministic  // 实际效果每帧不同，会导致错误复用
```

这会导致帧被错误复用，画面出现奇怪的"冻结"或闪烁。

### 3.3 用不稳定字符串做 cache_key

```rust
// 反模式
cache_key: Some(format!("temp_{}", rand::random::<u64>()))

// 反模式
cache_key: Some(format!("lut_{}", std::time::Instant::now().elapsed().as_nanos()))
```

cache_key 必须在相同语义下产生相同值。

### 3.4 多个概念不同的效果共用一个 plugin key

```rust
// 反模式
let def = EffectPluginDefinitionBuilder::new("plugin.example.everything", "Everything")
    .with_graph(|...| {
        // 根据某个参数切换完全不相关的处理逻辑
        if mode == "blur" {
            // ...
        } else if mode == "color_grade" {
            // ...
        } else if mode == "glow" {
            // ...
        }
    });
```

应该拆成多个独立的 plugin key，每个只做一件事。

### 3.5 处理器半途报错但写回部分像素

```rust
// 反模式
Arc::new(|buffer, width, height, params, frame_seed| {
    for y in 0..height {
        for x in 0..width {
            let result = process_pixel(x, y);
            if result.is_err() {
                // 错误：已经写了前面像素的结果
                return Err(...);
            }
            write_pixel(buffer, x, y, width, result.unwrap());
        }
    }
    Ok(())
})
```

应该先全部计算到临时缓冲区，确认全部成功后再写回。或者利用运行时提供的 staged buffer 保证——处理器在独立 buffer 上执行，失败时结果自动丢弃。

### 3.6 在 graph builder 中做文件 I/O

```rust
// 反模式
.with_graph(|effect, context, graph| {
    let file_content = std::fs::read_to_string("config.json").unwrap();
    // ...
})
```

Graph builder 在每帧渲染时都可能被调用。文件 I/O 应放在 custom render processor 中，并配合 cache_key 确保只在必要时重新读取。

## 4. 代码审查自检清单

在提交插件代码前，自问这几个问题：

1. 这个特效是否错误地依赖了时间线或导出逻辑？
2. 这个 cache_key 是否稳定——同一输入总能产生同样的 key？
3. 这个特效是否应该是 FrameDependent？
4. 预览和导出是否共享同一套语义？
5. 是否引入了"只在某一处生效"的私有快捷路径？
6. 参数路径是否稳定、可预测？

## 下一步

- [UI 扩展](./07-ui-extensions.md) —— 注册面板和菜单
- [打包与分发](./08-packaging.md) —— 发布你的插件
