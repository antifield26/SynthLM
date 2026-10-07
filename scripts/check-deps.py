"""DEC-022 crate-boundary gate (TSK-802).

Enforces the one-way crate DAG locked in `docs/DECISIONS.md` DEC-022:

    common <- profile <- planner / retrieval / eval <- acrd <- bridge <- ui
    (amendment TSK-205: `dsp` is a leaf that may depend on `common` only,
     and `acrd` may later depend on it)

Rules implemented
1. Rank rule: a crate may depend on crates of *strictly lower* rank only.
   Same-rank or upward edges are violations (the DEC calls the DAG one-way).
2. Direct-dep denylist: `bridge` must never depend on the model/analysis
   crates (`planner`/`retrieval`/`eval`/`dsp`), per DEC-022 option A and L8.
3. Optional/feature-gated dependencies are checked like normal ones.
4. `[dev-dependencies]` are NOT part of the shipped DAG: an upward dev-edge is
   reported as a warning (currently `acrd -> bridge`, tests only), not a failure.

Exit codes: 0 = clean (warnings allowed), 1 = violation.
Run: `python scripts/check-deps.py`
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Lower rank = closer to the bottom of the DAG (may be depended upon).
RANK = {
    "common": 0,
    "profile": 1,
    "dsp": 1,
    "planner": 2,
    "retrieval": 2,
    "eval": 2,
    "acrd": 3,
    "bridge": 4,
    "ui": 5,
}

# Crate package name -> short name used above.
PKG_TO_SHORT = {f"synthlm-{name}": name for name in RANK}

# DEC-022: bridge is a thin REAPER binding layer, never a model/analysis client.
BRIDGE_FORBIDDEN = {"planner", "retrieval", "eval", "dsp"}

DEP_SECTIONS = ("dependencies", "dev-dependencies", "build-dependencies")

FAILURES: list[str] = []
WARNINGS: list[str] = []


def main() -> int:
    members = _workspace_members()
    if not members:
        FAILURES.append("no workspace members found in Cargo.toml")
        return _report()

    for member in members:
        manifest = ROOT / "crates" / member / "Cargo.toml"
        if not manifest.is_file():
            FAILURES.append(f"{member}: manifest not found at {manifest}")
            continue
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        for section in DEP_SECTIONS:
            for dep_name in data.get(section, {}):
                short = PKG_TO_SHORT.get(dep_name)
                if short is None:
                    continue  # third-party dependency, not a workspace crate
                _check_edge(member, short, section)

    return _report()


def _check_edge(source: str, target: str, section: str) -> None:
    edge = f"{source} -> {target} [{section}]"
    if source == target:
        FAILURES.append(f"{edge}: self dependency")
        return
    if section == "dev-dependencies":
        if RANK[target] >= RANK[source]:
            WARNINGS.append(
                f"{edge}: reverse/sibling dev-edge (tests only, not shipped) — "
                "DEC-022 DAG applies to runtime deps"
            )
        return
    if source == "bridge" and target in BRIDGE_FORBIDDEN:
        FAILURES.append(
            f"{edge}: DEC-022 forbids bridge from depending on model/analysis "
            f"crate '{target}' (L8 process boundary)"
        )
        return
    if RANK[target] >= RANK[source]:
        FAILURES.append(
            f"{edge}: DEC-022 one-way DAG violated "
            f"(rank {target}={RANK[target]} must be < rank {source}={RANK[source]})"
        )


def _workspace_members() -> list[str]:
    data = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    raw = data.get("workspace", {}).get("members", [])
    members = []
    for entry in raw:
        # members are declared as "crates/<name>"
        members.append(str(entry).replace("\\", "/").rstrip("/").split("/")[-1])
    return members


def _report() -> int:
    for warning in WARNINGS:
        print(f"warn: {warning}")
    if FAILURES:
        print("DEC-022 GATE FAILED:")
        for failure in FAILURES:
            print(f"  - {failure}")
        return 1
    print(
        f"DEC-022 GATE OK ({len(RANK)} crates, 0 direction violations, "
        f"{len(WARNINGS)} dev-only warning(s))."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
