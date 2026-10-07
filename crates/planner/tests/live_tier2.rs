//! Live Tier2 connectivity smoke (human-authorized, TSK-116 verification).
//!
//! The two live tests (`live_tier2_transport_smoke`, `live_tier2_envelope_shape`)
//! run ONLY on explicit request (`cargo test -p synthlm-planner --test
//! live_tier2 -- --ignored --nocapture`); never in CI. They read the key from
//! process environment (launcher-injected `.env`) and print no secret
//! material: only status codes, JSON top-level key names, lengths, and
//! match booleans. Tier2 (`mimo-v2.6-flash`, ZDR) first per least-exposure
//! order. Human authorization for this exact invocation is on record;
//! the consent dialog (TSK-105) gates all product paths.
//!
//! `live_consent_gate_still_holds` is offline by construction (pure consent
//! gate, no transport, no key) and therefore runs in the default suite.

use synthlm_common::consent::{ConsentState, require_consent};
use synthlm_common::ipc::{ConsentTier, TimeoutConfig};
use synthlm_planner::model_gw::{HttpsTransport, ModelRequest, Transport};

const BASE_URL: &str = "https://opencode.ai/zen/go/v1";

fn live_key() -> String {
    std::env::var("OPENCODE_API_KEY").expect("key must be injected, never printed")
}

#[test]
#[ignore = "live network call to the Tier2 endpoint (mimo-v2.6-flash, ZDR) and needs OPENCODE_API_KEY; run with --ignored --nocapture"]
fn live_tier2_transport_smoke() {
    let key = live_key();
    assert!(!key.trim().is_empty(), "key must be non-blank");
    let mut transport = HttpsTransport::new(key, BASE_URL.to_owned())
        .expect("client builds offline")
        // Live-probed 2026-10-06: the Go surface requires x-opencode-session.
        .with_session_id("synthlm-live-001".to_owned());
    let request = ModelRequest::new(ConsentTier::Tier2, vec!["prompt".to_owned()], 64)
        .expect("whitelisted fields");
    let response = transport
        .send(&request, &TimeoutConfig::default())
        .expect("tier2 live call succeeds");
    assert_eq!(response.tier, ConsentTier::Tier2);
    assert!(!response.model.is_empty(), "model name echoed");
    println!(
        "live_ok=1 model_len={} latency_ms={}",
        response.model.len(),
        response.latency_ms
    );
}

#[test]
#[ignore = "live network call to the Tier2 endpoint (mimo-v2.6-flash, ZDR) and needs OPENCODE_API_KEY; run with --ignored --nocapture"]
fn live_tier2_envelope_shape() {
    // Raw shape probe: top-level JSON key names only, plus output-text
    // length and prefix-match boolean. No values, no prompt text logged.
    let key = live_key();
    let client = reqwest::blocking::Client::builder()
        .build()
        .expect("client builds");
    let body = serde_json::json!({
        "model": "mimo-v2.6-flash",
        "messages": [{"role": "user", "content": "Reply with exactly: LIVE-OK"}],
        "max_tokens": 256,
    });
    let response = client
        .post(format!("{BASE_URL}/chat/completions"))
        .bearer_auth(&key)
        .header("x-opencode-session", "synthlm-live-002")
        .timeout(std::time::Duration::from_secs(60))
        .json(&body)
        .send()
        .expect("posts");
    let status = response.status();
    println!("http_status={}", status.as_u16());
    assert!(status.is_success(), "non-2xx is itself the finding");
    let value: serde_json::Value = response.json().expect("JSON body");
    let keys: Vec<&String> = value.as_object().expect("object").keys().collect();
    println!("top_keys={keys:?}");
    let text = value.to_string();
    println!("body_len={}", text.len());
    println!("contains_live_ok={}", text.contains("LIVE-OK"));
}

#[test]
fn live_consent_gate_still_holds() {
    // Offline by construction: no transport, no key, no I/O — `require_consent`
    // is a pure match over the in-memory `ConsentState` (crates/common/src/
    // consent.rs, "Gate: require_consent": `Undecided` -> `ConsentRequired`,
    // never `Decided` -> cloud). This is the half of the TSK-116 smoke that
    // must stay honest on machines with no network or key, so it is not
    // `#[ignore]`d.
    //
    // The product gate is unchanged by the authorized probes: undecided
    // consent still BLOCKEDs before any transport use.
    let err = require_consent(&ConsentState::Undecided).expect_err("must block");
    assert!(err.blocked());
}
