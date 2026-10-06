# LICENSES（依赖许可登记册）

- 目的：登记一切新增依赖与外部组件的许可结论；新增依赖必须同步更新本表（AGENTS.md 红线 6）。
- 适用范围：全仓库；约束：内部非商业、无分发、不购买许可。
- 状态：Accepted（种子版，随 TSK-113 扩展）
- 最后核验日期：2026-10-06
- 依赖文档：docs/DECISIONS.md（DEC-025）、docs/EVALUATION.md（§6）。

| 依赖/组件 | 许可 | 来源 | 内部使用结论 | 日期 |
|---|---|---|---|---|
| reaper-rs（`reaper-medium` + `reaper-low`，git rev `659b22b` pinned） | MIT | <https://github.com/helgoboss/reaper-rs>（README 要求用 git master，不用 crates.io 陈旧版） | 可用，仅内部非商业运行；传递依赖（nutype git 分支、vst、winapi 等）以 Cargo.lock 为准 | 2026-10-06（rev 提交 2026-09-12 "cargo fmt"，完整 rev `659b22bfbc34bf4a5a40902e6fdf60ccfd34ca23`） |
| thiserror 2.0.21（`synthlm-bridge` 直接依赖，库边界错误类型） | MIT OR Apache-2.0 | crates.io/crates/thiserror（registry 缓存 manifest 核验） | 可用，仅内部非商业运行 | 2026-10-06 |
| serde 1.0.229（`synthlm-bridge` 直接依赖，anchor 序列化，derive） | MIT OR Apache-2.0 | crates.io/crates/serde（registry 缓存 manifest 核验） | 可用，仅内部非商业运行 | 2026-10-06 |
| serde_json 1.0.151（`synthlm-bridge` dev-only，单测 JSON 往返） | MIT OR Apache-2.0 | crates.io/crates/serde_json（registry 缓存 manifest 核验） | 可用，仅内部非商业运行（不进运行时） | 2026-10-06 |
| interprocess（待引入） | 0BSD/Apache-2.0 | crates.io | 可用（TSK-107 接入时登记版本） | 2026-10-06 |
| Demucs 官方权重 | 科研限定（代码 MIT） | issue #327 | 仅内部非商业运行；商用/分发重授权 | 2026-10-06 |
| MERT/MuQ 权重 | CC-BY-NC-4.0 | HF license 字段 | 仅内部非商业；不进产品基线 | 2026-10-06 |
| Rubber Band | GPL-2-or-later/商业 | breakfastquay 许可页 | 不购证→仅内部运行；分发即违法 | 2026-10-06 |
| OpenCode Go 云端（Tier1/ZDR-Tier2） | 用户确认条款（训练保留/ZDR；不声明计费） | 人类 2026-10-06 | 三档授权 + 审计；Key 仅 `.env` | 2026-10-06 |
| FFmpeg（二进制，Gyan 9.0.2-essentials，`C:\tools\ffmpeg`） | GPL-2+（`--enable-gpl` + `--enable-librubberband`） | `ffmpeg -buildconf` 存档 experiments/ffmpeg-buildconf-9.0.2.txt | 仅内部运行；禁作 LGPL fallback；LGPL 构建另寻（TSK-206） | 2026-10-06 |

注：当前 workspace 为零外部依赖骨架（`cargo tree --workspace` 验证，仅 path 内部 crates）；上表为 Phase 1 接入前的许可预登记，接入时补版本号。

## 附：2026-10-06 TSK-113 核验记录

- `cargo tree --workspace`：零外部依赖（输出仅 8 个内部包），引入记录义务当前为空。
- FFmpeg：`~/tools/ffmpeg/ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe`（Gyan essentials，人类授权安装，已加入用户 PATH；此前 `C:\tools\ffmpeg` 已迁走）。`ffmpeg -buildconf` 存档见 `experiments/ffmpeg-buildconf-9.0.2.txt`：含 `--enable-gpl --enable-version3 ... --enable-librubberband`（无 `--enable-nonfree`）→ **该二进制为 GPL-2+ 构建，不是 LGPL**。
- 结论（与 C-dsp §1.2 预判一致）：此二进制不可作 LGPL fallback；仅限内部非商业运行（DEC-025）；LGPL fallback 需另找非 gpl 构建，记入 TSK-206。
