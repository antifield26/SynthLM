//! TSK-704: true dual-process shared-memory stress.
//!
//! The parent (this test) binds an endpoint, spawns the `shm_ipc_child`
//! helper binary as a separate OS process, and pushes N announce frames,
//! each backed by its own frozen shm segment. The child opens every segment
//! in its own address space, verifies each payload byte-for-byte, and
//! acknowledges with the sequence number plus descriptor checksum.
//!
//! Pass criteria: zero lost frames, zero out-of-order frames, and the
//! per-frame latency distribution printed to the test output (record-only;
//! no timing assertion, so slow CI machines stay green).
//!
//! No new `unsafe`: both sides use only the approved `ShmSender` /
//! `read_block` faces from `synthlm_common::shm`.

use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use synthlm_common::ipc::{
    EndpointRole, MessageType, accept_next, bind_endpoint, recv_frame, send_frame, server_handshake,
};
use synthlm_common::shm::{ShmSender, announce_body};

/// Filler for `payload[8 + index]` of frame `seq`.
///
/// Must stay identical to `pattern_byte` in `src/bin/shm_ipc_child.rs`.
fn pattern_byte(seq: u64, index: usize) -> u8 {
    seq.wrapping_add(index as u64).rem_euclid(251) as u8
}

/// Payload layout (mirrors the child): `[0..8]` is the sequence `u64`
/// little-endian, the rest is deterministic filler.
fn build_payload(seq: u64, payload_bytes: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(payload_bytes);
    payload.extend_from_slice(&seq.to_le_bytes());
    for offset in 0..payload_bytes - 8 {
        payload.push(pattern_byte(seq, offset));
    }
    payload
}

fn unique_endpoint(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("synthlm-t704-{tag}-{}-{nanos}", std::process::id())
}

/// Child handle that kills the helper if the test unwinds mid-exchange.
///
/// A failed assertion must not leave an orphaned child blocked in `recv`.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// Interpolated percentile over ascending `sorted_ms`.
fn percentile(sorted_ms: &[f64], pct: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    let last = sorted_ms.len() - 1;
    let rank = (pct / 100.0 * last as f64).round() as usize;
    sorted_ms[rank.min(last)]
}

/// Spawn the child helper, exchange `frames` announces of `payload_bytes`
/// bytes, assert zero loss / zero misorder, and return per-frame RTTs in ms.
fn run_dual_process_stress(frames: u64, payload_bytes: usize) -> Vec<f64> {
    let endpoint = unique_endpoint(&format!("{frames}x{payload_bytes}"));
    let listener = bind_endpoint(&endpoint).expect("bind parent endpoint");
    let child_path = env!("CARGO_BIN_EXE_shm_ipc_child");
    let mut cmd = Command::new(child_path);
    cmd.arg("--endpoint");
    cmd.arg(&endpoint);
    cmd.arg("--frames");
    cmd.arg(frames.to_string());
    cmd.arg("--payload-bytes");
    cmd.arg(payload_bytes.to_string());
    cmd.stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // Keep the helper out of the way on interactive dev boxes; its
        // stdout/stderr still flow through the inherited handles.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = ChildGuard(cmd.spawn().expect("spawn shm_ipc_child"));
    let mut conn = accept_next(&listener).expect("accept child connection");
    server_handshake(&mut conn, EndpointRole::Acrd).expect("server handshake");

    // Hold every sender until the child has acked: dropping a sender unlinks
    // its segment, so the handles must outlive the whole exchange.
    let mut senders = Vec::new();
    let mut rtts_ms = Vec::new();
    for seq in 0..frames {
        let payload = build_payload(seq, payload_bytes);
        let sender = ShmSender::create(&payload).expect("create shm segment");
        let descriptor = sender.descriptor().clone();
        let mut body = announce_body(&descriptor);
        body["seq"] = serde_json::json!(seq);
        send_frame(&mut conn, MessageType::RenderResult, &body).expect("send announce");
        let started = Instant::now();
        let ack = recv_frame(&mut conn).expect("recv child ack");
        rtts_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(ack.kind, MessageType::ScoreReport, "ack kind at seq {seq}");
        let ack_seq = ack.body.get("seq").and_then(serde_json::Value::as_u64);
        assert_eq!(ack_seq, Some(seq), "ack order at seq {seq}");
        let ack_sum = ack.body.get("checksum").and_then(serde_json::Value::as_u64);
        assert_eq!(
            ack_sum,
            Some(descriptor.checksum),
            "ack checksum at seq {seq}"
        );
        senders.push(sender);
    }
    assert_eq!(rtts_ms.len() as u64, frames, "zero frames lost");
    drop(senders);
    let status = child.0.wait().expect("wait for child exit");
    assert!(status.success(), "child must exit 0, got {status}");
    rtts_ms
}

fn report_latency(label: &str, frames: u64, payload_bytes: usize, rtts_ms: &[f64]) {
    let mut sorted = rtts_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    println!(
        "dual-process shm stress [{label}]: frames={frames} payload_bytes={payload_bytes} lost=0 misordered=0"
    );
    println!(
        "rtt_ms: min={:.3} mean={:.3} p50={:.3} p95={:.3} max={:.3}",
        sorted.first().copied().unwrap_or(0.0),
        mean,
        percentile(&sorted, 50.0),
        percentile(&sorted, 95.0),
        sorted.last().copied().unwrap_or(0.0),
    );
}

/// 200 small frames across two real processes: the bulk-path handshake at
/// control-plane cost per frame.
#[test]
fn parent_child_shm_frames_zero_loss_ordered_with_latency_report() {
    let frames = 200;
    let payload_bytes = 4096;
    let rtts = run_dual_process_stress(frames, payload_bytes);
    report_latency("small-4kib", frames, payload_bytes, &rtts);
}

/// A few MiB-scale bulk frames across two real processes: the shm path
/// carrying payloads no control-plane frame could hold.
#[test]
fn parent_child_bulk_frames_cross_process() {
    let frames = 3;
    let payload_bytes = 4 * 1024 * 1024;
    let rtts = run_dual_process_stress(frames, payload_bytes);
    report_latency("bulk-4mib", frames, payload_bytes, &rtts);
}
