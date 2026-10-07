# ROADMAP（路线图）

- 目的：定义 Phase 0–4 目标、交付物、可验证验收、依赖、规模、Kill/反转条件，以及里程碑演示脚本、偿债计划与资源假设。
- 适用范围：SynthLM 全工程；约束：非商业、无分发、不购买许可；三档授权（Tier1 训练保留 / Tier2 ZDR / Tier3 本地）；本地端点测试递延到实现阶段（人类 2026-10-06）。
- 状态：Accepted（Phase 0–4 已关闭；Phase 5 规划见 §2b，2026-10-07）
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

### Phase 5 产品闭环与能力补全（L，2026-10-07 立项）

- 背景：Phase 0–4 关闭后评估结论——治理/安全/部件验证一流，但 **产品闭环仍是 seeded demo**（无 acrd 守护进程、无真模型进环、无 42230 真渲染进评价），且 CLAP/Demucs/真检索后端等能力缝未接线。本阶段并行补齐 **产品主路径** 与 **能力补全**。
- 目标：单命令可复现的真·5 分钟 3 候选（意图+参考音频 → 真模型/检索 Patch → 真渲染评价 → UI 试听应用回滚）；评价/检索/DSP 不再依赖占位。
- 交付物：
  1. **产品闭环（M5）**：acrd 守护进程（IPC 服务循环/任务状态机/WAL/watchdog）+ bridge↔acrd 真接线 + model-gw 真调用替换 demo 种子 + 42230 渲染进 `eval` + UI 主窗候选闭环 + 单命令 E2E。
  2. **能力补全（M6）**：CLAP ONNX 嵌入进 score/去重 + LanceDB 真后端 10k 基准 + Demucs ONNX 后端 + Profile/patch 残留校验 + WAL/DEC-027 迁移。
  3. **真实面扩大（M7）**：Tier1 点火 + 代理矩阵 + HiDPI/CPU + 跨用户 IPC + 扩大盲听 + 跨机性能复测。
- 验收（产品向，替代“库级绿”）：
  - 单命令 `acrd e2e --seed N`（或 runbook 等价物）一次跑通：真意图解析 → ≥3 候选 → 试听文件可播 → 应用/回滚零残留 → 审计字段齐全；
  - 候选排序含 CLAP 项（非 `None`）；检索查询 P95 达标（真 crate）；
  - 有/无系统代理下 transport 分类单测全绿；
  - 5 分钟闭环计时 ≤5min（人机协同见证表追加行）。
- 依赖：Phase 1–4 部件。规模：L。Kill：
  - K6：acrd 接线后连续两轮 E2E 不可复现 → 回退保持 demo 形态并冻结产品宣称；
  - K7：CLAP/Demucs/真检索三者中 ≥2 项无法在预算内接线 → 降级发布“文本规划 + 参数检索”子集并另立能力专项。
- 反转/降级：云端有效率 <95% 经 2 轮修复仍不达标 → 收紧 schema 或 Tier3 文本规划 + 人工确认（K1 已有）；渲染不可比 → K2 人工 A/B；出现不可逆残留 → K3 冻结写。
- 明确非目标（本阶段仍不做）：移动/Web、商用分发、自研采样引擎、购买 RB/JUCE、Tier3 模型更换。

## 2b 与 Phase 0–4 的衔接

- Phase 0–4 任务保持 Done 不回滚；评估暴露的残留债以 Phase 5 TSK 行 **转正**（禁止只留在证据栏）。
- 文档状态机：Phase 5 开表后允许 `Todo`/`In-Progress`；关闭时 TASK-INDEX 必须回到无活动行或 Blocked（含原因）。
- 代理策略已定（ARCH §5）：云端尊重系统代理，loopback/hermetic 永不代理——相关回归进 M7。

## 3 里程碑与演示脚本（可复现）

- M0 Phase 0 门禁：`ls docs/research/*.md` 计 10 + `cargo clippy -- -D warnings && cargo test && cargo doc` 绿 + DEC 计数 27。复现：全新 clone 按顺序跑同一命令。
- M1 安全底座：在空工程跑 `spike02→spike03f`（快照→改速→undo→P_EXT 回滚→删轨归零），History 恰增预期点数，工程 track/item 计数归零。复现：`-nonewinst` 依次跑三脚本读 `.out.txt`。
- M2 语义评价：对 5 款目标插件跑 20 行枚举脚本 + NCH `2.0→8.0` + `+6dB` 不涨分回归绿。复现：同一脚本同一工程跑三次。
- M3 闭环：在参考工程输入固定意图 + 固定参考音频，计时 ≤5min 得 3 候选，各试听正常，一键应用后一键回滚，审计日志字段齐全。复现：种子固定 + 授权档 Tier3（本地）可离线重跑。
- M4 终验：M3 + 100× 故障注入零残留 + 盲听排序相关达标 + 阶段报告。
- M5 产品闭环：空工程 + 固定意图/参考音频，`acrd e2e --seed N` 单命令出 3 真候选（model-gw 或受控 stub 明示），42230 渲染进评价，UI 或脚本完成应用→回滚，审计齐全。复现：同 seed 两次字节级计划一致（模型 stub 模式）或排序稳定（真模型模式）。
- M6 能力验收：CLAP 距离去重生效（同质 fixture 被合并、异质保留）；LanceDB 10k 真 crate P95 <100ms；Demucs stub→ONNX 可跑通一条 stem 并进缓存 GC；patch SELECT/add 校验红绿用例齐。
- M7 真实面：Tier1 至少一次 2xx 或书面 BLOCKED 原因；代理开/关矩阵全绿；HiDPI 截图无 tofu；跨用户 IPC 用例绿或显式 BLOCKED。

## 4 阶段间技术债偿还

- 每阶段预留 20% 带宽清 ⚠️需实测 TSK（A04 M1/M3/M6/M7、B 矩阵、C 导出/延迟、D 原型三项）；原型二选一（检索/UI）在用它的阶段前一期锁死，不拖入闭环阶段。
- 文档 slaves 代码：改架构必改 ARCHITECTURE + DEC（Superseded 链），改行为必补 TASK-INDEX 证据；CI `doc` 环节拦截缺头字段文档。

## 5 资源与算力假设

- 人力：单 Agent 流 + 人类决策点拍板；Phase 2 为长周期（L），其余 S/M。
- 算力：云端按次（不声明计费，审计字节数即 proxy，设用量告警）；本地回退零边际（CLAP CPU 可忽略，llama-server 按需起停）。
- 存储：索引 10k×512维约 20MB 级 + 载荷；模型懒下载（Demucs 166MB–1.26GB）；产物缓存 50GB 水位 + GC；`.rpp` 只存指针。
- 机器：主力 Windows + REAPER v7.60–7.82；macOS/Linux 为次发（IPC 三 OS 用例在 Phase 1 即跑）。
