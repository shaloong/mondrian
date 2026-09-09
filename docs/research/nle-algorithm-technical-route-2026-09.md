# Mondrian NLE 算法功能技术路线定案（2026-09）

> 本文是对《NLE-算法功能技术预研报告-2026-08》的决策性补充，不复述完整模型清单，而回答五个问题：现在做什么、不做什么、为什么、怎样落地、何时允许换路线。
>
> 调研截止：2026-09-01。外部事实优先引用厂商文档、论文作者仓库和模型卡。License 结论只用于工程筛选，不替代正式法务意见。

## 0. 一页定案

### 0.1 产品主线

Mondrian 不应与 Resolve、Premiere 比“AI 功能数量”，而应集中形成一条可被用户感知、又能由小团队支撑的主线：

**本地优先的可编辑分析资产 → 可信的粗剪与局部修复 → 所有结果可回退、可修正、可复算。**

前 12 个月只押两组英雄工作流：

1. **中文/多语素材理解与文本粗剪**：转写、词级定位、删字即剪片、全文搜索、视觉语义搜索、OCR 搜索；
2. **可信的运动选区与变速**：把现有平面/平移 Mask 跟踪产品化，再接 SAM 2.1 Roto 和 RIFE 变速，统一失败提示、局部重算和确定性回退。

这两组工作流能分别对标 Premiere/Resolve 的高频生产力功能，又能用“中文、OCR、全本地、可检查分析证据”形成差异。

### 0.2 优先级结论

| 级别 | 项目 | 决策 | 12 个月内目标 |
|---|---|---|---|
| P0 | 通用分析资产与任务底座 | **自研，立即做** | 支撑 transcript / shot / embedding / mask / motion 等可缓存、可取消、可复算产物 |
| P0 | ASR + 词级对齐 + 文本剪辑 | **集成模型，自研编辑语义与 UX** | 形成第一个完整英雄工作流 |
| P0 | 现有 Mask 跟踪产品化 | **深化已有实现** | 失败帧可见、锚点重跟、人工修正不丢失 |
| P0 | 场景切分、响度、多机位同步基础 | **经典算法优先** | 为搜索、Retime、多机位提供确定性底座 |
| P1 | 本地视觉/文本/OCR 联合搜索 | **SigLIP 2 + OCR + 自研索引/排序** | 中文搜索优于 Premiere 当前仅英语视觉查询的空档 |
| P1 | SAM 2.1 交互 Roto | **可替换后端，先 SAM 2.1** | 点/框提示、双向传播、局部纠错、可编辑 Mask 输出 |
| P1 | RIFE 变速补帧 | **先垂直集成，不先造通用光流平台** | 2×/4× 慢放、cut 门控、失败回退、代理/导出双档 |
| P1 | 2D 稳定 | **复用现有轨迹，自研路径与裁切优化** | 先交付确定性稳定，再由失败集决定 mesh/flow |
| P1 | 对白增强与多机位同步 | **DeepFilterNet 类后端 + 自研同步/漂移** | 强度混合、逐句 A/B、长素材不漂移 |
| Gate | Gyro 稳定 + Rolling Shutter | **先外部 Gyroflow 工作流/烘焙媒体，后决定自研** | 市场空白已缩小，不承担无证据的相机格式维护面 |
| P2 | Depth AOV + 确定性 Relight | **DA3 小/基础档 + 自研时序与着色** | 先服务景深、雾、深度选区，再做 Relight |
| P2 | Clean-plate Remove / Auto Reframe / SmartSwitch | **复用前述资产渐进实现** | 不引入新的通用大模型平台 |
| Gate | Camera Solve | **仅做研究样机，需求达标后立项** | COLMAP 4.x incremental/global 双基线；学习模型只做初始化候选 |
| Gate | OFX Host | **推迟到有明确插件/OEM 合作** | 不承诺 beta 前完成 |
| Gate | 表达式脚本、WASM/WGSL SDK | **先属性链接，脚本与 GPU 插件后置** | 不阻塞核心剪辑能力 |
| Watch | 生成式视频、重型修复、3DGS | **API/外部工具优先，不自建主线** | 只做有限、可标记、可撤销的补丁型输出 |

### 0.3 最重要的路线修正

原报告的“光流是基础设施”需要改成：**运动分析平台是基础设施，单一光流模型不是。** 平面跟踪、点轨迹、稠密光流、插帧中间流、gyro 姿态和 3D 相机轨迹具有不同语义。RIFE 也不是消费通用 fwd/bwd flow 的普通下游节点。应共享任务、缓存、身份、置信度和 UI，不强行共享一个 flow 输出。

原报告的“P0 全 CPU”也不成立。ASR 可以提供 CPU 保底，但语义搜索、SAM、RIFE 的有竞争力体验都需要硬件加速。正确产品承诺是：**基础编辑不依赖 ML；分析功能有 CPU/通用加速保底；高质量档按能力显式开放。**

### 0.4 怎样接近或超过竞品

| 工作流 | 近期“追平”标准 | Mondrian 应争取的“超过”点 | 不投入的方向 |
|---|---|---|---|
| 文本粗剪 | 转写、删字剪片、filler 批删、speaker 过滤 | 中文/粤语语料、精确可审计 range、用户修订不被重跑覆盖 | 自动决定最终成片 |
| 素材搜索 | 台词、元数据、视觉内容统一搜索 | 中文视觉查询 + OCR/场记板 + 命中证据 + 可移植 sidecar | 人脸身份识别、逐帧 VLM caption |
| 跟踪/Roto | 点/框提示、向前向后传播、可回时间线 | 已有平面 tracker 与 AI mask 同一修正 UX；低可信帧可见；局部重算 | 只展示一次点击的 demo 成功率 |
| 变速 | 代理预览、高质量导出、cut 不插帧 | 每段明确显示 sampled/blended/interpolated；失败可自动回退 | 8× 以上扩散慢动作主链路 |
| 稳定 | 2D 路径平滑与自动裁切 | 支持一组真实相机后的 gyro + rolling shutter 联合校正 | 一开始承诺所有相机遥测格式 |
| 深度/Relight | 可缓存深度、景深/雾/选区 | Depth AOV 是可复用标准资产，Relight 确定性且可关键帧 | 逐帧生成式重打光 |

竞争优势应落在完整工作流和失败恢复，不落在“默认模型比竞品多一个版本”。Resolve 21 已把内容搜索、具体人脸搜索、剧本组装和语音生成做进产品（[Blackmagic Resolve 21](https://www.blackmagicdesign.com/products/davinciresolve/whatsnew)）；Premiere 已经有本地 Media Intelligence、文本剪辑和云端 Extend。功能名称层面的空白很少，剩下的机会是中文、本地、透明证据和深度编辑语义。

## 1. 决策约束与评估方法

### 1.1 资源假设

以下排期以 4 名核心工程师为基线：2 名 Rust/媒体/GPU、1 名 ML/部署、1 名编辑器/产品集成；设计与 QA 各 0.5 人共享。12 个月可支配的有效研发量约 36–42 工程人月。若没有专职 ML/部署工程师，SAM/RIFE/语义搜索整体后移一个阶段。

成本估算均包含产品集成、失败路径、缓存、取消、基础测试，不包含自训大模型、跨三平台同时量产、独立法务和大规模用户研究。

### 1.2 排序规则

每项能力按五个维度排序：

- 用户每周能否反复使用，而非只在演示中惊艳；
- 是否复用 Mondrian 已有的时间线、Effect DAG、Mask、缓存、取消和 authoring transaction；
- 是否能形成中文、本地、可修正、可验证的明确差异；
- 12 个月内能否做到“可靠地比不用更快”；
- 模型、权重、运行时和插件是否允许稳定分发。

任何项目只因论文榜单领先而不能升级优先级。升级必须经过固定素材集、目标机器、产品动作和用户纠错成本四类门槛。

## 2. 先建的不是“AI 层”，而是 Analysis Artifact 层

Mondrian 已有的 Mask 跟踪是正确范型：Effects 只做纯分析，App 拥有有界 worker、缓存、取消和 stale 防护，Timeline 在事务中接收完整结果。新功能应深化这条边界，而不是另建一个无边界的 AI worker pool。

### 2.1 核心对象

建议新增独立的 `mondrian-analysis` foundation crate，承载类型与协议，不承载所有工作线程。各领域仍拥有自己的队列和容量策略。

```text
AnalysisRecipe
  ├─ kind + schema_version
  ├─ exact source fingerprint / stream / sample range
  ├─ preprocessing contract (raster, color/luma, audio rate/layout)
  ├─ provider + model artifact digest + parameter digest
  └─ requested quality / capability

AnalysisArtifact
  ├─ exact recipe identity
  ├─ typed payload: Transcript | ShotBoundaries | EmbeddingIndex |
  │                MaskSequence | PointTracks | MotionField | DepthAov ...
  ├─ coverage and confidence evidence
  ├─ provider/backend/runtime evidence
  └─ completed / failed / canceled / superseded disposition
```

### 2.2 存储边界

- Analysis Store 是独立语义，不能复用 `mondrian-render-cache`；后者按架构只存完整 post-Effect/post-composite RGBA32F Timeline frame；
- 大型派生产物默认存项目外的内容寻址 cache，不进入 `.mdp`；
- `.mdp` 保存可复算 recipe、用户采纳的编辑结果和必要的人工修正；
- 用户修订过的 transcript 是 author data，原始 ASR 假设仍是 derived artifact；
- 搜索 embedding、稠密 flow、深度帧不进入项目文件；
- 可选 sidecar 用于跨项目/机器复用，但必须绑定完整媒体 fingerprint 和 schema；Adobe 也把本地分析结果放在 cache 或 `.prmi` sidecar，而不是把模型状态混入时间线语义（[Adobe Media Intelligence](https://helpx.adobe.com/lu_en/premiere-pro/using/media-intelligence-and-search-panel.html)）。
- 大产物按约 8–32 帧或 64–256 MiB 切 immutable shard；每个 shard 可原子写入，但只有完整 manifest 才能发布“成功”。取消、超时和 superseded attempt 产生的部分 shard 不得被上层解释为完整结果。

### 2.3 共享什么，不共享什么

共享：

- source/range/recipe/model 身份；
- 有界任务、优先级、取消、generation、stale 防护；
- artifact publication、配额、LRU、模型下载与校验；
- 进度、置信度、失败原因和重算 UX；
- benchmark 记录和 license provenance。

不共享：

- 一个“万能 ML worker pool”；
- 一个被所有消费者强制接受的光流格式；
- Preview 与离线分析的同一 deadline/容量策略；
- 模型输出直接写 Timeline 的旁路；
- 因模型失败而静默输出 identity 或看似成功的结果。

### 2.4 ML 执行边界定案

首版使用**受监督的独立分析进程 + 后端适配器**，但不承诺跨进程 GPU 零拷贝：

1. App 通过有界版本化协议提交 recipe；
2. worker 读取代理帧或通过共享内存接收规范化 CPU tensor；
3. worker 内按模型选择 CTranslate2 / whisper.cpp / ONNX Runtime / 专用 runtime；
4. 返回小型结构化结果或发布到 artifact store；
5. App 重新验证 generation、source fingerprint 和 author target 后才能采纳。

原因是“ORT 独立进程 + D3D/Vulkan/IOSurface 零拷贝”不是一个跨平台开关。ONNX Runtime 的 I/O Binding 能减少同进程设备拷贝，但要求执行提供器、设备、队列和内存所有权正确配套（[ORT I/O Binding](https://onnxruntime.ai/docs/performance/tune-performance/iobinding.html)、[ORT Device Tensors](https://onnxruntime.ai/docs/performance/device-tensor.html)）。跨进程共享纹理应在某个英雄功能的拷贝成本被实测证明为瓶颈后单独立项。

Windows 路线：

- 2026 新项目优先评估 Windows ML 的动态 Execution Provider 目录；Microsoft 已把它作为硬件检测和 EP 注册入口（[Windows ML EP selection](https://learn.microsoft.com/en-us/windows/ai/new-windows-ml/select-execution-providers)）；
- DirectML 保留为兼容后端，不再写成长期唯一方案；其 ORT EP 处于 sustained engineering，且要求关闭 memory pattern 和并行 session execution（[DirectML EP](https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html)）；
- NVIDIA 的 TensorRT/CUDA 是可选高性能包，不作为基础功能可用性的前提。

跨进程 native GPU handle 的阶段门固定为：profiling 证明 tensor copy 占端到端耗时超过 15–20%，并且同 adapter、fence、worker crash/restart、device loss 和资源回收测试全部通过。否则共享 CPU memory 是更可靠的优化点。

## 3. 分领域路线定案

### 3.1 ASR、文本剪辑与素材理解：第一优先

#### 明确选型

- 多语默认：`faster-whisper` / CTranslate2，基准模型 `large-v3-turbo`，按机器下放 small/medium；
- 中文候选：SenseVoiceSmall 与 Whisper 做固定素材 A/B，不立即替换默认；维护者已明确官方权重可用于付费本地桌面软件，但需遵守 FunASR Model License 的署名与模型名要求（[官方澄清](https://github.com/FunAudioLLM/SenseVoice/issues/286)）；
- 中文质量升级候选：FireRedASR2S 已在 2026 提供方言、词级时间戳和 confidence，但官方只验证 Ubuntu，Windows 产品化未证明；只进入内部 benchmark，达到 CER 相对改善 ≥20% 且 RTF 不恶化超过 2× 才升级（[FireRedASR2S](https://github.com/FireRedTeam/FireRedASR2S)）；
- 词级对齐：采用 WhisperX 的“VAD 分段 + 语言专用强制对齐”方法，但把模型与对齐器做成独立 provider；WhisperX 代码是 BSD-2，而每种语言的对齐权重另行审查（[license](https://github.com/m-bain/whisperX/blob/main/LICENSE)）；
- 说话人分离不阻塞 v1。首版允许用户手工改 speaker；二期以 pyannote `community-1` 作质量基线，并评估轻量 provider。该模型为门控下载、CC-BY-4.0，分发体验和署名要单独处理（[model card](https://huggingface.co/pyannote/speaker-diarization-community-1)）；Sortformer 当前公开模型又有最多四说话人的边界，不能包装成无限人数通用方案（[NVIDIA NeMo 文档](https://github.com/NVIDIA-NeMo/Speech/blob/main/examples/speaker_tasks/diarization/README.md)）。

#### 实现顺序

1. source transcript artifact：segment、word、时间范围、置信度、语言、speaker placeholder；
2. 文本选区映射到一组精确 source ranges；
3. 通过现有 authoring session 生成 Lift/Extract/insert 等普通编辑动作；
4. filler/silence 检测只生成候选，批量应用前显示将删除的精确区间；
5. 时间线 transcript 从 source transcript 和剪辑映射投影，不复制第二份时间解释；
6. 用户改字与 speaker 修订独立保存，重跑 ASR 不覆盖人工修订。

#### 验收门槛

- 固定中文/英文口播、访谈、噪声、多人、方言素材集；
- 每个词的映射可回到精确 source range，批量删除后无负时长、重叠或脱离 linked edit policy；
- 30/60/120 分钟素材可取消、可恢复、重开项目不丢人工修订；
- 默认模型以目标语料 CER/WER、时间误差、RTF 和峰值内存综合胜出，不用单一公开榜单决定；
- 产品门槛：10 位目标剪辑师中至少 7 位在真实粗剪任务上节省 30% 以上时间，再扩大投入。

估算：分析底座复用后 6–8 工程人月。

### 3.2 搜索：视觉、台词、OCR 三路融合，不做“大模型问答”

Premiere 的视觉搜索在设备本地运行、结果可缓存，但官方截至 2025 文档仍只支持英语视觉查询；2026 文档也说明它不识别/标注具体人物（[Adobe 文档](https://helpx.adobe.com/lu_en/premiere-pro/using/media-intelligence-and-search-panel.html)、[2026 FAQ](https://helpx.adobe.com/fr/premiere/desktop/organize-media/file-organization/media-intelligence-and-search-panel.html)）。Mondrian 的现实差异点是中文、OCR 和透明的三路证据，不是再做一个聊天框。

#### 明确选型

- 视觉 embedding：SigLIP 2 Base 224 起步，模型卡标记 Apache-2.0，支持图文检索且具备多语预训练（[官方模型卡](https://huggingface.co/google/siglip2-base-patch16-224)）；
- 抽帧：先 scene boundary，再取镜头首/中/运动峰值关键帧，并保留每 2–5 秒的覆盖下限；不逐帧 embedding；
- OCR：首测 2026-06 发布的 PP-OCRv6 mobile/server 档，索引保存文本、框和时间范围，跨帧跟踪去重；PaddleOCR 官方已把 v6 统一识别扩大到 50 种语言（[PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR)）；
- 索引：SQLite FTS5 负责 transcript/OCR/metadata；10 万向量前直接扫描规范化 FP16 embedding，超过后再引 HNSW，避免把 alpha 状态的 sqlite 扩展变成核心依赖；
- 排序：词法与向量结果使用 Reciprocal Rank Fusion，再叠加时间/素材过滤；结果明确显示画面、对白、OCR 或元数据命中来源。

不做：人脸身份库、自动给陌生人命名、逐帧 VLM caption、LLM 重排全部素材。

估算：5–7 工程人月。门槛：内部 500 条中文真实查询集 Recall@10 达标，且用户从搜索到拖入时间线的中位用时比手工浏览降低 50%。

### 3.3 Mask 跟踪、点跟踪与平面跟踪：先深化已有资产

Mondrian 已经具有 translation / 8-DOF homography、质量证据、真实媒体解码、任务取消、LRU、stale 防护、事务发布和 Undo/Redo。原报告将“自研 planar tracker 2–3 人月”列为从零 P0，已与仓库现状不符。

近期工作不是换模型，而是补产品闭环：

1. 每帧保存/投影 match、inlier、RMS、coverage 或统一 confidence；
2. 时间线上标出首个失效帧和低可信区间；
3. 用户在任意帧设新锚点，局部向前/向后重跟；
4. 人工修正保存为独立 residual keys，重算自动轨迹不丢；
5. 增加尺度、旋转、运动模糊、出画、弱纹理和遮挡素材基准；
6. 只有当前方法在基准上持续失败的类别，才调用学习型点跟踪 provider。

学习型升级候选改为 TAPNext/TAPNext++，而不是停留在 BootsTAPIR/LocoTrack 清单。Google DeepMind 官方仓库已经包含 TAPNext/TAPNext++，并明确其发布 checkpoint 为 Apache-2.0（[tapnet](https://github.com/google-deepmind/tapnet)）。但模型仅作为新 provider，不能改变 `TrackingRecipe → generated keys → author transaction` 的现有权威边界。

估算：现有实现产品化 4–5 工程人月（含金字塔、前后向一致性、residual layer、UI 与真实媒体回归）；学习型 provider 另加 3–5，需由失败语料触发。

### 3.4 Roto / Matting：SAM 2.1 定基线，SAM 3.1 只做可选评测

#### 明确选型

- 默认后端：SAM 2.1 Base+/Small，先代理分辨率和单/少对象；官方代码、checkpoint、训练和 demo 均为 Apache-2.0，并支持视频中追加 prompt 与多对象传播（[SAM 2 官方仓库](https://github.com/facebookresearch/sam2)）；
- 输出：压缩的概率/alpha artifact + 用户采纳后的标准 Mask/Matte，不把模型隐状态写入项目；
- Matting：v1 先用 trimap + 经典 guided/edge refine；只有发丝质量成为付费阻塞点才接 ViTMatte 类后端；
- SAM 3.1：其多对象 Object Multiplex 与文本概念跟踪值得 benchmark，但自定义 SAM License 可被修改且包含用途、诉讼和终止条款，不能替代默认 Apache 路线（[SAM 3 License](https://github.com/facebookresearch/sam3/blob/main/LICENSE)、[SAM 3.1 release](https://github.com/facebookresearch/sam3/blob/main/RELEASE_SAM3p1.md)）。

#### 产品闭环

点/框/负点提示 → 当前帧即时 matte → 有界区间双向传播 → 低可信/边缘变化可视化 → 新提示只使相邻区间失效 → 用户可 freeze/bake 为标准 Mask。

不接受“一次点击跑完整片”作为完成标准。真正指标是每分钟素材需要多少次人工纠错，以及纠错后需要重算多少帧。

估算：7–10 工程人月。若 30 个目标镜头中位纠错密度仍高于每 3 秒一次，停止产品化，保留实验功能。

### 3.5 Retime 与光流：先交付插帧，再决定是否建设通用 MotionField

#### 明确选型

- v1：原帧采样与 frame blend 保底；RIFE/Practical-RIFE 作为高质量 interpolation provider。RIFE 官方代码为 MIT，并公开 4K 可降 scale 的实践路径（[RIFE license](https://raw.githubusercontent.com/hzwer/ECCV2022-RIFE/main/LICENSE)、[官方仓库](https://github.com/hzwer/ECCV2022-RIFE)）；
- 不按版本号追新：固定评测 Practical-RIFE 4.25、4.25-lite 与 4.26，选定后连 checkpoint digest 一起冻结；4.26 的输入尺寸约束也必须进入 provider contract（[Practical-RIFE](https://github.com/hzwer/Practical-RIFE)）；
- 强制 cut gate：场景切点两侧禁止插帧；
- 模型分析色彩域必须显式：working-linear 先转换到模型训练匹配的有限 encoded RGB，输出再回 working-linear；不能把仓库的 8-bit FFmpeg demo 当作 Mondrian 生产色彩路径；
- 代理档与导出档使用同一时间映射和结果语义，但允许不同 provider/分辨率；
- 失败回退是显式结果：interpolated / blended / sampled，并进入 Frame Delivery/Export evidence；
- 不以 NC 模型作 teacher 自行蒸馏作为既定路线，除非许可条款与法务书面允许该训练及所得权重分发。

SEA-RAFT 有不确定度输出、BSD-3 且比 RAFT 更高效，适合作为未来通用 dense motion provider 候选（[SEA-RAFT](https://github.com/princeton-vl/SEA-RAFT)），但只有稳定、RS 修复或多个 Effect 确认消费相同 motion contract 后才建设 `MotionFieldArtifact`。不要为了架构整齐先付出模型部署、4K 存储和格式兼容成本。

估算：RIFE 垂直链路 5–7 工程人月；通用 motion artifact 另加 3–5 工程人月。

### 3.6 稳定与 Rolling Shutter：二维先做稳，gyro 先借外部生态验证

- 2D 默认档：复用现有平面轨迹，路径平滑 + 自动裁切/缩放 + 用户 smooth/crop 控制；
- mesh warp 只有在 2D 基准明确出现前景/背景视差失败后再做；
- gyro 路线仍有质量价值，但已经不是无人覆盖的市场空白。Gyroflow 已公开 Resolve/Nuke/Vegas 的 OFX 以及 Adobe 工作流插件（[Gyroflow](https://github.com/gyroflow/gyroflow)、[Gyroflow plugins](https://github.com/gyroflow/gyroflow-plugins)）；
- 近期先支持“用户安装 Gyroflow → 外部分析/烘焙稳定媒体 → 作为新 Asset 回到 Mondrian”的明确工作流，或交换项目/遥测，不分发其 GPL 核心；
- 只有该工作流使用率和用户摩擦证明值得内建时，再自研一个格式族。工程量主要在遥测格式、时间对齐、镜头标定、逐行曝光和相机样本库，不是姿态积分公式；
- 自研时先只承诺一个格式族和一组真实相机，经过 30/60 分钟时间漂移、不同裁切/电子防抖和 VFR 素材验证后再扩展。

估算：2D 稳定 3–5 工程人月；外部 Gyroflow 工作流 1–2；若数据支持自研，第一个 gyro 格式族仍需 6–9，每个后续格式族约 1–3 并持续维护。

### 3.7 音频：确定性功能先于生成式功能

优先顺序：

1. BS.1770 响度测量、两遍归一、true-peak 限制；
2. 多机位先读 LTC、容器 timecode 和相机时间；音频路线用频谱 landmark 粗匹配，再用 GCC-PHAT 多窗口精修，RANSAC 拟合 `t_ref = a*t + b` 校正漂移，多机关系以图做全局最小二乘；
3. DeepFilterNet 类对白增强，必须有 dry/wet、响度补偿、逐句 bypass；
4. 说话人分离用于检索与 SmartSwitch，但不作为文本剪辑 v1 的阻塞条件；
5. stem separation、音乐 Remix、V2A、改口型后置。

Premiere 2026 已将生成音乐、音效和声景放入 beta（[Adobe 文档](https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/add-generated-music-to-your-timeline.html)），跟进这类功能需要持续模型/API 成本，不能成为 Mondrian 小团队的近期护城河。

估算：响度 + 同步 3–4 工程人月；对白增强 3–5；可靠 SmartSwitch 5–8。

### 3.8 Auto Reframe 与 SmartSwitch：规划问题先于模型问题

Auto Reframe 不以显著性模型为主权威：

- 用户指定主体、人脸/人体/Mask 轨迹优先，显著性只作无主体时的候选；
- 路径优化同时约束位置、速度、加速度、缩放、safe area 和镜头切点；
- 输出普通 position/scale keys，用户可直接继续编辑；
- 多人提供群组、当前说话人和手选模式，不让模型暗中决定叙事主体。

SmartSwitch 也不能简化成 Light-ASD：

1. 独立领夹麦/VAD 与机位的已知映射优先；
2. 其次是说话人聚类与画面人物的项目内映射；
3. 再用 active-speaker detection 解决同机多人；
4. 最上层必须是剪辑语法状态机：最短/最长镜头、说话后切换延迟、抢话、静音回广角、开场/收尾、避免同构跳切；
5. 输出候选 multicam sequence，每个切点显示触发证据，用户确认后才进入 author state。

二者都复用 Track/Mask、speaker、VAD、scene boundary 等资产，不需要新的通用大模型。估算：Auto Reframe 4–6 工程人月；SmartSwitch 5–7，排在可靠转写、同步和说话人之后。

### 3.9 深度、Relight 与相机反求：拆开，不打包成一个 3D 战略

#### Depth / Relight

- P2 默认：DA3 Small/Base 的 Apache 权重，低分辨率推理 + 边缘引导上采样 + 光流/关键帧时序稳定；官方仓库本身为 Apache-2.0，并输出 depth/confidence/pose 等结构（[DA3](https://github.com/ByteDance-Seed/Depth-Anything-3)、[license](https://raw.githubusercontent.com/ByteDance-Seed/Depth-Anything-3/main/LICENSE)）；
- 先交付 Depth AOV、景深、雾、深度选区，再做 normal 推导和确定性光源；
- 深度缓存必须是代理分辨率、按需时间 chunk/tile、带预算的 LRU。1080p 单通道 FP16 约 4.15 MB/帧，一小时 30 fps 已约 448 GB；绝不能默认把整片全分辨率 EXR 写盘。flow 的体积更高，更需按区间和消费者需求生成；
- 不承诺单帧 metric depth 等于可用于精确 CG 合成的摄像机几何；
- 首版产品名应是 Surface Relight / Light Shaping，不声称重建材质、移除原光或产生物理正确投影阴影；Blackmagic 自身也把 Relight 描述为由深度幻觉塑造现有照明，而非真实 3D 几何（[Resolve 18.5 guide](https://documents.blackmagicdesign.com/SupportNotes/DaVinci_Resolve_18.5_Beta_New_Features_Guide.pdf)）；
- 逐帧扩散 Relight 不进主链路，但 2026 的 LiveLight 已报告流式长视频和实时级结果，应纳入质量对标。其 MIT 仓库并不自动覆盖 Stable Diffusion/VAE/depth 等外部权重链，未审完前不打包（[LiveLight](https://github.com/mayuelala/LiveLight)）。

#### Camera Solve

- 基线改为 COLMAP 4.x 的 incremental `mapper` 与 `global_mapper` + bundle adjustment 双路线。独立 GLOMAP 仓库已在 2026-03 归档并明确迁移到 COLMAP，不应再维护 GLOMAP sidecar（[GLOMAP deprecated notice](https://github.com/colmap/glomap)、[COLMAP 4.0 release](https://github.com/colmap/colmap/releases)）；
- COLMAP 4 还把 ALIKED/LightGlue ONNX 匹配、global mapper、Python bindings 和 Windows 静态 CUDA 构建放在同一发行面，进程隔离集成比拼接多个旧 CLI 更可控；但 global mapper 更依赖可靠焦距先验，不能删除镜头模型和参数锁定 UI（[COLMAP CLI](https://github.com/colmap/colmap/blob/main/doc/cli.rst)）；
- VGGT 已有需申请的商业 checkpoint，但只有该 checkpoint 可商用，原 checkpoint 仍非商用；其许可排除军事用途（[VGGT 官方说明](https://github.com/facebookresearch/vggt)）；
- 2026 的 GLUEMAP 说明“前馈局部重建 + 全局 SfM + BA”是新的重要方向，但默认流水线依赖多个前馈模型和 checkpoint，产品许可与部署必须逐项审查（[GLUEMAP](https://github.com/colmap/gluemap)）；
- 因此只做独立 spike：同一 20–30 镜头集比较 COLMAP incremental/global、VGGT-commercial 初始化 + BA、DA3 pose 初始化 + BA。没有用户轨迹、镜头模型、参数锁定、重投影误差和失败修复 UI，不称为产品 Camera Solver。

完整专业 camera solve 不是 2–4 人月功能，合理量级为 10–18 工程人月，且需真实 VFX 用户共同验证。只有每月活跃用户中 10% 以上明确需要、或出现付费 VFX 客户/合作方时升级立项。

### 3.10 画质修复与生成式：从“集成模型”改为“建立评测门”

暂不在路线图中绑定 HAT、BasicVSR++、DOVE、SeedVR2 等具体默认模型。画质修复的最大风险不是模型缺失，而是：

- 模型对目标素材退化分布失配；
- 主观锐利度上升但身份、纹理和颗粒被重写；
- 长视频 temporal consistency 与 tile seam；
- 权重训练数据和分发条款；
- 每个新模型都增加大体积下载、显存和后端矩阵。

近期只做确定性 deband、deflicker、程序化 grain 和轻量时域降噪。重型超分/修复建立离线对比 harness，只有用户愿意为其等待、目标语料显著胜过经典基线、且能用单个可分发模型覆盖主要机器时再产品化。

生成式只保留 provider 接口。Adobe 官方明确 Extend 需要把 1.5–3 秒片段发到云端，视频最多延长 2 秒、音频 10 秒，并存在 8-bit 输出和地区不可用等限制（[Adobe Generative Extend FAQ](https://helpx.adobe.com/ca/premiere/desktop/edit-projects/edit-with-generative-ai/generative-extend-faq.html)）；但 2026-07/08 的 Premiere beta 已进一步加入 Generate Media Tool，在时间线上生成视频、音效、音乐和 soundscape，因此“生成式已经只收敛为 2 秒 Extend”也不再是完整竞品事实（[Adobe Generate Media](https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/generative-media-tool-overview.html)）。

Mondrian 仍应坚持补丁型入口，因为它能限制成本和失败范围。云端 provider 必须逐任务显示上传范围和预估成本，生成结果作为新媒体 Asset，并记录 provider/model/prompt/seed/hash/费用与来源声明；普通项目语义不能依赖云模型仍在线。端侧 Wan 2.2 只保留实验包：官方 TI2V-5B 的 5 秒 720p 在 RTX 4090 仍约需数分钟，不是面向普通用户的默认隐私档（[Wan 2.2 model card](https://huggingface.co/Wan-AI/Wan2.2-TI2V-5B)）。

## 4. 表达式与插件生态的重新排序

### 4.1 表达式

先做稳定 ID 驱动的 Property Link / Driver Graph，不先做 Rhai：

- 引用目标必须是 `SequenceId / ClipId / EffectId / ParameterId` 等稳定地址，不使用 `comp.clip[2]` 这类会随排序变化的字符串路径；
- 支持常量、算术、clamp/remap/ease、时间与有限噪声；
- 编译时提取完整依赖并检测环；
- 进入现有 Effect/automation fingerprint 和缓存身份；
- 不允许 IO、动态 eval、反射式遍历项目或运行时发现依赖。

Rhai 即使禁 IO，也不能自动满足纯函数、静态依赖和可重现性；把它放进逐帧属性求值会给 MFR、缓存和故障隔离引入长期负担。只有插件作者和高级用户的真实需求量证明 L0 不够时，再设计离线脚本动作，而不是 render-hot-path 脚本。

### 4.2 OpenFX

OFX 不是“bindgen + 2–4 人月”。一个可用 host 还包括：参数/clip/ROD/ROI/多分辨率、时域访问、线程、安全、插件发现、持久化、UI、崩溃隔离、厂商兼容矩阵和 GPU context。OpenFX 的可选 GPU suite 面向 OpenGL/OpenCL/CUDA/Metal，而不是 wgpu 的统一资源模型（[OpenFX 规范](https://openfx.readthedocs.io/_/downloads/en/latest/pdf/)）。

定案：

- 没有 2 家以上关键插件厂商愿意共同验证前，不做完整 OFX；
- 可安排一次 4–6 周、1–2 人月的兼容性 spike：只加载官方 sample 与 2 个开源插件，验证 Float32 CPU、参数、clip、ROI、temporal 和 process crash；spike 不对外宣称支持 OFX；
- 若有合作，先做隔离进程中的 CPU RGBA/float 最小 host，验证 3–5 个指定插件；
- GPU OFX 单独立项，不把 CPU host 的成功等同于 Sapphire/Boris 全生态兼容；
- 可发布 Windows CPU 子集按 12–18 工程人月，广泛跨平台/GPU/custom UI 累计 24–40+ 并持续维护，而不是 P1 的 2–4 人月。

### 4.3 WASM / WGSL

WASM 适合参数逻辑、自动化和 CPU 小算子；WGSL 插件不应以字符串片段注入融合 shader。第一版如启动，应是独立 pass、固定 bind-group schema、Naga 验证、资源上限、超时/设备丢失证据和明确色彩域。等自用 Effect API 稳定、出现外部开发者后再公开 SDK。

### 4.4 LLM 剪辑助手

当前 `mondrian-ai` 只有 provider/workflow contract，没有生产 provider 或 editor mutation adapter；这反而避免了过早形成第二条 authoring path。后续定案为：

```text
read-only typed tools
        ↓
LLM produces EditProposal
  - bound project generation
  - stable IDs and preconditions
  - ordered public editor actions
  - affected ranges and estimated external cost
        ↓
local validation → visible diff → user confirm
        ↓
one authoring transaction → ordinary Undo
```

LLM 不能直接写 OTIO/EDL 后绕过 Mondrian 动作语义；OTIO 只适合交换或候选结果导出。简单自然语言命令可用本地小模型，复杂粗剪允许可选云 provider，但权限、上传范围和费用逐任务展示。若黄金任务集的工具选择准确率低于 90%，缩小命令集合，不通过扩大写权限掩盖模型问题。

优先级 P2，4–6 工程人月；生成式模型不是前置，先把 `EditProposal`、前置条件、diff、确认和单事务撤销做对。

## 5. 12 个月实施计划与资源封顶

### Phase A：0–8 周，打通第一条 tracer bullet（约 7–9 人月）

- 本阶段只做“一个 transcript artifact + 一个 ASR provider”的薄切，不试图一次完成通用 ML 平台；通用 manifest、sharding、更多 provider 和安全加固跨 Phase B/C 完成；
- `mondrian-analysis` 的 recipe/artifact/evidence 基础类型；
- 一个受监督 worker、模型 registry、下载/哈希/许可清单、CPU 共享内存协议；
- ASR source transcript → 文本选区 → 普通 author actions 的端到端链路；
- scene boundary 经典基线；
- 现有 Mask tracker 的失败区间与新锚点重算原型；
- 建立固定评测素材集和 reference-machine manifest。

退出条件：30 分钟真实项目可完成转写、文本删除、Undo/Redo、保存重开；取消/崩溃/源文件变更均不会写入过期结果。

### Phase B：第 3–5 个月，形成可发布 P0（约 9–11 人月）

- 中文/英文模型路由、词级对齐、人工 transcript 修订；
- filler/silence 候选与批量确认；
- transcript/metadata/OCR 的统一搜索 UI；
- Mask 跟踪 residual correction 与质量可视化；
- BS.1770/true peak、GCC-PHAT 同步基础；
- 后台任务遵守现有 Execution Resource Decision，不占用 realtime Playback lane。

退出条件：目标用户任务节省时间达到门槛；CPU 保底机器与主流 GPU 机器都有明确、诚实的功能可用等级。

### Phase C：第 6–9 个月，只并行两个 P1（约 10–12 人月）

- 视觉语义搜索 + OCR 中文查询；
- SAM 2.1 Roto **或** RIFE Retime 先做用户价值更高的一项，另一项保持 spike；
- 选择依据是前期用户访谈和素材失败集，不按论文热度；
- 对所有模型结果实现局部失效、增量重算和可编辑标准资产输出。

退出条件：英雄功能在 30 个真实项目上纠错成本低于手工作业，并有明确性能/显存支持矩阵。

### Phase D：第 10–12 个月，质量化而非扩清单（约 8–10 人月）

- 将胜出的 P1 功能做缓存、代理/导出一致性、安装器、遥测和回归门；
- 第二个 P1 仅在容量允许时量产；
- Gyroflow 外部工作流、Depth/Light Shaping、Camera Solve 各限时 2–3 周验证，产出使用数据或 benchmark，不承诺产品；
- 决定第二年主差异项：稳定/gyro 内建或 Depth/Light Shaping，不能同时全面立项。

12 个月封顶：P0 全部 + 搜索 + SAM/RIFE 中至少一项产品化。若同时承诺 OFX、Camera Solve、超分、生成式和 3DGS，必然牺牲稳定性，应直接拒绝排期。

### 团队规模对应的承诺

| 核心工程师 | 12 个月合理承诺 |
|---:|---|
| 2 人 | Analysis 基础 + ASR 文本剪辑 v1 + 现有 Mask 跟踪产品化；不承诺视觉语义搜索和新视觉模型 |
| 4 人 | 本文基线：P0 全部 + 联合搜索 + SAM/RIFE 至少一个产品化 |
| 6 人 | 可同时推进 SAM 与 RIFE，并增加音频同步/增强；仍不应并行完整 OFX 或 Camera Solve |
| 8 人以上 | 才考虑单独建立 gyro/Depth 小组；插件兼容仍需商务合作和长期 QA 预算 |

## 6. 每项功能都必须有的 Gate

### 6.1 License / 供应链 Gate

“代码 Apache/MIT”不能自动推出“权重、转换版、依赖、训练数据和产品用途都安全”。每个可下载模型必须记录：

- 上游仓库与精确 commit；
- 原始 checkpoint URL、SHA-256、model card revision；
- code / weight / tokenizer / conversion / dataset notices 分列；
- 商用、再分发、地域、用途、署名、诉讼终止条款；
- 转换脚本与量化产物的可重现 provenance；
- 法务状态：research-only / approved-optional / approved-bundled / rejected。

任何 NC/无许可权重不得作为 teacher 的默认训练输入；“只蒸馏不分发原权重”不是自动免责。

落地形式应是签名的 Model Pack，而不是三个字符串：manifest、模型/外部 tensor、完整 license/NOTICE、model card、source provenance、golden conformance I/O 和签名 bundle 一起发布。状态机至少包含：

- `ApprovedBundled`：允许随安装器分发；
- `ApprovedOnDemand`：用户确认条款后按需下载；
- `ApprovedInternalEvaluation`：只进入研发 benchmark；
- `UserSuppliedUnsupported`：用户自行提供，公司不宣称支持或授权；
- `Blocked`：不得加载。

manifest 还需声明 ONNX IR/opset/custom ops、shape/precision、支持的 execution provider 和 benchmark revision。安装时检查 digest/signature、external-data 路径穿越、custom-op allowlist、shape/size 上限，并在隔离 worker 做加载 smoke test；模型升级必须生成新的 artifact identity。

AI-BOM 可映射到 SPDX 3.0 AI Profile，签名可使用 Sigstore/cosign 的 blob verification；二者是导出与验证格式，不替代 Mondrian 自己的准入状态机（[SPDX AI Profile](https://spdx.github.io/spdx-spec/v3.0.1/model/AI/AI/)、[Sigstore verification](https://docs.sigstore.dev/cosign/verifying/verify/)）。

### 6.2 产品质量 Gate

所有分析型功能必须同时报告：

- 覆盖了哪些 source range；
- 哪些帧/词/区域低可信；
- 使用哪个 provider、模型和质量档；
- 是否发生降级，降级为何；
- 用户修正后哪些区间需要重算；
- 结果是否进入 author state、仅为 cache，或已 bake 为媒体。

### 6.3 工程 Gate

- bounded admission、cancel、shutdown、crash/restart；
- source fingerprint、project generation、author revision 的 stale 防护；
- 目标机器 p50/p95、峰值内存/显存、缓存增长和冷启动；
- Preview/Export 在相同 author semantics 下的一致性；
- 模型缺失、下载失败、驱动不支持、OOM 时有 typed failure，不伪装成功；
- 依赖更新不静默改变既有项目输出；模型升级产生新的 artifact identity。

### 6.4 立项/升级的量化门槛

以下只约束 Mondrian 冻结的目标素材集，不宣称是所有内容的行业指标：

| 能力 | 进入发布线的最低门槛 | 低于门槛时的动作 |
|---|---|---|
| ASR / 对齐 | 目标集普通话 CER ≤5%、噪声 ≤10%；静音幻觉 <0.2 词/分钟；词边界 p95 清洁 ≤120 ms、噪声 ≤200 ms | 保留多模型路由或限制支持语料，不用更大的写权限/自动删除掩盖识别错误 |
| Scene boundary | 硬切 F1 ≥95%、渐变 F1 ≥85%、误切 <0.5 次/分钟；一小时 1080p CPU 分析 ≤5 分钟 | TransNet 增益不足 3 个百分点时保留纯经典路径 |
| Planar tracking | 适用镜头中位 corner error ≤1 px、P95 ≤3 px@1080p；静默漂移 <2% | 优先补金字塔、前后向一致性和 UI，不立即换大模型 |
| 学习点跟踪升级 | 每 100 帧人工修正次数相对经典 provider 降低 ≥30%，12 GB GPU 可运行 | 留在内部 benchmark，不进默认模型包 |
| Roto | 中位人工修正 ≤1 次/100 帧；低可信区间能覆盖 ≥90% 灾难失败 | 停止全片传播承诺，缩短区间或保留实验功能 |
| RIFE Retime | 2× 慢放盲测优于 frame blend ≥70%；灾难 artifact 检出召回 ≥90% | 只保留 frame blend 或明确 beta |
| 2D Stabilize | 相对未稳定盲测偏好 ≥80%；与 Resolve 偏好率不低于 45%，额外裁切不高 3 个百分点 | 优化路径/裁切；不靠 mesh/flow 掩盖全局策略问题 |
| 联合搜索 | 500 条中文真实查询 Recall@10 ≥90%，索引后查询 p95 <100 ms | 优先改抽帧、OCR 去重和融合排序，不升级更大 embedding |
| 多机位同步 | 中位偏差 ≤10 ms、P95 ≤20 ms；2 小时残余漂移 ≤20 ms；错误自动同步 <1% | 标红低相关段并要求用户锚点，不强行同步 |
| Camera Solve | 适用镜头成功率 ≥80%，中位重投影误差 <0.7 px、P95 <1.5 px | 保持外部交换/spike，不产品化 |
| 云生成补镜 | 四个候选至少一个可用的项目比例 ≥60%，失败率 <5%，成本估计误差 <5% | 只保留外部 provider/导入，不做核心入口 |

## 7. 对原报告的具体修订清单

| 原结论 | 定案后的修订 |
|---|---|
| 光流是第一基础设施 | 改为 Analysis Artifact/Task 是基础设施；按需新增 MotionField，不统一所有运动表示 |
| P0 同时做 7 大方向 | 收敛为 ASR 文本剪辑、现有跟踪产品化、低成本分析底座 |
| P0 全 CPU | 改为基础编辑 CPU 独立；AI 有 CPU 保底和显式加速档，不承诺高质量模型全 CPU |
| 自研 planar tracker 2–3 人月 | 仓库已实现；改为 4–5 人月补金字塔、纠错层、质量 UI 和基准深化 |
| BootsTAPIR/LocoTrack 为学习跟踪首选 | 更新候选为 TAPNext/TAPNext++，但先用失败语料证明需要升级 |
| SAM 3 可作为近期开启项 | 默认锁定 Apache 的 SAM 2.1；SAM 3.1 仅作法务审查后的可选 provider |
| 全片 flow/depth 以 FP16 EXR 缓存 | 改为代理分辨率、按 ROI/时间 shard、预算 LRU；完整 manifest 才能发布成功 |
| Gyro 是巨头未覆盖的最大差异项 | Gyroflow 已覆盖多宿主；先外部工作流验证使用率，再决定是否承担格式维护 |
| ORT 独立进程并直接共享 GPU buffer | 首版独立进程 + CPU shared memory；零拷贝按平台和英雄功能单独验证 |
| Windows 固定 DirectML | 跟进 Windows ML 动态 EP；DirectML 为兼容后端，TensorRT/CUDA 为可选加速 |
| GLOMAP + VGGT 是 3D 近期路线 | 独立 GLOMAP 已归档；用 COLMAP 4.x incremental/global 双基线，VGGT-commercial/DA3/GLUEMAP 只做 spike |
| L0 DSL + Rhai 是既定 P0 | P2 先稳定 ID 属性链接；Rhai 不进逐帧热路径 |
| OFX beta 前完成，2–4 人月 | 只够 1–2 人月 spike；Windows CPU 可发布子集 12–18，跨平台/GPU/UI 累计 24–40+ |
| WASM + WGSL 片段注入融合 pass | 延后；GPU 插件先独立 pass、固定 ABI、验证与资源上限 |
| NC 模型可作 teacher 自研规避 | 删除默认建议，必须逐许可证与法务确认训练/输出权利 |
| 画质增强绑定一长串 SOTA | 先建真实退化评测与确定性修复；重型模型通过用户等待意愿和目标语料 gate |
| 智能音频全链路纯 MIT | 分开审代码、权重、对齐器和说话人模型；SenseVoice/pyannote 等均有独立条款 |
| 生成式已收敛为 2 秒 Extend | 补丁型仍适合 Mondrian，但竞品已扩展到视频、音乐、音效和 soundscape 生成 |
| Resolve 式 Relight 1–2 人月达 80% | 改称 Light Shaping；先 depth effects，再以用户盲测决定是否扩展 Relight |
| Mocha OEM 通道成熟 | 公开资料不足以证明可采购 OEM SDK；优先外部交换，内部 tracker 连续不达标才询价 |
| 端侧 + 买断是技术路线 | 改为 local-first 能力原则；商业定价与模型/API 成本独立决策 |

## 8. 最终建议

如果只能选一个近期胜负手，选 **“中文文本粗剪 + 台词/画面/OCR 联合搜索”**。它比 Camera Solve、3DGS、生成式补镜覆盖更多用户，模型和算力成本可控，也最能复用 Mondrian 已有的强事务时间线。

如果只能选一个视觉差异项，先把**现有平面 Mask 跟踪做到可救、可修、可重算**，再根据用户失败素材在 SAM 2.1 Roto 与 RIFE Retime 中选一个产品化。第二年的押注由真实使用数据在稳定/gyro 外部工作流与 Depth/Light Shaping 之间选择，不从零训练视觉大模型。

Mondrian 最可能超越竞品的地方，不是单次推理分数，而是：分析资产拥有精确来源与版本、失败可见、修正不丢、后台工作不伤播放、任何结果最终都通过同一 authoring transaction 和 Preview/Export 语义。这正是现有架构已经具备、而应继续放大的优势。
