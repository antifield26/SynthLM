-- experiments/ext-smoke-baseline.lua (TSK-114: pre/post ping project-state snapshot)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "ext-smoke-baseline.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
log("tracks=" .. tostring(reaper.CountTracks(0)))
log("items=" .. tostring(reaper.CountMediaItems(0)))
log("undo_entries=" .. tostring(reaper.Undo_GetNumEntries and reaper.Undo_GetNumEntries() or "n/a"))
local f = io.open(out_path, "w")
if f then f:write(table.concat(lines, "\n") .. "\n") f:close() end
