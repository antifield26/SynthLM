//! Versioned plugin-profile schema (`Profile`) with fail-closed validation.
//!
//! Design notes (DEC-015, `docs/research/B-plugin-semantics.md` §4–§5):
//!
//! * `ident` is the only persistable live address. It is the exact string
//!   returned by `TrackFX_GetParamIdent` (e.g. `0:_Freq_Low_Shelf`,
//!   `17:wet`, VST3 `idx:paramID` such as `2:2`, CLAP `idx:clapID` such as
//!   `1:49`). Bare indices are never written to disk.
//! * JSFX ordinary parameters report a bare-numeric ident (`"4"`, …); only
//!   `:wet`/`:bypass`/`:delta` carry semantic idents there
//!   (`experiments/b-matrix-02-ident-env.out.txt`). A bare-numeric ident is
//!   therefore accepted only together with `name_regex`, so the entry stays
//!   resolvable if the slider order shifts.
//! * `role = preset_only` marks state that is reachable only via preset/chunk
//!   (e.g. Serum-style mod-matrix routing, which `get/set_parameter` cannot
//!   see — B §4). Such entries must not carry a live `ident` and must use the
//!   [`UiHint::Hidden`] widget, so planners can never emit a live parameter
//!   write for chunk-only state (DEC-013).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version accepted by [`Profile::validate`].
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Sound-semantic role of a whitelisted parameter.
///
/// Keyword roles come from B §4 (filter/cutoff/reso/drive/attack/decay/
/// sustain/release/lfo/rate/depth/env/mod/mix/wet/bypass) plus the two
/// whitelist control roles `macro` (a performance/macro knob, native or
/// designated master control) and `preset_only` (chunk-only state, never
/// written live). The remaining variants cover the discrete/mixer roles met
/// in the first whitelist batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundRole {
    /// Filter cutoff frequency.
    Cutoff,
    /// Filter resonance / Q emphasis.
    Resonance,
    /// Saturation / drive amount.
    Drive,
    /// Envelope attack time.
    Attack,
    /// Envelope decay time.
    Decay,
    /// Envelope sustain level.
    Sustain,
    /// Envelope release time.
    Release,
    /// LFO rate / chorus rate.
    LfoRate,
    /// LFO depth / chorus mod depth.
    LfoDepth,
    /// Detector / envelope-follower time constant.
    Env,
    /// Modulation source amount (e.g. mod wheel).
    Mod,
    /// Absolute frequency (EQ band, side-chain filter).
    Frequency,
    /// Gain (EQ band, in/out, dynamics gain).
    Gain,
    /// Compressor/expander threshold (or detector staging acting as one).
    Threshold,
    /// Compression strength / dynamic range.
    Dynamics,
    /// Filter shape (bell, shelf, …).
    Shape,
    /// Filter slope (dB/oct).
    Slope,
    /// Output / bus volume.
    Volume,
    /// Stereo pan / balance.
    Pan,
    /// Pitch / transpose amount.
    Pitch,
    /// Channel / routing selector.
    Channel,
    /// Bank / program selector.
    Program,
    /// Scale root / type.
    Scale,
    /// On/off or enable switch that is not bypass.
    Enable,
    /// Delay time.
    Delay,
    /// Portamento / glide time.
    Glide,
    /// Wet/dry blend where neither side alone is the master.
    Mix,
    /// Dry path level.
    Dry,
    /// Master wet amount.
    Wet,
    /// Plugin bypass switch.
    Bypass,
    /// Delta (dry/wet difference audition) switch.
    Delta,
    /// Whitelist macro: a native macro knob or the designated master control
    /// standing in for one on plugins without native macros.
    Macro,
    /// Chunk-only state (e.g. mod-matrix routing). Never addressed live;
    /// see the module docs. Mutually exclusive with `ident` and with any
    /// visible [`UiHint`].
    PresetOnly,
}

/// Widget hint for rendering a whitelisted parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiHint {
    /// Continuous horizontal/vertical slider.
    Slider,
    /// Continuous rotary knob.
    Knob,
    /// Two-state switch.
    Toggle,
    /// Multi-option selector.
    Select,
    /// No live widget; the entry is applied via preset/chunk only.
    /// Valid solely together with [`SoundRole::PresetOnly`].
    Hidden,
}

/// Value-mapping scale of a whitelisted parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scale {
    /// Uniform mapping over the normalized range.
    Linear,
    /// Logarithmic mapping (frequency, cutoff).
    Log,
    /// Stepped/discrete values (toggles, selectors, semitone lists).
    Indexed,
}

/// One whitelisted parameter: a stable address plus its sound semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamEntry {
    /// Stable ident from `TrackFX_GetParamIdent`, e.g. `17:wet`, `0:0`,
    /// `1:49`. `None` for [`SoundRole::PresetOnly`] entries and when only a
    /// display-name pattern is known (e.g. Vital CLAP macros whose numeric
    /// clap IDs are index-based). A bare-numeric JSFX ident (`"4"`) must be
    /// paired with `name_regex`.
    pub ident: Option<String>,
    /// Display-name pattern used when `ident` is absent or as a fallback
    /// resolver, e.g. `"Input Attack \\(ms\\)"`. Matched against
    /// `TrackFX_GetParamName`; the bridge compiles it as a regex, and
    /// [`Profile::validate`] rejects patterns that do not compile
    /// (fail-closed, TSK-604).
    pub name_regex: Option<String>,
    /// Legal labels for [`UiHint::Select`] entries, e.g. filter shapes.
    /// `None` (the shape of all current factory profiles: no ground-truth
    /// label list was captured in the b-matrix evidence, and labels are
    /// never invented) keeps the legacy lenient path: any string or
    /// non-negative integer index validates. `Some` enforces membership
    /// (see [`Profile::validate`]) and is only meaningful together with
    /// [`UiHint::Select`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Sound-semantic role (B §4 keyword set + macro/preset_only).
    pub role: SoundRole,
    /// Widget hint.
    pub ui: UiHint,
    /// Value-mapping scale.
    pub scale: Scale,
    /// Owning group; must be listed in [`Profile::groups`].
    pub group: String,
}

/// Versioned whitelist for one plugin (DEC-015, ARCHITECTURE §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Must equal [`CURRENT_SCHEMA_VERSION`]; anything else is rejected.
    pub schema_version: u32,
    /// Regex matched against the REAPER FX name (e.g. `"ReaEQ"`,
    /// `"CLAP.*Vital"` for the CLAP-only Vital profile, which must not match
    /// the VST3 variant whose parameter table is not portable — B §5).
    pub fx_ident_match: String,
    /// Group names referenced by [`ParamEntry::group`].
    pub groups: Vec<String>,
    /// Whitelist macros; factory profiles carry 8–16 entries (DEC-015).
    pub params: Vec<ParamEntry>,
}

/// Fail-closed validation / load errors for [`Profile`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProfileError {
    /// JSON parsing failed.
    #[error("profile JSON invalid: {0}")]
    Json(String),
    /// `schema_version` is not [`CURRENT_SCHEMA_VERSION`].
    #[error("unsupported schema_version {found}, expected {expected}")]
    VersionMismatch {
        /// Required version.
        expected: u32,
        /// Version seen in the document.
        found: u32,
    },
    /// `fx_ident_match` is empty.
    #[error("fx_ident_match must be non-empty")]
    EmptyFxMatch,
    /// `groups` is empty.
    #[error("groups must list at least one group")]
    EmptyGroups,
    /// `params` is empty.
    #[error("params must list at least one entry")]
    NoParams,
    /// Neither `ident` nor `name_regex` is set.
    #[error("params[{index}]: ident and name_regex are both missing")]
    MissingAddress {
        /// Offending entry index.
        index: usize,
    },
    /// `name_regex` is present but empty.
    #[error("params[{index}]: name_regex must be non-empty")]
    EmptyNameRegex {
        /// Offending entry index.
        index: usize,
    },
    /// `name_regex` is present but does not compile as a regex.
    #[error("params[{index}]: name_regex pattern does not compile: {pattern:?}")]
    InvalidNameRegex {
        /// Offending entry index.
        index: usize,
        /// The stored pattern text (config data, never caller material).
        pattern: String,
    },
    /// `options` is present but empty.
    #[error("params[{index}]: options must list at least one label")]
    OptionsEmpty {
        /// Offending entry index.
        index: usize,
    },
    /// A bare-numeric (JSFX-style) ident has no `name_regex` companion.
    #[error("params[{index}]: bare-numeric ident {ident:?} requires name_regex")]
    BareIdentWithoutNameRegex {
        /// Offending entry index.
        index: usize,
        /// The bare ident.
        ident: String,
    },
    /// A `preset_only` entry carries a live `ident`.
    #[error("params[{index}]: preset_only entries must not carry ident")]
    PresetOnlyWithIdent {
        /// Offending entry index.
        index: usize,
    },
    /// A `preset_only` entry uses a visible widget.
    #[error("params[{index}]: preset_only entries must use ui hidden")]
    PresetOnlyUiMismatch {
        /// Offending entry index.
        index: usize,
    },
    /// A live entry uses the chunk-only `hidden` widget.
    #[error("params[{index}]: hidden ui is reserved for preset_only entries")]
    HiddenUiMismatch {
        /// Offending entry index.
        index: usize,
    },
    /// `group` is not listed in `groups`.
    #[error("params[{index}]: unknown group {group:?}")]
    UnknownGroup {
        /// Offending entry index.
        index: usize,
        /// The unknown group name.
        group: String,
    },
    /// An `options` label is empty.
    #[error("params[{index}]: options labels must be non-empty")]
    EmptyOptionLabel {
        /// Offending entry index.
        index: usize,
    },
    /// An `options` label repeats.
    #[error("params[{index}]: options labels must be unique")]
    DuplicateOptionLabel {
        /// Offending entry index.
        index: usize,
    },
    /// `options` is present on a non-`select` entry.
    #[error("params[{index}]: options is only meaningful with ui select")]
    OptionsRequireSelect {
        /// Offending entry index.
        index: usize,
    },
    /// Serialization failed.
    #[error("profile serialization failed: {0}")]
    Serialize(String),
}

impl From<serde_json::Error> for ProfileError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err.to_string())
    }
}

/// Returns `true` for bare-numeric JSFX-style idents (`"4"`, `"11"`).
///
/// Bare idents encode slider position, not identity, so [`Profile::validate`]
/// requires them to be paired with `name_regex`. Idents containing a colon
/// (`"10:bypass"`, `"0:0"`, `"1:49"`) or a name suffix
/// (`"0:_Freq_Low_Shelf"`) are stable addresses, not bare indices.
pub fn is_bare_ident(ident: &str) -> bool {
    !ident.is_empty() && ident.bytes().all(|b| b.is_ascii_digit())
}

impl Profile {
    /// Parses JSON and runs [`Profile::validate`].
    pub fn from_json(text: &str) -> Result<Self, ProfileError> {
        let profile: Self = serde_json::from_str(text)?;
        profile.validate()?;
        Ok(profile)
    }

    /// Serializes to pretty JSON (round-trips through [`Profile::from_json`]).
    pub fn to_json(&self) -> Result<String, ProfileError> {
        serde_json::to_string_pretty(self).map_err(|err| ProfileError::Serialize(err.to_string()))
    }

    /// Fail-closed validation: version match, address rules (including
    /// regex compilation of every stored `name_regex`), JS bare-index
    /// rule, preset_only mutual exclusion, select-`options` shape,
    /// group membership.
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ProfileError::VersionMismatch {
                expected: CURRENT_SCHEMA_VERSION,
                found: self.schema_version,
            });
        }
        if self.fx_ident_match.is_empty() {
            return Err(ProfileError::EmptyFxMatch);
        }
        if self.groups.is_empty() {
            return Err(ProfileError::EmptyGroups);
        }
        if self.params.is_empty() {
            return Err(ProfileError::NoParams);
        }
        for (index, param) in self.params.iter().enumerate() {
            if param.ident.is_none() && param.name_regex.is_none() {
                return Err(ProfileError::MissingAddress { index });
            }
            if let Some(regex) = param.name_regex.as_ref()
                && regex.is_empty()
            {
                return Err(ProfileError::EmptyNameRegex { index });
            }
            if let Some(pattern) = param.name_regex.as_ref()
                && regex::Regex::new(pattern).is_err()
            {
                return Err(ProfileError::InvalidNameRegex {
                    index,
                    pattern: pattern.clone(),
                });
            }
            if let Some(ident) = param.ident.as_ref()
                && is_bare_ident(ident)
                && param.name_regex.is_none()
            {
                return Err(ProfileError::BareIdentWithoutNameRegex {
                    index,
                    ident: ident.clone(),
                });
            }
            if param.role == SoundRole::PresetOnly {
                if param.ident.is_some() {
                    return Err(ProfileError::PresetOnlyWithIdent { index });
                }
                if param.ui != UiHint::Hidden {
                    return Err(ProfileError::PresetOnlyUiMismatch { index });
                }
            } else if param.ui == UiHint::Hidden {
                return Err(ProfileError::HiddenUiMismatch { index });
            }
            if let Some(options) = param.options.as_ref() {
                if param.ui != UiHint::Select {
                    return Err(ProfileError::OptionsRequireSelect { index });
                }
                if options.is_empty() {
                    return Err(ProfileError::OptionsEmpty { index });
                }
                let mut seen = std::collections::HashSet::new();
                for label in options {
                    if label.is_empty() {
                        return Err(ProfileError::EmptyOptionLabel { index });
                    }
                    if !seen.insert(label.as_str()) {
                        return Err(ProfileError::DuplicateOptionLabel { index });
                    }
                }
            }
            if !self.groups.contains(&param.group) {
                return Err(ProfileError::UnknownGroup {
                    index,
                    group: param.group.clone(),
                });
            }
        }
        Ok(())
    }

    /// Finds an entry by exact `ident`.
    pub fn param_by_ident(&self, ident: &str) -> Option<&ParamEntry> {
        self.params
            .iter()
            .find(|p| p.ident.as_deref() == Some(ident))
    }

    /// Finds an entry by exact `name_regex` text.
    pub fn param_by_name_regex(&self, name_regex: &str) -> Option<&ParamEntry> {
        self.params
            .iter()
            .find(|p| p.name_regex.as_deref() == Some(name_regex))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_entry() -> ParamEntry {
        ParamEntry {
            ident: Some("17:wet".to_owned()),
            name_regex: Some("Wet".to_owned()),
            options: None,
            role: SoundRole::Wet,
            ui: UiHint::Slider,
            scale: Scale::Linear,
            group: "Master".to_owned(),
        }
    }

    fn valid_profile() -> Profile {
        Profile {
            schema_version: CURRENT_SCHEMA_VERSION,
            fx_ident_match: "ReaEQ".to_owned(),
            groups: vec!["Master".to_owned()],
            params: vec![valid_entry()],
        }
    }

    #[test]
    fn valid_profile_passes() {
        valid_profile().validate().expect("valid profile rejected");
    }

    #[test]
    fn unknown_version_rejected_before_anything_else() {
        let mut profile = valid_profile();
        profile.schema_version = CURRENT_SCHEMA_VERSION + 99;
        assert_eq!(
            profile.validate(),
            Err(ProfileError::VersionMismatch {
                expected: CURRENT_SCHEMA_VERSION,
                found: CURRENT_SCHEMA_VERSION + 99,
            })
        );
        let json = profile
            .to_json()
            .expect("serialization of test profile failed");
        assert!(matches!(
            Profile::from_json(&json),
            Err(ProfileError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn missing_address_rejected() {
        let mut profile = valid_profile();
        profile.params[0].ident = None;
        profile.params[0].name_regex = None;
        assert_eq!(
            profile.validate(),
            Err(ProfileError::MissingAddress { index: 0 })
        );
    }

    #[test]
    fn empty_name_regex_rejected() {
        let mut profile = valid_profile();
        profile.params[0].name_regex = Some(String::new());
        assert_eq!(
            profile.validate(),
            Err(ProfileError::EmptyNameRegex { index: 0 })
        );
    }

    #[test]
    fn uncompilable_name_regex_rejected_fail_closed() {
        let mut profile = valid_profile();
        profile.params[0].name_regex = Some("Cutoff (lo".to_owned());
        assert_eq!(
            profile.validate(),
            Err(ProfileError::InvalidNameRegex {
                index: 0,
                pattern: "Cutoff (lo".to_owned(),
            })
        );
    }

    #[test]
    fn escaped_name_regex_compiles() {
        // Mirrors JS General Dynamics: escaped parens are regex, not groups.
        let mut profile = valid_profile();
        profile.params[0].name_regex = Some("Input Attack \\(ms\\)".to_owned());
        profile.validate().expect("escaped pattern rejected");
    }

    #[test]
    fn select_options_shape_enforced() {
        fn select_profile(options: Option<Vec<String>>) -> Profile {
            let mut profile = valid_profile();
            profile.params[0].ui = UiHint::Select;
            profile.params[0].options = options;
            profile
        }
        select_profile(Some(vec!["Bell".to_owned(), "Shelf".to_owned()]))
            .validate()
            .expect("valid options rejected");
        assert_eq!(
            select_profile(Some(vec![])).validate(),
            Err(ProfileError::OptionsEmpty { index: 0 })
        );
        assert_eq!(
            select_profile(Some(vec!["Bell".to_owned(), String::new()])).validate(),
            Err(ProfileError::EmptyOptionLabel { index: 0 })
        );
        assert_eq!(
            select_profile(Some(vec!["Bell".to_owned(), "Bell".to_owned()])).validate(),
            Err(ProfileError::DuplicateOptionLabel { index: 0 })
        );
        // Options are meaningless off a selector: fail-closed, not ignored.
        let mut knob = valid_profile();
        knob.params[0].options = Some(vec!["Bell".to_owned()]);
        assert_eq!(
            knob.validate(),
            Err(ProfileError::OptionsRequireSelect { index: 0 })
        );
    }

    #[test]
    fn bare_js_ident_requires_name_regex() {
        // Mirrors JS General Dynamics: ordinary JSFX idents are bare numbers.
        let mut entry = valid_entry();
        entry.ident = Some("4".to_owned());
        entry.name_regex = None;
        let mut profile = valid_profile();
        profile.params[0] = entry;
        assert_eq!(
            profile.validate(),
            Err(ProfileError::BareIdentWithoutNameRegex {
                index: 0,
                ident: "4".to_owned(),
            })
        );

        let mut entry = valid_entry();
        entry.ident = Some("4".to_owned());
        entry.name_regex = Some("Input Attack \\(ms\\)".to_owned());
        let mut profile = valid_profile();
        profile.params[0] = entry;
        profile.validate().expect("paired bare ident rejected");
    }

    #[test]
    fn stable_idents_are_not_bare() {
        for ident in ["0:_Freq_Low_Shelf", "10:bypass", "0:0", "1:49", "17:wet"] {
            assert!(!is_bare_ident(ident), "{ident} misclassified as bare");
        }
        for ident in ["0", "4", "11"] {
            assert!(is_bare_ident(ident), "{ident} misclassified as stable");
        }
    }

    #[test]
    fn preset_only_forbids_ident_and_visible_ui() {
        let mut entry = valid_entry();
        entry.role = SoundRole::PresetOnly;
        entry.ident = None;
        entry.ui = UiHint::Hidden;
        let mut profile = valid_profile();
        profile.params[0] = entry;
        profile.validate().expect("valid preset_only rejected");

        let mut bad = valid_profile();
        bad.params[0].role = SoundRole::PresetOnly;
        bad.params[0].ident = None;
        bad.params[0].ui = UiHint::Knob;
        assert_eq!(
            bad.validate(),
            Err(ProfileError::PresetOnlyUiMismatch { index: 0 })
        );

        let mut bad = valid_profile();
        bad.params[0].role = SoundRole::PresetOnly;
        bad.params[0].ident = Some("8:8".to_owned());
        bad.params[0].ui = UiHint::Hidden;
        assert_eq!(
            bad.validate(),
            Err(ProfileError::PresetOnlyWithIdent { index: 0 })
        );

        let mut bad = valid_profile();
        bad.params[0].role = SoundRole::Wet;
        bad.params[0].ui = UiHint::Hidden;
        assert_eq!(
            bad.validate(),
            Err(ProfileError::HiddenUiMismatch { index: 0 })
        );
    }

    #[test]
    fn unknown_group_rejected() {
        let mut profile = valid_profile();
        profile.params[0].group = "Nope".to_owned();
        assert_eq!(
            profile.validate(),
            Err(ProfileError::UnknownGroup {
                index: 0,
                group: "Nope".to_owned(),
            })
        );
    }

    #[test]
    fn malformed_json_rejected() {
        assert!(matches!(
            Profile::from_json("{not json"),
            Err(ProfileError::Json(_))
        ));
    }

    #[test]
    fn json_roundtrip_preserves_profile() {
        let profile = valid_profile();
        let json = profile.to_json().expect("serialization failed");
        let back = Profile::from_json(&json).expect("roundtrip parse failed");
        assert_eq!(profile, back);
    }
}
