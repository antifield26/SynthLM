-- experiments/a04-spike05d-symmetric2.lua (TSK-110: stepwise + pcall)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "a04-spike05d-symmetric2.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function step(name, fn)
  log("STEP|" .. name .. "|enter")
  flush()
  local r = {pcall(fn)}
  local ok = r[1]
  log("STEP|" .. name .. "|ok=" .. tostring(ok) .. (ok and "" or " err=" .. tostring(r[2]):sub(1, 160)))
  flush()
  return ok
end
local function find_take_by_guid(guid)
  local n = reaper.CountMediaItems(0)
  for i = 0, n - 1 do
    local it = reaper.GetMediaItem(0, i)
    for t = 0, reaper.CountTakes(it) - 1 do
      local tk = reaper.GetTake(it, t)
      local _, g = reaper.GetSetMediaItemTakeInfo_String(tk, "GUID", "", false)
      if g == guid then return it, tk end
    end
  end
  return nil, nil
end
local function read_pext(tk)
  local _, v = reaper.GetSetMediaItemTakeInfo_String(tk, "P_EXT:SYNTHLM_SYM", "", false)
  return v
end
local G = {}
reaper.PreventUIRefresh(1)
step("setup", function()
  local ntr0 = reaper.CountTracks(0)
  G.ntr0 = ntr0
  G.u0 = reaper.Undo_GetNumEntries()
  reaper.InsertTrackAtIndex(ntr0, false)
  G.tr = reaper.GetTrack(0, ntr0)
  reaper.Undo_BeginBlock2(0)
  local item = reaper.CreateNewMIDIItemInProj(G.tr, 0, 2, false)
  local take = reaper.GetActiveTake(item)
  reaper.MIDI_InsertNote(take, true, false, 0, 480, 0, 60, 100, false)
  reaper.MIDI_Sort(take)
  reaper.MarkTrackItemsDirty(G.tr, item)
  reaper.Undo_EndBlock2(0, "SYNTHLM sym setup", -1)
  local _, guid = reaper.GetSetMediaItemTakeInfo_String(take, "GUID", "", false)
  G.guid = guid
  log("setup|delta=" .. tostring(reaper.Undo_GetNumEntries() - G.u0))
end)
step("pext-write", function()
  G.u1 = reaper.Undo_GetNumEntries()
  reaper.Undo_BeginBlock2(0)
  local it, tk = find_take_by_guid(G.guid)
  log("prefind=" .. tostring(tk ~= nil))
  reaper.GetSetMediaItemTakeInfo_String(tk, "P_EXT:SYNTHLM_SYM", "v1", true)
  reaper.Undo_EndBlock2(0, "SYNTHLM sym pext", -1)
  log("write|delta=" .. tostring(reaper.Undo_GetNumEntries() - G.u1))
end)
step("pext-verify", function()
  local _, tk = find_take_by_guid(G.guid)
  log("verify|found=" .. tostring(tk ~= nil) .. " pext=" .. tostring(tk and read_pext(tk) or "n/a"))
end)
step("undo-once", function()
  reaper.Undo_DoUndo2(0)
  local _, tk = find_take_by_guid(G.guid)
  local v = tk and read_pext(tk) or "n/a"
  log("undo|found=" .. tostring(tk ~= nil) .. " pext=" .. (v == "" and "<empty>" or tostring(v)))
end)
step("redo-once", function()
  reaper.Undo_DoRedo2(0)
  local _, tk = find_take_by_guid(G.guid)
  log("redo|found=" .. tostring(tk ~= nil) .. " pext=" .. tostring(tk and read_pext(tk) or "n/a"))
end)
step("cleanup", function()
  reaper.Undo_DoUndo2(0)
  reaper.Undo_DoUndo2(0)
  local _, tk = find_take_by_guid(G.guid)
  log("cleanup|item_gone=" .. tostring(tk == nil))
  local tr2 = reaper.GetTrack(0, G.ntr0)
  if tr2 then reaper.DeleteTrack(tr2) log("cleanup|track_deleted=1") end
end)
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
