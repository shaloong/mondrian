# Mondrian 产品路线图

> 更新日期：2026-07-19
>
> 当前主目标：Windows 优先，在不牺牲项目、时间、帧、色彩、音频与任务契约的前提下，完成可验证的真实项目制作闭环。
>
> 推进方式：按退出门槛推进，不以日期、代码量、类型数量或“已有 UI”代替完成度。

---

## 1. 产品目标与推进原则

Mondrian 当前阶段只围绕三个产品支柱安排优先级：

```text
能剪：编辑语义可靠、操作及时、Undo/Redo 可预测、项目不会因操作或升级损坏
能播：音频连续、视频按时钟呈现、慢素材能降级、拖动和旧任务不会拖垮交互
能交付：输入解释可信、预览与导出语义一致、输出标签和文件结构正确
```

### 1.1 成熟度定义

| 等级 | 含义 | 允许使用的“完成”表述 |
| --- | --- | --- |
| L0 可见 | 类型、UI、独立算法或实验路径存在 | 仅可称“原型”或“基础设施存在” |
| L1 可用 | 常见真实场景接入产品主路径，能保存、恢复、诊断和降级，并通过端到端验收 | 可称“Alpha/Beta 可用” |
| L2 专业 | 复杂项目、精度、性能、边界情况和互操作较完整 | 可称“专业能力” |
| L3 成熟 | 长期格式兼容、多平台、多硬件和第三方生态经过规模化验证 | 只由长期产品证据确认 |

首个可信 Beta 的最低目标如下。所有核心域达到 L1 以前，不以增加长尾格式、效果数量或平台数量代替闭环建设。

| 能力域 | Beta 下限 |
| --- | --- |
| 项目、资产与时间线 | L1 |
| 播放、缓存与代理 | L1+ |
| 色彩与 Alpha | L1+ |
| 效果、动画、标题 | L1 |
| 音频 | L1 |
| 导出 | L1+ |
| 稳定性与诊断 | L1+ |
| i18n 基础设施 | L1 |
| 跟踪、外部插件、协作 | L0 或不发布 |
| macOS/Linux | 保持可实现的适配器接缝，不要求首个 Beta 交付 |

### 1.2 不可妥协的规则

1. **“有实现”不等于“产品可用”。** 只有同时接入主路径、持久化、Undo/Redo、预览/导出、诊断和回归测试的能力才可进入 L1。
2. **基础契约先于功能扩张。** 项目迁移、时间映射、帧/色彩/Alpha、参数、音频时钟和任务取消不稳定时，不扩张依赖它们的长尾能力。
3. **Windows 先完成，跨平台现在预留。** 平台差异必须位于 `mondrian-platform*`、媒体/渲染器原生后端或明确适配器内，不得渗入项目模型、时间线语义和 UI 业务状态。
4. **预览与导出共享解释，不要求共享调度。** 输入解释、时间线求值、效果顺序、合成、Alpha 和最终输出变换必须一致；缓存寿命、优先级和是否读回 CPU 可以不同。
5. **降级必须可见且语义正确。** CPU 解码、代理、降低预览分辨率、丢弃迟到视频帧、HDR 到 SDR view transform 均可接受；静默猜测、错误色彩、隐式 RGBA8 量化和把 CPU 路径冒充 GPU 路径不可接受。
6. **保持模块深度。** 新模块必须用小而稳定的接口隐藏调度、缓存或求值复杂度；只有一个适配器且没有真实替换需求的透传模块不应被创建。

---

## 2. 代码现状审查结论

本节是 2026-07-19 的代码基线，不是目标清单。状态区分“模型/算法存在”“已接入主路径”“已有真实项目证据”，避免从类型或单元测试推断产品完成度。

| 能力域 | 已有事实 | 仍不足以宣称完成的部分 | 当前判断 |
| --- | --- | --- | --- |
| 核心时间与 ID | 作者位置/范围/曲线使用 canonical `TimelineTime` 与显式 Time Domain/Transform；`FramePosition` 仅作求值/显示 Adapter。Sequence 持久化一个显示设置并解析为 Viewer/时间轴共享的 NDF/DF/Frames 契约；版本化 fixture 覆盖 VFR PTS、23.976→29.97 嵌套、负时间和 100 小时项目 | 素材源 timecode、用户输入/解析、time-of-day/reel 语义及未来真正的 Feet+Frames 仍须各自完整产品契约，不能从显示格式化器反推“已支持” | L1 |
| 项目持久化 | `.mdp` manifest、schema v11 精确时间/跨媒体参数/Sequence author revision/统一时间显示设置文档、当前 fixture、SQLite 素材库、临时文件写入、自动保存、恢复候选和重连已接入 | Alpha 明确拒绝旧 schema、不承诺兼容迁移；替换目标文件前的持久化/崩溃语义仍需压力验证 | L1- |
| Undo/Redo | UI 外的活动 Sequence 命令历史可工作；主要编辑动作有回归测试，完整快照已改为可计量的序列化载荷，并受 200 条/128 MiB 双重硬预算和结构化淘汰诊断约束 | 跨 Sequence/Project 聚合事务仍需独立设计；超大项目是否引入 delta command 取决于基准证据，不能另建一套语义 | L1- |
| 素材管理 | 文件夹/Bin 层级、移动/重命名/删除、缩略图、离线提示、单文件/目录重连、代理模式已接入产品 UI | tags/metadata 字段尚未形成检索产品；素材使用位置反查和批量诊断不足 | L1- |
| 时间线编辑 | 多轨、移动、分割、普通 Trim、Ripple Delete、Insert/Overwrite、Roll/Slip/Slide、跨轨移动、链接片段跟随、锁定、吸附、多选和嵌套序列已有实现与测试 | Lift/Extract、显式 Link/Unlink、Track Targeting、转场 handles、反向/冻结/完整 time remap、VFR/混合帧率边界和复杂 ripple 传播未形成完整验收 | L1- |
| 播放与缓存 | UI 无关 `PlaybackEngine`、`FrameWorkBroker`、`PreviewFrameStore`、Evidence v2、Headless GPU Adapter，以及播放/拖动/静帧三种访问语义、FFmpeg session/ring/seek index、原子 generation/抢占/取消 disposition、deadline、预取和代理已接入；Broker 的请求龄期、失效龄期、deadline 与 worker 完成时刻已统一到可注入的 Monotonic Runtime Clock：Adapter 只在准入前提交不透明绝对 deadline 与剩余时长，Broker 单次降低、同键 rebind 更新并在 worker 发结果前一次性盖完成戳，因而排队不会续期、UI 延迟轮询不会制造虚假 Late；deadline/generation/抢占按最早权威时刻统一裁决，时钟回退会钳制、按回退事件计数并令门禁失败；取消原因与 request→checkpoint→return 证据已由播放域统一聚合并以 Playback/Interactive/Still 固定策略供诊断、Headless 与专业验收共用；访问模式/Broker Adapter/有界 job transport 已迁到不依赖 UI 的 `app::preview_access_mode`，deadline/执行质量/预取深度策略已迁到 `app::preview_scheduler_policy`，具体 FFmpeg worker、协作取消观察、结构化结果发布和有界关闭已迁到 `app::preview_media_task`，素材记录加完整 Viewer 意图到 canonical media key/色彩拒绝/不可用结果的解析已迁到 `app::preview_media_source`，Window/Headless 的完成、过期、终态 Delivery 与预卷顺序已统一到 `app::playback_preview`；`app::preview_execution` 原子拥有完整 generation binding、pending、执行质量、候选 ID、Viewer output key、UI 无关 GPU execution contract 与 exact registered-output reuse，Window/Headless 均直接消费该 contract；`app::native_video_import` 统一聚合 renderer/platform native import 事实与稳定 admission blocker，`app::preview_hardware_admission` 以单一快照投影硬解请求、native surface-specific downgrade 与 device selector，Window/Headless composition root 共用该状态；UI 无关 `app::preview_media_frame`、`app::preview_timeline_execution`、`app::preview_viewer_plan`、`app::preview_cpu_execution` 分别拥有解码驻留、canonical Timeline/嵌套求值及 media-demand collection、Viewer lowering 与 CPU 合成/输出语义；Timeline 执行对每个子 Sequence 按自身画布与共享运行时质量求值，Ready 结果必带 cache identity，Window 只提供 typed media outcome、消费统一的预取/preroll/输入色彩 demand 并投影执行事实；UI 无关 `app::preview_runtime` 是唯一生产组合根，按职责拥有 asset-library/proxy-dispatch Adapter、presentation、request scheduler、result pump、service lifecycle、hardware admission、evidence 与分层 diagnostics；Window `app_ui::preview` 只负责 GPU 输出 Widget 注册、CPU raster 无拷贝转换和 panel diagnostics 投影；专业验收消费 UI 无关 evidence，固定策略不可由 smoke 环境变量放宽，并以主视频/主音频流时长及声明帧数而非容器时长证明覆盖；Windows 原生 Private Commit/Working Set 探针与版本化整进程内存门禁已接入真实 cadence/terminal-stress 路径；加速 Headless 门禁已证明 30 分钟时钟数学，真实 `cpal_av_48khz_30min_v1` Adapter 也已接入产品 Audio Playback、Headless GPU、源缓存与整进程证据并 fail-closed | `app::preview_runtime::PreviewProductionRuntime<O>` 已成为 Window/Headless 共用的 UI 无关生产组合根；真实 CPAL、4K HEVC、连续播放、Seek 与取消门禁直接实例化 Headless 输出 specialization，不再借用 `WindowPreviewAdapter`；2026-07-18 已用不提交、权利未核实的本地长素材完成一次视频 v4（连续播放、100 次 seek、取消、内存平台、GPU presentation）和一次真实 CPAL/A/V v1 全绿报告，这些只能作为开发机证据，不能替代可再分发 canonical corpus、固定参考机/多驱动基线或声学 loopback | L1，单机产品路径已闭环，发布证据仍未闭环 |
| Windows 硬解/低拷贝 | FFmpeg 硬件设备/codec 探测、D3D11/D3D12 native frame 保留、D3D11→D3D12 导入、NV12/P010 GPU YUV 采样、准入与失败原因已有实现；2026-07-18 本地 2.88 秒 4K25 HEVC Main10 HLG 连续播放已取得 60/60 Ready、60/60 D3D12VA P010 原生 GPU 合成、零上传/回读/回退和约 1.8 ms GPU 执行 p95 的临时诊断帧证据；同日以本地权利未核实的 30m39s 4K59.94 HEVC Main10 HDR10 素材完成视频 v4：107,894 个 cadence 帧、100 次 seek、零不可用/错误 terminal、GPU timestamp 完整、取消 request→checkpoint 最大约 2.14 ms、整进程内存与静止收敛通过 | 单台 RTX 3050 Laptop/单驱动和不可再分发素材只能证明该开发机主路径；仍须 canonical 许可语料、固定参考机矩阵以及更多 GPU/驱动/显示链路证据 | L1 单机长期主路径已实证，发布矩阵未闭环 |
| 渲染与色彩 | working-space 合成、OCIO CPU/GPU 路径、结构化色彩/显示诊断、golden 测试、预览/导出报告对比、Windows 显示探测和 fail-closed 逻辑较深入 | 仍有 legacy/CPU/读回路径与真实显示 payload 限制；常见 Log/HDR 必须补齐参考样片端到端证明；Windows HDR 监看不能提前宣称稳定 | L1+ 架构，继续符合性收敛 |
| 效果与动画 | 稳定 `EffectId` 与 `ParameterId`、实例地址、版本化 `ParameterSchema`、定义默认值/可动画能力、受约束数值/enum/resource、三种执行插值语义、`PropertyBag`/`AnimatedProperty`、编辑器曲线预设、效果 DAG、mask、缓存策略和插件式 definition/DSL 已存在；视觉与音频 Processor 共用同一参数描述语言，项目加载会拒绝非法参数状态，内建执行按 ParameterId 精确寻址 | 只有部分声明效果生成真实 render op；文字和转场类型尚未接入时间线/渲染主路径；真实外部插件仍需 Adapter | L1- 地基，产品广度未完成 |
| 音频 | Track→Clip 已是 placement SSOT；Clip 持有 Component Edit，Sequence 持有 Processing Scope/Track Channel/Bus/Output/typed Route/Transition；Processor 参数持久化共享 Schema 快照与单一 exact curve，缺失插件可保留作者意图，内建 Gain 要求规范 Schema 精确匹配且不隐式补参数；播放与导出共用 `AudioProgramRuntime`、块式 decoder Adapter、嵌套公共输出、sample-accurate Gain/pan/fade/Transition、无隐式 clipping；semantic IR 已 prepare 为稠密拓扑 node slot、连续 Route/Contribution 区间、Transition binding、sample span、验证后的 automation event span 与 liveness scratch，执行时不扫描作者 Route/BTreeMap，也不逐样本校验或搜索作者曲线；Hold/Linear/Bezier 复用 core 的同一插值实现并保持 block partition exact；`AudioRenderSession` 在构造时冻结 `AudioRenderCapacity`，`render_into` 不扩容、不构造事件容器；静态 source map 以精确首样本 + `i128` 有理累加器覆盖分数正/反向速度；CPU scalar reference 与运行时 SIMD 共用 schedule 并有 1/8/32/64 轨 × 64/256/1024 frames 的 PCM 等价/p99 deadline 矩阵；产品 PCM 源使用文件指纹 + 10 秒窗口 + 128 项/256 MiB 加权 LRU，Runtime 仅保留 4096 帧热窗；Prepared PDC 已求解并预分配 Contribution/Route delay，根级 stateful Runtime 具备显式 generation entry、失败整代作废、Synthetic Clock 恢复与有界 RenderBlocked；本地真实 CPAL/A/V 30 分钟同步门禁已通过 | 插件参数事件 batch、真实 VST3/CLAP host、channel layout/组件选流、重采样、真实非零延迟 processor、stateful nested reverse checkpoint/materialization、send/sidechain、meter/loudness/limiter、完整编辑 UI/undo 命令、固定参考设备矩阵与声学 loopback 仍未完成。GPU 只作为计入 PDC/批量延迟且能证明 deadline 的可选 processor backend，不能成为 callback 依赖 | L1+ 执行地基；不宣称 DAW 完成度 |
| 导出 | 后台队列、取消、时间线逐帧合成、音频混编、FFmpeg 编码、色彩标签/HDR 元数据约束、结果 probe/校验和诊断已存在 | 产品 UI 主要暴露 H.264 预设；Windows 硬编检测未落地主路径；HEVC Main10、专业中间格式和长项目需真实 roundtrip，不以 enum/FFmpeg 参数单测视为交付 | L1- |
| 自研 UI | winit/wgpu 产品入口、retained widget、主题 token、事件/焦点/IME、Dock、面板和大量组件测试已建立 | 交互一致性和无障碍仍需真实工作流验证；产品字符串大量硬编码，中英文混用，尚无 message ID/pseudo-locale 基础 | L1-；i18n 为 L0 |
| 插件 | 内部效果 definition、graph DSL、能力/缓存/失败隔离契约已有 | 尚无稳定外部 ABI、包加载/权限/隔离/兼容矩阵；当前只能称内部扩展接缝 | L0 |
| AI | provider/workflow/orchestrator 类型和测试存在 | 主要动作仍为 stub，不应进入核心发布承诺 | L0 |
| 跨平台 | 平台 trait、Noop adapter、Windows 实现、非 Windows 的明确 unsupported 结果已存在；CI 编译/测试 Linux 与 Windows 的非 app 或部分 app 路径 | 原生硬解/显示管理主要为 Windows；macOS/Linux 产品入口、安装和真实 GPU/音频门禁未完成 | 接缝存在，实现后置 |

### 2.1 当前最重要的结构性风险

1. **产品证据弱于代码广度。** 测试数量很多，但 CI 主测试排除了完整 `mondrian-app`，真实 GPU、真实硬解、真实音频设备和长项目主要依赖手动/ignored smoke。
2. **关键复杂度已按行为形成明确 Locality。** Window/Headless 的完成收割、终态 Delivery 与预卷顺序统一在 UI 无关 `app::playback_preview`；`app::preview_execution` 原子拥有 generation binding、pending、执行质量、候选 ID、完整 output key、UI 无关 GPU execution contract 与 exact registered-output reuse；`app::preview_media_source` 从不可变素材记录与完整 Viewer 意图唯一解析 source/proxy 指纹、色彩/range/Alpha、native surface、代理意图与 canonical decode geometry，缺文件和色彩拒绝不再坍缩为无原因的 `None`；`app::preview_media_task` 独占具体 FFmpeg worker、协作取消观察、结构化终态结果和有界关闭，Window/Headless 不再各自拥有解码循环；`app::preview_media_frame` 以封闭单 payload（working CPU/source-domain/native surface）拥有解码驻留、惰性 working adaptation、质量/provenance、逻辑/采样几何及 reservation，空帧和矛盾 residency 已不可表达；`app::preview_timeline_execution` 是 canonical render-plan traversal、嵌套 Sequence lookup/depth、每个 Sequence 独立画布/共享 runtime quality、嵌套 working-space 合成与转换、typed pending/unavailable、Ready 必备 cache identity 和有序 execution facts 的单一实现；其 read-only media-demand collection 同时服务 prefetch、preroll 与输入色彩 evidence，Window 不再维护调度专用 nested walker；`app::preview_viewer_plan` 统一拥有 resolved element、稳定 cache identity、质量/provenance 聚合、deferred composite 分类与 GPU lowering；`app::preview_cpu_execution` 统一拥有 working-linear preparation/composite、Program Output、monitor adaptation、完整执行事实与阶段耗时；`app::preview_runtime::PreviewProductionRuntime<O>` 是 Window/Headless 共用的唯一生产组合根，`app::preview_frame_store::PreviewFrameStoreAdapter` 是其唯一 Frame Store Adapter；`app::preview_raster_frame` 拥有最终 CPU raster 的有效性、编码色彩、资源身份和内存 reservation，缓存与 stale pin 不再保存 Widget payload；跨 Renderer/显示契约/Window/Headless 的 GPU output blocker taxonomy 与 aggregate 由 UI 无关 `app::preview_gpu_output_blocker` 拥有；缓存驻留/淘汰算法由 `mondrian-playback::PreviewFrameStore` 拥有，GPU/CPU 色彩与合成数学由 renderer 拥有。`app::preview_runtime` 以 media adapter、presentation、request scheduler、result pump、service lifecycle、hardware admission、evidence 与分层 diagnostics 深 Module 保持 Locality；`app_ui::preview` 只保留 GPU 输出 Widget 注册、CPU raster 无拷贝转换和 panel diagnostics 投影；timeline evaluation 文件只适配 media outcome 与 execution facts，不再拥有递归语义；presentation 单独拥有最终 GPU/Raster/stale/CPU output 仲裁及唯一 `PreviewRasterFrame`→`ViewerFrameImage` 转换。该具体 Adapter 主协调器已完成职责级收敛；performance diagnostics 作为一套完整版本化规则书保留 locality，不按行数机械切碎。导出 queue 仍较大，后续仅按完整职责继续深化。
3. **项目版本化只有“拒绝”，没有“迁移”。** 这在 Alpha 继续变更数据结构时会快速成为真实项目风险。
4. **声明能力和视觉执行能力可能分离。** 某些效果、文字和转场已有类型或属性，却没有主路径 render op；路线图不得把它们列为已完成。
5. **音频/视频运行许可必须持续分离。** 当前 `Priming` 已可预填 PCM 但禁止设备提前消费，普通视频 Late/Recovering 也不会旋转音频 generation；本地真实 CPAL/A/V 30 分钟报告已证明一次单机产品路径，仍须在固定参考机矩阵覆盖慢首帧/seek、持续视频压力、设备失效和声学 loopback，才能形成发布级长期证据。

---

## 3. 目标架构：现在冻结接缝，后续增加适配器

以下结构在 M0 固化。它们不是要求重写已有代码，而是把现有较强实现收敛到明确所有权，避免后续效果、平台或格式扩张时复制语义。

```text
ProjectDocument + Migration Registry
              │
              ├── Editor Commands / Undo Transactions
              ├── Asset References / Relink
              └── Cache Semantic Revision

Media Access Contract ──> Playback Engine ──> Viewer Adapter
        │                       │
        │                       └── Audio Master Clock
        └── Still/Export Adapters

Timeline Frame Evaluation Contract
        ├── Viewer scheduling adapter
        ├── Export scheduling adapter
        ├── Thumbnail adapter
        └── Proxy adapter

Parameter Schema
        ├── Effect/Transform/Audio UI
        ├── Animation curves
        ├── Presets
        ├── Tracking output
        └── Future plugin adapter

Platform Capability Contract
        ├── Windows adapters（当前）
        ├── macOS adapters（后续）
        └── Linux adapters（后续）
```

### 3.1 项目文档与迁移

- `ProjectDocument` 是规范持久化语义；UI 临时状态、纹理句柄、线程状态和翻译后文本不得进入项目文件。
- archive format、document schema、SQLite schema 分别版本化，迁移按有序步骤执行；禁止用 serde 默认值无声吞掉语义变化。
- 每个迁移必须具备：旧 fixture、升级后不变量、再次保存/打开、失败不覆盖源文件、重复执行安全性。
- 缓存、代理、波形和缩略图不嵌入项目；它们的 key 必须包含足以反映项目语义和源文件 revision 的字段。
- 新字段必须定义缺省语义、旧版本迁移语义和 downgrade/unsupported 行为。

### 3.2 编辑命令与 Undo

- 时间线变更只经 editor/domain command 进入，Widget、Viewer 和平台回调不得直接修改 `Sequence`。
- 一次用户意图对应一个 undo transaction；链接片段、转场、marker、字幕、音频自动化等关联变化必须原子提交或全部失败。
- 在保持现有快照命令兼容的同时，为高频/大型操作引入语义 delta 或 copy-on-write 策略；设置明确的历史内存预算与淘汰诊断。
- 命令测试覆盖 execute → undo → redo、失败回滚、锁定轨道、跨序列引用和保存/重开后的最终语义。

### 3.3 播放引擎与媒体任务调度

- 播放引擎是深模块，拥有播放状态、音频主时钟、当前帧选择、预读、late-frame 策略、seek generation 和可观测性；UI 只提交意图并消费状态。
- 保留 `PlaybackCursor`、`ScrubCursor`、`RandomAccessStillFrame` 三种媒体访问语义，不为新调用方增加绕过它们的 FFmpeg helper。
- 预览、scrub、缩略图、波形、代理和导出共享任务优先级/取消语言，但可使用不同执行池。最低优先级顺序为：

```text
实时音频 > 当前待显示帧 > 最新 seek > 短预读 > 邻近 scrub 帧 > 缩略图 > 波形 > 代理 > 后台导出
```

- 每项任务必须携带 owner、priority、generation/cancellation、deadline（如适用）、资源预算、结构化结果；旧 generation 不得发布 UI 状态。
- 跨域统一只包含 `ExecutionPriority`、Adapter 单次降低的 deadline 与 terminal disposition/evidence 等值语义；禁止建立包揽预览、波形、缩略图、代理和导出的通用 Job 枚举或万能执行池。各深 Module 必须独立拥有容量、抢占/重试、worker locality 与资源预算。
- Preview 媒体任务只有一个 App 层生产实现：同一深 Module 必须完整拥有 job 消费、FFmpeg 调用、codec checkpoint 取消观察、execution lease 结算、结构化终态结果和有界关闭；Window、Headless 与未来平台适配器只能组合或消费它，不得复制解码循环、取消权威或事后猜测失败原因。
- 任务调度不得成为只转发到现有 worker 的浅模块。当前 Frame Work Broker、UI 无关 Waveform Analysis Service、Thumbnail Execution Service 与 Proxy Generation Service 已作为四种独立 Implementation 消费统一语言；导出迁移时仍须证明离线执行的深策略，不能退化为共享池调用。

### 3.4 帧、色彩与 Alpha 契约

- 帧必须明确：像素/纹理格式、数值范围、primaries、transfer、matrix、range、bit depth、alpha 模式、CPU/GPU residency 和所有权/同步。
- working intermediate 以 float/linear 为默认；legacy RGBA8 只能带结构化原因存在，不能作为未记录的内部捷径。
- 预览、导出、缩略图和代理通过同一输入解释与时间线求值契约；最终 display/output boundary 可由各适配器选择。
- 任何 GPU readiness 必须由真实帧执行证据确认，不能由 capability probe、shader cache 或 render plan 单独推断。

### 3.5 参数与动画 Schema

每个可编辑参数必须具备下列持久化契约。property path 只是实例级 UI/命令地址，不是身份，也不提供 Alpha 旧文档兼容：

- 稳定 `ParameterId` 或等价稳定机器标识，不能依赖显示名或 UI 顺序。
- 自动化位置与时间手柄使用规范化精确有理 Timeline Time，并声明 Sequence/Clip/Transition/Source 等所有者时间域；视频帧、音频 sample、UI snapping 和 SMPTE 只是在边界解析的网格/显示。
- 类型：bool、int、float/double、enum、color、point/vector、curve、text/resource reference。
- 单位与解释：pixel、normalized、percent、degree、frame/time、stop、nit、dB 等；无单位也必须显式。
- default、hard range、soft range、step、非法值策略。
- 是否可动画、允许的插值、空间/时间曲线语义。
- schema version、序列化格式和迁移策略。
- UI message ID、控件提示与 reset 行为；不把翻译后文本当 ID。
- 参数自身声明缓存影响，以及值变化是否可能改变资源或拓扑。
- 颜色/Alpha 处理域、CPU/GPU 能力、确定性、时间范围和 ROI 属于 Processor/Effect Definition 与编译图；参数只声明会触发哪类重新编译/失效，禁止两处复制能力真相。

效果、Transform、音频自动化、标题、跟踪结果、preset 和未来插件必须复用这套 schema，不建立彼此不兼容的参数系统。

### 3.6 音频图

- 项目/序列明确采样率和 channel layout；所有内部混音使用 float，输入统一重采样后进入图。
- 以 Track→Clip 为唯一 placement authority；Clip-local Component Edit 与 Sequence-owned Processing Scope 分离，通过受限 `{scope_id, scope_in}` 绑定，避免第二套范围/速度模型。
- 建立 Compiled Contribution → Track Mixer Channel → Mix Bus → Program Output 的类型化求值顺序；Scope/Track/Bus/Output 复用 Processor Rack/Instance 作者模型，gain、pan、fade、mute、Transition、路由和延迟能力属于同一图契约；solo 仅是 audition overlay。
- 作者数据归 Sequence/`mondrian-timeline`；`mondrian-audio` 负责验证后编译、Render Contract、独占 Session、公共 DSP、嵌套 Runtime 和媒体源接口，但不依赖 FFmpeg、CPAL、平台 UI 或 App。暂不拆格式占位 crate。
- 播放、导出、分析和 audition 消费同一不可变编译语义；旧 flat mixer 已删除，禁止恢复消费方私有混音或隐式 `tanh`/limiter。
- 实时 callback 禁止分配、文件 I/O、格式化日志、等待 decode worker 或锁住项目/UI 状态。
- 健康音频设备以实际消费的 sample position 为主时钟；设备丢失或宽限期结束后连续切换到基于单调时钟的 Synthetic Clock Master，而不是切到 video master。视频来不及时丢帧、重复或降质。只有启动预卷、设备切换或无法维持连续播放时才进入明确 buffering/recovery。

### 3.7 平台与原生后端

- `cfg(target_os)` 和 OS handle 只存在于平台、媒体/渲染原生后端或应用启动适配器；核心模型和 Widget 不含平台分支。
- Windows、Noop/headless 是当前真实适配器；macOS/Linux 后续实现同一 capability contract，不复制业务逻辑。
- Unsupported、degraded、ready 都是结构化能力状态。非 Windows 先返回明确 unsupported，也不能假装成功或静默使用错误格式。
- 项目文件、时间线语义、效果参数和 golden frame 期望不因平台分叉；允许后端性能不同，不允许结果语义未经说明地不同。

---

## 4. 正确性与降级政策

### 4.1 所有专业能力只有三种运行状态

| 状态 | 要求 | 示例 |
| --- | --- | --- |
| Verified | 有真实输入证据、完整执行路径和 reference/golden 验证 | Rec.709 limited H.264 正确解码、预览、导出并重导入 |
| Explicitly degraded | 结果仍正确，质量/性能降级被记录且用户可理解 | HEVC 硬解不可用后切换代理；HDR 素材经明确 view transform 映射到 SDR |
| Blocked/Unresolved | 无法保证正确时拒绝输出错误结果，并给出原因和修复动作 | 识别到 S-Log3 但 OCIO transform/元数据冲突无法解析 |

禁止第四种状态：**“识别或声明支持，但使用猜测参数显示/导出一个看似正常的结果”。**

### 4.2 色彩 Beta floor

色彩从功能扩张主线转为持续符合性保障线。在其他核心系统达到 L1 前，仅完成以下范围和回归：

**输入**

- Rec.709 limited/full、sRGB 图片、BT.2020 HLG、BT.2020 PQ。
- Apple Log、S-Log3/S-Gamut3 系列、ARRI LogC4 等常见 Log 只在“元数据或用户 override → 可解析 transform → reference 验证”整链成立时标记为 Verified。
- CICP、容器/codec side data、ICC presence 和用户 override 分开保存证据；冲突不能被后出现的字段静默覆盖。
- missing metadata 是未解析事实，不是“检测为 Rec.709”；可由项目 policy 假设，但报告必须区分检测和 policy assumption。

**工作与合成**

- 一个明确 scene-linear working role；float intermediate；premultiplied/straight alpha 的转换点显式且有测试。
- Blend、mask、effect、adjustment layer 在约定域执行；不允许单个 CPU 节点把整链无声量化为 RGBA8。
- CPU/GPU/预览/导出允许不同执行方式，但 reference vectors、golden frame 与健康报告语义必须一致。

**显示与输出**

- Windows SDR viewer 必须可信；HDR/Log 到 SDR 使用明确 view/tone-map transform。
- Windows HDR viewer 在完整 output texture → UI composite → swapchain → monitor contract 未闭环前标为 experimental 或 blocked。
- Rec.709 H.264/AAC 与 HLG/PQ HEVC Main10 写入正确 primaries/transfer/matrix/range；静态 HDR metadata 只有字段完整时才写入。
- Camera Log 不使用消费 codec 的普通 delivery 标签伪装输出；只允许经验证的专业中间格式或明确拒绝。

**暂缓扩张**

- 完整相机 RAW、所有厂商 IDT、动态 HDR10+/Dolby Vision、专业 SDI、任意显示校准工作流、macOS EDR 和 Linux HDR。

### 4.3 通用 Definition of Done

一个功能只有在适用项全部通过后才可标记完成：

1. 产品 UI/命令可达，且禁用/失败状态明确。
2. Viewer 主路径可用；涉及画面或声音时 Export 结果语义一致。
3. 保存、关闭、重新打开不丢失；涉及 schema 变化时有迁移 fixture。
4. Undo/Redo 正确且一次用户意图只产生一个 transaction。
5. 缓存 key/失效正确；源文件替换、relink、参数变化不会复用陈旧结果。
6. 代理与原片在时间、构图、颜色、Alpha 和音频语义上等价。
7. 长任务可取消；旧 generation 不发布结果；退出不在 UI 线程等待不可控 I/O。
8. 有结构化 diagnostics，能区分 supported、fallback、blocked 和 failed。
9. 有 domain test、主路径 integration test；关键视觉/音频能力有 reference/golden 或 roundtrip。
10. 不在 UI 或实时音频线程执行解码、磁盘 I/O、shader/processor 构建或大对象分配。
11. 文档、能力矩阵和用户可见状态与代码一致。

---

## 5. 验证资产与发布门禁

### 5.1 Reference Corpus

M0 建立可重现的 corpus manifest。规范清单当前只保留许可边界明确的自有/可生成色彩参考，主工作流素材仍不足。每个样本记录来源许可、SHA-256、容器、codec、分辨率、帧率模式、bit depth、CICP/side data、预期解释和可公开性。

最低覆盖：

- 1080p/4K H.264 8-bit Rec.709 limited/full。
- 4K HEVC Main10 Rec.709、HLG、PQ。
- AAC、PCM/WAV，44.1/48/96 kHz，mono/stereo，至少一种多声道输入。
- PNG/JPEG、sRGB PNG alpha、灰度/数据类图片。
- Apple Log、Sony S-Log3/S-Gamut3.Cine、ARRI LogC4 的已知正确 reference；包含 metadata 正确、缺失、冲突三类。
- CFR 与至少两种真实 VFR 手机素材；24/25/29.97/30/50/59.94 fps 混合。
- 长 GOP、损坏尾部、离线/替换文件、极短片段、无音频和仅音频素材。

公开 CI 使用体积受控子集；明确取得内部测试权利但禁止再分发的大型素材可由 manifest + 本地/夜间 runner 使用。许可未核实的下载/遗留样片只能进入 ignored manual 诊断，不得登记为规范 fixture、不得满足发布或专业门禁。禁止测试在运行时从不固定 URL 下载“最新样本”。

### 5.2 Golden Project

建立 5 分钟规范项目，至少包含：

- 4K25 HEVC Main10 HLG、4K/1080p Rec.709 H.264、sRGB PNG alpha、WAV + AAC。
- 3 条视频轨、4 条音频轨、链接音视频、嵌套序列。
- 标题、转场、基础 Transform、关键帧、颜色调整、LUT、mask、速度变化。
- 代理/原片切换、至少一个离线再重连素材、序列 In/Out。

每个候选构建必须完成：打开 → 播放 → seek/scrub → 编辑 → undo/redo → 保存 → 重开 → 导出 → 重导入 → 校验。任何一步只能通过自动或记录明确的人工证据，不允许用“单元测试覆盖了相关函数”替代。

### 5.3 Stress Project

建立 30–60 分钟压力项目，用于：

- 连续播放、频繁 seek、长 GOP、缓存淘汰和内存平台。
- 代理生成/切换、relink、自动保存与恢复。
- 长时间音频输出、underrun、A/V drift 和设备切换。
- 取消导出、失败重试、磁盘空间不足、输出路径不可写。
- 从每个受支持旧 schema 迁移到当前版本。

### 5.4 性能与正确性基线

M0 固定 Windows 参考机的 CPU、GPU、内存、存储、显示器/HDR 状态、驱动、FFmpeg 和构建 profile。数值在参考机固定前是初始目标；固定后只能通过有理由的基线变更调整，不能为让回归变绿而放宽。

| 指标 | Alpha/Beta 门槛 |
| --- | --- |
| 支持硬解的 4K23.976-60 HEVC Main10 | 1× 连续播放，无持续掉帧；实际 hardware/native path 有帧级证据 |
| 不支持硬解或压力过高 | 自动选择 1/2、1/4 或代理；UI 不冻结，降级原因可见 |
| warm seek 首张可用图像 | p95 ≤ 200 ms |
| accurate seek 稳定到目标帧 | p95 ≤ 500 ms |
| 常用时间线操作 | UI event → model/paint 可见反馈 p95 ≤ 50 ms |
| 连续播放 30 分钟 | 真实 CPAL callback（声学链路另以 loopback 验证）；无 underrun/recovery 或 render-to-silence；A/V drift 绝对值 ≤ 20 ms；当前视频 Ready ≥ 99.5%；分钟 5–10 与 25–30 的整进程 Private Commit 平均增长 ≤ 256 MiB、全程 ≤ 4 GiB，terminal stress 静止后仍收敛 |
| 100 次跨区 seek | 旧任务不发布；结束后缓存回到预算内，不单调增长 |
| Golden Project 导出 | 连续 3 次成功；时长误差 ≤ 1 video frame；A/V 起止误差 ≤ 20 ms |
| 项目恢复 | 恢复点可打开；正常情况下最大数据损失不超过配置的 autosave interval |

### 5.5 CI 分层

**每个 PR 必须：**

- `cargo fmt --all -- --check`。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`。
- workspace unit/integration tests，且完整 app domain/action/project tests 不再仅因 crate 名被整体排除。
- 项目 schema/migration fixtures、timeline semantic tests、renderer golden、preview/export parity、UI component extremes。

**Windows nightly/专用 runner 必须：**

- Reference Corpus 真实解码、硬解准入和 native import；记录 GPU/driver/codec evidence。
- Golden Project 完整工作流、真实 CPAL 输出或 loopback 验证、30 分钟播放、seek/memory stress。
- H.264/AAC 与 HEVC Main10 roundtrip、色彩标签、range、duration、A/V sync、可重导入。
- 真实 wgpu output/display smoke；unsupported 显示模式也必须得到预期 blocker，不得跳过为“通过”。

**发布候选必须：**

- Stress Project、升级/卸载/重装、日志打包、崩溃恢复和无网启动。
- 所有 P0/P1 blocker 为零；所有允许 fallback 都列入发布说明和 capability report。

---

## 6. 里程碑

里程碑没有硬编码发布日期。只有退出门槛全部满足，才进入下一阶段；允许在不扩大 WIP 的前提下提前准备下一阶段基础设施。

## M0 — 核心契约与架构收敛

**目标：** 保留现有实现，先消除会导致后续重复、虚假完成或项目不兼容的结构风险。

### 要求

**项目与编辑**

- [x] 建立 archive/document/SQLite version registry，并提供 schema v11 current document fixture、v0 SQLite fixture、幂等打开、事务回滚和失败不覆盖测试；Alpha 不保留旧 document schema 兼容。
- [x] Project document save revision 与持久化 `SequenceRevision` 已分离；每次作者事务、Undo/Redo 及继承的 Project 语义变化单调推进 Sequence revision，Playback 不再拿保存代次冒充 Timeline revision。Track/Clip/Effect/Mask/Parameter/Keyframe 与全部音频作者实体保留稳定强类型 ID，以 Sequence revision 作保守失效、以定义/内容/资源指纹作精细失效；当前文档会拒绝零 revision、重复 Sequence/Track/Clip/Effect/Mask/动画轨身份、重复 effect-local Parameter/曲线 keyframe 身份和悬空强 Clip 引用。
- [x] 高频 Timeline、Inspector、Viewer 变换与关键帧编辑统一提交 Sequence snapshot command；历史 Module 按目标 Sequence 失败关闭，默认同时硬限制 200 条与 128 MiB command-owned retained bytes，使用可精确计量的序列化快照和 `VecDeque` 常数时间淘汰，累计报告预算淘汰、分支 Redo 丢弃和超大命令未保留。历史构造/入栈失败会回滚作者 Sequence，Undo/Redo 失败不吞命令；选择和导航仍明确不是作者事务。
- [x] 精确 Timeline Time、显式 Time Domain/Transform、Frame/Sample Evaluation Grid 已落地并删除旧 `TimeCode`/`TimeTicks`。Sequence schema v11 只持久化一个 `TimelineDisplaySettings`，领域层解析为 Viewer/时间轴共享的有效 `TimelineDisplayContract`；Frames 与 SMPTE NDF/DF、signed timecode origin、负位置和 24 小时标签回绕均不改变作者时间。版本化 `timeline_time_contract_v1` fixture 通过不规则 VFR PTS、23.976 child→29.97 parent 显式嵌套变换、负时间和 100 小时项目验证精确投影；未实现完整电影尺语义的 Feet+Frames 已从类型和 UI 删除而非虚假暴露。

**播放与任务**

- [x] `PlaybackEngine`、`FrameWorkBroker`、`PreviewFrameStore`、Monotonic Runtime Clock、Playback/Frame Cancellation Evidence 与 Headless GPU Adapter 已从 UI 收敛；Broker execution lease 是优先级/访问类/worker lane 驻留证据的单一事实来源，App activity 原子计数已删除；访问模式/Broker Adapter/有界 job transport 位于 `app::preview_access_mode`，canonical media-source resolution 位于 `app::preview_media_source`，具体 FFmpeg worker、codec checkpoint 取消、终态结果和有界关闭位于 `app::preview_media_task`，Window 与 Headless 的“单次 Demand 采样→完成/过期→终态 Delivery→预卷”统一在 `app::playback_preview`；decoded/native payload 与单一 lazy CPU working adaptation、canonical Timeline/嵌套执行、Viewer plan、working-linear CPU composite/raster boundary 也已分别下沉为 UI 无关 App Module。`app::preview_timeline_execution` 对每个 Sequence 使用自身画布与同一运行时质量，统一嵌套 working-space 合成、typed media outcome、Ready cache identity、有序 execution facts 以及 prefetch/preroll/输入色彩共用的 media-demand collection；asset-library/proxy-dispatch adaptation、request scheduler、bounded result pump、service lifecycle、hardware admission、evidence、presentation 与分层 diagnostics 已按行为所有权迁入 UI 无关 `app::preview_runtime`；timeline evaluation 只适配 media outcome 与 execution facts，request scheduler 只做需求消费与 Broker 准入，Window Adapter 只投影输出。应用层 `PreviewRasterFrame` 已成为 CPU raster cache/stale-reuse 的单一 payload 契约，Window presentation 只在最终呈现时无拷贝转换为 Widget；最终 GPU/Raster/stale/CPU output 仲裁仍只在 presentation Module，且未扩大跨层 Interface。
- [ ] 帧工作已统一 generation、priority、deadline、原子取消 disposition、请求龄期、资源预算和诊断所有权；deadline 在 Adapter 准入边界以“不透明绝对值 + 当前剩余时长”提交，由 Broker 单次降低、同键 rebind 更新，并在 worker 发布前一次性记录完成时刻；首次 Broker close 同样以 Monotonic Runtime Clock 固定取消时刻，重复 close 不续期，App stop flag 不再拥有计时权；精确 Headless 测试覆盖截止边界、最早取消原因、rebind、close age 和晚轮询不制造 Late，时钟回退会钳制并使性能/专业门禁失败。`mondrian-core::execution_work` 现只统一 priority/deadline/terminal evidence 值语义而不拥有调度。独立 Implementation `app::waveform_service` 已取代 UI cache，以 Asset+source revision、512 项 demand/16 项 worker transport、bounded LRU/failure/evidence、generation cancellation、私有 16 MiB windowed decode cache 和流式 peak accumulator 完成生产迁移；`app::thumbnail_service` 已取代 Window worker/cache，以 source fingerprint + 完整色彩 contract、512 项 demand/16 项 worker transport、128 MiB 加权 LRU、bounded failure/evidence、generation cancellation、发布所有权和数量/时间双预算 completion pump 完成迁移；`app::proxy_generation` 已取代进程全局无界 FIFO 与 Preview 私有去重表，以 AppState 实例所有权、source fingerprint + artifact/color contract、512 项准入、User/PlaybackRecovery/Import 公平队列、同键提升、cache-root 出队许可、256 项可恢复失败、512 项终态证据和项目 generation 取消完成迁移，媒体层在 limiter wait、FFmpeg 10 ms checkpoint 与发布前观察同一 token。此项仍未完成：导出须迁移统一 intent/evidence 并保持离线资源隔离。
- [ ] FFmpeg open/stream-info/seek/packet I/O 已接入 request-scoped interrupt，外部 still 子进程可 kill/wait/join；播放域已统一 5 ms request→checkpoint、50 ms Playback/Interactive return、500 ms Still return 的 fail-closed 门禁，仍须在固定参考机用 pause/seek/close/quit、真实长 GOP、阻塞 I/O 与驱动路径取得最坏延迟证据，且 UI 线程不得 join worker。
- [ ] `ProcessMemoryProbe` 已建立无 OS 调用的接口与 Windows Process Status Adapter；专业门禁以固定 cadence 聚合 5–10/25–30 分钟 Private Commit 稳定窗、4 GiB 绝对上限和各门禁声明的 terminal stress 静止后收敛，unsupported/缺样/探针错误失败关闭；视频 v4 还要求主视频流时长及声明帧数覆盖，且专业 cadence/timeout/latency/readiness/hardware 阈值不读取 smoke 调参；2026-07-18 本地视频 v4 与 CPAL/A/V v1 均已通过该内存合约，仍须用自有、明确许可或确定性生成的参考素材在固定参考机矩阵复验并据实校准版本化阈值。

**帧、参数与音频**

- [x] Frame/Color/Alpha contract 已冻结为统一 `ColorFrameDescriptor`：画幅、图域、外部/working/device 颜色身份、sample encoding、CPU/GPU residency 与 `StraightCoverage`/`PremultipliedCoverage`/`Opaque` alpha association 缺一不可。CPU typed frame、GPU handle/resource table、native NV12/P010 import、OCIO stage、working composite、Viewer spatial、ICC calibration 与 export/readback 均传播或验证该契约；公共 working/color seam 只接受 straight/opaque，premultiplied 只允许作为显式空间滤波内部帧，shader flags 从 descriptor 推导。非法 alpha 在 OCIO、Viewer 与 calibration 规划期以结构化错误失败关闭；legacy RGBA8 composite、GPU output fallback 和 stage blocker 继续由 renderer-owned typed path/reason breakdown 报告，类型存在或 shader 可创建不等于能力可用。
- [x] 视觉与音频 Processor 参数共用稳定 ParameterId、定义默认值/可动画能力、独立实例地址、单位、enum/resource、hard/soft range、Hold/Linear/Bezier 执行语义、cache impact、schema version/message ID；UI 编辑预设只生成 Bezier handles。视觉链已贯通 UI、精确动画、持久化、编译图和 Viewer cache；音频实例持久化 Schema 快照与单一 exact curve，内建编译要求规范 Schema 精确匹配；项目加载与效果注册拒绝非法 schema。颜色域、CPU/GPU、确定性、时间范围与 ROI 仍只由 Processor/Effect Definition 和编译图拥有。
- [x] Track/Clip placement → Component Edit/Scope → Track/Bus/Program Output、Gain/pan/fade/Transition、headless compiler、recursive nested Runtime 和 reference PCM 已进入 `mondrian-audio`；播放/导出共用 decoder Adapter 和执行语义，旧 flat mixer 已删除。
- [x] `AudioDecodedSource` 已改为可失败的精确 interleaved block Interface；播放/导出共用文件指纹与 128 项/256 MiB 加权 LRU 的十秒 PCM 窗口，Runtime 仅保留对齐 4096 帧热窗，不再以整文件 PCM 作为产品执行源。
- [ ] source Seam 已从逐样本调用改为每个 Contribution 每块一次的索引化交错读取；semantic IR 已 prepare 为稠密 schedule、连续 Route/Contribution 区间、Transition binding、精确 sample span、预分段 automation event span 与 liveness scratch，CPU scalar reference/SIMD kernel 已建立；`dense_schedule_v2` 固定参考机矩阵覆盖 1/8/32/64 Track、0/2/8/16 Bus、显式 Track→Bus→Output Route 与 64/256/1024 帧 block，普通 CI 另有三 Bus scalar/SIMD PCM parity。Prepared latency solver 已在每个 Contribution→Track 与 port-specific Route→Bus/Output 汇合点计算补偿，累计 pre/post rack 与 Program Output latency；嵌套 Runtime 自底向上传递实例级 child-output latency 与 state-entry obligation，缺失依赖失败关闭而不猜 0。Session 已为每个 Contribution/Route 补偿输入预分配 fixed interleaved delay line，容量报告记录非零 line 与 retained samples，普通 CI 证明跨 block partition PCM 一致；含历史的计划必须用 fresh continuity epoch + exact first sample 显式进入，只接受连续 block，失败后 epoch 中毒且复用/跳块均拒绝。Offline export 已在精确范围起点进入状态；Realtime Playback generation 已穿过每个 PCM work request，每代首块显式 Enter、后续 Continue，Timeline Adapter 拒绝重复/缺失 entry、generation mismatch 与非连续坐标，不从 start sample 猜 reset。Renderer 明确声明 independent windows 或 generation-owned state；前者失败才可补等长静音，后者的执行失败或 PCM 合同违例会废弃整代、清空旧 PCM、提交最终设备观测切换 Synthetic Clock，并从 Playback 权威坐标以新代重新 Enter，因此根级 stateful plan 已可准入；连续失败代际使用有界恢复预算，耗尽进入 RenderBlocked 且不再调度，显式 reprime/源重绑/设备重开才开始新周期，完成 preroll 则清零连击。Stateful nested instance 已拥有独立 epoch 与预分配索引顺序表：非递减 child time-map 会从首个精确样本懒进入并补算所有跳过历史，root discontinuity 会使 child 重新进入；无状态 child 仍支持正反向任意索引。通用 stateful reverse 在 checkpoint/materialization/processor-specific reverse 能力落地前于构建期失败关闭，执行期另有防御，绝不输出随 block 分割变化的伪结果。当前可执行 Gain 仍严格为零延迟；下一个非零 processor 必须同时交付真实 processor state、自身 state-entry/latency/deadline 合同和嵌套正向矩阵，不能只借 PDC 基础设施宣称支持。继续扩展真实 processor deadline 矩阵与插件参数事件 batch；补齐 channel layout/组件选流/重采样，再实现隔离的 VST3/CLAP host、send/sidechain、meter/loudness 和编辑 UI/命令；GPU 仅允许作为声明固定批量延迟、计入 PDC、设备丢失与状态切换可控且实测优于 CPU SIMD 的可选 processor backend；callback 实时安全、underrun 与 A/V drift 继续由真实门禁证明。

**验证基础**

- [x] 建立版本化 Reference Corpus manifest、Golden/Stress Project 机器可读契约、Windows 参考机 profile、机器证据采集脚本和分层校验门禁；完整素材角色与真实执行仍按 M1/M2 退出门槛验收。
- [x] 将 capability probe、逐帧 decode provenance、最终 Viewer GPU completion 与 fallback/blocker 写入同一结构化报告，同时保持 media/renderer/playback 的诊断所有权；预取 aggregate 不得代替已呈现帧证据。
- [ ] 清理路线图与规格中的错误声明：类型存在、效果可选择、shader 可创建都不得自动写成产品支持。

### 退出门槛

- schema v11 current fixture 可保存、重开；旧/未来 schema 明确拒绝；故意失败不会破坏源文件。
- Headless 测试可驱动 play/seek/cancel，并以手动 Monotonic Runtime Clock 精确验证请求年龄、过期边界、最早取消原因、同键 rebind、worker 完成与 UI 轮询解耦以及回退证据，而不构造 Widget 或 native window。
- 一个参数从 schema → UI → animation → save/reopen → preview/export → cache invalidation 全链通过。
- 音频时钟、video target selection 和 fallback 决策可由结构化报告关联到同一次播放。
- corpus、Golden/Stress 项目和参考机都有可复现说明，不是空 README。

## M1 — Windows Alpha：完成 5 分钟真实项目

**目标：** 一个不理解内部架构的用户能独立完成包含常见 SDR/HDR、音频、标题、转场、基础效果和关键帧的 5 分钟项目，并得到可重复导出。

### 项目与素材

- [ ] 后台导入并正确探测 H.264、HEVC、AAC、PCM/WAV、PNG/JPEG；不支持/损坏文件不阻塞 UI。
- [ ] Bin、重命名、缩略图、离线占位、单个与目录重连可完成 Golden Project；relink 触发正确 cache/waveform/proxy invalidation。
- [ ] autosave、异常退出恢复、dirty/close 提示和另存为在真实项目上通过；恢复来源、时间和冲突可见。

### 编辑手感的最低闭环

- [ ] 巩固 Select、Cut、Move、Trim、Ripple、Roll、Slip、Slide、Insert、Overwrite、Delete 和 snapping 的 UI 可发现性与边界反馈。
- [ ] 补齐 Lift/Extract、显式 Link/Unlink、Track Targeting；链接片段与锁定/静音/可见状态行为一致。
- [ ] 实现可交付的 speed、reverse、freeze frame；复杂 time remap 可延后，但持久化格式现在必须可扩展。
- [ ] 转场验证 source handles，不足时拒绝或明确缩短；不得读取片段外错误帧。

### 播放、缓存与代理

- [ ] Windows 支持硬解的 4K23.976-60 HEVC Main10 进入真实 FFmpeg hardware → native surface → GPU YUV/working path；不支持时自动低分辨率/代理。
- [ ] seek/scrub 为 latest-wins；旧 generation 在预算内观察取消且不能发布旧帧。
- [ ] 共享门禁已要求 Playback/Interactive/Still 的请求观察和完整返回分别满足固定预算，并拒绝未知原因、缺失请求/检查点证据、非法时间顺序或 Broker 运行时钟回退；exact-still 已覆盖 in-process FFmpeg interrupt 与外部子进程回收，但尚缺固定参考机上真实长 GOP/阻塞 I/O/驱动的最坏延迟证据。
- [ ] CPU decoded cache、GPU/working cache、proxy index 都有字节预算、LRU/eviction、source revision 和颜色解释 key；整进程 Private Commit v1 已在本地视频与 CPAL/A/V 30 分钟门禁通过，尚待 canonical 素材和固定参考机矩阵复验。
- [ ] 播放开始后音频保持主时钟；视频迟到采用 drop/repeat/降质，不能把常态播放变成反复静音等待视频。

### 音频最低闭环

- [ ] Clip gain、pan、fade in/out 已有持久化作者语义和公共执行；补齐产品 UI、细粒度命令/Undo、保存重开与 Golden Project 操作验收。
- [ ] Track mute 已进入公共执行；实现 transient solo audition、master meter、显式基础 limiter，波形与缩放/代理/relink 后保持正确。
- [ ] 输入重采样和 channel mapping 有明确策略；unsupported layout 明确降级/拒绝。
- [ ] generation-owned 单调 `ExecutionCancellationToken` 已贯穿 Audio Playback→Timeline Runtime/嵌套→Decoded Source→媒体窗口，reprime/seek/recovery/shutdown 会先取消旧 token，取消结果不进入成功缓存或 failure memory，执行中 render/source 测试要求 50 ms 内观察。媒体 Adapter 已改为最多 8 个按完整源指纹/输出契约寻址的持久 FFmpeg 子进程 Session：顺序窗口复用连续输出、随机 miss 只重启对应 Session并以最多十秒 coarse preroll + output exact trim 保持顺序解码样本坐标、stdout 有界预读、stderr 有界保留但持续排空、等待输出每 5 ms 观察取消并 kill/wait/join；PCM 缓存按完整窗口键 single-flight。仍须在固定参考机同时证明 compressed-audio 冷启动、连续边界、随机重启、取消返回、跨源 LRU 与 256 MiB 缓存预算。
- [x] 加速 Headless 30 分钟 48 kHz/29.97 门禁通过：精确最终位置、Audio→Synthetic→Audio 连续切换、零交付时钟漂移、零 underrun recovery，Evidence 固定容量且全程最大值不因淘汰丢失。
- [ ] `cpal_av_48khz_30min_v1` 已实现真实墙钟产品路径 Adapter 和 fail-closed 规则：要求主音频流自身时长覆盖观察区间（不能用容器时长或 EOF 补零代替）、具体 48 kHz stereo CPAL generation、真实 callback consumption、Headless GPU presentation、Ready ≥99.5%、零 underrun/recovery/静默替代、A/V drift ≤20 ms、Session resident/peak 不超过容量且真实发生顺序复用、稳态顺序十秒窗口 ≤460 ms、源缓存 ≤256 MiB 与整进程内存平台；冷启动/随机重启分别留证据但不冒充稳态预算。2026-07-18 本地完整运行已全绿：86,474,752 callback frames/168,926 callbacks、53,964/53,964 Ready、107,924 次 GPU presentation、零 underrun/recovery/替代/drift/rejected，稳态窗口最大约 80 ms，缓存约 252.7 MiB/256 MiB，整进程 settled growth 约 85.8 MiB 且 post-stress 不增长。仍须固定参考机/设备矩阵复验；CPAL 只证明 OS 输出消费，声学端到端仍需独立 loopback。
- [x] 可延长 CPAL/A/V 开发 smoke 已在本机真实产品路径通过；默认 8 帧，诊断可扩到 3,600 帧而不改变专业门禁。修复 demand SSOT 后的 3,600 帧运行取得 5,813,760 callback frames/11,404 callbacks、3,603 Ready、7,247 次 GPU presentation、零 underrun/drift/rejected。

### 效果、动画、标题与转场

- [ ] 先交付少而完整的算子：Transform/Crop、Opacity/Blend、Primary Color、LUT、Gaussian Blur、Sharpen、基础 Mask、Cross Dissolve、Basic Title。
- [ ] 每个算子通过通用 DoD；不以 effect enum、属性面板或未连接的 `TextLayer`/`Transition` 类型作为完成。
- [ ] Hold、Linear、Bezier/Ease、关键帧增删移动复制、reset 和基础 curve editor 可完成 Golden Project。
- [ ] CPU fallback 不隐式 RGBA8，不在一帧内反复 GPU→CPU→GPU；fallback 原因在 Viewer/Export report 一致。

### 导出与颜色

- [ ] 产品 UI 完成 H.264/AAC MP4 与 HEVC Main10 的合法预设、范围、覆盖确认、队列、取消、失败原因和重试。
- [ ] 预览/导出使用同一 timeline/effect/color/alpha 解释；Golden frame 与 report signature 对齐。
- [ ] Rec.709、sRGB、HLG、PQ 与列入 Beta floor 的 Log reference 全链验证；无法解析的 Log 阻止并提示 override，不输出“差不多”的颜色。
- [ ] 导出后自动 probe 并重导入，校验 codec/container、分辨率、fps、时长、音频、primaries/transfer/matrix/range 与静态 HDR metadata。

### 退出门槛

- Golden Project 完整工作流连续 3 次通过，无人工修复项目文件或清缓存步骤。
- 达到第 5.4 节 4K 播放、seek、UI 响应、30 分钟同步和导出门槛。
- 所有运行路径被分类为 Verified、Explicitly degraded 或 Blocked/Unresolved。
- 已知 P0 数据损坏、错误色彩、A/V 失步和不可取消卡死为零。

## M2 — Windows Beta：生产可靠性与完整编辑体验

**目标：** 用户可以把 Mondrian 用于较长真实项目，升级、恢复和交付风险可控，常用编辑不再表现为“播放器加轨道”。

### 要求

- [ ] 补齐 marker、字幕/标题轨与 ripple/删除/嵌套序列的传播规则；建立复杂链接组与多选事务测试。
- [ ] 完整处理 VFR 手机素材、不同帧率 conform、drop/non-drop timecode、速度变化与音频 sample 对齐。
- [ ] 实现 tag/metadata 检索、素材使用位置反查、批量 relink 预览与冲突选择。
- [ ] 项目迁移覆盖所有已发布 Alpha schema；自动保存保留策略、恢复冲突、磁盘满/权限失败和备份可观测。
- [ ] 将基础效果扩展到 10–15 个算子族，而不是堆孤立预设：Geometry、Composite、Primary Color、Curves/LUT、Blur/Sharpen、Key/Matte、Mask、Distort、Temporal、Generator、Text、Transition、Utility。
- [ ] 完善音频 Clip/Track/Master bus、峰值/响度 meter、limiter、基础 EQ 或 compressor；效果必须满足实时安全和导出一致性。
- [ ] 提供可保存/导入导出的用户 preset；preset 引用稳定 ParameterId 和 schema version。
- [ ] 完成 Windows 安装、升级、卸载、日志/诊断包、crash dump 指引和无网工作流。
- [ ] 建立 i18n message catalog，消除产品 UI 硬编码字符串；先支持中文/英文、变量/复数、pseudo-locale 和文本扩展测试。
- [ ] Windows HDR viewer 只有在完整 payload/swapchain/monitor path 通过真实设备门禁后转为 Beta；否则继续 experimental/blocked，不阻塞 SDR Beta。

### 退出门槛

- Stress Project 通过连续播放、100 次跨区 seek、proxy 切换、保存/迁移/恢复和长导出。
- 从所有发布过的项目 schema 升级到当前版本均有 fixture 和失败回滚。
- 中文、英文和 pseudo-locale 下 Golden Project 工作流无截断导致的不可操作控件。
- 发布候选门禁全部通过，P0 为零，P1 有明确 owner 和发布决定。

## M3 — 专业能力深化与互操作

**目标：** 在 Windows Beta 闭环稳定后，按真实工作流把少数模块做深；扩展建立在 M0 接缝上，不重写核心项目/时间/帧/参数模型。

### 优先方向

1. **效果与合成：** 更完整的 key/matte、Bezier mask、corner pin/warp、多输入节点、channel/alpha utility、temporal effect、节点图交互和更丰富的 title/subtitle layout。
2. **音频：** 响度标准化、bus routing、更多内建效果、延迟补偿和自动化；内部音频图稳定并经过实时压力测试后才评估 VST3/CLAP host。
3. **跟踪：** 按 Parameter → Keyframe → Mask → Point Tracker → Planar Tracker → Stabilization/Corner Pin 推进；结果写入普通动画曲线，不建立不可编辑的私有 tracking 数据孤岛。
4. **互操作：** 先实现 OTIO/EDL adapter 和 roundtrip loss report；OTIO 只作为 editorial interchange，不替代 `.mdp` 项目格式。AAF/XML 按真实需求和测试素材再排期。
5. **导出：** 经验证的 ProRes/专业中间格式、图像序列、更多硬编适配器与断点/错误恢复；每种格式都有 capability probe、roundtrip 和颜色/音频校验。
6. **插件：** 先冻结内部 effect provider 契约，至少由内建与一个受控插件适配器共同使用，再评估 OpenFX adapter；不提前承诺 Rust/C ABI 稳定性。

### 退出门槛

- 每个新增专业方向有自己的 Golden Project 变体和 capability matrix。
- 外部交换/插件不能使未知参数或不支持效果静默丢失；必须保留 metadata 或生成 loss report。
- 新后端不得复制 preview/export 解释逻辑，不得绕过参数和帧契约。

## M4 — 跨平台实现与长期扩展

**目标：** 在 Windows 产品和平台接缝被真实验证后，为 macOS/Linux 增加适配器，并持续改善效果、性能和交互，而不是分叉产品模型。

### macOS/Linux 交付顺序

1. 构建、启动、项目/时间线/CPU reference 路径。
2. 文件/剪贴板/通知/显示 profile/日志/安装适配器。
3. 音频设备和 audio-master playback。
4. 原生硬解与 GPU residency：VideoToolbox/CVPixelBuffer、VA-API/DMABUF 等。
5. SDR display/output parity。
6. 平台 HDR/EDR，只在完整显示 payload 与真实设备门禁成立后发布。

### 跨平台门槛

- 同一 `.mdp` 在受支持平台往返不改写无关语义，稳定 fingerprint 仅因真实内容变化而改变。
- CPU reference/golden 在容差内一致；GPU 差异有后端专属 evidence，不改写预期颜色来适配某个平台。
- Unsupported native path 自动回到正确 CPU/低拷贝路径或明确阻止；不得产生错误色彩、Alpha 或时间映射。
- 平台实现只新增/替换 adapter，核心时间线、参数、项目迁移和 Viewer/Export 语义不分叉。

---

## 7. 接下来 90 天

90 天只维护一个主里程碑、一个验证/性能任务和一个小型基础设施任务；其他方向进入 backlog，不同时开七条主线。

### 第 1–2 周：事实基线与 M0 冻结

- 完成 Reference Corpus manifest、Golden/Stress Project 规范、Windows 参考机和性能采集说明。
- 给现有能力生成 capability matrix：declared、product-integrated、real-executed、fallback、blocked。
- 完成 project/time/parameter/frame/audio/task contract 审计，确定迁移版本和兼容策略。
- 将 RAW、动态 HDR、外部插件 ABI、跨平台 HDR 等明确移出当前 WIP。

**周末门槛：** 所有后续任务可指向具体 corpus 场景和验收指标；不存在仅写“优化”“完善”“支持更多”的任务。

### 第 3–5 周：迁移、所有权与音频时钟

- 落地项目 version registry 与 schema v11 current fixture；Alpha 旧 schema 明确拒绝。
- 将可 headless 驱动的播放/任务核心从 UI 适配器中收敛出来，保留现有成熟调度逻辑。
- 冻结 Timeline Time/Time Domain、稳定 ParameterId、统一曲线 schema 和 cache semantic revision；旧 `TimeCode`/`TimeTicks` 不作为兼容格式保留。
- 将 audio master、video late-frame、buffering 和 underrun 证据统一到播放报告。

**阶段门槛：** M0 退出门槛全部通过；不以“大文件拆小”代替深模块接口和独立测试。

### 第 6–9 周：Windows 4K 播放与编辑可靠性

- 在参考机上完成真实 HEVC Main10 hardware/native path；修复准入、同步、格式和 fallback 问题。
- 达到 warm/accurate seek、latest-wins、内存预算和 30 分钟 A/V 同步目标。
- 完成 Lift/Extract、Link/Unlink、Track Targeting、transition handle 与 speed/reverse/freeze 的产品路径。
- 完成 Clip gain/pan/fade、meter/limiter 和波形/relink/代理一致性。

**阶段门槛：** Golden Project 可不导出地完成导入、完整编辑、播放、保存重开；性能报告能定位而非仅报告“慢”。

### 第 10–13 周：创作与交付闭环

- 把 Basic Title 与 Cross Dissolve 接入真实 timeline/render/export；完成首批小而完整算子。
- 完成 H.264/AAC 与 HEVC Main10 产品预设、roundtrip 和色彩/音频校验。
- 对 Rec.709/sRGB/HLG/PQ/常见 Log reference 执行 preview/export golden；任何未验证识别进入 blocked/override 流程。
- 连续运行 Golden Project 三次并修复所有 P0。

**90 天退出目标：** M1 Windows Alpha 退出门槛通过；若未通过，继续修闭环，不以新增效果、格式或平台宣布下一个版本。

---

## 8. 明确后置

以下能力不是永久放弃，但不得抢占 M0/M1 的主线容量：

- 完整相机 RAW 与所有厂商色彩生态。
- 动态 HDR10+、Dolby Vision、专业 SDI 和广泛显示校准产品化。
- macOS EDR、Linux HDR，以及 Windows Beta 前的多平台同时发布。
- OpenFX、VST3、CLAP 等外部插件 host 和稳定二进制 ABI。
- 复杂 3D tracking、实时高阶 tracking、表达式和程序化 rig。
- 多人实时协作、云媒体/分布式缓存、审片平台深度集成。
- 高阶 AI 自动剪辑产品化；`mondrian-ai` 在核心编辑闭环稳定前保持实验性。
- 8K 优化、所有社交平台一键上传、广播级全格式 QC。

---

## 9. 路线图维护规则

- 每个里程碑只在退出门槛全部有证据时标记完成；单个 checkbox 不代表版本可发布。
- 路线图状态必须链接到测试、JSONL 报告、Golden/Stress run 或 issue；“代码已合并”不是证据类型。
- capability 从 Blocked/Degraded 提升为 Verified 时，必须增加真实执行 fixture；不得只修改默认开关或 UI 标签。
- 性能基线、golden hash、颜色 reference 和 migration fixture 的变化需要说明原因；不能为掩盖回归直接更新。
- 修改 crate 接缝、项目 schema、渲染/色彩/音频语义时，同步更新 `docs/architecture/` 与 `docs/specs/`。
- 每月复审一次 WIP：默认资源分配为 65% 当前唯一主里程碑、25% 测试/性能/稳定性/色彩符合性、10% 下一阶段必要基础；不将该比例解释为同时开启多条功能线。
