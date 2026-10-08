"""E2E runbook witness-table tool (TSK-807): add / check.

Why this exists: `experiments/e2e-runbook.md` carries a timing table whose
acceptance value depends on *who* witnessed the run. Before this script the
table held exactly one row and its 见证人 was a sub-agent, so the runbook
criterion ("人类见证行") could be mistaken for satisfied — nothing in the
repository distinguished a human-witnessed run from a machine one. This tool
makes that distinction explicit and auditable:

``add``
    Write one row into the runbook's timing table with the fields
    ``日期 | seed | 步骤 0..5 | 合计 | 是否 ≤5min | 见证人``. The row goes into
    the trailing empty template row when there is one, otherwise it is
    inserted directly after the last table row (never at end of file, which
    would drop it out of the table). A row whose 见证人 reads like a machine
    (机器 / 子 Agent / subagent / machine / bot) is REFUSED unless
    ``--machine`` is passed explicitly, so a machine row can never be added by
    accident. ``--seed``, ``--total``, ``--within-5min`` and ``--witness`` are
    mandatory: a witness row without them is not evidence. The script records
    only what the caller passes — it never invents a name, a timestamp or a
    result.

``check``
    Parse the table and report how many rows are human-witnessed, machine,
    empty (template) or malformed. Exits non-zero with an explicit message
    while there is no human row, so CI and readers see the open gap instead of
    assuming the table is filled. Exits 0 once at least one human row exists.

Both modes validate the table header first and refuse to touch a file whose
columns do not match, so a mis-shaped row can never enter the artifact.

Usage::

    python scripts/runbook_witness.py check
    python scripts/runbook_witness.py add --seed 8 --steps "12s,40s,35s,18s,20s,6s" \
        --total 2m11s --within-5min yes --witness "张三（本人）"
    python scripts/runbook_witness.py add --seed 7 --total 1m --within-5min yes \
        --witness "子 Agent" --machine
"""

from __future__ import annotations

import argparse
import re
import sys
from datetime import date
from pathlib import Path
from typing import NamedTuple

#: Repository root (this file lives in `scripts/`).
REPO_ROOT = Path(__file__).resolve().parent.parent

#: Runbook carrying the witness table.
DEFAULT_RUNBOOK = REPO_ROOT / "experiments" / "e2e-runbook.md"

#: Canonical table columns, in order.
COLUMNS = (
    "日期",
    "seed",
    "步骤 0",
    "步骤 1",
    "步骤 2",
    "步骤 3",
    "步骤 4",
    "步骤 5",
    "合计",
    "是否 ≤5min",
    "见证人",
)

#: Number of step-timing columns (`步骤 0`..`步骤 5`).
STEP_COUNT = 6

#: Cells meaning "not recorded".
PLACEHOLDERS = {"", "—", "-", "–", "n/a", "N/A", "待填", "TBD"}

#: Witness text that marks a machine-witnessed row (case-insensitive).
MACHINE_MARKERS = ("机器", "子agent", "subagent", "sub-agent", "machine", "bot", "自动")

#: Values accepted by `--within-5min`, mapped to the table cell.
WITHIN_5MIN_CELLS = {"yes": "✅", "no": "❌"}


def is_machine_witness(text: str) -> bool:
    """True when the 见证人 cell reads like a machine witness."""
    normalized = re.sub(r"[\s（）()\[\]·,，、]+", "", text).lower()
    return any(marker in normalized for marker in MACHINE_MARKERS)


def is_placeholder(cell: str) -> bool:
    """True when a cell carries no recorded value (template/blank)."""
    stripped = cell.strip()
    core = stripped.strip("（）()[]【】<>《》 \t")
    return stripped in PLACEHOLDERS or core in PLACEHOLDERS


def classify(cell: str) -> str:
    """Classify a 见证人 cell: ``human`` / ``machine`` / ``empty``."""
    if is_placeholder(cell):
        return "empty"
    return "machine" if is_machine_witness(cell.strip()) else "human"


def split_row(line: str) -> list[str]:
    """Split a markdown table row into trimmed cells."""
    return [cell.strip() for cell in line.strip().strip("|").split("|")]


#: Separator row cell such as ``---``, ``:--:``.
SEPARATOR_CELL = re.compile(r":?-{1,}:?")


class Table(NamedTuple):
    """The timing table of the runbook, plus where a new row belongs."""

    lines: list[str]
    rows: list[tuple[int, list[str]]]
    block_end: int

    @property
    def last_row_is_template(self) -> bool:
        """True when the final row is an untouched template row."""
        return bool(self.rows) and all(is_placeholder(cell) for cell in self.rows[-1][1])


def find_table(path: Path) -> Table:
    """Return the timing table of ``path``.

    Raises ``ValueError`` when the file or the canonical header is missing.
    """
    if not path.is_file():
        raise ValueError(f"runbook not found: {path.name}")
    lines = path.read_text(encoding="utf-8").splitlines()
    header: list[str] = []
    header_index = -1
    for index, line in enumerate(lines):
        if not line.lstrip().startswith("|"):
            continue
        cells = split_row(line)
        if cells[:2] == list(COLUMNS[:2]):
            header, header_index = cells, index
            break
    if header_index < 0:
        raise ValueError("no timing table header found (expected 日期 | seed | ... | 见证人)")
    if header != list(COLUMNS):
        raise ValueError(
            "timing table columns changed: got " + " | ".join(header) + "; expected " + " | ".join(COLUMNS)
        )
    rows: list[tuple[int, list[str]]] = []
    block_end = len(lines)
    for index in range(header_index + 1, len(lines)):
        line = lines[index]
        if not line.lstrip().startswith("|"):
            block_end = index
            break
        cells = split_row(line)
        if all(SEPARATOR_CELL.fullmatch(cell) for cell in cells):
            continue  # separator row (an empty cell is a template row, not a separator)
        rows.append((index + 1, cells))
    return Table(lines=lines, rows=rows, block_end=block_end)


def cmd_check(args: argparse.Namespace) -> int:
    """Report human/machine/empty/malformed row counts; fail without a human row."""
    path = Path(args.file)
    try:
        table = find_table(path)
    except (OSError, ValueError) as err:
        print(f"REFUSED: {err}")
        return 1

    human: list[int] = []
    machine: list[int] = []
    empty: list[int] = []
    malformed: list[int] = []
    for line_no, cells in table.rows:
        if len(cells) != len(COLUMNS):
            malformed.append(line_no)
            continue
        kind = classify(cells[-1])
        {"human": human, "machine": machine, "empty": empty}[kind].append(line_no)

    print(f"runbook witness check: {path.name}")
    print(
        f"  rows={len(table.rows)} human={len(human)} machine={len(machine)} "
        f"empty={len(empty)} malformed={len(malformed)}"
    )
    if human:
        print(f"  human witness rows at lines: {', '.join(str(n) for n in human)}")
    if machine:
        print(f"  machine witness rows at lines: {', '.join(str(n) for n in machine)}")
    if empty:
        print(f"  empty/template rows at lines: {', '.join(str(n) for n in empty)}")
    if malformed:
        print(f"  malformed rows at lines: {', '.join(str(n) for n in malformed)}")
        print("REFUSED: malformed row(s) break the table; fix them before reading counts")
        return 1
    if not human:
        print(
            "NO HUMAN WITNESS ROW YET: TSK-807's runbook criterion stays open. "
            "A human must run experiments/e2e-runbook.md and record the row with "
            "`python scripts/runbook_witness.py add ...`."
        )
        return 1
    print(f"OK: {len(human)} human witness row(s) recorded")
    return 0


def cmd_add(args: argparse.Namespace) -> int:
    """Write one witness row, refusing machine rows without ``--machine``."""
    path = Path(args.file)
    try:
        table = find_table(path)
    except (OSError, ValueError) as err:
        print(f"REFUSED: {err}")
        return 1

    witness = args.witness.strip()
    fields = {
        "date": args.date,
        "witness": witness,
        "steps": list(args.steps),
        "total": args.total,
    }
    for name, value in fields.items():
        texts = value if isinstance(value, list) else [value]
        for text in texts:
            if "|" in text or "\n" in text or "\r" in text:
                print(f"REFUSED: {name} may not contain '|' or a line break")
                return 1
    if is_placeholder(witness):
        print("REFUSED: --witness is empty; a witness row without a witness is not evidence")
        return 1
    if is_machine_witness(witness) and not args.machine:
        print(
            f"REFUSED: 见证人 '{witness}' reads as a machine witness. Human-witnessed rows "
            "are what TSK-807 needs; pass --machine only when you really mean to record a "
            "machine row (check counts it separately)."
        )
        return 1
    if not args.within_5min:
        print("REFUSED: --within-5min {yes,no} is required (the column is the point)")
        return 1
    if is_placeholder(args.total):
        print("REFUSED: --total is required; a row without a total time is not evidence")
        return 1

    cells = [
        args.date,
        str(args.seed),
        *args.steps,
        args.total,
        WITHIN_5MIN_CELLS[args.within_5min],
        witness,
    ]
    row = "| " + " | ".join(cells) + " |"
    try:
        # Byte-level edit: everything outside the touched line stays exactly as
        # it was (including the file's own line-ending style).
        raw = path.read_bytes()
        newline = b"\r\n" if b"\r\n" in raw else b"\n"
        parts = raw.split(newline)
        if table.last_row_is_template:
            # Fill the trailing template row in place instead of leaving a
            # stale empty row above the real one.
            target = table.rows[-1][0]
            parts[target - 1] = row.encode("utf-8")
            where = f"filled the empty template row at line {target}"
        else:
            parts.insert(table.block_end, row.encode("utf-8"))
            where = f"inserted after the last table row (line {table.block_end + 1})"
        path.write_bytes(newline.join(parts))
    except OSError as err:
        print(f"REFUSED: cannot write {path.name} ({err})")
        return 1

    kind = "machine" if is_machine_witness(witness) else "human"
    print(f"wrote 1 {kind} witness row to {path.name} ({where})")
    print(f"  {row}")
    if kind == "machine":
        print("note: machine rows do not satisfy TSK-807's human-witness criterion")
    return 0


def _steps(text: str) -> list[str]:
    """Parse ``--steps`` into exactly [`STEP_COUNT`] cells."""
    values = [part.strip() for part in text.split(",")]
    if len(values) != STEP_COUNT:
        raise argparse.ArgumentTypeError(
            f"--steps needs {STEP_COUNT} comma-separated values (步骤 0..5), got {len(values)}"
        )
    return [value if not is_placeholder(value) else "—" for value in values]


def build_parser() -> argparse.ArgumentParser:
    """CLI definition for both modes."""
    parser = argparse.ArgumentParser(
        prog="runbook_witness.py",
        description="Add or audit human/machine witness rows in the E2E runbook table (TSK-807).",
    )
    parser.add_argument(
        "--file",
        default=str(DEFAULT_RUNBOOK),
        help="runbook markdown (default: experiments/e2e-runbook.md)",
    )
    sub = parser.add_subparsers(dest="command")

    add = sub.add_parser("add", help="append one witness row")
    add.add_argument("--date", default=date.today().isoformat(), help="run date (default: today)")
    add.add_argument("--seed", type=int, required=True, help="plan seed used for the run")
    add.add_argument(
        "--steps",
        type=_steps,
        default=["—"] * STEP_COUNT,
        help='six comma-separated step timings, e.g. "12s,40s,35s,18s,20s,6s"',
    )
    add.add_argument("--total", required=True, help="total elapsed time, e.g. 2m11s")
    add.add_argument(
        "--within-5min",
        choices=sorted(WITHIN_5MIN_CELLS),
        help="did the whole run stay within 5 minutes? (required)",
    )
    add.add_argument("--witness", required=True, help="who witnessed the run")
    add.add_argument(
        "--machine",
        action="store_true",
        help="explicitly allow a machine witness (机器/子 Agent); such a row never counts as human",
    )
    add.set_defaults(func=cmd_add)

    check = sub.add_parser("check", help="count human/machine/empty rows")
    check.set_defaults(func=cmd_check)
    return parser


def main(argv: list[str] | None = None) -> int:
    """Entry point; returns the process exit code."""
    _harden_stdout()
    parser = build_parser()
    args = parser.parse_args(argv)
    if not getattr(args, "command", None):
        parser.print_help()
        return 1
    return int(args.func(args))


def _harden_stdout() -> None:
    """Never crash on a console codepage that lacks a character.

    A Windows console on a legacy codepage (GBK here) cannot encode ``✅``,
    which is part of the table cell; without this the row would be written
    correctly and then the echo would abort the process with a traceback.
    """
    reconfigure = getattr(sys.stdout, "reconfigure", None)
    if reconfigure is not None:
        try:
            reconfigure(errors="replace")
        except (ValueError, OSError):
            pass


if __name__ == "__main__":
    sys.exit(main())
