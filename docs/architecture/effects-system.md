# 效果系统设计

## 1. 效果节点图（Effect Graph）

每个 Clip 的效果以**有向无环图**（DAG）组织，支持串联和并联：

```text
输入帧 (RawTexture)
    │
    ▼
[ColorCorrection]   ← LUT / 颜色轮
    │
    ▼
[GaussianBlur]      ← 仅对蒙版区域
    │
    ▼
[Sharpen]
    │
    ▼
[Vignette]
    │
    ▼
输出帧 (ProcessedTexture)
```

---

## 2. LUT 调色

```rust
pub struct Lut3D {
    pub size: u32,             // 16 / 33 / 65 (LUT cube 边长)
    pub data: Vec<[f32; 3]>,   // RGB 值 (size³ 个点)
    pub name: String,
    pub path: PathBuf,         // 原始 .cube 文件
}

impl Lut3D {
    /// 从 .cube 文件解析
    pub fn from_cube_file(path: &Path) -> Result<Self>;

    /// 上传到 GPU (wgpu 3D Texture)
    pub fn upload_to_gpu(&self, device: &wgpu::Device) -> wgpu::Texture;

    /// 插值强度 (0.0 = 不应用, 1.0 = 完全应用)
    pub fn with_intensity(self, intensity: f32) -> LutEffect;
}
```text

**WGSL Shader 实现：**

```wgsl
// lut3d.wgsl
@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var lut_tex:   texture_3d<f32>;
@group(0) @binding(2) var samp:      sampler;

struct Uniforms { intensity: f32 }
@group(1) @binding(0) var<uniform> uniforms: Uniforms;

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let original = textureSample(input_tex, samp, uv);
    // 将 RGB 作为 3D 纹理坐标（三线性插值）
    let lut_coord = original.rgb * (LUT_SIZE - 1.0) / LUT_SIZE + 0.5 / LUT_SIZE;
    let graded = textureSample(lut_tex, samp, lut_coord).rgb;
    let result = mix(original.rgb, graded, uniforms.intensity);
    return vec4(result, original.a);
}
```

---

## 3. 内置效果列表

### 调色类

| 效果              | Shader             | 参数                       |
| ----------------- | ------------------ | -------------------------- |
| 颜色轮（Lumetri） | color_wheel.wgsl   | 阴影/中间调/高光 RGB 偏移  |
| 曲线调整          | curves.wgsl        | RGB/单通道曲线（256点LUT） |
| 色相/饱和度/亮度  | hsl.wgsl           | H/S/L 偏移量               |
| 3D LUT            | lut3d.wgsl         | .cube 文件 + 强度          |
| 白平衡            | white_balance.wgsl | 色温(K) + 色调             |
| 噪点              | grain.wgsl         | 强度 + 颗粒大小            |

### 模糊类

| 效果     | 参数             |
| -------- | ---------------- |
| 高斯模糊 | 半径（可带蒙版） |
| 径向模糊 | 中心点 + 强度    |
| 运动模糊 | 方向 + 强度      |
| 景深模糊 | 焦点距离 + 光圈  |

### 光效类

| 效果              | 参数               |
| ----------------- | ------------------ |
| 晕影（Vignette）  | 大小 + 羽化 + 颜色 |
| 镜头光晕          | 位置 + 强度 + 类型 |
| 发光（Glow）      | 阈值 + 强度 + 半径 |
| 色差（Chromatic） | 偏移量             |

### 抠图类

| 效果         | 参数                     |
| ------------ | ------------------------ |
| 绿幕抠图     | 关键色 + 容差 + 边缘羽化 |
| 亮度抠图     | 亮度范围                 |
| 蒙版（形状） | 矩形/椭圆/钢笔路径       |

---

## 4. 转场系统

```rust
pub trait Transition: Send + Sync {
    fn name(&self) -> &str;
    fn duration_default(&self) -> Duration;

    /// 渲染转场帧
    /// progress: 0.0 (全显A) → 1.0 (全显B)
    fn render(
        &self,
        ctx: &GpuContext,
        frame_a: &wgpu::Texture,   // 出场画面
        frame_b: &wgpu::Texture,   // 入场画面
        progress: f32,
        params: &TransitionParams,
    ) -> wgpu::Texture;
}

// 内置转场
pub struct CrossDissolve;      // 交叉溶解（最常用）
pub struct WipeLeft;           // 左推
pub struct WipeRight;          // 右推
pub struct ZoomIn;             // 推近
pub struct ZoomOut;            // 拉远
pub struct FilmBurn;           // 胶片燃烧
pub struct GlitchTransition;   // Glitch 故障
pub struct LensFlareTransition; // 镜头光晕擦除
```text

---

## 5. 文字动画系统

```rust
pub struct TextLayer {
    pub content: String,
    pub font_family: String,
    pub font_size: KeyframeTrack<f32>,
    pub color: KeyframeTrack<Color>,
    pub position: KeyframeTrack<Vec2>,
    pub opacity: KeyframeTrack<f32>,
    pub character_spacing: f32,
    pub line_spacing: f32,
    pub animation_preset: Option<TextAnimationPreset>,
}

pub enum TextAnimationPreset {
    FadeIn  { duration: Duration },
    TypeWriter { chars_per_second: f32 },
    SlideFromBottom { duration: Duration, easing: Easing },
    ScalePop { peak_scale: f32, duration: Duration },
    KaraokeHighlight { word_color: Color },  // 配合字幕时间轴
}
```

---

## 6. 蒙版系统

```rust
pub enum MaskShape {
    Rectangle { rect: Rect, corner_radius: f32 },
    Ellipse   { center: Vec2, radii: Vec2 },
    Path      { points: Vec<BezierPoint> },   // 钢笔工具
    Difference { a: Box<MaskShape>, b: Box<MaskShape> },
    Union     { shapes: Vec<MaskShape> },
}

pub struct Mask {
    pub shape: MaskShape,
    pub feather: f32,          // 边缘羽化（像素）
    pub invert: bool,
    pub tracking: Option<TrackingData>, // 绑定追踪数据（v0.6）
}
```text
