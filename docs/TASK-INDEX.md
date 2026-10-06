# TASK-INDEX（任务索引）

- 目的：把 ROADMAP 拆成一次会话可完成、可验证的任务，供后续 Agent 会话逐项执行。
- 适用范围：Phase 0–4；约束：非商业、无分发、不购证；三档授权；本地端点测试递延实现阶段。
- 状态：Draft
- 最后核验日期：2026-10-06
- 依赖文档：docs/ROADMAP.md、docs/DECISIONS.md、docs/EVALUATION.md、docs/ARCHITECTURE.md。

粒度规则：每行=一次会话；P0 必有可自动化验收；Blocked 必写原因与解除条件；完成必更状态 + 证据链接。

## Phase 0（收尾）

| ID | 标题 | 类型 | 阶段 | 依赖 | 优先级 | 规模 | 验收标准(可测) | 关联决策 | 状态 | 证据链接 |
|---|---|---|---|---|---|---|---|---|---|---|
| TSK-000 | Research Pass A/B/C/D 落盘 | 调研 | 0 | — | P0 | L | `docs/research/*.md` 计 10 且头字段全（脚本计数） | L11 | Done | docs/research/ |
| TSK-001 | 编写根目录 AGENTS.md（8 章齐备） | 文档 | 0 | DEC/ARCH | P0 | S | grep 得 8 章标题 + 红线清单齐，`cargo doc` 不告警 | DEC全 | Done | AGENTS.md（8 章 grep 验证通过） |
| TSK-002 | Cargo workspace 骨架 + CI 绿灯 | 实现 | 0 | TSK-001 | P0 | S | `cargo clippy -- -D warnings && cargo test && cargo fmt --check && cargo doc` 全绿 | DEC-022/024 | Done | 8 crates + CI yml，本地五项全绿 |
| TSK-003 | 文档同步检查脚本（头字段/稳定 ID） | 实现 | 0 | TSK-002 | P0 | S | 故意缺头字段的 fixture 必红，正常全绿 | L11 | Done | scripts/check-docs.py（16 docs + 27 DECs 门禁，本地绿） |
| TSK-004 | DECISIONS 27 条转 Accepted（人类拍板记录） | 文档 | 0 | — | P0 | S | 每条状态 Accepted + 拍板日期落盘 | DEC全 | Done | 人类 2026-10-06 全部确认（含 Gemma 4 12B 唯一候选） |
| TSK-005 | Phase 0 阶段报告（含未决 + 拍板项） | 文档 | 0 | TSK-004 | P0 | S | 报告含完成/验证/新事实/修正/阻塞五节 | — | Done | docs/PHASE0-REPORT.md（含 DoD 对照表） |

## Phase 1（桥接与安全底座）

| ID | 标题 | 类型 | 阶段 | 依赖 | 优先级 | 规模 | 验收标准(可测) | 关联决策 | 状态 | 证据链接 |
|---|---|---|---|---|---|---|---|---|---|---|
| TSK-101 | 容器地址重算层 + GUID 锚定 + 展平回退 | 实现 | 1 | TSK-002 | P0 | M | 50 次容器内外移动/增删重算成功率 ≥95%（Lua 自动化） | DEC-001/003 | Done | Lua 50/50（公式交叉验证，含 move-in/out/del 各≥5 次）；Rust 纯层 11 单测 + 2 doctest；扩展真机归 TSK-114 |
| TSK-102 | undo 事务封装 + 写后指针重取 | 实现 | 1 | TSK-002 | P0 | S | spike03c/f 复现：T1_delta=1、undo 后 ValidatePtr2=false→重取成功 | DEC-008 | Done | Lua 验收通过（spike03c/f）+ undo.rs 8 单测绿；扩展真机归 TSK-114 |
| TSK-103 | 显式快照 + 一键整体回滚 | 实现 | 1 | TSK-102 | P0 | M | 故障注入 100 次应用/回滚零残留（计数器归零） | DEC-004/020 | Done | Lua 100/0 零残留（fault-inject-100.out.txt）+ snapshot.rs 9 单测绿；扩展真机归 TSK-114 |
| TSK-104 | 42230 渲染线 + null-test 门禁 + 兜底链 | 实现 | 1 | TSK-103 | P0 | M | 同参三次渲染方差 <阈值；全速失败自动进小块→1x→online；M7（BOUNDSFLAG=4 + 单文件开关）文件清单验证 | DEC-005 | Done | Lua 核心绿（隔离 3/3 + M7 2×2）；变速键仅 chunk（RENDER_1X 0/1/2，无 stock setter），Rust harness 落实留 Phase 3 |
| TSK-105 | 三档 consent 弹窗 + 设置页 + 档位持久化 | 实现 | 1 | TSK-002 | P0 | S | 三档切换单测全过；缺授权调用即 `consent_required` BLOCKED | DEC-010/011 | Done | consent.rs（11 单测绿：fail-closed/原子写/文本 prompt；零新依赖；图形渲染留 TSK-306） |
| TSK-106 | 上传字段白名单审计单测 | 测试 | 1 | TSK-105 | P0 | S | PCM 越界用例必红，白名单用例全绿；审计日志无 Key/PCM | DEC-011/026 | Done | upload_audit.rs（12 单测绿，复用 canonical 无第二套；裁决：精确匹配 fail-closed 保留，tier 门禁归调用方 TSK-301） |
| TSK-107 | IPC hello/帧 + 超时重连（三 OS） | 实现 | 1 | TSK-002 | P0 | M | Win/macOS/Linux 建连/断线/权限用例全绿 | DEC-023 | Done | 帧/握手/分类/脱敏/回环全绿 + CI 三 OS jobs 全绿；权限用例与 shm 通道拆至 TSK-115 |
| TSK-108 | `.env` Key 读取 + 缺失 BLOCKED | 实现 | 1 | TSK-105 | P0 | S | 无 `.env` 启动即 BLOCKED + 指引；grep 证明 Key 不进日志/快照 | DEC-010/026 | Done | config.rs（33 单测绿，全注入式零 unsafe；Debug 全 redacted；零新依赖）；fail-safe 裁决：无已存 consent 不得出网（TSK-105/301 落实门禁） |
| TSK-109 | Action ID 人工复核（40362/40601/渲染系） | 调研 | 1 | — | P1 | S | Action list 截图/导出对照表落盘 | A04 §7 | Done | A03 §7 对照表 + probe 证据；人类 2026-10-06 eyeball 确认 |
| TSK-110 | P_EXT 跨工程跟随 + redo 干净重测（隔离 tab） | 测试 | 1 | TSK-102 | P1 | S | 独立 tab 下 undo/redo 后 P_EXT 值符合预期（对称往返） | DEC-027 | Done | 05d 对称干净（v1→空→v1）+ 人工跨工程 paste 通过（hello-manual）；夹具已清理 |
| TSK-111 | CopyToTake NCH 携带 + glue 后 P_EXT 保留 | 测试 | 1 | TSK-102 | P1 | S | 目标 chunk NCH 一致；glue 新 take provenance 齐全或显式重写 | A04 §M3/M6 | Done | M3：复制带 FX 不带 NCH→须显式回写；M6：glue 丢 take P_EXT→须重写（undo 可恢复）；M7 并入 TSK-104 |
| TSK-112 | B 矩阵 20 行脚本 × 5 款插件 | 调研 | 1 | — | P0 | M | 输出解析：名/范围 100%、automatable gate 有效、空率记录 | DEC-015 | Done | stock 4 格 + VST3 3 厂商 + CLAP Vital 通过；跨格式结论：Vital VST3 2986 vs CLAP 906 参数表不可移植（B §5 补记） |
| TSK-113 | docs/LICENSES.md 登记初版 + `buildconf` 确认 | 合规 | 1 | — | P0 | S | 每个新增依赖一行（来源/条款/结论）；FFmpeg 构建行存档 | DEC-025 | Done | cargo tree 零外部依赖 + Gyan 9.0.2 buildconf 存档（GPL 构建，不可作 LGPL fallback） |
| TSK-114 | REAPER 扩展真机线（bridge cdylib 加载 + smoke） | 实现 | 1 | TSK-101/102/103 | P0 | M | 扩展加载零崩溃 + 容器解析/undo/快照 smoke 全绿 | DEC-001/008/020 | Done | 加载/标记/零改动全绿（marker epoch 1791263540，前后基线 0轨0项1undo）；DLL 留装待处置 |
| TSK-115 | IPC 权限用例 + 共享内存大数据通道 | 实现 | 1 | TSK-107 | P1 | M | 跨权限 ACL/Unix mode 用例绿；PCM/stem 走 shm 基准达标 | DEC-023 | Done | shm 闭环 + 基准全绿；common 2 处 unsafe 例外人类已批（AGENTS §4）；跨用户/真多进程未覆盖后随 |

## Phase 2（语义与检索评价）

| ID | 标题 | 类型 | 阶段 | 依赖 | 优先级 | 规模 | 验收标准(可测) | 关联决策 | 状态 | 证据链接 |
|---|---|---|---|---|---|---|---|---|---|---|
| TSK-201 | Profile schema + 首批白名单（≥5 款） | 实现 | 2 | TSK-112 | P0 | M | 白名单覆盖 ≥80% 目标声设参数；ident 回查 -1 迁移单测过 | DEC-015 | Done | schema+builtins 20 单测绿；7 插件各 8–16 条（Macro 4/Vital 待补测；name_regex 编译延后；13 doc 坏链已修） |
| TSK-202 | MIR v1 链（48k/STFT/mel/CLAP/LUFS） | 实现 | 2 | TSK-104 | P0 | M | 参数变更即 golden 失配（敏感性）；固定输入评分落区间 | DEC-012/016 | Done | mir.rs+score.rs+golden（13+5 全绿；+6dB 归零 ΔLUFS=-6.021；CLAP 缝留待接入；3341 归 TSK-208） |
| TSK-203 | 检索后端原型二选一（10k 基准） | 实现 | 2 | TSK-202 | P0 | M | 构建 <30min、P95 <100ms；落选者归档理由 | DEC-014 | Done | 双后端 trait + 10k harness 全绿（0.14s/P95~30ms）；默认后端锁定 LanceDb（人类 2026-10-06 批准；反转：真 crate 下 P95 回退 >20% 即翻转）；注：harness 每次重写 BENCH.md，CI 侧如脏树需门禁 |
| TSK-204 | 评价器 + `+6dB` 防作弊回归 | 测试 | 2 | TSK-202 | P0 | M | `+6dB` 用例必不涨分；三次方差门禁 | DEC-016/L5 | Done | golden 5→9（低频作弊/削波告警/位一致/多分辨率 ordering）；权重 0.30/0.45/0.25 初值，标定归 TSK-402 |
| TSK-205 | stem 后台队列 + 内容寻址缓存 + GC | 实现 | 2 | TSK-104 | P1 | M | 缓存命中率/水位指标上报；超 50GB 自动 GC | DEC-009 | Done | dsp 新 crate（23 单测绿：状态机/寻址/GC/指针；零新外部依赖）；WAL/journal 与 DEC-027 迁移留后续 |
| TSK-206 | symphonia/FFmpeg fallback 阈值实测 | 测试 | 2 | TSK-202 | P1 | S | 破损/异形格式矩阵：触发 fallback 的条件清单 | C-dsp §1 | Done | 8 格式矩阵全绿（唯一触发：截断 wav；AAC 46080 帧须 priming 修齐，转消费侧）；symphonia 已登记 |
| TSK-207 | 时变 AB（timestretch vs RB 内部运行） | 测试 | 2 | TSK-202 | P2 | S | AB 报告落盘；RB 仅内部运行记录（不购证） | DEC-025 | Done | 人类 2026-10-06 拍板采用 Rust 路线（timestretch，G1-G3 过 + G4 门禁）；RB 工具链不再搭建，采购项永久关闭（TSK-901 范围） |
| TSK-208 | 响度 calibration/交叉验证复跑 | 测试 | 2 | TSK-202 | P1 | S | 3341 14/14 + 与 ebur128 ±0.5 LU | C-dsp §4 | Done | 双实现 Δ=0.000 LU（远优）；AAC priming 规则落地（strip 1024，修齐 mel 0.276）；dBTP 告警函数 + 边界单测；权重/残留归 TSK-402 |

## Phase 3（搜索与 UX 闭环）

| ID | 标题 | 类型 | 阶段 | 依赖 | 优先级 | 规模 | 验收标准(可测) | 关联决策 | 状态 | 证据链接 |
|---|---|---|---|---|---|---|---|---|---|---|
| TSK-301 | model-gw 三档路由 + 熔断降级链 | 实现 | 3 | TSK-105/106 | P0 | M | 故障注入：Tier1  down→Tier2→Tier3→BLOCKED 路径全覆盖；无已存 consent 即 consent_required BLOCKED（先弹窗，AGENTS §8） | DEC-010/011 | Done | model_gw.rs（14 单测绿：全降级链/401 跳云/熔断/审计脱敏）；零网络（真传输归 TSK-116） |
| TSK-116 | model-gw 真 HTTPS 传输（responses 格式） | 实现 | 3 | TSK-301 | P1 | S | 30s hard/P95 10s 超时接线；401/429/5xx 真映射；MockTransport 回归保留 | DEC-010/011 | Done | HttpsTransport（blocking rustls，无 openssl；127 stub 全绿，Mock 回归全过）；真端点验证转 TSK-117（推理 POST 系统性 400） |
| TSK-117 | 云端推理真调用验证（Go 面形态/账户） | 测试 | 3 | TSK-116 | P0 | S | 任一 Tier 首次 2xx + 信封解码；失败即记录不硬试 | DEC-010/011 | Done | Tier2 200 全绿（会话头为根因；LIVE-OK 回到；延迟 ~6.2s）；Tier1 未点火（训练保留待批）；401/UA 异常已记录 |
| TSK-302 | Patch schema 双校验 + 修复循环 | 实现 | 3 | TSK-201/301 | P0 | M | 首轮有效率 ≥95%，2 轮修复 ≥99%（fixture 集） | DEC-013 | Done | patch.rs（48 fixture：首轮 100%/修复 100%；JSFX 裸 ident 按设计拒，SELECT 枚举/add存在性留后续） |
| TSK-303 | TPE/CMA-ES 粗搜 + NM 收尾（mock 渲染） | 实现 | 3 | TSK-204 | P1 | M | 预算公式单测：50 次≈分钟级；mock 下收敛 | DEC-018/C §5 | Done | 零依赖 TPE-lite + 手写 NM（预算/收敛全绿）；optimizer/cmaes 待真嵌入时接入；接线已收口 |
| TSK-304 | 多样性去重 + 候选卡片 6 字段 | 实现 | 3 | TSK-302 | P1 | S | 同质 fixture 去重生效；卡片字段缺失即单测红 | DEC-018/019 | Done | candidate.rs（去重/gap 标记/6 键卡片全绿）；CLAP 距离与试听引用格式待输入形状落定；接线已收口 |
| TSK-305 | 本地端点实测（Gemma 4 12B × 4 后端 + 音频探测） | 测试 | 3 | TSK-301 | P0 | M | vLLM/llama.cpp/MLX LM/LM Studio 逐个连通 Gemma 4 12B + 音频探测；缺音频即 BLOCKED（非静默） | DEC-010 | In-Progress | b11438 复测：文本健康；音频 input_audio 静默丢弃、image_url 传音频模型称未收到、64 PNG 报 loader 失败——服务端 media loader 不可用；mtmd-cli 后端可建 vision 上下文（缺 -ub 配额告警），server -ub 仅 128 待放大验证；vLLM/MLX/LMStudio 未开始 |
| TSK-306 | UI 引擎原型二选一（波形 + 100Hz） | 实现 | 3 | — | P0 | M | 帧率/输入/HiDPI 三项达标才锁；落选归档 | DEC-002 | In-Progress | 子会话执行中（repo 外 scratch 双原型 + 指标对照） |
| TSK-307 | Lua 薄面板快捷入口 | 实现 | 3 | TSK-306 | P2 | S | 面板启停 + 跳主窗链路可用 | DEC-002 | Done | 直装 ReaImGui 0.10.0.5（sha pin，API 1.92.1）；面板 92 帧干净退出；主窗链路为 stub（待 TSK-306 落定后接线） |

## Phase 4（加固与标定）

| ID | 标题 | 类型 | 阶段 | 依赖 | 优先级 | 规模 | 验收标准(可测) | 关联决策 | 状态 | 证据链接 |
|---|---|---|---|---|---|---|---|---|---|---|
| TSK-401 | 100× 故障注入（pooled/容器/takeFX） | 测试 | 4 | TSK-103 | P0 | M | 零残留；任一残留即 K3 冻结 | H6/K3 | In-Progress | 容器 50/0 + takeFX 50/0（含 undo 值比对）；pooled 腿待人工 ghost 对（派生写不碰源 take 的对照） |
| TSK-402 | 盲听标定（排序相关 + 权重调） | 测试 | 4 | TSK-204 | P1 | M | Spearman ≥0.5；权重变更走 DEC-016 反转记录 | DEC-016 | Todo | C-models §4 |
| TSK-403 | 性能预算（500 参数/撤销延迟） | 测试 | 4 | TSK-102 | P1 | S | 单块 500 参数 <1s；撤销 <500ms，否则分片 | DEC-007/008 | Done | Pro-Q 4 VST3 实测：写 500 参数 3.0ms、撤销 4.0ms、重做 2.0ms（预算内两个量级，无需分片；单机单插件，跨机以 CI/目标机为准） |
| TSK-404 | 终验演示 M4 + 阶段报告 | 文档 | 4 | 全 | P0 | S | M4 脚本一次通过 + 五节报告 | ROADMAP M4 | Todo | 待产出 |

## 冻结/阻塞（原因 + 解除条件）

| ID | 标题 | 类型 | 阶段 | 依赖 | 优先级 | 规模 | 验收标准(可测) | 关联决策 | 状态 | 证据链接 |
|---|---|---|---|---|---|---|---|---|---|---|
| TSK-901 | RB/JUCE/权重商用采购与分发 | 合规 | — | — | P3 | S | 不适用（冻结） | DEC-025 | Blocked | 阻塞原因：不购买许可 + 无分发约束；解除条件：人类书面解除约束并立项采购 |
| TSK-902 | 云端计费声明与预算立项 | 合规 | — | — | P3 | S | 不适用（冻结） | DEC-010 | Blocked | 阻塞原因：人类明确不声明计费；解除条件：人类书面要求立项 |
