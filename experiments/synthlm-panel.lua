-- experiments/synthlm-panel.lua (TSK-307: thin ReaImGui panel, dockable, test-mode exits)
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local hb_path = base .. "synthlm-panel-heartbeat.out.txt"
local ctx = reaper.ImGui_CreateContext("SynthLM")
local dock_fn = reaper.APIExists("ImGui_SetDock") and reaper.ImGui_SetDock or nil
if dock_fn then pcall(dock_fn, ctx, 0) end
local count = 0
local frames = 0
local test_mode = reaper.GetExtState("SynthLM", "PanelTest", "") == "1"
local t0 = os.clock()
local function hb(msg)
  local f = io.open(hb_path, "w")
  if f then f:write(msg .. "\n") f:close() end
end
hb("panel_started dock=" .. tostring(dock_fn ~= nil))
local function loop()
  frames = frames + 1
  local visible, open = reaper.ImGui_Begin(ctx, "SynthLM", true)
  if visible then
    reaper.ImGui_Text(ctx, "SynthLM thin panel (TSK-307)")
    reaper.ImGui_Text(ctx, "frames=" .. tostring(frames) .. " clicks=" .. tostring(count))
    if reaper.ImGui_Button(ctx, "Ping (" .. tostring(count) .. ")") then
      count = count + 1
      reaper.ShowConsoleMsg("SynthLM panel ping " .. tostring(count) .. "\n")
    end
    reaper.ImGui_SameLine(ctx)
    if reaper.ImGui_Button(ctx, "Open main window (stub)") then
      reaper.ShowConsoleMsg("SynthLM: main window link stub\n")
    end
    reaper.ImGui_End(ctx)
  end
  if test_mode and (os.clock() - t0 > 3) then
    hb("panel_frames=" .. tostring(frames) .. " clicks=" .. tostring(count) .. " clean_exit=1")
    reaper.DeleteExtState("SynthLM", "PanelTest", false)
    return
  end
  if open then reaper.defer(loop) end
end
reaper.defer(loop)
