//! 10k benchmark harness for TSK-203 (DEC-014).
//!
//! Compares the two pattern-prototype backends under identical conditions:
//! synthetic random 512-dim unit vectors (fixed seed) plus simulated
//! structured payloads. Measures build time, query latency distribution
//! (P50/P95/max), estimated memory, exact self-hit rate, and filtered-query
//! latency. The report is written to `crates/retrieval/BENCH.md` on every
//! run so `cargo test` reproduces the evidence.
//!
//! The harness compiles only when at least one backend feature is enabled;
//! with neither feature it compiles to empty on purpose.

#![cfg(any(feature = "usearch-backend", feature = "lancedb-backend"))]

use std::fmt::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use synthlm_retrieval::{Modality, Payload, VectorIndex};

#[cfg(feature = "lancedb-backend")]
use synthlm_retrieval::LanceDbIndex;
#[cfg(feature = "usearch-backend")]
use synthlm_retrieval::UsearchIndex;

/// Number of entries per index (DEC-014 reversal gate references 10k).
const N: usize = 10_000;
/// Embedding width: CLAP-512 slot.
const DIM: usize = 512;
/// Rank depth per query.
const TOP_K: usize = 10;
/// Fresh random queries for the latency distribution.
const N_QUERIES: usize = 100;
/// Stored-vector probes for the exact self-hit check (every `N / N_PROBES`th).
const N_PROBES: usize = 50;
/// Fixed seed: runs are reproducible across machines.
const SEED: u64 = 0x5EED_0203;
/// Toolchain that produced the committed report (verified `rustc --version`,
/// 2026-10-06). Update when re-running elsewhere; the harness cannot observe
/// the compiler version without a build script by design (zero new deps).
const BENCH_RUSTC: &str = "1.98.1";
/// Bench machine (verified 2026-10-06 via system inspection).
const BENCH_MACHINE: &str = "12th Gen Intel Core i7-12700H, Windows 10 Home (x64)";

/// Deterministic xorshift-style generator (no `rand` dependency on purpose:
/// the harness must not add audit-surface deps for random data).
struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut z = self.0;
        z ^= z >> 29;
        z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z ^= z >> 27;
        z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        z
    }

    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform sample in `[-1, 1)`.
    fn next_signed(&mut self) -> f32 {
        (self.next_u32() as f32) / 2_147_483_648.0 - 1.0
    }
}

/// Fills `out` with a random unit vector (cosine search needs norms).
fn random_unit_vector(rng: &mut Lcg, out: &mut [f32]) {
    let mut norm = 0.0_f64;
    for slot in out.iter_mut() {
        let v = rng.next_signed();
        norm += f64::from(v) * f64::from(v);
        *slot = v;
    }
    let norm = norm.sqrt();
    if norm > 0.0 {
        let inv = 1.0 / norm;
        for slot in out.iter_mut() {
            *slot = (f64::from(*slot) * inv) as f32;
        }
    } else {
        out[0] = 1.0;
    }
}

/// Simulated preset payload: stable ident + tags + LUFS scalar.
fn sim_payload(rng: &mut Lcg, i: usize) -> Payload {
    const TAGS: [&str; 6] = ["bass", "pad", "lead", "analog", "fm", "dark"];
    let mut payload = Payload::new(&format!("preset-{i:05}"), Modality::Audio);
    payload.tags.push(TAGS[i % TAGS.len()].to_owned());
    if i.is_multiple_of(3) {
        payload.tags.push(TAGS[(i / 7) % TAGS.len()].to_owned());
    }
    payload.lufs = Some(-30.0 + (rng.next_u32() % 10_000) as f32 * 24.0 / 10_000.0);
    payload
}

struct BenchOutcome {
    backend: &'static str,
    build: Duration,
    latencies_ms: Vec<f64>,
    filtered_ms: Vec<f64>,
    filtered_hits: usize,
    mem_bytes: u64,
    self_hit_rate: f64,
}

impl BenchOutcome {
    fn p50(&self) -> f64 {
        percentile(&self.latencies_ms, 0.50)
    }
    fn p95(&self) -> f64 {
        percentile(&self.latencies_ms, 0.95)
    }
    fn max(&self) -> f64 {
        self.latencies_ms.iter().copied().fold(0.0_f64, f64::max)
    }
    fn filtered_p95(&self) -> f64 {
        percentile(&self.filtered_ms, 0.95)
    }
}

/// Shared percentile helper over an unsorted sample (sorts a copy).
fn percentile(sample: &[f64], pct: f64) -> f64 {
    if sample.is_empty() {
        return 0.0;
    }
    let mut sorted = sample.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (pct * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn summarize_latencies(lat: &[Duration]) -> Vec<f64> {
    lat.iter().map(|d| d.as_secs_f64() * 1000.0).collect()
}

/// Runs the full 10k protocol against one backend implementation.
fn run_bench<I: VectorIndex>(backend: &'static str) -> Result<BenchOutcome, String> {
    let mut rng = Lcg(SEED);
    let mut index = I::new(DIM).map_err(|err| format!("{backend}: new failed: {err}"))?;

    let mut vector = vec![0.0_f32; DIM];
    let mut probes: Vec<(u64, Vec<f32>)> = Vec::with_capacity(N_PROBES);
    let probe_every = N / N_PROBES;

    let start = Instant::now();
    for i in 0..N {
        random_unit_vector(&mut rng, &mut vector);
        let id = i as u64;
        index
            .add(id, &vector, sim_payload(&mut rng, i))
            .map_err(|err| format!("{backend}: add {i} failed: {err}"))?;
        if i.is_multiple_of(probe_every) {
            probes.push((id, vector.clone()));
        }
    }
    let build = start.elapsed();

    // Correctness probe: exact stored vectors must rank themselves first.
    let mut self_hits = 0_usize;
    for (id, stored) in &probes {
        let hits = index
            .search(stored, 1)
            .map_err(|err| format!("{backend}: probe failed: {err}"))?;
        if hits.first().is_some_and(|hit| hit.id == *id) {
            self_hits += 1;
        }
    }
    let self_hit_rate = self_hits as f64 / probes.len() as f64;

    // Latency distribution over fresh random queries.
    let mut latencies = Vec::with_capacity(N_QUERIES);
    for _ in 0..N_QUERIES {
        random_unit_vector(&mut rng, &mut vector);
        let query_start = Instant::now();
        let hits = index
            .search(&vector, TOP_K)
            .map_err(|err| format!("{backend}: query failed: {err}"))?;
        // Prevent dead-code elimination from skewing timings across builds.
        if hits.len() > TOP_K {
            return Err(format!("{backend}: returned more than top_k"));
        }
        latencies.push(query_start.elapsed());
    }

    // Filtered queries (~10% selectivity on the LUFS scalar).
    let mut filtered = Vec::with_capacity(N_QUERIES);
    let mut filtered_hits = 0_usize;
    for _ in 0..N_QUERIES {
        random_unit_vector(&mut rng, &mut vector);
        let query_start = Instant::now();
        let hits = index
            .search_filtered(&vector, TOP_K, &|payload| {
                payload.lufs.is_some_and(|lufs| lufs < -27.6)
            })
            .map_err(|err| format!("{backend}: filtered query failed: {err}"))?;
        filtered.push(query_start.elapsed());
        filtered_hits += hits.len();
    }

    Ok(BenchOutcome {
        backend,
        build,
        latencies_ms: summarize_latencies(&latencies),
        filtered_ms: summarize_latencies(&filtered),
        filtered_hits,
        mem_bytes: index.estimated_bytes(),
        self_hit_rate,
    })
}

/// Days-since-epoch to `(year, month, day)` (Hinnant's civil algorithm).
fn epoch_to_ymd(secs: u64) -> (i64, u32, u32) {
    let days = (secs / 86_400) as i64;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn fmt_ms(value: f64) -> String {
    format!("{value:.3}")
}

/// Selection rule (fixed before measuring): recommend the LanceDB-pattern
/// backend when its unfiltered P95 is within 15% of the USearch-pattern P95
/// (performance-parity gate), because it additionally provides predicate
/// pushdown plus table versioning; otherwise recommend the faster one.
/// TSK-203 acceptance: build < 30 min and P95 < 100 ms.
#[cfg(all(feature = "usearch-backend", feature = "lancedb-backend"))]
fn conclusion(usearch: &BenchOutcome, lancedb: &BenchOutcome) -> String {
    let up95 = usearch.p95();
    let lp95 = lancedb.p95();
    let mut text = String::new();
    let _ = writeln!(
        text,
        " Parities: build {:.2}s vs {:.2}s; unfiltered P95 {} ms vs {} ms; filtered P95 {} ms vs {} ms.",
        usearch.build.as_secs_f64(),
        lancedb.build.as_secs_f64(),
        fmt_ms(up95),
        fmt_ms(lp95),
        fmt_ms(usearch.filtered_p95()),
        fmt_ms(lancedb.filtered_p95()),
    );
    if lp95 <= up95 * 1.15 {
        let _ = writeln!(
            text,
            " RECOMMENDATION: LanceDB-pattern (`LanceDbIndex`) as the default backend."
        );
        let _ = writeln!(
            text,
            " Reason: latency parity holds (LanceDB-pattern P95 within 15% of USearch-pattern),"
        );
        let _ = writeln!(
            text,
            " while only it offers predicate pushdown (structured filtering before scoring,"
        );
        let _ = writeln!(
            text,
            " DEC-014) and table versioning for preset-library iteration (DEC-027)."
        );
        let _ = writeln!(
            text,
            " REJECTED: USearch-pattern (`UsearchIndex`) — no pushdown path (rank-then-filter"
        );
        let _ = writeln!(
            text,
            " default impl) and no dataset-versioning story; keep as the embedded fallback if"
        );
        let _ = writeln!(
            text,
            " real-crate benchmarks at 100k scale show HNSW winning on latency."
        );
    } else {
        let _ = writeln!(
            text,
            " RECOMMENDATION: USearch-pattern (`UsearchIndex`) as the default backend."
        );
        let _ = writeln!(
            text,
            " Reason: the parity gate failed — LanceDB-pattern P95 exceeds USearch-pattern by"
        );
        let _ = writeln!(
            text,
            " more than 15%, so raw latency wins over filtering/versioning convenience."
        );
        let _ = writeln!(
            text,
            " REJECTED: LanceDB-pattern (`LanceDbIndex`) — pushdown and versioning do not"
        );
        let _ = writeln!(
            text,
            " compensate for the measured latency gap at 10k; revisit if real-crate LanceDB"
        );
        let _ = writeln!(text, " columnar scans close the gap at larger scales.");
    }
    let _ = writeln!(
        text,
        " REVERSAL: re-run this harness against the real crates (`usearch 2.26.4` HNSW,"
    );
    let _ = writeln!(
        text,
        " `lancedb 0.39.0` on-disk tables, both Apache-2.0) at 10k/100k with real CLAP/ST"
    );
    let _ = writeln!(
        text,
        " embeddings; flip the default if the winner's P95 regresses >20% or recall@10 < 0.95."
    );
    text
}

fn row(outcome: &BenchOutcome) -> String {
    format!(
        "| {} | {:.2} | {} | {} | {} | {} | {:.1} | {:.3} | {:.1} | {:.0}% |\n",
        outcome.backend,
        outcome.build.as_secs_f64(),
        N_QUERIES,
        fmt_ms(outcome.p50()),
        fmt_ms(outcome.p95()),
        fmt_ms(outcome.max()),
        outcome.mem_bytes as f64 / 1_048_576.0,
        outcome.filtered_p95(),
        outcome.filtered_hits as f64 / N_QUERIES as f64,
        outcome.self_hit_rate * 100.0,
    )
}

#[test]
fn bench_10k_and_write_report() -> Result<(), String> {
    #[cfg(feature = "usearch-backend")]
    let usearch = run_bench::<UsearchIndex>("usearch-pattern")?;
    #[cfg(feature = "lancedb-backend")]
    let lancedb = run_bench::<LanceDbIndex>("lancedb-pattern")?;

    // Exact-search correctness gate: every stored-vector probe must self-hit.
    #[cfg(feature = "usearch-backend")]
    if usearch.self_hit_rate < 1.0 {
        return Err(format!(
            "usearch-pattern self-hit rate {} < 100%",
            usearch.self_hit_rate
        ));
    }
    #[cfg(feature = "lancedb-backend")]
    if lancedb.self_hit_rate < 1.0 {
        return Err(format!(
            "lancedb-pattern self-hit rate {} < 100%",
            lancedb.self_hit_rate
        ));
    }

    // TSK-203 acceptance gates: build < 30 min, P95 < 100 ms.
    #[cfg(feature = "usearch-backend")]
    if usearch.build.as_secs() >= 30 * 60 || usearch.p95() >= 100.0 {
        return Err("usearch-pattern misses TSK-203 gates".to_owned());
    }
    #[cfg(feature = "lancedb-backend")]
    if lancedb.build.as_secs() >= 30 * 60 || lancedb.p95() >= 100.0 {
        return Err("lancedb-pattern misses TSK-203 gates".to_owned());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("clock error: {err}"))?;
    let (year, month, day) = epoch_to_ymd(now.as_secs());

    let profile = if cfg!(debug_assertions) {
        "debug (cargo test)"
    } else {
        "release"
    };

    let mut report = String::new();
    let _ = writeln!(report, "# TSK-203 retrieval benchmark (10k, 512-dim)");
    let _ = writeln!(report);
    let _ = writeln!(report, "- Date (UTC): {year}-{month:02}-{day:02}");
    let _ = writeln!(report, "- Machine: {BENCH_MACHINE}");
    let _ = writeln!(
        report,
        "- Arch/profile: {}/{}",
        std::env::consts::ARCH,
        profile
    );
    let _ = writeln!(
        report,
        "- Toolchain: rustc {BENCH_RUSTC} (verified 2026-10-06; update on re-run elsewhere)"
    );
    let _ = writeln!(
        report,
        "- Crate: synthlm-retrieval {} (pattern prototypes, exact scan, in-memory)",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(
        report,
        "- Dataset: {N} synthetic unit vectors, dim {DIM}, seed {SEED:#x}; top_k {TOP_K};"
    );
    let _ = writeln!(
        report,
        "  {N_QUERIES} fresh queries + {N_PROBES} stored-vector self-hit probes per backend;"
    );
    let _ = writeln!(
        report,
        "  payload sim: stable ident + 1–2 tags + LUFS scalar; filtered queries ≈10% selectivity."
    );
    let _ = writeln!(report);
    let _ = writeln!(
        report,
        "| backend | build (s) | queries | P50 (ms) | P95 (ms) | max (ms) | est. mem (MB) | filt. P95 (ms) | filt. hits/q | self-hit |"
    );
    let _ = writeln!(report, "|---|---|---|---|---|---|---|---|---|---|");
    #[cfg(feature = "usearch-backend")]
    report.push_str(&row(&usearch));
    #[cfg(feature = "lancedb-backend")]
    report.push_str(&row(&lancedb));
    let _ = writeln!(report);
    let _ = writeln!(report, "## Conclusion");
    let _ = writeln!(report);
    #[cfg(all(feature = "usearch-backend", feature = "lancedb-backend"))]
    report.push_str(&conclusion(&usearch, &lancedb));
    #[cfg(all(feature = "usearch-backend", not(feature = "lancedb-backend")))]
    let _ = writeln!(
        report,
        " Single-backend build (usearch-pattern only): no comparison available; re-run with default features."
    );
    #[cfg(all(feature = "lancedb-backend", not(feature = "usearch-backend")))]
    let _ = writeln!(
        report,
        " Single-backend build (lancedb-pattern only): no comparison available; re-run with default features."
    );
    let _ = writeln!(report);
    let _ = writeln!(report, "## Caveats (read before locking the default)");
    let _ = writeln!(report);
    let _ = writeln!(
        report,
        "1. Pattern prototypes, not the real crates: both sides run exact flat cosine scans."
    );
    let _ = writeln!(
        report,
        "   Real USearch HNSW is approximate (recall/latency tradeoff at 100k+ unmeasured here);"
    );
    let _ = writeln!(
        report,
        "   real LanceDB adds Arrow/Lance I/O plus query planning. Real-crate versions to bind:"
    );
    let _ = writeln!(
        report,
        "   `usearch 2.26.4` / `lancedb 0.39.0` (both Apache-2.0; sources in module docs)."
    );
    let _ = writeln!(
        report,
        "2. Synthetic uniform vectors have no cluster structure, so ANN recall is intentionally"
    );
    let _ = writeln!(
        report,
        "   not measured here — it would be noise. Re-measure recall@10 with real CLAP/ST embeddings."
    );
    let _ = writeln!(
        report,
        "3. Memory is estimated (`estimated_bytes`: vectors + payloads + per-entry overhead),"
    );
    let _ = writeln!(
        report,
        "   not RSS. Real-crate RSS (HNSW graph / block cache) is ⚠️需实测."
    );
    let _ = writeln!(
        report,
        "4. Embedding bytes never leave process memory in this harness; the report carries only"
    );
    let _ = writeln!(
        report,
        "   aggregate statistics (privacy invariant, AGENTS.md §8)."
    );

    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("BENCH.md");
    std::fs::write(&path, &report).map_err(|err| format!("write BENCH.md failed: {err}"))?;
    Ok(())
}

#[test]
fn epoch_to_ymd_anchors() {
    assert_eq!(epoch_to_ymd(0), (1970, 1, 1));
    assert_eq!(epoch_to_ymd(1_767_225_600), (2026, 1, 1));
}

#[test]
fn percentile_definition() {
    let sample = vec![5.0, 1.0, 3.0, 2.0, 4.0];
    assert_eq!(percentile(&sample, 0.5), 3.0);
    assert_eq!(percentile(&sample, 0.95), 5.0);
    assert_eq!(percentile(&[], 0.95), 0.0);
}
