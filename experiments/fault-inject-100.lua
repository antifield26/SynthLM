-- experiments/fault-inject-100.lua (TSK-103 Lua part: 100 apply/rollback cycles)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "fault-inject-100.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function find_take(tr, idx)
  local it = reaper.GetMediaItem(0, idx)
  if not it then return nil, nil end
  return it, reaper.GetActiveTake(it)
end
local function read_all(tk)
  local _, pv = reaper.GetSetMediaItemTakeInfo_String(tk, "P_EXT:SYNTHLM_FI", "", false)
  local r = {reaper.MIDI_GetNote(tk, 0)}
  return pv, r[8]
end
reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
reaper.Undo_BeginBlock2(0)
local item = reaper.CreateNewMIDIItemInProj(tr, 0, 4, false)
local take = reaper.GetActiveTake(item)
reaper.MIDI_InsertNote(take, true, false, 0, 480, 0, 60, 100, false)
reaper.MIDI_Sort(take)
reaper.MarkTrackItemsDirty(tr, item)
reaper.Undo_EndBlock2(0, "SYNTHLM fi setup", -1)
local bad = 0
for i = 1, 100 do
  local vel = 40 + (i % 60)
  local val = "c" .. tostring(i)
  reaper.Undo_BeginBlock2(0)
  local _, tk = find_take(tr, 0)
  reaper.GetSetMediaItemTakeInfo_String(tk, "P_EXT:SYNTHLM_FI", val, true)
  reaper.MIDI_SetNote(tk, 0, nil, nil, nil, nil, nil, nil, vel, false)
  reaper.MIDI_Sort(tk)
  reaper.MarkTrackItemsDirty(tr, reaper.GetMediaItem(0, 0))
  reaper.Undo_EndBlock2(0, "SYNTHLM fi apply " .. tostring(i), -1)
  local _, tk2 = find_take(tr, 0)
  local pv, vv = read_all(tk2)
  if pv ~= val or vv ~= vel then bad = bad + 1 log("C|" .. tostring(i) .. "|apply-mismatch") end
  reaper.Undo_DoUndo2(0)
  local _, tk3 = find_take(tr, 0)
  if tk3 then
    local pv3, _ = read_all(tk3)
    local exp_prev = (i == 1) and "" or ("c" .. tostring(i - 1))
    local exp_vel = (i == 1) and 100 or (40 + ((i - 1) % 60))
    local _, vv3 = read_all(tk3)
    if pv3 ~= exp_prev or vv3 ~= exp_vel then bad = bad + 1 log("C|" .. tostring(i) .. "|undo-mismatch") end
  else bad = bad + 1 log("C|" .. tostring(i) .. "|undo-lost") end
  reaper.Undo_DoRedo2(0)
  local _, tk4 = find_take(tr, 0)
  if tk4 then
    local pv4, vv4 = read_all(tk4)
    if pv4 ~= val or vv4 ~= vel then bad = bad + 1 log("C|" .. tostring(i) .. "|redo-mismatch") end
  else bad = bad + 1 log("C|" .. tostring(i) .. "|redo-lost") end
  if i % 25 == 0 then log("progress|" .. tostring(i) .. "|bad=" .. tostring(bad)) flush() end
end
log("summary|bad=" .. tostring(bad) .. "/100")
reaper.DeleteTrack(tr)
log("tracks_after=" .. tostring(reaper.CountTracks(0)) .. " expect=" .. tostring(ntr0))
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
