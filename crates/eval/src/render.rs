//! 42230 true-render ingestion into the evaluation loop (TSK-504).
//!
//! Rust-side intake only: render execution stays in REAPER via Lua
//! (`Main_OnCommand(42230)`, DEC-005); this module reads the rendered wav
//! file back, turns it into mono f32 PCM, runs it through
//! [`crate::mir::analyze`], and gates the result before it may enter
//! [`crate::score::compare`]. Nothing here touches REAPER, the audio
//! thread, or the network (L8; `AGENTS.md` §3.2–§3.3): file I/O happens on
//! the caller thread, which must be off the DAW threads.
//!
//! Acceptance contract (DEC-005 / DEC-012 / DEC-016, A03 §8):
//!
//! - Rates: only 48 kHz (native) and 44.1 kHz are accepted. A 44.1 kHz
//!   render is ingested but flagged
//!   [`crate::render::RateNote::ResampleTodo`] — it is analysed with
//!   44.1 kHz-native [`crate::mir::MirParams`] (audibly incomparable to
//!   48 kHz products: [`crate::score::compare`] refuses the pair via
//!   [`crate::EvalError::ParamMismatch`]) and must be resampled to 48 kHz
//!   REAPER-side before it can join a 48 kHz comparison. Anything else is
//!   refused with [`crate::EvalError::UnsupportedRate`]. Never silently
//!   resampled here.
//! - Channels: the policy is explicit. Mono passes through
//!   ([`crate::render::ChannelHandling::MonoPassthrough`]); stereo is
//!   mean-downmixed ([`crate::render::ChannelHandling::StereoAveraged`]);
//!   anything beyond stereo is refused with
//!   [`crate::EvalError::UnsupportedChannels`] (pin the render to mono or
//!   stereo REAPER-side first: `RENDER_CHANNELS`, or 40901-mono /
//!   41223-stereo freeze). The handling is recorded on every
//!   [`crate::render::RenderProduct`].
//! - Lossy inputs: AAC-style decodes carry encoder priming plus tail
//!   padding, so they must be trimmed with the TSK-208 rule first —
//!   [`crate::render::CodecTrim::aac`] over
//!   [`crate::mir::strip_codec_padding`] — and the trim parameters travel
//!   on the product ([`crate::render::RenderProduct::trim`]).
//! - Determinism gate: three renders of the same bounds must be
//!   bit-identical under the A03 §8 method (mute non-target tracks, fixed
//!   bounds, fixed block). [`crate::render::NullTestGate::check_three`]
//!   enforces FNV-1a identity plus pairwise spectral-distance thresholds;
//!   over-threshold variance is [`crate::EvalError::RenderUnstable`].
//! - Fallback: when a render is unstable, the retry order is fixed —
//!   full-speed → small block → 1x offline → online (A03 §4). Speed/block
//!   switching executes REAPER-side (stock ReaScript has no setter for
//!   `RENDER_1X`, which lives only in the project chunk — A03 §8); the
//!   Rust side only decides and records via
//!   [`crate::render::FallbackPolicy`] /
//!   [`crate::render::FallbackRecord`].
//!
//! Every rejection in this module is `BLOCKED` (see
//! [`crate::EvalError::blocked`]): fix the render REAPER-side and ingest
//! again; nothing is retried silently.

use std::path::{Path, PathBuf};

use crate::EvalError;
use crate::mir::{AAC_PRIMING_SAMPLES, MirFeatures, MirParams, analyze, strip_codec_padding};
use crate::score::compare;

/// Native render rate in Hz (DEC-012 freeze; analysed with
/// [`crate::mir::MirParams::v1`]).
pub const RATE_48K: u32 = 48_000;

/// Legacy render rate in Hz: accepted, but flagged
/// [`crate::render::RateNote::ResampleTodo`] and never silently resampled.
pub const RATE_44K1: u32 = 44_100;

/// Null-test gate: maximum allowed pairwise `spec_l1` distance between
/// three renders of the same bounds.
///
/// Bit-identical renders score exactly `0.0` (the pipeline is a pure
/// function — see the determinism golden in `tests/golden_mir.rs`), so any
/// positive reading is drift; the epsilon only absorbs float summation
/// order, never real difference.
pub const NULL_TEST_MAX_SPEC_L1: f32 = 1e-6;

/// Null-test gate: maximum allowed pairwise `mel_l1` distance, same
/// rationale as [`crate::render::NULL_TEST_MAX_SPEC_L1`].
pub const NULL_TEST_MAX_MEL_L1: f32 = 1e-6;

/// Null-test gate: minimum allowed pairwise transient F1.
///
/// Identical renders agree on every onset (`1.0`); anything less means the
/// envelopes diverged.
pub const NULL_TEST_MIN_TRANSIENT_F1: f32 = 1.0;

/// Sample-rate acceptance note recorded on every ingested render.
///
/// 44.1 kHz renders are *not* converted: they are analysed at their native
/// rate (audibly incomparable to 48 kHz products by parameter mismatch)
/// and flagged for an explicit REAPER-side resample before joining a
/// 48 kHz comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateNote {
    /// 48 kHz: analysed with [`crate::mir::MirParams::v1`], comparable.
    Native48k,
    /// 44.1 kHz: ingested, analysed native-rate, resample still owed.
    ResampleTodo,
}

impl RateNote {
    /// Whether a REAPER-side resample to 48 kHz is still owed before this
    /// product may join a 48 kHz comparison.
    #[must_use]
    pub fn needs_resample(&self) -> bool {
        matches!(self, Self::ResampleTodo)
    }

    /// The [`crate::mir::MirParams`] this rate is analysed with.
    ///
    /// 48 kHz is exactly [`crate::mir::MirParams::v1`]; 44.1 kHz mirrors v1
    /// with the rate and the Nyquist edge adjusted, so
    /// [`crate::score::compare`] refuses cross-rate pairs instead of
    /// silently scoring incomparable numbers.
    #[must_use]
    pub fn mir_params(&self) -> MirParams {
        match self {
            Self::Native48k => MirParams::v1(),
            Self::ResampleTodo => MirParams {
                sample_rate: RATE_44K1,
                f_max_hz: RATE_44K1 as f32 * 0.5,
                ..MirParams::v1()
            },
        }
    }
}

/// Explicit channel policy applied while decoding a render to mono.
///
/// Mono passes through untouched; stereo is mean-downmixed
/// (`(L + R) * 0.5` per frame, deterministic). Renders with more than two
/// channels are refused with
/// [`crate::EvalError::UnsupportedChannels`] — the caller pins mono or
/// stereo REAPER-side first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelHandling {
    /// Single channel passed through untouched.
    MonoPassthrough,
    /// Two channels averaged to mono.
    StereoAveraged,
}

impl ChannelHandling {
    /// Channel count this handling was built from (1 or 2).
    #[must_use]
    pub fn channels_in(&self) -> usize {
        match self {
            Self::MonoPassthrough => 1,
            Self::StereoAveraged => 2,
        }
    }
}

/// Codec priming/padding trim recorded on lossy-decode products (TSK-208).
///
/// Lossless wav renders ingest with `trim: None`. AAC-style decodes must
/// first drop the encoder delay and the tail padding via
/// [`crate::mir::strip_codec_padding`]; the parameters used are kept here
/// so the product stays auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecTrim {
    /// Samples dropped from the head (encoder priming).
    pub trim_head: usize,
    /// Source frame count kept after the head drop (tail truncated past it).
    pub target_len: usize,
}

impl CodecTrim {
    /// The AAC consumer rule (TSK-208): drop
    /// [`crate::mir::AAC_PRIMING_SAMPLES`] priming samples, keep
    /// `source_len` frames.
    ///
    /// # Errors
    ///
    /// This constructor cannot fail; trimming itself clamps deterministically
    /// (see [`crate::mir::strip_codec_padding`]).
    #[must_use]
    pub fn aac(source_len: usize) -> Self {
        Self {
            trim_head: AAC_PRIMING_SAMPLES,
            target_len: source_len,
        }
    }
}

/// One ingested 42230 render: file provenance plus mono PCM plus MIR.
///
/// `path` + `bytes_hint` identify the REAPER-side file: `bytes_hint` is an
/// advisory expected size (0 skips the check); a nonzero hint that
/// disagrees with the bytes on disk is refused with
/// [`crate::EvalError::ByteMismatch`]. `trim` is `None` for lossless wav
/// renders and `Some` for lossy-decode products trimmed by the TSK-208 rule.
#[derive(Debug, Clone)]
pub struct RenderProduct {
    /// Rendered file this product was ingested from (provenance).
    pub path: PathBuf,
    /// Advisory expected file size in bytes (0 = unchecked).
    pub bytes_hint: u64,
    /// Actual file size in bytes observed at ingest.
    pub bytes_actual: u64,
    /// Render sample rate in Hz (48 000 or 44 100).
    pub sample_rate: u32,
    /// Decoded channel count (1 or 2).
    pub channels_in: usize,
    /// Channel policy applied while decoding to mono.
    pub channel_handling: ChannelHandling,
    /// Rate acceptance note (44.1 kHz owes a resample).
    pub rate_note: RateNote,
    /// Lossy-decode trim parameters, if any were applied.
    pub trim: Option<CodecTrim>,
    /// Mono f32 PCM at [`crate::render::RenderProduct::sample_rate`].
    pub pcm_mono: Vec<f32>,
    /// MIR v1 features of [`crate::render::RenderProduct::pcm_mono`].
    pub features: MirFeatures,
}

impl RenderProduct {
    /// FNV-1a (32-bit) hash of the mono PCM bit pattern.
    ///
    /// The null-test gate compares these across three renders; the hex
    /// form matches the A03 §8 evidence style (`11d6dc2f`).
    #[must_use]
    pub fn fnv(&self) -> u32 {
        fnv1a_f32(&self.pcm_mono)
    }

    /// Whether a REAPER-side resample to 48 kHz is still owed.
    #[must_use]
    pub fn needs_resample(&self) -> bool {
        self.rate_note.needs_resample()
    }
}

/// FNV-1a (32-bit) over the little-endian bytes of each sample.
///
/// Offset basis `0x811c9dc5`, prime `0x01000193`; deterministic across
/// platforms (`to_le_bytes` is endian-explicit). Any single-bit PCM drift
/// changes the hash.
#[must_use]
pub fn fnv1a_f32(samples: &[f32]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for sample in samples {
        for byte in sample.to_le_bytes() {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
    }
    hash
}

/// Ingest a lossless 42230 wav render: read → mono PCM → analyse.
///
/// See the module docs for the acceptance contract. Rejections
/// ([`crate::EvalError::RenderIo`], [`crate::EvalError::BadWav`],
/// [`crate::EvalError::UnsupportedRate`],
/// [`crate::EvalError::UnsupportedChannels`],
/// [`crate::EvalError::ByteMismatch`], plus the [`crate::mir::analyze`]
/// rejections for too-short/silent input) are all `BLOCKED`.
///
/// # Errors
///
/// `BLOCKED` ingest rejections listed above; never retries silently.
pub fn ingest_wav(path: &Path, bytes_hint: u64) -> Result<RenderProduct, EvalError> {
    ingest_wav_inner(path, bytes_hint, None)
}

/// Ingest a lossy-decode render with the TSK-208 trim applied first.
///
/// The wav on disk holds the raw decode (priming + source + tail padding);
/// `trim` (usually [`crate::render::CodecTrim::aac`]) selects the source
/// window via [`crate::mir::strip_codec_padding`] before analysis, and is
/// recorded on the product.
///
/// # Errors
///
/// Same `BLOCKED` rejections as [`crate::render::ingest_wav`].
pub fn ingest_wav_trimmed(
    path: &Path,
    bytes_hint: u64,
    trim: CodecTrim,
) -> Result<RenderProduct, EvalError> {
    ingest_wav_inner(path, bytes_hint, Some(trim))
}

/// Three-render determinism verdict (only produced when the gate passes).
///
/// `Ok` from [`crate::render::NullTestGate::check_three`] implies a stable
/// triple: identical FNV hashes, pairwise spectral distances within
/// [`crate::render::NULL_TEST_MAX_SPEC_L1`] /
/// [`crate::render::NULL_TEST_MAX_MEL_L1`], and full onset agreement. The
/// fields are kept for the audit record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NullVerdict {
    /// FNV-1a of each render's mono PCM, in input order.
    pub fnv: [u32; 3],
    /// Whether all three hashes are equal (always true on `Ok`).
    pub fnv_match: bool,
    /// Largest pairwise `spec_l1` over the three pairs.
    pub max_spec_l1: f32,
    /// Largest pairwise `mel_l1` over the three pairs.
    pub max_mel_l1: f32,
    /// Smallest pairwise transient F1 over the three pairs.
    pub min_transient_f1: f32,
}

/// Three-render null-test gate (A03 §8 determinism method).
///
/// The caller renders the same fixed bounds three times (non-target tracks
/// muted, block pinned) and ingests each via
/// [`crate::render::ingest_wav`]; the gate demands bit-identical PCM (FNV)
/// plus pairwise spectral distances within threshold. Any variance —
/// length drift, param drift, hash drift, spectral drift — is
/// [`crate::EvalError::RenderUnstable`] (`BLOCKED`): fix the render setup
/// REAPER-side (leaked live content is the classic cause, A03 §8) or walk
/// the [`crate::render::FallbackPolicy`] chain, then re-ingest.
pub struct NullTestGate;

impl NullTestGate {
    /// Check exactly three ingested renders; `Ok` verdict or `BLOCKED`.
    ///
    /// # Errors
    ///
    /// [`crate::EvalError::InvalidSamples`] when `renders` does not hold
    /// exactly three products; [`crate::EvalError::RenderUnstable`] on any
    /// variance over the gate (including length/param drift between the
    /// three, which is instability by definition).
    pub fn check_three(renders: &[RenderProduct]) -> Result<NullVerdict, EvalError> {
        if renders.len() != 3 {
            return Err(EvalError::InvalidSamples(format!(
                "null-test needs exactly three renders, got {}",
                renders.len()
            )));
        }
        let fnv = [renders[0].fnv(), renders[1].fnv(), renders[2].fnv()];
        let pairs = [
            (&renders[0], &renders[1]),
            (&renders[0], &renders[2]),
            (&renders[1], &renders[2]),
        ];
        let mut max_spec_l1 = 0.0_f32;
        let mut max_mel_l1 = 0.0_f32;
        let mut min_transient_f1 = 1.0_f32;
        for (a, b) in pairs {
            match compare(&a.features, &b.features) {
                Ok(score) => {
                    max_spec_l1 = max_spec_l1.max(score.spec_l1);
                    max_mel_l1 = max_mel_l1.max(score.mel_l1);
                    min_transient_f1 = min_transient_f1.min(score.transient_f1);
                }
                Err(pair_err) => {
                    return Err(EvalError::RenderUnstable {
                        detail: format!(
                            "render pair incomparable (length or param drift): {pair_err}"
                        ),
                    });
                }
            }
        }
        let fnv_match = fnv[0] == fnv[1] && fnv[1] == fnv[2];
        let stable = fnv_match
            && max_spec_l1 <= NULL_TEST_MAX_SPEC_L1
            && max_mel_l1 <= NULL_TEST_MAX_MEL_L1
            && min_transient_f1 >= NULL_TEST_MIN_TRANSIENT_F1;
        if stable {
            Ok(NullVerdict {
                fnv,
                fnv_match,
                max_spec_l1,
                max_mel_l1,
                min_transient_f1,
            })
        } else {
            Err(EvalError::RenderUnstable {
                detail: format!(
                    "variance over gate: fnv_match={fnv_match} \
                     fnv=[{:08x},{:08x},{:08x}] \
                     max_spec_l1={max_spec_l1:.3e} max_mel_l1={max_mel_l1:.3e} \
                     min_transient_f1={min_transient_f1:.4}",
                    fnv[0], fnv[1], fnv[2]
                ),
            })
        }
    }
}

/// Render-retry stage (decision side only).
///
/// Speed/block switching executes REAPER-side Lua (stock ReaScript has no
/// `RENDER_1X` setter — the key lives only in the project chunk, A03 §8);
/// this enum is the decision vocabulary the Lua side acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderStage {
    /// Full-speed offline render (first attempt, DEC-005 main path).
    FullSpeed,
    /// Pinned small block (64) offline render (A03 §4 first fallback).
    SmallBlock,
    /// 1x offline render (A03 §4 second fallback; `RENDER_1X 1` chunk-side).
    OneX,
    /// Real-time online render (A03 §4 last fallback; `RENDER_1X 2`).
    Online,
}

impl RenderStage {
    /// Whether the chain ends here (no further fallback exists).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Online)
    }

    /// Short stable label for audit records and UI surfaces.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::FullSpeed => "full-speed",
            Self::SmallBlock => "small-block",
            Self::OneX => "1x-offline",
            Self::Online => "online",
        }
    }
}

/// One recorded fallback step: where the chain moved and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackRecord {
    /// Stage abandoned.
    pub from: RenderStage,
    /// Stage to render with next (executed REAPER-side).
    pub to: RenderStage,
    /// Operator-readable cause (A03 §4 phenomenon + what to switch).
    pub reason: &'static str,
}

/// Fixed fallback decision table (A03 §4): full-speed → small block →
/// 1x offline → online, then terminal.
///
/// Pure decision + record: calling [`crate::render::FallbackPolicy::next`]
/// never renders anything. The caller appends each returned
/// [`crate::render::FallbackRecord`] to its audit trail, performs the
/// switch REAPER-side, re-renders, and re-ingests via
/// [`crate::render::ingest_wav`].
pub struct FallbackPolicy;

impl FallbackPolicy {
    /// Next fallback step after `current`, or `None` past
    /// [`crate::render::RenderStage::Online`] (chain exhausted → `BLOCKED`).
    #[must_use]
    pub fn next(current: RenderStage) -> Option<FallbackRecord> {
        match current {
            RenderStage::FullSpeed => Some(FallbackRecord {
                from: RenderStage::FullSpeed,
                to: RenderStage::SmallBlock,
                reason: "full-speed offline render unstable (head glitch or tail loss, A03 §4); \
                         retry with pinned small block 64",
            }),
            RenderStage::SmallBlock => Some(FallbackRecord {
                from: RenderStage::SmallBlock,
                to: RenderStage::OneX,
                reason: "small-block render still unstable; retry 1x offline \
                         (RENDER_1X is chunk-only with no stock setter — switch REAPER-side, A03 §8)",
            }),
            RenderStage::OneX => Some(FallbackRecord {
                from: RenderStage::OneX,
                to: RenderStage::Online,
                reason: "1x offline still unstable (e.g. missing-sample hosts or offline stall, A03 §4); \
                         retry online render REAPER-side",
            }),
            RenderStage::Online => None,
        }
    }

    /// The whole chain from [`crate::render::RenderStage::FullSpeed`]:
    /// three records ending at [`crate::render::RenderStage::Online`].
    #[must_use]
    pub fn full_chain() -> [FallbackRecord; 3] {
        [
            FallbackRecord {
                from: RenderStage::FullSpeed,
                to: RenderStage::SmallBlock,
                reason: "full-speed offline render unstable (head glitch or tail loss, A03 §4); \
                         retry with pinned small block 64",
            },
            FallbackRecord {
                from: RenderStage::SmallBlock,
                to: RenderStage::OneX,
                reason: "small-block render still unstable; retry 1x offline \
                         (RENDER_1X is chunk-only with no stock setter — switch REAPER-side, A03 §8)",
            },
            FallbackRecord {
                from: RenderStage::OneX,
                to: RenderStage::Online,
                reason: "1x offline still unstable (e.g. missing-sample hosts or offline stall, A03 §4); \
                         retry online render REAPER-side",
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Wav intake (std-only: no new decoder dependency, AGENTS.md red line 6)
// ---------------------------------------------------------------------------

/// File-name-only label for errors (never an absolute path, §8 scrub rule).
fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unnameable>")
        .to_owned()
}

/// Decoded wav header fields plus the raw data-chunk bytes.
struct WavFields {
    sample_rate: u32,
    channels: u16,
    format_tag: u16,
    bits_per_sample: u16,
    data: Vec<u8>,
}

/// Byte cursor over a wav image; overruns are
/// [`crate::EvalError::BadWav`], never panics.
struct WavCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    hint: String,
}

impl<'a> WavCursor<'a> {
    fn new(bytes: &'a [u8], hint: &str) -> Self {
        Self {
            bytes,
            pos: 0,
            hint: hint.to_owned(),
        }
    }

    fn bad(&self, detail: &str) -> EvalError {
        EvalError::BadWav {
            hint: self.hint.clone(),
            detail: detail.to_owned(),
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], EvalError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| self.bad("chunk overruns file"))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| self.bad("chunk overruns file"))?;
        self.pos = end;
        Ok(slice)
    }

    fn skip(&mut self, n: usize) -> Result<(), EvalError> {
        self.take(n).map(|_| ())
    }

    fn u16_le(&mut self) -> Result<u16, EvalError> {
        let raw = self.take(2)?;
        Ok(u16::from_le_bytes([raw[0], raw[1]]))
    }

    fn u32_le(&mut self) -> Result<u32, EvalError> {
        let raw = self.take(4)?;
        Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }
}

/// Parse a RIFF/WAVE image; accepts format tags 1 (PCM int) and 3
/// (IEEE float), mono or stereo, any order of `fmt `/`data` chunks with
/// unknown chunks skipped (including the RIFF pad byte after odd sizes).
fn parse_wav(bytes: &[u8], hint: &str) -> Result<WavFields, EvalError> {
    let bad = |detail: &str| EvalError::BadWav {
        hint: hint.to_owned(),
        detail: detail.to_owned(),
    };
    if bytes.get(0..4) != Some(b"RIFF".as_slice()) {
        return Err(bad("missing RIFF magic"));
    }
    if bytes.get(8..12) != Some(b"WAVE".as_slice()) {
        return Err(bad("missing WAVE magic"));
    }
    let mut cursor = WavCursor::new(bytes, hint);
    cursor.skip(12)?;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<Vec<u8>> = None;
    while cursor.remaining() >= 8 {
        let id = cursor.take(4)?.to_vec();
        let size_u32 = cursor.u32_le()?;
        let size = usize::try_from(size_u32).map_err(|_| bad("chunk size not addressable"))?;
        if id == b"fmt ".as_slice() {
            if fmt.is_some() {
                return Err(bad("duplicate fmt chunk"));
            }
            let mut body = WavCursor::new(cursor.take(size)?, hint);
            let tag = body.u16_le()?;
            let channels = body.u16_le()?;
            let rate = body.u32_le()?;
            body.skip(6)?;
            let bits = body.u16_le()?;
            fmt = Some((tag, channels, rate, bits));
        } else if id == b"data".as_slice() {
            if data.is_none() {
                data = Some(cursor.take(size)?.to_vec());
            } else {
                cursor.skip(size)?;
            }
        } else {
            cursor.skip(size)?;
        }
        if size % 2 == 1 {
            cursor.skip(1)?;
        }
    }
    let Some((format_tag, channels, sample_rate, bits_per_sample)) = fmt else {
        return Err(bad("missing fmt chunk"));
    };
    let Some(data) = data else {
        return Err(bad("missing data chunk"));
    };
    Ok(WavFields {
        sample_rate,
        channels,
        format_tag,
        bits_per_sample,
        data,
    })
}

/// One sample of a PCM-int / float data chunk as f32 (full-scale-normalised).
fn sample_to_f32(chunk: &[u8], format_tag: u16, bits: u16, hint: &str) -> Result<f32, EvalError> {
    let bad = |detail: String| EvalError::BadWav {
        hint: hint.to_owned(),
        detail,
    };
    match (format_tag, bits) {
        (1, 8) => {
            let byte = chunk
                .first()
                .copied()
                .ok_or_else(|| bad("short 8-bit sample".to_owned()))?;
            Ok((f32::from(byte) - 128.0) / 128.0)
        }
        (1, 16) => {
            let raw: [u8; 2] = chunk
                .first_chunk::<2>()
                .copied()
                .ok_or_else(|| bad("short 16-bit sample".to_owned()))?;
            Ok(f32::from(i16::from_le_bytes(raw)) / 32_768.0)
        }
        (1, 24) => {
            let raw: [u8; 3] = chunk
                .first_chunk::<3>()
                .copied()
                .ok_or_else(|| bad("short 24-bit sample".to_owned()))?;
            let magnitude =
                i32::from(raw[0]) | (i32::from(raw[1]) << 8) | (i32::from(raw[2]) << 16);
            let signed = (magnitude << 8) >> 8;
            Ok(signed as f32 / 8_388_608.0)
        }
        (1, 32) => {
            let raw: [u8; 4] = chunk
                .first_chunk::<4>()
                .copied()
                .ok_or_else(|| bad("short 32-bit sample".to_owned()))?;
            Ok(i32::from_le_bytes(raw) as f32 / 2_147_483_648.0)
        }
        (3, 32) => {
            let raw: [u8; 4] = chunk
                .first_chunk::<4>()
                .copied()
                .ok_or_else(|| bad("short float sample".to_owned()))?;
            Ok(f32::from_le_bytes(raw))
        }
        _ => Err(bad(format!(
            "unsupported wav encoding: format tag {format_tag}, {bits} bits \
             (render PCM int 8/16/24/32 or 32-bit float)"
        ))),
    }
}

/// Data-chunk bytes → mono f32 (mean-downmix when stereo; mono untouched).
fn data_to_mono(fields: &WavFields, hint: &str) -> Result<(Vec<f32>, usize), EvalError> {
    let bad = |detail: String| EvalError::BadWav {
        hint: hint.to_owned(),
        detail,
    };
    let channels = usize::from(fields.channels);
    if channels == 0 {
        return Err(bad("zero channels".to_owned()));
    }
    let bytes_per_sample = match (fields.format_tag, fields.bits_per_sample) {
        (1, 8) | (3, 8) => 1_usize,
        (1, 16) => 2_usize,
        (1, 24) => 3_usize,
        (1, 32) | (3, 32) => 4_usize,
        _ => {
            return Err(bad(format!(
                "unsupported wav encoding: format tag {}, {} bits \
                 (render PCM int 8/16/24/32 or 32-bit float)",
                fields.format_tag, fields.bits_per_sample
            )));
        }
    };
    let frame_bytes = channels
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| bad("channel count overflows frame".to_owned()))?;
    if !fields.data.len().is_multiple_of(frame_bytes) {
        return Err(bad("data length is not a whole number of frames".to_owned()));
    }
    let mut mono = Vec::with_capacity(fields.data.len() / frame_bytes);
    for frame in fields.data.chunks_exact(frame_bytes) {
        let mut sum = 0.0_f64;
        for channel in frame.chunks_exact(bytes_per_sample) {
            sum += f64::from(sample_to_f32(
                channel,
                fields.format_tag,
                fields.bits_per_sample,
                hint,
            )?);
        }
        mono.push((sum / channels as f64) as f32);
    }
    Ok((mono, channels))
}

fn ingest_wav_inner(
    path: &Path,
    bytes_hint: u64,
    trim: Option<CodecTrim>,
) -> Result<RenderProduct, EvalError> {
    let hint = file_label(path);
    let bytes = std::fs::read(path).map_err(|io_err| EvalError::RenderIo {
        hint: hint.clone(),
        detail: format!("read failed: {:?}", io_err.kind()),
    })?;
    let bytes_actual = bytes.len() as u64;
    if bytes_hint != 0 && bytes_actual != bytes_hint {
        return Err(EvalError::ByteMismatch {
            hint: bytes_hint,
            got: bytes_actual,
        });
    }
    let fields = parse_wav(&bytes, &hint)?;
    let rate_note = match fields.sample_rate {
        RATE_48K => RateNote::Native48k,
        RATE_44K1 => RateNote::ResampleTodo,
        other => return Err(EvalError::UnsupportedRate { got: other }),
    };
    let channel_handling = match fields.channels {
        1 => ChannelHandling::MonoPassthrough,
        2 => ChannelHandling::StereoAveraged,
        other => {
            return Err(EvalError::UnsupportedChannels {
                got: usize::from(other),
            });
        }
    };
    let (mut pcm_mono, channels_in) = data_to_mono(&fields, &hint)?;
    if let Some(active) = trim {
        pcm_mono = strip_codec_padding(&pcm_mono, active.trim_head, active.target_len).to_vec();
    }
    let params = rate_note.mir_params();
    let features = analyze(&pcm_mono, &params)?;
    Ok(RenderProduct {
        path: path.to_path_buf(),
        bytes_hint,
        bytes_actual,
        sample_rate: fields.sample_rate,
        channels_in,
        channel_handling,
        rate_note,
        trim,
        pcm_mono,
        features,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("synthlm-tsk504-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create tsk504 scratch dir");
        dir
    }

    /// Stationary tone with f64 phase accumulation (same rationale as the
    /// `sine_mix` generator in `tests/golden_mir.rs`: f32 angle error would
    /// amplitude-modulate the tone and fake spectral flux).
    fn sine(frames: usize, rate: f32, freq: f32, amp: f32) -> Vec<f32> {
        let step = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(rate);
        let mut phase = 0.0_f64;
        let mut out = Vec::with_capacity(frames);
        for _ in 0..frames {
            out.push((f64::from(amp) * phase.sin()) as f32);
            phase += step;
        }
        out
    }

    /// Minimal PCM16 wav writer (test-only): one entry per channel.
    fn write_wav_i16(path: &Path, channels: &[Vec<f32>], rate: u32) {
        assert!(!channels.is_empty(), "need at least one channel");
        let frames = channels[0].len();
        for channel in channels {
            assert_eq!(channel.len(), frames, "channels must share length");
        }
        let mut data = Vec::with_capacity(frames * channels.len() * 2);
        for i in 0..frames {
            for channel in channels {
                let quantized = (channel[i].clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                data.extend_from_slice(&quantized.to_le_bytes());
            }
        }
        let mut wav = Vec::with_capacity(44 + data.len());
        wav.extend_from_slice(b"RIFF");
        let riff_size = 36_u32 + data.len() as u32;
        wav.extend_from_slice(&riff_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&(channels.len() as u16).to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        let byte_rate = rate * channels.len() as u32 * 2;
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = channels.len() as u16 * 2;
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        std::fs::write(path, &wav).expect("write test wav");
    }

    fn fixture_spike_tone() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../experiments/spike-tone.wav")
    }

    #[test]
    fn ingest_accepts_48k_mono() {
        let path = scratch_dir().join("accept-48k.wav");
        write_wav_i16(&path, &[sine(48_000, 48_000.0, 440.0, 0.4)], RATE_48K);
        let product = ingest_wav(&path, 0).expect("48k mono must ingest");
        assert_eq!(product.sample_rate, RATE_48K);
        assert_eq!(product.channels_in, 1);
        assert_eq!(product.channel_handling, ChannelHandling::MonoPassthrough);
        assert_eq!(product.rate_note, RateNote::Native48k);
        assert!(!product.needs_resample());
        assert_eq!(product.trim, None);
        assert_eq!(product.pcm_mono.len(), 48_000);
        assert!(product.features.n_frames > 0);
        assert_eq!(product.features.params, MirParams::v1());
    }

    #[test]
    fn ingest_flags_44100_resample_todo() {
        // Fixed repo fixture (read-only): PCM16 mono 44.1 kHz, 1 s, 88244 bytes.
        let path = fixture_spike_tone();
        assert!(path.is_file(), "missing fixture {}", path.display());
        let product = ingest_wav(&path, 0).expect("44.1k fixture must ingest");
        assert_eq!(product.sample_rate, RATE_44K1);
        assert_eq!(product.channels_in, 1);
        assert_eq!(product.channel_handling, ChannelHandling::MonoPassthrough);
        assert_eq!(product.rate_note, RateNote::ResampleTodo);
        assert!(
            product.needs_resample(),
            "44.1k owes a REAPER-side resample"
        );
        assert_eq!(product.bytes_actual, 88_244);
        assert_eq!(product.pcm_mono.len(), 44_100);
        assert!(product.features.n_frames > 0);
    }

    #[test]
    fn ingest_rejects_bad_rate_many_channels_missing_and_size_hint() {
        let dir = scratch_dir();
        // Bad rate: 32 kHz is outside the accepted set.
        let bad_rate = dir.join("reject-rate.wav");
        write_wav_i16(&bad_rate, &[sine(32_000, 32_000.0, 440.0, 0.4)], 32_000);
        let err = ingest_wav(&bad_rate, 0).expect_err("32kHz must be refused");
        assert!(matches!(err, EvalError::UnsupportedRate { got: 32_000 }));
        assert!(err.blocked(), "rate rejection is BLOCKED");
        // Six channels: surround must be pinned to mono/stereo REAPER-side.
        let bed = sine(48_000, 48_000.0, 440.0, 0.2);
        let six = vec![
            bed.clone(),
            bed.clone(),
            bed.clone(),
            bed.clone(),
            bed.clone(),
            bed,
        ];
        let bad_ch = dir.join("reject-6ch.wav");
        write_wav_i16(&bad_ch, &six, RATE_48K);
        let err = ingest_wav(&bad_ch, 0).expect_err("6ch must be refused");
        assert!(matches!(err, EvalError::UnsupportedChannels { got: 6 }));
        assert!(err.blocked(), "channel rejection is BLOCKED");
        // Missing file.
        let missing = dir.join("does-not-exist-t504.wav");
        let err = ingest_wav(&missing, 0).expect_err("missing file must fail");
        assert!(matches!(err, EvalError::RenderIo { .. }));
        assert!(err.blocked(), "missing-file failure is BLOCKED");
        // Size-hint disagreement.
        let ok_path = dir.join("hint-check.wav");
        write_wav_i16(&ok_path, &[sine(48_000, 48_000.0, 440.0, 0.4)], RATE_48K);
        let actual = std::fs::metadata(&ok_path).expect("stat test wav").len();
        let err = ingest_wav(&ok_path, actual + 1).expect_err("wrong hint must fail");
        assert!(matches!(err, EvalError::ByteMismatch { hint: _, got: _ }));
        assert!(err.blocked(), "size mismatch is BLOCKED");
        // The true size ingests fine (hint is advisory, not a second gate).
        ingest_wav(&ok_path, actual).expect("exact hint must ingest");
    }

    #[test]
    fn stereo_averages_to_mono() {
        let dir = scratch_dir();
        let left = sine(48_000, 48_000.0, 440.0, 0.4);
        let path = dir.join("stereo.wav");
        write_wav_i16(&path, &[left.clone(), left.clone()], RATE_48K);
        let product = ingest_wav(&path, 0).expect("stereo must ingest");
        assert_eq!(product.channels_in, 2);
        assert_eq!(product.channel_handling, ChannelHandling::StereoAveraged);
        assert_eq!(product.channel_handling.channels_in(), 2);
        assert_eq!(ChannelHandling::MonoPassthrough.channels_in(), 1);
        assert_eq!(product.pcm_mono.len(), 48_000);
        // Identical channels average back to the channel (within i16 quant).
        for (got, want) in product.pcm_mono.iter().zip(left.iter()).take(2048) {
            assert!(
                (got - want).abs() < 5e-5,
                "downmix drifted: {got} vs {want}"
            );
        }
    }

    #[test]
    fn trim_recovers_aac_shaped_decode() {
        // Simulate an AAC decode: 1024 priming + 44100 source + 956 tail pad
        // (the TSK-206 matrix shape), then trim back to the source window.
        let dir = scratch_dir();
        let source = sine(44_100, 44_100.0, 440.0, 0.4);
        let mut decoded = vec![0.0_f32; 1024];
        decoded.extend_from_slice(&source);
        decoded.extend(std::iter::repeat_n(0.0_f32, 956));
        assert_eq!(decoded.len(), 46_080);
        let path = dir.join("aac-shaped.wav");
        write_wav_i16(&path, &[decoded], RATE_44K1);
        let trim = CodecTrim::aac(44_100);
        assert_eq!(trim.trim_head, AAC_PRIMING_SAMPLES);
        let product = ingest_wav_trimmed(&path, 0, trim).expect("trimmed ingest must pass");
        assert_eq!(product.trim, Some(trim));
        assert_eq!(product.pcm_mono.len(), 44_100);
        assert_eq!(product.rate_note, RateNote::ResampleTodo);
        for (got, want) in product.pcm_mono.iter().zip(source.iter()).take(4096) {
            assert!(
                (got - want).abs() < 5e-5,
                "trim window drifted: {got} vs {want}"
            );
        }
    }

    #[test]
    fn same_file_twice_is_bit_identical() {
        let path = scratch_dir().join("determinism.wav");
        write_wav_i16(&path, &[sine(48_000, 48_000.0, 445.3125, 0.4)], RATE_48K);
        let first = ingest_wav(&path, 0).expect("first ingest");
        let second = ingest_wav(&path, 0).expect("second ingest");
        assert_eq!(first.pcm_mono, second.pcm_mono, "PCM must be bit-identical");
        assert_eq!(first.fnv(), second.fnv(), "FNV must agree");
        assert_eq!(first.features.magnitude, second.features.magnitude);
        assert_eq!(first.features.log_mel, second.features.log_mel);
        assert_eq!(first.features.flux, second.features.flux);
        assert_eq!(first.features.onsets, second.features.onsets);
        assert_eq!(
            first.features.integrated_lufs,
            second.features.integrated_lufs
        );
        assert_eq!(
            first.features.normalized_dbtp,
            second.features.normalized_dbtp
        );
    }

    #[test]
    fn null_test_passes_on_three_identical() {
        let path = scratch_dir().join("null-pass.wav");
        write_wav_i16(&path, &[sine(48_000, 48_000.0, 445.3125, 0.4)], RATE_48K);
        let renders = [
            ingest_wav(&path, 0).expect("render 1"),
            ingest_wav(&path, 0).expect("render 2"),
            ingest_wav(&path, 0).expect("render 3"),
        ];
        let verdict = NullTestGate::check_three(&renders).expect("identical triple must pass");
        assert!(verdict.fnv_match);
        assert_eq!(verdict.fnv[0], verdict.fnv[1]);
        assert_eq!(verdict.fnv[1], verdict.fnv[2]);
        assert_eq!(verdict.max_spec_l1, 0.0);
        assert_eq!(verdict.max_mel_l1, 0.0);
        assert_eq!(verdict.min_transient_f1, 1.0);
    }

    #[test]
    fn null_test_blocks_on_single_drift() {
        let dir = scratch_dir();
        let base = sine(48_000, 48_000.0, 445.3125, 0.4);
        let steady = dir.join("null-steady.wav");
        write_wav_i16(&steady, std::slice::from_ref(&base), RATE_48K);
        let mut drifted = base;
        drifted[24_000] += 0.05;
        let drift_path = dir.join("null-drift.wav");
        write_wav_i16(&drift_path, &[drifted], RATE_48K);
        let renders = [
            ingest_wav(&steady, 0).expect("render 1"),
            ingest_wav(&steady, 0).expect("render 2"),
            ingest_wav(&drift_path, 0).expect("render 3 (drifted)"),
        ];
        assert_ne!(renders[0].fnv(), renders[2].fnv(), "drift must move FNV");
        let err = NullTestGate::check_three(&renders).expect_err("drifted triple must hang");
        assert!(matches!(err, EvalError::RenderUnstable { .. }));
        assert!(err.blocked(), "null-test failure is BLOCKED");
        // Wrong arity is a usage error, also BLOCKED.
        let err = NullTestGate::check_three(&renders[..2]).expect_err("pair must be refused");
        assert!(matches!(err, EvalError::InvalidSamples(_)));
    }

    #[test]
    fn fallback_walks_full_chain_with_records() {
        assert!(!RenderStage::FullSpeed.is_terminal());
        assert!(!RenderStage::SmallBlock.is_terminal());
        assert!(!RenderStage::OneX.is_terminal());
        assert!(RenderStage::Online.is_terminal());
        let mut stage = RenderStage::FullSpeed;
        let mut records = Vec::new();
        while let Some(record) = FallbackPolicy::next(stage) {
            assert_eq!(record.from, stage, "record must start where the chain is");
            assert!(!record.reason.is_empty(), "every step records its reason");
            stage = record.to;
            records.push(record);
        }
        assert_eq!(stage, RenderStage::Online, "chain must end at online");
        assert_eq!(records.len(), 3, "full chain is three steps");
        let expected = [
            (RenderStage::FullSpeed, RenderStage::SmallBlock),
            (RenderStage::SmallBlock, RenderStage::OneX),
            (RenderStage::OneX, RenderStage::Online),
        ];
        for (record, (from, to)) in records.iter().zip(expected) {
            assert_eq!(record.from, from);
            assert_eq!(record.to, to);
        }
        assert_eq!(records, FallbackPolicy::full_chain().to_vec());
        assert!(
            FallbackPolicy::next(RenderStage::Online).is_none(),
            "online is terminal: chain exhausted means BLOCKED, not a fourth retry"
        );
    }

    #[test]
    fn fnv_changes_on_bit_drift() {
        let a = sine(4096, 48_000.0, 440.0, 0.4);
        let mut b = a.clone();
        b[1000] = f32::from_bits(b[1000].to_bits() ^ 1);
        assert_ne!(fnv1a_f32(&a), fnv1a_f32(&b), "one ULP must move FNV");
        assert_eq!(fnv1a_f32(&a), fnv1a_f32(&a), "same bits hash equal");
    }
}
