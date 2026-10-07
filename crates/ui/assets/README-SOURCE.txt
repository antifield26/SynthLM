Noto Sans SC subset — source record (TSK-119)

Upstream: https://github.com/google/fonts/raw/main/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf
  (variable font, wght 100-900; name v2.004 / 241114210130, non-release)
  Downloaded 2026-10-06, 17,772,300 bytes (full copy kept out-of-repo at
  %TEMP%/synthlm-ui-tsk119/NotoSansSC-full.ttf; only the subset below is
  committed, per the >5MB rule).
License: SIL Open Font License 1.1 (see OFL.txt in this directory).
  Permissive for internal non-commercial use; registration in
  docs/LICENSES.md is left to the main session (subagent did not touch docs/).

Subset: NotoSansSC-subset.ttf (222,524 bytes, regenerated TSK-506)
Procedure (fonttools 4.66.1, 2026-10-06; same steps rerun 2026-10-07):
  1. instantiateVariableFont wght=400 (upstream default is Thin/100, too
     light for dark-theme UI body copy at small sizes).
  2. pyftsubset --text-file=charset.txt --unicodes="U+0020-007E"
     --layout-features="*" --no-hinting --desubroutinize.
  TSK-506 rerun used the same %TEMP% full copy (17,772,300 bytes intact)
  and the same two steps; output verified with zero missing glyphs for
  every charset.txt entry before commit.

Charset decision (charset.txt, 238 non-ASCII chars after TSK-506):
  - ASCII printable U+0020-U+007E (full range: status numbers, "dB",
    "100Hz", "Fc=440Hz", punctuation, undo descriptions, param values).
  - Exactly the non-ASCII characters in UI string literals in
    crates/ui/src/*.rs (TSK-119 base: 主控/波形/状态/候选/空态/试听/
    应用/回滚/置信度/改动参数/缩放/滤波器/截止/生产者/演示… plus
    middle dot U+00B7, em dash U+2014, Greek Delta U+0394 for ΔLUFS,
    full-width parens U+FF08/FF09 and full-width vertical bar U+FF5C;
    TSK-506 adds card/preview/red-row copy: 差异/胜出/预览/指令/事务/
    参数表/执行/回滚描述/快照/派生/引用/令牌/条目/失败… full list is
    charset.txt itself).
  - PLUS the CJK characters in experiments/e2e-demo/demo-plan.json
    (candidate difference sentences and intent copy): the wired demo
    renders that file's text, so shipping it as tofu is not acceptable.
    This is a demo-scoped extension, not a general fix: future
    model-generated sentences carry arbitrary CJK the subset cannot
    cover (full-font shipping or dynamic fallback is out of scope,
    left to the main session).
  - Deliberately NOT full GB2312/Big5 coverage: the UI ships fixed copy;
    any new CJK string must extend charset.txt (enforced by the
    ui_copy_covered_by_subset_charset unit test) and regenerate the subset.
  - Doc-comment-only symbols (Rightarrow/section marks) are excluded.
