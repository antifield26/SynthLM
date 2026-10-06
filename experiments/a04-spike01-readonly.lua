-- experiments/a04-spike01-readonly.lua
-- 目的: A04 只读核验 (不修改工程),验证 v7.82 API 存在性与版本基线
-- 运行: "C:\Program Files\REAPER (x64)\reaper.exe" -nonewinst experiments/a04-spike01-readonly.lua
-- 输出: 同目录 a04-spike01-readonly.out.txt
local out_path = debug.getinfo(1, "S").source:match("@?(.*[/\\])") .. "a04-spike01-readonly.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end

log("version=" .. tostring(reaper.GetAppVersion()))
local apis = {
  "CreateNewMIDIItemInProj",
  "MIDI_GetAllEvts", "MIDI_SetAllEvts", "MIDI_GetNote", "MIDI_SetNote",
  "MIDI_InsertNote", "MIDI_DisableSort", "MIDI_Sort", "MIDI_CountEvts",
  "MIDI_GetHash", "MIDI_GetTrackHash", "MIDI_RefreshEditors",
  "MIDIEditor_GetActive", "MIDIEditor_GetTake", "MIDIEditor_OnCommand",
  "TakeIsMIDI", "GetActiveTake", "GetTake", "GetMediaItemTakeByGUID",
  "GetSetMediaItemTakeInfo_String", "GetSetMediaItemInfo_String",
  "GetSetMediaTrackInfo_String", "GetMediaItemInfo_Value", "SetMediaItemInfo_Value",
  "GetMediaItemTakeInfo_Value", "SetMediaItemTakeInfo_Value",
  "SplitMediaItem", "AddTakeToMediaItem", "SetActiveTake",
  "TakeFX_AddByName", "TakeFX_CopyToTake", "TakeFX_Delete",
  "MarkTrackItemsDirty", "Undo_BeginBlock2", "Undo_EndBlock2",
  "Undo_GetNumEntries", "MIDIEditor_GetSetting_int",
  "GetSetProjectInfo_String", "Main_OnCommand",
}
for _, a in ipairs(apis) do
  log("APIExists(" .. a .. ")=" .. tostring(reaper.APIExists(a)))
end
-- I_MIXFLAG / I_TAKEFX_NCH / fade 键探测:对当前工程第一条轨/第一个 item 做只读 Get(不写)
local tr = reaper.GetTrack(0, 0)
if tr then
  log("track0_exists=true")
else
  log("track0_exists=false")
end
local n = reaper.CountMediaItems(0)
log("count_media_items=" .. tostring(n))
if n > 0 then
  local it = reaper.GetMediaItem(0, 0)
  local ok, v = pcall(reaper.GetMediaItemInfo_Value, it, "I_MIXFLAG")
  log("get_I_MIXFLAG_ok=" .. tostring(ok) .. " val=" .. tostring(v))
else
  log("get_I_MIXFLAG_ok=skipped_no_items")
end

local f = io.open(out_path, "w")
f:write(table.concat(lines, "\n") .. "\n")
f:close()
reaper.ShowConsoleMsg("A04 spike01 done -> " .. out_path .. "\n")
