-- experiments/a04-spike03d-t2t3.lua
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "a04-spike03d-t2t3.out.txt"
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
reaper.MIDI_InsertNote(take, true, false, 0, 480, 0, 60, 100, false)
reaper.MIDI_Sort(take)
reaper.MarkTrackItemsDirty(tr, item)
-- T2
local fxidx = reaper.TakeFX_AddByName(take, "ReaEQ", 0)
log("T2_fxidx=" .. tostring(fxidx))
local nch0 = reaper.GetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH")
log("T2_nch_before=" .. tostring(nch0))
reaper.SetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH", 8)
local nch1 = reaper.GetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH")
log("T2_nch_after8=" .. tostring(nch1))
local okc, chunk1 = reaper.GetItemStateChunk(item, "", false)
log("T2_chunk_ok=" .. tostring(okc))
log("T2_hasNCH=" .. tostring(chunk1 and chunk1:find("TAKEFX_NCH") ~= nil))
if chunk1 then
  for line in chunk1:gmatch("[^\n]*TAKEFX[^\n]*") do log("T2_chunkline=" .. line) end
end
flush()
-- T2b: try audio take? create empty audio item via AddMediaItemToTrack + AddTakeToMediaItem with source?
-- T3 P_EXT undo
reaper.Undo_BeginBlock2(0)
reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "v1", true)
reaper.Undo_EndBlock2(0, "SYNTHLM spike pext", -1)
local _, pv1 = reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "", false)
log("T3_before_undo=" .. tostring(pv1))
reaper.Undo_DoUndo2(0)
local _, pv2 = reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "", false)
log("T3_after_undo=" .. tostring(pv2 == "" and "<empty>" or pv2))
reaper.Undo_DoRedo2(0)
local _, pv3 = reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "", false)
log("T3_after_redo=" .. tostring(pv3))
-- T4 cmd text
if reaper.APIExists("kbd_getTextFromCmd") then
  for _, id in ipairs({40362, 40601, 41824, 42230}) do
    local _, txt = reaper.kbd_getTextFromCmd(id, 0)
    log("T4_" .. tostring(id) .. "=" .. tostring(txt))
  end
end
log("T4_editor_nil=" .. tostring(reaper.MIDIEditor_GetActive() == nil))
flush()
reaper.DeleteTrack(tr)
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
flush()
reaper.ShowConsoleMsg("t2t3 done\n")
