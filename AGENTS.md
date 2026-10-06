# AGENTS.md — SynthLM 后续所有 Agent 会话的强制契约（最高优先级）

- 适用范围：本仓库后续一切 Agent 会话（调研/设计/实现/测试/文档）。与本文冲突者，以本文为准。
- 状态：Accepted（人类 2026-10-06 确认）
- 人类已锁定约束：非商业、无分发、不购买许可；云端三档授权（Tier1 muse-spark-1.3-contributor 训练保留 / Tier2 mimo-v2.6-flash ZDR / Tier3 本地 Gemma 4 12B 唯一候选）；REAPER 最低 v7.60。

## 1 角色与权限

- Agent 可自主决策：实现细节、重构、测试补齐、文档措辞、P2/P3 任务排序。
- 必须停下问人（输出 BLOCKED + 候选方案，禁猜测）：涉及许可（新增依赖、分发物、GPL/NC 触线）、删除/覆盖用户数据、改变已锁定决策 L1–L11 或 DEC-001–027 反转、扩大范围（移动/Web/商用/自研采样引擎）、云端上传超白名单、Tier3 本地模型更换、费用发生。

## 2 工作流契约

- 先调研后设计，先测试后实现（可测部分）；增量提交（Conventional Commits：`docs:/feat:/fix:/chore:/spike:`）。
- 每会话开头必读：本文件 + `docs/TASK-INDEX.md` + 相关 ADR/ARCH 章节。
- 每步暂停制：完成一步只给「产出摘要 + 未决问题清单」，不倾倒全文。
- 每个任务完成必更新 TASK-INDEX 状态 + 证据链接；改架构/行为必同步对应文档（文档 slaves 代码）。
- spike 放 `experiments/`（命名 `aXX-*.lua` + 同名 `.out.txt`），不进 `src/`，不进主干验收；真机脚本必须可重跑（`-nonewinst` + 输出文件）。

## 3 禁止事项（红线，违反即返工）

1. 不宿主/不加载第三方插件二进制；不逆向、不 hook、不 patch 第三方插件（VST2 只发参数值）。
2. 音频线程零分配、零锁、零网络、零文件 I/O、零日志；`TrackFX_SetParam` 禁止在音频线程调用。
3. 模型推理、音频分析、渲染等待永不进 DAW 进程（L8）；bridge 只做控制面。
4. 不凭印象写外部 API：断言必带来源链接 + 核验日期，不确定标 `⚠️需实测` 并登记 TSK。
5. 不静默删除/覆盖用户工程与音频文件：一律派生新文件 + provenance（`P_EXT:SYNTHLM_*`），应用必经 undo 事务块且可整体回滚。
6. 不引入未登记许可的依赖：新增依赖同步更新 `docs/LICENSES.md`；GPL/AGPL 未购证与 NC 权重仅限内部运行，出现分发物即冻结（TSK-901）。
7. 不在 UI/日志/快照外泄 Key 与音频内容：Key 只读 `.env`（已 gitignore），缺失即 BLOCKED；原始音频默认不出网，上传字段走白名单审计单测。
8. 不跳过 AGENTS.md 写业务代码；`src/` 业务代码在 ARCHITECTURE + TASK-INDEX Accepted 前不得出现（骨架除外）。

## 4 代码规范

- Rust Edition 2024 统一（人类 2026-10-06 拍板），`clippy -D warnings` + `rustfmt` 零警告；公共 API 必备 rustdoc。
- 错误类型：库边界 `thiserror`，应用/脚本层 `anyhow`；`unwrap/expect` 禁止进主干（测试/spike 除外）。
- `unsafe` 许可制：仅 `bridge` 内 low 封装可用，必须逐处注释安全论证（前置条件/不变量/调用方义务）。例外（人类 2026-10-06 批准，仅此两处）：`common/shm.rs` 内 `shared_memory 0.12.4` 最小访问面 2 处（创建后独占映射读/冻结发布者有界拷贝，均 SAFETY 注释 + 单测覆盖 + 非音频/DAW 路径）；新增例外须人类另行批准。
- 命名：裸 FX 索引永不持久化，持久化用 ident/GUID/容器路径；`TakeFX_AddByName` 新建必须 `instantiate<0`（`0` 仅查询）；`I_TAKEFX_NCH` 先建实例再设；`I_MIXFLAG` 等版本 gated API 必 `APIExists` 守卫。
- crate 边界（DEC-022）：`common` ← `profile` ← `planner`/`retrieval`/`eval` ← `acrd` ← `bridge` ← `ui`；bridge 禁止依赖模型/分析；依赖方向违规 CI 拦截。

## 5 测试与 CI

- CI 必跑：`clippy` + `test` + `fmt --check` + `doc` + 文档同步检查（头字段/稳定 ID）；任一红即 BLOCKED。
- 单元测试覆盖核心逻辑；golden 音频回归：固定输入 → 固定评分区间，含 `+6dB` 防作弊回归（响度提升必不涨分）；同参三次渲染 null-test 方差门禁。
- bridge 集成测试用 mock REAPER API；真机 spike 只做证据，不进主干验收。
- 验收标准：P0 任务必有可自动化验证项；性能预算（单块 500 参数 <1s、撤销 <500ms）由 TSK-403 门禁。

## 6 状态维护

- TASK-INDEX 是唯一任务真源：状态机 Todo → In-Progress → Done（Blocked 须写原因 + 解除条件，参照 TSK-901/902）。
- DECISIONS 状态机：Proposed → Accepted →（Superseded by DEC-yyy）；推翻已锁定决策必须先走 §7 流程。
- 每会话结束：更新任务行状态 + 证据链接，附未决问题清单；禁止把“已验证”写给未跑过的结论。

## 7 变更流程

- 推翻 L1–L11 或 DEC 反转条件触发：开新 ADR（背景/选项/推荐/反转条件/影响面/核验依据），人类拍板前原决策继续有效。
- 范围变更拦截：凡涉 §1 问人清单，一律输出 BLOCKED + 候选方案，禁止猜测执行。
- 模型链变更：Tier 档位、上传字段白名单、Tier3 模型更换（当前 Gemma 4 12B 唯一候选）必须人类拍板；本地端点实测（TSK-305）在实现阶段执行，缺音频能力即报错（禁静默降级）。
- 遇到未决决策：输出 BLOCKED 并附候选方案，禁止用默认值蒙混。

## 8 日志与隐私

- 默认不出网：三档授权外一律不出网；授权档存设置（首次弹窗 + 设置页可改）；每次云端调用记审计（时间/模型/授权档/字段清单/字节数）。
- 日志脱敏：禁 PCM、禁 prompt 全文、禁绝对路径（用指纹 + 相对路径）；遥测默认关，opt-in 才开最小集。
- `.env` 永不提交；审计发现 Key 出镜或超范围字段即按 RSK-006 处理（切断云端转 Tier3）。
