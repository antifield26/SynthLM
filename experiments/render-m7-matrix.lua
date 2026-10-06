-- experiments/render-m7-matrix.lua (TSK-104 M7: source bits x single-file bit)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "render-m7-matrix.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local TMP = os.getenv("TEMP") or base
local sn = reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", 0, false)
local sb = reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 0, false)
reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
local wav = base .. "spike-tone.wav"
reaper.SetEditCurPos(20, false, false)
reaper.SetOnlyTrackSelected(tr)
reaper.InsertMedia(wav, 0)
reaper.SetEditCurPos(22, false, false)
reaper.InsertMedia(wav, 0)
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 4, true)
reaper.SelectAllMediaItems(0, false)
for i = 0, reaper.CountMediaItems(0) - 1 do
  local it = reaper.GetMediaItem(0, i)
  reaper.SetMediaItemSelected(it, reaper.GetMediaItem_Track(it) == tr)
end
reaper.GetSetProjectInfo(0, "RENDER_ADDTOPROJ", 0, true)
reaper.GetSetProjectInfo_String(0, "RENDER_FILE", TMP, true)
local SRC32, SINGLE = 32, 4 * 65536
local cases = {
  {"s0b0", 0, 0}, {"s0b1", 0, SINGLE},
  {"s32b0", SRC32, 0}, {"s32b1", SRC32, SINGLE},
}
for _, c in ipairs(cases) do
  local tag, src, sbit = c[1], c[2], c[3]
  local basev = sn - (sn % 1)
  -- clear known source/single bits then set case bits (preserve other user bits)
  local v = basev
  if (v & SRC32) ~= 0 then v = v - SRC32 end
  if (v & SINGLE) ~= 0 then v = v - SINGLE end
  v = v + src + sbit
  reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", v, true)
  reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", "synthlm-m7-" .. tag, true)
  reaper.Main_OnCommand(42230, 0)
  local _, tgt = reaper.GetSetProjectInfo_String(0, "RENDER_TARGETS", "", false)
  log(tag .. "|settings=" .. tostring(v) .. "|targets=" .. tostring(tgt))
  flush()
end
reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", sn, true)
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", sb, true)
reaper.DeleteTrack(tr)
for _, tag in ipairs({"s0b0", "s0b1", "s32b0", "s32b1"}) do
  os.remove(TMP .. "\\synthlm-m7-" .. tag .. ".wav")
end
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
