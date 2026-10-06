# ROADMAP（路线图）

- 目的：定义 Phase 0–4 目标、交付物、可验证验收、依赖、规模、Kill/反转条件，以及里程碑演示脚本、偿债计划与资源假设。
- 适用范围：SynthLM 全工程；约束：非商业、无分发、不购买许可；三档授权（Tier1 训练保留 / Tier2 ZDR / Tier3 本地）；本地端点测试递延到实现阶段（人类 2026-10-06）。
- 状态：Draft
- 最后核验日期：2026-10-06
- 依赖文档：docs/DECISIONS.md、docs/EVALUATION.md、docs/ARCHITECTURE.md。

## 1 长期愿景与非目标

- 愿景：REAPER 7 内一句意图 + 一段参考音色，5 分钟得 3 可用候选，一键应用、A/B 对比、原子回滚，零崩溃零破坏。
- 非目标（不做什么）：不做插件宿主（L1）；不逆向/hook（L2）；不做商用分发；不做移动/Web；不自研采样引擎（L7，Phase 3+ 议题另立）；不声明计费（人类决策）；本地端点不在 Phase 0 定档（实现阶段实测）。

## 2 阶段划分

### Phase 0 立项调研设计（本阶段，S）
- 目标：工程契约齐备，可长期执行。
- 交付物：research 10 篇 + DECISIONS（27 条 Accepted）+ EVALUATION + ARCHITECTURE + ROADMAP + TASK-INDEX + AGENTS.md + Cargo 骨架 + CI 绿灯 + 阶段报告。
- 验收（自动优先）：`docs/` 13 文件存在且头字段全；DEC 27 条无禁用语；EVAL H≥5/RSK≥12；ARCH 11 章；`cargo clippy/rustfmt/test/doc` 绿。
- 依赖：REAPER v7.82 实测机（已具备）。
- 规模：S。Kill：连续两里程碑不可复现即回 Research。

### Phase 1 桥接与安全底座（M）
- 目标：REAPER 内可快照、可写参、可回滚、可渲染一度量， consent 与审计先行。
- 交付物：`bridge`（FX/参数/包络/undo/P_EXT/NCH/地址重算/GUID 锚定）+ 快照/回滚 + 42230 渲染线（固定块/bounds + null-test）+ 三档 consent 弹窗/设置页 + 上传白名单审计单测 + IPC hello/帧 + golden 空架。
- 验收：故障注入 100 次应用/回滚零残留；`+6dB` 回归基线；spike03c/f 复现（T1_delta=1、指针重取）；缺 Key/ACTION 未复核即 BLOCKED。
- 依赖：Phase 0 Accepted。规模：M。Kill：K3（一次不可逆损坏即冻结写操作）。
- 反转：42230 阻塞>5s 则切 temp-tab 轮询（DEC-005）。

### Phase 2 语义与检索评价（L）
- 目标：Profile 资产 + 可比评价 + 可用召回。
- 交付物：`profile.json` schema + 首批白名单（≥5 款插件，20 行脚本矩阵全过）+ MIR v1 + USearch/LanceDB 原型二选一（10k 基准：构建<30min、P95<100ms）+ 评价器（LUFS 归一 + 多目标 + 防作弊回归）+ stem/DSP 缓存 + GC。
- 验收：白名单覆盖 ≥80% 目标声设参数；NCH 先建实例再设（spike04 复现 `2.0→8.0`）；盲听前测 Spearman≥0.5 起步；检索原型基准自动跑。
- 依赖：Phase 1 桥接。规模：L。Kill：K2（渲染不可比且兜底无效→转人工 A/B）。
- 反转：10k 构建>30min 则采样构建（DEC-014）。

### Phase 3 搜索与 UX 闭环（M，可重写模块）
- 目标：5 分钟 3 候选完整闭环。
- 交付物：model-gw 三档路由（Tier1/Tier2 responses + Tier3 本地兼容端点 + 音频探测报错）+ Patch schema 双校验 + TPE/CMA-ES 粗搜 + Nelder-Mead 收尾 + 多样性去重 + 候选卡片 6 字段 + 本地端点实测（vLLM/llama.cpp/MLX LM/LM Studio，无量化限制）。
- 验收：端到端 ≤5min 出 3 可用候选；Patch 首轮有效率 ≥95%（2 轮修复 ≥99%）；云端 P95 ≤10s/候选；本地探测缺音频即 BLOCKED。
- 依赖：Phase 2。规模：M。Kill：K1（双链撑不起闭环→转纯检索 + 人工确认）。
- 反转：有效率不达标则收紧 schema（DEC-013）；本阶段允许重写检索后端/权重/UI 引擎/本地档。

### Phase 4 加固与标定（M）
- 目标：达到产品级成功标准 S1–S4。
- 交付物：100× 故障注入报告 + 盲听标定 + 性能预算（500 参数<1s、撤销<500ms）+ 缓存水位 + 文档同步 + 终验阶段报告。
- 验收：真实工程 5 分钟 3 候选复现；零崩溃；零不可逆；差异摘要齐全。
- 依赖：Phase 3。规模：M。Kill：K5（演示不可复现→回 Research）。

## 3 里程碑与演示脚本（可复现）

- M0 Phase 0 门禁：`ls docs/research/*.md` 计 10 + `cargo clippy -- -D warnings && cargo test && cargo doc` 绿 + DEC 计数 27。复现：全新 clone 按顺序跑同一命令。
- M1 安全底座：在空工程跑 `spike02→spike03f`（快照→改速→undo→P_EXT 回滚→删轨归零），History 恰增预期点数，工程 track/item 计数归零。复现：`-nonewinst` 依次跑三脚本读 `.out.txt`。
- M2 语义评价：对 5 款目标插件跑 20 行枚举脚本 + NCH `2.0→8.0` + `+6dB` 不涨分回归绿。复现：同一脚本同一工程跑三次。
- M3 闭环：在参考工程输入固定意图 + 固定参考音频，计时 ≤5min 得 3 候选，各试听正常，一键应用后一键回滚，审计日志字段齐全。复现：种子固定 + 授权档 Tier3（本地）可离线重跑。
- M4 终验：M3 + 100× 故障注入零残留 + 盲听排序相关达标 + 阶段报告。

## 4 阶段间技术债偿还

- 每阶段预留 20% 带宽清 ⚠️需实测 TSK（A04 M1/M3/M6/M7、B 矩阵、C 导出/延迟、D 原型三项）；原型二选一（检索/UI）在用它的阶段前一期锁死，不拖入闭环阶段。
- 文档 slaves 代码：改架构必改 ARCHITECTURE + DEC（Superseded 链），改行为必补 TASK-INDEX 证据；CI `doc` 环节拦截缺头字段文档。

## 5 资源与算力假设

- 人力：单 Agent 流 + 人类决策点拍板；Phase 2 为长周期（L），其余 S/M。
- 算力：云端按次（不声明计费，审计字节数即 proxy，设用量告警）；本地回退零边际（CLAP CPU 可忽略，llama-server 按需起停）。
- 存储：索引 10k×512维约 20MB 级 + 载荷；模型懒下载（Demucs 166MB–1.26GB）；产物缓存 50GB 水位 + GC；`.rpp` 只存指针。
- 机器：主力 Windows + REAPER v7.60–7.82；macOS/Linux 为次发（IPC 三 OS 用例在 Phase 1 即跑）。
