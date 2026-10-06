-- experiments/perf-budget.lua (TSK-403: 500-param block + undo latency)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "perf-budget.out.txt"
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
local fx = reaper.TrackFX_AddByName(tr, "VST3:Pro-Q 4", false, -1000)
local n = reaper.TrackFX_GetNumParams(tr, fx)
log("fx_idx=" .. tostring(fx) .. " numparams=" .. tostring(n))
local N = math.min(500, n)
-- single undo block, 500 normalized writes
local t0 = os.clock()
reaper.Undo_BeginBlock2(0)
for i = 0, N - 1 do
  reaper.TrackFX_SetParamNormalized(tr, fx, i, 0.5)
end
reaper.Undo_EndBlock2(0, "SYNTHLM perf 500", -1)
local t1 = os.clock()
log(string.format("write500_ms=%.1f", (t1 - t0) * 1000))
flush()
local u0 = os.clock()
reaper.Undo_DoUndo2(0)
local u1 = os.clock()
log(string.format("undo_ms=%.1f", (u1 - u0) * 1000))
local r0 = os.clock()
reaper.Undo_DoRedo2(0)
local r1 = os.clock()
log(string.format("redo_ms=%.1f", (r1 - r0) * 1000))
flush()
reaper.DeleteTrack(tr)
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
