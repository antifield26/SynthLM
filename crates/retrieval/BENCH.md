# TSK-203 retrieval benchmark (10k, 512-dim)

- Date (UTC): 2026-10-06
- Machine: 12th Gen Intel Core i7-12700H, Windows 10 Home (x64)
- Arch/profile: x86_64/debug (cargo test)
- Toolchain: rustc 1.98.1 (verified 2026-10-06; update on re-run elsewhere)
- Crate: synthlm-retrieval 0.0.0 (pattern prototypes, exact scan, in-memory)
- Dataset: 10000 synthetic unit vectors, dim 512, seed 0x5eed0203; top_k 10;
  100 fresh queries + 50 stored-vector self-hit probes per backend;
  payload sim: stable ident + 1–2 tags + LUFS scalar; filtered queries ≈10% selectivity.

| backend | build (s) | queries | P50 (ms) | P95 (ms) | max (ms) | est. mem (MB) | filt. P95 (ms) | filt. hits/q | self-hit |
|---|---|---|---|---|---|---|---|---|---|
| usearch-pattern | 0.11 | 100 | 29.881 | 31.163 | 31.416 | 20.4 | 31.160 | 0.8 | 100% |
| lancedb-pattern | 0.10 | 100 | 27.128 | 28.472 | 30.093 | 20.5 | 3.294 | 10.0 | 100% |

## Conclusion

 Parities: build 0.11s vs 0.10s; unfiltered P95 31.163 ms vs 28.472 ms; filtered P95 31.160 ms vs 3.294 ms.
 RECOMMENDATION: LanceDB-pattern (`LanceDbIndex`) as the default backend.
 Reason: latency parity holds (LanceDB-pattern P95 within 15% of USearch-pattern),
 while only it offers predicate pushdown (structured filtering before scoring,
 DEC-014) and table versioning for preset-library iteration (DEC-027).
 REJECTED: USearch-pattern (`UsearchIndex`) — no pushdown path (rank-then-filter
 default impl) and no dataset-versioning story; keep as the embedded fallback if
 real-crate benchmarks at 100k scale show HNSW winning on latency.
 REVERSAL: re-run this harness against the real crates (`usearch 2.26.4` HNSW,
 `lancedb 0.39.0` on-disk tables, both Apache-2.0) at 10k/100k with real CLAP/ST
 embeddings; flip the default if the winner's P95 regresses >20% or recall@10 < 0.95.

## Caveats (read before locking the default)

1. Pattern prototypes, not the real crates: both sides run exact flat cosine scans.
   Real USearch HNSW is approximate (recall/latency tradeoff at 100k+ unmeasured here);
   real LanceDB adds Arrow/Lance I/O plus query planning. Real-crate versions to bind:
   `usearch 2.26.4` / `lancedb 0.39.0` (both Apache-2.0; sources in module docs).
2. Synthetic uniform vectors have no cluster structure, so ANN recall is intentionally
   not measured here — it would be noise. Re-measure recall@10 with real CLAP/ST embeddings.
3. Memory is estimated (`estimated_bytes`: vectors + payloads + per-entry overhead),
   not RSS. Real-crate RSS (HNSW graph / block cache) is ⚠️需实测.
4. Embedding bytes never leave process memory in this harness; the report carries only
   aggregate statistics (privacy invariant, AGENTS.md §8).

## TSK-602 appendix — real-crate backend (10k, 512-dim, `lancedb 0.39.0`)

- Date (UTC): 2026-10-07
- Machine: 12th Gen Intel Core i7-12700H, Windows 10 Home (x64) (same box as the TSK-203 baseline above)
- Arch/profile: x86_64/debug (`cargo test -p synthlm-retrieval --features lancedb-real`)
- Toolchain: rustc 1.98.1
- Backend: `RealLanceDbIndex` (feature `lancedb-real`, non-default), `lancedb =0.39.0`
  (+ `remote` feature, compile-only workaround, see caveat 2), one on-disk table per
  index in its own temp dir (removed on drop), unindexed flat cosine search, buffered
  adds with a single-commit batch flush. Build time below counts adds + the commit.
- Dataset: same synthetic protocol as TSK-203 (10000 unit vectors, dim 512, seed
  `0x5eed0203`, top_k 10; 100 fresh queries + 50 stored-vector self-hit probes).

| backend | build (s) | queries | P50 (ms) | P95 (ms) | max (ms) | filt-closure P95 (ms) | filt-sql P95 (ms) | filt. hits/q | self-hit |
|---|---|---|---|---|---|---|---|---|---|
| lancedb-real | 0.56 | 100 | 38.317 | 40.316 | 45.338 | 244.324 | 69.910 | 10.0 | 100% |
| lancedb-pattern (TSK-203 pinned baseline, same machine) | 0.10 | 100 | 27.128 | 28.472 | 30.093 | 3.294 | — | 10.0 | 100% |

Gates: build 0.56s < 30min ✓; P95 40.316ms < 100ms ✓; self-hit 100% with cosine
score ~1.0 ✓.

REVERSAL (TSK-203 acceptance rule: flip the default if the winner's P95 regresses
>20%): `(40.316 − 28.472) / 28.472 = +41.6%` → TRIGGERED. DECISION: keep the
`LanceDbIndex` pattern as the default backend; `lancedb-real` stays opt-in behind
its non-default feature. 不硬上. (This record lives here because TSK-602 may not
touch `docs/DECISIONS.md`; the DEC-014 reversal condition itself — 10k build
>30min — is far from triggering at 0.56s.)

Why the real crate is slower (all local, zero network): the same 10k×512 flat scan
now passes through Arrow/Lance commit + per-query DataFusion planning and
`_distance` materialization. The closure-filtered path additionally pays widening
refill scans (≈6 flat queries to collect 10 matches at 10% selectivity); the SQL
path prefilters server-side (69.910ms) but keeps the planning overhead.

Caveats:

1. `mem-est` inside the real harness counts process memory only (payload map +
   per-entry overhead, ≈1.0MB); the on-disk Lance table holding 19.5MiB of vectors
   plus fragment/manifest overhead is excluded. The prototype `est. mem` column
   counts in-memory vectors, so the two memory numbers are NOT comparable.
2. Stock bench machine cannot build `lancedb 0.39.0`: it needs a `protoc` binary
   (`PROTOC` env, for `lance-encoding`'s build script) and the `remote` feature
   (upstream gates `Error::Http` on `remote` but uses it in `job.rs`
   unconditionally; registry source verified 2026-10-07). `remote` drags the
   cloud-client crates into the lock file but no cloud call is ever made (all
   tables are local tempdirs). Default-feature builds and CI need neither.
3. Zero-norm queries: prototypes score them `0.0`; the real backend surfaces
   `IndexError::Backend` (cosine is undefined for zero vectors upstream). The
   harness uses unit vectors only, so parity is unaffected.
4. These numbers are printed by `tests/bench_10k_real.rs` to stdout only (it
   writes no files, so `cargo test` never dirties the tree) and transcribed here
   by hand. Rerun: `cargo test -p synthlm-retrieval --features lancedb-real`
   (first run fetches ~515 crates and needs `PROTOC` set).
