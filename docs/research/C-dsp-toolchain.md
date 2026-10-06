# C DSP 工具链（解码/重采样/时变/响度/stem）

- 目的：核实侧车进程 DSP 工具链的许可、Rust 可用性与工程可行性。
- 适用范围：侧车进程（独立进程 + IPC）；REAPER 进程内仅控制面。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：D-eng-eco（IPC/许可矩阵）；后续 DEC、ARCHITECTURE 依赖本文。

## 1 解码：symphonia（首选） vs FFmpeg（兜底）

- symphonia 许可 **MPL-2.0**（非 BSD-2，勘误任务假设）。可闭源链接，修改文件须开源披露。纯 Rust，MSRV 1.85，默认 SIMD。来源：https://github.com/pdeljanov/Symphonia + https://crates.io/crates/symphonia ，2026-10-05，高。⚠️法务确认禁 copyleft 政策是否含文件级。
- FFmpeg 基线 **LGPL-2.1+**；`--enable-gpl` 变 GPL-2+；`--enable-nonfree` 不可分发；无商业许可。`--enable-librubberband` 即触发 GPL。合规：不用 gpl/nonfree、动态链接、分发源码 + configure 文本 + 声明。来源：https://www.ffmpeg.org/legal.html + https://github.com/FFmpeg/FFmpeg/blob/n8.0.1/LICENSE.md ，高。模式：官方 LGPL 构建外调或动态链接；`ffmpeg -buildconf` 确认。symphonia 主力 + FFmpeg fallback 双后端。

## 2 重采样 rubato（MIT/Apache，无风险）

- 纯 Rust 分块，实时热路径。`Fft` 固定比率快又好；`Async::new_sinc` 可变比率最重（降 sinc_len/多项式阶换速）；`Async::new_poly` 最快有损。离线用 Fft，变速用 sinc（256 + BlackmanHarris2 起步），实时预览用 poly，具体以 AB 定。来源：https://crates.io/crates/rubato + https://docs.rs/rubato/latest/rubato/ ，高。

## 3 时变 Rubber Band（明确结论：GPL-2-or-later/商业双许可）

- 无 LGPL/permissive 选项。GPL 侧见 https://github.com/breakfastquay/rubberband ；商业侧 £590 起（署名）/£1490/£9320，见 https://breakfastquay.com/rubberband/license.html + https://breakfastquay.com/technology/license.html ，高。App Store 分发必须买商业。
- Rust 绑定 `rubberband/rubberband-sys` 经 bindgen 编 C++（需 Clang≥9），绑定不改变 GPL 传染。来源：https://docs.rs/crate/rubberband/latest ，高。
- R2 Faster（默认）vs R3 Finer（CPU 高）；Offline 双遍 vs RealTime 单遍。来源：https://breakfastquay.com/rubberband/integration.html ，高。
- 结论：闭源内嵌/链接必须买商业；独立进程 pipe 不能洗白 GPL（用户自装调用另议，体验差，须法务确认）。替代 `timestretch` 0.15.0（纯 Rust，CI 对标 RB CLI）泛化不如 R3，⚠️需 AB。来源：https://docs.rs/crate/timestretch/latest ，中。
- 决策（2026-10-06，人类拍板，TSK-207）：采用 Rust 路线——时变统一用 `timestretch`（G1-G3 全过 + G4 门禁已定，`experiments/timestretch-ab.out.txt`）；不采购 RB 商业许可、不搭建 RB 工具链；未来时变集成点为 `dsp` crate（消费者出现时接入）。

## 4 响度 BS.1770（三者皆 permissive）

- `ebur128` MIT，实现 BS.1770-4 + BS.2051-2（门控 400ms/75% overlap），过 EBU 3341/3342 全测试。来源：https://github.com/sdroege/ebur128 + https://crates.io/crates/ebur128 ，高。
- `bs1770` Apache-2.0，纯 Rust BS.1770-4 K 计权构建块。来源：https://github.com/ruuda/bs1770 ，高。
- `ebur128-stream` MIT/Apache，纯 Rust push 流、零分配、`no_std` 可选，自称 3341 14/14 + 与 ebur128 ±0.5 LU 交叉验证（⚠️复跑 calibration）。来源：https://crates.io/crates/ebur128-stream ，中。
- 建议：实时条用 stream，离线报告用 ebur128 或 stream 全栈。生产基准 BS.1770-4（K 计权 + -70/-10 门控 + Annex2 真峰）；立体声主路径 -5 差异小，合规文件引用写 -4（含 -5 兼容声明，须确认）。

## 5 stem 分离 Demucs（代码 MIT ≠ 权重可商用）

- 代码 MIT（2020-04-13 起）。来源：https://github.com/facebookresearch/demucs/blob/main/LICENSE ，高。
- 官方权重仅科研用（issue #327 原文 weights not covered by MIT, scientific purposes only；MusDB 条款连带）。来源：https://github.com/facebookresearch/demucs/issues/327 ，高。⚠️法务：官方权重随产品分发/商用推理须重授权或自训/换源。
- 成本（3 分钟歌）：CPU RTF~1.5（官方，~4.5min）；M4 Pro 实测单 specialist RTF 0.20 / 全 bag 0.49；GPU M4 MPS 全 bag ~47s，L4 ~7s。显存≥3GB，默认 ~7GB，不够降 `--segment` 或 `-d cpu`。来源：https://pypi.org/project/demucs/ + demucs-onnx 页，中（硬件相关，需实测）。
- 形态：Python+PyTorch ~2GB；`demucs-onnx` numpy+ort ~50MB，FP16 体积减半（316→166MB/stem，diff ~6e-5），CPU 1.31x。HTDemucs 分段 7.8s + overlap-add + shift trick（仅 GPU）。离线批处理，不进实时链；模型懒下载 + 服务端缓存。

## 6 选型初判

解码 symphonia（MPL）+ FFmpeg 兜底；重采样 rubato；时变买证后 RB（R3 离线/R2 实时）否则 timestretch；响度 ebur128-stream/ebur128；stem Demucs ONNX 侧车/云端（先解决权重授权）。

## 7 ⚠️清单（转 TSK）

symphonia 覆盖率/破损行为；`ffmpeg -buildconf` + LGPL 分发流程；rubato 三档 AB；RB 采购主体 + R2/R3 基准；响度 calibration 复跑；Demucs 权重授权 + 目标机 RTF/显存/懒加载。
