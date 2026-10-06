# D 工程与生态（GUI/IPC/稳定性/许可矩阵）

- 目的：核实 Rust GUI、IPC、稳定性模式与许可矩阵，为集成形态与侧车架构选型提供依据。
- 适用范围：Windows 首发 + macOS/Linux 次发；REAPER 7.x；Rust stable。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：C-dsp-toolchain；后续 DEC、ARCHITECTURE 依赖本文。

## 1 GUI：egui vs iced vs Tauri

- 许可皆 permissive：egui MIT/Apache（https://github.com/emilk/egui/blob/main/LICENSE-MIT）；iced MIT（https://github.com/iced-rs/iced）；Tauri Apache/MIT（https://github.com/tauri-apps/tauri/blob/dev/LICENSE_APACHE-2.0）；`tauri-egui` 已 ARCHIVED。2026-10-05，高。
- REAPER FX 内嵌（LICE 位图 `REAPER_FXEMBED_IBitmap` paint + 鼠标转发）无 HWND/GL 上下文，egui/iced 不可直接挂；离屏 blit 可行但输入/HiDPI 全手工。来源：https://github.com/justinfrankel/reaper-sdk/blob/main/sdk/reaper_plugin_fx_embed.h ，高。JUCE `ReaperEmbeddedViewDemo` 与 iPlug2 `DrawEmbeddedUI` 证明嵌入只适合小显/调试；iPlug2 另有 `IPlugReaperExtension` 可停靠原生窗（Win32 HWND/mac NSWindow + INI 持久化）。高。
- 取舍：FX 嵌入（跟链走，不停靠，小显）/ Lua 面板（ReaImGui docking，轻控制，`cfillion/reaimgui` + `gfx2imgui` + `rtk.Window`，天花板低）/ 扩展停靠窗（须桥接 Rust 运行时）/ **独立窗口（推荐主 UI）**。推荐：主 UI 独立窗口（egui 首选，iced 备选）+ 可选 Lua/ReaImGui 薄面板做 REAPER 内入口；Tauri 仅 Web 团队坚持且接受体积时备选。egui 即时模式贴 DAW 高频状态，iced Elm plumbing 啰嗦，⚠️小原型（波形 + 100Hz 刷新 + HiDPI）后锁。

## 2 IPC：interprocess 首选

- `interprocess` 许可 **0BSD/Apache**（最宽松）。来源：https://crates.io/crates/interprocess ，高。local socket（Unix=UDS，Windows=named pipe），async 仅 Tokio。
- 平台坑：Windows `\\.\pipe\NAME` 非文件，需 `first_pipe_instance` 常驻否则竞态（tokio NamedPipeServer 文档）；Unix UDS `sun_path` ~108B，macOS 仅 filesystem，Linux 可 abstract；路径放运行时短目录 + 启动清理；named pipe ACL 默认同用户，跨权限须显式安全描述符。三 OS 必跑建连/断线/权限用例，⚠️需实测。
- gRPC `tonic`（HTTP/2，hyper/tower/prost）：本地 IPC 大炮打蚊子（小消息 ~50B+ 开销），UDS 需定制 connector；仅强契约跨语言时选。WebSocket（tokio-tungstenite）：与前端同构、调试方便，但回环 TCP 抖动 + 占端口 + 防火墙弹窗，仅做调试/前端桥。
- 延迟定性（⚠️实测）：UDS/pipe 数十–数百 µs；回环 TCP/WS 百 µs–ms；gRPC 再加编解码。推荐：控制面 interprocess + 自定小帧头（首版 JSON/bincode，稳定后 prost 可选）；大数据（PCM/stem）走共享内存 + 事件通知；调试面可选 WS 只读；tokio 全程 async。

## 3 稳定性

- 进程隔离（Bitwig Together/By-plugin 沙盒档：崩只死沙盒，https://www.bitwig.com/support/technical_support/what-is-plug-in-crash-protection-26/ ，高）。
- watchdog 心跳（IPC ping/序列号）超时 kill→重拉→重建；先例 `plugin_host`（OutOfProcess 自动重启）、`maolan-plugin-host`。中。
- 状态快照：对照 `scuff` 隐形自动存（https://github.com/colugomusic/scuff ，高）：任务状态机 + 参数快照（chunk/preset）周期落盘，重拉后重放；WAL/journal + 原子写（tmp+rename）+ 版本化。
- 工程膨胀：快照只存参数 + 引用（指纹/相对路径），产物放外部内容寻址缓存，工程只存指针 + GC；大状态存 diff。与 A03 对齐。
- 沙盒能力不对称（DAWvid：沙盒内只读 transport，seek 需 companion extension 经 UDP 桥）须在协议层显式建模，不假设侧车直调 REAPER API。中。

## 4 许可矩阵初稿（⚠️全部法务终审）

- VST3 SDK **MIT**（3.8 起，旧 GPLv3/专有废止）：https://steinbergmedia.github.io/vst3_dev_portal/pages/VST+3+Licensing/VST3+License.html + https://github.com/steinbergmedia/vst3sdk ，高。保留文本即可；商标另合规。
- CLAP **MIT**：https://github.com/free-audio/clap/blob/1.2.7/LICENSE ，高。
- JUCE **AGPLv3/商业双许可**（JUCE 8，老 GPLv3 说法过期）：https://github.com/juce-framework/JUCE + https://juce.com/legal/juce-8-licence/ ，高。闭源分发须买商业，否则整物 AGPL 传染（含网络条款）。
- iPlug2 **permissive（ISC/zlib 类）** + 第三方各守其证：https://github.com/iPlug2/iPlug2/blob/master/LICENSE.txt ，高（逐文件头复核）。
- ReaLearn/Helgobox **GPL-3.0**：https://raw.githubusercontent.com/helgoboss/realearn/master/LICENSE ，高。仅参考，不得链接/复用。
- FFmpeg **LGPL-2.1+**（gpl 变 GPL-2+，nonfree 不可分发，无商业许可）：§C-dsp §1.2，高。动态链接 + 源码义务；禁 gpl/nonfree。
- Rubber Band **GPL-2-or-later/商业**：§C-dsp §3，高。闭源内嵌须买证（£590 起）。
- symphonia **MPL-2.0**：§C-dsp §1.1，高。可闭源链接。
- rubato/bs1770/ebur128/ebur128-stream/egui/iced/Tauri/interprocess：MIT/Apache/0BSD permissive，高。
- 模型权重：代码 MIT ≠ 权重可商用；Demucs 官方权重科研限定（https://github.com/facebookresearch/demucs/issues/327）；NC 条款禁商用。权重逐个建卡，默认官方权重仅对标，产品走自训/采购。

## 5 选型初判

UI 独立（egui 首选）+ Lua 薄面板；IPC interprocess + 共享内存，tonic/WS 非主通路；稳定性侧车隔离 + watchdog + 快照/WAL + 缓存外置；红线 GPL/AGPL（未购证 RB、ReaLearn、JUCE、FFmpeg-gpl）不得进闭源分发物。

## 6 ⚠️清单（转 TSK）

egui/iced 原型定锁；interprocess 三 OS 矩阵 + 共享内存基准；快照/journal/GC 与 A03 对齐 + 崩溃注入；JUCE/RB 采购与权重授权（法务）；VST3 MIT 文本随包 + 商标合规（法务）。
