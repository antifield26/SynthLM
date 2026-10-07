# SynthLM — AI Agentic Composer for REAPER

面向 REAPER 7 的 AI 作曲 / 声设 Agent：输入文本意图 + 参考音频，经本地 / 云端多模态模型，产出多个候选方案（第三方插件参数调节、音频派生编辑、轻度混音辅助），支持一键应用、A/B 对比、原子回滚。

- 状态：Phase 0–4 工程契约与验证闭环已关闭（45 任务 Done，F/B-001/002 冻结）；Phase 5–7（产品闭环 / 能力补全 / 真实面）见 `docs/ROADMAP.md` §2 与 `docs/TASK-INDEX.md`（`TSK-5xx/6xx/7xx`）。产品向演示仍为 seeded 形态：`docs/M4-REPORT.md` 与 `experiments/e2e-runbook.md`。
- 实现语言：Rust（Edition 2024 workspace：`common` / `profile` / `planner` / `retrieval` / `eval` / `dsp` / `acrd` / `bridge` / `ui`）。
- 目标宿主：REAPER ≥ 7.60（实测基线 7.82），通过 ReaScript / reaper-rs 官方与社区接口集成；**不做插件宿主**（L1）。

## 三档模型授权

| 档 | 模型 | 条件 |
|---|---|---|
| Tier1 | `Muse Spark 1.3 Contributor`（训练保留） | 接受训练保留 |
| Tier2 | `MiMo V2.6 Flash`（ZDR） | 仅接受上传 |
| Tier3 | 本地 `Bonsai-2-27B`（纯文本，llama.cpp `:8080`） | 不上传 |

API Key 只读仓库根 `.env`（已 gitignore，永不提交）；原始音频默认不出网。

## 快速开始（开发者）

```powershell
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo fmt --all -- --check
cargo doc --workspace --no-deps
python scripts/check-docs.py        # 文档同步门禁（头字段/DEC 计数）
python scripts/check-deps.py        # DEC-022 crate 依赖方向门禁
python scripts/check-contract.py    # 契约门禁（计数一致性/证据存在/绝对路径/裸链接棘轮）
python scripts/selftest-gates.py    # 门禁自检（植入违规必红，撤销后回绿）
python scripts/m4_gate.py           # 发布门禁（16 证据 + 任务全关）
```

真机 spike：`reaper.exe -nonewinst experiments/<name>.lua`，输出同名 `.out.txt`（只读证据，不进主干验收）。

## 文档索引

- `AGENTS.md` —— 所有 Agent 会话的强制契约（最高优先级，先读）。
- `docs/TASK-INDEX.md` —— 任务唯一真源（状态机 Todo → Done，Blocked 写原因）。
- `docs/DECISIONS.md` —— 27 条 ADR（默认值 + 反转条件）。
- `docs/ARCHITECTURE.md` —— 进程/线程边界、IPC、数据结构、崩溃恢复。
- `docs/EVALUATION.md` —— 可行性、对抗假设、风险登记册（RSK-001–014）、kill criteria、许可矩阵。
- `docs/ROADMAP.md` —— Phase 0–4 + 里程碑演示脚本。
- `docs/research/` —— A/B/C/D 调研笔记（来源 + 日期 + 置信度）。
- `docs/LICENSES.md` —— 依赖许可登记册（新增依赖必须同步更新）。
- `experiments/` —— spike 脚本与证据输出。

## 许可

- 本仓库代码：MIT（见 `LICENSE`）。
- 第三方依赖/权重/模型：见 `docs/LICENSES.md` 登记（GPL/AGPL 未购证与 NC 权重仅限内部运行；出现分发物即冻结，F/B-001）。
