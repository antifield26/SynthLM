# E2E M3 Harness 运行手册（TSK-405，人机协同）

- 范围：M3 闭环演示（固定意图 + 固定参考音频，≤5min 得 3 候选，试听正常，
  一键应用后一键回滚，审计齐全）。候选来自固定种子确定性占位，
  非模型推理（诚实演示）。
- 安全红线：全程只碰脚本自建的临时轨；Lua 从不调用存盘；结束删轨。
  REAPER 全程保持停止状态 except 步骤 3（跑完即杀进程，不存盘）。
- 产物目录：`experiments/e2e-demo/`（`demo-plan.json`、`demo-apply.lua`、
  `demo-preview-*.wav`、`demo-apply.out.txt`）。
- 已验证基线（2026-10-06，子 Agent 实测）：seed 7 全链通过，
  Lua 侧 `demo_apply_ok=true`，`restored_all=true`，`tracks_zero=true`。

## 步骤

### 0. 生成计划（机器，~10s）

```text
cargo run -p synthlm-acrd -- demo --seed 7
```

- 断言：退出码 0；`experiments/e2e-demo/` 内 5 文件齐
  （plan + lua + 3 wav）；`ranked:` 行打印 3 个 id。
- 断言（`demo-plan.json`）：`candidates` 长 3；`direction` 集恰为
  `{darker, spatial, transient}`；每候选 `diff_summary_zh` 非空、
  `patch.ops` 非空且值 ∈ 0.0..=1.0；`audit` 长 3 且每事件恰 5 字段
  （`ts_unix_ms/model/tier/fields/byte_count`），`tier == "tier3"`，
  `model == "seeded-demo-no-model-call"`。

### 1. 核对计划（人，~1min）

- 打开 `demo-plan.json`：确认 3 候选 `rank` 1..3 按 `score_total` 降序；
  `notes` 三条诚实声明在场；`target_snapshot` 非空。
- 确认 `intent` 含“宽为 EQ 代理”字样（ReaEQ 无真宽度参数，不伪装）。

### 2. 试听（人，~1min）

- 依次播放 `demo-preview-demo-wide/dark/bright.wav`（任意播放器）。
- 断言：三文件可播、非静音（各 1s，96044 字节）；wide 带拍频 shimmer、
  dark 偏闷、bright 偏亮（相对关系即可，绝对音色不做承诺）。

### 3. 一键应用 + 一键回滚（机器 + 人见证，~1min）

```text
"C:\Program Files\REAPER (x64)\reaper.exe" -nonewinst experiments/e2e-demo/demo-apply.lua
```

- 等待同目录 `demo-apply.out.txt` 出现 `demo_apply_ok=true` 后，
  **杀掉 REAPER 进程，不存盘**（脚本自身永不存盘；杀进程即零残留）。
- 断言（读 `.out.txt`）：
  - `tracks_before=0`（空工程；若非 0，说明打开了别的工程——仍安全，
    但先确认无人值守工程被意外修改，当场 BLOCKED 上报）；
  - `fx=0 via=add`，4 个 ident 全部 `from_ident` 解析，
    序号 = 4/10/15/7（与 `b-matrix-01-stock.out.txt` 一致）；
  - 3 行 `applied|<id>|...` 值与 `demo-plan.json` 对应 patch 一致（±1e-6）；
  - 3 行 `undo|<id>|...` 逆序回滚；
  - `restored_all=true`（4 参数回到初值，容差 1e-9）；
  - `tracks_after=<tracks_before>` 且 `tracks_zero=true`。

### 4. 核对审计（人，~30s）

- `demo-plan.json` 的 `audit`：3 事件，`ts_unix_ms` 递增
  （`1789000000007/8/9` @ seed 7），`fields == ["prompt","mir","meta"]`，
  `byte_count > 0`；`model` 占位如实（无模型调用即无真实审计时间戳，
  时间戳为确定性占位）。

### 5. 清理确认（机器，~10s）

- REAPER 进程已停止（`Get-Process reaper` 无输出）；
  用户工程目录无新增/修改文件（`git status --short` 无 REAPER 侧改动；
  本手册产物均为未提交的新文件，不触碰既有文件）。

## 总时长记录栏

| 日期 | seed | 步骤 0 | 步骤 1 | 步骤 2 | 步骤 3 | 步骤 4 | 步骤 5 | 合计 | 是否 ≤5min | 见证人 |
|---|---|---|---|---|---|---|---|---|---|---|
| 2026-10-06 | 7 | ~10s | —（机器代检） | —（文件校验代检） | ~15s | —（机器代检） | ~5s | <1min | ✅ | 子 Agent（REAPER 侧 out.txt 见上） |
| | | | | | | | | | | |

- 人类见证重跑时：在上表追加一行，`--seed` 可换（如 8/42）验证确定性
  （同 seed 字节一致）与多样性（异 seed 仅 confidence 抖动，排序稳定）。
