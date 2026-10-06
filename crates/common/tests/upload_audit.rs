//! TSK-106: upload-field whitelist audit integration tests.
//!
//! Covers DEC-011 (whitelist `[prompt, mir, meta]`, PCM stays local) and
//! DEC-026 (audit is exactly five secret-free fields) through the canonical
//! implementations only:
//!
//! - [`synthlm_common::config::validate_upload_fields`] for allow/deny,
//! - [`synthlm_common::config::Config::audit_event`] for audit construction,
//! - [`synthlm_common::ipc::AuditEvent`] five-field shape,
//! - [`synthlm_common::consent::require_consent`] for the pre-audit gate.
//!
//! No second whitelist is introduced here. All values are synthetic; the real
//! `.env` and the real user directory are never touched.

use std::collections::HashSet;

use synthlm_common::config::{
    ALLOWED_UPLOAD_FIELDS, ApiKey, Config, ConfigError, DEFAULT_BASE_URL, DEFAULT_TIER1_MODEL,
    DEFAULT_TIER2_MODEL, DEFAULT_TIER3_MODEL, validate_upload_fields,
};
use synthlm_common::consent::{ConsentError, ConsentState, ConsentStore, require_consent};
use synthlm_common::ipc::{ConsentTier, ErrorCode};

// ---------------------------------------------------------------------------
// Helpers (synthetic values only; no environment, no filesystem)
// ---------------------------------------------------------------------------

/// Clearly fake key material used as a *value* (never read from disk).
const SYNTHETIC_KEY: &str = "tsk106-synthetic-key-AAAA";
/// Second fake secret, used to prove error paths never echo caller input.
const SYNTHETIC_SECRET: &str = "tsk106-synthetic-secret-BBBB";

/// Build a [`Config`] without touching process environment.
///
/// The injectable `from_lookup` constructor is private, so integration tests
/// assemble the struct literally from synthetic parts instead of reading env.
fn synthetic_config(tier: ConsentTier) -> Config {
    Config {
        tier,
        api_key: ApiKey::new(SYNTHETIC_KEY.to_owned()).expect("synthetic key valid"),
        base_url: DEFAULT_BASE_URL.to_owned(),
        tier1_model: DEFAULT_TIER1_MODEL.to_owned(),
        tier2_model: DEFAULT_TIER2_MODEL.to_owned(),
        tier3_model: DEFAULT_TIER3_MODEL.to_owned(),
    }
}

fn owned(fields: &[&str]) -> Vec<String> {
    fields.iter().map(|f| (*f).to_owned()).collect()
}

// ---------------------------------------------------------------------------
// Whitelist: canonical value + allow-list cases
// ---------------------------------------------------------------------------

#[test]
fn whitelist_constant_pins_dec011_members() {
    assert_eq!(ALLOWED_UPLOAD_FIELDS, &["prompt", "mir", "meta"]);
}

#[test]
fn whitelist_singletons_and_legal_combos_all_green() {
    // Singletons.
    for field in ["prompt", "mir", "meta"] {
        validate_upload_fields(&owned(&[field])).expect("whitelisted singleton must pass");
    }
    // Legal combinations, including order permutations, duplicates, and the
    // empty set (zero fields leave the machine: vacuously allowed).
    for combo in [
        vec!["prompt", "mir"],
        vec!["mir", "meta"],
        vec!["prompt", "meta"],
        vec!["prompt", "mir", "meta"],
        vec!["meta", "mir", "prompt"],
        vec!["prompt", "prompt"],
        vec![],
    ] {
        validate_upload_fields(&owned(&combo)).expect("legal combo must pass");
    }
    // Same allow-list through the canonical audit constructor.
    let config = synthetic_config(ConsentTier::Tier1);
    let event = config
        .audit_event(owned(&["prompt", "mir", "meta"]), 4096)
        .expect("whitelist combo builds audit");
    assert_eq!(event.fields, owned(&["prompt", "mir", "meta"]));
    assert_eq!(event.byte_count, 4096);
}

// ---------------------------------------------------------------------------
// Whitelist: deny-list cases (each must be BLOCKED, value-free)
// ---------------------------------------------------------------------------

#[test]
fn validator_rejects_pcm_and_key_shaped_fields() {
    for field in ["pcm", "api_key", "OPENCODE_API_KEY", "key"] {
        let err = validate_upload_fields(&owned(&[field])).expect_err("must reject");
        assert_eq!(err, ConfigError::WhitelistViolation { index: 0 });
        assert_eq!(err.code(), ErrorCode::WhitelistViolation);
        assert!(err.blocked());
        assert!(!err.code().retryable());
    }
}

#[test]
fn validator_rejects_raw_key_value_itself() {
    // The secret *value* as a field name is still just "not in table".
    let err = validate_upload_fields(&owned(&[SYNTHETIC_KEY])).expect_err("raw key must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 0 });
    // Value-free error: neither rendering echoes the secret.
    assert!(!format!("{err}").contains(SYNTHETIC_KEY));
    assert!(!format!("{err:?}").contains(SYNTHETIC_KEY));
    assert!(!err.guidance().contains(SYNTHETIC_KEY));
}

#[test]
fn validator_rejects_prompt_fulltext_as_unknown_field_not_length() {
    // A realistic over-long prompt *body* passed where a field *name* belongs.
    // The current validator has no length branch: the only possible rejection
    // is `WhitelistViolation` ("field name not in table").
    let long_prompt = format!("Write a lush pad: {}", "ah ".repeat(4096));
    assert!(long_prompt.len() > 8000);
    let err = validate_upload_fields(std::slice::from_ref(&long_prompt))
        .expect_err("prompt body must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 0 });
    assert_eq!(err.code(), ErrorCode::WhitelistViolation);
    // Even a body that *contains* the word "prompt" is not the field "prompt".
    let prefixed = format!("prompt: {}", "x".repeat(8192));
    let err = validate_upload_fields(&[prefixed]).expect_err("prefixed body must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 0 });
    // The exact field name stays accepted: proof the gate is membership, not size.
    validate_upload_fields(&owned(&["prompt"])).expect("field name still accepted");
}

#[test]
fn validator_rejects_absolute_path_shaped_fields() {
    for path in [
        r"C:\Users\test\prompt.txt",
        "/home/user/audio.wav",
        "%APPDATA%\\SynthLM\\consent.json",
    ] {
        let err = validate_upload_fields(&owned(&[path])).expect_err("absolute path must reject");
        assert_eq!(err, ConfigError::WhitelistViolation { index: 0 });
        assert_eq!(err.code(), ErrorCode::WhitelistViolation);
        assert!(err.blocked());
    }
}

#[test]
fn whitelist_violation_index_points_at_offender() {
    // First offender wins; the index locates it (AGENTS.md §5 coverage rule).
    let err = validate_upload_fields(&owned(&["prompt", "pcm"])).expect_err("pcm at 1 must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 1 });

    let err = validate_upload_fields(&owned(&["pcm", "prompt"])).expect_err("pcm at 0 must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 0 });

    let err = validate_upload_fields(&owned(&["prompt", "mir", "api_key"]))
        .expect_err("api_key at 2 must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 2 });

    // Same index contract through the canonical audit constructor.
    let config = synthetic_config(ConsentTier::Tier1);
    let err = config
        .audit_event(owned(&["prompt", SYNTHETIC_SECRET]), 128)
        .expect_err("secret at 1 must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 1 });
}

// ---------------------------------------------------------------------------
// Normalization: record the current exact-match behavior as-is
// ---------------------------------------------------------------------------

#[test]
fn validator_has_no_case_or_whitespace_normalization() {
    // PENDING (TSK-106 open question): `validate_upload_fields` compares with
    // exact `==` against `ALLOWED_UPLOAD_FIELDS`; unlike `parse_consent_tier`
    // it does NOT trim or ASCII-lowercase. This test pins the *current*
    // behavior (reject) so a future normalization change fails loudly and
    // forces a human decision per AGENTS.md §7. Do NOT "fix" the
    // implementation to satisfy this test.
    for variant in [
        "Prompt", "PROMPT", "Mir", "MIR", "Meta", "META", " prompt", "prompt ", " prompt ",
        "mir\n", "\tm meta",
    ] {
        let err = validate_upload_fields(&owned(&[variant]))
            .expect_err("case/whitespace variant currently rejects");
        assert_eq!(
            err,
            ConfigError::WhitelistViolation { index: 0 },
            "{variant:?}"
        );
    }
    // Contrast: the tier parser *does* normalize (proves the asymmetry above
    // is validator-specific, not a crate-wide convention).
    assert!(synthlm_common::config::parse_consent_tier("  TIER2 ").is_ok());
}

// ---------------------------------------------------------------------------
// Audit event: exactly five secret-free fields
// ---------------------------------------------------------------------------

#[test]
fn audit_event_serialization_has_exactly_five_secret_free_fields() {
    let config = synthetic_config(ConsentTier::Tier2);
    let event = config
        .audit_event(owned(&["prompt", "mir"]), 2048)
        .expect("whitelisted audit builds");
    assert_eq!(event.tier, ConsentTier::Tier2);
    assert_eq!(event.model, DEFAULT_TIER2_MODEL);

    let value = serde_json::to_value(&event).expect("serialize audit");
    let obj = value.as_object().expect("audit is an object");
    let keys: HashSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        HashSet::from(["ts_unix_ms", "model", "tier", "fields", "byte_count"]),
        "audit schema must stay at exactly five fields (DEC-026)"
    );
    let text = serde_json::to_string(&event).expect("render audit");
    assert!(!text.contains("key"), "audit must not leak keys: {text}");
    assert!(!text.contains("pcm"), "audit must not leak audio: {text}");
    assert!(
        !text.contains(SYNTHETIC_KEY),
        "audit must not leak key value: {text}"
    );
    assert!(
        !text.contains(SYNTHETIC_SECRET),
        "audit must not leak caller secrets: {text}"
    );
}

#[test]
fn audit_event_rejects_before_build_and_errors_stay_value_free() {
    let config = synthetic_config(ConsentTier::Tier1);
    // PCM never reaches the record: construction fails first.
    let err = config
        .audit_event(owned(&["prompt", "pcm"]), 64)
        .expect_err("pcm audit must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 1 });
    assert_eq!(err.code(), ErrorCode::WhitelistViolation);
    assert!(err.blocked());
    // Absolute paths are equally barred from the record.
    let err = config
        .audit_event(owned(&[r"C:\Users\test\prompt.txt"]), 64)
        .expect_err("path audit must reject");
    assert_eq!(err, ConfigError::WhitelistViolation { index: 0 });
    assert!(!format!("{err:?}").contains(r"C:\Users\test\prompt.txt"));
}

// ---------------------------------------------------------------------------
// Consent gate: audit only after `require_consent` passes (existing API only)
// ---------------------------------------------------------------------------

#[test]
fn undecided_consent_blocks_before_audit() {
    // Fail-closed: no stored choice means no cloud call, hence no audit.
    let state = ConsentState::Undecided;
    let err = require_consent(&state).expect_err("undecided must BLOCK");
    assert_eq!(err, ConsentError::ConsentRequired);
    assert_eq!(err.code(), ErrorCode::ConsentRequired);
    assert!(err.blocked());
    assert!(!err.code().retryable());
    // The audit constructor is deliberately NOT reached on this path: callers
    // (model-gw, TSK-301) must gate on `require_consent` first. No new gate
    // is introduced in this test; the ordering *is* the gate.
}

#[test]
fn decided_consent_passes_gate_then_audit_builds() {
    for tier in [ConsentTier::Tier1, ConsentTier::Tier2] {
        let state = ConsentState::Decided(ConsentStore::new(tier));
        let allowed = require_consent(&state).expect("decided tier passes gate");
        assert_eq!(allowed, tier);
        // The tier returned by the gate drives the audit record.
        let config = synthetic_config(allowed);
        let event = config
            .audit_event(owned(&["prompt", "mir", "meta"]), 512)
            .expect("audit builds after gate");
        assert_eq!(event.tier, tier);
    }
    // PENDING (门禁归属): `Config::audit_event` itself does NOT check consent
    // or `upload_allowed` — a Tier3 config still builds a record below. The
    // enforcement point is the caller-side `require_consent` gate above (plus
    // model-gw policy in TSK-301), not the audit constructor. Recorded here
    // so a future "audit enforces tier" change is a conscious ADR, not drift.
    let tier3 = synthetic_config(ConsentTier::Tier3);
    assert!(!tier3.upload_allowed());
    tier3
        .audit_event(owned(&["prompt"]), 8)
        .expect("audit constructor currently tier-agnostic");
}
