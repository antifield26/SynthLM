-- experiments/container-50op.lua (TSK-101 Lua acceptance: formula vs ground truth, 50 ops)
-- Ground truth = container_item.X walk (verified keys only).
-- Formula under test: addr = 0x2000000 + (p+1)*(count+1) + (c+1), 1-based.
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "container-50op.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local FLAG = 0x2000000
local function cfg(tr, fx, key)
  local ok, v = reaper.TrackFX_GetNamedConfigParm(tr, fx, key)
  if not ok then return nil end
  return tonumber(v)
end
local function formula(count, c0, p0) return FLAG + (p0 + 1) * (count + 1) + (c0 + 1) end
-- full enumeration: returns map guid -> {addr, top, path}
local function enumerate(tr)
  local map = {}
  local count = reaper.TrackFX_GetCount(tr)
  for f = 0, count - 1 do
    local g = reaper.TrackFX_GetFXGUID(tr, f)
    if g then map[g] = {addr = f, top = f, path = {}} end
    local k = cfg(tr, f, "container_count")
    if k then
      for j = 0, k - 1 do
        local a = cfg(tr, f, "container_item." .. j)
        if a then
          local g2 = reaper.TrackFX_GetFXGUID(tr, a)
          if g2 then map[g2] = {addr = a, top = f, path = {j}} end
        end
      end
    end
  end
  return map, count
end
-- deterministic LCG
local seed = 12345
local function rnd(n) seed = (seed * 1103515245 + 12345) % 2147483648 return seed % n end

reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
-- setup: ReaEQ top first, then Container appended, then move ReaEQ inside.
-- dest formula always uses the count FRESH at operation time.
reaper.TrackFX_AddByName(tr, "ReaEQ", false, -1)
local cidx = reaper.TrackFX_AddByName(tr, "Container", false, -1)
log("setup|container_idx=" .. tostring(cidx))
local count = reaper.TrackFX_GetCount(tr)
local dest = formula(count, cidx, 0)
reaper.TrackFX_CopyToTrack(tr, 0, tr, dest, true)
local gmap = enumerate(tr)
local tgt_guid = nil
for g, e in pairs(gmap) do
  if #e.path == 1 then
    local _, nm = reaper.TrackFX_GetFXName(tr, e.addr)
    if nm and nm:find("ReaEQ") then tgt_guid = g break end
  end
end
log("setup|target_guid=" .. tostring(tgt_guid and "found" or "MISSING"))
flush()
if not tgt_guid then
  log("ABORT|no-target")
  reaper.DeleteTrack(tr)
  reaper.PreventUIRefresh(-1)
  flush()
  return
end
local succ, total = 0, 50
for i = 1, total do
  local op = rnd(5)
  local ok = true
  local note = ""
  if op == 0 then
    reaper.TrackFX_AddByName(tr, "ReaEQ", false, -1)
    note = "add-top"
  elseif op == 1 then
    -- delete random top non-container FX
    local cnt = reaper.TrackFX_GetCount(tr)
    local cands = {}
    for f = 0, cnt - 1 do
      local cc = cfg(tr, f, "container_count")
      if not cc then
        local g = reaper.TrackFX_GetFXGUID(tr, f)
        if g ~= tgt_guid then cands[#cands + 1] = f end
      end
    end
    if #cands > 0 then
      reaper.TrackFX_Delete(tr, cands[rnd(#cands) + 1])
      note = "del-top"
    else note = "del-top-skip" end
  elseif op == 2 then
    -- move a top ReaEQ into container end
    local cnt = reaper.TrackFX_GetCount(tr)
    local src = nil
    for f = 0, cnt - 1 do
      if not cfg(tr, f, "container_count") then
        local _, nm = reaper.TrackFX_GetFXName(tr, f)
        if nm and nm:find("ReaEQ") then src = f break end
      end
    end
    if src then
      local cc = reaper.TrackFX_GetCount(tr)
      local kk = 0
      local cmap = enumerate(tr)
      -- find container top index by guid scan
      local ctop = nil
      for f = 0, cc - 1 do
        if cfg(tr, f, "container_count") then ctop = f break end
      end
      local inner = cfg(tr, ctop, "container_count") or 0
      reaper.TrackFX_CopyToTrack(tr, src, tr, formula(cc, ctop, inner), true)
      note = "move-in"
    else note = "move-in-skip" end
  elseif op == 3 then
    -- move random inner non-target FX out to top end
    local cmap = enumerate(tr)
    local cands = {}
    for g, e in pairs(cmap) do
      if #e.path == 1 and g ~= tgt_guid then cands[#cands + 1] = e.addr end
    end
    if #cands > 0 then
      local cnt = reaper.TrackFX_GetCount(tr)
      reaper.TrackFX_CopyToTrack(tr, cands[rnd(#cands) + 1], tr, cnt, true)
      note = "move-out"
    else note = "move-out-skip" end
  else
    reaper.TrackFX_AddByName(tr, "ReaEQ", false, -1)
    local cnt = reaper.TrackFX_GetCount(tr)
    reaper.TrackFX_Delete(tr, cnt - 1)
    note = "churn"
  end
  -- verify: ground truth + formula cross-check + param read
  local map2, cnt2 = enumerate(tr)
  local e = map2[tgt_guid]
  if not e then
    log(string.format("R|%d|%s|TARGET-LOST", i, note))
  else
    local faddr = formula(cnt2, e.top, e.path[1] or 0)
    local fok = (faddr == e.addr)
    local v1 = reaper.TrackFX_GetParam(tr, e.addr, 0)
    local v2 = fok and reaper.TrackFX_GetParam(tr, faddr, 0) or nil
    local pok = fok and (v1 == v2)
    if fok and pok then succ = succ + 1 end
    log(string.format("R|%d|%s|formula=%s param=%s", i, note, tostring(fok), tostring(pok)))
  end
  if i % 10 == 0 then flush() end
end
log("summary|success=" .. tostring(succ) .. "/" .. tostring(total))
reaper.DeleteTrack(tr)
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
