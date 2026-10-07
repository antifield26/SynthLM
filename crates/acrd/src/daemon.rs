//! acrd sidecar daemon: accept loop, hello handshake, minimal per-frame
//! replies, graceful shutdown, heartbeat watchdog file, and startup journal
//! replay (TSK-501 skeleton; ARCHITECTURE §2/§5/§7/§8, DEC-009/023/027).
//!
//! What this skeleton does: bind the default endpoint with
//! [`synthlm_common::ipc::bind_listener`], run the hello handshake from
//! [`synthlm_common::ipc::server_handshake`], and answer every known frame
//! with the smallest wire-legal reply ([`crate::daemon::decide`]; unknown
//! wire types fail frame parsing and get an `error` frame — the connection
//! stays up, nothing panics). Each connection runs on its own thread;
//! [`crate::daemon::Shutdown::request`] stops accepting, drains the
//! in-flight handlers, and returns `Ok` (exit code 0). Ctrl-C/SIGTERM are
//! bridged to that same request path via the `ctrlc` crate (handlers run on
//! a dedicated thread, never in async-signal context).
//!
//! Honest follow-ups (recorded, not silent): Windows service control is
//! unsupported (an SCM mode needs a service design plus human approval).
//! A real cross-process kill -9 drill (as opposed to the handle-drop
//! simulation in [`crate::task::TaskLog`] tests) still needs an owner and a
//! schedule. The new `ctrlc` (+ unix-only `nix`) lockfile entries need a
//! `docs/LICENSES.md` row sync by the owning session (this task is scoped
//! to `crates/acrd/`).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use interprocess::local_socket::{Listener, Stream};
use synthlm_common::consent::{FALLBACK_DIR_NAME, dir_writable, path_fingerprint};
use synthlm_common::ipc::{
    EndpointRole, ErrorCode, IpcError, MessageType, PROTOCOL_MAJOR, PROTOCOL_MINOR, WireFrame,
    accept_next, bind_endpoint, bind_listener, connect_to, now_ms, read_frame, server_handshake,
    set_stream_timeouts, write_frame,
};

use crate::dispatch::Dispatcher;
use crate::task::{TaskLog, TaskState};

/// Heartbeat file name inside the daemon state directory.
pub const HEARTBEAT_FILE_NAME: &str = "heartbeat.json";

/// Default heartbeat period (ARCHITECTURE §8 watchdog).
pub const DEFAULT_HEARTBEAT_MS: u64 = 5_000;

/// Default per-connection frame idle timeout.
pub const DEFAULT_FRAME_TIMEOUT_MS: u64 = 30_000;

/// Daemon failure. All variants are secret-free by construction: they name
/// operations and endpoint ids only, never keys, PCM, prompts, or absolute
/// paths (AGENTS.md §8).
#[derive(Debug)]
pub enum DaemonError {
    /// The endpoint could not be bound (carries the transport kind string).
    Bind(String),
    /// The task journal failed at startup.
    Journal(String),
    /// The serve-session task bookkeeping failed.
    SessionTask(String),
    /// A heartbeat file operation failed.
    Heartbeat(String),
    /// The OS-signal hook (Ctrl-C/TERM) could not be installed.
    Signal(String),
    /// The dispatch layer (planner chain) failed to open.
    Dispatch(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::Bind(detail) => write!(f, "daemon bind: {detail}"),
            DaemonError::Journal(detail) => write!(f, "daemon journal: {detail}"),
            DaemonError::SessionTask(detail) => write!(f, "daemon session task: {detail}"),
            DaemonError::Heartbeat(detail) => write!(f, "daemon heartbeat: {detail}"),
            DaemonError::Signal(detail) => write!(f, "daemon signal: {detail}"),
            DaemonError::Dispatch(detail) => write!(f, "daemon dispatch: {detail}"),
        }
    }
}

impl std::error::Error for DaemonError {}

/// Serve options (all knobs configurable; tests inject temp dirs and short
/// periods so no real user directory or fixed port is ever touched).
#[derive(Clone, Debug)]
pub struct ServeOptions {
    /// Endpoint id to bind (`None` = the default bus endpoint from
    /// [`synthlm_common::ipc::bind_listener`]).
    pub endpoint_id: Option<String>,
    /// State directory: journal + heartbeat live here.
    pub state_dir: PathBuf,
    /// Heartbeat period in milliseconds.
    pub heartbeat_interval_ms: u64,
    /// Per-connection frame idle timeout in milliseconds. Best-effort:
    /// transports without timeout support (Windows named pipes in
    /// `interprocess` 2.4.4 — verified 2026-10-07) ignore it; see
    /// [`crate::daemon::ServeReport::frame_timeout_enforced`].
    pub frame_timeout_ms: u64,
}

/// Graceful-shutdown handle: `request` stops the accept loop; in-flight
/// connections finish their current reply before their threads join.
#[derive(Clone, Debug, Default)]
pub struct Shutdown {
    /// Set by [`crate::daemon::Shutdown::request`].
    flag: Arc<AtomicBool>,
    /// Endpoint id used for the wakeup dial that unblocks a blocked accept.
    wake_endpoint: Option<String>,
}

impl Shutdown {
    /// Shutdown handle whose `request` also dials `endpoint_id` once
    /// (best-effort) to unblock a thread parked in accept.
    pub fn with_wakeup(endpoint_id: impl Into<String>) -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            wake_endpoint: Some(endpoint_id.into()),
        }
    }

    /// Request shutdown: stop accepting, drain in-flight connections.
    pub fn request(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(id) = &self.wake_endpoint {
            // Best-effort: the dial only exists to park a byte in the listen
            // backlog so a blocked accept returns; immediate drop makes the
            // server side see a clean EOF.
            let _ = connect_to(id);
        }
    }

    /// Whether shutdown was requested.
    pub fn is_requested(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// One-step outcome for a frame read: pure and unit-testable, so the serve
/// loop itself stays a thin effect layer.
#[derive(Debug)]
pub enum StepOutcome {
    /// Send this reply, keep the connection open.
    Reply(MessageType, serde_json::Value),
    /// Send nothing (inbound `error` frames: answering would ping-pong),
    /// keep the connection open.
    Silent,
    /// Close the connection without a reply (clean EOF, idle timeout,
    /// unreadable transport).
    Close,
    /// Best-effort reply, then close (the stream is unresynchronizable, e.g.
    /// after an oversize prefix).
    CloseWithReply(MessageType, serde_json::Value),
}

/// Wire string for an [`synthlm_common::ipc::ErrorCode`] (mirrors the serde
/// renames; the `error_code_strings_match_wire` test keeps them in sync).
pub fn error_code_as_str(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Timeout => "timeout",
        ErrorCode::TransportClosed => "transport_closed",
        ErrorCode::RenderUnstable => "render_unstable",
        ErrorCode::CloudUnavailable => "cloud_unavailable",
        ErrorCode::VersionMismatch => "version_mismatch",
        ErrorCode::FrameTooLarge => "frame_too_large",
        ErrorCode::TruncatedFrame => "truncated_frame",
        ErrorCode::ProtocolViolation => "protocol_violation",
        ErrorCode::AuthDenied => "auth_denied",
        ErrorCode::ConsentRequired => "consent_required",
        ErrorCode::WhitelistViolation => "whitelist_violation",
        ErrorCode::AudioCapabilityMissing => "audio_capability_missing",
        ErrorCode::Internal => "internal",
    }
}

/// Build a secret-free `error` body for `code` + short `detail`.
pub(crate) fn error_body(code: ErrorCode, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "code": error_code_as_str(code),
        "retryable": code.retryable(),
        "detail": detail,
    })
}

/// Skeleton hello body: our version plus the acrd role.
pub(crate) fn hello_body() -> serde_json::Value {
    serde_json::json!({
        "version": {"major": PROTOCOL_MAJOR, "minor": PROTOCOL_MINOR},
        "role": "acrd",
    })
}

/// Decide the reply for one frame-read outcome.
///
/// Known frames get the smallest wire-legal reply (skeleton payloads carry
/// `"skeleton": true` so no caller mistakes them for planner output);
/// unknown wire types fail parsing upstream and arrive here as
/// [`synthlm_common::ipc::IpcError::Json`], which gets an `error` reply with
/// the connection kept open — never a panic, never a disconnect.
pub fn decide(outcome: &Result<WireFrame, IpcError>) -> StepOutcome {
    match outcome {
        Ok(frame) => match frame.kind {
            MessageType::Error => StepOutcome::Silent,
            MessageType::Hello => StepOutcome::Reply(MessageType::Hello, hello_body()),
            MessageType::SnapshotSubmit | MessageType::PlanRequest => StepOutcome::Reply(
                MessageType::PlanResponse,
                serde_json::json!({"ops": [], "target_snapshot": "", "skeleton": true}),
            ),
            MessageType::PatchApply => StepOutcome::Reply(
                MessageType::PatchResult,
                serde_json::json!({"ok": true, "applied": 0, "skeleton": true}),
            ),
            MessageType::RenderRequest => StepOutcome::Reply(
                MessageType::RenderResult,
                serde_json::json!({"ok": true, "skeleton": true}),
            ),
            MessageType::PlanResponse
            | MessageType::PatchResult
            | MessageType::RenderResult
            | MessageType::ScoreReport => StepOutcome::Reply(
                frame.kind,
                serde_json::json!({"ok": true, "skeleton": true}),
            ),
            MessageType::ConsentGet => StepOutcome::Reply(
                MessageType::ConsentGet,
                serde_json::json!({"tier": "tier3", "skeleton": true}),
            ),
            MessageType::ConsentSet => StepOutcome::Reply(
                MessageType::ConsentSet,
                serde_json::json!({"ok": true, "skeleton": true}),
            ),
            MessageType::AuditEvent => StepOutcome::Reply(
                MessageType::AuditEvent,
                serde_json::json!({"ok": true, "skeleton": true}),
            ),
        },
        Err(err) => match err {
            IpcError::TransportClosed
            | IpcError::Truncated { .. }
            | IpcError::Timeout
            | IpcError::Io(_) => StepOutcome::Close,
            IpcError::FrameTooLarge { .. } => StepOutcome::CloseWithReply(
                MessageType::Error,
                error_body(ErrorCode::FrameTooLarge, "frame exceeds size cap"),
            ),
            IpcError::VersionMismatch { .. } => StepOutcome::Reply(
                MessageType::Error,
                error_body(
                    ErrorCode::VersionMismatch,
                    "protocol major version mismatch",
                ),
            ),
            IpcError::Json(_) | IpcError::Protocol(_) => StepOutcome::Reply(
                MessageType::Error,
                error_body(ErrorCode::ProtocolViolation, "malformed frame"),
            ),
            IpcError::Peer { code, .. } => {
                StepOutcome::Reply(MessageType::Error, error_body(*code, "peer error echoed"))
            }
        },
    }
}

/// Per-connection counters (fed into [`crate::daemon::ServeReport`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct ConnectionStats {
    /// Whether the hello handshake succeeded.
    pub handshake_ok: bool,
    /// Frames answered (including `error` replies).
    pub frames_replied: u64,
    /// Replies of kind `error`.
    pub errors_replied: u64,
    /// Whether the peer closed cleanly at a frame boundary.
    pub clean_eof: bool,
}

/// Apply one [`crate::daemon::StepOutcome`] to the stream.
///
/// Returns `false` when the connection must close (`Close`,
/// `CloseWithReply`, or an unanswerable write failure); `true` keeps it open.
fn pump_step(
    stream: &mut Stream,
    step: StepOutcome,
    clean_eof: bool,
    stats: &mut ConnectionStats,
) -> bool {
    match step {
        StepOutcome::Reply(kind, body) => {
            if write_frame(stream, kind, &body).is_err() {
                return false;
            }
            stats.frames_replied = stats.frames_replied.saturating_add(1);
            if kind == MessageType::Error {
                stats.errors_replied = stats.errors_replied.saturating_add(1);
            }
            true
        }
        StepOutcome::Silent => true,
        StepOutcome::Close => {
            if clean_eof {
                stats.clean_eof = true;
            }
            false
        }
        StepOutcome::CloseWithReply(kind, body) => {
            let _ = write_frame(stream, kind, &body);
            stats.errors_replied = stats.errors_replied.saturating_add(1);
            false
        }
    }
}

/// Serve one connection: hello handshake (reusing
/// [`synthlm_common::ipc::server_handshake`] semantics, which answers
/// handshake failures best-effort), then answer frames per
/// [`crate::daemon::decide`] until EOF, shutdown, or an unrecoverable
/// stream state. Never panics on peer input.
///
/// The skeleton path: kept for the unit-tested handshake contract; the
/// serving path ([`crate::daemon::serve_until_shutdown`]) uses
/// [`crate::daemon::serve_connection_dispatched`] instead.
pub fn serve_connection(stream: &mut Stream, shutdown: &Shutdown) -> ConnectionStats {
    let mut stats = ConnectionStats::default();
    match server_handshake(stream, EndpointRole::Acrd) {
        Ok(_) => stats.handshake_ok = true,
        Err(_) => return stats,
    }
    loop {
        if shutdown.is_requested() {
            break;
        }
        let outcome = read_frame(stream);
        let clean = matches!(outcome, Err(IpcError::TransportClosed));
        let step = decide(&outcome);
        if !pump_step(stream, step, clean, &mut stats) {
            break;
        }
    }
    stats
}

/// Serve one connection through the true dispatcher
/// ([`crate::dispatch::Dispatcher`]): hello handshake, then one
/// `dispatch_outcome` per frame until EOF, shutdown, or an unrecoverable
/// stream state. Never panics on peer input; a poisoned dispatch store
/// answers `error` (internal) without dropping the connection.
pub fn serve_connection_dispatched(
    stream: &mut Stream,
    shutdown: &Shutdown,
    dispatcher: &Arc<Mutex<Dispatcher>>,
) -> ConnectionStats {
    let mut stats = ConnectionStats::default();
    match server_handshake(stream, EndpointRole::Acrd) {
        Ok(_) => stats.handshake_ok = true,
        Err(_) => return stats,
    }
    loop {
        if shutdown.is_requested() {
            break;
        }
        let outcome = read_frame(stream);
        let clean = matches!(outcome, Err(IpcError::TransportClosed));
        let step = match dispatcher.lock() {
            Ok(mut guard) => guard.dispatch_outcome(&outcome),
            Err(_) => StepOutcome::Reply(
                MessageType::Error,
                error_body(ErrorCode::Internal, "dispatch store poisoned"),
            ),
        };
        if !pump_step(stream, step, clean, &mut stats) {
            break;
        }
    }
    stats
}

/// Watchdog heartbeat payload: pid + monotonic sequence + wall-clock time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heartbeat {
    /// Writer pid.
    pub pid: u32,
    /// Monotonic sequence number (1-based per process run).
    pub seq: u64,
    /// Unix epoch milliseconds at write time.
    pub ts_unix_ms: i64,
}

/// Heartbeat file path for a state directory.
pub fn heartbeat_path(state_dir: &Path) -> PathBuf {
    state_dir.join(HEARTBEAT_FILE_NAME)
}

/// Current wall-clock milliseconds as `i64` (clamped, never panics).
fn now_ms_i64() -> i64 {
    i64::try_from(now_ms()).unwrap_or(i64::MAX)
}

/// Atomically write `bytes` to `path` (same-directory tmp + rename).
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Write one heartbeat file commit (`pid` + `seq` + time), atomically.
pub fn write_heartbeat(path: &Path, seq: u64) -> Result<(), DaemonError> {
    let body = serde_json::json!({
        "pid": std::process::id(),
        "seq": seq,
        "ts_unix_ms": now_ms_i64(),
    });
    let text = serde_json::to_string(&body)
        .map_err(|_| DaemonError::Heartbeat("heartbeat encode failed".to_owned()))?;
    atomic_write(path, text.as_bytes())
        .map_err(|_| DaemonError::Heartbeat("heartbeat write failed".to_owned()))?;
    Ok(())
}

/// Read back a heartbeat file.
pub fn read_heartbeat(path: &Path) -> Result<Heartbeat, DaemonError> {
    let text = fs::read_to_string(path)
        .map_err(|_| DaemonError::Heartbeat("heartbeat unreadable".to_owned()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| DaemonError::Heartbeat("heartbeat corrupt".to_owned()))?;
    let pid = value
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| DaemonError::Heartbeat("heartbeat pid invalid".to_owned()))?;
    let seq = value
        .get("seq")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| DaemonError::Heartbeat("heartbeat seq invalid".to_owned()))?;
    let ts_unix_ms = value
        .get("ts_unix_ms")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| DaemonError::Heartbeat("heartbeat time invalid".to_owned()))?;
    Ok(Heartbeat {
        pid,
        seq,
        ts_unix_ms,
    })
}

/// Pure staleness predicate: `true` once `now - last >= threshold`.
/// A `now` behind `last` (clock skew) is never stale.
pub fn is_stale_heartbeat(last_ts_ms: i64, now_ms: i64, threshold_ms: u64) -> bool {
    if now_ms < last_ts_ms {
        return false;
    }
    u64::try_from(now_ms - last_ts_ms).unwrap_or(u64::MAX) >= threshold_ms
}

/// Sleep `total_ms` in small chunks, returning early on shutdown so the
/// heartbeat thread never delays the drain by more than ~50ms.
fn wait_interruptible(total_ms: u64, shutdown: &Shutdown) {
    let mut waited: u64 = 0;
    while waited < total_ms {
        if shutdown.is_requested() {
            break;
        }
        let chunk = total_ms.saturating_sub(waited).min(50);
        std::thread::sleep(Duration::from_millis(chunk));
        waited = waited.saturating_add(chunk);
    }
}

/// Heartbeat loop: write every `interval_ms`, read back each commit as a
/// watchdog self-check (sequence match + freshness), counting anomalies.
fn heartbeat_loop(
    path: &Path,
    interval_ms: u64,
    shutdown: &Shutdown,
    writes: &Arc<AtomicU64>,
    failures: &Arc<AtomicU64>,
) {
    let interval = interval_ms.max(1);
    let threshold = interval.saturating_mul(5).max(1_000);
    let mut seq: u64 = 0;
    loop {
        if shutdown.is_requested() {
            break;
        }
        seq = seq.saturating_add(1);
        match write_heartbeat(path, seq) {
            Ok(()) => {
                writes.fetch_add(1, Ordering::SeqCst);
                let verified = match read_heartbeat(path) {
                    Ok(back) => {
                        back.seq == seq
                            && back.pid == std::process::id()
                            && !is_stale_heartbeat(back.ts_unix_ms, now_ms_i64(), threshold)
                    }
                    Err(_) => false,
                };
                if !verified {
                    failures.fetch_add(1, Ordering::SeqCst);
                }
            }
            Err(_) => {
                failures.fetch_add(1, Ordering::SeqCst);
            }
        }
        wait_interruptible(interval, shutdown);
    }
}

/// Serve summary: graceful shutdown always returns `Ok` (exit code 0).
#[derive(Clone, Copy, Debug, Default)]
pub struct ServeReport {
    /// Accepted connections (including stillborn wakeup dials).
    pub connections: u64,
    /// Frames answered across all connections.
    pub frames_replied: u64,
    /// `error` replies across all connections.
    pub errors_replied: u64,
    /// Accept-loop transport failures (non-fatal; loop sleeps and retries).
    pub accept_errors: u64,
    /// Successful heartbeat commits.
    pub heartbeats: u64,
    /// Heartbeat write/verify failures (counted, never fatal).
    pub heartbeat_failures: u64,
    /// Tasks present after the startup journal replay.
    pub journal_tasks: u64,
    /// Corrupt journal lines skipped by the startup replay.
    pub journal_skipped: u64,
    /// Whether the frame idle timeout is enforced on this transport.
    /// Starts `true`; any failed timeout setup flips it to `false`
    /// (Windows named pipes have no timeout support, so idle connections
    /// there drain on peer EOF instead of timing out).
    pub frame_timeout_enforced: bool,
}

/// Accept loop: heartbeat thread plus one handler thread per connection.
/// Returns when [`crate::daemon::Shutdown`] is requested; joins every
/// handler (drain) before returning.
///
/// Connections share one [`crate::dispatch::Dispatcher`] (mutex-guarded, so
/// the WAL file stays the single serialization point across connections).
/// Opening the dispatcher journals nothing by itself; per-connection frames
/// drive the planner chain.
pub fn serve_until_shutdown(
    listener: Listener,
    opts: &ServeOptions,
    shutdown: &Shutdown,
) -> Result<ServeReport, DaemonError> {
    // The heartbeat lives in the state dir; ensure it exists even if a
    // caller bypasses the journal open (or the dir was removed at runtime).
    fs::create_dir_all(&opts.state_dir)
        .map_err(|_| DaemonError::Heartbeat("state dir not creatable".to_owned()))?;
    let hb_path = heartbeat_path(&opts.state_dir);
    let writes = Arc::new(AtomicU64::new(0));
    let failures = Arc::new(AtomicU64::new(0));
    let hb_shutdown = shutdown.clone();
    let hb_interval = opts.heartbeat_interval_ms;
    let hb_writes = Arc::clone(&writes);
    let hb_failures = Arc::clone(&failures);
    let hb_handle = std::thread::spawn(move || {
        heartbeat_loop(
            &hb_path,
            hb_interval,
            &hb_shutdown,
            &hb_writes,
            &hb_failures,
        )
    });

    let mut report = ServeReport {
        frame_timeout_enforced: true,
        ..ServeReport::default()
    };
    let dispatcher = match Dispatcher::open(&opts.state_dir) {
        Ok(dispatcher) => dispatcher,
        Err(crate::dispatch::DispatchError::Store) => {
            return Err(DaemonError::Journal(
                "dispatch journal not openable".to_owned(),
            ));
        }
        Err(_) => {
            return Err(DaemonError::Dispatch(
                "dispatch profile not loadable".to_owned(),
            ));
        }
    };
    let dispatcher = Arc::new(Mutex::new(dispatcher));
    let mut handles = Vec::new();
    loop {
        if shutdown.is_requested() {
            break;
        }
        match accept_next(&listener) {
            Ok(mut stream) => {
                report.connections = report.connections.saturating_add(1);
                // Best-effort: transports without timeout support (Windows
                // named pipes) serve without an idle timeout rather than
                // dropping the connection; the flag records which mode
                // holds. Never fail a connection over this.
                if set_stream_timeouts(
                    &stream,
                    Some(opts.frame_timeout_ms),
                    Some(opts.frame_timeout_ms),
                )
                .is_err()
                {
                    report.frame_timeout_enforced = false;
                }
                let flag = shutdown.clone();
                let bus = Arc::clone(&dispatcher);
                handles.push(std::thread::spawn(move || {
                    serve_connection_dispatched(&mut stream, &flag, &bus)
                }));
            }
            Err(_) => {
                if shutdown.is_requested() {
                    break;
                }
                report.accept_errors = report.accept_errors.saturating_add(1);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    for handle in handles {
        if let Ok(stats) = handle.join() {
            report.frames_replied = report.frames_replied.saturating_add(stats.frames_replied);
            report.errors_replied = report.errors_replied.saturating_add(stats.errors_replied);
        }
    }
    let _ = hb_handle.join();
    report.heartbeats = writes.load(Ordering::SeqCst);
    report.heartbeat_failures = failures.load(Ordering::SeqCst);
    Ok(report)
}

/// Default state directory (DEC-027): `%APPDATA%\SynthLM\acrd` on Windows,
/// `$XDG_DATA_HOME/synthlm/acrd` (else `~/.local/share/synthlm/acrd`) on
/// Unix, temp-dir fallback when no base is available.
pub fn default_state_dir() -> PathBuf {
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA")
            && !appdata.is_empty()
        {
            return PathBuf::from(appdata).join("SynthLM").join("acrd");
        }
    } else if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("synthlm").join("acrd");
    } else if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("synthlm")
            .join("acrd");
    }
    std::env::temp_dir().join("synthlm-acrd")
}

/// Resolve the daemon state dir against `base`, falling back to the
/// project-relative dir on an unwritable base (DEC-027 reversal).
///
/// A writable `base` wins with no notice. Otherwise the state dir becomes
/// `project_dir` joined with [`synthlm_common::consent::FALLBACK_DIR_NAME`] and
/// `acrd` (consent and daemon state never share a directory), plus an
/// actionable notice carrying relative names and the opaque
/// [`synthlm_common::consent::path_fingerprint`] only — never an absolute path
/// (AGENTS.md §8). Besides the writability probe no journal or heartbeat file
/// is touched here.
pub fn resolve_state_dir_in(base: &Path, project_dir: &Path) -> (PathBuf, Option<String>) {
    if dir_writable(base) {
        return (base.to_path_buf(), None);
    }
    let fallback = project_dir.join(FALLBACK_DIR_NAME).join("acrd");
    let notice = format!(
        "daemon state directory not writable (dir fingerprint {}); fell back to project-relative `.synthlm/acrd/`. Make the default state directory writable or pass `--state-dir`, then restart (see DEC-027).",
        path_fingerprint(base)
    );
    (fallback, Some(notice))
}

/// Resolve the daemon state dir against [`crate::daemon::default_state_dir`]
/// with the same project-relative fallback as
/// [`crate::daemon::resolve_state_dir_in`].
pub fn resolve_state_dir(project_dir: &Path) -> (PathBuf, Option<String>) {
    resolve_state_dir_in(&default_state_dir(), project_dir)
}

/// Run the daemon: replay the journal, record the serve session as a task
/// (`Running`, → `Done` on graceful shutdown so a crash replays it as
/// `Pending`), install the Ctrl-C/TERM hook, bind the endpoint, and serve
/// until shutdown.
///
/// The signal hook runs [`crate::daemon::Shutdown::request`] on a dedicated
/// thread (the `ctrlc` crate never runs handlers in async-signal context),
/// so the request path — atomic flag plus a best-effort wakeup dial — is
/// safe there. Shutdown drains in-flight connections and returns `Ok`.
pub fn run_serve(opts: &ServeOptions) -> Result<ServeReport, DaemonError> {
    let mut journal =
        TaskLog::open(&opts.state_dir).map_err(|err| DaemonError::Journal(err.to_string()))?;
    let journal_tasks = u64::try_from(journal.task_count()).unwrap_or(u64::MAX);
    let journal_skipped = journal.skipped_corrupt();
    let session_id = format!("serve-{}-{}", std::process::id(), now_ms());
    journal
        .create_task(session_id.clone(), "serve")
        .map_err(|err| DaemonError::SessionTask(err.to_string()))?;
    journal
        .transition(&session_id, TaskState::Running)
        .map_err(|err| DaemonError::SessionTask(err.to_string()))?;
    match journal.get(&session_id) {
        Some(task)
            if task.id == session_id
                && task.kind == "serve"
                && task.state == TaskState::Running => {}
        _ => {
            return Err(DaemonError::SessionTask(
                "session task not journaled".to_owned(),
            ));
        }
    }

    let listener = match &opts.endpoint_id {
        Some(id) => bind_endpoint(id),
        None => bind_listener(),
    }
    .map_err(|err| DaemonError::Bind(format!("endpoint bind failed: {:?}", err.kind())))?;

    let wake_id = opts
        .endpoint_id
        .clone()
        .unwrap_or_else(|| synthlm_common::ipc::SOCKET_ID.to_owned());
    let shutdown = Shutdown::with_wakeup(wake_id);
    {
        let flag = shutdown.clone();
        ctrlc::set_handler(move || flag.request())
            .map_err(|err| DaemonError::Signal(format!("signal hook failed: {err}")))?;
    }
    let mut report = serve_until_shutdown(listener, opts, &shutdown)?;
    report.journal_tasks = journal_tasks;
    report.journal_skipped = journal_skipped;
    let _ = journal.transition(&session_id, TaskState::Done);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use synthlm_common::ipc::{
        EndpointRole, RetryPolicy, client_handshake, connect_to_with_retry, current_version,
        decode_frame_bytes, parse_error, recv_frame, send_frame,
    };

    /// Unique test endpoint id (loopback/namespaced transport only).
    fn unique_endpoint(tag: &str) -> String {
        format!("synthlm-t501-{tag}-{}", std::process::id())
    }

    /// Unique temp state dir (removed after each test).
    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("synthlm-acrd-daemon-{}-{tag}", std::process::id()))
    }

    /// Fast test options: temp dir, short heartbeat + frame timeout.
    fn test_options(endpoint: &str, tag: &str) -> (ServeOptions, PathBuf) {
        let dir = scratch_dir(tag);
        let _ = fs::remove_dir_all(&dir);
        (
            ServeOptions {
                endpoint_id: Some(endpoint.to_owned()),
                state_dir: dir.clone(),
                heartbeat_interval_ms: 50,
                frame_timeout_ms: 2_000,
            },
            dir,
        )
    }

    fn retry_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 50,
            base_delay_ms: 10,
            max_delay_ms: 50,
        }
    }

    #[test]
    fn decide_answers_every_known_kind_with_a_legal_reply() {
        for kind in MessageType::all() {
            let frame = WireFrame {
                ver: current_version(),
                kind: *kind,
                body: serde_json::json!({}),
            };
            match decide(&Ok(frame)) {
                StepOutcome::Reply(reply_kind, body) => {
                    assert!(body.is_object(), "{kind:?} reply must be an object");
                    let expected = match kind {
                        MessageType::Error => None,
                        MessageType::Hello => Some(MessageType::Hello),
                        MessageType::SnapshotSubmit | MessageType::PlanRequest => {
                            Some(MessageType::PlanResponse)
                        }
                        MessageType::PatchApply => Some(MessageType::PatchResult),
                        MessageType::RenderRequest => Some(MessageType::RenderResult),
                        MessageType::PlanResponse => Some(MessageType::PlanResponse),
                        MessageType::PatchResult => Some(MessageType::PatchResult),
                        MessageType::RenderResult => Some(MessageType::RenderResult),
                        MessageType::ScoreReport => Some(MessageType::ScoreReport),
                        MessageType::ConsentGet => Some(MessageType::ConsentGet),
                        MessageType::ConsentSet => Some(MessageType::ConsentSet),
                        MessageType::AuditEvent => Some(MessageType::AuditEvent),
                    };
                    assert_eq!(Some(reply_kind), expected, "{kind:?} reply kind");
                }
                StepOutcome::Silent => {
                    assert_eq!(*kind, MessageType::Error, "only error stays silent");
                }
                other => panic!("{kind:?} must reply or stay silent, got {other:?}"),
            }
        }
    }

    #[test]
    fn error_code_strings_match_wire() {
        for code in ErrorCode::all() {
            let wire = serde_json::to_value(*code).expect("serialize code");
            assert_eq!(
                wire.as_str(),
                Some(error_code_as_str(*code)),
                "{code:?} string drift"
            );
        }
    }

    #[test]
    fn unknown_wire_type_gets_error_reply_and_stays_open() {
        // Unknown `"type"` strings fail frame parsing upstream...
        let payload = format!(
            "{{\"ver\":{{\"major\":{},\"minor\":{}}},\"type\":\"no.such.type\",\"body\":{{}}}}",
            PROTOCOL_MAJOR, PROTOCOL_MINOR
        );
        let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(payload.as_bytes());
        let err = decode_frame_bytes(&bytes).expect_err("unknown type must fail");
        // ...and the daemon answers `error` while keeping the connection.
        match decide(&Err(err)) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::Error);
                assert_eq!(
                    body.get("code").and_then(serde_json::Value::as_str),
                    Some("protocol_violation")
                );
                assert_eq!(
                    body.get("retryable").and_then(serde_json::Value::as_bool),
                    Some(false)
                );
            }
            other => panic!("unknown type must get an error reply, got {other:?}"),
        }
        // Truncated input closes (the peer is gone mid-frame).
        match decide(&Err(IpcError::Truncated {
            expected: 10,
            got: 3,
        })) {
            StepOutcome::Close => {}
            other => panic!("truncated must close, got {other:?}"),
        }
        // Oversize prefixes reply best-effort, then must drop the connection.
        match decide(&Err(IpcError::FrameTooLarge {
            bytes: usize::MAX,
            max: 1,
        })) {
            StepOutcome::CloseWithReply(kind, _) => assert_eq!(kind, MessageType::Error),
            other => panic!("oversize must close with reply, got {other:?}"),
        }
    }

    #[test]
    fn handshake_and_minimal_roundtrip_over_test_endpoint() {
        let endpoint = unique_endpoint("roundtrip");
        let listener = bind_endpoint(&endpoint).expect("bind test endpoint");
        let server = std::thread::spawn(move || {
            let mut stream = accept_next(&listener).expect("accept");
            serve_connection(&mut stream, &Shutdown::default())
        });

        let mut client = connect_to_with_retry(&endpoint, &retry_policy()).expect("connect");
        let peer = client_handshake(&mut client, EndpointRole::Ui).expect("handshake");
        assert_eq!(peer.role, EndpointRole::Acrd);

        send_frame(
            &mut client,
            MessageType::SnapshotSubmit,
            &serde_json::json!({"snapshot_id": "s1"}),
        )
        .expect("send snapshot");
        let reply = recv_frame(&mut client).expect("recv plan");
        assert_eq!(reply.kind, MessageType::PlanResponse);

        // Unknown wire type over the live connection: error frame, no drop.
        let payload = format!(
            "{{\"ver\":{{\"major\":{},\"minor\":{}}},\"type\":\"no.such.type\",\"body\":{{}}}}",
            PROTOCOL_MAJOR, PROTOCOL_MINOR
        );
        let mut raw = (payload.len() as u32).to_le_bytes().to_vec();
        raw.extend_from_slice(payload.as_bytes());
        {
            use std::io::Write as _;
            client.write_all(&raw).expect("send unknown");
            client.flush().expect("flush");
        }
        let reply = recv_frame(&mut client).expect("recv error");
        assert_eq!(reply.kind, MessageType::Error);
        let body = parse_error(&reply).expect("parse error");
        assert_eq!(body.code, ErrorCode::ProtocolViolation);

        // Connection is still alive.
        send_frame(&mut client, MessageType::ConsentGet, &serde_json::json!({}))
            .expect("send consent.get");
        let reply = recv_frame(&mut client).expect("recv consent");
        assert_eq!(reply.kind, MessageType::ConsentGet);
        drop(client);

        let stats = server.join().expect("server thread");
        assert!(stats.handshake_ok);
        assert!(stats.frames_replied >= 3);
        assert!(stats.errors_replied >= 1);
        assert!(stats.clean_eof);
    }

    #[test]
    fn graceful_shutdown_drains_and_reports_ok() {
        let endpoint = unique_endpoint("shutdown");
        let (opts, dir) = test_options(&endpoint, "shutdown");
        let shutdown = Shutdown::with_wakeup(endpoint.clone());
        let worker_shutdown = shutdown.clone();
        let listener = bind_endpoint(&endpoint).expect("bind test endpoint");
        let server =
            std::thread::spawn(move || serve_until_shutdown(listener, &opts, &worker_shutdown));

        let mut client = connect_to_with_retry(&endpoint, &retry_policy()).expect("connect");
        client_handshake(&mut client, EndpointRole::Bridge).expect("handshake");
        // The serving path dispatches for real: snapshot first, then plan.
        send_frame(
            &mut client,
            MessageType::SnapshotSubmit,
            &serde_json::json!({
                "task_id": "t-shutdown-1",
                "snapshot_id": "snap-shutdown-1",
                "take_guid": "{take-shutdown-1}",
            }),
        )
        .expect("send snapshot.submit");
        let reply = recv_frame(&mut client).expect("recv snapshot ack");
        assert_eq!(reply.kind, MessageType::AuditEvent);
        send_frame(
            &mut client,
            MessageType::PlanRequest,
            &serde_json::json!({"task_id": "t-shutdown-1", "snapshot_id": "snap-shutdown-1"}),
        )
        .expect("send plan.request");
        let reply = recv_frame(&mut client).expect("recv plan.response");
        assert_eq!(reply.kind, MessageType::PlanResponse);
        assert_eq!(
            reply
                .body
                .get("mock")
                .and_then(|mock| mock.get("transport"))
                .and_then(serde_json::Value::as_str),
            Some("MockTransport"),
            "serving path must run the mock planner chain"
        );
        drop(client);

        // Let the heartbeat tick at least once, then shut down gracefully.
        std::thread::sleep(Duration::from_millis(200));
        shutdown.request();
        let report = server.join().expect("server thread").expect("serve ok");
        assert!(report.connections >= 1);
        assert!(report.frames_replied >= 1);
        assert!(report.heartbeats >= 1, "main loop must write heartbeats");
        assert_eq!(
            report.heartbeat_failures, 0,
            "local heartbeat writes must verify"
        );
        if cfg!(windows) {
            assert!(
                !report.frame_timeout_enforced,
                "windows named pipes have no timeout support"
            );
        }
        assert!(heartbeat_path(&dir).is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn heartbeat_roundtrip_and_staleness_rules() {
        let dir = scratch_dir("heartbeat");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        let path = heartbeat_path(&dir);
        write_heartbeat(&path, 1).expect("write 1");
        write_heartbeat(&path, 2).expect("write 2");
        let back = read_heartbeat(&path).expect("read back");
        assert_eq!(back.pid, std::process::id());
        assert_eq!(back.seq, 2);

        assert!(is_stale_heartbeat(1_000, 1_000 + 5_000, 5_000));
        assert!(!is_stale_heartbeat(1_000, 1_000 + 4_999, 5_000));
        assert!(
            !is_stale_heartbeat(2_000, 1_000, 5_000),
            "clock skew never stale"
        );
        assert!(is_stale_heartbeat(7, 7, 0), "zero threshold is immediate");

        fs::write(&path, b"not json").expect("corrupt file");
        read_heartbeat(&path).expect_err("corrupt heartbeat must fail");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_state_dir_ends_in_acrd() {
        let dir = default_state_dir();
        assert_eq!(dir.file_name().and_then(|name| name.to_str()), Some("acrd"));
    }

    #[test]
    fn state_dir_fallback_prefers_writable_base() {
        let base = scratch_dir("state-ok");
        let _ = fs::remove_dir_all(&base);
        let project = scratch_dir("state-proj-unused");
        let (dir, notice) = resolve_state_dir_in(&base, &project);
        assert_eq!(dir, base);
        assert_eq!(notice, None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn state_dir_fallback_uses_project_relative_without_absolute_paths() {
        // A regular file where a directory is expected: directory creation
        // fails on every OS, which simulates "state dir not writable" without
        // touching permissions.
        let scratch = scratch_dir("state-blocked");
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(&scratch).expect("scratch dir");
        let blocker = scratch.join("blocker");
        fs::write(&blocker, b"x").expect("blocker file");
        let project = scratch_dir("state-proj");
        let (dir, notice) = resolve_state_dir_in(&blocker, &project);
        assert_eq!(dir, project.join(".synthlm").join("acrd"));
        let notice = notice.expect("fallback notice");
        assert!(
            notice.contains(".synthlm/acrd"),
            "relative dir named: {notice}"
        );
        assert!(notice.contains("fingerprint"), "opaque id: {notice}");
        let scratch_text = scratch.to_string_lossy();
        assert!(
            !notice.contains(scratch_text.as_ref()),
            "no absolute path: {notice}"
        );
        let _ = fs::remove_dir_all(&scratch);
    }
}
