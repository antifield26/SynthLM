"""Real-technology visibility gate (TSK-808): run the four suites whose
assertions depend on extra tooling and print one explicit verdict per suite.

Why this exists: the 2026-10-07 workspace audit found that the four "real
technology" assertions — FFmpeg CLI decode matrix + AAC cross-check, the real
`lancedb` crate, the ONNX Runtime CLAP seam, and the Demucs ONNX backend — all
sit outside the CI configuration. Each needs something the default build does
not have: a non-default cargo feature, a downloaded weight, or an `ffmpeg`
binary. When that something is missing they `#[ignore]` themselves, print a
skip token, or return early, so a green `cargo test --workspace` said nothing
about them. This gate does not make them run; it makes their absence visible
and countable, so a silent skip can no longer be mistaken for a pass.

Suites (the narrowest invocation that exercises each assertion):
1. ffmpeg-decode — `cargo test -p synthlm-eval --test decode_matrix
   --test loudness_xcheck -- --nocapture`; the FFmpeg comparison side prints
   `FFMPEG-SKIP` when no binary resolves (`FFMPEG_BIN`, else `ffmpeg` on PATH).
2. lancedb-real  — `cargo test -p synthlm-retrieval --features lancedb-real
   --test bench_10k_real`; building the real `lancedb =0.39.0` crate needs a
   `protoc` binary.
3. onnx-clap     — `cargo test -p synthlm-eval --features onnx`; the `ort`
   `download-binaries` build needs network for the prebuilt runtime.
4. demucs-onnx   — `cargo test -p synthlm-dsp --features onnx -- --nocapture`;
   same build dependency, plus a cached weight for the live forward pass.

Classification rules (per suite):
- non-zero exit, no test ever ran, and an environment signature in the output
  (`protoc`, a C toolchain, network, toolchain install, ...) -> SKIPPED with
  that first output line as the reason;
- non-zero exit from a real compile/link error, or from tests that ran and
  failed -> FAILED (a skip token never covers a red suite);
- green run with a skip token (`FFMPEG-SKIP`, `SKIP ...`), or with every test
  `#[ignore]`d, or with zero tests run at all -> SKIPPED with the reason; the
  zero-tests case is exactly the false green this gate exists to catch;
- green run with at least one test executed -> EXECUTED, with counts.

Output contract (AGENTS §8: no PCM, no absolute paths, no test dumps): one
`REALTECH <suite> <EXECUTED|SKIPPED|FAILED> <detail>` line per suite, then
`REALTECH SUMMARY executed=N skipped=M failed=K`. Details are single-lined,
truncated, and stripped of directory parts so no personal path is logged.

Exit codes:
- 0 — nothing FAILED; SKIPPED is reported but not fatal, because CI runners
  legitimately lack an FFmpeg binary, `protoc`, cached weights or network;
- 1 — at least one suite FAILED, or `--require` was given and a suite
  SKIPPED (use `--require` on machines that are supposed to have everything).

Run `python scripts/test-realtech.py`; add `--require` for strict mode,
`--suite NAME` to rerun one suite, `--timeout SECONDS` to bound every suite
(per-suite defaults are generous: a slow build is not a test failure), and
`--target-dir DIR` to compile outside a shared `target/` lock.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CARGO = "cargo"
OUTPUT_LIMIT = 400_000  # chars kept from the tail of the combined output
DETAIL_LIMIT = 160
EXECUTED, SKIPPED, FAILED = "EXECUTED", "SKIPPED", "FAILED"

TEST_RESULT = re.compile(
    r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored"
)
# Uppercase tokens only: Rust test names are lower_snake_case, so an uppercase
# SKIP can only come from an intentional `eprintln!("SKIP ...")` style message.
SKIP_LINE = re.compile(r"FFMPEG-SKIP|\bSKIP\b")
# Missing tool / missing network / missing toolchain evidence. Consulted only
# when the build failed before any test ran, so a real assertion failure can
# never be relabelled a skip.
ENV_MARKERS = (
    "protoc",
    "cmake",
    "clang",
    "nasm",
    "pkg-config",
    "link.exe",
    "linking with `cc` failed",
    "onnxruntime",
    "download-binaries",
    "spurious network error",
    "failed to download",
    "error sending request",
    "could not connect",
    "connection refused",
    "network is unreachable",
    "dns error",
    "certificate",
    "rustup",
    "toolchain",
    "not installed",
    "command not found",
    "os error 2",
    "no such file or directory",
    "the system cannot find the file",
    "permission denied",
    "no space left",
)
# Drive-letter and POSIX absolute paths; replaced by their basename so logs
# carry the file name (useful) without the machine's directory layout (banned).
ABS_PATH = re.compile(
    r"[A-Za-z]:\\[^\s\"'`,;)<>=]*|(?<![\w])/(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_.-]*"
)
ERROR_LINE = re.compile(
    r"^\s*(?:error(?:\[E\d+\])?:|Error:|thread '[^']*' panicked|assertion)", re.I
)
# Cargo progress lines ("Compiling pkg-config v0.3.34") mention tool names
# without naming a problem, so they must never become a skip *reason*.
PROGRESS_LINE = re.compile(
    r"^(?:Compiling|Checking|Downloading|Downloaded|Fresh|Building|Updating|"
    r"Locking|Adding|Removing|Blocking|Finished|Running|Doc-tests|Executable)\b"
)
# A marker line that also reads like a diagnostic is the preferred reason.
DIAGNOSTIC_LINE = re.compile(
    r"^\s*(?:error|Error|Caused by:)|"
    r"(?:not found|could not find|failed|missing|unable to|no such|cannot|"
    r"denied|refused|unreachable)",
    re.I,
)


@dataclass(frozen=True)
class Suite:
    """One real-technology suite: what to run and how long to allow."""

    name: str
    args: tuple[str, ...]
    timeout_s: int


@dataclass(frozen=True)
class Result:
    """One suite's verdict, ready to print."""

    name: str
    verdict: str
    detail: str


SUITES: tuple[Suite, ...] = (
    Suite(
        name="ffmpeg-decode",
        args=(
            "test",
            "-p",
            "synthlm-eval",
            "--test",
            "decode_matrix",
            "--test",
            "loudness_xcheck",
            "--",
            "--nocapture",
        ),
        timeout_s=1800,
    ),
    Suite(
        name="lancedb-real",
        args=(
            "test",
            "-p",
            "synthlm-retrieval",
            "--features",
            "lancedb-real",
            "--test",
            "bench_10k_real",
        ),
        # TSK-602 gates a build < 30 min, so the ceiling must not turn a slow
        # but valid lancedb build into a FAILED line.
        timeout_s=3600,
    ),
    Suite(
        name="onnx-clap",
        args=("test", "-p", "synthlm-eval", "--features", "onnx"),
        timeout_s=2400,
    ),
    Suite(
        name="demucs-onnx",
        args=("test", "-p", "synthlm-dsp", "--features", "onnx", "--", "--nocapture"),
        timeout_s=2400,
    ),
)


def redact(text: str) -> str:
    """One line, no absolute paths, bounded length (AGENTS §8)."""

    def basename(match: re.Match[str]) -> str:
        tail = re.split(r"[\\/]", match.group(0).rstrip("\\/"))[-1]
        return f".../{tail}" if tail else "..."

    flat = " ".join(ABS_PATH.sub(basename, text).split())
    if len(flat) > DETAIL_LIMIT:
        # ASCII-only suffix: this line must survive any console code page.
        flat = flat[: DETAIL_LIMIT - 3].rstrip() + "..."
    return flat or "(no output)"


def cargo_argv(suite: Suite, target_dir: str | None) -> list[str]:
    argv = [CARGO, *suite.args]
    if target_dir:
        # `--target-dir` is a cargo flag, so it must stay before the `--` that
        # forwards the rest to the test binary.
        at = argv.index("--") if "--" in argv else len(argv)
        argv[at:at] = ["--target-dir", target_dir]
    return argv


def env_reason(output: str) -> str | None:
    """First diagnostic output line naming a missing tool, network or
    toolchain problem (cargo progress lines are ignored: `Compiling
    pkg-config v0.3.34` names a crate, not a failure)."""
    fallback: str | None = None
    for line in output.splitlines():
        stripped = line.strip()
        if not stripped or PROGRESS_LINE.match(stripped):
            continue
        low = stripped.lower()
        if not any(marker in low for marker in ENV_MARKERS):
            continue
        if DIAGNOSTIC_LINE.search(stripped):
            return stripped
        if fallback is None:
            fallback = stripped
    return fallback


def error_reason(output: str) -> str:
    """First compiler/panic line, else the last non-empty line."""
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    for line in lines:
        if ERROR_LINE.match(line):
            return line
    for line in lines:
        if "test result: FAILED" in line:
            return line
    return lines[-1] if lines else "(no output)"


def classify(suite: Suite, returncode: int, output: str) -> Result:
    counts = TEST_RESULT.findall(output)
    passed = sum(int(m[0]) for m in counts)
    failed = sum(int(m[1]) for m in counts)
    ignored = sum(int(m[2]) for m in counts)
    targets = len(counts)
    ran = passed + failed

    if returncode != 0:
        if ran == 0:
            reason = env_reason(output)
            if reason:
                return Result(suite.name, SKIPPED, redact(reason))
        return Result(suite.name, FAILED, redact(error_reason(output)))

    skip = next((line for line in output.splitlines() if SKIP_LINE.search(line)), None)
    if skip is not None:
        return Result(suite.name, SKIPPED, redact(skip.strip()))
    if ran == 0:
        if ignored > 0:
            return Result(
                suite.name,
                SKIPPED,
                f"all {ignored} test(s) #[ignore]d; nothing executed",
            )
        return Result(
            suite.name,
            SKIPPED,
            "no tests ran (0 passed, 0 failed) — feature gate or filter matched nothing",
        )
    return Result(
        suite.name,
        EXECUTED,
        f"{passed} passed in {targets} target(s), {failed} failed, {ignored} ignored",
    )


def run_suite(suite: Suite, target_dir: str | None, timeout_s: int) -> Result:
    argv = cargo_argv(suite, target_dir)
    env = dict(os.environ, CARGO_TERM_COLOR="never")
    try:
        proc = subprocess.run(
            argv,
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=env,
            timeout=timeout_s,
        )
    except subprocess.TimeoutExpired:
        return Result(
            suite.name,
            FAILED,
            f"timed out after {timeout_s}s (no verdict; raise --timeout if the "
            "build is legitimately slow)",
        )
    except OSError as exc:
        return Result(suite.name, SKIPPED, redact(f"cannot spawn cargo: {exc}"))
    output = (proc.stdout or "") + (proc.stderr or "")
    if len(output) > OUTPUT_LIMIT:
        # Keep the tail: cargo diagnostics and the `test result:` summary live
        # at the end, and the head is just build progress.
        output = output[-OUTPUT_LIMIT:]
    return classify(suite, proc.returncode, output)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Visibility gate for the real-technology suites (TSK-808)."
    )
    parser.add_argument(
        "--require",
        action="store_true",
        help="treat SKIPPED as fatal too (for machines that should have everything)",
    )
    parser.add_argument(
        "--suite",
        action="append",
        choices=[suite.name for suite in SUITES],
        help="run only the named suite (repeatable)",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=None,
        help="override every suite's timeout in seconds",
    )
    parser.add_argument(
        "--target-dir",
        default=None,
        help="cargo --target-dir to use (default: cargo's own target/)",
    )
    args = parser.parse_args()

    selected = [
        suite for suite in SUITES if not args.suite or suite.name in args.suite
    ]
    results: list[Result] = []
    for suite in selected:
        timeout_s = args.timeout if args.timeout is not None else suite.timeout_s
        result = run_suite(suite, args.target_dir, timeout_s)
        results.append(result)
        print(f"REALTECH {result.name} {result.verdict} {result.detail}", flush=True)

    executed = sum(1 for r in results if r.verdict == EXECUTED)
    skipped = sum(1 for r in results if r.verdict == SKIPPED)
    failed = sum(1 for r in results if r.verdict == FAILED)
    print(f"REALTECH SUMMARY executed={executed} skipped={skipped} failed={failed}")

    if failed:
        return 1
    if args.require and skipped:
        print(
            f"REALTECH REQUIRE: {skipped} suite(s) skipped, which --require makes fatal"
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
