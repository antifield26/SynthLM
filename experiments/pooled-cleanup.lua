-- experiments/pooled-cleanup.lua (delete SYNTHLM-POOLED track, report leftovers)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "pooled-cleanup.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
for i = reaper.CountTracks(0) - 1, 0, -1 do
  local tr = reaper.GetTrack(0, i)
  local _, name = reaper.GetSetMediaTrackInfo_String(tr, "P_NAME", "", false)
  if name == "SYNTHLM-POOLED" then
    reaper.DeleteTrack(tr)
    log("deleted_fixture_track=1")
  end
end
local left = 0
for i = 0, reaper.CountMediaItems(0) - 1 do
  local it = reaper.GetMediaItem(0, i)
  local tk = reaper.GetActiveTake(it)
  if tk and reaper.TakeIsMIDI(tk) then
    local _, buf = reaper.MIDI_GetAllEvts(tk, "")
    if buf and #buf == 60 then
      left = left + 1
      log(string.format("leftover|pos=%.1f", reaper.GetMediaItemInfo_Value(it, "D_POSITION")))
    end
  end
end
log("leftover_60B_items=" .. tostring(left))
local f = io.open(out_path, "w")
if f then f:write(table.concat(lines, "\n") .. "\n") f:close() end
