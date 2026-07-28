//! Enumerating the fonts installed on this system.
//!
//! The launcher can't bundle a font for every taste, and the WebView has no way
//! to ask "what's installed?" — `document.fonts` only knows about faces the page
//! itself loaded. So the list has to come from the platform.
//!
//! On Linux that means `fc-list`, the fontconfig query tool. It ships with
//! fontconfig itself, which is a hard dependency of essentially every desktop
//! (GTK and Qt both link it), so it is present wherever Lychi runs. If it is
//! somehow missing, [`installed_families`] returns an empty list and the caller
//! falls back to the built-in stack — a missing picker, not a broken app.
//!
//! ## Why families, not faces
//!
//! `fc-list` reports one line per *face*: "DejaVu Sans:style=Bold",
//! "DejaVu Sans:style=Italic", and so on. A font picker wants the family name
//! once. Deduplication happens here rather than in the UI so the frontend never
//! has to know about fontconfig's output shape.

use std::process::Command;

/// A font family the user can pick.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FontFamily {
    /// The family name, exactly as it must appear in CSS (e.g. "JetBrains Mono").
    pub name: String,
    /// Whether every face in this family is fixed-width. Drives which picker the
    /// family appears in: a proportional font in the monospace slot would
    /// misalign every column of command output.
    pub monospace: bool,
}

/// Families that are almost never what a user wants to read UI text in, and
/// which crowd out the real choices. Symbol/icon fonts render as boxes or
/// pictograms when applied to normal text, so offering them is offering a way
/// to make the app unreadable.
const NOISE_MARKERS: &[&str] = &[
    "emoji",
    "symbols",
    "symbol",
    "awesome",
    "icons",
    "dingbat",
    "webdings",
    "wingdings",
    "material design",
    "octicons",
];

/// Is this family name one of the symbol/icon fonts we hide from the picker?
fn is_noise(name: &str) -> bool {
    let lower = name.to_lowercase();
    NOISE_MARKERS.iter().any(|m| lower.contains(m))
}

/// Every font family installed on this system, sorted by name.
///
/// Returns an empty vector when fontconfig isn't available rather than an error:
/// the picker is an enhancement, and the CSS fallback stack still produces a
/// readable app without it.
pub fn installed_families() -> Vec<FontFamily> {
    // `%{family[0]}` takes the first (canonical) family name — fontconfig often
    // lists localized aliases after it, and those aren't valid CSS. `%{spacing}`
    // is 100 for mono-spaced faces.
    let Ok(output) = Command::new("fc-list")
        .args(["--format", "%{family[0]}\t%{spacing}\n"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&output.stdout);
    parse_fc_list(&text)
}

/// Parse `fc-list` output into a deduplicated, sorted family list.
///
/// Split out from the command call so the parsing rules are testable without a
/// working fontconfig — this is where every real decision lives.
fn parse_fc_list(text: &str) -> Vec<FontFamily> {
    use std::collections::HashMap;

    // family name → whether EVERY face seen so far is monospaced.
    let mut families: HashMap<String, bool> = HashMap::new();

    for line in text.lines() {
        let mut parts = line.split('\t');
        let Some(name) = parts.next() else { continue };
        let name = name.trim();
        if name.is_empty() || is_noise(name) {
            continue;
        }
        // fontconfig's spacing value: 100 = MONO, 90 = DUAL, 110 = CHARCELL.
        // Anything else (or absent) is proportional.
        let mono = matches!(parts.next().map(str::trim), Some("100") | Some("110"));

        families
            .entry(name.to_string())
            // A family counts as monospace only if EVERY face is. A mixed family
            // (a "Sans" and "Sans Mono" sharing a name) is safer treated as
            // proportional than as a mono option that misaligns output.
            .and_modify(|m| *m = *m && mono)
            .or_insert(mono);
    }

    let mut out: Vec<FontFamily> = families
        .into_iter()
        .map(|(name, monospace)| FontFamily { name, monospace })
        .collect();
    // Sorted so the picker is stable across launches — `HashMap` order is
    // arbitrary and would otherwise reshuffle the list every time.
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_family_and_spacing() {
        let out = parse_fc_list("DejaVu Sans\t0\nJetBrains Mono\t100\n");
        assert_eq!(out.len(), 2);
        let jb = out.iter().find(|f| f.name == "JetBrains Mono").unwrap();
        assert!(jb.monospace);
        let dv = out.iter().find(|f| f.name == "DejaVu Sans").unwrap();
        assert!(!dv.monospace);
    }

    #[test]
    fn faces_of_one_family_collapse_to_a_single_entry() {
        // fc-list reports every face; the picker wants the family once.
        let text = "DejaVu Sans\t0\nDejaVu Sans\t0\nDejaVu Sans\t0\n";
        assert_eq!(parse_fc_list(text).len(), 1);
    }

    #[test]
    fn a_family_is_monospace_only_if_every_face_is() {
        // A mixed family treated as mono would misalign command output, so the
        // conservative answer is the correct one.
        let text = "Mixed\t100\nMixed\t0\n";
        let out = parse_fc_list(text);
        assert_eq!(out.len(), 1);
        assert!(!out[0].monospace, "mixed family must not claim monospace");
    }

    #[test]
    fn charcell_spacing_counts_as_monospace() {
        // 110 = CHARCELL, a stricter fixed-width than 100. Terminal fonts use it.
        let out = parse_fc_list("Terminus\t110\n");
        assert!(out[0].monospace);
    }

    #[test]
    fn symbol_and_icon_fonts_are_hidden() {
        // Applied to UI text these render as boxes — offering them is offering a
        // way to make the app unreadable.
        let text = "Noto Color Emoji\t0\nFont Awesome 6 Free\t0\nDejaVu Sans\t0\n";
        let out = parse_fc_list(text);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "DejaVu Sans");
    }

    #[test]
    fn output_is_sorted_case_insensitively() {
        let out = parse_fc_list("zed Mono\t100\nAlpha\t0\nbeta\t0\n");
        let names: Vec<&str> = out.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta", "zed Mono"]);
    }

    #[test]
    fn malformed_lines_are_skipped_not_panicked_on() {
        // A hand-mangled or truncated fc-list run must not take down the panel.
        let out = parse_fc_list("\n\t\nNoSpacingColumn\nGood\t100\n");
        assert!(out.iter().any(|f| f.name == "Good"));
        assert!(out.iter().all(|f| !f.name.is_empty()));
    }

    #[test]
    fn enumerating_never_panics_on_this_machine() {
        // Whatever fontconfig does or doesn't exist here, this must return.
        let _ = installed_families();
    }
}
