# C 模型栈与检索/评价/搜索核实

- 目的：核实音频理解模型、文本规划模型、检索层、评价器、局部搜索五块的许可可用性、Rust 推理路径与成本量级，为 SynthLM 本地优先架构选型提供依据。含多处对原假设的勘误（CLAP 非 MIT、MERT 权重非商用）。
- 适用范围：CLAP / MERT / Qwen2-Audio / MuLan（含 MuQ-MuLan、LINE MuLan）；llama-cpp-rs 约束解码；Qdrant / USearch / LanceDB；rustfft / realfft / ruststft / ebur128 系评价器；贝叶斯优化 / CMA-ES / SPSA。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：无（Phase 0 并行调研；后续 ARCHITECTURE.md、DEC 许可决策依赖本文）。

> 约定：每条结论后标注来源 + 置信度。`⚠️需实测` 表示文档有声明但行为/性能细节官方未说明，不得直接作为工程假设。许可结论引用 LICENSE 原文或 SPDX，禁止凭印象。

## 0 顶层结论（先读）

1. **CLAP 代码不是 MIT，是 CC0-1.0**（仓库 LICENSE 全文为 CC0，GitHub license 字段 `CC0-1.0`）；pyproject classifier 自称 Apache 系自相矛盾，以 LICENSE 文件为准，商用友好但需法务确认。权重：HF `laion/clap-htsat-fused` 标 `apache-2.0`，原始 `.pt` 无独立权重许可文件。来源：https://github.com/LAION-AI/CLAP/blob/main/LICENSE、https://huggingface.co/laion/clap-htsat-fused ，2026-10-05，高（文件本身）/中（权重商用）。
2. **MERT 代码 Apache-2.0，但权重 CC-BY-NC-4.0，非商用**，产品线不可用，只能做科研基线。原“MIT?”假设错误。来源：https://raw.githubusercontent.com/yizhilll/MERT/main/LICENSE（Apache 全文）、https://huggingface.co/m-a-p/MERT-v1-95M（frontmatter `license: cc-by-nc-4.0`），2026-10-05，高。
3. **Qwen2-Audio 权重 Apache-2.0，商用无需额外申请**（与 Qwen1 代的 Tongyi-Qianwen 限制不同，已切换）。8.2B 总参数，GGUF Q4_K_M 约 4.8GB + mmproj，8–16GB 档可用，纯 CPU 可跑但音频编码器侧需实测。来源：https://huggingface.co/Qwen/Qwen2-Audio-7B（`license: apache-2.0`）、https://github.com/QwenLM/Qwen2-Audio（License Agreement 节）、https://arxiv.org/html/2407.10759v1 ，2026-10-05，高（许可）/中（端侧性能）。
4. **Google MuLan 原版未开源权重，不可用**；开源替代只有 NC 的 MuQ-MuLan（码 MIT / 权重 CC-BY-NC-4.0）与小众 Apache 的 LINE 日语 MuLan。产品级音乐-文本联合嵌入当前只有 CLAP 可选。来源：https://research.google/pubs/mulan-a-joint-embedding-of-music-audio-and-natural-language/、https://huggingface.co/OpenMuQ/MuQ-MuLan-large、https://huggingface.co/line-corporation/japanese-mulan-base ，2026-10-05，高。
5. 文本规划：llama.cpp 原生 grammar + `json_schema_to_grammar` 成熟（server `json_schema` 字段 / `--json`），但 Rust 绑定分裂：utilityai 版缺 binding 且有 crash 报告，cron-icle/eugenehp 版跟踪上游紧但无 semver。`gbnf` crate（MIT）只是子集。推荐走 **llama-server HTTP sidecar** 而非进程内 FFI。来源见 §2，2026-10-05，中高。
6. 检索三件套全是 Apache-2.0（Qdrant / USearch / LanceDB）；本地单用户 DAW 优先嵌入式（USearch / LanceDB），不要为预设库起 server。跨空间（ST 文本向量 vs CLAP 音频向量）不能单索引，必须多路 + RRF/加权融合。来源见 §3，2026-10-05，高（许可）/高（架构判断）。
7. 评价器 Rust 链完整：`ruststft`（STFT/mel/MFCC）+ `ebur128`/`ebur128-stream`（BS.1770/ R128，MIT/纯 Rust）+ CLAP cosine；最大陷阱是**不做 LUFS 归一就比响度作弊**，以及频段权重与窗/跳参数不一致。来源见 §4，2026-10-05，中高。
8. 局部搜索预算公式 `T = N × (t_render + t_metric) / 并行度`；秒级渲染下 50 次≈分钟级、200 次≈一刻钟、500 次≈三刻钟以上。连续小维度用 CMA-ES/TPE + Nelder-Mead 收尾，高维连续用手写 SPSA，离散预设短名单用 TPE/bandit。argmin 的 CMA-ES PR 未合入，别指望。来源见 §5，2026-10-05，中（量级估算，⚠️需实测渲染耗时后代入）。

## 1 音频理解模型：许可 × 成本 × Rust 路径

### 1.1 LAION-CLAP（勘误：代码 CC0-1.0，不是 MIT）

- 代码许可：**CC0-1.0**。仓库 `LICENSE` 全文为 Creative Commons CC0（"Waiver … for any purpose whatsoever, including without limitation commercial"），GitHub 字段 `Creative Commons Zero v1.0 Universal`。注意同一仓库 `pyproject.toml` classifier 却写 `License :: OSI Approved :: Apache Software License`，自相矛盾，以 LICENSE 文件为准。来源：https://raw.githubusercontent.com/LAION-AI/CLAP/main/LICENSE、https://github.com/LAION-AI/CLAP ，2026-10-05，高。
- 权重许可：HF `laion/clap-htsat-fused` frontmatter `license: apache-2.0`；原始 `.pt`（`lukewys/laion_clap` 的 `630k-audioset-fusion-best.pt` 约 1.86GB，经 `hook.py` 下载）无独立权重许可文件，社区 mirror 标注上游 CC0-1.0。结论：商用友好度高，但权重链条（CC0 代码 vs Apache-2.0 HF port vs 训练数据版权保留声明"Due to copyright reasons, we cannot release the dataset"）需法务做一次确认，⚠️勿直接写死"可商用"。来源：https://huggingface.co/laion/clap-htsat-fused、https://huggingface.co/lukewys/laion_clap/blob/main/630k-audioset-fusion-best.pt、https://github.com/LAION-AI/CLAP/blob/main/README.md ，2026-10-05，中。
- 成本：约 **0.2B 参数**（HF 页面 `0.2B params`），`projection_dim = 512`（transformers `ClapConfig` 默认，`enable_fusion` 可变长）。fp32 < 1GB，int8 约 200MB，CPU 可实时出嵌入。来源：https://huggingface.co/laion/clap-htsat-fused、https://huggingface.co/docs/transformers/en/model_doc/clap ，2026-10-05，高（参数规模）/中（端侧吞吐⚠️需实测）。
- Rust 路径：**首选 `ort`（ONNX Runtime Rust 绑定）跑导出的 ONNX**，`ort` 文档明确支持 ONNX 全量算子与 CUDA/CPU/WASM 后端，`ort-candle`/`ort-tract` 纯 Rust 后端算子覆盖有限（transformer 类尚可）。`candle` 无原生 CLAP 实现，自写 HTSAT+文本塔成本高。HTSAT 的 Swin 式 patch/fusion 算子 ONNX 导出原则可行（标准 transformer+conv），但 fusion 变长路径要实测验证数值一致性。来源：https://ort.pyke.io/、https://ort.pyke.io/backends、https://ort.pyke.io/backends/candle、https://github.com/pykeio/ort ，2026-10-05，中，⚠️需实测导出。
- 8–16GB / 纯 CPU：无压力。CLAP 只做离线/按需嵌入，不占常驻显存；纯 CPU + ONNX int8 即满足预设库向量化。来源：同上 + §1.1 规模推算，2026-10-05，高（定性）。

### 1.2 MERT（勘误：权重 NC，产品不可用）

- 代码许可：**Apache-2.0**。GitHub 字段 `Apache License 2.0`，`LICENSE` 原文为 Apache-2.0 标准文本（§2 版权许可、§3 专利许可）。来源：https://github.com/yizhilll/MERT、https://raw.githubusercontent.com/yizhilll/MERT/main/LICENSE ，2026-10-05，高。
- 权重许可：**CC-BY-NC-4.0，非商用**。`m-a-p/MERT-v1-95M` README frontmatter `license: cc-by-nc-4.0`（v1-95M/v1-330M 同）。结论：SynthLM 产品线**不可用**，仅可作论文复现/评估基线对比；任何"蒸馏 MERT 特征进产品模型"都沾 NC，禁止。来源：https://huggingface.co/m-a-p/MERT-v1-95M ，2026-10-05，高。
- 成本（仅评估用）：95M/330M 两档，5s 预训练上下文，24kHz，75Hz 特征率（README 表格）。95M 推理约 bundled < 1GB。来源：同上 HF README，2026-10-05，高。
- Rust 路径：HuBERT 式 conv+transformer，原型 ONNX 可导，但 NC 使产品化路径无意义，不建议投入。来源：架构判断 + https://arxiv.org/html/2306.00107 ，2026-10-05，中。

### 1.3 Qwen2-Audio（Apache-2.0，无额外商用审批）

- 权重许可：**Apache-2.0**。`Qwen/Qwen2-Audio-7B` 与 `-Instruct` 页面 `License: apache-2.0`，README frontmatter `license: apache-2.0`；官方仓库 License Agreement 节原文"Check the license of each model inside its HF repo. It is NOT necessary for you to submit a request for commercial usage." 注意与 Qwen1 代 outpatient 的 Tongyi-Qianwen 许可区分。transformers 侧 `qwen2_audio` config 文件头 Apache-2.0。来源：https://huggingface.co/Qwen/Qwen2-Audio-7B、https://github.com/QwenLM/Qwen2-Audio ，2026-10-05，高。
- 成本：论文报告**总量 8.2B**（Qwen-7B + Whisper-large-v3 初始化的音频编码器 + stride-2 pooling，每帧≈40ms）。safetensors 约 15–16.8GB；社区 GGUF：Q4_K_M **4.79GB**、Q5_K_M 5.56GB、Q6_K 6.37GB、Q8_0 8.25GB、f16 15.5GB，另 mmproj 0.8（Q8）/1.4GB（f16）。Qwen 官方 7B GGUF PPL 参考：fp16 7.93 → q4_k_m 8.02，4-bit 退化小。来源：https://arxiv.org/html/2407.10759v1、https://huggingface.co/second-state/Qwen2-Audio-7B-Instruct-GGUF、https://huggingface.co/mradermacher/Qwen2-Audio-7B-GGUF、https://huggingface.co/Qwen/Qwen2-7B-Instruct-GGUF ，2026-10-05，高（尺寸）/中（PPL 迁移到 Audio 版⚠️需实测）。
- 8–16GB 显存：8GB 用 Q4_K_M + 部分 offload（NexaAI 标注默认 q4_k_m 需 4.2GB RAM）；16GB 可 Q5_K_M/Q6_K 全量。纯 CPU：文本侧可跑（llama.cpp 量化成熟），音频编码器（mel+Whisper 塔）在 CPU 上偏重，长音频（>30s 官方称最佳 <30s）延迟需实测。来源：https://huggingface.co/NexaAI/Qwen2-Audio-7B-GGUF、https://github.com/QwenLM/Qwen2-Audio ，2026-10-05，中，⚠️需实测。
- Rust 路径：llama.cpp `conversion/qwen.py` 已注册 `Qwen2AudioForConditionalGeneration → MODEL_ARCH.QWEN2`，文本 LLM 侧 GGUF 转换有人走通（mradermacher/second-state 均为 llama.cpp b5501 量化）；但**音频 millmproj/mtmd 路径在 llama.cpp 的支持度需实测**（`common`/`mtmd` 特性在 cron-icle fork 默认开，不等于 Qwen2-Audio 音频塔被官方 CI 覆盖）。`candle` 自实现 8B 多模态不现实。推荐：llama-server sidecar + HTTP，不做 Rust 原生。来源：https://github.com/ggml-org/llama.cpp/blob/d59d455f/conversion/qwen.py、https://github.com/cron-icle/llamacpp-rs ，2026-10-05，中，⚠️需实测音频输入端到端。

### 1.4 MuLan（原版不可用；替代品多为 NC）

- Google MuLan：**无官方权重发布，不可用**。论文训练数据为 4400 万互联网音乐视频（37 万小时），属专有数据；官网仅论文页，无 HF/代码权重。来源：https://research.google/pubs/mulan-a-joint-embedding-of-music-audio-and-natural-language/、https://arxiv.org/pdf/2208.12415.pdf ，2026-10-05，高。
- MuQ-MuLan（社区最接近的开放复刻）：**代码 MIT，权重 CC-BY-NC-4.0**。HF README 原文"The code is released under the MIT license. The model weights … are released under the CC-BY-NC 4.0 license." 约 700M 参数，中英双语。结论同 MERT：仅评估，不进产品。来源：https://huggingface.co/OpenMuQ/MuQ-MuLan-large、https://github.com/lzqlzzq/flashMuQ ，2026-10-05，高。
- LINE `japanese-mulan-base`：**Apache-2.0**（HF 页面 License 字段），但训练仅约 2 万内部音乐-文本对，日语中心，AST+GLuCoSE 小模型，泛化到中文/英文预设检索存疑。来源：https://huggingface.co/line-corporation/japanese-mulan-base ，2026-10-05，中。
- 推理成本：MuQ-MuLan ~700M 需 CUDA 才顺滑，Rust/ONNX 路径社区无现成，⚠️需实测。产品决策：音乐-文本联合嵌入**现在只剩 CLAP 一条产品安全路线**，或自采数据训小对比模型（远期）。

### 1.5 音频栈 Rust 推理小结

| 模型 | 产品可用性 | 常驻成本 | Rust 路径 |
|---|---|---|---|
| CLAP | 可（法务确认后） | 可忽略（按需嵌入） | `ort` + ONNX，int8 CPU |
| MERT | 否（NC） | —（仅评估） | 不投入 |
| Qwen2-Audio | 可（Apache-2.0） | 5–9GB（量化） | llama-server sidecar |
| MuLan 原版 | 否（未发布） | — | — |
| MuQ-MuLan | 否（NC，仅评估） | ~700M CUDA | 不投入 / 评估用 Python |

## 2 文本规划模型：GGUF 约束 JSON Patch 可行性

### 2.1 本地 7B–14B GGUF 能否稳定产出 schema 约束 JSON Patch

- 能，但"稳定"来自三件套而非单靠模型：**GBNF 约束解码（语法正确）+ JSON Schema 校验（语义正确）+ 失败修复循环（重试/repair）**，再加 Rust 侧 `serde_json` + `json-patch` crate 做 RFC 6902 应用前校验。llama.cpp server 原生支持 `json_schema`/`response_format` 与 `--json/-j`，约束只管输出不管提示词（tool calling 除外），所以 prompt 里仍要复述 schema。来源：llama.cpp `tools/server/server-schema.cpp`（`json_schema`→`json_schema_to_grammar` 转换）、grammars 文档、https://github.com/ggerganov/llama.cpp/pull/5978 ，2026-10-05，高（机制）/中（Patch 语义稳定率⚠️需实测）。
- 覆盖度边界（C++ 版已知限制）：`properties` 与 `anyOf/oneOf` 混用受限（issue #7703）、`prefixItems` 坏、`minimum/maximum` 仅 integer、嵌套 `$ref` 问题（#8073）、远程 `$ref` C++ 版不支持、`additionalProperties` 默认 false（与 JSON Schema 规范默认 true 相反，为速度与防幻觉）。JSON Patch schema（op 枚举 `add/remove/replace/...` + path 正则 `^/…`）恰好落在支持良好的子集，适合用；复杂嵌套 `oneOf` 的 effect 参数要拆平。来源：grammars README（ via https://github.com/ggml-org/llama.cpp 相关文档与 PR #5978 讨论），2026-10-05，高。
- 7B vs 14B：Q4_K_M 下 7B PPL 退化约 +0.1（7.93→8.02），语法约束下 JSON 有效性两者都高，差异在**语义 adherence**（op 选对、path 存在、value 类型/范围对）。规划低频调用（一次编曲意图 → 几十个 patch），优先 14B Q4（~8GB）若显存允许，否则 7B Q5/Q6。纯 CPU 可接受（规划非实时）。来源：https://huggingface.co/Qwen/Qwen2-7B-Instruct-GGUF ，2026-10-05，中，⚠️需在 SynthLM patch schema 上实测有效率/修复轮数。
- 新后端 `llguidance`：Rust 编写的约束解码库，JSON Schema 覆盖优于旧 GBNF，"very fast … excellent JSON Schema coverage but requires the Rust compiler, which complicates the llama.cpp build"。对 SynthLM 是利好（同为 Rust 生态），但构建复杂度上升；且它是 llama.cpp 内部后端，不是给 SynthLM 直接调用的独立约束 API（除非走 server）。来源：https://github.com/ggml-org/llama.cpp/blob/master/docs/llguidance.md ，2026-10-05，高。

### 2.2 约束解码 Rust 生态成熟度

- `utilityai/llama-cpp-rs`（`llama-cpp-2`）：issue #864 明确缺 `json_schema_to_grammar` 绑定、无 tool-calling 模板返回 grammar、手动 GBNF 采样 crash 报告（`GGML_ASSERT(cur_p.selected >= 0)` / 空 grammar 栈；评论区有人指出 sampler 链顺序放错也会触发，属易错 API）。`gbnf` crate（**MIT**，https://crates.io/crates/gbnf、https://github.com/richardanaya/gbnf）可补 JSON→GBNF，但只覆盖子集（boolean/number/string、enum、oneOf、array；无 min/max、property 下划线 bug），不够产品级。来源：https://github.com/utilityai/llama-cpp-rs/issues/864、https://crates.io/crates/gbnf ，2026-10-05，高。
- `cron-icle/llamacpp-rs` / `eugenehp/llama-cpp-rs` 新分支：自称紧跟上游、默认开 `common`（含 JSON-schema-to-grammar）与 `mtmd`，`json_schema_to_grammar`/chat-template/state 存取皆有安全封装与单测，但声明"tracks llama.cpp closely and does not follow semver — pin an exact version"。即：功能全但 API 不稳定。来源：https://github.com/cron-icle/llamacpp-rs、https://github.com/eugenehp/llama-cpp-rs ，2026-10-05，中。
- `guidance`（微软）：无官方 Rust 绑定，不选。llguidance 可通过 llama.cpp 间接用。
- 决策：**约束解码走 llama-server HTTP（`json_schema` 字段），Rust 侧只做校验与重试**；进程内 FFI 仅在延迟证明必要时考虑，且 pin commit + `llama-gbnf-validator` 回归。来源：综合 §2，2026-10-05，中（架构建议）。

## 3 检索层：向量化 × 混合检索 × 许可与维度

### 3.1 预设向量化三路

- 文本路 `sentence-transformers`：代码 Apache-2.0，但** checkpoint 逐个看 license**（多数 Apache/MIT，部分 NC/研究限制）；ONNX 导出成熟（TEI 即用 `ort` 跑 ONNX embedding）。维度由 checkpoint 定（常见 384/768/1024；选型时 pin 如 MiniLM-384 并全文记录）。来源：公知生态 + https://ort.pyke.io/（TEI 用 ort），2026-10-05，中，⚠️checkpoint 许可逐个核实。
- 音频路 CLAP：联合空间 512 维（§1.1），文本 query 可直接检索音频（零样本分类/检索即论文三大任务）。MERT 系只有音频塔、无文本塔，不能做文本→音频，只能音频→音频相似。来源：https://huggingface.co/docs/transformers/en/model_doc/clap ，2026-10-05，高。
- 结构化 MIR（librosa/essentia 类特征）：速度/调性/响度/频谱质心/过零率/chroma 统计，几十维稠密或标量列。**许可警告**：Essentia 据公开知识为 AGPL 系（本次未拉取 LICENSE 原文，⚠️需复核 SPDX 后再定，静态链入闭源插件高风险）；librosa 为 ISC（同需复核原文）。Rust 侧不建议绑 C++ Essentia，建议 `ruststft` + 自研 chroma/onset/pitch（`resonant-analysis` 有 onset/pitch/tempo/MFCC/chroma 现成）重做特征链， Degree：来源：https://crates.io/crates/ruststft、https://github.com/sunsided/stft、https://crates.io/crates/resonant ，2026-10-05，中。
- 关键架构点：三路**不在同一空间**，不要拼成一个向量单索引。文本向量、音频向量各一索引（维度各自一致即可），结构化走 payload/列式过滤 + 标量排序；查询时多路召回再融合。来源：架构推导，2026-10-05，高（判断）。

### 3.2 混合检索方案与许可

- **Qdrant**：**Apache-2.0**（`LICENSE` 全文 Apache，GitHub 字段 Apache-2.0）。Rust 写，HNSW + payload 过滤 + 多向量/混合检索成熟；是 server 形态（运维负担）。来源：https://github.com/qdrant/qdrant、https://github.com/qdrant/qdrant/blob/7b196be23191ca4367d7f8ea6adf2f15075e22fd/LICENSE ，2026-10-05，高。
- **USearch**：**Apache-2.0**（`LICENSE` 全文 Apache，字段 Apache-2.0）。单文件嵌入式 HNSW 库，C++ 主体 + Rust 等十余语言绑定， footprint 最小，适合 DAW 进程内；单索引单空间，多路需应用层开多索引 + 自融合（RRF/加权）。来源：https://github.com/unum-cloud/usearch、https://github.com/unum-cloud/USearch/blob/main/LICENSE ，2026-10-05，高。
- **LanceDB**：**Apache-2.0**（crates.io `lancedb` License 字段 + GitHub Apache-2.0）。嵌入式 serverless（Lance 列式），Rust SDK `lancedb` crate；无 server 运维，版本化数据集友好（预设库迭代）。多向量支持随版本变，选型时锁定版本实测。来源：https://crates.io/crates/lancedb、https://github.com/lancedb/lancedb ，2026-10-05，高（许可）/中（多向量版本⚠️需实测）。
- 维度约束：同一索引内维度必须一致；CLAP 512 与 ST 384/768 **不可混插**。融合用 RRF 或归一化加权（文本分 + 音频分 + 结构化先验），权重做成可调参数进局部搜索（§5）。来源：向量库通用约束 + 架构推导，2026-10-05，高。
- 决策：单机 DAW 场景 **USearch（进程内极简）或 LanceDB（嵌入式+版本化）二选一**，Qdrant 留给云端/团队共享预设库。来源：综合，2026-10-05，中（建议）。

## 4 评价器：Rust 实现路径与已知陷阱

### 4.1 实现路径（全 Rust，无 Python 依赖）

- 解码：`symphonia` 系（解 MP3/FLAC/WAV；许可⚠️需复核，勿假设）或 `houn`d（WAV）+ 统一重采样到 48k/44.1k（`rubato`/`samplerate` 择一，⚠️需实测质量）。来源：生态公知，2026-10-05，低–中，⚠️需实测。
- STFT/谱：`rustfft` + `realfft`（实数 FFT，约 2× 快、省一半内存；API 与 rustfft 对齐）上层用 **`ruststft`**：流/批 STFT、Hann 等全窗库、幅度/功率/dB helper、`mel` 特性（librosa 兼容 filterbank、DCT-II MFCC），`#![forbid(unsafe_code)]`，无 std 可配。来源：https://docs.rs/realfft/latest/realfft/、https://crates.io/crates/ruststft、https://github.com/sunsided/stft ，2026-10-05，高。备选门面 `resonant`（FFT/filter/analysis 一行式，MIT/Apache 双许可，`resonant-analysis` 含 onset/pitch/tempo/MFCC/chroma）：https://crates.io/crates/resonant ，2026-10-05，中。
- 谱距离：多分辨率 STFT（n_fft 512/1024/2048）log 幅 L1 + 线性幅 L2 混合；梅尔距离：mel（80–128 带）log-mel L1/L2。窗/跳/采样率三渲染必须一致，否则不可比。来源：音频评估公知实践 + ruststft mel 能力，2026-10-05，中。
- CLAP 相似度：候选渲染与参考/文本 prompt 的余弦（`ort` 跑 CLAP ONNX，§1.1）。语义分，不敏感微小 EQ/响度，必须与谱距离配对使用。来源：CLAP 论文任务定义（retrieval/zero-shot），2026-10-05，中高。
- 瞬态对齐：onset 包络互相关峰值偏移（ms）+ 容差窗内 F1；`resonant-analysis` onset + 自写峰值 picking。来源：https://crates.io/crates/resonant ，2026-10-05，中，⚠️需实测与听感相关性。
- LUFS 归一（ITU-R BS.1770）：**`ebur128` crate（MIT，libebur128 Rust 移植，通过 EBU TECH 3341/3342 全测试，M/S/I + LRA + true peak，全采样率重算滤波系数）**；或纯 Rust 流式 **`ebur128-stream`**（BS.1770-4：K 计权、M 400ms / S 3s / I 全节目门控、绝对门 -70 LUFS + 相对门 -10 LU、4× 过采样 12-tap true peak；注意非 48k 下 true peak 表为近似，应重采样到 48k）。流程：先测 integrated LUFS → 增益到目标（如 -14 LUFS）→ 再算谱/CLAP 分。来源：https://crates.io/crates/ebur128、https://github.com/sdroege/ebur128、https://docs.rs/ebur128/latest/ebur128/、https://docs.rs/ebur128-stream/latest/ebur128_stream/、https://docs.rs/math-dsp/latest/math_audio_dsp/ebur128/index.html ，2026-10-05，高。

### 4.2 已知陷阱（必读）

- **L5 响度作弊**：任何与能量正相关的分（谱 L2、CLAP 余弦在归一化不足时）都会被"调大声"刷高。强制：所有比较先 LUFS 归一，并上报 ΔLUFS 与 true-peak（dBTP，> -1 则扣分/限幅告警）。来源：母带/评估公知 + ebur128 门控与 true-peak 定义，2026-10-05，高（判断）。
- **频段权重**：线性谱 L2 被低频高能量主导；用 log-mel + 分频带加权（低/中/高分别归一）或 K 计权思想；mel 已感知化但低频泄漏仍需窗函数纪律（Hann， 50–75% overlap）。来源：信号处理公知，2026-10-05，中。
- **参数一致性**：采样率/STFT 窗跳/mel 带数任一不一致 → 分数不可比。评价器入口统一：decode → 单声道/立体声策略固定 → 重采样 48k → 固定 STFT 参数 → LUFS 归一 → 多指标。来源：工程纪律，2026-10-05，高。
- **时间对齐容差**：瞬态/节拍比较给 ±20–50ms 容差窗，直接逐采样 L1 会因梳状相位惩罚错杀。来源：公知，2026-10-05，中。
- **CLAP 盲区**：语义相似≠保真（换音色同旋律分高）。CLAP 只做"像不像这个描述"，保真靠谱距离+瞬态。来源：CLAP 任务定义，2026-10-05，中高。

### 4.3 盲听标定记录（2026-10-06，TSK-402 通过）

- 设计：属性锚定排序（B 亮度 5 级截止、B800 最暗；T 瞬态 3 级；C 作弊对），10 clip 盲化，评分者给亮度/锐度 1–5 分；预测=质心/峰值 flux 序。
- 结果：B 人-真值 ρ=0.825（唯一偏离：B20000 被评 3，顶端饱和）；T ρ=1.0；全 10 点人-质心 ρ=0.7455（clip06 +6dB 被评最亮=5，响度串扰经典案例）；预测与真值两系完全一致。
- 结论：三项 ≥0.5，权重 0.30/0.45/0.25 维持，不触发 DEC-016 反转；C 对人类判 03>06 系响度混杂，评价器归一化免疫正确。

## 5 局部搜索：不可微 × 秒级渲染下的预算与收益

### 5.1 预算公式与量级

- 公式：`T_total = N_evals × (t_render + t_metric) / P`（P 并行度；DAW 内通常 P=1，离线批处理可 P=4–8）。t_metric（谱+mel+CLAP+LUFS）相对 t_render 可忽略（ms–百 ms vs 秒），除非 CLAP 走 CPU 大批量。
- 量级表（t_render 取 3s/5s/10s 三档，P=1）：
  - N=50：2.5min / 4min / 8min —— 预设微调/单参数扫，可交互等待。
  - N=150：7.5min / 12.5min / 25min —— CMA-ES 中等维度一次完整 run。
  - N=300：15min / 25min / 50min —— BO/大种群，上限。
  - N=1000：50min / 1.4h / 2.8h —— 过夜/后台 only。
- 结论：秒级渲染下**单次搜索预算锁 50–200 次评估**；超过走多保真（短预览 render 粗筛 + 全 render 验证）与早停。来源：算术 + 渲染耗时假设，2026-10-05，中，⚠️t_render 必须用 SynthLM 真实链实测代入。

### 5.2 方法选型（Rust 可用性）

- **Nelder-Mead**（`argmin` 自带；单纯形，<10 维连续，约 50–150 次收敛）：做收尾抛光，不做全局。来源：https://docs.rs/argmin/latest/argmin/、https://github.com/argmin-rs/argmin ，2026-10-05，高（存在性）/中（收敛次数，问题相关）。
- **CMA-ES**：连续、中维、非凸/噪声鲁棒。种群 `λ ≈ 4 + 3·ln(D)`（D=8 → λ≈10；20 代 ≈ 200 评估 ≈ 10–30min @3–5s）。Rust 状况：**`argmin` 的 CMA-ES PR #225 未合入**（eigen 后端 Vec/nalgebra 不稳定、高迭代 panic 报告），别用 argmin 的；改用专用 **`cmaes` crate**（`CMAESOptions` + restart 策略）或 **`optimizer` crate 的 `CmaEsSampler`**（需 `cma-es` 特性 + nalgebra）。来源：https://github.com/argmin-rs/argmin/pull/225、https://docs.rs/cmaes/latest/cmaes/index.html、https://docs.rs/optimizer/latest/optimizer/、https://github.com/raimannma/rust-optimizer ，2026-10-05，高（PR 未合入事实）/中（预算）。
- **贝叶斯优化（GP-EI）**：`optimizer` crate `GpSampler`（`gp` 特性）。最省评估（<10 维约 30–80 次），但代理 O(N³)、核函数对离散/类别参数差；音频参数多混合类型时不如 TPE。来源：https://docs.rs/optimizer/latest/optimizer/ ，2026-10-05，中。
- **TPE/bandit**：`optimizer` 的 `TpeSampler`（默认特性，Optuna 式 API，12 sampler/8 pruner），混合离散连续全局粗搜首选；`Median/Hyperband` pruner 做早停。来源：同上，2026-10-05，中高。
- **SPSA**：每迭代恒 2 次评估（与 D 无关），约 100–300 迭代（200–600 评估）适合高维连续噪声目标；**Rust 无标准 crate，需手写 ~30 行**（扰动 ±c·Bernoulli、增益序列 a/(k+A)^α）。来源：SPSA 文献公知 + Rust 生态检索空白，2026-10-05，中，⚠️需实测。
- 推荐两段式：**TPE/CMA-ES 粗搜（50–150）→ Nelder-Mead/SPSA 精修**；离散预设短名单只用 TPE/bandit 不做连续优化；融合权重（§3.2）与评价器频段权重（§4.2）进同一搜索空间联合调。来源：综合建议，2026-10-05，中。

## 6 ⚠️需实测清单（转 TSK）

1. CLAP ONNX 导出数值一致性（fusion 变长路径）+ `ort` CPU int8 吞吐（384/512 batch）。
2. Qwen2-Audio 音频端到端经 llama.cpp（mmproj/mtmd）可用性；Q4/Q5 在 8GB/16GB 下首 token 与吞吐；>30s 音频退化。
3. llama-server `json_schema` 对 SynthLM patch schema 的有效率、平均修复轮数；7B vs 14B 对照。
4. `lancedb` 锁定版本的 multi-vector 与过滤能力；USearch 多索引融合延迟（10k/100k 预设规模）。
5. ST checkpoint 许可逐项确认（文本嵌入模型）；Essentia/symphonia/rustfft 许可 SPDX 复核。
6. 真实链 t_render 分布（短 preview vs 全 render）→ 代入 §5.1 定预算与早停阈值。
7. 评价器与听感相关性小标定（谱权重/容差窗/CLAP-谱配比），防 L5 响度作弊回归测试（固定 +6dB 必须不涨分）。
8. SPSA 手写实现收敛性（Rosenbrock + 真实 8 维音色空间对照）；`cmaes` vs `optimizer` 二选一基准。

## 7 来源索引（核验日期统一 2026-10-05）

- CLAP 代码许可：https://github.com/LAION-AI/CLAP/blob/main/LICENSE（CC0 全文）；https://github.com/LAION-AI/CLAP（字段 CC0-1.0）；pyproject 矛盾 classifier 见仓库 `pyproject.toml`。高。
- CLAP 权重：https://huggingface.co/laion/clap-htsat-fused（apache-2.0）；https://huggingface.co/lukewys/laion_clap/blob/main/630k-audioset-fusion-best.pt；https://huggingface.co/docs/transformers/en/model_doc/clap。中高。
- MERT：https://github.com/yizhilll/MERT；https://raw.githubusercontent.com/yizhilll/MERT/main/LICENSE；https://huggingface.co/m-a-p/MERT-v1-95M。高。
- Qwen2-Audio：https://huggingface.co/Qwen/Qwen2-Audio-7B；https://github.com/QwenLM/Qwen2-Audio；https://arxiv.org/html/2407.10759v1；GGUF 尺寸 https://huggingface.co/second-state/Qwen2-Audio-7B-Instruct-GGUF、https://huggingface.co/mradermacher/Qwen2-Audio-7B-GGUF、https://huggingface.co/NexaAI/Qwen2-Audio-7B-GGUF；PPL https://huggingface.co/Qwen/Qwen2-7B-Instruct-GGUF；转换注册 https://github.com/ggml-org/llama.cpp/blob/d59d455f/conversion/qwen.py。高（许可/尺寸）/中（性能）。
- MuLan 系：https://research.google/pubs/mulan-a-joint-embedding-of-music-audio-and-natural-language/；https://arxiv.org/pdf/2208.12415.pdf；https://huggingface.co/OpenMuQ/MuQ-MuLan-large；https://github.com/lzqlzzq/flashMuQ；https://huggingface.co/line-corporation/japanese-mulan-base。高。
- Rust 推理：https://ort.pyke.io/；https://ort.pyke.io/backends；https://ort.pyke.io/backends/candle；https://github.com/pykeio/ort。中高。
- 约束解码：https://github.com/ggml-org/llama.cpp/blob/master/docs/llguidance.md；https://github.com/ggerganov/llama.cpp/pull/5978；https://github.com/utilityai/llama-cpp-rs/issues/864；https://github.com/cron-icle/llamacpp-rs；https://github.com/eugenehp/llama-cpp-rs；https://crates.io/crates/gbnf；https://github.com/richardanaya/gbnf。高（机制/issue 事实）/中（选型建议）。
- 检索：https://github.com/qdrant/qdrant；https://github.com/qdrant/qdrant/blob/7b196be23191ca4367d7f8ea6adf2f15075e22fd/LICENSE；https://github.com/unum-cloud/usearch；https://github.com/unum-cloud/USearch/blob/main/LICENSE；https://crates.io/crates/lancedb；https://github.com/lancedb/lancedb。高。
- 评价器：https://docs.rs/realfft/latest/realfft/；https://crates.io/crates/ruststft；https://github.com/sunsided/stft；https://crates.io/crates/resonant；https://crates.io/crates/ebur128；https://github.com/sdroege/ebur128；https://docs.rs/ebur128/latest/ebur128/；https://docs.rs/ebur128-stream/latest/ebur128_stream/；https://docs.rs/math-dsp/latest/math_audio_dsp/ebur128/index.html。高。
- 搜索：https://docs.rs/argmin/latest/argmin/；https://github.com/argmin-rs/argmin；https://github.com/argmin-rs/argmin/pull/225；https://docs.rs/cmaes/latest/cmaes/index.html；https://docs.rs/optimizer/latest/optimizer/；https://github.com/raimannma/rust-optimizer。中高。
