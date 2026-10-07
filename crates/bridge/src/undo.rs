//! Undo transaction guard + dirty marking + take refetch (TSK-102).
//!
//! Pure-Rust side of DEC-008 ("一求解一 undo 点 + 脏标记 + 重取"): every solve
//! wraps its REAPER writes in one
//! `Undo_BeginBlock2(0) … MarkTrackItemsDirty(逐轨) … Undo_EndBlock2(0, desc, -1)`
//! transaction (`docs/research/A03-undo-state-render.md` §1,
//! `docs/research/A04-items-midi-takes.md` §3), then re-resolves all pointers
//! because undo/redo invalidates them (A04 §3, spike03f).
//!
//! Like [`crate::container_addr`], this layer is split into a mockable seam
//! ([`ReaperUndo`](crate::undo::ReaperUndo), pure helpers) plus a thin live
//! adapter ([`LiveUndo`](crate::undo::LiveUndo)); unit tests run against the mock, never against a live REAPER instance
//! (DEC-024). Control plane only: main thread, never the audio thread
//! (`AGENTS.md` §3.2).
//!
//! # Live mapping (reaper-rs rev `659b22b`, checked 2026-10-06)
//!
//! - `Undo_BeginBlock2` / `Undo_EndBlock2`: `reaper-medium` safe wrappers
//!   (`Reaper::undo_begin_block_2` / `undo_end_block_2`,
//!   `main/medium/src/reaper.rs`). `UndoScope::All` converts to
//!   `UNDO_STATE_ALL as i32 == -1`, i.e. the DEC-008 `extraflags = -1`.
//! - `MarkTrackItemsDirty`: **no** medium wrapper exists (medium only offers
//!   project-dirty: `mark_project_dirty` / `is_project_dirty`); the adapter
//!   goes through `reaper-low` `unsafe` with a per-site safety justification
//!   (`AGENTS.md` §4 unsafe 许可制).
//! - Take-by-GUID: **no** medium wrapper; parse via medium `string_to_guid`
//!   (safe, `MainThreadOnly`) then `reaper-low` `GetMediaItemTakeByGUID`.
//! - Project for every `*2` call: `ProjectContext::CurrentProject` (raw null =
//!   current tab, A03 §1).

use std::collections::HashSet;

use thiserror::Error;

/// Transient 0-based track index used for dirty marking.
///
/// Resolved fresh from the live track list on every operation and never
/// persisted: `AGENTS.md` §4 forbids persisting bare indices (persist
/// TrackGUID/FXGUID/ident instead); this type only travels from a fresh
/// resolve to the immediately following `MarkTrackItemsDirty` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirtyTrack(pub u32);

/// Library-boundary error type (`thiserror`, per `AGENTS.md` §4).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UndoError {
    /// [`undo_description`] / [`UndoBlock::begin`] got an empty op id.
    #[error("empty undo op id: descriptions require a non-empty op name")]
    EmptyOp,
}

/// Mockable seam over the undo/dirty/take-lookup calls (cf.
/// [`crate::container_addr::ReaperFxChain`]).
///
/// The TSK-102 adapter ([`LiveUndo`]) implements this against
/// `reaper-medium` (primary) with `reaper-low` fallback for the
/// `MarkTrackItemsDirty` / take-by-GUID gaps. All methods are control-plane
/// calls: main thread only, never the audio thread.
pub trait ReaperUndo {
    /// Live take handle returned by [`ReaperUndo::find_take_by_guid`].
    ///
    /// The mock uses a plain id; [`LiveUndo`] uses
    /// `reaper_medium::MediaItemTake`. Callers must re-fetch after every
    /// undo/redo instead of caching this (A04 §3: pointers invalidate).
    type Take;

    /// `Undo_BeginBlock2(0)` — opens a block in the current tab (A03 §1).
    fn begin_block(&mut self);

    /// `Undo_EndBlock2(0, desc, -1)` — closes with `UNDO_STATE_ALL` (DEC-008).
    fn end_block(&mut self, desc: &str);

    /// `MarkTrackItemsDirty` for one track (pooled: call once per track,
    /// A04 §3).
    ///
    /// Returns `false` when `track` no longer resolves (chain mutated under
    /// us); the caller treats that as stale and re-resolves instead of
    /// assuming the mark landed.
    fn mark_track_items_dirty(&mut self, track: DirtyTrack) -> bool;

    /// Re-finds a take by GUID without touching stale pointers (A04 §3).
    ///
    /// Returns `None` when the take is gone (or the GUID string is invalid);
    /// callers treat `None` as stale and re-resolve via a fresh item/take
    /// scan, never by reusing a cached pointer.
    fn find_take_by_guid(&self, take_guid: &str) -> Option<Self::Take>;
}

/// Builds the stable undo-point description `SynthLM: <op> <n>params`
/// (DEC-008).
///
/// # Errors
///
/// [`UndoError::EmptyOp`] when `op` is empty.
pub fn undo_description(op: &str, param_count: usize) -> Result<String, UndoError> {
    if op.is_empty() {
        return Err(UndoError::EmptyOp);
    }
    Ok(format!("SynthLM: {op} {param_count}params"))
}

/// RAII undo transaction: construction issues `BeginBlock2(0)`, `Drop` issues
/// `EndBlock2(0, desc, -1)`, so an early return or a panic can never leave a
/// `Begin` unpaired (A03 §1: Begin/End 必须配对).
pub struct UndoBlock<'a, B: ReaperUndo + ?Sized> {
    backend: &'a mut B,
    desc: String,
}

impl<'a, B: ReaperUndo + ?Sized> UndoBlock<'a, B> {
    /// Opens a block: validates the `SynthLM: <op> <n>params` description
    /// first, then issues `BeginBlock2(0)`.
    ///
    /// Validating before the `Begin` means failure never leaves an unpaired
    /// block behind.
    ///
    /// # Errors
    ///
    /// [`UndoError::EmptyOp`] when `op` is empty (no `Begin` issued).
    pub fn begin(backend: &'a mut B, op: &str, param_count: usize) -> Result<Self, UndoError> {
        let desc = undo_description(op, param_count)?;
        backend.begin_block();
        Ok(Self { backend, desc })
    }

    /// The description that `Drop` will hand to `EndBlock2`.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.desc
    }

    /// Exclusive access to the wrapped backend for writes inside the block.
    ///
    /// Lets [`crate::snapshot`] `restore` / `apply_derived`
    /// perform their DAW writes between the paired `BeginBlock2` (issued by
    /// [`UndoBlock::begin`]) and the `EndBlock2` (issued by `Drop`),
    /// preserving the single-undo-point guarantee (DEC-008/020) without
    /// manual begin/end pairing.
    pub fn backend_mut(&mut self) -> &mut B {
        self.backend
    }
}

impl<B: ReaperUndo + ?Sized> Drop for UndoBlock<'_, B> {
    fn drop(&mut self) {
        self.backend.end_block(&self.desc);
    }
}

impl<B: ReaperUndo + ?Sized> std::fmt::Debug for UndoBlock<'_, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UndoBlock")
            .field("desc", &self.desc)
            .finish()
    }
}

/// Order-preserving dedupe of the dirty-track list.
///
/// Pooled writes (A04 §3) naturally list the same track twice; marking it
/// twice is pure undo/refresh noise, so collapse before calling out.
#[must_use]
pub fn dedupe_dirty_tracks(tracks: &[DirtyTrack]) -> Vec<DirtyTrack> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(tracks.len());
    for &track in tracks {
        if seen.insert(track) {
            out.push(track);
        }
    }
    out
}

/// Marks every listed track dirty exactly once (deduped).
///
/// Returns the tracks the backend could **not** mark (vanished mid-op);
/// callers treat a non-empty return as stale and re-resolve rather than
/// assuming the marks landed. Empty means every mark landed.
#[must_use]
pub fn mark_dirty_many<B: ReaperUndo + ?Sized>(
    backend: &mut B,
    tracks: &[DirtyTrack],
) -> Vec<DirtyTrack> {
    let mut missed = Vec::new();
    for track in dedupe_dirty_tracks(tracks) {
        if !backend.mark_track_items_dirty(track) {
            missed.push(track);
        }
    }
    missed
}

/// Re-finds a take by GUID after a mutation/undo that may have invalidated
/// pointers (A04 §3: `ValidatePtr2` goes `false` after `Undo_DoUndo2`).
///
/// `None` (take gone or GUID invalid) means stale: the caller must re-scan
/// (`CountMediaItems` + `GetMediaItem` + `GetActiveTake` pattern) instead of
/// reusing any cached pointer.
pub fn refetch_take<B: ReaperUndo + ?Sized>(backend: &B, take_guid: &str) -> Option<B::Take> {
    backend.find_take_by_guid(take_guid)
}

/// Live main-thread backend: `medium` for undo, `low` for dirty/take-lookup.
///
/// Build from `ReaperSession::reaper()` (a `&Reaper<MainThreadScope>`); every
/// method must run on the REAPER main thread. `unsafe` appears only in the
/// `low` calls below, each with its own justification (`AGENTS.md` §4).
pub struct LiveUndo<'a> {
    reaper: &'a reaper_medium::Reaper<reaper_medium::MainThreadScope>,
}

impl<'a> LiveUndo<'a> {
    /// Wraps a main-thread medium session handle (adapter 起点, TSK-102).
    #[must_use]
    pub fn new(reaper: &'a reaper_medium::Reaper<reaper_medium::MainThreadScope>) -> Self {
        Self { reaper }
    }
}

impl ReaperUndo for LiveUndo<'_> {
    type Take = reaper_medium::MediaItemTake;

    fn begin_block(&mut self) {
        use reaper_medium::ProjectContext::CurrentProject;
        self.reaper.undo_begin_block_2(CurrentProject);
    }

    fn end_block(&mut self, desc: &str) {
        use reaper_medium::{ProjectContext::CurrentProject, UndoScope};
        // `UndoScope::All.to_raw()` is `UNDO_STATE_ALL as i32 == -1`
        // (`main/low/src/bindings.rs`: `UNDO_STATE_ALL = 4294967295`),
        // i.e. exactly the DEC-008 `extraflags = -1`.
        self.reaper
            .undo_end_block_2(CurrentProject, desc, UndoScope::All);
    }

    fn mark_track_items_dirty(&mut self, track: DirtyTrack) -> bool {
        use reaper_medium::ProjectContext::CurrentProject;
        let Some(medium_track) = self.reaper.get_track(CurrentProject, track.0) else {
            return false;
        };
        // SAFETY (bridge 内 low 封装, AGENTS.md §4):
        // - Main thread: `LiveUndo` only wraps `&Reaper<MainThreadScope>`,
        //   and `get_track` above already ran `require_main_thread`
        //   (it panics off-thread instead of reaching this call).
        // - Track pointer: freshly returned by medium `get_track`, valid for
        //   this call. Caller obligation: never cache it across undo/redo —
        //   pointers invalidate then and must be re-resolved (A04 §3).
        // - `item = null`: asks REAPER to mark the whole track's items dirty.
        //   TODO(M57-handoff): 需真机 — confirm a null item marks all items of
        //   the track on the v7.60 floor and v7.82 (vs requiring a per-item
        //   loop); until proven, MIDI writes keep passing an explicit item
        //   where one is at hand.
        unsafe {
            self.reaper
                .low()
                .MarkTrackItemsDirty(medium_track.as_ptr(), std::ptr::null_mut());
        }
        true
    }

    fn find_take_by_guid(&self, take_guid: &str) -> Option<Self::Take> {
        // Medium has no take-by-GUID wrapper: parse via medium (safe,
        // `MainThreadOnly`, `Err` on invalid strings) then look up via low.
        let guid: reaper_low::raw::GUID = self.reaper.string_to_guid(take_guid).ok()?;
        // SAFETY (bridge 内 low 封装, AGENTS.md §4):
        // - Main thread: same argument as above (`string_to_guid` ran
        //   `require_main_thread` first).
        // - `proj = null` means the current tab, matching
        //   `ProjectContext::CurrentProject.to_raw()`.
        // - `&guid` outlives the call; a null return (take gone) maps to
        //   `None` via `MediaItemTake::new` and is never dereferenced.
        // TODO(M57-handoff): 需真机 — verify the `{xyz-...}` brace format
        // round-trips through `string_to_guid` on the v7.60 floor (current
        // spike evidence is v7.82 only).
        let ptr = unsafe {
            self.reaper
                .low()
                .GetMediaItemTakeByGUID(std::ptr::null_mut(), &guid)
        };
        reaper_medium::MediaItemTake::new(ptr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::panic::AssertUnwindSafe;

    /// Mock backend: records begin/end/dirty calls, serves takes from a set.
    struct MockUndo {
        begins: u32,
        ends: Vec<String>,
        dirtied: Vec<DirtyTrack>,
        takes: HashSet<String>,
        live_tracks: HashSet<DirtyTrack>,
    }

    impl MockUndo {
        fn new(live_tracks: &[DirtyTrack], takes: &[&str]) -> Self {
            Self {
                begins: 0,
                ends: Vec::new(),
                dirtied: Vec::new(),
                takes: takes.iter().map(|s| (*s).to_string()).collect(),
                live_tracks: live_tracks.iter().copied().collect(),
            }
        }
    }

    impl ReaperUndo for MockUndo {
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

    #[test]
    fn desc_format_is_stable() {
        assert_eq!(
            undo_description("apply", 12).unwrap(),
            "SynthLM: apply 12params"
        );
        assert_eq!(
            undo_description("preview", 0).unwrap(),
            "SynthLM: preview 0params"
        );
        assert_eq!(undo_description("", 3).unwrap_err(), UndoError::EmptyOp);
    }

    #[test]
    fn guard_pairs_begin_end_on_normal_exit() {
        let mut backend = MockUndo::new(&[], &[]);
        let guard = UndoBlock::begin(&mut backend, "apply", 3).unwrap();
        assert_eq!(guard.description(), "SynthLM: apply 3params");
        drop(guard);
        assert_eq!(backend.begins, 1);
        assert_eq!(backend.ends, vec!["SynthLM: apply 3params".to_string()]);
    }

    #[test]
    fn guard_pairs_begin_end_on_early_return() {
        fn run(backend: &mut MockUndo, bail: bool) {
            {
                let _guard = UndoBlock::begin(&mut *backend, "apply", 1).unwrap();
                if bail {
                    return;
                }
            }
            backend.mark_track_items_dirty(DirtyTrack(0));
        }
        let mut backend = MockUndo::new(&[DirtyTrack(0)], &[]);
        run(&mut backend, true);
        assert_eq!(backend.begins, 1);
        assert_eq!(backend.ends.len(), 1);
        assert!(backend.dirtied.is_empty());
        run(&mut backend, false);
        assert_eq!(backend.begins, 2);
        assert_eq!(backend.ends.len(), 2);
        assert_eq!(backend.dirtied, vec![DirtyTrack(0)]);
    }

    #[test]
    fn guard_pairs_begin_end_on_panic() {
        let mut backend = MockUndo::new(&[], &[]);
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = UndoBlock::begin(&mut backend, "apply", 2).unwrap();
            panic!("simulated solve failure");
        }));
        assert!(outcome.is_err());
        // `Drop` ran during unwinding, so the block is still closed exactly once.
        assert_eq!(backend.begins, 1);
        assert_eq!(backend.ends, vec!["SynthLM: apply 2params".to_string()]);
    }

    #[test]
    fn failed_begin_leaves_no_unpaired_block() {
        let mut backend = MockUndo::new(&[], &[]);
        let err = UndoBlock::begin(&mut backend, "", 2).unwrap_err();
        assert_eq!(err, UndoError::EmptyOp);
        assert_eq!(backend.begins, 0);
        assert!(backend.ends.is_empty());
    }

    #[test]
    fn dirty_list_is_deduped() {
        let tracks = [
            DirtyTrack(0),
            DirtyTrack(1),
            DirtyTrack(0),
            DirtyTrack(2),
            DirtyTrack(1),
        ];
        assert_eq!(
            dedupe_dirty_tracks(&tracks),
            vec![DirtyTrack(0), DirtyTrack(1), DirtyTrack(2)]
        );
        assert!(dedupe_dirty_tracks(&[]).is_empty());
    }

    #[test]
    fn mark_dirty_many_marks_once_and_reports_missed() {
        let mut backend = MockUndo::new(&[DirtyTrack(0), DirtyTrack(1)], &[]);
        let missed = mark_dirty_many(
            &mut backend,
            &[DirtyTrack(0), DirtyTrack(1), DirtyTrack(0), DirtyTrack(9)],
        );
        // Each live track marked exactly once despite the duplicate input.
        assert_eq!(backend.dirtied, vec![DirtyTrack(0), DirtyTrack(1)]);
        // The vanished track is reported, not silently dropped.
        assert_eq!(missed, vec![DirtyTrack(9)]);
    }

    #[test]
    fn refetch_hit_returns_handle_miss_returns_none() {
        let backend = MockUndo::new(&[], &["{take-guid-1}"]);
        assert_eq!(
            refetch_take(&backend, "{take-guid-1}"),
            Some("{take-guid-1}".to_string())
        );
        // Take gone (e.g. after undo re-created the item): caller sees stale.
        assert_eq!(refetch_take(&backend, "{take-guid-gone}"), None);
    }
}
