//! SynthLM parameter-semantics layer: plugin profiles and whitelists.
//!
//! Implements TSK-201 under DEC-015 and `docs/ARCHITECTURE.md` §6:
//! every supported plugin ships a versioned JSON [`schema::Profile`]
//! that maps a small (8–16) set of whitelist macros to stable REAPER
//! parameter addresses. Live addresses use `ident` (see
//! [`TrackFX_GetParamIdent`](https://www.reaper.fm/sdk/reascript/reascripthelp.html),
//! verified 2026-10-05); bare FX indices are never persisted.
//!
//! Built-in profiles live in `crates/profile/profiles/*.json` and are embedded
//! at compile time via [`crate::builtins`]. Rationale for `include_str!` over runtime
//! file loading: profiles are versioned together with the code that validates
//! them, tests never depend on the process working directory, and there is no
//! startup file-lookup failure mode. Per-user overrides (DEC-027,
//! `%APPDATA%/SynthLM`) layer on top of these built-ins in a later task.

pub mod builtins;
pub mod schema;

pub use builtins::{builtin_table, load_all_builtins, load_builtin};
pub use schema::{
    CURRENT_SCHEMA_VERSION, ParamEntry, Profile, ProfileError, Scale, SoundRole, UiHint,
    is_bare_ident,
};
