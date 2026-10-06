# C-live-endpoint（OpenCode Go 真端点探测记录）

- 目的：记录云端推理链首次真机探测的已证实事实与 BLOCKED 项，供 TSK-117 跟踪。
- 适用范围：`https://opencode.ai/zen/go/v1`（Go 面）；Tier1/2 推理调用。
- 状态：Draft
- 最后核验日期：2026-10-06
- 依赖文档：docs/DECISIONS.md DEC-010/011；官方文档 https://opencode.ai/docs/zen/（2026-10-06 抓取）。

## 已证实（2026-10-06 实测）

1. 鉴权通过：`GET /v1/models` + Bearer → 200，36 个模型，含 `muse-spark-1.3-contributor` 与 `mimo-v2.6-flash`（均为非 `-free` ID）。
2. 推理 POST 系统性 400（空体）：`/responses` × {muse 非 free, muse-free}、`chat/completions` × {mimo 非 free, mimo-free, `opencode/` 前缀}，Tier1/2 全覆盖；401 仅出现在密钥缺失对照组（fresh shell），故非鉴权问题。
3. 官方文档模型 ID 与端点均为**按模型绑定**：`muse-spark-1.3-contributor-free → /zen/v1/responses`、`mimo-v2.6-flash-free → /zen/v1/chat/completions`；注意文档基地址为 `/zen/v1`，而本项目预设为 `/zen/go/v1`（Go 面，行为可能不同）。
4. 隐私条款（官方原文）：Meta Contributor Free 换折扣价取训练权（与 Tier1 理解一致）；**MiMo-V2.6-Flash Free 在免费期内“collected data may be used to improve the model”**——Tier2-as-ZDR 前提动摇（付费 mimo 是否零保留不明，价目表无付费 mimo 行）。
5. 计费现实：Zen 为 pay-as-you-go（$20 起充，余额 <$5 自动补 $20，可关/可设月限额）；免费档免费但账户可能仍需建账——与“不声明计费”约束存在张力，需人类确认口径。

## 补记（2026-10-06，人类澄清）

- 不使用 free 模型：free 变体仅 OpenCode 产品内可用；本项目只走 Go 面 `https://opencode.ai/zen/go/v1`（不用 `/zen/v1` 端点），两档 ID 为 `muse-spark-1.3-contributor` / `mimo-v2.6-flash`（均无 free 后缀）——与 DEC-010 原值一致，无需更改。
- 因此此前 400 与模型 ID 无关（付费 ID 已覆盖测试）；剩余候选：(a) 账户余额/月限额/模型访问开关（dashboard 核对）；(b) Go 面请求形态。
- ZDR 推论：官方例外清单仅列 free 变体，付费 `mimo-v2.6-flash` 适用“默认零保留”总则——Tier2-as-ZDR 成立（以官方总则为准）。
- 计费：付费档按量计费（muse-spark-1.3 有价目；mimo 付费价目表未见），用量经审计字节数 proxy + 月限额控制（人类侧建账）。

## 解决（2026-10-06，真机验证通过）

- 根因：缺 `x-opencode-session` 会话头（官方 Go 文档“Where can I use it”节要求；无头即系统性 400）。附带发现：自定义 UA（`synthlm/0.0.0`）两次与 401 相关——本客户端保持默认 UA，只发会话头。
- 验证：Tier2 `chat/completions` 200，标准信封 + `reasoning_content` 扩展；`finish=stop` 且 `LIVE-OK` 回到；延迟 ~6.2s；`live_tier2.rs`（`#[ignore]`）3 项全过。
- 代码已落实：按 Tier 分路径（Tier1 `/responses` / Tier2 `/chat/completions`）、`with_session_id` 构造器、stub 会话头捕获；Tier1 路径未点火（训练保留，需另行批准）。

## BLOCKED（TSK-117）→ 已关闭，见上节

- 现象：一切推理 POST 400 空体（5 组合全灭）。
- 候选：(a) workspace 模型访问未启用/账单未建（dashboard 核对：模型开关、余额、月限额）；(b) Go 面请求形态与文档 `/zen/v1` 不同（需 opencode-go 文档/SDK 参照）；(c) 密钥 scope 不足（models 可读但推理被拒）。
- 解除条件：任一组合首次 2xx，或官方明确 Go 面形态/账户要求。
