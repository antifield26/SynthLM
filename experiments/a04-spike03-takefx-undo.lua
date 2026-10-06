-- experiments/a04-spike03-takefx-undo.lua (robust)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "a04-spike03-takefx-undo.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function run()
  reaper.PreventUIRefresh(1)
  local ntr0 = reaper.CountTracks(0)
  reaper.InsertTrackAtIndex(ntr0, false)
  local tr = reaper.GetTrack(0, ntr0)
  log("tr_ok=" .. tostring(tr ~= nil))
  local item = reaper.CreateNewMIDIItemInProj(tr, 0, 2, false)
  log("item_ok=" .. tostring(item ~= nil))
  local take = reaper.GetActiveTake(item)
  reaper.MIDI_InsertNote(take, true, false, 0, 480, 0, 60, 100, false)
  reaper.MIDI_Sort(take)
  reaper.MarkTrackItemsDirty(tr, item)
  -- T1
  local u0 = reaper.Undo_GetNumEntries()
  reaper.Undo_BeginBlock2(0)
  reaper.MIDI_SetNote(take, 0, nil, nil, nil, nil, nil, nil, 77, false)
  reaper.MIDI_Sort(take)
  reaper.Undo_EndBlock2(0, "SYNTHLM spike vel-no-dirty", -1)
  local u1 = reaper.Undo_GetNumEntries()
  log("T1_vel_no_dirty_delta=" .. tostring(u1-u0))
  local r1 = {reaper.MIDI_GetNote(take, 0)}
  log("T1_vel_after=" .. tostring(r1[8]))
  reaper.Undo_DoUndo2(0)
  local r2 = {reaper.MIDI_GetNote(take, 0)}
  log("T1_vel_after_undo=" .. tostring(r2[8]))
  reaper.Undo_DoRedo2(0)
  local r3 = {reaper.MIDI_GetNote(take, 0)}
  log("T1_vel_after_redo=" .. tostring(r3[8]))
  flush()
  -- T2
  local fxidx = reaper.TakeFX_AddByName(take, "ReaEQ", 0)
  log("T2_fxidx=" .. tostring(fxidx))
  local nch0 = reaper.GetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH")
  log("T2_nch_before=" .. tostring(nch0))
  reaper.SetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH", 8)
  local nch1 = reaper.GetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH")
  log("T2_nch_after8=" .. tostring(nch1))
  local okc, chunk1 = reaper.GetItemStateChunk(item, "", false)
  log("T2_chunk_ok=" .. tostring(okc) .. " hasNCH=" .. tostring(chunk1 and chunk1:find("TAKEFX_NCH") ~= nil))
  flush()
  -- T3
  reaper.Undo_BeginBlock2(0)
  reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "v1", true)
  reaper.Undo_EndBlock2(0, "SYNTHLM spike pext", -1)
  local _, pv1 = reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "", false)
  reaper.Undo_DoUndo2(0)
  local _, pv2 = reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_UNDO", "", false)
  log("T3_pext_v1=" .. tostring(pv1) .. " after_undo=" .. tostring(pv2))
  reaper.Undo_DoRedo2(0)
  flush()
  -- T4 action existence via Main_OnCommand lookup: use APIExists on NamedCommandLookup + kbd_getTextFromCmd
  log("T4_NamedCommandLookup_exists=" .. tostring(reaper.APIExists("NamedCommandLookup")))
  if reaper.APIExists("kbd_getTextFromCmd") then
    for _, id in ipairs({40362, 40601, 41824, 42230}) do
      local _, txt = reaper.kbd_getTextFromCmd(id, 0)
      log("T4_cmd_" .. tostring(id) .. "=" .. tostring(txt))
    end
  else
    log("T4_kbd_getTextFromCmd_missing")
  end
  log("T4_midieditor_active_nil=" .. tostring(reaper.MIDIEditor_GetActive() == nil))
  reaper.DeleteTrack(tr)
  log("tracks_after=" .. tostring(reaper.CountTracks(0)))
  reaper.PreventUIRefresh(-1)
  reaper.UpdateArrange()
end
local ok, err = xpcall(run, debug.traceback)
if not ok then log("FATAL=" .. tostring(err)) end
flush()
reaper.ShowConsoleMsg("A04 spike03 done ok=" .. tostring(ok) .. "\n")
