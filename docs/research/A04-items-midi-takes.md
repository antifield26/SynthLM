# A04 Item / Take / MIDI 编辑能力

- 目的：核实 MIDI item 与 MIDI editor、audio item/take 编辑、Take FX 通道、P_EXT 跟随语义、非破坏性派生约定的真实能力，为候选派生写与回滚设计提供依据。
- 适用范围：REAPER 最低版本 v7.60，实测基线 v7.82/x64；Lua ReaScript 为准，附 C 签名要点。
- 状态：Draft（文档核验完成 + 真机 spike 部分验证通过，剩余 3 项转 TSK）
- 最后核验日期：2026-10-05
- 依赖文档：A01（FX 寻址）、A03（undo/渲染）。

> 每条结论后标注来源 + 置信度。`⚠️需实测` 不得作为 SLA。spike 证据在 `experiments/a04-spike*.out.txt`，运行方式见各 lua 头注（`reaper.exe -nonewinst <script>`）。

## 1 MIDI item 创建/读写（存在性：高）

- `CreateNewMIDIItemInProj(track,starttime,endtime,qnIn)`：建空 MIDI item，后续 `MIDI_InsertNote/SetAllEvts` 填充。实测建空 item + `GetActiveTake` + `TakeIsMIDI=true` 通过。来源：https://www.reaper.fm/sdk/reascript/reascripthelp.html v7.82 + spike02（`midi_item_created=true take_exists=true TakeIsMIDI=true`），2026-10-05，高。
- `MIDI_GetAllEvts/MIDI_SetAllEvts`：批量路线，缓冲格式 `{offset,flag,msglen,msg[]}`，Lua 用 `string.pack/unpack`。实测 `buf_len=36`（单音符）、空回写 buf 不变、hash 不变。来源：同上 + spike02（`getallevts_ok=true buf_len=36 nullwrite_buf_same=true`），高。
- `MIDI_GetNote/MIDI_SetNote/MIDI_InsertNote/MIDI_DisableSort/MIDI_Sort/MIDI_CountEvts/MIDI_GetHash/MIDI_GetTrackHash/MIDI_RefreshEditors/TakeIsMIDI`：均存在（spike01 `APIExists=true` 全通过，版本 `7.82/x64`）。`SetNote` 批量用 `DisableSort+循环+Sort`，或 `SetAllEvts` 一次提交。来源：同上 + spike01，2026-10-05，高。
- `MIDI_GetHash(take,notesonly)` / `MIDI_GetTrackHash`：7.60 已有（2017 论坛已在用，非新增）；只做快筛，裁决以 `GetAllEvts` 为准（juliansader 明确 hash 不完全可靠）。实测 hash_len=16，空回写不变。来源：https://forum.cockos.com/showthread.php?t=168563 + spike02，高/中。

## 2 MIDIEditor：后台 take 操作为主（高）

- `MIDIEditor_GetActive/GetTake/EnumTakes/OnCommand` 均需已打开 editor 句柄；`OnCommand` 对无效指针返 false。MIDI 数据读写（`MIDI_*` 全系以 `MediaItem_Take*` 为首参）不需要 editor。结论：SynthLM 走后台 take 操作，不依赖 editor 打开；仅用户已开 editor 时用 `GetTake/EnumTakes` 解析目标 + `MIDI_RefreshEditors` 同步。实测 `MIDIEditor_GetActive()==nil`（无 editor 时为空，符合预期）。来源：官方文档 + spike03f，高。

## 3 Undo/dirty（核心，分版本）

- 标准写法（兼容 v7.60）：`Undo_BeginBlock2(0) … MIDI 写 … MarkTrackItemsDirty(逐轨) … Undo_EndBlock2(0,desc,-1)`。`extraflags=-1`；pooled 疑似场景多标轨。来源：官方签名 + https://forum.cockos.com/archive/index.php/t-205780.html （v5.50+ 无 dirty 建不出 undo 点）+ sockmonkey72 实践，2026-10-05，高。
- v7.78 行为变更（不可依赖）：`MIDI_ setting APIs automatically mark dirty [p=1925555]`；v7.60 必须手写 dirty。实证：v7.82 上有实质改动（vel 100→77）但无 dirty 仍 `T1_delta=1`（自动建点）；空回写无 dirty 则 `delta=0`。来源：https://www.cockos.com/reaper/whatsnew.txt v7.78 + spike03c（`T1_delta=1`）/ spike02（`undo_no_dirty_delta=0`），2026-10-05，高。
- pooled 跨轨 dirty 缺陷 v7.78 修（`MarkTrackItemsDirty correctly handles pooled` [t=310092]）；v7.60 有单轨 dirty 跨轨丢失 bug，设计上规避 pooled 写（默认派生新 item）。来源：同 whatsnew + 论坛帖，高。
- 硬规则：指针在 Undo/Redo 后失效。实测 `ValidatePtr2` 写前 `true`，`Undo_DoUndo2` 后 take/item 均为 `false`；必须重取（`CountMediaItems+GetMediaItem+GetActiveTake`，轨同理）。来源：spike03f（`validate_after_undo_take=false item=false`），2026-10-05，高。

## 4 v7.60 vs v7.82 差异（MIDI/Item 相关）

- 7.60–7.82 无新增 `MIDI_* / MIDIEditor_*` 函数；只有 v7.80 `MIDIEditor_GetSetting(timebase_unit/pixels_per_timebase_unit)` 两 key（7.60 查返 -1）。`MIDI_GetHash` 系 7.60 已有。来源：whatsnew 全文检索，2026-10-05，高。
- Item 相关新增仅：v7.81 `I_MIXFLAG`（floor 无，需 `APIExists` 守卫）；v7.81 fade 键语义变 + v7.82 部分回滚（只用 `D_FADEINLEN/OUTLEN` 跨版本稳定）；v7.62 `RENDER_STATS_SUMMARY`（floor 无，本体 `RENDER_STATS` 可用）；v7.79 `Undo_GetNumEntries` 系（spike01 在 7.82 为 true，7.60 无，测试脚本外一律守卫）。`I_TAKEFX_NCH` 自 v7.07 可用，早于 floor。来源：同上 + spike01，高。

## 5 Item/take 基础与 P_EXT（高，附实测）

- `GetSelectedMediaItem` 官方 discourages（增删后逐个调低效），正式代码走 `CountMediaItems+GetMediaItem+IsMediaItemSelected`。`GetActiveTake/GetTake/GetMediaItemTake_Item/GetMediaItemTake_Track/GetMediaItemTake_Source/GetTakeName/GetMediaItemTakeByGUID` 均存在（spike01 全 true）。`SplitMediaItem` 原变左半、返右半；`AddTakeToMediaItem` 只建空 take；`SetActiveTake` 切换。实测 split 右半存在、计数 1→2。来源：官方文档 + spike02，高。
- `P_NAME/P_EXT:xyz/GUID`（take/item）、`P_EXT:xyz`（track/envelope）、`P_POOL_EXT:xyz`（automation item，注意前缀不同）均存在；`P_EXT:ORIGINAL_FILENAME` 为导入复制源文件名。`P_EXT` v6.36 引入、master P_EXT v6.43 修复，均早于 floor。`P_EXT` 跟 chunk 走、同工程复制跟随；`SetProjExtState` 以 GUID 为键不跟随。实测 take/item/track `P_EXT` 往返均为 `spike-*`。来源：官方文档 + Mespotine t-263217 + spike02，高。
- `P_EXT` 进 undo（7.82 验证干净对称）：写 `P_EXT:SYNTHLM_SYM=v1` 包独立 block，undo 后重取为空（item 完好），redo 后重取恢复 `v1`，各段 undo delta 恰为 1（`a04-spike05d-symmetric2.out.txt`：`verify=v1 → undo=<empty> → redo=v1`，2026-10-06）。规则：provenance 放 item/take/track P_EXT（随 undo 回滚、复制跟随），不放 ProjExtState。来源：MonkeyBars 实测表 + spike03f/spike05d，高；跨工程粘贴已由人工验证通过（`manual-pext-read.out.txt pext_value=hello-manual`，TSK-110 已关闭）。

## 6 Take FX 通道 I_TAKEFX_NCH（已实证，有前置条件）

- schwa 规则：take 通道数一般自动 = max（源声道，take FX 输出，轨通道），允许 `TAKEFX_NCH` chunk 键手工覆盖。来源：https://forum.cockos.com/showthread.php?t=286031 ，高。
- API：`Get/SetMediaItemTakeInfo_Value(take,"I_TAKEFX_NCH",n)`（注意是 TakeInfo 系，非 TakeFX_*）。文档括号 "returned value is read-only" 含义经实测澄清：无 FX 时 set 无效（spike02：MIDI 空 take `2.0→2.0 chunk无NCH`）；有 FX 实例（`TakeFX_AddByName(take,"ReaEQ",-1000)` 返回 0，注意 `instantiate=0` 仅查询，须用负值新建）后再 set 生效（spike04：MIDI 与 audio 均为 `2.0→8.0 chunk_hasNCH=true`）。`TakeFX_SetPinMappings` 只改单 pin 映射，不改总数。复制 take FX 时须同时搬运 NCH（SWS #938 丢通道教训；原生 `TakeFX_CopyToTake(src_take,0,dest_take,0,false)` 实测不带 NCH：源 `nch 2.0→8.0 chunk_hasNCH=true` → 目标 FX 数 0→1 但 NCH 仍 `2.0` 且 chunk 无 NCH，故复制后须显式回写 NCH，证据 `experiments/takefx-copy-nch.out.txt`，2026-10-06）。来源：whatsnew v7.07 + SWS https://github.com/reaper-oss/sws/issues/938 + spike02/04 + TSK-111 M3 spike，高。

## 7 非破坏性与派生

- 原地类：`ApplyNudge/SetMediaItemPosition/SetMediaItemLength/SetMediaItemInfo_Value(D_POSITION/D_LENGTH)/MoveMediaItemToTrack` 直接改 arrange 状态。来源：官方文档，高。
- 派生类无专用 ReaScript 函数，走 `Main_OnCommand(nativeID)`：40362 Glue ignoring time selection / 41588 within time selection / 40601 Render as new take / 40209-40361-41993 Apply FX as new take / 42437 dry-run selected items（供 `RENDER_STATS`）。native ID 跨版本稳定（schwa t=47729），SWS/自定义须 `NamedCommandLookup`。⚠️ 本轮 `kbd_getTextFromCmd(40362/40601,0)` 返 nil（section 约定不明），ID 以 `reaper-menu.ini` + Edgemeal 回帖为准，置信度中高，需在目标机 Action list 人工复核后方可硬编码，转 TSK。
- 推荐模式：`40601` render-to-new-take（源保留）+ 新 take 写 `P_EXT:SYNTHLM_PROV_{SRC_GUID,SRC_FILE,OP}`；MIDI 源无文件名（官方原文 in-project MIDI 无关联文件），用 take GUID + `P_EXT:ORIGINAL_FILENAME` 兜底；默认派生新 item（`CreateNewMIDIItemInProj` + 填充 + 命名打标），禁改用户源 take；glue 单 item 后新 take 不继承 take P_EXT（实测：audio item 新 take 写 `P_EXT:SYNTHLM_GLUE=probe` → `Main_OnCommand(40362)` 后新 active take 同键为空 `pext_kept=false`，Undo 后恢复 `probe`；故 glue 后重写 provenance，证据 `experiments/glue-pext.out.txt`，2026-10-06）。

## 8 Item bounds 与渲染

- `RENDER_BOUNDSFLAG=4` 即选中 media items（消费 `B_UISEL` 选择集）；`RENDER_SETTINGS &32/&64` 为 selected-items 开关，`&(4<<16)` 控制渲单文件还是逐项；`RENDER_STATS valuestr="42437"` 先 dry-run 再回统计。来源：v7.82 `GetSetProjectInfo[_String]` 原文，高；`&(4<<16)` 组合行为转 TSK（M7）。

## 9 ⚠️剩余实测（转 TASK-INDEX）

- M1 P_EXT 跨工程复制跟随（2026-10-06 人工验证通过：`manual-pext-read.out.txt pext_value=hello-manual`）；M3 ✅ 实测不带 NCH，复制后须显式回写（`takefx-copy-nch.out.txt nch_carried=false`，2026-10-06）；M6 ✅ 实测单 item glue 丢 take P_EXT，glue 后重写 provenance（`glue-pext.out.txt pext_kept=false`，2026-10-06）；M7 `BOUNDSFLAG=4+&(4<<16)` 文件清单。

## 附：来源与证据

- 签名：https://www.reaper.fm/sdk/reascript/reascripthelp.html （v7.82），2026-10-05。
- 版本：https://www.cockos.com/reaper/whatsnew.txt （v7.07/v7.60–v7.82），2026-10-05。
- 论坛：t-205780（dirty）、t=310092（pooled）、t=286031（schwa NCH）、t-263217（Mespotine P_EXT）、SWS #938、t=47729（native ID）。
- 实测：`experiments/a04-spike01-readonly.out.txt`（API 全 true）、`a04-spike02-midi-take.out.txt`（MIDI/P_EXT/split）、`a04-spike03c-t1.out.txt`（`T1_delta=1` auto-dirty）、`a04-spike03f-t3refetch.out.txt`（指针失效 + P_EXT 回滚）、`a04-spike04-nch-audio.out.txt`（NCH `2.0→8.0`）、`takefx-copy-nch.out.txt`（M3：复制带 FX 不带 NCH）、`glue-pext.out.txt`（M6：glue 丢 P_EXT，undo 恢复）。
