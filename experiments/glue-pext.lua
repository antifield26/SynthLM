-- experiments/glue-pext.lua (TSK-111 M6: glue 单 item 后 take P_EXT 是否保留)
-- 运行: "C:\Program Files\REAPER (x64)\reaper.exe" -nonewinst experiments/glue-pext.lua
-- 输出: experiments/glue-pext.out.txt (分步 flush, 可重跑, glue 后 Undo 恢复再删轨)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "glue-pext.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n").."\n") f:close() end
end
local function step(name, fn)
  log("STEP|" .. name .. "|enter")
  flush()
  local r = {pcall(fn)}
  local ok = r[1]
  log("STEP|" .. name .. "|ok=" .. tostring(ok) .. (ok and "" or " err=" .. tostring(r[2]):sub(1, 200)))
  flush()
  return ok
end
local function read_pext(tk)
  local _, v = reaper.GetSetMediaItemTakeInfo_String(tk, "P_EXT:SYNTHLM_GLUE", "", false)
  return v
end
local G = {}
reaper.PreventUIRefresh(1)
step("setup-track", function()
  G.ntr0 = reaper.CountTracks(0)
  reaper.InsertTrackAtIndex(G.ntr0, false)
  G.tr = reaper.GetTrack(0, G.ntr0)
  log("setup|tracks_before=" .. tostring(G.ntr0) .. " track_ok=" .. tostring(G.tr ~= nil))
end)
step("audio-item", function()
  local wav = base .. "spike-tone.wav"
  G.item = reaper.AddMediaItemToTrack(G.tr)
  log("audio|item_ok=" .. tostring(G.item ~= nil))
  G.take = reaper.AddTakeToMediaItem(G.item)
  log("audio|take_ok=" .. tostring(G.take ~= nil))
  local src = reaper.PCM_Source_CreateFromFile(wav)
  log("audio|src_ok=" .. tostring(src ~= nil))
  if src then
    reaper.SetMediaItemTake_Source(G.take, src)
    reaper.GetSetMediaItemTakeInfo_String(G.take, "P_NAME", "spike-tone", true)
    reaper.SetMediaItemInfo_Value(G.item, "D_POSITION", 0)
    reaper.SetMediaItemInfo_Value(G.item, "D_LENGTH", 1)
    reaper.UpdateArrange()
    local _, guid = reaper.GetSetMediaItemTakeInfo_String(G.take, "GUID", "", false)
    G.take_guid = guid
    log("audio|take_guid_len=" .. tostring(guid and #guid or -1))
    log("audio|TakeIsMIDI=" .. tostring(reaper.TakeIsMIDI(G.take)))
  end
end)
step("pext-write", function()
  local tr = reaper.GetTrack(0, G.ntr0)
  local it = reaper.GetTrackMediaItem(tr, 0)
  local tk = reaper.GetActiveTake(it)
  reaper.GetSetMediaItemTakeInfo_String(tk, "P_EXT:SYNTHLM_GLUE", "probe", true)
  log("pext|write_verify=" .. tostring(read_pext(tk)))
  local _, guid = reaper.GetSetMediaItemTakeInfo_String(tk, "GUID", "", false)
  G.take_guid = guid
  G.u0 = reaper.Undo_GetNumEntries()
  log("pext|undo0=" .. tostring(G.u0))
end)
step("glue", function()
  reaper.PreventUIRefresh(-1)
  local tr = reaper.GetTrack(0, G.ntr0)
  reaper.SelectAllMediaItems(0, false)
  local it = reaper.GetTrackMediaItem(tr, 0)
  reaper.SetMediaItemSelected(it, true)
  reaper.UpdateArrange()
  local n_before = reaper.CountTrackMediaItems(tr)
  log("glue|items_before=" .. tostring(n_before))
  local u1 = reaper.Undo_GetNumEntries()
  reaper.Main_OnCommand(40362, 0) -- Glue items ignoring time selection
  reaper.UpdateArrange()
  log("glue|undo_delta=" .. tostring(reaper.Undo_GetNumEntries() - u1))
  local tr2 = reaper.GetTrack(0, G.ntr0)
  local n_after = reaper.CountTrackMediaItems(tr2)
  log("glue|items_after=" .. tostring(n_after))
  local it2 = reaper.GetTrackMediaItem(tr2, 0)
  log("glue|item_ok=" .. tostring(it2 ~= nil))
  G.new_take = it2 and reaper.GetActiveTake(it2) or nil
  local v = G.new_take and read_pext(G.new_take) or "n/a"
  log("glue|new_take_ok=" .. tostring(G.new_take ~= nil) .. " pext=" .. (v == "" and "<empty>" or tostring(v)))
  local kept = (v == "probe")
  log("glue|pext_kept=" .. tostring(kept))
  log("conclusion=" .. (kept and "glue 单 item 保留 take P_EXT" or "glue 后重写 provenance"))
  reaper.PreventUIRefresh(1)
end)
step("undo-restore", function()
  reaper.Undo_DoUndo2(0)
  reaper.UpdateArrange()
  local tr = reaper.GetTrack(0, G.ntr0)
  local n = tr and reaper.CountTrackMediaItems(tr) or -1
  log("undo|items_after_undo=" .. tostring(n))
  local it = tr and n > 0 and reaper.GetTrackMediaItem(tr, 0) or nil
  local tk = it and reaper.GetActiveTake(it) or nil
  local v = tk and read_pext(tk) or "n/a"
  log("undo|take_ok=" .. tostring(tk ~= nil) .. " pext=" .. (v == "" and "<empty>" or tostring(v)))
end)
step("cleanup", function()
  local tr2 = reaper.GetTrack(0, G.ntr0)
  if tr2 then reaper.DeleteTrack(tr2) log("cleanup|track_deleted=1") end
  log("cleanup|tracks_after=" .. tostring(reaper.CountTracks(0)))
end)
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
log("done=1")
flush()
