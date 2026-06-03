# GPU 渲染引擎设计

## 1. 渲染管线总览

```text
FrameRequest(timecode)
    │
    ▼
┌─────────────────────────────────────────────────┐
│  BatchedCompositor (Pass Fusion)                │
│                                                  │
│  1. 从 Timeline 收集活跃 Layer 列表              │
│  2. 从 DecoderPool 获取每个 Layer 的原始帧       │
│  3. 将帧数据上传至 GPU Texture (YUV/RGBA)        │
│  4. GPU compute：YUV→RGB 颜色转换 + 效果链       │
│  5. Pass Fusion：单次 render pass 合成最多 4 层  │
│  6. 输出最终帧 (CompositedFrame + CallbackTrait)  │
│     → 零拷贝回调给 egui 预览窗口                 │
└───────────────────────┬─────────────────────────┘
                        │
                        ▼
              CompositedFrame（零拷贝）
              ┌──────────┼──────────┐
              ▼                     ▼
      预览窗口 Surface       帧缓冲（用于导出）
```

BatchedCompositor 使用 pass fusion 技术，单次 render pass 可同时合成最多 4 个 layer，大幅减少 GPU command submission 次数。超过 4 层时自动拆分为多次 pass。

---

## 2. Layer 合成模型

```text
Layer[0] (最底层，z-index=0)  ← 背景
Layer[1]  ← 视频 Track 1
Layer[2]  ← 视频 Track 2
Layer[3]  ← 文字叠加
Layer[4]  ← Logo 水印
    │
    ▼ 从下到上逐层合成
最终帧输出
```

每个 Layer 包含：

- `texture`: wgpu::Texture（YUV/RGBA）
- `transform`: Mat3（位置/缩放/旋转，在关键帧处求值）
- `opacity`: f32
- `blend_mode`: BlendMode
- `mask`: `Option<MaskTexture>`
- `effects`: `Vec<EffectNode>`（效果链）

---

## 3. wgpu 渲染架构

### 3.1 Shader 模块 (7 个 GPU compute shader)

```text
shaders/
├── composite.wgsl          两层 Alpha 混合 + 多种混合模式（Normal/Multiply/Screen/Overlay/Add/SoftLight）
├── composite_fused.wgsl    融合合成（单 pass 最多 4 层，配合 BatchedCompositor）
├── color_convert.wgsl      YUV→RGB 颜色空间转换（BT.709/BT.2020）+ GPU 颜色转换
├── gpu_texture.wgsl        GPU 纹理上传 & 采样（零拷贝路径）
├── lut3d.wgsl              3D LUT 调色（16³ / 33³ / 65³, compute shader 加速）
├── blur_gaussian.wgsl      高斯模糊（可分离卷积, compute shader 加速）
└── color_adjust.wgsl       颜色调整（色相/饱和度/亮度/对比度, compute shader 加速）
```

其中 `lut3d`、`blur_gaussian`、`color_adjust` 三个效果通过 GPU compute shader 直接加速，绕过 CPU→GPU 往返。

### 3.2 渲染资源管理

```rust
pub struct GpuContext {
    pub device:  wgpu::Device,
    pub queue:   wgpu::Queue,
    pub adapter: wgpu::Adapter,

    /// 纹理缓存（避免重复上传）
    texture_cache: LruCache<FrameKey, wgpu::Texture>,
}

pub struct RenderPipeline {
    /// YUV → RGB 转换 pipeline
    yuv_to_rgb: wgpu::RenderPipeline,
    /// 层合成 pipeline
    composite:  wgpu::RenderPipeline,
    /// 效果链 compute pipeline
    effects:    HashMap<EffectType, wgpu::ComputePipeline>,
    /// Uniform buffer（变换矩阵 / 效果参数）
    uniforms:   wgpu::Buffer,
}
```

### 3.3 帧合成流程（伪代码）

```rust
fn render_frame(ctx: &GpuContext, layers: &[Layer]) -> wgpu::Texture {
    let mut encoder = ctx.device.create_command_encoder(&Default::default());

    // 从最底层开始，逐层叠加
    let mut accum_texture = create_black_texture(ctx, output_size);

    for layer in layers.iter() {
        // 1. 确保该层纹理已上传
        let layer_tex = upload_or_cache(ctx, layer);

        // 2. 应用效果链（compute shader）
        let processed_tex = apply_effects(ctx, &mut encoder, layer_tex, &layer.effects);

        // 3. 合成到 accumulator（render pass）
        composite_layer(
            ctx, &mut encoder,
            &accum_texture,   // dst
            &processed_tex,   // src
            &layer.transform,
            layer.opacity,
            layer.blend_mode,
        );
    }

    ctx.queue.submit([encoder.finish()]);
    accum_texture
}
```

---

## 4. YUV → RGB Shader

```wgsl
// yuv_to_rgb.wgsl
@group(0) @binding(0) var y_plane:  texture_2d<f32>;
@group(0) @binding(1) var uv_plane: texture_2d<f32>;
@group(0) @binding(2) var samp:     sampler;

// BT.709 YUV → RGB 矩阵
const YUV_TO_RGB: mat3x3<f32> = mat3x3<f32>(
    vec3(1.0,  1.0,  1.0),
    vec3(0.0, -0.187, 1.856),
    vec3(1.575, -0.468, 0.0),
);

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let y  = textureSample(y_plane,  samp, uv).r;
    let uv_val = textureSample(uv_plane, samp, uv).rg - vec2(0.5);
    let yuv = vec3(y - 0.0625, uv_val.x, uv_val.y);
    let rgb = YUV_TO_RGB * yuv;
    return vec4(clamp(rgb, vec3(0.0), vec3(1.0)), 1.0);
}
```

---

## 5. 混合模式实现

```wgsl
// composite.wgsl - 支持的混合模式
fn blend(mode: u32, src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    switch mode {
        case 0u: { return normal_blend(src, dst); }       // 正常
        case 1u: { return multiply_blend(src, dst); }     // 正片叠底
        case 2u: { return screen_blend(src, dst); }       // 滤色
        case 3u: { return overlay_blend(src, dst); }      // 叠加
        case 4u: { return add_blend(src, dst); }          // 线性减淡
        case 5u: { return soft_light_blend(src, dst); }   // 柔光
        default: { return normal_blend(src, dst); }
    }
}

fn normal_blend(src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    let a = src.a + dst.a * (1.0 - src.a);
    let rgb = (src.rgb * src.a + dst.rgb * dst.a * (1.0 - src.a)) / max(a, 0.0001);
    return vec4(rgb, a);
}
```

---

## 6. 实时性能策略

| 策略                | 说明                                    |
| ------------------- | --------------------------------------- |
| Pass Fusion         | 单次 render pass 合成最多 4 层，减少 GPU submission |
| 脏区域更新          | 只重绘变化的 Layer，跳过未变化层        |
| 纹理缓存            | LRU，容量 = GPU 显存的 30%（约 2GB）    |
| 分辨率降采样预览    | 编辑时用 1/2 分辨率预览，回放时全分辨率 |
| 帧预渲染队列        | 播放时提前预渲染 4 帧（lookahead）      |
| Compute Shader 并行 | 多个效果节点并行执行 compute pass       |

### 6.1 GPU 性能剖析

设置 `MONDRIAN_RENDER_PROFILE=1` 环境变量可在运行时输出 GPU 渲染性能 JSON 报告，包含每帧耗时、pass 数量、纹理上传时间等指标：

```bash
# Windows (PowerShell)
$env:MONDRIAN_RENDER_PROFILE=1; cargo run -p mondrian-app

# Linux / macOS
MONDRIAN_RENDER_PROFILE=1 cargo run -p mondrian-app
```

---

## 7. 零拷贝回调渲染 (CompositedFrame + CallbackTrait)

BatchedCompositor 输出 `CompositedFrame`，通过 `CallbackTrait` 以零拷贝方式直接提交给 egui 预览窗口，无需 CPU readback 或额外的纹理拷贝。

```rust
/// 零拷贝合成结果，直接回调到 egui 渲染管线
pub struct CompositedFrame {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: (u32, u32),
    pub color_space: ColorSpace,
}

/// 回调 trait，egui 通过此接口消费合成帧
pub trait CallbackTrait {
    fn paint(&self, frame: &CompositedFrame, ui: &mut egui::Ui);
}
```

egui 和 Compositor 共享同一 `wgpu::Device` 和 `wgpu::Queue`（unified GPU），避免跨设备拷贝开销。

---

## 8. GPU 颜色转换

YUV 到 RGB 的颜色空间转换在 GPU compute shader 中完成（`color_convert.wgsl`），支持：

- BT.601（SD）
- BT.709（HD）
- BT.2020（4K HDR）
- 10-bit / 12-bit 输入

转换矩阵作为 uniform buffer 传入，允许运行时切换色彩空间而无需重新编译 shader。

---

## 9. 色彩显示链路

预览合成输出不会直接以”源素材空间”显示，而是经过两级转换：

1. 序列工作空间 -> 序列输出色彩空间
2. 序列输出色彩空间 -> 显示器 profile

`DisplayColorProfile` 负责表示显示端的参考校准信息，当前以 Rec.709 / Display P3 的参考配置为起点，并支持导入 `.icc/.icm` 作为显示器目标 profile。

核心颜色管理不再只依赖单个函数，而是通过可哈希的 `ColorTransformPlan` 描述节点链路：`DecodeTransfer`、`ConvertPrimaries`、`ToneMapAces`、`Lut3D`、`DisplayProfile`。preview 和 export 共用同一套计划与签名语义，因此缓存、测试和导出结果不会因为路径不同而漂移。

导入阶段仍会解析 `desc/mluc`、`rXYZ/gXYZ/bXYZ`、`rTRC/gTRC/bTRC`、`A2B/B2A` 等 tag 来生成名称、基础矩阵和 gamma 补偿（用于回退路径与签名）。渲染阶段优先走 `moxcms` 的标准 ICC transform（源工作色域 profile -> 目标显示器 ICC），由 CMS 执行完整 LUT/CLUT/mAB/mBA 链路；仅在 transform 构建失败或色域不支持时，才回退到矩阵 + gamma 近似路径。ICC 在这里是可派生的显示 profile 表示，不是唯一真相。

当 nested sequence 进入 preview 时，也会先解析自己的序列色彩上下文，再回到父序列工作空间参与合成，避免把子序列的显示意图直接混进父序列。

```rust
pub enum ColorSpace {
    Srgb,           // sRGB (Web / 消费级显示器)
    Rec709,         // BT.709 (HD 广播标准)
    Rec2020,        // BT.2020 (4K HDR)
    DciP3,          // DCI-P3 (电影院)
    AppleLog,       // Apple Log (iPhone ProRes LOG)
    SLog3,          // Sony S-Log3
    ArriLogC4,      // ARRI LogC4
}

/// 色彩空间转换（通过 ICC profile 或 LUT）
pub struct ColorTransform {
    pub src: ColorSpace,
    pub dst: ColorSpace,
    pub lut: Option<Lut3D>,   // 自定义 LUT 覆盖
}
```
