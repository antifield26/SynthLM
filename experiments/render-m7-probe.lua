-- experiments/render-m7-probe.lua (TSK-104 M7: full targets string + settings dump)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "render-m7-probe.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local TMP = os.getenv("TEMP") or base
local sn = reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", 0, false)
local sb = reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 0, false)
log("cur|settings=" .. tostring(sn) .. " boundsflag=" .. tostring(sb))
reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
local wav = base .. "spike-tone.wav"
reaper.SetEditCurPos(10, false, false)
reaper.SetOnlyTrackSelected(tr)
reaper.InsertMedia(wav, 0)
reaper.SetEditCurPos(12, false, false)
reaper.InsertMedia(wav, 0)
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 4, true)
reaper.SelectAllMediaItems(0, false)
for i = 0, reaper.CountMediaItems(0) - 1 do
  local it = reaper.GetMediaItem(0, i)
  reaper.SetMediaItemSelected(it, reaper.GetMediaItem_Track(it) == tr)
end
local SINGLE = 4 * 65536
local cur = reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", 0, false)
log("settings_before=" .. tostring(cur) .. " single_bit_set=" .. tostring((cur & SINGLE) ~= 0))
reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", cur & (~SINGLE), true)
reaper.GetSetProjectInfo(0, "RENDER_ADDTOPROJ", 0, true)
reaper.GetSetProjectInfo_String(0, "RENDER_FILE", TMP, true)
reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", "synthlm-m7p", true)
reaper.Main_OnCommand(42230, 0)
local _, tgt = reaper.GetSetProjectInfo_String(0, "RENDER_TARGETS", "", false)
log("targets_len=" .. tostring(tgt and #tgt or -1))
log("targets_full=" .. tostring(tgt))
flush()
-- candidate second-file probes (documented as guesses)
for _, suf in ipairs({"-001", "-01", "_001", " 2", "-2", "_2", " (2)"}) do
  local p = TMP .. "\\synthlm-m7p" .. suf .. ".wav"
  log("probe|" .. suf .. "=" .. tostring(reaper.file_exists(p)))
end
reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", sn, true)
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", sb, true)
reaper.DeleteTrack(tr)
os.remove(TMP .. "\\synthlm-m7p.wav")
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
