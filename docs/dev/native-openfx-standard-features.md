# OpenFX 标准能力与实际支持矩阵

源码审计日期：2026-10-08。范围：本仓库实际 native OpenFX host、Rust
adapter、Effects 图与产品接入。此文记录可定位的源码能力及限制，**不是编译、
真实插件执行、跨平台或性能合格的替代证据**。运行验收必须另外记录实际源码、
构建与插件产物哈希、测试命令和结果。

完整有限 Float 颜色 association、实际 image UniqueIdentifier/FieldNone、CPU
线程与互斥量套件闭环目前仅有待审查草稿；尚未应用、编译或运行验收，均不计入
已实现能力。每行的支持范围只覆盖声明的上下文、格式、时间与消费者；头文件、
enum、upstream wrapper 和测试名字都不能单独证明运行支持。

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
| B05 | suites name/version/null，必需 | 部分：实际 expose 的名称/版本表稳定；unknown suite null。Interact/Progress/Timeline/Message/Thread 的指针存在并未补齐产品语义 | `vendor/host/src/ofxhImageEffect.cpp::Host::fetchSuite`; `ofxhHost.cpp::fetchSuite` | [Suite inventory](https://openfx.readthedocs.io/en/main/Reference/suites/ofxSuiteReference.html) |
| B06 | Property V1 全部 typed scalar/N set/get、reset/dimension，必需 | 实现并有 read-only enforcement；host identity 的 upstream spec 将若干只读字段标成 writable，host schema 仍部分不真实；MaxParameters=-1 与 4096 cap 不符 | `vendor/host/src/ofxhPropertySuite.cpp`; `ofxhHost.cpp::hostStuffs`; `host_descriptor.cpp` | [Property](https://openfx.readthedocs.io/en/main/Reference/suites/ofxPropertySuiteReference.html) |
| B07 | Memory V1 alloc/free，必需 | 实现 malloc/free；null/adversarial ABI与内存不足行为全面资格未完成；不纳入父图预算 | `vendor/host/src/ofxhHost.cpp::Memory` | [Memory](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMemory_8h.html) |
| B08 | imageMemory alloc/free/lock/unlock，必需 | 实现，bootstrap memory 独立 fixture 已验证 Host 所有权；nested lock/生命周期/RSS ceiling 尚待完整性能验收 | `vendor/host/src/ofxhMemory.cpp`; `ofxhImageEffect.cpp`; `OwnedImageEffectCache` | [Image suite](https://openfx.readthedocs.io/en/main/Reference/suites/ofxImageEffectSuiteReference.html) |
| B09 | Message V1 log/info/warning/error/fatal，必需 | 部分：stdout 日志存在，未接到产品通知；合法 NULL/descriptor transient handle 在 vendor 实现中不能正确 dispatch，descriptor 被当 Instance，NULL 回 BadHandle | `vendor/ofxhImageEffect.cpp::message`; `host_descriptor.cpp::vmessage`; `effect_instance.cpp::vmessage` | [Message](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMessage_8h.html) |
| B10 | Message V1 question reply，interactive 条件必需 | 未实现交互 question；render worker instance `vmessage` 总回 OK，Host 自身 question 回 No，不能算合法交互问答。非交互 question 本来就是插件调用错误 | 同 B09；Instance interactive=false | [Message](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMessage_8h.html) |
| B11 | Message V2 persistent set/clear，optional exposed | 部分：套件 non-null，但只是即时日志/no-op clear，无持续 error/warning 状态、关联实例或可见清除 | `effect_instance.cpp::setPersistentMessage/clearPersistentMessage` | [Message V2](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMessage_8h.html) |
| B12 | MultiThread V1 multiThread/CPU/index/spawned，必需套件 | 部分：报告一 CPU，同步 callback(0,1)；不拒绝递归调用，未实现 host SMP。不能将宏开关当实际并行支持 | `vendor/ofxhImageEffect.cpp` 非 `OFX_SUPPORTS_MULTITHREAD` 分支；`app/build.rs` | [MultiThread](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMultiThread_8h.html) |
| B13 | MultiThread V1 mutexCreate/Destroy/Lock/UnLock/TryLock，必需套件 | 部分缺语义：handle=0，忽略初始 lockCount，lock 全部 no-op；插件自己生成线程也可能使用这些 mutex，不能据 host callback 单线程证明安全 | 同 B12 | [Mutex](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxMultiThread_8h.html) |
| B14 | Progress V1/V2 start/update/end，optional exposed | 部分：start/end no-op、update 恒继续；owner kill/reap 可取消工作，但 plugin progress/abort polling 没有共享 cancellation 状态 | `effect_instance.cpp::progress*`; `vendor Instance::abort`; `resident.rs` | [Progress](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxProgress_8h.html) |
| B15 | Timeline V1 getTime/getBounds/gotoTime，optional exposed | 部分：读当前 SDK time/range；gotoTime no-op 却 suite 回 OK。产品导航请求没有入 author/navigation owner | `effect_instance.cpp::timeLine*`; `vendor gTimelineSuite` | [Timeline](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxTimeLine_8h.html) |
| B16 | Interact V1 swapBuffers/redraw/getPropertySet，optional | 未接入：wrapper/suite存在；host overlays/customInteract=false，未创建 interact，不能执行真实 draw/input | `Host::fetchSuite`; `Host` descriptor；产品无 interact owner | [Interacts](https://openfx.readthedocs.io/en/main/Reference/ofxInteracts.html) |
| B17 | ParametricParameter V1 全部 curve/control-point API，optional | 拒绝/未实现：宏未启用、suite=null，无 canonical parametric function author schema | `app/build.rs`; `Host::fetchSuite`; `newParam` | [Parametric](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxParametricParam_8h.html) |
| B18 | Dialog V1 requestDialog/notifyRedrawPending，optional | 拒绝/未实现：suite=null、无 UI-thread dialog action dispatcher | `Host::fetchSuite`; product request enums | [Dialog](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxDialog_8h.html) |
| B19 | DrawSuite V1 全 6 functions，optional 1.5 | 拒绝/未实现：get/setColour、width/stipple、draw、UTF-8 text 均未 expose | `Host::fetchSuite`; no Draw adapter | [Draw](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxDrawSuite.h) |
| B20 | OpenGLRender V1 3 functions，optional | 拒绝/未实现：macro off、suite=null，没有 GL texture/context ownership | `app/build.rs`; `Host::fetchSuite` | [GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |
| B21 | OpenCLProgram V1 compileProgram，optional | 拒绝/未实现：suite=null，host queue/program/kernel/device lifetime 未接入 | `Host::fetchSuite` | [GPU](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxGPURender.h) |

## 3. 全动作 / 生命周期

| ID | 动作 / 要求性质 | 当前 runtime 状态 | 代码依据 | 官方 primary |
| --- | --- | --- | --- | --- |
| A01 | Load/Describe/DescribeInContext/Unload，必需 | 实现：selected native SDK 动作链；discovery 只枚举，不冒充完整 Describe | `SelectedPluginBinary`; `selectFilter`; `vendor/ofxhImageEffectAPI.cpp` | [Actions](https://openfx.readthedocs.io/en/main/Reference/ofxImageEffectActions.html) |
| A02 | Create/DestroyInstance，必需 | 实现：Create 前提供 complete author snapshot；Destroy 在 module unload 前。合法 callback image scopes 有显式 borrow guard | `MondrianOpenFxSession::initialize`; `FrameBorrow/ImageAccess`; `NativeSession::drop` | [Core actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A03 | Begin/InstanceChanged/End，条件必需 | 实现当前 author/time/clip facts 通知；value 相等但 curve source revision 改变仍通知。native-origin author edits/state 更新没有回传事务 | `render.cpp::applyParameters`; curves callback；`parameter_instance.*` | [Recursion](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html) |
| A04 | Begin/EndInstanceEdit，UI 条件必需 | 未接入，upstream wrapper 不被 product 调用；无 native gesture author owner | `vendor Instance::beginInstanceEditAction`; resident `Request` | [Core actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A05 | SyncPrivateData，保存条件必需 | 未接入，不能保存插件 private state 或 native-origin参数写入；只能保存产品已经拥有的 canonical 参数与 resources | `vendor Instance::syncPrivateDataAction`; resident `Request`; `app/openfx_catalog.rs` | [Sync action](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A06 | PurgeCaches，memory-pressure 条件必需 | 未接入，没有 plugin cache 压力/内存错误 retry 请求 | `vendor Instance::purgeCachesAction`; resident `Request` | [Purge](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxCore.h) |
| A07 | BeginSequenceRender/Render/EndSequenceRender，必需 | 实现独立帧：一帧一 begin/end、frameStep=1、scale=1、FieldNone；Render 必须 OK、取过 Output、image handles 完整释放、完整有限 staging、End 成功后 publish | `render.cpp::render`; `resident.rs::render` | [Render actions](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| A08 | 必须顺序 / optional sequential，协商 | Required(1) 明确拒绝；0/optional(2) 按独立帧执行。无长序列 begin/end、连续 state、ordered seek/reset/abort/restart 闭环 | `admitsIndependentFrames`; `render.cpp` | [Thread safety](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html) |
| A09 | GetClipPreferences，必需 | 实现：当前 Float/RGBA/PAR/alpha checked negotiation。合法 preferred output extended Float PM 仍不被 shared graph完整表示，见 I06 | `render.cpp::render`; `FilterClipInstance::getPremult`; `resident::normalize_native_output_sample` | [Clip preferences](https://openfx.readthedocs.io/en/main/Reference/ofxClipPreferences.html) |
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
| I02 | Float32 RGBA，协商 | 实现；finite input/output，128 MiB raster cap，stable mapping与底部原点/负 rowBytes。更大帧是产品 admission 限制 | `loadFrame`; `validate_frame`; `FrameImage`; resident mappedbuffers | [Images](https://openfx.readthedocs.io/en/main/Reference/ofxImages.html) |
| I03 | Byte/Short/Half，协商 | 拒绝/未实现；不是“把 Float 改枚举”即可，实际layout、quantization、HDR、colour/alpha映射缺失 | Host SupportedPixelDepths=Float；clipprefs exactcheck | [Pixel depths](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I04 | RGB/Alpha/componentsNone，协商/未连接信息 | 仅RGBA执行；RGB/Alpha不能渲染，disconnected aux getImage=null。None/unmapped disconnected元信息尚待完整fixture | Host SupportedComponents=RGBA；`FilterClipInstance` | [Clip](https://openfx.readthedocs.io/en/main/Reference/ofxImageClip.html) |
| I05 | Source premult/opaque 与 pixels一致，条件必需 | 当前Source永远报Unpremultiplied；public input裸pixels无association。真实PM/opaque input不能准确绑定 | `FilterClipInstance::getPremult`; `OpenFxFloatFrame` | [Premult](https://openfx.readthedocs.io/en/main/Reference/ofxImageClip.html) |
| I06 | Output Straight/PM/Opaque，条件必需 | 部分：clip/image报真实请求；parent归一化PM到Straight，Opaque alpha=1。PM仅α[0,1]、zeroα需RGBzero、division可表示；signed/>1/emission拒绝。RawStraight可携extendedα但图/renderer标签仍coverage，不能算完整全域 | `getOutputPreMultiplication`; NativeSession Result；`normalize_native_output_sample` | [Clip preferences](https://openfx.readthedocs.io/en/main/Reference/ofxClipPreferences.html) |
| I07 | 完整 finite Float PM/RGBA，包括HDR/负色/zeroα emission，条件必需 | **尚未runtime实现**；拒绝有效表示当前被当host unavailable/worker fault。fullFloat草稿未应用不计支持 | current normalizer；完整有限 Float association 设计草稿（尚未应用） | [Pixels](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h)，[OpenEXR association](https://openexr.com/en/latest/TechnicalIntroduction.html) |
| I08 | PAR/canonical project size/extent/offset/clipRoD，必需 | 实现：canonical x=width*PAR/y=height/offset0；Rust/native按结果是否finite拒绝overflow；imagebounds/renderwindow保持pixel坐标 | `getProjectSize/Extent`; `getRegionOfDefinition`; `loadFrame`; geometry fixtures | [Coordinates](https://openfx.readthedocs.io/en/main/Reference/ofxCoordSystem.html) |
| I09 | 底部原点、negative rowBytes/连续RGBA存储，必需 | 实现并有不对称raster oracle；Source/Output scope handles独立，parent借用mapping只在action内 | `FrameImage`; `ImageAccess`; `imageHandles`; actualBasic fixtures | [Images](https://openfx.readthedocs.io/en/main/Reference/ofxImages.html) |
| I10 | `kOfxImagePropUniqueIdentifier`，必需image metadata | **缺失真实语义**：Image(clip) ctor默认空，product没有赋值，所有Source/Output/帧均空字符串；合法image-analysis缓存可能错误命中 | `FrameImage`; `vendor/ofxhClip.cpp::imageBaseStuffs` | [Image header](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
| I11 | Progressive FieldNone，必需image metadata | 部分：Render FieldToRender和clipFieldOrder/defaultOutput为None，但真实Source/Output image `kOfxImagePropField`仍默认空字符串。现canonical fixture未检查imageField | `FrameImage`; `getFieldOrder`; `render`; `vendor imageBaseStuffs` | [Fields](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxImageEffect.h) |
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
| W04 | CPU shared compiled graph/Preview/Export semantics | 实现工厂/独立residentinstance/ownerdeadline/cancel、共享CompiledEffectGraph；Nondeterministic+Uncacheable防错误cache；仍Stateless/Frame声明、opaqueprivate/sequentialstate未完整authorcontract | `app/src/openfx_effect.rs`; renderer/exportsolegraph |
| W05 | scene-linearcolour/alpha parity和真实LUT输出 | 普通coverage路径已有产品OCIO及LUT 输出资格；没有nativeOFXcolourstyle/config/input/output negotiation，也没有全Floatconsumer域；S04/S08才可扩展，不能据LUTGREEN宣称全OFX色彩兼容 | `EffectColorDomainContract::SCENE_LINEAR`; 产品 stock OCIO output；[Colour](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxColour.h) |
| W06 | Resident steady-frame latency | **待验收**：ignored release gate已在tracked，测实际productresidentworker，warmup16..1000不入sample；200..10000帧，p50/p95/p99/max、declaredFPSstagepercent预算、pixeloracle/同worker等；没有本agent measuredGREEN | `app/tests/openfx_host.rs::official_basic_production_resident_steady_frame_performance_gate`; `scripts/validation/collect-native-perf-context.py` |
| W07 | native完整性能预算/RSS/插件SMP/4K重型效果/multipleinstances | **待验收/未闭合**；128MiB raster/mappings预算存在，但pluginMemory/heap/GPU资源不算在graph估计内；Basic算术fixture不能证明所有plugineffect实时 | `resident::required_frame_storage`; Memorysuite；factoryresidentestimate |
| W08 | 每帧binaryidentityverification开销 | 当前wholebinarySHA至少parent before+after与workerverify，源读可定位三个wholefilehash；尚无测量/优化GREEN。不能stat代hash或forevercache；binding/resource bundle snapshot设计仅有草稿 | `resident.rs` SHA调用；`openfx_host.rs::verify_binary`; 原生绑定与资源快照的设计草稿（尚未实现） |
| W09 | 用户通知/故障解释 | catalogstatus是i18n产品UI；nativepluginwarnings/errors/messages未真实hook到通知center；question/dialog更未完整UIownership | `app/openfx_catalog`; `effect_instance.cpp::vmessage`; B09–B15 |
| W10 | hostSDK版权/第三方effectdistribution | vendor BSD-3-Clause源码与license已随产品，未分发第三方effectbinary。接入标准SDK不代表取得vendor插件或其assets/license授权；常规loader不bypasslicensing | `native/vendor/LICENSE.md`, `README.md`; [官方license](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/LICENSE.md) |

## 8. 剩余的最小闭环切片与验收

顺序先修已开放suite和image正确性，再扩大context/布局/UI/GPU。每个切片都要
测试当前实现真实 RED、应用修复后真实 GREEN，再跑 product worker 与共享图；
明示hostcapability仅在闭环通过后开放。扩大能力前应先完成独立闭环，再更新 capability 声明。

| Slice | 闭环目标 | 必需代码authority | 独立验收（不能mirror实现） | 性能约束 |
| --- | --- | --- | --- | --- |
| S01 | actualFrameImage metadata：UniqueIdentifier、progressive FieldNone | `FrameState` owner epoch + `FrameImage`；counter寿命覆盖session、Source/Output独立、所有fetch借用有效 | 插件按SourceID缓存独立analysis，改变raster/seek/newsession不错误复用；sameimage repeatedfetch合法稳定；真实imageField必须None，不能只查render/clipprop | 无额外rasterhash/clone；generation allocation次数明确 |
| S02 | Message handle correctness + persistent typed responses、Progress/Timeline真实行为 | product suite dispatch真实Host/Descriptor/Instance，workerresponse与apppostcommit通知/navowner | NULL/descriptor/instance消息不crash；warning/errorset/clear可见；noninteractivequestion拒绝；交互questionreply和timelinecallback可取消；scopedprogress取消及时 | boundedutf8/responsequeue；noUIblockedrealtime；日志不塞满workerpipe |
| S03 | MultiThread套件全语义，包括真实mutex + boundedSMP | host suite domainownedscheduler/mutexobjects；不是开启vendor宏 | recursionErrExists；每callbackindex覆盖一次+join；initiallockcount/trylock/otherthreadblocking/unlock；callbackspawnstatus真；shutdown无悬空handle | boundedthreads、nestedbudget；同机器1CPU vsavailableCPU p99；不与NLEworkers过度订阅 |
| S04 | fullfiniteassociation纵向：rawOFX→Coretypedframe→Effects→soleNormalOver→cache→nested→opaque真实output | **待审查草稿，不计入已实现**；完整有限 Float association；actualSourcebinding、planactualformat/admission、cache3/header2 | currentAPIraw RED；zeroαemission/negativeRGB/signed/>1/subnormal/MAX；samevalidworker不退役；rawcachebits、nestedf64oracle、OCIOopaqueRGBA8；GPUcoverageconsumer typedrefusal无DSP重放 | existingstaging/outputclone预算；FiniteStraightCoverage=可表示Float32转换，不承诺bitlossless；无epsilon/clamp |
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


## 9. 维护与支持声明

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
