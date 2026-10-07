//! 10k benchmark harness for the real-crate backend (TSK-602, DEC-014).
//!
//! Runs the same protocol as `bench_10k.rs` (10k synthetic 512-dim unit
//! vectors, fixed seed; top_k 10; 100 fresh queries + 50 stored-vector
//! self-hit probes) against [`RealLanceDbIndex`], plus the SQL-prefiltered
//! path ([`RealLanceDbIndex::search_filtered_sql`]). Results are printed to
//! stdout and checked against the TSK-602 gates (build < 30 min,
//! P95 < 100 ms, 100% self-hit); nothing is written to the repo.
//!
//! No file output is deliberate: `bench_10k.rs` already chose env-gated
//! writes (`SYNTHLM_WRITE_BENCH=1`) so `cargo test` stays tree-clean, and a
//! second writer would double the pollution surface. The numbers printed
//! here are transcribed into `BENCH.md` by hand with their machine/profile
//! context, and any rerun reproduces them from the fixed seed.
//!
//! The harness compiles and runs only with the `lancedb-real` feature; the
//! default build is unaffected.

#![cfg(feature = "lancedb-real")]

use std::time::{Duration, Instant};

use synthlm_retrieval::{Modality, Payload, RealLanceDbIndex, VectorIndex};

/// Number of entries (DEC-014 reversal gate references 10k).
const N: usize = 10_000;
/// Embedding width: CLAP-512 slot.
const DIM: usize = 512;
/// Rank depth per query.
const TOP_K: usize = 10;
/// Fresh random queries for the latency distribution.
const N_QUERIES: usize = 100;
/// Stored-vector probes for the exact self-hit check (every `N / N_PROBES`th).
const N_PROBES: usize = 50;
/// Fixed seed, identical to `bench_10k.rs`: runs are reproducible.
const SEED: u64 = 0x5EED_0203;
/// SQL prefilter matching the prototype harness closure
/// (`lufs < -27.6` ≈ 10% selectivity on the simulated LUFS range).
const FILTER_SQL: &str = "lufs < -27.6";

/// Deterministic xorshift-style generator (same stream as `bench_10k.rs`; no
/// `rand` dependency so the harness adds no audit-surface deps).
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

/// Simulated preset payload, identical to `bench_10k.rs`.
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

/// Percentile over an unsorted sample (sorts a copy).
fn percentile(sample: &[f64], pct: f64) -> f64 {
    if sample.is_empty() {
        return 0.0;
    }
    let mut sorted = sample.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (pct * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn fmt_ms(value: f64) -> String {
    format!("{value:.3}")
}

#[test]
fn bench_10k_real_crate() -> Result<(), String> {
    let mut rng = Lcg(SEED);
    let mut index = RealLanceDbIndex::new(DIM).map_err(|err| format!("new failed: {err}"))?;

    let mut vector = vec![0.0_f32; DIM];
    let mut probes: Vec<(u64, Vec<f32>)> = Vec::with_capacity(N_PROBES);
    let probe_every = N / N_PROBES;

    // Build: buffered adds plus the single commit flush (counted together,
    // since a real deployment pays the commit before serving queries).
    let build_start = Instant::now();
    for i in 0..N {
        random_unit_vector(&mut rng, &mut vector);
        let id = i as u64;
        index
            .add(id, &vector, sim_payload(&mut rng, i))
            .map_err(|err| format!("add {i} failed: {err}"))?;
        if i.is_multiple_of(probe_every) {
            probes.push((id, vector.clone()));
        }
    }
    index
        .flush()
        .map_err(|err| format!("flush failed: {err}"))?;
    let build = build_start.elapsed();

    // Correctness: exact stored vectors must rank themselves first with a
    // cosine score of (very nearly) 1.0.
    for (id, stored) in &probes {
        let hits = index
            .search(stored, 1)
            .map_err(|err| format!("probe failed: {err}"))?;
        let hit = hits
            .first()
            .ok_or_else(|| format!("probe {id} returned empty"))?;
        if hit.id != *id {
            return Err(format!("probe {id} self-hit failed (got {})", hit.id));
        }
        if (hit.score - 1.0).abs() >= 1e-3 {
            return Err(format!("probe {id} score {} != ~1.0", hit.score));
        }
    }

    // Latency distribution over fresh random queries.
    let mut latencies = Vec::with_capacity(N_QUERIES);
    for _ in 0..N_QUERIES {
        random_unit_vector(&mut rng, &mut vector);
        let query_start = Instant::now();
        let hits = index
            .search(&vector, TOP_K)
            .map_err(|err| format!("query failed: {err}"))?;
        if hits.len() > TOP_K {
            return Err("returned more than top_k".to_owned());
        }
        latencies.push(query_start.elapsed());
    }

    // Closure-filtered queries (client-side refill over flat ranking).
    let mut filtered = Vec::with_capacity(N_QUERIES);
    let mut filtered_hits = 0_usize;
    for _ in 0..N_QUERIES {
        random_unit_vector(&mut rng, &mut vector);
        let query_start = Instant::now();
        let hits = index
            .search_filtered(&vector, TOP_K, &|payload| {
                payload.lufs.is_some_and(|lufs| lufs < -27.6)
            })
            .map_err(|err| format!("filtered query failed: {err}"))?;
        filtered.push(query_start.elapsed());
        filtered_hits += hits.len();
    }

    // SQL-prefiltered queries (true server-side pushdown).
    let mut sql_filtered = Vec::with_capacity(N_QUERIES);
    let mut sql_hits = 0_usize;
    for _ in 0..N_QUERIES {
        random_unit_vector(&mut rng, &mut vector);
        let query_start = Instant::now();
        let hits = index
            .search_filtered_sql(&vector, TOP_K, FILTER_SQL)
            .map_err(|err| format!("sql query failed: {err}"))?;
        sql_filtered.push(query_start.elapsed());
        sql_hits += hits.len();
    }

    let to_ms = |samples: &[Duration]| {
        samples
            .iter()
            .map(|d| d.as_secs_f64() * 1000.0)
            .collect::<Vec<_>>()
    };
    let lat_ms = to_ms(&latencies);
    let filt_ms = to_ms(&filtered);
    let sql_ms = to_ms(&sql_filtered);
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    eprintln!(
        "TSK-602 real-crate 10k (lancedb 0.39.0, dim {DIM}, top_k {TOP_K}, {}/{}) \
         build {:.2}s | P50 {}ms P95 {}ms max {}ms | \
         closure-filt P95 {}ms hits/q {:.1} | sql-filt P95 {}ms hits/q {:.1} | \
         self-hit 100% | mem-est {:.1}MB",
        std::env::consts::ARCH,
        profile,
        build.as_secs_f64(),
        fmt_ms(percentile(&lat_ms, 0.50)),
        fmt_ms(percentile(&lat_ms, 0.95)),
        fmt_ms(lat_ms.iter().copied().fold(0.0_f64, f64::max)),
        fmt_ms(percentile(&filt_ms, 0.95)),
        filtered_hits as f64 / N_QUERIES as f64,
        fmt_ms(percentile(&sql_ms, 0.95)),
        sql_hits as f64 / N_QUERIES as f64,
        index.estimated_bytes() as f64 / 1_048_576.0,
    );

    // TSK-602 acceptance gates: build < 30 min, P95 < 100 ms.
    if build.as_secs() >= 30 * 60 {
        return Err(format!("build {}s exceeds 30min gate", build.as_secs()));
    }
    if percentile(&lat_ms, 0.95) >= 100.0 {
        return Err(format!(
            "P95 {}ms exceeds 100ms gate",
            fmt_ms(percentile(&lat_ms, 0.95))
        ));
    }
    if filtered_hits == 0 || sql_hits == 0 {
        return Err("filtered paths returned zero hits; predicate wiring suspect".to_owned());
    }
    Ok(())
}
