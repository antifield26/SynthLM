-- experiments/fault-inject-401.lua (TSK-401: container-inner + takeFX legs, 50 cycles each)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "fault-inject-401.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function cfg(tr, fx, key)
  local ok, v = reaper.TrackFX_GetNamedConfigParm(tr, fx, key)
  if not ok then return nil end
  return tonumber(v)
end
local FLAG = 0x2000000
reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
-- fixture A: container + inner ReaEQ (target param 0)
reaper.TrackFX_AddByName(tr, "ReaEQ", false, -1)
reaper.TrackFX_AddByName(tr, "Container", false, -1)
local cnt = reaper.TrackFX_GetCount(tr)
reaper.TrackFX_CopyToTrack(tr, 0, tr, FLAG + 1 * (cnt + 1) + 2, true)
-- fixture B: MIDI item + take + takeFX + NCH
local item = reaper.CreateNewMIDIItemInProj(tr, 0, 2, false)
local take = reaper.GetActiveTake(item)
reaper.MIDI_InsertNote(take, true, false, 0, 480, 0, 60, 100, false)
reaper.MIDI_Sort(take)
local tfx = reaper.TakeFX_AddByName(take, "ReaEQ", -1000)
reaper.SetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH", 8)
reaper.MarkTrackItemsDirty(tr, item)
local function enum_inner(tr)
  local map, n2 = {}, reaper.TrackFX_GetCount(tr)
  for f = 0, n2 - 1 do
    if cfg(tr, f, "container_count") then
      local k = cfg(tr, f, "container_count")
      for j = 0, k - 1 do
        local a = cfg(tr, f, "container_item." .. j)
        if a then
          local g = reaper.TrackFX_GetFXGUID(tr, a)
          if g then map[g] = a end
        end
      end
    end
  end
  return map
end
local function first_reaeq(tr, map)
  for g, a in pairs(map) do
    local _, nm = reaper.TrackFX_GetFXName(tr, a)
    if nm and nm:find("ReaEQ") then return g, a end
  end
  return nil, nil
end
local badA, badB = 0, 0
-- leg A: 50x container-inner param write + undo-verify + redo
for i = 1, 50 do
  local v = (i % 100) / 100
  local map = enum_inner(tr)
  local g0, a0 = first_reaeq(tr, map)
  if not a0 then badA = badA + 1 log("A|" .. tostring(i) .. "|no-target") else
    local v0 = reaper.TrackFX_GetParam(tr, a0, 0)
    reaper.Undo_BeginBlock2(0)
    local mapw = enum_inner(tr)
    local _, aw = first_reaeq(tr, mapw)
    reaper.TrackFX_SetParamNormalized(tr, aw, 0, v)
    reaper.Undo_EndBlock2(0, "SYNTHLM fi401a " .. tostring(i), -1)
    reaper.Undo_DoUndo2(0)
    local mapu = enum_inner(tr)
    local _, au = first_reaeq(tr, mapu)
    local vu = au and reaper.TrackFX_GetParam(tr, au, 0) or nil
    if vu ~= v0 then badA = badA + 1 log("A|" .. tostring(i) .. "|undo-mismatch") end
    reaper.Undo_DoRedo2(0)
  end
  if i % 25 == 0 then log("A|progress|" .. tostring(i) .. "|bad=" .. tostring(badA)) flush() end
end
-- leg B: 50x takeFX param + NCH rewrite + undo-verify
for i = 1, 50 do
  local v = 0.3 + (i % 40) / 100
  local it0 = reaper.GetMediaItem(0, 0)
  local tk0 = it0 and reaper.GetActiveTake(it0) or nil
  local p0 = tk0 and reaper.TakeFX_GetParam(tk0, 0, 0) or nil
  reaper.Undo_BeginBlock2(0)
  local it = reaper.GetMediaItem(0, 0)
  local tk = it and reaper.GetActiveTake(it) or nil
  if tk then
    reaper.TakeFX_SetParamNormalized(tk, 0, 0, v)
    reaper.SetMediaItemTakeInfo_Value(tk, "I_TAKEFX_NCH", 8)
    reaper.MarkTrackItemsDirty(tr, it)
  end
  reaper.Undo_EndBlock2(0, "SYNTHLM fi401b " .. tostring(i), -1)
  reaper.Undo_DoUndo2(0)
  local it2 = reaper.GetMediaItem(0, 0)
  local tk2 = it2 and reaper.GetActiveTake(it2) or nil
  if not tk2 then
    badB = badB + 1 log("B|" .. tostring(i) .. "|take-lost")
  else
    local p2 = reaper.TakeFX_GetParam(tk2, 0, 0)
    if p2 ~= p0 then badB = badB + 1 log("B|" .. tostring(i) .. "|undo-mismatch") end
  end
  reaper.Undo_DoRedo2(0)
  if i % 25 == 0 then log("B|progress|" .. tostring(i) .. "|bad=" .. tostring(badB)) flush() end
end
log("summary|badA=" .. tostring(badA) .. "/50 badB=" .. tostring(badB) .. "/50")
reaper.DeleteTrack(tr)
log("tracks_after=" .. tostring(reaper.CountTracks(0)) .. " expect=" .. tostring(ntr0))
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
