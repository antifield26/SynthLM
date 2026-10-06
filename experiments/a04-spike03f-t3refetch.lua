-- experiments/a04-spike03f-t3refetch.lua
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "a04-spike03f-t3refetch.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
local item = reaper.CreateNewMIDIItemInProj(tr, 0, 2, false)
local take = reaper.GetActiveTake(item)
reaper.Undo_BeginBlock2(0)
reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "v1", true)
reaper.Undo_EndBlock2(0, "SYNTHLM spike pext", -1)
local _, pv1 = reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "", false)
log("T3_before=" .. tostring(pv1)) flush()
log("validate_before=" .. tostring(reaper.ValidatePtr2(0, take, "MediaItem_Take*"))) flush()
reaper.Undo_DoUndo2(0)
log("undo_done=1") flush()
log("validate_after_undo_take=" .. tostring(reaper.ValidatePtr2(0, take, "MediaItem_Take*"))) flush()
log("validate_after_undo_item=" .. tostring(reaper.ValidatePtr2(0, item, "MediaItem*"))) flush()
-- refetch
local n = reaper.CountMediaItems(0)
log("count_items=" .. tostring(n)) flush()
if n > 0 then
  local it2 = reaper.GetMediaItem(0, 0)
  local tk2 = reaper.GetActiveTake(it2)
  local _, pv2 = reaper.GetSetMediaItemTakeInfo_String(tk2, "P_EXT:SYNTHLM_UNDO", "", false)
  log("T3_after_undo_refetch=" .. tostring(pv2 == "" and "<empty>" or pv2)) flush()
end
reaper.Undo_DoRedo2(0)
log("redo_done=1") flush()
local n2 = reaper.CountMediaItems(0)
log("count_items2=" .. tostring(n2)) flush()
if n2 > 0 then
  local it3 = reaper.GetMediaItem(0, 0)
  local tk3 = reaper.GetActiveTake(it3)
  local _, pv3 = reaper.GetSetMediaItemTakeInfo_String(tk3, "P_EXT:SYNTHLM_UNDO", "", false)
  log("T3_after_redo_refetch=" .. tostring(pv3)) flush()
end
local _, txt = reaper.kbd_getTextFromCmd(40362, 0)
log("T4_40362=" .. tostring(txt)) flush()
local _, txt2 = reaper.kbd_getTextFromCmd(40601, 0)
log("T4_40601=" .. tostring(txt2)) flush()
-- refetch track before delete (track ptr may be stale after undo/redo)
local tr2 = reaper.GetTrack(0, ntr0)
if tr2 then reaper.DeleteTrack(tr2) log("deleted_via_refetch=1") else log("track_gone=1") end
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1") flush()
