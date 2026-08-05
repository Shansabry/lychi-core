use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, Output, OutputType,
    Row, Section,
};
use crate::error::LychiError;

use super::shell_exec;

static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

/// A parsed SSH host entry from ~/.ssh/config.
#[derive(Debug, Clone)]
struct SshHost {
    alias: String,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
}

impl SshHost {
    fn display_description(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref user) = self.user {
            if let Some(ref host) = self.hostname {
                parts.push(format!("{user}@{host}"));
            } else {
                parts.push(format!("{user}@{}", self.alias));
            }
        } else if let Some(ref host) = self.hostname {
            parts.push(host.clone());
        }
        if let Some(port) = self.port
            && port != 22
        {
            parts.push(format!(":{port}"));
        }
        if let Some(ref id) = self.identity_file {
            // Show just the filename
            let name = Path::new(id)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(id);
            parts.push(format!("key={name}"));
        }
        if let Some(ref proxy) = self.proxy_jump {
            parts.push(format!("via {proxy}"));
        }
        if parts.is_empty() {
            self.alias.clone()
        } else {
            parts.join(" ")
        }
    }
}

/// Cache for parsed SSH config with mtime-based invalidation.
struct SshCache {
    mtime: SystemTime,
    hosts: Vec<SshHost>,
}

static SSH_CACHE: Mutex<Option<SshCache>> = Mutex::new(None);

/// Parse SSH hosts from ~/.ssh/config (with Include support).
fn load_ssh_hosts() -> Vec<SshHost> {
    let config_path = match dirs::home_dir() {
        Some(home) => home.join(".ssh").join("config"),
        None => return Vec::new(),
    };

    let mtime = match std::fs::metadata(&config_path) {
        Ok(m) => m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        Err(_) => return Vec::new(),
    };

    // Check cache
    if let Ok(cache) = SSH_CACHE.lock()
        && let Some(ref c) = *cache
        && c.mtime == mtime
    {
        return c.hosts.clone();
    }

    let hosts = load_ssh_config_recursive(&config_path, 0);

    if let Ok(mut cache) = SSH_CACHE.lock() {
        *cache = Some(SshCache {
            mtime,
            hosts: hosts.clone(),
        });
    }

    hosts
}

/// Max Include recursion depth to prevent infinite loops.
const MAX_INCLUDE_DEPTH: u8 = 10;

/// Load and parse an SSH config file, resolving Include directives recursively.
fn load_ssh_config_recursive(path: &Path, depth: u8) -> Vec<SshHost> {
    if depth > MAX_INCLUDE_DEPTH {
        tracing::warn!("ssh: Include depth exceeded at {}", path.display());
        return Vec::new();
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let base_dir = path.parent().unwrap_or_else(|| Path::new("~/.ssh"));

    parse_ssh_config(&content, base_dir, depth)
}

fn parse_ssh_config(content: &str, base_dir: &Path, depth: u8) -> Vec<SshHost> {
    let mut hosts = Vec::new();
    let mut current: Option<SshHost> = None;
    let mut in_match_block = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split on first whitespace or '='
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };

        let key_lower = key.to_lowercase();

        if key_lower == "host" {
            // Flush previous host
            if let Some(h) = current.take() {
                hosts.push(h);
            }
            in_match_block = false;

            // Skip wildcard and pattern hosts
            if value.contains('*') || value.contains('?') || value.contains('!') {
                continue;
            }

            // Split multi-alias Host lines: "Host web-prod web-staging" → two entries
            let aliases: Vec<&str> = value.split_whitespace().collect();
            if aliases.len() > 1 {
                // Create entries for all but the last alias (last one becomes `current`)
                for &alias in &aliases[..aliases.len() - 1] {
                    hosts.push(SshHost {
                        alias: alias.to_string(),
                        hostname: None,
                        user: None,
                        port: None,
                        identity_file: None,
                        proxy_jump: None,
                    });
                }
                current = Some(SshHost {
                    alias: aliases[aliases.len() - 1].to_string(),
                    hostname: None,
                    user: None,
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                });
            } else {
                current = Some(SshHost {
                    alias: value.to_string(),
                    hostname: None,
                    user: None,
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                });
            }
        } else if key_lower == "match" {
            // Flush and skip Match blocks
            if let Some(h) = current.take() {
                hosts.push(h);
            }
            in_match_block = true;
        } else if key_lower == "include" {
            // Flush current before processing includes
            if let Some(h) = current.take() {
                hosts.push(h);
            }
            in_match_block = false;

            // Resolve Include paths
            let include_paths = resolve_include(value, base_dir);
            for inc_path in include_paths {
                let mut included = load_ssh_config_recursive(&inc_path, depth + 1);
                hosts.append(&mut included);
            }
        } else if !in_match_block && let Some(ref mut host) = current {
            match key_lower.as_str() {
                "hostname" => host.hostname = Some(value.to_string()),
                "user" => host.user = Some(value.to_string()),
                "port" => host.port = value.parse().ok(),
                "identityfile" => host.identity_file = Some(value.to_string()),
                "proxyjump" => host.proxy_jump = Some(value.to_string()),
                _ => {}
            }
        }
        // For multi-alias hosts, propagate fields to previously pushed aliases
        // that share the same block (they were pushed with None fields).
        // We handle this by back-patching after the block is complete — see below.
    }

    // Flush last host
    if let Some(h) = current {
        hosts.push(h);
    }

    // Back-patch multi-alias entries: when a Host line had multiple aliases,
    // we pushed earlier aliases with None fields. The last alias got the fields.
    // Walk backwards and copy fields from a populated entry to preceding empty siblings.
    backpatch_multi_alias(&mut hosts);

    hosts
}

/// Resolve an Include directive value to actual file paths.
/// Handles `~` expansion and simple `*` glob in the last path segment.
fn resolve_include(value: &str, base_dir: &Path) -> Vec<PathBuf> {
    let expanded = if value.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            PathBuf::from(value.replacen('~', &home.to_string_lossy(), 1))
        } else {
            return Vec::new();
        }
    } else if !value.starts_with('/') {
        // Relative to base_dir (usually ~/.ssh/)
        base_dir.join(value)
    } else {
        PathBuf::from(value)
    };

    // Check for glob wildcard in filename
    let file_name = expanded.file_name().and_then(|n| n.to_str()).unwrap_or("");

    if file_name.contains('*') {
        // Simple glob: match * against directory entries
        let parent = expanded.parent().unwrap_or(Path::new("."));
        let pattern = file_name;
        let Ok(entries) = std::fs::read_dir(parent) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                simple_glob_match(pattern, &name)
            })
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        paths.sort();
        paths
    } else if expanded.is_file() {
        vec![expanded]
    } else {
        Vec::new()
    }
}

/// Simple glob matching: supports `*` as "match any" in a single segment.
/// E.g., `*.conf` matches `prod.conf`, `*` matches everything.
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        return name.starts_with(prefix) && name.ends_with(suffix);
    }
    pattern == name
}

/// Back-patch multi-alias Host entries.
/// When "Host a b c" is parsed, a and b get pushed with empty fields,
/// c gets the actual fields. Copy c's fields back to a and b.
fn backpatch_multi_alias(hosts: &mut [SshHost]) {
    let len = hosts.len();
    if len < 2 {
        return;
    }
    // Walk backwards: if an entry has fields and the previous entry has none,
    // copy fields to the previous entry.
    for i in (1..len).rev() {
        let has_fields = hosts[i].hostname.is_some()
            || hosts[i].user.is_some()
            || hosts[i].port.is_some()
            || hosts[i].identity_file.is_some()
            || hosts[i].proxy_jump.is_some();
        let prev_empty = hosts[i - 1].hostname.is_none()
            && hosts[i - 1].user.is_none()
            && hosts[i - 1].port.is_none()
            && hosts[i - 1].identity_file.is_none()
            && hosts[i - 1].proxy_jump.is_none();

        if has_fields && prev_empty {
            let fields = hosts[i].clone();
            hosts[i - 1].hostname = fields.hostname;
            hosts[i - 1].user = fields.user;
            hosts[i - 1].port = fields.port;
            hosts[i - 1].identity_file = fields.identity_file;
            hosts[i - 1].proxy_jump = fields.proxy_jump;
        }
    }
}

/// Resolve an SSH row action into the command it stands for.
///
/// Mirrors `packages::resolve_action`: the frontend sends back an action id and
/// a target, never a command string, so a row can never smuggle an arbitrary
/// invocation through the row-action channel. The alias is validated against
/// the same rules `~/.ssh/config` itself allows.
pub fn resolve_action(id: &str, target: &str) -> Result<String, String> {
    if id != "connect" {
        return Err(format!("Unknown SSH action '{id}'"));
    }
    if !is_valid_host_alias(target) {
        return Err(format!("Invalid SSH host '{target}'"));
    }
    Ok(format!("ssh {target}"))
}

/// Whether `s` is a plausible SSH host alias.
///
/// Allowlist rather than denylist, for the same reason as package names: the
/// set of legal aliases is small and describable, whereas enumerating dangerous
/// bytes invites omissions. Aliases in `~/.ssh/config` are hostnames or
/// hostname-like labels, optionally `user@host`, so alphanumerics plus
/// `-_.@` covers real configs.
///
/// The resolved string is passed as an argument and never interpolated into a
/// shell, but this still matters: without it a crafted alias could reach
/// `open_in_terminal` carrying `UserConfirmed` clearance.
fn is_valid_host_alias(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'))
}

pub struct SshHandler;

impl Default for SshHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl SshHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ActionHandler for SshHandler {
    fn triggers(&self) -> &'static [crate::action_registry::Trigger] {
        use crate::action_registry::Trigger;
        static TRIGGERS: &[Trigger] = &[Trigger::keywords(&["ssh"])];
        TRIGGERS
    }

    fn id(&self) -> &str {
        "ssh"
    }

    fn description(&self) -> &str {
        "Connect to SSH hosts from ~/.ssh/config"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Developer
    }

    async fn execute(&self, ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let query = args.trim();

        if query.is_empty() {
            let hosts = load_ssh_hosts();
            if hosts.is_empty() {
                return Ok(ActionResult::ok(
                    "No SSH hosts found. Add hosts to ~/.ssh/config",
                    OutputType::Text,
                ));
            }
            // Rows, not a joined string. The alias/description split was already
            // there — it was just being flattened into "  alias — desc" with
            // hand-written padding that no other handler agreed on, and the
            // result could not be acted on. Same data, one renderer, and each
            // host now carries its own Connect action.
            let rows: Vec<Row> = hosts
                .iter()
                .map(|h| {
                    let row = Row::new(&h.alias)
                        .subtitle(h.display_description())
                        .action("connect", "Connect", &h.alias, None);
                    // ProxyJump is the one field worth surfacing on the row: it
                    // means the connection is indirect, which changes what a
                    // failure means. The rest stays in the subtitle.
                    match h.proxy_jump {
                        Some(ref j) => row.accessory_text(format!("via {j}")),
                        None => row,
                    }
                })
                .collect();
            return Ok(ActionResult {
                success: true,
                output: Output::Rows {
                    sections: vec![Section {
                        title: Some(format!("SSH hosts ({})", rows.len())),
                        rows,
                        handler: "ssh".to_string(),
                    }],
                },
                ..Default::default()
            });
        }

        let hosts = load_ssh_hosts();

        // Find matching host: exact alias first, then substring
        let host = hosts
            .iter()
            .find(|h| h.alias.eq_ignore_ascii_case(query))
            .or_else(|| {
                hosts
                    .iter()
                    .find(|h| h.alias.to_lowercase().contains(&query.to_lowercase()))
            });

        let ssh_cmd = if let Some(h) = host {
            // Use the alias directly — SSH config handles the rest
            format!("ssh {}", h.alias)
        } else {
            // Treat as raw host (user@host or just host)
            format!("ssh {query}")
        };

        // The ssh invocation is a plain `ssh <host>` (no shell metacharacters),
        // and it has already been routed/validated by the Rules Engine, so it
        // carries user clearance for the shell gate's decider.
        let pid = shell_exec::open_in_terminal(
            &ssh_cmd,
            None,
            ctx.terminal.as_deref(),
            shell_exec::Clearance::UserConfirmed,
        )?;
        crate::process_tracker::track(pid, &ssh_cmd, None);

        let desc = host
            .map(|h| format!("{} ({})", h.alias, h.display_description()))
            .unwrap_or_else(|| query.to_string());

        Ok(ActionResult::ok(
            format!("Connecting to {desc}"),
            OutputType::Status,
        ))
    }

    async fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        let hosts = load_ssh_hosts();
        let partial = partial.trim();

        if partial.is_empty() {
            // Show all hosts alphabetically
            return hosts
                .iter()
                .enumerate()
                .map(|(i, h)| CompletionItem {
                    label: h.alias.clone(),
                    icon_path: None,
                    score: (1000 - i as u16).max(1),
                    description: Some(h.display_description()),
                    reason: None,
                    thumb_b64: None,
                    run: Some(format!("ssh {}", h.alias)),
                    ..Default::default()
                })
                .take(20)
                .collect();
        }

        // Fuzzy match using cached nucleo matcher
        let mut matcher_guard = MATCHER.lock().unwrap();
        let matcher = matcher_guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));

        let pattern = Pattern::new(
            partial,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );

        let mut scored: Vec<(u32, &SshHost)> = hosts
            .iter()
            .filter_map(|h| {
                let haystack = format!("{} {}", h.alias, h.display_description());
                let haystack_chars: Vec<char> = haystack.chars().collect();
                let utf32 = nucleo_matcher::Utf32Str::Unicode(&haystack_chars);
                let score = pattern.score(utf32, matcher);
                score.map(|s| (s, h))
            })
            .collect();

        scored.sort_by_key(|b| std::cmp::Reverse(b.0));

        scored
            .into_iter()
            .take(20)
            .enumerate()
            .map(|(i, (score, h))| CompletionItem {
                label: h.alias.clone(),
                icon_path: None,
                score: (score.min(999) as u16).saturating_sub(i as u16).max(1),
                description: Some(h.display_description()),
                reason: None,
                thumb_b64: None,
                run: Some(format!("ssh {}", h.alias)),
                ..Default::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CONFIG: &str = r#"
# Global defaults
Host *
    ServerAliveInterval 60

Host web-prod
    HostName 10.0.1.50
    User deploy
    Port 22
    IdentityFile ~/.ssh/id_prod

Host db-staging
    HostName staging-db.example.com
    User admin
    Port 5432

Host jump
    HostName bastion.example.com
    User ec2-user

Match host *.internal
    ProxyJump jump

Host dev
    HostName 192.168.1.100
"#;

    fn parse_test(content: &str) -> Vec<SshHost> {
        parse_ssh_config(content, Path::new("/tmp"), 0)
    }

    #[test]
    fn parse_hosts() {
        let hosts = parse_test(SAMPLE_CONFIG);
        assert_eq!(hosts.len(), 4, "expected 4 hosts, got: {hosts:?}");
    }

    #[test]
    fn skip_wildcard() {
        let hosts = parse_test(SAMPLE_CONFIG);
        assert!(
            !hosts.iter().any(|h| h.alias == "*"),
            "wildcard Host * should be skipped"
        );
    }

    #[test]
    fn parse_fields() {
        let hosts = parse_test(SAMPLE_CONFIG);
        let web = hosts.iter().find(|h| h.alias == "web-prod").unwrap();
        assert_eq!(web.hostname.as_deref(), Some("10.0.1.50"));
        assert_eq!(web.user.as_deref(), Some("deploy"));
        assert_eq!(web.port, Some(22));
        assert_eq!(web.identity_file.as_deref(), Some("~/.ssh/id_prod"));
    }

    #[test]
    fn parse_optional_fields() {
        let hosts = parse_test(SAMPLE_CONFIG);
        let dev = hosts.iter().find(|h| h.alias == "dev").unwrap();
        assert_eq!(dev.hostname.as_deref(), Some("192.168.1.100"));
        assert!(dev.user.is_none());
        assert!(dev.port.is_none());
        assert!(dev.identity_file.is_none());
    }

    #[test]
    fn port_parsing() {
        let hosts = parse_test(SAMPLE_CONFIG);
        let db = hosts.iter().find(|h| h.alias == "db-staging").unwrap();
        assert_eq!(db.port, Some(5432));
    }

    #[test]
    fn skip_match_blocks() {
        let hosts = parse_test(SAMPLE_CONFIG);
        // "dev" appears after Match block — should still be parsed
        assert!(hosts.iter().any(|h| h.alias == "dev"));
        // No host named "*.internal" should appear
        assert!(!hosts.iter().any(|h| h.alias.contains("internal")));
    }

    #[test]
    fn display_description() {
        let host = SshHost {
            alias: "prod".to_string(),
            hostname: Some("10.0.1.1".to_string()),
            user: Some("admin".to_string()),
            port: Some(2222),
            identity_file: None,
            proxy_jump: None,
        };
        let desc = host.display_description();
        assert!(desc.contains("admin@10.0.1.1"), "desc: {desc}");
        assert!(desc.contains(":2222"), "desc: {desc}");
    }

    #[test]
    fn display_proxy_jump() {
        let host = SshHost {
            alias: "internal".to_string(),
            hostname: Some("10.0.1.50".to_string()),
            user: None,
            port: None,
            identity_file: None,
            proxy_jump: Some("bastion".to_string()),
        };
        let desc = host.display_description();
        assert!(desc.contains("via bastion"), "desc: {desc}");
    }

    #[test]
    fn parse_proxy_jump() {
        let config = r#"
Host internal
    HostName 10.0.1.50
    ProxyJump bastion
"#;
        let hosts = parse_test(config);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].proxy_jump.as_deref(), Some("bastion"));
    }

    #[test]
    fn multi_alias_host() {
        let config = r#"
Host web-prod web-staging web-dev
    HostName 10.0.1.50
    User deploy
    Port 2222
"#;
        let hosts = parse_test(config);
        assert_eq!(hosts.len(), 3, "expected 3 hosts, got: {hosts:?}");

        // All three should have the same fields
        for alias in &["web-prod", "web-staging", "web-dev"] {
            let h = hosts
                .iter()
                .find(|h| h.alias == *alias)
                .unwrap_or_else(|| panic!("missing alias: {alias}"));
            assert_eq!(
                h.hostname.as_deref(),
                Some("10.0.1.50"),
                "hostname for {alias}"
            );
            assert_eq!(h.user.as_deref(), Some("deploy"), "user for {alias}");
            assert_eq!(h.port, Some(2222), "port for {alias}");
        }
    }

    #[test]
    fn include_resolves_glob() {
        // Test that resolve_include handles ~ expansion and relative paths
        let paths = resolve_include("/nonexistent/path/*.conf", Path::new("/tmp"));
        assert!(paths.is_empty(), "nonexistent glob should return empty");
    }

    #[test]
    fn backpatch_preserves_different_blocks() {
        let config = r#"
Host alpha
    HostName alpha.example.com
    User root

Host beta gamma
    HostName shared.example.com
    User deploy
"#;
        let hosts = parse_test(config);
        assert_eq!(hosts.len(), 3);

        let alpha = hosts.iter().find(|h| h.alias == "alpha").unwrap();
        assert_eq!(alpha.hostname.as_deref(), Some("alpha.example.com"));
        assert_eq!(alpha.user.as_deref(), Some("root"));

        let beta = hosts.iter().find(|h| h.alias == "beta").unwrap();
        assert_eq!(beta.hostname.as_deref(), Some("shared.example.com"));
        assert_eq!(beta.user.as_deref(), Some("deploy"));

        let gamma = hosts.iter().find(|h| h.alias == "gamma").unwrap();
        assert_eq!(gamma.hostname.as_deref(), Some("shared.example.com"));
        assert_eq!(gamma.user.as_deref(), Some("deploy"));
    }

    /// Row actions arrive from the frontend as (id, target) pairs, so this
    /// resolver is a trust boundary: whatever it returns is executed with
    /// `UserConfirmed` clearance. These pin that it refuses anything it does
    /// not recognise rather than passing it through.
    mod row_actions {
        use super::super::{is_valid_host_alias, resolve_action};

        #[test]
        fn connect_resolves_to_a_plain_ssh_invocation() {
            assert_eq!(resolve_action("connect", "prod").unwrap(), "ssh prod");
            // user@host is a legal alias form and must survive.
            assert_eq!(
                resolve_action("connect", "deploy@web1.example.com").unwrap(),
                "ssh deploy@web1.example.com"
            );
        }

        #[test]
        fn an_unknown_action_id_is_refused() {
            // The failure mode this prevents: a new row action added to the UI
            // without a resolver arm silently resolving to something else.
            assert!(resolve_action("delete", "prod").is_err());
            assert!(resolve_action("", "prod").is_err());
        }

        #[test]
        fn shell_metacharacters_never_reach_the_command() {
            // The alias is not interpolated into a shell today, but it is
            // executed with user clearance — so the validator, not the caller's
            // current implementation, is what keeps this safe.
            for hostile in [
                "prod; rm -rf /",
                "prod && curl evil.sh",
                "prod$(whoami)",
                "prod`id`",
                "prod|tee /tmp/x",
                "prod\nssh other",
                "../../etc/passwd",
                "prod' -oProxyCommand=evil",
            ] {
                assert!(
                    resolve_action("connect", hostile).is_err(),
                    "must reject: {hostile}"
                );
            }
        }

        #[test]
        fn real_aliases_are_accepted() {
            // The other half: a validator that rejected everything would pass
            // the test above while breaking the feature.
            for ok in ["prod", "web-1", "db_2", "host.example.com", "u@h", "a1"] {
                assert!(is_valid_host_alias(ok), "must accept: {ok}");
            }
            assert!(!is_valid_host_alias(""));
            assert!(!is_valid_host_alias(&"a".repeat(256)));
        }
    }
}
