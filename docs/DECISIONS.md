# DECISIONS（ADR）

- 目的：基于 Research Pass（A/B/C/D）定稿 ≥25 条决策，每条给具体默认值与反转条件，供 ARCHITECTURE/ROADMAP/TASK-INDEX 约束实现。
- 适用范围：SynthLM 全工程；约束条件：非商业、无分发、不上传、不购买许可；目标最低 REAPER v7.60，实测基线 v7.82。
- 状态：Accepted（人类 2026-10-06 全部确认；DEC-010 Tier3 本地模型于 2026-10-06 由 Gemma 4 12B 更换为 Bonsai-2-27B，Gemma 废弃）
- 最后核验日期：2026-10-06
- 依赖文档：docs/research/A01–A06、B-plugin-semantics、C-models-retrieval-eval、C-dsp-toolchain、D-eng-eco。

> 格式硬要求：无“视情况而定”；每条有推荐默认值 + 可观察反转条件。核验依据引用 research 文档与 spike。

## 形态与集成

DEC-001：集成形态（侧车 + REAPER 编排）
状态：Accepted（人类 2026-10-06 确认）
背景：L1 不做插件宿主、L8 模型永不进 DAW 进程，必须选编排主体。
选项：A 侧车 acrd + REAPER 做 FX/路由执行面（L1/L8 原生）；B 独立 App 自带混音（重造 DAW，否决）；C 纯 ReaScript（性能/undo/类型安全不足）。
推荐：A。acrd 做规划/检索/评价/渲染编排，REAPER 只执行链/容器/发送变更。
反转条件：实测证明容器地址脆弱致任务失败率 >20% 且展平回退仍失败。
影响面：REQ 编排、RSK 容器脆弱、TSK 桥接层。
核验依据：A02（容器可编排但易碎）、A05（Extension 主写）、L1/L8。

DEC-002：UI 载体（独立窗口为主 + Lua 薄面板）
状态：Accepted（人类 2026-10-06 确认）
背景：D 组证明 FX 嵌入只适合小显，Lua 面板天花板低。
选项：A 独立 egui 窗 + 可选 ReaImGui 快捷面板；B FX 嵌入全功能（否决，LICE 位图限制）；C Tauri（否决，体积/更新器成本）。
推荐：A。主 UI 独立 egui（100Hz 状态原型后锁 vs iced 二选一定案），REAPER 内只留轻入口。
反转条件：egui 原型波形 + 100Hz 刷新 + HiDPI 任一不达标则切 iced。
影响面：REQ-UI、TSK-ui-proto。
核验依据：D-eng-eco §1。

DEC-003：图与管线表达（REAPER 为真源 + 自有描述器单向生成）
状态：Accepted（人类 2026-10-06 确认）
背景：容器地址 stride 脆弱，不可双向同步裸索引。
选项：A REAPER 为真源，自有图只做生成器（持久化 TrackGUID/FXGUID/容器路径，每次重算）；B 双向同步（否决，漂移不可收敛）；C 纯自有图（否决，与 L1 冲突）。
推荐：A。附展平回退（容器异常→串行轨 + 发送）。
反转条件：GUID 稳定性实测六组全过且 stride 封装失败率 <5%，才考虑弱双向缓存。
影响面：A01/A02、TSK-addr-layer。
核验依据：A01 §4、A02 §2.3。

DEC-004：与工程同步粒度（显式快照）
状态：Accepted（人类 2026-10-06 确认）
背景：实时监听 undo/指针失效风险高（A04 已证 Undo 后指针失效）。
选项：A 显式快照（求解前冻结参数/chunk/GUID+hash）；B 实时监听（否决，dirty/undo 噪音）。
推荐：A。每次求解前快照，求解中不跟随外部编辑，完成后 diff 提示。
反转条件：快照耗时 >2s 且用户明确要跟随模式（opt-in）。
影响面：A04 §3、TSK-snapshot。
核验依据：A04（ValidatePtr2=false）、A03。

## 音频与实时

DEC-005：离线渲染候选路径（当前工程 42230 固定小块）
状态：Accepted（人类 2026-10-06 确认）
背景：无单步 Render API，只有 RENDER_* + Main_OnCommand。
选项：A 当前工程设 RENDER_* 后 42230（强制关窗）固定块 64/固定 bounds；B temp tab 渲染（焦点/undo 污染）；C 命令行 -renderproject（离线批处理 only）。
推荐：A 为交互主路径；C 为批量兜底；B 仅隔离实验。
反转条件：42230 在目标机阻塞/弹窗/完成检测 >5s，则切 B + 轮询 EnumProjects。
影响面：A03 §3–4、TSK-render-harness。
核验依据：A03 spike 结论、spike 待测 M7。

DEC-006：预览播放隔离（独立 candidate 轨 + mute 切换）
状态：Accepted（人类 2026-10-06 确认）
背景：冻结 selection-based 不适合高频预览；不能动用户轨。
选项：A 独立 preview bus/candidate 轨，源 mute，用户轨不动；B Monitoring FX（否决，污染 Ping/PDC）；C 原轨 solo 切换（否决，破坏混音）。
推荐：A。候选各占一轨/容器快照，A/B 对比只切 mute/solo，不改用户链。
反转条件：轨数 >16 致工程卡顿，则切容器内快照 + 单轨切换。
影响面：A02/A03、REQ-preview。
核验依据：A02 §5、A03 §3.5。

DEC-007：参数写入平滑节流（main-thread 队列 + 30–60Hz 差分）
状态：Accepted（人类 2026-10-06 确认）
背景：ReaScript/OSC 无采样级能力，defer 约 30Hz。
选项：A Extension main-thread 合并队列，差分 + 30–60Hz 节流，插件内 smoothing 做采样插值；B 逐 tick 全量写（否决，undo/GUI 噪音）；C OSC 主写（否决，float32 + 无事务）。
推荐：A。
反转条件：单 undo 块 500 参数端到端 >1s，则加脏标记合并 + 分片提交。
影响面：A06、TSK-param-queue。
核验依据：A06 §3–5、A05。

DEC-008：undo/redo 事务边界（一求解一 undo 点 + 脏标记 + 重取）
状态：Accepted（人类 2026-10-06 确认）
背景：A03/A04 已证 Begin/End + dirty + 指针失效。
选项：A `Undo_BeginBlock2(0)…MarkTrackItemsDirty(逐轨)…Undo_EndBlock2(0,desc,-1)`，读写后重取指针；B extraflags=0（否决，纯 state 易丢点）；C 跨 tab 原子（否决，无隔离保证）。
推荐：A。desc 稳定命名 `SynthLM: <op> Nparams`；MIDI 必 dirty（v7.60），pooled 多标轨。
反转条件：profiler 证明 -1 致撤销延迟 >500ms，才细化掩码（FX|TRACKCFG 优先）。
影响面：A03 §1、A04 §3、TSK-undo-harness。
核验依据：spike03c/f（T1_delta=1、指针失效）。

DEC-009：大文件/stem 后台任务与缓存（内容寻址 + 懒下载）
状态：Accepted（人类 2026-10-06 确认）
背景：Demucs CPU 分钟级、GPU 秒级，模型 166MB–1.26GB，不能进实时链。
选项：A 后台任务队列 + 外部内容寻址缓存 + 工程只存指针 + GC；B 工程内嵌产物（否决，.rpp 膨胀）；C 实时 stem（否决）。
推荐：A。Demucs 走 ONNX 侧车/本地批处理（不上传），大状态存 diff。
反转条件：缓存命中率 <50% 且磁盘 >50GB，则收紧保留策略（只留胜出者）。
影响面：C-dsp §5、A03、TSK-cache-gc。
核验依据：C-dsp-toolchain §5。

## 模型

DEC-010：模型选型（云端 OpenCode Go 为主 + 本地端点保留）
状态：Accepted（人类 2026-10-06 确认）
背景：2026-10-06 人类决策覆盖此前本地主路径：在性能与资源占用权衡下弃用本地模型为主路径，但保留本地端点支持。L3 反转条件（用户明确接受上传）被本次决策触发，仅限模型推理链。
选项：A 云端主路径（OpenCode Go 预设）+ 本地 OpenAI 兼容端点保留为可切换后端；B 纯本地（否决，人类已否决为主路径）；C 云端唯一无回退（否决，无降级）。
推荐：A。云端预设：Base URL `https://opencode.ai/zen/go/v1`，Tier1 Model `muse-spark-1.3-contributor`（Muse Spark 1.3 Contributor，responses 格式，条款含保留数据训练模型，人类已确认）；Tier2 Model `mimo-v2.6-flash`（MiMo-V2.6-Flash，ZDR 协议，用于不接受训练保留但接受上传）；Tier3 本地模型定为 `Bonsai-2-27B`（唯一候选，2026-10-06 接替已废弃的 Gemma 4 12B；后端收束为 llama.cpp only，vLLM/MLX/LM Studio 不再覆盖，2026-10-06）。Tier3 仅文本（Bonsai-2 仅 text+image 输入；llama.cpp 端点多模态输入实测不可用，TSK-305）；参考音频的语义理解只走 Tier1/Tier2（见 DEC-011 音频字段规则）。授权三档：首次启动弹窗三选一（接受训练保留 / 仅接受上传 / 不上传），设置页可改；不接受保留→Tier2，不接受上传→Tier3。API Key 只从 `.env` 读取，禁止进代码/日志/快照；不声明计费。本地端点启动时做音频输入能力探测，不支持即报错（BLOCKED + 指引），不静默降级为纯文本。上传内容默认仅提示词 + MIR 特征/候选元数据，原始音频默认不出网（见 DEC-011/017）。
反转条件：云端连通性实测连续失败率 >20%、或延迟 P95 >10s、或用户撤回上传授权，则切 Tier3（Bonsai-2-27B 本地）为主；Tier3 本地模型更换须人类另行拍板（当前唯一候选）。
影响面：DEC-011/013/017、L3、TSK-model-harness。
核验依据：人类 2026-10-06 决策（用户给定预设，置信度高）；连通性与 responses 格式 ⚠️需实测；C-models-retrieval-eval §1-2（本地档保留依据）。

DEC-011：云端后端协议与降级（OpenCode Go responses + 本地回退）
状态：Accepted（人类 2026-10-06 确认）
背景：2026-10-06 人类启用云端为主（覆盖此前默认关闭）；L3 要求显式开关 + 明示上传内容 + 可审计仍然有效。
选项：A HTTPS POST 到 Base URL（responses 格式，`model=muse-spark-1.3-contributor`），超时/重试/熔断 + 失败降级到本地端点 → 缓存 → BLOCKED；B 云端无降级（否决）；C 静默上传原始音频（红线否决）。
推荐：A。HTTPS POST 到 Go 面按模型端点（Tier1 `{base}/responses` / Tier2 `{base}/chat/completions`，2026-10-06 实测），必带稳定 `x-opencode-session` 会话头（缺失即系统性 400；自定义 UA 曾两次相关 401，故保持默认 UA），超时/重试/熔断 + 失败降级链 Tier1→Tier2→Tier3（本地）→缓存→BLOCKED；授权档存设置（首次弹窗 + 设置页可改），每次云端调用记审计日志（时间/模型/授权档/上传字段清单/字节数，不记 Key 与音频 PCM）；上传字段白名单配可执行审计单测（允许 prompt/MIR/元数据/音频摘要字段 `audio_ref`（仅 Tier1/Tier2，单次摘录，随调用审计），PCM 原始文件默认禁，越界即测试失败）；Tier3 永不上传音频（本地文本规划 only）；`.env` 缺 Key 即 BLOCKED 并指引，不猜测。原始音频默认不出网，几何/特征级上传需在调用前明示。
反转条件：用户撤回上传授权或审计发现超范围字段上传，即刻切断云端并转本地。
影响面：DEC-010/013/017/026、L3、TSK-cloud-stub。
核验依据：人类 2026-10-06 预设（URL/模型名原文引用）；连通性 ⚠️需实测（含 401/超时/重试路径）。

DEC-012：音频→结构化中间表示（MIR schema v1）
状态：Accepted（人类 2026-10-06 确认）
背景：评价可比性要求 decode→声道→重采样→STFT→LUFS 全固定。
选项：A 固定 48k + STFT（Hann 2048/hop 512）+ 80 带 log-mel + CLAP512 + LUFS-I/TP/LRA + 瞬态包络，JSON schema 版本化；B 浮动参数（否决，不可比）；C Essentia 全家（否决，AGPL 待复核）。
推荐：A。`mira v1` 字段冻结，变更走 schema 升版。
反转条件：听感相关性标定证明 mel 带数/窗长显著偏离，才升 v2。
影响面：C-models §4、TSK-mir-schema。
核验依据：C-models §4、L5。

DEC-013：JSON Patch schema 与约束解码（云端 responses 结构化 + 本地 llama-server 回退 + 双校验）
状态：Accepted（人类 2026-10-06 确认）
背景：主路径云端 responses 格式，本地端点保留；Rust FFI 仍不用。
选项：A 云端请求带结构化输出约束（responses 格式 + Patch JSON Schema）+ 本地端点用 llama-server `json_schema` 约束，两侧统一经 Rust 侧 serde + json-patch RFC6902 应用前校验 + 失败修复循环；B 进程内 FFI（否决）；C 无约束自由文本（否决）。
推荐：A。Patch 只含 whitelist role 参数，path 用 ident（FromIdent 回查，-1 迁移除）；云端与本地共用同一 schema 文件与校验器。
反转条件：云端结构化有效率 <95% 经 2 轮修复仍不达标，则收紧 schema（缩枚举/拆步）或切本地端点对照。
影响面：DEC-010/011、C-models §2、B §4、TSK-patch-schema。
核验依据：C-models §2；云端 responses 字段 ⚠️需实测。

DEC-014：检索索引构建时机（首次全量 + 增量 + 懒嵌入）
状态：Accepted（人类 2026-10-06 确认）
背景：CLAP/ST 跨空间不可单索引。
选项：A 首次扫描全量向量化（后台）+ 文件变更增量 + 音频嵌入懒算；B 每次求解全量（否决，慢）；C 云端索引（否决，不上传）。
推荐：A。文本/音频各一索引（USearch 或 LanceDB 二选一，原型后锁），结构化走 payload 过滤，查询多路 RRF 融合。
反转条件：10k 预设首次构建 >30min，则切采样构建 + 按需补齐。
影响面：C-models §3、TSK-index-build。
核验依据：C-models §3。

## 数据与评价

DEC-015：Plugin Profile 存储（JSON schema 版 + ident 键）
状态：Accepted（人类 2026-10-06 确认）
背景：裸索引漂移，ident 才可持久化。
选项：A 每插件 `profile.json`（`{schema_version, fx_ident_match, groups, params[{ident,role,ui,scale}]}`），用户目录版本化 + 工程 P_EXT 存指针；B 全量裸索引（否决）；C 二进制私有格式（否决，不可审）。
推荐：A。出厂 8–16 宏，Kontakt 只收 slot 子集，矩阵类标 `preset-only`。
反转条件：profile 损坏率 >1% 或加载 >500ms，则切分片 + 校验和。
影响面：B §4、TSK-profile-schema。
核验依据：B-plugin-semantics §4。

DEC-016：评分权重初始化与校准（LUFS 归一后等权 + 防作弊回归）
状态：Accepted（人类 2026-10-06 确认）
背景：L5 无条件（先归一再相似度，多目标）。
选项：A 先测 Integrated LUFS→增益到 -14→再算谱 L1 + log-mel + CLAP cosine + 瞬态 F1，等权起步，上报 ΔLUFS/dBTP（>-1 扣分），+6dB 必不涨分回归；B 直接比响度（否决，作弊）；C 单指标（否决）。
推荐：A。权重经听感标定调，不拍脑袋。
反转条件：标定证明某指标与听感负相关，则降权或剔除。
影响面：L5/L6、C-models §4、TSK-eval-calib。
核验依据：C-models §4、L5。

DEC-017：反馈采集范围与隐私（只存本地；推理上传走三档授权）
状态：Accepted（人类 2026-10-06 确认）
背景：人类明确反馈不出网；模型推理上传另由三档授权管控（2026-10-06）。
选项：A 只存本地（胜出者 + diff + 评分，不存音频内容，只存指纹/路径），脱敏，opt-in 才记；B 云端回流（否决）；C 默认全量记（否决）。
推荐：A。
反转条件：用户书面要求云端个性化（与 D11 同条件）。
影响面：L3、TSK-feedback-store。
核验依据：人类约束 + D-eng-eco。

DEC-018：候选多样性（3–5 + 距离去重 + 差异句式）
状态：Accepted（人类 2026-10-06 确认）
背景：L6 无条件（3–5 + 差异摘要）。
选项：A 生成后按参数/CLAP 距离去重（阈值），强制覆盖不同方向（更暗/瞬态/空间），每候选一句话差异；B 纯 top-k（否决，同质）；C >5（否决，5 分钟做不完）。
推荐：A。候选数默认 3，可配 5。
反转条件：用户盲听证明多样性与可用性负相关，才收紧到 3 纯优。
影响面：L6、TSK-diversity。
核验依据：L6、C-models §5。

## UX

DEC-019：候选卡片密度（固定 6 字段）
状态：Accepted（人类 2026-10-06 确认）
背景：信息过多拖慢 5 分钟闭环。
选项：A 固定字段：差异句/置信度/ΔLUFS/改动参数数/试听按钮/应用-回滚；B 全参数表（否决）；C 纯句话（否决，不可审计）。
推荐：A。
反转条件：可用性测试证明字段不足致误应用 >10%，才加字段。
影响面：REQ-UX、TSK-ui-cards。
核验依据：成功标准（5 分钟 3 候选）。

DEC-020：应用粒度（整链快照默认 + 单插件/单参数组可选）
状态：Accepted（人类 2026-10-06 确认）
背景：原子回滚要求整链可还原。
选项：A 默认整链快照应用（一 undo 点），可选单插件/单参数组（同样一 undo 点）；B 单参数直接写无快照（否决，不可回滚）；C 多 undo 点（否决，噪音）。
推荐：A。全部走 DEC-008 事务 + 派生文件 + provenance。
反转条件：整链快照 >2s，则默认切单插件粒度。
影响面：DEC-008、TSK-apply-granularity。
核验依据：A03/A04、工程红线。

DEC-021：失败与不确定性呈现（置信度 + 为何改 + BLOCKED）
状态：Accepted（人类 2026-10-06 确认）
背景：禁止猜测，遇未决必须停。
选项：A 每候选置信度 + 改参理由（role 级），失败给候选方案 + BLOCKED，不静默降级；B 静默最佳（否决）；C 堆栈暴露（否决）。
推荐：A。
反转条件：用户明确要极简模式（隐藏理由，仍保留日志）。
影响面：AGENTS 变更流程、TSK-ux-states。
核验依据：第 7 节工作流契约。

## 工程与合规

DEC-022：workspace 划分与 crate 边界（单向依赖）
状态：Accepted（人类 2026-10-06 确认）
背景：L8 进程边界 + 音频线程红线必须在依赖方向体现。
选项：A crates：`common`（类型/错误）← `profile` ← `planner`/`retrieval`/`eval` ← `acrd`（sidecar）← `bridge`（reaper-rs low/medium 封装）← `ui`（egui）；音频/DSP 只在 acrd/eval，bridge 禁止模型/分析依赖；B 大单 crate（否决，边界漏）；C bridge 依赖模型（否决，违反 L8）。
推荐：A。`unsafe` 仅 bridge/low 封装，须安全论证注释。
反转条件：编译增量 >5min 或循环依赖无法解，才合并相邻叶 crate。
影响面：ARCHITECTURE、TSK-skeleton。
核验依据：L8/L9、A05。
补记（2026-10-06，TSK-205）：新增 `dsp` 叶 crate（stem 队列/内容寻址缓存/GC），仅依赖 `common`，`acrd` 后续可依赖；方向与本 DEC 单向性一致，不触发反转。

DEC-023：IPC 协议演进（version 握手 + JSON 首版）
状态：Accepted（人类 2026-10-06 确认）
背景：跨平台 + 长周期演进。
选项：A interprocess local socket + length-prefix 帧 + `hello{version}` 握手 + 首版 JSON（稳定后 prost 可选）+ 超时/重连/幂等；B 无版本裸流（否决）；C 首版 gRPC（否决，过重）。
推荐：A。semver，破坏性变更升 major + 双版本兼容一期。
反转条件： профилирование 证明 JSON 编解码占延迟 >30%，才切 bincode/prost。
影响面：D §2、TSK-ipc-proto。
核验依据：D-eng-eco §2。

DEC-024：测试策略（单元 + golden 音频 + mock REAPER + spike）
状态：Accepted（人类 2026-10-06 确认）
背景：可度量成功标准 + L11 禁止印象 API。
选项：A 单元（逻辑）+ golden 音频回归（固定输入→固定评分区间，含 +6dB 防作弊）+ bridge 用 mock REAPER API + 真机 spike（experiments/，不进主干验收）；B 纯单元（否决，音频不可信）；C 真机全量（否决，脆弱）。
推荐：A。CI 跑 lint/test/doc，golden 失效应 BLOCKED。
反转条件：golden 抖动（同机三次方差超阈）无法收敛，则放宽区间 + 人工听辨仲裁。
影响面：CI、TSK-test-harness。
核验依据：A03 §4、L5/L11。

DEC-025：许可与开源策略（内部非商业无分发，登记制，不购证）
状态：Accepted（人类 2026-10-06 确认）
背景：人类明确非商业/无分发/不上传/不购买。
选项：A 内部使用：GPL/AGPL/NC（含 RB 未购证、Demucs 官方权重、MERT/MuQ）仅限本机运行与研发，不分发即不触发传染/商用限制；新增依赖必须登记 docs/LICENSES.md；商用/分发需重审；B 购买豁免（否决，无预算）；C 无视许可（红线否决）。
推荐：A。FFmpeg 用 LGPL 构建（`buildconf` 确认），symphonia MPL 合规声明保留，VST3/CLAP/iPlug2 文本随包。
反转条件：任一“无分发”前提被打破（对外给二进制/镜像/权重），立即冻结并走法务重审。
影响面：C/D 许可矩阵、TSK-license-register。
核验依据：人类约束 + C/D 许可表。

DEC-026：遥测与日志脱敏（默认无遥测，日志禁音频内容）
状态：Accepted（人类 2026-10-06 确认）
背景：L3 + 不上传。
选项：A 遥测默认关（opt-in + 内容清单 + 可审计），日志禁音频 PCM/文本 prompt 全文/绝对路径（指纹 + 相对路径），错误上报本地文件；B 默认上报（否决）；C 全量 debug 日志（否决，膨胀 + 泄漏）。
推荐：A。
反转条件：稳定性 NARCIS 要求远程诊断，且用户书面 opt-in，才开最小遥测。
影响面：AGENTS 日志隐私、TSK-log-scrub。
核验依据：L3、D §3。

DEC-027：配置/状态存放与迁移（用户目录 + 工程指针 + 版本化）
状态：Accepted（人类 2026-10-06 确认）
背景：A03（ProjExt vs ExtState）+ 膨胀控制。
选项：A 配置/模型缓存/索引放用户目录（`%APPDATA%/SynthLM` + XDG 对应）版本化迁移（`config_version` + 自动 migrate），工程内只存 P_EXT 指针 + provenance，大产物外部缓存 + GC；B 全塞 .rpp（否决，膨胀）；C 全放工程外无指针（否决，丢关联）。
推荐：A。`SetProjExtState` 只存指针/小快照（base64，单行），`SetExtState` 只存偏好（单行），大快照先测体积。
反转条件：用户目录不可写（便携/权限），则回退工程相对目录 + 显式提示。
影响面：A03 §2、DEC-009、TSK-state-migrate。
核验依据：A03 spike、A04 P_EXT。
