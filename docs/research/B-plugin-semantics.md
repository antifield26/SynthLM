# B 插件参数语义层

- 目的：核实 REAPER 暴露的插件参数元数据能力边界，沉淀“声设相关参数”识别启发式，评估已有先例复用性与大参数量插件分层白名单策略。
- 适用范围：`TrackFX_*/TakeFX_*` 参数元数据系；JSFX/自带 vs VST3/CLAP 差异；ReaLearn/spk77/ReaPack；Kontakt/Serum/Vital；REAPER 7.x（文档 v7.82）。
- 状态：Draft
- 最后核验日期：2026-10-05
- 依赖文档：A01、A05。

> 每条结论后标注来源 + 置信度。`⚠️需实测` 不得作为工程假设。签名逐字取自官方 `reascripthelp.html`（v7.82）。

## 1 参数元数据：能拿到什么、拿不到什么

来源总表：https://www.reaper.fm/sdk/reascript/reascripthelp.html ，2026-10-05，高。

- `TrackFX_GetNumParams` 个数；`GetParamName` 显示名（跨格式可变、重名不唯一）。高/中。
- `GetParam(min/max)` + `GetParamEx(+mid)`：`mid` 不是默认值也不是中心承诺，零语义说明。默认值仅 `GetNamedConfigParm("param.X.default_value")`（归一化，`if available` 可失败）。JSFX 例外：`sliderN:default<min,max,step>` 明文。来源：官方命名配置段 + https://www.reaper.fm/sdk/js/js.php ，高。
- 单位：无独立单位 API，只能从 `GetFormattedParamValue/FormatParamValue` 显示串解析（dB/Hz/%）；VST3 `ParameterInfo.units` 有但 REAPER 未透出；CLAP `clap_param_info_t` 无单位字段（仅 name/module/min/max/default）。来源：https://steinbergmedia.github.io/vst3_doc/vstinterfaces/structSteinberg_1_1Vst_1_1ParameterInfo.html + https://raw.githubusercontent.com/free-audio/clap/main/include/clap/ext/params.h ，高。
- step：`GetParameterStepSizes(step,smallstep,largestep,istoggle)` 对连续参数普遍全 0/false，仅离散/toggle 有意义（Justin 原文：连续参数 not apply，ReaControlMIDI ch / JS General Dynamics param 0 才有意义）。枚举标签无独立 API（VST3 `stepCount/kIsList`、CLAP `IS_STEPPED/IS_ENUM` 未透出）。来源：https://forum.cockos.com/showthread.php?t=175163 ，高。
- 格式化：`FormatParamValue/FormatParamValueNormalized` 明确仅 Cockos VST 扩展支持（`Note: only works with FX that support Cockos VST extensions`），第三方失败模式 ⚠️需实测。高。
- 标识：`GetParamIdent/FromIdent(:wet,:bypass,:delta)`；ident 跨版本稳定性无承诺，裸索引永不持久化。来源：官方 `#TrackFX_GetParamFromIdent`，高。
- 可自动化：`GetNamedConfigParm("param.X.automatable")` 返 1.0；VST3 `kCanAutomate/kIsReadOnly/kIsHidden`、CLAP `IS_AUTOMATABLE/IS_READONLY/IS_HIDDEN` 被压缩为二值。`GetFXEnvelope(create=false)` 返 nil 辅助判断。⚠️需实测一致率。高。
- 分组：`GetParamSectionName` 返 VST3 unit / CLAP module 名，但 `not all plug-ins support`；CLAP module 为 `/` 分隔路径。VST2/JSFX 大面积为空。来源：官方原文 + CLAP params.h，高。
- 自带插件专属键（不可泛化）：`BANDTYPEx/BANDENABLEDx[ReaEQ]`、`THRESHOLD/CEILING/TRUEPEAK[ReaLimit]`、`NUMCHANNELS[ReaSurroundPan]`、`FILE/MODE[RS5k]`、`VIDEO_CODE[video]`、`GainReduction_dB[ReaComp+]`。来源：官方命名配置段全文，高。
- VST3/CLAP 整块：`vst_chunk[_program]/clap_chunk`（base64），与 `SetPreset` 等价性 ⚠️需实测。`GetPreset` 可返 `.vstpreset` 全路径。高。

## 2 声设相关参数识别

推荐优先级：
1. 可自动化 gate：`automatable==1.0` 且 `GetFXEnvelope(...,false)!=nil`。高。
2. `:wet/:bypass/:delta` 特判 + ident 持久化。高。
3. 分组优先于关键词：section/module 非空按组聚类（Filter/Env/LFO/Osc）。高。
4. 关键词最低优先级（仅排序）：filter/cutoff/reso/drive/attack/decay/sustain/release/lfo/rate/depth/env/mod/morph/spread/detune/unison/glide/chorus/delay/reverb/wet/mix/bypass + 中文同义词。低，须语料校准。
5. 动态信号：`last_touched` + ReaLearn Learn 思想，用户摸一下即锁定。来源：官方命名配置段 + https://docs.helgoboss.org/realearn/targets/fx-parameter/set-value.html ，中。
6. Glue 语义借用（相对/绝对/takeover/step/retrigger）：来源 https://docs.helgoboss.org/realearn/user-interface/mapping-panel/glue-section.html + https://docs.helgoboss.org/realearn/further-concepts/glue.html ，高（存在性）。

不可靠之处：关键词误杀（Attack/压缩器 vs 合成器、Mix 遍地）；step 全 0 正常（连续参数本无 step）；第三方格式化常失败失单位信号；section 大面积为空；无 `IsEnvelopeVisible` 直达 API（全文检索无）；`mid/default` 不可做零点假设。详见子 agent 原文 §2.2。

## 3 先例复用性（许可红线）

- ReaLearn/Helgobox **GPL-3.0**：`LICENSE` 全文 GPLv3（官网明确从 LGPL-3.0 改为 GPL-3.0），**不得链接/内嵌，只能借设计**。来源：https://raw.githubusercontent.com/helgoboss/realearn/master/LICENSE + https://github.com/helgoboss/helgobox + https://www.helgoboss.org/projects/realearn ，高。
- 可借设计：Source character（Range vs Encoder relative 1/2/3 vs Toggle-only）、Tag 分组、Virtual control 双 compartment 解耦、Glue（Mode/Takeover/Step/Retrigger/feedback_value_table）、Pot macro（bank→section→macro 三级：`target.fx_parameter.macro.{name,section,bank}`）。来源：https://docs.helgoboss.org/realearn/further-concepts/source.html 等 + https://docs.helgoboss.org/realearn/targets/pot.html ，高。
- spk77 包络 morph（GPL v3，只借逻辑）：`spk77_Create envelope points from FX param values.lua` 全量建包络做 preset 间 morph；DarkStar 警示数百参数灾难，只对变化参数建点。来源：https://raw.githubusercontent.com/ReaTeam/ReaScripts/master/Envelopes/spk77_Create%20envelope%20points%20from%20FX%20param%20values.lua + https://forum.cockos.com/showthread.php?t=178354 ，高。
- `.rpl` 私有文本库（`<REAPER_PRESET_LIBRARY>` + base64 chunk），ReaScript 无 `.rpl` API（A01 已验证）；`.vstpreset` 二进制走 `Vst::PresetFile`，REAPER 侧 `SetPreset` 接受全路径；CLAP 无统一预设格式。来源：https://github.com/geraintluff/jsfx/blob/master/atlantis-reverb.jsfx.rpl + https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/Locations+Format/Preset+Format.html ，高。
- ReaPack 本体 LGPL-3.0，SWS MIT：来源 https://github.com/cfillion/reapack/blob/master/COPYING + https://github.com/reaper-oss/sws/blob/master/COPYING ，高。

## 4 大参数量分层白名单

- VST3 `ParameterInfo{title,units,stepCount,unitId,flags}` + unit 树、CLAP `param_info{flags,name,module,min/max/default}`：REAPER 仅透出名，flags 不透出。分组有但稀疏，回落链：section/module→白名单group→Other 折叠。高。
- Kontakt：automation slot 手动白名单（`$CONTROL_PAR_AUTOMATION_ID 0..2047`，`ALLOW_AUTOMATION` 默认开但读回恒 0 实现坑）。来源：NI KSP 手册 + vi-control 实测，中。
- Serum 约 299 参数但矩阵 routing 不在 VST 参数里（`save_state` 能抓，`get/set_parameter` 抓不到）；Vital 有内建分组 + `macro_control_1..4` + `ValueDetails`。宏旋钮是官方白名单入口。来源：https://github.com/DBraun/DawDreamer/issues/212 + Vital doxygen，中。
- DecentSampler ⚠️需实测（未抓到官方暴露声明）。
- 业界三件套：白名单/宏为主、自动分组为辅、折叠 500+（默认只展 8–16 宏 + 变化参数）。
- 建议分层：L0 全量表（ident 持久化）→ L1 分组（section/白名单group/Other）→ L2 白名单宏（`whitelist.json`，Kontakt 只收 slot 子集，矩阵类标 `preset-only`）→ L3 手势层（抄 Glue + spk77 morph，自写）。

## 5 实测补记（2026-10-06，真机 v7.82，TSK-112）

证据：`experiments/b-matrix-01-stock.out.txt`、`b-matrix-02-ident-env.out.txt`、`b-matrix-03-vst3.out.txt`。

- ReaEQ（VST，19 参数）：全 automatable；值域归一 0..1；格式化可用；连续参数 step 全 0；ident 稳定（`0:_Freq_Low_Shelf`）；section 全空；VST 侧 `default_value` 缺席。
- ReaControlMIDI ch（param 8）step=0.0625（=1/16），toggle 参数 `istoggle=true`；印证“有意义 step 少数派”。
- JSFX：step/default 来自 slider 定义（`default_value` 可用）；普通参数 ident 为裸索引（仅 wet/bypass/delta 有语义 ident），持久化须特殊处理。
- `:wet/:bypass/:delta` 双向往返通过，非法返回 -1；`GetFXEnvelope(false)` 默认全 nil。
- 第三方 VST3：OTT 24 参数、Pro-Q 4 **740** 参数、Serum 2 FX **2625** 参数（白名单强制）；三家 `FormatParamValue*` 均可用（Cockos 扩展覆盖好于预期， vendor-dependent，仍保留失败分支）；Pro-Q 4 透出 VST3 unit（`Band 1`）——section 策略在优质 VST3 上成立；VST3 ident 为 `idx:paramID` 形。
- 本地化名存在（通道/正常），关键词须容错。
- CLAP（Vital，`experiments/b-matrix-04-clap.out.txt`）：906 参数，ident `idx:clapID` 形，`default_value`/格式化可用，section 全空（Vital 未填 module）；Macro 1–3 位于索引 211–213。同插件 VST3 版 **2986** 参数、CLAP 版 906——参数表跨格式不可移植，白名单必须按格式分别维护；但前序参数名/顺序与 Macro 位置一致，Macro 入口可按名复用。`:wet` 双格式均可解析。

## 6 ⚠️需实测（转 TSK）+ 20 行枚举脚本

必测 10 项：四格式元数据成功率；mid vs default；Format 失败模式；section 空率；单位/枚举解析率；ident 跨存盘稳定性；`.rpl` 能否经 preset API 遍历；500+ 参数耗时与折叠阈值；`GetFXEnvelope(false)` 与 automatable 不一致率；无 Begin 时单调 End 充分性。

```lua
local tr = reaper.GetSelectedTrack(0, 0) local fx = 0
local n = reaper.TrackFX_GetNumParams(tr, fx)
for i = 0, math.min(n - 1, 19) do
  local _, nm = reaper.TrackFX_GetParamName(tr, fx, i, "")
  local v, mn, mx = reaper.TrackFX_GetParam(tr, fx, i, 0, 0)
  local _, a, b, mid = reaper.TrackFX_GetParamEx(tr, fx, i, 0, 0, 0)
  local okF, fmt = reaper.TrackFX_GetFormattedParamValue(tr, fx, i, "")
  local okS, st, ss, ls, tg = reaper.TrackFX_GetParameterStepSizes(tr, fx, i, 0, 0, 0, false)
  local _, id = reaper.TrackFX_GetParamIdent(tr, fx, i, "")
  local sec = reaper.TrackFX_GetParamSectionName(tr, fx, i, "")
  local _, au = reaper.TrackFX_GetNamedConfigParm(tr, fx, "param."..i..".automatable", "")
  local _, dv = reaper.TrackFX_GetNamedConfigParm(tr, fx, "param."..i..".default_value", "")
  reaper.ShowConsoleMsg(string.format("%d|%s|v=%.4f mn=%.4f mx=%.4f mid=%.4f|fmt=%s(%s)|step=%s tg=%s|id=%s|sec=%s|auto=%s def=%s\n", i, nm, v, mn, mx, mid, fmt, tostring(okF), tostring(st), tostring(tg), id, sec, au, dv))
end
```

判定：`okF=false` 即 Cockos 扩展不支持；step 全 0 正常；`sec=""` 回落白名单；`au~=1.0` gate 掉；`dv=""` 勿做零点假设。
