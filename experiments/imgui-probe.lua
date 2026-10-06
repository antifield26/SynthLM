-- experiments/imgui-probe.lua (TSK-307 unblock verification)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "imgui-probe.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
for _, a in ipairs({
  "GetVersion", "CreateContext", "DestroyContext",
  "Begin", "End", "Text", "Button",
  "CreateFont", "Attach",
}) do
  log("APIExists(ImGui_" .. a .. ")=" .. tostring(reaper.APIExists("ImGui_" .. a)))
end
local ok, ver = pcall(reaper.ImGui_GetVersion)
log("GetVersion_ok=" .. tostring(ok) .. " val=" .. tostring(ver and ver:sub(1, 40) or ver))
local f = io.open(out_path, "w")
if f then f:write(table.concat(lines, "\n") .. "\n") f:close() end
