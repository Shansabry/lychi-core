use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::entry::{DesktopEntry, exec_basename, make_acronym, strip_field_codes, tokenize};

/// Return the XDG application directories, in **precedence order** (highest
/// first), per the XDG Base Directory spec.
///
/// The spec says application directories are `$XDG_DATA_HOME/applications`
/// followed by `$XDG_DATA_DIRS/applications`, and that the first occurrence of
/// a given desktop-file ID wins. Reading these from the environment is the
/// only correct source: a hardcoded list is the build host masquerading as the
/// target, and it costs NixOS/Guix users (whose profiles live under paths no
/// list can guess) their entire app index.
///
/// The fallbacks below are the spec defaults, used only when the variable is
/// unset or empty — never as a supplement to a set value.
pub fn watch_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    for base in data_home().into_iter().chain(data_dirs()) {
        let apps = base.join("applications");
        if !dirs.contains(&apps) {
            dirs.push(apps);
        }
    }

    dirs
}

/// `$XDG_DATA_HOME`, defaulting to `~/.local/share`.
fn data_home() -> Option<PathBuf> {
    match env_path("XDG_DATA_HOME") {
        Some(p) => Some(p),
        None => dirs::home_dir().map(|h| h.join(".local/share")),
    }
}

/// `$XDG_DATA_DIRS` split on ':', defaulting to the spec's
/// `/usr/local/share:/usr/share`.
///
/// Flatpak and snap export directories are appended as a *supplement*, since
/// well-configured systems put them on `XDG_DATA_DIRS` but many do not, and
/// missing them silently loses every Flatpak app.
fn data_dirs() -> Vec<PathBuf> {
    data_dirs_from(std::env::var("XDG_DATA_DIRS").ok().as_deref())
}

/// The `XDG_DATA_DIRS` rule as a pure function of the variable's value, so the
/// NixOS/Guix case (a set value pointing at profile paths no list could guess)
/// is assertable without a NixOS machine.
fn data_dirs_from(raw: Option<&str>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match raw {
        Some(v) if !v.trim().is_empty() => v
            .split(':')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect(),
        _ => vec![
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ],
    };

    for extra in [
        dirs::home_dir().map(|h| h.join(".local/share/flatpak/exports/share")),
        Some(PathBuf::from("/var/lib/flatpak/exports/share")),
        Some(PathBuf::from("/var/lib/snapd/desktop")),
    ]
    .into_iter()
    .flatten()
    {
        if !out.contains(&extra) {
            out.push(extra);
        }
    }

    out
}

/// Read an env var as a path, treating unset and empty as equivalent (the spec
/// says an empty value must be treated as unset).
fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => None,
    }
}

/// Discover all desktop entries from the XDG application directories.
///
/// Entries are keyed by their **desktop file ID** (the path relative to the
/// applications directory, with `/` replaced by `-`, e.g.
/// `kde4-konsole.desktop`). Directories are visited in precedence order and
/// the first ID seen wins, so a user's `~/.local/share/applications` override
/// shadows the system copy instead of appearing twice.
pub fn discover_entries() -> Vec<DesktopEntry> {
    let dirs = watch_dirs();
    let env = ParseEnv::from_env();

    let mut by_id: HashMap<String, DesktopEntry> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for dir in &dirs {
        for (id, path) in desktop_files_in(dir) {
            // First directory wins — XDG precedence.
            if by_id.contains_key(&id) {
                continue;
            }
            if let Some(entry) = parse_desktop_file_in(&path, &env) {
                order.push(id.clone());
                by_id.insert(id, entry);
            }
        }
    }

    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// Recursively collect `.desktop` files under `dir`, returning
/// `(desktop_file_id, path)` pairs.
///
/// Subdirectories matter: the spec builds the ID from the path relative to the
/// applications directory with separators turned into `-`, and distributions
/// really do nest (`applications/kde4/konsole.desktop`).
fn desktop_files_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    collect_desktop_files(dir, dir, &mut out, 0);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Bound on directory nesting — deep enough for real layouts, shallow enough
/// that a symlink cycle cannot spin forever.
const MAX_DEPTH: usize = 8;

fn collect_desktop_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };

    for file in read_dir.flatten() {
        let path = file.path();
        let Ok(ft) = file.file_type() else { continue };

        if ft.is_dir() {
            collect_desktop_files(root, &path, out, depth + 1);
        } else if path.extension().is_some_and(|ext| ext == "desktop")
            && let Some(id) = desktop_file_id(root, &path)
        {
            out.push((id, path));
        }
    }
}

/// Build the desktop file ID: path relative to the applications dir, `/` → `-`.
fn desktop_file_id(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('/', "-"))
}

/// The user's locale, decomposed into the lookup keys the desktop spec defines.
///
/// For `LANG=de_DE.UTF-8@euro` the spec's match order is
/// `de_DE@euro`, `de_DE`, `de@euro`, `de` — most specific first, with the
/// unlocalized `Name=` as the final fallback.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Locale {
    /// Candidate suffixes in descending priority, e.g. ["de_DE", "de"].
    keys: Vec<String>,
}

impl Locale {
    /// Resolve from the environment, honoring the POSIX precedence
    /// `LC_ALL` > `LC_MESSAGES` > `LANG`.
    pub fn from_env() -> Self {
        let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|k| match std::env::var(k) {
                Ok(v) if !v.trim().is_empty() => Some(v),
                _ => None,
            })
            .unwrap_or_default();
        Self::parse(&raw)
    }

    /// Decompose a POSIX locale string `lang_COUNTRY.ENCODING@MODIFIER` into
    /// the spec's candidate keys.
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        // "C" and "POSIX" mean "no localization" — the unlocalized key only.
        if raw.is_empty() || raw == "C" || raw == "POSIX" || raw.starts_with("C.") {
            return Self::default();
        }

        // Strip the encoding: it never takes part in the match.
        let (head, modifier) = match raw.split_once('@') {
            Some((h, m)) => (h, Some(m)),
            None => (raw, None),
        };
        let head = head.split('.').next().unwrap_or(head);

        let (lang, country) = match head.split_once('_') {
            Some((l, c)) => (l, Some(c)),
            None => (head, None),
        };

        if lang.is_empty() {
            return Self::default();
        }

        let mut keys = Vec::new();
        let mut push = |k: String| {
            if !k.is_empty() && !keys.contains(&k) {
                keys.push(k);
            }
        };

        match (country, modifier) {
            (Some(c), Some(m)) => {
                push(format!("{lang}_{c}@{m}"));
                push(format!("{lang}_{c}"));
                push(format!("{lang}@{m}"));
                push(lang.to_string());
            }
            (Some(c), None) => {
                push(format!("{lang}_{c}"));
                push(lang.to_string());
            }
            (None, Some(m)) => {
                push(format!("{lang}@{m}"));
                push(lang.to_string());
            }
            (None, None) => push(lang.to_string()),
        }

        Self { keys }
    }

    /// Pick the best-matching value from the localized variants of one key.
    /// Returns `None` when no variant matches the user's locale.
    fn best<'a>(&self, variants: &'a HashMap<String, String>) -> Option<&'a str> {
        self.keys
            .iter()
            .find_map(|k| variants.get(k))
            .map(|s| s.as_str())
    }
}

/// One key's values: the unlocalized value plus every `key[locale]` variant.
#[derive(Debug, Default)]
struct LocalizedValue {
    plain: Option<String>,
    variants: HashMap<String, String>,
}

impl LocalizedValue {
    fn set(&mut self, locale: Option<&str>, value: &str) {
        match locale {
            // First occurrence wins — the spec says later duplicate keys in the
            // same group are undefined behaviour, and every other reader takes
            // the first.
            Some(l) => {
                self.variants
                    .entry(l.to_string())
                    .or_insert_with(|| value.to_string());
            }
            None => {
                if self.plain.is_none() {
                    self.plain = Some(value.to_string());
                }
            }
        }
    }

    /// The value to display: the best locale match, else the unlocalized value.
    fn resolved(&self, locale: &Locale) -> Option<&str> {
        locale.best(&self.variants).or(self.plain.as_deref())
    }

    /// Every value a user might reasonably type, in display-first order: the
    /// resolved (localized) value plus the unlocalized one. Indexing both means
    /// a German user finds "Rechner" *and* still finds "Calculator".
    fn all(&self, locale: &Locale) -> Vec<&str> {
        let mut out = Vec::new();
        for v in [self.resolved(locale), self.plain.as_deref()]
            .into_iter()
            .flatten()
        {
            if !out.contains(&v) {
                out.push(v);
            }
        }
        out
    }
}

/// Parse a single .desktop file into a `DesktopEntry`, using the environment's
/// locale for display names.
///
/// Returns `None` if the entry is hidden, not shown in this desktop, has no
/// runnable `TryExec=`, or lacks `Exec=` / `Name=`.
pub fn parse_desktop_file(path: &PathBuf) -> Option<DesktopEntry> {
    parse_desktop_file_in(path, &ParseEnv::from_env())
}

pub(crate) fn parse_desktop_file_in(path: &PathBuf, env: &ParseEnv) -> Option<DesktopEntry> {
    let content = fs::read_to_string(path).ok()?;
    parse_desktop_content(&content, path, env)
}

/// Everything about the *machine* that changes how a .desktop file is read:
/// which language to display and which desktop we are running under.
///
/// Read once per discovery pass and threaded through, so parsing is a pure
/// function of (file, environment) and can be tested for any environment
/// rather than only the one the build host happens to have.
#[derive(Debug, Default, Clone)]
pub(crate) struct ParseEnv {
    locale: Locale,
    desktops: Vec<String>,
}

impl ParseEnv {
    pub(crate) fn from_env() -> Self {
        Self {
            locale: Locale::from_env(),
            desktops: current_desktops(),
        }
    }
}

fn parse_desktop_content(content: &str, path: &Path, env: &ParseEnv) -> Option<DesktopEntry> {
    let locale = &env.locale;
    let mut name = LocalizedValue::default();
    let mut generic_name = LocalizedValue::default();
    let mut keywords = LocalizedValue::default();
    let mut exec = None;
    let mut try_exec: Option<String> = None;
    let mut icon = None;
    let mut wm_class = None;
    let mut categories_raw: Option<String> = None;
    let mut only_show_in: Option<String> = None;
    let mut not_show_in: Option<String> = None;
    let mut is_terminal_app = false;
    let mut no_display = false;
    let mut hidden = false;
    let mut entry_type: Option<String> = None;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_desktop_entry = group.trim() == "Desktop Entry";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }

        // `key = value` with arbitrary surrounding whitespace. Splitting on the
        // separator instead of matching a literal prefix is what makes
        // `NoDisplay = true` behave the same as `NoDisplay=true`.
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let (key, key_locale) = split_key_locale(key.trim());

        match key {
            "Name" => name.set(key_locale, value),
            "GenericName" => generic_name.set(key_locale, value),
            "Keywords" => keywords.set(key_locale, value),
            // The rest are locale-independent; a stray `Exec[de]=` is not a
            // thing, so only the unlocalized form is read.
            _ if key_locale.is_some() => {}
            "Exec" => {
                if exec.is_none() {
                    exec = Some(strip_field_codes(value));
                }
            }
            "TryExec" => {
                if try_exec.is_none() {
                    try_exec = Some(value.to_string());
                }
            }
            "Icon" => {
                if icon.is_none() {
                    icon = Some(value.to_string());
                }
            }
            "StartupWMClass" => {
                if wm_class.is_none() {
                    wm_class = Some(value.to_string());
                }
            }
            "Categories" => {
                if categories_raw.is_none() {
                    categories_raw = Some(value.to_string());
                }
            }
            "OnlyShowIn" => {
                if only_show_in.is_none() {
                    only_show_in = Some(value.to_string());
                }
            }
            "NotShowIn" => {
                if not_show_in.is_none() {
                    not_show_in = Some(value.to_string());
                }
            }
            "Type" => {
                if entry_type.is_none() {
                    entry_type = Some(value.to_string());
                }
            }
            "Terminal" => is_terminal_app = parse_bool(value),
            "NoDisplay" => no_display = parse_bool(value),
            "Hidden" => hidden = parse_bool(value),
            _ => {}
        }
    }

    if no_display || hidden {
        return None;
    }
    // Only Type=Application is launchable. Link/Directory entries are not apps.
    // A missing Type is tolerated: plenty of real files omit it.
    if let Some(t) = entry_type.as_deref()
        && t != "Application"
    {
        return None;
    }
    if !shown_in(
        only_show_in.as_deref(),
        not_show_in.as_deref(),
        &env.desktops,
    ) {
        return None;
    }
    // TryExec names a binary whose absence means the app is not installed.
    if let Some(te) = try_exec.as_deref()
        && !try_exec_available(te)
    {
        return None;
    }

    let display_name = name.resolved(locale)?.to_string();
    let exec = exec?;
    let exec_base = exec_basename(&exec);

    // Index every name the user might type — the localized one and the
    // unlocalized original — not just the one shown.
    let mut name_tokens: Vec<String> = Vec::new();
    let mut acronyms: Vec<String> = Vec::new();
    for n in name.all(locale) {
        for t in tokenize(n) {
            if !name_tokens.contains(&t) {
                name_tokens.push(t);
            }
        }
        let a = make_acronym(n);
        if !a.is_empty() && !acronyms.contains(&a) {
            acronyms.push(a);
        }
    }

    let mut keyword_list: Vec<String> = Vec::new();
    for k in keywords.all(locale) {
        for t in tokenize(k) {
            if !keyword_list.contains(&t) {
                keyword_list.push(t);
            }
        }
    }
    // Localized generic names are searchable too ("Navigateur Web").
    let generic_variants: Vec<String> = generic_name
        .all(locale)
        .into_iter()
        .map(str::to_string)
        .collect();

    let aliases: Vec<String> = name
        .all(locale)
        .into_iter()
        .filter(|n| !n.eq_ignore_ascii_case(&display_name))
        .map(str::to_string)
        .collect();

    let categories = parse_categories(categories_raw.as_deref());

    Some(DesktopEntry {
        name: display_name,
        aliases,
        exec,
        exec_basename: exec_base,
        wm_class,
        generic_name: generic_variants.first().cloned(),
        generic_names: generic_variants,
        keywords: keyword_list,
        name_tokens,
        acronym: acronyms.first().cloned().unwrap_or_default(),
        acronyms,
        icon,
        categories,
        is_terminal_app,
        desktop_path: path.to_string_lossy().into_owned(),
        icon_path: OnceLock::new(),
    })
}

/// Split `Name[de_DE]` into `("Name", Some("de_DE"))`.
fn split_key_locale(key: &str) -> (&str, Option<&str>) {
    match key.find('[') {
        Some(i) if key.ends_with(']') => {
            let locale = &key[i + 1..key.len() - 1];
            (key[..i].trim_end(), Some(locale.trim()))
        }
        _ => (key, None),
    }
}

/// Parse a .desktop boolean. The spec says only lowercase `true`/`false` are
/// valid, but files in the wild write `True`, `TRUE`, and `yes`; the old
/// exact-match comparison silently treated all of them as `false`, which let
/// `NoDisplay = True` entries leak into the launcher.
fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "1"
    )
}

/// Honor `OnlyShowIn=` / `NotShowIn=` against `$XDG_CURRENT_DESKTOP`.
///
/// `XDG_CURRENT_DESKTOP` is a colon-separated list (`ubuntu:GNOME`), and the
/// comparison is case-insensitive in practice even though the spec's registered
/// names are cased. When the variable is unset we cannot tell which desktop we
/// are on, so `OnlyShowIn` entries are **shown** rather than hidden — losing an
/// app is worse than showing one extra.
/// The rule is a pure function of its inputs. Kept separate from the
/// environment read so it is testable for any desktop — a test that consults
/// `$XDG_CURRENT_DESKTOP` only ever asserts what the build host happens to be.
fn shown_in(only: Option<&str>, not: Option<&str>, current: &[String]) -> bool {
    if let Some(not) = not
        && !current.is_empty()
        && split_list(not)
            .iter()
            .any(|d| current.iter().any(|c| c.eq_ignore_ascii_case(d)))
    {
        return false;
    }

    if let Some(only) = only {
        if current.is_empty() {
            return true;
        }
        return split_list(only)
            .iter()
            .any(|d| current.iter().any(|c| c.eq_ignore_ascii_case(d)));
    }

    true
}

fn current_desktops() -> Vec<String> {
    match std::env::var("XDG_CURRENT_DESKTOP") {
        Ok(v) if !v.trim().is_empty() => v
            .split(':')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Split a `;`-separated .desktop list value.
fn split_list(raw: &str) -> Vec<&str> {
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Is the `TryExec=` binary present? An absolute path must exist; a bare name
/// is looked up on `$PATH`.
fn try_exec_available(try_exec: &str) -> bool {
    let te = try_exec.trim().trim_matches('"');
    if te.is_empty() {
        return true;
    }

    let path = Path::new(te);
    if path.is_absolute() || te.contains('/') {
        return path.exists();
    }

    let Ok(paths) = std::env::var("PATH") else {
        // No PATH to check against — do not hide the app on a guess.
        return true;
    };
    paths
        .split(':')
        .filter(|p| !p.is_empty())
        .any(|p| Path::new(p).join(te).exists())
}

/// Parse the Categories= field into lowercased tokens.
/// Input: "Network;WebBrowser;" → ["network", "webbrowser"]
fn parse_categories(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else { return Vec::new() };
    split_list(raw).into_iter().map(str::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse under an explicit environment. Never reads process env, so every
    /// assertion holds on any machine and in CI.
    fn parse_in(content: &str, locale: &str, desktops: &[&str]) -> Option<DesktopEntry> {
        parse_desktop_content(
            content,
            Path::new("/tmp/test.desktop"),
            &ParseEnv {
                locale: Locale::parse(locale),
                desktops: desktops.iter().map(|s| s.to_string()).collect(),
            },
        )
    }

    fn parse(content: &str, locale: &str) -> Option<DesktopEntry> {
        parse_in(content, locale, &[])
    }

    const CALC: &str = "\
[Desktop Entry]
Type=Application
Name=Calculator
Name[de]=Rechner
Name[de_AT]=Taschenrechner
GenericName=Calculator
GenericName[de]=Taschenrechner
Keywords=math;
Keywords[de]=Mathematik;
Exec=/usr/bin/galculator
";

    /// Keywords= reaches the index through `tokenize`; these assert the
    /// normalization a parsed entry's `keywords` field is expected to have.
    #[test]
    fn parse_keywords_basic() {
        let kws = tokenize("internet;web;Browser;");
        assert!(kws.contains(&"internet".to_string()));
        assert!(kws.contains(&"web".to_string()));
        assert!(kws.contains(&"browser".to_string()));
    }

    #[test]
    fn parse_keywords_deduped() {
        let kws = tokenize("editor;Editor;editor;");
        assert_eq!(kws.iter().filter(|k| k.as_str() == "editor").count(), 1);
    }

    #[test]
    fn parse_keywords_stopwords_removed() {
        let kws = tokenize("app;application;text;editor;");
        assert!(!kws.contains(&"app".to_string()));
        assert!(!kws.contains(&"application".to_string()));
        assert!(kws.contains(&"text".to_string()));
        assert!(kws.contains(&"editor".to_string()));
    }

    #[test]
    fn keywords_field_reaches_the_entry() {
        let e = parse(
            "[Desktop Entry]\nName=X\nExec=/usr/bin/x\nKeywords=internet;Browser;\n",
            "C",
        )
        .unwrap();
        assert!(e.keywords.contains(&"internet".to_string()));
        assert!(e.keywords.contains(&"browser".to_string()));
    }

    #[test]
    fn exec_basename_extraction() {
        assert_eq!(exec_basename("/usr/bin/code %U"), "code");
        assert_eq!(exec_basename("firefox"), "firefox");
        assert_eq!(
            exec_basename("/usr/bin/org.gnome.Nautilus"),
            "org.gnome.nautilus"
        );
        // env + KEY=VALUE prefix
        assert_eq!(
            exec_basename("env BAMF_HINT=/foo /usr/bin/code --flag"),
            "code"
        );
        // Quoted binary
        assert_eq!(exec_basename("\"code\" --new-window %U"), "code");
    }

    #[test]
    fn acronym_generation() {
        assert_eq!(make_acronym("Visual Studio Code"), "vsc");
        assert_eq!(make_acronym("GIMP"), "g");
        assert_eq!(make_acronym("Firefox"), "f");
    }

    // --- Locale decomposition ---

    #[test]
    fn locale_parse_full_form() {
        let l = Locale::parse("de_DE.UTF-8@euro");
        assert_eq!(l.keys, vec!["de_DE@euro", "de_DE", "de@euro", "de"]);
    }

    #[test]
    fn locale_parse_lang_country() {
        assert_eq!(Locale::parse("pt_BR.UTF-8").keys, vec!["pt_BR", "pt"]);
    }

    #[test]
    fn locale_parse_lang_only() {
        assert_eq!(Locale::parse("fr").keys, vec!["fr"]);
    }

    #[test]
    fn locale_c_and_posix_are_unlocalized() {
        assert!(Locale::parse("C").keys.is_empty());
        assert!(Locale::parse("POSIX").keys.is_empty());
        assert!(Locale::parse("C.UTF-8").keys.is_empty());
        assert!(Locale::parse("").keys.is_empty());
    }

    // --- i18n names (the highest-volume real-user bug) ---

    #[test]
    fn localized_name_is_the_display_name() {
        let e = parse(CALC, "de_DE.UTF-8").unwrap();
        assert_eq!(e.name, "Rechner");
    }

    #[test]
    fn most_specific_locale_wins() {
        let e = parse(CALC, "de_AT.UTF-8").unwrap();
        assert_eq!(e.name, "Taschenrechner");
    }

    #[test]
    fn unlocalized_name_used_when_locale_has_no_variant() {
        let e = parse(CALC, "ja_JP.UTF-8").unwrap();
        assert_eq!(e.name, "Calculator");
        assert!(e.aliases.is_empty());
    }

    #[test]
    fn c_locale_keeps_the_unlocalized_name() {
        let e = parse(CALC, "C").unwrap();
        assert_eq!(e.name, "Calculator");
    }

    #[test]
    fn both_localized_and_original_name_are_searchable() {
        let e = parse(CALC, "de_DE.UTF-8").unwrap();
        // The original name stays reachable as an alias...
        assert_eq!(e.aliases, vec!["Calculator".to_string()]);
        // ...and both tokenize into the search index.
        assert!(e.name_tokens.contains(&"rechner".to_string()));
        assert!(e.name_tokens.contains(&"calculator".to_string()));
    }

    #[test]
    fn localized_keywords_are_indexed() {
        let e = parse(CALC, "de_DE.UTF-8").unwrap();
        assert!(e.keywords.contains(&"mathematik".to_string()));
        assert!(e.keywords.contains(&"math".to_string()));
    }

    #[test]
    fn localized_generic_name_is_preferred() {
        let e = parse(CALC, "de_DE.UTF-8").unwrap();
        assert_eq!(e.generic_name.as_deref(), Some("Taschenrechner"));
        assert!(e.generic_names.iter().any(|g| g == "Calculator"));
    }

    #[test]
    fn split_key_locale_forms() {
        assert_eq!(split_key_locale("Name"), ("Name", None));
        assert_eq!(split_key_locale("Name[de]"), ("Name", Some("de")));
        assert_eq!(
            split_key_locale("Name[sr@latin]"),
            ("Name", Some("sr@latin"))
        );
    }

    // --- Whitespace / case fragility ---

    #[test]
    fn spaces_around_separator_are_tolerated() {
        let e = parse(
            "[Desktop Entry]\nName = Spaced\nExec = /usr/bin/spaced\n",
            "C",
        )
        .unwrap();
        assert_eq!(e.name, "Spaced");
        assert_eq!(e.exec, "/usr/bin/spaced");
    }

    #[test]
    fn nodisplay_hidden_regardless_of_spacing_and_case() {
        for line in [
            "NoDisplay=true",
            "NoDisplay = true",
            "NoDisplay=True",
            "NoDisplay=TRUE",
        ] {
            let content = format!("[Desktop Entry]\nName=X\nExec=/usr/bin/x\n{line}\n");
            assert!(parse(&content, "C").is_none(), "not hidden by: {line}");
        }
    }

    #[test]
    fn hidden_regardless_of_case() {
        let e = parse(
            "[Desktop Entry]\nName=X\nExec=/usr/bin/x\nHidden=True\n",
            "C",
        );
        assert!(e.is_none());
    }

    #[test]
    fn terminal_flag_tolerates_spacing_and_case() {
        let e = parse(
            "[Desktop Entry]\nName=X\nExec=/usr/bin/x\nTerminal = True\n",
            "C",
        )
        .unwrap();
        assert!(e.is_terminal_app);
    }

    #[test]
    fn false_is_still_false() {
        let e = parse(
            "[Desktop Entry]\nName=X\nExec=/usr/bin/x\nNoDisplay=false\nTerminal=False\n",
            "C",
        )
        .unwrap();
        assert!(!e.is_terminal_app);
    }

    #[test]
    fn comments_are_ignored() {
        let e = parse(
            "[Desktop Entry]\n# NoDisplay=true\nName=X\nExec=/usr/bin/x\n",
            "C",
        );
        assert!(e.is_some());
    }

    #[test]
    fn keys_outside_desktop_entry_group_are_ignored() {
        let e = parse(
            "[Desktop Entry]\nName=X\nExec=/usr/bin/x\n[Desktop Action New]\nName=New Window\nNoDisplay=true\n",
            "C",
        )
        .unwrap();
        assert_eq!(e.name, "X");
    }

    #[test]
    fn group_header_with_whitespace_still_matches() {
        let e = parse("[ Desktop Entry ]\nName=X\nExec=/usr/bin/x\n", "C");
        assert!(e.is_some());
    }

    // --- Type / OnlyShowIn / NotShowIn / TryExec ---

    #[test]
    fn non_application_types_are_skipped() {
        assert!(parse("[Desktop Entry]\nType=Link\nName=X\nURL=http://x\n", "C").is_none());
        assert!(
            parse(
                "[Desktop Entry]\nType=Directory\nName=X\nExec=/usr/bin/x\n",
                "C"
            )
            .is_none()
        );
    }

    #[test]
    fn missing_type_is_tolerated() {
        assert!(parse("[Desktop Entry]\nName=X\nExec=/usr/bin/x\n", "C").is_some());
    }

    fn desktops(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_show_in_matches_current_desktop() {
        assert!(shown_in(Some("GNOME;"), None, &desktops(&["GNOME"])));
    }

    #[test]
    fn only_show_in_hides_elsewhere() {
        assert!(!shown_in(Some("GNOME;"), None, &desktops(&["KDE"])));
    }

    #[test]
    fn only_show_in_matches_any_entry_of_a_compound_desktop() {
        // XDG_CURRENT_DESKTOP=ubuntu:GNOME must satisfy OnlyShowIn=GNOME.
        assert!(shown_in(
            Some("GNOME;"),
            None,
            &desktops(&["ubuntu", "GNOME"])
        ));
    }

    #[test]
    fn desktop_comparison_is_case_insensitive() {
        assert!(shown_in(Some("gnome;"), None, &desktops(&["GNOME"])));
        assert!(!shown_in(None, Some("gnome;"), &desktops(&["GNOME"])));
    }

    #[test]
    fn not_show_in_hides_here_and_shows_elsewhere() {
        assert!(!shown_in(None, Some("KDE;"), &desktops(&["KDE"])));
        assert!(shown_in(None, Some("KDE;"), &desktops(&["GNOME"])));
    }

    #[test]
    fn unknown_desktop_shows_everything() {
        // Losing an app is worse than showing one extra, so an unset
        // XDG_CURRENT_DESKTOP must never hide an entry.
        assert!(shown_in(Some("GNOME;"), None, &[]));
        assert!(shown_in(None, Some("KDE;"), &[]));
    }

    #[test]
    fn show_keys_are_honored_end_to_end() {
        let gnome_only = "[Desktop Entry]\nName=X\nExec=/usr/bin/x\nOnlyShowIn=GNOME;\n";
        assert!(parse_in(gnome_only, "C", &["GNOME"]).is_some());
        assert!(parse_in(gnome_only, "C", &["KDE"]).is_none());

        let not_kde = "[Desktop Entry]\nName=X\nExec=/usr/bin/x\nNotShowIn=KDE;\n";
        assert!(parse_in(not_kde, "C", &["KDE"]).is_none());
        assert!(parse_in(not_kde, "C", &["GNOME"]).is_some());
    }

    #[test]
    fn no_show_keys_means_shown() {
        assert!(shown_in(None, None, &desktops(&["KDE"])));
    }

    #[test]
    fn current_desktops_splits_the_colon_list() {
        // Only asserts the parse shape, never the host's actual desktop.
        assert!(current_desktops().iter().all(|d| !d.contains(':')));
    }

    #[test]
    fn try_exec_absolute_path_must_exist() {
        assert!(!try_exec_available("/definitely/not/here/binary"));
        assert!(try_exec_available("/bin/sh"));
    }

    #[test]
    fn try_exec_bare_name_is_looked_up_on_path() {
        assert!(try_exec_available("sh"));
        assert!(!try_exec_available("lychi-nonexistent-binary-xyz"));
    }

    #[test]
    fn missing_try_exec_binary_hides_the_entry() {
        let e = parse(
            "[Desktop Entry]\nName=X\nExec=/usr/bin/x\nTryExec=/definitely/not/here\n",
            "C",
        );
        assert!(e.is_none());
    }

    #[test]
    fn present_try_exec_binary_keeps_the_entry() {
        let e = parse(
            "[Desktop Entry]\nName=X\nExec=/usr/bin/x\nTryExec=/bin/sh\n",
            "C",
        );
        assert!(e.is_some());
    }

    // --- Duplicate keys ---

    #[test]
    fn first_duplicate_key_wins() {
        let e = parse(
            "[Desktop Entry]\nName=First\nName=Second\nExec=/usr/bin/a\nExec=/usr/bin/b\n",
            "C",
        )
        .unwrap();
        assert_eq!(e.name, "First");
        assert_eq!(e.exec, "/usr/bin/a");
    }

    // --- Desktop file IDs / XDG dirs ---

    #[test]
    fn desktop_file_id_flattens_subdirectories() {
        assert_eq!(
            desktop_file_id(
                Path::new("/usr/share/applications"),
                Path::new("/usr/share/applications/kde4/konsole.desktop")
            )
            .as_deref(),
            Some("kde4-konsole.desktop")
        );
    }

    #[test]
    fn desktop_file_id_at_root_is_the_filename() {
        assert_eq!(
            desktop_file_id(
                Path::new("/usr/share/applications"),
                Path::new("/usr/share/applications/firefox.desktop")
            )
            .as_deref(),
            Some("firefox.desktop")
        );
    }

    #[test]
    fn watch_dirs_are_unique_and_end_in_applications() {
        let dirs = watch_dirs();
        assert!(!dirs.is_empty());
        for d in &dirs {
            assert_eq!(d.file_name().and_then(|s| s.to_str()), Some("applications"));
        }
        let mut sorted = dirs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), dirs.len(), "watch_dirs returned duplicates");
    }

    #[test]
    fn xdg_data_dirs_is_honored_not_guessed() {
        // The NixOS/Guix case: profile paths no hardcoded list could contain.
        let nix = "/home/u/.nix-profile/share:/run/current-system/sw/share";
        let dirs = data_dirs_from(Some(nix));
        assert!(dirs.contains(&PathBuf::from("/home/u/.nix-profile/share")));
        assert!(dirs.contains(&PathBuf::from("/run/current-system/sw/share")));
        // A set value REPLACES the defaults; it is not a supplement to them.
        assert!(!dirs.contains(&PathBuf::from("/usr/share")));
    }

    #[test]
    fn unset_or_empty_xdg_data_dirs_uses_the_spec_defaults() {
        for raw in [None, Some(""), Some("   ")] {
            let dirs = data_dirs_from(raw);
            assert!(dirs.contains(&PathBuf::from("/usr/share")), "{raw:?}");
            assert!(dirs.contains(&PathBuf::from("/usr/local/share")), "{raw:?}");
        }
    }

    #[test]
    fn flatpak_exports_survive_a_custom_xdg_data_dirs() {
        // Flatpak apps must not depend on the variable being well-configured.
        let dirs = data_dirs_from(Some("/opt/only/share"));
        assert!(dirs.contains(&PathBuf::from("/var/lib/flatpak/exports/share")));
    }

    #[test]
    fn flatpak_exports_are_always_covered() {
        // Flatpak apps must not depend on XDG_DATA_DIRS being well-configured.
        let dirs = watch_dirs();
        assert!(
            dirs.iter()
                .any(|d| d.starts_with("/var/lib/flatpak/exports/share")),
            "system flatpak exports missing from {dirs:?}"
        );
    }
}
