# OpenFX 标准能力与实际支持矩阵

规范版本审计日期：2026-10-08；2026-10-17 增量验证 Host 属性和参数准入，
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

2026-10-17 修复前 Product 的新增压力门仍有缺口：4096 总参数用例在
resident `shutdown()` 超时，后续 Graph 未执行；单次观测中 inner 已于约53.77ms
返回，父侧约101.07ms仍无 OS 退出观察。4097 拒绝用例另有 libtest 通过但
Job 首次计数非零的失败，未记录关联进程身份。两项根因仍开放，原预算与断言保留。

Describe 请求/响应必须 schema2；保留原隐藏入口以使旧 worker 明确拒绝新请求。
通知属于已核验 binary/请求 class 的整次元数据请求，不冒充发出通知的 class 或 Render invocation。
单请求最多32条，文本4096/id256 UTF-8字节；丢失、Fatal 均拒定义并保留可信前缀。
Catalog 每 bundle 最多256条、text+id合计1 MiB，溢出丢弃整批准备结果；结果槽1，
取消交接复用原 owner。这些是冷路径通知边界，不是整堆内存或 realtime 保证。
每行只覆盖声明的上下文、格式、时间与消费者；头文件、enum、upstream wrapper
和测试名字都不能单独证明运行支持。

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
| B06 | Property V1 全部 typed scalar/N set/get、reset/dimension，必需 | 已实现，部分验证：七项 Host 身份/私有指针属性仅对插件只读，内部构造仍可赋值；MaxParameters=4096 与完整 descriptor 准入共用常量。上述六门验证拒写、合法 label 更新、Group/Page 计数及完整数值元数据；完整 Property V1 与4096实例退休资格仍待 | `vendor/host/src/ofxhPropertySuite.cpp`; `ofxhHost.cpp::hostStuffs`; `Host::kMaxParameters`; `tests/openfx_host.rs` | [Property](https://openfx.readthedocs.io/en/main/Reference/suites/ofxPropertySuiteReference.html) |
| B07 | Memory V1 alloc/free，必需 | 实现 malloc/free；null/adversarial ABI与内存不足行为全面资格未完成；不纳入父图预算 | `vendor/host/src/ofxhHost.cpp::Memory` | [Memory](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMemory_8h.html) |
| B08 | imageMemory alloc/free/lock/unlock，必需 | 实现，bootstrap memory 独立 fixture 已验证 Host 所有权；nested lock/生命周期/RSS ceiling 尚待完整性能验收 | `vendor/host/src/ofxhMemory.cpp`; `ofxhImageEffect.cpp`; `OwnedImageEffectCache` | [Image suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxImageEffectSuiteReference.html) |
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
| A04 | Begin/EndInstanceEdit，UI 条件必需 | 未接入，upstream wrapper 不被 product 调用；无 native gesture author owner | `vendor Instance::beginInstanceEditAction`; resident `Request` | [Core actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A05 | SyncPrivateData，保存条件必需 | 未接入，不能保存插件 private state 或 native-origin参数写入；只能保存产品已经拥有的 canonical 参数与 resources | `vendor Instance::syncPrivateDataAction`; resident `Request`; `app/openfx_catalog.rs` | [Sync action](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A06 | PurgeCaches，memory-pressure 条件必需 | 未接入，没有 plugin cache 压力/内存错误 retry 请求 | `vendor Instance::purgeCachesAction`; resident `Request` | [Purge](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A07 | BeginSequenceRender/Render/EndSequenceRender，必需 | 实现独立帧：一帧一 begin/end、frameStep=1、scale=1、FieldNone；Render 必须 OK、取过 Output、image handles 完整释放、完整有限 staging、End 成功后 publish | `render.cpp::render`; `resident.rs::render` | [Render actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A08 | 必须顺序 / optional sequential，协商 | Required(1) 明确拒绝；0/optional(2) 按独立帧执行。无长序列 begin/end、连续 state、ordered seek/reset/abort/restart 闭环 | `admitsIndependentFrames`; `render.cpp` | [Thread safety](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html) |
| A09 | GetClipPreferences，必需 | 实现：Float/RGBA/PAR/真实 Source association 与协商 Output association 同时 checked。有限 signed/>1 alpha 和 PM zero-alpha emission 保留 typed 原始信号；共享图的具体 coverage 消费者仍需显式准入，见 I05–I07 | `render.cpp::render`; `FilterClipInstance::getPremult`; `resident.rs::materialize_native_output` | [Clip preferences](https://openfx.readthedocs.io/en/main/Reference/ofxClipPreferences.html) |
| A10 | GetRegionOfDefinition，image-domain 条件必需 | 未接入 effect RoD；Source clip RoD 全 canvas，output 被 canvas clip。没有接受 expanded/infinite output 的 evidence | `image_clip.cpp::getRegionOfDefinition`; product render no effect RoD dispatch | [Image actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A11 | GetRegionsOfInterest，ROI 条件必需 | 部分：SDK wrapper 会调用或按非 tiles fallback；结果不用于 source scheduling/alloc。host full-frame/1x 路线 | `render.cpp` map rois；`vendor getRegionOfInterestAction` | [Coordinates](https://openfx.readthedocs.io/en/main/Reference/ofxCoordSystem.html) |
| A12 | GetFramesNeeded，temporal 条件必需 | 拒绝/未接入：temporal descriptor 拒绝，getImage 只准 supplied current time | `admitsIndependentFrames`; `FilterClipInstance::getImage` | [Frames](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A13 | IsIdentity，可选 optimization | 未接入；完整 Render 对已准入普通效果仍可正确。未来 bypass 必须检查 source/time/parameter/alpha/colour exact binding | `render.cpp` 无 identity dispatcher | [Identity](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A14 | GetTimeDomain，Generator/General 条件必需 | 未接入；当前 Filter range 来自 owner，不能替代新增 context output domain | `FilterEffectInstance::timeLineGetBounds`; Filter-only selection | [Time domain](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A15 | GetOutputColourspace，colour 条件必需 1.5 | 未接入，未 expose Colour style/config、clip preferred/actual colourspaces | `render.cpp`; host/descriptor/property tables | [Colour](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxColour.h) |
| A16 | GPUContextAttached/Detached，GPU 条件必需 | 未接入，所有原生 GPU API disabled；wgpu 本身不是 OFX 外部 queue/texture capability | product native host + `app/build.rs` | [GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| A17 | Dialog action / interact separate main lifecycle，条件必需 | 未接入；built-in Viewer anchor/ROI 操作不能证明插件 interactor接通 | product无交互owner；upstream Interact wrappers | [Interact actions](https://openfx.readthedocs.io/en/main/Reference/ofxInteracts.html) |
| A18 | action error/default/abort/memory policy，必需 | 部分：支持合法 ReplyDefault 的部分动作、Render必须OK；native失效/cancel/deadline retire/reap无stale publish。abort恒0；ErrMemory purge/retry、GPU-specific retry未接入 | `accepted`; `render`; `resident ProcessOwner`; `vendor Instance::abort` | [Status/core](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |

## 4. 六个 contexts 与完整 CPU image contract

Contexts 是协商能力，标准并不要求任意 Host 支持六个。
本产品目标“所有三类规范插件”若包含只提供其余 context 的插件，需明确
完成这些 vertical slices，不能把合法 refusal 描述成产品已兼容。
[官方 contexts](https://openfx.readthedocs.io/en/main/Reference/ofxImageEffectContexts.html)。

| ID | Context/能力 | 当前 runtime | 代码 / independent seam | 剩余切片 |
| --- | --- | --- | --- | --- |
| C01 | Filter：Source/Output | 实现：current-frame Float RGBA CPU；optional aux clips disconnected；required extra clip 拒绝 | `Host SupportedContexts`; `describe` clip checks；`FilterClipInstance::getConnected` | 普通 Filter 基础 suite/image 缺口见 S01–S04 |
| C02 | Generator：Output、host defaults/RoD | 拒绝/未实现 | Filter-only `selectFilter`；固定 required input mapping | S09，canonical source-less author node，无 fake Source |
| C03 | Transition：SourceFrom/SourceTo/Output，host Transition | 拒绝/未实现 | 无 named source protocol/context selection | S10，timeline exact phase+two compiled inputs，参数不得作为普通插件控制 |
| C04 | General：Output+arbitrary named inputs | 拒绝/未实现 | optional Filter clips永远 disconnected | S10，typed named source bindings/signatures/resource admission |
| C05 | Paint：Source/Alpha Brush/Output | 拒绝/未实现 | 无 Alpha layout/brush nativeinput | S10后专门 brush/mask clip + size/PAR contract |
| C06 | Retimer：Source/Output，host SourceTime | 拒绝/未实现 | current-frame getImage only | S11，canonical retime + temporal exact source queries |

| ID | Image/timing能力 / 要求性质 | 当前 runtime 状态 | 代码依据 | 官方 primary |
| --- | --- | --- | --- | --- |
| I01 | CPU render capability，optional 1.5.1 | 实现：host true、descriptor默认true、instance继承且可instance-local改变；false于Render前拒绝 | `Host::makeDescriptor`; `FilterEffectInstance`; `admitsCpu` | [GPU/CPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| I02 | Float32 RGBA，协商 | 实现：完整 finite Float32 RGBA，128 MiB 单 raster cap，固定 pair of mappings，底部原点/negative rowBytes；bulk byte copy 后唯一 typed storage validation，Opaque ignored-A 规范化1；更大帧为 admission 限制，r103 raw/copy 与 public associated output 控制通过，整批 source/artifact lease 稳定 | `loadFrame`; `validate_frame`; `FrameImage`; `resident.rs::{copy_native_input,materialize_native_output}` | [Images](https://openfx.readthedocs.io/en/main/Reference/ofxImages.html) |
| I03 | Byte/Short/Half，协商 | 拒绝/未实现；不是“把 Float 改枚举”即可，实际layout、quantization、HDR、colour/alpha映射缺失 | Host SupportedPixelDepths=Float；clipprefs exactcheck | [Pixel depths](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I04 | RGB/Alpha/componentsNone，协商/未连接信息 | 仅RGBA执行；RGB/Alpha不能渲染，disconnected aux getImage=null。None/unmapped disconnected元信息尚待完整fixture | Host SupportedComponents=RGBA；`FilterClipInstance` | [Clip](https://openfx.readthedocs.io/en/main/Reference/ofxImageClip.html) |
| I05 | Source premult/opaque 与 pixels一致，条件必需 | 实现：immutable prepared format 的 actual Straight/PM/Opaque association 传到 Source clip/image，pixels 不被隐式 unassociate；preference 与 supplied format 不符拒绝，不存在裸 Vec 旁路 | `EffectValueFormat`; `OpenFxFilterSession::input_format`; `FrameState`; `FilterClipInstance::getPremult` | [Premult](https://openfx.readthedocs.io/en/main/Reference/ofxImageClip.html) |
| I06 | Output Straight/PM/Opaque，条件必需 | 实现：clip/image 与 Output preference 一致；public output 为 actual StraightFloat/PMFloat/Opaque typed frame，PM 不再强制除回 coverage；Opaque A=1，所有实际输出 lane finite/extent checked。r103 raw output、Source association 与 nested/opaque product 控制通过，整批 source/artifact lease 稳定 | `render.cpp::outputAlpha`; `resident.rs::materialize_native_output`; `EffectFloatFrame::from_pixels_controlled` | [Clip preferences](https://openfx.readthedocs.io/en/main/Reference/ofxClipPreferences.html) |
| I07 | 完整 finite Float PM/RGBA，包括HDR/负色/zeroα emission，条件必需 | 实现限定域：native→Core/Effects→Normal Over→缓存→nested→opaque output 的 finite Float signal 已接实际调用链；r103 原始 cache bits、独立 nested oracle 与真实 opaque 输出闭环通过。PM zero-alpha emission、signed/>1/HDR/negative 不 clamp。非线性 blend/OCIO/GPU coverage consumer 经 explicit format/admission，不能直接消费所有 Float。r76 GPU 独立 overflow/recovery GREEN，其他新载体待验收 | `mondrian-core::ColorFrameAlpha`; `EffectFloatFrame`; `float_frame::over_float_sample`; `float_frame/exact_over.rs`; `heterogeneous_gpu.rs`; render-cache typed header | [Pixels](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h)，[OpenEXR association](https://openexr.com/en/latest/TechnicalIntroduction.html) |
| I08 | PAR/canonical project size/extent/offset/clipRoD，必需 | 实现：canonical x=width*PAR/y=height/offset0；Rust/native按结果是否finite拒绝overflow；imagebounds/renderwindow保持pixel坐标 | `getProjectSize/Extent`; `getRegionOfDefinition`; `loadFrame`; geometry fixtures | [Coordinates](https://openfx.readthedocs.io/en/main/Reference/ofxCoordSystem.html) |
| I09 | 底部原点、negative rowBytes/连续RGBA存储，必需 | 实现并有不对称raster oracle；Source/Output scope handles独立，parent借用mapping只在action内 | `FrameImage`; `ImageAccess`; `imageHandles`; actualBasic fixtures | [Images](https://openfx.readthedocs.io/en/main/Reference/ofxImages.html) |
| I10 | `kOfxImagePropUniqueIdentifier`，必需image metadata | 实现：parent 单次 typed EffectProcessorOwnerId namespace 经 wire4 Prepare/16-byte CABI 初始化，session generation 每次 image authority 改变递增，Source/Output distinct，同 action repeated fetch stable；fresh worker/seek 不重用裸 process-local namespace。r76 原实现跨子进程 stale-analysis 实际 RED；r103 新 namespace 的 native 与 public cache 断言通过，整批 source/artifact lease 稳定 | `frame_state.h::FrameState`; `image_clip.cpp::FrameImage`; `resident.rs::{Prepare,OpenFxFilterSession::prepare}` | [Image header](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I11 | Progressive FieldNone，必需image metadata | 实现：实际 Source/Output Image 的 Field=FieldNone，clip order 与 Render field 亦 None；仅 progressive CPU，不含 interlaced 调度。r103 对应真实 native field 断言通过，整批 source/artifact lease 稳定 | `image_clip.cpp::FrameImage`; `getFieldOrder`; `render.cpp::render` | [Fields](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I12 | Upper/Lower/Both/field extraction、half-frame，协商 | 拒绝/未实现；setableFielding=false，无合法field scheduling/Sourceimageextract | `Host`; fixedRenderFieldNone | [Rendering](https://openfx.readthedocs.io/en/main/Reference/ofxRendering.html) |
| I13 | Multiple resolution/depth/PAR，协商 | host全部false；独立source/outputdimensions/PAR/depth不支持 | `Host`; fixednativeframeextent；clipprefschecks | [Clip preferences](https://openfx.readthedocs.io/en/main/Reference/ofxClipPreferences.html) |
| I14 | Tiles/ROI/expanded或infinite RoD/renderScale，协商 | hostdisabled；1x fullcanvas；optionalBounds可返回largerfullsource，ROI结果未调度；不能claim expanded/tiledcorrectness | `Host`; `getImage`; `render` | [Coordinates](https://openfx.readthedocs.io/en/main/Reference/ofxCoordSystem.html) |
| I15 | Temporal off-time Source access，协商 | 明确拒绝temporal descriptor；合法currentgetImage与finite timeexactmatch；未支持discontinuousframes-needed/boundaryframes | `admitsIndependentFrames`; `getImage` | [Frames/actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I16 | FrameRate/Range/Duration/continuous samples，必需已选上下文 | 实现owner-derivedframe/rate/range/PAR；ContinuousSamples=false、setableFrameRate=false；subframeSource与outputframe-rate改变未支持 | `OpenFxFrameTiming`; `FilterEffectInstance`; `FilterClipInstance` | [Clip](https://openfx.readthedocs.io/en/main/Reference/ofxImageClip.html) |
| I17 | Render thread safety/singleInstance/license约束，条件必需 | serial instance render实现；每worker隔离静态module内存。但应用可能多个worker同plugin，未enforce SingleInstance声明，未qualify厂商external/license/session约束 | `isSingleInstance`未product使用；`OpenFxProcessorFactory` | [Thread safety](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html) |
| I18 | RenderQualityDraft，optional1.4；Thumbnail/NoSpatial，optional1.5.1 | 未plumb product intent；固定false或缺property。普通fullquality route可用；LUT baking不得据name推断spatial-free | `render.cpp` fixedargs；hostpropertytables | [Image header](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h)，[release](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/release-notes.md) |
| I19 | Image colour style None/Basic/Core/Full/OCIO，optional1.5 | 未实现native negotiation；产品scene-linear假定不等于标准config names/OCIO metadata。GetOutputColourspace、clippreferred/actual、viewportview/looks不支持 | `Host`; `OpenFxProcessorFactory` SCENE_LINEAR；noColouradapter | [Colour](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxColour.h) |

## 5. 全部 17 参数类型与 Parameter V1

数据类型存在与专业工作流完整性分开：当前插件曲线走唯一 Core evaluator，
没有另建 OFX Bézier evaluator。自带 UI 的插件参数写入也必须进入 canonical
authoring transaction/Undo/save；worker中局部赋值不能替代这个闭环。

| ID | 规范参数种类 / 功能 | 当前 runtime | 代码依据 | Primary / 剩余slice |
| --- | --- | --- | --- | --- |
| P01 | Integer，Double | 实现完整SDK有限numeric范围、常量/曲线/source revision、atomicvalidation；Double语义仅Plain/Scale | `render::numericLayout/applyParameters`; `Integer/DoubleParameter`; `openfx_effect` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h) |
| P02 | Double2D/Double3D | 实现tuple atomic、f64default/min/max/displayrange、canonicalcurvecomponent；空间/time语义不支持 | `NumericTupleParameter`; `decode_native_parameter`; `PropertyValue::DoubleVec*` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05 |
| P03 | Integer2D/Integer3D | 实现tuple integer、拒绝fraction/out-of-range/default错误；integerinterp canonical规则 | 同P02 | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h) |
| P04 | RGB/RGBA | 实现unmanaged f64tuple/default/曲线；swatch不是nativeColour API实现 | 同P02；`DoubleRgb/DoubleColor` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h) |
| P05 | Boolean | 常量实现；Host BooleanAnimation=false，animating schema拒绝 | `BooleanParameter`; hostflags；descriptionvalidation | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S06 |
| P06 | Choice | 常量实现：numericindex为identity、labels允许重复、rankorder/defaultchecked；ChoiceAnimation=false | `ChoiceParameter`; nativechoiceinfo；`PropertyValue::Enum` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S06 |
| P07 | StrChoice，1.5 | 常量实现UTF-8stablekey/ranklabel/default；HostsupportsStrChoice=true但animation=false | `StrChoiceParameter`; `PropertyEnumOption` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S06 |
| P08 | String single/multiline/file/directory | 实现常量与typedresourcecontract；path存在/新文件/UTF-8规则bounded、pack/Undo/save由资源事务资格验收；StringAnimation=false | `StringParameter`; `OpenFxStringMode`; `ParameterResourceContract` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h) |
| P09 | String label / rich-text | 原始description可读取mode；canonical注册明确拒绝，未提供readonlylabel/RTFcontrol | `build_definition` Stringguard | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05 |
| P10 | Custom | 拒绝/未实现；customvalue与interpolation callback不能用普通String假装 | `newParam` fallsnull；`describe` unsupported | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S07 |
| P11 | Bytes，1.5 | **拒绝/未实现**；no binary-valueproperty/IPC/instance/authorpersist | `newParam`; descriptorfactory；`describe` unsupported | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S07 |
| P12 | Group/Page | nativeinstances创建但productdescription略过，无page/parentlayout；MaxPages=0 | `newParam`; `describe` typecontinue | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05 |
| P13 | PushButton | nativeinstance创建但productdescription略过，无用户command/InstanceChangedgesture | 同P12 | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05+S07 |
| P14 | Plain/Scale/Angle/Time/AbsoluteTime/X/Y/XY/absolute空间/defaultCoordinateSystem | 仅Plain/Scale读取；其余拒绝，默认值normalised/canonical与unit语义未入author schema；不要把spatial double简单Unitless | native`describe`doubleTypeguard；`with_channel_numeric_contracts(Unitless)` | [released Params](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxParam.h)；S05 |
| P15 | paramDefine/GetHandle/SetGetPropertySet/GetPropertySet | 实现admittedtypes；descriptor staticproperties与instance指针生命周期 | vendor`OfxParameterSuiteV1`；product`newParam` | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html) |
| P16 | paramGetValue/GetValueAtTime | 实现常量与canonical numericcurves；FrameSample off-time无authority明确拒绝；numeric keys/tuple一致 | `parameter_instance.cpp::queryParameter`; `curves.rs::CurveContext` | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html) |
| P17 | paramGetDerivative/GetIntegral | 实现admitted Double/tuple canonical evaluator；unsupportedcalculus显式状态，nonfinite/overflow请求不改输出；Discrete不是浮点calculus | `curves`; `NumericTupleState`; nativecalculusfixtures | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html) |
| P18 | paramGetNumKeys/GetKeyTime/GetKeyIndex | 实现canonical sparsecurvekeys/time/indexdirection；constant0keys；FrameSample无suppliedcurve拒绝 | `ParameterQueryInstance`; `curves.rs` | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html) |
| P19 | paramSetValue/SetValueAtTime/DeleteKey/DeleteAllKeys/Copy/EditBegin/EditEnd | 部分：局部constantset存在但noauthorresponse；animatingwrite/delete/copy/edit缺失/unsupported，不形成Undo/save。不能仅SDKsetterOK认为可persist | `parameter_instance.*`; vendor`paramChangedByPlugin`; `FilterEffectInstance::editBegin/End` | [Parameter suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxParametersSuiteReference.html)；S07 |
| P20 | Animates/IsAnimating/IsAutoKeying/Persistant/EvaluateOnChange/CacheInvalidation/CanUndo | 部分：author supports admittednumericcurves/cachepolicyUncacheable；完整autokey/nativeprivatepersist/dirtymetadata响应无资格 | host/parameterpropertytables；`openfx_effect` | [Parameter properties](https://openfx.readthedocs.io/en/main/Reference/ofxParameter.html)；S06–S07 |
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
| W04 | CPU shared compiled graph/Preview/Export semantics | 实现：sole typed CompiledEffectGraph、prepared factory、exclusive resident owner、shared Preview/Export/Nested/cache semantics；actual alpha enters format binding，dynamic association GPU consumer refusal不重放已推进 DSP；仍独立帧/无完整连续 private state | `app/src/openfx_effect.rs`; renderer/exportsolegraph |
| W05 | scene-linearcolour/alpha parity和真实LUT输出 | 有限 Float typed association 与 Normal Over/opaque delivery 已入源码，stock OCIO consumer有 canonical finite/alpha/extent admission；r76 GPU GREEN仅证明列明硬件闭环。仍无 native OFX Colour style/config/clip negotiation，不能据 LUT 或 Float 源码宣称全部 OFX 色彩兼容 | `ColorFrameAlpha`; `EffectFloatFrame`; stock OCIO input/working/output seams；S08 native Colour 仍未实现 |
| W06 | Resident steady-frame latency | 实际 r30 原 Float 前 public production Basic gate 四组均 FAIL：1080p/UHD×30/60，32 warmup排除、600 samples/组，像素 oracle无错误。当前 Float/bulk/new resource source须 fresh release 重测同一预算；不得把源码优化或 strict GREEN 计为性能 PASS | `official_basic_production_resident_steady_frame_performance_gate`; collector；`.scratch/plugin-standard-audit/audio-release-baseline-r30/{1080p30,uhd30,1080p60,uhd60}.json` |
| W07 | native完整性能预算/RSS/插件SMP/4K重型效果/multipleinstances | 部分 known-host ledger：2 shared rasters+pre-DSP one returned Vec+8 MiB reserved callback stack/guard/fixed mutex state+48-byte namespace POD；actual Vec capacity admission已入源码。当前 factory分别传递 active三帧峰值与 retained两帧费用；本轮未据此重授UHD常驻/性能资格。UID std::string/SDK property copies、其他 SDK/OS/CRT/plugin heap未完整计量，不能称 whole RSS cap | `OpenFxFilterSession::storage`; `OpenFxProcessorFactory::storage`; `NativeOutputAllocation::acquire`; `FloatProcessorResidency`; `ThreadSuite::residentBytes`; `FrameState` |
| W08 | 每帧binaryidentityverification开销 | 保留 parent before/after wholebinary SHA 与 child verified binding/CABI identity；bulk优先降低 O(n) byte/alpha运输成本，未削弱 hash。r30旧版本已测慢；immutable module/resources snapshot优化仍未实现，不允许 stat 代 hash 或永久无校验 cache | `resident.rs` SHA calls；`openfx_host.rs::verify_binary`; bulk POD copy/typed admission |
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
| S09 | Generator context、effectRoD/TimeDomain/identity正确基础 | canonicalsource-lessnode、outputformat/projectdomain；nodeactualRoi | 输出不依赖fakeinput；finite/infinite/expandedRoD、projectPAR、identityreturnedsource/time；无source的defaultprefs | boundedcanvas/ROIalloc；identitybypass保留exactbinding |
| S10 | namedinputs + General/Transition/Paint，layoutintersection | compilednamedsourcebinding、asset/mask/timelineowner、nativeprotocolactualclips | 两输入不同签名/offset/PAR；requiredvsoptional；exacttransition0/.5/1；AlphaBrush/size分离；Source/OutputimageID独立 | onlydemandedinputmaterialization；noextrafullclone；keepatomicoutput |
| S11 | temporalaccess/retimer/requiredsequential真实schedule | exactowner/timeGrid/framesneededranges、boundedprefetch、orderednativeinstancecontract | off-time/subframe/edgeframes/discontinuousranges；seek/cancel/resume/exportstate；sameoutputexactindependentoracle；不得重放statefulnative来fallback | boundedqueue/mapping/RSS；latency/progressbudget；longsequencebegin/end真实 |
| S12 | Byte/Short/Half/RGB/Alpha + multires/PAR/depth/fields/ROIscale | Coretypedpixellayout/color/alpha、compiledROI/extent、nativeactualstorage | independentquantization/halfHDR/negative/alpha/rowstride；nonsquarePAR ROI enclosure；fieldtimes；imagehandles/temporarylifetime | layoutconvertplannedfusion；tilevsfullpixelscorrectness与p99、allocation计量 |
| S13 | DrawSuiteOverlayV2/legacyInteract/customUI/Dialog专业交互 | appUIowner、displaycontextcommandtransport、canonicalgesturetransaction | pen/keys/focus/releaseoffwidget、DPI/resize/projectPAR/viewport、Tabmodaltrap、drawtextUnicode、Undo/save/reopen；无GL依赖Drawfirst，legacyGL另能力 | boundedcommands/UIqueue；render无UIroundtrip阻塞 |
| S14 | 原生GPU全API按平台独立资格 | renderer device/queue/storage/sync authority + isolatednativeinterop，actualGpuActions | GL/CUDA/Metal/OpenCL各真实fixture；buffer/image/stride、queue completion、contextattach/detach、CPU fallbackrequested、errorretry、Preview/exportequal | 不每帧全device同步/CPUreadback；真实nativebufferzero-copy证据、copycount和p99预算 |
| S15 | catalog/platform/nativebindingresources完整产品接入 | appcatalog + storageownedverifiedbundlebinding、platformloader | commonpaths/OFX_PLUGIN_PATH/@skip/icons、UTF-8/MacOSbundledeps、changedbinary/resourcesrebind、projectrestoremissingdependency可操作 | measuredhash成本后优化；loadedmoduleidentity不可与mutable外部path混同 |

**验收结论**：现在能真实执行的范围是限定的 CPU Filter、当前帧、Float RGBA
与已接通 canonical author controls。剩余部分包含已开放 suite 的 correctness
缺口，以及可合法协商拒绝但产品目标要求接通的能力。任何“所有符合规范插件
完美兼容、最新标准所有feature、性能通过”表述，在上述独立执行/平台/性能
收据完成前都没有依据。


## 9. 当前资格边界（root 执行收据）

| 已发生证据 | 能证明的范围 | 不能推出的结论 |
| --- | --- | --- |
| r30 实际 release public Basic 四组 gate FAIL，600 samples/组，pixel oracle通过 | Float 前程序正确执行但延迟未达原预算 | 不证明当前新 Float/bulk 更快或已合格 |
| r72 旧 metadata/thread 独立 native 行为 RED | fixture到达实际 native action，UID/field/mutex/thread 原语义缺口存在 | 不算后续修复 GREEN |
| r76 RTX3050 / DX12 实际 GPU finite/admission/overflow/recovery GREEN | 该硬件实际 GPU 上传、拒绝、健康资源释放/恢复闭环 | 不等于 OFX native GPU suite支持，亦非跨平台证明 |
| r76 旧 namespace 跨子进程 private analysis cache stale RGB 实际 RED | process-local namespace可导致合规缓存错误重用 | r90 cross-child/public-cache 断言通过；不代表整批 artifact lease 通过 |
| r88 全 workspace all-target/all-feature strict gate GREEN | 已应用源码和全 test targets可编译，lint无warning | 不替代 ignored native fixture、product工作流或 release性能验收 |
| r90 native 11 项与 public 3 项控制通过，nested/output test 失败 | 原生 image/namespace/thread/mutex/resource 控制与 raw/cache 前置断言到达真实执行；嵌套入口错误期待 StraightCoverage | 整批 source 稳定，但 dev/test 产物替换导致 artifact lease 失败；不能计为整批资格通过 |
| r102 全 workspace all-target/all-feature strict gate GREEN | actual-alpha admission 与所有当前 test targets 可编译，fmt/lint无warning | 不替代运行、原生 GUI、release 性能或跨平台资格 |
| r103 修复后的统一 test-profile 资格 GREEN：20 exact gates，0 failure，0 ignored | 一次匹配构建，product/lib/integration/两种插件产物 read-sharing 锁定；source/artifact SHA 全程稳定；nested/opaque 实际旧失败已修正 | 仅命名工作流；fresh release deadline、冷启动、完整 NLE、厂商/平台矩阵仍待验收 |

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

依赖未开放 context、format、temporal、interact 或 GPU suite 的插件不能执行，
即使其二进制完全符合 OpenFX 标准。因此，本矩阵不构成“全部合规插件兼容”
或“最新标准全部 feature 支持”的声明。
