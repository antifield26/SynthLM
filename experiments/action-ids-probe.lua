-- experiments/action-ids-probe.lua (TSK-109: glue IDs no-op probe on empty scratch)
-- 只触发 40362/40601（选中零 item），记录 undo/计数差值；渲染/冻结 ID 不点火。
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "action-ids-probe.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
log("tracks0=" .. tostring(reaper.CountTracks(0)) .. " items0=" .. tostring(reaper.CountMediaItems(0)))
for _, id in ipairs({40362, 40601}) do
  local u0 = reaper.Undo_GetNumEntries()
  local it0 = reaper.CountMediaItems(0)
  reaper.Main_OnCommand(id, 0)
  log(string.format("cmd%d|undo_delta=%d items_delta=%d", id, reaper.Undo_GetNumEntries() - u0, reaper.CountMediaItems(0) - it0))
  flush()
end
reaper.DeleteTrack(tr)
log("tracks_after=" .. tostring(reaper.CountTracks(0)) .. " (expect " .. tostring(ntr0) .. ")")
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
