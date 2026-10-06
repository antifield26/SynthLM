# A01 FX 链与参数控制、预设、实例寻址

- 目的：核实 REAPER ReaScript/C++ API 中 FX 链增删、参数读写、预设切换、实例寻址的真实能力边界，为 SynthLM FX 控制层选型提供依据。
- 适用范围：`TrackFX_*` / `TakeFX_*`（链管理、参数、预设、使能、GUID）；`Begin/EndParamEdit`；reaper-rs low/medium 覆盖度；REAPER 7.x。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：无（Phase 0 首批）；后续 DEC-0xx、ARCHITECTURE.md 依赖本文。

> 约定：每条结论后标注来源 + 置信度。`⚠️需实测` 表示文档有声明但行为细节官方未说明，不得直接作为工程假设。

## 1 链管理 API（存在性：高置信）

- `TrackFX_AddByName(MediaTrack* track, const char* fxname, bool recFX, int instantiate)`：存在。`instantiate<0` 总是新建；`=0` 仅查询；`>0` 无则添加；`<=-1000` 用作插入位置。`fxname` 可带 `VST3:/VST2:/VST:/AU:/JS:/DX:` 前缀，或 `FXADD:` 取 FX 浏览器选中项。来源：https://www.reaper.fm/sdk/reascript/reascripthelp.html#TrackFX_AddByName ，2026-10-05，高。
- `TrackFX_Delete(MediaTrack* track, int fx) -> bool`：存在。来源：同上 `#TrackFX_Delete`，2026-10-05，高。
- `TrackFX_GetCount / TrackFX_GetRecCount`：存在；后者返回 record-input FX 数量，`0x1000000` 寻址 input FX。来源：同上对应锚点，2026-10-05，高。
- `TrackFX_GetFXName / TrackFX_GetFXGUID / TrackFX_CopyToTrack / TrackFX_CopyToTake / TakeFX_CopyToTake / TakeFX_CopyToTrack / TrackFX_GetEnabled / TrackFX_SetEnabled`（及 Take 侧对应）：均存在。`Copy*` 返回 `void`，无成功指示，⚠️需实测失败行为。`SetEnabled` 返回 `void`，越界行为 ⚠️需实测。来源：同上对应锚点，2026-10-05，高。
- `TrackFX_GetByName` 已废弃（文档明确 Deprecated in favor of AddByName）。来源：同上，2026-10-05，高。

## 2 参数读写 API（存在性：高；数值语义：⚠️需实测）

- `TrackFX_GetParam(track, fx, param, minvalOut, maxvalOut) -> double` / `TrackFX_SetParam(track, fx, param, val) -> bool`：存在；Take 侧同形存在。文档未说明原始值 vs 归一化关系，`SetParam` 与 `SetParamNormalized` 是否等价 ⚠️需实测。来源：同上对应锚点，2026-10-05，高。
- `GetNumParams / GetParamName / GetFormattedParamValue / FormatParamValue / FormatParamValueNormalized`：存在；后两者文档明确 Note: only works with FX that support Cockos VST extensions（非全插件通用）。来源：同上，2026-10-05，高。
- `GetParamNormalized / SetParamNormalized / GetParamEx(min/mid/max) / GetParamIdent / GetParamFromIdent(:wet/:bypass/:delta) / GetParameterStepSizes`：存在，Track/Take 双侧。来源：同上，2026-10-05，高。
- `Get/SetNamedConfigParm`：存在；可读写键含 `vst_chunk[_program]`、`clap_chunk`、`param.X.*`、`container_*`、`chain_pdc_*`、`renamed_name`、`parallel` 等；可读键含 `fx_type/fx_ident/parent_container/container_count/container_item.X/param.X.default_value`。v7.06+ 推荐 `parent_container` + `container_item.X` 导航而非手算 `0x2000000` 公式。`vst_chunk` 是否覆盖全部参数、与 `SetPreset` 等价性 ⚠️需实测。来源：同上 `#TrackFX_GetNamedConfigParm`，2026-10-05，高。

## 3 Begin/EndParamEdit

- `TrackFX_BeginParamEdit / TakeFX_BeginParamEdit`：官方页面全文检索 0 次 —— 不存在。来源：官方页面全文统计，2026-10-05，高。
- `TrackFX_EndParamEdit(track, fx, param) -> bool` / Take 侧同形：存在，但官方零语义说明。reaper-rs medium 注释称重要用于 Touch 自动化，主线程调用。单独调 End 无 Begin 配对是否足以闭环 ⚠️需实测。来源：官方锚点 + https://raw.githubusercontent.com/helgoboss/reaper-rs/master/main/medium/src/reaper.rs ，2026-10-05，中。

## 4 实例寻址：索引漂移（高） vs GUID 稳定性（⚠️需实测）

- 索引是位置编码：普通 `0..n-1`；`+0x1000000` input/monitoring；`+0x2000000` 容器（stride = GetCount+1）；`CopyToTrack` 另有 `dest|0x800000` 未用槽位。增删/移动必然漂移，必须每次重查。来源：官方各 TrackFX 锚点重复声明，2026-10-05，高。
- `GetFXGUID` 官方无稳定性承诺（无 stable/persistent 陈述）。假设跨保存稳定、复制换 GUID、删除后悬空均为推测，⚠️需实测六组对比（增删/移动/复制is_move真假/跨轨/容器进出/撤销/存盘重载）。来源：官方锚点全文核查 + reaper-rs medium 注释仅称 Returns GUID，2026-10-05，高（无说明本身）/低（稳定性假设）。

## 5 预设读写

- `SetPreset(track, fx, presetname)` / `GetPreset`：存在；仅承诺下拉框显示名往返 + VST3 `.vstpreset` 全路径；不承诺覆盖全部参数、不承诺跨插件可移植。全文未出现 `.rpl`，不可假设支持。来源：官方对应锚点，2026-10-05，高。
- `GetPresetIndex / SetPresetByIndex(idx==-2 factory/-1 default user) / NavigatePresets`：存在，Track/Take 双侧。VST3 factory 下 `index==-1` 歧义（FX 不存在 vs factory 激活）⚠️需实测。来源：官方锚点 + reaper-rs medium 注释引 Justin，2026-10-05，高/中。
- 快照/恢复全部参数应优先考虑 `vst_chunk/clap_chunk` 或 Track/Take chunk，而非仅依赖 SetPreset；三者等价性分插件类型 ⚠️需实测。

## 6 reaper-rs 绑定覆盖度（2026-10-05，中）

- `reaper-low`（自生成）：几乎全覆盖；`BeginParamEdit` 0 处（不存在），`EndParamEdit`/`GetFXGUID`/`SetPreset` 均多处命中。来源：`main/low/src/reaper.rs` 字符串计数，中。
- `reaper-medium`（手写）：`track_fx_*` 约 42 个 fn；缺口：`take_fx_*` 0 个（Take 侧未 lift）、原始 `track_fx_get/set_param` 缺失（仅 normalized/ex）、`copy_to_take` 缺失。缺口部分回落到 low 或自封装。来源：`main/medium/src/reaper.rs` 检索，中，⚠️需实测/复核。

## 7 ⚠️需实测清单（转 TSK）

1. GUID 六组稳定性对比；2. SetPreset vs chunk 等价性（分 VST2/VST3/CLAP/JS）；3. `.rpl` 经 API 读写（期望否）；4. 无 Begin 时单调 End 对 Touch 充分性；5. `SetParam` vs `SetParamNormalized` 数值映射；6. `FormatParamValue*` 非 Cockos 扩展失败模式；7. `SetEnabled(void)/Copy*(void)` 失败观测手段。
