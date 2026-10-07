# M0-7-REPORT（Phase 0–7 合并终验报告）

- 目的：作为仓库唯一的阶段报告真源，记录 Phase 0 立项关闭、M0–M4 工程闭环终验、M5–M7 交付核验的完成项、验证结果、新事实、决策修正、残留移交，以及 2026-10-07 整改与门禁现状。
- 适用范围：Phase 0–7（ROADMAP M0–M7；任务号 TSK-0xx/1xx/2xx/3xx/4xx/5xx/6xx/7xx）；Phase 8 在办行（能力接线与债清偿，TSK-805–808）见 `docs/TASK-INDEX.md`，不在本报告关闭范围内。
- 状态：Accepted（Phase 0 于 2026-10-06 关闭；Phase 0–4 与 Phase 5–7 终验均已落盘；2026-10-07 三份阶段报告合并为本文件并追加 §7）
- 最后核验日期：2026-10-07
- 依赖文档：docs/TASK-INDEX.md（Phase 0–7 计 64 Done、0 活动行，活动行集中在 Phase 8；F/B-001/002 冻结）、docs/ROADMAP.md、docs/DECISIONS.md（27 DECs，全部 Accepted 有效）、docs/EVALUATION.md、docs/ARCHITECTURE.md、AGENTS.md、scripts/m4_gate.py。

合并说明（2026-10-07）：本文件由原「Phase 0 阶段报告」「M0–M4 终验报告」「M5–M7 终验报告」三份文档合并而成，三份原件已删除；原有 2026-10-07 更正记录按日期保留在 §6，不改写历史，只追加更正。

## 1 Phase 0 关闭（立项、调研与设计）

### 1.1 完成

- research 11 篇（A01–A06、B、C-models、C-dsp、C-live-endpoint、D；2026-10-07 复核更正：Phase 0 关闭当时计 10 篇，后续新增 `docs/research/C-live-endpoint.md` 至 11 篇）+ DECISIONS 27 条（Accepted）+ EVALUATION（H6/RSK14）+ ARCHITECTURE（11 章）+ ROADMAP（Phase 0–4 + M0–M4）+ TASK-INDEX（Phase 0 当时 43 行）+ AGENTS.md（8 章）+ LICENSES 种子 + Cargo workspace（8 crates，DEC-022 依赖方向）+ CI（clippy/test/fmt/doc/doc-sync）本地全绿。

### 1.2 验证

- 真机 spike（REAPER v7.82）：API 全存在；MIDI 有改无 dirty 仍建点（T1_delta=1，7.78+ auto-dirty）；空回写无点；undo 后指针失效（ValidatePtr2=false→重取）；P_EXT 进 undo；NCH 须先建实例（`instantiate<0`）再设（`2.0→8.0`）；`TakeFX_AddByName(...,0)` 仅查询。
- 门禁：doc-sync 正向绿（当时 16 docs/27 DECs）+ 反向红（坏 fixture 必红，含非 UTF-8 硬化）。2026-10-07 文档合并后计数变为 18 docs，判定逻辑不变（见 §7）。

### 1.3 新事实（覆写初始假设处已标勘误）

- CLAP 代码 CC0-1.0（非 MIT）；MERT/MuQ 权重 NC；symphonia MPL-2.0（非 BSD）；VST3 SDK 3.8+ MIT；`BeginParamEdit` 不存在；`I_TAKEFX_NCH` 自 v7.07 可用；7.60–7.82 无新增 `MIDI_*` 函数。

### 1.4 DoD 对照

- [x] research 齐备 + 来源日期 + 需实测转 TSK
- [x] DECISIONS ≥25 条 + 默认值 + 反转条件（27 条 Accepted）
- [x] EVALUATION（H≥5、RSK≥12、kill、许可矩阵）
- [x] ARCHITECTURE（线程/IPC/数据结构/恢复，与 L1–L11 无冲突）
- [x] ROADMAP（Phase 0–4 验收 + 演示脚本）
- [x] TASK-INDEX（P0 可测 + 依赖 + 规模）
- [x] AGENTS.md（红线/工作流/状态/变更）
- [x] 骨架 + CI 绿灯
- [x] 本阶段报告 + 未决清单

## 2 M0–M4（Phase 0–4 工程闭环终验）

### 2.1 完成

- `scripts/m4_gate.py` 一次通过：16 证据文件齐 + 0 未关闭任务（F/B-001/002 冻结除外；原 TSK-901/902 已改号）。
- 100× 故障注入零残留（fault-inject-100/401 + pooled 真对）；盲听 ρ=0.825/1.0；全门禁绿。

### 2.2 验证

- cargo 全套件绿 + docsync 绿 + doc 零警告（本轮复核）。
- 云端 Tier1/2 真调用通过；Tier3 文本健康、音频 BLOCKED；扩展 smoke 通过。

### 2.3 新事实

- E2E 缺口：各部件经 mock/分段验证，但"5 分钟交互闭环"当时尚无单命令可跑形态（acrd 未接线成可运行守护进程）。不虚构为已验证；该缺口已由 TSK-405/505 处置（见 §3.1）。

## 3 M5–M7（Phase 5–7 交付核验）

### 3.1 完成

- M5 闭环：TSK-505 单命令 E2E 经 Tier1 真机见证（`brighter highs` seed 7：3 真 patch 6/7/7 ops 零修复；应用→渲染→null-test 全等→参数全复原→清轨；审计恰 1 调用 tier1）；LiveTier1 路径落地；TSK-506 主窗卡片（6 字段＋选胜出＋应用预览＋缺字段红行）真机点选验证。
- M6 能力：601 CLAP 谱指纹后端（`clap_cos` 出 `Some`，去重半径 0.02）＋onnx 缝；602 真 crate 接入＋10k 独立复现（build 0.56s／P95 40.316ms／self-hit 100%，2026-10-07 复核更正：原写 0.61s／40.27ms，以 `crates/retrieval/BENCH.md` 为准）；603 Demucs 真权重单 stem（本地 CPU，cache 二次命中）；604 SELECT 枚举＋add 全红＋name_regex 编译 fail-closed；605 journal 重放＋migrate 矩阵＋只读回退；606 双路 RRF＋`audio_ref` 定稿。
- M7 真实面：701 Tier1 点火（200＋output 信封）＋4xx 终端分类；702 代理矩阵 6 格＋loopback 硬旁路；703 重绘抽稀（单核 56.6%→16.7%，−70.6%）＋96DPI 零 tofu；704 真双进程 shm（200×4KiB 零丢零错序 p95 4.71ms）＋跨用户 BLOCKED＋指引；705 第二轮盲听 11 clips 三组全对（ρ＝1.0×3，累计 n＝21）；706 单机预算复测（写 1–2ms／撤销 2ms；证据缺口见 §6 第 3 条）。

### 3.2 验证

- 本轮复核（2026-10-07）：`clippy --workspace --all-targets -D warnings` 零错误；`cargo test --workspace` 零失败；`fmt --check` 干净；`cargo doc --workspace --no-deps` 零警告；`python scripts/check-docs.py` doc-sync OK（当时 19 docs，27 DECs）。
- 云端：Tier1 LIVE-OK（审计 1 调用，字段限 `[prompt,mir,meta]`，14 字节）；约 4 次 Tier1 调用量级个位数 KB。
- 反转求值：真 crate P95 ＋41.6% vs TSK-203 基线 → 触发"回退＞20% 则不晋升"规则，pattern 保持默认、real 常闭 opt-in（BENCH 手工追加，无 DEC 反转）。
- 盲听：首轮 n＝10（B 0.825／T 1.0／全 0.7455）＋次轮 n＝11 全对；权重 0.30／0.45／0.25 维持。

### 3.3 新事实

- 闭合白名单下 `add` 无合法形态：语义收紧为一律红（`AddToExistingTarget`，repair 移除），DEC-013 细化，无 ADR 反转。
- 回滚预览按钮合成点击约 10 次未翻转（相邻应用按钮同法成功；双臂对称＋构造单测绿）：判 harness 瞄准问题，未立为 bug，留人类亲手确认（见 §4）。
- `reacontrolmidi.json` 3 条目 ident／name 编码损坏（`3:`/`4:`/`8:` 通道类，预存）：加载与门禁不受影响（死条目），未猜测修复（见 §4）。
- 真 crate 特性构建需外部 `protoc`（本机无，借 temp-dir protoc 36.2 编过；增量缓存有效）：纯构建环境要求，已记 BENCH，不影响默认构建。
- `ort` 特性构建下载预构 ONNX Runtime 二进制（构建时联网；默认关闭，主构建离线）：许可已登记，非商业内部运行合规。

## 4 移交、冻结项与待人类事项

- Phase 0 → Phase 1 入口前置：TSK-109（Action ID 人工复核）、TSK-110/111（隔离 tab 重测）、TSK-112（B 矩阵×5 插件）、TSK-113（LICENSES 扩展）——均已 Done（见 `docs/TASK-INDEX.md`）。
- M0–M4 → 后续：新立 TSK-405（交互式 E2E M3 harness，已 Done）；pooled ghost 残留已确认删除；听音已归档；产品闭环与能力缺口转入 Phase 5–7。
- M5–M7 待人类（无截止，按需）：704 双用户 `runas` 手动观测；706 第二台目标机（或以 CI／用户机为准）；回滚按钮亲手一点；mel 标定 spike；Demucs FT／6s 钉选＋RTF 基线；fusion 公开重导出；渲染侧 `audio_ref` 碰撞 fail-closed；`reacontrolmidi` 三条目重探（b-matrix 重跑或已知参数表对照，禁猜）；CI 机装 `protoc`（真 crate 特性门）。上述未转 F/B（残留债未清零是 TSK-707 的验收缺口），现由 Phase 8 行 TSK-805–808 承接。
- 代码内 `TODO(M57-handoff)` 标记（文档漂移收口时由死任务号转 tag）：bridge 真机 floor 项（chunk 上限／MIDI 格式／GUID 花括号／null-dirty／40601，v7.82 证据之外待 v7.60 floor）；planner Retry-After 日期形；dsp 懒下载／sidecar／六 stem 映射；candidate 嵌入距离替换。认领任一即开对应 TSK。
- 冻结项：F/B-001（RB/JUCE/权重商用采购与分发；人类锁定"不购证 + 无分发"）、F/B-002（云端计费声明与预算立项；人类明确不声明计费）持续有效，本报告不解除。解冻均需人类书面解除约束。

## 5 决策与口径现状

- DEC 状态：27 DECs 全部 Accepted 有效；无 DEC 反转，无 ADR 反转。评分权重维持 0.30／0.45／0.25；检索默认后端维持（pattern 原型，真 crate 非默认）；Tier 档位与 Tier3 模型不变。
- Tier3 模型：人类 2026-10-06 由 `Gemma 4 12B` 改为 `Bonsai-2-27B`（唯一候选，llama.cpp only、纯文本；音频能力缺失即 BLOCKED，TSK-305）。该变更当时以原地改写 DEC-010 落地，2026-10-07 补立修正记录（不改写历史）。
- 审计口径：`OnnxClapEmbedder::embed` 保持 BLOCKED（mel 标定 spike 前不出伪数），默认谱指纹生效。
- 云侧口径：Tier1 训练保留 / Tier2 ZDR / Tier3 本地；原始音频默认不出网；审计字段白名单 `[prompt,mir,meta]`。

## 6 更正（2026-10-07）

本报告所合并的原报告中与仓库/外部事实不符处，此处不改写原文，只做更正/补记记录：

1. **计数（历史引文：原文 63 Done 为误）**：原 M5–M7 报告头部与 TSK-707 证据栏写"63 Done"，实际 `docs/TASK-INDEX.md` 当时为 **Phase 0–7 计 64 Done + 2 Blocked**（Phase 0–4 45 + Phase 5–7 19）。
2. **CI 状态**：原 M5–M7 报告 §2 只声明"本地五项绿"（该声明属实，2026-10-07 复跑仍绿：clippy/test/fmt/doc/check-docs）。但 GitHub Actions 侧当时为**红**——run #60（该报告落盘提交 `d108b43`）macOS/Ubuntu 的 `clippy -D warnings` 失败；63 次运行中 44 次失败，其中 #10–#43 连续 34 次失败。AGENTS §5「任一红即 BLOCKED」的约束在本轮关闭时**未被遵守也未被记录**。HEAD `fdd81fe`(#63) 已三 OS 全绿。
3. **TSK-706 证据**：该行数字（write 1–2ms / undo 2ms / redo 2–3ms）无仓库内产物（out.txt 已还原），现存 `experiments/perf-budget.out.txt` 属 TSK-403（3.0/4.0/2.0ms）；已在 TASK-INDEX 该行标注，缺口登记为 Phase 8 行。
4. **代理误分类补记（2026-10-07）**：本机 `http_proxy` 环境下 `HttpsTransport` 曾把连接拒绝误分类为 `ServerError`；已改为 loopback/hermetic 永不走系统代理（ARCH §5）。产品闭环与能力缺口转入 Phase 5–7（TSK-5xx/6xx/7xx），不在当时报告内伪关闭。
5. **残留债归属更正**：原 M5–M7 报告 §5 残留债（21 处 `TODO(M57-handoff)`、9 项待人类事项）未转 F/B，当时曾转记一次性真值修复行与 TSK-805/806/807；其中真值修复行完成后已从 `docs/TASK-INDEX.md` 删除，内容全部并入本节 §7，其余由 TASK-INDEX Phase 8 行 TSK-805/806/807 承接（另加 TSK-808）。

其余「新事实」与「移交」内容经复核仍然成立。

## 7 整改与门禁（2026-10-07）

Phase 0–7 关闭后做了一轮整改：一次性修复项已完成并记入本节；未完成项转 TASK-INDEX Phase 8 行（TSK-805–808）。本节只记事实与现状。

### 7.1 真值更正

- TASK-INDEX 逐行更正（TSK-000/003/105/107/108/202/203/301/602/706/707）：Phase 0–7 计数 63→64；单文件测试数 18（`config.rs`）/20（`consent.rs`）/15+9（`mir.rs`+`score.rs`）/37（`model_gw.rs`）；BENCH.md 数字改为 build 0.56s / P95 40.316ms；research 11 篇。
- EVALUATION（§8-1 Tier1 点火对账）、ROADMAP（research/docs 计数与 Phase 3 交付物）、DECISIONS（DEC-010 补立 2026-10-07 修正记录）加日期更正注，不改写历史。

### 7.2 契约门禁落地

- `scripts/check-deps.py`：DEC-022 crate 依赖方向违规即红。
- `scripts/check-contract.py`：头字段位置（前 15 非空行）、"N Done" 计数一致性、TASK-INDEX 证据文件存在且非空、个人绝对路径禁用（AGENTS §8）、裸 intra-doc 链接棘轮（基线 `scripts/link-baseline.txt`）。
- `scripts/selftest-gates.py`：为上述每类缺陷各植入 1 例，断言门禁必红、撤销后回绿。
- 三个脚本与既有 `check-docs.py` 一并接入 CI（`.github/workflows/ci.yml`）。

### 7.3 AGENTS §8 隐私清理

- eval 两个测试（`decode_matrix.rs`、`loudness_xcheck.rs`）删除硬编码个人 FFmpeg 路径，改 `FFMPEG_BIN`／PATH 探测；探测缺失时打印显式 `FFMPEG-SKIP` token（不静默跳过）。
- `experiments/` 5 个文件 9 处个人绝对路径脱敏（roundtrip 字节级证明仅路径子串变化）。
- 个人绝对路径由 `check-contract.py` 第 4 项持续门禁；`crates/common/tests/upload_audit.rs` 的脱敏夹具为唯一白名单例外。

### 7.4 LICENSES.md 登记修复

- 字体子集登记改为实际 222,524 B（原记 149KB 失效）；bridge 的 `serde_json` 更正为**运行时**依赖（原误标 dev-only）；按 crate×依赖补齐 12 行缺行（profile/planner/dsp/retrieval/acrd 等）；`protoc` 36.2 登记为仅构建期依赖；"零外部依赖"矛盾注与表格断裂修复（该口径只描述 TSK-113 初始骨架）。

### 7.5 测试诚实化

- `crates/planner/tests/live_tier2.rs` 可离线用例去掉 `#[ignore]`：套件由 460 变为 **461 passed / 0 failed / 3 ignored**。
- 待办（TSK-808）：真 crate／Demucs 断言的 CI 矩阵与断言计数门禁。

### 7.6 门禁现状（本地，2026-10-07）

- `cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo fmt --all -- --check`、`cargo doc --workspace --no-deps` 全绿；`cargo test` = 461 passed / 0 failed / 3 ignored。
- 四个 Python 门禁全绿：`python scripts/check-docs.py`、`python scripts/check-deps.py`、`python scripts/check-contract.py`、`python scripts/selftest-gates.py`（本地）。
- 诚实边界：以上为本地复跑结论；GitHub Actions 侧曾长期红（见 §6 第 2 条），HEAD `fdd81fe`(#63) 三 OS 全绿。真技术断言（LanceDB／ONNX／Demucs／FFmpeg）与真端点用例仍在 CI 配置之外，转 TSK-808。
- 遗留（脚本属主待办，不在文档范围内）：`scripts/selftest-gates.py` 的头字段负例与证据 token 负例仍指向 Phase 8 已删行/已重命名文件；`scripts/check-contract.py` 的历史豁免集合仍含已删除的评估报告路径（无害残留）。
