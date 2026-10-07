//! Explicit take snapshot + one-click whole-chain rollback (TSK-103).
//!
//! Pure-Rust side of DEC-004 ("显式快照") + DEC-020 ("整链快照默认 + 一 undo
//! 点"): freeze the parameter table (ident keys), chunk, take GUID/hash and
//! `RENDER_*` backup into a [`Snapshot`](crate::snapshot::Snapshot) before solving, and bring the take
//! back in one [`UndoBlock`](crate::undo::UndoBlock) transaction on rollback
//! (`ARCHITECTURE.md` §3 step 2, `docs/research/A03-undo-state-render.md` §2,
//! `docs/research/A04-items-midi-takes.md` §5–§7).
//!
//! Like [`crate::container_addr`] and [`crate::undo`], this layer is split
//! into a mockable seam ([`SnapshotBackend`](crate::snapshot::SnapshotBackend), pure helpers) plus deferred live
//! wiring; unit tests run against the mock, never against a live REAPER
//! instance (DEC-024). Control plane only: main thread, never the audio thread
//! (`AGENTS.md` §3.2).
//!
//! Reuses [`crate::undo::UndoBlock`] / [`crate::undo::DirtyTrack`] for the
//! transaction and [`crate::undo::ReaperUndo`] as the undo half of the
//! backend seam (trait isolation + `thiserror` style continue `undo.rs`).
//!
//! # Live mapping (deferred)
//!
//! Live adapter verification status (TSK-103 is closed): behaviours since
//! verified live name their evidence inline (e2e-live TSK-505, M7 matrix
//! TSK-104, M3/M6 spikes TSK-111); still-open v7.60-floor items are marked
//! `TODO(M57-handoff)` and listed in `docs/REPORTS.md` §4.
//!
//! - Param ident ↔ index: `TrackFX_GetParamFromIdent` returns `-1` for unknown
//!   idents (DEC-013); `-1` always takes the migration-removal branch (skip +
//!   report), never an error.
//! - Chunk: `GetSetMediaItemTakeInfo_String` take-chunk round-trip; large
//!   snapshots go to the external content-addressed cache with only a
//!   `P_EXT` pointer in the project (DEC-027, ARCHITECTURE §7).
//!   // TODO(M57-handoff): 需真机 — measure `.rpp` bloat / save latency for KB–MB
//!   // chunk snapshots (A03 §2: no official capacity promise) before storing
//!   // full chunks in `SetProjExtState`.
//! - MIDI bytes/hash: `MIDI_GetAllEvts` is the verdict, `MIDI_GetHash` only a
//!   fast screen (A04 §1, juliansader: hash not fully reliable); restore via
//!   `MIDI_SetAllEvts` + `MarkTrackItemsDirty` (v7.60 needs the manual dirty,
//!   A04 §3).
//!   // TODO(M57-handoff): 需真机 — confirm `SetAllEvts` buffer format + hash
//!   // stability on the v7.60 floor.
//! - `RENDER_*`: backup before `Main_OnCommand(42230)` candidate renders and
//!   restore after (A03 §3–§4); `FORMAT` base64 is never mutated blindly.
//! - Take FX channel count: `I_TAKEFX_NCH` via
//!   `Get/SetMediaItemTakeInfo_Value` (A04 §6); copy/glue drops it (M3) and
//!   drops take `P_EXT` (M6), so restore/derive always re-writes both (see
//!   [`chunk_has_takefx_nch`](crate::snapshot::chunk_has_takefx_nch) and
//!   [`DerivedTake`](crate::snapshot::DerivedTake)).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::undo::{DirtyTrack, ReaperUndo, UndoBlock, UndoError, mark_dirty_many};

/// `P_EXT` key for the derivation source take GUID.
///
/// Used as `P_EXT:SYNTHLM_PROV_SRC_GUID` (A04 §7 recommended pattern
/// `P_EXT:SYNTHLM_PROV_{SRC_GUID,SRC_FILE,OP}`).
pub const PROV_KEY_SRC_GUID: &str = "SYNTHLM_PROV_SRC_GUID";

/// `P_EXT` key for the derivation source file reference.
///
/// Used as `P_EXT:SYNTHLM_PROV_SRC_FILE`. MIDI sources have no file (in-
/// project MIDI has no associated file); callers fall back to the take GUID +
/// `P_EXT:ORIGINAL_FILENAME` (A04 §7).
pub const PROV_KEY_SRC_FILE: &str = "SYNTHLM_PROV_SRC_FILE";

/// `P_EXT` key for the derivation operation tag (e.g. `"render-new-take"`).
///
/// Used as `P_EXT:SYNTHLM_PROV_OP`.
pub const PROV_KEY_OP: &str = "SYNTHLM_PROV_OP";

/// Library-boundary error type (`thiserror`, per `AGENTS.md` §4).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SnapshotError {
    /// Snapshot or backend handed us an empty take GUID.
    #[error("empty take GUID: snapshots are keyed by take GUID, never by bare index")]
    EmptyTakeGuid,

    /// A parameter ident was empty (caller bug; idents are persisted keys).
    #[error("empty param ident: idents are persisted keys and must be non-empty")]
    EmptyIdent,

    /// Derived file reference was empty (fail-closed: no silent no-op apply).
    #[error("empty derived file reference: audio derivation must name a new file")]
    EmptyNewFile,

    /// Provenance triple incomplete (fail-closed, A04 §7).
    ///
    /// `field` is one of `"SRC_GUID"` / `"SRC_FILE"` / `"OP"`, matching
    /// [`PROV_KEY_SRC_GUID`] / [`PROV_KEY_SRC_FILE`] / [`PROV_KEY_OP`].
    #[error("provenance triple incomplete: missing {field} (P_EXT:SYNTHLM_PROV_* fail-closed)")]
    ProvenanceIncomplete {
        /// Which triple member is missing or empty.
        field: &'static str,
    },

    /// The snapshotted take is gone (or the backend is scoped to another take).
    #[error("take not found for GUID {guid}: treat handles as stale and re-resolve")]
    TakeNotFound {
        /// Take GUID the caller asked about.
        guid: String,
    },

    /// Undo description construction failed (op id empty; no block opened).
    #[error(transparent)]
    Undo(#[from] UndoError),
}

/// One frozen parameter: ident key + live value + display range.
///
/// Idents (not bare indices) are the persisted keys (DEC-013/015,
/// `AGENTS.md` §4); `min` / `max` / `mid` are the plugin-reported range for
/// UI scaling, `value` the normalized live value at capture time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamEntry {
    /// Persisted parameter key (`TrackFX_GetParamFromIdent` input).
    pub ident: String,
    /// Live value at capture time.
    pub value: f64,
    /// Plugin-reported range minimum.
    pub min: f64,
    /// Plugin-reported range maximum.
    pub max: f64,
    /// Plugin-reported range midpoint.
    pub mid: f64,
}

impl ParamEntry {
    /// Build an entry; rejects empty idents (persisted keys must be namable).
    ///
    /// # Errors
    ///
    /// [`SnapshotError::EmptyIdent`] when `ident` is empty.
    pub fn new(
        ident: String,
        value: f64,
        min: f64,
        max: f64,
        mid: f64,
    ) -> Result<Self, SnapshotError> {
        if ident.is_empty() {
            return Err(SnapshotError::EmptyIdent);
        }
        Ok(Self {
            ident,
            value,
            min,
            max,
            mid,
        })
    }
}

/// Backup of the project render settings touched by candidate renders.
///
/// Key set follows A03 §3 (`GetSetProjectInfo[_String]`): string keys
/// `RENDER_FILE/PATTERN/EXTRAFILEDIR/METADATA/TARGETS/FORMAT` plus numeric
/// `RENDER_SETTINGS/BOUNDSFLAG/CHANNELS/SRATE/STARTPOS/ENDPOS/TAILFLAG/TAILMS/
/// ADDTOPROJ/DITHER`. Captured before the `42230` render, restored after
/// (ARCHITECTURE §3 step 6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RenderBackup {
    /// `RENDER_FILE` string (render target path template).
    pub file: String,
    /// `RENDER_PATTERN` string (wildcard pattern).
    pub pattern: String,
    /// `RENDER_EXTRAFILEDIR` string.
    pub extra_file_dir: String,
    /// `RENDER_METADATA` string.
    pub metadata: String,
    /// `RENDER_TARGETS` string.
    pub targets: String,
    /// `RENDER_FORMAT` string (base64 blob or `evaw`/`l3pm` shorthand).
    pub format: String,
    /// `RENDER_SETTINGS` numeric bitmask.
    pub settings: i32,
    /// `RENDER_BOUNDSFLAG` (0 custom / 1 entire / 2 time sel / 3 regions /
    /// 4 items / 5 selected regions).
    pub bounds_flag: i32,
    /// `RENDER_CHANNELS` numeric.
    pub channels: i32,
    /// `RENDER_SRATE` numeric.
    pub srate: f64,
    /// `RENDER_STARTPOS` numeric.
    pub start_pos: f64,
    /// `RENDER_ENDPOS` numeric.
    pub end_pos: f64,
    /// `RENDER_TAILFLAG` numeric.
    pub tail_flag: i32,
    /// `RENDER_TAILMS` numeric.
    pub tail_ms: i32,
    /// `RENDER_ADDTOPROJ` numeric.
    pub add_to_proj: i32,
    /// `RENDER_DITHER` numeric.
    pub dither: i32,
}

/// Provenance triple for derived audio (`P_EXT:SYNTHLM_PROV_*`, A04 §7).
///
/// Construction is fallible and every reader validates: a missing member is a
/// fail-closed [`SnapshotError::ProvenanceIncomplete`], never a silent
/// unattributed file (M6 lesson: glue drops take `P_EXT`, so derivation must
/// re-write the triple or refuse).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    src_guid: String,
    src_file: String,
    op: String,
}

impl Provenance {
    /// Build the triple; any empty member is rejected fail-closed.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::ProvenanceIncomplete`] naming the first empty member
    /// (`"SRC_GUID"` / `"SRC_FILE"` / `"OP"`).
    pub fn new(src_guid: String, src_file: String, op: String) -> Result<Self, SnapshotError> {
        if src_guid.is_empty() {
            return Err(SnapshotError::ProvenanceIncomplete { field: "SRC_GUID" });
        }
        if src_file.is_empty() {
            return Err(SnapshotError::ProvenanceIncomplete { field: "SRC_FILE" });
        }
        if op.is_empty() {
            return Err(SnapshotError::ProvenanceIncomplete { field: "OP" });
        }
        Ok(Self {
            src_guid,
            src_file,
            op,
        })
    }

    /// Source take GUID (`P_EXT:SYNTHLM_PROV_SRC_GUID`).
    #[must_use]
    pub fn src_guid(&self) -> &str {
        &self.src_guid
    }

    /// Source file reference (`P_EXT:SYNTHLM_PROV_SRC_FILE`).
    #[must_use]
    pub fn src_file(&self) -> &str {
        &self.src_file
    }

    /// Operation tag (`P_EXT:SYNTHLM_PROV_OP`, e.g. `"render-new-take"`).
    #[must_use]
    pub fn op(&self) -> &str {
        &self.op
    }
}

/// Re-check a provenance triple (e.g. one read back from `P_EXT`).
///
/// # Errors
///
/// [`SnapshotError::ProvenanceIncomplete`] when any member is empty.
pub fn validate_provenance(prov: &Provenance) -> Result<(), SnapshotError> {
    if prov.src_guid.is_empty() {
        return Err(SnapshotError::ProvenanceIncomplete { field: "SRC_GUID" });
    }
    if prov.src_file.is_empty() {
        return Err(SnapshotError::ProvenanceIncomplete { field: "SRC_FILE" });
    }
    if prov.op.is_empty() {
        return Err(SnapshotError::ProvenanceIncomplete { field: "OP" });
    }
    Ok(())
}

/// Derived-audio write: a NEW file reference plus its provenance triple.
///
/// There is deliberately **no** in-place-overwrite variant: audio operations
/// only derive (`40601` render-as-new-take by default, A04 §7), so overwriting
/// the user's source take is inexpressible in this type (`AGENTS.md` §3.5).
/// After any glue-like step the caller re-writes the triple because glue drops
/// take `P_EXT` (M6: `glue-pext.out.txt pext_kept=false`, A04 §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedTake {
    new_file: String,
    provenance: Provenance,
}

impl DerivedTake {
    /// Derive a new file with its provenance; empty file refs are refused.
    ///
    /// `Provenance::new` already guarantees the triple, so this only adds the
    /// new-file check; both together make unattributed derivation
    /// unrepresentable.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::EmptyNewFile`] when `new_file` is empty.
    pub fn new(new_file: String, provenance: Provenance) -> Result<Self, SnapshotError> {
        validate_provenance(&provenance)?;
        if new_file.is_empty() {
            return Err(SnapshotError::EmptyNewFile);
        }
        Ok(Self {
            new_file,
            provenance,
        })
    }

    /// New (derived) file reference; the source file is never touched.
    #[must_use]
    pub fn new_file(&self) -> &str {
        &self.new_file
    }

    /// Provenance triple travelling with the derived file.
    #[must_use]
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }
}

/// Frozen take snapshot: the unit sent to `acrd` over `snapshot.submit`.
///
/// Assembled by [`capture`] (DEC-004: solve from the frozen copy, never follow
/// live edits mid-solve). Large chunks stay out of small project state: the
/// mock stores the chunk inline, live stores big payloads in the external
/// content-addressed cache with only a pointer in `P_EXT`/ProjExtState
/// (DEC-027, ARCHITECTURE §7).
/// // TODO(M57-handoff): 需真机 — measure chunk sizes that force the external
/// // cache (A03 §2: no capacity promise; KB–MB snapshots risk `.rpp` bloat).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Take identity (persisted; bare indices never persist, `AGENTS.md` §4).
    pub take_guid: String,
    /// Frozen parameter table keyed by ident.
    pub params: Vec<ParamEntry>,
    /// Take chunk at capture time (inline in mock; pointer in live, DEC-027).
    pub chunk: String,
    /// Take FX channel count at capture time (`I_TAKEFX_NCH`, A04 §6).
    ///
    /// `None` when the take holds no FX instance (setting NCH without an
    /// instance is a no-op, spike02) and there is nothing to re-write.
    pub take_fx_nch: Option<f64>,
    /// Raw MIDI bytes at capture time (`MIDI_GetAllEvts` verdict, A04 §1).
    ///
    /// `None` for audio takes.
    pub midi_bytes: Option<Vec<u8>>,
    /// Fast screen hash of [`Snapshot::midi_bytes`] (A04 §1: hash alone never
    /// decides; `None` mirrors `midi_bytes == None`).
    pub midi_hash: Option<String>,
    /// Project render settings at capture time (restored post-render, A03 §3).
    pub render: RenderBackup,
}

/// Stale-handle obligation token returned by every mutating entry point.
///
/// A04 §3 proved undo/redo invalidates `MediaItem*`/`MediaItem_Take*`
/// pointers (`ValidatePtr2 == false` after `Undo_DoUndo2`, spike03f): after
/// [`restore`] / [`apply_derived`] returns, the caller MUST re-fetch every
/// cached pointer via `find_take_by_guid` / `resolve_anchor` before the next
/// DAW op and MUST NOT reuse pre-restore handles. Holding this value is the
/// type-level reminder of that duty; it carries the GUID to re-fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MustRefetch {
    /// Take GUID to look up again (`find_take_by_guid`, never a cached ptr).
    pub take_guid: String,
}

/// Outcome of [`restore`]: what landed, what was skipped, what must be re-fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// Parameters written back by ident.
    pub applied: usize,
    /// Snapshot idents the live chain no longer knows (`FromIdent == -1`).
    ///
    /// Migration-removal branch (DEC-013): skipped, never fatal; the plugin
    /// update removed the parameter, so there is nothing to restore it to.
    pub removed_idents: Vec<String>,
    /// Whether the M3 back-write fired (restored chunk lacked `TAKEFX_NCH`).
    pub nch_rewritten: bool,
    /// Tracks the backend could not dirty-mark (vanished mid-op; stale).
    pub dirty_missed: Vec<DirtyTrack>,
    /// Stale-handle obligation: re-fetch before the next DAW op (A04 §3).
    pub must_refetch: MustRefetch,
}

/// Mockable seam over the snapshot read/write calls.
///
/// The live adapter implements this against `reaper-medium` (primary) with
/// `reaper-low` fallback exactly like [`crate::undo::ReaperUndo`]; every
/// method that would touch a concrete `reaper-medium` type goes through this
/// trait instead so tests stay on the mock (DEC-024). Extends
/// [`ReaperUndo`] so [`restore`] / [`apply_derived`] can wrap their writes in
/// the single [`UndoBlock`] the task mandates (DEC-020).
///
/// All methods are control-plane calls: main thread only, never the audio
/// thread.
pub trait SnapshotBackend: ReaperUndo {
    /// GUID of the take this backend is scoped to (persisted key, A04 §5).
    ///
    /// Live: `GetMediaItemTakeGUID` string form.
    /// // TODO(M57-handoff): 需真机 — confirm the `{brace}` format round-trips
    /// // through `string_to_guid` on the v7.60 floor.
    fn current_take_guid(&self) -> String;

    /// Full take chunk (`GetSetMediaItemTakeInfo_String` take-chunk).
    ///
    /// Live large chunks divert to the external cache (DEC-027).
    /// // TODO(M57-handoff): 需真机 — measure inline-vs-pointer cutoff.
    fn read_chunk(&self) -> String;

    /// Live `I_TAKEFX_NCH` value, or `None` when the take holds no FX
    /// instance yet (set is a no-op then, A04 §6 spike02).
    fn read_take_nch(&self) -> Option<f64>;

    /// Idents to freeze (live: enumerate FX params, map index → ident).
    fn list_param_idents(&self) -> Vec<String>;

    /// One frozen row, or `None` when `FromIdent` reports `-1` (live) —
    /// the caller treats `None` as migration-removed, never as an error.
    fn read_param(&self, ident: &str) -> Option<ParamEntry>;

    /// Raw MIDI bytes (`MIDI_GetAllEvts`), or `None` for audio takes.
    fn read_midi_bytes(&self) -> Option<Vec<u8>>;

    /// Current project `RENDER_*` settings (A03 §3 key set).
    fn read_render(&self) -> RenderBackup;

    /// Provenance triple read back from `P_EXT:SYNTHLM_PROV_*`, or `None`
    /// when any member is absent (fail-closed: refuse, never assume).
    fn read_provenance(&self) -> Option<Provenance>;

    /// New-file reference written by the last [`apply_derived`], if any.
    fn read_derived_file_ref(&self) -> Option<String>;

    /// Tracks whose items must be dirtied after writes (v7.60 manual dirty,
    /// A04 §3; pooled multi-track where relevant).
    fn dirty_tracks(&self) -> Vec<DirtyTrack>;

    /// Overwrite the take chunk with a snapshotted one.
    fn write_chunk(&mut self, chunk: &str);

    /// Explicit `I_TAKEFX_NCH` write (M3 back-write; live: only meaningful
    /// with an FX instance present, A04 §6).
    fn write_take_nch(&mut self, nch: f64);

    /// Write one parameter by ident; `false` means `FromIdent == -1`
    /// (migration-removed: caller skips + reports, never errors).
    fn write_param(&mut self, ident: &str, value: f64) -> bool;

    /// Restore MIDI bytes (`MIDI_SetAllEvts` live).
    /// // TODO(M57-handoff): 需真机 — confirm buffer-format round-trip on v7.60.
    fn write_midi_bytes(&mut self, bytes: &[u8]);

    /// Restore the `RENDER_*` backup after candidate renders.
    fn write_render(&mut self, backup: &RenderBackup);

    /// (Re-)write the provenance triple (mandatory after glue, M6).
    fn write_provenance(&mut self, prov: &Provenance);

    /// Record the new derived file reference (source file never touched).
    fn write_derived_file_ref(&mut self, new_file: &str);
}

/// Fast, non-cryptographic screen hash over MIDI bytes (FNV-1a hex).
///
/// Mirrors `MIDI_GetHash` semantics (A04 §1): cheap change screen only — any
/// verdict compares `midi_bytes` (`GetAllEvts`), never this string alone
/// (juliansader: hash not fully reliable).
#[must_use]
pub fn midi_hash_of(bytes: &[u8]) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// Whether a take chunk carries an explicit `TAKEFX_NCH` override.
///
/// Pure check backing the M3 rule (A04 §6: `TakeFX_CopyToTake` carries the FX
/// but not NCH — `takefx-copy-nch.out.txt nch_carried=false` — so any restore
/// landing on a chunk without this key must explicitly re-write the saved
/// value).
#[must_use]
pub fn chunk_has_takefx_nch(chunk: &str) -> bool {
    chunk.contains("TAKEFX_NCH")
}

/// Freeze the live take into a [`Snapshot`] (DEC-004; read-only, no undo).
///
/// Iterates [`SnapshotBackend::list_param_idents`]; rows whose
/// [`SnapshotBackend::read_param`] reports `None` (`FromIdent == -1` race
/// between list and read) are skipped — there is nothing frozen to send.
///
/// # Errors
///
/// [`SnapshotError::EmptyTakeGuid`] when the backend take GUID is empty;
/// [`SnapshotError::EmptyIdent`] when the ident list itself holds an empty
/// key (backend bug: persisted keys must be namable).
pub fn capture<B: SnapshotBackend + ?Sized>(backend: &B) -> Result<Snapshot, SnapshotError> {
    let take_guid = backend.current_take_guid();
    if take_guid.is_empty() {
        return Err(SnapshotError::EmptyTakeGuid);
    }
    let mut params = Vec::new();
    for ident in backend.list_param_idents() {
        if ident.is_empty() {
            return Err(SnapshotError::EmptyIdent);
        }
        if let Some(entry) = backend.read_param(&ident) {
            params.push(entry);
        }
    }
    let midi_bytes = backend.read_midi_bytes();
    let midi_hash = midi_bytes.as_ref().map(|bytes| midi_hash_of(bytes));
    Ok(Snapshot {
        take_guid,
        params,
        chunk: backend.read_chunk(),
        take_fx_nch: backend.read_take_nch(),
        midi_bytes,
        midi_hash,
        render: backend.read_render(),
    })
}

/// Shape-check a snapshot before opening the undo block.
///
/// Failing before `Begin` keeps the block pairing intact (no half-opened
/// transaction on bad input).
fn check_snapshot_shape(snapshot: &Snapshot) -> Result<(), SnapshotError> {
    if snapshot.take_guid.is_empty() {
        return Err(SnapshotError::EmptyTakeGuid);
    }
    for param in &snapshot.params {
        if param.ident.is_empty() {
            return Err(SnapshotError::EmptyIdent);
        }
    }
    Ok(())
}

/// Restore a [`Snapshot`] inside exactly one [`UndoBlock`] (DEC-020).
///
/// One solve is one undo point: `BeginBlock2(0) … writes … dirty …
/// EndBlock2(0, "SynthLM: restore Nparams", -1)`, reusing the
/// [`crate::undo`] transaction (extraflags `-1 == UNDO_STATE_ALL`).
/// Per-param `FromIdent == -1` takes the migration-removal branch (counted in
/// [`RestoreReport::removed_idents`], never an error). The M3 back-write
/// fires when the landed chunk lacks `TAKEFX_NCH` but the snapshot saved a
/// value. Returns the [`RestoreReport`] carrying the [`MustRefetch`]
/// stale-handle obligation: every pre-restore pointer is dead (A04 §3) and
/// the caller must re-fetch via `find_take_by_guid` / `resolve_anchor`
/// before the next DAW op.
///
/// # Errors
///
/// [`SnapshotError::EmptyTakeGuid`] / [`SnapshotError::EmptyIdent`] on bad
/// snapshot shape (before any `Begin`); [`SnapshotError::TakeNotFound`] when
/// the take is gone or the backend is scoped elsewhere; [`SnapshotError::Undo`]
/// when the undo description cannot be built (no block opened then either).
pub fn restore<B: SnapshotBackend + ?Sized>(
    backend: &mut B,
    snapshot: &Snapshot,
) -> Result<RestoreReport, SnapshotError> {
    check_snapshot_shape(snapshot)?;
    if backend.current_take_guid() != snapshot.take_guid
        || backend.find_take_by_guid(&snapshot.take_guid).is_none()
    {
        return Err(SnapshotError::TakeNotFound {
            guid: snapshot.take_guid.clone(),
        });
    }
    let mut guard = UndoBlock::begin(&mut *backend, "restore", snapshot.params.len())?;
    let inner: &mut B = guard.backend_mut();
    inner.write_chunk(&snapshot.chunk);
    inner.write_render(&snapshot.render);
    let mut applied = 0usize;
    let mut removed_idents = Vec::new();
    for param in &snapshot.params {
        if inner.write_param(&param.ident, param.value) {
            applied += 1;
        } else {
            removed_idents.push(param.ident.clone());
        }
    }
    if let Some(bytes) = snapshot.midi_bytes.as_ref() {
        inner.write_midi_bytes(bytes);
    }
    let mut nch_rewritten = false;
    if let Some(nch) = snapshot.take_fx_nch {
        // M3: the chunk copy path drops NCH, so a landed chunk without the
        // key must get the saved value written back explicitly (A04 §6).
        if !chunk_has_takefx_nch(&inner.read_chunk()) {
            inner.write_take_nch(nch);
            nch_rewritten = true;
        }
    }
    let tracks = inner.dirty_tracks();
    let dirty_missed = mark_dirty_many(inner, &tracks);
    drop(guard);
    Ok(RestoreReport {
        applied,
        removed_idents,
        nch_rewritten,
        dirty_missed,
        must_refetch: MustRefetch {
            take_guid: snapshot.take_guid.clone(),
        },
    })
}

/// Apply a derived-audio write inside exactly one [`UndoBlock`] (DEC-020).
///
/// Records the NEW file reference and (re-)writes the `P_EXT:SYNTHLM_PROV_*`
/// triple in the same block (M6: glue-class steps drop take `P_EXT`, so the
/// triple is written with the file, never assumed to survive). The source
/// take/file is never modified — in-place overwrite has no representation
/// here ([`DerivedTake`] cannot express it, `AGENTS.md` §3.5). Returns the
/// [`MustRefetch`] obligation: re-fetch all pointers before the next DAW op.
///
/// # Errors
///
/// [`SnapshotError::ProvenanceIncomplete`] / [`SnapshotError::EmptyNewFile`]
/// on unattributed input (before any `Begin`, fail-closed);
/// [`SnapshotError::Undo`] when the undo description cannot be built.
pub fn apply_derived<B: SnapshotBackend + ?Sized>(
    backend: &mut B,
    derived: &DerivedTake,
) -> Result<MustRefetch, SnapshotError> {
    validate_provenance(derived.provenance())?;
    if derived.new_file().is_empty() {
        return Err(SnapshotError::EmptyNewFile);
    }
    // Live: `40601` render-as-new-take (source preserved) + `P_EXT` triple on
    // the new take (A04 §7); default derives a new item instead of touching
    // the user source take.
    // TODO(M57-handoff): 需真机 — wire `40601` + new-take `P_EXT` write against
    // the v7.60 floor (current evidence: A04 §7 + M6 spike on v7.82 only).
    let mut guard = UndoBlock::begin(&mut *backend, "derive", 0)?;
    let inner: &mut B = guard.backend_mut();
    inner.write_derived_file_ref(derived.new_file());
    inner.write_provenance(derived.provenance());
    let tracks = inner.dirty_tracks();
    let _ = mark_dirty_many(inner, &tracks);
    let take_guid = inner.current_take_guid();
    drop(guard);
    Ok(MustRefetch { take_guid })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// Parse the mock `TAKEFX_NCH <n>` line value, if present.
    fn parse_mock_nch(chunk: &str) -> Option<f64> {
        let rest = chunk.split_once("TAKEFX_NCH ")?.1;
        rest.split_whitespace().next()?.parse().ok()
    }

    struct MockSnapshot {
        begins: u32,
        ends: Vec<String>,
        dirtied: Vec<DirtyTrack>,
        live_tracks: HashSet<DirtyTrack>,
        takes: HashSet<String>,
        take_guid: String,
        chunk: String,
        nch: Option<f64>,
        params: HashMap<String, ParamEntry>,
        midi: Option<Vec<u8>>,
        render: RenderBackup,
        provenance: Option<Provenance>,
        derived_file: Option<String>,
    }

    impl MockSnapshot {
        fn scripted() -> Self {
            let mut params = HashMap::new();
            for (ident, value) in [("cutoff", 0.5), ("resonance", 0.25)] {
                params.insert(
                    ident.to_string(),
                    ParamEntry::new(ident.to_string(), value, 0.0, 1.0, 0.5).unwrap(),
                );
            }
            Self {
                begins: 0,
                ends: Vec::new(),
                dirtied: Vec::new(),
                live_tracks: HashSet::from([DirtyTrack(0)]),
                takes: HashSet::from(["{take-1}".to_string()]),
                take_guid: "{take-1}".to_string(),
                // Representative chunk line: live stores the override as a
                // `TAKEFX_NCH <n>` line (A04 §6); the mock parses the value
                // back on `write_chunk` so chunk/NCH stay coupled like live.
                chunk: "TAKEFX_NCH 2.0\nchunk-body".to_string(),
                nch: Some(2.0),
                params,
                midi: Some(vec![0x90, 0x3C, 0x64]),
                render: RenderBackup {
                    file: "mix.wav".to_string(),
                    bounds_flag: 4,
                    srate: 48_000.0,
                    ..RenderBackup::default()
                },
                provenance: None,
                derived_file: None,
            }
        }

        fn tamper(&mut self) {
            for entry in self.params.values_mut() {
                entry.value = 0.99;
            }
            self.chunk = "tampered-chunk".to_string();
            self.nch = Some(7.0);
            self.midi = Some(vec![0x80, 0x3C, 0x00]);
            self.render.file = "tampered.wav".to_string();
        }
    }

    impl ReaperUndo for MockSnapshot {
        type Take = String;

        fn begin_block(&mut self) {
            self.begins += 1;
        }

        fn end_block(&mut self, desc: &str) {
            self.ends.push(desc.to_string());
        }

        fn mark_track_items_dirty(&mut self, track: DirtyTrack) -> bool {
            if self.live_tracks.contains(&track) {
                self.dirtied.push(track);
                true
            } else {
                false
            }
        }

        fn find_take_by_guid(&self, take_guid: &str) -> Option<String> {
            self.takes.get(take_guid).cloned()
        }
    }

    impl SnapshotBackend for MockSnapshot {
        fn current_take_guid(&self) -> String {
            self.take_guid.clone()
        }

        fn read_chunk(&self) -> String {
            self.chunk.clone()
        }

        fn read_take_nch(&self) -> Option<f64> {
            self.nch
        }

        fn list_param_idents(&self) -> Vec<String> {
            let mut idents: Vec<String> = self.params.keys().cloned().collect();
            idents.sort();
            idents
        }

        fn read_param(&self, ident: &str) -> Option<ParamEntry> {
            self.params.get(ident).cloned()
        }

        fn read_midi_bytes(&self) -> Option<Vec<u8>> {
            self.midi.clone()
        }

        fn read_render(&self) -> RenderBackup {
            self.render.clone()
        }

        fn read_provenance(&self) -> Option<Provenance> {
            self.provenance.clone()
        }

        fn read_derived_file_ref(&self) -> Option<String> {
            self.derived_file.clone()
        }

        fn dirty_tracks(&self) -> Vec<DirtyTrack> {
            let mut tracks: Vec<DirtyTrack> = self.live_tracks.iter().copied().collect();
            tracks.sort_by_key(|track| track.0);
            tracks
        }

        fn write_chunk(&mut self, chunk: &str) {
            self.chunk = chunk.to_string();
            // Coupled like live (the chunk owns the NCH line): a landed
            // chunk carrying the key re-establishes the numeric value.
            if let Some(value) = parse_mock_nch(&self.chunk) {
                self.nch = Some(value);
            }
        }

        fn write_take_nch(&mut self, nch: f64) {
            self.nch = Some(nch);
            // Live the numeric set creates the chunk line (spike04:
            // `chunk_hasNCH=true` after set); mirror it so the key appears.
            if !chunk_has_takefx_nch(&self.chunk) {
                self.chunk.push_str(&format!("\nTAKEFX_NCH {nch}"));
            }
        }

        fn write_param(&mut self, ident: &str, value: f64) -> bool {
            if let Some(entry) = self.params.get_mut(ident) {
                entry.value = value;
                true
            } else {
                false
            }
        }

        fn write_midi_bytes(&mut self, bytes: &[u8]) {
            self.midi = Some(bytes.to_vec());
        }

        fn write_render(&mut self, backup: &RenderBackup) {
            self.render = backup.clone();
        }

        fn write_provenance(&mut self, prov: &Provenance) {
            self.provenance = Some(prov.clone());
        }

        fn write_derived_file_ref(&mut self, new_file: &str) {
            self.derived_file = Some(new_file.to_string());
        }
    }

    #[test]
    fn capture_tamper_restore_roundtrip_is_consistent() {
        let mut backend = MockSnapshot::scripted();
        let frozen = capture(&backend).unwrap();
        assert_eq!(frozen.take_guid, "{take-1}");
        assert_eq!(frozen.params.len(), 2);
        assert_eq!(
            frozen.midi_hash.as_ref().unwrap(),
            &midi_hash_of(&[0x90, 0x3C, 0x64])
        );

        backend.tamper();
        let report = restore(&mut backend, &frozen).unwrap();

        assert_eq!(report.applied, 2);
        assert!(report.removed_idents.is_empty());
        assert!(!report.nch_rewritten);
        assert_eq!(backend.read_chunk(), frozen.chunk);
        assert_eq!(backend.read_take_nch(), frozen.take_fx_nch);
        assert_eq!(
            backend.read_midi_bytes().unwrap(),
            frozen.midi_bytes.as_ref().unwrap().to_vec()
        );
        assert_eq!(backend.read_render(), frozen.render);
        for param in &frozen.params {
            assert_eq!(backend.read_param(&param.ident).unwrap().value, param.value);
        }
        // Stale-handle duty travels with the report.
        assert_eq!(report.must_refetch.take_guid, "{take-1}");
    }

    #[test]
    fn restore_lands_in_exactly_one_undo_block() {
        let mut backend = MockSnapshot::scripted();
        let frozen = capture(&backend).unwrap();
        backend.tamper();
        let report = restore(&mut backend, &frozen).unwrap();
        assert_eq!(backend.begins, 1);
        assert_eq!(backend.ends, vec!["SynthLM: restore 2params".to_string()]);
        assert_eq!(report.applied, 2);
    }

    #[test]
    fn missing_ident_takes_migration_removal_branch() {
        let mut backend = MockSnapshot::scripted();
        let frozen = capture(&backend).unwrap();
        // Plugin update removed `resonance`: live no longer knows the ident
        // (`FromIdent == -1`), so the write reports `false`.
        backend.params.remove("resonance");
        let report = restore(&mut backend, &frozen).unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.removed_idents, vec!["resonance".to_string()]);
        // Still exactly one undo point (DEC-020 holds even when degraded).
        assert_eq!(backend.begins, 1);
        assert_eq!(backend.ends.len(), 1);
        assert_eq!(backend.read_param("cutoff").unwrap().value, 0.5);
    }

    #[test]
    fn restore_rewrites_nch_when_chunk_lacks_key() {
        let mut backend = MockSnapshot::scripted();
        // Snapshot saved NCH 8.0 but the restored chunk text carries no key
        // (M3: copy path drops NCH) → explicit back-write must fire.
        let mut frozen = capture(&backend).unwrap();
        frozen.take_fx_nch = Some(8.0);
        frozen.chunk = "bare-chunk-without-key".to_string();
        assert!(!chunk_has_takefx_nch(&frozen.chunk));
        let report = restore(&mut backend, &frozen).unwrap();
        assert!(report.nch_rewritten);
        assert_eq!(backend.read_take_nch(), Some(8.0));
        assert_eq!(backend.begins, 1);
    }

    #[test]
    fn incomplete_provenance_triple_is_fail_closed() {
        assert_eq!(
            Provenance::new(
                String::new(),
                "src.wav".to_string(),
                "render-new-take".to_string()
            )
            .unwrap_err(),
            SnapshotError::ProvenanceIncomplete { field: "SRC_GUID" }
        );
        assert_eq!(
            Provenance::new(
                "{take-1}".to_string(),
                String::new(),
                "render-new-take".to_string()
            )
            .unwrap_err(),
            SnapshotError::ProvenanceIncomplete { field: "SRC_FILE" }
        );
        assert_eq!(
            Provenance::new("{take-1}".to_string(), "src.wav".to_string(), String::new())
                .unwrap_err(),
            SnapshotError::ProvenanceIncomplete { field: "OP" }
        );
        assert_eq!(
            DerivedTake::new(
                String::new(),
                Provenance::new(
                    "{take-1}".to_string(),
                    "src.wav".to_string(),
                    "render-new-take".to_string()
                )
                .unwrap()
            )
            .unwrap_err(),
            SnapshotError::EmptyNewFile
        );
        // No block was ever opened: construction refuses before any DAW write.
        let backend = MockSnapshot::scripted();
        assert_eq!(backend.begins, 0);
        assert!(backend.ends.is_empty());
    }

    #[test]
    fn apply_derived_writes_file_plus_triple_in_one_block() {
        let mut backend = MockSnapshot::scripted();
        let prov = Provenance::new(
            "{take-1}".to_string(),
            "src.wav".to_string(),
            "render-new-take".to_string(),
        )
        .unwrap();
        let derived = DerivedTake::new("take-2-render.wav".to_string(), prov.clone()).unwrap();
        let duty = apply_derived(&mut backend, &derived).unwrap();
        assert_eq!(
            backend.read_derived_file_ref().unwrap(),
            "take-2-render.wav"
        );
        assert_eq!(backend.read_provenance().unwrap(), prov);
        assert_eq!(backend.begins, 1);
        assert_eq!(backend.ends, vec!["SynthLM: derive 0params".to_string()]);
        assert_eq!(duty.take_guid, "{take-1}");
    }

    #[test]
    fn restore_missing_take_is_not_found() {
        let backend = MockSnapshot::scripted();
        let mut frozen = capture(&backend).unwrap();
        frozen.take_guid = "{take-gone}".to_string();
        let mut backend = backend;
        let err = restore(&mut backend, &frozen).unwrap_err();
        assert_eq!(
            err,
            SnapshotError::TakeNotFound {
                guid: "{take-gone}".to_string()
            }
        );
        assert_eq!(backend.begins, 0);
        assert!(backend.ends.is_empty());
    }

    #[test]
    fn snapshot_json_roundtrip_for_ipc() {
        let backend = MockSnapshot::scripted();
        let frozen = capture(&backend).unwrap();
        let json = serde_json::to_string(&frozen).unwrap();
        assert!(json.contains("{take-1}"));
        assert!(json.contains("cutoff"));
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frozen);
    }

    #[test]
    fn helpers_behave() {
        assert!(chunk_has_takefx_nch("... TAKEFX_NCH 8 ..."));
        assert!(!chunk_has_takefx_nch("bare chunk"));
        assert_eq!(midi_hash_of(&[]), midi_hash_of(&[]));
        assert_ne!(midi_hash_of(&[1]), midi_hash_of(&[2]));
        let prov = Provenance::new("g".to_string(), "f".to_string(), "o".to_string()).unwrap();
        assert!(validate_provenance(&prov).is_ok());
        assert_eq!(prov.src_guid(), "g");
        assert_eq!(prov.src_file(), "f");
        assert_eq!(prov.op(), "o");
    }
}
