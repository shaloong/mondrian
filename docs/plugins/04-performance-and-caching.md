# 性能与缓存

本文介绍 Mondrian 特效运行时的缓存模型，以及插件开发者如何利用缓存让特效在预览和导出中都保持高效。

## 1. 缓存层次

Mondrian 特效运行时有两层缓存：

### 1.1 全图输出缓存（Whole-Graph Output Cache）

对整个 `CompiledEffectGraph` 的最终输出做缓存。

适用条件：
- 完全相同的图结构
- 完全相同的输入帧
- 完全相同的参数值

缓存 key 构成：
- 图签名（graph signature）
- 输入签名（input signature）
- 输出宽高
- frame seed（仅 frame-dependent 特效）

### 1.2 子树/节点输出缓存（Subtree / Node Output Cache）

对昂贵的 **deterministic** 子树做缓存。

适用条件：
- 同一棵子树被不同图复用
- 同一棵子树在同一编译图中被多次使用
- 子树成本足够高，缓存收益大于哈希开销

缓存 key 构成：
- 子树签名（subtree signature）
- 输入签名
- 输出宽高
- frame seed（仅 frame-dependent）

## 2. EffectCachePolicy

### Deterministic

相同输入、相同参数、相同外部依赖 → **输出完全一致**。

适合：
- 模糊（blur）
- 锐化（sharpen）
- 暗角（vignette）
- 3D LUT
- 确定性自定义滤波器

### FrameDependent

输出显式依赖 frame seed 或时间采样噪声。

适合：
- 噪点（grain）
- 时域噪声（temporal noise）
- 抖动、闪烁、随机化效果

**如果把 frame-dependent 效果误报为 deterministic，会导致错误的帧复用。**

## 3. cache_key 设计

`cache_key` 用于表达："这个自定义效果的结果依赖什么稳定外部资源"。

### 适合放入 cache_key 的内容

- LUT 文件路径
- 模型文件版本
- 外部卷积核资源 ID
- 预计算 kernel/config 版本
- 外部依赖的哈希或版本号

### 不适合放入 cache_key 的内容

- 每帧变化的临时值
- 未标准化的调试字符串
- 带随机部分的值
- 用户本地路径（不同机器路径不同）

### 示例

```rust
// 好的 cache_key：稳定、可复现
Some(Arc::new(|effect, context| {
    let lut_path = effect.evaluate_str_by_suffix(
        "plugin.example.lut.path", context.time, ""
    );
    let file_hash = compute_sha256(&lut_path).unwrap_or_default();
    Some(format!("lut:sha256:{}", file_hash))
}))

// 不好的 cache_key：包含时间戳
Some(Arc::new(|effect, context| {
    Some(format!("lut:{}", std::time::SystemTime::now().duration_since(...)))
}))
```

## 4. 缓存启用策略

运行时**不会对所有节点一律缓存**。只有满足以下条件的子树才会启用节点缓存：

- 计算成本足够高，值得缓存
- 或者存在明显的跨图复用价值
- 且 cache policy 已明确声明

这个策略避免了：
- 哈希计算成本超过渲染成本
- 小图/轻量特效过度缓存
- 内存增长失控

## 5. 对你的插件的建议

### 5.1 新插件的默认选择

| 特效特征 | 推荐 Policy | 推荐提供 cache_key |
|----------|-------------|-------------------|
| 纯内置算子组合，无外部依赖 | `Deterministic` | 不需要 |
| 依赖外部文件（LUT、模型） | `Deterministic` | 强烈建议 |
| 包含随机/噪声/时间扰动 | `FrameDependent` | 视情况 |
| 自定义像素处理器 | 视算法而定 | 视情况 |

### 5.2 大特效拆分子树

如果自定义特效内部能拆出稳定的独立子树，优先通过 DSL 把稳定部分显式表达出来，让运行时可以对子树做独立缓存和复用：

```rust
// 不推荐：整棵大树只有一个 cache_key
graph.apply(EffectRenderOp::Custom {
    key: "plugin.example.big_effect".to_string(),
    params: whole_params,
    cache_key: Some("big-effect-v1".to_string()),
    cache_policy: EffectCachePolicy::Deterministic,
});

// 推荐：把稳定子步骤拆开
graph.apply(EffectRenderOp::GaussianBlur { radius: 4.0 }); // 自动缓存
graph.apply(EffectRenderOp::Custom {
    key: "plugin.example.expensive_step".to_string(),
    params: only_expensive_part,
    cache_key: Some("expensive-step-v1".to_string()),
    cache_policy: EffectCachePolicy::Deterministic,
});
```

### 5.3 核心原则

- 只要特效依赖外部资源，尽量提供稳定 `cache_key`
- 如果特效依赖 frame seed、随机噪声、时间扰动，明确标为 `FrameDependent`
- 不要为了"看起来高级"随意声明 cache key —— 错误 key 比没有 key 更危险
- 错误声明 deterministic 会导致画面错误；这比性能差更糟糕

## 6. 预览 vs 导出

预览和导出共享同一套缓存语义，但可以有不同的调度策略：

- **预览**：可能使用更低的分辨率、更激进的缓存淘汰、更短的缓存 TTL
- **导出**：全分辨率渲染，缓存可以长驻，逐帧顺序执行

插件不需要——也不应该——在代码中区分 preview 和 export。运行时负责调度。

## 下一步

- [版本契约](./05-versioning-and-compatibility.md) —— 管理版本与兼容性
- [最佳实践](./06-best-practices.md) —— 更多设计建议
