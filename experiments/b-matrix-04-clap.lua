-- experiments/b-matrix-04-clap.lua (TSK-112: CLAP cell + VST3/CLAP cross-format)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "b-matrix-04-clap.out.txt"
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

reaper.PreventUIRefresh(1)
for _, name in ipairs({"CLAP: Vital", "VST3: Vital"}) do
  local ntr = reaper.CountTracks(0)
  reaper.InsertTrackAtIndex(ntr, false)
  local tr = reaper.GetTrack(0, ntr)
  local fx = reaper.TrackFX_AddByName(tr, name, false, -1000)
  log("FX|" .. name .. "|idx=" .. tostring(fx))
  if fx and fx >= 0 then
    local n = reaper.TrackFX_GetNumParams(tr, fx)
    log("FX|" .. name .. "|numparams=" .. tostring(n))
    local _, fxname = reaper.TrackFX_GetFXName(tr, fx)
    log("FX|" .. name .. "|fxname=" .. tostring(fxname))
    for i = 0, math.min(n - 1, 9) do
      log(string.format("P|%s|%d|name=%s", name, i, q(reaper.TrackFX_GetParamName, tr, fx, i)))
      log(string.format("P|%s|%d|fmt=%s", name, i, q(reaper.TrackFX_GetFormattedParamValue, tr, fx, i)))
      log(string.format("P|%s|%d|step=%s", name, i, q(reaper.TrackFX_GetParameterStepSizes, tr, fx, i)))
      log(string.format("P|%s|%d|ident=%s", name, i, q(reaper.TrackFX_GetParamIdent, tr, fx, i)))
      log(string.format("P|%s|%d|sec=%s", name, i, q(reaper.TrackFX_GetParamSectionName, tr, fx, i)))
      log(string.format("P|%s|%d|auto=%s", name, i, q(reaper.TrackFX_GetNamedConfigParm, tr, fx, "param."..i..".automatable")))
      log(string.format("P|%s|%d|def=%s", name, i, q(reaper.TrackFX_GetNamedConfigParm, tr, fx, "param."..i..".default_value")))
    end
    log(string.format("P|%s|wet=%s", name, tostring(reaper.TrackFX_GetParamFromIdent(tr, fx, ":wet"))))
    -- macro hunt: first 3 params whose name matches macro
    local found = 0
    for i = 0, n - 1 do
      if found >= 3 then break end
      local ok, nm = reaper.TrackFX_GetParamName(tr, fx, i)
      if ok and nm and nm:lower():find("macro") then
        log(string.format("MACRO|%s|%d|%s", name, i, nm))
        found = found + 1
      end
      if i > 600 then break end
    end
    log("MACRODONE|" .. name .. "|found=" .. tostring(found))
  end
  flush()
  reaper.DeleteTrack(tr)
end
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
