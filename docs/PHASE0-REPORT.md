# PHASE0-REPORT（Phase 0 阶段报告）

- 目的： closure Phase 0（立项、调研与设计），记录完成项、验证结果、新事实、决策修正与移交 Phase 1 的阻塞点。
- 适用范围：Phase 0 DoD 对照；后续阶段入口。
- 状态：Accepted（Phase 0 于 2026-10-06 关闭；Edition 2024 切换已验证全绿）
- 最后核验日期：2026-10-06
- 依赖文档：docs/research/、DECISIONS、EVALUATION、ARCHITECTURE、ROADMAP、TASK-INDEX、AGENTS.md。

## 1 完成了什么

- research 10 篇（A01–A06、B、C-models、C-dsp、D）+ DECISIONS 27 条（Accepted）+ EVALUATION（H6/RSK14）+ ARCHITECTURE（11 章）+ ROADMAP（Phase 0–4 + M0–M4）+ TASK-INDEX（43 行）+ AGENTS.md（8 章）+ LICENSES 种子 + Cargo workspace（8 crates，DEC-022 依赖方向）+ CI（clippy/test/fmt/doc/doc-sync）本地全绿。

## 2 验证了什么

- 真机 spike（v7.82）：API 全存在；MIDI 有改无 dirty 仍建点（T1_delta=1，7.78+ auto-dirty）；空回写无点；undo 后指针失效（ValidatePtr2=false→重取）；P_EXT 进 undo；NCH 须先建实例（`instantiate<0`）再设（`2.0→8.0`）；`TakeFX_AddByName(...,0)` 仅查询。
- 门禁：doc-sync 正向绿（16 docs/27 DECs）+ 反向红（坏 fixture 必红，含非 UTF-8 硬化）。

## 3 新事实（覆写初始假设处已标勘误）

- CLAP 代码 CC0-1.0（非 MIT）；MERT/MuQ 权重 NC；symphonia MPL-2.0（非 BSD）；VST3 SDK 3.8+ MIT；`BeginParamEdit` 不存在；`I_TAKEFX_NCH` 自 v7.07 可用；7.60–7.82 无新增 MIDI_* 函数。

## 4 决策修正（人类拍板链）

- 本地主路径 → 云端三档（Tier1 训练保留 / Tier2 ZDR mimo-v2.6-flash / Tier3 本地 Gemma 4 12B 唯一候选），L3 部分触发（限推理链）；不上传→原始音频默认不出网 + 白名单审计；本地端点实测递延实现阶段（TSK-305）；不声明计费；F/B-001/002 冻结（原 TSK-901/902 改号）。

## 5 阻塞点（移交 Phase 1）

- TSK-109（Action ID 人工复核）、TSK-110/111（隔离 tab 重测）、TSK-112（B 矩阵×5 插件）、TSK-113（LICENSES 扩展）为 Phase 1 入口前置；其余按 TASK-INDEX 执行。

## DoD 对照

- [x] research 齐备 + 来源日期 + 需实测转 TSK
- [x] DECISIONS ≥25 条 + 默认值 + 反转条件（27 条 Accepted）
- [x] EVALUATION（H≥5、RSK≥12、kill、许可矩阵）
- [x] ARCHITECTURE（线程/IPC/数据结构/恢复，与 L1–L11 无冲突）
- [x] ROADMAP（Phase 0–4 验收 + 演示脚本）
- [x] TASK-INDEX（P0 可测 + 依赖 + 规模）
- [x] AGENTS.md（红线/工作流/状态/变更）
- [x] 骨架 + CI 绿灯
- [x] 本报告 + 未决清单（见各步暂停记录）
