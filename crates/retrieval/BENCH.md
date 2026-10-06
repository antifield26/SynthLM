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
