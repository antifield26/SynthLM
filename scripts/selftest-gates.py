"""Self-test for the contract gates (TSK-802/803 acceptance).

A gate that cannot fail is decoration. This script plants one violation per
check, asserts the gate exits non-zero with the expected message, then restores
the workspace with `git checkout --` and asserts the gate is green again.

Covered:
  check-deps.py     : bridge -> eval dependency (DEC-022 deny-list)
  check-contract.py : bogus "N Done" claim, dangling TASK-INDEX evidence token,
                      missing doc header field, planted absolute path,
                      bare intra-doc link growth beyond the ratchet baseline

Requires a clean worktree (mutations are reverted with git). Exit 0 = all
mutations were caught and the gates are green afterwards.

Run: `python scripts/selftest-gates.py`
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
FAKE_PATH = "C:" + "\\" + "Users" + "\\" + "example" + "\\" + "secret.txt"
RESULTS: list[tuple[str, bool, str]] = []


def run_script(script: str) -> tuple[int, str]:
    proc = subprocess.run(
        [sys.executable, f"scripts/{script}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    body = (proc.stdout or "") + (proc.stderr or "")
    detail = [ln.strip() for ln in body.splitlines() if ln.strip() and "GATE FAILED" not in ln]
    return proc.returncode, (detail[-1] if detail else "")


def git_restore(*paths: str) -> None:
    subprocess.run(["git", "checkout", "--", *paths], cwd=ROOT, check=True)


def expect_violation(name: str, script: str, marker: str, apply, undo) -> None:
    apply()
    try:
        code, detail = run_script(script)
        caught = code == 1 and marker in detail
        RESULTS.append((name, caught, f"exit={code} :: {detail[:110]}"))
    finally:
        undo()
    code, detail = run_script(script)
    RESULTS.append((name + " [restored]", code == 0, f"exit={code} :: {detail[:110]}"))


def require_clean_worktree() -> None:
    out = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, capture_output=True, text=True, check=True)
    modified = [ln for ln in out.stdout.splitlines() if ln.strip() and not ln.startswith("??")]
    if modified:
        print("selftest needs a clean worktree (untracked files are fine); commit or stash first:")
        print("\n".join(modified))
        sys.exit(2)


def main() -> int:
    require_clean_worktree()

    bridge = ROOT / "crates" / "bridge" / "Cargo.toml"
    bridge_original = bridge.read_text(encoding="utf-8")

    def plant_dep() -> None:
        bridge.write_text(
            bridge_original.replace('thiserror = "2"', 'thiserror = "2"\nsynthlm-eval = { path = "../eval" }', 1),
            encoding="utf-8",
        )

    expect_violation(
        "deps: bridge -> eval planted",
        "check-deps.py",
        "DEC-022 forbids bridge",
        plant_dep,
        lambda: git_restore("crates/bridge/Cargo.toml"),
    )

    roadmap = ROOT / "docs" / "ROADMAP.md"
    roadmap_original = roadmap.read_text(encoding="utf-8")

    expect_violation(
        "contract: bogus '99 Done' claim",
        "check-contract.py",
        "claims '99 Done'",
        lambda: roadmap.write_text(roadmap_original + "\n- 99 Done\n", encoding="utf-8"),
        lambda: git_restore("docs/ROADMAP.md"),
    )

    index = ROOT / "docs" / "TASK-INDEX.md"
    index_original = index.read_text(encoding="utf-8")

    expect_violation(
        "contract: dangling evidence token",
        "check-contract.py",
        "does not exist",
        lambda: index.write_text(
            index_original.replace("scripts/check-deps.py + `.github/workflows/ci.yml`", "scripts/does-not-exist.py"),
            encoding="utf-8",
        ),
        lambda: git_restore("docs/TASK-INDEX.md"),
    )

    report = ROOT / "docs" / "M4-REPORT.md"
    report_original = report.read_text(encoding="utf-8")

    expect_violation(
        "contract: header field dropped",
        "check-contract.py",
        "header field '目的' missing",
        lambda: report.write_text(
            "\n".join(ln for ln in report_original.splitlines() if not ln.startswith("- 目的")), encoding="utf-8"
        ),
        lambda: git_restore("docs/M4-REPORT.md"),
    )

    probe = ROOT / "docs" / "TMP-PATH-PROBE.md"
    expect_violation(
        "contract: absolute path planted",
        "check-contract.py",
        "absolute path",
        lambda: probe.write_text(f"# probe\n\n- fake path: {FAKE_PATH}\n", encoding="utf-8"),
        lambda: probe.unlink(missing_ok=True),
    )

    dsp_lib = ROOT / "crates" / "dsp" / "src" / "lib.rs"
    dsp_original = dsp_lib.read_text(encoding="utf-8")

    expect_violation(
        "contract: bare doc link growth",
        "check-contract.py",
        "bare intra-doc links grew",
        lambda: dsp_lib.write_text(dsp_original + "\n//! [`Foo`]\n", encoding="utf-8"),
        lambda: git_restore("crates/dsp/src/lib.rs"),
    )

    for script in ("check-deps.py", "check-contract.py"):
        code, detail = run_script(script)
        RESULTS.append((f"final {script}", code == 0, f"exit={code} :: {detail[:110]}"))

    width = max(len(name) for name, _, _ in RESULTS)
    for name, ok, detail in RESULTS:
        print(f"{'PASS' if ok else 'FAIL'}  {name.ljust(width)}  {detail}")
    failed = [name for name, ok, _ in RESULTS if not ok]
    if failed:
        print(f"\nSELFTEST FAILED: {len(failed)} assertion(s): {', '.join(failed)}")
        return 1
    print(f"\nSELFTEST OK ({len(RESULTS)} assertions: every planted violation was caught, all gates green after restore).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
