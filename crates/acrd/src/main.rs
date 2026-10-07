//! `acrd`: SynthLM sidecar daemon.
//!
//! Hosts planner/retrieval/eval/DSP off the DAW process (L8). Today it also
//! hosts the TSK-405 deterministic M3 demo harness (`acrd demo --seed N`,
//! fixed-seed candidates instead of model calls); the TSK-501 skeleton
//! daemon entrypoint is `acrd serve` (bind + hello + minimal replies +
//! journal replay + heartbeat).

use synthlm_acrd::{daemon, demo};

/// Default demo output directory (relative to the workspace root).
const DEFAULT_DEMO_OUT_DIR: &str = "experiments/e2e-demo";

fn main() {
    let code = run();
    if code != 0 {
        std::process::exit(code);
    }
}

/// Argument dispatch: `acrd demo --seed N [--out-dir DIR]`,
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
        Some("serve") => run_serve_cmd(&args),
        _ => {
            eprintln!(
                "{}",
                demo::DemoError::BadUsage("expected subcommand `demo` or `serve`".to_owned())
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
    println!("       acrd serve [--endpoint <id>] [--state-dir <dir>]");
    println!("                  [--heartbeat-ms <n>] [--frame-timeout-ms <n>]");
    println!("       acrd --help");
    println!();
    println!("Deterministic M3 demo harness (TSK-405): fixed-seed ReaEQ");
    println!("candidates, eval-scored previews, demo-plan.json + demo-apply.lua.");
    println!("Default out dir: {DEFAULT_DEMO_OUT_DIR}");
    println!();
    println!("Skeleton daemon (TSK-501): bind the bus endpoint, hello");
    println!("handshake, minimal per-frame replies, journal replay, heartbeat.");
    println!("State dir default follows DEC-027 (see acrd --help text source).");
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
