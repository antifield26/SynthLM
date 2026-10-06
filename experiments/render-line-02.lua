-- experiments/render-line-02.lua (TSK-104: isolated determinism + M7 multi + full restore)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "render-line-02.out.txt"
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
    local b = f:read(65536)
    if not b then break end
    for i = 1, #b do h = ((h ~ b:byte(i)) * 0x01000193) & 0xFFFFFFFF end
  end
  f:close()
  return string.format("%08x", h)
end
local TMP = os.getenv("TEMP") or base
local NUMKEYS = {"RENDER_SETTINGS","RENDER_BOUNDSFLAG","RENDER_CHANNELS","RENDER_SRATE","RENDER_STARTPOS","RENDER_ENDPOS","RENDER_TAILFLAG","RENDER_TAILMS","RENDER_ADDTOPROJ","RENDER_DITHER"}
local STRKEYS = {"RENDER_FILE","RENDER_PATTERN"}
local saved_n, saved_s = {}, {}
for _, k in ipairs(NUMKEYS) do saved_n[k] = reaper.GetSetProjectInfo(0, k, 0, false) end
for _, k in ipairs(STRKEYS) do local _, v = reaper.GetSetProjectInfo_String(0, k, "", false) saved_s[k] = v end
local _, loop0s, loop0e = reaper.GetSet_LoopTimeRange(false, false, 0, 0, false)
log("backup|dither=" .. tostring(saved_n["RENDER_DITHER"]))
reaper.PreventUIRefresh(1)
-- mute all pre-existing tracks, remember states
local mutes = {}
for i = 0, reaper.CountTracks(0) - 1 do
  local t = reaper.GetTrack(0, i)
  mutes[i] = reaper.GetMediaTrackInfo_Value(t, "B_MUTE")
  reaper.SetMediaTrackInfo_Value(t, "B_MUTE", 1)
end
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
reaper.SetMediaTrackInfo_Value(tr, "B_MUTE", 0)
local wav = base .. "spike-tone.wav"
reaper.SetEditCurPos(0, false, false)
reaper.SetOnlyTrackSelected(tr)
reaper.InsertMedia(wav, 0)
reaper.SetEditCurPos(2, false, false)
reaper.InsertMedia(wav, 0)
-- inventory items on temp track
local n = 0
for i = 0, reaper.CountMediaItems(0) - 1 do
  local it = reaper.GetMediaItem(0, i)
  if reaper.GetMediaItem_Track(it) == tr then
    n = n + 1
    log("tempitem|pos=" .. tostring(reaper.GetMediaItemInfo_Value(it, "D_POSITION")) .. "|len=" .. tostring(reaper.GetMediaItemInfo_Value(it, "D_LENGTH")))
  end
end
log("tempitems=" .. tostring(n))
reaper.GetSet_LoopTimeRange(true, false, 0, 3, false)
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 2, true)
reaper.GetSetProjectInfo(0, "RENDER_ADDTOPROJ", 0, true)
reaper.GetSetProjectInfo(0, "RENDER_DITHER", 0, true)
reaper.GetSetProjectInfo_String(0, "RENDER_FILE", TMP, true)
local hashes = {}
for r = 1, 3 do
  local pat = "synthlm-iso" .. tostring(r)
  reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", pat, true)
  reaper.Main_OnCommand(42230, 0)
  local p = TMP .. "\\" .. pat .. ".wav"
  hashes[r] = fnv1a(p)
  log(string.format("iso|r%d|fnv=%s", r, tostring(hashes[r])))
  flush()
end
log("iso|all_equal=" .. tostring(hashes[1] ~= nil and hashes[1] == hashes[2] and hashes[2] == hashes[3]))
-- M7 multi: bounds=selected items, single-bit CLEAR
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 4, true)
reaper.SelectAllMediaItems(0, false)
for i = 0, reaper.CountMediaItems(0) - 1 do
  local it = reaper.GetMediaItem(0, i)
  reaper.SetMediaItemSelected(it, reaper.GetMediaItem_Track(it) == tr)
end
local cur = reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", 0, false)
local SINGLE = 4 * 65536
reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", cur & (~SINGLE), true)
reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", "synthlm-m7-multi", true)
reaper.Main_OnCommand(42230, 0)
local _, tgt = reaper.GetSetProjectInfo_String(0, "RENDER_TARGETS", "", false)
log("m7|targets_multi=" .. tostring(tgt and tgt:sub(1, 400) or "nil"))
flush()
-- restore: settings, loop, mutes
for _, k in ipairs(NUMKEYS) do reaper.GetSetProjectInfo(0, k, saved_n[k], true) end
for _, k in ipairs(STRKEYS) do reaper.GetSetProjectInfo_String(0, k, saved_s[k], true) end
reaper.GetSet_LoopTimeRange(true, false, loop0s, loop0e, false)
for i, m in pairs(mutes) do
  local t = reaper.GetTrack(0, i)
  if t then reaper.SetMediaTrackInfo_Value(t, "B_MUTE", m) end
end
log("restore|ok=1")
reaper.DeleteTrack(tr)
for _, f in ipairs({"synthlm-det1.wav","synthlm-det2.wav","synthlm-det3.wav","synthlm-m7-single.wav","synthlm-iso1.wav","synthlm-iso2.wav","synthlm-iso3.wav"}) do
  os.remove(TMP .. "\\" .. f)
end
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
