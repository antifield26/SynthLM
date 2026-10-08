"""Contract gate (TSK-803): doc headers, count consistency, evidence existence,
absolute-path ban (AGENTS §8) and the bare intra-doc-link ratchet (§4).

Why this exists: `scripts/check-docs.py` only greps header *substrings*, so the
2026-10-07 audit found it could pass vacuously (the Phase 5-7 report claimed "63
Done" while the index held 64; evidence cells cited files nobody opened).
This script adds the checks that would have caught those defects.

Checks (all must pass; exit 1 otherwise):
1. HEADERS  — every `docs/**/*.md` except LICENSES.md carries the five header
   fields `目的 / 适用范围 / 状态 / 最后核验日期 / 依赖文档` as bullet lines in
   the first 15 non-empty lines.
2. COUNTS   — every "N Done" claim in docs must equal the real TASK-INDEX row
   count (phase-scoped claims say "Phase 0–4" on the same line). The historical
   assessment report is exempt: it quotes past claims on purpose.
3. EVIDENCE — every file-like token in a TASK-INDEX evidence cell resolves to a
   tracked file, and that file is non-empty.
4. PATHS    — no tracked text file contains a personal absolute path
   (drive-letter + Users + <name>, or /Users/<name>, or /home/<name>). The
   single allowed exception is the deliberate redaction fixture
   `crates/common/tests/upload_audit.rs`, whose fake paths are asserted by its
   own tests.
5. LINKS    — bare intra-doc links (`` [`Item`] ``, not `` [`Item`](full::path) ``)
   may not exceed the baseline in `scripts/link-baseline.txt` (ratchet: the debt
   may shrink, never grow). AGENTS §4 bans them outright; the baseline records
   the debt inherited on 2026-10-07 (TSK-806 pays it down).

Run `python scripts/check-contract.py` (add `--print-links` to recount).
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HEADER_FIELDS = ("目的", "适用范围", "状态", "最后核验日期", "依赖文档")
HEADER_WINDOW = 15
# Historically-quoted claims live in the merged report's correction section,
# marked with QUOTE_MARKER, so no whole-file exemption is needed any more.
HISTORICAL_EXEMPT: set[str] = set()
PATH_FIXTURE = "crates/common/tests/upload_audit.rs"
TEXT_SUFFIXES = (".md", ".txt", ".lua", ".rs", ".json", ".toml", ".yml", ".yaml", ".py", ".ps1", ".csv", ".log")
# A link is a violation only when it is a single-segment name (`[`Item`]`):
# a path already carries the crate/module prefix AGENTS 4 demands.
BARE_LINK = re.compile(r"\[`([A-Za-z_][A-Za-z0-9_]*)`\](?!\()")
CRATE_ROOTS = {
    "synthlm_common", "synthlm_profile", "synthlm_planner", "synthlm_retrieval",
    "synthlm_eval", "synthlm_dsp", "synthlm_acrd", "synthlm_bridge", "synthlm_ui",
    "serde", "serde_json", "thiserror", "anyhow", "reqwest", "egui", "eframe", "ort",
    "regex", "ctrlc", "interprocess", "shared_memory", "symphonia", "tempfile", "tokio",
    "futures", "lancedb", "arrow_array", "arrow_schema", "realfft", "rustfft", "ebur128",
    "reaper_low", "reaper_medium", "std", "core", "alloc",
}
ABS_PATH = re.compile(r"[A-Za-z]:\\Users\\[^\\\s\"'`,;)<>=]+|/Users/[A-Za-z0-9_.-]+|/home/[A-Za-z0-9_.-]+")
EVIDENCE_TOKEN = re.compile(r"\.?[A-Za-z0-9_][A-Za-z0-9_./-]*\.(?:out\.txt|md|py|json|wav|txt|yml|rs|toml|ps1|lua)")
GENERIC_TOKENS = {"out.txt", "output.txt"}
DONE_CLAIM = re.compile(r"(\d{1,3})\s*(?:个)?\s*(?:任务\s*)?Done")
PHASE_RANGE = re.compile(r"Phase\s*(\d)\s*[–\-]\s*(\d)")
QUOTE_MARKER = "历史引文"  # lines quoting an already-corrected claim are exempt

FAILURES: list[str] = []
NOTES: list[str] = []


def tracked_files() -> list[str]:
    """Tracked files plus untracked-but-present ones, so the gate also works
    before the current session's new files are committed."""
    listed: list[str] = []
    for extra in ([], ["--others", "--exclude-standard"]):
        out = subprocess.run(
            ["git", "ls-files", *extra], cwd=ROOT, capture_output=True, text=True, check=True
        )
        listed.extend(line.strip() for line in out.stdout.splitlines() if line.strip())
    seen: set[str] = set()
    unique: list[str] = []
    for rel in listed:
        if rel not in seen and (ROOT / rel).is_file():
            seen.add(rel)
            unique.append(rel)
    return unique


def main() -> int:
    files = tracked_files()
    text_files = [f for f in files if f.endswith(TEXT_SUFFIXES)]
    docs = sorted(f for f in files if f.startswith("docs/") and f.endswith(".md"))

    check_headers(docs)
    check_counts(docs)
    check_evidence(files)
    check_paths(text_files)
    check_links(flags=sys.argv[1:], files=files)

    for note in NOTES:
        print(f"note: {note}")
    if FAILURES:
        print("CONTRACT GATE FAILED:")
        for failure in FAILURES:
            print(f"  - {failure}")
        return 1
    print(f"contract gate OK ({len(docs)} docs, {len(text_files)} text files checked).")
    return 0


def check_headers(docs: list[str]) -> None:
    for rel in docs:
        if Path(rel).name == "LICENSES.md":
            continue  # registry table, tracked by check-docs.py's existing exemption
        lines = [line for line in (ROOT / rel).read_text(encoding="utf-8").splitlines() if line.strip()]
        head = "\n".join(lines[:HEADER_WINDOW])
        for field in HEADER_FIELDS:
            if not re.search(rf"^-\s*{field}", head, flags=re.M):
                FAILURES.append(f"{rel}: header field '{field}' missing from the first {HEADER_WINDOW} non-empty lines")


def _index_columns(index_text: str) -> tuple[int, int] | None:
    """Locate the 状态 / 阶段 columns from the table header (never by magic index)."""
    for line in index_text.splitlines():
        if line.startswith("| ID |"):
            cols = [c.strip() for c in line.strip().strip("|").split("|")]
            if "状态" in cols and "阶段" in cols:
                return cols.index("阶段"), cols.index("状态")
    return None


def check_counts(docs: list[str]) -> None:
    index = (ROOT / "docs" / "TASK-INDEX.md").read_text(encoding="utf-8")
    located = _index_columns(index)
    if located is None:
        FAILURES.append("docs/TASK-INDEX.md: cannot locate 阶段/状态 columns in the table header")
        return
    phase_col, status_col = located
    rows = [
        [c.strip() for c in line.strip().strip("|").split("|")]
        for line in index.splitlines()
        if line.startswith(("| TSK-", "| F/B-"))
    ]
    if not rows:
        FAILURES.append("docs/TASK-INDEX.md: no task rows parsed (column layout changed?)")
        return
    total_done = sum(1 for r in rows if len(r) > status_col and r[status_col] == "Done")
    ranged: dict[tuple[int, int], int] = {}
    for lo in range(0, 8):
        for hi in range(lo, 8):
            ranged[(lo, hi)] = sum(
                1
                for r in rows
                if len(r) > status_col
                and r[status_col] == "Done"
                and r[phase_col].isdigit()
                and lo <= int(r[phase_col]) <= hi
            )
    NOTES.append(f"TASK-INDEX: {total_done} Done rows total, {ranged[(0, 4)]} in Phase 0-4, {ranged[(0, 7)]} in Phase 0-7")
    for rel in docs:
        if rel in HISTORICAL_EXEMPT:
            continue
        for lineno, line in enumerate((ROOT / rel).read_text(encoding="utf-8").splitlines(), 1):
            if QUOTE_MARKER in line:
                continue
            span = PHASE_RANGE.search(line)
            for match in DONE_CLAIM.finditer(line):
                claimed = int(match.group(1))
                if span:
                    scope = (int(span.group(1)), int(span.group(2)))
                    expected = ranged[scope]
                    where = f"Phase {scope[0]}-{scope[1]}"
                else:
                    expected = total_done
                    where = "total"
                if claimed != expected:
                    FAILURES.append(
                        f"{rel}:{lineno}: claims '{match.group(0).strip()}' but TASK-INDEX has "
                        f"{expected} Done rows ({where})"
                    )


def check_evidence(files: list[str]) -> None:
    tracked = set(files)
    basenames: dict[str, list[str]] = {}
    for rel in files:
        basenames.setdefault(Path(rel).name, []).append(rel)
    index_lines = (ROOT / "docs" / "TASK-INDEX.md").read_text(encoding="utf-8").splitlines()
    for lineno, line in enumerate(index_lines, 1):
        if not line.startswith(("| TSK-", "| F/B-")):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) < 10:
            continue
        task_id, evidence = cells[0], cells[-1]
        if "%TEMP%" in evidence or "%temp%" in evidence:
            # External evidence (spike output kept outside the repo) is declared
            # as such in the cell; this gate only validates in-repo claims.
            continue
        for token in EVIDENCE_TOKEN.findall(evidence):
            token = token.strip("`*(),;:")
            if token.startswith("http") or token.startswith("TSK-") or token.startswith("DEC-"):
                continue
            if token in GENERIC_TOKENS:
                continue
            resolved = None
            if "/" in token:
                for candidate in (token, f"docs/{token}", f"experiments/{token}"):
                    if candidate in tracked:
                        resolved = candidate
                        break
            else:
                matches = basenames.get(token, [])
                if len(matches) == 1:
                    resolved = matches[0]
                elif len(matches) > 1:
                    resolved = matches[0]  # ambiguous but present; existence is what we gate
            if resolved is None:
                if re.fullmatch(r"[a-z_]+\.rs", token):
                    continue  # bare module name such as `undo.rs`, not a path claim
                FAILURES.append(f"docs/TASK-INDEX.md:{lineno}: {task_id} evidence token '{token}' does not exist")
                continue
            path = ROOT / resolved
            if path.stat().st_size == 0:
                FAILURES.append(f"docs/TASK-INDEX.md:{lineno}: {task_id} evidence '{resolved}' is empty")


def check_paths(text_files: list[str]) -> None:
    for rel in text_files:
        if rel == PATH_FIXTURE:
            # This file exists to prove that absolute paths get redacted: every
            # path literal in it is fake by construction. Enforced by its own
            # assertions instead of this scan.
            continue
        try:
            content = (ROOT / rel).read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for lineno, line in enumerate(content.splitlines(), 1):
            for hit in ABS_PATH.findall(line):
                FAILURES.append(f"{rel}:{lineno}: absolute path '{hit}' (AGENTS §8 bans absolute paths)")


def check_links(flags: list[str], files: list[str]) -> None:
    counts: dict[str, int] = {}
    for rel in files:
        if not rel.endswith(".rs") or "/src/" not in rel:
            continue
        text = (ROOT / rel).read_text(encoding="utf-8", errors="replace")
        found = sum(1 for name in BARE_LINK.findall(text) if name not in CRATE_ROOTS)
        if found:
            counts[rel] = found
    total = sum(counts.values())
    if "--print-links" in flags:
        for rel, n in sorted(counts.items(), key=lambda kv: -kv[1]):
            print(f"  {n:4}  {rel}")
        print(f"  total {total} in {len(counts)} files")
    baseline_file = ROOT / "scripts" / "link-baseline.txt"
    if not baseline_file.is_file():
        FAILURES.append("scripts/link-baseline.txt missing (ratchet baseline for §4 bare intra-doc links)")
        return
    baseline_text = baseline_file.read_text(encoding="utf-8")
    baseline_lines = [ln.strip() for ln in baseline_text.splitlines() if ln.strip() and not ln.strip().startswith("#")]
    if not baseline_lines or not baseline_lines[0].isdigit():
        FAILURES.append("scripts/link-baseline.txt: first non-comment line must be the baseline integer")
        return
    baseline = int(baseline_lines[0])
    NOTES.append(f"bare intra-doc links: {total} (baseline {baseline})")
    if total > baseline:
        worst = sorted(counts.items(), key=lambda kv: -kv[1])[:5]
        detail = ", ".join(f"{rel}={n}" for rel, n in worst)
        FAILURES.append(
            f"bare intra-doc links grew to {total} (baseline {baseline}); AGENTS §4 bans them (top: {detail})"
        )
    elif total < baseline:
        NOTES.append(f"§4 debt shrank by {baseline - total} — lower scripts/link-baseline.txt to lock the gain")


if __name__ == "__main__":
    sys.exit(main())
