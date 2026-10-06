-- experiments/a04-spike03b-min.lua
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "a04-spike03b-min.out.txt"
local f = io.open(out_path, "w")
f:write("min_ok=1\n")
f:write("has_Undo_DoUndo2=" .. tostring(reaper.APIExists("Undo_DoUndo2")) .. "\n")
f:write("has_kbd=" .. tostring(reaper.APIExists("kbd_getTextFromCmd")) .. "\n")
f:write("has_TakeFX_AddByName=" .. tostring(reaper.APIExists("TakeFX_AddByName")) .. "\n")
f:close()
reaper.ShowConsoleMsg("min done\n")
