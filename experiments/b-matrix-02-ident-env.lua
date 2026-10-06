-- experiments/b-matrix-02-ident-env.lua (TSK-112 subset: FromIdent + envelope existence)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "b-matrix-02-ident-env.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
reaper.PreventUIRefresh(1)
local ntr = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr, false)
local tr = reaper.GetTrack(0, ntr)
for _, spec in ipairs({{"ReaEQ", 0}, {"JS: General Dynamics", 0}}) do
  local name = spec[1]
  local fx = reaper.TrackFX_AddByName(tr, name, false, -1000)
  log("FX|" .. name .. "|idx=" .. tostring(fx))
  if fx and fx >= 0 then
    for _, ident in ipairs({":wet", ":bypass", ":delta"}) do
      log("FROMIDENT|" .. name .. "|" .. ident .. "=" .. tostring(reaper.TrackFX_GetParamFromIdent(tr, fx, ident)))
    end
    local _, id0 = reaper.TrackFX_GetParamIdent(tr, fx, 0)
    log("IDENT0|" .. name .. "=" .. tostring(id0))
    if id0 then
      log("ROUNDTRIP|" .. name .. "=" .. tostring(reaper.TrackFX_GetParamFromIdent(tr, fx, id0)))
    end
    log("BOGUS|" .. name .. "=" .. tostring(reaper.TrackFX_GetParamFromIdent(tr, fx, ":nope_xyz")))
    local n = reaper.TrackFX_GetNumParams(tr, fx)
    local nilc, created = 0, 0
    for i = 0, n - 1 do
      local env = reaper.GetFXEnvelope(tr, fx, i, false)
      if env then created = created + 1 else nilc = nilc + 1 end
    end
    log("ENV|" .. name .. "|n=" .. tostring(n) .. " existing=" .. tostring(created) .. " nil=" .. tostring(nilc))
    log("FORMATNORM|" .. name .. "=" .. tostring(reaper.TrackFX_FormatParamValueNormalized(tr, fx, 0, 0.5)))
  end
  flush()
end
reaper.DeleteTrack(tr)
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
