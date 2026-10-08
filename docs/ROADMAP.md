# Mondrian 产品路线图

> 更新日期：2026-10-08
>
> 当前状态：M1 Windows Alpha 收口中（最新具名完整 Golden 与 CLAP 原生侧链参考已通过；CI、设备和完整工作区测试资格待补）
>
> 当前主线：完成 M1 当前候选资格收口，再推进 M2 的产品可靠性与交互完整性

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
| M1 Windows Alpha | **收口中** | 核心工作流已有具名基线；3b44084 完整 Golden 已通过；CI、设备和完整工作区门禁仍待验收 |
| M2 Beta | **下一阶段** | 产品交互、恢复、复杂项目、设备矩阵、独立色彩资格和发布工程 |
| M3 专业能力与互操作 | **后续** | 高级效果、响度、插件、跟踪、交换格式 |
| M4 多平台实机资格 | **后续** | macOS/Linux 真实设备、打包、签名与长期维护 |

### 旧阶段 2–4 清单核查（`feat/nle-backlog`，2026-10-08）

这里的“已接入”表示当前源码有产品路径和针对性测试，不等于完成跨厂商、跨设备的发布资格。
每次平台实机或插件矩阵资格仍须绑定当时的源码版本重新验证。

| 原需求 | 当前代码状态 | 下一步 |
| --- | --- | --- |
| Dropdown 更新标签、分隔行与混合模式分组 | 已接入 `set_label`、递归标签本地化、非动作分隔行与类型化混合模式顺序；菜单三态勾选、稳定语义身份、父子关系及本地化名称已验证，输入仍由原控件处理 | 原生屏幕阅读器与物理输入验收 |
| 外部媒体拖入素材库/轨道 | 已接入轨道与素材库单事务提交，取消或未落在轨道不留新素材 | macOS 文件拖入实机验收 |
| Viewer 实时变换、锚点与蒙版编辑 | 已接入位置/缩放/旋转/锚点手柄、Power Window 形状与贝塞尔点插入 | 高分屏、代理/原片和多层项目实机验收 |
| 约束贝塞尔关键帧 | Auto/Continuous 作者约束、稳定 ID 的点与手柄编辑、多选、框选及整组时间／数值／插值／剪切粘贴／鼠标拖动已接入；参数重置使用当前定义默认值，一次清除动画并保留稳定身份，覆盖视觉与音频；框选支持替换／追加／切换、只读检查、取消及布局变化恢复；视觉与音频共用原子准入、作者手势和 Undo，支持取消恢复、保存后保留 Redo 及 Unicode 重开；参数重置与整组编辑的原生参考输出按具体验收版本记录 | 完整作者范围与多键组拖动原生输出、高分屏／性能／平台矩阵；见 [验收记录](dev/modal-resource-lut-acceptance.md) |
| 模态与资源编辑 | 实际路由限制 Tab／Shift+Tab 和后台输入，等待检查的确认按钮退出焦点顺序，超时后可重试；取消／过期／锁定／Save As 目录变化不能提交旧资源草稿；确认通过作者事务，单步 Undo／Redo、保存撤销状态和 Unicode 项目重开已验证 | 原生文件选择器与物理键盘、macOS 实机验收；见 [验收记录](dev/modal-resource-lut-acceptance.md) |
| 通知 | 终态通知与键盘历史已接入；完整本地化无障碍文本、分页／边界／失焦、窄宽度伪语言绘制及高负载优先级已验证；最多两条弹出，错误优先，持续进度不弹出 | 原生屏幕阅读器、物理输入与跨平台验收；见 [验收记录](dev/modal-resource-lut-acceptance.md) |
| 运动跟踪蒙版 | 目标平移/平面单应跟踪、取消、重算、源指纹及原子结果发布已接入 | 遮挡、失焦、长片段和真实素材矩阵 |
| 插件抽象 | 视觉 Effect Definition/Compiled Graph、执行会话独占的有界处理器资源、机器分级驻留配额与隔离音频 Worker 已接入 | 驻留压力/多效果性能资格、插件管理、能力协商与厂商矩阵 |
| 特效库 | 19 个内建视觉效果和统一 Effect Browser 已接入 | 按真实剪辑工作流补齐效果与独立像素参考 |
| 项目打包 | `.mdp`、媒体、LUT 与 OCIO 依赖盘点、搬迁和重开验证已接入 | 大项目、缺失资源和跨平台搬迁实机验收 |
| i18n | Fluent 中英文、伪语言和主要界面路径已接入；状态提示与历史保存消息 ID/参数并即时切换语言，159 个非测试模块调用点已迁移；内置特效、Title、Mask 的参数名称、分组和枚举选项按稳定定义地址展示本地化文本，作者键值保持不变 | 清除剩余用户可见字面量，类型化内部诊断原因并补齐语言验收 |
| 参数数值输入 | Rack、Mixer 和组件参数保留 f64；整数拒绝小数，JSON 保存重开保持浮点位值；文本草稿在确认前不写入项目，Escape 取消不产生关键帧或撤销记录；Inspector 位置/缩放/锚点与通用 Vec2/Vec3/Vec4 控件只提交变化分量，保留其他分量、独立关键帧及未修改曲线的共享存储；Clip/Effect/Mask 统一参数写入契约；Undo/Redo 保留当前编辑时间，跨帧率按真实播放时间重新绑定；撤销删除当前序列时恢复原序列导航，后续 Viewer 完整变换可精确撤销重做；Inspector 滑块拖动保留捕获并持续刷新 Viewer；数值曲线点／手柄拖动持续更新 canonical 作者状态与 Viewer，结束时保留一个历史项，取消和回到原点不留历史 | 完整键盘操作、厂商参数与平台矩阵 |
| CLAP / VST3 | 用户选择的 Float32 音频效果可隔离处理；自有参考插件的约束曲线、剪贴板、参数重置与 Undo／保存后 Redo／Unicode 重开已通过逐样本输出验收；b5eb715 的 CLAP／VST3 同主布局辅助输入已通过真实打包 worker、自有立体声侧链、激活／状态／分块／重置、PDC 与保存重开参考验收；Mixer 总线删除投影已统一领域依赖检查，启用／禁用侧链连接与锁定来源回归通过 | Mixer／Inspector 侧链来源、启用、静态增益与断开控件已接入当前观察绑定；安全前向连接的阶段调度正在收口；多布局、晚到辅助信号的主路径 PDC、MIDI／编辑器、厂商状态／参数及性能资格待推进 |
| OpenFX | Filter / RGBA Float32 / Double/Boolean/Integer 子集接入共享效果图，插件声明的默认插值贯通新关键帧与数值写入，视觉参数 Inspector 按数值声明保留整数与 Double 输入精度；工厂绑定作者实例与执行会话，普通 CPU 合成/时间采样/混合执行取消及逻辑帧预算贯通；原生图像按 action 借用并检查释放，UHD 参考路径在 768 MiB 逻辑工作预算内通过；Preview/CPU-prefix/fallback/Export 已接入原生驻留配额并跨帧复用，取消及故障仍回收；canonical 常量／曲线的完整值、导数、积分和关键帧查询已通过真实 SDK；拒绝未支持的有序/时间采样契约 | 插件发起的持久作者写入、更多上下文／像素格式、厂商／平台与性能矩阵 |

---

## 3. M0 — 核心契约与架构收敛（完成）

M0 已完成以下结果，技术合同以 `CONTEXT.md`、ADR 和架构文档为准：

- current document v27/library v6、封闭 `ClipContent`、强编辑关系和显式 schema 拒绝。
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

资格以具体源码和报告为准；下表区分已验收基线与最新修订待补项。`0adaea304c6d6f672ef61c43c0ea4a9f668971fa`
曾在 2026-10-08 完成 Windows 产品重建、CLAP／VST3 自有单主总线参考验收与完整
Golden 连续三轮；源码起止干净且一致，产品哈希全程一致。见
[验收记录](dev/modal-resource-lut-acceptance.md#clean-native-audio-reset-and-current-candidate-qualification)。
这份候选尚未推送，不能继承 2026-08 历史版本的三平台 CI、4K／设备或完整工作区
测试资格。最新原生 CLAP 修订 `ae3c78f` 已通过激活生命周期、辅助输入、PDC、
Undo／保存重开与逐样本输出参考，见
[侧链验收](dev/modal-resource-lut-acceptance.md#clean-activated-latency-clap-auxiliary-qualification)。
随后 3b44084 完整 Golden 通过 3/3，模态 Tab、资源事务、LUT 输出／打包搬迁和
原生音频历史专项通过，见
[当前完整验收](dev/modal-resource-lut-acceptance.md#current-activated-host-modal-resource-and-lut-acceptance)。
当前 `b5eb715` 已通过 CLAP／VST3 原生侧链及三项主输入参数／状态回归、完整
Golden 连续 3/3，以及模态／资源／LUT／打包／音频历史专项。源码和二进制身份
稳定，见 [当前验收](dev/modal-resource-lut-acceptance.md#current-vst3-auxiliary-modal-resource-and-lut-acceptance)。
历史记录继续保留其原始版本；下表分别列出实现状态与实际资格。

| 能力 | Engineering state | 具名资格与待补项 |
| --- | --- | --- |
| 项目与恢复 | 实现闭合：durable save/autosave、SQLite snapshot、非阻塞 close、恢复确认和故障注入不覆盖源 | b5eb715 完整 Golden 已通过 3/3，资源事务／原生重开专项通过 |
| 素材与代理 | 实现闭合：有界 probe、fingerprint、稳定 AssetId、离线/重连、proxy/original 与 cache revision | b5eb715 Golden Proxy/Relink 已覆盖 |
| 编辑 | M1 核心操作、Link、Lift/Extract、T/S、snapping、常速/反向/hold 与 Transition 已接入 | b5eb715 Golden Editorial/Transport 阶段已通过；专项 Retime 矩阵保留具名历史记录并随候选验收 |
| 播放 | 实现闭合：Clock、latest-wins、严格帧覆盖、取消、CPU/native decode 与 Viewer completion | 历史 v7 4K Main10 基线已通过；当前候选专门矩阵待验收 |
| 音频 | 实现闭合：gain/pan/fade、mute/solo、meter、automation、PDC、limiter、嵌套与共享 Runtime | b5eb715 CLAP／VST3 侧链、主输入参数／状态、历史／PCM 和 Golden 参考已通过；历史 CPAL A/V recovery 基线保留，当前设备矩阵待验收 |
| 视觉 | 作者级 Crop、基础算子、Basic Title、Cross Dissolve 与 Hold/Linear/Bezier 作者语义已接入 | b5eb715 Golden 视觉阶段已通过；完整 CPU/GPU reference 矩阵保留历史基线，当前专项待验收 |
| 色彩 | Rec.709/sRGB、PQ、HLG 与 straight Alpha 已有独立 reference；未知解释会阻止或要求 override | b5eb715 Golden 色彩／媒体阶段与 LUT 资源输出专项已通过；完整独立 reference 矩阵保留历史基线，当前专项待验收；Camera Log 属于 M2 |
| 导出 | H.264 High/AAC SDR、HEVC Main10、不可变 snapshot、取消、失败清理与完成前 probe 已接入 | b5eb715 Golden 交付／重开阶段已通过；历史长 Work Area 基线保留，当前专项待验收 |
| 平台与质量 | 三平台生产入口和 Adapter 已进入 CI 配置；本地 workspace 门禁可执行 | 历史三平台 CI 基线保留；当前未推送候选的三平台 CI 与完整工作区测试待验收 |

### 4.3 退出门槛

- [x] current schema 可保存、恢复、重开；未来/未知 schema 和不确定持久化结果失败关闭。
- [x] 一个参数与一个音频编辑从作者命令贯通 Undo/Redo、保存重开、Preview/Export 和 cache invalidation。
- [x] 同一 Hero Sequence 完成编辑、代理/重连、嵌套、恢复、标题、转场、效果、音频和两种交付。
- [x] 作者级 Crop 贯通 stable ParameterId、动画、Inspector、Undo/Redo、持久化、Preview/Export、cache identity 与 CPU/GPU reference。
- [ ] 当前候选 HLG 1000-nit、PQ、Rec.709/sRGB/Alpha 的独立 reference 矩阵验收；既有具名基线继续保留。
- [ ] 当前候选完成 v7 4K Main10 播放/seek/supersession 和真实 CPAL A/V recovery；历史具名报告只作回归基线。
- [x] 所有生产结果归类为 `Verified`、`ExplicitlyDegraded` 或 `Blocked/Unresolved`；没有隐式 RGBA8、错误源帧、Video Master 或静默效果旁路。
- [ ] 当前候选的 Windows/Linux/macOS native CI 通过，平台专属类型不泄漏到共享语义。
- [x] 具名 b5eb715 完整 Golden 连续三轮通过；保留源码、二进制与各阶段报告身份。后续源码修订须重新验收。
- [ ] 当前候选完整 workspace fmt/clippy/test 与专门设备门禁收口；已通过的针对性库回归和严格 Clippy 不替代全部矩阵。

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

- **已建立的发布前置门禁：** `windows-commercial-engine-v1` 把精确、干净、可信分支源码与
  Complete Golden 三连跑、真实 4K Main10 Video 和 30 分钟 CPAL A/V Playback 证据封装为
  可复验哈希制品；Release 对缺失、过期或不匹配证据 fail-closed。它只证明 Windows 引擎候选，
  在下述候选包闭环完成前 GitHub Release 必须保持 draft，Linux/macOS 不生成发布制品。
- **已建立的 portable 候选门禁：** `windows-portable-candidate-v1` 只构建一次 ZIP，以
  source/package/contract 哈希绑定身份，并由三个独立干净 `windows-2022` runner 验证同一字节、
  隔离用户状态和净化 PATH 下的完整运行时。它不替代安装或产品工作流资格。
- 建立安装、重装、失败回滚、卸载、文件关联、离线运行、依赖许可、crash dump、日志脱敏和
  诊断包；候选包必须可复现并绑定源码、工具链、配置与资产清单。
- 覆盖 Intel/AMD/NVIDIA、集显/独显、常见声卡和显示缩放；Unsupported 原生路径要回到已验证
  Adapter 或明确阻止，不因驱动差异静默改变颜色、Alpha、时间或音频布局。
- 建立 P0/P1 阻断、已知问题、当前格式说明和回滚策略；第一版公开格式不承担未发行 alpha
  格式迁移，公开 v1 以后所有兼容性变化再进入强制 migration/拒绝门禁。每个候选按受影响范围
  重跑 source-attested gates。

### 5.7 M2 退出门槛

- 核心编辑、关键帧、重连、恢复与诊断在完整 Golden 中可发现、可 Undo/Redo、可保存重开，
  失败时给出可操作原因；无第二作者路径。
- 五分钟、30 分钟与长项目矩阵通过；8/16/32 GiB 策略均有压力降级和恢复证据，后台工作不饿死
  音频、UI、保存或当前帧。
- 发布的颜色、音频、编码和容器目标均具备独立 reference/roundtrip/QC；Preview/Export 无已知
  P0/P1 语义分歧。
- Windows 候选包在干净机器完成三轮安装→创建/打开当前 schema 项目→编辑→恢复→双格式交付→卸载闭环，
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
