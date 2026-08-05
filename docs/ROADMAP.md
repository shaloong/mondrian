# Mondrian 产品路线图

> 更新日期：2026-08-05
>
> 当前主目标：以同一套跨平台产品实现完成可信的真实项目制作闭环；M1/M2 暂以 Windows 实机作为发布资格证据平台，但不得把 Windows 专属实现写入共享语义。
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
| macOS/Linux | 与 Windows 共用生产契约和产品入口；常规 CI 必须编译/测试，真实设备与发布资格证据可晚于 Windows |

### 1.2 不可妥协的规则

1. **“有实现”不等于“产品可用”。** 只有同时接入主路径、持久化、Undo/Redo、预览/导出、诊断和回归测试的能力才可进入 L1。
2. **基础契约先于功能扩张。** 项目迁移、时间映射、帧/色彩/Alpha、参数、音频时钟和任务取消不稳定时，不扩张依赖它们的长尾能力。
3. **跨平台实现现在完成，实机资格分阶段取得。** Windows、Linux、macOS 必须共用项目、时间线、播放、音频、效果、色彩、Viewer 与导出契约；平台差异只位于 `mondrian-platform*`、媒体/渲染器原生后端或明确 Adapter 内。M1/M2 允许只以 Windows 真实 GPU、硬解、音频设备和显示器证据阻止发布，但这不能授权共享 Module 依赖 Win32、D3D12 或 Windows 行为，也不能把 Linux/macOS 的缺失实现伪装成“已预留”。
4. **预览与导出共享解释，不要求共享调度。** 输入解释、时间线求值、效果顺序、合成、Alpha 和最终输出变换必须一致；缓存寿命、优先级和是否读回 CPU 可以不同。
5. **降级必须可见且语义正确。** CPU 解码、代理、降低预览分辨率、丢弃迟到视频帧、HDR 到 SDR view transform 均可接受；静默猜测、错误色彩、隐式 RGBA8 量化和把 CPU 路径冒充 GPU 路径不可接受。
6. **保持模块深度。** 新模块必须用小而稳定的接口隐藏调度、缓存或求值复杂度；只有一个适配器且没有真实替换需求的透传模块不应被创建。

---

## 2. 代码现状审查结论

本节是 2026-08-05 的代码基线，不是目标清单。状态区分“模型/算法存在”“已接入主路径”“已有真实项目证据”，避免从类型或单元测试推断产品完成度。

| 能力域 | 已有事实 | 仍不足以宣称完成的部分 | 当前判断 |
| --- | --- | --- | --- |
| 核心时间与 ID | 作者位置、范围、曲线与 handle 使用 canonical `TimelineTime` 和显式 Time Domain/Transform。`FramePosition` 只表示一个显式 Evaluation/Display Grid 上的精确坐标，不能丢弃 `time_base` 后把整数帧套到另一网格。声称已在 Sequence 网格上的执行请求必须严格匹配；允许接收外部/UI 网格的语义 Adapter 则先转换为精确 `TimelineTime`，再按显式舍入策略在目标 Sequence 网格量化一次。Sequence 的单一显示设置解析为 Viewer/时间轴共享的 NDF/DF/Frames 契约；fixture 覆盖跨网格 Action、VFR PTS、混合帧率嵌套、负时间和长项目 | 素材源 timecode、用户输入/解析、time-of-day/reel 语义及未来真正的 Feet+Frames 仍须分别定义输入、持久化、舍入、错误提示与 roundtrip 证据，不能从格式化器反推“已支持” | L1；精确作者时间与帧网格边界闭合 |
| 项目持久化 | 当前格式为 document v24/library v5。`AuthoringSession` 生成不可变作者与 SQLite revision 快照；SQLite online backup、身份绑定 sibling archive 与唯一 Storage Seam 共同产生 durable publication evidence。只有 durable evidence 可推进保存 baseline 或清理 Recovery Authority；旧 Session、旧 Persistence Generation、过期 destination 和非耐久结果都无权发布。运行时 Library 采用不可变代际目录与 path-paired lease，已打开目录不被 rename/swap；Recovery 以规范 manifest、文件身份和完整 Project/author/library/document evidence 闭合发现、选择、加载、保留与退役。Window close/quit 已使用 move-only 非阻塞 pause ticket：Save 与 FIFO barrier 保持一个 worker 顺序且绑定精确请求，作者 Action 和 Project-scoped completion 在 quiescing 期间冻结，UI 以有界 cadence 保持响应；保存失败会恢复 generation，协议故障只允许显式 Discard 后无权威 detach | Alpha 对非 current document 明确拒绝且不承诺迁移。M1 仍须以恢复来源/时间/目标/冲突 UI 为输入，验证磁盘满、权限失败、反复崩溃、close/Save As 和慢盘下不覆盖源、不误清 Recovery Authority、旧 completion 不发布，并提交三轮可重现故障证据 | L1 所有权、代际、正常恢复与非阻塞 close handoff 闭合；M1 故障注入/实机交互证据未完成 |
| Undo/Redo | `AuthoringSession` 是唯一可变 `ProjectDocument` 权威。Sequence 与作用域化 Project restore point 是两种连续 typed-state endpoint；Undo/Redo 在移动栈前验证 source/scope、复用 unaffected Sequence、深验完整 Project，并保留当前 document revision、更新时间与有效导航。no-op 在 mutable access 前返回，只有 touched Track/collection detach；History 以 200 条/256 MiB 的版本化 logical charge 计费，未保留事务建立连续性屏障，且该计费不得冒充 allocator/RSS | 当前机器可读门禁必须在同一优化构建和 source attestation 下完成 5/30/120 分钟 × active-heavy/project-heavy 六格，输出连续 6 reports + completion；同时证明 endpoint 连续性、scope/source mismatch 失败关闭、COW locality、200 条深度、120 分钟 precompose Undo/Redo、正常路径无 oversize/barrier，并把专用 retention-disabled/oversize/barrier probe 与正常路径分开。Private Commit 双门禁和 History logical charge 必须独立判定；当前工作树不得继承旧运行结果 | L1 事务架构闭合；当前工作树定量矩阵、8 GiB cross-load 与产品级交互门禁待重跑 |
| 素材管理 | 文件夹/Bin、移动/重命名/退役、缩略图、离线、重连和代理已接入产品 UI。library v5 的 `retired_at` 是持久化强记录：移除只改变普通 Library/Bin 可见性，事务不得删除 Asset row、Timeline/其他 Sequence 引用、Undo/Redo endpoint 或 proxy membership；同源重导入恢复原 `AssetId`，在 Project+History+SQLite 没有统一 purge authority 前禁止物理清除。Import/重连以完整文件指纹和 probe candidate 两阶段提交，旧 generation 不能写新 Library；文件只有被证明恰好一帧才分类为 `StillImage`，否则按时变媒体失败关闭 | 仍须以目录批量重连、运行中 source replacement、动画/多页图片及多素材嵌套为输入，证明 waveform/thumbnail/decode/proxy 全部按新 revision 失效、冲突有选择界面、旧结果不发布且所有强引用保持；验收需含保存重开、Undo/Redo、代理往返和压力证据 | L1 持久化与执行边界闭合；批量产品验收未完成 |
| 时间线编辑 | 多轨 Move/Split/Trim/Ripple/Overwrite/Roll/Slip/Slide/Insert/Lift/Extract、锁定、吸附、多选、Link Group 和嵌套已有类型化作者事务。Insert/Lift/Extract 显式携带 content/target/ripple Track、半开范围及 automation/Transition/navigation 策略；候选求值与执行共用验证，锁轨、链接闭包或 handle 不满足时整项失败。Track Targeting 与 Sync-Lock 是独立会话状态；定向 Split 返回完整链接成员身份 receipt。Transition 使用强端点、精确范围和 unclamped 双源 handle demand，创建/缩短/删除各为一次 Undo | Golden v12 已在同一 Hero 中通过当前声明的 Lift/Extract、Link、Transition、嵌套、Undo/Redo 与保存重开切片；仍须以复杂 J/L linked edge、mute/visibility、真实媒体与嵌套 Transition、reverse/freeze/variable remap 为输入，定义 source/handle/automation 传播和失败提示，并完成 T/S 工作区持久化、代理/原片与独立视觉 reference | L1+ 作者、命令与基础 UI 闭合；扩展产品交互和媒体边界证据未完成 |
| 播放与缓存 | UI 无关 `PlaybackEngine`、`FrameWorkBroker`、`PreviewFrameStore`、三种访问语义、FFmpeg Session/isolated helper、generation/cancellation/deadline 和 Window/Headless production runtime 已接入。解码选择以 `DecodedTemporalExtent` 的半开 `[start,end)` 为权威：正 duration 与首个 successor 共同收紧边界，跨 forward call 最多保留 selected + successor 两个候选。Playback/Still 只接受因果覆盖请求的帧，否则失败关闭；Scrub 才可把非覆盖近邻标为 Explicitly degraded。取消记录 request→checkpoint→return，旧 generation 不得发布。单一 `viewer_gpu_device_progress` 非 UI worker 驱动 exact submission/callback；替换 Window 时同一 device generation、retained owner 与 cleanup authority 一并 handoff，迟到 callback 只能 retire 旧提交 | 当前工作树仍须重跑 canonical 30 分钟 Video+CPAL/A/V、100 次 seek、5 ms checkpoint/50 ms Playback-Interactive/500 ms Still 取消、GPU exact-completion、Window replacement/handoff、完整产品进程树内存与 post-reap 门禁；还需多 GPU/驱动/音频设备、真实阻塞媒体、8 GiB 降级和声学 loopback。任何单元测试、旧运行或 probe 存在都不能替代这些门禁 | L1 代码契约闭合；当前工作树长时、硬件和 Window handoff 门禁未确认 |
| 执行资源治理 | 版本化协调 Module 把物理内存档位、七个重任务域和进程/系统压力投影为不可变 admission decision；各域仍独立拥有队列、deadline、取消和失败分类。Windows、Linux、macOS 已分别以系统 API、`/proc`、Mach/libproc 实现物理/系统内存和完整产品进程树观测；结果携带不可互换的 Windows Private Commit、Linux anonymous resident 或 macOS physical footprint metric。平台实现按 `memory`、`process_memory`、`playback_scheduling` 深 Module 保持 Locality；Windows MMCSS、macOS pthread QoS 与 Linux per-thread nice 均为线程仿射且可恢复，Linux UI/event loop 不进入实时调度类。原生优先级不可用或失败时，本次驻留稳定进入 `PortableFallback`，Pause/Stop 后才重置并允许重试，不产生 `Unsupported → Inactive` 的伪状态跳变。8/16/32 GiB 分别开放 1/2/4 个粗粒度域槽，Preview/Export/Viewer 在执行前冻结 hard grant | 必须在独立 8 GiB 环境以 Preview+音频+Import+Thumbnail+Waveform+Proxy+Export 为输入，证明实时音频/UI 不饥饿、no-overlap handoff、旧结果不发布、压力恢复可重复、内存/吞吐有实测证据；平台资格报告必须绑定 exact metric，Windows Private Commit 门禁不得接受 Linux/macOS footprint 替代。逻辑 byte grant 不得冒充 VRAM/RSS | L1 调度、工作集策略与三平台观测 Implementation 闭合；跨负载实机门禁未完成 |
| 原生硬解/低拷贝 | Windows D3D11/D3D12、macOS VideoToolbox/CVPixelBuffer→Metal、Linux VA-API→DRM PRIME DMA-BUF→Vulkan 已接入同一生产 Interface。媒体层分别保留并验证原生资源；Metal/Vulkan 薄 Adapter 只校验/包装平面，YUV sampling、range/matrix/chroma、source-to-working OCIO 与 working-frame 资源事务由同一 renderer Module 执行。Linux 仅接受 FourCC 与 NV12/P010 一致、单 typed layer、双平面、object/offset/pitch/modifier 全部可证的描述符，其他布局结构化回退；报告区分 native GPU、upload、readback、degraded 与 blocked | 当前 Windows 工作树须在 canonical 4K HEVC Main10 长素材上重跑连续播放、100 seek、取消、exact GPU completion、零错误 readback/fallback、helper post-reap 和内存收敛。macOS/Linux 专属代码仍须在原生 CI 编译通过，并以真实 Metal/VideoToolbox、Vulkan/VA-API 设备做像素 reference、同步、seek/cancel、device loss、内存与性能资格；本 Windows 主机的跨 target 会先被 OCIO C++、DBus/sysroot 工具链阻止，不能冒充平台验证 | 三平台生产路径已实现；Windows 共享编译通过，macOS/Linux 原生 CI 与实机资格未确认 |
| 渲染与色彩 | Preview/Export 共用按 Sequence/Registry revision 与作者 fingerprint 绑定的不可变 `PreparedVisualProgram`；逐帧 `PreparedVisualFrameClosure` 固定 nested time、canvas/color、Transition/temporal binding 与 instance path，区间 `PreparedVisualRangeClosure` 固定 selected-range 的媒体、Transition 和字体依赖。`CompiledEffectGraph` 是唯一生产 IR，动态 topology/cache 只驻留于调用方 `EffectExecutionSession`。生产帧入口由 `TimelineCompositeScratch` 一次绑定 Generation，并用同一 Session 完成普通 Render Plan 求值、全部 temporal root/sample 精确时刻重求值及后续像素执行；Preview/Export 不再拼接无 Session 的第二套 temporal 准备。有限时域 CPU-F32 执行把根图、精确请求、按 `(Clip time, graph value)` 寻址的 time-expanded program 与原始 Source demand 冻结为一个对象；temporal tap 可采样稳定 Definition-stage input，并在各采样时刻从同一 Prepared Effect Program 重求值全部上游效果、动画参数、动态 topology、frame seed 与 Mask。expanded schedule/use-count 驱动精确去重和 live-set；完整请求超出 grant 时会在最多 4,096 个非重叠 tile 内自动执行。`PreparedMaskRaster` 按每个可达时间上下文准备 Path/BVH，以全局坐标、受限 row scratch 和合作取消服务 direct/tiled 路径；Blur/Vignette/Grain、Mask、含 Dissolve 的 temporal DAG、signed multi-tap、sampled upstream 参数差异、同代 topology 复用及换代清空均有 evidence。异构 CPU prefix 已按唯一 value plan 执行 source-closed DAG、synthetic MaskSource 与 fan-out/join live-set；单次 upload 后的 GPU suffix 也直接由该 plan 准备为 dispatch/release schedule，线性 point tail 保持 fused pass，共享 GPU value 的双 point 分支可经 scene-linear Normal Blend 汇合。wgpu Adapter 在记录前校验 materialization/wait/signal/release/terminal live-set，并把单 command buffer 无物理别名时的保守 recording residency 与抽象 plan peak 分开准入；真实 GPU 已通过完整 scalar graph parity。Mask preparation/raster 保留 attempt 的取消/deadline 原因，提交开始后的取消、GPU 或证据失败是本次 attempt 终态，不隐式整图 CPU 重跑 | Temporal 与异构仍是 bounded execution：internal same-stage temporal address、unbounded/stateful、multi-transfer、GPU Mask/MultiInput/非 Normal join、颜色域转换、temporal×heterogeneous 和广义 GPU parity 未闭合。后续必须以生产有限时域/扩大 ROI Processor、嵌套和更多 placement 为输入，证明 exact window、halo/full-frame/unknown、continuity、resource lifetime、seek/cancel 与 CPU/GPU reference parity；缺少 exact mode、live-set 或 Adapter 时在像素执行前阻止。Log/HDR 仍需独立 reference | L1+ 唯一 IR、Prepared closure、精确 ROI、effected temporal upstream、owner-scoped temporal topology、受限自动切片、bounded temporal DAG scalar 与 bounded GPU fan-out/join 边界闭合；通用 Temporal/ROI/异构执行未完成 |
| 效果与动画 | 稳定 Effect/Parameter/Keyframe 身份、版本化 Parameter Schema、Property/Animation、DAG、Mask 和 plugin definition/DSL 已接入。Clip 直接启用状态、Solid Color、Transform/Opacity/Basic Title 参数及数值曲线由唯一 `ClipProductAction` 承载；Inspector、Viewer、Headless 只投影同一 Interface。参数批次只携带 `ClipId + AnimationParameterAddress + PropertyValue`，当前 Track/锁定与 Clip-local author time 在深层 Module 重解析；任一空批次、重复、stale、类型错误或无效成员会在候选前整批拒绝。曲线权威时写入完整 key，保留 Keyframe 身份、Bezier handle/插值/flags，不在曲线下制造隐藏 static 值。Effect 子实体仍由独立 `VisualEffectProductAction` 承载：添加只实例化当前可执行注册 Definition 并为每个实例 fork owner-local `AnimationTrackId`；排序使用 EffectId 相对 anchor；参数写使用稳定地址，字符串 path/index 不充当外部身份。Basic Title 与 Cross Dissolve 已贯通作者、Undo、保存重开和共享 Preview/Export 路径；效果库只展示可构图定义 | 仍须以 Ease/handle、copy/paste/reset、长文本/缺字、真实媒体/嵌套 Transition 及首批算子为输入，逐项证明产品可选、图可执行、CPU/GPU backend、缓存失效、Preview/Export parity 和独立视觉 reference；没有 render op 或 backend 的 definition 必须保持 blocked | L1 地基、Clip 直接参数与视觉 Effect 作者 Action 闭合；首批算子与产品广度未完成 |
| 音频 | 当前 document v24 承载规范 signal layout、Component matrix、typed Route/send、Rack、Automation 与 Transition；Track→Clip 是 placement SSOT。播放、导出、Audition、Analysis 和嵌套共用 `compile → prepare → Session → Runtime`；dense schedule、PDC、tail、lookahead 和 Processor Host 均有显式预算，callback 不扫描作者图、不分配、不做 I/O 或隐式 clipping。Clip/Scope/Channel/Route/Processor 的数值自动化现共用稳定 `AudioAutomationTarget`、显式 `AuthoringTimeDomain`、单一事务 Interface 与同一个 CurveEditor Adapter；静态控件在曲线权威时不可编辑。根 Playback Runtime 已接受 open-Session solo overlay，嵌套输出保持 canonical；Track/Bus/Program Output 的 post-mute meter 以单次 block-atomic bank 无锁发布，失败块和 hidden priming 不会泄漏。首个生产非零算法延迟 Processor 是 linked-channel sample-peak Lookahead Limiter：毫秒 Lookahead 在 prepare 时向上量化并进入 PDC，Ceiling/Release 保持 signal-time automation，固定容量 monotonic window 与 SIMD gain pass 已通过独立 scalar reference、分块、seek、资源和保存重开证据 | 仍须以多布局设备、stateful reverse、sidechain、独立 oversampled true-peak/响度、更多生产 Processor、外部插件 Adapter 与完整自动化/solo/meter Golden 为输入，证明 layout negotiation、PDC/entry/tail、实时 deadline、嵌套与播放/导出 PCM 一致。当前 meter 与 limiter 只承诺 sample peak，绝不能作为 true-peak/响度证据。不支持布局或预算必须带原因拒绝。当前工作树真实 CPAL/A/V 长时、多设备及声学 loopback 门禁待重跑 | L1+ 执行地基、Mixer/Rack、基础精确自动化、瞬态 solo 与分层 sample meter 产品路径闭合；插件、标准响度与发布设备证据未完成 |
| 导出 | 产品/Headless 共用内建 preset 目录和单一 delivery resolver；App 预检、队列、执行与完成 probe 共用容器、codec/profile、位深、range、chroma、Alpha、fps、画幅、色彩目标及音频合同。`Completed` 只有在 Required/Forbidden stream、mux、CICP/range、时长、stream-local PTS/time-base 和音频格式均有精确证据后发布；不可变快照冻结媒体画幅、Program Output/Color Engine 与执行 closure，目标尺寸进入 decode cache identity | 仍须以长 Work Area、变帧率/重采样、多布局、覆盖目标、失败重试、磁盘失败和取消为输入，证明 H.264/AAC 与 HEVC Main10 的像素/音频 roundtrip、A/V 边界、色彩/Alpha/位深、失败清理和 durable publication；GOP/二遍、硬编、图像序列和专业中间格式只有具备 capability probe、独立 reference 与重导入证据才可开放 | L1+ 交付合同闭合；长项目与发布机证据未完成 |
| 自研 UI | winit/wgpu 产品入口、retained widget、主题 token、事件/焦点/IME、Dock、面板和大量组件测试已建立 | 交互一致性和无障碍仍需真实工作流验证；产品字符串大量硬编码，中英文混用，尚无 message ID/pseudo-locale 基础 | L1-；i18n 为 L0 |
| 插件 | 内部效果 definition、graph DSL、能力/缓存/失败隔离契约已有；plugin contract 已成为不可变 `EffectDefinition` 的一部分，不再由平行全局合同注册表提供，运行期 quarantine 只按精确 Definition Registry generation 记录，不能污染同 key 的替换定义 | 尚无稳定外部 ABI、包加载/权限/进程隔离/兼容矩阵；generation-scoped quarantine 也不是外部插件 host，当前只能称内部扩展接缝 | L0 |
| AI | Provider trait、workflow schema 与 fail-closed orchestrator 存在；没有生产 Adapter 时，未知/未绑定 Provider 与 `place_on_timeline` 都返回结构化失败，且不会发布虚假的 step/workflow completion | 尚无生产 Provider 或 UI-independent typed editor Action Adapter，不进入核心发布承诺 | L0 |
| 跨平台 | Platform Execution Contract、Noop/headless、winit/wgpu 产品入口、显示 ICC/HDR、物理/系统/进程树内存、播放线程调度，以及 D3D12/Metal/Vulkan 原生视频导入均有三平台生产 Implementation；平台差异局限于深 Adapter，Project/Timeline/Playback/Audio/Effects/Color/Viewer/Export 不携带 OS handle 或第二套解释。常规 CI 已配置 Windows/Linux/macOS workspace、完整 App path 和 Linux UI Gate，Release 也构建三平台 | 当前本机只完成 Windows 全特性编译；macOS/Linux 必须由原生 runner 关闭 FFI/HAL 编译，随后补真实设备、默认/非默认音频设备与多声道布局、ICC/HDR 显示、动态依赖打包和安装/卸载资格。原生设备缺失允许语义正确的 CPU/upload/SDR fallback，但 capability 不能由 OS 名称单独冒充执行成功 | 共享架构与三平台 Implementation 已收敛；原生 CI/设备/发布资格仍未闭合 |

本轮音频执行需求已收敛为编译后 Signal Closure 产生的
[`AudioProgramExecutionDemand`](../crates/mondrian-audio/src/plan.rs)
的 `ProvenSilent`/`RequiresExecution`。播放预热、Playback
和 Export 只消费这一证据；App/UI 不再遍历 Track/Clip 猜测是否“有声音”，因此
mute 后的 pre-mute send、processor-only Bus 和仅用于呈现的 Track visibility
不会被错误剪掉。它完成的是执行准入语义，不代表 Rack/Automation UI、插件或
DAW 级产品广度已经完成。

Export 的不可变快照现含私有、非持久化的 prepared execution attachment：
同一 selected range 的精确 Visual Program closure 与 Audio Program occurrence
closure、媒体/Component 身份、每个文件型图片的完整 source extent、Transition
handle 义务和 Basic Title 字体查询在 capture/admission 闭合。队列准入会重新验证
作者 fingerprint 与闭包证据，并在独立 byte grant 下冻结精确 font bytes/face；
worker 只消费冻结的 Program、字体和素材身份，不读取 live Effect registry、系统
字体目录或重新遍历作者 Track/Clip。反序列化或手工构造的快照没有可执行
attachment，必须在准入处重新准备并验证。

作者性能门禁使用 [schema 10 严格六报告协议](../crates/mondrian-app/src/app/authoring_perf.rs)。
同一优化构建与 source attestation 必须按固定顺序产生 5/30/120 分钟 ×
active-heavy/project-heavy 的 6 份 report 和唯一 completion，并由 completion
证明运行期间 source 未变。每格都要独立通过 timing/reference v4、Locality v3、
History 和 Windows Private Commit 门禁；Locality 同时要求 project median
`<= 2 × active median + 5,000 us` 且 project nearest-rank p95 不超过 reference
p95 budget。retention-disabled、oversize 与 barrier 只由各自专用 probe 判定，
不得混入正常路径；logical History charge 与进程内存证据也不得互相替代。
当前工作树必须重新生成完整协议证据，旧 source 或单次本机报告不赋予当前状态。

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
2. **关键复杂度已有 Locality，但必须防止重新耦合。** Preview production runtime 统一 Window/Headless 的媒体解析、任务、Timeline materialization、Viewer lowering、presentation 与 evidence；Renderer 独占递归闭包、合成和色彩，Playback 独占 Frame Store，Export 独占 admission/lifecycle/cancellation。后续拆分只在形成完整深职责时进行，不按行数制造浅文件，也不允许新 Adapter 复制 nested walker、解码循环、GPU completion 或 Preview/Export 解释。
3. **App 产品动作仍是分片迁移，而非全域闭合。** `ProductAction` 已覆盖 `SelectClip`、`MoveClip`、`TrimClips`、`Seek`、精确 In/Out 与 Lift/Extract intent 的 Timeline slice，以同一请求承载 Clip Processing Scope、Track、Bus、Program Output Rack 与参数自动化的 Audio Processor authoring slice，Viewer 的序列预览分辨率 slice，Project 生命周期/色彩/默认设置和 Sequence 导航/创建/复制/删除/设置 slice，完整 Export 草稿/入队/取消/终态历史 slice，Project Asset Library 的导入/生成/组织/解释/重连/代理/退休 slice，以及 Clip 直接启用/Solid Color/原子参数/数值曲线、Timeline Track、Sequence-owned Video Transition 与 Clip-local visual Effect 的完整作者 slice。Range slice 的唯一 `ui.timeline` codec 以带 time base 的 `FramePosition` 承载 ruler In/Out，拒绝旧裸 frame；Set/Clear 在 mutable access 前拒绝 no-op，Lift/Extract 在 dispatch 时读取最新作者 In/Out 和 Session Target/Sync-Lock，再降低为完整 `RangeEditRequest`，不复制易过期的范围或 Track scope。Current-selection slice 由严格、按变体定形的 `TimelineSelectionEdit` codec 承载 Link/Unlink、Trim/Roll-to-playhead 与批量 Enabled，不让 Widget 复制 selection/Track 快照；深层 Module 在 dispatch 时解析最新身份、按结构编辑需要展开完整 Link Group，并让可用性与执行共用同一纯 Trim/Roll 准备。锁定的未选中 Link Group 成员会拒绝完整 Trim，重复 Enabled 不创建事务；被替代的五个独立字符串解释已经删除。`TrackProductAction` 已用唯一 `ui.track` codec 承载 Add/Move/Visibility/Mute/Lock/Target/Sync-Lock：现有 Track 只携带稳定 `TrackId`，Move 使用同类 Before/After anchor 而非易过期索引，Visibility/Mute 的媒体类型适用性由权威状态重验，作者控制走单事务/Undo，T/S 只改 Session 且 no-op 不伪报成功；旧 Timeline Track 字符串解释、`is_video_track` 和 `target_index` 已删除。`VideoTransitionProductAction` 已独立承载 Select/CreateCrossDissolve/SetRange/Remove；Timeline Adapter 将 resize frame proposal 一次降低为精确 Sequence-local `TimelineTimeRange`，外部 payload 不再携带隐式网格的裸 start/end frame，App 重新解析强端点、Track lock、source extent、handle policy 与 no-op 后才提交一个事务，旧 `ui.timeline` Transition 解释已删除。Transition handle 观察由深层 Module 按 Authoring Session/Generation、Sequence revision 与素材库实例/revision 准备一份不可变快照，Timeline 投影不再按 Track/Transition 重复访问素材库；作者结构损坏与可恢复外部依赖缺失分别报告。Viewer/Inspector 不再各自解释 Transform；二者都生成同一个稳定地址 `ClipProductAction`，Track/媒体类型快照、字符串 path 和百分比专用 payload 已删除。`clip_authoring` 统一 placement/lock/time/address/schema/no-op 预检和单事务提交；Effect 子实体仍由独立 `VisualEffectProductAction` 承载。Visual Effect 已删除旧 `ui.effects`、Inspector Effect JSON payload 和 editor-state 索引排序变体；Definition 缺失、stale identity、锁定与 no-op 不得伪报成功或复制完整 Sequence 候选。Asset Library 的单卡片与多选操作统一降低到原子 `RemoveEntries`/`MoveEntries`，目录导航留在 Window/Shell。Export 入队直接复用 `TimelineExportRequest`，不保留带兼容默认值的平行 UI payload；取消和清理以 Queue 的精确结果为准。Sequence New 返回真实事务结果，不再吞错后伪报成功。所有已迁移 slice 共用只暴露 `allows(&ProductAction)` 的借用式 `ProductActionAvailability`，查询不复制作者图或 Export Job 列表，字符串仅是外部 envelope；Shell 画布缩放仍由 Window Adapter 独占。其余 `Action::Custom` namespace/name 分派和较宽 UI projection 仍是明确技术债。后续必须按可独立验证的完整 vertical slice 迁移，并删掉被替代的字符串解释；不能把这些已迁移切片写成全 App 已类型化，也不能建立与旧层次并存的平行全量 Action hierarchy。
   本轮继续闭合 Timeline 创建/结构和 Audio Component source slice：Basic Title、直接 Asset placement、精确时间 Insert 与 Precompose 均由 `TimelineProductAction` 承载；placement 只携带稳定 Asset/Track 身份和显式 `FramePosition`，Track 类型由权威状态推导；Insert 使用 canonical `TimelineTime` 并让 admission/execution 共用准备；Precompose 深 Module 严格解析 Clip/Link Group/锁定闭包且只分离受影响 Track。打开嵌套已归入 `SequenceProductAction` 并重验当前父子引用。Component source 由 canonical `AudioComponentSource` 和完整 Track/Clip/Edit 地址进入 `ui.audio` codec；Timeline 负责锁定、来源类型、重复选择与完整候选校验，App Audio Module 只证明素材目录或子序列公开输出依赖。旧 `ui.inspector` source payload/dispatcher、Timeline 字符串 dispatcher、`drop_asset` 与旧 nested-open payload 已删除；Shell 等未迁移 slice 仍按上一段技术债处理。

4. **当前 Alpha 项目版本化有严格拒绝、尚无兼容迁移。** 这是未发布 Alpha 的有意清理策略；一旦发布首个承诺兼容的 Alpha，之后每次 schema 变化必须同时提交旧 fixture、事务迁移、失败不覆盖和升级后重开证据，不能继续靠拒绝真实用户项目。
5. **声明能力和视觉执行能力可能分离。** Basic Title、Cross Dissolve、Primary Color 与显式域 LUT 已进入各自声明的主路径，但其完成不能外推到其他仅有类型、属性或 definition 的效果；路线图仍须逐项核对产品选择、Render Plan、具体 backend、Preview/Export 回归证据和独立视觉 reference。GPU LUT 仍明确阻塞，White Balance 仍为 modeled-only。
6. **音频/视频运行许可必须持续分离。** `Priming` 可预填 PCM 但禁止设备提前消费，普通视频 Late/Recovering 不旋转音频 generation。当前工作树仍须在多设备/驱动矩阵覆盖慢首帧/seek、持续视频压力、设备失效和声学 loopback；长时门禁重跑前不得写成发布级同步证据。

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
        │                       └── Clock Master (Audio Device / Synthetic)
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
        ├── Windows adapters（D3D12/Vulkan、原生媒体与设备）
        ├── macOS adapters（Metal、VideoToolbox 与平台设备）
        └── Linux adapters（Vulkan、VA-API 与平台设备）
```

### 3.1 项目文档与迁移

- `ProjectDocument` 是规范持久化语义；UI 临时状态、纹理句柄、线程状态和翻译后文本不得进入项目文件。
- `AuthoringSession` 是唯一可变 Project 权威；保存、执行和 UI 只能取得只读引用或不可变快照，旧 Session 的后台完成不得作用到新 Session。
- archive format、document schema、SQLite schema 分别版本化，迁移按有序步骤执行；禁止用 serde 默认值无声吞掉语义变化。
- archive/manifest 必须在身份绑定的 sibling 对象上完成写入、flush 与 retained-object 验证，再经唯一 Storage Seam 按冻结的 create/replace 意图发布；只有 durable evidence 是成功，命名空间前、耐久未确认和后置状态不确定必须保持类型化。SQLite 必须用 online backup 获取一致快照，不复制活动 WAL 文件集合。
- 每个迁移必须具备：旧 fixture、升级后不变量、再次保存/打开、失败不覆盖源文件、重复执行安全性。
- 缓存、代理、波形和缩略图不嵌入项目；它们的 key 必须包含足以反映项目语义和源文件 revision 的字段。
- 新字段必须定义缺省语义、旧版本迁移语义和 downgrade/unsupported 行为。

### 3.2 编辑命令与 Undo

- 时间线变更只经 `AuthoringSession` 候选事务进入；Widget、Viewer、平台回调和生产 App 命令不得直接借用规范 `Sequence`/`ProjectDocument` 的可变引用。
- 一次用户意图对应一个 undo transaction；链接片段、转场、marker、字幕、音频自动化等关联变化必须原子提交或全部失败。
- 普通 Sequence 事务只复制/安装目标 Sequence，但必须以 Project overlay 验证全部跨 Sequence 强引用、嵌套输出和环；Project 结构事务仍以完整 detached `ProjectDocument` candidate 作为唯一输入、验证与安装 Interface。所有 revision 计算、结构化精确 `before` equality、真实 affected scope、验证、类型化 History 候选和逻辑预算准入必须先于 commit boundary；作用域化 History 端点不能反向成为第二套 Project 编辑 Interface。
- History 只有两种连续 typed-state endpoint：Sequence before/after 为 `AuthoringSnapshot<Sequence>`；Project before/after 为 `AuthoringSnapshot<ProjectRestorePoint>`，只保留 Project-owned author fields、Sequence order/default/结构 active fallback 和精确 affected Sequence body/presence。Project Undo/Redo 必须先验证 current 匹配 opposite source endpoint，再复用 unaffected Sequence 物化完整 target；`document_revision`、`meta.updated_at` 与仍有效的 active navigation 保持当前值。source/scope mismatch、缺失 unaffected body、完整验证或 stale History revision 均在移动栈前失败关闭；禁止 JSON 恢复、command replay 或第二套 delta author model。作者模型内的 COW 分配由跨 Undo/Redo 的单一 retained-allocation index 去重；no-op/compaction 在 mutable access 前返回，多轨操作只 detach touched Track/collection。默认 200 条/256 MiB 是版本化 conservative logical footprint，不是 allocator、transient 或 RSS。schema 10 的 protocol-v1 正式矩阵须以 6 reports + completion 证明上述作用域、局部性、连续 200 条深度、120 分钟 precompose Undo/Redo、正常路径无 oversize/barrier，以及专用未保留 probe 的正确屏障分类；Windows Private Commit 与 History charge 保持独立门禁。
- 命令测试覆盖 execute → undo → redo、失败回滚、锁定轨道、跨序列引用和保存/重开后的最终语义。

### 3.3 播放引擎与媒体任务调度

- 播放引擎是深模块，拥有播放状态、音频主时钟、当前帧选择、预读、late-frame 策略、seek generation 和可观测性；UI 只提交意图并消费状态。
- 保留 `PlaybackCursor`、`ScrubCursor`、`RandomAccessStillFrame` 三种媒体访问语义，不为新调用方增加绕过它们的 FFmpeg helper。
- 跨域只共享类型化 intent、generation/cancellation、deadline 与 terminal evidence，不共享一个全域任务全序。产品级粗粒度 admission 只有三档：第一档始终是 realtime Preview/Audio；第二档是显式用户 Import、Existing-Asset mutation、Export 与 Proxy；第三档是自动 derived-media/recovery。实时播放关闭新的重任务域派发；离开实时后，显式意图先于自动工作。每个深 Module 继续拥有自己的内部顺序，例如当前帧、seek、预读、scrub 或队列重试，资源协调 Module 不得重新解释这些域内语义。

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

- Sequence 拥有采样率和 channel layout；Project 只提供新建 Sequence 的默认值。所有内部混音使用 float，输入统一重采样后进入图。
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
- Windows、macOS、Linux 与 Noop/headless 都必须实现同一小型 Platform Capability Interface，不复制业务逻辑。D3D12、Vulkan、Metal、VideoToolbox、VA-API 等具体类型和同步原语只能留在对应 Adapter；共享 Renderer/Media Interface 只携带类型化能力、资源身份、同步义务和执行证据。
- Unsupported、degraded、ready 都是结构化能力状态。某个平台尚无正确的原生低拷贝或 HDR 路径时可以回到语义正确的 CPU/upload/SDR 路径或明确阻止，但不能假装成功、静默使用错误格式，或把缺失 Adapter 当作长期平台策略。
- 项目文件、时间线语义、效果参数和 golden frame 期望不因平台分叉；允许后端性能不同，不允许结果语义未经说明地不同。
- M1/M2 的跨平台工程门禁至少包括三平台产品入口编译、共享 CPU reference、平台能力合同、无平台类型泄漏和 Adapter failure/degrade 测试；Windows 另承担当前真实 GPU、硬解、音频、显示和长时运行资格。缺少 Linux/macOS 实机证据必须明确记录为资格缺口，不能反向降低其生产实现要求。

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

- Windows/D3D12、macOS/Metal、Linux/Vulkan 的 SDR Viewer 必须消费同一色彩/Alpha/合成语义；HDR/Log 到 SDR 使用明确 view/tone-map transform。M1/M2 可只要求 Windows 实机测量，但 Metal/Vulkan 不能以长期空 Adapter、错误像素或 Windows 行为模拟代替生产实现。
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

当前机器可读合同是 `windows-alpha-golden-v12`、封闭 schema v4；未知字段、未执行义务和缺失执行前提均失败关闭。Hero Sequence 固定为 7,500 帧、3840×2160、25 fps、square pixel、progressive、linear Rec.2020 working、Rec.709 legal Program Output、10-bit 交付默认值与 48 kHz stereo；H.264/AAC 和 HEVC Main10 使用稳定内建 preset ID 与完整 signal/probe 合同。

七个切片必须通过普通产品接口进入同一 Hero，并各自提交不可互相替代的证据：

1. **PCM/Clip 音频：** 稳定 Component/Edit 身份、gain/pan/fade、Undo/Redo、保存重开和 Program PCM。
2. **Editorial/Transport：** 定向 Split/Insert 的类型化 receipt、Lift 不移时、Extract 按 T/S 与 Sync-Lock 闭合半开范围，并分别单步 Undo/Redo；scrub/settled seek/play 必须消费生产 Preview presentation，fallback 如实分类。
3. **Proxy/Relink/Constant Retime：** 真实 H.264 在 Proxy→Original→Proxy、离线、Relink 后保持强作者锚点；`1/2` source map 必须令 Preview、Export、重导入命中正确源时间，并以错误 `1/1` 反事实帧防止 metadata-only 通过。
4. **Delivery：** 非零 Work Area 经产品 Trim/Transform/Opacity 和 durable reopen 后，分别完成 H.264/AAC 与 HEVC Main10 导出、probe、像素/PCM/AAC 回读和普通重导入。
5. **Visual authoring：** Basic Title、Cross Dissolve、Primary Color 与显式域 LUT 经过产品编辑、Undo/Redo、保存重开及 Preview/Export 同义检查。
6. **Recovery/Nesting：** Autosave Recovery、单事务 Precompose、强引用 child、手动保存退役恢复点及再次重开；Hero 始终是 primary。
7. **HLG/Alpha：** 真实 HLG Main10 与 sRGB straight-Alpha 文件保持 source extent/zero-rate hold、原片色彩 reference、Program Output、导出与重导入证据。

`GoldenAcceptancePlan` 同时编译全局义务账本和 Hero 绑定账本。每个切片必须声明稳定 role，全部义务都绑定 `hero`，运行时所有 stage 报告同一 primary `SequenceId`，最终只允许 Hero 与其强引用 nested child；逐阶段作者锚点必须允许合法增量而拒绝旧内容漂移。结构计划、单切片、退出码或多个隔离 Sequence 的并集都不能发布 `complete_golden_project: true`。

每个最终候选构建必须由官方监督器连续运行 3 次；三轮使用独立 run/Project identity，不得人工修复或清缓存，并都满足 schema v4、同一 Hero、全部七切片、最终两条 Sequence、切片/export 引用和 `complete_golden_project: true`。任一轮失败即整项未通过。当前工作树仍须重新取得这三轮证据；该门禁也不替代独立 HLG/PQ/Log 数值 reference、合格发布机、30 分钟 CPAL/GPU/内存/A/V 或故障注入。

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
| 连续播放 30 分钟 | 真实 CPAL callback（声学链路另以 loopback 验证）；无 underrun/recovery 或 render-to-silence；A/V drift 绝对值 ≤ 20 ms；当前视频 Ready ≥ 99.5%；分钟 5–10 与 25–30 的完整产品进程树（App + demux/FFmpeg 等 descendants）内存平均增长 ≤ 256 MiB、全程 ≤ 4 GiB，terminal stress 静止后仍收敛。当前 Windows profile 明确使用 Private Commit；其他平台必须建立绑定其 typed metric 的独立基线，不能直接套用或改名。仅主进程、子进程遗漏、metric 不符或 inventory/query 不完整均失败关闭 |
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

- [x] 建立 document/library version registry，并提供 current document v24/library v5 fixture、幂等打开、事务回滚和失败不覆盖测试；library v4→v5 仅事务重写 typed native-audio layout 字段，普通标签不受影响。Alpha 对其他 document schema 与未知作者字段失败关闭。当前语义包含有序 proxy membership、封闭 `ClipContent`、多成员 Link Group、强端点 Transition、Mask/Basic Title 作者状态、Project 色彩环境、Sequence `color`/`delivery`、Clip-local 视觉时间和唯一 `ClipSourceTimeMap`；终端源边界只由 placement duration 与映射推导，不再持久化可漂移的平行字段。
- [x] Timeline placement 继续作为单一事实来源：ClipContent 封闭 variant 删除平行 kind payload，Clip Link Group 使用至少两成员的集合语义；复制/razor/overwrite fragment/precompose/Sequence duplicate 会 fork 完整 Clip/Track/Effect/Mask/动画/音频身份并重映射内部强引用。视频 Transition 已冻结 Sequence-owned 强端点、唯一 edit pair、精确范围和 unclamped 双源 handle demand；这只完成作者/Adapter 地基，不勾选 Cross Dissolve 执行。
- [x] `AuthoringSession` 已成为唯一可变 Project 权威，统一拥有文档、素材库、导航、project-wide Undo/Redo、`AuthoringSessionId`、`AuthorGeneration` 与 manual/autosave baseline。Project document save revision 与持久化 `SequenceRevision` 分离；候选事务只有在完整校验、历史记录和 revision/generation 前进都成功后才原子安装，失败不改变文档/代次/历史。生产代码不再暴露“先修改再补记”的 Sequence 可变入口。
- [x] History 以 200 条与 256 MiB versioned logical charge 双限额保留 Sequence 或作用域化 Project typed-state endpoint；完整 detached `ProjectDocument` candidate 仍是唯一事务输入。Undo/Redo 在移动栈前验证 current source/scope，复用 unaffected Sequence，并保留当前 document revision、更新时间与有效导航。retained-allocation index 去重 COW 作者分配，no-op 不 detach，多轨编辑只 detach touched collection；任何未保留提交建立连续性屏障。logical charge 不含 transient、allocator 或 RSS，选择/导航也不是作者事务。
- [x] 保存/另存/Autosave 共用 UI 无关 worker：不可变作者/SQLite snapshot 绑定 Session、Persistence Generation、Author Generation、destination 与 lease，SQLite 使用 online backup；retained sibling 完成 flush/验证后只经 `mondrian-storage` 发布。`CreateNew`/`ReplaceExisting` 在准入冻结，只有 durable evidence 推进 baseline 或清理 Recovery Authority；不确定 binding 被 poison。Session handoff 先关闭准入并等待 FIFO barrier，Library 使用不可变 generation directory 并在最后引用释放后清理；旧 Session/generation/destination 无权发布。Window close/quit 已改为非阻塞生命周期状态机：保存请求与 barrier 保持 worker FIFO，UI 不等待，quiescing 期间作者 Action/Project completion 冻结；Headless 同步入口复用相同 ticket 完成协议。
- [x] 精确 `TimelineTime`、显式 Time Domain/Transform 与 Frame/Sample Evaluation Grid 已落地。Current document v24 只持久化一个显示设置，Frames/SMPTE、origin、负时间与标签回绕都不改变作者时间。`FramePosition` 仅是一个显式 Evaluation/Display Grid 上的坐标：执行请求若声明自己属于目标 Sequence 网格就必须严格匹配；Action 等允许跨输入网格的语义 Adapter 必须保留 `time_base`，先转为精确 `TimelineTime`，再用显式舍入在 Sequence 网格量化一次。Move/Trim/Seek 产品动作现均携带完整 `FramePosition`，由 Timeline Gesture 或 Playback Adapter 单次降低；Move 只携带稳定 Track/Clip ID 并从作者状态派生媒体类型。Clip-local visual time 与 source-local `ClipSourceTimeMap` 分离；常速 map 以精确 `source_origin + local × scale` 表达 forward/reverse/hold，终端边界由 duration 推导。完整 `SourceSampleTarget` 把精确源时间与 `Covering`/`StrictPredecessor` 半开边界一起贯穿 Timeline、Prepared Visual/Audio Schedule、Preview、Export、嵌套与 cache key，并只在声明的媒体、音频或子 Sequence 网格消费一次；浮点秒、微秒 key、nearest frame 与 epsilon reverse 均不是执行语义。跨网格 Action、VFR、混合帧率嵌套、负时间、正反向变速与 reverse hold fixture 固定该契约。

**播放与任务**

- [x] `PlaybackEngine`、`FrameWorkBroker`、`PreviewFrameStore`、Monotonic Runtime Clock、Cancellation Evidence 与 Window/Headless Preview Runtime 已从 UI 收敛。Broker lease 是 worker 驻留权威；media source/task、Timeline materialization、Viewer lowering、CPU execution、presentation、hardware admission 和 diagnostics 各有唯一深 Module。Renderer 拥有递归闭包与画面数学，Window 只注册最终输出并投影状态；计划帧不能声称已执行，只有 renderer/GPU completion evidence 可发布 executed residency。
- [x] Preview 输出使用稳定 typed unavailability：`NoContent`、正确性/依赖 `Blocked`、已准入执行 `Failed` 在 Timeline、素材、色彩、合成、Program Output 和 presentation 间保真传播。空 Timeline/嵌套是透明 Ready；没有 root/output 才是 `NoContent`。终态不可用撤销 stale 资格，只有同一 Sequence/画幅/显示契约的 `Pending` 可暂用旧帧；不能从字符串或 `Option` 猜失败类型。
- [x] 帧工作统一 generation、priority、deadline、取消 disposition、请求龄期、资源预算与 terminal evidence；deadline 只在 Adapter 边界降低一次，同键 rebind 不续期，worker 发布前记录完成时刻，时钟回退会钳制并令专业门禁失败。Waveform、Thumbnail、Proxy 与 Export 分别拥有有界队列、缓存/重试、generation cancellation 和终态历史；它们只共享值语义，不共享万能 Job、worker pool 或容量。
- [x] FFmpeg open/stream-info/seek/packet I/O 使用 request-scoped interrupt 和结构化 checkpoint；可能不返回的 seek/read 位于可终止的 per-source isolated helper，终止会 poison paired packet-source/codec Session 并在真实 reap 前保留 active evidence，不能靠 callback poll 或文件存在冒充恢复。`DecodedTemporalExtent` 用 `[start,end)` 表达已证明的 presentation interval，duration 与首个 successor 取更早边界；Session 最多保留 selected+successor 两个候选。Playback/Still 只发布覆盖请求的因果帧，非覆盖选择失败关闭；Scrub 可发布近邻但必须标为 degraded。5 ms checkpoint、50 ms Playback/Interactive return、500 ms Still return 是当前专业门禁，代码存在不等于当前工作树已通过。
- [x] `ProcessMemoryProbe` 已在 Windows Process Status/Tool Help、Linux `/proc`、macOS libproc/rusage 上实现 current-process 与完整 product-tree；物理/系统内存也有三平台 Adapter。结果绑定 scope、Backend、typed private-memory metric、双 inventory、PID/start identity 和 checked aggregate；Windows 专业门禁只接受 Private Commit。固定 cadence、terminal-stress 收敛、主流时长/声明帧数覆盖、原生 frame/fence/session/residency 退休证据及 8/16/32 GiB 资格合同已进入 Video+Audio gate。`viewer_gpu_device_progress` 的 bounded non-UI wait/callback ownership 和 Window device-generation handoff 也已接入。上述为代码能力；当前工作树仍须重跑 16 GiB 完整基线、GPU progress/Window handoff 与 8 GiB 降级矩阵，macOS/Linux 仍需原生 CI 与设备报告。

**帧、参数与音频**

- [x] Frame/Color/Alpha contract 已冻结为统一 `ColorFrameDescriptor`：画幅、图域、外部/working/device 颜色身份、sample encoding、CPU/GPU residency 与 `StraightCoverage`/`PremultipliedCoverage`/`Opaque` alpha association 缺一不可。CPU typed frame、GPU handle/resource table、native NV12/P010 import、OCIO stage、working composite、Viewer spatial、ICC calibration 与 export/readback 均传播或验证该契约；公共 working/color seam 只接受 straight/opaque，premultiplied 只允许作为显式空间滤波内部帧，shader flags 从 descriptor 推导。非法 alpha 在 OCIO、Viewer 与 calibration 规划期以结构化错误失败关闭；legacy RGBA8 composite、GPU output fallback 和 stage blocker 继续由 renderer-owned typed path/reason breakdown 报告，类型存在或 shader 可创建不等于能力可用。
- [x] 视觉与音频 Processor 参数共用稳定 ParameterId、定义默认值/可动画能力、独立实例地址、单位、enum/resource、hard/soft range、Hold/Linear/Bezier 执行语义、cache impact、schema version/message ID；UI 编辑预设只生成 Bezier handles。视觉链已贯通 UI、精确动画、持久化、编译图和 Viewer cache；音频实例持久化 Schema 快照与单一 exact curve，内建编译要求规范 Schema 精确匹配；项目加载与效果注册拒绝非法 schema。颜色域、CPU/GPU、确定性、时间范围与 ROI 仍只由 Processor/Effect Definition 和编译图拥有。
- [x] Track/Clip placement → Component Edit/Scope → Track/Bus/Program Output、Gain/pan/fade/Transition、headless compiler、recursive nested Runtime 和 reference PCM 已进入 `mondrian-audio`；播放/导出共用 decoder Adapter 和执行语义，旧 flat mixer 已删除。
- [x] `AudioDecodedSource` 已改为可失败的精确 interleaved block Interface；播放/导出共用文件指纹与 128 项/256 MiB 加权 LRU 的十秒 PCM 窗口，Runtime 仅保留对齐 4096 帧热窗，不再以整文件 PCM 作为产品执行源。
- [x] source Seam 已从逐样本调用改为每个 Contribution 每块一次的索引化交错读取；semantic IR 已 prepare 为稠密 schedule、连续 Route/Contribution 区间、Transition binding、精确 sample span、预分段 automation span 与 liveness scratch，CPU scalar reference/SIMD kernel 已建立。`dense_schedule_v2` 固定参考机矩阵覆盖 1/8/32/64 Track、0/2/8/16 Bus、显式 Track→Bus→Output Route 与 64/256/1024 帧 block；普通 CI 覆盖三 Bus scalar/SIMD PCM parity。
- [x] Prepared latency solver 已在每个 Contribution→Track 与 port-specific Route→Bus/Output 汇合点计算补偿，累计 pre/post Rack 与 Program Output algorithmic latency；Processor contract 将其与 `None/Finite/Infinite` tail 分离，Rack 有界求和并拒绝溢出。Scope Contribution 的 causal execution span 覆盖 source/rack algorithmic latency 与 tail，源结束后不再调用媒体 Adapter而是送静音冲刷；未来 Clip 的 stateful occurrence 保持 pending，到首个相交样本才懒进入并连续运行。每个 Scope 输入、Rack Processor、Edit、Fader 与 Route 都保存并扣除 input-signal delay 后求值参数；Program Output 将总算法延迟变为有预算的内部 lookahead，fresh epoch 以有界块预热丢弃后直接返回请求的 Timeline PCM，公共 meter/continuity 保持请求坐标。已归一化的嵌套 child output 以零算法延迟进入父层但继续传播 state-entry obligation，不会二次补偿。Session 为每个 Contribution/Route 补偿输入预分配 fixed delay line，prepare 对总字节与 lookahead fail-closed，Session 构造复核容量；fresh continuity epoch、Offline 精确入口、Realtime generation Enter/Continue、失败中毒/有界恢复、独立 nested epoch、正向补算和 stateful reverse fail-closed 已进入产品路径与普通 CI。
- [x] Current document v24 的 Sequence、Render Contract、PCM request/cache/buffer 已统一规范信号布局；library v5 的 fingerprinted Component Catalog 贯通播放、波形与 Export 的绝对 stream selection，原生 probe 只保留 `Exact/Unspecified/Unsupported` 三态且 custom native speaker mask 可进入唯一 canonical speaker set。旧物理绑定若不能由同一次 probe 与 fingerprint 闭合证明就撤销，fresh candidate 原子提交前失败关闭。媒体缓存保持 native-layout PCM；source→Sequence、child→parent 与 Program→delivery 是三个显式布局边界，未知布局拒绝。Prepared schedule 保留全局唯一 Processor Instance ID、Rack 顺序和 owner/insertion origin；共享 Scope 为每个 Contribution 建立独立 Session state，主路由与 send 共用预分段 typed Route 自动化。
- [x] 用户可见 Component 选择与显式重绑已贯通：Inspector 以稳定 Edit ID 在 Asset Component/嵌套公开输出域内选择并进入 Sequence Undo；资产级“重新探测候选→显式重绑”分离自动证据与用户意图，保留逻辑 ID，更新后会在当前 transport anchor 失效并重建音频 Runtime。Canonical authored mix matrix 已完成持久化、验证、稠密调度执行、嵌套布局边界和播放/导出交付分层。Processor Host 已完成 semantic preservation、prepare-time Resolver/Factory、Session 实例、mode/algorithmic-latency/tail/state/scratch、显式 lookahead/Session 字节预算、主 Bus/辅助输入查询和公共参数 batch 分层；内建定义位于独立深 Module，Gain、状态型 Sample Delay 与 linked-channel sample-peak Lookahead Limiter 走同一 Host。Limiter 是首个生产非零 algorithmic-latency Processor，已用真实并行 PDC、独立 scalar reference、Scalar/SIMD、分块、seek、自动化、精确 scratch 拒绝、保存重开，以及 1/8/32/64 轨逐轨实例化的 release 实时负载矩阵证明；它不承担 true-peak 声明。Sample Delay 的可听延迟明确不是可补偿 latency。未来外部插件、完整编辑 UI、设备矩阵、true-peak/响度和可选 GPU backend 必须扩展这些已冻结接缝，不能建立第二套音频图、自动化或补偿语义。

- [x] Component 逻辑来源与其他 Component 字段已共用唯一 `AudioComponentEditRequest`：Inspector 只投影 canonical `AudioComponentSource`，外部动作携带完整稳定 Track/Clip/Edit 地址；Timeline 原子校验锁定、来源类型、重复选择和完整 Audio Program，App Audio Adapter 只证明当前素材目录或子序列公开输出依赖。旧 `ui.inspector` payload/dispatcher 已删除，no-op、stale 地址和依赖失败均不推进 generation/history，成功切换只产生一次可 Undo/Redo 的作者事务。

**验证基础**

- [x] 已建立版本化 Reference Corpus、Golden/Stress 机器合同、Windows 参考机 profile、机器资格和分层门禁。generated fixture 以固定 recipe 加 run-local artifact attestation 识别，不能伪造跨 encoder 位级稳定性或给陈旧产物重新背书。Playback 合同固定 Video+Audio 输入用途、45 分钟监督期限、进度 journal、16 GiB baseline profile 与结构化结果；Golden 使用 `windows-alpha-golden-v12`/schema v4，固定 Hero、七切片 role/export/window obligation 和“未执行不得通过”。
- [x] Golden v12 由单一深 Module 编译全局义务与 Hero 绑定两份账本，并校验 schema v4、Sequence role、Hero 身份、三轮规则和 slice/export 引用。Planner 即使结构完整也必须保持 `complete_golden_project: false`；只有监督执行观察到全部义务才可完成。删除义务、把 role 改为 diagnostic、单切片成功或隔离 Sequence 并集都必须重新产生精确 blocker。
- [x] Golden 单 Project Coordinator/监督器机制已进入专用进程生命周期：真实 App composition 锁定 Hero ID、设置和色彩环境；七阶段只复用该 Hero，Recovery 只可新增一个被强引用 child。open、Autosave Recovery、durable reopen 和逐阶段作者锚点均拒绝身份/既有内容漂移；Headless presentation 与非零 Work Area 导出必须证明结果来自同一节目。机制完成不等于当前候选通过；当前工作树仍须按第 5.2 节重新连续三轮运行。
- [x] 将 capability probe、逐帧 decode provenance、最终 Viewer GPU completion 与 fallback/blocker 写入同一结构化报告，同时保持 media/renderer/playback 的诊断所有权；预取 aggregate 不得代替已呈现帧证据。
- [x] 路线图、效果规格与色彩规格已统一五级能力口径：作者模型存在、产品可选择、图可执行、具体 backend 可执行、真实 preview/export 已验证。效果库只暴露可构图 definition；共享 Render Plan 对启用但未实现/缺失定义/运行时不可用/资源无效/构图崩溃失败关闭；CPU、GPU、颜色 reference 与产品发布证据分别列示，类型、OCIO 映射、shader 创建或单次 lower 成功均不得自动写成产品支持。

### 退出门槛

- schema v24 current fixture 可保存、重开；Project 色彩环境/新 Sequence 模板、封闭 Sequence `color`/`delivery`、Clip-local 视觉作者时间、单一 Clip 源时间映射、Basic Title/Mask 动画与强编辑关系 round-trip；旧/未来 schema 与未知作者字段明确拒绝；故意失败不会破坏源文件。
- Headless 测试可驱动 play/seek/cancel，并以手动 Monotonic Runtime Clock 精确验证请求年龄、过期边界、最早取消原因、同键 rebind、worker 完成与 UI 轮询解耦以及回退证据，而不构造 Widget 或 native window。
- 一个参数从 schema → UI → animation → save/reopen → preview/export → cache invalidation 全链通过。
- 音频时钟、video target selection 和 fallback 决策可由结构化报告关联到同一次播放。
- corpus、Golden/Stress 项目和参考机都有可复现说明，不是空 README。

## M1 — Alpha（Windows 实机资格）：完成 5 分钟真实项目

**目标：** 一个不理解内部架构的用户能独立完成包含常见 SDR/HDR、音频、标题、转场、基础效果和关键帧的 5 分钟项目，并得到可重复导出。产品实现和 Interface 同时面向 Windows、Linux、macOS；本阶段只要求 Windows 完成真实设备与长时运行资格。

### 项目与素材

- [ ] 以 H.264、HEVC、AAC、PCM/WAV、PNG/JPEG 及损坏/截断/不支持变体为输入完成有界后台导入；probe、fingerprint、stream/layout/color 证据必须原子提交，旧 generation 不发布。不支持或损坏输入返回可操作原因且 UI 仍响应。验收含批量取消、项目切换、保存重开、相同路径替换和资源压力。
- [ ] Bin、重命名、缩略图、离线占位、单个与目录重连可完成 Golden Project。`proxy-relink-v1` 已在 Hero 专属 Track 以 CFR `200..350`、VFR `350..500` 两个真实 H.264 placement 证明 Import/Trim/代理生成；CFR 还证明离线诊断、异步两阶段显式 Relink 的 operation/generation/terminal evidence、`AssetId`/Clip/名称保持、Asset Library revision、reload event、replacement source 解析，以及源路径参与代理身份后旧代理不被误用并重新生成。Recovery 会复核两组 Track/Clip/Asset 强锚点和 proxy intent。目录批量重连、运行中 waveform/thumbnail/decoded cache 组合失效、更多素材/嵌套压力和离线占位交互仍未补齐，因此本项不提前勾选。
- [ ] durable worker、SQLite snapshot、Session/generation completion、dirty/manual/autosave baseline 和深 Recovery Module 已完成；fixture-free Golden 已在 Hero Sequence 的独立 `175..200` 窗口覆盖正式 Precompose、Autosave、关闭原 Session、产品恢复 Action、恢复点保留、当前手动保存退役和再次重开。Hero primary、强引用 child、完整 parent/child 作者快照、Preview 像素与 Export 递归执行均不漂移，且组合协调器会复核此前阶段锚点。Window Save/Discard→Close/Quit 已由非阻塞 pause ticket 闭合，受控慢保存证明开始调用立即返回、后续作者 Action 冻结、精确保存请求耐久后才退役 Session；保存发布失败会保持 Project 并恢复 admission，协议故障须显式 Discard 才退役且迟到 completion 无权发布。仍须补齐恢复来源/时间/目标/冲突 UI、磁盘满/权限失败注入、反复崩溃和 Save As/close 实机压力边界，因此本项不提前勾选。

### 编辑手感的最低闭环

- [ ] 以 Select、Cut、Move、Trim、Ripple、Roll、Slip、Slide、Insert、Overwrite、Delete 和 snapping 的鼠标/键盘路径为输入，统一可发现性、预览与边界反馈；一次手势只提交一次事务，锁轨、非法 handle 或不完整链接组必须在 commit 前显示原因并保持作者状态不变。Move/Trim 已收敛到一个深 `timeline_clip_gesture` Module：显式输入网格、稳定身份、完整 Link Group、所有锁与目标轨道先准备；Move 以精确 `TimelineTime` 增量保留 sample-accurate A/V offset，链接成员越过 Track 集合边界时整项拒绝而不 clamp。Trim 以每组第一个显式成员为锚，将请求转换为精确 edge delta，先与全组合法区间求交再统一应用；不同时长成员由最严格边界限制，既有 sample/J/L edge offset、source/Clip-local/native-audio 时间与单步 Undo 均不塌缩。Selection Trim 保持 primary-first 顺序并复用同一 admission/commit；stale/locked/no-op 不产生部分提交。这里只完成“同步修剪时保留既有 J/L 偏移”，仍须补齐创建或独立改变 J/L 偏移的产品策略、统一交互反馈、snapping/ghost 证据、p95 UI 反馈、保存重开和 Golden 操作录制后才能勾选。
- [ ] 补齐 Lift/Extract、显式 Link/Unlink、Track Targeting；链接片段与锁定/静音/可见状态行为一致。当前实现已有 Sequence-local Link Group、锁轨原子拒绝、稳定身份选择、单次 Undo/Redo，以及显式 content/ripple Track、半开范围、Transition/automation 校验和独立 T/S 控制。Track 产品操作现由唯一 `TrackProductAction` 承载：Add、同类稳定锚点 Move、视频 Visibility、音频 Mute、两类 Lock 走封闭 codec；T/S 明确只改 Session，所有重复值与已满足排序均不创建事务或伪报成功，旧索引/媒体布尔协议已删除。正式 Golden v12 已在同一 Hero 证明 Lift 不移时、Extract 按 Sync-Lock 闭合时间及各自单步 Undo/Redo。此项仍未完成，因为还要求 T/S 工作区持久化、复杂 J/L linked-edge、明确失败提示、保存重开与更完整的产品操作录制证据。
- [ ] 实现可交付的 speed、reverse、freeze frame。Current document v24 的封闭 `ClipSourceTimeMap` 已承载 signed constant rate 与视频 hold；变号以旧 exclusive terminal boundary 为新 origin，负向采样使用 exact strict predecessor，hold 保留捕获画面的边界。类型化 `set_rate`/`hold_frame` Action、Inspector、完整链接组、锁轨、单步 Undo/Redo、source extent/Transition handle preflight、Prepared Visual/Audio Schedule、Preview/Export decode identity 与嵌套降低均消费同一规范映射；零速率不能冒充 rate，音频不能执行伪 freeze，stateful nested audio reverse 在没有连续性证明时失败关闭。Fixture-independent Golden 已连续执行 linked `+3/2 → -3/2 → reverse hold`，对完整 `SourceSampleTarget` 而非裸时间比较 Preview/Export，并把稠密音频 Schedule 真正渲染到 48 kHz 网格，证明 strict predecessor 取前一物理 sample；Undo/Redo 和 durable reopen 保留方向与 hold 边界。真实 Golden v12 又让 CFR 与合法 20/60 ms VFR H.264 分别执行 `1/2 → -1/2 → reverse hold`：Preview/Export 保留完整 target，代理 Headless 呈现 reverse/hold，原片快照分别导出并重导入，解码证据保留 requested/selected/duration PTS；VFR strict predecessor 的 60 ms 区间必须紧邻 20 ms covering 区间，成片同时远离 adjacent-covering 与 wrong-direction 反事实。当前三轮完整 Golden、混合帧率/真实手机 VFR、音频变速策略和长时门禁仍待关闭，因此本项不提前勾选。Variable remap 只能增加经过验证、可保存重开且 Preview/Export/音频一致的分段变体，不能复用普通参数自动化或派生第二套 source range。
- [ ] 视频 Transition 作者模型、共享 Preview/Export CPU 执行、产品命令、基础时间线 UI 与 GPU lowering 已闭合：强端点、同轨非重叠、unclamped 双源 demand、媒体/嵌套范围准入、默认拒绝、显式缩短与单次 Undo 均有测试；App 选择只保存稳定 Transition ID；精确相邻且未锁定的视频 cut 可创建约 1 秒居中 Cross Dissolve，Overlay 优先于 Clip 命中，可选择、普通 Delete、拖动两侧范围并显示当前 handle 失败诊断，Ripple Delete 不误用于转场。ID 与视图投影成对产生，预览拖动不修改作者模型，释放时只提交一个 `VideoTransitionProductAction`；Resize 在 Timeline Adapter 处从显式 Sequence 显示网格一次降低为精确 `TimelineTimeRange`，App 不再从裸 frame payload 重建作者时间，重复范围也不产生 History。Create/SetRange/Remove 在事务前从当前强端点重新解析 Track/lock/source extent，旧 Timeline 字符串解释已删除。Viewer GPU 执行图以普通 Source 为唯一输入准备结构，Cross Dissolve 两端各自完成颜色、变换、透明度与效果后，由专用 working-linear pass 做 coverage-correct 插值；真实 wgpu readback 已逐通道对照 Export/CPU 共用公式，并有独立执行证据。`visual-authoring-roundtrip-v1` 已通过产品创建、单事务、Undo/Redo、保存重开以及 Headless Preview/Export 系数一致性，现与 Foundation Audio 共存于同一 Hero Sequence；但仍只使用无限 handle 的生成源。仍须用真实媒体/嵌套端点、代理/原片和独立视觉 reference 完成扩展验收。绝不读取片段外错误帧或隐式重复边界帧。

### 播放、缓存与代理

- [ ] 三平台生产路径已经能够把 D3D11/D3D12、VideoToolbox/CVPixelBuffer、VA-API/DRM PRIME 输入同一 GPU YUV→working 执行链；下一门槛以 4K23.976–60 HEVC Main10 Long-GOP 为输入，在支持设备上观察逐帧 hardware→native surface→GPU YUV/working 证据。Windows 完成 M1 发布资格；macOS/Linux 先通过原生 CI，实机资格可晚于 Windows。缺设备、格式、可信 DRM layout、同步或预算时必须自动选择 CPU transfer、低分辨率或代理并显示原因，不能错误读回、改色或伪报 zero-copy。验收绑定 OS/GPU/driver/codec、Ready/Stale/fallback、内存和 30 分钟 cadence。
- [ ] seek/scrub 必须 latest-wins：新意图原子取代旧 generation，旧请求在固定 checkpoint/return 预算内结束且永不发布；settled seek 仍可重新提交 exact Still。验收覆盖跨区 100 seek、连续拖动、暂停/项目切换、helper poison/reap、迟到 GPU callback 和最终目标帧身份。
- [ ] Playback/Interactive/Still 的共享门禁必须分别满足 5 ms cancellation checkpoint、50 ms Playback/Interactive return 和 500 ms Still return，并拒绝未知原因、缺失 request/checkpoint、非法时序或 Broker 时钟回退。输入必须覆盖 canonical Long-GOP Main10、真实阻塞网络/可移动介质、generation supersession、helper poison/reap 和 8 GiB 降级；Playback/Still 的非覆盖 `DecodedTemporalExtent` 必须失败关闭，只有 Scrub 可携带 degraded 近邻。当前工作树及更多驱动的最坏延迟矩阵待重跑。
- [ ] CPU decoded cache、GPU/working cache 与 proxy index 必须分别有字节预算、LRU/eviction、source revision 和颜色解释 key。`proxy-relink-v1` 要求同一 Hero 在 Proxy→Original→Proxy 与 Relink 后把旧路径判为 Missing、发布新代理；50% retime 的 Preview 消费代理而不可变 Export 消费重连原片，二者仍命中同一完整源采样目标，不能只比较裸时间。完成证据还须覆盖多素材/嵌套、cache eviction、完整产品进程树 typed private-memory metric 收敛、8 GiB 最低档及多 GPU/驱动；当前 Windows profile 使用 Private Commit，其他平台不得冒充同一基线。当前工作树长时基线未确认。
- [ ] 播放开始后健康音频设备保持主时钟；以慢首帧、视频持续迟到、seek、设备丢失/重连为输入，视频只能 drop/repeat/降质，设备不可用时连续转 Synthetic Clock，不能转 Video Master 或反复静音等待。验收要求 30 分钟 drift/underrun/recovery 证据、设备 generation 关联和声学 loopback。

### 音频最低闭环

- [ ] Clip gain、pan、fade in/out 已贯通稳定 Edit ID 的产品 UI、单字段类型化命令、完整作者校验、细粒度 Undo、保存重开和 scalar/SIMD 公共执行；0 fade 规范为 `None`，非法时长原子拒绝。规范 PCM 的 Headless Golden foundation 切片现已真实执行 -6 dB、+0.25 pan、双侧 1 秒 equal-power fade、四次 Undo/Redo、耐久保存重开和稳定 ID 后置条件；该音频 Track、Clip、Component Edit 与 Audio Program 还会在同一 Hero Sequence 的 Visual 作者阶段前后作完整投影比较。完整 Rack/Automation 编辑 UI、更多布局和真实插件路径仍未闭合，因此本项保持未完成。
- [ ] 以 Track mute、transient solo audition、master meter、基础 limiter、缩放、Proxy/Relink 为输入，保持作者 mute 与临时 solo 分离、meter 不改变信号、limiter 延迟进入 PDC、waveform/cache 绑定 source revision；未知布局或超预算显式阻止。当前 root Playback solo 已是按 Authoring Session/Sequence 绑定且失败回滚的瞬态 overlay，嵌套输出不继承父 Track 身份；Track/Bus/Program Output meter 在完整成功块后一次无锁发布，失败块、hidden priming 与未执行 target 不产生虚假读数，Mixer 仅投影真实 evidence。仍须完成同一 Golden 中的播放/导出 PCM、meter/clip evidence、Undo/保存重开、Proxy/Relink 缓存失效和真实设备长时验证，故本项不提前勾选。
- [x] 规范 Component matrix 已有可审阅、可 Undo 的产品编辑闭环：`AudioComponentEditRequest` 以稳定 Track/Clip/Edit 地址原子替换 `Standard | Explicit` 策略；Inspector 只用当前精确 Asset binding/probe 或子序列公开输出解析源布局，显示源/Sequence 目标和全部非零系数；标准策略可先审阅再显式快照，自定义可从空白矩阵建立，单系数提交会重建完整规范稀疏矩阵。源证据缺失、不支持或与既有显式矩阵不符时保留作者意图并明确 fail-closed，不按通道数猜 speaker 语义。
- [ ] 完成 custom media layout probe、Sequence→设备布局协商和非标准设备/编码拒绝的产品证据。当前 library v5 已让 FFmpeg native speaker mask 精确进入 canonical layout，未知顺序/位置保持 Unsupported；Playback/Export 已共用 `AudioProgramDeliveryRuntime`，其 evidence 含 Program/target layout、Identity/Standard/ProvenSilence 和 coefficient count；Export preset admission、FFmpeg 命令与完成 probe 共用一份编码布局能力表，MP3 非 Mono/Stereo 及无显式 lowering 的 custom/Discrete 在入队前拒绝。CPAL 默认输出现在逐次枚举真实 `supported_output_configs`，只选择精确 rate/channel 候选并确定性选择全部已知 scalar format；成功 evidence 保留 host/device/name-query、semantic layout、buffer range 与候选收敛计数，失败保留稳定拒绝码/request/selected contract/backend detail，App 只有在当前 stream contract 与 delivery target 相等时才组合完整监控路径。CPAL 仅给通道数，因此目前只承认版本化 Mono/Stereo 约定与 ordinal Discrete；named 5.1/7.1/custom speaker 即使数量相同也拒绝。仍须完成非默认设备选择、默认设备切换检测、按平台证明 speaker 位置/顺序的多声道 Adapter，以及以新 generation 无阻塞重协商的产品交互；最终诊断还须串联源 Component 布局与失败阶段，不能把固定 Stereo 产品策略冒充完整硬件能力发现。
- [ ] 为 Clip/Track/Bus/Program Output 的 Rack、参数自动化和 meter 提供同一套稳定 Scope/Processor/ParameterId 驱动的编辑 UI/命令。共享作者地基已使用 `AudioProcessorRackEditRequest` 封闭 Rack 地址与静态/拓扑编辑代数：Scope 或类型化 Channel Strip/Pre/Post 地址、稳定相对 placement、Processor/Parameter ID、完整 Audio Program 原子校验、单次事务、无操作零提交及 Undo/Redo 均有跨 Timeline/App 证据。所有 keyed 音频编辑现从 Rack/Channel Strip/Routing 枚举移出，统一由 `AudioAutomationTarget + AudioAutomationEditRequest` 覆盖 Component volume/pan、Scope input gain、Channel fader、Route gain 与 Processor parameter；`inspect_audio_automation` 在一个只读 Interface 中返回稳定地址、Sequence/Component/Scope `AuthoringTimeDomain`、schema 范围、曲线/静态权威及 Track/共享 Scope 锁 blocker。App 的单一 `audio_automation` Adapter 只把显式精确 viewport 映射到 normalized CurveEditor，虚拟边界不可编辑、真实点保持 `KeyframeId`，Move 保留 interpolation/Bezier handles，一次手势至多提交一次事务；删除末键对 optional curve 规范回静态值，Processor intrinsic curve 保持空曲线默认值。Mixer 已对 Track/Bus/Output fader 与 Route/Send level 提供曲线；Inspector 对 Component volume/pan 提供曲线；共享 Rack 对 Scope input 和全部可自动化 Processor 参数提供曲线。静态控件在 key 权威时禁用，不把 fallback 冒充播放头值。Track/Bus/Output 的 post-mute meter 与 Track solo 现共用同一 Mixer projection；solo 是 App Session transient Action，不进入作者事务，meter 则读取 exact prepared Runtime 的完整原子 observation bank。Bus/Route 生命周期、cycle/lock 准入、稳定 ID 重连、事务 Bus rename 及 Rack 插入/排序/旁路/删除仍由各自深 Module 承担且不复制曲线写路径。首个 lookahead limiter 已通过 state、entry/deadline/PDC、播放/导出一致性、callback 实时安全和多轨负载矩阵。仍缺插值/handle/copy/reset 完整自动化交互 Golden、外部插件 Adapter、标准响度/true-peak 与真实设备长时证据，因此本项不提前勾选。
- [x] 输入重采样和标准 channel mapping 已有明确策略：mono/stereo/5.1(side) 全组合及未标记 1/2 声道离散默认使用固定矩阵，LFE 不进入标准 downmix；未标记 3+ 声道、5.1(back)、7.1 与其他布局明确拒绝，不调用 FFmpeg 隐式猜测。
- [ ] generation-owned 单调 `ExecutionCancellationToken` 已贯穿 Audio Playback→Timeline Runtime/嵌套→Decoded Source→媒体窗口，reprime/seek/recovery/shutdown 会先取消旧 token，取消结果不进入成功缓存或 failure memory，执行中 render/source 测试要求 50 ms 内观察。媒体 Adapter 已改为最多 8 个按完整源指纹/输出契约寻址的持久 FFmpeg 子进程 Session：顺序窗口复用连续输出、随机 miss 只重启对应 Session并以最多十秒 coarse preroll + output exact trim 保持顺序解码样本坐标、stdout 有界预读、stderr 有界保留但持续排空、等待输出每 5 ms 观察取消并 kill/wait/join；PCM 缓存按完整窗口键 single-flight。仍须在固定参考机同时证明 compressed-audio 冷启动、连续边界、随机重启、取消返回、跨源 LRU 与 256 MiB 缓存预算。
- [x] 加速 Headless 30 分钟时钟数学门禁已实现：它要求精确最终位置、Audio→Synthetic→Audio 连续切换、亚帧 Clock phase 的点误差与不确定度上界、零 underrun recovery，且固定容量 Evidence 不得因淘汰丢失全程最大值；该确定性门禁不替代真实 CPAL/GPU 墙钟运行。
- [ ] `cpal_av_48khz_30min_v2` 的真实墙钟门禁要求主音频流自身覆盖观察区间，不能用容器时长或 EOF 补零；必须先稳定观察一个具体 48 kHz stereo CPAL generation，再精确请求一次 validation-only 真实 stream recycle，证明真实流销毁后的 Audio→Synthetic 切换、新 generation 的 hidden-preroll/exact-trim、带不确定度的 Synthetic→Audio 相位交接，以及最终 generation 连续 30 分钟的 callback consumption。同期必须有 Headless GPU presentation、Ready ≥99.5%、恢复稳定后零额外 loss/underrun/recovery/静默替代、可证明的 A/V phase error ≤20 ms、Session resident/peak 有界且实际顺序复用、十秒稳态窗口 ≤460 ms、源缓存 ≤256 MiB 与完整产品进程树内存收敛（包含持久 FFmpeg 音频子进程）。冷启动/随机重启须单列证据；多设备/驱动与声学 loopback 是独立验证矩阵，不能用假 Adapter 或事件注入替代此门禁。当前工作树真实墙钟门禁尚待重跑。

### 效果、动画、标题与转场

- [ ] 先交付少而完整的算子：Transform/Crop、Opacity/Blend、Primary Color、LUT、Gaussian Blur、Sharpen、基础 Mask、Cross Dissolve、Basic Title。
- [x] Basic Title 已作为 current document v24 的封闭 `ClipContent` 贯通持久化/校验、菜单创建、Inspector/Undo、Clip-local 视觉自动化、共享 Render Plan、working-linear straight-alpha、效果/Transform/Mask/合成、Cross Dissolve 端点、嵌套、Preview 与 Export。字体 bytes/face 参与输出身份，缺失字体或未声明 fallback 失败关闭；当前 roundtrip 覆盖产品编辑、插值、单事务 Undo/Redo、保存重开和 Preview/Export 同义，但不替代完整 Golden、独立排版 reference、缺字/长文本、多语言和压力验收。
- [x] Primary Color 与 LUT 的作者/执行地基已闭合：Primary 使用 Sequence working space、对应亮度系数和 0.18 scene-linear contrast pivot，CPU/GPU Render Op 与缓存身份一致；LUT 强制显式处理色彩空间，严格校验单一 3D `.cube`、`DOMAIN_MIN/MAX`、有限完整 payload，以四面体插值执行并用全文件 SHA-256 失效。Visual report schema v8 已通过产品添加/设参、单事务、Preview/Export 图签名和像素一致、保存重开身份/参数/内容哈希一致，并在 Hero Sequence 保留稳定类型化实体身份。该勾选只表示这两个算子的当前地基和生成回归切片，不表示 GPU LUT、独立色彩 reference、真实 Log/HDR LUT 或全部首批算子完成。
- [ ] 每个算子通过通用 DoD；不以 effect enum、属性面板或未连接的 `TextLayer`/`Transition` 类型作为完成。
- [ ] Hold、Linear、Bezier/Ease、关键帧增删移动复制、reset 和基础 curve editor 可完成 Golden Project。当前视觉 Golden 切片已证明 Hold/Linear/Bezier 两关键帧的正式作者接口、求值、单步 Undo/Redo、schema 保存重开和 Preview/Export 同义；曲线产品路径现按稳定 `AnimationTrackId + ParameterId + KeyframeId` 发出细粒度 Insert/Move/Delete，移动原子保留非 Linear 插值/handles/flags，一次拖动只提交一次事务，虚拟 Clip 边界与真实首尾关键帧也已分离。仍未证明 Ease/handle 编辑、复制/粘贴/reset、跨参数/多通道完整产品 UI 与完整 Golden 操作，因此本项保持未完成。
- [ ] CPU fallback 不隐式 RGBA8，不在一帧内反复 GPU→CPU→GPU；fallback 原因在 Viewer/Export report 一致。

### 导出与颜色

- [ ] 产品 UI 已完成 H.264/AAC SDR MP4 与 HEVC Main10 的稳定合法预设、类型化 profile/位深/range/chroma/Alpha 合同、统一兼容性阻塞、队列、取消和失败原因；Golden v12 的两条交付合同已绑定稳定内建 preset ID，普通 CI 会逐项对照真实 `resolve_export_delivery` 与共享 `expected_export_video_signal`，防止 8/10-bit、profile、chroma、pixel format、range、Rec.709 CICP、静态 HDR metadata 缺席策略、Alpha、分辨率和 AAC 码率漂移。生产队列现在以 `Required/Forbidden` 类型化流合同在发布 `Completed` 前精确 probe mux/major brand、codec/profile、位深、rational fps、画幅、pixel format、CICP/range、静态 HDR metadata 精确值或缺席、音频 codec/sample rate/channel layout、stream-local PTS/duration/time-base 与时长；不可变导出依赖冻结源画幅，媒体/嵌套/Transition/生成层统一把作者 Transform 投影到实际交付采样画幅，并将目标尺寸纳入 decode cache identity；25 帧 generated-delivery slice 现已在 Hero 的 `150..175` Work Area 上、经过 durable reopen 后真实完成 H.264/HEVC 导出和普通媒体重导入，独立 Transform/Opacity 采样、Program reference/解码成片像素、生产 Program PCM/AAC 回读及两编码一致性均失败关闭；Proxy/Relink 又分别以 CFR `280..281` 与 VFR `430..431` 证明 reverse-hold H.264/AAC 成片来自原片严格前驱区间，不是 adjacent covering、错误方向或代理像素；Color Media slice 又以 4K 作者→1080p 交付验证构图没有重复缩放。当前全部已实现 preset 参数已进入同一可编辑草稿和产品表单：容器、codec/profile、跟随序列或显式画幅、位深/range、chroma、Alpha、CRF/完整 VBV、GIF palette/dither、禁用/AAC/MP3/PCM 及其参数；选择内建 preset 会精确重置，非法中间组合保留供继续编辑但无法构造入队动作。仍需覆盖确认、失败重试、完整独立色彩 reference、长项目与合格发布机证据；不能因当前短窗合同通过而勾选整项。
- [ ] 预览/导出使用同一 timeline/effect/color/alpha 解释；Golden frame 与 report signature 对齐。视觉作者切片已对同一帧证明 Basic Title、Cross Dissolve、Primary+显式域 LUT 在 Preview/Export 及保存重开间不漂移；Recovery/Nesting 切片进一步执行同一父子 Sequence 的递归 Preview 与真实 Export 帧渲染，并在恢复/手动保存重开后比较像素与执行诊断。Color Media slice 已让真实 HLG Main10 与 sRGB straight-Alpha 文件进入产品导入、时间线、Preview、Program Output、H.264 导出和普通重导入，当前采样最大通道误差为 3；但 HLG 绝对数值、PQ/Log、代理与真实媒体嵌套色彩边、GPU LUT 仍未全部纳入同一顶层报告。
- [ ] Rec.709、sRGB、HLG、PQ 与列入 Beta floor 的 Log reference 全链验证；当前 sRGB Alpha 已有独立 source→working 数值 oracle，HLG Main10 已有确定性码值、CICP/range、工作空间单调/中性/通道主导性和编码 roundtrip，但尚未以独立绝对 HLG 数值 oracle 关闭完整传递函数，因此本项不勾选。无法解析的 Log 必须阻止并提示 override，不输出“差不多”的颜色。
- [ ] 导出后自动 probe 已进入生产完成态，generated-delivery slice 也已在 Hero Sequence 的非零 Work Area 上真实重导入并校验 MP4、codec/profile、分辨率、rational fps、位深/pixel format、stream-local A/V 起止、48 kHz Stereo AAC、生产 PCM/AAC 回读、primaries/transfer/matrix/range、静态 HDR metadata 缺席和成片像素；仍须在独立真实色彩 reference、长项目和其他已支持容器/音频布局上关闭此项。

### 退出门槛

- Windows、Linux、macOS 的产品入口、平台专属 Adapter 与共享 CPU reference 在各自原生 CI 中编译/测试；各平台图形、媒体、音频、内存和显示 Adapter 的 capability/fallback/blocked 结果均为类型化且不改变项目、时间、色彩或 Alpha 语义。Linux/macOS 缺少真实设备报告不阻止 Windows Alpha，但原生 CI 编译失败、缺少生产 Adapter、错误像素或平台专属类型泄漏均阻止 M1 工程门槛。
- [x] 最终候选的 `windows-alpha-golden-v12`/schema v4 完整工作流按第 5.2 节连续 3 次通过；每轮使用独立 run/Project identity，七个 stage 绑定同一 Hero，最终仅保留 Hero 与一个强引用 child，并观察全部切片、CFR/VFR signed-retime 反事实、物理 PTS 区间、Preview/Export/Headless/重导入证据。正式运行 `20260805T055754Z-complete-golden-a405aa50` 在 `Pr/All` 素材身份检查后连续 `3/3` 通过，三个进程自然退出；aggregate SHA-256 为 `93e15bd69aaefb28a5b94fb7c8234e22c80c7158ea540516aed1556974c82db5`。该证据只关闭版本化完整 Golden 重复门槛，不替代 4K/长时 A/V、内存、真实设备或独立绝对色彩 reference 门禁。
- 达到第 5.4 节 4K 播放、seek、UI 响应、30 分钟同步和导出门槛。
- 所有运行路径被分类为 Verified、Explicitly degraded 或 Blocked/Unresolved。
- 已知 P0 数据损坏、错误色彩、A/V 失步和不可取消卡死为零。

## M2 — Beta（Windows 实机资格）：生产可靠性与完整编辑体验

**目标：** 用户可以把 Mondrian 用于较长真实项目，升级、恢复和交付风险可控，常用编辑不再表现为“播放器加轨道”。

### 要求

- [ ] 以 marker、字幕/标题轨、复杂 Link Group、多选、ripple/delete 和嵌套为输入，冻结传播矩阵；一次意图必须原子更新所有强引用与时间域，锁轨或不完整 scope 整项拒绝。验收覆盖 execute/undo/redo、保存重开、嵌套 roundtrip 与 Golden 可见结果。
- [ ] 以真实 VFR 手机素材、混合 frame rate、DF/NDF timecode、正反速度和 sample-accurate 音频边界为输入；作者时间保持精确有理数，只在显式 frame/sample grid 降低一次，无法证明 cadence/conform 时阻止并提示。验收比较 source PTS、Viewer/Export 帧、音频样本边界和 roundtrip。
- [ ] 以 tag/metadata、素材使用位置和多文件 relink 候选为输入，提供预览与冲突选择；稳定 `AssetId`、强引用和 source revision 不得因检索或换源漂移，冲突/缺文件不能静默挑选。验收覆盖批量提交/回滚、旧 generation 丢弃、缓存失效、保存重开和代理往返。
- [ ] 对每个已发布 Alpha schema 提供只读旧 fixture、事务迁移、current document v24/library v5 重开与再次保存；未知/未来 schema、磁盘满、权限失败或中断必须不覆盖源并保留 Recovery Authority。验收包含幂等升级、失败重试、恢复冲突、备份发现和可观测诊断。
- [ ] 扩展 10–15 个效果算子族：Geometry、Composite、Primary Color、Curves/LUT、Blur/Sharpen、Key/Matte、Mask、Distort、Temporal、Generator、Text、Transition、Utility。每族至少一个算子通过通用 DoD、独立数值/视觉 reference、CPU/GPU 能力声明、Preview/Export parity、缓存失效和保存重开；缺 backend/continuity/资源时保持 blocked，不以 enum 或 preset 数量计完成。
- [ ] 以 Clip/Track/Master bus、峰值/响度 meter、limiter 和基础 EQ/compressor 为输入复用公共 Processor Host；callback 无分配/I/O，PDC/tail/state/automation 与 Export 一致，超预算或设备布局不支持时带原因拒绝。验收包含 scalar/SIMD PCM reference、seek/reentry、嵌套、长时 CPAL、导出回读和实时 deadline。
- [ ] 用户 preset 必须引用稳定 ParameterId、Processor/Effect identity 和 schema version；导入未知/不兼容字段不得静默丢失，须保留 metadata 或给出 loss report/阻止。验收覆盖导出→新项目导入→保存重开→Preview/Export 等价及旧版本 fixture。
- [ ] 在干净 Windows VM 上覆盖安装、离线启动、原位升级、失败回滚、卸载、日志/诊断包与 crash dump 指引；项目与用户数据默认保留，半安装状态不能启动成“成功”。验收提交签名/版本/capability 报告、无网回归和故障注入后的可恢复状态。
- [ ] 建立以稳定 message ID 为身份的 i18n catalog，覆盖中文/英文、变量、复数、pseudo-locale 和文本扩展；缺 key 不得持久化翻译后文本或使关键操作不可达，需显式 fallback/diagnostic。验收在三种 locale 运行 Golden 工作流并检查截断、IME、键盘导航和可操作性。
- [ ] Windows HDR Viewer 只在真实 HDR 素材与 reference patch 经 output texture→UI composite→swapchain→monitor 全链证明 transfer、primaries、nits、Alpha 与 device-loss 恢复后转为 Beta；任一 payload/显示能力不确定时明确 experimental/blocked 或正确映射到 SDR，不能冒充 HDR。证据必须绑定 GPU/driver/monitor capability 与画面测量。

### 退出门槛

- Stress Project 通过连续播放、100 次跨区 seek、proxy 切换、保存/迁移/恢复和长导出。
- 从所有发布过的项目 schema 升级到当前版本均有 fixture 和失败回滚。
- 中文、英文和 pseudo-locale 下 Golden Project 工作流无截断导致的不可操作控件。
- 发布候选门禁全部通过，P0 为零，P1 有明确 owner 和发布决定。

## M3 — 专业能力深化与互操作

**目标：** 在 Beta 闭环稳定后，按真实工作流把少数模块做深；扩展建立在 M0 接缝上，不重写核心项目/时间/帧/参数模型。

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

## M4 — 多平台实机资格、发布工程与长期扩展

**目标：** 在 M1/M2 已共用的 Windows、macOS、Linux 生产实现上补齐非 Windows 真实设备、性能、安装和发布资格，并持续改善效果、性能和交互；M4 不承担“首次让核心架构跨平台”的补课工作。

### macOS/Linux 资格闭合顺序

1. 在真实发行版/硬件上复核既有构建、启动、项目/时间线、CPU reference 与故障诊断。
2. 取得文件/剪贴板/通知/显示 profile/日志/安装 Adapter 的真实桌面环境证据。
3. 取得音频设备、Audio Device Clock 与 Synthetic Clock Master handoff 的长时证据。
4. 取得原生硬解与 GPU residency 证据：VideoToolbox/CVPixelBuffer、VA-API/DMABUF 等。
5. 取得 Vulkan/Metal SDR display/output、device-loss 和 Preview/Export parity 证据。
6. 平台 HDR/EDR 只在完整显示 payload 与真实设备门禁成立后发布。

### 跨平台门槛

- 同一 `.mdp` 在受支持平台往返不改写无关语义；规范 Project/document semantic fingerprint 仅因持久化语义变化而改变，不与基于文件元数据的媒体 revision evidence 混用。
- CPU reference/golden 在容差内一致；GPU 差异有后端专属 evidence，不改写预期颜色来适配某个平台。
- Unsupported native path 自动回到正确 CPU/低拷贝路径或明确阻止；不得产生错误色彩、Alpha 或时间映射。
- 平台实现只新增/替换 adapter，核心时间线、参数、项目迁移和 Viewer/Export 语义不分叉。

---

## 7. 接下来 90 天

这 90 天只有一个主里程碑：通过 M1 Alpha 的完整退出门槛，其中真实设备资格以 Windows 为准，生产实现与常规 CI 仍覆盖 Windows、Linux、macOS。
验证/性能作为并行证据线，小型基础设施只服务于可复现报告；不同时开启
外部插件、RAW、动态 HDR 或 macOS/Linux 实机发布资格。

### 第 1–3 周：封存本轮架构基线

- Prepared Visual Program/Frame Closure/Range Closure、精确 Effect Execution Modes/Demand、受限异构 tracer、缓存复用准入、Preview/Export 动态 preflight、稳定媒体持久化契约、跨执行域资源决策和作者性能工具均已进入唯一生产路径；被替代的 Preview/Export 作者 walker、进程全局 Effect 编译/帧缓存、平行 plugin-contract registry、Core Render Graph 与未接生产的旧 renderer pipeline 已删除。平台侧的 memory/process-tree/scheduling Locality 与 D3D12/Metal/Vulkan 原生视频 Adapter 也已进入生产组合；剩余证据是原生 CI、设备 reference 与 M1 实机门禁，不能从代码存在推断通过。
- 视觉资源准备与动态编译驻留的所有权已经封存：每个 Prepared Visual Program cache 直接拥有独立的 entry+logical-byte 有界 LUT cache，Preview Runtime 与每个 Export attempt 不共享；低频 prepare 全文件读取/哈希后才能命中，scope rotation/clear/reconfigure 同步回收驻留，超限资源可供当前 Program 使用但不留驻。Prepared Program 不可变且 logical charge 覆盖实际 LUT payload、作者/求值器、零时刻拓扑和执行证据；非零动态 topology variant 只进入对应 Preview/Export `EffectExecutionSession` 的 entry+byte grant。所有 logical charge 明确不等于 allocator、GPU memory、Working Set 或 RSS。
- 对全部 ADR、`CONTEXT.md`、架构文档、路线图与 crate manifest 做一致性门禁；当前事实、原生 CI/实机尚未验证的 Implementation、规划能力和发布证据必须分别表述。
- 在最终合并状态执行 workspace format/clippy/test、manifest 依赖方向、Prepared Visual reference parity、媒体迁移/替换竞态和资源策略确定性测试。
- 在最终合并态以同一优化构建和 source attestation 重跑 schema 10/protocol-v1 的 5/30/120 分钟 × active-heavy/project-heavy 作者矩阵，产生连续 **6 reports + 1 completion**，并由 completion 证明 source 未变。六格都必须通过 reference v4、Locality v3、History 与 Windows Private Commit：除操作绝对 p95 外，project median 必须 `<= 2 × active median + 5,000 us` 且 project p95 不超过 reference budget；同时证明两类 History endpoint、scope/source mismatch 失败关闭、unaffected payload 不进入 restore point、no-op/touched-Track COW locality、200 条深度、120 分钟 precompose Undo/Redo 和正常路径无 oversize/barrier。retention-disabled、oversize、barrier 由独立 probe 判定；Windows Private Commit 峰值增量/绝对上限与 logical History charge 分开，Working Set 只作诊断。其他平台必须建立绑定其 typed private-memory metric 的独立基线，不得复用或改名这一 Windows 门禁。若出现主导成本，只能深化现有 Authoring Session/Undo 内部表示，不派生第二套 delta command 模型。

**阶段门槛：** 没有第二套 Timeline/Effect/媒体契约；所有声明为“当前”的能力都能指向生产入口和失败关闭测试；本轮结构变更形成一个可复现的 clean baseline。

### 第 4–6 周：把视觉需求规划接入唯一执行闭环

- 让 temporal frame provider 从 `EffectExecutionDemand` 扩展根与嵌套 Sequence 的精确输入窗口，并定义 seek、重入、source/Clip handle 边界解析、缓存身份和取消语义；通用 planner 保留 signed owner-domain 时间，不得把素材边界策略泛化为时间零点 clamp。
- 已让 CPU-F32 scalar executor 消费同一 demand：精确输入 tile 保留完整画布坐标，有限核消费实现推导 halo，full-frame/unknown 证据不降级。根图、请求、按 `(Clip time, graph value)` 寻址的 time-expanded program 与 Source demand 现被冻结为单一 `PreparedEffectTemporalExecution`；temporal tap 按稳定 Definition-stage input 寻址，跨时间边严格前移 stage，并从同一 Prepared Effect Program 在采样时刻重求值上游动画参数、动态 topology、frame seed 与 Mask。stage contract 漂移、超出声明 temporal/spatial coverage、internal same-stage address、超过 512 contexts/65,536 values 均在像素前失败。expanded schedule/use-count 的纯 dry-run 与执行共同驱动 last-use move、fan-out clone 和 Blend/MultiInput/Mask join；精确重复 graph/source value 只保留到最后 edge。需求批次在物化前给出精确 Float32 source coverage，Preview/Export 先做 CPU grant 准入；Effect Session 再把 retained coverage、每个可达时间上下文的 Prepared Mask geometry/row scratch、resident value、kernel scratch、唯一 final output 和单 tile live-set 纳入同一硬上限。完整请求不能直接容纳时，Session 自动生成确定性、非重叠、最多 4,096 块的二维计划；取消或任何计划/账本失败都不发布半帧或局部缓存，最终 `Vec` 进入 `Arc<Vec<_>>` 不复制整帧像素。Blur/Vignette/Grain/Mask、signed temporal fan-out/join DAG、重复 future sample、坐标种子 Dissolve 和采样时刻参数差异已有 production/reference evidence；Timeline 门禁证明 future offset 经过 Clip retime 只映射一次且 upstream stage 可进入同一生产批次。allocation-free UHD 门禁证明两帧 pixel-local 请求在标准 384 MiB grant 下形成 64 个 `480x270` tile，逻辑峰值 402,278,400 bytes。下一步是不改变该语义参考地补真实 4K 进程内存/吞吐 evidence、unbounded/stateful continuity、internal same-stage temporal semantics、temporal×heterogeneous 与 CPU/GPU parity；不得用错误裁剪像素或隐式整图 fallback 换取性能。
- 以已经接入 Preview/Export UI 无关生产调度的 bounded CPU-F32-DAG→GPU-F32-suffix route 为执行基线：`PreparedHeterogeneousEffectRoute` 已在媒体物化前冻结图、extent、budget、source-closed CPU dispatch DAG、单次 upload 与 GPU dispatch/release schedule，Preview/Export worker 只绑定像素和执行。CPU Implementation 直接消费唯一 graph-value plan 的 materialization input/output，按精确 use-count 对 fan-out clone、last-use move、synthetic MaskSource 与 Blend/MultiInput/Mask join 做 live-set 管理；copy/kernel/Path-BVH/Mask raster 共享 attempt checkpoint，并把优化后的峰值帧数、Mask retained geometry/row scratch 与 kernel scratch 纳入 Effect Session。融合容量内的 GPU 线性 point tail 保持单 pass，超出容量则拆为精确准入的 point dispatch；共享上传值的双 point 分支与 scene-linear Normal Blend join 已按同一 plan 的 token/materialization 执行，wgpu Adapter 另以保守 recording bytes 准入单 command buffer 的实际非别名资源。Gaussian→Basic Correction/Grain、CPU fan-out/Blend→GPU point tail、generated Mask/Mask→GPU point tail 与 GPU fan-out/Normal-Blend join 均有 scalar reference，后者另有真实 GPU parity/资源证据。Preview 根节点持有逐 placement 路线账本，当前嵌套 child 物化继续要求完整 CPU 路线。下一步沿唯一 `CompiledEffectGraph` 深化多次 transfer、GPU Mask/MultiInput/非 Normal join、嵌套 Viewer 物化、更多 placement、颜色域转换、外部 lane executor，以及 temporal×heterogeneous 组合。现有 Preview worker/completion lease 与 Export attempt-local upload/dispatch/wait/readback 不能被第二套 runtime 取代。Linear Stage Placement 不参与执行；缺少 exact mode、已声明 transfer、具体 Adapter 或可证明 live-set 时必须在像素执行前类型化拒绝，异构提交开始后不做隐式整图 CPU fallback。
- 以至少一个需要有限历史的 Processor 和一个扩大 ROI 的 Processor 证明 Preview/Export 共用相同 IR、嵌套闭包和结果；stateful Processor 必须有连续性 Session 或保持阻塞。

**阶段门槛：** Temporal/ROI/异构能力不再只有 Demand 与 placement evidence；所选证明算子在 Preview、Export、seek、取消、嵌套和 reference parity 中全部闭合，其他未实现合同继续失败关闭。

### 第 7–9 周：大项目伸缩与资源压力

- 用固定 5/30/120 分钟 Stress Project 记录作者操作小样本回归分位、文档序列化体积、History versioned logical charge、Prepared Program 编译与查询、Preview 内存和导出并发。schema 10 Windows 作者报告内的原生探针独立记录 Windows Private Commit 增量/绝对峰值以及只供诊断的 Working Set/OS lifetime peak；Linux/macOS 报告必须携带各自 typed metric 和独立预算。逻辑 History 计费仍与进程证据分离，不得冒充 allocator/RSS，优化必须对应测得的主导成本。
- 以已经接入的版本化 OS/process pressure Adapter、升降级迟滞、分域 slot handoff 和 idle Window 单调采样为基线，在独立 8 GiB 环境执行 Preview+音频+Import+缩略图+波形+代理+导出的真实并发/恢复矩阵；同时核对各深 Module 的独立队列、取消、safe-boundary yield 和终态证据，不能用纯 state-machine 测试代替进程内存与实际吞吐事实。
- 16 GiB 参考环境继续承担 4K HEVC Main10 正常路径门槛；8 GiB 环境验证有界缓存、后台暂停/降并行和可见的 Preview 降级，不要求 4K 实时流畅，也不得错误色彩、错误时间或失去作者数据。
- 对设备丢失、GPU device loss、磁盘满、权限失败和 worker 异常退出做故障注入；音频设备不可用必须连续切换到 Synthetic Clock Master，不能退化为 Video Master。

**阶段门槛：** 并发压力不会饿死实时音频或 UI，旧任务不能发布，新策略不改变作者/色彩/导出语义；所有内存档位均有可复现的降级与恢复证据。

### 第 10–13 周：完成 M1 产品证据

- 闭合 Project Recovery 冲突/选择 UI、反复崩溃、close/Save As、磁盘/权限失败；完整编辑操作、Clip 音频、代理/重连、保存重开和 Undo/Redo 进入同一 Hero Sequence。
- 对 Basic Title、Cross Dissolve 和首批基础算子完成外部 reference、长文本/缺字、嵌套、转场、CPU/GPU backend 与 Preview/Export parity，而不是用“definition 存在”计数。
- 对 Rec.709/sRGB/HLG/PQ/常见 Log 做独立数值/reference 媒体门禁；不确定输入进入 blocked/override，绝不静默显示貌似合理但错误的颜色。
- 完成 H.264/AAC 与 HEVC Main10 的长项目 roundtrip、A/V 边界、色彩/Alpha/位深、取消、失败清理和发布后校验。
- 在最终候选构建上让完整 Golden Project 监督器连续三轮通过；隔离 slice、旧构建或测试专用解释均不能拼成完成证据。

**90 天退出目标：** M1 Alpha 全部门槛有版本化证据，Windows 实机矩阵完成且三平台工程门禁通过；若任一 P0/P1 未闭合，继续修复同一里程碑，不以新增效果、格式、平台或类型数量宣布下一阶段。

---

## 8. 明确后置

以下能力不是永久放弃，但不得抢占 M0/M1 的主线容量：

- 完整相机 RAW 与所有厂商色彩生态。
- 动态 HDR10+、Dolby Vision、专业 SDI 和广泛显示校准产品化。
- macOS EDR、Linux HDR，以及 Beta 前的多平台同时发布。
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
