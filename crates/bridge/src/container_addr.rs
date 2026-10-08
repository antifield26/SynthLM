//! FX container address encode/decode + GUID anchoring + flatten fallback (TSK-101).
//!
//! This is the pure-Rust layer of the container address recomputation strategy
//! (DEC-001/DEC-003, `ARCHITECTURE.md` §2): REAPER stays the source of truth,
//! our side only generates addresses. Bare FX indices are never persisted
//! (`AGENTS.md` §4 naming rule); callers persist [`FxAnchor`](crate::container_addr::FxAnchor) and recompute via
//! [`resolve_anchor`](crate::container_addr::resolve_anchor) / [`verify_fresh`](crate::container_addr::verify_fresh) before every DAW operation.
//!
//! # Addressing scheme (Justin convention, 1-based positions)
//!
//! Container paths use **1-based** positions at every level, exactly as in
//! Justin's `get_fx_id_from_container_path` / `get_container_path_from_fx_id`
//! (`https://forum.cockos.com/showthread.php?t=284400`, verified via
//! `docs/research/A02-containers-routing.md` §2 on 2026-10-05 and re-verified
//! verbatim 2026-10-06):
//!
//! ```text
//! encode([c1]):            0x2000000 + c1
//! encode([c1, p2, .., pm]): rv = 0x2000000 + c1; sc = top_count + 1
//!                           per deeper level v: rv += sc * v; sc *= (cc + 1)
//!                           where cc = container_count(rv) queried live
//! ```
//!
//! Notes for future live-chain work (TSK-102 adapter, main thread only):
//! - Top-level FX *outside* containers keep 0-based bare indices
//!   (`FxLoc::Top`); only container-space addresses carry the flag.
//! - A `container_count` query failure (`ccok ~= true` in Justin's code, i.e.
//!   target is not a container or the address is stale) maps to the retryable
//!   [`ContainerError::ContainerCountUnavailable`](crate::container_addr::ContainerError::ContainerCountUnavailable).
//! - `TrackFX_GetFXGUID` stability across save/reload is explicitly
//!   ⚠️需实测 (`docs/research/A01-fx-chain-params.md` §4); a GUID mismatch is
//!   therefore retryable ([`ContainerError::GuidMismatch`](crate::container_addr::ContainerError::GuidMismatch)), never fatal.
//! - Take chains use the `TakeFX_*` twins of the same scheme (Justin posted both);
//!   the live adapter can serve them through the same [`ReaperFxChain`](crate::container_addr::ReaperFxChain) trait.
//! - v7.06+ offers `parent_container` + `container_item.<i>` navigation as an
//!   alternative to hand-computed arithmetic (`A01` §2); the pure decode here
//!   stays arithmetic so encode/decode round-trip without extra chain walks.
//! - GUID relocation after moves mirrors MT4U's `GUID_2_ID` scan-by-GUID idea
//!   (`https://github.com/MT4Mars/MT4U/blob/main/MT4U_FX_Rack_Reaper7/MT4U_FX_Navigator.eel`,
//!   fetched 2026-10-06); the depth cap mirrors its traversal stopper.
//!
//! # Call pattern (recompute before every op)
//!
//! ```
//! use synthlm_bridge::container_addr::{ContainerPath, FxAnchor, FxLoc};
//!
//! // Persist the anchor (GUIDs + 1-based container path), never the bare index.
//! let anchor = FxAnchor::new(
//!     "{track-guid}".to_string(),
//!     "{fx-guid}".to_string(),
//!     FxLoc::Nested(ContainerPath::new(vec![2, 1]).unwrap()),
//! )
//! .unwrap();
//! assert!(anchor.loc.is_nested());
//! ```
//!
//! Live resolution needs a [`ReaperFxChain`](crate::container_addr::ReaperFxChain) (mocked in tests, wired to
//! `reaper-medium`/`reaper-low` in TSK-102): first [`resolve_anchor`](crate::container_addr::resolve_anchor), then
//! [`verify_fresh`](crate::container_addr::verify_fresh) with the cached address before every subsequent DAW op.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Flag bit marking container-space FX addresses (`0x2000000`).
///
/// Present on every address produced by [`crate::container_addr::encode_path`]; tested with a bit-test
/// in [`crate::container_addr::decode_address`], mirroring Justin's `fxidx & 0x2000000`.
pub const CONTAINER_FLAG: i32 = 0x2000000;

/// Flag bit for record-input / monitoring FX (`0x1000000`, `A01` §1).
///
/// Recognised only to reject: input-FX-inside-container combinations are
/// orthogonal to this layer (`A02` §3) and left for a later task.
pub const INPUT_FX_FLAG: i32 = 0x1000000;

/// Maximum container nesting depth accepted by [`crate::container_addr::decode_address`].
///
/// Precedent: MT4U's traversal stopper of 30 (see module docs); mpl's scripts
/// unrolled 10 levels (`A02` §2). Deeper chains report
/// [`ContainerError::DepthExceeded`] so the caller can fall back to
/// [`crate::container_addr::flatten_fallback`] (DEC-003) instead of looping forever.
pub const MAX_CONTAINER_DEPTH: usize = 32;

/// Budget of re-resolve attempts handed out by [`crate::container_addr::flatten_fallback`].
///
/// The pure layer only classifies; the executor decrements and, at zero,
/// proceeds to flatten. Actual track+sends materialisation needs main-thread
/// REAPER handles (TSK-102+), hence stub status per DEC-003.
pub const MAX_RESOLVE_RETRIES: u8 = 3;

/// Mockable seam over the live REAPER FX chain (one track).
///
/// The TSK-102 adapter implements this against `reaper-medium` (primary) with
/// `reaper-low` fallback for the Take-side / raw-value gaps (`A01` §6).
/// Object-safe so tests and callers can use `&dyn ReaperFxChain`.
///
/// All methods are control-plane queries: main thread only, never the audio
/// thread (`ARCHITECTURE.md` §2/§4).
pub trait ReaperFxChain {
    /// Live top-level FX count (`TrackFX_GetCount`; containers occupy one slot).
    fn top_fx_count(&self) -> u32;

    /// Children inside the container at container-space `addr`
    /// (`TrackFX_GetNamedConfigParm "container_count"`).
    ///
    /// Must fail when `addr` is not a container or the key is unavailable;
    /// [`crate::container_addr::encode_path`]/[`crate::container_addr::decode_address`] normalise any failure to the
    /// retryable [`ContainerError::ContainerCountUnavailable`].
    fn container_child_count(&self, addr: i32) -> Result<u32, ContainerError>;

    /// Identity of the FX at `addr` (`TrackFX_GetFXGUID`).
    ///
    /// Adapter contract: chain-state failures (FX vanished, address stale)
    /// must surface as retryable errors; only caller bugs may be
    /// non-retryable.
    fn fx_guid(&self, addr: i32) -> Result<String, ContainerError>;

    /// Owning track identity, stored into [`FxAnchor::track_guid`] for routing.
    fn track_guid(&self) -> String;
}

/// Library-boundary error type (`thiserror`, per `AGENTS.md` §4).
///
/// Retryability follows `ARCHITECTURE.md` §8: chain-mutation signals
/// (`retryable() == true`) mean "re-scan and recompute"; structural caller
/// bugs (`false`) mean "fix the caller" (BLOCKED-class, never silent).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContainerError {
    /// [`crate::container_addr::ContainerPath`] built from an empty position list.
    #[error("empty container path: at least the 1-based top position is required")]
    EmptyPath,

    /// Zero position in a 1-based path (`level` = 0-based index into the path).
    #[error("position 0 at path level {level}: container paths are 1-based (Justin convention)")]
    ZeroPosition {
        /// 0-based index into the path holding the zero.
        level: usize,
    },

    /// Empty GUID string supplied to [`FxAnchor::new`] (`field` names the field).
    #[error("empty {field}: anchors require non-empty GUID strings")]
    EmptyGuidId {
        /// Either `"track_guid"` or `"fx_guid"`.
        field: &'static str,
    },

    /// Top position outside `1..=top_count` (1-based; chain mutated or wrong track).
    #[error("top position {position} out of range for {top_count} top-level FX")]
    TopPositionOutOfRange {
        /// 1-based top position requested.
        position: u32,
        /// Live top-level FX count.
        top_count: u32,
    },

    /// Bare 0-based top index at/over the live top count (index drift, `A01` §4).
    #[error("top index {index} out of range for {top_count} top-level FX")]
    TopIndexOutOfRange {
        /// 0-based top index requested.
        index: u32,
        /// Live top-level FX count.
        top_count: u32,
    },

    /// `container_count` query failed at `addr` (not a container / stale address).
    ///
    /// This is the task-required "container_count 校验失败即返回可重试错误".
    #[error(
        "container_count unavailable at {addr:#X} (path level {level}): not a container or stale address"
    )]
    ContainerCountUnavailable {
        /// Container-space address that was queried.
        addr: i32,
        /// 0-based index into the path of the container being descended into.
        level: usize,
    },

    /// Child position outside `1..=child_count`.
    #[error(
        "child position {position} out of range at {addr:#X}: container holds {child_count} children"
    )]
    ChildPositionOutOfRange {
        /// Container-space address of the container.
        addr: i32,
        /// 0-based index into the path of the offending position.
        level: usize,
        /// 1-based child position requested.
        position: u32,
        /// Live child count of the container.
        child_count: u32,
    },

    /// Cached address no longer matches recomputation (stride changed).
    #[error("stale address: cached {cached:#X} but recomputed {recomputed:#X}")]
    StaleAddress {
        /// Address the caller cached from an earlier resolution.
        cached: i32,
        /// Address recomputed from the live chain just now.
        recomputed: i32,
    },

    /// FX at the resolved address is not the anchored one (moved / replaced).
    ///
    /// Caller should relocate by GUID scan (MT4U `GUID_2_ID` precedent) or abort.
    #[error("GUID mismatch at {addr:#X}: anchor expects a different FX")]
    GuidMismatch {
        /// Resolved container-space address.
        addr: i32,
        /// GUID stored in the anchor.
        expected: String,
        /// GUID observed live at `addr`.
        observed: String,
    },

    /// Address cannot be decoded against the live chain (bad residue or chain race).
    #[error("unresolvable address {addr:#X} against the live chain")]
    UnresolvableAddress {
        /// Address that failed decoding.
        addr: i32,
    },

    /// Address uses an unsupported space (negative, bare input-FX flag, ...).
    #[error("unsupported address space for {addr:#X}")]
    UnsupportedAddressSpace {
        /// Offending address.
        addr: i32,
    },

    /// Checked `i32` arithmetic overflow while composing an address.
    #[error("address arithmetic overflow composing container address")]
    AddressOverflow,

    /// Nesting deeper than [`crate::container_addr::MAX_CONTAINER_DEPTH`] while decoding.
    #[error("container nesting deeper than {limit} levels")]
    DepthExceeded {
        /// Depth cap that was hit.
        limit: usize,
    },
}

impl ContainerError {
    /// Whether the caller may usefully re-scan and recompute (`ARCHITECTURE.md` §8).
    ///
    /// `true` = chain mutated under us (top insert/delete, move, undo);
    /// `false` = caller bug or unsupported input (fix input, or flatten).
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            ContainerError::TopPositionOutOfRange { .. }
            | ContainerError::TopIndexOutOfRange { .. }
            | ContainerError::ContainerCountUnavailable { .. }
            | ContainerError::ChildPositionOutOfRange { .. }
            | ContainerError::StaleAddress { .. }
            | ContainerError::GuidMismatch { .. }
            | ContainerError::UnresolvableAddress { .. } => true,
            ContainerError::EmptyPath
            | ContainerError::ZeroPosition { .. }
            | ContainerError::EmptyGuidId { .. }
            | ContainerError::UnsupportedAddressSpace { .. }
            | ContainerError::AddressOverflow
            | ContainerError::DepthExceeded { .. } => false,
        }
    }
}

/// 1-based container path, Justin convention.
///
/// `positions[0]` is the 1-based top slot (`1..=top_count`); each further entry
/// is the 1-based child slot inside the container above. Length 1 addresses the
/// container itself. All entries are `>= 1` by construction; residue `0` is
/// Justin's "bad address" (`curidx < 1 → nil`).
///
/// ```
/// # use synthlm_bridge::container_addr::{ContainerPath, ContainerError};
/// assert!(ContainerPath::new(vec![]).is_err());
/// assert!(matches!(
///     ContainerPath::new(vec![2, 0]),
///     Err(ContainerError::ZeroPosition { level: 1 })
/// ));
/// let path = ContainerPath::new(vec![2, 1]).unwrap();
/// assert_eq!(path.depth(), 2);
/// assert_eq!(path.top_position(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerPath {
    positions: Vec<u32>,
}

impl ContainerPath {
    /// Build a path, enforcing non-empty + all positions `>= 1`.
    ///
    /// # Errors
    ///
    /// [`ContainerError::EmptyPath`] for `vec![]`;
    /// [`ContainerError::ZeroPosition`] for the first zero entry.
    pub fn new(positions: Vec<u32>) -> Result<Self, ContainerError> {
        if positions.is_empty() {
            return Err(ContainerError::EmptyPath);
        }
        let mut zero_level: Option<usize> = None;
        for (level, &position) in positions.iter().enumerate() {
            if position == 0 {
                zero_level = Some(level);
                break;
            }
        }
        if let Some(level) = zero_level {
            return Err(ContainerError::ZeroPosition { level });
        }
        Ok(Self { positions })
    }

    /// 1-based positions, top slot first.
    #[must_use]
    pub fn positions(&self) -> &[u32] {
        &self.positions
    }

    /// Nesting depth = number of positions (`>= 1`).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.positions.len()
    }

    /// 1-based top slot (`positions[0]`; safe by the non-empty invariant).
    #[must_use]
    pub fn top_position(&self) -> u32 {
        self.positions[0]
    }
}

/// Addressable FX location: bare top-level slot or container-space path.
///
/// `Top` uses 0-based bare indices (official `TrackFX_*` convention, `A01` §4);
/// `Nested` uses 1-based Justin paths (forum t=284400). Keeping both bases
/// explicit in the type prevents the classic 0/1-based mix-up at call sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FxLoc {
    /// 0-based bare top-level index (no flag).
    Top {
        /// 0-based index into the top-level chain.
        index: u32,
    },
    /// 1-based container path (flag-composed address).
    Nested(ContainerPath),
}

impl FxLoc {
    /// Whether this location lives inside a container.
    #[must_use]
    pub const fn is_nested(&self) -> bool {
        match self {
            FxLoc::Top { .. } => false,
            FxLoc::Nested(_) => true,
        }
    }
}

/// Persistable anchor: `(TrackGUID, FXGUID, container path)` (DEC-003).
///
/// This — never a bare index — is what callers store in snapshots / `P_EXT`
/// pointers (`AGENTS.md` §4). GUID emptiness is rejected at construction;
/// deeper stability promises are explicitly ⚠️需实测 (`A01` §4), so resolution
/// treats mismatches as retryable, never fatal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxAnchor {
    /// Owning track identity (routes the anchor to the right chain object).
    pub track_guid: String,
    /// FX identity verified at the resolved address on every op.
    pub fx_guid: String,
    /// Where the FX lives (bare top slot or container path).
    pub loc: FxLoc,
}

impl FxAnchor {
    /// Build an anchor; rejects empty GUID strings.
    ///
    /// # Errors
    ///
    /// [`ContainerError::EmptyGuidId`] when either GUID string is empty.
    pub fn new(track_guid: String, fx_guid: String, loc: FxLoc) -> Result<Self, ContainerError> {
        if track_guid.is_empty() {
            return Err(ContainerError::EmptyGuidId {
                field: "track_guid",
            });
        }
        if fx_guid.is_empty() {
            return Err(ContainerError::EmptyGuidId { field: "fx_guid" });
        }
        Ok(Self {
            track_guid,
            fx_guid,
            loc,
        })
    }
}

/// Encode a 1-based container path to a container-space address.
///
/// Faithful port of Justin's `get_fx_id_from_container_path`, plus fail-fast
/// bounds checks (Justin lets out-of-range positions produce garbage that only
/// fails later at use; we return retryable errors instead).
///
/// # Errors
///
/// Retryable: [`ContainerError::TopPositionOutOfRange`],
/// [`ContainerError::ContainerCountUnavailable`],
/// [`ContainerError::ChildPositionOutOfRange`].
/// Non-retryable: [`ContainerError::ZeroPosition`] (defence in depth;
/// [`ContainerPath::new`] already rejects zeros),
/// [`ContainerError::AddressOverflow`].
pub fn encode_path<C: ReaperFxChain + ?Sized>(
    chain: &C,
    path: &ContainerPath,
) -> Result<i32, ContainerError> {
    let positions = path.positions();
    let first = positions[0];
    if first == 0 {
        return Err(ContainerError::ZeroPosition { level: 0 });
    }
    let top_count = chain.top_fx_count();
    if first > top_count {
        return Err(ContainerError::TopPositionOutOfRange {
            position: first,
            top_count,
        });
    }
    let first_i = i32::try_from(first).map_err(|_| ContainerError::AddressOverflow)?;
    let mut addr = CONTAINER_FLAG
        .checked_add(first_i)
        .ok_or(ContainerError::AddressOverflow)?;
    let count_i = i32::try_from(top_count).map_err(|_| ContainerError::AddressOverflow)?;
    let mut stride = count_i
        .checked_add(1)
        .ok_or(ContainerError::AddressOverflow)?;
    for (depth, &position) in positions.iter().enumerate().skip(1) {
        let level = depth - 1;
        let child_count = chain
            .container_child_count(addr)
            .map_err(|_| ContainerError::ContainerCountUnavailable { addr, level })?;
        if position == 0 {
            return Err(ContainerError::ZeroPosition { level: depth });
        }
        if position > child_count {
            return Err(ContainerError::ChildPositionOutOfRange {
                addr,
                level: depth,
                position,
                child_count,
            });
        }
        let position_i = i32::try_from(position).map_err(|_| ContainerError::AddressOverflow)?;
        let step = stride
            .checked_mul(position_i)
            .ok_or(ContainerError::AddressOverflow)?;
        addr = addr
            .checked_add(step)
            .ok_or(ContainerError::AddressOverflow)?;
        let width = i32::try_from(child_count)
            .map_err(|_| ContainerError::AddressOverflow)?
            .checked_add(1)
            .ok_or(ContainerError::AddressOverflow)?;
        stride = stride
            .checked_mul(width)
            .ok_or(ContainerError::AddressOverflow)?;
    }
    Ok(addr)
}

/// Encode either location form to a live REAPER FX address.
///
/// # Errors
///
/// [`ContainerError::TopIndexOutOfRange`] for a drifted bare index
/// (retryable), otherwise whatever [`crate::container_addr::encode_path`] reports.
pub fn encode_loc<C: ReaperFxChain + ?Sized>(
    chain: &C,
    loc: &FxLoc,
) -> Result<i32, ContainerError> {
    match loc {
        FxLoc::Top { index } => {
            let top_count = chain.top_fx_count();
            if *index < top_count {
                i32::try_from(*index).map_err(|_| ContainerError::AddressOverflow)
            } else {
                Err(ContainerError::TopIndexOutOfRange {
                    index: *index,
                    top_count,
                })
            }
        }
        FxLoc::Nested(path) => encode_path(chain, path),
    }
}

/// Decode a live address back to a location (Justin inverse + re-encode check).
///
/// Faithful port of Justin's `get_container_path_from_fx_id`: bit-test the
/// flag, peel `cur = base % stride` / `remain = base / stride` guided by live
/// `container_count` queries, then **re-encode and compare** so a wrong path
/// can never be returned (mismatch → [`ContainerError::UnresolvableAddress`]).
///
/// # Errors
///
/// Retryable: [`ContainerError::TopIndexOutOfRange`],
/// [`ContainerError::ContainerCountUnavailable`],
/// [`ContainerError::UnresolvableAddress`].
/// Non-retryable: [`ContainerError::UnsupportedAddressSpace`],
/// [`ContainerError::AddressOverflow`],
/// [`ContainerError::DepthExceeded`].
pub fn decode_address<C: ReaperFxChain + ?Sized>(
    chain: &C,
    addr: i32,
) -> Result<FxLoc, ContainerError> {
    if addr < 0 {
        return Err(ContainerError::UnsupportedAddressSpace { addr });
    }
    if addr & CONTAINER_FLAG == 0 {
        if addr & INPUT_FX_FLAG != 0 {
            return Err(ContainerError::UnsupportedAddressSpace { addr });
        }
        let index =
            u32::try_from(addr).map_err(|_| ContainerError::UnsupportedAddressSpace { addr })?;
        let top_count = chain.top_fx_count();
        if index < top_count {
            return Ok(FxLoc::Top { index });
        }
        return Err(ContainerError::TopIndexOutOfRange { index, top_count });
    }
    let top_count = chain.top_fx_count();
    let top_count_i = i32::try_from(top_count).map_err(|_| ContainerError::AddressOverflow)?;
    let mut stride = top_count_i
        .checked_add(1)
        .ok_or(ContainerError::AddressOverflow)?;
    // FLAG bit is set and the sign bit is clear, so `addr >= CONTAINER_FLAG`.
    let base = addr - CONTAINER_FLAG;
    let mut current = base % stride;
    let mut remain = base / stride;
    if current < 1 {
        // Justin: `if curidx < 1 then return nil end -- bad address`.
        return Err(ContainerError::UnresolvableAddress { addr });
    }
    let mut node_addr = CONTAINER_FLAG
        .checked_add(current)
        .ok_or(ContainerError::AddressOverflow)?;
    let mut positions: Vec<u32> = Vec::new();
    loop {
        if positions.len() >= MAX_CONTAINER_DEPTH {
            return Err(ContainerError::DepthExceeded {
                limit: MAX_CONTAINER_DEPTH,
            });
        }
        let level = positions.len();
        let child_count = chain.container_child_count(node_addr).map_err(|_| {
            ContainerError::ContainerCountUnavailable {
                addr: node_addr,
                level,
            }
        })?;
        let current_u =
            u32::try_from(current).map_err(|_| ContainerError::UnresolvableAddress { addr })?;
        positions.push(current_u);
        let remain_u =
            u32::try_from(remain).map_err(|_| ContainerError::UnresolvableAddress { addr })?;
        if remain_u <= child_count {
            if remain_u > 0 {
                positions.push(remain_u);
            }
            break;
        }
        let width = child_count
            .checked_add(1)
            .ok_or(ContainerError::AddressOverflow)?;
        let next_current_u = remain_u % width;
        let next_remain_u = remain_u / width;
        if next_current_u < 1 {
            return Err(ContainerError::UnresolvableAddress { addr });
        }
        let next_current_i = i32::try_from(next_current_u)
            .map_err(|_| ContainerError::UnresolvableAddress { addr })?;
        let width_i = i32::try_from(width).map_err(|_| ContainerError::AddressOverflow)?;
        let step = stride
            .checked_mul(next_current_i)
            .ok_or(ContainerError::AddressOverflow)?;
        node_addr = node_addr
            .checked_add(step)
            .ok_or(ContainerError::AddressOverflow)?;
        stride = stride
            .checked_mul(width_i)
            .ok_or(ContainerError::AddressOverflow)?;
        current = next_current_i;
        remain = i32::try_from(next_remain_u)
            .map_err(|_| ContainerError::UnresolvableAddress { addr })?;
    }
    let path = ContainerPath { positions };
    let check = encode_path(chain, &path)?;
    if check != addr {
        return Err(ContainerError::UnresolvableAddress { addr });
    }
    Ok(FxLoc::Nested(path))
}

fn check_guid<C: ReaperFxChain + ?Sized>(
    chain: &C,
    addr: i32,
    expected: &str,
) -> Result<(), ContainerError> {
    let observed = chain.fx_guid(addr)?;
    if observed == expected {
        Ok(())
    } else {
        Err(ContainerError::GuidMismatch {
            addr,
            expected: expected.to_string(),
            observed,
        })
    }
}

/// Resolve an anchor against the live chain: recompute + GUID-verify.
///
/// First resolution after a (re)scan. Callers holding a cached address must
/// use [`crate::container_addr::verify_fresh`] instead so stride drift is caught.
///
/// # Errors
///
/// Whatever [`crate::container_addr::encode_loc`] reports, plus [`ContainerError::GuidMismatch`]
/// (retryable) when the FX at the recomputed address is not the anchored one.
pub fn resolve_anchor<C: ReaperFxChain + ?Sized>(
    chain: &C,
    anchor: &FxAnchor,
) -> Result<i32, ContainerError> {
    let addr = encode_loc(chain, &anchor.loc)?;
    check_guid(chain, addr, &anchor.fx_guid)?;
    Ok(addr)
}

/// Recompute-before-every-op entry point: recompute, compare with the cached
/// address, then GUID-verify.
///
/// This is the interface the task mandates ("每次操作前重算"): a stride change
/// (top insert/delete, container child add/remove above the target) surfaces as
/// [`ContainerError::StaleAddress`]; a same-number/different-target shift (top
/// insert *at/before* the slot keeps the number but moves the FX) surfaces as
/// [`ContainerError::GuidMismatch`]. Both are retryable: re-scan, relocate by
/// GUID, or abort to [`crate::container_addr::flatten_fallback`].
///
/// # Errors
///
/// Retryable [`ContainerError::StaleAddress`] / [`ContainerError::GuidMismatch`]
/// plus whatever [`crate::container_addr::encode_loc`] reports.
pub fn verify_fresh<C: ReaperFxChain + ?Sized>(
    chain: &C,
    cached_addr: i32,
    anchor: &FxAnchor,
) -> Result<i32, ContainerError> {
    let recomputed = encode_loc(chain, &anchor.loc)?;
    if recomputed != cached_addr {
        return Err(ContainerError::StaleAddress {
            cached: cached_addr,
            recomputed,
        });
    }
    check_guid(chain, recomputed, &anchor.fx_guid)?;
    Ok(recomputed)
}

/// Flatten-fallback decision stub (DEC-003).
///
/// Retryable cause → re-resolve with a bounded budget; anything else (caller
/// bug, unsupported space, runaway depth/overflow) → flatten the container
/// into serial tracks + sends. The pure layer only *decides*; materialising
/// tracks/sends needs main-thread REAPER handles, deferred to TSK-102+.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlattenDecision {
    /// Re-scan and recompute first; executor decrements the budget.
    RetryResolve {
        /// Remaining re-resolve attempts (starts at [`crate::container_addr::MAX_RESOLVE_RETRIES`]).
        attempts_left: u8,
    },
    /// Give up on container addressing: rebuild as serial track + sends.
    FlattenToLinearTrack {
        /// Anchor that failed to resolve (drives the rebuild).
        anchor: FxAnchor,
        /// Why container addressing was abandoned.
        cause: ContainerError,
    },
}

/// Classify a resolution failure into retry-vs-flatten (DEC-003 stub).
#[must_use]
pub fn flatten_fallback(cause: &ContainerError, anchor: &FxAnchor) -> FlattenDecision {
    if cause.retryable() {
        FlattenDecision::RetryResolve {
            attempts_left: MAX_RESOLVE_RETRIES,
        }
    } else {
        FlattenDecision::FlattenToLinearTrack {
            anchor: anchor.clone(),
            cause: cause.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MockNode {
        child_count: Option<u32>,
        guid: &'static str,
    }

    struct MockChain {
        top_count: u32,
        nodes: HashMap<i32, MockNode>,
        track_guid: &'static str,
    }

    impl ReaperFxChain for MockChain {
        fn top_fx_count(&self) -> u32 {
            self.top_count
        }

        fn container_child_count(&self, addr: i32) -> Result<u32, ContainerError> {
            self.nodes
                .get(&addr)
                .and_then(|node| node.child_count)
                .ok_or(ContainerError::UnresolvableAddress { addr })
        }

        fn fx_guid(&self, addr: i32) -> Result<String, ContainerError> {
            self.nodes
                .get(&addr)
                .map(|node| node.guid.to_string())
                .ok_or(ContainerError::UnresolvableAddress { addr })
        }

        fn track_guid(&self) -> String {
            self.track_guid.to_string()
        }
    }

    /// Demo chain (also used by the module doctest):
    /// N = 4 top FX; 0-based top index 1 (1-based position 2) is container C0
    /// with 3 children; C0's child position 1 is nested container C1 with 2
    /// children. Stride L0 = 5, L1 = 5 * (3 + 1) = 20.
    fn nested_chain() -> MockChain {
        let mut nodes: HashMap<i32, MockNode> = HashMap::new();
        nodes.insert(
            0,
            MockNode {
                child_count: None,
                guid: "guid-top0",
            },
        );
        // Bare address of C0: a bare slot is never a container-space query target.
        nodes.insert(
            1,
            MockNode {
                child_count: None,
                guid: "guid-c0",
            },
        );
        nodes.insert(
            2,
            MockNode {
                child_count: None,
                guid: "guid-top2",
            },
        );
        nodes.insert(
            3,
            MockNode {
                child_count: None,
                guid: "guid-top3",
            },
        );
        // C0 encoded: FLAG + 2 = 33554434.
        nodes.insert(
            33_554_434,
            MockNode {
                child_count: Some(3),
                guid: "guid-c0",
            },
        );
        // C0 children: [2,1] C1 = 33554434 + 5 * 1 = 33554439 (container, cc = 2).
        nodes.insert(
            33_554_439,
            MockNode {
                child_count: Some(2),
                guid: "guid-c1",
            },
        );
        // C0 children: [2,2] = 33554444, [2,3] = 33554449 (plain leaves).
        nodes.insert(
            33_554_444,
            MockNode {
                child_count: None,
                guid: "guid-c0-k2",
            },
        );
        nodes.insert(
            33_554_449,
            MockNode {
                child_count: None,
                guid: "guid-c0-k3",
            },
        );
        // C1 children: [2,1,1] = 33554439 + 20 * 1 = 33554459,
        //              [2,1,2] = 33554439 + 20 * 2 = 33554479.
        nodes.insert(
            33_554_459,
            MockNode {
                child_count: None,
                guid: "guid-leaf-a",
            },
        );
        nodes.insert(
            33_554_479,
            MockNode {
                child_count: None,
                guid: "guid-leaf-b",
            },
        );
        MockChain {
            top_count: 4,
            nodes,
            track_guid: "track-1",
        }
    }

    fn anchor_to(loc: FxLoc, fx_guid: &str) -> FxAnchor {
        FxAnchor::new("track-1".to_string(), fx_guid.to_string(), loc).unwrap()
    }

    #[test]
    fn justin_single_level_vector() {
        // Justin's call shape get_fx_id_from_container_path(tr, 2, 1):
        // rv = FLAG + 2; rv += 5 * 1.
        let chain = nested_chain();
        let path = ContainerPath::new(vec![2, 1]).unwrap();
        let addr = encode_path(&chain, &path).unwrap();
        assert_eq!(addr, 33_554_439);
        assert_eq!(decode_address(&chain, addr).unwrap(), FxLoc::Nested(path));
    }

    #[test]
    fn top_level_bare_is_zero_based() {
        let chain = nested_chain();
        assert_eq!(encode_loc(&chain, &FxLoc::Top { index: 0 }).unwrap(), 0);
        assert_eq!(encode_loc(&chain, &FxLoc::Top { index: 3 }).unwrap(), 3);
        assert_eq!(decode_address(&chain, 1).unwrap(), FxLoc::Top { index: 1 });
        let err = encode_loc(&chain, &FxLoc::Top { index: 9 }).unwrap_err();
        assert_eq!(
            err,
            ContainerError::TopIndexOutOfRange {
                index: 9,
                top_count: 4
            }
        );
        assert!(err.retryable());
    }

    #[test]
    fn three_level_roundtrip() {
        let chain = nested_chain();
        // [2,1,2]: FLAG + 2 + 5 * 1 + 20 * 2 = 33554479.
        let leaf = ContainerPath::new(vec![2, 1, 2]).unwrap();
        assert_eq!(encode_path(&chain, &leaf).unwrap(), 33_554_479);
        assert_eq!(
            decode_address(&chain, 33_554_479).unwrap(),
            FxLoc::Nested(leaf)
        );
        // Intermediate containers decode to their own prefixes.
        assert_eq!(
            decode_address(&chain, 33_554_439).unwrap(),
            FxLoc::Nested(ContainerPath::new(vec![2, 1]).unwrap())
        );
        assert_eq!(
            decode_address(&chain, 33_554_434).unwrap(),
            FxLoc::Nested(ContainerPath::new(vec![2]).unwrap())
        );
        // The nested container itself encodes with FLAG set (Justin: FLAG + idx1).
        assert_eq!(
            encode_path(&chain, &ContainerPath::new(vec![2]).unwrap()).unwrap(),
            33_554_434
        );
    }

    #[test]
    fn stride_change_invalidates_cached_address() {
        let chain = nested_chain();
        let anchor = anchor_to(
            FxLoc::Nested(ContainerPath::new(vec![2, 1]).unwrap()),
            "guid-c1",
        );
        let cached = resolve_anchor(&chain, &anchor).unwrap();
        assert_eq!(cached, 33_554_439);
        // Simulate a top-level insert: N 4 -> 5 shifts stride 5 -> 6.
        let mut grown = nested_chain();
        grown.top_count = 5;
        let err = verify_fresh(&grown, cached, &anchor).unwrap_err();
        // FLAG + 2 + 6 * 1 = 33554440.
        assert_eq!(
            err,
            ContainerError::StaleAddress {
                cached: 33_554_439,
                recomputed: 33_554_440
            }
        );
        assert!(err.retryable());
    }

    #[test]
    fn guid_mismatch_after_same_number_shift() {
        // Same numbers can still point elsewhere (insert at/before the slot):
        // the GUID check is the second line of defence.
        let mut chain = nested_chain();
        chain.nodes.insert(
            33_554_439,
            MockNode {
                child_count: Some(2),
                guid: "guid-intruder",
            },
        );
        let anchor = anchor_to(
            FxLoc::Nested(ContainerPath::new(vec![2, 1]).unwrap()),
            "guid-c1",
        );
        let err = verify_fresh(&chain, 33_554_439, &anchor).unwrap_err();
        assert_eq!(
            err,
            ContainerError::GuidMismatch {
                addr: 33_554_439,
                expected: "guid-c1".to_string(),
                observed: "guid-intruder".to_string(),
            }
        );
        assert!(err.retryable());
    }

    #[test]
    fn count_query_failure_is_retryable() {
        let chain = nested_chain();
        // Child position past the live container_count.
        let err = encode_path(&chain, &ContainerPath::new(vec![2, 4]).unwrap()).unwrap_err();
        assert_eq!(
            err,
            ContainerError::ChildPositionOutOfRange {
                addr: 33_554_434,
                level: 1,
                position: 4,
                child_count: 3,
            }
        );
        assert!(err.retryable());
        // Descending into a plain (non-container) top FX: count query fails.
        let err = encode_path(&chain, &ContainerPath::new(vec![3, 1]).unwrap()).unwrap_err();
        assert_eq!(
            err,
            ContainerError::ContainerCountUnavailable {
                addr: 33_554_435,
                level: 0
            }
        );
        assert!(err.retryable());
        // Top position past the live top count.
        let err = encode_path(&chain, &ContainerPath::new(vec![9]).unwrap()).unwrap_err();
        assert_eq!(
            err,
            ContainerError::TopPositionOutOfRange {
                position: 9,
                top_count: 4
            }
        );
        assert!(err.retryable());
    }

    #[test]
    fn bad_paths_rejected_at_construction() {
        assert_eq!(
            ContainerPath::new(vec![]).unwrap_err(),
            ContainerError::EmptyPath
        );
        assert_eq!(
            ContainerPath::new(vec![2, 0]).unwrap_err(),
            ContainerError::ZeroPosition { level: 1 }
        );
        assert!(!ContainerError::EmptyPath.retryable());
        assert!(!ContainerError::ZeroPosition { level: 0 }.retryable());
    }

    #[test]
    fn anchor_json_roundtrip() {
        let anchor = anchor_to(
            FxLoc::Nested(ContainerPath::new(vec![2, 1, 2]).unwrap()),
            "guid-leaf-b",
        );
        let json = serde_json::to_string(&anchor).unwrap();
        assert!(json.contains("\"track_guid\":\"track-1\""));
        assert!(json.contains("\"fx_guid\":\"guid-leaf-b\""));
        assert!(json.contains("\"Nested\""));
        assert!(json.contains("[2,1,2]"));
        let back: FxAnchor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, anchor);
    }

    #[test]
    fn flatten_decisions() {
        let anchor = anchor_to(FxLoc::Top { index: 0 }, "guid-top0");
        let retryable = ContainerError::StaleAddress {
            cached: 1,
            recomputed: 2,
        };
        assert_eq!(
            flatten_fallback(&retryable, &anchor),
            FlattenDecision::RetryResolve {
                attempts_left: MAX_RESOLVE_RETRIES
            }
        );
        assert_eq!(MAX_RESOLVE_RETRIES, 3);
        let fatal = ContainerError::EmptyPath;
        assert_eq!(
            flatten_fallback(&fatal, &anchor),
            FlattenDecision::FlattenToLinearTrack {
                anchor: anchor.clone(),
                cause: fatal
            }
        );
    }

    #[test]
    fn input_flag_and_negative_rejected() {
        let chain = nested_chain();
        let err = decode_address(&chain, INPUT_FX_FLAG + 3).unwrap_err();
        assert_eq!(
            err,
            ContainerError::UnsupportedAddressSpace {
                addr: INPUT_FX_FLAG + 3
            }
        );
        assert!(!err.retryable());
        let err = decode_address(&chain, -1).unwrap_err();
        assert_eq!(err, ContainerError::UnsupportedAddressSpace { addr: -1 });
        // Residue 0 in 1-based space is Justin's "bad address".
        let err = decode_address(&chain, CONTAINER_FLAG).unwrap_err();
        assert_eq!(
            err,
            ContainerError::UnresolvableAddress {
                addr: CONTAINER_FLAG
            }
        );
        assert!(err.retryable());
    }

    #[test]
    fn retryable_taxonomy_covers_all_variants() {
        // ARCHITECTURE.md §8 mapping, locked in code: chain-mutation signals
        // retry; caller bugs / unsupported input do not.
        assert!(
            ContainerError::TopPositionOutOfRange {
                position: 9,
                top_count: 4
            }
            .retryable()
        );
        assert!(
            ContainerError::TopIndexOutOfRange {
                index: 9,
                top_count: 4
            }
            .retryable()
        );
        assert!(ContainerError::ContainerCountUnavailable { addr: 1, level: 0 }.retryable());
        assert!(
            ContainerError::ChildPositionOutOfRange {
                addr: 1,
                level: 1,
                position: 9,
                child_count: 3,
            }
            .retryable()
        );
        assert!(
            ContainerError::StaleAddress {
                cached: 1,
                recomputed: 2
            }
            .retryable()
        );
        assert!(
            ContainerError::GuidMismatch {
                addr: 1,
                expected: "a".to_string(),
                observed: "b".to_string(),
            }
            .retryable()
        );
        assert!(ContainerError::UnresolvableAddress { addr: 1 }.retryable());
        assert!(!ContainerError::EmptyPath.retryable());
        assert!(!ContainerError::ZeroPosition { level: 0 }.retryable());
        assert!(!ContainerError::EmptyGuidId { field: "fx_guid" }.retryable());
        assert!(!ContainerError::UnsupportedAddressSpace { addr: -1 }.retryable());
        assert!(!ContainerError::AddressOverflow.retryable());
        assert!(
            !ContainerError::DepthExceeded {
                limit: MAX_CONTAINER_DEPTH
            }
            .retryable()
        );
    }
}
