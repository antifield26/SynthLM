-- experiments/container-probe.lua (diagnose CopyToTrack-into-container addressing)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "container-probe.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function cfg(tr, fx, key)
  local ok, v = reaper.TrackFX_GetNamedConfigParm(tr, fx, key)
  return ok, v
end
reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
reaper.TrackFX_AddByName(tr, "ReaEQ", false, -1)
local cidx = reaper.TrackFX_AddByName(tr, "Container", false, -1)
local count = reaper.TrackFX_GetCount(tr)
log("count=" .. tostring(count) .. " cidx=" .. tostring(cidx))
local okcc, cc = cfg(tr, cidx, "container_count")
log("container_count ok=" .. tostring(okcc) .. " v=" .. tostring(cc))
local dest = 0x2000000 + 1 * (count + 1) + (cidx + 1)
log("dest=" .. tostring(dest))
reaper.TrackFX_CopyToTrack(tr, 0, tr, dest, true)
local count2 = reaper.TrackFX_GetCount(tr)
log("count_after=" .. tostring(count2))
local okcc2, cc2 = cfg(tr, 0, "container_count")
log("top0_container_count ok=" .. tostring(okcc2) .. " v=" .. tostring(cc2))
for f = 0, count2 - 1 do
  local g = reaper.TrackFX_GetFXGUID(tr, f)
  local _, nm = reaper.TrackFX_GetFXName(tr, f)
  log(string.format("top|%d|guid=%s|name=%s", f, tostring(g and "Y" or "n"), tostring(nm)))
end
local okit, it0 = cfg(tr, 0, "container_item.0")
log("container_item.0 ok=" .. tostring(okit) .. " v=" .. tostring(it0))
if okit then
  local n = tonumber(it0)
  if n then
    local g = reaper.TrackFX_GetFXGUID(tr, n)
    local _, nm = reaper.TrackFX_GetFXName(tr, n)
    log("inner|guid=" .. tostring(g and "Y" or "n") .. "|name=" .. tostring(nm))
  end
end
-- also try: what does parent_container say for top fx?
local okp, pv = cfg(tr, 0, "parent_container")
log("top0_parent ok=" .. tostring(okp) .. " v=" .. tostring(pv))
reaper.DeleteTrack(tr)
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
