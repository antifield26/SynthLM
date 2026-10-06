# M4-REPORT（终验报告）

- 目的：Phase 4 关闭项——发布门禁结果、E2E 缺口诚实声明、移交后续。
- 适用范围：M4（ROADMAP 定义：M3 + 100× 注入 + 盲听 + 本报告）。
- 状态：Accepted
- 最后核验日期：2026-10-06
- 依赖文档：docs/TASK-INDEX.md、docs/ROADMAP.md、scripts/m4_gate.py。

## 1 完成

- `scripts/m4_gate.py` 一次通过：16 证据文件齐 + 0 未关闭任务（TSK-901/902 冻结除外）。
- 100× 故障注入零残留（fault-inject-100/401 + pooled 真对）；盲听 ρ=0.825/1.0；全门禁绿。

## 2 验证

- cargo 全套件绿 + docsync 绿 + doc 零警告（本轮复核）。
- 云端 Tier1/2 真调用通过；Tier3 文本健康、音频 BLOCKED；扩展 smoke 通过。

## 3 新事实

- E2E 缺口：各部件经 mock/分段验证，但“5 分钟交互闭环”尚无单命令可跑形态（acrd 未接线成可运行守护进程）。不虚构为已验证。

## 4 修正

- 无权重/决策反转；DEC-010 Tier3→Bonsai 与分层调整已于前期落定。

## 5 阻塞与移交

- 新立 TSK-405（交互式 E2E M3 harness：acrd 接线 + 固定种子演示，需人机协同）。
- 待人类：pooled ghost 残留删除、新权重（TSK-305 已关，按需重开）、听音已完成归档。
