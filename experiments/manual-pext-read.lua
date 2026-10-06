-- experiments/manual-pext-read.lua (人工验证第 3 步：在目标工程选中粘贴后的 item 再跑)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "manual-pext-read.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local item = reaper.GetSelectedMediaItem(0, 0)
log("selected_item=" .. tostring(item ~= nil))
if item then
  local take = reaper.GetActiveTake(item)
  local _, v = reaper.GetSetMediaItemTakeInfo_String(take, "P_EXT:SYNTHLM_MANUAL", "", false)
  log("pext_value=" .. tostring(v == "" and "<empty>" or v))
  log("take_is_midi=" .. tostring(reaper.TakeIsMIDI(take)))
end
local f = io.open(out_path, "w")
if f then f:write(table.concat(lines, "\n") .. "\n") f:close() end
reaper.ShowConsoleMsg("manual read done\n")
