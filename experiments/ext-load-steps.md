# TSK-114 真机加载 smoke — 人类协同操作清单

- 范围：本机可做部分（构建 + 导出表 + lint/test/fmt/doc）已由子 Agent 完成；
  以下真机步骤需人类协同（Agent **不复制 DLL、不重启 REAPER**）。
- 前置产物：`target/debug/synthlm_bridge.dll`（`cargo build -p synthlm-bridge`
  已验证产出，约 0.8MB，`cargo clippy/test/fmt/doc` 全绿）。
- 导出表已验证（含 `ReaperPluginEntry` + `DllMain`，65 个导出；其余
  `cpp_to_rust_*` 系 reaper-low 自带 C++ shim）。
- 依据：reaper-rs rev `659b22b` 的 `README.md:243`–`276`
  （`reaper_` 文件名前缀规则 + `REAPER/UserPlugins` 目录 + 重启加载）。

## 步骤（人类执行）

1. **确认 REAPER 已完全退出**（扩展在启动时加载，须重启才生效/卸载；
   不要在 REAPER 运行时覆盖 `UserPlugins` 下的旧文件）。
2. **复制并改名**（满足 `reaper_` 前缀规则，README `243`–`253`）：
   - 从：`%USERPROFILE%\projects\SynthLM\target\debug\synthlm_bridge.dll`
   - 到：`%APPDATA%\REAPER\UserPlugins\reaper_synthlm_bridge.dll`
   - 注意：这一步由人类手动做（Agent 约束：不复制任何 DLL 到 REAPER 目录）。
3. **正常启动 REAPER**（不要 `-nonewinst`，要完整启动以加载扩展），打开
   ReaScript 控制台，断言看到版本行：
   `SynthLM bridge 0.0.0 loaded (control-plane only, no project touched)`
4. **Action 断言**：Action List 搜索 `SynthLM: test ping` → Run；
   断言 `%TEMP%\synthlm_ext_smoke.txt` 存在，内容形如：
   ```text
   synthlm-bridge ext smoke ok
   version: 0.0.0
   epoch_secs: <数字>
   ```
   且工程零改动（undo 历史无新增点）。
5. **干净卸载断言**：退出 REAPER（应无崩溃/无报错弹窗）；如需卸载，删掉
   `reaper_synthlm_bridge.dll` 后再启动，版本行消失即干净。
6. 回填结果到 TSK-114（本清单 + 各断言通过/失败）。

## 安全声明（红线对照）

- 入口只做：版本日志 + 注册自有测试 Action + 成功返回；无第三方插件
  加载/逆向/hook（`AGENTS.md` §3.1）。
- Action 回调只写 `%TEMP%` 标记文件，不碰工程/take/FX/undo（§3.5）。
- 日志/标记文件均无绝对路径、无 Key、无音频内容（§3.7/§8）。

## 失败 → BLOCKED 的上报格式

- 现象（哪一步）+ ReaScript 控制台原文 + 是否崩溃 + DLL 时间戳；
  不要猜测修入口语义，先回 TSK-114 登记。
