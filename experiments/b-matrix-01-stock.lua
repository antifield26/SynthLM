-- experiments/b-matrix-01-stock.lua (TSK-112 subset: stock plugins only)
-- 20-param enumeration per B-plugin-semantics §5.2, exact Lua arities, pcall-guarded.
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "b-matrix-01-stock.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function q(fn, ...)
  local r = {pcall(fn, ...)}
  if not r[1] then return "ERR:" .. tostring(r[2]) end
  table.remove(r, 1)
  local parts = {}
  for i, v in ipairs(r) do parts[#parts+1] = tostring(v) end
  return table.concat(parts, "|")
end

local targets = {"ReaEQ", "ReaControlMIDI", "ReaSamplOmatic5000", "JS: General Dynamics"}
reaper.PreventUIRefresh(1)
for _, name in ipairs(targets) do
  local ntr = reaper.CountTracks(0)
  reaper.InsertTrackAtIndex(ntr, false)
  local tr = reaper.GetTrack(0, ntr)
  local fx = reaper.TrackFX_AddByName(tr, name, false, -1000)
  log("FX|" .. name .. "|idx=" .. tostring(fx))
  if fx and fx >= 0 then
    local n = reaper.TrackFX_GetNumParams(tr, fx)
    log("FX|" .. name .. "|numparams=" .. tostring(n))
    for i = 0, math.min(n - 1, 19) do
      log(string.format("P|%s|%d|name=%s", name, i, q(reaper.TrackFX_GetParamName, tr, fx, i)))
      log(string.format("P|%s|%d|val=%s", name, i, q(reaper.TrackFX_GetParam, tr, fx, i)))
      log(string.format("P|%s|%d|ex=%s", name, i, q(reaper.TrackFX_GetParamEx, tr, fx, i)))
      log(string.format("P|%s|%d|fmt=%s", name, i, q(reaper.TrackFX_GetFormattedParamValue, tr, fx, i)))
      log(string.format("P|%s|%d|step=%s", name, i, q(reaper.TrackFX_GetParameterStepSizes, tr, fx, i)))
      log(string.format("P|%s|%d|ident=%s", name, i, q(reaper.TrackFX_GetParamIdent, tr, fx, i)))
      log(string.format("P|%s|%d|sec=%s", name, i, q(reaper.TrackFX_GetParamSectionName, tr, fx, i)))
      log(string.format("P|%s|%d|auto=%s", name, i, q(reaper.TrackFX_GetNamedConfigParm, tr, fx, "param."..i..".automatable")))
      log(string.format("P|%s|%d|def=%s", name, i, q(reaper.TrackFX_GetNamedConfigParm, tr, fx, "param."..i..".default_value")))
    end
    log(string.format("P|%s|fmtval_reaeq_p0=%s", name, q(reaper.TrackFX_FormatParamValue, tr, fx, 0, 0.5)))
  end
  flush()
  reaper.DeleteTrack(tr)
end
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
reaper.ShowConsoleMsg("b-matrix-01 done\n")
