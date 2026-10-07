# ASSESSMENT-2026-10-07（工作区项目评估）

- 目的：对 SynthLM 工作区做一次端到端、可复核的独立评估，核验"文档自述状态"与"仓库实际证据"是否一致，识别风险与缺口并给出处置建议。
- 适用范围：仓库全量（AGENTS.md / docs / crates / experiments / .github / git 历史 / GitHub Actions）；评估时点 commit `fdd81fe`（2026-10-07 19:46，工作树干净）。
- 状态：Draft（评估报告，非 TASK-INDEX 任务；未登记 TSK 号，处置建议见 §7）
- 最后核验日期：2026-10-07
- 依赖文档：AGENTS.md、docs/TASK-INDEX.md、docs/DECISIONS.md、docs/EVALUATION.md、docs/ROADMAP.md、docs/M57-REPORT.md、docs/LICENSES.md。

## 0 方法（可复核）

- 本报告由评估会话（Lead）执行，方法：静态阅读 + 亲自跑门禁 + 外部事实源（GitHub API）+ 两个独立子代理（代码审计 / 文档与证据审计，只读、不跑 cargo）。
- 子代理结论在采纳前由 Lead 抽样复算（见 §3 的"复核"列），未复算者标注为"未复核"。
- 透明性说明：文档/证据审计子代理产出了完整报告（结论已抽样复算）；代码审计子代理产出了完整报告（HIGH 级结论已逐条复算，见 §4 P0-4/P1-5/P1-6）。其早期扫描输出中"证据文件名 MISSING"一类判定经复核为**误报**（只在仓库根查找裸文件名），未采纳。
- **更正记录**：本报告初稿曾写"跟踪文件 0 处绝对路径"，该结论来自一次有缺陷的扫描（把路径字符串当作管道输入而非 `-Path`），已作废并重扫——真实结果是 13 处命中（见 §3 与 P1-5）。
- 本人亲自执行的命令与结果：
  - `cargo clippy --workspace --all-targets -- -D warnings` → exit 0（2.1s，缓存热）
  - `cargo test --workspace` → exit 0（87.4s；460 passed / 0 failed / 4 ignored）
  - `cargo fmt --all -- --check` → exit 0（0.8s）
  - `cargo doc --workspace --no-deps` → exit 0（9s，零警告）
  - `python scripts/check-docs.py` → exit 0；`python scripts/m4_gate.py` → exit 0
  - `acrd demo --seed 7` 两次 → 5 个产物 SHA256 完全一致（确定性成立）
  - 全仓扫描：跟踪文本文件中的 Key/绝对路径（0 命中）、`unsafe`/`unwrap` 分布、25 个直接外部依赖与 LICENSES 登记比对

## 1 结论摘要

**总评：工程纪律与文档体系显著高于同类个人项目（契约先行、红线可追溯、零 unwrap 进主干、隐私卫生满分）；但"已验证"的口径被系统性高估，且被高估的部分恰好集中在"里程碑是否真的闭环"这一最关键的判断上。代码质量与过程可信度之间出现了明显落差。**

三句话：

1. **代码是真的、门禁是真的**：9 个 crate、约 2.25 万行非测试代码 + 1.07 万行测试、465 个 `#[test]`、460 通过 0 失败；clippy/fmt/doc/两个文档门禁本地全绿；HEAD 的 GitHub CI 三 OS 全绿。
2. **"闭环"是半成品**：产品路径目前是"CLI 生成 plan + Lua → 人工在 REAPER 跑脚本"；UI 内嵌 seeded demo 计划、未接 acrd；bridge 的 IPC 客户端在扩展里零引用；live 候选的 `direction=unknown`、`confidence=0.5`、差异句为模板句。
3. **真实技术栈全部落在产品路径之外**：`dsp` 是孤儿 crate（无人依赖）、`acrd serve` 只服务 mock、`clap_cos` 在产品侧硬编码 `None`、检索默认后端是内存原型（真 LanceDB 非默认特性）——TSK-601/602/603 的"真实技术"只在各自测试/基准里成立。
4. **过程契约被破了且没被记录**：63 次 CI 运行里 44 次失败（含连续 34 次），最后一次"终验报告 + 关闭 Phase 5–7"正是在 CI 红的 commit 上做出的，而 AGENTS §5 写着"任一红即 BLOCKED"；个人绝对路径被写入公开仓库（§8 禁项）。

## 2 事实快照

| 维度 | 数值 | 来源 |
|---|---|---|
| crate 数 / 行数 | 9 / 非测试 22,508 行 + 测试 10,742 行（合计 33,250） | Lead 统计（含 tests/ 目录） |
| 测试 | `#[test]` 465（374 单测 + 91 集成，15 个测试文件）；实跑 460 passed、0 failed、4 ignored | cargo test（Lead 亲跑） |
| tracked 文件 / 工作树 | 212 / 干净 | git |
| 提交 | 110 commits，2026-10-06 12:10 → 2026-10-07 19:46（约 31.6 小时），单一 GitHub 身份 | git log |
| 提交类型 | docs 49 / feat 38 / chore 7 / test 6 / spike 6 / fix 4 | git log |
| CI（GitHub Actions） | 63 runs：19 success / 44 failure；runs 10–43 连续 34 次失败；55–62 连续 8 次失败；HEAD(#63) 三 OS 全绿 | api.github.com（外部事实源） |
| 直接外部依赖 | 25 个，LICENSES.md 登记 25/25；Cargo.lock 788 包 | Cargo.toml + LICENSES.md |
| 忽略测试 | 4 个：3 个真端点 live 用例 + 1 个需第二 OS 用户 | cargo test 输出 |
| 仓库可见性 | **public**（private:false） | api.github.com |

被忽略的 4 个用例全部是"需要外部环境"（真云端端点、第二 OS 用户），不是掩盖失败——但这也意味着**云端链路在 CI 中永不执行**。

## 3 独立复核结果（正向，已核实）

| 项 | 结论 | 复核方式 |
|---|---|---|
| 本地五项门禁 | 全绿（含耗时） | Lead 亲跑 |
| 测试真实性 | 460 通过、0 失败 | Lead 亲跑 |
| `unwrap/expect/panic` 进主干 | **0 处**（3 处命中均在 `//!`/`///` 示例代码里） | Lead 脚本按 `#[cfg(test)]` 行号切分后统计 |
| `unsafe` 许可面 | 7 处：bridge 5（extension 3 + undo 2）+ `common/shm.rs` 2，全部有逐处 SAFETY 论证 | Lead 脚本 + 逐处阅读 |
| 密钥卫生 | `.env` 已 gitignore 且**从未进入任何 commit**；190 个跟踪文本文件 0 处 Key/Token/Bearer | Lead 扫描（已修正版）+ `git log --all -- .env` |
| 日志脱敏（AGENTS §8） | **不合格**：13 处绝对路径命中——2 个测试源硬编码 `C:\Users\<用户名>\tools\ffmpeg\...`，4 个 spike 日志与 1 份步骤文档写入个人路径/`%TEMP%` 全路径（详见 P1-5） | Lead 重扫（修正版） |
| 依赖许可登记 | 25/25 直接依赖均有登记行 | Lead 脚本比对 |
| 评分器 golden 回归 | 9 个 golden 用例 + `+6dB` 防作弊回归（`golden_plus_6db_must_not_score_higher`）真实存在且通过 | 代码 + 亲跑 |
| demo 确定性 | 同 seed 两次运行 5 产物 byte-identical | Lead 亲跑 |
| 提交规范 | Conventional Commits 100% 合规 | git log |

## 4 主要发现（按严重度）

### P0-1 CI 长期红，"终验关闭"发生在红 CI 上（过程契约破裂）

- 事实：63 次运行中 44 次失败。runs 10–43 连续 34 次失败（macOS/Ubuntu 的 `cargo clippy -D warnings`），此后 55–62 又连续 8 次失败于同一环节；Windows job 几乎始终绿。
- 关键点：`docs/M57-REPORT.md` 落盘、TSK-707 关闭的那一次提交（run 60, `d108b43`）CI 是**失败**的；最后两个 commit 的标题本身即"fix: collapse nested if in non-Windows model path (CI unix/macos clippy)"，说明代码长期只在 Windows 侧被验证。
- 契约对照：AGENTS §5"CI 必跑…任一红即 BLOCKED"；TASK-INDEX TSK-107 证据栏写"CI 三 OS jobs 全绿"。
- 影响：M57-REPORT §2 的措辞是"本地五项绿"（技术上是真的），但 Phase 5–7 的关闭动作建立在"CI 红"的事实上而无任何记录或 F/B 行；这使"CI 拦截"这类承诺失去约束力。

### P0-2 "产品闭环"的接线程度被高估（TSK-502/505/506）

| 自述 | 实际 |
|---|---|
| "UI 主窗接线（卡片 6 字段 + 试听 + 应用/回滚）" | `crates/ui/src/main.rs:23` 用 `include_str!("../../../experiments/e2e-demo/demo-plan.json")` **编译期内嵌 seeded demo**；UI 与 acrd 之间无 IPC。 |
| "bridge↔acrd 真 IPC 接线" | `crates/bridge/src/extension.rs` 对 `client`/`Client` **零引用**；`synthlm-bridge` 只出现在 acrd 的 **dev-dependencies**（测试用）。`bridge` 的 `SnapshotBackend`/`ReaperFxChain` 只有 `#[cfg(test)]` mock 实现（snapshot.rs:750、container_addr.rs:721），live 面 = `LiveUndo` 的 4 个 API + `%TEMP%` ping。全仓 21 处 `TODO(M57-handoff)` 中 10 处在 bridge。 |
| "单命令 E2E M3" | 真实 REAPER 侧执行由 `experiments/e2e-live/e2e-apply.lua` 完成，需人工 `reaper.exe -nonewinst` 拉起。 |

- 结论：当前可运行的端到端形态 = **CLI 生成 plan + Lua 脚本 → 人工在 REAPER 执行**。bridge 扩展（TSK-114）目前只做"版本日志 + 写 `%TEMP%` 标记"的 smoke。
- 影响：这不是"没做完"的问题（Phase 5–7 本就允许留接线），而是 Done 行与验收标准的表述让读者以为闭环已经自动跑通。

### P0-3 live 候选的区分度是占位（评价环未真正生效）

- 证据（`experiments/e2e-live/e2e-plan.json`，LiveTier1 真实调用产物）：3 个候选全部 `direction="unknown"`、`confidence=0.5`、`delta_lufs=0.0`，`diff_summary_zh` 为模板句"模型建议调整 N 个参数。"。
- 根因（代码）：`crates/acrd/src/e2e.rs:479-503` 的 `direction_label()` 仅对 mock 后端按 id 后缀映射 lane；live 后端一律返回 `"unknown"`。
- 契约对照：TSK-505 验收"3 候选差异句非空"——**形式满足、实质未满足**：模板句无法区分候选，且"方向/置信度"在 live 路径不是测量值。
- 影响：MIR/评分/盲听权重（TSK-202/204/402/601）在 live 产品路径中尚未对"候选谁更好"产生可观测影响；此前所有评分验证都发生在离线 fixture 上。

### P0-4 真实技术栈全部落在产品路径之外（TSK-601/602/603/205）

| 环节 | 产品侧现状 | 证据（已复核） |
|---|---|---|
| 守护进程规划 | `acrd serve` 硬连 `ModelBackend::MockSeeded` + `MockTransport::all_ok()`；wire 上选 `live-tier2` 返回 `UnsupportedBackend` | `crates/acrd/src/dispatch.rs:13-14, 99-102, 164-165`（代码自述为"by design"） |
| 评分/CLAP | `clap_cos` 在 acrd **只出现一次**且硬编码 `None`；`score_pair`/`dedup_by_clap`/`SpectralClapEmbedder` 在 acrd/ui/bridge **0 引用** | `dispatch.rs:814`；Lead 全仓符号引用扫描 |
| 检索 | 默认特性 `usearch-backend`/`lancedb-backend` 都是**空特性**（内存精确扫描原型）；真 crate 在非默认 `lancedb-real` | `crates/retrieval/Cargo.toml` `[features]`；`real_lancedb.rs` |
| DSP/stem | `dsp` 是**孤儿 crate**：全仓只有它自己的 3 个文件提到它，`synthlm_dsp` 在 acrd/ui/bridge 0 引用 | Lead 全仓引用扫描 |
| "CLAP" 本体 | 默认装配的 `SpectralClapEmbedder` 是"重采样→RMS 归一→波形 32×16 DCT→L2"的谱指纹；`OnnxClapEmbedder::embed` 恒返回 `Err(ClapUnavailable)` | `crates/eval/src/clap.rs:198-254, 525-531`；与 TSK-601 行的"谱指纹后端"自述一致 |

- 影响：TSK-601/602/603 的 Done 是"能力在库内实现并有测试"，但**没有任何一条进入产品二进制**；Phase 6 的"能力补全"是把零件造好放在另一个房间。这与 ROADMAP Phase 6 的"接入产品环时以 TSK-606/505 会合"存在事实落差。

### P1-1 人工见证类验收在仓库内不可核验

- `experiments/e2e-runbook.md` 见证表只有 1 行，见证人 = "子 Agent"，步骤 1/2/4 标"（机器代检）"；TSK-505 验收明写"runbook 表追加见证行"——**未做**（无 live 行）。
- 盲听相关数（ρ=0.825/1.0/0.7455、第二轮 ρ=1.0×3、n=21）仓库内无评分单、无逐条排序原始数据；仓库里的 `crates/eval/tests/blind_calib*.rs` 是**合成刺激 + 构造式 ground truth 的模型自检**（`blind_calib2.rs` 头部自述 key 与 ground truth"reported to the main session only"），它不是人-模型相关性证据。
- 同类：TSK-703 以"96DPI 截图零 tofu"结案，而 `%TEMP%/synthlm-ui-proto/TSK-306-results.md:15,46` 明确记着"HiDPI（150%）未实测…须在目标机补测"——原条件是 150% 缩放，结案证据是 100% 缩放。
- 说明：这些**未必是虚假**——人的听感与真机操作本来就在仓库外。问题在于验收标准写成"人类确认"却在索引里以"已验证"落盘，随后的自动化门禁也无法覆盖（证据落在 `%TEMP%` 的还有 TSK-306 原型报告、盲听刺激/密钥目录）。

### P1-2 契约声称的自动化门禁并不存在

| 契约原文 | 实际 |
|---|---|
| AGENTS §4"依赖方向违规 CI 拦截"（DEC-022） | `.github/workflows/ci.yml` 仅 clippy/test/fmt/doc/check-docs；无依赖方向检查、无 cargo-deny。 |
| AGENTS §5"性能预算…由 TSK-403 门禁" | 无 `benches/`、无 criterion、无预算断言测试；TSK-403 是一次性 REAPER Lua 采样（`experiments/perf-budget.out.txt`: `write500_ms=3.0` / `undo_ms=4.0` / `redo_ms=2.0`）。 |
| "文档同步检查（头字段/稳定 ID）" | `scripts/check-docs.py` 只做**子串存在性**：5 个字段名出现在文件任意位置即通过、DEC 唯一数 ≥25、2 个禁用语仅检 `推荐/反转条件/选项/背景` 行、且 `LICENSES.md` 被豁免；`active-rows` 计算后从不失败。 |
| M4 门禁"16 证据 + 0 未关" | `scripts/m4_gate.py` 只 `is_file()`（0 字节也算过）+ 状态字符串比对，不校验证据内容；成功横幅硬编码"0 open"。 |

### P1-3 证据数字与自述不一致（抽样 6 处，均已复核）

| 位置 | 自述 | 实际 |
|---|---|---|
| M57-REPORT:7 / TASK-INDEX TSK-707 | "63 Done＋F/B-001/002" | `64 Done + 2 Blocked`（Phases 6/15/8/11/5/6/6/7 = 64） |
| TASK-INDEX TSK-602 | build 0.61s / P95 40.27ms | `crates/retrieval/BENCH.md`：lancedb-real **0.56s / 40.316ms** |
| TASK-INDEX TSK-706 | write 1–2ms / undo 2ms / redo 2–3ms | 无产物（单元格自述"out.txt 已还原"）；现存 `perf-budget.out.txt` 是 TSK-403 的 3.0/4.0/2.0 |
| TASK-INDEX TSK-108 / 105 / 301 / 202 | config.rs 33 / consent.rs 11 / model_gw.rs 14 / mir+score 13+5 | 实测 18 / 20 / 37 / 15+9（**双向漂移**，非单向夸大） |
| TASK-INDEX TSK-203 | 10k harness 0.14s / P95~30ms | BENCH.md 无 0.14s（0.11 / 0.10s；P95 31.163 / 28.472ms） |
| TASK-INDEX TSK-000 / ROADMAP | research 计 10 篇；TSK-003"16 docs"；M57"19 docs" | research 实际 **11** 篇；docs 现 **20** 篇（加本报告 21） |
| TASK-INDEX TSK-108 / 105 / 301 / 202 之外的登记面 | LICENSES.md 按"包名"登记（25/25 包都有行） | 按 **crate×dep 使用面**缺 11 行（profile/planner/dsp 的 serde/thiserror、retrieval 的 thiserror、acrd 的 interprocess 等）；且 `LICENSES.md:14` 把 bridge 的 `serde_json` 标为"dev-only，不进运行时"，实际它在 `bridge/Cargo.toml` 的 `[dependencies]` 且在 `client.rs` 使用 |

结论：这些多为**时间漂移**（先写数字、后改代码/文档，未回填），但后果是 TASK-INDEX 不能作为"免复核"的真源使用。

### P1-4 公开仓库 vs 锁定约束"无分发"（需人类拍板）

- 事实：`github.com/antifield26/SynthLM` **public**（API `private:false`）。
- 约束对照：AGENTS 头部"人类已锁定约束：非商业、无分发、不购买许可"；F/B-001 的冻结触发条件是"出现分发物"。
- 现状核查（**未触线**）：仓库内无 GPL/NC 二进制与权重（Demucs 权重懒下载不入库；FFmpeg 仅 `buildconf` 文本；UI 字体为 OFL 子集且带 `OFL.txt` + 来源记录 `README-SOURCE.txt`）。
- 风险：公开即"源码分发"动作，会把后续"仅内部运行"的宽松前提收紧——任何把 CC-BY-NC 权重（MERT/MuQ）、Demucs 科研权重、GPL 采样器产物入库或随 release 分发，都将直接触发 F/B-001。
- 需人类确认：public 是有意为之，还是应转 private（或明确记录"源码公开不算分发"的口径）。

### P1-5 个人绝对路径被写入公开仓库（违反 AGENTS §8）

- 事实：修正扫描（13 处命中）显示 `C:\Users\<用户名>\...` 出现在
  - 源码常量：`crates/eval/tests/decode_matrix.rs:32`、`crates/eval/tests/loudness_xcheck.rs:38`（FFmpeg 默认路径）
  - spike 日志：`experiments/decode-matrix.out.txt:4`、`render-line-01.out.txt:8`、`render-line-02.out.txt:9`、`render-m7-matrix.out.txt:1-4`（`%TEMP%` 全路径）
  - 步骤文档：`experiments/ext-load-steps.md:17`（完整工程路径 + DLL 路径）
  - `docs/LICENSES.md:57` 亦写着 `~/tools/ffmpeg/...`
- 对照：AGENTS §8"日志脱敏：…禁绝对路径（用指纹 + 相对路径）"；且仓库为 **public**（P1-4），等于把用户名与目录结构一并发布。
- 正向：`crates/common/tests/upload_audit.rs` 用 `C:\Users\<夹具用户>\...` 作为**脱敏断言的夹具**，属于正确用法。

### P1-6 真技术断言大多不在 CI 配置内（绿 ≠ 断言过）

- `crates/eval/tests/decode_matrix.rs:31-32`、`loudness_xcheck.rs:37-38` 硬编码个人 FFmpeg 路径，找不到就写 "skipped" 行——**不失败**。
- `#[ignore]` 5 个：`planner/tests/live_tier2.rs:22,45,78`（需网络+Key；:78 实际可离线）+ `common/src/perm.rs:207`（还需环境变量，等于永不执行；另一条同文件用例是 cfg-gated）。
- `dsp/src/demucs.rs` 的权重路径测试在 `onnx` 特性关闭（默认）时早退；`retrieval/tests/bench_10k_real.rs` 需 `lancedb-real`（默认关）。
- 结论：**LanceDB / ONNX / Demucs / FFmpeg 四条"真实技术"断言的执行都在 CI 之外**；CI 绿只覆盖纯逻辑与 golden。

### P2 治理与卫生（多数条目已抽样复核；第 1/6 条为判读）

1. **自证循环**：几乎所有"验证"由同一 Agent 在最后一次提交里同时写入代码、证据与结论；例如 TSK-707 关闭时 CI 红。建议对 P0 级结论引入异源复核。
2. **文档漂移系统性**：18/20 文档的"最后核验日期"早于其最后一次提交；Tier3 旧称"Gemma 4 12B 唯一候选"仍留在 TASK-INDEX TSK-004 与 PHASE0-REPORT:24；EVALUATION:94 仍写"Tier1 未点火"（与 TSK-701/M57 §2 冲突）；ROADMAP Phase 3 列 vLLM/MLX/LM Studio（与锁定"Bonsai-2-27B 唯一候选"冲突）；`acrd e2e --help` 写 "live Tier2 text path" 而实测为 live-tier1。
3. **DEC 状态机**：DEC-010 由 Gemma 4 12B 原地改写为 Bonsai-2-27B，未走 `Superseded`/新 DEC 链（AGENTS §6/§7 要求）。
4. **LICENSES.md 内部矛盾**：:52 有"并非零外部依赖"的更正注，:56 仍写"cargo tree：零外部依赖"；字体条目写"子集 149KB"而实际 222,524B；`protoc`（真 crate 特性构建依赖）未登记。
5. **残留债未清零**：TSK-707 验收要求"残留债清零或转 F/B"，但 M57 §5 列 9 项 + 代码内 **21 处 `TODO(M57-handoff)`**（tracked 文件共 22 处提及，含 M57-REPORT 自述 1 处；bridge 10 / dsp 6 / planner 5），对应 0 活动行、仅 2 个 F/B。
6. **验收标准可被"形式满足"**：差异句模板（P0-3）、证据栏只写数字、m4_gate 只看文件存在。
7. **环境**：仓库根 `target/` 约 **39 GB**（未跟踪）；tracked WAV 18 个 / 4.09 MB PCM（clone 体积与"发布音频"面）；`%TEMP%` 残留 **169 个 `synthlm*` 目录 ≈ 2.1 GB**（测试/钉选产物，部分不自清理）。
8. **文档门禁副作用**：把新文档放进 `docs/` 会改变 check-docs 的文档计数与头字段要求（本报告已按 5 字段格式书写）。
9. **§4 intra-doc 规则未遵守**：裸链接 `` [`X`] ``（非 `` [`X`](full::path) `` 形式）在我的宽口径下 **439 处 / 41 个源文件**（子代理严口径 175 处 / 31 文件）；`cargo doc` 零警告正是因为同模块内可解析——**门禁绿而规则破**。
10. **`#![warn(missing_docs)]` 只有 1/9 crate** 开启（retrieval）；公共 API rustdoc 缺失面未被机械约束。
11. **测试自描述诚实度**：多处测试用 `#[ignore]`/早退/skipped 行取代失败（P1-6），使"全绿"覆盖不到真技术；CI 无覆盖率或断言计数门禁。

## 5 成熟度评估

| 维度 | 评级 | 依据 |
|---|---|---|
| 架构与边界设计 | **A-** | 9 crate 分层、DEC-022 方向可读、音频线程红线结构性满足（bridge 不做音频回调、不宿主插件）；扣分：`dsp` 孤儿、spec 里的 `acrd←bridge`、`bridge←ui` 边不存在 |
| 代码质量 | **A** | 0 unwrap/expect 进主干、unsafe 全在批准面且有 SAFETY、clippy/doc/fmt 全绿、Edition 2024 统一；扣分：439 处裸 doc 链接、仅 1/9 crate 开 missing_docs |
| 测试 | **B** | 465 用例全绿、golden + 防作弊 + null-test 真实；扣分：真技术断言（LanceDB/ONNX/Demucs/FFmpeg）全在 CI 配置外，多处 skip 而非失败，无 bench/无覆盖率门禁 |
| 产品闭环 | **C-** | CLI+Lua+人工是唯一真路径；`acrd serve` 只服务 mock；UI 内嵌 demo；bridge 快照/链路层只有测试 mock |
| 技术栈落地度 | **D** | TSK-601/602/603/205 的真实技术在库内成立但**未进入任何产品二进制**（`dsp` 无人依赖、`clap_cos` 硬编码 None、检索默认内存原型） |
| 评估/排序有效性 | **C-** | live 路径方向/置信度/差异句均为占位，评分未进入产品决策 |
| 文档与过程可信度 | **C+** | 文档体系完整、可读性高，但关键"已验证"口径被高估，证据数字漂移、CI 红期关闭 |
| 合规与隐私 | **B-** | Key/依赖包登记/Font 记录齐；扣分：个人绝对路径入公开仓库（§8）、LICENSES 按 crate 缺行 + serde_json 误标、`protoc` 未登记、公开仓库策略待确认 |

## 6 建议（按顺序，最小改动优先）

1. **先修证据链，不加功能**：对 19 个 Phase 5–7 Done 行逐行判定"证据是否在仓库内可复核"；不可复核者把状态降回 Todo 或转 F/B-nnn（附原因 + 解除条件），并把"人类见证"改写为可落盘形式（评分单、截图、runbook 行、哈希）。
2. **把契约变成门禁**：新增脚本 + CI 步骤，覆盖（a）DEC-022 依赖方向、（b）Done 计数与文档声明一致性、（c）证据文件非空且含断言 token、（d）"CI 红时不得关闭任务行"的流程约束（例如 TASK-INDEX 状态变更需 CI 绿）。
3. **明确 UI 定位（二选一）**：接 acrd（`client.rs` 的帧构造可复用）或把 UI 标注为"demo 计划查看器"，同步修正 TSK-506 的表述。
4. **给 live 路径补区分度**：把 601 的嵌入距离 / 202 的 MIR 接进 live 排名；同时把 TSK-505 验收从"差异句非空"改为可测指标（候选间 patch 距离下限、渲染产物 FNV 互异、score 差阈值）。
5. **合规收口**：确认公开仓库策略；**从 spike 日志与测试源码中清除个人绝对路径（§8）**；登记 `protoc`；修 LICENSES 按 crate 缺行与 serde_json 误标；为 DEC-010 补 Superseded 链。
6. **把"真实技术"接进产品或降级宣称**：`dsp` 接进 acrd（否则 TSK-205/603 应改为"库内能力，未接线"）、`acrd serve` 补 live 规划或明确标注只服务 mock、检索默认后端与 TSK-203/602 的表述对齐。
7. **测试诚实化**：把 FFmpeg/权重/真 crate 类断言从"skip 不失败"改为"CI 显式矩阵 job（可标记 allowed-to-skip 但需计数）》；`live_tier2.rs:78` 这类可离线用例去掉 `#[ignore]`。
8. **卫生**：清理 39 GB `target/`（会丢增量缓存，按需）与 `%TEMP%` 2.1 GB 残留；建立"最后核验日期随提交更新"的机械检查。

## 7 未决问题（需人类拍板，Agent 不猜测）

- **Q1**：GitHub 仓库 public 是否符合锁定约束"无分发"？转 private，还是书面记录"源码公开不算分发、但权重/二进制不得入库"的口径？（涉 F/B-001 触发线）
- **Q2**：本次评估发现是否登记进 TASK-INDEX？（§6 规定它是唯一任务真源；本报告目前未占用 TSK 号）
- **Q3**：本报告是否提交入库（`git add docs/ASSESSMENT-2026-10-07.md`）？当前为未跟踪新文件。
- **Q4**：P0-3（live 候选占位）是立即修代码，还是先改验收标准？（两者都触及已 Accepted 的 TSK-505 表述）
- **Q5**：CI 长期红是否需要按 §5 追认一次"事后 BLOCKED 复盘"，还是以"HEAD 绿"收口？
- **Q6**：个人绝对路径已进入公开仓库（P1-5）——是否按 RSK-006 走一次处置（改写日志/常量 + 视需要清理 git 历史），还是仅在后续提交中修正？
- **Q7**：`dsp` 孤儿与 `acrd serve` 仅 mock（P0-4）是"按计划留接线"，还是应把 TSK-205/601/602/603 的状态从 Done 调整为"库内完成、未接线"？
