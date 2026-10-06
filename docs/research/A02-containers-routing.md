# A02 FX 容器 / 文件夹 FX / ReaInsert / 多轨路由

- 目的：核实 REAPER 7 FX Container、文件夹 FX、ReaInsert、多轨路由/发送的真实能力，判定 L1（管线编排通过 REAPER FX 链/容器/路由实现）是否充分。
- 适用范围：SynthLM L1 编排层；REAPER 7.x（基线 7.82，2026-10-04）。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：A01（FX 基础寻址）。

## 1 结论总览

1. FX Container 可被 ReaScript 创建/查询/嵌套，但无专用 Container API，只能靠 `GetNamedConfigParm` + `0x2000000` 手工寻址，嵌套可行但易碎，须封装地址层 + GUID 锚定。高。
2. 发送/接收路由 API 完整：`CreateTrackSend / GetTrackNumSends / Get/SetTrackSendInfo_Value / RemoveTrackSend` 均存在。高。
3. `I_NCHAN` 按 64ch 设计必安全；新文档写 2–128，65–128 ⚠️需实测。中高。
4. 无独立 Folder FX API；文件夹父轨即普通轨，其 FX 链即 Folder FX；`I_FOLDERDEPTH` 维护关系。高。
5. ReaInsert 即普通 FX，PDC 由引擎处理；Ping 不可脚本化，录制/渲染对齐有已知坑，硬件环路不得承诺自动补偿。中，细节⚠️需实测。

## 2 FX Container

- 官方引入：v7.0（2023-10-16）changelog：sub-chains、configurable IO/channel、parameter mappings、feedback、internal modulation、parallel。API 新增 `FX_GetNamedConfigParm(container_count)`、`TrackFX_/TakeFX_ via documented addressing scheme`、`GetTouchedOrFocusedFX`。来源：https://forum.cockos.com/showthread.php?t=283579 ，2026-10-05，高。
- 可用键：`container_count`、`parent_container`、`container_item.<i>`、`container_map.add`、`param.<N>.container_map.fx_index/fx_parm`、`container_map.add.<fx>.<param>`（快捷写法 ⚠️需实测）、`container_nch(_in/_out)`（键名 ⚠️需实测）、`fx_type/fx_name/renamed_name`。来源：https://forum.cockos.com/showthread.php?t=284400 （Justin 官方示例 `get_fx_id_from_container_path/fx_map_parameter`），2026-10-05，高；mpl/reaKontrol/MT4U 佐证，中。
- 寻址：顶层 0-based；容器内 `0x2000000 + stride` 公式，stride=`GetCount+1`，逐层乘 `(1+container_count)`；顶层增删导致子地址变化，不得持久化裸 index，须 `(TrackGUID, FXGUID, 容器路径)` + 每次重算。来源：同上 Justin 帖 + https://github.com/MT4Mars/MT4U/blob/main/MT4U_FX_Rack_Reaper7/MT4U_FX_Navigator.eel ，2026-10-05，高。
- 创建/查询/嵌套/移入：`AddByName(track,"Container")` 创建；`GetNamedConfigParm(container_count/parent_container/fx_type)` 查询；`CopyToTrack(...,dest_inside_container,is_move)` 移入；嵌套逐层展开（mpl 上限 10 层写法）。来源：https://forum.cockos.com/showthread.php?p=2714770 ，2026-10-05，高。
- 限制：容器内 FX 不能直接挂 envelope，须先 `container_map.add` 映射到父容器再 `GetFXEnvelope`。来源：Justin 回帖同上，高。无专用 Container API（社区抱怨未被采纳），地址脆弱，日志应同时打印路径与 GUID。中高。

## 3 文件夹 FX / Input FX

- 文件夹关系由 `I_FOLDERDEPTH`（1=起点，0=普通，负=结束）、`I_FOLDERCOMPACT`（折叠 UI）、`GetParentTrack/GetTrackDepth` 维护。来源：https://reascript.dev/ + https://www.reaper.fm/sdk/reascript/reascripthelp.html （v7.82），2026-10-05，高。
- `TrackFX_GetRecCount` 为 input FX 数量，与 `GetCount` 配合，`0x1000000` 寻址；与容器正交。来源：官方 API 列表 + https://github.com/MichaelPilyavskiy/ReaScripts/blob/master/Various/mpl_Mapping%20Panel%20(background).lua ，2026-10-05，高/中。

## 4 发送/路由

- 核心函数签名与 category（0=sends/-1=receives/1=硬件输出）存在。来源：https://www.reaper.fm/sdk/reascript/reascripthelp.html ，2026-10-05，高。
- 参数键：`B_MUTE/B_PHASE/B_MONO/D_VOL/D_PAN/D_PANLAW/I_SENDMODE(0 post-fader/1 pre-fx/2 post-fx deprecated/3 post-fx)/I_AUTOMODE/I_SRCCHAN/I_DSTCHAN/I_MIDIFLAGS/P_DESTTRACK/P_SRCTRACK`。`I_SRCCHAN/DSTCHAN` 多通道编码细节、硬件 `&512`、mono `&1024` 按 reapy + 论坛实现为准，范围扩展 ⚠️需实测。来源：https://python-reapy.readthedocs.io/en/latest/_modules/reapy/core/track/send.html + https://forum.cockos.com/showthread.php?t=278144 ，2026-10-05，中高/中。
- 父发送：`B_MAINSEND/C_MAINSEND_OFFS/C_MAINSEND_NCH`；`I_NCHAN` 旧绑定 2–64、新文档 2–128，取交集 64 为安全上限。Take 通道数由 max（源声道，take FX 输出，轨通道）决定（schwa 官方回复），另有 `TAKEFX_NCH` chunk 覆盖。来源：https://reascript.dev/ + https://forum.cockos.com/showthread.php?t=286031 ，2026-10-05，高。

## 5 ReaInsert

- 即普通 FX：`AddByName(track,"ReaInsert")` + `Get/SetParam` + `SetEnabled` + `GetIOSize/SetPinMappings` 照常。补偿分两层：驱动往返延迟 + 面板内 Ping 附加补偿，执行方为引擎/PDC。来源：https://prorec.com/latency-and-delay-compensation-in-reaper/ ，2026-10-05，中。
- Ping 触发无公开 API；Ping 结果可读性、PDC 禁用键、参数序号均 ⚠️需实测（以 `GetParamName` 实机枚举为准，不可硬编码）。录制错位、长轨漂移、Monitoring FX 污染 Ping 等多起个案。来源：https://forum-amz.cockos.com/showthread.php?t=220528 + https://forum.rme-audio.de/viewtopic.php?id=33861 + https://forum.cockos.com/showthread.php?t=186837 ，2026-10-05，中。
- 含义：纯软件管线 PDC 自动处理；硬件环路设计为引导式校准（Ping→录 click 基准→测偏移→记元数据），CI 只覆盖纯软件。

## 6 L1 映射与回退

- 节点→轨/容器/容器内 FX；节点内串并行→链顺序 + 容器并行 + pin 映射；边→发送/父发送；分组→文件夹父轨；多通道并行→`I_NCHAN`+`container_nch*`+pin（mpl Multi-mono 为先例 https://github.com/MichaelPilyavskiy/ReaScripts/blob/master/FX/mpl_Multi-mono%20container.lua ）；侧链→发送到 3/4 通道 + pin（v7 支持拖拽到容器内 FX）。参数联动 `container_map/plink` 双轨易错需封装。来源：v7.0 帖 + 上述脚本，2026-10-05，高/中。
- 回退：容器异常→展平为串行轨+发送；通道超 64→多轨拆分，128 作实验开关；硬件节点标 `calibration_required`；MIDI 路由第一阶段标实验性。

## 7 ⚠️需实测清单

`container_nch*` 键名、`container_map.add.<fx>.<param>` 快捷写法、`param.N.container_map.delete`、`I_NCHAN=128` 跨平台可用性、ReaInsert 参数表/Ping/PDC 键、`AddByName("Container")` 跨语言稳定性、Master/Monitoring 链容器差异。

## 8 公式验证补记（2026-10-06，TSK-101，`experiments/container-50op.out.txt`）

- 地面真值法：`container_count` + `container_item.N`（0-based）递归枚举，全程只用已核验键；公式 `addr = 0x2000000 + (p+1)*(count+1) + (c+1)` 用每轮新鲜 count 独立重算并与真值比对。
- 50/50 通过（move-in 6、move-out 5、del-top 6、add-top 9、churn 8，其余为安全 skip 轮，验证步同样执行）：公式成立条件 = 每次操作前重算，过期 stride 地址必失效。
- 附带发现：`CopyToTrack` 入容器可用过期 stride 目标（REAPER 侧结构化归一，探针 `container-probe.out.txt`：dest=…37 落位后真值为 …35）；容器显示名本地化（`容器`）；`parent_container` 顶层返回空（ok=false）；嵌套容器未测（后续 spike）。
- Rust 层 `container_addr.rs` 实现同一公式（decode 带 re-encode 校验）。
