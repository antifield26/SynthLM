-- experiments/a04-spike02-midi-take.lua
-- 目的: A04 写路径实测 (建临时轨,测完删除),验证 undo/dirty/P_EXT/NCH/Split/TakeIsMIDI
-- 运行: reaper.exe -nonewinst experiments/a04-spike02-midi-take.lua
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "a04-spike02-midi-take.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w"); f:write(table.concat(lines, "\n").."\n"); f:close()
end

reaper.PreventUIRefresh(1)
local ntr0 = reaper.CountTracks(0)
log("tracks_before=" .. tostring(ntr0))
log("undo_entries_before=" .. tostring(reaper.Undo_GetNumEntries and reaper.Undo_GetNumEntries() or "n/a"))

-- 1. 建临时轨
reaper.InsertTrackAtIndex(ntr0, false)
local tr = reaper.GetTrack(0, ntr0)
log("temp_track_created=" .. tostring(tr ~= nil))

-- 2. 建空 MIDI item (2 秒)
local item = reaper.CreateNewMIDIItemInProj(tr, 0, 2, false)
log("midi_item_created=" .. tostring(item ~= nil))
local take = item and reaper.GetActiveTake(item) or nil
log("take_exists=" .. tostring(take ~= nil))
log("TakeIsMIDI=" .. tostring(take and reaper.TakeIsMIDI(take) or "n/a"))

-- 3. InsertNote + Sort,包 undo + dirty
local u0 = reaper.Undo_GetNumEntries()
reaper.Undo_BeginBlock2(0)
reaper.MIDI_InsertNote(take, true, false, 0, 480, 0, 60, 100, false)
reaper.MIDI_Sort(take)
reaper.MarkTrackItemsDirty(tr, item)
reaper.Undo_EndBlock2(0, "SYNTHLM spike insert", -1)
local u1 = reaper.Undo_GetNumEntries()
log("undo_insert_delta=" .. tostring(u1 - u0))
local _, _, _, s0, e0, _, p0, v0 = reaper.MIDI_GetNote(take, 0)
log(string.format("note0 start=%s end=%s pitch=%s vel=%s", tostring(s0), tostring(e0), tostring(p0), tostring(v0)))
local _, cnt1 = reaper.MIDI_CountEvts(take)
-- MIDI_CountEvts returns retval,notecnt,ccevtcnt,textsyx; 取第2个
log("midi_count_raw=" .. tostring(cnt1))

-- 4. GetHash vs GetAllEvts 空回写
local okH, hash1 = reaper.MIDI_GetHash(take, true)
local okE, buf1 = reaper.MIDI_GetAllEvts(take, "")
log("gethash_ok=" .. tostring(okH) .. " hash_len=" .. tostring(hash1 and #hash1 or -1))
log("getallevts_ok=" .. tostring(okE) .. " buf_len=" .. tostring(buf1 and #buf1 or -1))
reaper.Undo_BeginBlock2(0)
reaper.MIDI_SetAllEvts(take, buf1)
reaper.MarkTrackItemsDirty(tr, item)
reaper.Undo_EndBlock2(0, "SYNTHLM spike null-write", -1)
local okH2, hash2 = reaper.MIDI_GetHash(take, true)
local okE2, buf2 = reaper.MIDI_GetAllEvts(take, "")
log("nullwrite_hash_same=" .. tostring(hash1 == hash2))
log("nullwrite_buf_same=" .. tostring(buf1 == buf2))

-- 5. 无 dirty 对照:只 SetAllEvts 回写,不标 dirty,看 undo 是否增加
local ua = reaper.Undo_GetNumEntries()
reaper.Undo_BeginBlock2(0)
reaper.MIDI_SetAllEvts(take, buf1)
reaper.Undo_EndBlock2(0, "SYNTHLM spike no-dirty", -1)
local ub = reaper.Undo_GetNumEntries()
log("undo_no_dirty_delta=" .. tostring(ub - ua) .. " (7.82 auto-dirty 可能为1, 7.60 期望0)")

-- 6. P_EXT 写读 (take/item/track)
reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_PROV", "spike-take", true)
local _, rv1 = reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_PROV", "", false)
log("take_pext_roundtrip=" .. tostring(rv1))
reaper.GetSetMediaItemInfo_String(item, "P_EXT:SYNTHLM_PROV", "spike-item", true)
local _, rv2 = reaper.GetSetMediaItemInfo_String(item, "P_EXT:SYNTHLM_PROV", "", false)
log("item_pext_roundtrip=" .. tostring(rv2))
reaper.GetSetMediaTrackInfo_String(tr, "P_EXT:SYNTHLM_PROV", "spike-track", true)
local _, rv3 = reaper.GetSetMediaTrackInfo_String(tr, "P_EXT:SYNTHLM_PROV", "", false)
log("track_pext_roundtrip=" .. tostring(rv3))
-- GUID 查询
local _, tguid = reaper.GetSetMediaItemTakeInfo_String(take, "GUID", "", false)
log("take_guid_len=" .. tostring(tguid and #tguid or -1))

-- 7. I_TAKEFX_NCH 读写
local nch0 = reaper.GetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH")
log("takefx_nch_initial=" .. tostring(nch0))
reaper.SetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH", 8)
local nch1 = reaper.GetMediaItemTakeInfo_Value(take, "I_TAKEFX_NCH")
log("takefx_nch_after_set8=" .. tostring(nch1))
-- chunk 含 TAKEFX_NCH?
local _, chunk = reaper.GetItemStateChunk(item, "", false)
log("chunk_has_TAKEFX_NCH=" .. tostring(chunk and chunk:find("TAKEFX_NCH") ~= nil or false))

-- 8. SplitMediaItem (1s 处)
local right = reaper.SplitMediaItem(item, 1.0)
log("split_right_exists=" .. tostring(right ~= nil))
log("count_items_after_split=" .. tostring(reaper.CountMediaItems(0)))

-- 9. 清理:删除临时轨(含其 items)
reaper.DeleteTrack(tr)
log("tracks_after_delete=" .. tostring(reaper.CountTracks(0)))
log("undo_entries_after=" .. tostring(reaper.Undo_GetNumEntries()))
reaper.PreventUIRefresh(-1)
reaper.UpdateArrange()
flush()
reaper.ShowConsoleMsg("A04 spike02 done\n")
