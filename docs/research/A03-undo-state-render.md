# A03 Undo 事务 / 状态持久化 / 渲染与冻结

- 目的：核实 Undo 原子事务、工程内/全局状态持久化、离线渲染与冻结的真实行为，为候选应用/回滚与预览链选型提供依据。
- 适用范围：`Undo_*`、`SetProjExtState/SetExtState`、`Main_OnCommand` 渲染/冻结、`GetSetProjectInfo RENDER_*`、`RenderFileSection`；REAPER 7.x。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：A01、A02。

## 1 Undo 事务

- `Undo_BeginBlock2(proj)/Undo_EndBlock2(proj,desc,extraflags)` 可把 N 次 SetParam/增删 FX 包成一个合并点；必须配对，不可跨脚本 Begin/End；deferred 脚本不可靠。来源：https://www.reaper.fm/sdk/reascript/reascripthelp.html + https://forum.cockos.com/showthread.php?t=100849 （Schwa）+ https://forums.cockos.com/showthread.php?p=872795 ，2026-10-05，高。
- `descchange` 正常显示自定义名；`extraflags=0` + 纯 action 块易退化为 `ReaScript: run`；直接改 state（SetParam/MIDI）建议 `-1`（UNDO_STATE_ALL）；MIDI 需 `MarkTrackItemsDirty`；`2=UNDO_STATE_FX`。掩码：1 TRACKCFG/2 FX/4 ITEMS/8 MISCCFG/16 FREEZE/32 TRACKENV/64 FXENV/128 POOLEDENVS。来源：同上 + https://forum.cockos.com/archive/index.php/t-205780.html + https://forum.cockos.com/showthread.php?page=23&t=109934 ，2026-10-05，高/中。
- `*2` 首参 `proj`，0=当前 tab；`Main_OnCommand` 只作用活动工程，`Main_OnCommandEx` 可指定工程；多 tab 隔离性 ⚠️需实测。来源：同上，2026-10-05，高。
- 即使脚本未改工程也可能出现 `ReaScript: Run`（收拢前人未提交 latent state，非损坏；Schwa）。脏检查以 `IsProjectDirty/GetProjectStateChangeCount` 为准。来源：https://forum.cockos.com/showthread.php?t=276071 ，2026-10-05，高。

## 2 状态持久化

- `SetProjExtState(proj,extname,key,value)`：存 `.rpp` 随工程走，下次加载恢复；`SetExtState(section,key,value,persist)`：`true` 进 `reaper-extstate.ini` 全局，`false` 仅内存。值均为字符串；`key==""` 删整个 extname，`value==""` 删 key。`GetProjExtState` 读上次保存值。`P_EXT:xyz`（`GetSetMediaTrackInfo_String`）才跟 chunk 走、复制带走；`SetProjExtState` 不跟轨走。来源：官方文档 + https://forum.cockos.com/showthread.php?p=1872714 （Xenakios）+ https://forum.cockos.com/showthread.php?p=1964462 ，2026-10-05，高。
- 官方无容量承诺；KB～MB JSON 快照致 `.rpp` 膨胀/保存变慢/截断 ⚠️需实测。来源：文档缺条款本身，高（无承诺）/低（实际可用）。
- 换行坑：`SetProjExtState` 多行自动转 base64，`SetExtState` 不会（写多行污染 ini；5.28 加 changelog 防护）。硬规则：`SetExtState(persist=true)` 只写单行，任意数据先 base64；ProjExt 同样建议主动 base64。来源：https://forum.cockos.com/showthread.php?t=298318 （mpl+Justin）+ https://github.com/acklin83/Rea-Sixty/commit/bebe07f822911483626491c46019290be994e4d7> ，2026-10-05，高。

## 3 渲染/冻结

- 无单步 `RenderProject()`；范式为备份→`GetSetProjectInfo[_String](RENDER_*)`→`Main_OnCommand`→恢复。来源：https://myrrc.dev/ad2render/ ，2026-10-05，高。
- ID：40015 弹对话框；41824 用最近设置直接渲染；42230 同上且强制关窗（X-Raym 批量用，schwa 确认）；41855 新文件名；41823 进队列；原生 ID 永不变可硬编码，SWS/自定义须 `NamedCommandLookup`。来源：同上 + https://forum.cockos.com/showthread.php?t=263342 + https://github.com/X-Raym/REAPER-ReaScripts/blob/master/Various/X-Raym_Render%20selected%20tracks%20individually%20through%20master.lua + https://forum.cockos.com/showthread.php?t=47729 ，2026-10-05，高/中。
- `GetSetProjectInfo_String` 键：`RENDER_FILE/PATTERN/EXTRAFILEDIR/METADATA/TARGETS/STATS(SUMMARY只读)/FORMAT(FORMAT2 base64，亦可 evaw/l3pm 简写)`。数值版（社区验证）：`RENDER_SETTINGS/BOUNDSFLAG(0 custom/1 entire/2 time sel/3 regions/4 items/5 selected regions)/CHANNELS/SRATE/STARTPOS/ENDPOS/TAILFLAG/TAILMS/ADDTOPROJ/DITHER`。`FORMAT` base64 无把握不动。来源：官方文档 + https://python-reapy.readthedocs.io/en/latest/_modules/reapy/core/project/project.html + https://github.com/poulhoi/phoi_ReaScripts/blob/master/Render/phoi_Create%20sampled%20notes%20from%20selected%20midi%20item.lua ，2026-10-05，高/中高；新值 ⚠️需实测。
- `RenderFileSection(source,target,start%,end%,playrate)` 仅单源文件截段/变速，不走 FX/混音，不可用于候选试听；播放时不可用。来源：官方文档，2026-10-05，高。
- 冻结无 `TrackFX_Freeze`；只能 `Main_OnCommand(41223 stereo/40901 mono/40877 multichannel/41644 unfreeze)` 选轨触发；selection-based、有 undo 点，不适合高频预览循环，只用于固化胜出者。来源：https://github.com/iddqdsound/reaper/blob/main/iddqd_freeze%20tracks%20to%20stereo%20(adding%20a%20freeze%20icon%20to%20tracks).lua 等，2026-10-05，高。
- Temp 工程渲染：A 进程内（新 tab/复制工程→设 RENDER_*→42230→轮询 `EnumProjects(0x40000000)`/RENDER_TARGETS 文件出现→读回；焦点被抢、空工程 bounds、完成无事件只能轮询）；B 命令行 `reaper.exe -renderproject file.rpp`（按上次设置，适合离线批处理）。无整体工程 chunk API，只能 `Main_SaveProjectEx` 到临时 `.rpp` 文本 hack。来源：`EnumProjects` 官方文档 + https://forum.cockos.com/showthread.php?t=152797 + https://forum.cockos.com/showthread.php?t=178689 ，2026-10-05，中高。

## 4 采样精确性与速度

- 离线块大小留空则沿用声卡块大小，同时影响渲染；take pitch 包络 4096 大块出台阶；小块（64/1）保 MIDI/自动化精度。候选对比须固定块大小+bounds，否则差异来自块对齐。来源：https://forum.cockos.com/showthread.php?p=2073039 + https://forum.cockos.com/showthread.php?t=303892 ，2026-10-05，高/中。
- VST2 仅块精度；VST3 有采样精度标准但实现未必落实；JSFX 可采样精度但包络到参数仍可能 1/4 块。来源：https://forum.cockos.com/showthread.php?p=2883351 ，2026-10-05，中高。
- 全速离线部分插件不兼容（开头 glitch、长尾丢失、VEPro 缺采样、Pianoteq 卡死），兜底：小块→1x offline→online→冻其余轨；可勾 Inform plugins of offline state。来源：https://forum.cockos.com/showthread.php?p=2417993 等，2026-10-05，高（现象）/中（名单）。
- `CalcMediaSrcLoudness` 走 dry-run，经 `RENDER_STATS` 取回，可作响度归一轻量替代。来源：官方文档，2026-10-05，高。

## 5 对 SynthLM 建议

求解写参统一 `Undo_BeginBlock2(0)...Undo_EndBlock2(0,desc,-1)`；快照存 ProjExtState、偏好存 ExtState 单行/base64；预览优先当前工程 42230 固定小块+固定 bounds 而非冻结循环；渲染前后备份/恢复 RENDER_*；全速失败按 §4 兜底。

## 6 ⚠️需实测清单

SetParam 连写 0/-1 命名；ProjExt MB 级耗时/体积；41824 vs 42230 阻塞/关窗/完成延迟；BOUNDSFLAG=5/FORMAT 往返；temp tab 焦点/undo/dirty 隔离；目标插件集 null-test 差异。

## 7 Action ID 复核补记（2026-10-06，TSK-109）
| ID | 动作名 | 核验 |
|---|---|---|
| 40362 | Item: Glue items（ignoring time selection） | 空选点火：零 item 变化 + 1 undo 点（`experiments/action-ids-probe.out.txt`：`undo_delta=1 items_delta=0`）；空选调用须包事务或避免 |
| 40601 | Item: Render items as new take | 空选点火：零变化 + 0 undo 点（同证据 `undo_delta=0`）；干净 |
| 41824/42230 | Render project（后者强制关窗） | 未点火（防写文件）；以 X-Raym 脚本 + schwa 说明 + MCP 用法三源交叉为准 |
| 41223/40901/40877/41644 | Freeze stereo/mono/multichannel/unfreeze | 未点火（防冻用户轨）；以 iddqdsound/EUGEN 脚本 + menu 用法为准 |
| 42437 | dry-run selected items | 未点火；以 `RENDER_STATS` 文档原文为准 |

native ID 跨版本稳定（schwa t=47729）；SWS/自定义须 `NamedCommandLookup`。`kbd_getTextFromCmd` 返 nil 系语义误用（该 API 取按键绑定文本，非动作名）。人工eyeball（Action list 过滤对照）待人类确认后关闭 TSK-109。

## 8 渲染确定性与 M7 规则补记（2026-10-06，TSK-104）

- 确定性成立（隔离条件下 3/3 逐字节一致，FNV 均为 `11d6dc2f`，`render-line-02.out.txt`）：前提是静音非相关轨 + 固定内容；首轮在用户 live 工程（有外来未静音内容）中 r1≠r2=r3，根因为外来内容污染——门禁方法（mute others + 固定 bounds）有效。离线块大小 pin 需 SWS/`set_config_var`（stock Lua 无偏好写入 API），本次未改块大小。
- M7 完整规则（`render-m7-matrix.out.txt` 2×2）：`RENDER_SETTINGS &32` 选 item 源；`&(4<<16)` 开 → 单文件，关 → 逐项文件（后缀 `-001`/`-002`）；源为 master 时无论该位均为单文件；`RENDER_TARGETS` 以分号分隔列出全部目标，可直接计数。
- 待定：变速切换键（1x/online 兜底的设置键）与块大小 pin，需 SWS 路径，记 TSK-104 余项。
