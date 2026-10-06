//! Embedded CJK font registration (TSK-119).
//!
//! egui ships no CJK glyphs (TSK-306 §1: default font renders CJK as tofu),
//! so the UI embeds a subset of Noto Sans SC (SIL OFL 1.1, see
//! `assets/OFL.txt` and `assets/README-SOURCE.txt`) and prepends it to the
//! proportional and monospace families.

/// Embedded subset font bytes (`assets/NotoSansSC-subset.ttf`).
pub const NOTO_SC_SUBSET: &[u8] = include_bytes!("../assets/NotoSansSC-subset.ttf");

/// Font family key registered for the embedded Noto Sans SC subset.
pub const NOTO_SC_FAMILY: &str = "noto-sc";

/// Install the embedded CJK subset on `ctx`.
///
/// Prepends [`crate::fonts::NOTO_SC_FAMILY`] to both the proportional and
/// monospace families so Chinese copy renders instead of tofu while ASCII
/// keeps egui's default metrics where the subset lacks a glyph. Infallible
/// by construction (missing families are simply skipped).
pub fn install_cjk(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    defs.font_data.insert(
        NOTO_SC_FAMILY.to_owned(),
        egui::FontData::from_static(NOTO_SC_SUBSET).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(list) = defs.families.get_mut(&family) {
            list.insert(0, NOTO_SC_FAMILY.to_owned());
        }
    }
    ctx.set_fonts(defs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_bytes_are_nonempty_ttf() {
        assert!(!NOTO_SC_SUBSET.is_empty());
        // TrueType / OpenType magic (`00 01 00 00` or `OTTO`).
        let magic = &NOTO_SC_SUBSET[..4];
        assert!(magic == [0x00, 0x01, 0x00, 0x00] || magic == *b"OTTO");
    }

    /// Every non-ASCII character used by UI copy must be present in the
    /// subset source list (`assets/charset.txt`); the screenshot proof
    /// (TSK-119 section 2) is the visual half of this regression guard.
    /// Comment text is ignored: only code (string literals carrying UI
    /// copy) counts, mirroring the subset generation procedure recorded
    /// in `assets/README-SOURCE.txt`.
    #[test]
    fn ui_copy_covered_by_subset_charset() {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let charset_file = std::fs::read_to_string(manifest.join("assets").join("charset.txt"))
            .expect("read charset.txt");
        let covered: std::collections::HashSet<char> = charset_file.chars().collect();
        let mut missing = Vec::new();
        for entry in std::fs::read_dir(manifest.join("src")).expect("list src") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("read rs");
                for line in text.lines() {
                    if line.trim_start().starts_with("//") {
                        continue;
                    }
                    let code = line.split("//").next().unwrap_or("");
                    for ch in code.chars() {
                        if !ch.is_ascii() && !covered.contains(&ch) && !missing.contains(&ch) {
                            missing.push(ch);
                        }
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "UI copy uses chars outside assets/charset.txt subset: {missing:?}"
        );
    }
}
