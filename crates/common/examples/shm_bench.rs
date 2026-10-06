//! TSK-115 benchmark: control-plane small frames vs shm bulk blocks.
//!
//! Run with `cargo run -p synthlm-common --example shm_bench --release` and
//! paste the table into `crates/common/SHM_BENCH.md` with machine + date.
//!
//! What it measures (all on this machine, no network):
//!
//! - `frame-1kib`: mean round-trip of a 1 KiB `score.report` frame echoed over
//!   a local socket (`bind_endpoint` / `connect_to_with_retry` + handshake).
//! - `shm-1mib` / `shm-16mib`: mean wall time of `write_pcm_f32` +
//!   `read_pcm_f32` + equality check for 1 MiB / 16 MiB PCM blocks, plus
//!   payload throughput (payload bytes per second, one direction).

use std::time::Instant;

use synthlm_common::ipc::{
    EndpointRole, MessageType, RetryPolicy, accept_next, bind_endpoint, client_handshake,
    connect_to_with_retry, recv_frame, send_frame, server_handshake,
};
use synthlm_common::shm::{read_pcm_f32, write_pcm_f32};

fn ramp_f32(len: usize) -> Vec<f32> {
    (0..len).map(|i| i as f32 * 0.25 - 1.0).collect()
}

fn bench_frame_rtt_1kib(iters: usize) -> Result<f64, Box<dyn std::error::Error>> {
    let body = serde_json::json!({ "pad": "x".repeat(1024) });
    let endpoint = format!("synthlm-bench-frame-{}", std::process::id());
    let listener = bind_endpoint(&endpoint)?;
    let server = std::thread::spawn(move || -> Result<(), String> {
        let mut conn = accept_next(&listener).map_err(|e| e.to_string())?;
        server_handshake(&mut conn, EndpointRole::Acrd).map_err(|e| e.to_string())?;
        for _ in 0..iters {
            let frame = recv_frame(&mut conn).map_err(|e| e.to_string())?;
            send_frame(&mut conn, frame.kind, &frame.body).map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    let policy = RetryPolicy {
        max_attempts: 50,
        base_delay_ms: 10,
        max_delay_ms: 50,
    };
    let mut client = connect_to_with_retry(&endpoint, &policy)?;
    client_handshake(&mut client, EndpointRole::Bridge)?;
    let start = Instant::now();
    for _ in 0..iters {
        send_frame(&mut client, MessageType::ScoreReport, &body)?;
        let echo = recv_frame(&mut client)?;
        assert_eq!(echo.kind, MessageType::ScoreReport);
    }
    let mean_us = start.elapsed().as_secs_f64() / iters as f64 * 1e6;
    server
        .join()
        .map_err(|_| "bench server thread panicked".to_string())?
        .map_err(|e| format!("bench server failed: {e}"))?;
    Ok(mean_us)
}

fn bench_shm_block(
    payload_bytes: usize,
    iters: usize,
) -> Result<(f64, f64), Box<dyn std::error::Error>> {
    assert!(
        payload_bytes.is_multiple_of(4),
        "payload must be whole f32 samples"
    );
    let samples = ramp_f32(payload_bytes / 4);
    let mut total_s = 0.0;
    for _ in 0..iters {
        let start = Instant::now();
        let sender = write_pcm_f32(&samples)?;
        let back = read_pcm_f32(sender.descriptor())?;
        assert_eq!(back, samples);
        total_s += start.elapsed().as_secs_f64();
    }
    let mean_ms = total_s / iters as f64 * 1e3;
    let mib_per_s = payload_bytes as f64 / (1024.0 * 1024.0) / (total_s / iters as f64);
    Ok((mean_ms, mib_per_s))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let frame_iters = 500;
    let frame_us = bench_frame_rtt_1kib(frame_iters)?;
    // 1 MiB of f32 PCM = 262_144 samples; 16 MiB = 4_194_304 samples.
    let (ms_1mib, tp_1mib) = bench_shm_block(1024 * 1024, 20)?;
    let (ms_16mib, tp_16mib) = bench_shm_block(16 * 1024 * 1024, 5)?;
    println!("| payload | path | iters | mean | throughput |");
    println!("|---|---|---|---|---|");
    println!(
        "| 1 KiB control frame | local-socket echo RTT | {frame_iters} | {frame_us:.1} us | n/a (control plane) |"
    );
    println!(
        "| 1 MiB PCM block | shm write+read+verify | 20 | {ms_1mib:.2} ms | {tp_1mib:.1} MiB/s |"
    );
    println!(
        "| 16 MiB PCM block | shm write+read+verify | 5 | {ms_16mib:.2} ms | {tp_16mib:.1} MiB/s |"
    );
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("shm_bench failed: {e}");
        std::process::exit(1);
    }
}
