//! Shared-memory bulk channel: threshold policy, PCM block handoff, and the
//! control-plane announce envelope.
//!
//! Backend choice (compared 2026-10-06; decision recorded here, licenses
//! pending registration in `docs/LICENSES.md` by the owning task):
//!
//! - `shared_memory 0.12.4` (MIT OR Apache-2.0;
//!   <https://github.com/elast0ny/shared_memory-rs>): **chosen**. True OS
//!   segments (`shm_open`/`mmap` on Unix incl. macOS, file mapping on
//!   Windows), named by `os_id` — which maps exactly onto this module's
//!   "shm name" descriptor field — with owner-only (`0o600`-equivalent)
//!   creation flags and owner auto-unlink on drop, so no segment leaks when
//!   the sender goes away. Already present in the offline cargo cache, so no
//!   new network fetch was needed.
//! - `memmap2 0.9.11` (MIT OR Apache-2.0): rejected. Its file-backed
//!   `map`/`map_mut` are `unsafe fn` in 0.9.11 too, so it saves no `unsafe`
//!   over `shared_memory`, while leaving backing-file lifecycle (unique
//!   naming, unlinking, size discipline) to hand-rolled code.
//!
//! Protocol (DEC-023 control plane + bulk side channel, D-eng-eco §2):
//!
//! - Payloads at or above [`crate::shm::SHM_THRESHOLD_BYTES`] (16 MiB, equal to
//!   [`crate::ipc::MAX_FRAME_BYTES`]) travel via shared memory; anything that
//!   would not fit a control-plane frame *must* use this path.
//! - The sender publishes only a small descriptor — shm name + length +
//!   checksum — inside a normal control-plane frame ([`crate::shm::announce_body`] /
//!   [`crate::shm::parse_announce`], carried e.g. as the body of `render.result`, which by
//!   definition holds audio *references*, never PCM). The frame itself is the
//!   event notification: the receiver opens the segment only after the
//!   announce arrives.
//! - Discipline (safety basis): after [`crate::shm::ShmSender::create`] returns, the
//!   mapping is frozen — the sender exposes no mutator — so receivers may
//!   copy it out concurrently. The sender keeps its [`crate::shm::ShmSender`] alive until
//!   receivers are done; dropping it unlinks the segment (owner semantics).
//!
//! `unsafe` sites: two (`ShmSender::create`, [`crate::shm::read_block`]), each with a
//! `SAFETY` comment. This is the minimum the `shared_memory 0.12.4` API
//! allows (access is via raw-pointer slices); no safe-API alternative exists
//! in the pinned version.
//!
//! Blocking contract: like the rest of `ipc`, segment create/open blocks the
//! calling thread. Never call from an audio thread (AGENTS.md red line 2).

use serde::{Deserialize, Serialize};
use shared_memory::{Shmem, ShmemConf};
use thiserror::Error;

/// Payloads at or above this size travel via shared memory.
///
/// Equals [`crate::ipc::MAX_FRAME_BYTES`] (16 MiB): anything that would not
/// fit a control-plane frame must use the shm path. See [`should_use_shm`].
pub const SHM_THRESHOLD_BYTES: usize = crate::ipc::MAX_FRAME_BYTES;

/// Route a payload: `true` means shared memory, `false` means control-plane
/// frame. The boundary is inclusive: exactly 16 MiB already goes via shm.
pub fn should_use_shm(payload_len_bytes: usize) -> bool {
    payload_len_bytes >= SHM_THRESHOLD_BYTES
}

/// FNV-1a 64-bit checksum over raw bytes.
///
/// Chosen over `std`'s `DefaultHasher` deliberately: `SipHash` keys are
/// randomized per process, so a sender and a receiver in different processes
/// would disagree. FNV-1a is deterministic across processes and needs no
/// dependency; it is an integrity check against truncation/corruption, not a
/// cryptographic digest.
pub fn checksum_fnv1a64(data: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Small descriptor published over the control plane: shm name + length +
/// checksum. Contains no audio bytes (AGENTS.md §8: raw audio stays out of
/// frames, logs, and audit).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmDescriptor {
    /// OS segment name (`shared_memory` `os_id`).
    pub shm_name: String,
    /// Payload length in bytes (exact mapping size).
    pub len_bytes: u64,
    /// [`checksum_fnv1a64`] over the payload.
    pub checksum: u64,
}

/// Shared-memory channel failures.
#[derive(Debug, Error)]
pub enum ShmError {
    /// Zero-length payloads cannot back a mapping (backend rejects size 0).
    #[error("refusing empty payload: shared-memory mappings require size > 0")]
    EmptyPayload,
    /// Underlying `shared_memory` backend failure.
    #[error("shared-memory backend: {0}")]
    Backend(#[from] shared_memory::ShmemError),
    /// Descriptor length disagrees with the actual mapping size. The mapping
    /// is never touched in this case: opens use the OS-reported size, not the
    /// descriptor value, and backends may round the mapping *up* (e.g. to a
    /// page), so the gate is `actual < expected`, never strict equality.
    #[error("length mismatch: descriptor says {expected} bytes but mapping holds {actual}")]
    LengthMismatch {
        /// Byte count claimed by the descriptor.
        expected: u64,
        /// Actual mapping size in bytes.
        actual: usize,
    },
    /// Payload bytes fail the descriptor checksum (truncation/corruption).
    #[error("checksum mismatch: descriptor {expected:#x} but mapping reads {actual:#x}")]
    ChecksumMismatch {
        /// Checksum claimed by the descriptor.
        expected: u64,
        /// Checksum over the bytes actually read.
        actual: u64,
    },
    /// PCM decode needs whole `f32` samples; the payload is not a multiple
    /// of 4 bytes.
    #[error("pcm length {bytes} is not a multiple of 4 (f32)")]
    PcmLengthInvalid {
        /// Payload length in bytes.
        bytes: usize,
    },
    /// Announce body is not a `{ shm: <descriptor> }` envelope.
    #[error("announce body schema: {0}")]
    Announce(#[from] serde_json::Error),
}

/// Owning sender handle for one frozen payload block.
///
/// Holds the mapping open (and owns unlink-on-drop) until receivers are done;
/// drop only after the announce has been consumed.
pub struct ShmSender {
    shmem: Shmem,
    descriptor: ShmDescriptor,
}

impl ShmSender {
    /// Copy `data` into a fresh segment and freeze it.
    ///
    /// # Errors
    ///
    /// [`ShmError::EmptyPayload`] for empty input, [`ShmError::Backend`] if
    /// the OS refuses the segment.
    pub fn create(data: &[u8]) -> Result<Self, ShmError> {
        if data.is_empty() {
            return Err(ShmError::EmptyPayload);
        }
        let mut shmem = ShmemConf::new().size(data.len()).create()?;
        // SAFETY: the mapping was just created by this process with a fresh
        // random `os_id` (no other mapper can exist yet — receivers only open
        // after we publish the descriptor), `size == data.len() > 0`, so the
        // slice is exactly `data.len()` valid, writable, exclusively owned
        // bytes; access is single-threaded within this call.
        let slot: &mut [u8] = unsafe { shmem.as_slice_mut() };
        slot.copy_from_slice(data);
        let descriptor = ShmDescriptor {
            shm_name: shmem.get_os_id().to_owned(),
            len_bytes: data.len() as u64,
            checksum: checksum_fnv1a64(data),
        };
        Ok(Self { shmem, descriptor })
    }

    /// The descriptor to publish over the control plane.
    pub fn descriptor(&self) -> &ShmDescriptor {
        &self.descriptor
    }

    /// Whether this handle still owns the segment (owns unlink-on-drop).
    ///
    /// Always `true` for handles from [`crate::shm::ShmSender::create`]; exposed so
    /// callers can assert ownership before handing the descriptor out.
    /// Reading this also keeps the field use honest: the mapping itself is
    /// held open purely for its lifetime.
    pub fn is_owner(&self) -> bool {
        self.shmem.is_owner()
    }

    /// Payload length in bytes.
    pub fn len_bytes(&self) -> u64 {
        self.descriptor.len_bytes
    }
}

/// Copy one frozen little-endian `f32` PCM block into a fresh segment.
///
/// Encoding is explicit LE bytes (portable across endians); see
/// [`read_pcm_f32`].
///
/// # Errors
///
/// Same as [`crate::shm::ShmSender::create`].
pub fn write_pcm_f32(samples: &[f32]) -> Result<ShmSender, ShmError> {
    if samples.is_empty() {
        return Err(ShmError::EmptyPayload);
    }
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    ShmSender::create(&bytes)
}

/// Open the announced segment, verify length + checksum, and return a copy
/// of the payload.
///
/// The mapping is opened with the OS-reported size and rejected when it holds
/// *fewer* bytes than the descriptor claims, *before* any byte is touched, so
/// a lying descriptor can cause an error but never an out-of-bounds read.
/// Backends may round the mapping up (e.g. Windows page granularity), so only
/// the first `len_bytes` are read and checksummed.
///
/// # Errors
///
/// [`ShmError::LengthMismatch`], [`ShmError::ChecksumMismatch`], or
/// [`ShmError::Backend`] if the segment is gone (sender dropped first).
pub fn read_block(descriptor: &ShmDescriptor) -> Result<Vec<u8>, ShmError> {
    let expected_len =
        usize::try_from(descriptor.len_bytes).map_err(|_| ShmError::LengthMismatch {
            expected: descriptor.len_bytes,
            actual: usize::MAX,
        })?;
    if expected_len == 0 {
        return Err(ShmError::LengthMismatch {
            expected: 0,
            actual: 0,
        });
    }
    let shmem = ShmemConf::new()
        .size(expected_len)
        .os_id(descriptor.shm_name.as_str())
        .open()?;
    if shmem.len() < expected_len {
        return Err(ShmError::LengthMismatch {
            expected: descriptor.len_bytes,
            actual: shmem.len(),
        });
    }
    // SAFETY: the sender-side discipline freezes the mapping after publish
    // (`ShmSender` exposes no mutator), so no writer exists while we read;
    // the copy is bounded by `expected_len`, which was just verified to not
    // exceed the mapping size, and no borrow is retained. The opened
    // (non-owner) handle unmaps without unlinking on drop.
    let bytes: Vec<u8> = unsafe { shmem.as_slice() }[..expected_len].to_vec();
    let actual = checksum_fnv1a64(&bytes);
    if actual != descriptor.checksum {
        return Err(ShmError::ChecksumMismatch {
            expected: descriptor.checksum,
            actual,
        });
    }
    Ok(bytes)
}

/// Decode one announced little-endian `f32` PCM block.
///
/// # Errors
///
/// Same as [`crate::shm::read_block`], plus [`ShmError::PcmLengthInvalid`] when the
/// verified payload is not a whole number of samples.
pub fn read_pcm_f32(descriptor: &ShmDescriptor) -> Result<Vec<f32>, ShmError> {
    let bytes = read_block(descriptor)?;
    let (chunks, remainder) = bytes.as_chunks::<4>();
    if !remainder.is_empty() {
        return Err(ShmError::PcmLengthInvalid { bytes: bytes.len() });
    }
    Ok(chunks
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}

/// Build the control-plane announce body for `descriptor`.
///
/// The envelope is `{ "shm": <descriptor> }`, sent as the body of a normal
/// frame (e.g. `render.result`, which carries audio references, never PCM).
/// The frame's arrival *is* the event notification.
pub fn announce_body(descriptor: &ShmDescriptor) -> serde_json::Value {
    serde_json::json!({ "shm": descriptor })
}

/// Parse an [`crate::shm::announce_body`] envelope back into its descriptor.
///
/// # Errors
///
/// [`ShmError::Announce`] when the body is not a `{ shm: … }` envelope.
pub fn parse_announce(body: &serde_json::Value) -> Result<ShmDescriptor, ShmError> {
    #[derive(Deserialize)]
    struct Envelope {
        shm: ShmDescriptor,
    }

    let envelope: Envelope = serde_json::from_value(body.clone())?;
    Ok(envelope.shm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp_f32(len: usize) -> Vec<f32> {
        (0..len).map(|i| i as f32 * 0.25 - 1.0).collect()
    }

    #[test]
    fn threshold_policy_routes_at_16mib() {
        assert!(!should_use_shm(0));
        assert!(!should_use_shm(1024));
        assert!(!should_use_shm(SHM_THRESHOLD_BYTES - 1));
        assert_eq!(SHM_THRESHOLD_BYTES, 16 * 1024 * 1024);
        assert_eq!(SHM_THRESHOLD_BYTES, crate::ipc::MAX_FRAME_BYTES);
        assert!(should_use_shm(SHM_THRESHOLD_BYTES));
        assert!(should_use_shm(SHM_THRESHOLD_BYTES + 1));
    }

    #[test]
    fn checksum_is_deterministic_and_sensitive() {
        let data = b"synthlm pcm probe";
        assert_eq!(checksum_fnv1a64(data), checksum_fnv1a64(data));
        assert_eq!(checksum_fnv1a64(&[]), 0xcbf2_9ce4_8422_2325);
        let mut flipped = data.to_vec();
        flipped[0] ^= 1;
        assert_ne!(checksum_fnv1a64(data), checksum_fnv1a64(&flipped));
    }

    #[test]
    fn descriptor_json_roundtrip() {
        let sender = ShmSender::create(b"abc").expect("create");
        let value = serde_json::to_value(sender.descriptor()).expect("to json");
        let back: ShmDescriptor = serde_json::from_value(value).expect("from json");
        assert_eq!(&back, sender.descriptor());
    }

    #[test]
    fn announce_envelope_roundtrip() {
        let sender = ShmSender::create(b"announce me").expect("create");
        let body = announce_body(sender.descriptor());
        assert!(body.get("shm").is_some(), "envelope must hold `shm`");
        assert_eq!(
            parse_announce(&body).expect("parse announce"),
            *sender.descriptor()
        );
    }

    #[test]
    fn announce_rejects_foreign_body() {
        let err = parse_announce(&serde_json::json!({"nope": 1})).expect_err("must fail");
        assert!(matches!(err, ShmError::Announce(_)), "{err:?}");
    }

    #[test]
    fn empty_payload_refused() {
        assert!(matches!(
            ShmSender::create(&[]),
            Err(ShmError::EmptyPayload)
        ));
        assert!(matches!(write_pcm_f32(&[]), Err(ShmError::EmptyPayload)));
    }

    #[test]
    fn small_pcm_block_roundtrips_exactly() {
        let samples = ramp_f32(4096);
        let sender = write_pcm_f32(&samples).expect("write pcm");
        assert_eq!(sender.len_bytes(), 4096 * 4);
        let back = read_pcm_f32(sender.descriptor()).expect("read pcm");
        assert_eq!(back, samples);
    }

    #[test]
    fn raw_bytes_roundtrip_with_odd_length() {
        let data: Vec<u8> = (0..257u32).map(|i| (i % 251) as u8).collect();
        let sender = ShmSender::create(&data).expect("create");
        let back = read_block(sender.descriptor()).expect("read back");
        assert_eq!(back, data);
    }

    #[test]
    fn pcm_decode_rejects_non_sample_multiple() {
        let sender = ShmSender::create(&[1, 2, 3, 4, 5]).expect("create");
        let err = read_pcm_f32(sender.descriptor()).expect_err("must fail");
        assert!(
            matches!(err, ShmError::PcmLengthInvalid { bytes: 5 }),
            "{err:?}"
        );
    }

    #[test]
    fn tampered_checksum_rejected() {
        let sender = ShmSender::create(b"tamper target").expect("create");
        let mut descriptor = sender.descriptor().clone();
        descriptor.checksum ^= 0xffff;
        let err = read_block(&descriptor).expect_err("must fail");
        assert!(matches!(err, ShmError::ChecksumMismatch { .. }), "{err:?}");
    }

    #[test]
    fn tampered_length_rejected() {
        let sender = ShmSender::create(b"length target").expect("create");
        let mut descriptor = sender.descriptor().clone();
        descriptor.len_bytes += 1;
        // Either the length gate or the backend refuses; both reject, never read OOB.
        assert!(read_block(&descriptor).is_err());
    }

    #[test]
    fn missing_segment_is_backend_error() {
        let descriptor = ShmDescriptor {
            shm_name: format!("synthlm-t115-gone-{}", std::process::id()),
            len_bytes: 64,
            checksum: 0,
        };
        let err = read_block(&descriptor).expect_err("must fail");
        assert!(matches!(err, ShmError::Backend(_)), "{err:?}");
    }

    /// Full closed loop: sender writes a PCM block, publishes the descriptor
    /// in a control-plane frame over a local socket (the event notification),
    /// and the receiver opens the segment and reads back bit-identical audio.
    #[test]
    fn control_plane_announce_loop_reads_back_identical_pcm() {
        use crate::ipc::{
            EndpointRole, MessageType, RetryPolicy, accept_next, client_handshake,
            connect_to_with_retry, recv_frame, send_frame, server_handshake,
        };

        let expected = ramp_f32(65_536); // 256 KiB of PCM
        let sender = write_pcm_f32(&expected).expect("write pcm");
        let announce = announce_body(sender.descriptor());

        let endpoint = format!("synthlm-t115-shm-{}", std::process::id());
        let listener = crate::ipc::bind_endpoint(&endpoint).expect("bind");
        let server = std::thread::spawn(move || {
            let mut conn = accept_next(&listener).expect("accept");
            server_handshake(&mut conn, EndpointRole::Acrd).expect("server handshake");
            let frame = recv_frame(&mut conn).expect("recv announce");
            assert_eq!(frame.kind, MessageType::RenderResult);
            let descriptor = parse_announce(&frame.body).expect("parse announce");
            let got = read_pcm_f32(&descriptor).expect("read pcm");
            send_frame(
                &mut conn,
                MessageType::ScoreReport,
                &serde_json::json!({"ok": true}),
            )
            .expect("ack");
            got
        });

        let policy = RetryPolicy {
            max_attempts: 50,
            base_delay_ms: 10,
            max_delay_ms: 50,
        };
        let mut client = connect_to_with_retry(&endpoint, &policy).expect("connect");
        client_handshake(&mut client, EndpointRole::Bridge).expect("client handshake");
        send_frame(&mut client, MessageType::RenderResult, &announce).expect("send announce");
        let ack = recv_frame(&mut client).expect("recv ack");
        assert_eq!(ack.kind, MessageType::ScoreReport);

        let got = server.join().expect("server thread");
        assert_eq!(got, expected, "receiver must read back bit-identical PCM");
    }

    /// A threshold-size (16 MiB) block round-trips through the shm path.
    #[test]
    fn threshold_size_block_roundtrips() {
        let samples = ramp_f32(SHM_THRESHOLD_BYTES / 4);
        assert!(should_use_shm(samples.len() * 4));
        let sender = write_pcm_f32(&samples).expect("write 16MiB");
        let back = read_pcm_f32(sender.descriptor()).expect("read 16MiB");
        assert_eq!(back, samples);
    }
}
