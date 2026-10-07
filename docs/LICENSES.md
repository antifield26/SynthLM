# LICENSES（依赖许可登记册）

- 目的：登记一切新增依赖与外部组件的许可结论；新增依赖必须同步更新本表（AGENTS.md 红线 6）。
- 适用范围：全仓库；约束：内部非商业、无分发、不购买许可。
- 状态：Accepted（种子版，随 TSK-113 扩展）
- 最后核验日期：2026-10-06
- 依赖文档：docs/DECISIONS.md（DEC-025）、docs/EVALUATION.md（§6）。

| 依赖/组件 | 许可 | 来源 | 内部使用结论 | 日期 |
|---|---|---|---|---|
| reaper-rs（`reaper-medium` + `reaper-low`，git rev `659b22b` pinned） | MIT | <https://github.com/helgoboss/reaper-rs>（README 要求用 git master，不用 crates.io 陈旧版） | 可用，仅内部非商业运行；传递依赖（nutype git 分支、vst、winapi 等）以 Cargo.lock 为准 | 2026-10-06（rev 提交 2026-09-12 "cargo fmt"，完整 rev `659b22bfbc34bf4a5a40902e6fdf60ccfd34ca23`） |
| thiserror 2.0.21（`synthlm-bridge` 直接依赖，库边界错误类型） | MIT OR Apache-2.0 | crates.io/crates/thiserror（registry 缓存 manifest 核验） | 可用，仅内部非商业运行 | 2026-10-06 |
| serde 1.0.229（`synthlm-bridge` 直接依赖，anchor 序列化，derive） | MIT OR Apache-2.0 | crates.io/crates/serde（registry 缓存 manifest 核验） | 可用，仅内部非商业运行 | 2026-10-06 |
| serde_json 1.0.151（`synthlm-bridge` dev-only，单测 JSON 往返） | MIT OR Apache-2.0 | crates.io/crates/serde_json（registry 缓存 manifest 核验） | 可用，仅内部非商业运行（不进运行时） | 2026-10-06 |
| interprocess 2.4.4（`synthlm-common` 直接依赖，TSK-107 IPC local socket；默认特性，无 tokio） | 0BSD OR Apache-2.0 | <https://crates.io/crates/interprocess>（仓库 <https://github.com/kotauskas/interprocess>；API 按 docs.rs 2.4.4 于 2026-10-06 核验） | 可用，仅内部非商业运行；传递依赖（libc/recvmsg/widestring/windows-sys 等）以 Cargo.lock 为准 | 2026-10-06 |
| serde 1.0.229（`synthlm-common` 直接依赖，IPC 帧/audit 序列化，`derive` 特性） | MIT OR Apache-2.0 | <https://crates.io/crates/serde>（registry 核验） | 可用，仅内部非商业运行 | 2026-10-06 |
| serde_json 1.0.151（`synthlm-common` 直接依赖，IPC 首版 JSON 帧） | MIT OR Apache-2.0 | <https://crates.io/crates/serde_json>（registry 核验） | 可用，仅内部非商业运行 | 2026-10-06 |
| serde_json 1.0.151（`synthlm-acrd` 直接依赖，TSK-405 demo 产物序列化） | MIT OR Apache-2.0 | crates.io（与 workspace 同版本，无新增包） | 仅内部非商业运行 | 2026-10-06 |
| serde_json 1.0.151（`synthlm-ui` 直接依赖，TSK-506 计划 JSON 解析） | MIT OR Apache-2.0 | crates.io（与 workspace 同版本，无新增包） | 仅内部非商业运行 | 2026-10-07 |
| regex 1.13.1（`synthlm-profile` 直接依赖，TSK-604 存储 `name_regex` 编译校验） | MIT OR Apache-2.0 | crates.io/crates/regex（Cargo.lock pin；2026-10-07 核验） | 仅内部非商业运行 | 2026-10-07 |
| thiserror 2.0.21（`synthlm-common` 直接依赖，库边界 `IpcError` 类型） | MIT OR Apache-2.0 | <https://crates.io/crates/thiserror>（registry 核验） | 可用，仅内部非商业运行 | 2026-10-06 |
| shared_memory 0.12.4（`synthlm-common` 直接依赖，TSK-115 shm 通道；owner-only 创建） | MIT OR Apache-2.0 | <https://github.com/elast0ny/shared_memory-rs>（crates.io；2026-10-06 核验） | 可用，仅内部非商业运行；含 2 处论证 unsafe（common/shm.rs） | 2026-10-06 |
| symphonia 0.6.1（`synthlm-eval` dev-only，TSK-206 解码矩阵） | MPL-2.0 | <https://github.com/pdeljanov/Symphonia>（crates.io；2026-10-06 核验；0.6 breaking API 注意） | 仅内部非商业运行，不进运行时；修改其文件须开源该文件 | 2026-10-06 |
| ebur128-stream 0.2.0（`synthlm-eval` dev-only，TSK-208 交叉验证；生产 metering 仍用 `ebur128`） | MIT OR Apache-2.0 | <https://github.com/vanjamodrinjak21/ebur128-stream>（crates.io；纯 Rust `forbid(unsafe_code)`；2026-10-06 核验） | 仅内部非商业运行，不进运行时 | 2026-10-06 |
| reqwest 0.12（`synthlm-planner` 直接依赖，TSK-116 真传输；`default-features=false` + `blocking,json,rustls-tls`，无 native-tls/openssl） | MIT OR Apache-2.0 | crates.io（2026-10-06 核验） | 仅内部非商业运行 | 2026-10-06 |
| webpki-roots 1.0.9（reqwest 经 rustls 传递依赖） | **MPL-2.0（文件级）** | crates.io（2026-10-06 核验） | 内部运行合规（DEC-025）；修改其文件须开源该文件；随 reqwest 链复核 | 2026-10-06 |
| eframe 0.36.2 / egui =0.36.2（`synthlm-ui` 直接依赖，TSK-119 主窗） | MIT OR Apache-2.0 | crates.io（2026-10-06 核验） | 仅内部非商业运行 | 2026-10-06 |
| anyhow 1.0.104（`synthlm-ui` 直接依赖，应用层错误） | MIT OR Apache-2.0 | crates.io（2026-10-06 核验） | 仅内部非商业运行 | 2026-10-06 |
| Noto Sans SC（`crates/ui/assets/` 子集 149KB，SIL OFL 1.1，OFL 文本随包） | SIL OFL 1.1 | google/fonts `NotoSansSC[wght].ttf` v2.004（Regular 400 实例化；2026-10-06 核验） | 内部使用合规（署名与许可文本随包）；上游变更需重跑子集化 | 2026-10-06 |
| ctrlc 3.5.2（`synthlm-acrd` 直接依赖，TSK-501 优雅停机；另有 unix-only 传递 `nix`，本机未下载待复核） | MIT OR Apache-2.0（本地 manifest 核验） | crates.io（2026-10-06 核验） | 仅内部非商业运行 | 2026-10-06 |
| ReaImGui 0.10.0.5（二进制扩展，用户侧安装，非仓库分发） | LGPL-3.0（另有 GPL-3.0 文本；仓库已归档并迁 codeberg） | <https://github.com/cfillion/reaimgui>（COPYING/COPYING.LESSER；2026-10-06 核验；sha256 800b216e… pin） | 用户机直装（ReaPack 默认仓亦有）；本仓库不分发该二进制 | 2026-10-06 |
| realfft 3.5.0（`synthlm-eval` 直接依赖，MIR v1 STFT 实数 FFT） | MIT | crates.io/crates/realfft（registry 缓存 manifest 核验） | 可用，仅内部非商业运行；传递依赖 rustfft（MIT OR Apache-2.0）以 Cargo.lock 为准 | 2026-10-06 |
| rustfft 6.4.1（`synthlm-eval` 直接依赖，`Complex` 类型 + realfft 后端） | MIT OR Apache-2.0 | crates.io/crates/rustfft（registry 缓存 manifest 核验） | 可用，仅内部非商业运行；传递依赖（num-complex/num-traits/num-integer/primal-check/transpose/strength_reduce，均为 MIT OR Apache-2.0）以 Cargo.lock 为准 | 2026-10-06 |
| ebur128 0.1.10（`synthlm-eval` 直接依赖，MIR v1 LUFS/true-peak，EBU R128） | MIT | crates.io/crates/ebur128（registry 缓存 manifest 核验；上游声明通过 EBU TECH 3341/3342 测试集） | 可用，仅内部非商业运行；纯 Rust（默认特性无 C 编译，`c-tests` 未启用）；传递依赖（bitflags 1.3.2、dasp_frame/dasp_sample 0.11.0、smallvec 1.16.2，均为 MIT 系）以 Cargo.lock 为准 | 2026-10-06 |
| thiserror 2.0.21（`synthlm-eval` 直接依赖，库边界 `EvalError` 类型） | MIT OR Apache-2.0 | crates.io/crates/thiserror（registry 核验） | 可用，仅内部非商业运行 | 2026-10-06 |
| lancedb 0.39.0（`synthlm-retrieval` 可选依赖，`lancedb-real` 非默认特性，TSK-602 真后端；`features=["remote"]` 纯为绕过上游门控 bug，运行时只连本地表目录） | Apache-2.0 | <https://crates.io/crates/lancedb>（registry 缓存 manifest 核验；上游 <https://github.com/lancedb/lancedb>；2026-10-07 核验） | 可用，仅内部非商业运行；传递依赖（arrow 58 / datafusion 54 / lance 12 / object_store / tokio 等，`remote` 另带 tonic/reqwest/arrow-flight 纯编译依赖，从不发起云调用）以 Cargo.lock 为准 | 2026-10-07 |
| arrow-array 58.4.0 / arrow-schema 58.4.0（`synthlm-retrieval` 可选依赖，随 `lancedb-real`；RecordBatch 组装，须满足 lancedb `arrow ^58` 同版本类型） | Apache-2.0 AND MIT（array）/ Apache-2.0（schema） | <https://crates.io/crates/arrow-array>（registry 缓存 manifest 核验；2026-10-07） | 可用，仅内部非商业运行 | 2026-10-07 |
| futures 0.3.34（`synthlm-retrieval` 可选依赖，随 `lancedb-real`；`TryStreamExt` 收批量查询流） | MIT OR Apache-2.0 | <https://crates.io/crates/futures>（registry 缓存 manifest 核验；2026-10-07） | 可用，仅内部非商业运行 | 2026-10-07 |
| tempfile 3.27.0（`synthlm-retrieval` 可选依赖，随 `lancedb-real`；每索引独立表目录，`TempDir` Drop 即删） | MIT OR Apache-2.0 | <https://crates.io/crates/tempfile>（registry 缓存 manifest 核验；2026-10-07） | 可用，仅内部非商业运行 | 2026-10-07 |
| tokio 1.53.2（`synthlm-retrieval` 可选依赖，随 `lancedb-real`；`default-features=false + rt`，私有 current-thread runtime 桥接 async API，不跑网络/音频路径） | MIT | <https://crates.io/crates/tokio>（registry 缓存 manifest 核验；2026-10-07） | 可用，仅内部非商业运行 | 2026-10-07 |
| Demucs 官方权重 | 科研限定（代码 MIT） | issue #327 | 仅内部非商业运行；商用/分发重授权 | 2026-10-06 |
| MERT/MuQ 权重 | CC-BY-NC-4.0 | HF license 字段 | 仅内部非商业；不进产品基线 | 2026-10-06 |
| Rubber Band | GPL-2-or-later/商业 | breakfastquay 许可页 | 不购证→仅内部运行；分发即违法 | 2026-10-06 |
| OpenCode Go 云端（Tier1/ZDR-Tier2） | 用户确认条款（训练保留/ZDR；不声明计费） | 人类 2026-10-06 | 三档授权 + 审计；Key 仅 `.env` | 2026-10-06 |
| FFmpeg（二进制，Gyan 9.0.2-essentials，`C:\tools\ffmpeg`） | GPL-2+（`--enable-gpl` + `--enable-librubberband`） | `ffmpeg -buildconf` 存档 experiments/ffmpeg-buildconf-9.0.2.txt | 仅内部运行；禁作 LGPL fallback；LGPL 构建另寻（TSK-206）；分发即触发 F/B-001 | 2026-10-06 |

| ort 2.0.0-rc.13（`synthlm-eval` 可选依赖，仅 `onnx` 特性启用；预构建 ONNX Runtime 二进制经 `download-binaries` 在特性构建时获取，运行时纯本地） | MIT OR Apache-2.0 | <https://github.com/pykeio/ort>（crates.io；docs.rs 2.0.0-rc.13 `Session::builder/commit_from_file/inputs/outputs` 于 2026-10-07 核验） | 可用，仅内部非商业运行；默认特性关闭，主构建不触网；`ort` 内部 FFI unsafe 计入依赖边界（见 `crates/eval/src/clap.rs`），本 crate 不新增 `unsafe` | 2026-10-07 |
| LAION CLAP ONNX 权重（`lquint/clap-htsat-unfused-onnx` rev `b31e0c5b0737a45ca1b04b8d151bed48afd78fcc`，`model.onnx` 119654416 B，SHA256 `0763d8c6d03fe1675a3905b96ae3ff9ebfe316e3c0af9b64f8658ece57f0d5a5`，opset 18，基座 `laion/clap-htsat-unfused`） | Apache-2.0 | <https://huggingface.co/lquint/clap-htsat-unfused-onnx>（HF 模型页许可 + API rev + `curl -I -L` 头 `X-Linked-Size/X-Linked-ETag` 于 2026-10-07 核验；导出脚本 `export_clap.py` 同仓） | 仅内部非商业运行；本变更不下载权重，懒缓存 `%APPDATA%/SynthLM/models/clap/model.onnx`（DEC-027），以 rev + 字节数 + SHA 固定来源，`verify_cached_weight` 卡大小 | 2026-10-07 |

注（2026-10-07 校正）：workspace **并非**零外部依赖骨架。直接依赖已接入（reaper-rs git、serde/serde_json、thiserror、interprocess、shared_memory、reqwest+rustls、realfft/rustfft/ebur128、symphonia、eframe/egui 等），传递依赖以 `Cargo.lock` 为准；上表为接入后的许可登记（TSK-113 起持续维护）。新增依赖仍必须同步本表（AGENTS 红线 6）。

## 附：2026-10-06 TSK-113 核验记录

- `cargo tree --workspace`：零外部依赖（输出仅 8 个内部包），引入记录义务当前为空。
- FFmpeg：`~/tools/ffmpeg/ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe`（Gyan essentials，人类授权安装，已加入用户 PATH；此前 `C:\tools\ffmpeg` 已迁走）。`ffmpeg -buildconf` 存档见 `experiments/ffmpeg-buildconf-9.0.2.txt`：含 `--enable-gpl --enable-version3 ... --enable-librubberband`（无 `--enable-nonfree`）→ **该二进制为 GPL-2+ 构建，不是 LGPL**。
- 结论（与 C-dsp §1.2 预判一致）：此二进制不可作 LGPL fallback；仅限内部非商业运行（DEC-025）；LGPL fallback 需另找非 gpl 构建，记入 TSK-206。
