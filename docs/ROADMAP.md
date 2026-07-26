# Mondrian 产品路线图

> 更新日期：2026-07-25
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

本节是 2026-07-26 的代码基线，不是目标清单。状态区分“模型/算法存在”“已接入主路径”“已有真实项目证据”，避免从类型或单元测试推断产品完成度。

| 能力域 | 已有事实 | 仍不足以宣称完成的部分 | 当前判断 |
| --- | --- | --- | --- |
| 核心时间与 ID | 作者位置/范围/曲线使用 canonical `TimelineTime` 与显式 Time Domain/Transform；`FramePosition` 仅作求值/显示 Adapter。Sequence 持久化一个显示设置并解析为 Viewer/时间轴共享的 NDF/DF/Frames 契约；版本化 fixture 覆盖 VFR PTS、23.976→29.97 嵌套、负时间和 100 小时项目 | 素材源 timecode、用户输入/解析、time-of-day/reel 语义及未来真正的 Feet+Frames 仍须各自完整产品契约，不能从显示格式化器反推“已支持” | L1 |
| 项目持久化 | `.mdp` archive/document/SQLite 独立版本轴已到 schema v22；`AuthoringSession` 生成不可变文档+SQLite revision 快照，后台 worker 用 SQLite online backup、验证后的 sibling archive、flush 和平台原子替换完成发布；manual/autosave 分离 baseline，Session/request/generation 可拒绝旧会话或过期完成。深 Project Recovery Module 统一 strict schema-v1 manifest、Project/path、author/library/document revision、SHA-256、精确子路径、发现/选择、保留和退役；新 manifest 先原子发布再删除旧 archive，恢复必须来自 canonical manifest 且与现存 ProjectId 相符，当前手动保存才可先发布空 manifest 再清理，stale completion 无权退役恢复点。Save As 保持 live runtime/SQLite 不移动：过期完成只把保留的恢复权限原子重绑定到新规范路径，恢复选择沿 manifest 的实际 runtime 打开，后续 autosave/恢复/覆盖保存仍闭合。v18–v22 依次固定 Basic Title、Project 色彩环境/Sequence 模板/嵌套色彩边、Clip-local 视觉作者时间、封闭 `color`/`delivery` 和单一 Clip 源时间映射 | Alpha 明确拒绝旧 schema、不承诺兼容迁移；M1 仍须完成恢复冲突/目标选择 UI、磁盘满/权限失败注入、反复崩溃与 close/Save As 三轮压力，不把单次生成项目恢复门禁写成全部产品恢复完成 | L1 地基与正常恢复闭环；M1 故障/交互闭环未完成 |
| Undo/Redo | `AuthoringSession` 是唯一可变 `ProjectDocument` 权威；所有生产编辑只修改候选快照，经完整验证、revision/generation 前进、项目级 bounded history 记录后原子安装。Undo/Redo 可跨 Sequence 导航和 Project 聚合，使用 200 条/128 MiB 双预算、序列化载荷和结构化淘汰证据；失败事务不改变文档、代次或历史。产品 `Action` 接口在历史为空时返回类型化 `ActionNotExecuted`，Window 通过可用性状态抑制禁用手势，Headless/脚本/自动化不能把未执行意图当成成功 | 高频/大型项目是否需要 delta/COW 必须由内存与延迟基准决定；M1 仍须覆盖所有产品操作和 Golden Project，而不是再建第二套 command 语义 | L1 架构闭合，操作覆盖继续扩展 |
| 素材管理 | 文件夹/Bin 层级、移动/重命名/删除、缩略图、离线提示、单文件/目录重连、代理模式已接入产品 UI。文件型图片只有在探针明确证明恰好一帧时才具有独立 `StillImage` 素材身份，未知/多帧失败关闭为时变 `Video`；静帧继续复用封闭 `ClipContent::Media`，时间线以显式零速 source hold 表达可任意延长的放置时长，Preview/Thumbnail/Export 共用媒体解析而不伪造视频时长。`proxy-relink-v1` 已用真实 H.264 导入、两次 FFmpeg 代理生成和 canonical Preview source resolver 证明 Proxy→Original→Proxy、离线诊断、重连后 `AssetId`/Clip 引用/名称保持、Asset Library revision 单次前进、旧路径代理不可复用及新代理发布 | tags/metadata 字段尚未形成检索产品；素材使用位置反查和批量诊断不足；目录批量重连、waveform/thumbnail 在运行中重连后的组合失效、动画图片/多页图片的分类与播放语义、多个素材/嵌套压力与完整产品交互闭环仍未闭合 | L1，单素材代理/重连及静帧主路径已有真实证据 |
| 时间线编辑 | 多轨、移动、分割、普通 Trim、Ripple Delete、Overwrite、Roll/Slip/Slide、跨轨移动、锁定、吸附、多选和嵌套序列已有实现与测试。专业 Insert 与 Lift/Extract 均已收敛为 UI 无关的原子 Sequence 操作：请求显式携带 content/target/ripple Track、精确时间与 automation/Transition/navigation 策略，不读取选择或 UI 状态。Insert 能跨轨开缝、切分跨界 Clip、保持完整链接组、拒绝锁轨/不一致链接、迁移安全转场并只在完整输入闭包下跟随 Bus/Output 自动化；Lift/Extract 复用规范 Clip 分片和 Sequence-time automation Module，分别保留或关闭节目时间，保护未目标化但 Sync-Locked Track 的相交内容，且完整候选评估与执行共享同一验证。Track Targeting/Sync-Lock 已成为按稳定 Sequence/Track ID 寻址、默认开启且彼此独立的编辑会话状态，视频/音频轨头 T/S 控制、Action 可用性、上下文菜单和单次 Undo 的 Lift/Extract 产品路径已经接通。`editorial-transport-v2` 又在同一 Hero 上通过普通 Action 固定精确 T/S scope：Lift 只移除目标 Track 的十帧 Clip 且不关闭节目时间，Extract 修剪目标 Track 并让未目标化但 Sync-Locked 的第二音轨同步关闭五帧，两者均以半开 In/Out、逐事务 generation/revision 和单步 Undo/Redo 记录类型化证据。Asset Insert Action 在一个 Author Transaction 中注册音频 Scope、放置内容并提交一次 Undo；原局部碰撞逻辑已更名 `ClipOverlapMode::PushForward`。定向 Split 返回请求 Clip 及全部链接成员的完整左右 `ClipId` receipt，不要求调用者扫描前后集合猜身份，也不会误切同一 playhead 上的无关 Track；全局 Razor 仍是显式的跨轨操作。Clip 已用封闭 `ClipContent` 删除矛盾 kind payload，并增加 Sequence-local Basic Title；链接改为可承载任意成员的 `ClipLinkGroupId` 集合；复制/切割/覆写片段/序列复制会 fork 完整作者身份图；显式视频 Transition 已有强端点、精确范围、同轨不重叠不变量和不裁切的双源 handle demand；Cross Dissolve 产品命令会用媒体/嵌套真实范围默认拒绝短 handle，显式缩短与创建/删除均为单次 Undo 事务 | T/S 会话状态的版本化工作区持久化、复杂 J/L linked-edge 决策、mute/visibility 与链接/范围编辑的完整交互矩阵、reverse/freeze/time remap 仍未闭环；Cross Dissolve、Basic Title 和多轨 Insert 已有局部 Golden，但仍缺真实媒体/嵌套/独立视觉 reference 的顶层验收 | L1+ 作者、命令与基础 UI 闭环；Insert/Lift/Extract 已进入当前 Golden Hero 并完成 v11 三连过 |
| 播放与缓存 | UI 无关 `PlaybackEngine`、`FrameWorkBroker`、`PreviewFrameStore`、Evidence v2、Headless GPU Adapter，以及播放/拖动/静帧三种访问语义、FFmpeg session/ring/seek index、原子 generation/抢占/取消 disposition、deadline、预取和代理已接入；Broker 的请求龄期、失效龄期、deadline 与 worker 完成时刻已统一到可注入的 Monotonic Runtime Clock：Adapter 只在准入前提交不透明绝对 deadline 与剩余时长，Broker 单次降低、同键 rebind 更新并在 worker 发结果前一次性盖完成戳，因而排队不会续期、UI 延迟轮询不会制造虚假 Late；deadline/generation/抢占按最早权威时刻统一裁决，时钟回退会钳制、按回退事件计数并令门禁失败；取消原因与 request→checkpoint→return 证据已由播放域统一聚合并以 Playback/Interactive/Still 固定策略供诊断、Headless 与专业验收共用；访问模式/Broker Adapter/有界 job transport 已迁到不依赖 UI 的 `app::preview_access_mode`，deadline/执行质量/预取深度策略已迁到 `app::preview_scheduler_policy`，具体 FFmpeg worker、协作取消观察、结构化结果发布和有界关闭已迁到 `app::preview_media_task`，素材记录加完整 Viewer 意图到 canonical media key/色彩拒绝/不可用结果的解析已迁到 `app::preview_media_source`，Window/Headless 的完成、过期、终态 Delivery 与预卷顺序已统一到 `app::playback_preview`；`app::preview_execution` 原子拥有完整 generation binding、pending、执行质量、候选 ID、Viewer output key、UI 无关 GPU execution contract 与 exact registered-output reuse，Window/Headless 均直接消费该 contract；`app::native_video_import` 统一聚合 renderer/platform native import 事实与稳定 admission blocker，`app::preview_hardware_admission` 以单一快照投影硬解请求、native surface-specific downgrade 与 device selector，Window/Headless composition root 共用该状态；UI 无关 `app::preview_media_frame`、`app::preview_timeline_execution`、`app::preview_viewer_plan`、`app::preview_cpu_execution` 分别拥有解码驻留、canonical Timeline/嵌套求值及 media-demand collection、Viewer lowering 与 CPU 合成/输出语义；Timeline 执行对每个子 Sequence 按自身画布与共享运行时质量求值，Ready 结果必带 cache identity，Window 只提供 typed media outcome、消费统一的预取/preroll/输入色彩 demand 并投影执行事实；UI 无关 `app::preview_runtime` 是唯一生产组合根，按职责拥有 asset-library/proxy-dispatch Adapter、presentation、request scheduler、result pump、service lifecycle、hardware admission、evidence 与分层 diagnostics；Window `app_ui::preview` 只负责 GPU 输出 Widget 注册、CPU raster 无拷贝转换和 panel diagnostics 投影；专业验收消费 UI 无关 evidence，固定策略不可由 smoke 环境变量放宽，并以主视频/主音频流时长及声明帧数而非容器时长证明覆盖；Windows 原生 Private Commit/Working Set 探针与版本化整进程内存门禁已接入真实 cadence/terminal-stress 路径；加速 Headless 门禁已证明 30 分钟时钟数学，真实 `cpal_av_48khz_30min_v1` Adapter 也已接入产品 Audio Playback、Headless GPU、源缓存与整进程证据并 fail-closed | `app::preview_runtime::PreviewProductionRuntime<O>` 已成为 Window/Headless 共用的 UI 无关生产组合根；真实 CPAL、4K HEVC、连续播放、Seek 与取消门禁直接实例化 Headless 输出 specialization，不再借用 `WindowPreviewAdapter`；2026-07-22 的清洁 `c484c47` 基线使用确定性生成 corpus，在 16 GiB 固定 Windows 参考机上完整通过 Video v5 与 CPAL/A/V v1，闭合 30 分钟 cadence、100 次 seek、取消、GPU presentation、设备/合成时钟、A/V drift 与整进程内存证据 | L1，M0 单机基线已闭环；多驱动/设备与声学 loopback 仍属发布证据 |
| Windows 硬解/低拷贝 | FFmpeg 硬件设备/codec 探测、D3D11/D3D12 native frame 保留、D3D11→D3D12 导入、NV12/P010 GPU YUV 采样、准入与失败原因已有实现；2026-07-22 确定性生成的 1812 秒 4K25 HEVC Main10 Rec.709 素材在 RTX 3050 Laptop/驱动 `32.0.15.9159` 上完成 Video v5：44,999 Ready、2 Stale、45,129 次 GPU completion，全部执行证据为 D3D12VA P010 原生路径且零 fallback/readback；播放 helper 单次启动并 clean close，100 次 seek 压力后的全部 helper 均完成 post-reap 核算 | M0 固定机主路径已经 canonical corpus 实证；仍须更多 GPU/驱动、HDR 显示链路和 8 GiB 降级压力矩阵 | L1 单机长期主路径已实证，发布矩阵未闭环 |
| 渲染与色彩 | working-space 合成、OCIO CPU/GPU 路径、结构化色彩/显示诊断、golden 测试、预览/导出报告对比、Windows 显示探测和 fail-closed 逻辑较深入 | 仍有 legacy/CPU/读回路径与真实显示 payload 限制；常见 Log/HDR 必须补齐参考样片端到端证明；Windows HDR 监看不能提前宣称稳定 | L1+ 架构，继续符合性收敛 |
| 效果与动画 | 稳定 `EffectId`/`ParameterId`/实例地址/`KeyframeId`、版本化 `ParameterSchema`、定义默认值/可动画能力、受约束数值/enum/resource、三种执行插值语义、`PropertyBag`/`AnimatedProperty`、编辑器曲线预设、效果 DAG、mask、缓存策略和插件式 definition/DSL 已存在；视觉与音频 Processor 共用同一参数描述语言，项目加载会拒绝非法参数状态，内建执行按 ParameterId 精确寻址。曲线选择/剪贴板/编辑已不再把 path、time 或点索引当身份；原子 `EditKeyframe` 保留 ID、Bezier handles/flags，控件区分 evaluated display samples、虚拟边界锚点和真实关键帧，并按一次手势一次事务发出细粒度 Insert/Move/Delete。Mask 标量 Property Bag 自 schema v17 起持久化，Basic Title 封闭 Property Bag/精确动画自 schema v18 起持久化；Cross Dissolve 已进入共享 Preview/Export working-linear coverage-correct CPU/GPU 合成并有产品时间线手势；Basic Title 已贯通创建、Inspector、Undo、共享 Render Plan、后台 Preview、Export、嵌套、转场端点与缺失字体 fail-closed，断开的 `TextLayer` 已删除；未知插件定义失败关闭；效果库只列出有可执行图的定义 | 只有部分声明效果生成真实 render op；Ease/handle 编辑、复制/粘贴/reset 和跨参数曲线产品 UI 尚未闭合；CPU/GPU backend 与真实 preview/export 验证仍须分别声明；Cross Dissolve 与 Basic Title 仍须外部参考视觉验收；真实外部插件仍需 Adapter | L1 地基，产品广度未完成 |
| 音频 | Track→Clip 已是 placement SSOT；Sequence 作者层统一持有 Component/Scope、Track/Bus/Output、typed Route/send、Rack、Automation 与 Transition；播放、导出、Audition、Analysis 和嵌套只允许共用 `compile → prepare → Session → Runtime`。稠密 schedule 已拥有拓扑 slot、连续 Route/Contribution ranges、精确 source/causal-execution/automation spans、liveness scratch、PDC 和 scalar/SIMD；callback 不扫描作者图、不分配、不隐式 clipping。schema v15 引入并由当前 v18 继续承载规范信号布局、显式 Component matrix、可自动化 Route 和精确 Samples 参数单位。Processor semantic IR 在 prepare 期由 Resolver 绑定固定 mode/algorithmic-latency/tail/state/scratch Factory，Render Contract 显式限制 Processor 私有字节、公共输出 lookahead 与 PDC delay-line 字节，Session 构造复核总量；`built_in_processors` 独立拥有规范 Gain 与状态型 Sample Delay，二者共用 Host 与预分配 Parameter-ID batch。算法延迟与 `None/Finite/Infinite` tail 已分离；Contribution 会在源结束后以显式静音冲刷有限尾音，并在首次真实相交样本懒进入局部状态。每个 Processor/Scope/Edit/Fader/Route 按自身 input-signal delay 求值自动化；Program Output 在 Session 内预热并丢弃固定 lookahead，直接返回 Timeline-aligned PCM，嵌套 child 不再向父层泄漏或重复补偿 latency。Clip Component 的 source/enabled/gain/pan/fade 已由稳定 Edit ID 的单字段 Inspector 意图进入完整候选校验、原子 Sequence snapshot、细粒度 Undo 与保存重开；0 fade 只有 `None` 一种作者表示，静态 gain/fade 已有 scalar/SIMD PCM 对照。非零延迟测试 Adapter 覆盖 PDC/entry/分块/mode/poisoning/stage-time/公开输出对齐/嵌套/预算；Sample Delay 覆盖整数作者值、可听延迟不被 PDC 抵消、clip tail、未来 clip 懒激活、seek 重入、分块和预算拒绝。Program Output 已有 callback 无分配 sample-peak/RMS/clip/non-finite observation；本地真实 CPAL/A/V 30 分钟同步门禁已通过 | 仍需隔离 VST3/CLAP、完整 matrix 编辑/探测/Adapter 布局协商、首个生产级非零算法延迟 Processor、stateful reverse checkpoint/materialization、typed sidechain、true-peak/响度/limiter/ballistics UI、完整 Rack/Automation 编辑 UI/Undo、Clip 音频 Golden Project 操作、固定参考设备矩阵与声学 loopback。GPU 仅能作为固定批量延迟已计入 PDC、deadline 有证据且可控降级的可选 Processor backend，不能成为 callback 依赖 | L1+ 执行地基；Clip 基础产品入口闭合，不宣称 DAW 完成度 |
| 导出 | 后台队列、取消、时间线逐帧合成、音频混编、FFmpeg 编码、色彩标签/HDR 元数据约束、结果 probe/校验和诊断已存在；产品与 Headless 共用稳定内建预设目录和单一 `resolve_export_delivery`：H.264 High 8-bit 4:2:0、HEVC Main/Main10、AV1 Main、类型化 ProRes profile、显式/跟随 Sequence 的位深与 range、显式 chroma/Alpha、CRF+完整 VBV、禁用音频和无静默尺寸归一化均在 App 预检、队列准入、执行与 probe 合同间一致；`ExportColorTarget` 独立表达跟随 Program Output、显式 Colorimetric 或 Project-engine Rendering View，Camera Log 中间格式不再靠非法 Sequence 输出伪造；生产 `Completed` 现要求 mux/major brand、Required/Forbidden 流、codec/profile、位深、rational fps、画幅、pixel format、CICP/range、静态 HDR、时长和音频格式全部有精确证据，并向验收层保留 stream-local PTS/duration/time-base；不可变媒体依赖冻结源画幅，导出把作者空间 Transform 从源/Sequence 画幅投影到实际采样/交付画幅，decode cache 同时包含目标尺寸，避免 4K 作者 Transform 在 1080p 交付上重复缩放或跨尺寸复用；产品导出面板以稳定内建 preset ID 重置一份可编辑的完整类型化草稿，已开放当前真实实现的容器、codec/profile、画幅、8/10/12-bit、range、chroma、Alpha、CRF/VBV、GIF、音频编码参数及通用色彩目标；色彩目标以“处理方式 + 目标空间”分离，Rendering View 只列显示目标，Colorimetric 才允许 Camera Log 等编码空间，非法组合显示统一阻塞且不能入队；Project 只拥有一个精确色彩引擎，Sequence 继续拥有可编辑的节目输出与交付默认语义，导出快照冻结两者；25 帧 H.264/HEVC Main10 产品导出与重导入已在 Hero Sequence 的非零 Work Area 中通过 durable reopen、像素、音频回读和 A/V 边界门禁 | 输出帧率 cadence、音频重采样/布局覆盖、GOP/二遍编码、硬编 profile、图像序列均尚无完整产品语义，不能以占位控件冒充；覆盖确认、失败重试仍未完成；Windows 硬编检测未落地主路径；完整 HLG/PQ/Log 独立数值 reference、专业中间格式、长项目和合格发布机证据包仍未完成 | L1+，交付合同、完成态证据和 Hero 短窗真实闭环已成立 |
| 自研 UI | winit/wgpu 产品入口、retained widget、主题 token、事件/焦点/IME、Dock、面板和大量组件测试已建立 | 交互一致性和无障碍仍需真实工作流验证；产品字符串大量硬编码，中英文混用，尚无 message ID/pseudo-locale 基础 | L1-；i18n 为 L0 |
| 插件 | 内部效果 definition、graph DSL、能力/缓存/失败隔离契约已有 | 尚无稳定外部 ABI、包加载/权限/隔离/兼容矩阵；当前只能称内部扩展接缝 | L0 |
| AI | provider/workflow/orchestrator 类型和测试存在 | 主要动作仍为 stub，不应进入核心发布承诺 | L0 |
| 跨平台 | 平台 trait、Noop adapter、Windows 实现、非 Windows 的明确 unsupported 结果已存在；Linux 独立 App UI Gate 运行 `mondrian-app --all-targets`，Windows 矩阵以非增量、单 Cargo 作业运行同一完整 App 测试并编译 feature-gated `mondrian-golden` 产品验证入口，避免 default/validation 混合 feature 的 MSVC 增量链接产物充当门禁证据并约束 16 GiB 证据机峰值内存 | 原生硬解/显示管理主要为 Windows；macOS/Linux 产品入口、安装和真实 GPU/音频门禁未完成 | 接缝存在，实现后置 |

效果与色彩的当前基线还包括：Primary Color 由 Sequence working space
驱动，CPU/GPU 共用该空间的亮度系数，曝光使用线性 stops、对比度以
scene-linear 0.18 为枢轴；工作空间进入效果图与缓存身份。LUT 必须分别
作者化处理色彩空间和资源身份，`.cube` 的 `DOMAIN_MIN/MAX`、完整 payload、
四面体插值及 SHA-256 内容失效均属于执行合同。旧 White Balance 的
additive-RGB 近似已删除，该类型保持 modeled-only 且不出现在效果库；在
建立有 observer/illuminant/chromatic-adaptation 语义的算法和参考证据前，
不得用“看起来像白平衡”的输出代替正确实现。

### 2.1 当前最重要的结构性风险

1. **专用硬件证据弱于代码广度。** PR CI 已在 Linux 独立 App UI Gate 和 Windows 矩阵中运行 `mondrian-app --all-targets`，Windows 以非增量、单 Cargo 作业再编译 feature-gated `mondrian-golden` 入口，不再靠测试名过滤作者域或 UI 子集，也不接受混合 feature 的损坏增量链接缓存或无界构建并发；但真实 GPU、真实硬解、真实音频设备和长项目仍主要依赖 Windows 专用 runner/ignored 门禁，不能用普通 CI 通过代替 Golden Project 与设备证据。
2. **关键复杂度已按行为形成明确 Locality。** Window/Headless 的完成收割、终态 Delivery 与预卷顺序统一在 UI 无关 `app::playback_preview`；`app::preview_execution` 原子拥有 generation binding、pending、执行质量、候选 ID、完整 output key、UI 无关 GPU execution contract 与 exact registered-output reuse；`app::preview_media_source` 从不可变素材记录与完整 Viewer 意图唯一解析 source/proxy 指纹、色彩/range/Alpha、native surface、代理意图与 canonical decode geometry，缺文件和色彩拒绝不再坍缩为无原因的 `None`；`app::preview_media_task` 独占具体 FFmpeg worker、协作取消观察、结构化终态结果和有界关闭，Window/Headless 不再各自拥有解码循环；`app::preview_media_frame` 以封闭单 payload（working CPU/source-domain/native surface）拥有解码驻留、惰性 working adaptation、质量/provenance、逻辑/采样几何及 reservation，空帧和矛盾 residency 已不可表达；`app::preview_timeline_execution` 是 canonical render-plan traversal、嵌套 Sequence lookup/depth、每个 Sequence 独立画布/共享 runtime quality、嵌套 working-space 合成与转换、typed pending/unavailable、Ready 必备 cache identity 和有序 execution facts 的单一实现；其 read-only media-demand collection 同时服务 prefetch、preroll 与输入色彩 evidence，Window 不再维护调度专用 nested walker；`app::preview_viewer_plan` 统一拥有 resolved element、稳定 cache identity、质量/provenance 聚合、deferred composite 分类与 GPU lowering；`app::preview_cpu_execution` 统一拥有 working-linear preparation/composite、Program Output、monitor adaptation、完整执行事实与阶段耗时；`app::preview_runtime::PreviewProductionRuntime<O>` 是 Window/Headless 共用的唯一生产组合根，`app::preview_frame_store::PreviewFrameStoreAdapter` 是其唯一 Frame Store Adapter；`app::preview_raster_frame` 拥有最终 CPU raster 的有效性、编码色彩、资源身份和内存 reservation，缓存与 stale pin 不再保存 Widget payload；跨 Renderer/显示契约/Window/Headless 的 GPU output blocker taxonomy 与 aggregate 由 UI 无关 `app::preview_gpu_output_blocker` 拥有；缓存驻留/淘汰算法由 `mondrian-playback::PreviewFrameStore` 拥有，GPU/CPU 色彩与合成数学由 renderer 拥有。`app::preview_runtime` 以 media adapter、presentation、request scheduler、result pump、service lifecycle、hardware admission、evidence 与分层 diagnostics 深 Module 保持 Locality；`app_ui::preview` 只保留 GPU 输出 Widget 注册、CPU raster 无拷贝转换和 panel diagnostics 投影；timeline evaluation 文件只适配 media outcome 与 execution facts，不再拥有递归语义；presentation 单独拥有最终 GPU/Raster/stale/CPU output 仲裁及唯一 `PreviewRasterFrame`→`ViewerFrameImage` 转换。该具体 Adapter 主协调器已完成职责级收敛；performance diagnostics 作为一套完整版本化规则书保留 locality，不按行数机械切碎。导出已把 admission/lifecycle/cancellation/evidence 拆到独立深 service；Timeline/音频/色彩/编码执行继续保留在同一 Export implementation 内，后续只在形成完整深职责时再拆，不按行数制造浅文件。
3. **当前 Alpha 项目版本化有严格拒绝、尚无兼容迁移。** 这是未发布 Alpha 的有意清理策略；一旦发布首个承诺兼容的 Alpha，之后每次 schema 变化必须同时提交旧 fixture、事务迁移、失败不覆盖和升级后重开证据，不能继续靠拒绝真实用户项目。
4. **声明能力和视觉执行能力可能分离。** Basic Title、Cross Dissolve、Primary Color 与显式域 LUT 已进入各自声明的主路径，但其完成不能外推到其他仅有类型、属性或 definition 的效果；路线图仍须逐项核对产品选择、Render Plan、具体 backend、Preview/Export 回归证据和独立视觉 reference。GPU LUT 仍明确阻塞，White Balance 仍为 modeled-only。
5. **音频/视频运行许可必须持续分离。** 当前 `Priming` 已可预填 PCM 但禁止设备提前消费，普通视频 Late/Recovering 也不会旋转音频 generation；清洁 `c484c47` 的固定参考机 CPAL/A/V 30 分钟基线已证明一次单机产品路径，仍须在多设备/驱动矩阵覆盖慢首帧/seek、持续视频压力、设备失效和声学 loopback，才能形成发布级长期证据。

---

## 3. 目标架构：现在冻结接缝，后续增加适配器

以下结构在 M0 固化。它们不是要求重写已有代码，而是把现有较强实现收敛到明确所有权，避免后续效果、平台或格式扩张时复制语义。

```text
ProjectDocument + Migration Registry
              │
       AuthoringSession
              ├── Candidate Author Transactions
              ├── Project-wide Bounded Undo/Redo
              ├── Asset Library Revision
              └── Immutable Persistence/Execution Snapshots

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
- `AuthoringSession` 是唯一可变 Project 权威；保存、执行和 UI 只能取得只读引用或不可变快照，旧 Session 的后台完成不得作用到新 Session。
- archive format、document schema、SQLite schema 分别版本化，迁移按有序步骤执行；禁止用 serde 默认值无声吞掉语义变化。
- archive/manifest 先写 sibling 临时文件、flush、重开验证，再经平台原子替换发布；SQLite 必须用 online backup 获取一致快照，不复制活动 WAL 文件集合。
- 每个迁移必须具备：旧 fixture、升级后不变量、再次保存/打开、失败不覆盖源文件、重复执行安全性。
- 缓存、代理、波形和缩略图不嵌入项目；它们的 key 必须包含足以反映项目语义和源文件 revision 的字段。
- 新字段必须定义缺省语义、旧版本迁移语义和 downgrade/unsupported 行为。

### 3.2 编辑命令与 Undo

- 时间线变更只经 `AuthoringSession` 候选事务进入；Widget、Viewer、平台回调和生产 App 命令不得直接借用规范 `Sequence`/`ProjectDocument` 的可变引用。
- 一次用户意图对应一个 undo transaction；链接片段、转场、marker、字幕、音频自动化等关联变化必须原子提交或全部失败。
- 当前快照历史保持单一语义并受明确内存预算约束；只有基准证明序列化快照成为瓶颈时才在同一事务接口内引入 delta/copy-on-write，不能建立第二套 Undo 解释。
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
- 任务调度不得成为只转发到现有 worker 的浅模块。当前 Frame Work Broker、UI 无关 Waveform Analysis Service、Thumbnail Execution Service、Proxy Generation Service 与 Export Execution Service 已作为五种独立 Implementation 消费统一语言；Export 使用独立有界队列、不可变闭包快照、一次性重载荷消费和原子成品发布，不与实时/交互工作共享执行池。

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
- Processor Instance ID 在一个 Sequence 的全部 Scope/Track/Bus/Output Rack 中唯一；Preparation 保留 Rack 顺序并按 Contribution 或 Routing Node + insertion point 生成独占可变状态的 occurrence。共享 Scope 只共享作者定义，不能令重叠 clip 共享 DSP 状态。
- 每个 generated Processor 只接收 Session 预分配的 Parameter-ID lanes 与块内 sample-offset event batch；静态参数为 offset-zero 单事件，变化曲线逐样本精确降低。VST3/CLAP/native/GPU Adapter 只能在能力协商后降低为其 ABI，不能把 parameter index 或 UI 刷新率变成语义。
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

M0 建立可重现的 corpus manifest。固定文件由 manifest 固定 SHA-256/字节数；确定性生成文件固定配方哈希、语义 probe 契约和权利依据，并由每次参考运行的 attestation 固定实际产物哈希，避免把不同 FFmpeg/encoder 版本的输出伪称为同一字节资产。现有规范清单包含许可明确的色彩数值参考、可本地生成的 1812 秒 4K25 HEVC Main10 Long-GOP 播放压力码流、1835 秒 48 kHz stereo AAC 设备时钟压力流，以及 MOV 承载的 305 秒 48 kHz stereo PCM S16LE Golden 音频作者样本。前两种压力流只证明播放、Seek、取消、缓存、内存与设备时钟负载，明确不得充当色彩 reference；PCM 样本用解析式且左右不等的信号验证 gain/pan/fade、通道身份、导入与持久化，必须由 probe 与产品导入同时证明 FL/FR Stereo 而不能从“双通道”猜测，也不冒充声学 loopback。Golden 的 PCM/AAC、generated Rec.709 H.264（仅代理/重连语义）、项目自有 HLG Main10 编码色块与 sRGB straight-Alpha PNG 角色均已绑定；后两项由同一配方生成并分别固定 HEVC Main10/CICP/range 与 PNG sRGB/RGBA/Alpha probe，实际字节只由运行 attestation 固定。当前 HLG slice 已证明码值、元数据、工作空间中性/单调/通道主导性，sRGB slice 有独立数值矩阵 oracle；这仍不等于完整 HLG 绝对传递函数、PQ 或相机 Log reference。Golden/Stress 主工作流仍缺完整 PQ/Log、VFR、多声道与损坏素材覆盖。每个样本记录来源许可、身份策略、容器、codec、分辨率、帧率模式、bit depth、CICP/side data、预期解释和可公开性。

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

机器可读合同身份已升级为 `windows-alpha-golden-v11`、封闭 schema v4：未知字段失败关闭；Hero Sequence 固定 7,500 帧、3840×2160、25/1、square pixel、progressive、linear Rec.2020 working、Rec.709 legal Program Output、10-bit 交付默认值和 48 kHz stereo；H.264/AAC 与 HEVC Main10 绑定稳定内建 preset ID 及完整 signal/probe 合同。七个切片仍只通过普通产品接口执行，并分别保留 PCM/Clip 音频、Editorial/Transport、Proxy/Relink/Constant Retime、短窗交付、视觉作者、Recovery/Nesting 与 HLG/Alpha 的局部证据；全部七个切片现已真实进入同一 Hero Sequence。Editorial 只绑定两条完整 pristine 音轨，定向 Split/Insert 消费产品返回的类型化身份 receipt；v2 又把 Lift/Extract 加入顶层必需操作，并通过不会前进作者代次的 T/S 会话 Action 得到唯一 scope。Lift 移除目标 Clip 但不关闭节目时间，Extract 修剪目标 Track 并让未目标化但 Sync-Locked 的第二音轨同步关闭五帧，两者均验证精确半开 In/Out 和单步 Undo/Redo；每次 scrub、settled seek 与 play 则通过生产 Preview Runtime 的 GPU 优先/CPU Raster fallback 仲裁消费精确 Frame Presentation Ticket，fallback 路径会如实写入证据，不能冒充 GPU。Delivery 复用 Foundation 的 PCM Placement，在 `150..175` 非零 Work Area 上通过产品 Action 完成 Trim、Transform 与 Opacity，durable reopen 后才执行 H.264 High/HEVC Main10 导出及普通媒体重导入；Program reference、成片解码像素、生产音频 Program Runtime、AAC `AudioSourceReader` 回读、精确 stream PTS/duration/time-base 与两种编码的一致性共同形成闭环。Proxy/Relink 在 Hero 新建专属视频轨，把真实 H.264 源显式 Trim 到不重叠的 `200..350` 窗口，并以 Track/Clip/Asset 局部强锚点证明 Proxy→Original→Proxy、离线、Relink 与 replacement proxy 后作者状态不漂移；随后通过普通 `SetClipForwardRate` 事务把同一 Clip 精确设为 `1/2`，在时间线帧 250 要求 Preview 与 Export 都解析到源时间 1 秒。该切片分别以正确 50% 和错误 100% 源时间渲染 Program reference，通过生产 Headless Viewer 呈现当前代理帧，再从不可变原片快照导出精确单帧区间 `250..251` 的 H.264/AAC、普通重导入并用有损容差和反事实距离共同判定画面；单帧足以验证唯一被比较的时间点，同时保留生产 mux/probe 的 stream-local A/V 边界断言，避免渲染不参与判断的帧。Relink 只前进 Asset Library revision，Recovery 与最终重开还必须保留重连记录、proxy intent 和精确 source map。Recovery/Nesting 在 Hero 新建专属视频轨，把生成源显式 Trim 到不重叠的 `175..200` 窗口，再以单个 Project Author Transaction 完成 Precompose；Hero 始终是 primary Sequence，仅新增一个强引用 child。Color Media 在 Hero 的 `350..375` 窗口新建两条直接相邻的专属视频轨，HLG 位于 straight-Alpha 下方；HLG 保留精确 source interval，PNG 保留显式 zero-rate hold，色彩 reference 强制取原片路径而不是代理。Autosave 恢复、Color durable reopen、covering manual save 与最终重开会逐项比较完整 Hero/child、稳定 ID，以及各阶段 Track/Clip/Asset 局部 SHA-256 锚点，防止“实体 ID 仍在但内容已被后续阶段改写”的虚假通过。其余局部证据不能因共用一个 Project 就自动成为完整节目证据。生成画面、identity LUT 与 Solid Color 嵌套只提供回归一致性；HLG slice 当前只有编码码值和结构性工作空间检查，不替代完整绝对 HLG/PQ/Log 独立色彩 reference；短窗播放也不替代 30 分钟 CPAL/GPU/内存/A/V 门禁。正式运行 `20260726T075954Z-complete-golden-30ffb812` 已在 v11 下完成新的 `3/3`，aggregate SHA-256 为 `e596fc2756eada710d84a0873dcd5e13f49c7d33ee35b8c959e6e130465ece4f`；v10/v9 只保留为历史证据。

顶层 `GoldenAcceptancePlan` 同时编译两份 fixture/operation/content/export 账本：全局账本识别完全未实现的义务，Hero 账本识别只存在于隔离 Sequence 的义务。每个切片必须声明稳定的验收 Sequence role；只有全部义务都绑定到 `hero`、并且运行时所有 Hero 切片报告同一个 primary `SequenceId`，顶层 Coordinator 才能继续执行并最终声明 `complete_golden_project: true`。嵌套 child Sequence 合法，但不能替代 Hero 身份。Rust 与 PowerShell 均按 schema v4、Hero 身份、三轮要求和 slice/export 引用失败关闭，不能由结构账本、单个切片、旧脚本、进程退出码或多个隔离 Sequence 的并集虚构完整性。

`GoldenProductWorkflowDriver` 持有一个真实 App composition、固定 `ProjectId + project path`；其私有 Hero binding 锁定初始 `SequenceId`、完整设置和 Project Color Environment。规范 v11 合同中的七个切片全部复用该 Hero 身份，只有 Recovery/Nesting 通过普通产品事务新增一个被 Hero 强引用的 Nested Composition child，因此完整 Project 精确包含两条 Sequence，而不是为每项能力建立诊断孤岛。组合门禁在 Visual 前后比较完整 Hero 音频投影，在 Editorial、Delivery、Proxy、Recovery 与 Color 前后比较各前序阶段的 Track-owned/局部作者锚点；Proxy 锚点覆盖重连后 Asset、proxy intent 和精确 `1/2` source map，Recovery 锚点只拥有自己的 parent Track 与 child，Color 锚点覆盖两条专属 Track、Clip、Asset 和共享 placement，允许后续阶段合法增量作者却拒绝旧内容漂移。正式监督运行 `20260726T075954Z-complete-golden-30ffb812` 已在无人工修复和无清缓存步骤下，用三个不同 run/Project identity 连续 `3/3` 通过，三轮均报告七个 primary stage 共享一个 Hero、最终恰好两条 Sequence，并观察到固定语料 constant-retime 义务；aggregate SHA-256 为 `e596fc2756eada710d84a0873dcd5e13f49c7d33ee35b8c959e6e130465ece4f`。此前 v10/v9 和八条隔离 Sequence 只保留为历史。该证据关闭当前 M1 Golden 的“单项目组合执行”门槛，但仍不替代独立绝对 HLG/PQ/Log reference、合格发布机、长时 CPAL/GPU/内存/A/V 同步或故障注入证据。

### 5.3 Stress Project

建立 30–60 分钟压力项目，用于：

- 连续播放、频繁 seek、长 GOP、缓存淘汰和内存平台。
- 代理生成/切换、relink、自动保存与恢复。
- 长时间音频输出、underrun、A/V drift 和设备切换。
- 取消导出、失败重试、磁盘空间不足、输出路径不可写。
- 从每个受支持旧 schema 迁移到当前版本。

### 5.4 性能与正确性基线

M0 固定 Windows 参考机类的 CPU、GPU、内存、存储、显示器/HDR 状态、驱动、FFmpeg 和构建 profile；内存合同明确分为 8 GiB `minimum-supported`、16 GiB `standard-playback` 和 32 GiB `professional-large-project`：8 GiB 必须正确、有界且可显式降级，但不承担原生 4K Main10 实时阈值；完整 M0 Playback baseline 要求 16 GiB 档，32 GiB 是大型专业项目推荐档。物理安装容量决定档位，OS 可见容量仍单独留证据。实际机器由操作员提供不含硬件序列号的稳定 opaque ID。一次可作为基线的 Reference Playback Run 必须把同一清洁 Git revision、机器报告与资格判定、corpus/配方/实际产物哈希、完整 Video+Audio gate 命令和结构化通过报告封装为不可混淆的 evidence bundle；部分门禁、脏树或低于所选门禁档位的运行只能诊断。数值在参考机固定前是初始目标；固定后只能通过有理由的基线变更调整，不能为让回归变绿而放宽。

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
- workspace unit/integration tests；独立 App UI Gate 必须运行 `mondrian-app --all-targets`，不得用 `app::`/`app_ui` 名称过滤冒充完整产品覆盖。
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

- [x] 建立 archive/document/SQLite version registry，并提供 schema v22 current document fixture、v0 SQLite fixture、幂等打开、事务回滚和失败不覆盖测试；Alpha 不保留旧 document schema 兼容。v16 将 proxy membership 规范为有序集合；v17 冻结封闭 ClipContent、多成员 Link Group、强端点视频 Transition 和可持久化 Mask Property Bag；v18 增加完整持久化和校验的 Basic Title 封闭作者状态；v19 固定 Project 色彩环境、新 Sequence 模板和嵌套 Clip 色彩边所有权；v20 固定所有 Clip-owned 视觉处理共享的 Clip-local 作者时间；v21 分离 Sequence `color`/`delivery` 深结构并对 Project/Sequence 作者 JSON 启用未知字段失败关闭；v22 删除可漂移的 Clip `source_in/source_out/speed`，以封闭 `ClipSourceTimeMap` 持久化唯一源采样映射并由 duration 推导终端边界。
- [x] Timeline placement 继续作为单一事实来源：ClipContent 封闭 variant 删除平行 kind payload，Clip Link Group 使用至少两成员的集合语义；复制/razor/overwrite fragment/precompose/Sequence duplicate 会 fork 完整 Clip/Track/Effect/Mask/动画/音频身份并重映射内部强引用。视频 Transition 已冻结 Sequence-owned 强端点、唯一 edit pair、精确范围和 unclamped 双源 handle demand；这只完成作者/Adapter 地基，不勾选 Cross Dissolve 执行。
- [x] `AuthoringSession` 已成为唯一可变 Project 权威，统一拥有文档、素材库、导航、project-wide Undo/Redo、`AuthoringSessionId`、`AuthorGeneration` 与 manual/autosave baseline。Project document save revision 与持久化 `SequenceRevision` 分离；候选事务只有在完整校验、历史记录和 revision/generation 前进都成功后才原子安装，失败不改变文档/代次/历史。生产代码不再暴露“先修改再补记”的 Sequence 可变入口。
- [x] 历史默认同时硬限制 200 条与 128 MiB command-owned retained bytes，使用可精确计量的序列化 Sequence/Project 快照和 `VecDeque` 常数时间淘汰，累计报告预算淘汰、分支 Redo 丢弃和超大命令未保留；Undo/Redo 可跨 Sequence 导航并为恢复内容分配新 revision。选择和导航仍明确不是作者事务。
- [x] 保存/另存/Autosave 已统一到 UI 无关持久化 worker：不可变作者快照绑定 Session/request/generation/SQLite revision，SQLite 走 online backup，archive 与 manifest 走 flush+验证+平台原子替换；旧 Session/过期完成不能清除新编辑，失败保持 dirty，close/quit 的必要保存同步等待结果。
- [x] 精确 Timeline Time、显式 Time Domain/Transform、Frame/Sample Evaluation Grid 已落地并删除旧 `TimeCode`/`TimeTicks`。当前 document schema v22 的 Sequence 只持久化一个 `TimelineDisplaySettings`，领域层解析为 Viewer/时间轴共享的有效 `TimelineDisplayContract`；Frames 与 SMPTE NDF/DF、signed timecode origin、负时间和 24 小时标签回绕均不改变作者时间。每个 Clip 另持久化一个 `clip_time_in`：Transform、Opacity、视觉 Effect、Mask 与 Basic Title 在同一 Clip-local 域求值；普通 move/slip/source retime 保持它，trim-in/split/overwrite 右片段按移除的 placement 时长推进。source-local 时间只负责媒体/嵌套取样，并由封闭 `ClipSourceTimeMap` 作为唯一作者事实：当前常速变体用精确 `source_origin + local × scale` 同时表达正向、反向和 hold，终端边界由 duration 推导，作者事务对完整映射算术失败关闭；未来 variable remap 只能扩展经过验证的分段变体。版本化 `timeline_time_contract_v1` fixture 通过不规则 VFR PTS、23.976 child→29.97 parent 显式嵌套变换、负时间和 100 小时项目验证精确投影；未实现完整电影尺语义的 Feet+Frames 已从类型和 UI 删除而非虚假暴露。媒体 Render Plan、Preview key/Broker job/request 与 Export cache 现只携带 canonical source-local `TimelineTime`；普通媒体不按 Sequence fps 二次量化，显式 source frame-rate override 才以 Floor 投影一次，嵌套按 child fps 求值，FFmpeg Adapter 最后以 checked nearest + stream start PTS 降低，浮点秒和微秒 key 已从产品执行闭环删除。

**播放与任务**

- [x] `PlaybackEngine`、`FrameWorkBroker`、`PreviewFrameStore`、Monotonic Runtime Clock、Playback/Frame Cancellation Evidence 与 Headless GPU Adapter 已从 UI 收敛；Broker execution lease 是优先级/访问类/worker lane 驻留证据的单一事实来源，App activity 原子计数已删除；访问模式/Broker Adapter/有界 job transport 位于 `app::preview_access_mode`，canonical media-source resolution 位于 `app::preview_media_source`，具体 FFmpeg worker、codec checkpoint 取消、终态结果和有界关闭位于 `app::preview_media_task`，Window 与 Headless 的“单次 Demand 采样→完成/过期→终态 Delivery→预卷”统一在 `app::playback_preview`；decoded/native payload 与单一 lazy CPU working adaptation、canonical Timeline/嵌套执行、Viewer plan、working-linear CPU composite/raster boundary 也已分别下沉为 UI 无关 App Module。`app::preview_timeline_execution` 对每个 Sequence 使用自身画布与同一运行时质量，统一嵌套 working-space 合成、typed media outcome、Ready cache identity、有序 execution facts 以及 prefetch/preroll/输入色彩共用的 media-demand collection；asset-library/proxy-dispatch adaptation、request scheduler、bounded result pump、service lifecycle、hardware admission、evidence、presentation 与分层 diagnostics 已按行为所有权迁入 UI 无关 `app::preview_runtime`；timeline evaluation 只适配 media outcome 与 execution facts，request scheduler 只做需求消费与 Broker 准入，Window Adapter 只投影输出。应用层 `PreviewRasterFrame` 已成为 CPU raster cache/stale-reuse 的单一 payload 契约，Window presentation 只在最终呈现时无拷贝转换为 Widget；最终 GPU/Raster/stale/CPU output 仲裁仍只在 presentation Module，且未扩大跨层 Interface。Viewer GPU-output 的预算、报告以及 attempt→health 纯分类已统一到 UI 无关 `app::viewer_gpu_output_health`；Window 只提交显示/纹理注册事实，不再另行定义 Ready/Degraded/终态失败语义。`app::viewer_gpu_output_residency` 统一声明态/执行态 residency 推导：计划帧不得声称 zero-copy、上传或读回成功，只有 renderer 完成记录可发布 executed 事实；平台探测作为显式 Session 快照供硬解准入和 Window/Headless residency 共用，不再逐帧重探或跨 capability generation 拼接证据。
- [x] Preview 生产输出已统一为版本稳定的 typed unavailability：`NoContent`、正确性/依赖 `Blocked`、已准入执行 `Failed` 与负责阶段从 Timeline/嵌套求值、素材解析/解码、输入色彩、CPU/GPU 合成、Program Output/monitor adaptation 到最终 raster/Window/Headless/evidence 全链路保真传播，不再坍缩为 `Option`、unit `Unavailable` 或依赖字符串反推分类。空根 Timeline 是可预期无内容；空嵌套 Sequence 产生透明层而不阻断父级；任一终态不可用都会撤销 current/pinned stale 资格，只有 `Pending` 可在同一 Sequence/画幅/显示契约作用域复用旧帧。Preview render evidence schema v2 对 blocked/failed fail-closed、按阶段聚合，同时不把正常 no-content 当作执行失败。
- [x] 帧工作已统一 generation、priority、deadline、原子取消 disposition、请求龄期、资源预算和诊断所有权；deadline 在 Adapter 准入边界以“不透明绝对值 + 当前剩余时长”提交，由 Broker 单次降低、同键 rebind 更新，并在 worker 发布前一次性记录完成时刻；首次 Broker close 同样以 Monotonic Runtime Clock 固定取消时刻，重复 close 不续期，App stop flag 不再拥有计时权；精确 Headless 测试覆盖截止边界、最早取消原因、rebind、close age 和晚轮询不制造 Late，时钟回退会钳制并使性能/专业门禁失败。`mondrian-core::execution_work` 只统一 priority/deadline/terminal evidence 值语义而不拥有调度。独立 Implementation `app::waveform_service` 已取代 UI cache，以 Asset+source revision、512 项 demand/16 项 worker transport、bounded LRU/failure/evidence、generation cancellation、私有 16 MiB windowed decode cache 和流式 peak accumulator 完成生产迁移；`app::thumbnail_service` 已取代 Window worker/cache，以 source fingerprint + 完整色彩 contract、512 项 demand/16 项 worker transport、128 MiB 加权 LRU、bounded failure/evidence、generation cancellation、发布所有权和数量/时间双预算 completion pump 完成迁移；`app::proxy_generation` 已取代进程全局无界 FIFO 与 Preview 私有去重表，以 AppState 实例所有权、source fingerprint + artifact/color contract、512 项准入、User/PlaybackRecovery/Import 公平队列、同键提升、cache-root 出队许可、256 项可恢复失败、512 项终态证据和项目 generation 取消完成迁移，媒体层在 limiter wait、FFmpeg 10 ms checkpoint 与发布前观察同一 token。Export 已使用独立 64 项离线队列、不可变最小 Sequence/媒体闭包、规范化输出占用、单调 attempt generation、统一取消/终态 Evidence、256 项轻量历史、bounded stderr 与 panic isolation；源修订在准备及发布前双重校验，验证后的同目录临时成品通过平台原子发布，UI/Headless 只观察轻量快照。上述 Module 共享值语义而不共享容量、worker、重试或缓存策略。
- [x] FFmpeg open/stream-info/seek/packet I/O 已接入 request-scoped interrupt，取消结果会保留首次观测 checkpoint 与 cooperative/FFmpeg-I/O/isolated-demux-termination 机制；loopback HTTP stall 已在 media Interface 与生产 Preview worker 两层证明部分 blocked input-open 会由真实 `AVIOInterruptCB` 有界返回，而真实 4K 长门禁又证明 `av_read_frame` 可以在 callback 持续返回取消后仍不返回，因此 callback 证据不能冒充恢复能力。每名有界 Preview worker 持有唯一、无锁的 `PreviewDecodeExecutionObserver`：正式 diagnostics/Headless Evidence 可区分 input open、stream info、hardware device、codec open、seek/flush、packet read、codec send/receive、materialization、output lease 与 session retire，并以 request/progress sequence 证明观察一致性；同一快照还分别保留 callback poll、观察取消次数、最后取消所属 request，以及 isolated helper 的 launch/validated-ready/Seek/Read/Packet/EOF/跨请求复用/active/peak/回收终态。每次成功 spawn 创建单一 evidence lease，只有子进程真实 reap 后才会计入 clean/canceled/failed/forced 之一，遗失回收路径会保留非零 active 而不能伪装成功。生产 Playback/Scrub/Exact 已统一到版本化 v2 packaged-helper Seam：一个 helper 只拥有一个源 Session 的 `AVFormatContext`，以严格单调 command ID、单 in-flight、真实 Seek ACK、pull-based 单包 Read、可 seek 的 EOF 与有界 Close 服务多次请求；父进程继续拥有 codec、DPB、D3D12VA 与 native surface，不按帧 spawn helper。isolated termination 会令 paired packet-source/codec Session 整体不可恢复并直接退役，不再先 flush 一个永远无法复用的硬件 codec。合成 B-frame packaged 门禁已逐项证明每个访问模式跨请求复用、InputOpen/StreamInfo/Seek/PacketRead 的 isolated-demux-termination、源修订失败与 launch→reap 一对一核算；协议/FFmpeg 失败或取消会 poison 源并以新 nonce/process 重开，不完整文件指纹不授权 Session/frame/index cache 复用。独立 sampler 只克隆只读 watch，以 100 ms 采样、进展变化/5 秒 heartbeat 稀疏写入并逐条 flush schema-v3 JSONL。专业 Video v5 接受策略不再以 helper 文件存在代替执行：必须观察 validated stream、真实 Seek/Packet、跨请求复用、至少一次 clean close、零 active、全部 launch 已归入唯一回收终态，且 failure/forced close 为零。短 Main10 `uhd_hevc_main10_isolated_demux_qualification_v1` 另以真实 Seek/PacketRead→generation supersession→Broker/media 取消→最新帧 GPU presentation 的闭环失败关闭，并要求 worker `Idle` 与 post-reap 核算，明确不冒充 30 分钟性能/内存证明。Reference Playback Gate Supervisor 以计划内 45 分钟墙钟期限、异步双管道读取和完整后代进程树终止约束 Video/Audio 进程；timeout 必定失败并保留日志/进度证据。播放域已统一 5 ms request→checkpoint、50 ms Playback/Interactive return、500 ms Still return 的 fail-closed 门禁。开发机无调试探针的真实生成 Main10 短资格门禁连续 5/5 通过后，清洁 `c484c47` 基线又完成全部 30 分钟 cadence、pause、100 次 seek、取消、close/quit、真实驱动与最坏延迟证据；helper 的 launch→post-reap 核算闭合且 UI 线程未承担 worker join。
- [x] `ProcessMemoryProbe`、Windows Process Status Adapter、固定 cadence 的 Private Commit 平台、4 GiB 绝对上限、terminal-stress 收敛、主视频流时长/声明帧数覆盖及不可由 smoke 参数放宽的专业阈值均已进入 Video v5；原生 `AVFrame`、Frame Store、renderer copy fence、worker session 与 residency-family ACK 的独立退休证明也已接入。确定性 1812 秒视频与 1835 秒音频配方、8/16/32 GiB 机器资格、完整 Video+Audio 编排及 evidence bundle 已闭合。清洁 `c484c47` 的完整 `windows-playback-m0-v3` 运行在 16 GiB `standard-playback` 固定机上以 `passed-baseline` 通过：Video Private Commit 峰值约 1.37 GiB、末端压力后约 1.23 GiB；Audio 稳态增长约 85.5 MiB、末端压力后只增约 5 KiB，均满足绝对上限与收敛门槛。

  - 清洁 revision `4b57403` 的正式运行完成全部 44,994 个原生 D3D12VA P010 cadence Playback 帧后，第二个 exact 停在 codec；压缩墙钟运行又可完成 45,000 帧并追加 2 次 seek，证明累计帧数或 device 存活本身不是充分条件。清洁 revision `68d76aa` 的正式运行 `20260721T175344Z-local-windows-dev-01-990fd909` 再次完成 44,994 个 Playback 帧，但首个后续 exact 在取消 30 秒后仍不返回；禁用 FFmpeg frame threading 得到相同结果。另一次完整生产运行约在第 28,353 个 Playback 帧进入超过 11 分钟不返回的 codec 调用，证明故障也不是 exact-only。单一 Interactive pool、完整 output-lease 与进程共享 immutable device 继续作为所有权不变量；“成功 native exact 后无条件终结健康 codec”这一历史规则已删除，lease 释放后只有源修订、执行族或 codec/output contract 不匹配，或 helper 被 poison，才禁止复用。
  - 清洁 revision `ed73f1e` 的 v3 Video diagnostic `20260721T213615Z-local-windows-dev-01-a6c50a4e` 完成全部 44,994 个真实 cadence D3D12VA P010 Playback 帧，随后首个 cross-region exact 的 NonPlayback request 2 在 `PacketRead` 停留完整 30 秒；Playback worker 已回到 Idle，Broker 已请求取消，media cancellation fact 仍为 0，测试自身有界失败而非等待外部 45 分钟 kill。这把本次故障精确到 `av_read_frame` 调用族，但旧 schema 尚不能区分 callback 未再轮询、active probe 未观察取消或 FFmpeg 忽略取消返回；因此不能再笼统写成 codec 卡死。
  - 清洁 revision `2eeefd3` 的 v3 Video diagnostic `20260721T230356Z-local-windows-dev-01-0153581d` 再次完成全部 44,994 个 Playback 帧；首个 exact request 停在 `PacketRead` 时 callback 已轮询至少 641 次、为该 request 返回取消至少 596 次，但 `av_read_frame` 仍不返回。这排除了 request probe 绑定错误，确立“压缩包源进程隔离”而非更多 callback/轮换启发式为恢复 Seam。仓库内合成 H.264/B-frame fixture 已证明每个访问模式跨请求复用，以及 InputOpen/StreamInfo/Seek/PacketRead kill/wait/join 的 checkpoint 归因；真实生成 Main10 短资格又连续 5 次证明 cross-region seek、生产取消、GPU recovery、clean close 与 post-reap 静止。
  - 清洁 revision `c484c47` 的完整基线 `20260722T065141Z-local-windows-dev-01-f4fc3eff` 在同一未变 revision 上通过全部 Video+Audio gate。Video 完成 45,001 个 cadence 请求、45,129 次 GPU completion 和 104 次非播放请求，播放 helper 只启动一次并 clean close，非播放 helper 两次生命周期均 clean reap，failure/forced/active 均为零；Audio 保持设备时钟约 1799.994 秒，仅以 synthetic clock 连续承接约 32.6 ms，零 underrun、零 render substitution，A/V drift p99/max 均为 0。该 bundle 是本地 `target/validation` 证据，不提交大型生成媒体或运行报告；可由固定 recipe、机器合同和 runner 复现。

**帧、参数与音频**

- [x] Frame/Color/Alpha contract 已冻结为统一 `ColorFrameDescriptor`：画幅、图域、外部/working/device 颜色身份、sample encoding、CPU/GPU residency 与 `StraightCoverage`/`PremultipliedCoverage`/`Opaque` alpha association 缺一不可。CPU typed frame、GPU handle/resource table、native NV12/P010 import、OCIO stage、working composite、Viewer spatial、ICC calibration 与 export/readback 均传播或验证该契约；公共 working/color seam 只接受 straight/opaque，premultiplied 只允许作为显式空间滤波内部帧，shader flags 从 descriptor 推导。非法 alpha 在 OCIO、Viewer 与 calibration 规划期以结构化错误失败关闭；legacy RGBA8 composite、GPU output fallback 和 stage blocker 继续由 renderer-owned typed path/reason breakdown 报告，类型存在或 shader 可创建不等于能力可用。
- [x] 视觉与音频 Processor 参数共用稳定 ParameterId、定义默认值/可动画能力、独立实例地址、单位、enum/resource、hard/soft range、Hold/Linear/Bezier 执行语义、cache impact、schema version/message ID；UI 编辑预设只生成 Bezier handles。视觉链已贯通 UI、精确动画、持久化、编译图和 Viewer cache；音频实例持久化 Schema 快照与单一 exact curve，内建编译要求规范 Schema 精确匹配；项目加载与效果注册拒绝非法 schema。颜色域、CPU/GPU、确定性、时间范围与 ROI 仍只由 Processor/Effect Definition 和编译图拥有。
- [x] Track/Clip placement → Component Edit/Scope → Track/Bus/Program Output、Gain/pan/fade/Transition、headless compiler、recursive nested Runtime 和 reference PCM 已进入 `mondrian-audio`；播放/导出共用 decoder Adapter 和执行语义，旧 flat mixer 已删除。
- [x] `AudioDecodedSource` 已改为可失败的精确 interleaved block Interface；播放/导出共用文件指纹与 128 项/256 MiB 加权 LRU 的十秒 PCM 窗口，Runtime 仅保留对齐 4096 帧热窗，不再以整文件 PCM 作为产品执行源。
- [x] source Seam 已从逐样本调用改为每个 Contribution 每块一次的索引化交错读取；semantic IR 已 prepare 为稠密 schedule、连续 Route/Contribution 区间、Transition binding、精确 sample span、预分段 automation span 与 liveness scratch，CPU scalar reference/SIMD kernel 已建立。`dense_schedule_v2` 固定参考机矩阵覆盖 1/8/32/64 Track、0/2/8/16 Bus、显式 Track→Bus→Output Route 与 64/256/1024 帧 block；普通 CI 覆盖三 Bus scalar/SIMD PCM parity。
- [x] Prepared latency solver 已在每个 Contribution→Track 与 port-specific Route→Bus/Output 汇合点计算补偿，累计 pre/post Rack 与 Program Output algorithmic latency；Processor contract 将其与 `None/Finite/Infinite` tail 分离，Rack 有界求和并拒绝溢出。Scope Contribution 的 causal execution span 覆盖 source/rack algorithmic latency 与 tail，源结束后不再调用媒体 Adapter而是送静音冲刷；未来 Clip 的 stateful occurrence 保持 pending，到首个相交样本才懒进入并连续运行。每个 Scope 输入、Rack Processor、Edit、Fader 与 Route 都保存并扣除 input-signal delay 后求值参数；Program Output 将总算法延迟变为有预算的内部 lookahead，fresh epoch 以有界块预热丢弃后直接返回请求的 Timeline PCM，公共 meter/continuity 保持请求坐标。已归一化的嵌套 child output 以零算法延迟进入父层但继续传播 state-entry obligation，不会二次补偿。Session 为每个 Contribution/Route 补偿输入预分配 fixed delay line，prepare 对总字节与 lookahead fail-closed，Session 构造复核容量；fresh continuity epoch、Offline 精确入口、Realtime generation Enter/Continue、失败中毒/有界恢复、独立 nested epoch、正向补算和 stateful reverse fail-closed 已进入产品路径与普通 CI。
- [x] Sequence、Render Contract、PCM request/cache/buffer 已统一规范信号布局（v15 引入，当前 v18 承载）；公共 DSP 可保真承载 Mono、命名扬声器集合和 1–64 Discrete Bus。素材库 schema v2 的 fingerprinted Component Catalog 已贯通播放/波形/Export 绝对 stream selection。媒体缓存保持 native-layout PCM；`Standard`/显式稀疏矩阵进入 Prepared Contribution，source→Sequence、child→parent 与 Program→delivery 是三个不混淆的边界，未知布局失败关闭。Prepared schedule 现保留全局唯一 Processor Instance ID、Rack 顺序和 owner/insertion origin；共享 Scope 为每个 Contribution 生成独立 Session state，Gain 通过预分配 Parameter-ID lane/sample-offset batch 执行，scalar/SIMD 与 block partition 保持 PCM 一致。主路由与并行 send 共用带 enabled/static gain/Sequence-time automation 的 typed Route 边；编译只将启用边纳入 signal closure，prepare 将 Route 曲线降为预分段 schedule，Session 只使用预分配 gain lane 并在 PDC 后按目标时间应用增益。
- [x] 用户可见 Component 选择与显式重绑已贯通：Inspector 以稳定 Edit ID 在 Asset Component/嵌套公开输出域内选择并进入 Sequence Undo；资产级“重新探测候选→显式重绑”分离自动证据与用户意图，保留逻辑 ID，更新后会在当前 transport anchor 失效并重建音频 Runtime。Canonical authored mix matrix 已完成持久化、验证、稠密调度执行、嵌套布局边界和播放/导出交付分层。Processor Host 已完成 semantic preservation、prepare-time Resolver/Factory、Session 实例、mode/algorithmic-latency/tail/state/scratch、显式 lookahead/Session 字节预算、主 Bus/辅助输入查询和公共参数 batch 分层；内建定义位于独立深 Module，Gain 与状态型 Sample Delay 走同一 Host，非零延迟测试 Adapter 贯通 PDC。Sample Delay 的可听延迟明确不是可补偿 latency。未来生产 Processor、外部插件、完整编辑 UI、设备矩阵和可选 GPU backend 必须扩展这些已冻结接缝，不能建立第二套音频图、自动化或补偿语义。

**验证基础**

- [x] 建立版本化 Reference Corpus manifest、Golden/Stress Project 机器可读契约、Windows 参考机 profile、机器证据采集与资格验证、分层校验门禁；generated fixture 采用“固定 recipe、run-local artifact hash”而非伪造跨 encoder 位级稳定性，既有产物只有 attestation 同时匹配当前 recipe 与 artifact identity 才能复用，不能被新配方重新背书。Reference Playback 生成器不再为陈旧产物重写新配方 attestation，并强制 AAC `channel_layout=stereo`。`windows-playback-m0-v3` 固化完整 Video+Audio gate、素材用途、环境绑定、45 分钟外部进程期限、Video 解码进度 journal、16 GiB baseline 内存档位和预期报告 profile。`windows-alpha-golden-v11` 使用封闭 schema v4，固定 Hero Sequence、完整 Sequence/交付合同、fixture purpose、slice role/export/window obligation 和“未执行不得通过”规则；真实 H.264 fixture 的用途同时声明 proxy/relink 与 constant-retime。七个局部切片的作者事务、源/代理指纹、媒体 probe、结构化不可用、持久化、交付 probe、恢复身份、静帧 hold 与像素/语义证据仍有效；它们不再被误写成同一节目闭环。App Action 不允许未知/未实现意图静默成功；缺 GPU/工具链/系统等执行前提仍失败关闭。
- [x] Golden v11 的结构完整性由单一深 Module 负责：确定性全局账本与 Hero 账本分别计算必需、已规划、缺失与未绑定义务；Rust/PowerShell 同步校验 schema、Sequence role、Hero 身份、三轮要求和 slice export 引用。所有独立切片无论成功或失败都显式声明 `complete_golden_project: false`；当前全局账本和 Hero 账本均无缺失或未绑定义务，结构计划为 `Complete`，但 planner 本身仍必须报告 `complete_golden_project: false`，防止“声明了计划”冒充“观察到执行”。普通 CI 还固定要求 Proxy/Relink 同时承载 `constant-retime` 与 `h264-aac-sdr`，不能通过同步删掉全局及切片义务来伪造闭合；测试会把 Color role 临时改成 diagnostic 并确认 HLG/Alpha 精确重新成为 Hero blocker。
- [x] Golden 单 Project Coordinator 和监督器的机制已进入专用进程主生命周期：Headless driver 固定 `ProjectId + path + AppState`，私有绑定锁定 Hero ID、设置与 Project 色彩环境；规范合同中的七个阶段只复用 Hero，Recovery 可通过产品事务新增一个合法嵌套 child。open、阶段绑定、Autosave Recovery 和 durable reopen 前后均失败关闭身份漂移。Coordinator 要求全部 stage 的 primary `SequenceId` 唯一一致、最终 reopen 保留 7,500 帧 Hero 与恰好一个 nested child；完整投影/Track-owned 锚点证明后续阶段没有改写既有作者状态，Proxy 锚点覆盖重连 Asset、proxy intent 与 source map，Recovery 锚点覆盖自己的 parent Track/child，Color 锚点覆盖 `350..375` 的 HLG/Alpha Track、Clip、Asset 与 placement。真实 Headless presentation 证明 seek/play/retime 输出而非直接注入 Delivery，非零 Work Area 的交付/重导入证据证明导出来自同一节目。v11 正式监督运行 `20260726T075954Z-complete-golden-30ffb812` 已连续三次通过，aggregate SHA-256 为 `e596fc2756eada710d84a0873dcd5e13f49c7d33ee35b8c959e6e130465ece4f`；v10/v9 只保留为历史。这仍不扩张为独立 HDR/Log 数值、长时性能或设备证据。
- [x] 将 capability probe、逐帧 decode provenance、最终 Viewer GPU completion 与 fallback/blocker 写入同一结构化报告，同时保持 media/renderer/playback 的诊断所有权；预取 aggregate 不得代替已呈现帧证据。
- [x] 路线图、效果规格与色彩规格已统一五级能力口径：作者模型存在、产品可选择、图可执行、具体 backend 可执行、真实 preview/export 已验证。效果库只暴露可构图 definition；共享 Render Plan 对启用但未实现/缺失定义/运行时不可用/资源无效/构图崩溃失败关闭；CPU、GPU、颜色 reference 与产品发布证据分别列示，类型、OCIO 映射、shader 创建或单次 lower 成功均不得自动写成产品支持。

### 退出门槛

- schema v22 current fixture 可保存、重开；Project 色彩环境/新 Sequence 模板、封闭 Sequence `color`/`delivery`、Clip-local 视觉作者时间、单一 Clip 源时间映射、Basic Title/Mask 动画与强编辑关系 round-trip；旧/未来 schema 与未知作者字段明确拒绝；故意失败不会破坏源文件。
- Headless 测试可驱动 play/seek/cancel，并以手动 Monotonic Runtime Clock 精确验证请求年龄、过期边界、最早取消原因、同键 rebind、worker 完成与 UI 轮询解耦以及回退证据，而不构造 Widget 或 native window。
- 一个参数从 schema → UI → animation → save/reopen → preview/export → cache invalidation 全链通过。
- 音频时钟、video target selection 和 fallback 决策可由结构化报告关联到同一次播放。
- corpus、Golden/Stress 项目和参考机都有可复现说明，不是空 README。

## M1 — Windows Alpha：完成 5 分钟真实项目

**目标：** 一个不理解内部架构的用户能独立完成包含常见 SDR/HDR、音频、标题、转场、基础效果和关键帧的 5 分钟项目，并得到可重复导出。

### 项目与素材

- [ ] 后台导入并正确探测 H.264、HEVC、AAC、PCM/WAV、PNG/JPEG；不支持/损坏文件不阻塞 UI。
- [ ] Bin、重命名、缩略图、离线占位、单个与目录重连可完成 Golden Project。单素材 `proxy-relink-v1` 已在 Hero Sequence 的专属 Track 与 `200..350` 窗口证明真实 H.264 Trim、离线诊断、显式 Relink、`AssetId`/Clip/名称保持、Asset Library revision、reload event、replacement source 解析，以及源路径参与代理身份后旧代理不被误用并重新生成；随后 Autosave Recovery 会复核 Track/Clip/Asset 局部强锚点和 proxy intent。目录批量重连、运行中 waveform/thumbnail/decoded cache 组合失效、多个素材/嵌套压力和离线占位交互仍未补齐，因此本项不提前勾选。
- [ ] durable worker、SQLite snapshot、Session/generation completion、dirty/manual/autosave baseline 和深 Recovery Module 已完成；fixture-free Golden 已在 Hero Sequence 的独立 `175..200` 窗口覆盖正式 Precompose、Autosave、关闭原 Session、产品恢复 Action、恢复点保留、当前手动保存退役和再次重开。Hero primary、强引用 child、完整 parent/child 作者快照、Preview 像素与 Export 递归执行均不漂移，且组合协调器会复核此前阶段锚点。仍须补齐恢复来源/时间/目标/冲突 UI、磁盘满/权限失败注入、反复崩溃和 Save As/close 压力边界，因此本项不提前勾选。

### 编辑手感的最低闭环

- [ ] 巩固 Select、Cut、Move、Trim、Ripple、Roll、Slip、Slide、Insert、Overwrite、Delete 和 snapping 的 UI 可发现性与边界反馈。
- [ ] 补齐 Lift/Extract、显式 Link/Unlink、Track Targeting；链接片段与锁定/静音/可见状态行为一致。显式 Link/Unlink 已闭合 Sequence-local 集合语义、完整组扩展、多组 fresh-ID 合并、单组 identity 保留、跨轨锁原子拒绝、稳定 ID 选择、Ctrl/Cmd 多选、右键保留选择、单次 Undo/Redo、上下文菜单可发现性与链接标记。Lift/Extract 现已闭合精确半开范围、显式 content/ripple Track、锁轨/链接/Transition/automation/作者身份的候选原子校验、规范 Clip 分片、未目标化 Sync-Locked 内容保护、一次 Author Transaction/Undo、Action 可用性和上下文菜单；Track Targeting/Sync-Lock 以默认开启的独立 T/S 轨头控制进入会话状态且不会污染 Sequence 作者模型。`editorial-transport-v2` 已把二者纳入同一 Hero 的顶层必需操作，精确证明 Lift 不移时、Extract 跨 Sync-Lock 关闭时间以及各自单步 Undo/Redo，并已随当前 v11 完成三连过。T/S 的版本化工作区持久化、复杂 J/L linked-edge 产品决策、mute/visibility 与链接/范围编辑的完整交互矩阵仍未完成，因此本项不提前勾选。
- [ ] 实现可交付的 speed、reverse、freeze frame；schema v22 的封闭 `ClipSourceTimeMap`、正向常速/视频 hold 原子 Module、类型化 Action、完整链接组/锁轨/Undo 与失败关闭的 source extent/已解析 Transition handle preflight 已落地。Inspector 现只为媒体/嵌套源投影规范 source map，以 0.01% basis-point 把百分比精确量化为 `TimeScale`，正向变速默认原子覆盖完整链接组；视频 Clip 可在其半开范围内按精确 Sequence playhead 创建或更新 picture-only hold，已知静帧不会伪装成 `0%` 视频，负 map 只读显示且不能偷渡为未完成的 reverse。专用单 Project/单 Hero exact-semantic gate 已证明 linked 变速、picture-only hold、Preview/Export render-plan、音频 compile+dense prepare、Undo/Redo 和 `.mdp` 新 Session 重开共享同一映射；metadata-only 素材不冒充真实像素证据。Golden v11 又把 50% 常速并入固定 H.264 Proxy/Relink Hero：同一 canonical source time 驱动原片 Program reference、代理 Headless Viewer、原片生产导出和普通重导入，并以 100% 反事实帧防止 metadata-only 假通过，现已完成完整三连过。仍须完成 direction-aware strict-predecessor lowering 后的真实 reverse；复杂 time remap 可延后，但只能扩展经过验证的分段映射变体。
- [ ] 视频 Transition 作者模型、共享 Preview/Export CPU 执行、产品命令、基础时间线 UI 与 GPU lowering 已闭合：强端点、同轨非重叠、unclamped 双源 demand、媒体/嵌套范围准入、默认拒绝、显式缩短与单次 Undo 均有测试；App 选择只保存稳定 Transition ID；精确相邻且未锁定的视频 cut 可创建约 1 秒居中 Cross Dissolve，Overlay 优先于 Clip 命中，可选择、普通 Delete、拖动两侧范围并显示当前 handle 失败诊断，Ripple Delete 不误用于转场。ID 与视图投影成对产生，预览拖动不修改作者模型，释放时只提交一个类型化事务。Viewer GPU 执行图以普通 Source 为唯一输入准备结构，Cross Dissolve 两端各自完成颜色、变换、透明度与效果后，由专用 working-linear pass 做 coverage-correct 插值；真实 wgpu readback 已逐通道对照 Export/CPU 共用公式，并有独立执行证据。`visual-authoring-roundtrip-v1` 已通过产品创建、单事务、Undo/Redo、保存重开以及 Headless Preview/Export 系数一致性，现与 Foundation Audio 共存于同一 Hero Sequence；但仍只使用无限 handle 的生成源。仍须用真实媒体/嵌套端点、代理/原片和独立视觉 reference 完成扩展验收。绝不读取片段外错误帧或隐式重复边界帧。

### 播放、缓存与代理

- [ ] Windows 支持硬解的 4K23.976-60 HEVC Main10 进入真实 FFmpeg hardware → native surface → GPU YUV/working path；不支持时自动低分辨率/代理。
- [ ] seek/scrub 为 latest-wins；旧 generation 在预算内观察取消且不能发布旧帧。
- [ ] 共享门禁已要求 Playback/Interactive/Still 的请求观察和完整返回分别满足固定预算，并拒绝未知原因、缺失请求/检查点证据、非法时间顺序或 Broker 运行时钟回退；exact-still 已覆盖 in-process FFmpeg interrupt 与外部子进程回收，M0 固定参考机的生成 Long-GOP Main10 长门禁也已通过。M1 仍须扩展到更多驱动、真实阻塞网络/可移动介质和 8 GiB 降级路径的最坏延迟矩阵。
- [ ] CPU decoded cache、GPU/working cache、proxy index 都有字节预算、LRU/eviction、source revision 和颜色解释 key；整进程 Private Commit v1 已用 canonical 生成素材在 M0 固定参考机完成 Video+CPAL/A/V 基线。`proxy-relink-v1` 已在同一 Golden Hero 的项目 cache root 真实生成 H.264 proxy、用 canonical Preview resolver 完成 Proxy→Original→Proxy，并在 relink 后证明新路径为 Missing 而非复用旧代理、随后发布新鲜 replacement proxy；其 50% 变速证据还要求 Headless Viewer 消费代理，而不可变生产导出消费重连后的原片并回读到正确源帧。仍须多个素材/嵌套项目、cache eviction/内存压力、8 GiB 最低档和更多 GPU/驱动的组合矩阵。
- [ ] 播放开始后音频保持主时钟；视频迟到采用 drop/repeat/降质，不能把常态播放变成反复静音等待视频。

### 音频最低闭环

- [ ] Clip gain、pan、fade in/out 已贯通稳定 Edit ID 的产品 UI、单字段类型化命令、完整作者校验、细粒度 Undo、保存重开和 scalar/SIMD 公共执行；0 fade 规范为 `None`，非法时长原子拒绝。规范 PCM 的 Headless Golden foundation 切片现已真实执行 -6 dB、+0.25 pan、双侧 1 秒 equal-power fade、四次 Undo/Redo、耐久保存重开和稳定 ID 后置条件；该音频 Track、Clip、Component Edit 与 Audio Program 还会在同一 Hero Sequence 的 Visual 作者阶段前后作完整投影比较。完整 Rack/Automation 编辑 UI、更多布局和真实插件路径仍未闭合，因此本项保持未完成。
- [ ] Track mute 已进入公共执行；实现 transient solo audition、master meter、显式基础 limiter，波形与缩放/代理/relink 后保持正确。
- [ ] 为已执行的规范 Component matrix 提供矩阵编辑器、可审阅预设与 Undo；custom media layout probe、Sequence→设备布局协商和非标准设备/编码拒绝必须保留源布局、目标布局、所选矩阵及拒绝原因，不调用 FFmpeg/CPAL 隐式猜测。
- [ ] 为 Clip/Track/Bus/Program Output 的 Rack、参数自动化和 meter 提供同一套稳定 Scope/Processor/ParameterId 驱动的编辑 UI/命令；首个生产级 lookahead limiter 必须复用公共 Processor Host，并以真实 state、entry/deadline/PDC、嵌套、播放/导出一致性和 callback 实时安全证据验收。
- [x] 输入重采样和标准 channel mapping 已有明确策略：mono/stereo/5.1(side) 全组合及未标记 1/2 声道离散默认使用固定矩阵，LFE 不进入标准 downmix；未标记 3+ 声道、5.1(back)、7.1 与其他布局明确拒绝，不调用 FFmpeg 隐式猜测。
- [ ] generation-owned 单调 `ExecutionCancellationToken` 已贯穿 Audio Playback→Timeline Runtime/嵌套→Decoded Source→媒体窗口，reprime/seek/recovery/shutdown 会先取消旧 token，取消结果不进入成功缓存或 failure memory，执行中 render/source 测试要求 50 ms 内观察。媒体 Adapter 已改为最多 8 个按完整源指纹/输出契约寻址的持久 FFmpeg 子进程 Session：顺序窗口复用连续输出、随机 miss 只重启对应 Session并以最多十秒 coarse preroll + output exact trim 保持顺序解码样本坐标、stdout 有界预读、stderr 有界保留但持续排空、等待输出每 5 ms 观察取消并 kill/wait/join；PCM 缓存按完整窗口键 single-flight。仍须在固定参考机同时证明 compressed-audio 冷启动、连续边界、随机重启、取消返回、跨源 LRU 与 256 MiB 缓存预算。
- [x] 加速 Headless 30 分钟 48 kHz/29.97 门禁通过：精确最终位置、Audio→Synthetic→Audio 连续切换、零交付时钟漂移、零 underrun recovery，Evidence 固定容量且全程最大值不因淘汰丢失。
- [ ] `cpal_av_48khz_30min_v1` 已实现真实墙钟产品路径 Adapter 和 fail-closed 规则：要求主音频流自身时长覆盖观察区间（不能用容器时长或 EOF 补零代替）、具体 48 kHz stereo CPAL generation、真实 callback consumption、Headless GPU presentation、Ready ≥99.5%、零 underrun/recovery/静默替代、A/V drift ≤20 ms、Session resident/peak 不超过容量且真实发生顺序复用、稳态顺序十秒窗口 ≤460 ms、源缓存 ≤256 MiB 与整进程内存平台；冷启动/随机重启分别留证据但不冒充稳态预算。清洁 `c484c47` 固定参考机基线完成 86,475,264 callback frames、53,964/53,964 Ready、107,918 次 GPU presentation、零 underrun/recovery/替代，delivery drift p99/max 为 0，源缓存约 252.7 MiB/256 MiB，整进程 settled growth 约 85.5 MiB 且 post-stress 只增约 5 KiB。M1 仍须设备丢失/重连、多设备与驱动矩阵；CPAL 只证明 OS 输出消费，声学端到端仍需独立 loopback。
- [x] 可延长 CPAL/A/V 开发 smoke 已在本机真实产品路径通过；默认 8 帧，诊断可扩到 3,600 帧而不改变专业门禁。修复 demand SSOT 后的 3,600 帧运行取得 5,813,760 callback frames/11,404 callbacks、3,603 Ready、7,247 次 GPU presentation、零 underrun/drift/rejected。

### 效果、动画、标题与转场

- [ ] 先交付少而完整的算子：Transform/Crop、Opacity/Blend、Primary Color、LUT、Gaussian Blur、Sharpen、基础 Mask、Cross Dissolve、Basic Title。
- [x] Basic Title 已作为第五种封闭 `ClipContent` 贯通 schema v18 持久化/校验、菜单创建、空闲轨放置、Inspector/Undo、统一 Clip-local 视觉自动化、共享 Render Plan、working-linear straight-alpha 生成、效果/Transform/Mask/合成、Cross Dissolve 端点、嵌套、后台 Preview 与 Export。具体系统字体依赖及 face bytes/index 参与输出身份，缺失字体或未声明 fallback 失败关闭；标题安全区使用已校验的 Sequence 设置。`visual-authoring-roundtrip-v1` 已用真实系统字体证明产品创建/编辑、三种插值、单事务 Undo/Redo、保存重开、Preview 栅格/像素与 Export 语义一致，并已进入 Foundation Audio 所在的 Hero Sequence；该生成回归证据仍不替代完整 Golden、独立排版 reference、缺字/长文本、多语言和压力覆盖。
- [x] Primary Color 与 LUT 的作者/执行地基已闭合：Primary 使用 Sequence working space、对应亮度系数和 0.18 scene-linear contrast pivot，CPU/GPU Render Op 与缓存身份一致；LUT 强制显式处理色彩空间，严格校验单一 3D `.cube`、`DOMAIN_MIN/MAX`、有限完整 payload，以四面体插值执行并用全文件 SHA-256 失效。Visual report schema v8 已通过产品添加/设参、单事务、Preview/Export 图签名和像素一致、保存重开身份/参数/内容哈希一致，并在 Hero Sequence 保留稳定类型化实体身份。该勾选只表示这两个算子的当前地基和生成回归切片，不表示 GPU LUT、独立色彩 reference、真实 Log/HDR LUT 或全部首批算子完成。
- [ ] 每个算子通过通用 DoD；不以 effect enum、属性面板或未连接的 `TextLayer`/`Transition` 类型作为完成。
- [ ] Hold、Linear、Bezier/Ease、关键帧增删移动复制、reset 和基础 curve editor 可完成 Golden Project。当前视觉 Golden 切片已证明 Hold/Linear/Bezier 两关键帧的正式作者接口、求值、单步 Undo/Redo、schema 保存重开和 Preview/Export 同义；曲线产品路径现按稳定 `AnimationTrackId + ParameterId + KeyframeId` 发出细粒度 Insert/Move/Delete，移动原子保留非 Linear 插值/handles/flags，一次拖动只提交一次事务，虚拟 Clip 边界与真实首尾关键帧也已分离。仍未证明 Ease/handle 编辑、复制/粘贴/reset、跨参数/多通道完整产品 UI 与完整 Golden 操作，因此本项保持未完成。
- [ ] CPU fallback 不隐式 RGBA8，不在一帧内反复 GPU→CPU→GPU；fallback 原因在 Viewer/Export report 一致。

### 导出与颜色

- [ ] 产品 UI 已完成 H.264/AAC SDR MP4 与 HEVC Main10 的稳定合法预设、类型化 profile/位深/range/chroma/Alpha 合同、统一兼容性阻塞、队列、取消和失败原因；Golden v11 的两条交付合同已绑定稳定内建 preset ID，普通 CI 会逐项对照真实 `resolve_export_delivery` 与共享 `expected_export_video_signal`，防止 8/10-bit、profile、chroma、pixel format、range、Rec.709 CICP、静态 HDR metadata 缺席策略、Alpha、分辨率和 AAC 码率漂移。生产队列现在以 `Required/Forbidden` 类型化流合同在发布 `Completed` 前精确 probe mux/major brand、codec/profile、位深、rational fps、画幅、pixel format、CICP/range、静态 HDR metadata 精确值或缺席、音频 codec/sample rate/channel layout、stream-local PTS/duration/time-base 与时长；不可变导出依赖冻结源画幅，媒体/嵌套/Transition/生成层统一把作者 Transform 投影到实际交付采样画幅，并将目标尺寸纳入 decode cache identity；25 帧 generated-delivery slice 现已在 Hero 的 `150..175` Work Area 上、经过 durable reopen 后真实完成 H.264/HEVC 导出和普通媒体重导入，独立 Transform/Opacity 采样、Program reference/解码成片像素、生产 Program PCM/AAC 回读及两编码一致性均失败关闭；Proxy/Relink 另以 `250..251` 验证 50% 常速的 H.264/AAC 成片不是错误的 100% 源帧；Color Media slice 又以 4K 作者→1080p 交付验证构图没有重复缩放。当前全部已实现 preset 参数已进入同一可编辑草稿和产品表单：容器、codec/profile、跟随序列或显式画幅、位深/range、chroma、Alpha、CRF/完整 VBV、GIF palette/dither、禁用/AAC/MP3/PCM 及其参数；选择内建 preset 会精确重置，非法中间组合保留供继续编辑但无法构造入队动作。仍需覆盖确认、失败重试、完整独立色彩 reference、长项目与合格发布机证据；不能因当前短窗合同通过而勾选整项。
- [ ] 预览/导出使用同一 timeline/effect/color/alpha 解释；Golden frame 与 report signature 对齐。视觉作者切片已对同一帧证明 Basic Title、Cross Dissolve、Primary+显式域 LUT 在 Preview/Export 及保存重开间不漂移；Recovery/Nesting 切片进一步执行同一父子 Sequence 的递归 Preview 与真实 Export 帧渲染，并在恢复/手动保存重开后比较像素与执行诊断。Color Media slice 已让真实 HLG Main10 与 sRGB straight-Alpha 文件进入产品导入、时间线、Preview、Program Output、H.264 导出和普通重导入，当前采样最大通道误差为 3；但 HLG 绝对数值、PQ/Log、代理与真实媒体嵌套色彩边、GPU LUT 仍未全部纳入同一顶层报告。
- [ ] Rec.709、sRGB、HLG、PQ 与列入 Beta floor 的 Log reference 全链验证；当前 sRGB Alpha 已有独立 source→working 数值 oracle，HLG Main10 已有确定性码值、CICP/range、工作空间单调/中性/通道主导性和编码 roundtrip，但尚未以独立绝对 HLG 数值 oracle 关闭完整传递函数，因此本项不勾选。无法解析的 Log 必须阻止并提示 override，不输出“差不多”的颜色。
- [ ] 导出后自动 probe 已进入生产完成态，generated-delivery slice 也已在 Hero Sequence 的非零 Work Area 上真实重导入并校验 MP4、codec/profile、分辨率、rational fps、位深/pixel format、stream-local A/V 起止、48 kHz Stereo AAC、生产 PCM/AAC 回读、primaries/transfer/matrix/range、静态 HDR metadata 缺席和成片像素；仍须在独立真实色彩 reference、长项目和其他已支持容器/音频布局上关闭此项。

### 退出门槛

- [x] 当前 `windows-alpha-golden-v11` 完整工作流已由官方监督命令连续 3 次通过；正式运行 `20260726T075954Z-complete-golden-30ffb812` 的三轮使用不同 run/Project identity，七个 stage 的 primary `SequenceId` 均绑定各轮同一条 7,500 帧 Hero，最终均恰好保留 Hero 与一个 nested child，并观察到固定语料 constant-retime 的 Preview/Export/Headless/重导入反事实证据。aggregate SHA-256 为 `e596fc2756eada710d84a0873dcd5e13f49c7d33ee35b8c959e6e130465ece4f`；v10/v9 与八条隔离 Sequence 结果只保留为历史回归。
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
2. **音频：** 在既有 typed Route/send、稠密 schedule、PDC、公共 Processor Host 与自动化地基上增加 authorable sidechain、true-peak/标准响度、更多内建 Processor 和可审阅的复杂路由工作流；VST3/CLAP 必须经隔离 Adapter 接入 deadline/crash containment、state chunk、bus/layout negotiation 和参数身份映射，不得把插件回调或扫描放进实时 callback。GPU 只作为声明固定批量延迟、计入 PDC、设备丢失/状态切换可控、deadline 有证据且实测优于 CPU SIMD 的可选 Processor backend。
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

- 落地项目 version registry 与 schema v22 current fixture；Alpha 旧 schema 明确拒绝。
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

- 对已接入真实 timeline/render/export 的 Basic Title 和已完成产品手势/CPU/GPU lowering 的 Cross Dissolve 执行 Golden Project、外部参考视觉、长文本/缺字/嵌套/转场与故障压力验收；完成其余首批小而完整算子。
- 完成 H.264/AAC 与 HEVC Main10 产品预设、roundtrip 和色彩/音频校验。
- 对 Rec.709/sRGB/HLG/PQ/常见 Log reference 执行 preview/export golden；任何未验证识别进入 blocked/override 流程。
- 已建立一次命令连续运行三轮的 Golden Project 监督器；先让全部阶段收敛到同一 Hero Sequence，再取得新的三轮通过证据。此前隔离 Sequence 的三轮不再计作完整 M1，每个候选构建仍必须重跑并修复全部 P0。

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
