-- experiments/leftover-scan.lua (one-shot: count 60B MIDI items = pooled ghost residue)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "leftover-scan.out.txt"
local n = 0
for i = 0, reaper.CountMediaItems(0) - 1 do
  local tk = reaper.GetActiveTake(reaper.GetMediaItem(0, i))
  if tk and reaper.TakeIsMIDI(tk) then
    local _, buf = reaper.MIDI_GetAllEvts(tk, "")
    if buf and #buf == 60 then n = n + 1 end
  end
end
local f = io.open(out_path, "w")
if f then f:write("leftover_60B=" .. tostring(n) .. "\n") f:close() end
