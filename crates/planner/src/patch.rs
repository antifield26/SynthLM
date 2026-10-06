//! JSON Patch plan schema with two-phase validation and a repair loop.
//!
//! Implements the TSK-302 slice of DEC-013 and ARCHITECTURE §8:
//!
//! - Patch plans are an RFC 6902 subset: [`crate::patch::PatchOp`] carries an
//!   op (`add` / `remove` / `replace`), an ident-shaped
//!   [`crate::patch::IdentPath`] (`param/<ident>` or `macro/<name>`), and a
//!   JSON value. Bare numeric indices (`param/4`, `/0`) are rejected at shape
//!   level: bare JSFX idents encode slider position, not identity
//!   ([`synthlm_profile::schema::is_bare_ident`]), so planners
//!   must address such parameters by name pattern or by a stable semantic
//!   ident (`11:wet`) instead.
//! - Two-phase validation ([`crate::patch::validate`]): (a) shape checks (op
//!   spelling, path grammar, value JSON type per op) with no profile needed;
//!   (b) semantic checks against a [`synthlm_profile::schema::Profile`]
//!   (ident resolvable by exact ident or by exact stored `name_regex` text,
//!   otherwise [`crate::patch::PatchErrorKind::UnresolvableIdent`];
//!   `macro/` paths must address [`synthlm_profile::schema::SoundRole::Macro`]
//!   entries; [`synthlm_profile::schema::SoundRole::PresetOnly`]
//!   entries never accept single-param ops; values must fit the target
//!   widget/scale).
//! - Repair loop ([`crate::patch::repair_round`],
//!   [`crate::patch::validate_with_repair`]): a pure function drops illegal
//!   ops (including the `FromIdent` -1 migration/removal branch: unresolvable
//!   idents are removed, never guessed) and substitutes coercible values,
//!   keeping an audit trail ([`crate::patch::RepairReport`]). Residual errors
//!   are `BLOCKED` (terminal, never retried), reusing the
//!   [`synthlm_common::ipc::ErrorCode`] taxonomy so verdicts cannot
//!   drift from [`crate::model_gw::GatewayError`].
//!
//! ## Path grammar
//!
//! Documented shape (enforced without a `regex` dependency so no
//! `docs/LICENSES.md` entry is needed):
//!
//! ```text
//! path      := ("param/" key) | ("macro/" name)
//! key/name  := 1+ chars, no `/`, not all ASCII digits
//! ```
//!
//! Anything else (JSON-pointer `/0` style, missing head, empty tail,
//! all-digit tail) is a shape error. Resolution tries exact `ident` first,
//! then exact stored `name_regex` text (literal equality with the profile
//! entry, not a live regex match — the bridge compiles the pattern against
//! live parameter names later).
//!
//! ## Value rules per target widget
//!
//! | target [`synthlm_profile::schema::UiHint`] | accepted | repairable |
//! |---|---|---|
//! | Slider / Knob (linear, log) | Number in `0.0..=1.0` | numeric Strings (`"0.5"`), then clamp |
//! | Toggle (indexed) | Bool | `0.0`/`1.0` numbers, bool-word Strings (`"true"`, `"off"`, `"1"`) |
//! | Select (indexed) | String label or non-negative integer Number | integral floats (`2.0` → `2`) |
//!
//! `Remove` ops must carry JSON `null` (a stray value is nulled by repair).
//! `Add` / `Replace` need a non-null scalar (arrays/objects are rejected).
//! Without target state `add` and `replace` both mean "set parameter"; the
//! bridge enforces existence.
//!
//! Blocking contract: everything here is pure computation (no I/O, no clock).
//! Never call from an audio thread anyway (AGENTS.md red line 2); this is a
//! control-plane helper.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use synthlm_common::ipc::ErrorCode;
use synthlm_profile::schema::{Profile, SoundRole, UiHint};

// ---------------------------------------------------------------------------
// Op kind (lenient wire parsing: unknown spellings become Invalid)
// ---------------------------------------------------------------------------

/// RFC 6902 subset op: `add` / `remove` / `replace`.
///
/// Wire parsing is total: any other spelling (including RFC 6902 `move`,
/// `copy`, `test`, which need target state the planner never has) becomes
/// [`crate::patch::PatchOpKind::Invalid`], and shape validation
/// flags it per-op so repair can drop just that op instead of rejecting the
/// whole plan document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchOpKind {
    /// Set a parameter (target existence is enforced by the bridge).
    Add,
    /// Clear a parameter binding. The op value must be JSON `null`.
    Remove,
    /// Set a parameter (same as `add` without target state).
    Replace,
    /// Unknown op spelling; always a shape error (see
    /// [`crate::patch::PatchErrorKind::InvalidOp`]).
    Invalid,
}

impl PatchOpKind {
    /// Wire spelling (`"add"`, `"remove"`, `"replace"`, `"invalid"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PatchOpKind::Add => "add",
            PatchOpKind::Remove => "remove",
            PatchOpKind::Replace => "replace",
            PatchOpKind::Invalid => "invalid",
        }
    }

    /// Whether this op sets a value (`add` / `replace`).
    #[must_use]
    pub fn is_setter(self) -> bool {
        matches!(self, PatchOpKind::Add | PatchOpKind::Replace)
    }
}

impl<'de> Deserialize<'de> for PatchOpKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "add" => PatchOpKind::Add,
            "remove" => PatchOpKind::Remove,
            "replace" => PatchOpKind::Replace,
            _ => PatchOpKind::Invalid,
        })
    }
}

impl Serialize for PatchOpKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            PatchOpKind::Add => "add",
            PatchOpKind::Remove => "remove",
            PatchOpKind::Replace => "replace",
            PatchOpKind::Invalid => "invalid",
        })
    }
}

// ---------------------------------------------------------------------------
// Ident path
// ---------------------------------------------------------------------------

/// Raw patch path: `param/<ident-or-pattern>` or `macro/<name>`.
///
/// Deserialization accepts any string (never fails) so that malformed paths
/// surface as per-op shape errors ([`crate::patch::PatchErrorKind::BadPath`],
/// [`crate::patch::PatchErrorKind::BareIndex`]) instead of
/// aborting the whole plan parse. Use [`crate::patch::IdentPath::parse`]
/// for the validated view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdentPath(String);

impl IdentPath {
    /// Wrap a raw path string (validation happens in
    /// [`crate::patch::IdentPath::parse`]).
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The raw path text.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.0
    }

    /// Validate the grammar (`param/<key>` or `macro/<name>`, non-empty
    /// non-numeric tail) and return the structured view.
    ///
    /// # Errors
    ///
    /// Returns [`crate::patch::PathRejection::BadPath`] for a
    /// wrong head, missing separator, empty tail, or embedded `/`; returns
    /// [`crate::patch::PathRejection::BareIndex`] for an
    /// all-digit tail (bare FX indices are never persisted, DEC-015).
    pub fn parse(&self) -> Result<ParsedPath, PathRejection> {
        let (head, tail) = self.0.split_once('/').ok_or(PathRejection::BadPath)?;
        let parsed_head = match head {
            "param" => PathHead::Param,
            "macro" => PathHead::Macro,
            _ => return Err(PathRejection::BadPath),
        };
        if tail.is_empty() || tail.contains('/') {
            return Err(PathRejection::BadPath);
        }
        if tail.bytes().all(|b| b.is_ascii_digit()) {
            return Err(PathRejection::BareIndex);
        }
        Ok(match parsed_head {
            PathHead::Param => ParsedPath::Param {
                key: tail.to_owned(),
            },
            PathHead::Macro => ParsedPath::Macro {
                name: tail.to_owned(),
            },
        })
    }
}

/// Which `param/` vs `macro/` namespace a path addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathHead {
    /// Live parameter namespace.
    Param,
    /// Whitelist-macro namespace.
    Macro,
}

/// Validated view of an [`crate::patch::IdentPath`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedPath {
    /// `param/<key>`: `key` is tried as exact `ident`, then as exact stored
    /// `name_regex` text.
    Param {
        /// Address key (ident or stored pattern text).
        key: String,
    },
    /// `macro/<name>`: must resolve to a
    /// [`synthlm_profile::schema::SoundRole::Macro`] entry.
    Macro {
        /// Macro name (ident or stored pattern text of a macro entry).
        name: String,
    },
}

/// Why [`crate::patch::IdentPath::parse`] rejected a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathRejection {
    /// Wrong head, missing `/`, empty tail, or embedded `/`.
    BadPath,
    /// All-digit tail: a bare FX index, never a stable address.
    BareIndex,
}

// ---------------------------------------------------------------------------
// Patch op / plan
// ---------------------------------------------------------------------------

/// One RFC 6902-subset operation against whitelist addresses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchOp {
    /// Operation (`add` / `remove` / `replace`; anything else parses to
    /// [`crate::patch::PatchOpKind::Invalid`] for per-op repair).
    pub op: PatchOpKind,
    /// Target address (`param/<ident>` or `macro/<name>`).
    pub path: IdentPath,
    /// New value (`null` for `remove`; scalar for `add` / `replace`).
    pub value: serde_json::Value,
}

/// A validated-unit plan: ops plus the frozen snapshot they apply to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchPlan {
    /// Operations in application order.
    pub ops: Vec<PatchOp>,
    /// Frozen snapshot id / fingerprint this plan was solved against
    /// (DEC-004 explicit snapshot; must be non-empty).
    pub target_snapshot: String,
}

impl PatchPlan {
    /// Parse a plan document from JSON (total over per-op spellings: illegal
    /// ops/paths/values parse fine and fail [`crate::patch::validate`]).
    ///
    /// # Errors
    ///
    /// Returns the serde error when the document is not a plan object at all
    /// (wrong JSON types, unknown fields).
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

// ---------------------------------------------------------------------------
// Errors (value-free, BLOCKED-only, taxonomy-mapped)
// ---------------------------------------------------------------------------

/// Per-op / per-plan validation failure kind.
///
/// Variants are split by repair policy: `OutOfRange`, `CoerceToNumber`,
/// `CoerceToBool`, `CoerceToInt`, and `UnexpectedValue` are fixable (repair
/// substitutes a value and keeps the op); `EmptySnapshot` / `EmptyOps` are
/// plan-level residuals; everything else removes the op (the `FromIdent` -1
/// migration/removal branch: unresolvable idents are dropped, never
/// guessed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchErrorKind {
    /// Unknown op spelling (RFC 6902 `move`/`copy`/`test` included).
    InvalidOp,
    /// Path is not `param/<key>` or `macro/<name>`.
    BadPath,
    /// All-digit path tail: a bare FX index, never persisted.
    BareIndex,
    /// `add` / `replace` with JSON `null` (no value to invent: removed).
    MissingValue,
    /// `remove` with a non-null value (fixable: repair nulls it).
    UnexpectedValue,
    /// `add` / `replace` with an array/object value (no safe scalar
    /// reading: removed).
    InvalidValueType,
    /// Empty `target_snapshot` (plan-level residual).
    EmptySnapshot,
    /// Zero ops (plan-level residual).
    EmptyOps,
    /// Neither exact `ident` nor exact stored `name_regex` text hit.
    UnresolvableIdent,
    /// `macro/<name>` hit a non-macro entry.
    RoleNotWhitelisted,
    /// Single-param op against a chunk-only `preset_only` entry.
    PresetOnlySingleParam,
    /// Value JSON type cannot fit the target widget (bool for a slider,
    /// fractional index for a select, …).
    TypeMismatch,
    /// Continuous number outside `0.0..=1.0` (fixable: clamp).
    OutOfRange,
    /// Numeric string for a continuous target (fixable: parse).
    CoerceToNumber,
    /// Bool-word/`0`/`1` string or `0.0`/`1.0` number for a toggle
    /// (fixable: to bool).
    CoerceToBool,
    /// Integral float for an indexed select (fixable: to int).
    CoerceToInt,
}

/// One validation failure: an op index (or `None` for plan-level errors)
/// plus a value-free kind.
///
/// Stores no path text and no values, mirroring
/// [`crate::model_gw::GatewayError`]: formatting an error can
/// never echo caller material.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("patch op {op_index:?}: {kind:?} (see DEC-013)")]
pub struct PatchError {
    /// Op index in the validated plan, or `None` for plan-level errors
    /// (`EmptySnapshot`, `EmptyOps`).
    pub op_index: Option<usize>,
    /// Failure class (drives repair policy and the retry verdict).
    pub kind: PatchErrorKind,
}

impl PatchError {
    /// Map to the [`synthlm_common::ipc::ErrorCode`] taxonomy.
    /// Shape failures are protocol violations; semantic failures address
    /// state outside the whitelist. Both are terminal.
    #[must_use]
    pub fn code(self) -> ErrorCode {
        match self.kind {
            PatchErrorKind::UnresolvableIdent
            | PatchErrorKind::RoleNotWhitelisted
            | PatchErrorKind::PresetOnlySingleParam => ErrorCode::WhitelistViolation,
            PatchErrorKind::InvalidOp
            | PatchErrorKind::BadPath
            | PatchErrorKind::BareIndex
            | PatchErrorKind::MissingValue
            | PatchErrorKind::UnexpectedValue
            | PatchErrorKind::InvalidValueType
            | PatchErrorKind::EmptySnapshot
            | PatchErrorKind::EmptyOps
            | PatchErrorKind::TypeMismatch
            | PatchErrorKind::OutOfRange
            | PatchErrorKind::CoerceToNumber
            | PatchErrorKind::CoerceToBool
            | PatchErrorKind::CoerceToInt => ErrorCode::ProtocolViolation,
        }
    }

    /// Whether this failure is user-visible `BLOCKED` (never retried).
    ///
    /// Derived from [`crate::patch::PatchError::code`] so
    /// the verdict cannot drift from the taxonomy; currently always `true`.
    #[must_use]
    pub fn blocked(self) -> bool {
        !self.code().retryable()
    }

    /// Static remediation hint for UI / BLOCKED surfaces.
    #[must_use]
    pub fn guidance(self) -> &'static str {
        match self.kind {
            PatchErrorKind::InvalidOp => {
                "use add, remove, or replace only; move/copy/test need target state the planner never has (see DEC-013)"
            }
            PatchErrorKind::BadPath => {
                "use param/<ident> or macro/<name> paths; JSON-pointer /N style is rejected (see DEC-013)"
            }
            PatchErrorKind::BareIndex => {
                "bare numeric indices are never stable addresses; use the ident or name pattern (see DEC-015)"
            }
            PatchErrorKind::MissingValue => {
                "add/replace need a scalar value; a null carries nothing to write (see DEC-013)"
            }
            PatchErrorKind::UnexpectedValue => {
                "remove carries no value; repair nulls it automatically (see DEC-013)"
            }
            PatchErrorKind::InvalidValueType => {
                "add/replace values must be scalars (number, bool, or string); arrays/objects are rejected (see DEC-013)"
            }
            PatchErrorKind::EmptySnapshot => {
                "plans must name the frozen snapshot they were solved against (see DEC-004)"
            }
            PatchErrorKind::EmptyOps => {
                "plans must carry at least one op; an empty plan is not applicable (see DEC-013)"
            }
            PatchErrorKind::UnresolvableIdent => {
                "ident resolves to no profile entry (FromIdent -1 equivalent): drop the op or migrate the profile (see DEC-013)"
            }
            PatchErrorKind::RoleNotWhitelisted => {
                "macro/ paths must address macro-role entries; retarget or drop the op (see DEC-015)"
            }
            PatchErrorKind::PresetOnlySingleParam => {
                "preset_only state is chunk-only and never written live; route it through a preset, not a single-param op (see DEC-013)"
            }
            PatchErrorKind::TypeMismatch => {
                "value type cannot fit the target widget (e.g. bool for a slider); fix the emitter or drop the op (see DEC-013)"
            }
            PatchErrorKind::OutOfRange => {
                "continuous values are normalized to 0.0..=1.0; repair clamps automatically (see DEC-013)"
            }
            PatchErrorKind::CoerceToNumber
            | PatchErrorKind::CoerceToBool
            | PatchErrorKind::CoerceToInt => {
                "scalar spelling does not match the target widget; repair coerces automatically (see DEC-013)"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Role whitelist
// ---------------------------------------------------------------------------

/// Whether `role` may be written by single-param live ops.
///
/// The writable whitelist is every [`synthlm_profile::schema::SoundRole`]
/// except [`synthlm_profile::schema::SoundRole::PresetOnly`],
/// which is chunk-only state (e.g. mod-matrix routing, B §4) and must travel
/// via presets, never via live parameter writes.
#[must_use]
pub fn is_whitelisted_role(role: SoundRole) -> bool {
    !matches!(role, SoundRole::PresetOnly)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Shape-check one op: op spelling, path grammar, value JSON type per op.
///
/// Returns the op's shape errors (empty = clean). Semantic checks are skipped
/// for shape-dirty ops by [`crate::patch::validate`], so each op reports its
/// root cause only.
fn shape_errors(op: &PatchOp, index: usize) -> Vec<PatchError> {
    let mut errors = Vec::new();
    if matches!(op.op, PatchOpKind::Invalid) {
        errors.push(PatchError {
            op_index: Some(index),
            kind: PatchErrorKind::InvalidOp,
        });
        return errors;
    }
    if let Err(rejection) = op.path.parse() {
        errors.push(PatchError {
            op_index: Some(index),
            kind: match rejection {
                PathRejection::BadPath => PatchErrorKind::BadPath,
                PathRejection::BareIndex => PatchErrorKind::BareIndex,
            },
        });
        return errors;
    }
    match op.op {
        PatchOpKind::Add | PatchOpKind::Replace => {
            if op.value.is_null() {
                errors.push(PatchError {
                    op_index: Some(index),
                    kind: PatchErrorKind::MissingValue,
                });
            } else if !is_scalar(&op.value) {
                errors.push(PatchError {
                    op_index: Some(index),
                    kind: PatchErrorKind::InvalidValueType,
                });
            }
        }
        PatchOpKind::Remove => {
            if !op.value.is_null() {
                errors.push(PatchError {
                    op_index: Some(index),
                    kind: PatchErrorKind::UnexpectedValue,
                });
            }
        }
        PatchOpKind::Invalid => {}
    }
    errors
}

/// Whether a value is a scalar patch payload (number, bool, or string).
fn is_scalar(value: &serde_json::Value) -> bool {
    value.is_number() || value.is_boolean() || value.is_string()
}

/// Resolve a parsed path against `profile`.
///
/// `param/<key>` tries exact `ident`, then exact stored `name_regex` text.
/// `macro/<name>` resolves the same way but only against macro-role entries:
/// a hit on a non-macro entry is [`crate::patch::PatchErrorKind::RoleNotWhitelisted`].
fn resolve<'a>(
    parsed: &ParsedPath,
    profile: &'a Profile,
) -> Result<&'a synthlm_profile::schema::ParamEntry, PatchErrorKind> {
    let key = match parsed {
        ParsedPath::Param { key } => key.as_str(),
        ParsedPath::Macro { name } => name.as_str(),
    };
    let by_ident = profile.param_by_ident(key);
    let by_pattern = profile.param_by_name_regex(key);
    match parsed {
        ParsedPath::Param { .. } => {
            if let Some(entry) = by_ident.or(by_pattern) {
                Ok(entry)
            } else {
                Err(PatchErrorKind::UnresolvableIdent)
            }
        }
        ParsedPath::Macro { .. } => {
            if let Some(entry) = by_ident.or(by_pattern) {
                if entry.role == SoundRole::Macro {
                    Ok(entry)
                } else {
                    Err(PatchErrorKind::RoleNotWhitelisted)
                }
            } else {
                Err(PatchErrorKind::UnresolvableIdent)
            }
        }
    }
}

/// Semantic-check one shape-clean op against `profile`.
fn semantic_errors(
    op: &PatchOp,
    parsed: &ParsedPath,
    profile: &Profile,
    index: usize,
) -> Vec<PatchError> {
    let mut errors = Vec::new();
    let entry = match resolve(parsed, profile) {
        Ok(entry) => entry,
        Err(kind) => {
            errors.push(PatchError {
                op_index: Some(index),
                kind,
            });
            return errors;
        }
    };
    if !is_whitelisted_role(entry.role) {
        errors.push(PatchError {
            op_index: Some(index),
            kind: PatchErrorKind::PresetOnlySingleParam,
        });
        return errors;
    }
    if matches!(op.op, PatchOpKind::Remove) {
        return errors;
    }
    if let Some(kind) = value_mismatch(&op.value, entry.ui) {
        errors.push(PatchError {
            op_index: Some(index),
            kind,
        });
    }
    errors
}

/// Check a setter value against the target widget. Returns `None` when the
/// value fits as-is; otherwise the fixable-or-fatal semantic kind.
///
/// `Remove` values never reach here (shape-checked to `null`), and
/// `preset_only` targets never reach here (rejected above).
fn value_mismatch(value: &serde_json::Value, ui: UiHint) -> Option<PatchErrorKind> {
    match ui {
        UiHint::Slider | UiHint::Knob => match value {
            serde_json::Value::Number(number) => {
                if let Some(scalar) = number.as_f64() {
                    if (0.0..=1.0).contains(&scalar) {
                        None
                    } else {
                        Some(PatchErrorKind::OutOfRange)
                    }
                } else {
                    Some(PatchErrorKind::TypeMismatch)
                }
            }
            serde_json::Value::String(text) => {
                if coerce_string_to_number(text).is_some() {
                    Some(PatchErrorKind::CoerceToNumber)
                } else {
                    Some(PatchErrorKind::TypeMismatch)
                }
            }
            _ => Some(PatchErrorKind::TypeMismatch),
        },
        UiHint::Toggle => match value {
            serde_json::Value::Bool(_) => None,
            serde_json::Value::Number(number) => {
                if let Some(scalar) = number.as_f64() {
                    if scalar == 0.0 || scalar == 1.0 {
                        Some(PatchErrorKind::CoerceToBool)
                    } else {
                        Some(PatchErrorKind::TypeMismatch)
                    }
                } else {
                    Some(PatchErrorKind::TypeMismatch)
                }
            }
            serde_json::Value::String(text) => {
                if coerce_string_to_bool(text).is_some() {
                    Some(PatchErrorKind::CoerceToBool)
                } else {
                    Some(PatchErrorKind::TypeMismatch)
                }
            }
            _ => Some(PatchErrorKind::TypeMismatch),
        },
        UiHint::Select => match value {
            serde_json::Value::String(_) => None,
            serde_json::Value::Number(number) => {
                if let Some(scalar) = number.as_i64() {
                    if scalar >= 0 {
                        None
                    } else {
                        Some(PatchErrorKind::TypeMismatch)
                    }
                } else if number.as_u64().is_some() {
                    // Large non-negative integers past i64 range: valid as-is.
                    None
                } else if let Some(scalar) = number.as_f64() {
                    if scalar.is_finite() && scalar >= 0.0 && scalar.fract() == 0.0 {
                        Some(PatchErrorKind::CoerceToInt)
                    } else {
                        Some(PatchErrorKind::TypeMismatch)
                    }
                } else {
                    Some(PatchErrorKind::TypeMismatch)
                }
            }
            _ => Some(PatchErrorKind::TypeMismatch),
        },
        UiHint::Hidden => Some(PatchErrorKind::PresetOnlySingleParam),
    }
}

/// Parse a trimmed string as a finite f64 (rejects `NaN`/`inf` spellings,
/// which [`str::parse`] would otherwise accept).
fn coerce_string_to_number(text: &str) -> Option<f64> {
    let scalar: f64 = text.trim().parse().ok()?;
    if scalar.is_finite() {
        Some(scalar)
    } else {
        None
    }
}

/// Parse a bool-word string (`true`/`false`/`on`/`off`/`1`/`0`,
/// ASCII case-insensitive, surrounding whitespace ignored).
fn coerce_string_to_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "1" => Some(true),
        "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Shape-check a whole plan (no profile needed): per-op shape plus the
/// `target_snapshot` / non-empty-ops plan rules.
#[must_use]
pub fn validate_shape(plan: &PatchPlan) -> Vec<PatchError> {
    let mut errors = Vec::new();
    if plan.target_snapshot.is_empty() {
        errors.push(PatchError {
            op_index: None,
            kind: PatchErrorKind::EmptySnapshot,
        });
    }
    if plan.ops.is_empty() {
        errors.push(PatchError {
            op_index: None,
            kind: PatchErrorKind::EmptyOps,
        });
    }
    for (index, op) in plan.ops.iter().enumerate() {
        errors.extend(shape_errors(op, index));
    }
    errors
}

/// Full two-phase validation: shape first, then semantics for shape-clean
/// ops only (each op reports its root cause, never a cascade).
#[must_use]
pub fn validate(plan: &PatchPlan, profile: &Profile) -> Vec<PatchError> {
    let mut errors = Vec::new();
    if plan.target_snapshot.is_empty() {
        errors.push(PatchError {
            op_index: None,
            kind: PatchErrorKind::EmptySnapshot,
        });
    }
    if plan.ops.is_empty() {
        errors.push(PatchError {
            op_index: None,
            kind: PatchErrorKind::EmptyOps,
        });
    }
    for (index, op) in plan.ops.iter().enumerate() {
        let shape = shape_errors(op, index);
        if !shape.is_empty() {
            errors.extend(shape);
            continue;
        }
        if matches!(op.op, PatchOpKind::Invalid) {
            continue;
        }
        let parsed = match op.path.parse() {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        errors.extend(semantic_errors(op, &parsed, profile, index));
    }
    errors
}

// ---------------------------------------------------------------------------
// Repair
// ---------------------------------------------------------------------------

/// Audit trail of one [`crate::patch::repair_round`]: removed op indices,
/// value-substituted op indices (both into the round input plan), and the
/// round errors neither action could absorb (plan-level residuals).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairReport {
    /// Input-plan indices of dropped ops.
    pub removed: Vec<usize>,
    /// Input-plan indices of value-substituted (kept) ops.
    pub replaced: Vec<usize>,
    /// Errors that are neither removable per-op nor substitutable
    /// (plan-level: [`crate::patch::PatchErrorKind::EmptySnapshot`],
    /// [`crate::patch::PatchErrorKind::EmptyOps`]).
    pub residual_errors: Vec<PatchError>,
}

impl RepairReport {
    /// Whether the round changed anything (empty when only plan-level
    /// residuals remain, which signals the loop to stop).
    #[must_use]
    pub fn progressed(&self) -> bool {
        !self.removed.is_empty() || !self.replaced.is_empty()
    }
}

/// One repair round: drop illegal ops, substitute fixable values.
///
/// Pure function: no profile, no I/O. Policy per error kind (first match per
/// op wins; removal beats substitution):
///
/// - remove: `InvalidOp`, `BadPath`, `BareIndex`, `MissingValue`,
///   `InvalidValueType`, `UnresolvableIdent` (the `FromIdent` -1
///   migration/removal branch), `RoleNotWhitelisted`,
///   `PresetOnlySingleParam`, `TypeMismatch`;
/// - substitute: `UnexpectedValue` (null the stray value), `OutOfRange`
///   (clamp to `0.0..=1.0`), `CoerceToNumber` / `CoerceToBool` /
///   `CoerceToInt` (apply the coercion the validator already proved);
/// - residual: plan-level errors, which no op edit can absorb.
#[must_use]
pub fn repair_round(plan: &PatchPlan, errors: &[PatchError]) -> (PatchPlan, RepairReport) {
    let mut remove = vec![false; plan.ops.len()];
    let mut substitute: Vec<Option<serde_json::Value>> = vec![None; plan.ops.len()];
    let mut residual_errors = Vec::new();

    for error in errors {
        let Some(index) = error.op_index else {
            residual_errors.push(*error);
            continue;
        };
        if index >= plan.ops.len() {
            continue;
        }
        if remove[index] {
            continue;
        }
        match error.kind {
            PatchErrorKind::InvalidOp
            | PatchErrorKind::BadPath
            | PatchErrorKind::BareIndex
            | PatchErrorKind::MissingValue
            | PatchErrorKind::InvalidValueType
            | PatchErrorKind::UnresolvableIdent
            | PatchErrorKind::RoleNotWhitelisted
            | PatchErrorKind::PresetOnlySingleParam
            | PatchErrorKind::TypeMismatch => {
                remove[index] = true;
                substitute[index] = None;
            }
            PatchErrorKind::UnexpectedValue => {
                if substitute[index].is_none() {
                    substitute[index] = Some(serde_json::Value::Null);
                }
            }
            PatchErrorKind::OutOfRange => {
                if substitute[index].is_none()
                    && let Some(clamped) = clamp_number(&plan.ops[index].value)
                {
                    substitute[index] = Some(clamped);
                }
            }
            PatchErrorKind::CoerceToNumber => {
                if substitute[index].is_none()
                    && let Some(coerced) = coerce_value_to_number(&plan.ops[index].value)
                {
                    substitute[index] = Some(coerced);
                }
            }
            PatchErrorKind::CoerceToBool => {
                if substitute[index].is_none()
                    && let Some(coerced) = coerce_value_to_bool(&plan.ops[index].value)
                {
                    substitute[index] = Some(coerced);
                }
            }
            PatchErrorKind::CoerceToInt => {
                if substitute[index].is_none()
                    && let Some(coerced) = coerce_value_to_int(&plan.ops[index].value)
                {
                    substitute[index] = Some(coerced);
                }
            }
            PatchErrorKind::EmptySnapshot | PatchErrorKind::EmptyOps => {
                residual_errors.push(*error);
            }
        }
    }

    let mut ops = Vec::with_capacity(plan.ops.len());
    let mut removed = Vec::new();
    let mut replaced = Vec::new();
    for (index, op) in plan.ops.iter().enumerate() {
        if remove[index] {
            removed.push(index);
        } else if let Some(value) = substitute[index].take() {
            let mut fixed = op.clone();
            fixed.value = value;
            ops.push(fixed);
            replaced.push(index);
        } else {
            ops.push(op.clone());
        }
    }
    (
        PatchPlan {
            ops,
            target_snapshot: plan.target_snapshot.clone(),
        },
        RepairReport {
            removed,
            replaced,
            residual_errors,
        },
    )
}

/// Clamp a JSON number into `0.0..=1.0` (`None` for non-numbers).
fn clamp_number(value: &serde_json::Value) -> Option<serde_json::Value> {
    let scalar = value.as_f64()?;
    if !scalar.is_finite() {
        return None;
    }
    Some(
        serde_json::Number::from_f64(scalar.clamp(0.0, 1.0))
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
    )
}

/// Coerce a numeric string to a number (`None` when unparseable).
fn coerce_value_to_number(value: &serde_json::Value) -> Option<serde_json::Value> {
    let text = value.as_str()?;
    coerce_string_to_number(text)
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
}

/// Coerce a bool-word string or `0.0`/`1.0` number to a bool.
fn coerce_value_to_bool(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::String(text) => coerce_string_to_bool(text).map(serde_json::Value::Bool),
        serde_json::Value::Number(number) => {
            let scalar = number.as_f64()?;
            if scalar == 0.0 {
                Some(serde_json::Value::Bool(false))
            } else if scalar == 1.0 {
                Some(serde_json::Value::Bool(true))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Coerce an integral float to an int number.
fn coerce_value_to_int(value: &serde_json::Value) -> Option<serde_json::Value> {
    let scalar = value.as_f64()?;
    if scalar.is_finite() && scalar >= 0.0 && scalar.fract() == 0.0 {
        let truncated = scalar.trunc();
        if truncated <= f64::from(u32::MAX) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let int = truncated as u64;
            return Some(serde_json::Value::Number(serde_json::Number::from(int)));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// validate_with_repair
// ---------------------------------------------------------------------------

/// Default repair budget: two rounds (DEC-013 reversal condition: structured
/// validity `<95%` after 2 repair rounds triggers schema tightening).
pub const DEFAULT_MAX_REPAIR_ROUNDS: u32 = 2;

/// Outcome of [`crate::patch::validate_with_repair`]: the surviving plan,
/// one [`crate::patch::RepairReport`] per repair round, and
/// the final residual errors (empty = accepted).
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationOutcome {
    /// Surviving plan (repaired ops substituted, illegal ops dropped).
    pub plan: PatchPlan,
    /// One report per repair round actually run.
    pub reports: Vec<RepairReport>,
    /// Final residual errors. Empty means the plan is accepted; non-empty is
    /// `BLOCKED` (every [`crate::patch::PatchError`] is
    /// terminal — see [`crate::patch::PatchError::blocked`] —
    /// so residual errors return directly, never retry, echoing the
    /// [`crate::model_gw`] taxonomy).
    pub residual_errors: Vec<PatchError>,
}

impl ValidationOutcome {
    /// Whether the plan survived (no residual errors).
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.residual_errors.is_empty()
    }

    /// How many repair rounds ran.
    #[must_use]
    pub fn rounds_used(&self) -> usize {
        self.reports.len()
    }
}

/// Validate, repair up to `max_rounds` rounds, and report.
///
/// Each round validates the current plan; when errors remain,
/// [`crate::patch::repair_round`] runs and the loop continues
/// with the repaired plan. The loop stops early on a clean plan or on a
/// round that changes nothing (plan-level residuals only). Residual errors
/// are `BLOCKED`: terminal verdicts returned directly, never retried.
#[must_use]
pub fn validate_with_repair(
    plan: &PatchPlan,
    profile: &Profile,
    max_rounds: u32,
) -> ValidationOutcome {
    let mut current = plan.clone();
    let mut reports = Vec::new();
    for _ in 0..max_rounds {
        let errors = validate(&current, profile);
        if errors.is_empty() {
            return ValidationOutcome {
                plan: current,
                reports,
                residual_errors: Vec::new(),
            };
        }
        let (repaired, report) = repair_round(&current, &errors);
        let progressed = report.progressed();
        reports.push(report);
        current = repaired;
        if !progressed {
            break;
        }
    }
    let residual_errors = validate(&current, profile);
    ValidationOutcome {
        plan: current,
        reports,
        residual_errors,
    }
}

// ---------------------------------------------------------------------------
// Tests (unit: grammar, coercion, repair purity; corpus gates live in
// tests/patch_fixtures.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_profile() -> Profile {
        Profile::from_json(
            r#"{
            "schema_version": 1,
            "fx_ident_match": "Test",
            "groups": ["Main", "Preset"],
            "params": [
                {"ident": "0:cutoff", "name_regex": "Cutoff", "role": "cutoff", "ui": "knob", "scale": "log", "group": "Main"},
                {"ident": "1:bypass", "name_regex": "Bypass", "role": "bypass", "ui": "toggle", "scale": "indexed", "group": "Main"},
                {"ident": "2:mode", "name_regex": "Mode", "role": "shape", "ui": "select", "scale": "indexed", "group": "Main"},
                {"ident": null, "name_regex": "Macro 1", "role": "macro", "ui": "knob", "scale": "linear", "group": "Main"},
                {"ident": null, "name_regex": "Matrix", "role": "preset_only", "ui": "hidden", "scale": "indexed", "group": "Preset"}
            ]
        }"#,
        )
        .expect("test profile must validate")
    }

    fn plan_of(ops: Vec<PatchOp>) -> PatchPlan {
        PatchPlan {
            ops,
            target_snapshot: "snap-test".to_owned(),
        }
    }

    fn set_op(path: &str, value: serde_json::Value) -> PatchOp {
        PatchOp {
            op: PatchOpKind::Replace,
            path: IdentPath::new(path),
            value,
        }
    }

    #[test]
    fn op_kind_wire_spellings_roundtrip() {
        for (text, expected) in [
            ("add", PatchOpKind::Add),
            ("remove", PatchOpKind::Remove),
            ("replace", PatchOpKind::Replace),
            ("move", PatchOpKind::Invalid),
            ("ADD", PatchOpKind::Invalid),
            ("", PatchOpKind::Invalid),
        ] {
            let doc = format!(r#"{{"op": {text:?}, "path": "param/x", "value": 0.5}}"#);
            let op: PatchOp = serde_json::from_str(&doc).expect("op doc must parse");
            assert_eq!(op.op, expected, "spelling {text:?}");
            assert_eq!(
                op.op.is_setter(),
                matches!(expected, PatchOpKind::Add | PatchOpKind::Replace)
            );
        }
        assert_eq!(PatchOpKind::Add.as_str(), "add");
        assert_eq!(PatchOpKind::Invalid.as_str(), "invalid");
    }

    #[test]
    fn path_grammar_accepts_ident_and_macro_forms() {
        for raw in [
            "param/0:cutoff",
            "param/Cutoff",
            "param/11:wet",
            "macro/Macro 1",
            "macro/15:_Global_Gain",
        ] {
            assert!(
                IdentPath::new(raw).parse().is_ok(),
                "{raw} must parse, got {:?}",
                IdentPath::new(raw).parse()
            );
        }
    }

    #[test]
    fn path_grammar_rejects_bare_indices_and_pointer_style() {
        for (raw, expected) in [
            ("/0", PathRejection::BadPath),
            ("0", PathRejection::BadPath),
            ("param", PathRejection::BadPath),
            ("param/", PathRejection::BadPath),
            ("params/0:cutoff", PathRejection::BadPath),
            ("param/a/b", PathRejection::BadPath),
            ("param/4", PathRejection::BareIndex),
            ("param/11", PathRejection::BareIndex),
            ("macro/4", PathRejection::BareIndex),
        ] {
            assert_eq!(
                IdentPath::new(raw).parse(),
                Err(expected),
                "{raw} misclassified"
            );
        }
    }

    #[test]
    fn whitelist_is_every_role_but_preset_only() {
        assert!(is_whitelisted_role(SoundRole::Cutoff));
        assert!(is_whitelisted_role(SoundRole::Macro));
        assert!(is_whitelisted_role(SoundRole::Bypass));
        assert!(!is_whitelisted_role(SoundRole::PresetOnly));
    }

    #[test]
    fn valid_ops_pass_both_phases() {
        let profile = test_profile();
        let plan = plan_of(vec![
            set_op("param/0:cutoff", serde_json::json!(0.5)),
            set_op("param/Cutoff", serde_json::json!(0.25)),
            set_op("macro/Macro 1", serde_json::json!(1.0)),
            PatchOp {
                op: PatchOpKind::Replace,
                path: IdentPath::new("param/1:bypass"),
                value: serde_json::Value::Bool(true),
            },
            PatchOp {
                op: PatchOpKind::Replace,
                path: IdentPath::new("param/2:mode"),
                value: serde_json::json!("Band"),
            },
            PatchOp {
                op: PatchOpKind::Remove,
                path: IdentPath::new("param/0:cutoff"),
                value: serde_json::Value::Null,
            },
        ]);
        assert!(validate(&plan, &profile).is_empty());
    }

    #[test]
    fn semantic_errors_cover_unresolvable_role_and_preset_only() {
        let profile = test_profile();
        let plan = plan_of(vec![
            set_op("param/99:nope", serde_json::json!(0.5)),
            set_op("macro/Cutoff", serde_json::json!(0.5)),
            set_op("param/Matrix", serde_json::json!(1)),
        ]);
        let errors = validate(&plan, &profile);
        let kinds: Vec<PatchErrorKind> = errors.iter().map(|error| error.kind).collect();
        assert!(kinds.contains(&PatchErrorKind::UnresolvableIdent));
        assert!(kinds.contains(&PatchErrorKind::RoleNotWhitelisted));
        assert!(kinds.contains(&PatchErrorKind::PresetOnlySingleParam));
        for error in &errors {
            assert!(error.blocked());
            assert!(!error.guidance().is_empty());
        }
    }

    #[test]
    fn repair_round_is_pure_and_value_free_audited() {
        let profile = test_profile();
        let plan = plan_of(vec![
            set_op("param/0:cutoff", serde_json::json!(1.5)),
            set_op("param/99:nope", serde_json::json!(0.5)),
            set_op("param/1:bypass", serde_json::json!("off")),
        ]);
        let before = plan.clone();
        let errors = validate(&plan, &profile);
        let (first, first_report) = repair_round(&plan, &errors);
        let (second, second_report) = repair_round(&plan, &errors);
        assert_eq!(first, second, "repair must be deterministic");
        assert_eq!(first_report, second_report);
        assert_eq!(plan, before, "repair must not mutate its input");
        assert_eq!(first_report.removed, vec![1]);
        assert_eq!(first_report.replaced, vec![0, 2]);
        assert!(first_report.residual_errors.is_empty());
        assert_eq!(first.ops.len(), 2);
        assert_eq!(first.ops[0].value, serde_json::json!(1.0));
        assert_eq!(first.ops[1].value, serde_json::Value::Bool(false));
    }

    #[test]
    fn chained_string_then_clamp_needs_two_rounds() {
        let profile = test_profile();
        let plan = plan_of(vec![set_op("param/0:cutoff", serde_json::json!("2.5"))]);
        let outcome = validate_with_repair(&plan, &profile, DEFAULT_MAX_REPAIR_ROUNDS);
        assert!(outcome.accepted());
        assert_eq!(outcome.rounds_used(), 2);
        assert_eq!(outcome.plan.ops.len(), 1);
        assert_eq!(outcome.plan.ops[0].value, serde_json::json!(1.0));
    }

    #[test]
    fn residual_errors_are_blocked_terminal() {
        let profile = test_profile();
        let plan = plan_of(vec![
            set_op("param/99:nope", serde_json::json!(0.5)),
            set_op("param/0:cutoff", serde_json::Value::Bool(true)),
        ]);
        let outcome = validate_with_repair(&plan, &profile, DEFAULT_MAX_REPAIR_ROUNDS);
        assert!(!outcome.accepted());
        assert!(!outcome.residual_errors.is_empty());
        for error in &outcome.residual_errors {
            assert!(error.blocked());
            assert!(!error.code().retryable());
        }
        assert!(outcome.plan.ops.is_empty());
    }

    #[test]
    fn all_patch_errors_are_blocked_with_guidance() {
        let kinds = [
            PatchErrorKind::InvalidOp,
            PatchErrorKind::BadPath,
            PatchErrorKind::BareIndex,
            PatchErrorKind::MissingValue,
            PatchErrorKind::UnexpectedValue,
            PatchErrorKind::InvalidValueType,
            PatchErrorKind::EmptySnapshot,
            PatchErrorKind::EmptyOps,
            PatchErrorKind::UnresolvableIdent,
            PatchErrorKind::RoleNotWhitelisted,
            PatchErrorKind::PresetOnlySingleParam,
            PatchErrorKind::TypeMismatch,
            PatchErrorKind::OutOfRange,
            PatchErrorKind::CoerceToNumber,
            PatchErrorKind::CoerceToBool,
            PatchErrorKind::CoerceToInt,
        ];
        for kind in kinds {
            let error = PatchError {
                op_index: Some(0),
                kind,
            };
            assert!(error.blocked(), "{kind:?} must be BLOCKED");
            assert!(!error.code().retryable(), "{kind:?} must never retry");
            assert!(!error.guidance().is_empty());
        }
        let plan_level = PatchError {
            op_index: None,
            kind: PatchErrorKind::EmptySnapshot,
        };
        assert!(plan_level.blocked());
        assert!(!format!("{plan_level}").is_empty());
    }
}
