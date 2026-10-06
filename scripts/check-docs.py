"""Doc-sync gate for CI (TSK-003): headers, DEC count, banned phrases, AGENTS chapters."""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FAILURES: list[str] = []


def fail(msg: str) -> None:
    FAILURES.append(msg)


HEADER_FIELDS = ["目的", "适用范围", "状态", "最后核验日期", "依赖文档"]
DOC_FILES = sorted((ROOT / "docs").rglob("*.md"))
# TASK-INDEX uses a different but compatible header; still requires the 5 fields.
for path in DOC_FILES:
    if path.name == "LICENSES.md":
        continue
    rel = path.relative_to(ROOT).as_posix()
    try:
        text = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError) as exc:
        fail(f"{rel}: unreadable as UTF-8 ({exc})")
        continue
    for field in HEADER_FIELDS:
        if field not in text:
            fail(f"{rel}: missing header field '{field}'")

dec = (ROOT / "docs" / "DECISIONS.md").read_text(encoding="utf-8")
dec_ids = re.findall(r"^DEC-\d{3}", dec, flags=re.M)
if len(set(dec_ids)) < 25:
    fail(f"DECISIONS.md: only {len(set(dec_ids))} unique DEC ids (<25)")
dec_lines = [
    line
    for line in dec.splitlines()
    if line.startswith(("推荐：", "反转条件：", "选项：", "背景："))
]
dec_body = "\n".join(dec_lines)
for banned in ("视情况而定", "平衡考虑"):
    if banned in dec_body:
        fail(f"DECISIONS.md: banned phrase '{banned}' present in DEC body")

agents = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
for n in range(1, 9):
    if not re.search(rf"^## {n} ", agents, flags=re.M):
        fail(f"AGENTS.md: missing chapter '## {n} '")

tasks = (ROOT / "docs" / "TASK-INDEX.md").read_text(encoding="utf-8")
for token in ("TSK-", "Todo", "Blocked", "Done"):
    if token not in tasks:
        fail(f"TASK-INDEX.md: missing token '{token}'")

if FAILURES:
    print("doc-sync FAILED:")
    for f in FAILURES:
        print(f"  - {f}")
    sys.exit(1)
print(f"doc-sync OK ({len(DOC_FILES)} docs, {len(set(dec_ids))} DECs).")
