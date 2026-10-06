# A05 集成通道对比：reaper-rs vs ReaScript vs OSC vs ReaLearn

- 目的：对比四种 REAPER 集成通道的能力、精度、线程约束与许可代价，选定 SynthLM 主写入通道。
- 适用范围：集成形态决策 D1/D2、线程模型、参数语义层复用判断。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：A01–A03。

## 1 reaper-rs（推荐主写入通道，MIT）

- 本质：C++ ReaPlug Extension 的 Rust 绑定；`LICENSE: MIT`。crates.io 已过时，须用 `branch="master"` git 依赖。stars ~112–122，Last push 2026-03-31（镜像页，⚠️以 git log 实测为准）。来源：https://github.com/helgoboss/reaper-rs ，2026-10-05，高。
- 三层：`reaper-low`（自生成，~95%）/ `reaper-medium`（手写类型安全，~13%，约100/800）/ `reaper-high/rx/fluent`（unstable，作者明示不应使用）。ReaScript 可见集 ≠ Extension 可见集；旧版 REAPER 缺新函数时 GetFunc 返回 NULL，须 `APIExists` 判空。来源：同仓库 README + https://docs.rs/reaper-low/latest/reaper_low/ ，2026-10-05，高。
- 音频钩子：`Audio_RegHardwareHook` + `OnAudioBuffer` 示例存在，但≠可在音频线程调任意 API。`CreateTakeAudioAccessor/CreateTrackAudioAccessor/DestroyAudioAccessor/AudioAccessorValidateState` 官方明确 must ONLY call from main thread；传输控制类 Only call from UI thread；`Audio_IsRunning/IsPreBuffer` 标注 threadsafe；`ShowConsoleMsg` v7.0+ 支持多线程。音频回调内禁工程变更，参数落地回 main thread。`TrackFX_SetParam` 音频线程行为未明示，按禁止处理。来源：`reaper_plugin_functions.h` + https://www.cockos.com/reaper/sdk/vst/ + v7.0 changelog，2026-10-05，高/中高。

## 2 ReaScript Lua/EEL2/Python（原型/批量脚本）

- EEL2/Lua 开箱即用；Python 需另装、Preferences 配 DLL，REAPER 7 未移除（v7.82 仍列 `RPR_*` 约定）但官方定性 harder/slower，已边缘化，不选为主通道。Lua v7 起 5.4.6（6.x 5.3.5），升级可破坏旧脚本。来源：https://www.reaper.fm/sdk/reascript/reascript.php + reascripthelp v7.82，2026-10-05，高。
- 执行上下文 main thread；`defer()` 常驻约 30Hz timer 量级（版本相关 ⚠️需实测），只适合控制率，不适合逐采样平滑；跨脚本通信 `gmem_*`（协作式，无原子保证）。普通脚本默认建 undo 点，defer 默认不建。来源：同上 Advanced 章 + Reaticulate `rfx.lua` 实践，2026-10-05，高。
- 平滑必须二段式：ReaScript/Extension 只发控制率目标（30–60Hz 差分节流），采样级插值靠插件内 smoothing。

## 3 OSC（外部控制器/演示，不作主路径）

- 语义在 `Default.ReaperOSC` 注释中；官方页只讲 pattern 机制。`FX_PARAM_VALUE n/track/@/fx/@/fxparam/@/value`（normalized 0..1）+ `f` raw + `s` 字符串 + `INST/LAST_TOUCHED/FOCUSED` 变体；类型前缀关键，发整数 0/1 会被忽略须发 float（论坛实测）。精度 float32 normalized 为主，精确 dB/Hz 用 `f/.../db|hz` 或 `s` 回读校验。来源：https://www.reaper.fm/sdk/osc/osc.php + Default.ReaperOSC，2026-10-05，高。
- 反馈 UDP 双向，默认只发选中轨参数，全量易刷屏（注释明示删减 TIME/BEAT/VU/FX_PARAM）。无增删 FX 对应项（仅 BYPASS/OPEN_UI/PRESET±/PARAM/WETDRY/EQ/INST）。无事务语义，不保证采样精确。来源：同上，2026-10-05，高。

## 4 ReaLearn/Helgobox（设计参考，GPL-3.0 不依赖）

- 仓库已合入 `helgoboss/helgobox`，`LICENSE: GPL-3.0` 全文已核验；运行时捆绑受 copyleft 约束，只能借鉴设计。来源：https://github.com/helgoboss/helgobox ，2026-10-05，高。
- 可借鉴：Source character（Range/Button/Encoder-relative/Toggle-only）+ Virtual control（Multi vs Button，compartment 解耦）+ tag（mapping/instance/compartment 三级）+ conditional activation + Glue（`target_interval/step_size/out_of_range` 单映射变换）。来源：https://docs.helgoboss.org ，2026-10-05，高。
- 无开箱 preset morph（Pot 目标仅 Browse/Preview/Load；社区 morph 靠 spk77 包络插值脚本）。SynthLM morph 应自研“预设向量插值→控制率包络→插件内平滑”。来源：ReaLearn docs + ReaTeam 脚本，中高。

## 5 通道对比

| 维度 | Extension (reaper-rs) | ReaScript Lua | OSC | ReaLearn |
|---|---|---|---|---|
| 权限 | 全 API（GetFunc 为准） | 大部分（子集） | 参数/走带子集，无增删 | 其 VST 内全，外部不可编程 |
| 线程 | main + 受限 audio hook | main 串行 | 外部投递再串行化 | 内实时安全，外仅 MIDI/OSC/Lua |
| 精度 | double SetParam | double 同 API | float32 normalized 为主 | 内高精度，外受限 |
| 增删 FX | ✅ | ✅ | ❌ | 间接 |
| undo | 手动 Begin/End 可控 | 自动+手动 | 不经 undo | 内部管理 |
| 许可 | MIT 轻量 | 随工程 | 协议无依赖 | GPL-3.0 重 |

结论：主写路径 = Extension（medium 优先、缺口回落 low）main-thread 队列 + 合并 undo；Lua 仅原型/迁移脚本；OSC 仅可选监听/演示；Helgobox 不捆绑。
