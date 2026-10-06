-- experiments/pooled-verify.lua (TSK-401 pooled leg: shared-source proof + derive-write isolation)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "pooled-verify.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function take_buf(tk)
  local _, b = reaper.MIDI_GetAllEvts(tk, "")
  return b
end
-- collect all MIDI takes in project
local takes = {}
for i = 0, reaper.CountMediaItems(0) - 1 do
  local it = reaper.GetMediaItem(0, i)
  local pos = reaper.GetMediaItemInfo_Value(it, "D_POSITION")
  for t = 0, reaper.CountTakes(it) - 1 do
    local tk = reaper.GetTake(it, t)
    if reaper.TakeIsMIDI(tk) then
      takes[#takes + 1] = {item = it, take = tk, pos = pos, buf = take_buf(tk)}
    end
  end
end
log("midi_takes=" .. tostring(#takes))
for i, e in ipairs(takes) do
  log(string.format("take%d|pos=%.1f|buflen=%d", i, e.pos, e.buf and #e.buf or -1))
end
flush()
-- group by identical buffer
local groups = {}
for i, e in ipairs(takes) do
  local k = e.buf or ("nil" .. tostring(i))
  groups[k] = groups[k] or {}
  groups[k][#groups[k] + 1] = i
end
local pooled = false
for _, idx in pairs(groups) do
  if #idx >= 2 then
    pooled = true
    log("shared_buffer|takes=" .. table.concat(idx, ","))
  end
end
log("pooled_candidate=" .. tostring(pooled))
flush()
if pooled then
  -- propagation proof: modify first take of the shared group, check the other moves too
  local pair = nil
  for _, idx in pairs(groups) do if #idx >= 2 then pair = idx break end end
  local A, B = takes[pair[1]], takes[pair[2]]
  local _, _, _, _, _, _, _, v0 = reaper.MIDI_GetNote(A.take, 0)
  reaper.Undo_BeginBlock2(0)
  reaper.MIDI_SetNote(A.take, 0, nil, nil, nil, nil, nil, nil, (v0 % 100) + 1, false)
  reaper.MIDI_Sort(A.take)
  reaper.MarkTrackItemsDirty(reaper.GetMediaItemTake_Track(A.take), A.item)
  reaper.Undo_EndBlock2(0, "SYNTHLM pooled probe", -1)
  local bA = take_buf(A.take)
  local bB = take_buf(B.take)
  log("propagate|both_changed=" .. tostring(bA == bB and bA ~= A.buf))
  -- restore
  reaper.Undo_DoUndo2(0)
  local rA = take_buf(A.take)
  local _, tkx = nil, nil
  log("restore|A_back=" .. tostring(rA == A.buf))
  flush()
  -- derive-write: new item from A's notes + provenance, originals must not move
  local beforeA = take_buf(A.take)
  local beforeB = take_buf(B.take)
  reaper.Undo_BeginBlock2(0)
  local tr = reaper.GetMediaItemTake_Track(A.take)
  local posA = reaper.GetMediaItemInfo_Value(A.item, "D_POSITION")
  local newitem = reaper.CreateNewMIDIItemInProj(tr, posA + 4, posA + 6, false)
  local newtake = reaper.GetActiveTake(newitem)
  reaper.MIDI_SetAllEvts(newtake, beforeA)
  reaper.GetSetMediaItemTakeInfo_String(newtake, "P_EXT:SYNTHLM_PROV_OP", "pooled-derive", true)
  reaper.MarkTrackItemsDirty(tr, newitem)
  reaper.Undo_EndBlock2(0, "SYNTHLM pooled derive", -1)
  -- refetch originals by position (pointers may be stale after undo)
  local aftA, aftB = nil, nil
  for i = 0, reaper.CountMediaItems(0) - 1 do
    local it = reaper.GetMediaItem(0, i)
    local tk = reaper.GetActiveTake(it)
    if tk and reaper.TakeIsMIDI(tk) then
      local p = reaper.GetMediaItemInfo_Value(it, "D_POSITION")
      if math.abs(p - A.pos) < 0.001 then aftA = take_buf(tk) end
      if math.abs(p - B.pos) < 0.001 then aftB = take_buf(tk) end
    end
  end
  log("derive|origA_untouched=" .. tostring(aftA == beforeA))
  log("derive|origB_untouched=" .. tostring(aftB == beforeB))
  -- cleanup derived item only (keep ghost pair for user inspection)
  for i = reaper.CountMediaItems(0) - 1, 0, -1 do
    local it = reaper.GetMediaItem(0, i)
    local tk = reaper.GetActiveTake(it)
    if tk then
      local _, v = reaper.GetSetMediaItemTakeInfo_String(tk, "P_EXT:SYNTHLM_PROV_OP", "", false)
      if v == "pooled-derive" then
        reaper.DeleteTrackMediaItem(reaper.GetMediaItem_Track(it), it)
        log("derive|derived_item_removed=1")
        break
      end
    end
  end
end
flush()
reaper.UpdateArrange()
log("done=1")
flush()
