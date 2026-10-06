-- experiments/render-line-01.lua (TSK-104: backup RENDER_* -> 3x determinism -> M7 -> restore)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local DIR = base
local out_path = base .. "render-line-01.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function fnv1a(path)
  local f = io.open(path, "rb")
  if not f then return nil end
  local h = 0x811C9DC5
  while true do
    local b = f:read(4096)
    if not b then break end
    for i = 1, #b do h = ((h ~ b:byte(i)) * 0x01000193) & 0xFFFFFFFF end
  end
  f:close()
  return string.format("%08x", h)
end
local function fsize(path)
  local f = io.open(path, "rb")
  if not f then return -1 end
  local n = f:seek("end")
  f:close()
  return n
end
local NUMKEYS = {"RENDER_SETTINGS","RENDER_BOUNDSFLAG","RENDER_CHANNELS","RENDER_SRATE","RENDER_STARTPOS","RENDER_ENDPOS","RENDER_TAILFLAG","RENDER_TAILMS","RENDER_ADDTOPROJ","RENDER_DITHER"}
local STRKEYS = {"RENDER_FILE","RENDER_PATTERN"}
local saved_n, saved_s = {}, {}
for _, k in ipairs(NUMKEYS) do saved_n[k] = reaper.GetSetProjectInfo(0, k, 0, false) end
for _, k in ipairs(STRKEYS) do local _, v = reaper.GetSetProjectInfo_String(0, k, "", false) saved_s[k] = v end
log("backup|ok=1")
flush()
reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
reaper.SetMediaTrackInfo_Value(tr, "B_MUTE", 0)
local wav = DIR .. "spike-tone.wav"
reaper.SetEditCurPos(0, false, false)
reaper.SetOnlyTrackSelected(tr)
reaper.InsertMedia(wav, 0)
reaper.SetEditCurPos(2, false, false)
reaper.InsertMedia(wav, 0)
log("items=" .. tostring(reaper.CountMediaItems(0)))
-- determinism: bounds = time selection 0..3, file temp
reaper.GetSet_LoopTimeRange(true, false, 0, 3, false)
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 2, true)
reaper.GetSetProjectInfo(0, "RENDER_ADDTOPROJ", 0, true)
reaper.GetSetProjectInfo_String(0, "RENDER_FILE", os.getenv("TEMP") or DIR, true)
reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", "synthlm-det-$region", true)
local hashes = {}
for r = 1, 3 do
  reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", "synthlm-det" .. tostring(r), true)
  reaper.Main_OnCommand(42230, 0)
  local p = ((os.getenv("TEMP") or DIR) .. "\\synthlm-det" .. tostring(r) .. ".wav")
  local w = 0
  while w < 40 and not reaper.file_exists(p) do reaper.defer(function() end) w = w + 1 end
  -- poll by re-checking (defer is async; use busy re-stat instead)
  local tries = 0
  while tries < 100 and not reaper.file_exists(p) do tries = tries + 1 end
  hashes[r] = fnv1a(p)
  log(string.format("det|r%d|exists=%s|size=%s|fnv=%s", r, tostring(reaper.file_exists(p)), tostring(fsize(p)), tostring(hashes[r])))
  flush()
end
log("det|all_equal=" .. tostring(hashes[1] ~= nil and hashes[1] == hashes[2] and hashes[2] == hashes[3]))
-- M7: selected-items bounds, single-file bit on/off
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 4, true)
reaper.SelectAllMediaItems(0, true)
local cur = reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", 0, false)
local SINGLE = 4 * 65536
reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", cur | SINGLE, true)
reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", "synthlm-m7-single", true)
reaper.Main_OnCommand(42230, 0)
local ps = ((os.getenv("TEMP") or DIR) .. "\\synthlm-m7-single.wav")
log("m7|single_exists=" .. tostring(reaper.file_exists(ps)) .. "|size=" .. tostring(fsize(ps)))
local _, tgt1 = reaper.GetSetProjectInfo_String(0, "RENDER_TARGETS", "", false)
log("m7|targets_single=" .. tostring(tgt1 and tgt1:sub(1, 200) or "nil"))
flush()
-- restore everything
for _, k in ipairs(NUMKEYS) do reaper.GetSetProjectInfo(0, k, saved_n[k], true) end
for _, k in ipairs(STRKEYS) do reaper.GetSetProjectInfo_String(0, k, saved_s[k], true) end
log("restore|ok=1")
reaper.DeleteTrack(tr)
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
