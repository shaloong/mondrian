# OpenFX 标准能力与实际支持矩阵

规范版本审计日期：2026-10-08；2026-10-23 复核 SinglePAR/ReplyDefault 协商与限定 Windows owned 原生门，
2026-10-20 复核退出观测，2026-10-17 增量验证 Host 属性和参数准入，
2026-10-16 复核 Message/Progress/Timeline 及通知调用链。范围：实际 native host、Rust
adapter、Effects 图与产品接入。此文记录可定位的源码能力及限制，**不是编译、
真实插件执行、跨平台或性能合格的替代证据**。运行验收保留当前提交、环境、
输入条件、命令与通过或失败结论。执行门禁所需的临时绑定在复核后清理；
不积累历史构建产物、整源哈希清单或重复收据。

有限 Float association 的 typed graph、缓存与 Normal Over、实际 image
UniqueIdentifier/FieldNone、CPU callback 线程和真实 mutex、bulk POD transport、
pre-DSP output-owner capacity admission、parent-issued instance namespace 已应用。
这表示当前 tracked 调用链存在，并不自动表示每一条原生或产品工作流已通过运行验收。
既有有限控制覆盖 raw disk-cache → nested → opaque output、公开线程/互斥与取消回收；
不能代替当前产物的性能和全面标准资格。

2026-10-16 Windows x64：当前正常 Product、新 libtest 与重建 SessionProbe 通过
76 项不同的普通控制及 22 次原生用例，包含 Describe/Catalog、三个精确生命周期模式、
相关 S02、官方 Basic direct/resident 和活动 Describe 取消。原生门均 exact1、stderr0，
原 180 秒执行/2 秒回收规则不变。真实动作观测暴露了 scanner→live 的重复 Load/Describe
及 Describe 失败缺 Unload；现由原 ImageEffectPlugin 单一状态及持久句柄负责，
成功只加载/描述一次，Load 失败不卸载，Describe 失败配对一次卸载；回调结束后才释放描述符。
此结果限上述 owned/官方样例，不是第三方、GUI、其它平台、GPU 或性能资格。

2026-10-17 Windows x64：正常 Product/runner 的六个 Host 属性/Describe 原生门
全部 exact1、stderr0，原180秒/2秒 Job 回收通过。覆盖七项 Host 身份/私有指针
属性拒写、插件 label 可写、上限4096声明，以及含 Group/Page 和纯数值参数的
4096接受/4097拒绝；两个属性门还执行现有 resident/共享 Graph 闭环。
这只验证上述 owned probe 子集，未新增性能、第三方或其它平台资格。

2026-10-22 Windows x64：ImageMemoryOwnership 通过真实 HostSupport、产品 Host
及官方 Basic 的二十项控制。原八项保持：
Host/Instance 字节分配失败各释放唯一 owner，返回 ErrMemory/null；Source/Output
FrameImage 分配失败不残留 handle、不改像素，随后正常取图和释放成功。
真实 root/Filter Descriptor 的 imageMemoryAlloc 均 BadHandle/null、零分配。
同一 Host/Instance 随后通过 257B 分配、16 字节对齐、嵌套锁、重锁内容保持、
多余 unlock 无操作及一次完整释放。真实 Source/Output ClipDescriptor 的图像与
RoD 四项按 ClipBase/ClipInstance RTTI 准入，均返回
BadHandle、清空对应输出且零分配，像素、handle数及fetch/lifetime状态不变。
同实例 Source/Output 的 clipGetHandle、RoD、取图与释放两项随后成功。
另外四项 root/Filter Effect Descriptor 的 Source/Output clipGetHandle 均
BadHandle、clip/property输出为空、零分配且图像状态不变，随后两项实例恢复成功。
该入口原将有效 Base 强转 Instance；同一测试源修复前原十四项通过后触发
0xC0000005，修复为 RTTI 实例准入并清空失败输出后二十项全过、stderr0。
原180秒/185秒外层/2秒清理与owner正常退出，独立源码、构建和结果复核通过。
资格限真实有效 Base/ClipBase 错误类型及有限内存/图像生命周期；不保证任意野指针、
整宿主泄漏、RSS/预算、实时性/性能、GPU或其它平台。图像/RoD 与本次 clipGetHandle 修复均已重链正常 Product（当前44310，dev opt1/debug2）；
构建已独立复核，直接 Host/Basic 验收仍不授该入口 Product IPC 运行资格。此前 Product bootstrap memory
曾通过，后来配对门在 shutdown 超时（2441行）、Graph 未执行，仍保持 HOLD。
2026-10-17 修复前 Product 的新增压力门仍有缺口：4096 总参数用例在
resident `shutdown()` 超时，后续 Graph 未执行；单次观测中 inner 已于约53.77ms
返回，父侧约101.07ms仍无 OS 退出观察。4097 拒绝用例另有 libtest 通过但
Job 首次计数非零的失败，未记录关联进程身份。两项根因仍开放，原预算与断言保留。

2026-10-20 Windows x64：退出观测仍按原绝对期限检查查询前后时刻；迟到退出
结算唯一 Child，但不能算 graceful。可选 schema5 保留原4096B 映射和第一次原
cleanup 的查询/kill/wait 记录，另记最后一个认证 Child Mapping 的析构入口。
它发生在 mmap/file 字段自动释放之前，不是释放完成或退出权威。
六项映射普通控制全部 exact PASS；既有期限/真实 OS Child 分支资格保持，
解码器148项纯字节控制通过。原预算不变；不是插件退出时限或性能资格。

此前历史退出观测曾配对的非 harness Product/集成测试为 test-profile opt0/debuginfo0；
它与上述当前 dev Product 是不同产物角色。
原同 Child 事件等待已替代 Windows 轮询，仍有无 trace 的 fixed-canvas 原门和
Choice 正向第二次 shutdown 报 DeadlineExceeded，不能用新等待方式宣布根因关闭。
2026-10-23 当前正常 dev Product 的一次 schema5 fixed-canvas 完整原门通过：
direct 与 retained Graph 两个真实 resident 均 graceful exit0，观测区间分别约
99.54/87.29ms；inner 返回到父观察 OS exit 约60.83/74.07ms，均无 cleanup kill/wait。
原预声明判据是：若 Deadline 后首次原 cleanup 查询仍 Pending，可排除仅父观察迟到；
本次无失败清理，未触发该判据，假设仍未决。不做 UTC 与 QPC 的跨时钟相减归因。
这只是有限仪器化样本，不是性能基准、长期上界或旧 FAIL 的替代结果。
另一个 debugger 样本认证同 resident 正常 CRT 到 LdrShutdownProcess 返回并自然
exit0，但父观察已迟于原期限，整门仍 FAIL。不得拼接两样本来归因耗时。
worker 直接返回 main，早于 UI/graphics/应用日志初始化；未发现额外 App shutdown。
Rust/CRT/导入模块的退出职责尚未定位到具体耗时原因，不授共同根因修复、全关闭
时限、第三方、性能或跨平台资格。原 shared100ms 与180秒/2秒 Job 预算保持。

2026-10-24 补充限定诊断，未修改生产代码：同一最小程序的普通返回、线程、
本地监听/TCP四组共16次退出尾段均约2.7–4.4ms；单纯网络初始化不解释旧长尾。
同程序五组各4次：Misc模块释放后尾段3.18–3.50ms，故意保留到进程退出时
54.57–72.02ms且OpenGL驻留；owned Catalog模块保留/释放均约2.5–3.1ms。
这是模块驻留的条件对照，不证明旧Worker为何驻留或共同根因已修复。
当前97595db8正常Product/App的Direct临时授权/拒绝/恢复原门通过；沿用schema5，
sealed后解码：graceful16.47ms/native析构3.65ms/inner到退出观察9.23ms，无kill/wait。
一次owned Generator调试观测显示子进程退出时GL0、测试入口GL1；真实Misc调试
运行则在原120s安装断言失败（120.29s、exit101、进程树回收），只采到metadata
Product的GL0，未到resident退出，不作正常性能/兼容资格或生产回归判断。
不拼接这些不同条件的时序来归因，也不重跑相同调试路径。F06/GL仍开放。

2026-10-18 Windows x64：`bed1d101` 在提交既有参数来源后、插件回调前更新实例
`IsAnimating`，与常量/CurrentFrame/Curve 查询共用来源权威。修复前真实 CreateInstance
查询成功却返回0（预期1）；修复后九轮同当前值来源切换的查询、通知、像素及同PID
断言通过；fixture源码亦检查Destroy，resident完整记录直到Unload return0、未报告属性错配。
新门最终 shutdown 超时，整门仍 HOLD。
四个原有生命周期、稀疏曲线、常量和 CurrentFrame 查询回归均 exact1/stderr0、原180秒/2秒
Job 回收通过。此结果不关闭既有退出问题，不新增第三方、GPU、性能或其它平台资格。

2026-10-18 Windows x64：`cbc78220` 修复清理失败覆盖原始错误的问题。修复前，
真实 TCP Shutdown 超时在注入 OS 查询/kill 拒绝后错误返回清理 Unavailable；
修复后三个 exact 控制分别保留 DeadlineExceeded、原错误响应内容及 render admission
Canceled。前两项使用真实受 Job 管理的 sleeping Child/TCP，第三项先准备正常 Product
及官方 Basic；每项均先解除注入、由同一 owner 实际 kill/reap，再检查目标结果。
两个既有 owner 控制及 `authored_native_factory_reuses_instances_and_rejects_stale_output`
产品回归也通过；六门均 stderr0、原180秒/2秒 Job 回收通过，并经独立复核。
原 fatal 例外、Retiring 拒绝复用及 sole Child 所有权不变；output allocation、inner render
及 GUI 的同策略改动仅有源码审查和相关回归，未增加逐点故障注入资格。
App 单测和配对 Product/集成程序构建通过；严格 Clippy 仍被既有的1项 unused import
和9项 dead code 阻断。本片不关闭旧退出失败，不证明 final Drop 有界、冷构造失败
owner 存续、全 shutdown 100ms、第三方、其它平台或 realtime。

Describe 请求/响应必须 schema2；保留原隐藏入口以使旧 worker 明确拒绝新请求。
通知属于已核验 binary/请求 class 的整次元数据请求，不冒充发出通知的 class 或 Render invocation。
单请求最多32条，文本4096/id256 UTF-8字节；丢失、Fatal 均拒定义并保留可信前缀。
Catalog 每 bundle 最多256条、text+id合计1 MiB，溢出丢弃整批准备结果；结果槽1，
取消交接复用原 owner。这些是冷路径通知边界，不是整堆内存或 realtime 保证。
每行只覆盖声明的上下文、格式、时间与消费者；头文件、enum、upstream wrapper
和测试名字都不能单独证明运行支持。

2026-10-19 Windows x64：String setter 现复用既有 `permitsParameterWrite`，
保留 null/长度及时间错误的原优先级。普通 CPU Render 仅在原回调作用域内阻止
插件写参数，保存并恢复原权限；与图像访问独立，不阻止宿主赋值或 Create 初始化。
真实 SessionProbe 的无 GUI 两次 Render 中，Double/String 的普通及当前时间 setter
均返回 Unsupported、即时值不变；Create 四个 setter 和宿主 A/B 赋值通过。
原 readonly-GUI 的 Begin、两次 Render、End String 拒写、原 `gui_cycle`、同实例
Close/Destroy、输入/作者值及 Float 像素均通过。其它 setter 复用权限的结论来自源码，
未逐类型运行；异常与嵌套恢复亦仅作源码审查。
新配对正常 Product 的 String 全模式/合法64KiB/非法输入后同 worker 复用门，
以及 OpenFX 三模块96项普通测试通过；stderr0，原180秒/2秒 Job 回收通过。
此资格限上述普通 CPU Render 与内部 readonly-GUI 策略；完整 action-specific/
analysis Render 写权限和 C2 canonical author 桥仍缺，不代表插件写入已可 Undo/save、
Product GUI、旧退出修复或完整标准兼容。

2026-10-22 Windows x64：项目几何与图像栅格分离的限定切片通过。
Effects 的 `EffectProjectGeometry` 来自 owner Sequence，并参与原图身份和
processor 绑定；原 resident v10/C ABI 独立传递 project width/height/PAR。
固定零 offset 的 project Size/Extent/PAR 不再由本帧 raster 推导。owned
SessionProbe 在 3×5 图像、10×7/PAR2 项目下验证 Create/Render 查询与全部15像素；
Direct image PAR1→1.2→1 合法，真实 Graph 同项目复用、12×9 项目替换 owner。
独立 raw App libtest 门验证 width/height/PAR 改绑拒绝、不发布 output、原 owner
恢复可用；拒绝发生在 `loadFrame` 之前由源码审查确认，运行断言检查无新增 invocation。
raw 门不执行 Product worker；集成门使用正常 dev Product。
两新门和 Generator 两门、官方 Basic 两门、capabilities 回归共七门均 exact1、
stderr0、原180秒/190秒外层/2秒清理通过。
资格仅限上述 Windows CPU/owned 子集。后续 CPU 图像值与缓存已携带独立
PAR/双轴采样率：Effects352、相关 Renderer207、缓存19、相关 App694、Export4
控制通过；Export 含两个真实 FFmpeg 控制。App 保留原筛选与既有排除项，
Unit/Any 的原始源/代理/质量分流、过渡与前瞻一致性，以及奇数源 effect/mask
和 Export 解码均通过。共享 Any/Unit 能力与 provider 前 typed 准入也通过 strict。
native v11 actual sampling 现通过 Windows owned 采样门。2026-10-23 的真实媒体门
验证原始 PPM3×5、Clip PAR4/3 经 Unit native 后再合成 Preview2×2/3×2；全部像素、
双轴 sampling、同一 owner、递增 invocation、作者状态与 consuming cleanup 均通过。
SinglePAR 九门与七个受影响 worker 控制在当前 SDK 修复后全部回归通过：成功偏好请求
不得改写 connected clip 的实际 PAR；零/NaN 与 Source/Output/共同 remap 在 Render 前
拒绝。同值和 ReplyDefault 合法/NaN 残值保持三帧 PAR1→1.2→1、像素及同一 owner。
Generator 使用实际 Output PAR，project PAR2 独立；断开可选 clip 的属性/fetch/Render
通过，但未测试其 PAR 改写。`541630cb` 进一步修复 SDK 将 ReplyDefault 当 OK 消费
outArgs 的根因：调用前已安装宿主默认值，Default 直接保留它们，不新增快照或状态源。
新增八门验证 writable components/alpha 的 Default/OK 对照：七门完整通过；Default
Opaque 的三帧 literal、association、sampling 和 owner 断言全部通过，最终原 shutdown
却返回 DeadlineExceeded，整门仍 FAIL/HOLD。Depth 只验证实际拒写，不声称成功残值。
当前正常 dev opt1/debug2 Product 与同期 opt0/debug0 integration 已重建并独立复核。
24项原生门准确为23整门通过、1关闭失败；另有三模块114项普通测试通过。所有原生门
stderr0、原180/190/2与外层 Job 回收正常；不授 parent direct-CABI、完整 Default 字段、
Generator 全 Default 或 F06 根因修复资格。原生 OFX 仍为 Uncacheable，限制传递至嵌套图、
Preview/persistent cache；未来启用复用前必须纳入宿主执行语义身份。
此资格限固定帧/owned 插件，不授完整 Timeline 编辑/Export、其它插件或完整 Context。
Unit+Temporal
源物化明确拒绝；合法 Unit Title crop、作者尺寸 Nested 和 Full accumulator
保持原语义，实际非 Unit 输入在 provider 前拒绝且不 quarantine。
proxy/Reduced 的 native renderScale、空间默认值与 Rectangle 仍未实现或验收，
GPU 与其它平台不在此资格内。

## 1. 规范版本与证据

官方 GitHub `releases/latest` 实际返回 **OFX_Release_1.5.1**，发布时间为
2025-11-20；2026-10-08 的最新已发布标准仍为 1.5.1。
官方发布说明将 CPU capability、thumbnail intent、no-spatial intent 列入
1.5.1；1.5 已加入 colour、DrawSuite、CUDA/Metal/OpenCL、choice order、Bytes。
[官方发布](https://github.com/AcademySoftwareFoundation/openfx/releases/tag/OFX_Release_1.5.1)，
[不可变发布说明](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/release-notes.md)。

版本核对以不可变 `OFX_Release_1.5.1` 的 Core、Param、ImageEffect、
Colour、GPU、Draw、MultiThread 头文件为依据。在线 `/main/` 文档可能包含
NEXT；页脚显示 1.5.1 并不证明所有后续符号已发布。
[已发布头文件](https://github.com/AcademySoftwareFoundation/openfx/tree/OFX_Release_1.5.1/include)，
[开发分支头文件](https://github.com/AcademySoftwareFoundation/openfx/tree/main/include)。

| 不可变 1.5.1 对照结果 | 当前 vendored main 符号 | 本产品状态 |
| --- | --- | --- |
| 1.5.1 含 Bytes 参数类型 | `kOfxParamTypeBytes`, `OfxBytes` | 尚未实现；不能只因头文件存在算支持 |
| 1.5.1 **无**参数 interpolation 声明 | `kOfxParamInterpType`、Step/Linear/Smooth | 产品已有实现与 fixture；本轮运行收据另行记录；这是 NEXT 能力，不是 1.5.1 必需功能 |
| 1.5.1 **无** colour-managed RGB/RGBA 参数声明 | `kOfxParamPropColourManagement`、None/Managed/SRGB | main 扩展尚未实现；不能把它误报成 1.5.1 缺失要求 |
| 1.5.1 **无** obsolete 插件属性 | `kOfxImageEffectPluginPropObsolete` | 未实现 |
| 1.5.1 **无** unlicensed policy/status | `kOfxImageEffectPropBehaviourWhenUnlicensed`, `kOfxStatUnlicensed` | 未实现；普通错误仍走失败路径，不代表支持 licensing policy |
| NEXT 说明新增 project-load change reason、ARM64EC packaging、descriptor GPU/tile inheritance 修正 | main release-notes-next | 未作为本产品完整 NEXT 兼容声明 |

以上区分有 immutable header 对照，不依赖旧架构文档。
[官方 NEXT 说明](https://github.com/AcademySoftwareFoundation/openfx/blob/main/release-notes-next.md)。

当前 Host `kOfxPropAPIVersion=[1,4]`；ImageEffect `OfxPlugin::apiVersion=1`
是另一种版本号。完成几个 1.5.1 特性，不足以把 Host 宣称为完整 1.5.1。
实证：`native/openfx/host/host_descriptor.cpp::Host`。
[API 标识](https://openfx.readthedocs.io/en/main/Reference/ofxImageEffectAPI.html)。

### 分类

- **必需**：已接入 ImageEffect API 的 bootstrap、对象/句柄/内存、已开放套件语义。
- **协商**：Host 可合法不支持的 contexts、formats、temporal、tiles、GPU、interacts。
  合法拒绝该能力并不意味着兼容依赖它的插件。
- **条件必需**：启用某工作流后必须满足的动作，例如编辑、保存 private state、interact 生命周期。
- **实现**：tracked 已有实际调用链；只允许在声明范围内使用。
- **部分**：有调用链，但有已知语义缺口或只支持限定域。
- **拒绝**：有明确 admission/缺失套件，依赖该能力无法执行。
- **未接入**：存在 upstream wrapper/常量，产品没有驱动工作流。
- **待验收**：本轮缺该闭环的真实独立效果/平台/性能收据。

## 2. Bootstrap、套件与对象语义

路径前缀 `app/` 指 `crates/mondrian-app/`；`native/` 指其
`native/openfx/`。表中使用的是 tracked 源码与符号，可直接定位。

| ID | 规范能力 / 要求性质 | 实际状态与边界 | 代码依据 | 官方 primary |
| --- | --- | --- | --- | --- |
| B01 | binary load/count/get，必需 | 实现：独立 worker discovery，UTF-8 Windows loader；预算上限 1024 plugins 是产品 admission 限制 | `app/src/openfx_adapter.rs::inspect_loaded_binary`; `native/host/render.cpp::SelectedPluginBinary` | [Core](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| B02 | 可选 `OfxSetHost(const OfxHost*)`，存在时先于 count/get | 实现：真实 Host + 已构造 ImageEffect cache；Failed silent empty，其他 non-success 明确错误，缺 export 合法 | `NativeDiscoveryHost`; `SelectedBinaryBootstrap`; `MondrianOpenFxDiscoveryOwner` | [Core](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| B03 | mixed API / unknown version binary，必需 | 实现：先 exact ImageEffect literal+NUL/version，未知 API/version 跳过，不解析无关 ID/body；原 export index 保留 | `is_image_effect_api`; `SelectedPluginBinary::initialize` | [Plugin struct](https://openfx.readthedocs.io/en/main/Reference/ofxPluginStruct.html) |
| B04 | 每个 accepted plugin setHost / host 生命周期，必需 | 实现：native selection SDK 调用 per-plugin setHost；owner 保证 cache/module/unload 在 Host lease 内；一个 native worker 一个 Host，第二 owner 即时 Busy，无每帧锁 | `OwnedImageEffectCache`; `ImageEffectHostLease`; `NativeSession` 非 Send/Sync | [Host](https://openfx.readthedocs.io/en/main/Reference/ofxHostStruct.html) |
| B05 | suites name/version/null，必需 | 部分：unknown suite=null；MultiThread 与 Message/Progress/Timeline 接产品实现。各自边界见下；Interact 尚未接入，不因 non-null 计支持 | `host_descriptor.cpp::Host::fetchSuite`; `thread_suite.*`; `aux_suite.*` | [Suite inventory](https://openfx.readthedocs.io/en/main/Reference/suites/ofxSuiteReference.html) |
| B06 | Property V1 全部 typed scalar/N set/get、reset/dimension，必需 | 部分验证：Host 身份/私有指针属性对插件只读；MaxParameters=4096 与 descriptor 准入共用常量；六项元数据门覆盖 label、Group/Page 与数值。Windows owned Clip Int/Double、Effect Double 和四种 raw getter/Clip String 的两帧 Direct/Graph 功能断言通过；共享 GetN 负 count guard 覆盖每帧18项Image/优先级与12项String。固定 setter 按声明维度判断，真实 Param Label/Icon 与变长 Main PageChild 每帧24+8项及两路径像素/Source/author/恢复值通过；CPU/PAR/String 三原回归通过。新增共享读取维度准入97595db8：raw固定/变长/空/链式及Hook四种类型独立原生64失败→0，CMake入口通过；真实Image/Clip/Effect/action检查及四版本Graph、真实Misc安装/恢复/Timeline37.80s与其它回归共9原生门通过，141普通通过/79忽略。五项旧整门仍原shared100ms shutdown FAIL；不授完整Property V1、GUI刷新或4096实例退休 | `vendor/host/src/ofxhPropertySuite.cpp`; `ofxhClip.cpp`; `ofxhImageEffect.cpp`; `Host::kMaxParameters`; `tests/openfx_host.rs` | [Property](https://openfx.readthedocs.io/en/main/Reference/suites/ofxPropertySuiteReference.html) |
| B07 | Memory V1 alloc/free，必需 | 实现 malloc/free；null/adversarial ABI与内存不足行为全面资格未完成；不纳入父图预算 | `vendor/host/src/ofxhHost.cpp::Memory` | [Memory](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMemory_8h.html) |
| B08 | imageMemory alloc/free/lock/unlock，必需 | 部分验证：真实 Host/Basic 八项控制通过原字节/FrameImage 失败恢复、真实 Descriptor 错误类型拒绝、Host/Instance 正常分配、16对齐、nested lock/relock 内容与完整释放。类型准入及 RAII 保留唯一 owner；不纳入父图预算。任意野指针、完整生命周期矩阵/RSS ceiling 未验收；Product shutdown 缺口见上方 | vendor/host/src/ofxhMemory.cpp; ofxhImageEffect.cpp; tests/openfx_reference/image_memory_ownership.cpp; OwnedImageEffectCache | [Image suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxImageEffectSuiteReference.html) |
| B09 | Message V1 log/info/warning/error/fatal，必需 | 实现：AuxSuite 区分 NULL/descriptor/instance；Render notice 经 owner/invocation 校验，Describe-only 经独立 schema2 整请求结果交给当前 Catalog。Log 入 tracing，拒绝不完整/Fatal 定义。Windows 有上述有限原生资格，非全场景 Message 资格 | `aux_suite.cpp::message`; `openfx_host/description.rs`; `app/openfx_catalog`; `app/preview_processor_feedback` | [Message](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMessage_8h.html) |
| B10 | Message V1 question reply，interactive 条件必需 | 实现受控 question：实际 GUI/control 授权与当前 invocation 才可请求 Yes/No，非交互明确 Failed；完整 GUI 生命周期与平台资格另行验收 | `aux_suite.cpp::message/bindControl`; live question transport | [Message](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMessage_8h.html) |
| B11 | Message V2 persistent set/clear，optional exposed | 实现实例级 persistent revision、set/clear 与当前 scope 通知；Error 阻断，author change 后可同 owner 恢复，拒绝 stale feedback；本轮未重跑对应原生控制 | `aux_suite.cpp::persistent/clear`; `resident.rs`; `app/preview_processor_feedback.rs` | [Message V2](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMessage_8h.html) |
| B12 | MultiThread V1 multiThread/CPU/index/spawned，必需套件 | 实现限定的 1-CPU grant：独立 lazy callback thread，callback(0,1)，真实 spawned/index，阻塞至自身 accepted job 完成；nested ErrExists，其他同时调用 Failed。8 MiB stack+guard 预准入，非全核 SMP；r103 对应真实原生与公开产品控制通过，整批 source/artifact lease 稳定 | `thread_suite.cpp::ThreadSuite::{run,numCPUs,index,isSpawned}`; `State::{work,jobCompleted,jobBusy}` | [MultiThread](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMultiThread_8h.html) |
| B13 | MultiThread V1 mutexCreate/Destroy/Lock/UnLock/TryLock，必需套件 | 实现：最多 1024 个 owner/generation checked mutex，真实跨线程 mutual exclusion、递归 depth、TryLock 不改失败锁计数；initial count<=0 合法初始解锁，positive 由创建线程持有；destroy/stale/reused handle 有边界；r103 对应真实原生与公开产品控制通过，整批 source/artifact lease 稳定 | `thread_suite.cpp::MutexState`; `ThreadSuite::{createMutex,destroyMutex,lockMutex,unlockMutex,tryMutex}` | [Mutex](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMultiThread_8h.html) |
| B14 | Progress V1/V2 start/update/end，optional exposed | 实现 begin/update/end 生命周期、实时反馈与 invocation cancellation 查询；完整产品取消时延与并发资格仍须分场景验证 | `aux_suite.cpp`; invocation control; `resident.rs` | [Progress](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxProgress_8h.html) |
| B15 | Timeline V1 getTime/getBounds/gotoTime，optional exposed | 部分：getTime/getBounds 读实际快照；无法满足的有限 gotoTime 明确 Failed。产品导航尚未接入 | `aux_suite.cpp`; invocation timeline snapshot | [Timeline](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxTimeLine_8h.html) |
| B16 | Interact V1 swapBuffers/redraw/getPropertySet，optional | 未接入：wrapper/suite存在；host overlays/customInteract=false，未创建 interact，不能执行真实 draw/input | `Host::fetchSuite`; `Host` descriptor；产品无 interact owner | [Interacts](https://openfx.readthedocs.io/en/main/Reference/ofxInteracts.html) |
| B17 | ParametricParameter V1 全部 curve/control-point API，optional | 拒绝/未实现：宏未启用、suite=null，无 canonical parametric function author schema | `app/build.rs`; `Host::fetchSuite`; `newParam` | [Parametric](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxParametricParam_8h.html) |
| B18 | Dialog V1 requestDialog/notifyRedrawPending，optional | 拒绝/未实现：suite=null、无 UI-thread dialog action dispatcher | `Host::fetchSuite`; product request enums | [Dialog](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxDialog_8h.html) |
| B19 | DrawSuite V1 全 6 functions，optional 1.5 | 拒绝/未实现：get/setColour、width/stipple、draw、UTF-8 text 均未 expose | `Host::fetchSuite`; no Draw adapter | [Draw](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxDrawSuite.h) |
| B20 | OpenGLRender V1 3 functions，optional | 拒绝/未实现：macro off、suite=null，没有 GL texture/context ownership | `app/build.rs`; `Host::fetchSuite` | [GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| B21 | OpenCLProgram V1 compileProgram，optional | 拒绝/未实现：suite=null，host queue/program/kernel/device lifetime 未接入 | `Host::fetchSuite` | [GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |

## 3. 全动作 / 生命周期

| ID | 动作 / 要求性质 | 当前 runtime 状态 | 代码依据 | 官方 primary |
| --- | --- | --- | --- | --- |
| A01 | Load/Describe/DescribeInContext/Unload，必需 | 实现：selected 路径复用持久 SDK handle 与单一五态，不再叠加 scanner 生命周期；失败退役及 descriptor 存活有上述 Windows 动作控制。discovery 仍只枚举；未授 generic SDK scanner 的全面资格 | `selectFilter`; `ImageEffectPlugin::getPluginHandle/unload` | [Actions](https://openfx.readthedocs.io/en/main/Reference/ofxImageEffectActions.html) |
| A02 | Create/DestroyInstance，必需 | 实现：Create 前提供 complete author snapshot；Destroy 在 module unload 前。合法 callback image scopes 有显式 borrow guard | `MondrianOpenFxSession::initialize`; `FrameBorrow/ImageAccess`; `NativeSession::drop` | [Core actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A03 | Begin/InstanceChanged/End，条件必需 | 实现当前 author/time/clip facts 通知；value 相等但 curve source revision 改变仍通知。native-origin author edits/state 更新没有回传事务 | `render.cpp::applyParameters`; curves callback；`parameter_instance.*` | [Recursion](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html) |
| A04 | Begin/EndInstanceEdit，UI 条件必需 | 已接只读交互生命周期：原Control提供interactive borrow时setInterfaceOpen调用Begin，关闭/销毁调用End；native-origin作者手势与完整厂商GUI仍未验收 | FilterEffectInstance::setInterfaceOpen/closeInterface；原CPU GUI action transport | [Core actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A05 | SyncPrivateData，保存条件必需 | 未接入，不能保存插件 private state 或 native-origin参数写入；只能保存产品已经拥有的 canonical 参数与 resources | `vendor Instance::syncPrivateDataAction`; resident `Request`; `app/openfx_catalog.rs` | [Sync action](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A06 | PurgeCaches，memory-pressure 条件必需 | 未接入，没有 plugin cache 压力/内存错误 retry 请求 | `vendor Instance::purgeCachesAction`; resident `Request` | [Purge](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A07 | BeginSequenceRender/Render/EndSequenceRender，必需 | 普通 Render 分支实现独立帧：一帧一 begin/end、frameStep=1、scale=1、FieldNone；Render 必须 OK、取过 Output、image handles 完整释放、完整有限 staging、End 成功后 publish；可用的 A13 identity 由宿主复制，不伪造 Render 或 Output fetch | `render.cpp::render`; `resident.rs::render` | [Render actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A08 | 必须顺序 / optional sequential，协商 | Required(1) 明确拒绝；0/optional(2) 按独立帧执行。无长序列 begin/end、连续 state、ordered seek/reset/abort/restart 闭环 | `admitsRandomAccessFrames`; `render.cpp` | [Thread safety](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html) |
| A09 | GetClipPreferences，必需 | 部分验证：Filter 检查 Float/RGBA/真实 Source association 与协商 Output association，Generator 仅实际 Output。SinglePAR connected 默认值来自实际 frame；OK 仅允许合法同值 PAR，违规在 Render 前拒绝，ReplyDefault PAR 残值忽略；当前九门八整门通过，noop 三帧功能通过后原关闭超时。SDK 保留已安装的全部 Default；components/alpha 八门六整门通过，Default Alpha components/opaque 两门功能通过后原关闭失败，depth 写入仍真实拒绝。七相关 worker 回归六整门通过，optional-unmapped 功能通过后原关闭超时。场序协商见 I11、固定帧率见 I16；F06 根因仍开放。typed alpha/coverage 边界见 I05–I07 | `FilterEffectInstance::setCustomOutArgs/mainEntry`; `render.cpp::render`; `FilterClipInstance::getPremult` | [Clip preferences](https://openfx.readthedocs.io/en/main/Reference/ofxClipPreferences.html) |
| A10 | GetRegionOfDefinition / image-domain 调度 | 有限Windows CPU切片已验收：context default、完整有限RoD→viewport、负原点/扩张/空/PAR/错误拒绝及真实Constant Size通过；原bank按实际临时需求授权，原128MiB Invert Graph不改预算通过；真实Graph拒绝无DSP、保留反馈/owner、不隔离及后续帧通过。上游ROI/完整可变图像域仍缺失 | `image_domain.h`; `render.cpp`; ADR0015；兼容性矩阵 | [Image actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A11 | GetRegionsOfInterest，ROI 条件必需 | 部分：SDK wrapper 会调用或按非 tiles fallback；结果不用于 source scheduling/alloc。host full-frame/1x 路线 | `render.cpp` map rois；`vendor getRegionOfInterestAction` | [Coordinates](https://openfx.readthedocs.io/en/main/Reference/ofxCoordSystem.html) |
| A12 | GetFramesNeeded，temporal 条件必需 | 限定Filter Source已实现/验收：同一native instance在RoD/DSP前查询；保留离散ranges、Default/空集合并原子拒绝非法依赖。effect/Source声明独立检查，实际像素由原invocation input service按需取得；Output temporal、必须顺序仍拒绝 | render.cpp::prepareFrame；HostSupport getFrameNeededAction；[ADR0016](../adr/0016-plan-native-temporal-dependencies-in-the-existing-graph.md) | [Frames](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A13 | IsIdentity，可选 optimization | Filter当前Source/time保留严格copy优化；合法temporal Source identity走原Render/input service。真实TimeBlur C ABI temporal Identity/完整RGBA有限验收；Generator不查询Source identity，非法Output/格式仍拒绝。旧关闭失败保持F06条件，不授完整lifecycle/平台资格 | render.cpp::render；TemporalThirdParty；[ADR0016](../adr/0016-plan-native-temporal-dependencies-in-the-existing-graph.md) | [Identity](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A14 | GetTimeDomain，仅 General context | 未接入；General 仍拒绝。Filter/Generator 时域由 host owner 控制，Timeline Bounds 查询不是该 action，也不是 Generator 的前置义务 | `FilterEffectInstance::timeLineGetBounds`; 无 `getTimeDomainAction` 驱动 | [固定 SDK action 契约](https://github.com/AcademySoftwareFoundation/openfx/blob/e40728885390ec16276d11e00025de9b4282060c/include/ofxImageEffect.h#L206-L226) |
| A15 | GetOutputColourspace，colour 条件必需 1.5 | 未接入，未 expose Colour style/config、clip preferred/actual colourspaces | `render.cpp`; host/descriptor/property tables | [Colour](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxColour.h) |
| A16 | GPUContextAttached/Detached，GPU 条件必需 | 未接入，所有原生 GPU API disabled；wgpu 本身不是 OFX 外部 queue/texture capability | product native host + `app/build.rs` | [GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| A17 | Dialog action / interact separate main lifecycle，条件必需 | 未接入；built-in Viewer anchor/ROI 操作不能证明插件 interactor接通 | product无交互owner；upstream Interact wrappers | [Interact actions](https://openfx.readthedocs.io/en/main/Reference/ofxInteracts.html) |
| A18 | action error/default/abort/memory policy，必需 | 部分：支持合法 ReplyDefault 的部分动作、Render必须OK；native失效/cancel/deadline retire/reap无stale publish。abort已接实例取消回调，完整 action 级取消闭环未独立验收；ErrMemory purge/retry、GPU-specific retry未接入 | `accepted`; `render`; `resident ProcessOwner`; `vendor Instance::abort` | [Status/core](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |

## 4. 六个 contexts 与完整 CPU image contract

Contexts 是协商能力，标准并不要求任意 Host 支持六个。
本产品目标“所有三类规范插件”若包含只提供其余 context 的插件，需明确
完成这些 vertical slices，不能把合法 refusal 描述成产品已兼容。
[官方 contexts](https://openfx.readthedocs.io/en/main/Reference/ofxImageEffectContexts.html)。

| ID | Context/能力 | 当前 runtime | 代码 / independent seam | 剩余切片 |
| --- | --- | --- | --- | --- |
| C01 | Filter：Source/Output | Windows Float RGBA CPU，当前及声明temporal Source；有限真实TimeBlur/原图执行见I15。optional aux clips disconnected，required extra clip拒绝 | Host SupportedContexts；describe clip checks；FilterClipInstance | Filter suite/image及更多输入资格仍见S01–S04/S11 |
| C02 | Generator：Output、host defaults/RoD | 限定Windows CPU/FloatRGBA/current-frame；Output必需、可选输入断开。Direct/source-free graph、有限RoD与默认值迁移保持资格；新增真实Constant作为既有Solid Clip效果的Full Preview/Export工作帧、历史及保存重开闭环通过 | `describe_openfx_generator`; Effects NoImage；ADR0013/0015；兼容矩阵 | 专用Generator Clip、其它原生placement/动画/Reduced/temporal/GPU/平台资格仍未完成 |
| C03 | Transition：SourceFrom/SourceTo/Output，host Transition | 拒绝/未实现 | 无 named source protocol/context selection | S10，timeline exact phase+two compiled inputs，参数不得作为普通插件控制 |
| C04 | General：Output+arbitrary named inputs | 拒绝/未实现 | optional Filter clips永远 disconnected | S10，typed named source bindings/signatures/resource admission |
| C05 | Paint：Source/Alpha Brush/Output | 拒绝/未实现 | 无 Alpha layout/brush nativeinput | S10后专门 brush/mask clip + size/PAR contract |
| C06 | Retimer：Source/Output，host SourceTime | 拒绝/未实现Retimer context；Filter时域Source与公开Clip变速不等于该context | context admission；原Clip source-time mapper | S11：Retimer SourceTime参数、context与独立真实调度资格 |

| ID | Image/timing能力 / 要求性质 | 当前 runtime 状态 | 代码依据 | 官方 primary |
| --- | --- | --- | --- | --- |
| I01 | CPU render capability，optional 1.5.1 | 实现：host true、descriptor默认true、instance继承且可instance-local改变；false于Render前拒绝 | `Host::makeDescriptor`; `FilterEffectInstance`; `admitsCpu` | [GPU/CPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| I02 | Float32 RGBA，协商 | 实现：完整 finite Float32 RGBA，128 MiB 单 raster cap，Filter 双 raster mapping、Generator 单 Output mapping，底部原点/negative rowBytes；bulk byte copy 后唯一 typed storage validation，Opaque ignored-A 规范化1；更大帧为 admission 限制，既有 Filter r103 raw/copy 与 public associated output 控制通过，整批 source/artifact lease 稳定；Generator 有 S09 owned 及发布 Constant/CheckerBoard 的有限处理资格，具体像素/PAR/alpha 条件见兼容矩阵 | `loadFrame`; `validate_frame`; `FrameImage`; `resident.rs::{copy_native_input,materialize_native_output}` | [Images](https://openfx.readthedocs.io/en/main/Reference/ofxImages.html) |
| I03 | Byte/Short/Half，协商 | 拒绝/未实现；不是“把 Float 改枚举”即可，实际layout、quantization、HDR、colour/alpha映射缺失 | Host SupportedPixelDepths=Float；clipprefs exactcheck | [Pixel depths](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I04 | RGB/Alpha/componentsNone，协商/未连接信息 | 仅RGBA执行；RGB/Alpha不能渲染。未连接clip按实际连接状态返回unmapped None/None、getImage Failed/null。Windows owned门已过六项属性、同实例RGBA逐bit×0.5及Source不变，但最后原shutdown DeadlineExceeded，整门未通过；原时域准入3门及CPU/PAR resident/Graph两门通过。不授完整生命周期、第三方或其它平台资格 | Host SupportedComponents=RGBA；`FilterClipInstance`；`product_worker_reports_none_for_disconnected_optional_clip_unmapped_format` | [Clip](https://openfx.readthedocs.io/en/main/Reference/ofxImageClip.html) |
| I05 | Source premult/opaque 与 pixels一致，条件必需 | 实现：immutable prepared format 的 actual Straight/PM/Opaque association 传到 Source clip/image，pixels 不被隐式 unassociate；preference 与 supplied format 不符拒绝，不存在裸 Vec 旁路 | `EffectValueFormat`; `OpenFxFilterSession::input_format`; `FrameState`; `FilterClipInstance::getPremult` | [Premult](https://openfx.readthedocs.io/en/main/Reference/ofxImageClip.html) |
| I06 | Output Straight/PM/Opaque，条件必需 | 实现：clip/image 与 Output preference 一致；public output 为 actual StraightFloat/PMFloat/Opaque typed frame，PM 不再强制除回 coverage；Opaque A=1，所有实际输出 lane finite/extent checked。r103 raw output、Source association 与 nested/opaque product 控制通过，整批 source/artifact lease 稳定 | `render.cpp::outputAlpha`; `resident.rs::materialize_native_output`; `EffectFloatFrame::from_pixels_controlled` | [Clip preferences](https://openfx.readthedocs.io/en/main/Reference/ofxClipPreferences.html) |
| I07 | 完整 finite Float PM/RGBA，包括HDR/负色/zeroα emission，条件必需 | 实现限定域：native→Core/Effects→Normal Over→缓存→nested→opaque output 的 finite Float signal 已接实际调用链；r103 原始 cache bits、独立 nested oracle 与真实 opaque 输出闭环通过。PM zero-alpha emission、signed/>1/HDR/negative 不 clamp。非线性 blend/OCIO/GPU coverage consumer 经 explicit format/admission，不能直接消费所有 Float。r76 GPU 独立 overflow/recovery GREEN，其他新载体待验收 | `mondrian-core::ColorFrameAlpha`; `EffectFloatFrame`; `float_frame::over_float_sample`; `float_frame/exact_over.rs`; `heterogeneous_gpu.rs`; render-cache typed header | [Pixels](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h)，[OpenEXR association](https://openexr.com/en/latest/TechnicalIntroduction.html) |
| I08 | PAR/canonical project size/extent/offset/clipRoD，必需 | 部分支持：owner Sequence 项目几何独立于 image raster/PAR，canonical project x=projectWidth*projectPAR/y=projectHeight、offset0；改变项目替换原 owner，raw 改绑拒绝。Direct image PAR 动态正控通过；image bounds/renderWindow 为 pixel 坐标，clip RoD 使用 image PAR。Timeline 源 SAR 传递、proxy/renderScale、非零 offset 尚未闭合 | `EffectProjectGeometry`; `getProjectSize/Extent`; `getRegionOfDefinition`; `loadFrame`; `project_geometry` 与 raw 改绑测试 | [Coordinates](https://openfx.readthedocs.io/en/main/Reference/ofxCoordSystem.html) |
| I09 | 底部原点、negative rowBytes/连续RGBA存储，必需 | 实现并有不对称raster oracle；Source/Output scope handles独立，parent借用mapping只在action内。`clipGetImage` bad_alloc 返回 ErrMemory/null；图像、RoD与clipGetHandle按实际实例准入，拒绝真实Descriptor且保留正常实例恢复。Host/Basic二十项有限控制通过；新修复无正常Product IPC资格，完整故障/生命周期边界见上方 | `FrameImage`; `ImageAccess`; `imageHandles`; actualBasic fixtures | [Images](https://openfx.readthedocs.io/en/main/Reference/ofxImages.html) |
| I10 | `kOfxImagePropUniqueIdentifier`，必需image metadata | 实现：parent 单次 typed EffectProcessorOwnerId namespace 经 wire4 Prepare/16-byte CABI 初始化，session generation 每次 image authority 改变递增，Source/Output distinct，同 action repeated fetch stable；fresh worker/seek 不重用裸 process-local namespace。r76 原实现跨子进程 stale-analysis 实际 RED；r103 新 namespace 的 native 与 public cache 断言通过，整批 source/artifact lease 稳定 | `frame_state.h::FrameState`; `image_clip.cpp::FrameImage`; `resident.rs::{Prepare,OpenFxFilterSession::prepare}` | [Image header](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I11 | Progressive FieldNone，必需image metadata | 部分验证：实际 Source/Output Image、clip order 与 Render field 均 None，仅 progressive CPU。host SetableFielding=false；OK Upper/Lower/未知场序在 Render 前以 native4 拒绝，ReplyDefault 残值不生效。Windows owned Filter FloatRGBA 六门整门通过；三个正控各验证三帧全15像素、SAM1→1.2→1、同owner及原shutdown。基线两个正控及当前PAR回归仍有原shutdown期限失败，F06根因未解决；不含 interlaced 调度、parent直接CABI、其它平台或实时性能 | `image_clip.cpp::FrameImage/getFieldOrder`; `render.cpp::render`; `FilterEffectInstance::mainEntry` | [Fields](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I12 | Upper/Lower/Both/field extraction、half-frame，协商 | 拒绝/未实现；setableFielding=false，无合法field scheduling/Sourceimageextract | `Host`; fixedRenderFieldNone | [Rendering](https://openfx.readthedocs.io/en/main/Reference/ofxRendering.html) |
| I13 | Multiple resolution/depth/PAR，协商 | host 全部 false；独立 Source/Output dimensions/PAR/depth 不支持。已验证 connected PAR remap/零/NaN 请求拒绝；每帧同一实际 PAR 可更新，且独立于 project PAR。断开 optional 仅验证原属性/fetch/Render，PAR 改写未验收 | `Host`; fixed native frame extent；`FilterEffectInstance`；SinglePAR 九门 | [Clip preferences](https://openfx.readthedocs.io/en/main/Reference/ofxClipPreferences.html) |
| I14 | Tiles / ROI / expanded/infinite RoD / renderScale | Partially Supported：有限完整RoD→viewport及原bank动态临时准入通过Windows CPU实际原生/公开流程回归，原预算回归已修复。Tile/非单位scale/上游ROI调度仍未支持，无限/超限明确拒绝；不宣称完整可变图像域、GPU或性能达标 | `Host`; `getImage`; `render`; ADR0015 | [Coordinates](https://openfx.readthedocs.io/en/main/Reference/ofxCoordSystem.html) |
| I15 | Temporal off-time Source access，协商 | Partially Supported：原bank/Control/CPU source job/Frame Store支持有限Filter历史/分数帧；真实TimeBlur C ABI/Preview/Export、seek取消、历史归档、有限播放、公开变速/倒放/定格及精确PTS Before均有条件化验收。已知范围外无图像的Source RoD为空，in-range失败/内存拒绝保留。其它输入/context/顺序/GPU/平台仍开放 | FrameBorrow；IPC17 borrowed source service；兼容矩阵与[ADR0016](../adr/0016-plan-native-temporal-dependencies-in-the-existing-graph.md)为当前条件来源 | [Frames/actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I16 | FrameRate/Range/Duration/continuous samples，必需已选上下文 | 部分验证：owner-derived frame/rate/range/PAR，ContinuousSamples=false、SetableFrameRate=false；OK 输出率须单值、有限、正且等于本次实际 rate，否则 native4/零 Render；ReplyDefault 残值忽略。Windows owned FloatRGBA Filter/无 Source Generator 七原门整门通过：不同率/零/NaN 真写入反控，同值/Default/Generator 暖24→30000/1001→24、全像素/SAM/同owner及原shutdown。相关30门26整门通过，四项原关闭失败见 A09，基线退出失败也保留；不授F06修复。Mapped/unmapped 在此均投影同一适配器 rate，不证明原媒体fps/重定时或帧率转换；连续采样的零率例外及其它平台未验收。查询边界见 B06 | `OpenFxFrameTiming`; `FilterEffectInstance::mainEntry/getFrameRate`; `FilterClipInstance` | [Clip](https://openfx.readthedocs.io/en/main/Reference/ofxImageClip.html) |
| I17 | Render thread safety/singleInstance/license约束，条件必需 | serial instance render实现；每worker隔离静态module内存。但应用可能多个worker同plugin，未enforce SingleInstance声明，未qualify厂商external/license/session约束 | `isSingleInstance`未product使用；`OpenFxProcessorFactory` | [Thread safety](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html) |
| I18 | RenderQualityDraft，optional1.4；Thumbnail/NoSpatial，optional1.5.1 | 未plumb product intent；固定false或缺property。普通fullquality route可用；LUT baking不得据name推断spatial-free | `render.cpp` fixedargs；hostpropertytables | [Image header](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h)，[release](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/release-notes.md) |
| I19 | Image colour style None/Basic/Core/Full/OCIO，optional1.5 | 未实现native negotiation；产品scene-linear假定不等于标准config names/OCIO metadata。GetOutputColourspace、clippreferred/actual、viewportview/looks不支持 | `Host`; `OpenFxProcessorFactory` SCENE_LINEAR；noColouradapter | [Colour](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxColour.h) |

## 5. 全部 17 参数类型与 Parameter V1

数据类型存在与专业工作流完整性分开：当前插件曲线走唯一 Core evaluator，
没有另建 OFX Bézier evaluator。自带 UI 的插件参数写入也必须进入 canonical
authoring transaction/Undo/save；worker中局部赋值不能替代这个闭环。

| ID | 规范参数种类 / 功能 | 当前 runtime | 代码依据 | Primary / 剩余slice |
| --- | --- | --- | --- | --- |
| P01 | Integer，Double | 实现完整SDK有限numeric范围、常量/曲线/source revision、atomicvalidation；Double语义为Plain/Scale及有限scalar Angle（P14） | `render::numericLayout/applyParameters`; `Integer/DoubleParameter`; `openfx_effect` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h) |
| P02 | Double2D/Double3D | 实现tuple atomic、f64default/min/max/displayrange、canonicalcurvecomponent；空间/time语义不支持 | `NumericTupleParameter`; `decode_native_parameter`; `PropertyValue::DoubleVec*` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05 |
| P03 | Integer2D/Integer3D | 实现tuple integer、拒绝fraction/out-of-range/default错误；integerinterp canonical规则 | 同P02 | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h) |
| P04 | RGB/RGBA | 实现unmanaged f64tuple/default/曲线；swatch不是nativeColour API实现 | 同P02；`DoubleRgb/DoubleColor` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h) |
| P05 | Boolean | 已启用 Boolean/Hold；Windows owned有限闭环已验：九轮同current来源/queries/keys/revision/通知/像素、两次显式关闭、七帧正反seek、五帧共享Graph及非动画曲线Create前status4拒绝。107普通OFX与另206项受影响App控制通过；Choice候选上原两门回归通过，原shared100ms及180s/Job2s不变。旧退出根因、Boolean Graph独立自然关闭、第三方/长期/其它平台仍未验 | `BooleanParameter`; hostflags；descriptionvalidation；同Child事件等待 | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S06 |
| P06 | Choice | 部分：已启用固定ordinal域/Hold，keys必须精确0..N−1；同count重复labels/rank只是呈现。非动画曲线status4、描述/Create/Render域漂移status7四原生拒绝门已验，count A→B→A也使session失效。正向九轮查询/通知/像素、首次关闭及七帧seek已过，但第二关闭DeadlineExceeded、Graph未到，完整闭环未验。原常量完整门已验；动态options刷新仍属P21 | `ChoiceParameter`; 既有Curve.schema冻结N/private op6；session sticky atomic fault；Create/统一发布前拒绝 | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S06 |
| P07 | StrChoice，1.5 | 常量实现UTF-8stablekey/ranklabel/default；HostsupportsStrChoice=true但animation=false | `StrChoiceParameter`; `PropertyEnumOption` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S06 |
| P08 | String single/multiline/file/directory | 实现常量与typedresourcecontract；path存在/新文件/UTF-8规则bounded、pack/Undo/save由资源事务资格验收；StringAnimation=false | `StringParameter`; `OpenFxStringMode`; `ParameterResourceContract` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h) |
| P09 | String label / rich-text | 原始description可读取mode；canonical注册明确拒绝，未提供readonlylabel/RTFcontrol | `build_definition` Stringguard | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05 |
| P10 | Custom | 拒绝/未实现；customvalue与interpolation callback不能用普通String假装 | `newParam` fallsnull；`describe` unsupported | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S07 |
| P11 | Bytes，1.5 | **拒绝/未实现**；no binary-valueproperty/IPC/instance/authorpersist | `newParam`; descriptorfactory；`describe` unsupported | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S07 |
| P12 | Group/Page | nativeinstances创建但productdescription略过，无page/parentlayout；MaxPages=0 | `newParam`; `describe` typecontinue | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05 |
| P13 | PushButton | nativeinstance创建但productdescription略过，无用户command/InstanceChangedgesture | 同P12 | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05+S07 |
| P14 | Plain/Scale/Angle/Time/AbsoluteTime/X/Y/XY/absolute空间/defaultCoordinateSystem | Plain/Scale、scalar Angle 保原资格；XY/XYAbsolute Double2 支持 Canonical/Normalised 默认坐标，原生先解析所属项目窗口再校验范围/Create。Effects 共享解析器实例化完整 canonical schema，原事务/曲线/Reset/公开保存重开已验；显式 Sequence 几何变更只迁移默认值与绑定，保留作者值/曲线/身份并原子拒绝不支持迁移。真实 Constant 与 CheckerBoard 有有限 Windows CPU 实际资格 | `project_properties.rs`; `openfx_effect.rs`; `parameter_instance.cpp`; ADR0013/兼容矩阵 | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h); 其余 Time/单轴空间/复合 Angle、动态描述符与扩展 RoD/ROI/GPU 仍有缺口；不授 full standard/跨平台/RT，原 Angle/F06 关闭失败保持开放 |
| P15 | paramDefine/GetHandle/SetGetPropertySet/GetPropertySet | 实现admittedtypes；descriptor staticproperties与instance指针生命周期 | vendor`OfxParameterSuiteV1`；product`newParam` | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html) |
| P16 | paramGetValue/GetValueAtTime | 实现常量与canonical numericcurves；FrameSample off-time无authority明确拒绝；numeric keys/tuple一致 | `parameter_instance.cpp::queryParameter`; `curves.rs::CurveContext` | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html) |
| P17 | paramGetDerivative/GetIntegral | 实现admitted Double/tuple canonical evaluator；unsupportedcalculus显式状态，nonfinite/overflow请求不改输出；Discrete不是浮点calculus | `curves`; `NumericTupleState`; nativecalculusfixtures | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html) |
| P18 | paramGetNumKeys/GetKeyTime/GetKeyIndex | 实现canonical sparsecurvekeys/time/indexdirection；constant0keys；FrameSample无suppliedcurve拒绝 | `ParameterQueryInstance`; `curves.rs` | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html) |
| P19 | paramSetValue/SetValueAtTime/DeleteKey/DeleteAllKeys/Copy/EditBegin/EditEnd | 部分：String复用readonly-GUI检查；普通CPU Render作用域拒写，Double/String两setter两帧已运行，Create及宿主A/B正控可用。其它setter共享检查仅源码审查；局部constantset仍noauthorresponse，完整action/analysis准入、C2桥及animatingwrite/delete/copy/edit缺失，不形成Undo/save | `parameter_instance.*`; `FrameState::permitsParameterWrite`; `RenderParameterWriteBlock`; vendor`paramChangedByPlugin`; `FilterEffectInstance::editBegin/End` | [Setting parameters](https://openfx.readthedocs.io/en/latest/Reference/ofxParameter.html#setting-parameters)；[Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html)；S07 |
| P20 | Animates/IsAnimating/IsAutoKeying/Persistant/EvaluateOnChange/CacheInvalidation/CanUndo | 部分：IsAnimating由既有参数来源推导，九轮functional与四原回归通过，新门shutdown仍HOLD；author supports admittednumericcurves/cachepolicyUncacheable；完整autokey/nativeprivatepersist/dirtymetadata响应无资格 | host/parameterpropertytables；`render.cpp`；`openfx_effect` | [Parameter properties](https://openfx.readthedocs.io/en/main/Reference/ofxParameter.html)；S06–S07 |
| P21 | Secret/enabled/label/hint/choiceoptions/ranges/script/parent/parameterinteract dynamicmetadata | 部分：注册时snapshot，secret/disabled不提供editcontrol；postInstanceChanged动态值未回refresh，可能staleUI/default | `describe`; `build_definition`; no metadataresponse | [Parameter properties](https://openfx.readthedocs.io/en/main/Reference/ofxParameter.html)；S05 |
| P22 | Parametric曲线（独立于timeline），optional | 未实现，不用timelinekeyframes代替x-function；需要曲线default/domain/controlpoint/animation及sharedauthorowner | no suite；no type | [Parametric](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxParametricParam_8h.html)；S08 |

## 6. 交互与原生 GPU 全 families

| ID | 能力 / 条件必需语义 | 当前 | 必需verticalslice / primary |
| --- | --- | --- | --- |
| X01 | OverlayInteractV1：GL draw、focus、pen、keys、viewport pixelScale/project/PAR | 未接入；hostoverlays=false | S13；[Interact](https://openfx.readthedocs.io/en/main/Reference/ofxInteracts.html) |
| X02 | OverlayInteractV2 + DrawSuite：drawcontext、standardcolors、logicalpixels、DPI/UTF-8、focus/pen/key lifecycle | 未接入；header不是rendereradapter | S13优先新Draw路线；[Draw](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxDrawSuite.h) |
| X03 | CustomParameterInteractV1与尺寸协商 | 未接入；customInteract=false | S13+S05，nativegesture必须进Undo/save；[Interact](https://openfx.readthedocs.io/en/main/Reference/ofxInteracts.html) |
| X04 | Plugin dialog：UI线程modal ownership、request/paint、no concurrentactions | 未接入；suite=null | S13，复用产品modal Tab/windowowner；[Dialog](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxDialog_8h.html) |
| X05 | OpenGL：texturehandles、clipLoad/Free/Flush、contextattach/detach/needed/enabled | 未实现；GPU-onlyplugin拒绝CPU路线 | S14，每平台实际queue/storage/同步；[GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| X06 | CUDA：devptr/rowstride、streamenabled、device/allocator、completion/instanceinheritance | 未实现 | S14，不得加每帧deviceSynchronize掩盖lifetime；[GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| X07 | Metal：buffer/commandqueue、device、CPUownership/completion | 未实现；wgpu Metalbackend不等于可导出OFX对象 | S14，必须macOS真nativefixture/texture-transfer验收；[GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| X08 | OpenCL：buffer/image区别、queue、programcompile、completion | 未实现；buffer和image的ABI不可混用 | S14，独立buffer+image+ProgramSuitefixture；[GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| X09 | 私有vendor suites/contexts/components/actions | unknownsuite=null；不包含于通用1.5.1义务 | 需要清楚公开的privatecontract再独立接入；无manufacturer名称分支。[Extension model](https://openfx.readthedocs.io/en/main/Reference/ofxStructure.html) |

## 7. 产品安装、项目、颜色、性能

| ID | 产品工作流 | 实际 / 待验收 | 代码与证据 |
| --- | --- | --- | --- |
| W01 | user-selected `.ofx.bundle`/directbinary，平台布局 | 实现Win64/Win32/Linux-x86[-64]/MacOS；实际Windows验证，macOS/其他平台本轮未receipt；ARM64EC未实现 | `resolve_openfx_binary`; `vendor ofxhBinary.cpp`; [官方 packaging](https://openfx.readthedocs.io/en/main/Reference/ofxPackaging.html) |
| W02 | 系统插件路径/OFX_PLUGIN_PATH自动扫描、@目录skip、pluginicons/XML | 产品当前是userselectedinstall和savedlistrestore，无系统catalogscan/icon/XML读取；packaging搜索是推荐方案，不应假造mandatory扫描义务 | `app/src/app/openfx_catalog.rs`; `resolve_openfx_binary`；[Packaging](https://openfx.readthedocs.io/en/main/Reference/ofxPackaging.html) |
| W03 | restored definitions/author参数/resource事务、Undo、save/reopen、package | 已有typedresourceauthor路径和ownedlibrarypack；当前做真实资格。插件binary不自动打进项目；externalbundle/dylib/license/resources依赖不等于已hermeticproject | `openfx_effect`; `app/openfx_catalog`; 资源事务资格；[Resources](https://openfx.readthedocs.io/en/main/Reference/ofxPackaging.html) |
| W04 | CPU shared compiled graph/Preview/Export semantics | 零输入节点使用既有placement画布与原Effects bank，按自身声明选择staging，保留实际输出alpha、时间/采样、真实Source分支及原typed合成/资源准入。普通Renderer覆盖Solid/Media/Adjustment/Transition/混合及根/嵌套路由；真实Constant root Solid Full Preview与Export工作帧/语义、Undo/Redo及保存重开通过 | `openfx_effect.rs`; `execution.rs::source_staging_format`; `timeline_composite.rs`; ADR0015；兼容矩阵；原生其它placement/动画/Reduced/视频编码/设备/RT/性能/平台及专用Generator Clip仍未验收；显式Catalog有限资格见兼容矩阵；不是新的Timeline source类型 |
| W05 | scene-linearcolour/alpha parity和真实LUT输出 | 有限 Float typed association 与 Normal Over/opaque delivery 已入源码，stock OCIO consumer有 canonical finite/alpha/extent admission；r76 GPU GREEN仅证明列明硬件闭环。仍无 native OFX Colour style/config/clip negotiation，不能据 LUT 或 Float 源码宣称全部 OFX 色彩兼容 | `ColorFrameAlpha`; `EffectFloatFrame`; stock OCIO input/working/output seams；S08 native Colour 仍未实现 |
| W06 | Resident steady-frame latency | 当前4f3ffda2实际release/default features四组仍FAIL：1080p/UHD×30/60，32 warmup排除、600 samples/组、原80%阶段预算，每组600次超预算。p50为35.48/121.18/35.17/121.17ms；较同条件前一正式70aaf7cd下降约16–24%，合计较3fd7463c下降约44–48%。逐像素、同Worker、关闭及回收检查无错，六项原生回归通过。旧UHD60的1248.47ms长尾未复现亦未解释，不授deadline/长时上界资格 | `official_basic_production_resident_steady_frame_performance_gate`; collector；兼容矩阵当前表。旧MinGW/validation基线不作匹配比较；不是成熟宿主、纯DSP或RT资格 |
| W07 | native完整性能预算/RSS/插件SMP/4K重型效果/multipleinstances | 部分 known-host ledger：Filter 两 shared rasters/Generator 单 Output raster，加 pre-DSP returned Vec、8 MiB reserved callback stack/guard/fixed mutex state、48-byte namespace POD。native active/retained 分别为 Filter 三/两帧、Generator 两/一帧加 fixed owners；executor staging 另计。NoImage 返回 capacity 不得超过本次已计费 staging，post-call 检查在 publish/retain 前拒绝，不能阻止任意 Rust provider 事先超量分配；其它临时副本须计入 factory active peak。有限 Generator Direct/graph 通过不重授UHD常驻/性能或完整内存资格。UID std::string/SDK property copies、其他 SDK/OS/CRT/plugin heap未完整计量，不能称 whole RSS cap | `OpenFxFilterSession::storage_for_context`; `OpenFxProcessorFactory::storage`; `NativeOutputAllocation::acquire`; `FloatProcessorResidency`; `ThreadSuite::residentBytes`; `FrameState` |
| W08 | 身份与像素准入热点 | 身份前后约1ms保持原检查；native CRT逐分量调用已由等价位分类替换，原75776位模式控制资格保留。真实UHD128次检查点仅约0.17ms；普通输出循环的逐像素Rust谓词调用经最终机器码确认。4f3ffda2仅为原谓词增加内联提示，fresh正常parent不再保留该独立谓词/直接调用；361项Effects release单测及六native门通过，普通/插桩差距收敛。全部数值、alpha、错误、检查点、所有权不变。Native输出校验先于End，parent仍是独立发布边界 | `float_frame.rs::validate_sample`；`frame_state.h::finiteFloatRgba`；既有两个诊断example及兼容矩阵。有限优化已收口；deadline/旧长尾仍开放，不增identity cache、不删扫描 |
| W09 | 用户通知/故障解释 | 当前 Preview scope 的 notice/persistent/clear、Log 及 Describe-only/Catalog 冷路径通知已接入；当前结果消费一次，取消结果不发布。导航、Dialog/Interact 和完整交互资格仍待 | `app/preview_processor_feedback`; `app/openfx_catalog`; B09–B15 |
| W10 | hostSDK版权/第三方effectdistribution | vendor BSD-3-Clause源码与license已随产品，未分发第三方effectbinary。接入标准SDK不代表取得vendor插件或其assets/license授权；常规loader不bypasslicensing | `native/vendor/LICENSE.md`, `README.md`; [官方license](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/LICENSE.md) |

## 8. 剩余的最小闭环切片与验收

S01/S03/S04 已应用源码，但其新产物与全产品资格必须继续完成，不能仍当未应用草稿。
S02 已补齐 Describe-only/Catalog 有界通知；后续推进产品导航和具体交互/平台资格，
不要重复实现已修的 suite dispatch。扩大 context/布局/UI/原生 GPU 前，每个新切片都要
测试当前实现真实 RED、应用修复后真实 GREEN，再跑 product worker 与共享图；
明示hostcapability仅在闭环通过后开放。扩大能力前应先完成独立闭环，再更新 capability 声明。

| Slice | 闭环目标 | 必需代码authority | 独立验收（不能mirror实现） | 性能约束 |
| --- | --- | --- | --- | --- |
| S01 | 已入源码：actual image FieldNone 与 parent-owned namespace/generation；r103 cross-worker private analysis 与 public namespace-cache 断言通过，整批 source/artifact lease 稳定 | `FrameState`+typed parent owner+`FrameImage`；Source/Output separate，action-scope handles | 插件按SourceID缓存独立analysis，改变raster/seek/newsession不错误复用；sameimage repeatedfetch合法稳定；真实imageField必须None，不能只查render/clipprop | 无额外rasterhash/clone；generation allocation次数明确 |
| S02 | dispatch/persistent/Progress/Log 与有界 Describe-only 通知已实现；剩余产品导航及交互资格 | 复用现有 AuxSuite、typed feedback 与 Catalog owner；Describe 不伪造 Render invocation | 本轮不 Render 即交付6条有序UTF-8/4条Feed、容量/失败前缀、取消不发布、单生命周期均通过；其它交互场景分别验收 | bounded UTF-8/response/queue；不是 whole-heap 或 realtime 资格 |
| S03 | 已入源码：真实 spawned 1-CPU callback/mutex 生命周期；r103 原生 thread/mutex/completion/capacity 与公开产品 thread/mutex/completion/cancel 控制通过，整批 source/artifact lease 稳定。若扩大 SMP，须新的 domain grant/scheduling/resource policy | `ThreadSuite` sole owner；递归/并发拒绝，accepted job own completion | recursionErrExists；每callbackindex覆盖一次+join；initiallockcount/trylock/otherthreadblocking/unlock；callbackspawnstatus真；shutdown无悬空handle | boundedthreads、nestedbudget；同机器1CPU vsavailableCPU p99；不与NLEworkers过度订阅 |
| S04 | 已入源码：finite typed association / exact dyadic Normal Over / cached/nested/opaque route；r103 raw/cache/nested/opaque native/output 断言通过；全图与 release 性能仍需分别验收，coverage consumer仍有明确限制 | Core/Effects sole typed frame 与 exact arithmetic；actual Source/output association；compiled binding/admission/cache header | currentAPIraw RED；zeroαemission/negativeRGB/signed/>1/subnormal/MAX；samevalidworker不退役；rawcachebits、nestedf64oracle、OCIOopaqueRGBA8；GPUcoverageconsumer typedrefusal无DSP重放 | existingstaging/outputclone预算；FiniteStraightCoverage=可表示Float32转换，不承诺bitlossless；无epsilon/clamp |
| S05 | 参数unit/spatialdefault/group/page/button/label、dynamicmetadata | Coreauthor parametercontracts、OFXmetadataresponse、appinspectorcontrols | PAR/projectoffset非零/canonicalvsnormalizeddefault正确；dynamicsecret/enabled/options不丢authorvalue；Button一次gesture/Undo/save；RTL/keyboardtab可用 | coldmetadata更新；不每帧重注册definition或构建整UI |
| S06 | discrete/Stringanimation完整标准语义 + hostschema truthful | Corecanonicalcurves/value/queryschema；OFXparametercallback；admissioncaps与Hostprops | Bool/Choice/StrChoice/Stringholdkeys、off-timequery、seek、pre/post-range、keysindex；sameframe revision正确通知；write-only/read-only全部suiteN/reset | no secondevaluator；稀疏keys计量，IPCbounded；不能每帧serialize全历史 |
| S07 | nativeorigin author edits / opaqueBytes/Custom / SyncPrivateData持久化 | canonicaleditortransaction、revisioncheckedownedresponse、boundedbinaryschema；SDKrecursivechange、savebarrier | nativecontrolchange→一次Undo/Redo→save/reopen相同pixels/state；hiddenprivateBytesroundtrip；partialcallback失败原子；staleedit不覆写newauthorstate；noPTRpersist | coldstatecopybounded；framework不等save/UI；snapshotimmutable不replayDSP |
| S08 | released1.5 Colour API + latermanagedRGB参数（分开能力） | renderer soleColor pipeline、config/style/clippreferredactualmetadata、alphaadmission；parammanagedfutureCore | officialColourSpace独立inputs/outputconfig、HDR/negativeAP0/sRGB、Rawmask noCM、generatoroutputresolution、stockOCIOoracle；不推断workingprimaries | cacheprocessor/configsignature；融合合法conversion；测colourtransformcopycounts |
| S09 | 限定Generator及既有Timeline效果闭环已验收；完整context未验 | Output-only、所属项目默认值/迁移、有限RoD与原bank临时准入保持既有资格。新增实际Constant root Solid在Full16x16/24fps完成Preview/Export工作帧与语义、历史及公开保存重开；严格区别staging和实际关联alpha | 具体来源/普通及原生门限/限制统一见兼容矩阵与ADR0015，不积压原始日志 | 专用Generator Clip、其它原生placement/动画/Reduced、上游ROI/temporal/GPU/性能/whole-heap/其它平台仍有缺口；F06/GL开放 |
| S10 | namedinputs + General/Transition/Paint，layoutintersection | compilednamedsourcebinding、asset/mask/timelineowner、nativeprotocolactualclips；GetTimeDomain 仅 General | 两输入不同签名/offset/PAR；requiredvsoptional；exacttransition0/.5/1；AlphaBrush/size分离；Source/OutputimageID独立 | onlydemandedinputmaterialization；noextrafullclone；keepatomicoutput |
| S11 | Filter时域有限切片已验收；Retimer/requiredsequential仍开放 | 原owner/timeGrid、framesneeded ranges、bounded input service、bank/Control与单一Timeline source map | 当前条件见ADR0016/兼容矩阵：真实off-time/subframe/首尾空缺、seek取消恢复、PreviewExport/历史及倒放PTS边界；不得重放DSP来fallback | 更多输入/插件、Retimer/ordered native contract、长期调度、heap/latency/performance仍未完成 |
| S12 | Byte/Short/Half/RGB/Alpha + multires/PAR/depth/fields/ROIscale | Coretypedpixellayout/color/alpha、compiledROI/extent、nativeactualstorage | independentquantization/halfHDR/negative/alpha/rowstride；nonsquarePAR ROI enclosure；fieldtimes；imagehandles/temporarylifetime | layoutconvertplannedfusion；tilevsfullpixelscorrectness与p99、allocation计量 |
| S13 | DrawSuiteOverlayV2/legacyInteract/customUI/Dialog专业交互 | appUIowner、displaycontextcommandtransport、canonicalgesturetransaction | pen/keys/focus/releaseoffwidget、DPI/resize/projectPAR/viewport、Tabmodaltrap、drawtextUnicode、Undo/save/reopen；无GL依赖Drawfirst，legacyGL另能力 | boundedcommands/UIqueue；render无UIroundtrip阻塞 |
| S14 | 原生GPU全API按平台独立资格 | renderer device/queue/storage/sync authority + isolatednativeinterop，actualGpuActions | GL/CUDA/Metal/OpenCL各真实fixture；buffer/image/stride、queue completion、contextattach/detach、CPU fallbackrequested、errorretry、Preview/exportequal | 不每帧全device同步/CPUreadback；真实nativebufferzero-copy证据、copycount和p99预算 |
| S15 | catalog/platform/nativebindingresources完整产品接入 | appcatalog + storageownedverifiedbundlebinding、platformloader | commonpaths/OFX_PLUGIN_PATH/@skip/icons、UTF-8/MacOSbundledeps、changedbinary/resourcesrebind、projectrestoremissingdependency可操作 | measuredhash成本后优化；loadedmoduleidentity不可与mutable外部path混同 |

**验收结论**：运行资格限 CPU Filter 的既有工作流，以及上述 Windows owned
Generator Direct/source-free graph 的固定画布、当前帧、Float RGBA 子集。
Generator context 整体仍有限且未完整验收；既有Solid Clip效果的Timeline及显式Catalog安装/恢复有限资格见兼容矩阵，不授专用Generator Clip或完整源物化。
剩余部分包含已开放 suite 的 correctness
缺口，以及可合法协商拒绝但产品目标要求接通的能力。任何“所有符合规范插件
完美兼容、最新标准所有feature、性能通过”表述，在上述独立执行/平台/性能
收据完成前都没有依据。


## 9. 当前资格边界（root 执行收据）

| 已发生证据 | 能证明的范围 | 不能推出的结论 |
| --- | --- | --- |
| 当前4f3ffda2 release public Basic四组仍600/600超阶段预算，像素/生命周期通过；六native及361项Effects单测通过 | 有限inline优化验收闭合：相同MSVC条件下较前一正式基线p50下降约16–24%，全部准入/所有权规则保留；原性能预算仍未达到，见W06/W08及兼容矩阵 | 旧UHD60单次1248.47ms最大值未归因；不授成熟宿主比较、deadline、GPU/RT、广泛第三方或其它平台资格 |
| r72 旧 metadata/thread 独立 native 行为 RED | fixture到达实际 native action，UID/field/mutex/thread 原语义缺口存在 | 不算后续修复 GREEN |
| r76 RTX3050 / DX12 实际 GPU finite/admission/overflow/recovery GREEN | 该硬件实际 GPU 上传、拒绝、健康资源释放/恢复闭环 | 不等于 OFX native GPU suite支持，亦非跨平台证明 |
| r76 旧 namespace 跨子进程 private analysis cache stale RGB 实际 RED | process-local namespace可导致合规缓存错误重用 | r90 cross-child/public-cache 断言通过；不代表整批 artifact lease 通过 |
| r88 全 workspace all-target/all-feature strict gate GREEN | 已应用源码和全 test targets可编译，lint无warning | 不替代 ignored native fixture、product工作流或 release性能验收 |
| r90 native 11 项与 public 3 项控制通过，nested/output test 失败 | 原生 image/namespace/thread/mutex/resource 控制与 raw/cache 前置断言到达真实执行；嵌套入口错误期待 StraightCoverage | 整批 source 稳定，但 dev/test 产物替换导致 artifact lease 失败；不能计为整批资格通过 |
| r102 全 workspace all-target/all-feature strict gate GREEN | actual-alpha admission 与所有当前 test targets 可编译，fmt/lint无warning | 不替代运行、原生 GUI、release 性能或跨平台资格 |
| r103 修复后的统一 test-profile 资格 GREEN：20 exact gates，0 failure，0 ignored | 一次匹配构建，product/lib/integration/两种插件产物 read-sharing 锁定；source/artifact SHA 全程稳定；nested/opaque 实际旧失败已修正 | 仅命名工作流；fresh release deadline、冷启动、完整 NLE、厂商/平台矩阵仍待验收 |
| 2026-10-17，`2eb160ca` 固定 canvas 输出控制，部分验证 | Windows 当前 test-profile Product/owned SessionProbe，StraightFloat 3×5/PAR2：原门三帧 direct 像素通过后关闭超时；单次既有 close 观测下 direct 与 retained Graph 全门通过。原 unfetched 拒绝门另行通过，均保留原100ms及180s/2s回收规则 | 正常关闭 RED 未解决，完整切片暂缓验收；不是退出修复、可变 RoD、tiles/multires、第三方、GPU、跨平台或性能资格 |

以上序号指 root 本轮实际资格收据。r103 已完成；后续仍应按真实产物和运行结果更新本节，
失败要记录 oracle/错误来源；不能把构建、runner参数或0test失败误记行为 RED。

## 10. 维护与支持声明

每次更新先核对当前 released tag，再将 NEXT 与 vendor 扩展独立列出。只有
完整调用链、独立效果 oracle、对象生命周期和消费者契约闭环通过后，才将
相应行从“部分”“拒绝”“未接入”改为“实现”。性能结论必须限定平台、机器、
构建 profile、帧尺寸、插件与 declared budget，记录 warmup、p50/p95/p99/max。

线程套件的 NumCPUs 表示 host 实际授予的并发计算数，可小于机器 CPU 数；
真实互斥量仍须保护插件自建线程。将 suite 表中的函数指针暴露给插件，就承担
该函数的标准语义。插件内部 SMP 与 host 并发 Render、tile scheduling 是三种
不同能力，不能用其中一种的测试替代其余两种。
[线程套件已发布契约](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxMultiThread.h)，
[线程与递归安全](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html)。

依赖未开放 context、format、temporal Output/Generator、ordered scheduling、interact 或 GPU suite 的插件不能执行，
即使其二进制完全符合 OpenFX 标准。因此，本矩阵不构成“全部合规插件兼容”
或“最新标准全部 feature 支持”的声明。
