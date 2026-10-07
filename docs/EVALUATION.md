# EVALUATION（可行性评估）

- 目的：基于 Research Pass 与 DECISIONS，对技术可行性做可证伪评估，登记风险、kill criteria 与许可矩阵，决定是否进入架构设计。
- 适用范围：SynthLM 全工程；约束：非商业、无分发、不上传（模型推理链经 2026-10-06 人类授权例外）、不购买许可；REAPER 最低 v7.60，基线 v7.82。
- 状态：Accepted（风险登记与 kill criteria 持续有效；§8 对账见 2026-10-07 更新）
- 最后核验日期：2026-10-06
- 依赖文档：docs/research/A01–A06、B、C-models-retrieval-eval、C-dsp-toolchain、D-eng-eco；docs/DECISIONS.md（DEC-001–027，其中 DEC-010/011/013 已按云端预设更新）。

## 1 问题陈述与成功标准（可度量）

- 问题：REAPER 7 工程中，用户给一句自然语言意图 + 一段参考音色，得到多个可用候选（第三方插件参数调节、采样派生编辑、轻度混音辅助），可一键应用、A/B 对比、原子回滚。
- 产品级成功标准（真实工程，5 分钟闭环）：
  - S1：3 个可用候选生成并可试听，端到端 ≤5min（含渲染与评价）。
  - S2：REAPER 零崩溃（bridge/侧车崩溃不带走工程，任务 BLOCKED 而非丢工程）。
  - S3：工程零破坏（应用前快照齐全；任何应用可一键整体回滚；音频文件一律派生，禁止原地覆盖）。
  - S4：候选附差异摘要与置信度（L6），评分先 LUFS 归一（L5）。
- Phase 0 可自动化验证代理指标：spike 通过率（A04 类实测全过）、golden 音频回归区间稳定（同机三次方差内）、`+6dB` 防作弊回归（响度提升必不涨分）。

## 2 技术可行性（逐项 verdict）

- REAPER 集成：可行（有条件）。链/参数/预设/发送 API 全存在（A01/A02/A05 高置信）；容器可编排但地址脆弱，须地址重算层 + GUID 锚定 + 展平回退（A02）。残留风险 RSK-001/002。
- 参数语义：可行（有条件）。元数据可枚举但单位/默认值/step/分组稀疏（B 高置信）；声设识别靠 automatable gate + 分组 + 白名单宏，不靠关键词硬过滤；Kontakt/Serum 矩阵类标 `preset-only`。残留 RSK-003/004。
- 模型栈：云端 OpenCode Go（`https://opencode.ai/zen/go/v1`，Tier1 `muse-spark-1.3-contributor` / Tier2 `mimo-v2.6-flash` ZDR，Key 仅 `.env`）为主，Tier3 本地唯一候选 `Bonsai-2-27B`（llama.cpp，纯文本；2026-10-06 接替已废弃 Gemma 4 12B）。CLAP 保留作音频嵌入（检索/评价），非规划候选。残留风险：连通性/有效率/延迟均 ⚠️需实测（RSK-005/006；条款与计费已由人类确认）。
- 评价器：可行。Rust 链完整（ruststft + ebur128/stream + CLAP cosine），纪律是 decode→48k→固定 STFT→LUFS 归一→多指标；陷阱是响度作弊与窗/跳不一致（C-models §4，L5）。残留 RSK-007。
- 音频 DSP：可行（有条件）。symphonia 主力 + FFmpeg 兜底；rubato 无风险；响度三 crate 无风险；Rubber Band 不购证则内部 GPL 运行或切 timestretch（不分发即不触发传染，但仍登记）；Demucs 官方权重仅科研，内部非商业可用，产品化另议（DEC-025）。残留 RSK-008/009。
- GUI：可行。独立 egui 主窗 + Lua 薄面板（D）；FX 嵌入否决；egui/iced 二选一待原型。残留 RSK-010。
- 进程模型：可行。acrd 侧车 + bridge 主线程队列 + IPC（interprocess）+ 共享内存大数据；音频线程零分配零锁（L9）；Undo 后指针重取已实证（A04 spike03f）。残留 RSK-011/012。

## 3 对抗性分析（危险假设 + 证伪路径）

- H1“容器地址层在真实工程必崩”：证伪 = 目标工程连续 50 次容器内外移动/增删后重算成功率 ≥95%，否则展平回退覆盖率 100%。关联 RSK-001。
- H2“渲染不可比（块对齐/插件离线不兼容污染评价）”：证伪 = 同参数三次渲染 null-test 差值 <阈值（固定块 64 + 固定 bounds）；全速失败按小块→1x→online 兜底后仍不可比即定罪。关联 RSK-007。
- H3“云端有效率/延迟撑不起 5 分钟闭环”：证伪 = Patch schema 首轮有效率 ≥95%（2 轮修复后 ≥99%），P95 全链路 ≤10s/候选；否则收紧 schema 或切本地端点。关联 RSK-005。
- H4“第三方参数元数据稀疏到白名单建不起来”：证伪 = 目标插件集（≥5 款：JSFX/ReaEQ/VST3/CLAP/采样器各一）20 行枚举脚本跑通率：名/范围 100%、automatable gate 有效、section 空率可接受（回落白名单）；否则扩大白名单人工标注。关联 RSK-003。
- H5“评价器高分≠好听（响度作弊/频段偏置）”：证伪 = `+6dB` 回归必不涨分 + 盲听标定（候选排序与人工排序 Spearman ≥0.5）；否则调权重/加瞬态项。关联 L5、RSK-007。
- H6“一次应用即弄脏/弄坏工程（undo 漏点/指针悬空）”：证伪 = 故障注入（pooled MIDI、容器内 FX、take FX）下 100 次应用/回滚零残留（ValidatePtr2 全重取 + P_EXT 回滚实证复现）；出一次不可逆即定罪。关联 RSK-011。

## 4 风险登记册

| ID | 风险 | 概率×影响 | 缓解 | 触发信号 | 负责人 |
|---|---|---|---|---|---|
| RSK-001 | 容器地址 stride 脆弱致误操作 | 中×高 | 地址重算层 + GUID 锚定 + 操作前后 `container_count` 校验 + 展平回退 | 容器任务失败率 >5% | 桥接负责人 |
| RSK-002 | FX GUID 跨复制/撤销不稳定 | 中×高 | 永不持久化裸索引；ident/路径 + 每次重查；六组实测 | 重查命中率 <95% | 桥接负责人 |
| RSK-003 | 第三方元数据稀疏（单位/默认/section 缺失） | 高×中 | automatable gate + 白名单宏 + 关键词仅排序；20 行脚本矩阵 | 白名单覆盖 <80% 目标参数 | 语义层负责人 |
| RSK-004 | 大参数插件（Kontakt/Serum 矩阵）不可单参 | 高×中 | 标 `preset-only` + 整预设/morph；Kontakt 只收 slot 子集 | 单参写入无声变化 | 语义层负责人 |
| RSK-005 | 云端连通性/有效率/延迟不达标 | 中×高 | 超时 30s + 2 次退避 + 本地回退 + 审计；schema 收紧 | P95 >10s / 有效率 <95% | 模型负责人 |
| RSK-006 | API Key 泄漏/费用失控（`.env`） | 低×高 | Key 只读 `.env`（已 gitignore），禁进日志/快照；调用审计（字节数/字段清单）；费用上限告警 | 审计发现 Key 出镜或超预算 | 安全负责人 |
| RSK-007 | 渲染非确定性污染评分 | 中×高 | 固定块/bounds；null-test 门禁；小块→1x→online 兜底；+6dB 回归 | 同参三次方差超阈 | 评价器负责人 |
| RSK-008 | Rubber Band/Demucs 许可误用 | 低×高 | 不购证→仅内部运行 + 登记；分发/商用即冻结重审（DEC-025） | 出现分发物含 GPL/NC 权重 | 合规负责人 |
| RSK-009 | stem/大文件拖慢闭环或撑爆磁盘 | 中×中 | 后台队列 + 内容寻址缓存 + 工程只存指针 + GC；CPU 分钟级预期管理 | 缓存 >50GB / 命中 <50% | DSP 负责人 |
| RSK-010 | GUI 原型不达标（egui 高频刷新/HiDPI） | 中×中 | 波形 + 100Hz 原型后锁 egui/iced；Lua 面板解耦 | 原型帧率/输入任一挂 | UI 负责人 |
| RSK-011 | undo/指针失效致工程残留 | 中×高 | 一求解一 undo 点 + 逐轨 dirty + 用后重取；故障注入 100 次零残留 | 残留事件 ≥1 | 桥接负责人 |
| RSK-012 | IPC 跨平台 pipe/权限/路径坑 | 中×中 | interprocess + 短路径 + 三 OS 建连/断线/权限用例；共享内存大数据 | 任一 OS 用例红 | 平台负责人 |
| RSK-013 | 范围蔓延（自研采样引擎/移动/Web/商用分发） | 中×高 | AGENTS 拦截规则 + DEC-025 分发冻结线；Phase 3+ 议题另立 | 出现 L1–L12 例外 PR | 创始 Agent |
| RSK-014 | `.env`/Action ID/插件名硬编码脆弱（`ReaEQ` vs 全名、`instantiate=0` 仅查询） | 中×中 | 实测命名 + `APIExists` 守卫 + Action list 人工复核；NCH 先加 FX 再设 | 目标机查询返 -1/nil | 桥接负责人 |

## 5 Kill criteria 与反转条件

- K1：H3 证伪失败（云端 + 本地回退双双撑不起闭环）→ 冻结模型链投入，转纯检索 + 人工确认模式。
- K2：H2 证伪失败（渲染不可比且兜底无效）→ 冻结自动评价，转人工 A/B（评分仅作参考）。
- K3：H6 定罪（一次不可逆工程损坏）→ 冻结一切写操作版本，直至 100 次故障注入零残留。
- K4：分发/商用前提被打破（对外给二进制/权重）→ 冻结并走法务重审（DEC-025 反转条件）。
- K5：连续两阶段里程碑演示脚本不可复现 → 回到 Research Pass 补 spike，不进入实现。
- L1–L12 反转：仅 ADR 流程 + 人类拍板；L3 已被 2026-10-06 云端决策部分触发（限模型推理链，审计约束不解除）。

## 6 许可与合规矩阵（内部非商业无分发，不购证）

| 组件 | 许可 | 来源 | 内部使用结论 |
|---|---|---|---|
| VST3 SDK | MIT（3.8+） | 官网许可页 + GitHub | 可用，保留文本；商标另合规 |
| CLAP | MIT | free-audio/clap LICENSE | 可用 |
| iPlug2 | permissive | iPlug2 LICENSE.txt | 可用，逐文件头复核 |
| JUCE | AGPLv3/商业 | JUCE LICENSE + juce.com | 不用（不购证）；误引入即移除 |
| ReaLearn/Helgobox | GPL-3.0 | realearn LICENSE | 仅参考设计，不链接/复用 |
| reaper-rs | MIT | helgoboss/reaper-rs | 可用（git master  pinned） |
| symphonia | MPL-2.0 | crates.io/GitHub | 可用，改文件须开源该文件 |
| FFmpeg | LGPL-2.1+（禁 gpl/nonfree 构建） | ffmpeg.org/legal | LGPL 构建动态链接 + `buildconf` 确认 |
| Rubber Band | GPL-2-or-later/商业 | breakfastquay 许可页 | 不购证→仅内部运行；分发即违法 |
| rubato/bs1770/ebur128/ebur128-stream/egui/iced/interprocess/USearch/LanceDB | MIT/Apache/0BSD | 各仓库 LICENSE | 可用，保留声明 |
| Demucs 代码/官方权重 | 代码 MIT / 权重科研限定 | demucs LICENSE + issue #327 | 内部非商业可用；商用/分发重授权 |
| MERT/MuQ 权重 | CC-BY-NC-4.0 | HF license 字段 | 内部非商业可用；不进产品基线 |
| CLAP 权重 | HF apache-2.0（链条待法务确认） | laion/clap-htsat-fused | 内部可用；确认后转正 |
| Qwen2-Audio 权重 | Apache-2.0 | Qwen HF/仓库 | 可用（含云端调用场景以 API 条款为准 ⚠️需复核 OpenCode Go 条款） |
| OpenCode Go 云端 | 用户确认条款（2026-10-06）：muse-spark-1.3-contributor 保留数据训练模型；ZDR 档 mimo-v2.6-flash；不声明计费 | 三档授权 + 审计 + 白名单可执行测试；Key 仅 `.env`；连通性仍 ⚠️需实测 |

## 7 成本估算

- 开发工时（量级）：Phase 0 剩余（DEC→ARCH→ROADMAP→TASK→AGENTS→骨架）S；Phase 1 桥接 + 快照 + 渲染线 M；Phase 2 语义层 + 检索 + 评价 L；Phase 3 搜索 + UX M；Phase 4 加固 + 标定 M。具体拆分见 ROADMAP/TASK-INDEX。
- 推理算力：主路径云端按次计费（单价/限额未核验，⚠️需实测首账单；审计字节数即成本 proxy）；本地回退零边际（CLAP ort CPU 可忽略；llama-server 按需起停）。
- 存储：CLAP 按需（<1GB）；Tier3 Bonsai-2-27B 服务中（llama.cpp :8080）；Demucs ONNX 166MB–1.26GB 懒下载；索引（10k 预设 × 512维 ≈ 20MB 级 + 载荷）；产物缓存设 50GB 水位 + GC。

## 8 未决问题清单（2026-10-07 对账）

1. ~~首轮连通性实测~~ → **已关闭（部分）**：Tier2 真 200（TSK-117）；Tier1 未点火、4xx 超出 401/429 分类仍待真端点（TSK-116 TODO / Phase 7 TSK-701）。
2. 费用模型确认（单价、限额、告警阈值）→ 仍开放；人类维持不声明计费，审计字节数作 proxy；解除需人类书面立项（F/B-002 冻结）。
3. ~~本地回退端点地址与模型档~~ → **已关闭**：Tier3=`Bonsai-2-27B` llama.cpp `:8080`，纯文本；音频能力缺失即 BLOCKED（TSK-305）。
4. ~~上传字段白名单终稿~~ → **已关闭**：`prompt/mir/meta/audio_ref` + fail-closed 审计单测（TSK-106/118）。
5. ~~是否放行 ARCHITECTURE~~ → **已放行并 Accepted**（2026-10-07 状态对齐）。
6. **新增（2026-10-07 评估）**：产品闭环缺口（acrd 未接线成守护进程、E2E 为 seeded demo）→ Phase 5（TSK-5xx）；能力补全（CLAP/Demucs/真检索后端）→ Phase 6（TSK-6xx）；代理矩阵/HiDPI/跨用户/扩大盲听等真实面 → Phase 7（TSK-7xx）。冻结项单列 F/B-nnn。
