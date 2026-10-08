"""Blind-listening scoresheet pipeline (TSK-807): generate / score / selftest.

Why this exists: the blind-listening acceptance class ("Spearman ρ ≥ 0.5")
is recorded in `docs/REPORTS.md` as prose only (§3.3/§4), and the round-2
harness `crates/eval/tests/blind_calib2.rs` writes its clips and answer key
into `%TEMP%` — nothing in the repository keeps the human's raw ordering, so
the number cannot be re-checked after the fact. This script supplies the two
missing repository-side artifacts: a *scoresheet* the human fills in (rank per
clip, per series) and an *answer key* that stays outside the repository, so
the rater cannot see the ground truth while ranking.

Modes
-----
``generate``
    Write a blank scoresheet CSV (columns ``clip_id,series,human_rank,notes``)
    plus the answer key JSON. Clip ids are blinded (``clip01``…), the
    permutation is seeded, and the key holds the per-series ground-truth
    order (as clip ids, plus the underlying stimulus ids for audit). The key
    is written OUTSIDE the repository by default
    (``%TEMP%/synthlm-blind-<round>-key.json``), mirroring how
    `crates/eval/tests/blind_calib2.rs` keeps its key out of the blind
    directory. An existing sheet or key is never overwritten without
    ``--force`` (a half-filled sheet is human work).

``score``
    Read a *filled* scoresheet plus the key, print Spearman ρ per series and
    pooled (no paths, no personal data), and optionally append one row to a
    results markdown file (``--append docs/...``). Ranking uses average ranks
    for ties, so a rater who declares two clips equal is scored correctly
    instead of being rejected. Any empty ``human_rank`` cell refuses the run
    (no partial evidence); exit code is 1 when the sheet is incomplete or a
    series misses ``--min-rho`` (default 0.5), 0 otherwise.

``--selftest``
    Prove the maths on built-in fixtures — perfect agreement (ρ = +1.0),
    perfect reversal (ρ = −1.0) and a tie fixture (truth ranks
    ``[1,2,3,4]`` vs human ``[1,2,2,4]``, ρ = √0.9 ≈ +0.9487) — and exit
    non-zero if any fixture is off by more than 1e-12. The tie fixture is
    the sharp one: a tie-blind shortcut would print 0.95 and raw ranks
    without averaging would print 0.9.

Ranking convention: ``human_rank`` is the position of the clip inside its
series (1 = first). The key's ``ground_truth_order`` lists the clip ids in the
same direction (for example dark→bright), so ρ = 1 means the human reproduced
the ground truth exactly.

Privacy (AGENTS §8): only counts, clip/stimulus ids and ρ values are printed
or stored in the markdown row. Absolute locations never appear — an
out-of-repo file is shown as ``%TEMP%/<name>`` or ``外部（仓库外）/<name>``.

Usage::

    python scripts/blind_scores.py generate --round demo \
        --sheet %TEMP%/synthlm-blind-demo-sheet.csv
    python scripts/blind_scores.py score \
        --sheet %TEMP%/synthlm-blind-demo-sheet.csv \
        --key %TEMP%/synthlm-blind-demo-key.json
    python scripts/blind_scores.py score --sheet sheet.csv --key key.json \
        --append experiments/blind-results.md
    python scripts/blind_scores.py --selftest
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

#: Repository root (this file lives in `scripts/`).
REPO_ROOT = Path(__file__).resolve().parent.parent

#: Scoresheet columns, in order. `notes` is free text for the rater.
SHEET_COLUMNS = ("clip_id", "series", "human_rank", "notes")

#: Columns `score` insists on; `notes` is optional for hand-made sheets.
REQUIRED_COLUMNS = ("clip_id", "series", "human_rank")

#: Canonical header/separator of the results markdown table (`--append`).
RESULTS_HEADER = "| 日期 | round | 片段数 | 系列数 | 各系列 ρ | 合并 ρ | 阈值 | 判定 | 评分单 | 答案键 |"
RESULTS_SEPARATOR = "|---|---|---|---|---|---|---|---|---|---|"

#: Default acceptance threshold for ρ (per series and pooled).
DEFAULT_MIN_RHO = 0.5

#: Tolerance for the `--selftest` fixture assertions.
SELFTEST_TOL = 1e-12

#: Built-in series template: the TSK-705 round-2 series shapes
#: (`crates/eval/tests/blind_calib2.rs`), used when `--spec` is not given so
#: the pipeline is runnable end to end. It is a *shape* template: generating
#: a sheet from it produces no listening evidence by itself.
BUILTIN_SERIES = (
    ("B2", ("B1000", "B2500", "B6000", "B14000")),  # dark→bright
    ("T2", ("T120", "T040", "T010")),  # dull→sharp
    ("W", ("W04", "W08", "W16", "W32")),  # dull→rich
)


def permute(n: int, seed: int) -> list[int]:
    """Deterministic Fisher–Yates permutation of ``0..n`` (xorshift64*).

    Same algorithm, constants and loop direction as ``permute`` in
    `crates/eval/tests/blind_calib2.rs`, so a Rust round and a sheet
    generated here can share one blinding.
    """
    idx = list(range(n))
    s = seed if seed != 0 else 1
    mask = (1 << 64) - 1
    for i in range(n - 1, 0, -1):
        s ^= s >> 12
        s ^= (s << 25) & mask
        s ^= s >> 27
        s = (s * 0x2545_F491_4F6C_DD1D) & mask
        j = s % (i + 1)
        idx[i], idx[j] = idx[j], idx[i]
    return idx


def average_ranks(values: list[float]) -> list[float]:
    """Ranks of ``values`` (ascending), ties sharing their average rank."""
    order = sorted(range(len(values)), key=lambda i: values[i])
    ranks = [0.0] * len(values)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and values[order[j + 1]] == values[order[i]]:
            j += 1
        shared = (i + j) / 2.0 + 1.0
        for k in range(i, j + 1):
            ranks[order[k]] = shared
        i = j + 1
    return ranks


def spearman(xs: list[float], ys: list[float]) -> float | None:
    """Spearman ρ with average ranks for ties.

    Returns ``None`` when ρ is undefined: fewer than two pairs, or a
    constant sequence (zero rank variance, e.g. every clip tied).
    """
    if len(xs) != len(ys) or len(xs) < 2:
        return None
    rx, ry = average_ranks(xs), average_ranks(ys)
    n = len(rx)
    mx, my = sum(rx) / n, sum(ry) / n
    cov = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    vx = math.sqrt(sum((a - mx) ** 2 for a in rx))
    vy = math.sqrt(sum((b - my) ** 2 for b in ry))
    if vx == 0.0 or vy == 0.0:
        return None
    return cov / (vx * vy)


def stable_seed(text: str) -> int:
    """Deterministic 64-bit seed for a round name (no wall clock)."""
    digest = hashlib.sha256(text.encode("utf-8")).digest()
    return int.from_bytes(digest[:8], "big")


def display_ref(path: Path) -> str:
    """Path text safe for logs: repo-relative, `%TEMP%/<name>` or a label."""
    resolved = path.resolve()
    try:
        return resolved.relative_to(REPO_ROOT).as_posix()
    except ValueError:
        pass
    try:
        return f"%TEMP%/{resolved.relative_to(Path(tempfile.gettempdir()).resolve()).as_posix()}"
    except ValueError:
        return f"外部（仓库外）/{resolved.name}"


def inside_repo(path: Path) -> bool:
    """True when ``path`` resolves inside this repository."""
    try:
        path.resolve().relative_to(REPO_ROOT)
    except ValueError:
        return False
    return True


def load_spec(path: str | None) -> tuple[list[tuple[str, tuple[str, ...]]], str]:
    """Load the series spec, or fall back to [`BUILTIN_SERIES`].

    Spec JSON shape: ``{"series": [{"name": "B2", "ground_truth_order":
    ["B1000", ...]}, ...]}``. Returns ``(series, provenance)``.
    """
    if path is None:
        return list(BUILTIN_SERIES), "内置模板（TSK-705 系列形状；本身不是听音证据）"
    raw = json.loads(Path(path).read_text(encoding="utf-8"))
    series: list[tuple[str, tuple[str, ...]]] = []
    for entry in raw.get("series", []):
        name = str(entry["name"])
        order = tuple(str(s) for s in entry["ground_truth_order"])
        if len(order) < 2:
            raise ValueError(f"series {name}: ground_truth_order needs >= 2 ids")
        series.append((name, order))
    if not series:
        raise ValueError("spec has no series")
    return series, display_ref(Path(path))


def cmd_generate(args: argparse.Namespace) -> int:
    """Write a blank scoresheet plus the out-of-repo answer key."""
    try:
        series, spec_source = load_spec(args.spec)
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as err:
        print(f"REFUSED: spec unreadable ({type(err).__name__}); no files written")
        return 1

    sheet_path = Path(args.sheet)
    key_path = Path(args.key_out) if args.key_out else Path(
        tempfile.gettempdir()
    ) / f"synthlm-blind-{_safe_name(args.round)}-key.json"
    if inside_repo(key_path) and not args.allow_in_repo_key:
        print(
            "REFUSED: --key-out resolves inside the repository; the rater could "
            "read the ground truth. Pass a path outside the repo (the default "
            "is %TEMP%) or, only for a dry run, --allow-in-repo-key."
        )
        return 1
    if sheet_path.resolve() == key_path.resolve():
        print("REFUSED: --sheet and --key-out are the same file; the key must stay separate")
        return 1
    for path, label in ((sheet_path, "sheet"), (key_path, "key")):
        if path.exists() and not args.force:
            print(
                f"REFUSED: {label} already exists ({display_ref(path)}); "
                "pass --force to overwrite (a half-filled sheet is human work)"
            )
            return 1

    # Blinded clip ids: one seeded permutation over all stimuli.
    flat: list[tuple[str, str]] = [
        (name, stim) for name, order in series for stim in order
    ]
    order = permute(len(flat), args.seed)
    clips: list[tuple[str, str, str]] = []
    for position, stim_index in enumerate(order, start=1):
        name, stim = flat[stim_index]
        clips.append((f"clip{position:02d}", name, stim))

    series_order = sorted(name for name, _ in series)
    stim_to_clip = {(name, stim): clip for clip, name, stim in clips}
    key: dict[str, object] = {
        "round": args.round,
        "seed": args.seed,
        "created_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "ranking_convention": "human_rank = 该系列内排序位置（1 = 第一位），与 ground_truth_order 同向",
        "series_order": series_order,
        "series": {
            name: {
                "n": len(ground_truth),
                "ground_truth_order": [
                    stim_to_clip[(name, stim)] for stim in ground_truth
                ],
                "stimulus_order": list(ground_truth),
            }
            for name, ground_truth in series
        },
        "mapping": {
            clip: {"series": s, "stimulus": stim} for clip, s, stim in clips
        },
        "sheet_columns": list(SHEET_COLUMNS),
        "spec_source": spec_source,
    }

    key_path.parent.mkdir(parents=True, exist_ok=True)
    sheet_path.parent.mkdir(parents=True, exist_ok=True)
    key_path.write_text(json.dumps(key, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    with sheet_path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.writer(handle, lineterminator="\n")
        writer.writerow(SHEET_COLUMNS)
        for clip, name, _stim in clips:
            writer.writerow([clip, name, "", ""])

    print(f"round={args.round} seed={args.seed}")
    print(f"clips={len(clips)} series={len(series)}（盲编号 clip01..clip{len(clips):02d}）")
    for name, ground_truth in series:
        print(f"  series {name}: n={len(ground_truth)}")
    print(f"sheet -> {display_ref(sheet_path)}")
    print(f"key   -> {display_ref(key_path)}（盲态隔离：评分前不要给评分人看）")
    print(f"spec  -> {spec_source}")
    print("next: 听音后填写 human_rank（1 = 该系列第一位），再运行 score")
    return 0


def _safe_name(text: str) -> str:
    """Round name reduced to filename-safe characters."""
    cleaned = "".join(c if c.isalnum() or c in "-_" else "-" for c in text)
    return cleaned or "round"


def read_sheet(path: Path) -> tuple[list[dict[str, str]], list[str]]:
    """Read the scoresheet; returns ``(rows, missing_columns)``."""
    with path.open("r", encoding="utf-8-sig", newline="") as handle:
        reader = csv.DictReader(handle)
        columns = reader.fieldnames or []
        missing = [c for c in REQUIRED_COLUMNS if c not in columns]
        rows = [{k: (v or "").strip() for k, v in row.items() if k} for row in reader]
    return rows, missing


def cmd_score(args: argparse.Namespace) -> int:
    """Score a filled sheet against the key; optionally append a results row."""
    try:
        key = json.loads(Path(args.key).read_text(encoding="utf-8"))
        rows, missing_columns = read_sheet(Path(args.sheet))
    except (OSError, json.JSONDecodeError) as err:
        print(f"REFUSED: sheet/key unreadable ({type(err).__name__}); nothing scored")
        return 1
    if missing_columns:
        print(f"REFUSED: sheet is missing column(s) {', '.join(missing_columns)}")
        return 1

    mapping: dict[str, dict[str, str]] = key.get("mapping", {})
    series_key: dict[str, dict[str, object]] = key.get("series", {})
    round_name = str(key.get("round", "?"))

    blank = [row.get("clip_id", "") for row in rows if not row.get("human_rank", "")]
    if blank:
        shown = ", ".join(blank[:5]) + ("…" if len(blank) > 5 else "")
        print(
            f"REFUSED: {len(blank)}/{len(rows)} human_rank cells are empty "
            f"({shown}); no partial evidence is scored or appended"
        )
        return 1

    unknown = [row.get("clip_id", "") for row in rows if row.get("clip_id", "") not in mapping]
    if unknown:
        print(f"REFUSED: sheet has {len(unknown)} clip id(s) absent from the key: {', '.join(unknown[:5])}")
        return 1
    seen = {row.get("clip_id", "") for row in rows}
    if len(seen) != len(rows):
        print(f"REFUSED: sheet repeats clip id(s) ({len(rows)} rows, {len(seen)} unique)")
        return 1
    absent = sorted(set(mapping) - seen)
    if absent:
        print(f"REFUSED: sheet is missing {len(absent)} clip(s) of round {round_name}: {', '.join(absent[:5])}")
        return 1

    by_series: dict[str, list[tuple[str, int]]] = {name: [] for name in series_key}
    for row in rows:
        clip = row["clip_id"]
        series = mapping[clip]["series"]
        raw = row["human_rank"]
        try:
            rank = int(raw)
        except ValueError:
            print(f"REFUSED: clip {clip} has non-integer human_rank '{raw}'")
            return 1
        by_series.setdefault(series, []).append((clip, rank))

    per_series: list[tuple[str, int, float | None, bool]] = []
    for name in sorted(by_series):
        entries = by_series[name]
        n = len(entries)
        if n == 0:
            print(f"REFUSED: series {name} is declared in the key but has no clips in the sheet")
            return 1
        ranks = [rank for _clip, rank in entries]
        if min(ranks) != 1 or max(ranks) > n:
            print(
                f"REFUSED: series {name} ranks must lie in 1..={n} and include 1 "
                f"(got {sorted(ranks)})"
            )
            return 1
        truth = list(series_key.get(name, {}).get("ground_truth_order", []))
        truth_rank = {clip: i + 1 for i, clip in enumerate(truth)}
        if sorted(truth_rank) != sorted(clip for clip, _ in entries):
            print(f"REFUSED: series {name} sheet/key clip sets differ")
            return 1
        rho = spearman(ranks, [truth_rank[clip] for clip, _ in entries])
        per_series.append((name, n, rho, rho is not None and rho >= args.min_rho))

    pooled_human: list[float] = []
    pooled_truth: list[float] = []
    for name, _n, _rho, _ok in per_series:
        truth_rank = {
            clip: i + 1
            for i, clip in enumerate(series_key.get(name, {}).get("ground_truth_order", []))
        }
        for clip, rank in sorted(by_series[name]):
            pooled_human.append(float(rank))
            pooled_truth.append(float(truth_rank[clip]))
    pooled = spearman(pooled_human, pooled_truth)

    print(f"blind_scores score round={round_name} clips={len(rows)} series={len(per_series)}")
    for name, n, rho, ok in per_series:
        print(f"  series {name}: n={n} rho={_fmt_rho(rho)} min_rho={args.min_rho:.2f} pass={'yes' if ok else 'no'}")
    all_pass = all(ok for _name, _n, _rho, ok in per_series) and pooled is not None and pooled >= args.min_rho
    print(f"  pooled: n={len(pooled_human)} rho={_fmt_rho(pooled)} pass={'yes' if all_pass else 'no'}")

    if args.append:
        verdict = "通过" if all_pass else "未通过"
        series_text = " / ".join(f"{name}={_fmt_rho(rho)}" for name, _n, rho, _ok in per_series)
        key_ref = (
            "仓库内（违规：评分前应移出）"
            if inside_repo(Path(args.key))
            else "仓库外（盲态）"
        )
        row = "| {} | {} | {} | {} | {} | {} | {:.2f} | {} | {} | {} |".format(
            datetime.now().strftime("%Y-%m-%d"),
            round_name,
            len(rows),
            len(per_series),
            series_text,
            _fmt_rho(pooled),
            args.min_rho,
            verdict,
            display_ref(Path(args.sheet)),
            key_ref,
        )
        try:
            _append_row(Path(args.append), row)
        except (OSError, ValueError) as err:
            print(f"REFUSED: cannot append to {display_ref(Path(args.append))} ({err})")
            return 1
        print(f"appended 1 row -> {display_ref(Path(args.append))}")

    return 0 if all_pass else 1


def _fmt_rho(rho: float | None) -> str:
    """ρ as a signed 3-decimal string, or ``n/a`` when undefined."""
    return "n/a（全部并列）" if rho is None else f"{rho:+.3f}"


def _append_row(path: Path, row: str) -> None:
    """Append one results row, creating the canonical table when absent."""
    if path.exists():
        text = path.read_text(encoding="utf-8")
        if RESULTS_HEADER not in text:
            raise ValueError("file has no canonical results table header")
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{RESULTS_HEADER}\n{RESULTS_SEPARATOR}\n", encoding="utf-8")
    with path.open("a", encoding="utf-8", newline="") as handle:
        handle.write(row + "\n")


def cmd_selftest() -> int:
    """Prove the ρ implementation on three built-in fixtures."""
    fixtures = [
        (
            "perfect agreement",
            [1.0, 2.0, 3.0, 4.0],
            [1.0, 2.0, 3.0, 4.0],
            1.0,
        ),
        (
            "perfect reversal",
            [1.0, 2.0, 3.0, 4.0],
            [4.0, 3.0, 2.0, 1.0],
            -1.0,
        ),
        (
            "tie (average ranks)",
            [1.0, 2.0, 3.0, 4.0],
            [1.0, 2.0, 2.0, 4.0],
            math.sqrt(0.9),
        ),
    ]
    failures = 0
    print(f"blind_scores selftest ({len(fixtures)} fixtures)")
    for label, truth, human, expected in fixtures:
        rho = spearman(human, truth)
        ok = rho is not None and abs(rho - expected) <= SELFTEST_TOL
        failures += 0 if ok else 1
        shown = rho if rho is not None else float("nan")
        print(
            f"  {label}: n={len(truth)} rho={shown:+.4f} expected={expected:+.4f} "
            f"{'OK' if ok else 'MISMATCH'}"
        )
    if failures:
        print(f"SELFTEST FAILED: {failures} fixture(s) wrong")
        return 1
    print("selftest OK: rho = +1.0000 (agreement), -1.0000 (reversal), +0.9487 (ties, sqrt(0.9))")
    return 0


def build_parser() -> argparse.ArgumentParser:
    """CLI definition for all three modes."""
    parser = argparse.ArgumentParser(
        prog="blind_scores.py",
        description="Blind-listening scoresheet pipeline (TSK-807): generate a blank "
        "sheet + out-of-repo answer key, score a filled sheet with Spearman rho, "
        "or run --selftest.",
    )
    parser.add_argument(
        "--selftest",
        action="store_true",
        help="prove the Spearman implementation on built-in fixtures and exit",
    )
    sub = parser.add_subparsers(dest="command")

    generate = sub.add_parser("generate", help="write a blank sheet + answer key")
    generate.add_argument("--round", default="demo", help="round label (default: demo)")
    generate.add_argument("--sheet", required=True, help="scoresheet CSV to create")
    generate.add_argument(
        "--key-out",
        default=None,
        help="answer key JSON; default %%TEMP%%/synthlm-blind-<round>-key.json "
        "(outside the repo on purpose)",
    )
    generate.add_argument(
        "--seed", type=int, default=None, help="blinding seed (default: stable hash of --round)"
    )
    generate.add_argument("--spec", default=None, help="optional series spec JSON")
    generate.add_argument(
        "--force", action="store_true", help="overwrite an existing sheet/key"
    )
    generate.add_argument(
        "--allow-in-repo-key",
        action="store_true",
        help="allow a key inside the repo (breaks blindness; dry runs only)",
    )
    generate.set_defaults(func=cmd_generate)

    score = sub.add_parser("score", help="score a filled sheet against the key")
    score.add_argument("--sheet", required=True, help="filled scoresheet CSV")
    score.add_argument("--key", required=True, help="answer key JSON")
    score.add_argument(
        "--append", default=None, help="markdown results file to append one row to"
    )
    score.add_argument(
        "--min-rho", type=float, default=DEFAULT_MIN_RHO, help="acceptance threshold (default: 0.5)"
    )
    score.set_defaults(func=cmd_score)
    return parser


def main(argv: list[str] | None = None) -> int:
    """Entry point; returns the process exit code."""
    _harden_stdout()
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.selftest:
        return cmd_selftest()
    if not getattr(args, "command", None):
        parser.print_help()
        return 1
    if args.command == "generate" and args.seed is None:
        args.seed = stable_seed(args.round)
    return int(args.func(args))


def _harden_stdout() -> None:
    """Never crash on a console codepage that lacks a character.

    The report prints ρ and CJK labels; a legacy Windows codepage (GBK here)
    may not encode every one of them, and a traceback after the work is done
    would report failure for a successful run.
    """
    reconfigure = getattr(sys.stdout, "reconfigure", None)
    if reconfigure is not None:
        try:
            reconfigure(errors="replace")
        except (ValueError, OSError):
            pass


if __name__ == "__main__":
    sys.exit(main())
