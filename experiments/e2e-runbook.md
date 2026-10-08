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
| 待填 | 待填 | — | — | — | — | — | — | — | — | 待填 |

- 人类见证重跑时：在上表追加一行，`--seed` 可换（如 8/42）验证确定性
  （同 seed 字节一致）与多样性（异 seed 仅 confidence 抖动，排序稳定）。
- 末行是**人类见证空模板行**（`待填`）：谁见证谁填，禁止代填姓名、日期或结果；
  上表第一行的 `子 Agent` 是机器行，**不构成**人类见证验收（见下节 `check`）。

## 见证行记录（`scripts/runbook_witness.py`，TSK-807）

上表由脚本读写，把「人类见证行」与「机器行」分开计数，避免机器行被当成验收证据：

```text
python scripts/runbook_witness.py check
python scripts/runbook_witness.py add --seed 8 \
    --steps "12s,40s,35s,18s,20s,6s" --total 2m11s \
    --within-5min yes --witness "人类见证人（本人姓名/代号）"
```

- `add`：写一行进表内——末行是空模板行时就地填入，否则插在表格最后一行之后
  （不会落到表外）。`--seed/--total/--within-5min/--witness` 必填；缺一项即拒绝。
- `add` 拒绝机器见证：`见证人` 含 `机器`/`子 Agent`/`subagent`/`machine`/`bot`
  时直接报错，除非显式 `--machine`（该行会被 `check` 单独计为机器行）。
- `check`：统计 `human/machine/empty/malformed` 行。**当前实测**
  （2026-10-08）：`rows=2 human=0 machine=1 empty=1 malformed=0`，
  退出码 1 并打印 `NO HUMAN WITNESS ROW YET`——即人类见证缺口是公开未闭合状态。
  出现至少一行人类见证后 `check` 退出码 0。
- 两种模式都先校验表头（`日期 | seed | 步骤 0..5 | 合计 | 是否 ≤5min | 见证人`），
  列不符即拒绝写入，避免畸形行进入产物。

## 盲听评分单（`scripts/blind_scores.py`，TSK-807）

盲听 ρ 此前只有文字结论（`docs/REPORTS.md` §3.3/§4），无仓库内原始排序；
本脚本补齐「空评分单 → 人类排序 → ρ 计算 → 结果落盘」的可复核链路（仅标准库）：

```text
# 1) 生成空评分单 + 答案键（键默认写仓库外 %TEMP%，评分人看不到）
python scripts/blind_scores.py generate --round blind3 \
    --sheet experiments/blind3-sheet.csv
# 2) 人类听音后填 human_rank（1 = 该系列第一位，可并列），notes 可写听感
# 3) 评分：逐系列 ρ + 合并 ρ，并把一行结果追加进结果表
python scripts/blind_scores.py score --sheet experiments/blind3-sheet.csv \
    --key %TEMP%/synthlm-blind-blind3-key.json \
    --append experiments/blind-results.md
# 4) 数学自检：ρ = +1.0000（完全一致）/ −1.0000（完全反转）/ +0.9487（并列，√0.9）
python scripts/blind_scores.py --selftest
```

- 任一 `human_rank` 为空即**拒绝**评分与追加（无部分证据）；非整数、越界排名同样拒绝。
- 并列名次按平均秩（average ranks）计算 Spearman，不会因评分人给出并列而失真。
- 答案键默认落在仓库外（`%TEMP%/synthlm-blind-<round>-key.json`）；`--key-out` 指到仓库内
  会被拒绝（仅 `--allow-in-repo-key` 可越过，且只用于干跑），与
  `crates/eval/tests/blind_calib2.rs` 把键放在盲目录之外同一原则。
- 输出只有计数、clip/stimulus id 与 ρ 值，不打印绝对路径（AGENTS §8）；
  `--min-rho`（默认 0.5）未达标时 `score` 退出码 1。
- 脚本不会生成任何排名：空评分单不是证据，ρ 只来自人类实际填写的排序。

## HiDPI 150% 截图（`synthlm-ui --scale`，TSK-807）

150% 截图此前记「未实测」（切换 OS 缩放要注销重登）。`--scale` 在进程内覆盖
egui 的 `pixels_per_point`，不改系统设置、不用重登：

```text
cargo run -p synthlm-ui -- --scale 1.5
# 有界运行（自动关窗，便于截图）：先设 SYNTHLM_UI_RUN_SECS=30
```

- 首帧在 stderr 打印 `FIRST_FRAME PPP=1.500 ZOOM=…`：这是与截图同源的机器证据，
  请与截图一起登记（`--scale` 缺省时不覆盖，保持显示器原生缩放）。
- 机器侧已实测（2026-10-08，`SYNTHLM_UI_RUN_SECS=5` 有界运行，退出码 0）：
  默认 `FIRST_FRAME PPP=1.000 ZOOM=1.000`；`--scale 1.5` → `FIRST_FRAME PPP=1.500 ZOOM=1.500`。
  即覆盖已生效；**150% 截图的「无 tofu」判定仍待人类**（本行只证机器侧链路）。
- `--scale` 非有限值（`nan`/`inf`）或 ≤0 直接报错退出，不静默回落 1.0
  （实测 `--scale 0` → `Error: --scale value '0' must be a finite number > 0 (e.g. 1.5)`，退出码 1）。
- 截图与「无 tofu」判断仍由人类给出并存到 `experiments/`（例如
  `experiments/ui-scale-150.png`），本手册与脚本不代拍、不代填、不代判。
