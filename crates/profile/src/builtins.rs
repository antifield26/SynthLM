//! Compile-time embedded factory profiles (TSK-201 first batch).
//!
//! Each entry pairs a short builtin name with the JSON source embedded via
//! `include_str!`, so the profiles ship inside the binary and tests never
//! depend on the process working directory. Per-user overrides live in the
//! user directory (DEC-027) and layer on top of these built-ins in a later
//! task; the factory set itself is immutable.
//!
//! Evidence provenance: every `ident` / `name_regex` below is traceable to
//! the `experiments/b-matrix-*.out.txt` capture named in the per-builtin
//! test comments. Pin tests assert at least three such anchors per plugin so
//! a typo or an invented parameter name fails the suite instead of shipping.

use crate::schema::{Profile, ProfileError};

/// (builtin name, embedded JSON source) table.
pub fn builtin_table() -> [(&'static str, &'static str); 7] {
    [
        ("reaeq", include_str!("../profiles/reaeq.json")),
        ("ott", include_str!("../profiles/ott.json")),
        ("pro-q4", include_str!("../profiles/pro-q4.json")),
        ("serum2-fx", include_str!("../profiles/serum2-fx.json")),
        ("vital-clap", include_str!("../profiles/vital-clap.json")),
        (
            "js-general-dynamics",
            include_str!("../profiles/js-general-dynamics.json"),
        ),
        (
            "reacontrolmidi",
            include_str!("../profiles/reacontrolmidi.json"),
        ),
    ]
}

/// Loads and validates one built-in profile by name.
pub fn load_builtin(name: &str) -> Result<Profile, ProfileError> {
    for (builtin_name, source) in builtin_table() {
        if builtin_name == name {
            return Profile::from_json(source);
        }
    }
    Err(ProfileError::Json(format!(
        "unknown builtin profile {name:?}"
    )))
}

/// Loads and validates every built-in profile.
pub fn load_all_builtins() -> Result<Vec<(&'static str, Profile)>, ProfileError> {
    let mut loaded = Vec::with_capacity(builtin_table().len());
    for (name, source) in builtin_table() {
        loaded.push((name, Profile::from_json(source)?));
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CURRENT_SCHEMA_VERSION, SoundRole};

    fn must_load(name: &str) -> Profile {
        load_builtin(name).expect("builtin profile must load and validate")
    }

    /// Factory policy (DEC-015): 8–16 macros per plugin, and at least one
    /// entry carrying a macro or preset-only role so every file documents its
    /// L2 whitelist entry point.
    fn assert_factory_shape(name: &str, profile: &Profile) {
        assert!(
            (8..=16).contains(&profile.params.len()),
            "{name}: expected 8-16 params, got {}",
            profile.params.len()
        );
        assert!(
            profile
                .params
                .iter()
                .any(|p| matches!(p.role, SoundRole::Macro | SoundRole::PresetOnly)),
            "{name}: no macro or preset_only role declared"
        );
    }

    fn assert_json_roundtrip(name: &str, profile: &Profile) {
        let json = profile.to_json().expect("serialization failed");
        let back = Profile::from_json(&json).expect("roundtrip parse failed");
        assert_eq!(*profile, back, "{name}: roundtrip mismatch");
        assert_eq!(back.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn all_builtins_load_validate_and_roundtrip() {
        let loaded = load_all_builtins().expect("builtins must all load");
        assert_eq!(loaded.len(), 7);
        for (name, profile) in &loaded {
            profile.validate().expect("builtin failed validation");
            assert_factory_shape(name, profile);
            assert_json_roundtrip(name, profile);
        }
    }

    #[test]
    fn unknown_builtin_name_rejected() {
        assert!(matches!(
            load_builtin("no-such-plugin"),
            Err(ProfileError::Json(_))
        ));
    }

    #[test]
    fn unknown_schema_version_rejected_at_load() {
        let (_, source) = builtin_table()[0];
        let tampered = source.replace("\"schema_version\": 1", "\"schema_version\": 999");
        assert_ne!(tampered, source, "tamper fixture did not apply");
        assert!(matches!(
            Profile::from_json(&tampered),
            Err(ProfileError::VersionMismatch {
                expected: CURRENT_SCHEMA_VERSION,
                found: 999
            })
        ));
    }

    // Evidence pins: each asserts idents / name patterns that appear
    // verbatim in the cited b-matrix capture, so invented parameter names
    // cannot slip in. Citations are `file:line` of the `ident=` / `name=`
    // rows (evidence dated 2026-10-06, REAPER v7.82).

    #[test]
    fn reaeq_pins() {
        // b-matrix-01-stock.out.txt: ReaEQ, 19 params.
        let p = must_load("reaeq");
        for ident in ["0:_Freq_Low_Shelf", "4:_Gain_Band_2", "17:wet"] {
            // Lines 8, 44, 161 (`ident=true|…`).
            assert!(
                p.param_by_ident(ident).is_some(),
                "reaeq missing ident {ident}"
            );
        }
        for regex in ["Freq-Low Shelf", "Gain-Band 2", "Wet"] {
            // Lines 3, 39, 157 (`name=true|…`).
            assert!(
                p.param_by_name_regex(regex).is_some(),
                "reaeq missing name pattern {regex}"
            );
        }
        assert_factory_shape("reaeq", &p);
    }

    #[test]
    fn ott_pins() {
        // b-matrix-03-vst3.out.txt: VST3 OTT, 24 params, idx:paramID idents.
        let p = must_load("ott");
        for ident in ["0:0", "1:1", "2:2"] {
            // Lines 8, 16, 24.
            assert!(p.param_by_ident(ident).is_some(), "ott missing {ident}");
        }
        for regex in ["Depth", "Time", "In Gain"] {
            // Lines 4, 12, 20.
            assert!(
                p.param_by_name_regex(regex).is_some(),
                "ott missing {regex}"
            );
        }
        assert_factory_shape("ott", &p);
    }

    #[test]
    fn pro_q4_pins() {
        // b-matrix-03-vst3.out.txt: Pro-Q 4, 740 params, section `Band 1`.
        let p = must_load("pro-q4");
        for ident in ["2:2", "3:3", "13:13"] {
            // Lines 189, 197, 277.
            assert!(p.param_by_ident(ident).is_some(), "pro-q4 missing {ident}");
        }
        for regex in ["Band 1 Frequency", "Band 1 Gain", "Band 1 Attack"] {
            // Lines 185, 193, 273.
            assert!(
                p.param_by_name_regex(regex).is_some(),
                "pro-q4 missing {regex}"
            );
        }
        assert_factory_shape("pro-q4", &p);
    }

    #[test]
    fn serum2_fx_pins() {
        // b-matrix-03-vst3.out.txt: Serum 2 FX, 2625 params (first 20 shown).
        let p = must_load("serum2-fx");
        for ident in ["18:19", "19:20", "8:8"] {
            // Lines 482 (Bus 1 Vol), 490 (Bus 2 Vol), 402 (Mod Wheel).
            assert!(
                p.param_by_ident(ident).is_some(),
                "serum2-fx missing {ident}"
            );
        }
        for regex in ["Bus 1 Vol", "Bus 2 Vol", "Mod Wheel"] {
            // Lines 478, 486, 398.
            assert!(
                p.param_by_name_regex(regex).is_some(),
                "serum2-fx missing {regex}"
            );
        }
        // Mod-matrix routing is chunk-only (B §4): preset_only entry present.
        assert!(
            p.params.iter().any(|e| e.role == SoundRole::PresetOnly),
            "serum2-fx missing preset_only matrix entry"
        );
        assert_factory_shape("serum2-fx", &p);
    }

    #[test]
    fn vital_clap_pins() {
        // b-matrix-04-clap.out.txt: CLAP Vital, 906 params, section empty.
        let p = must_load("vital-clap");
        for ident in ["1:49", "6:54", "7:55"] {
            // Lines 14, 50, 56.
            assert!(
                p.param_by_ident(ident).is_some(),
                "vital-clap missing {ident}"
            );
        }
        for regex in [
            "Chorus Filter Cutoff",
            "Chorus Frequency",
            "Chorus Mod Depth",
            "Macro 1",
            "Macro 2",
            "Macro 3",
        ] {
            // Name rows lines 11, 46, 53; macros at indices 211-213,
            // lines 75-77 (ident unknown → name_regex only).
            assert!(
                p.param_by_name_regex(regex).is_some(),
                "vital-clap missing {regex}"
            );
        }
        // Macro 1-3 are live macro-role entries (knob-addressable via
        // macro/ paths). No Macro 4 exists: the evidence matrix reports
        // found=3, so none is invented here (TSK-604).
        for macro_name in ["Macro 1", "Macro 2", "Macro 3"] {
            let entry = p
                .param_by_name_regex(macro_name)
                .expect("vital-clap missing macro");
            assert_eq!(
                entry.role,
                SoundRole::Macro,
                "{macro_name} must be macro-role"
            );
            assert!(
                entry.ident.is_none(),
                "{macro_name} must stay ident-less (index-based CLAP IDs)"
            );
        }
        assert_factory_shape("vital-clap", &p);
    }

    #[test]
    fn js_general_dynamics_pins() {
        // b-matrix-01-stock.out.txt (JS section, 13 params) +
        // b-matrix-02-ident-env.out.txt (bare idents, :wet/:bypass/:delta).
        let p = must_load("js-general-dynamics");
        // Ordinary JSFX idents are bare numbers (lines 566, 602).
        for ident in ["4", "8"] {
            let entry = p
                .param_by_ident(ident)
                .expect("js-general-dynamics missing bare ident");
            assert!(
                entry.name_regex.is_some(),
                "bare ident {ident} lacks name_regex pairing"
            );
        }
        // Semantic REAPER idents (b-matrix-02 lines 11-13).
        assert!(
            p.param_by_ident("11:wet").is_some(),
            "js-general-dynamics missing 11:wet"
        );
        for regex in [
            "Input Attack \\(ms\\)",
            "Wet Mix \\(dB\\)",
            "Input Release \\(ms\\)",
        ] {
            // Lines 561, 597, 570 (`name=true|…`).
            assert!(
                p.param_by_name_regex(regex).is_some(),
                "js-general-dynamics missing {regex}"
            );
        }
        assert_factory_shape("js-general-dynamics", &p);
    }

    #[test]
    fn reacontrolmidi_pins() {
        // b-matrix-01-stock.out.txt: ReaControlMIDI, 18 params.
        let p = must_load("reacontrolmidi");
        for ident in ["8:_通道", "2:_Program", "10:_Snap_to_Scale"] {
            // Lines 254, 200, 272.
            assert!(
                p.param_by_ident(ident).is_some(),
                "reacontrolmidi missing {ident}"
            );
        }
        for regex in ["通道", "Program", "Snap to Scale"] {
            // Lines 249, 195, 267.
            assert!(
                p.param_by_name_regex(regex).is_some(),
                "reacontrolmidi missing {regex}"
            );
        }
        assert_factory_shape("reacontrolmidi", &p);
    }
}
