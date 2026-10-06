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

## 修订（2026-10-06 人类裁决）

- **不使用 free 模型**：free 档仅 OpenCode TUI 内可用；本项目两档 ID 无 `-free` 后缀：Tier1 `muse-spark-1.3-contributor`、Tier2 `mimo-v2.6-flash`（与 DEC-010 原值一致，DEC 无需更改）。
- 据此 `-free` 探针结论作废；400 成因收窄至 Go 面形态/账户设置（模型 ID 与鉴权均已排除）。
- 计费含义：非 free 即按量付费（价目表有 muse-spark-1.3 行；付费 mimo 无公开行，单价未知）——“不声明计费”约束与付费调用冲突，生产用量需人类另行确认；本次连通性点测（数个 tiny 调用）属已批准范围。

## BLOCKED（TSK-117，持续）

- 现象：一切推理 POST 400 空体（5 组合全灭）。
- 候选：(a) workspace 模型访问未启用/账单未建（dashboard 核对：模型开关、余额、月限额）；(b) Go 面请求形态与文档 `/zen/v1` 不同（需 opencode-go 文档/SDK 参照）；(c) 密钥 scope 不足（models 可读但推理被拒）。
- 解除条件：任一组合首次 2xx，或官方明确 Go 面形态/账户要求。
