//! TSK-704 dual-process shm stress: child endpoint.
//!
//! Helper binary spawned by the `dual_process_shm` integration test, which
//! acts as the parent. The child connects to the parent-bound endpoint,
//! shakes hands as Bridge, receives exactly `--frames` shared-memory
//! announce frames, verifies every payload byte against the deterministic
//! pattern documented here and in the parent test, and acknowledges each
//! frame with its sequence number plus descriptor checksum. Any mismatch or
//! transport failure exits nonzero with a secret-free message on stderr
//! (sequence numbers and byte counts only; never payload bytes or paths).
//!
//! Usage: `shm_ipc_child --endpoint <id> --frames <n> --payload-bytes <b>`

use synthlm_common::ipc::{
    EndpointRole, MessageType, RetryPolicy, client_handshake, connect_to_with_retry, recv_frame,
    send_frame,
};
use synthlm_common::shm::{parse_announce, read_block};

/// Filler for `payload[8 + index]` of frame `seq`.
///
/// Must stay identical to the builder in `tests/dual_process_shm.rs`; the
/// two copies are intentionally small and cross-referenced rather than
/// sharing a public API added only for tests.
fn pattern_byte(seq: u64, index: usize) -> u8 {
    seq.wrapping_add(index as u64).rem_euclid(251) as u8
}

/// Parsed `--endpoint / --frames / --payload-bytes` configuration.
struct ChildConfig {
    endpoint: String,
    frames: u64,
    payload_bytes: usize,
}

/// Usage hint printed when arguments are missing or malformed.
const USAGE: &str = "usage: shm_ipc_child --endpoint <id> --frames <n> --payload-bytes <b>";

/// Upper bound for `--payload-bytes` (well above any stress size, far below
/// address-space concerns).
const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Parse `std::env::args` (flags already stripped) into a config.
fn parse_args(args: &[String]) -> Result<ChildConfig, String> {
    let mut endpoint: Option<String> = None;
    let mut frames: Option<u64> = None;
    let mut payload_bytes: Option<usize> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--endpoint" => {
                index += 1;
                endpoint = args.get(index).cloned();
            }
            "--frames" => {
                index += 1;
                let raw = args.get(index).ok_or_else(|| USAGE.to_owned())?;
                frames = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("bad --frames value: {raw}"))?,
                );
            }
            "--payload-bytes" => {
                index += 1;
                let raw = args.get(index).ok_or_else(|| USAGE.to_owned())?;
                payload_bytes = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("bad --payload-bytes value: {raw}"))?,
                );
            }
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
        index += 1;
    }
    let config = ChildConfig {
        endpoint: endpoint.ok_or_else(|| USAGE.to_owned())?,
        frames: frames.ok_or_else(|| USAGE.to_owned())?,
        payload_bytes: payload_bytes.ok_or_else(|| USAGE.to_owned())?,
    };
    if config.frames == 0 || config.frames > 100_000 {
        return Err(format!(
            "--frames out of range 1..=100000: {}",
            config.frames
        ));
    }
    if config.payload_bytes < 16 {
        return Err(format!(
            "--payload-bytes must hold seq + filler (>= 16): {}",
            config.payload_bytes
        ));
    }
    if config.payload_bytes > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "--payload-bytes above {MAX_PAYLOAD_BYTES}: {}",
            config.payload_bytes
        ));
    }
    Ok(config)
}

/// Receive one announce, verify the segment byte-for-byte, acknowledge it.
///
/// Returns the acknowledged sequence number.
fn serve_one(
    stream: &mut interprocess::local_socket::Stream,
    payload_bytes: usize,
) -> Result<u64, String> {
    let frame = recv_frame(stream).map_err(|e| format!("recv announce failed: {e}"))?;
    if frame.kind != MessageType::RenderResult {
        return Err(format!(
            "expected render.result announce, got {}",
            frame.kind.as_str()
        ));
    }
    let seq = frame
        .body
        .get("seq")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "announce body carries no u64 `seq`".to_owned())?;
    let descriptor =
        parse_announce(&frame.body).map_err(|e| format!("announce envelope invalid: {e}"))?;
    let payload =
        read_block(&descriptor).map_err(|e| format!("shm read failed at seq {seq}: {e}"))?;
    if payload.len() != payload_bytes {
        return Err(format!(
            "payload length mismatch at seq {seq}: got {}, want {payload_bytes}",
            payload.len()
        ));
    }
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&payload[..8]);
    let seen = u64::from_le_bytes(prefix);
    if seen != seq {
        return Err(format!(
            "sequence mismatch: announce says {seq}, payload says {seen}"
        ));
    }
    for (offset, byte) in payload[8..].iter().enumerate() {
        let want = pattern_byte(seq, offset);
        if *byte != want {
            return Err(format!(
                "payload content mismatch at seq {seq} byte {}",
                offset + 8
            ));
        }
    }
    let ack = serde_json::json!({"seq": seq, "len_bytes": descriptor.len_bytes, "checksum": descriptor.checksum});
    send_frame(stream, MessageType::ScoreReport, &ack)
        .map_err(|e| format!("ack send failed at seq {seq}: {e}"))?;
    Ok(seq)
}

fn run() -> Result<(), String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let config = parse_args(&raw)?;
    let policy = RetryPolicy {
        max_attempts: 100,
        base_delay_ms: 10,
        max_delay_ms: 100,
    };
    let mut stream = connect_to_with_retry(&config.endpoint, &policy)
        .map_err(|e| format!("connect to parent endpoint failed: {e}"))?;
    client_handshake(&mut stream, EndpointRole::Bridge)
        .map_err(|e| format!("handshake with parent failed: {e}"))?;
    let mut received: u64 = 0;
    while received < config.frames {
        let seq = serve_one(&mut stream, config.payload_bytes)?;
        if seq != received {
            return Err(format!(
                "out-of-order frame: got seq {seq}, want {received}"
            ));
        }
        received += 1;
    }
    println!("shm_ipc_child: verified {received} frames, exiting");
    Ok(())
}

fn main() {
    if let Err(message) = run() {
        eprintln!("shm_ipc_child failed: {message}");
        std::process::exit(1);
    }
}
