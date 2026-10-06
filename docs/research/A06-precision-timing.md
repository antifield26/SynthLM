# A06 参数写入精度、时序与线程约束

- 目的：明确参数写入语义（直接写 vs 包络 vs 自动化模式）、undo 噪音控制、批量性能边界与线程红线。
- 适用范围：参数应用层、预览播放隔离、undo 事务边界（D7/D8）、音频线程红线。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：A01、A03、A05。

## 1 写入语义

- `TrackFX_SetParam/SetParamNormalized`：立即生效直接写入，不经过 automation 模式判断（Reaticulate flush 前暂存并关闭 AUTOMODE/global override 佐证）。包络写入（`GetFXEnvelope+Insert/SetEnvelopePoint+SortPoints`）写时间线，需 `SetTrackAutomationMode/SetGlobalAutomationOverride` 配合。一个写此刻声音，一个写时间线数据，不可混用同一参数而不仲裁。来源：Reaticulate `rfx.lua` + 官方 API 文档，2026-10-05，高。
- `TrackFX_EndParamEdit`：手势结束通知（fader touch 语义），批量后调用收尾；是否影响 undo 合并 ⚠️需实测。中。
- 容器 FX 不能直接挂 envelope，须 `container_map.*` 映射到顶层（Justin 官方回帖）。来源：https://forum.cockos.com/showthread.php?t=284400 ，2026-10-05，高。

## 2 Undo 噪音

- ReaScript 普通脚本默认建点，defer 默认不建；Extension `BeginBlock` 抑制中间各 API 建点至 `EndBlock` 合并。`SetParam` 单调调用会留 `Edit FX Parameter` 痕迹（Reaticulate 注释 opcode_flush generates undo）。实践：`PreventUIRefresh(1)+BeginBlock→循环SetParam→EndBlock("SynthLM: apply N params", FX|TRACKCFG)→PreventUIRefresh(-1)+按需UpdateArrange`；`Begin` 未配对 `End` 留巨型块。来源：官方文档 + https://forum.cockos.com/showthread.php?t=100849 ，2026-10-05，高。

## 3 批量性能（经验法则，非 SLA，⚠️需实测）

- 官方无 qps 承诺。defer tick ~30Hz × 每 tick <50 参数 + PreventUIRefresh 通常流畅；Extension 单 undo 块数百参数一次性可接受；持续 >200–500 writes/s 先差分+节流 30–60Hz+脏标记合并；OSC 默认 `DEVICE_FX_PARAM_COUNT 16`，全量反馈易淹没。来源：论坛/源码实践 + Default.ReaperOSC，中/高。
- 必须实测：单调 SetParam 耗时、100/500/1000 块端到端耗时、undo 块大小对撤销延迟影响。

## 4 线程约束（违反即崩溃/竞争）

| 类别 | 约束 | 来源 |
|---|---|---|
| `Create/Destroy/ValidateAudioAccessor` | 仅 main thread | 头文件原文，高 |
| 工程变更（DeleteTrack/AddByName/SetParam/GetSetChunk/InsertEnvelopePoint/Undo_*） | 仅 main thread，音频回调禁调 | SDK 并发模型 + 实践，中高；SetParam 音频线程按禁止处理 |
| 传输控制 hostcb 标注项 | 仅 UI thread | https://www.cockos.com/reaper/sdk/vst/ ，高 |
| `Audio_IsRunning/IsPreBuffer` | 线程安全 | 头文件 threadsafe，高 |
| ReaScript（含 deferred） | main 串行，无真并行；gmem 无原子保证 | 文档+实践，中高 |

## 5 落地规则

主写 = Extension main-thread 队列 + 合并 undo；平滑二段式（控制率目标→插件内插值）；直接调制 vs 包络留痕分开并仲裁（抄 Reaticulate push/pop）；容器/take/master 三路径分别封装 + `ValidatePtr2` + `GetFunc/APIExists` 兼容旧版；性能/undo/线程各做最小复现脚本，⚠️清零后定 SLA。
