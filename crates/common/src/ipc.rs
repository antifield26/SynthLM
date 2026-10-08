//! IPC protocol kernel: framing, handshake, error taxonomy, retry policy,
//! audit events, and local-socket transport.
//!
//! Implements DEC-023 and ARCHITECTURE §5:
//!
//! - transport: `interprocess` local socket (Windows named pipe / Unix UDS),
//!   short names plus startup cleanup; large payloads stay out of this
//!   protocol (shared memory arrives in a later task).
//! - framing: `u32` little-endian length prefix + JSON body carrying
//!   `ver` + `type` + `body` (first JSON version; `prost` only if profiling
//!   proves JSON exceeds 30% of latency, per DEC-023 reversal condition).
//! - handshake: `hello{version}`; major version must match, minor is free.
//! - errors: `ErrorCode` is the single source of truth for
//!   retryable-vs-blocked; `consent_required`, auth, whitelist violations,
//!   and missing audio capability never retry.
//! - audit: `AuditEvent` carries only time / model / tier / field list /
//!   byte count. The type has no key/PCM fields by construction (AGENTS.md
//!   §8); the `audit_event_serialization_contains_no_secrets` unit test is the
//!   backstop.
//!
//! `interprocess` API verified against docs.rs 2.4.4 on 2026-10-06:
//! <https://docs.rs/interprocess/2.4.4/interprocess/local_socket/>
//!
//! Platform notes (D-eng-eco §2): Windows named-pipe servers must exist
//! before clients connect (`connect_to_with_retry` covers the startup
//! race); Unix UDS paths are length-limited, so filesystem names live under
//! `socket_file_path` (short, temp-dir based) and stale files are reclaimed
//! at bind time by `interprocess` (`reclaim_name` defaults on).
//!
//! Blocking contract: every helper that touches a socket blocks the calling
//! thread (`connect_to_with_retry` sleeps between attempts). Never call these
//! from an audio thread (AGENTS.md red line 2); they are connect/handshake
//! time utilities for `acrd` / `bridge` / `ui` control threads.

use std::io::{Read, Write};
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Listener, ListenerOptions, Stream,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// Current IPC protocol major version.
///
/// A major bump means wire-incompatible framing; per DEC-023 the old major
/// stays accepted for one release cycle (see the private `check_version`
/// helper for the current gate).
pub const PROTOCOL_MAJOR: u16 = 1;

/// Current IPC protocol minor version (backward-compatible extensions).
pub const PROTOCOL_MINOR: u16 = 0;

/// Semver-style protocol version exchanged in every frame and `hello`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    /// Major version: must match exactly for peers to interoperate.
    pub major: u16,
    /// Minor version: informational only, never gates compatibility.
    pub minor: u16,
}

/// Version this build speaks.
pub fn current_version() -> Version {
    Version {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    }
}

/// Reject peers whose major version differs from ours.
fn check_version(peer: &Version) -> Result<(), IpcError> {
    let ours = current_version();
    if peer.major != ours.major {
        return Err(IpcError::VersionMismatch {
            peer_major: peer.major,
            peer_minor: peer.minor,
            our_major: ours.major,
            our_minor: ours.minor,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

/// IPC message types (ARCHITECTURE §5).
///
/// Wire strings are fixed by `serde(rename)` and mirrored by
/// [`MessageType::as_str`]; the `message_type_wire_strings_match` unit test
/// keeps the two in sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    /// Handshake: `{ version, role }`.
    #[serde(rename = "hello")]
    Hello,
    /// Bridge submits a frozen project snapshot for solving.
    #[serde(rename = "snapshot.submit")]
    SnapshotSubmit,
    /// UI/acrd requests a patch plan.
    #[serde(rename = "plan.request")]
    PlanRequest,
    /// acrd returns a patch plan.
    #[serde(rename = "plan.response")]
    PlanResponse,
    /// acrd/bridge requests patch application.
    #[serde(rename = "patch.apply")]
    PatchApply,
    /// Bridge reports the application result.
    #[serde(rename = "patch.result")]
    PatchResult,
    /// acrd requests an offline render.
    #[serde(rename = "render.request")]
    RenderRequest,
    /// Render worker returns audio references + fingerprints (never PCM).
    #[serde(rename = "render.result")]
    RenderResult,
    /// Eval reports candidate scores.
    #[serde(rename = "score.report")]
    ScoreReport,
    /// Query the current consent tier.
    #[serde(rename = "consent.get")]
    ConsentGet,
    /// Set the consent tier (user-driven only).
    #[serde(rename = "consent.set")]
    ConsentSet,
    /// Cloud-call audit record (see [`crate::ipc::AuditEvent`]).
    #[serde(rename = "audit.event")]
    AuditEvent,
    /// Error envelope `{ code, retryable, detail }`.
    #[serde(rename = "error")]
    Error,
}

impl MessageType {
    /// Wire string for this message type (mirrors the `serde` renames).
    pub fn as_str(self) -> &'static str {
        match self {
            MessageType::Hello => "hello",
            MessageType::SnapshotSubmit => "snapshot.submit",
            MessageType::PlanRequest => "plan.request",
            MessageType::PlanResponse => "plan.response",
            MessageType::PatchApply => "patch.apply",
            MessageType::PatchResult => "patch.result",
            MessageType::RenderRequest => "render.request",
            MessageType::RenderResult => "render.result",
            MessageType::ScoreReport => "score.report",
            MessageType::ConsentGet => "consent.get",
            MessageType::ConsentSet => "consent.set",
            MessageType::AuditEvent => "audit.event",
            MessageType::Error => "error",
        }
    }

    /// Every message type the protocol knows about (for exhaustive tests).
    pub fn all() -> &'static [MessageType] {
        &[
            MessageType::Hello,
            MessageType::SnapshotSubmit,
            MessageType::PlanRequest,
            MessageType::PlanResponse,
            MessageType::PatchApply,
            MessageType::PatchResult,
            MessageType::RenderRequest,
            MessageType::RenderResult,
            MessageType::ScoreReport,
            MessageType::ConsentGet,
            MessageType::ConsentSet,
            MessageType::AuditEvent,
            MessageType::Error,
        ]
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// Maximum accepted frame payload in bytes (16 MiB).
///
/// Snapshots carry chunk strings, so the cap is generous but finite; larger
/// payloads are rejected before allocation (see [`crate::ipc::read_frame`]).
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Length-prefix width in bytes (`u32` little-endian).
pub const LEN_PREFIX_BYTES: usize = 4;

/// On-wire frame: version + type + JSON body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFrame {
    /// Protocol version of the sender.
    pub ver: Version,
    /// Message discriminator (serialized as `"type"` on the wire).
    #[serde(rename = "type")]
    pub kind: MessageType,
    /// Message payload; schema is per-`kind` and versioned separately.
    pub body: serde_json::Value,
}

/// Encode one length-prefixed frame for `kind` + JSON `body`.
pub fn encode_frame(kind: MessageType, body: &serde_json::Value) -> Result<Vec<u8>, IpcError> {
    let frame = WireFrame {
        ver: current_version(),
        kind,
        body: body.clone(),
    };
    let payload = serde_json::to_vec(&frame).map_err(IpcError::Json)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            bytes: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    let len = u32::try_from(payload.len()).map_err(|_| IpcError::FrameTooLarge {
        bytes: payload.len(),
        max: MAX_FRAME_BYTES,
    })?;
    let mut out = Vec::with_capacity(LEN_PREFIX_BYTES + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Encode one frame from any serializable body (convenience wrapper).
pub fn encode_json_frame<T: Serialize>(kind: MessageType, body: &T) -> Result<Vec<u8>, IpcError> {
    let value = serde_json::to_value(body).map_err(IpcError::Json)?;
    encode_frame(kind, &value)
}

/// Parse and validate a frame payload (length prefix already stripped).
fn parse_payload(payload: &[u8]) -> Result<WireFrame, IpcError> {
    if payload.is_empty() {
        return Err(IpcError::Protocol("empty frame payload".to_owned()));
    }
    let frame: WireFrame = serde_json::from_slice(payload).map_err(IpcError::Json)?;
    check_version(&frame.ver)?;
    Ok(frame)
}

/// Decode one complete frame from a byte slice (prefix + payload).
///
/// Used by tests and by receivers that already own a full buffer; trailing
/// bytes after the first frame are ignored.
pub fn decode_frame_bytes(buf: &[u8]) -> Result<WireFrame, IpcError> {
    if buf.len() < LEN_PREFIX_BYTES {
        return Err(IpcError::Truncated {
            expected: LEN_PREFIX_BYTES,
            got: buf.len(),
        });
    }
    let mut prefix = [0u8; LEN_PREFIX_BYTES];
    prefix.copy_from_slice(&buf[..LEN_PREFIX_BYTES]);
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            bytes: len,
            max: MAX_FRAME_BYTES,
        });
    }
    if buf.len() - LEN_PREFIX_BYTES < len {
        return Err(IpcError::Truncated {
            expected: len,
            got: buf.len() - LEN_PREFIX_BYTES,
        });
    }
    parse_payload(&buf[LEN_PREFIX_BYTES..LEN_PREFIX_BYTES + len])
}

/// Read exactly `out.len()` bytes, counting progress.
///
/// A first-byte EOF at a frame boundary reports [`IpcError::TransportClosed`]
/// (clean peer shutdown); any later short read is [`IpcError::Truncated`].
fn read_full(reader: &mut impl Read, out: &mut [u8], boundary: bool) -> Result<(), IpcError> {
    let mut got = 0usize;
    while got < out.len() {
        match reader.read(&mut out[got..]) {
            Ok(0) => {
                if boundary && got == 0 {
                    return Err(IpcError::TransportClosed);
                }
                return Err(IpcError::Truncated {
                    expected: out.len(),
                    got,
                });
            }
            Ok(n) => got += n,
            Err(e) => return Err(map_io(e)),
        }
    }
    Ok(())
}

/// Write one length-prefixed frame to any byte stream.
pub fn write_frame(
    writer: &mut impl Write,
    kind: MessageType,
    body: &serde_json::Value,
) -> Result<(), IpcError> {
    let bytes = encode_frame(kind, body)?;
    writer.write_all(&bytes).map_err(map_io)?;
    Ok(())
}

/// Read one length-prefixed frame from any byte stream.
///
/// The declared length is capped at [`crate::ipc::MAX_FRAME_BYTES`] *before* allocation,
/// so a hostile prefix cannot force a large allocation.
pub fn read_frame(reader: &mut impl Read) -> Result<WireFrame, IpcError> {
    let mut prefix = [0u8; LEN_PREFIX_BYTES];
    read_full(reader, &mut prefix, true)?;
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_FRAME_BYTES {
        // Do not consume the (bogus) payload: the caller must drop the
        // connection; resynchronization is impossible without trust.
        return Err(IpcError::FrameTooLarge {
            bytes: len,
            max: MAX_FRAME_BYTES,
        });
    }
    let mut payload = vec![0u8; len];
    read_full(reader, &mut payload, false)?;
    parse_payload(&payload)
}

/// Send one frame over a connected socket.
pub fn send_frame(
    stream: &mut Stream,
    kind: MessageType,
    body: &serde_json::Value,
) -> Result<(), IpcError> {
    write_frame(stream, kind, body)
}

/// Receive one frame from a connected socket.
pub fn recv_frame(stream: &mut Stream) -> Result<WireFrame, IpcError> {
    read_frame(stream)
}

// ---------------------------------------------------------------------------
// Hello / handshake
// ---------------------------------------------------------------------------

/// Which sidecar-bus participant is speaking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointRole {
    /// Planning/retrieval/eval sidecar daemon.
    Acrd,
    /// REAPER extension (control plane only, never model code).
    Bridge,
    /// Standalone UI window.
    Ui,
}

/// `hello` body: `{ version, role }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Sender's protocol version.
    pub version: Version,
    /// Sender's bus role.
    pub role: EndpointRole,
}

/// Encode our `hello` frame for `role`.
pub fn hello_frame(role: EndpointRole) -> Result<Vec<u8>, IpcError> {
    encode_json_frame(
        MessageType::Hello,
        &Hello {
            version: current_version(),
            role,
        },
    )
}

/// Validate that `frame` is a version-compatible `hello` and return it.
pub fn parse_hello(frame: &WireFrame) -> Result<Hello, IpcError> {
    if frame.kind != MessageType::Hello {
        return Err(IpcError::Protocol(format!(
            "expected hello, got {}",
            frame.kind.as_str()
        )));
    }
    let hello: Hello = serde_json::from_value(frame.body.clone()).map_err(IpcError::Json)?;
    check_version(&hello.version)?;
    Ok(hello)
}

/// Encode an error envelope frame.
pub fn error_frame(code: ErrorCode, detail: &str) -> Result<Vec<u8>, IpcError> {
    encode_json_frame(MessageType::Error, &ErrorBody::new(code, detail))
}

/// Validate that `frame` is an `error` envelope and return it.
///
/// The wire `retryable` flag is recomputed from [`crate::ipc::ErrorCode`] (never
/// trusted), so a peer cannot talk us into retrying a terminal error.
pub fn parse_error(frame: &WireFrame) -> Result<ErrorBody, IpcError> {
    if frame.kind != MessageType::Error {
        return Err(IpcError::Protocol(format!(
            "expected error, got {}",
            frame.kind.as_str()
        )));
    }
    let body: ErrorBody = serde_json::from_value(frame.body.clone()).map_err(IpcError::Json)?;
    Ok(body.normalized())
}

/// Client side of the handshake: send `hello`, expect `hello` back.
///
/// A peer `error` reply surfaces as [`IpcError::Peer`].
pub fn client_handshake<S: Read + Write>(
    stream: &mut S,
    role: EndpointRole,
) -> Result<Hello, IpcError> {
    let out = hello_frame(role)?;
    stream.write_all(&out).map_err(map_io)?;
    let reply = read_frame(stream)?;
    match reply.kind {
        MessageType::Hello => parse_hello(&reply),
        MessageType::Error => {
            let body = parse_error(&reply)?;
            Err(IpcError::Peer {
                code: body.code,
                detail: body.detail,
            })
        }
        other => Err(IpcError::Protocol(format!(
            "unexpected handshake reply: {}",
            other.as_str()
        ))),
    }
}

/// Server side of the handshake: expect `hello`, reply `hello` (or `error`).
///
/// On rejection an `error` frame is sent best-effort before returning the
/// local error, so the peer never hangs waiting for a reply.
pub fn server_handshake<S: Read + Write>(
    stream: &mut S,
    role: EndpointRole,
) -> Result<Hello, IpcError> {
    // Note: `read_frame` already enforces the version gate, so a major
    // mismatch surfaces here (not in `parse_hello`); every first-frame
    // failure gets a best-effort `error` reply before returning.
    let first = match read_frame(stream) {
        Ok(frame) => frame,
        Err(e) => {
            reply_handshake_error(stream, &e);
            return Err(e);
        }
    };
    let peer = match parse_hello(&first) {
        Ok(hello) => hello,
        Err(e) => {
            reply_handshake_error(stream, &e);
            return Err(e);
        }
    };
    let out = hello_frame(role)?;
    stream.write_all(&out).map_err(map_io)?;
    Ok(peer)
}

/// Best-effort handshake rejection (never obscures the original error).
fn reply_handshake_error(stream: &mut impl Write, e: &IpcError) {
    if matches!(e, IpcError::TransportClosed) {
        return; // Peer is gone; a reply cannot land.
    }
    let detail = handshake_reject_detail(e);
    if let Ok(reply) = error_frame(e.code(), detail) {
        let _ = stream.write_all(&reply);
    }
}

/// Short, secret-free rejection reason for handshake `error` frames.
fn handshake_reject_detail(e: &IpcError) -> &'static str {
    match e {
        IpcError::VersionMismatch { .. } => "protocol major version mismatch",
        _ => "first frame must be hello",
    }
}

// ---------------------------------------------------------------------------
// Error taxonomy
// ---------------------------------------------------------------------------

/// Wire error codes (ARCHITECTURE §5, §8).
///
/// [`ErrorCode::retryable`] is the single source of truth: anything
/// non-retryable is user-visible `BLOCKED` with guidance, never silently
/// retried. In particular `consent_required`, auth denial, whitelist
/// violations, and missing audio capability are terminal by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    /// Remote call timed out (cloud 30s hard / P95 10s budget, ARCH §5).
    #[serde(rename = "timeout")]
    Timeout,
    /// Socket closed mid-conversation (includes connect failures).
    #[serde(rename = "transport_closed")]
    TransportClosed,
    /// Render null-test variance over threshold; re-render may converge.
    #[serde(rename = "render_unstable")]
    RenderUnstable,
    /// Cloud tier unreachable after local retries; fail over Tier1→Tier2.
    #[serde(rename = "cloud_unavailable")]
    CloudUnavailable,
    /// Peer speaks another protocol major.
    #[serde(rename = "version_mismatch")]
    VersionMismatch,
    /// Frame exceeds [`crate::ipc::MAX_FRAME_BYTES`].
    #[serde(rename = "frame_too_large")]
    FrameTooLarge,
    /// Stream ended mid-frame.
    #[serde(rename = "truncated_frame")]
    TruncatedFrame,
    /// Wrong message kind / order / malformed envelope.
    #[serde(rename = "protocol_violation")]
    ProtocolViolation,
    /// Credentials rejected (missing/invalid key material). Never retry:
    /// retrying cannot mint a valid key and risks lockout.
    #[serde(rename = "auth_denied")]
    AuthDenied,
    /// No upload consent for the requested tier. Never retry: only an
    /// explicit user tier change (consent dialog / settings) unblocks.
    #[serde(rename = "consent_required")]
    ConsentRequired,
    /// Upload field outside the audit whitelist. Never retry: the request
    /// itself is policy-illegal; fix the caller.
    #[serde(rename = "whitelist_violation")]
    WhitelistViolation,
    /// Local endpoint lacks audio input capability. Never retry: requires
    /// user action (another endpoint build / model).
    #[serde(rename = "audio_capability_missing")]
    AudioCapabilityMissing,
    /// Bug or unclassified local failure.
    #[serde(rename = "internal")]
    Internal,
}

impl ErrorCode {
    /// Whether this failure class may enter backoff retry.
    ///
    /// Terminal classes (`consent_required`, auth, whitelist, audio
    /// capability, protocol/version/frame/internal) always return `false`.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::Timeout
                | ErrorCode::TransportClosed
                | ErrorCode::RenderUnstable
                | ErrorCode::CloudUnavailable
        )
    }

    /// Whether this failure is user-visible `BLOCKED` (never retried).
    pub fn blocked(self) -> bool {
        !self.retryable()
    }

    /// Every error code (for exhaustive tests).
    pub fn all() -> &'static [ErrorCode] {
        &[
            ErrorCode::Timeout,
            ErrorCode::TransportClosed,
            ErrorCode::RenderUnstable,
            ErrorCode::CloudUnavailable,
            ErrorCode::VersionMismatch,
            ErrorCode::FrameTooLarge,
            ErrorCode::TruncatedFrame,
            ErrorCode::ProtocolViolation,
            ErrorCode::AuthDenied,
            ErrorCode::ConsentRequired,
            ErrorCode::WhitelistViolation,
            ErrorCode::AudioCapabilityMissing,
            ErrorCode::Internal,
        ]
    }
}

/// Wire `error` body: `{ code, retryable, detail }`.
///
/// `detail` must stay short and secret-free: no keys, no PCM, no prompt
/// text, no absolute paths (AGENTS.md §8). Fingerprints / relative paths
/// only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Machine-readable failure class.
    pub code: ErrorCode,
    /// Retry hint, always equal to `code.retryable()` when constructed via
    /// [`ErrorBody::new`] or [`crate::ipc::parse_error`].
    pub retryable: bool,
    /// Short human hint (e.g. `"cloud tier1 timed out after 30000ms"`).
    pub detail: String,
}

impl ErrorBody {
    /// Build an envelope; `retryable` is derived from `code`, never setbid.
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        let retryable = code.retryable();
        Self {
            code,
            retryable,
            detail: detail.into(),
        }
    }

    /// Re-derive `retryable` from `code`, discarding any wire value.
    pub fn normalized(mut self) -> Self {
        self.retryable = self.code.retryable();
        self
    }
}

/// Local IPC failure.
#[derive(Debug, Error)]
pub enum IpcError {
    /// Underlying transport I/O failure (excluding clean shutdown/timeout,
    /// which have dedicated variants).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Frame JSON malformed or body schema mismatch.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Peer protocol major differs from ours.
    #[error("version mismatch: peer {peer_major}.{peer_minor}, ours {our_major}.{our_minor}")]
    VersionMismatch {
        /// Peer's major version.
        peer_major: u16,
        /// Peer's minor version.
        peer_minor: u16,
        /// Our major version.
        our_major: u16,
        /// Our minor version.
        our_minor: u16,
    },
    /// Frame exceeds [`crate::ipc::MAX_FRAME_BYTES`].
    #[error("frame too large: {bytes} bytes (max {max})")]
    FrameTooLarge {
        /// Declared or encoded payload size.
        bytes: usize,
        /// Enforced cap.
        max: usize,
    },
    /// Stream ended before the frame completed.
    #[error("truncated frame: expected {expected} bytes, got {got}")]
    Truncated {
        /// Bytes required.
        expected: usize,
        /// Bytes actually read.
        got: usize,
    },
    /// Peer shut down cleanly at a frame boundary.
    #[error("transport closed")]
    TransportClosed,
    /// Wrong message kind/order or empty payload.
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// Peer sent an `error` envelope.
    #[error("peer error [{code:?}]: {detail}")]
    Peer {
        /// Peer's failure class.
        code: ErrorCode,
        /// Peer's hint text.
        detail: String,
    },
    /// A socket read/write hit its configured timeout.
    #[error("operation timed out")]
    Timeout,
}

impl IpcError {
    /// Best-effort mapping of a local failure to a wire [`crate::ipc::ErrorCode`].
    pub fn code(&self) -> ErrorCode {
        match self {
            IpcError::Io(_) => ErrorCode::TransportClosed,
            IpcError::Json(_) => ErrorCode::ProtocolViolation,
            IpcError::VersionMismatch { .. } => ErrorCode::VersionMismatch,
            IpcError::FrameTooLarge { .. } => ErrorCode::FrameTooLarge,
            IpcError::Truncated { .. } => ErrorCode::TruncatedFrame,
            IpcError::TransportClosed => ErrorCode::TransportClosed,
            IpcError::Protocol(_) => ErrorCode::ProtocolViolation,
            IpcError::Peer { code, .. } => *code,
            IpcError::Timeout => ErrorCode::Timeout,
        }
    }
}

/// Map raw I/O errors: timeouts become [`IpcError::Timeout`].
fn map_io(e: std::io::Error) -> IpcError {
    if e.kind() == std::io::ErrorKind::TimedOut {
        IpcError::Timeout
    } else {
        IpcError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Retry / timeout / circuit-breaker policy
// ---------------------------------------------------------------------------

/// Backoff policy for retryable failures.
///
/// Default (`max_attempts = 3`) encodes ARCH §5: one initial attempt plus
/// two retries for cloud calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first (must be >= 1 to attempt at all).
    pub max_attempts: u32,
    /// First backoff step; doubles per retry.
    pub base_delay_ms: u64,
    /// Backoff ceiling.
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 200,
            max_delay_ms: 5_000,
        }
    }
}

impl RetryPolicy {
    /// Backoff delay before retry number `retry_index` (0-based):
    /// `min(max_delay, base * 2^retry_index)`, saturating and capped.
    pub fn delay_for_attempt(&self, retry_index: u32) -> Duration {
        let shift = retry_index.min(20);
        let grown = self.base_delay_ms.saturating_mul(1u64 << shift);
        Duration::from_millis(grown.min(self.max_delay_ms))
    }
}

/// Timeout budgets (ARCHITECTURE §5). All fields configurable; the cloud
/// P95 budget (10 s per candidate) is the headline knob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeoutConfig {
    /// `hello` handshake round-trip budget.
    pub handshake_ms: u64,
    /// Cloud P95 budget per candidate (foil target; breach feeds eval).
    pub cloud_p95_ms: u64,
    /// Cloud hard timeout per attempt (ARCH §5: 30 s + 2 retries).
    pub cloud_hard_ms: u64,
    /// Offline render attempt budget.
    pub render_ms: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            handshake_ms: 5_000,
            cloud_p95_ms: 10_000,
            cloud_hard_ms: 30_000,
            render_ms: 60_000,
        }
    }
}

impl TimeoutConfig {
    /// Override the cloud P95 budget (keeps every other default).
    pub fn with_cloud_p95_ms(mut self, ms: u64) -> Self {
        self.cloud_p95_ms = ms;
        self
    }

    /// Override the cloud hard timeout per attempt.
    pub fn with_cloud_hard_ms(mut self, ms: u64) -> Self {
        self.cloud_hard_ms = ms;
        self
    }
}

/// Circuit state for the Tier1→Tier2→Tier3→cache→BLOCKED melt chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitState {
    /// Traffic flows.
    Closed,
    /// Failing fast until the cooldown elapses.
    Open,
    /// One probe allowed through after cooldown; its outcome closes or
    /// re-opens the circuit.
    HalfOpen,
}

/// Consecutive-failure circuit breaker with an injectable clock (`now_ms`).
///
/// Time is `u64` milliseconds so tests can drive the clock deterministically;
/// production passes [`crate::ipc::now_ms`].
#[derive(Clone, Debug)]
pub struct CircuitBreaker {
    /// Consecutive failures that trip the circuit.
    threshold: u32,
    /// How long `Open` refuses attempts.
    cooldown_ms: u64,
    /// Current consecutive failure count.
    consecutive_failures: u32,
    /// Current state.
    state: CircuitState,
    /// When the circuit last opened (`now_ms` basis).
    opened_at_ms: u64,
}

impl CircuitBreaker {
    /// Build a breaker tripping after `failure_threshold` consecutive
    /// failures (clamped to >= 1) with a `cooldown_ms` open period.
    pub fn new(failure_threshold: u32, cooldown_ms: u64) -> Self {
        Self {
            threshold: failure_threshold.max(1),
            cooldown_ms,
            consecutive_failures: 0,
            state: CircuitState::Closed,
            opened_at_ms: 0,
        }
    }

    /// Current state.
    pub fn state(&self) -> CircuitState {
        self.state
    }

    /// Current consecutive failure count.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Record a success: reset the count and close the circuit.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.state = CircuitState::Closed;
    }

    /// Record a failure; trip to `Open` once the threshold is reached.
    pub fn record_failure(&mut self, now_ms: u64) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= self.threshold {
            self.state = CircuitState::Open;
            self.opened_at_ms = now_ms;
        }
    }

    /// Whether an attempt may proceed now.
    ///
    /// Transitions `Open` → `HalfOpen` once the cooldown has elapsed.
    pub fn can_attempt(&mut self, now_ms: u64) -> bool {
        match self.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                if now_ms.saturating_sub(self.opened_at_ms) >= self.cooldown_ms {
                    self.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// Wall-clock milliseconds since the Unix epoch (0 on clock error).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Decide whether a failure may be retried and how long to wait.
///
/// `attempts_used` counts attempts already consumed (1-based, including the
/// failed one). Returns `None` for terminal codes, for `attempts_used == 0`,
/// and once `attempts_used >= policy.max_attempts`.
pub fn should_retry(code: ErrorCode, attempts_used: u32, policy: &RetryPolicy) -> Option<Duration> {
    if !code.retryable() || attempts_used == 0 || attempts_used >= policy.max_attempts.max(1) {
        return None;
    }
    Some(policy.delay_for_attempt(attempts_used.saturating_sub(1)))
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// Upload-consent tier authorizing a cloud call (DEC-010/011).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsentTier {
    /// Accepted training-data retention (cloud Tier1).
    #[serde(rename = "tier1")]
    Tier1,
    /// Upload accepted, retention declined (cloud Tier2 ZDR).
    #[serde(rename = "tier2")]
    Tier2,
    /// No upload (local Tier3 only; no cloud call should exist).
    #[serde(rename = "tier3")]
    Tier3,
}

/// Cloud-call audit record (DEC-011, ARCH §9).
///
/// Exactly five fields — time / model / tier / field list / byte count — and
/// nothing else. There is deliberately no slot for keys, PCM, prompts, or
/// paths: callers must keep such material out of audit entirely.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unix epoch milliseconds of the call.
    pub ts_unix_ms: i64,
    /// Model id used (e.g. `"muse-spark-1.3-contributor"`).
    pub model: String,
    /// Consent tier authorizing the call.
    pub tier: ConsentTier,
    /// Whitelisted upload field names (e.g. `["prompt","mir","meta"]`).
    pub fields: Vec<String>,
    /// Request body size in bytes.
    pub byte_count: u64,
}

impl AuditEvent {
    /// Build an audit record with an explicit timestamp.
    pub fn new(
        model: impl Into<String>,
        tier: ConsentTier,
        fields: Vec<String>,
        byte_count: u64,
        ts_unix_ms: i64,
    ) -> Self {
        Self {
            ts_unix_ms,
            model: model.into(),
            tier,
            fields,
            byte_count,
        }
    }

    /// Build an audit record stamped with [`crate::ipc::now_ms`].
    pub fn now(
        model: impl Into<String>,
        tier: ConsentTier,
        fields: Vec<String>,
        byte_count: u64,
    ) -> Self {
        let ts = i64::try_from(now_ms()).unwrap_or(i64::MAX);
        Self::new(model, tier, fields, byte_count, ts)
    }
}

// ---------------------------------------------------------------------------
// Transport (interprocess local socket)
// ---------------------------------------------------------------------------

/// Default bus endpoint id (namespaced on Windows/Linux-abstract,
/// temp-dir socket file elsewhere).
pub const SOCKET_ID: &str = "synthlm-acrd-v1";

/// Filesystem path for endpoint `id` when namespaced names are unsupported
/// (macOS and other BSDs without abstract UDS).
///
/// Kept short (`temp_dir` + `{id}.sock`) to respect `sun_path` limits
/// (D-eng-eco §2). `id` must be filename-safe (letters, digits, `-`, `.`).
pub fn socket_file_path(id: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{id}.sock"))
}

/// Bind a listener on endpoint `id`.
///
/// Uses a namespaced name where supported (Windows named pipe, Linux
/// abstract UDS) and a short temp-dir socket file otherwise. Stale socket
/// files are reclaimed at bind time by `interprocess`.
///
/// Accept loop (caller imports `interprocess::local_socket::prelude::*`):
/// `for conn in listener.incoming() { … }`, or [`crate::ipc::accept_next`] for one shot.
pub fn bind_endpoint(id: &str) -> std::io::Result<Listener> {
    if GenericNamespaced::is_supported() {
        let name = id.to_ns_name::<GenericNamespaced>()?;
        ListenerOptions::new().name(name).create_sync()
    } else {
        let path = socket_file_path(id);
        let name = path.to_fs_name::<GenericFilePath>()?;
        ListenerOptions::new().name(name).create_sync()
    }
}

/// Bind the default bus endpoint ([`crate::ipc::SOCKET_ID`]).
pub fn bind_listener() -> std::io::Result<Listener> {
    bind_endpoint(SOCKET_ID)
}

/// Accept one connection (convenience; equivalent to `Listener::accept`).
///
/// The `Listener` trait must be in scope; this helper keeps call sites free
/// of the import.
pub fn accept_next(listener: &Listener) -> std::io::Result<Stream> {
    use interprocess::local_socket::traits::Listener as _;
    listener.accept()
}

/// Connect to endpoint `id`. Fails immediately if no listener exists yet;
/// use [`crate::ipc::connect_to_with_retry`] to ride out sidecar startup.
pub fn connect_to(id: &str) -> std::io::Result<Stream> {
    use interprocess::local_socket::traits::Stream as _;
    if GenericNamespaced::is_supported() {
        let name = id.to_ns_name::<GenericNamespaced>()?;
        Stream::connect(name)
    } else {
        let path = socket_file_path(id);
        let name = path.to_fs_name::<GenericFilePath>()?;
        Stream::connect(name)
    }
}

/// Connect to the default bus endpoint ([`crate::ipc::SOCKET_ID`]).
pub fn connect() -> std::io::Result<Stream> {
    connect_to(SOCKET_ID)
}

/// Connect to endpoint `id`, retrying transport failures with backoff.
///
/// Only transport-level connect errors are retried (they map to
/// [`ErrorCode::TransportClosed`], always retryable); terminal errors cannot
/// arise before the handshake. Blocks the calling thread — never call from
/// an audio thread.
pub fn connect_to_with_retry(id: &str, policy: &RetryPolicy) -> Result<Stream, IpcError> {
    let mut attempts_used = 0u32;
    loop {
        match connect_to(id) {
            Ok(stream) => return Ok(stream),
            Err(io_err) => {
                attempts_used = attempts_used.saturating_add(1);
                if attempts_used >= policy.max_attempts.max(1) {
                    return Err(IpcError::Io(io_err));
                }
                std::thread::sleep(policy.delay_for_attempt(attempts_used.saturating_sub(1)));
            }
        }
    }
}

/// Connect to the default bus endpoint with backoff (see
/// [`crate::ipc::connect_to_with_retry`]).
pub fn connect_with_retry(policy: &RetryPolicy) -> Result<Stream, IpcError> {
    connect_to_with_retry(SOCKET_ID, policy)
}

/// Apply send/receive timeouts to a connected stream (`None` = block).
///
/// Timeouts surface as [`IpcError::Timeout`] via [`crate::ipc::read_frame`]/writes.
/// Recommended: handshake budget from [`TimeoutConfig::handshake_ms`].
pub fn set_stream_timeouts(
    stream: &Stream,
    send_ms: Option<u64>,
    recv_ms: Option<u64>,
) -> std::io::Result<()> {
    use interprocess::local_socket::traits::Stream as _;
    stream.set_send_timeout(send_ms.map(Duration::from_millis))?;
    stream.set_recv_timeout(recv_ms.map(Duration::from_millis))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn sample_body() -> serde_json::Value {
        serde_json::json!({"snapshot_id": "s1", "params": [1, 2, 3]})
    }

    #[test]
    fn frame_roundtrip_all_message_types() {
        for kind in MessageType::all() {
            let body = sample_body();
            let bytes = encode_frame(*kind, &body).expect("encode");
            // Length prefix is u32 LE of the payload length.
            let mut prefix = [0u8; 4];
            prefix.copy_from_slice(&bytes[..4]);
            assert_eq!(u32::from_le_bytes(prefix) as usize, bytes.len() - 4);
            let back = decode_frame_bytes(&bytes).expect("decode");
            assert_eq!(back.kind, *kind);
            assert_eq!(back.body, body);
            assert_eq!(back.ver, current_version());
        }
    }

    #[test]
    fn stream_frame_roundtrip_via_cursor() {
        let body = sample_body();
        let mut buf = Vec::new();
        write_frame(&mut buf, MessageType::PlanRequest, &body).expect("write");
        let mut cursor = std::io::Cursor::new(buf);
        let back = read_frame(&mut cursor).expect("read");
        assert_eq!(back.kind, MessageType::PlanRequest);
        assert_eq!(back.body, body);
    }

    #[test]
    fn message_type_wire_strings_match() {
        for kind in MessageType::all() {
            let wire = serde_json::to_value(kind).expect("serialize kind");
            assert_eq!(wire, serde_json::Value::String(kind.as_str().to_owned()));
            let back: MessageType = serde_json::from_value(wire).expect("deserialize kind");
            assert_eq!(&back, kind);
        }
    }

    #[test]
    fn version_mismatch_rejected() {
        // Same envelope, next major: must be refused before touching body.
        let frame = WireFrame {
            ver: Version {
                major: PROTOCOL_MAJOR + 1,
                minor: 0,
            },
            kind: MessageType::Hello,
            body: sample_body(),
        };
        let mut payload = serde_json::to_vec(&frame).expect("payload");
        let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
        bytes.append(&mut payload);
        let err = decode_frame_bytes(&bytes).expect_err("major bump must fail");
        assert!(matches!(err, IpcError::VersionMismatch { .. }), "{err:?}");

        // Minor drift is fine.
        let ok_frame = WireFrame {
            ver: Version {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR + 9,
            },
            kind: MessageType::Hello,
            body: sample_body(),
        };
        let back =
            decode_frame_bytes(&encode_frame(MessageType::Hello, &ok_frame.body).expect("enc"))
                .expect("minor drift ok");
        assert_eq!(back.ver.major, PROTOCOL_MAJOR);
    }

    #[test]
    fn truncated_frames_rejected() {
        let bytes = encode_frame(MessageType::ScoreReport, &sample_body()).expect("encode");

        // Slice path: short prefix.
        let err = decode_frame_bytes(&bytes[..2]).expect_err("short prefix");
        assert!(matches!(err, IpcError::Truncated { .. }), "{err:?}");

        // Slice path: declared payload longer than available.
        let err = decode_frame_bytes(&bytes[..bytes.len() - 4]).expect_err("cut payload");
        assert!(matches!(err, IpcError::Truncated { .. }), "{err:?}");

        // Stream path: clean EOF at a frame boundary.
        let mut empty = std::io::Cursor::new(Vec::new());
        let err = read_frame(&mut empty).expect_err("empty stream");
        assert!(matches!(err, IpcError::TransportClosed), "{err:?}");

        // Stream path: EOF mid-payload reports progress.
        let cut = bytes[..bytes.len() - 4].to_vec();
        let mut cursor = std::io::Cursor::new(cut);
        let err = read_frame(&mut cursor).expect_err("mid-frame EOF");
        match err {
            IpcError::Truncated { expected, got } => {
                assert!(got < expected);
                assert!(expected > 0);
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn oversize_frames_rejected_before_allocation() {
        // Hostile prefix: must fail without reading the payload.
        let mut hostile = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes().to_vec();
        hostile.extend_from_slice(b"{}");
        let err = decode_frame_bytes(&hostile).expect_err("oversize prefix");
        assert!(matches!(err, IpcError::FrameTooLarge { .. }), "{err:?}");

        // Stream path: same, with no payload following at all.
        let mut cursor = std::io::Cursor::new((MAX_FRAME_BYTES as u32 + 1).to_le_bytes().to_vec());
        let err = read_frame(&mut cursor).expect_err("oversize stream prefix");
        assert!(matches!(err, IpcError::FrameTooLarge { .. }), "{err:?}");

        // Encode path: body over the cap is refused.
        let big = serde_json::Value::String("x".repeat(MAX_FRAME_BYTES + 1));
        let err = encode_frame(MessageType::SnapshotSubmit, &big).expect_err("encode oversize");
        assert!(matches!(err, IpcError::FrameTooLarge { .. }), "{err:?}");
    }

    #[test]
    fn error_taxonomy_full_coverage() {
        let retryable: HashSet<ErrorCode> = [
            ErrorCode::Timeout,
            ErrorCode::TransportClosed,
            ErrorCode::RenderUnstable,
            ErrorCode::CloudUnavailable,
        ]
        .into_iter()
        .collect();
        assert_eq!(ErrorCode::all().len(), 13);
        for code in ErrorCode::all() {
            assert_eq!(
                code.retryable(),
                retryable.contains(code),
                "{code:?} misclassified"
            );
            assert_eq!(code.blocked(), !code.retryable());
            // Constructor derives the flag; it cannot be set inconsistently.
            assert_eq!(ErrorBody::new(*code, "d").retryable, code.retryable());
        }
        // Terminal classes from the task statement: never retried, ever.
        for terminal in [
            ErrorCode::ConsentRequired,
            ErrorCode::AuthDenied,
            ErrorCode::WhitelistViolation,
            ErrorCode::AudioCapabilityMissing,
        ] {
            assert!(terminal.blocked(), "{terminal:?} must be blocked");
            assert!(should_retry(terminal, 1, &RetryPolicy::default()).is_none());
        }
    }

    #[test]
    fn error_wire_flag_not_trusted() {
        // A peer claiming `retryable: true` for consent_required must not
        // change our verdict after parsing.
        let raw = serde_json::json!({
            "ver": {"major": PROTOCOL_MAJOR, "minor": PROTOCOL_MINOR},
            "type": "error",
            "body": {
                "code": "consent_required",
                "retryable": true,
                "detail": "tampered"
            }
        });
        let bytes = {
            let payload = serde_json::to_vec(&raw).expect("payload");
            let mut out = (payload.len() as u32).to_le_bytes().to_vec();
            out.extend_from_slice(&payload);
            out
        };
        let frame = decode_frame_bytes(&bytes).expect("decode");
        let body = parse_error(&frame).expect("parse error");
        assert_eq!(body.code, ErrorCode::ConsentRequired);
        assert!(!body.retryable, "wire flag must be recomputed");
    }

    #[test]
    fn audit_event_serialization_contains_no_secrets() {
        let event = AuditEvent::new(
            "muse-spark-1.3-contributor",
            ConsentTier::Tier1,
            vec!["prompt".to_owned(), "mir".to_owned(), "meta".to_owned()],
            4096,
            1_789_000_000_000,
        );
        let value = serde_json::to_value(&event).expect("serialize audit");
        let obj = value.as_object().expect("audit is an object");
        let keys: HashSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            HashSet::from(["ts_unix_ms", "model", "tier", "fields", "byte_count"]),
            "audit schema must stay at exactly five fields"
        );
        let text = serde_json::to_string(&event).expect("render audit");
        assert!(!text.contains("key"), "audit must not leak keys: {text}");
        assert!(!text.contains("pcm"), "audit must not leak audio: {text}");
    }

    #[test]
    fn retry_policy_math() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 200,
            max_delay_ms: 5_000,
        };
        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(200));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(400));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(800));
        assert_eq!(policy.delay_for_attempt(100), Duration::from_millis(5_000));

        // attempts_used is 1-based including the failed attempt.
        assert_eq!(
            should_retry(ErrorCode::Timeout, 1, &policy),
            Some(Duration::from_millis(200))
        );
        assert_eq!(
            should_retry(ErrorCode::Timeout, 2, &policy),
            Some(Duration::from_millis(400))
        );
        assert_eq!(should_retry(ErrorCode::Timeout, 3, &policy), None);
        assert_eq!(should_retry(ErrorCode::Timeout, 0, &policy), None);
        assert_eq!(should_retry(ErrorCode::ConsentRequired, 1, &policy), None);
    }

    #[test]
    fn circuit_breaker_trips_and_recovers() {
        let mut breaker = CircuitBreaker::new(3, 1_000);
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.can_attempt(0));
        breaker.record_failure(0);
        breaker.record_failure(10);
        assert!(breaker.can_attempt(20));
        breaker.record_failure(20);
        assert_eq!(breaker.state(), CircuitState::Open);
        assert!(!breaker.can_attempt(500));
        assert!(breaker.can_attempt(1_020));
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert_eq!(breaker.consecutive_failures(), 0);
    }

    #[test]
    fn handshake_roundtrip_over_tcp_loopback() {
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            server_handshake(&mut conn, EndpointRole::Acrd).expect("server hello")
        });
        let mut client = TcpStream::connect(addr).expect("connect");
        let peer = client_handshake(&mut client, EndpointRole::Ui).expect("client hello");
        assert_eq!(peer.role, EndpointRole::Acrd);
        assert_eq!(peer.version, current_version());
        let client_hello = server.join().expect("server thread");
        assert_eq!(client_hello.role, EndpointRole::Ui);
    }

    #[test]
    fn handshake_rejects_wrong_major() {
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            server_handshake(&mut conn, EndpointRole::Acrd)
        });
        let mut client = TcpStream::connect(addr).expect("connect");
        // Client speaks the next major: server must answer `error`, and the
        // client surfaces it as a version-mismatch peer error.
        let bad = WireFrame {
            ver: Version {
                major: PROTOCOL_MAJOR + 1,
                minor: 0,
            },
            kind: MessageType::Hello,
            body: serde_json::to_value(Hello {
                version: Version {
                    major: PROTOCOL_MAJOR + 1,
                    minor: 0,
                },
                role: EndpointRole::Ui,
            })
            .expect("hello body"),
        };
        let payload = serde_json::to_vec(&bad).expect("payload");
        let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&payload);
        use std::io::Write as _;
        client.write_all(&bytes).expect("send bad hello");
        let reply = read_frame(&mut client).expect("server replies");
        assert_eq!(reply.kind, MessageType::Error);
        let body = parse_error(&reply).expect("parse");
        assert_eq!(body.code, ErrorCode::VersionMismatch);
        assert!(!body.retryable);

        let err = server
            .join()
            .expect("server thread")
            .expect_err("server rejects");
        assert!(matches!(err, IpcError::VersionMismatch { .. }), "{err:?}");
    }

    fn unique_test_id(tag: &str) -> String {
        format!("synthlm-t107-{tag}-{}", std::process::id())
    }

    #[test]
    fn local_socket_handshake_and_echo() {
        let id = unique_test_id("echo");
        let listener = bind_endpoint(&id).expect("bind local socket");
        let server = std::thread::spawn(move || {
            let mut conn = accept_next(&listener).expect("accept");
            let peer = server_handshake(&mut conn, EndpointRole::Acrd).expect("server hello");
            assert_eq!(peer.role, EndpointRole::Bridge);
            let frame = recv_frame(&mut conn).expect("recv");
            assert_eq!(frame.kind, MessageType::ScoreReport);
            send_frame(&mut conn, MessageType::ScoreReport, &frame.body).expect("echo");
        });

        let policy = RetryPolicy {
            max_attempts: 50,
            base_delay_ms: 10,
            max_delay_ms: 50,
        };
        let mut client = connect_to_with_retry(&id, &policy).expect("connect");
        let peer = client_handshake(&mut client, EndpointRole::Bridge).expect("client hello");
        assert_eq!(peer.role, EndpointRole::Acrd);

        let body = serde_json::json!({"spec_l1": 0.5, "delta_lufs": -0.2});
        send_frame(&mut client, MessageType::ScoreReport, &body).expect("send");
        let echo = recv_frame(&mut client).expect("recv echo");
        assert_eq!(echo.kind, MessageType::ScoreReport);
        assert_eq!(echo.body, body);
        server.join().expect("server thread");
    }

    #[test]
    fn connect_retry_gives_up() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 1,
            max_delay_ms: 1,
        };
        let id = unique_test_id("nobody-listens-here");
        let err = connect_to_with_retry(&id, &policy).expect_err("must fail");
        assert!(matches!(err, IpcError::Io(_)), "{err:?}");
    }
}
