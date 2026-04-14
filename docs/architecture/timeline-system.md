# 时间线系统设计

## 1. 数据模型

```text
Timeline (项目序列)
│
├── settings: SequenceSettings  ← 帧率/分辨率/音频采样率
│
├── video_tracks: Vec<Track>
│   ├── Track { id, name, clips: Vec<Clip> }
│   │   ├── Clip { id, asset_id, in_point, out_point, position, ... }
│   │   └── ...
│   └── ...
│
└── audio_tracks: Vec<Track>
    ├── Track { id, name, clips: Vec<Clip> }
    └── ...
```

---

## 2. 核心数据结构

```rust
/// 时间码（帧精确）
/// 内部用有理数表示，避免浮点误差
pub struct TimeCode {
    frame: i64,
    time_base: Rational,   // e.g. 1/25 for 25fps
}

/// 序列（Sequence）= 一条时间线
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub settings: SequenceSettings,
    pub video_tracks: Vec<Track>,
    pub audio_tracks: Vec<Track>,
    pub duration: TimeCode,        // 计算属性（最后一个 clip 的结束位置）
    pub playhead: TimeCode,        // 当前播放位置
    pub work_area: Option<TimeRange>, // 工作区间（渲染范围）
}

/// 轨道
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub track_type: TrackType,     // Video / Audio
    pub height: f32,               // UI 高度（像素）
    pub is_muted: bool,
    pub is_locked: bool,
    pub is_solo: bool,
    pub clips: Vec<Clip>,
    pub blend_mode: BlendMode,     // 仅视频轨有效
    pub opacity_keyframes: KeyframeTrack<f32>,
}

/// 剪辑（Clip）— 时间线上的一个素材片段
pub struct Clip {
    pub id: ClipId,
    pub asset_id: AssetId,         // 指向素材库中的原始媒体
    pub position: TimeCode,        // 在时间线上的起始位置
    pub duration: TimeCode,        // 在时间线上的持续时长
    pub source_in: TimeCode,       // 素材内的入点
    pub source_out: TimeCode,      // 素材内的出点
    pub speed: SpeedCurve,         // 变速曲线（1.0 = 正常速度）
    pub effects: Vec<EffectRef>,   // 绑定的效果列表
    pub transform: Transform2D,    // 基础变换
    pub keyframes: ClipKeyframes,  // 所有关键帧数据
    pub linked_clip: Option<ClipId>, // 关联的音频/视频 clip
}
```text

---

## 3. 关键帧系统

### 关键帧类型

```rust
pub enum InterpolationType {
    Hold,       // 阶梯（不插值）
    Linear,     // 线性插值
    Bezier,     // 贝塞尔曲线（最常用）
    EaseIn,     // 缓入（贝塞尔预设）
    EaseOut,    // 缓出（贝塞尔预设）
    EaseInOut,  // 缓入缓出（贝塞尔预设）
}

pub struct Keyframe<T> {
    pub time: TimeCode,
    pub value: T,
    pub interpolation: InterpolationType,
    /// 贝塞尔控制点（仅 Bezier 类型有效）
    pub control_in: Option<Vec2>,
    pub control_out: Option<Vec2>,
}

/// 关键帧轨道（泛型，支持 f32/Vec2/Vec3/Color 等）
pub struct KeyframeTrack<T: Interpolatable> {
    pub keyframes: Vec<Keyframe<T>>,
}

impl<T: Interpolatable> KeyframeTrack<T> {
    /// 在指定时间码处求值（插值）
    pub fn evaluate(&self, time: TimeCode) -> T;

    /// 添加关键帧（自动排序）
    pub fn add_keyframe(&mut self, kf: Keyframe<T>);

    /// 删除关键帧
    pub fn remove_keyframe(&mut self, time: TimeCode) -> Option<Keyframe<T>>;
}
```

### 贝塞尔曲线插值

$$
B(t) = (1-t)^3 P_0 + 3(1-t)^2 t P_1 + 3(1-t) t^2 P_2 + t^3 P_3
$$

其中 $t \in [0, 1]$，$P_0, P_3$ 是关键帧值，$P_1, P_2$ 是控制点。

---

## 4. 变速系统（Speed Curve）

```rust
pub enum SpeedCurve {
    /// 均匀速度（1.0 = 正常，2.0 = 2倍速，0.5 = 慢放）
    Constant(f64),

    /// 变速曲线（关键帧驱动，对标 PR 的时间重映射）
    Keyframed {
        /// 每个关键帧：(时间线时间, 素材时间)
        curve: KeyframeTrack<f64>,
    },

    /// 倒放
    Reverse,
}

impl SpeedCurve {
    /// 给定时间线时间 → 素材时间
    pub fn map_time(&self, timeline_time: TimeCode) -> TimeCode;
}
```text

---

## 5. Transform2D（2D 变换）

```rust
pub struct Transform2D {
    pub position: KeyframeTrack<Vec2>,     // 位置 (x, y) 单位：像素
    pub scale: KeyframeTrack<Vec2>,         // 缩放 (x, y) 1.0 = 100%
    pub rotation: KeyframeTrack<f32>,       // 旋转（度）
    pub anchor_point: KeyframeTrack<Vec2>,  // 锚点（相对于自身）
    pub opacity: KeyframeTrack<f32>,        // 不透明度 0.0~1.0
}

impl Transform2D {
    /// 在指定时间码求值，返回最终变换矩阵（Mat3）
    pub fn evaluate_matrix(&self, time: TimeCode) -> glam::Mat3;
}
```

---

## 6. 时间线操作（Command 模式）

所有操作通过 Command 包装，支持撤销/重做：

```rust
pub trait Command: Send + Sync {
    fn execute(&mut self, timeline: &mut Sequence) -> Result<()>;
    fn undo(&mut self, timeline: &mut Sequence) -> Result<()>;
    fn description(&self) -> &str;
}

// 具体命令示例
pub struct AddClipCommand { track_id, asset_id, position, ... }
pub struct MoveClipCommand { clip_id, new_position, old_position }
pub struct SplitClipCommand { clip_id, split_point }
pub struct DeleteClipCommand { clip_id, saved_clip: Option<Clip> }
pub struct TrimClipCommand  { clip_id, edge: TrimEdge, delta: TimeCode }
pub struct SetKeyframeCommand { clip_id, property, keyframe }

/// 命令历史管理器
pub struct CommandHistory {
    undo_stack: Vec<Box<dyn Command>>,
    redo_stack: Vec<Box<dyn Command>>,
    max_history: usize,   // 默认 200 步
}
```text

---

## 7. 时间线查询接口

```rust
impl Sequence {
    /// 获取指定时间码处所有活跃的 Clip（用于渲染）
    pub fn active_clips_at(&self, time: TimeCode) -> Vec<ActiveClip>;

    /// 吸附计算：给定位置，返回最近的吸附点
    pub fn snap_points(&self, near: TimeCode, threshold: TimeCode)
        -> Option<TimeCode>;

    /// 获取时间范围内所有的关键帧时间点
    pub fn keyframe_times_in(&self, range: TimeRange) -> Vec<TimeCode>;

    /// 计算时间线总时长
    pub fn total_duration(&self) -> TimeCode;
}
```

---

## 8. 性能设计

| 目标                | 策略                              |
| ------------------- | --------------------------------- |
| 多轨道查询 O(log n) | Clip 按 position 排序 + 二分查找  |
| 吸附响应 < 1ms      | 预计算所有 snap point 列表        |
| 关键帧求值 < 0.1ms  | 缓存最近一次求值结果              |
| 撤销栈内存控制      | 每个 command 记录最小增量（diff） |

---

## 9. Effect Controls 与关键帧编辑

当前 `mondrian-app` 已提供右侧 `Effect Controls` 面板。
面板不再硬编码属性列表，而是基于 `PropertyHost::property_bag()` 自动发现并渲染可编辑属性。
这意味着后续新增内建属性或插件属性后，面板可直接展示，无需再手工加 UI 行。
当前面板采用扁平布局，仅保留“选段类型 · 选段名称”和属性行本身，不再嵌套额外卡片。
属性按类别分组展示，当前内建分组为：

- 运动：位置、缩放、旋转、锚点
- 不透明度：不透明度、混合模式

编辑行为约定：

1. 面板仅在时间线存在选择时显示；单选片段进入可编辑模式。
2. 数值编辑默认在当前播放头位置写入关键帧（`SetKeyframe`），并进入撤销栈。
3. 秒表仅出现在可关键帧属性前面；不可关键帧属性（例如混合模式）不显示秒表。
4. 所有属性改动通过 `AppState::mutate_clip_property` 统一落库，触发：
    - `TimelineModified` 事件
    - 项目自动保存
    - 统一撤销/重做历史（`SequenceSnapshotCommand`）
5. `Linear` 与 `Bezier` 仍由底层关键帧系统支持，但面板不再占用单独的插值下拉区域。

插值求值说明：

- `Linear` 使用线性插值。
- `Bezier` 使用三次贝塞尔曲线进行时间重参数化，先解 `x(t)` 再取 `y(t)` 参与属性插值。
- 当控制点为空时，采用标准 ease 曲线默认控制点。
