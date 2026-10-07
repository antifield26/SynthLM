# M57-REPORT（Phase 5–7 终验报告）

- 目的：Phase 5–7 关闭项——M5 产品闭环、M6 关键能力、M7 真实面的交付核验、残留债移交。
- 适用范围：ROADMAP M5–M7（TSK-501–506、601–606、701–707）。
- 状态：Accepted（待人类确认）
- 最后核验日期：2026-10-07
- 依赖文档：docs/TASK-INDEX.md（63 Done＋F/B-001/002 冻结，0 活动行）、docs/ROADMAP.md、docs/DECISIONS.md（27 DECs intact）。

## 1 完成

- M5 闭环：TSK-505 单命令 E2E 经 Tier1 真机见证（`brighter highs` seed 7：3 真 patch 6/7/7 ops 零修复；应用→渲染→null-test 全等→参数全复原→清轨；审计恰 1 调用 tier1）；LiveTier1 路径落地；TSK-506 主窗卡片（6 字段＋选胜出＋应用预览＋缺字段红行）真机点选验证。
- M6 能力：601 CLAP 谱指纹后端（`clap_cos` 出 `Some`，去重半径 0.02）＋onnx 缝；602 真 crate 接入＋10k 独立复现（build 0.61s／P95 40.27ms／self-hit 100%）；603 Demucs 真权重单 stem（本地 CPU，cache 二次命中）；604 SELECT 枚举＋add 全红＋name_regex 编译 fail-closed；605 journal 重放＋migrate 矩阵＋只读回退；606 双路 RRF＋`audio_ref` 定稿。
- M7 真实面：701 Tier1 点火（200＋output 信封）＋4xx 终端分类；702 代理矩阵 6 格＋loopback 硬旁路；703 重绘抽稀（单核 56.6%→16.7%，−70.6%）＋96DPI 零 tofu；704 真双进程 shm（200×4KiB 零丢零错序 p95 4.71ms）＋跨用户 BLOCKED＋指引；705 第二轮盲听 11 clips 三组全对（ρ＝1.0×3，累计 n＝21）；706 单机预算复测（写 1–2ms／撤销 2ms）。

## 2 验证

- 本轮复核（2026-10-07）：`clippy --workspace --all-targets -D warnings` 零错误；`cargo test --workspace` 零失败；`fmt --check` 干净；`cargo doc --workspace --no-deps` 零警告；`python scripts/check-docs.py` doc-sync OK（19 docs，27 DECs）。
- 云端：Tier1 LIVE-OK（审计 1 调用，字段限 `[prompt,mir,meta]`，14 字节）；约 4 次 Tier1 调用量级个位数 KB。
- 反转求值：真 crate P95 ＋41.6% vs TSK-203 基线 → 触发“回退＞20% 则不晋升”规则，pattern 保持默认、real 常闭 opt-in（BENCH 手工追加，无 DEC 反转）。
- 盲听：首轮 n＝10（B 0.825／T 1.0／全 0.7455）＋次轮 n＝11 全对；权重 0.30／0.45／0.25 维持。

## 3 新事实

- 闭合白名单下 `add` 无合法形态：语义收紧为一律红（`AddToExistingTarget`，repair 移除），DEC-013 细化，无 ADR 反转。
- 回滚预览按钮合成点击约 10 次未翻转（相邻应用按钮同法成功；双臂对称＋构造单测绿）：判 harness 瞄准问题，未立为 bug，留人类亲手确认（见 §5）。
- `reacontrolmidi.json` 3 条目 ident／name 编码损坏（`3:`/`4:`/`8:` 通道类，预存）：加载与门禁不受影响（死条目），未猜测修复（见 §5）。
- 真 crate 特性构建需外部 `protoc`（本机无，借 temp-dir protoc 36.2 编过；增量缓存有效）：纯构建环境要求，已记 BENCH，不影响默认构建。
- `ort` 特性构建下载预构 ONNX Runtime 二进制（构建时联网；默认关闭，主构建离线）：许可已登记，非商业内部运行合规。

## 4 修正

- 无 DEC 反转：27 DECs 全部 Accepted 有效；评分权重维持；检索默认后端维持；Tier 档位与 Tier3 模型不变。
- 审计口径：`OnnxClapEmbedder::embed` 保持 BLOCKED（mel 标定 spike 前不出伪数），默认谱指纹生效。

## 5 阻塞与移交

- F/B-001（reesch／分发冻结）、F/B-002 持续有效，本报告不解除。
- 待人类（无截止，按需）：704 双用户 `runas` 手动观测；706 第二台目标机（或以 CI／用户机为准）；回滚按钮亲手一点；mel 标定 spike；Demucs FT／6s 钉选＋RTF 基线；fusion 公开重导出；渲染侧 audio_ref 碰撞 fail-closed；reacontrolmidi 三条目重探（b-matrix 重跑或已知参数表对照，禁猜）；CI 机装 `protoc`（真 crate 特性门）。
- 本报告落盘后 TSK-707 关闭，Phase 5–7 无剩余活动行。
