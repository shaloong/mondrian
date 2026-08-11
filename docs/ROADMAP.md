# Mondrian 产品路线图

> 更新日期：2026-08-11
>
> 当前状态：M1 Alpha 收口完成（实现、候选资格与三平台 native CI 均已闭环）
>
> 当前主线：冻结 M1，随后只推进 M2 的产品可靠性与交互完整性

ROADMAP 只记录产品范围、里程碑状态和退出门槛。实现细节放在
[`CONTEXT.md`](../CONTEXT.md) 与 [`docs/architecture/`](architecture/)；测试命令、
报告和历史运行放在 [`docs/dev/reference-validation.md`](dev/reference-validation.md)
与 [`docs/dev/performance-profiling.md`](dev/performance-profiling.md)。已完成能力不在
这里复制类名、内部账本、逐次性能样本或调试过程。

---

## 1. 产品目标

Mondrian 的优先级始终是：

1. **能剪：** 作者语义可靠，Undo/Redo 可预测，保存与恢复不会破坏项目。
2. **能播：** 音频保持 Clock Master；视频可降级但不能改变时间、色彩或来源。
3. **能交付：** Preview 与 Export 共用解释，输出文件经过自动检查后才发布完成。

### 成熟度

| 等级 | 定义 |
| --- | --- |
| L0 基础 | 类型、算法或实验入口存在，不能称为产品能力 |
| L1 Alpha 可用 | 主路径可操作、可保存、可恢复、可诊断，并有端到端证据 |
| L2 Beta/专业 | 复杂项目、交互、设备、性能和互操作矩阵完整 |
| L3 成熟 | 长期格式兼容、多平台实机和第三方生态经过规模验证 |

M1 的目标是 L1，不要求所有高级组合都可执行。暂未实现的组合必须在像素、音频、
文件或作者状态改变前明确 `Blocked/Unresolved`；失败关闭是 Alpha 的合法结果，错误
执行或静默降级不是。

### 不可妥协的架构规则

- `AuthoringSession` 是唯一可变 Project 权威；作者事务、History 与文件发布互不冒充。
- `TimelineTime` 是规范作者时间；帧、样本和 timecode 只在显式 Evaluation Grid 降低。
- Preview 与 Export 共用 Timeline、Effect、Color、Alpha 和 Audio Program 解释，但可拥有
  不同调度 Adapter。
- `ColorFrameDescriptor`、强类型 ID、Effect Execution Contract 和 Audio Render Contract
  必须完整传播；不能从字符串、裸通道数、裸帧号或资源存在推断语义。
- `EventBus` 只发布提交后的通知，不承担请求、状态权威、实时队列或 Undo/Redo。
- Module 应以小而稳定的 Interface 隐藏复杂 Implementation；新增 Seam 至少要有真实
  Adapter 变化，避免透传 Module。深 Module 为调用方提供 Leverage，为维护者保留 Locality。
- 平台差异只能位于平台、媒体、Renderer 原生后端或明确 Adapter；共享作者与执行语义
  不得依赖 Windows 行为。

---

## 2. 里程碑状态

| 里程碑 | 状态 | 结果 |
| --- | --- | --- |
| M0 核心契约与架构收敛 | **完成** | 项目、时间、编辑、音频、色彩、执行与验证权威已收敛到唯一生产路径 |
| M1 Windows Alpha | **收口中** | 核心工作流、作者级 Crop 与 HLG 独立 reference 已闭合；当前候选资格尚待重跑 |
| M2 Beta | **下一阶段** | 产品交互、恢复、复杂项目、设备矩阵、独立色彩资格和发布工程 |
| M3 专业能力与互操作 | **后续** | 高级效果、响度、插件、跟踪、交换格式 |
| M4 多平台实机资格 | **后续** | macOS/Linux 真实设备、打包、签名与长期维护 |

---

## 3. M0 — 核心契约与架构收敛（完成）

M0 已完成以下结果，技术合同以 `CONTEXT.md`、ADR 和架构文档为准：

- current document v25/library v5、封闭 `ClipContent`、强编辑关系和显式 schema 拒绝。
- 单一事务型 Authoring Session、Project/Sequence Undo/Redo、History 双预算和连续性屏障。
- crash-consistent `.mdp` 发布、SQLite snapshot、Recovery Authority、Session/generation 隔离。
- 精确作者时间、显式 Time Domain/Transform、帧/样本 Evaluation Grid 和规范源采样目标。
- UI 无关 Playback、FrameWorkBroker、Preview Frame Store、取消/deadline 与终态证据。
- 统一 Frame/Color/Alpha contract、OCIO 色彩执行、working-linear Float32 合成。
- Sequence-owned Audio Program、Prepared Audio Plan、Audio Render Session、PDC 与 Processor Host。
- Prepared Visual Program/Range Closure/Frame Closure、唯一 Effect graph-value plan 和有界异构执行。
- Reference Corpus、Golden/Stress contract、结构化诊断和跨平台常规 CI。

M0 不再重新打开第二套 Timeline、音频图、颜色解释、项目格式或旧 Renderer pipeline。

---

## 4. M1 — Windows Alpha：五分钟真实项目闭环

### 4.1 范围

M1 面向一个普通用户完成包含真实媒体、基础编辑、音频、标题、Cross Dissolve、基础效果、
关键帧和 H.264/AAC 或 HEVC Main10 输出的五分钟项目。Windows 提供真实设备资格；
Linux/macOS 共享生产 Interface 和常规 CI，但实机发布资格属于 M4。

### 4.2 收口矩阵

| 能力 | Engineering state | 当前候选资格 |
| --- | --- | --- |
| 项目与恢复 | 实现闭合：durable save/autosave、SQLite snapshot、非阻塞 close、恢复确认和故障注入不覆盖源 | 当前 Golden v14 已通过 3/3 |
| 素材与代理 | 实现闭合：有界 probe、fingerprint、稳定 AssetId、离线/重连、proxy/original 与 cache revision | Golden Proxy/Relink 已覆盖 |
| 编辑 | M1 核心操作、Link、Lift/Extract、T/S、snapping、常速/反向/hold 与 Transition 已接入 | Golden Editorial/Transport 与 Retime 已覆盖 |
| 播放 | 实现闭合：Clock、latest-wins、严格帧覆盖、取消、CPU/native decode 与 Viewer completion | 当前源码 v7 4K Main10 gate 已重跑通过（passed-baseline） |
| 音频 | 实现闭合：gain/pan/fade、mute/solo、meter、automation、PDC、limiter、嵌套与共享 Runtime | 当前源码 CPAL A/V recovery gate 已通过（passed-baseline） |
| 视觉 | 作者级 Crop、基础算子、Basic Title、Cross Dissolve 与 Hold/Linear/Bezier 作者语义已接入 | 当前 Golden 与 CPU/GPU reference 已闭合 |
| 色彩 | Rec.709/sRGB、PQ、HLG 与 straight Alpha 已有独立 reference；未知解释会阻止或要求 override | M1 色彩资格闭合；Camera Log 属于 M2 |
| 导出 | H.264 High/AAC SDR、HEVC Main10、不可变 snapshot、取消、失败清理与完成前 probe 已接入 | 当前 Golden 与长 Work Area 候选证据均已闭合 |
| 平台与质量 | 三平台生产入口和 Adapter 已进入 CI 配置；本地 workspace 门禁可执行 | Windows/Linux/macOS native CI 通过（1.97.1） |

### 4.3 退出门槛

- [x] current schema 可保存、恢复、重开；未来/未知 schema 和不确定持久化结果失败关闭。
- [x] 一个参数与一个音频编辑从作者命令贯通 Undo/Redo、保存重开、Preview/Export 和 cache invalidation。
- [x] 同一 Hero Sequence 完成编辑、代理/重连、嵌套、恢复、标题、转场、效果、音频和两种交付。
- [x] 作者级 Crop 贯通 stable ParameterId、动画、Inspector、Undo/Redo、持久化、Preview/Export、cache identity 与 CPU/GPU reference。
- [x] HLG 通过独立 1000-nit 绝对 reference；PQ、Rec.709/sRGB/Alpha 的既有证据保持通过。
- [x] 当前源码完成 v7 4K Main10 播放/seek/supersession和真实 CPAL A/V recovery；历史报告只作回归基线。
- [x] 所有生产结果归类为 `Verified`、`ExplicitlyDegraded` 或 `Blocked/Unresolved`；没有隐式 RGBA8、错误源帧、Video Master 或静默效果旁路。
- [x] 当前源码的 Windows/Linux/macOS native CI 通过，平台专属类型不泄漏到共享语义。
- [x] 当前源码 `windows-alpha-golden-v14` 连续三轮通过，并完成 workspace fmt/clippy/test。

### 4.4 已接受的 Alpha 限制

以下能力不阻止 M1，因为当前实现会在执行前类型化拒绝；它们进入 M2/M3，不能在 M1
以兼容分支或私有解释临时补齐：

- Export 的 heterogeneous Cross Dissolve endpoint、Adjustment accumulator 同步/readback、
  temporal×heterogeneous、GPU 后再次切换 backend、图内 readback 和外部 lane executor。
- 高级 variable retime、完整 J/L 创建交互、自由 Path/Bezier Mask 编辑、tracking 与节点图 UI。
- 具名 5.1/7.1/custom 物理扬声器 Adapter、true-peak/标准响度与外部插件 host。
- 常见 Camera Log、HDR monitor 输出资格、多 GPU/驱动和 8 GiB 全域并发矩阵。
- 批量冲突式目录重连、完整多语言、安装签名、崩溃包和 macOS/Linux 实机发布资格。

这些限制不得造成作者数据丢失。可恢复依赖应保留完整意图；不支持的执行合同应返回稳定、
可操作的 blocker。

### 4.5 证据索引

- Golden/Reference contract 与运行方法：`docs/dev/reference-validation.md`
- 性能、长时 A/V、作者矩阵与机器合同：`docs/dev/performance-profiling.md`
- 项目、播放、Renderer、音频和导出语义：`docs/architecture/`
- 接受的架构决策：`docs/adr/`

ROADMAP 只保留通过/未通过和证据入口，不复制 JSONL 每个采样值。源码或门槛合同变化时，
受影响的候选资格必须重跑；历史报告仍是基线，但不能替代当前 source attestation。

---

## 5. M2 — Beta：产品可靠性与完整编辑体验

### 5.1 目标与推进顺序

M2 把 M1 的可用闭环提升为可长期日用的 Windows Beta。顺序固定为：先交互和恢复，
再复杂项目与执行组合，随后颜色/音频/导出矩阵，最后安装发布；不能用新增专业功能掩盖
基础工作流的不完整。

### 5.2 编辑、交互与项目工作流

- **Timeline 编辑：** 完成 J/L edit、ripple/roll/slip/slide 的完整键盘与指针交互、同步锁、
  多选/Link Group 反馈、Transition handle 诊断，以及复杂冲突下可预测的 selection 结果。
- **关键帧与曲线：** 补齐 Bezier handle 编辑、Auto/Continuous/Ease、copy/paste/reset、批量移动、
  时间/value snapping；仍只写现有 Hold/Linear/Bezier 作者语义，不另建 UI 曲线格式。
- **素材与恢复：** 批量目录重连、同名/多候选冲突确认、替换素材、缺失字体/LUT/插件诊断、
  Save As、autosave/recovery 浏览与“保留源文件”失败指引。
- **产品 UI：** 完整快捷键发现、上下文菜单、拖放反馈、键盘导航、IME、焦点/无障碍语义，
  中英文 i18n；Widget 不持有第二份业务状态。
- **项目可维护性：** 大项目搜索/筛选、素材文件夹操作、可解释后台任务和可导出的诊断包。

### 5.3 复杂项目、播放与资源治理

- 覆盖手机 VFR、混合帧率/采样率、长 GOP、旋转/非方形像素、长时间线、多层嵌套、
  proxy/original 切换与源文件热变化；每种时间映射必须有 exact source-target 证据。
- 在 8/16/32 GiB 机器合同上验证 active working set、cache trim、后台公平性和恢复；内存压力
  只能降低质量、并发或复用，不能改变作者语义、Clock 或输出来源。
- 完成 window/device replacement、GPU device loss、音频设备切换、睡眠/唤醒、长时 seek/
  supersession，以及 Preview worker 崩溃后的有界恢复。
- 建立五分钟 Hero、30 分钟节目和多小时长项目三级工作负载；性能报告绑定源码、fixture、
  Adapter 和机器身份，禁止用孤立 micro-benchmark 代替产品门禁。

### 5.4 视觉执行与效果

- 完成 Export Transition heterogeneous endpoint、Adjustment accumulator GPU 同步/readback、
  有界 temporal×heterogeneous，以及需要时的 GPU→CPU/GPU 再分段；每条路线在像素工作前
  完成拓扑、内存、transfer、lifetime 和 cancellation 预检。
- 为显式颜色域转换、LUT GPU 执行、嵌套/代理颜色边和 mask/effect 组合建立 Preview/Export parity；
  不允许以 RGBA8 临时旁路或整图 CPU 重跑掩盖半途失败。
- 完成自由 Path/Bezier Mask 的产品级编辑、羽化/扩张交互和缓存失效；tracking、warp、
  temporal/stateful 专业效果仍属于 M3。
- Effect 浏览、Inspector、节点诊断和插件缺失状态必须从同一 Definition/Compiled Graph 投影，
  不创建另一套“UI 可用效果”清单。

### 5.5 色彩、音频与交付

- **色彩：** 覆盖主流 Camera Log 输入、HDR→SDR、SDR/HDR 混合、GPU LUT、代理/嵌套转换和
  PQ/HLG 显示路径；每个已发布目标需要独立 reference、CPU/GPU tolerance 和 metadata 验证。
- **音频：** 提供具名 stereo/5.1/7.1/custom 物理设备 Adapter、长期 device recovery、
  loudness/true-peak 测量、更多内建 Processor，以及基础 Route/Send 产品交互；复杂外部插件留 M3。
- **导出：** 扩展专业预设、图像序列、硬编/软编 capability 矩阵、暂停/恢复与失败诊断；
  job snapshot、临时文件、发布原子性、reimport/probe/QC 仍是统一门槛。
- **一致性：** Preview、Scopes、Export 与 reimport 对同一 Timeline/Color/Audio Program 的差异
  必须是明确 Adapter 策略，并在报告中可定位，不能拥有独立解释。

### 5.6 Windows 发布工程

- 建立安装、升级、回滚、卸载、文件关联、离线运行、依赖许可、crash dump、日志脱敏和
  诊断包；候选包必须可复现并绑定源码、工具链、配置与资产清单。
- 覆盖 Intel/AMD/NVIDIA、集显/独显、常见声卡和显示缩放；Unsupported 原生路径要回到已验证
  Adapter 或明确阻止，不因驱动差异静默改变颜色、Alpha、时间或音频布局。
- 建立 P0/P1 阻断、已知问题、迁移说明和回滚策略；每个候选按受影响范围重跑 source-attested gates。

### 5.7 M2 退出门槛

- 核心编辑、关键帧、重连、恢复与诊断在完整 Golden 中可发现、可 Undo/Redo、可保存重开，
  失败时给出可操作原因；无第二作者路径。
- 五分钟、30 分钟与长项目矩阵通过；8/16/32 GiB 策略均有压力降级和恢复证据，后台工作不饿死
  音频、UI、保存或当前帧。
- 发布的颜色、音频、编码和容器目标均具备独立 reference/roundtrip/QC；Preview/Export 无已知
  P0/P1 语义分歧。
- Windows 候选包在干净机器完成三轮安装→打开旧/新项目→编辑→恢复→双格式交付→卸载闭环，
  无已知 P0/P1，P2 有明确文档与规避方式。

---

## 6. M3 — 专业能力、插件与互操作

### 6.1 高级画面与图形

- 高级 key/matte、garbage matte、warp、motion/optical-flow retime、稳定 tracking、
  temporal/stateful Effect、可编辑节点图和受控缓存生命周期。
- 专业字幕/图文：样式、区域、排版、fallback、导入导出和渲染一致性；标题仍是封闭 Clip Content，
  不退回 renderer-only overlay。
- 多机位、同步、compound/nested 工作流深化，以及代理与原片之间可审计的高阶分析结果。

### 6.2 音频制作

- 标准响度与 true-peak 交付、更多 Channel Layout、Bus/Send/Sidechain、自动化写入模式、
  更多内建 Processor 和可审计离线 bounce。
- VST3/CLAP Adapter 必须具备进程/线程隔离、deadline、state migration、bus negotiation、
  latency/PDC、crash quarantine 和缺失插件恢复；插件不得直接成为作者权威。

### 6.3 Effect 插件平台

- 内建效果与至少一个受控第三方 Adapter 共用 Definition、Parameter Schema、Compiled Graph、
  capability、缓存、失败隔离和 Preview/Export 解释后，才评估 OpenFX 等更广 ABI。
- 插件版本、资源、GPU/CPU 能力、确定性、temporal extent 和 state migration 必须持久化为
  可诊断合同；未知能力失败关闭，不能假定“像素大概兼容”。

### 6.4 交换格式、专业媒体与 QC

- OTIO/EDL 优先实现可往返子集和 machine-readable loss report；AAF/XML 按已验证客户场景推进。
- 未知字段、缺失插件、未支持 transition/effect/audio route 必须保留 metadata 或报告损失，
  不得静默展平、删轨、改时或烘焙成不可追溯结果。
- 增加专业中间格式、图像序列、alpha/HDR 组合、硬编矩阵、断点恢复、校验和、响度/色域/
  黑帧/冻结帧等自动 QC；完成状态始终晚于验证与 durable publication。

### 6.5 M3 退出门槛

- 至少一个真实专业项目完成导入→编辑→插件/高级效果→音频混合→专业母版→交换格式回导，
  loss report 与最终 QC 可审计。
- 第三方插件崩溃、超时、缺失、升级和 state 迁移不会破坏 Project、实时线程或 Export job；
  Preview/Export 对相同插件状态具有一致结论。
- 已声明可往返的交换子集通过版本化 corpus；未支持语义全部显式保留或报告，无静默数据丢失。

---

## 7. M4 — macOS/Linux 实机资格与长期发布

### 7.1 共享跨平台合同

- 同一 `.mdp` 跨平台往返不改写无关作者语义；schema migration、强类型 ID、时间、颜色、Alpha、
  Audio Program 和 Effect contract 保持一致。
- CPU reference 跨平台一致；GPU/native 差异使用后端 tolerance、驱动身份和 typed evidence 解释。
  Unsupported 路径只能进入已验证 fallback 或明确 blocker。
- 平台 Adapter 不得向共享 Timeline/Project/Effect/Audio API 泄漏 COM、CoreMedia、Metal、VA-API、
  PipeWire 等原生类型。

### 7.2 macOS

- VideoToolbox/CVPixelBuffer→Metal 零拷贝或低拷贝、颜色附件、VFR、seek/supersession、device loss。
- CoreAudio 设备枚举、layout、热插拔、时钟与 recovery；Retina/EDR/HDR、ICC/profile 和多显示器。
- 原生窗口/菜单/IME/文件选择、sandbox/权限、应用包、签名、公证、升级/卸载和 crash report。
- Apple Silicon 为主资格，Intel 支持范围必须明确；每个发布版本绑定 macOS/硬件矩阵。

### 7.3 Linux

- VA-API/DMABUF→Vulkan 路径、软件 fallback、颜色 metadata、VFR、seek/supersession 和驱动隔离。
- PipeWire 优先并覆盖 ALSA fallback、设备热插拔、layout、时钟与 recovery；Wayland/X11 的窗口、
  输入、缩放、拖放、剪贴板和桌面门户行为一致。
- 选择明确发行版/驱动支持矩阵，完成 AppImage/Flatpak 或其他确定发布载体、依赖许可、更新和诊断。

### 7.4 长期兼容与发布运维

- 建立旧项目 corpus、跨版本 migration、前向拒绝、插件缺失、媒体离线和跨平台 roundtrip 门禁；
  任何格式演进都要有恢复/回滚策略。
- 建立多平台可复现构建、签名资产治理、SBOM/许可证、安全更新、崩溃/性能遥测隐私策略和
  长期支持窗口。
- 每个平台独立拥有 native device、安装和长时压力资格；共享单测或 CI 编译不能替代实机声明。

### 7.5 M4 退出门槛

- Windows、macOS、Linux 各自在声明的最低/推荐硬件上完成三轮安装→跨平台项目往返→编辑→
  恢复→Preview→双格式交付→卸载；作者状态和输出语义无未解释差异。
- 各平台通过真实视频/音频/显示 device loss 与长时压力矩阵，无已知 P0/P1；fallback 与 blocker
  在 UI、日志和报告中一致。
- 发布包可复现、签名/公证或发行载体合规，升级和旧项目 migration 可回滚，安全与许可证清单完整。

---

## 8. 明确后置与非目标

以下事项只有在其前置里程碑稳定且存在真实需求/fixture 后才进入路线图，不得提前挤占主线：

- 完整相机 RAW/debayer、动态 HDR10+/Dolby Vision、SDI/专业采集输出和广播自动化。
- 复杂 3D/compositing、深度/体积、云媒体、多人实时协作和分布式渲染。
- 8K 全矩阵优化、移动端编辑器、社交平台一键发布和大规模资产管理系统。
- 高阶 AI 自动剪辑、生成式画面/声音或云模型编排；AI 只能调用公开作者 Action，不能成为
  第二作者路径或绕过 Project/Effect/Export 合同。

---

## 9. 路线图维护规则

- 每个里程碑最多保留目标、范围、退出门槛、状态和证据入口；实现流水账不得回填 ROADMAP。
- 已完成项压缩为结果；详细测试名、hash、逐次性能数字和历史修复记录放入验证文档或报告。
- 新能力先判断属于当前里程碑还是后置；不能通过扩大当前里程碑制造永久未完成。
- “代码存在”不是产品证据；同样，“尚未覆盖所有设备”也不能抹掉已经完成的工程里程碑。
- capability 只有 `Verified`、`ExplicitlyDegraded`、`Blocked/Unresolved`；提升状态必须增加真实 fixture。
- 修改 Project schema、Timeline、Renderer/Color、Audio 或持久化 Seam 时，同步更新相应架构文档。
- M1 冻结后，默认资源分配为 70% M2 主线、20% 验证/性能/稳定性、10% 后续必要基础。
