-- experiments/manual-pext-cleanup.lua (删除 SYNTHLM-MANUAL 夹具轨)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "manual-pext-cleanup.out.txt"
local found = 0
for i = reaper.CountTracks(0) - 1, 0, -1 do
  local tr = reaper.GetTrack(0, i)
  local _, name = reaper.GetSetMediaTrackInfo_String(tr, "P_NAME", "", false)
  if name == "SYNTHLM-MANUAL" then
    reaper.DeleteTrack(tr)
    found = found + 1
  end
end
local f = io.open(out_path, "w")
if f then f:write("deleted=" .. tostring(found) .. "\n") f:close() end
