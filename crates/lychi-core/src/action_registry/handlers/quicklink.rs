//! The quicklink handler — runs user-defined parameterized commands.
//!
//! Supersedes the old `bang` handler, which could only produce URLs. A
//! quicklink carries an explicit [`QuicklinkKind`], so one keyword table can
//! now yield a web search, a shell command, an opened path, or another Lychi
//! command.
//!
//! ## Authorization happens on the EXPANDED string
//!
//! This is the load-bearing rule of this module. A template is authored once;
//! its expansion depends on runtime input, so the template's own text is not
//! evidence about what will actually run:
//!
//! ```text
//! template:  rm -rf {path}    → looks parameterised and unremarkable
//! expanded:  rm -rf '/'       → what the denylist must actually judge
//! ```
//!
//! So [`ActionHandler::assess_risk`] expands first and asks the central decider
//! in [`crate::rules::shell`] about the exact string that will run. Because
//! `assess_risk` runs *before* execution, the Rules Engine turns a `Confirm`
//! into a real prompt and a `Deny` into a block — the same flow `run` uses.
//!
//! Escaping (in [`crate::quicklinks::expand`]) and authorization are separate
//! defenses, and both are needed. Quoting stops runtime *input* from adding
//! shell syntax; the decider judges what the *template* does. Neither subsumes
//! the other — a user can author `rm -rf {path}`, which quotes perfectly and is
//! still a command the denylist has to catch.
//!
//! ## Shell quicklinks do not spawn here
//!
//! A `Shell` quicklink never reaches this handler's `execute`. The executor
//! resolves it to `action_id: "run"` with the expanded command, so it travels
//! the identical path as a typed `run` command — Rules Engine, then
//! `shell_exec`'s own spawn-point gate. Adding a second spawn point would mean
//! a second place that must remember to call the gate, which is the exact shape
//! of the bypasses found in the earlier security audit.
//!
//! [`resolve_route`] is the single function that decides which handler an
//! expanded quicklink belongs to, so that mapping lives in one place.

use async_trait::async_trait;

use crate::action_registry::{
    ActionHandler, ActionResult, CommandCategory, CompletionItem, ExecContext, RiskAssessment,
    RiskContext, RiskLevel,
};
use crate::error::LychiError;
use crate::quicklinks::{Quicklink, QuicklinkKind, expand_template};

pub struct QuicklinkHandler {
    links: Vec<Quicklink>,
}

impl QuicklinkHandler {
    pub fn new(links: Vec<Quicklink>) -> Self {
        Self { links }
    }

    /// The configured keywords (lowercased), for the router to recognise.
    pub fn keywords(&self) -> Vec<String> {
        self.links
            .iter()
            .map(|l| l.keyword.to_lowercase())
            .collect()
    }

    /// Find a quicklink by keyword, case-insensitively.
    pub fn find(&self, keyword: &str) -> Option<&Quicklink> {
        self.links
            .iter()
            .find(|l| l.keyword.eq_ignore_ascii_case(keyword.trim()))
    }

    /// Resolve `"keyword input"` to its quicklink and expanded template.
    ///
    /// Shared by [`ActionHandler::assess_risk`] and [`ActionHandler::execute`]
    /// so the string that gets *judged* is byte-for-byte the string that gets
    /// *run*. Two independent expansions could drift apart, and that gap is
    /// precisely the time-of-check/time-of-use hole this handler exists to
    /// close.
    fn resolve(&self, args: &str) -> Option<(&Quicklink, String)> {
        let (keyword, input) = match args.trim().split_once(char::is_whitespace) {
            Some((k, q)) => (k, q.trim()),
            None => (args.trim(), ""),
        };
        let link = self.find(keyword)?;
        let expanded = expand_template(&link.template, input, link.kind).ok()?;
        Some((link, expanded.text))
    }

    /// Which handler an expanded quicklink should be dispatched to, and with
    /// what args. Returns `None` for an unknown keyword or unparseable
    /// template.
    ///
    /// The executor calls this instead of routing every quicklink to this
    /// handler, so `Shell` and `Command` kinds re-enter the normal pipeline and
    /// meet the gates that already guard those actions. Keeping the mapping in
    /// one function means "which gate applies to which kind" is stated once.
    pub fn resolve_route(&self, args: &str) -> Option<(String, String)> {
        let (link, expanded) = self.resolve(args)?;
        Some(match link.kind {
            // Through `run`: Rules Engine + shell_exec's spawn-point decider.
            QuicklinkKind::Shell => ("run".to_string(), expanded),
            // Back through the router, so the target command's own gate applies.
            QuicklinkKind::Command => ("__reroute__".to_string(), expanded),
            // Handled here — the URI and path deciders run in `execute`.
            QuicklinkKind::Url | QuicklinkKind::Open => {
                ("quicklink".to_string(), args.trim().to_string())
            }
        })
    }
}

/// Expand a leading `~` to the home directory. `Open` paths never reach a
/// shell, so this one substitution is done explicitly rather than inherited.
fn expand_tilde(path: &str) -> String {
    let trimmed = path.trim();
    if let Some(rest) = trimmed.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return format!("{}/{}", home.to_string_lossy(), rest);
    }
    if trimmed == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return home.to_string_lossy().into_owned();
    }
    trimmed.to_string()
}

#[async_trait]
impl ActionHandler for QuicklinkHandler {
    fn id(&self) -> &str {
        "quicklink"
    }

    fn description(&self) -> &str {
        "User-defined shortcuts (gh, npm, mdn, …)"
    }

    fn category(&self) -> CommandCategory {
        CommandCategory::Web
    }

    /// Judge the EXPANDED command, before anything runs.
    ///
    /// Only `Shell` quicklinks can carry real risk: a URL is gated by the URI
    /// decider at open time, a path by the path decider, and a `Command`
    /// re-enters the router and meets that command's own gate.
    fn assess_risk(&self, args: &str, _ctx: &RiskContext<'_>) -> RiskAssessment {
        let Some((link, expanded)) = self.resolve(args) else {
            return RiskAssessment::level(RiskLevel::Low);
        };
        if link.kind != QuicklinkKind::Shell {
            return RiskAssessment::level(RiskLevel::Low);
        }

        use crate::rules::shell::{ShellDecision, authorize};
        match authorize(&expanded) {
            ShellDecision::Allow => RiskAssessment::level(RiskLevel::Low),
            // Both Confirm and Deny surface as a confirmation here. The engine's
            // `RiskAssessment` vocabulary has no Deny variant — but the absolute
            // block is not lost: `run` re-checks with the central decider at the
            // spawn point, where a hard `Deny` is refused outright regardless of
            // any confirmation given. This layer's job is to make sure the user
            // is asked; `shell_exec`'s job is to make sure a denied command
            // cannot run even if they say yes.
            ShellDecision::Confirm { reason } | ShellDecision::Deny { reason } => {
                RiskAssessment::confirm(format!("{reason}\n\n{expanded}"))
            }
        }
    }

    async fn completions(&self, _partial: &str) -> Vec<CompletionItem> {
        // The keyword row is rendered by the executor's completion path (it owns
        // the keyword; the handler only ever sees the args after it).
        Vec::new()
    }

    async fn execute(&self, _ctx: &ExecContext, args: &str) -> Result<ActionResult, LychiError> {
        let (keyword, _) = match args.trim().split_once(char::is_whitespace) {
            Some((k, q)) => (k, q),
            None => (args.trim(), ""),
        };

        let Some((link, expanded)) = self.resolve(args) else {
            // Either an unknown keyword, or a template that fails to parse
            // (only reachable via a hand-edited config, since save-time
            // validation rejects malformed templates).
            return Ok(ActionResult::err(format!(
                "Unknown or malformed shortcut: {keyword}"
            )));
        };

        match link.kind {
            QuicklinkKind::Url => {
                use crate::rules::uri::{UriDecision, authorize_uri};
                match authorize_uri(&expanded) {
                    UriDecision::Allow => Ok(ActionResult::navigate(expanded, true)),
                    UriDecision::Deny { reason } => Ok(ActionResult::err(reason)),
                }
            }
            QuicklinkKind::Open => {
                use crate::rules::path::{PathDecision, authorize_path};
                let path = expand_tilde(&expanded);
                match authorize_path(std::path::Path::new(&path)) {
                    PathDecision::Allow { path } => Ok(ActionResult::navigate(
                        format!("file://{}", path.display()),
                        true,
                    )),
                    PathDecision::Deny { reason } => Ok(ActionResult::err(reason)),
                }
            }
            // Unreachable in normal operation: the executor consults
            // `resolve_route` first and sends these kinds to `run` / back
            // through the router. Reaching here means a caller bypassed that,
            // so refuse rather than quietly becoming a second spawn point.
            QuicklinkKind::Shell | QuicklinkKind::Command => Ok(ActionResult::err(format!(
                "Quicklink '{}' must be dispatched through the router",
                link.keyword
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_registry::Output;

    fn link(keyword: &str, kind: QuicklinkKind, template: &str) -> Quicklink {
        Quicklink {
            keyword: keyword.to_string(),
            name: String::new(),
            kind,
            template: template.to_string(),
        }
    }

    fn handler() -> QuicklinkHandler {
        QuicklinkHandler::new(vec![
            link(
                "gh",
                QuicklinkKind::Url,
                "https://github.com/search?q={query}",
            ),
            link("legacy", QuicklinkKind::Url, "https://legacy.com/?q="),
            link("co", QuicklinkKind::Shell, "git checkout {branch}"),
            link("wipe", QuicklinkKind::Shell, "rm -rf {path}"),
            link("mk", QuicklinkKind::Shell, "mkdir {dir}"),
            link("proj", QuicklinkKind::Open, "~/projects/{name}"),
            link("n", QuicklinkKind::Command, "note add {text}"),
        ])
    }

    async fn run(input: &str) -> ActionResult {
        handler()
            .execute(&ExecContext::default(), input)
            .await
            .unwrap()
    }

    fn risk(input: &str) -> RiskAssessment {
        handler().assess_risk(
            input,
            &RiskContext {
                cwd: None,
                workspace_root: None,
            },
        )
    }

    // ---- URL kind -------------------------------------------------------

    #[tokio::test]
    async fn url_quicklink_navigates_with_encoded_input() {
        match run("gh tokio runtime").await.output {
            Output::Navigate { url, .. } => {
                assert_eq!(url, "https://github.com/search?q=tokio%20runtime");
            }
            other => panic!("expected navigate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn legacy_prefix_template_still_appends() {
        // Back-compat with every shipped built-in and existing user config.
        match run("legacy rust").await.output {
            Output::Navigate { url, .. } => assert_eq!(url, "https://legacy.com/?q=rust"),
            other => panic!("expected navigate, got {other:?}"),
        }
    }

    // ---- Shell kind: the security-relevant half -------------------------

    #[test]
    fn a_benign_shell_quicklink_is_low_risk() {
        assert_eq!(risk("co main").level, RiskLevel::Low);
    }

    #[test]
    fn a_destructive_expansion_is_flagged_even_though_the_template_looked_fine() {
        // `rm -rf {path}` is an innocuous-looking template. The gate must judge
        // what it EXPANDS to — the whole reason assessment is post-expansion.
        let r = risk("wipe /");
        assert_ne!(
            r.level,
            RiskLevel::Low,
            "rm -rf / must not slip through as low risk"
        );
    }

    #[test]
    fn a_mutating_expansion_asks_first() {
        // `mkdir` is in MODERATE_PATTERNS → Confirm, not silent execution.
        assert_ne!(risk("mk newdir").level, RiskLevel::Low);
    }

    #[test]
    fn risk_is_assessed_on_the_expansion_not_the_template() {
        // Same quicklink, different input: one benign, one not. If assessment
        // read the template, these would be identical.
        assert_eq!(risk("co main").level, RiskLevel::Low);
        assert_ne!(risk("co main; mkdir x").level, RiskLevel::Low);
    }

    #[test]
    fn shell_quicklinks_route_to_run_rather_than_spawning() {
        // A second spawn point would be a second place that must remember the
        // gate. It dispatches to `run` instead, which owns that gate.
        let (action, args) = handler().resolve_route("co main").unwrap();
        assert_eq!(action, "run");
        assert_eq!(args, "git checkout 'main'");
    }

    #[test]
    fn injected_shell_syntax_is_quoted_into_a_single_argument() {
        let (action, args) = handler().resolve_route("co main; rm -rf ~").unwrap();
        assert_eq!(action, "run");
        assert_eq!(args, "git checkout 'main; rm -rf ~'");
    }

    #[tokio::test]
    async fn calling_execute_directly_for_a_shell_kind_refuses() {
        // Defense in depth: if some future caller skips `resolve_route`, this
        // handler must not become an ungated spawn point.
        let r = run("co main").await;
        assert!(!r.success, "expected refusal, got {r:?}");
    }

    // ---- Open kind ------------------------------------------------------

    #[tokio::test]
    async fn open_quicklink_produces_a_file_url() {
        match run("proj lychi").await.output {
            Output::Navigate { url, .. } => {
                assert!(url.starts_with("file://"), "got {url}");
                assert!(url.ends_with("/projects/lychi"), "got {url}");
            }
            other => panic!("expected navigate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_quicklink_refuses_a_traversal_escape() {
        let h = QuicklinkHandler::new(vec![link("esc", QuicklinkKind::Open, "/{p}")]);
        let r = h
            .execute(&ExecContext::default(), "esc ../../../../../../etc/passwd")
            .await
            .unwrap();
        assert!(!r.success, "traversal above root must be denied, got {r:?}");
    }

    // ---- Command kind ---------------------------------------------------

    #[test]
    fn command_quicklink_reenters_the_router_verbatim() {
        let (action, args) = handler().resolve_route("n buy milk").unwrap();
        assert_eq!(action, "__reroute__");
        assert_eq!(args, "note add buy milk");
    }

    #[test]
    fn url_and_open_kinds_stay_with_this_handler() {
        let h = handler();
        assert_eq!(h.resolve_route("gh x").unwrap().0, "quicklink");
        assert_eq!(h.resolve_route("proj x").unwrap().0, "quicklink");
    }

    #[test]
    fn an_unknown_keyword_has_no_route() {
        assert!(handler().resolve_route("nope whatever").is_none());
    }

    // ---- misc -----------------------------------------------------------

    #[tokio::test]
    async fn unknown_keyword_reports_rather_than_running_anything() {
        assert!(!run("nope whatever").await.success);
    }

    #[test]
    fn an_unknown_keyword_is_low_risk_rather_than_panicking() {
        assert_eq!(risk("nope whatever").level, RiskLevel::Low);
    }

    #[test]
    fn keywords_are_lowercased_for_the_router() {
        let h = QuicklinkHandler::new(vec![link("GH", QuicklinkKind::Url, "https://x.com/{q}")]);
        assert_eq!(h.keywords(), vec!["gh"]);
        assert!(h.find("gh").is_some());
    }
}
