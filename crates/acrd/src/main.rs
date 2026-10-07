//! `acrd`: SynthLM sidecar daemon.
//!
//! Hosts planner/retrieval/eval/DSP off the DAW process (L8). Today it also
//! hosts the TSK-405 deterministic M3 demo harness (`acrd demo --seed N`,
//! fixed-seed candidates instead of model calls), the TSK-505 single-command
//! live e2e (`acrd e2e --intent TEXT --seed N`, LiveTier2 only with stored
//! Tier2 consent plus a configured key, BLOCKED otherwise with no seeded
//! fallback), and the TSK-501 skeleton daemon entrypoint (`acrd serve`).

use synthlm_acrd::{daemon, demo, e2e};

/// Default demo output directory (relative to the workspace root).
const DEFAULT_DEMO_OUT_DIR: &str = "experiments/e2e-demo";

fn main() {
    let code = run();
    if code != 0 {
        std::process::exit(code);
    }
}

/// Argument dispatch: `acrd demo --seed N [--out-dir DIR]`,
/// `acrd e2e [--intent TEXT] --seed N [--out-dir DIR]`,
/// `acrd serve [--endpoint ID] [--state-dir DIR] [--heartbeat-ms N]
/// [--frame-timeout-ms N]`, `acrd --help`.
fn run() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return 0;
    }
    match args.get(1).map(String::as_str) {
        Some("demo") => run_demo_cmd(&args),
        Some("e2e") => run_e2e_cmd(&args),
        Some("serve") => run_serve_cmd(&args),
        Some("cache") => run_cache_cmd(&args),
        _ => {
            eprintln!(
                "{}",
                demo::DemoError::BadUsage(
                    "expected subcommand `demo`, `e2e`, `serve`, or `cache`".to_owned()
                )
            );
            print_usage();
            2
        }
    }
}

/// `acrd demo` argument handling (unchanged TSK-405 harness).
fn run_demo_cmd(args: &[String]) -> i32 {
    let mut seed: Option<u64> = None;
    let mut out_dir = DEFAULT_DEMO_OUT_DIR.to_owned();
    let mut rest = args.iter().skip(2);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--seed" => {
                let raw = rest
                    .next()
                    .map_or_else(|| "<missing>".to_owned(), std::string::ToString::to_string);
                match raw.parse::<u64>() {
                    Ok(value) => seed = Some(value),
                    Err(_) => {
                        eprintln!(
                            "{}",
                            demo::DemoError::BadUsage(format!(
                                "--seed needs a non-negative integer, got {raw:?}"
                            ))
                        );
                        return 2;
                    }
                }
            }
            "--out-dir" => {
                out_dir = rest
                    .next()
                    .map_or_else(|| "<missing>".to_owned(), std::string::ToString::to_string);
                if out_dir == "<missing>" {
                    eprintln!(
                        "{}",
                        demo::DemoError::BadUsage("--out-dir needs a directory".to_owned())
                    );
                    return 2;
                }
            }
            unknown => {
                eprintln!(
                    "{}",
                    demo::DemoError::BadUsage(format!("unknown flag {unknown:?}"))
                );
                print_usage();
                return 2;
            }
        }
    }
    let Some(seed) = seed else {
        eprintln!(
            "{}",
            demo::DemoError::BadUsage("--seed N is required".to_owned())
        );
        print_usage();
        return 2;
    };
    match demo::run_demo(seed, std::path::Path::new(&out_dir)) {
        Ok(summary) => {
            println!("demo ok: seed={}", summary.seed);
            println!("out: {}", summary.out_dir.display());
            println!("plan: {}", summary.plan_path.display());
            println!("lua: {}", summary.lua_path.display());
            println!("ranked: {}", summary.ranked_ids.join(", "));
            0
        }
        Err(err) => {
            eprintln!("acrd demo: {err}");
            1
        }
    }
}

/// Short usage text (stdout for `--help`, stderr tail otherwise).
fn print_usage() {
    println!("usage: acrd demo --seed <u64> [--out-dir <dir>]");
    println!("       acrd e2e [--intent <text>] --seed <u64> [--out-dir <dir>]");
    println!("       acrd serve [--endpoint <id>] [--state-dir <dir>]");
    println!("                  [--heartbeat-ms <n>] [--frame-timeout-ms <n>]");
    println!("       acrd cache status|gc [--cache-dir <dir>]");
    println!("       acrd --help");
    println!();
    println!("Deterministic M3 demo harness (TSK-405): fixed-seed ReaEQ");
    println!("candidates, eval-scored previews, demo-plan.json + demo-apply.lua.");
    println!("Default out dir: {DEFAULT_DEMO_OUT_DIR}");
    println!();
    println!("Single-command live M3 e2e (TSK-505): fixed intent through the");
    println!("live Tier2 text path, dual patch validation with repair counts,");
    println!("3 diversified candidates with Chinese difference sentences,");
    println!("e2e-plan.json + e2e-apply.lua (inline 42230 renders, FNV records,");
    println!("reverse rollback, null-test). Needs stored Tier2 consent plus a");
    println!("configured key; BLOCKED otherwise with no seeded fallback.");
    println!("Default out dir: {}", e2e::DEFAULT_E2E_OUT_DIR);
    println!();
    println!("Skeleton daemon (TSK-501): bind the bus endpoint, hello");
    println!("handshake, minimal per-frame replies, journal replay, heartbeat.");
    println!("State dir default follows DEC-027 (see acrd --help text source).");
}

/// `acrd e2e` argument handling: `--seed` required, `--intent` defaults to
/// the fixed brighter-mix intent, `--out-dir` defaults to `experiments/e2e`.
/// The live leg runs inside [`e2e::run_e2e_live`]; any BLOCKED gate exits 1
/// (never falls back to the seeded demo path).
fn run_e2e_cmd(args: &[String]) -> i32 {
    let mut intent = e2e::DEFAULT_INTENT_ZH.to_owned();
    let mut seed: Option<u64> = None;
    let mut out_dir = e2e::DEFAULT_E2E_OUT_DIR.to_owned();
    let mut rest = args.iter().skip(2);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--intent" => {
                let raw = rest
                    .next()
                    .map_or_else(|| "<missing>".to_owned(), std::string::ToString::to_string);
                if raw == "<missing>" || raw.trim().is_empty() {
                    eprintln!(
                        "{}",
                        e2e::E2eError::BadUsage("--intent needs non-empty text".to_owned())
                    );
                    return 2;
                }
                intent = raw;
            }
            "--seed" => {
                let raw = rest
                    .next()
                    .map_or_else(|| "<missing>".to_owned(), std::string::ToString::to_string);
                match raw.parse::<u64>() {
                    Ok(value) => seed = Some(value),
                    Err(_) => {
                        eprintln!(
                            "{}",
                            e2e::E2eError::BadUsage(format!(
                                "--seed needs a non-negative integer, got {raw:?}"
                            ))
                        );
                        return 2;
                    }
                }
            }
            "--out-dir" => {
                out_dir = rest
                    .next()
                    .map_or_else(|| "<missing>".to_owned(), std::string::ToString::to_string);
                if out_dir == "<missing>" {
                    eprintln!(
                        "{}",
                        e2e::E2eError::BadUsage("--out-dir needs a directory".to_owned())
                    );
                    return 2;
                }
            }
            unknown => {
                eprintln!(
                    "{}",
                    e2e::E2eError::BadUsage(format!("unknown flag {unknown:?}"))
                );
                print_usage();
                return 2;
            }
        }
    }
    let Some(seed) = seed else {
        eprintln!(
            "{}",
            e2e::E2eError::BadUsage("--seed N is required".to_owned())
        );
        print_usage();
        return 2;
    };
    match e2e::run_e2e_live(&intent, seed, std::path::Path::new(&out_dir)) {
        Ok(summary) => {
            println!("e2e ok: seed={} backend={}", summary.seed, summary.backend);
            println!("out: {}", summary.out_dir.display());
            println!("plan: {}", summary.plan_path.display());
            println!("lua: {}", summary.lua_path.display());
            println!("ranked: {}", summary.ranked_ids.join(", "));
            println!(
                "repair: rounds={} removed={} replaced={}",
                summary.repair_rounds, summary.removed_total, summary.replaced_total
            );
            0
        }
        Err(err) => {
            eprintln!("acrd e2e: {err}");
            1
        }
    }
}

/// `acrd serve` argument handling: all knobs optional; numeric knobs must be
/// non-negative integers.
fn run_serve_cmd(args: &[String]) -> i32 {
    let mut endpoint: Option<String> = None;
    let mut state_dir: Option<std::path::PathBuf> = None;
    let mut heartbeat_ms = daemon::DEFAULT_HEARTBEAT_MS;
    let mut frame_timeout_ms = daemon::DEFAULT_FRAME_TIMEOUT_MS;
    let mut rest = args.iter().skip(2);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--endpoint" => match rest.next() {
                Some(value) => endpoint = Some(value.clone()),
                None => {
                    eprintln!(
                        "{}",
                        demo::DemoError::BadUsage("--endpoint needs an id".to_owned())
                    );
                    return 2;
                }
            },
            "--state-dir" => match rest.next() {
                Some(value) => state_dir = Some(std::path::PathBuf::from(value)),
                None => {
                    eprintln!(
                        "{}",
                        demo::DemoError::BadUsage("--state-dir needs a directory".to_owned())
                    );
                    return 2;
                }
            },
            "--heartbeat-ms" => match rest.next().map(|raw| raw.parse::<u64>()) {
                Some(Ok(value)) => heartbeat_ms = value,
                _ => {
                    eprintln!(
                        "{}",
                        demo::DemoError::BadUsage(
                            "--heartbeat-ms needs a non-negative integer".to_owned()
                        )
                    );
                    return 2;
                }
            },
            "--frame-timeout-ms" => match rest.next().map(|raw| raw.parse::<u64>()) {
                Some(Ok(value)) => frame_timeout_ms = value,
                _ => {
                    eprintln!(
                        "{}",
                        demo::DemoError::BadUsage(
                            "--frame-timeout-ms needs a non-negative integer".to_owned()
                        )
                    );
                    return 2;
                }
            },
            unknown => {
                eprintln!(
                    "{}",
                    demo::DemoError::BadUsage(format!("unknown flag {unknown:?}"))
                );
                print_usage();
                return 2;
            }
        }
    }
    let opts = daemon::ServeOptions {
        endpoint_id: endpoint,
        state_dir: state_dir.unwrap_or_else(daemon::default_state_dir),
        heartbeat_interval_ms: heartbeat_ms,
        frame_timeout_ms,
    };
    match daemon::run_serve(&opts) {
        Ok(report) => {
            // Counts only: no absolute paths on stdout (AGENTS.md §8).
            println!(
                "serve ok: connections={} frames={} errors={} heartbeats={} journal_tasks={} journal_skipped={} timeouts={}",
                report.connections,
                report.frames_replied,
                report.errors_replied,
                report.heartbeats,
                report.journal_tasks,
                report.journal_skipped,
                if report.frame_timeout_enforced {
                    "on"
                } else {
                    "off"
                }
            );
            0
        }
        Err(err) => {
            eprintln!("acrd serve: {err}");
            1
        }
    }
}

/// `acrd cache status|gc [--cache-dir <dir>]` (TSK-805 wire 1).
///
/// The first product surface over `synthlm-dsp`: the stem/artifact cache was
/// fully implemented and tested but unreachable from any binary. Everything
/// printed here is counts and sizes — never paths or PCM (AGENTS.md §8).
fn run_cache_cmd(args: &[String]) -> i32 {
    let mut subcommand: Option<String> = None;
    let mut cache_dir: Option<String> = None;
    let mut rest = args.iter().skip(2);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--cache-dir" => match rest.next() {
                Some(value) => cache_dir = Some(value.clone()),
                None => {
                    eprintln!("acrd cache: --cache-dir needs a value");
                    return 2;
                }
            },
            other if subcommand.is_none() => subcommand = Some(other.to_owned()),
            other => {
                eprintln!("acrd cache: unexpected argument `{other}`");
                return 2;
            }
        }
    }
    let Some(subcommand) = subcommand else {
        print_usage();
        return 2;
    };
    let root = match cache_dir {
        Some(dir) => std::path::PathBuf::from(dir),
        None => match synthlm_dsp::default_cache_root() {
            Some(dir) => dir,
            None => {
                eprintln!("acrd cache: no user cache directory available; pass --cache-dir");
                return 1;
            }
        },
    };
    match subcommand.as_str() {
        "status" => match synthlm_acrd::cache::status(&root) {
            Ok(status) => {
                println!(
                    "cache status: entries={} bytes_used={} watermark_bytes={} within_watermark={} hits={} misses={}",
                    status.entries,
                    status.bytes_used,
                    status.watermark_bytes,
                    status.within_watermark,
                    status.hits,
                    status.misses,
                );
                0
            }
            Err(err) => {
                eprintln!("acrd cache status: {err}");
                1
            }
        },
        "gc" => match synthlm_acrd::cache::collect(&root, &std::collections::HashSet::new()) {
            Ok(report) => {
                println!(
                    "cache gc: reclaimed_bytes={} orphans_removed={}",
                    report.reclaimed_bytes(),
                    report.orphans_removed,
                );
                0
            }
            Err(err) => {
                eprintln!("acrd cache gc: {err}");
                1
            }
        },
        other => {
            eprintln!("acrd cache: unknown subcommand `{other}` (expected status|gc)");
            2
        }
    }
}
