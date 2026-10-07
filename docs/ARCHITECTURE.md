# ARCHITECTURE（架构设计）

- 目的：定义 SynthLM 进程/线程边界、数据流、IPC、核心数据结构、状态、错误恢复与安全边界，作为实现阶段的强制依据。
- 适用范围：acrd 侧车、REAPER 桥接、独立 UI、本地/云端模型链；REAPER 最低 v7.60。
- 状态：Accepted（人类 2026-10-06 确认实现依据；2026-10-07 补代理策略）
- 最后核验日期：2026-10-06
- 依赖文档：docs/research/A01–A06、B、C-models-retrieval-eval、C-dsp-toolchain、D-eng-eco；docs/DECISIONS.md（DEC-001–027）；docs/EVALUATION.md。

## 1 架构原则（呼应锁定决策与人类约束）

- P1 编排不宿主（L1）：管线=REAPER FX 链/容器/发送/文件夹，acrd 只发控制消息，永不加载第三方插件二进制。
- P2 参数只发值（L2）：不逆向 VST2，不 hook/patch 插件。
- P3 授权三档（L3 + 2026-10-06 人类决策）：Tier1 训练保留（muse-spark）/ Tier2 ZDR（mimo-v2.6-flash）/ Tier3 本地（不上传）；首次弹窗 + 设置页可改；原始音频默认不出网。
- P4 检索 + 受限 Patch + 搜索（L4）：禁从零预测参数为主路径。
- P5 先 LUFS 归一再评分、多目标（L5，无条件）。
- P6 候选 3–5 + 差异摘要（L6，无条件）。
- P7 采样=派生编辑 + 采样器宏（L7）：禁碰第三方采样器内部音色。
- P8 模型/分析永不进 DAW 进程（L8）：全在 acrd；进程间只传控制消息与缓冲句柄。
- P9 音频线程零分配零锁零网络零日志零阻塞（L9，无条件）。
- P10 Profile 是核心资产（L10）：优先级高于 UI 与模型。
- P11 事实断言带来源 + 日期（L11）：不确定标 ⚠️需实测。
- P12 内部非商业无分发不购证（人类 2026-10-05/06）：GPL/NC 仅内部运行；Key 仅 `.env`。

## 2 组件图与进程/线程边界

进程：
- `reaper.exe`（用户进程）：`bridge` 扩展（reaper-rs medium 为主、low 兜底 Take/原始值/CopyToTake，A01 §8）跑 main thread；只做链/参数/包络/undo/快照读写，不做模型/分析/渲染等待。
- `acrd`（侧车守护进程）：`planner`（Patch 规划）/ `retrieval`（USearch 或 LanceDB 二选一，原型后锁）/ `eval`（ruststft + ebur128/stream + CLAP cosine）/ `dsp`（symphonia 主 + FFmpeg 兜底、rubato、timestretch/RB 未购证仅内部、Demucs ONNX 批处理）/ `model-gw`（三档路由）。
- `synthlm-ui`（独立 egui 窗，主 UI）+ 可选 REAPER 内 ReaImGui 薄面板（快捷入口）。

线程（bridge 侧）：
- main thread：唯一可调工程变更 API 的线程（AddByName/SetParam/Undo/Create/Split/P_EXT/NCH）。
- deferred/gmem：仅控制率协作，无原子保证，不做平滑主体。
- audio hook（若用）：只读原子/拷贝，禁一切工程 API（`SetParam` 音频线程按禁止处理，A05/A06）。

依赖方向（DEC-022）：`common` ← `profile` ← `planner`/`retrieval`/`eval` ← `acrd` ← `bridge` ← `ui`；bridge 禁止依赖模型/分析；`unsafe` 仅 low 封装 + 安全论证注释。

## 3 数据流（一次完整交互时序）

1. UI 采集：自然语言意图 + 参考音频（本地文件/工程内 take 指纹）+ 授权档读取（未授权即弹窗三选一）。
2. bridge 显式快照（DEC-004/008）：参数表（ident 键）+ chunk + take GUID/hash + RENDER_* 备份，打包 `Snapshot` 发 acrd（IPC）。
3. acrd：MIR 化（48k/STFT 2048/512/80 mel/CLAP512/LUFS，DEC-012 v1）→ 多路召回（文本/音频各索引 + RRF）→ model-gw 按授权档选 Tier（Tier1/Tier2 responses 结构化 / Tier3 本地兼容端点，先做音频能力探测）→ Patch Plan（whitelist role only，ident path）。
4. acrd 校验：serde + json-patch RFC6902 应用前校验 + 白名单审计（含上传字段白名单单测门禁），失败进修复循环（≤2 轮）。
5. bridge 应用：main-thread 合并队列 30–60Hz 差分写入，一求解一 undo 点（`BeginBlock2(0)…dirty逐轨…EndBlock2(0,desc,-1)`），写后重取指针（A04 指针失效教训）。
6. 渲染评价：42230 固定块 64 + 固定 bounds 渲染候选 → LUFS 归一到 -14 → 谱/mel/CLAP/瞬态多目标评分 → 去重（距离阈值）→ 3 候选 + 一句话差异。
7. UI 呈现：卡片 6 字段（差异句/置信度/ΔLUFS/改动数/试听/应用-回滚）；应用默认整链快照粒度；失败给理由 + BLOCKED。
8. 审计记账：模型/授权档/上传字段清单/字节数（不记 Key/PCM）；反馈只存本地（DEC-017）。

## 4 音频路径与线程模型

- 可做（main thread）：一切 `TrackFX_*/TakeFX_*`、`GetSet*Chunk`、`InsertEnvelopePoint`、`Undo_*`、`Split/AddTake/SetActiveTake`、`P_EXT`、`I_TAKEFX_NCH`（先 `TakeFX_AddByName(take,name,-1000)` 建实例再设，`0` 仅查询，A04 spike04）、`I_MIXFLAG` 仅 ≥7.81（`APIExists` 守卫）。
- 不可做（audio 回调/任何非 main 线程）：上列全部 + `Create/DestroyAudioAccessor`（官方 main-only）+ 传输控制（UI-only）；`Audio_IsRunning/IsPreBuffer` 可读（threadsafe）。
- 平滑二段式：acrd/bridge 只发 30–60Hz 控制率目标 → 插件内 smoothing 做采样插值；ReaScript/OSC 不担逐采样职责。
- 渲染 determinism：固定块 + 固定 bounds + null-test 门禁；全速失败兜底小块→1x→online；候选对比失配即 BLOCKED（H2）。

## 5 IPC 协议（DEC-023）

- 传输：interprocess local socket（Win named pipe / Unix UDS，短路径 + 启动清理）；大数据走共享内存 + 事件通知；WS 仅调试只读。
- 帧：length-prefix + `ver` + `type` + `body`；首版 JSON（稳定后 prost 可选）；`hello{version}` 握手，major 破坏性变更双版本兼容一期。
- 消息类型：`snapshot.submit` / `plan.request|response` / `patch.apply|result` / `render.request|result` / `score.report` / `consent.get|set` / `audit.event` / `error`（`code, retryable, detail`，禁 Key/PCM）。
- 错误语义：`retryable=true` 进退避重试（云端 30s 超时 + 2 次）；`retryable=false`（鉴权/白名单越界/音频能力缺失）直接 BLOCKED + 指引；`consent_required` 缺授权即弹窗。
- 超时：云端 P95 预算 10s/候选；超限熔断 Tier1→Tier2→Tier3→缓存→BLOCKED。
- 代理策略（2026-10-07 定稿，TSK-702 矩阵回归锁定）：
  - 默认跟随系统代理：非 loopback 云端 Base URL 走 reqwest 系统代理（`http_proxy`/`https_proxy`/`HTTP_PROXY`/`HTTPS_PROXY`；`no_proxy`/`NO_PROXY` 豁免由 reqwest 语义执行），企业出口保持可用。
  - 绝不经代理：loopback（`127.0.0.1`/`localhost`/`::1`/`0.0.0.0`，含 stub 与本地探测）由 `HttpsTransport::new` 硬旁路直连，`new_hermetic` 在任何 base 下都不走代理——否则本机代理会把 TCP refuse 合成 502/503，把 `ConnectionFailed` 误分类为 `ServerError`；Tier3 本地端点不经 `HttpsTransport`，`.env` 内网地址自然不出网（或走 `NO_PROXY` 豁免）。
  - `SYNTHLM_NO_PROXY`-style 显式覆盖留待后续任务，本次只跟随 + 声明，不新增 env 键。
  - 测试铁律：loopback stub 测试一律 `new_hermetic`（或等价显式注入），禁读写进程级代理变量；代理矩阵（开/关 × 拒绝/超时/401）回归见 `planner::model_gw` 单测。

## 6 核心数据结构

- `PluginProfile { schema_version, fx_ident_match, groups[], params[{ident, name_regex?, options?[], role, ui, scale, group}] }`：ident 持久化，裸索引永不落盘；`name_regex` 存文本、加载即编译校验（非法 fail-closed）；`options` 仅 `select` 可带（枚举标签集，无真值不编）；Kontakt slot 子集；矩阵类 `preset-only`（B §4，DEC-015）。
- `PatchPlan { snapshot_id, ops[{op, ident, value, reason_role}], model_tier, consent_tier }`：RFC6902 子集 + whitelist 角色枚举；云端/本地共用一 schema（DEC-013）。
- `Candidate { id, patch, render_ref, score, diff_summary_zh, confidence, delta_lufs, changed_params }`：3–5 个，去重阈值 + 强制方向覆盖（DEC-018）。
- `Score { spec_l1, mel_l1, clap_cos, transient_f1, lufs_i, true_peak, delta_lufs }`：LUFS 归一后算分；`+6dB` 回归门禁（DEC-016）。
- `Feedback { winner_id, diff, score_snapshot, audio_fingerprint }`：本地 only，不存 PCM 全文（DEC-017）。

## 7 状态与持久化（DEC-027）

- 用户目录（`%APPDATA%/SynthLM` + XDG 对应，`config_version` 迁移）：配置/授权档/模型缓存/索引/产物缓存（内容寻址 + 50GB 水位 GC）。
- 工程内（小）：`SetProjExtState` 只存指针/小快照（base64 单行）；`P_EXT:SYNTHLM_*` 存 provenance（跟 chunk、进 undo、复制跟随，A04 实证）；`SetExtState` 只存偏好（单行）。
- 大快照/产物一律外部缓存 + 工程存指针；`.rpp` 体积回归（MB 级读写耗时）列入 TSK。
- 迁移：版本化 `migrate()`，不可写时回退工程相对目录 + 显式提示。

## 8 错误处理与崩溃恢复

- 分级：`BLOCKED`（缺授权/缺 Key/音频能力缺失/白名单越界，附候选方案）/ `RETRYABLE`（超时/渲染抖动，进退避）/ `FATAL`（不可逆残留风险，冻结写操作，K3）。
- bridge 永不抛异常穿透 REAPER：`Begin` 未配对 `End` 即 bug（CI 门禁）；删轨/undo 后指针一律重取（ValidatePtr2 实证）。
- acrd watchdog：心跳超时 kill→重拉→按快照 journal 重放；任务状态机（pending/running/done/failed）+ 参数快照周期落盘（WAL + tmp+rename 原子写）。
- Demucs/stem/大渲染只进后台队列，失败不堵交互链；预览轨隔离保证用户轨不动。

## 9 可观测性（DEC-026）

- 遥测默认关（opt-in + 内容清单 + 可审计）；日志禁 PCM/prompt 全文/绝对路径（指纹 + 相对路径）；审计事件本地文件（含模型/授权档/字段清单/字节数）。
- 指标：Patch 有效率、云端 P95、渲染 null-test 方差、undo 残留计数、缓存命中率；任一超阈即对应 RSK 触发。

## 10 安全边界

- 不加载未知二进制（L1/P1）：acrd 不 `dlopen` 第三方插件；解码/推理只用登记依赖（LICENSES 登记制，DEC-025）。
- 不逆向/hook/patch（L2）；VST2 只发参数值。
- 出网默认关（P3）：三档授权外一律不出网；云端请求体最小字段 + 白名单审计单测；Key 只读 `.env`（已 gitignore），缺失即 BLOCKED。
- 不静默删/覆盖：音频一律派生新文件 + provenance；应用必经 undo 事务 + 整链快照（K3）。
- 许可红线：GPL/AGPL 未购证与 NC 权重仅内部运行；出现分发物即冻结重审。

## 11 演进路线（Phase 3 可重写模块）

- 允许重写：检索后端二选一落定（USearch↔LanceDB）、评价器权重（标定后）、UI 引擎（egui↔iced 落定）、本地模型档（按需升级）。
- 禁止重写（除非 ADR + 人类）：进程边界（L8）、音频线程红线（L9）、LUFS 先行（L5）、候选数与摘要（L6）、Profile 资产地位（L10）、三档授权与审计（DEC-010/011）。
- 技术债偿还：每阶段留 20% 带宽清 ⚠️需实测 TSK；ARCH 与 TASK-INDEX 改动同步（文档 slaves 代码）。
