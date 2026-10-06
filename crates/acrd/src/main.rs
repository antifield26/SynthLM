//! `acrd`: SynthLM sidecar daemon.
//!
//! Hosts planner/retrieval/eval/DSP off the DAW process (L8). Today it also
//! hosts the TSK-405 deterministic M3 demo harness (`acrd demo --seed N`,
//! fixed-seed candidates instead of model calls); the full daemon entrypoint
//! lands with TSK-105/107/301.

mod demo;

/// Default demo output directory (relative to the workspace root).
const DEFAULT_DEMO_OUT_DIR: &str = "experiments/e2e-demo";

fn main() {
    let code = run();
    if code != 0 {
        std::process::exit(code);
    }
}

/// Argument dispatch: `acrd demo --seed N [--out-dir DIR]`, `acrd --help`.
fn run() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return 0;
    }
    if args.get(1).map(String::as_str) != Some("demo") {
        eprintln!(
            "{}",
            demo::DemoError::BadUsage("expected subcommand `demo`".to_owned())
        );
        print_usage();
        return 2;
    }
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
    println!("       acrd --help");
    println!();
    println!("Deterministic M3 demo harness (TSK-405): fixed-seed ReaEQ");
    println!("candidates, eval-scored previews, demo-plan.json + demo-apply.lua.");
    println!("Default out dir: {DEFAULT_DEMO_OUT_DIR}");
}
