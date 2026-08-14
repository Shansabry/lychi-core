//! A rough capability tier for the currently-selected model — an *estimate*,
//! never a benchmark.
//!
//! There is no way to measure how well a model will actually perform from inside
//! a settings panel without running graded prompts against it. What we *can* read
//! cheaply are three proxies that correlate with capability:
//!
//!   1. **Parameter count** — the strongest single signal. A 3B model and a 70B
//!      model are not in the same league, and the number is discoverable (Ollama
//!      `/api/show` `details.parameter_size`, the built-in models' known specs,
//!      or parsed from a model id like `llama3:8b`).
//!   2. **Quantization** — a heavily quantized model (Q2/Q3) is a visibly weaker
//!      version of the same weights. It shifts the estimate down a tier.
//!   3. **Provider tier** — a hosted frontier model (cloud, or a BYO endpoint
//!      pointed at a Claude/GPT/Gemini-class id) is Full by construction.
//!
//! The output is deliberately coarse — three tiers — because the inputs only
//! justify a coarse answer. Param count *correlates with* capability; it does not
//! *equal* it (a well-tuned 7B can beat a mediocre 13B). So this is surfaced to
//! the user as an estimate, and the only behaviour it drives is showing the
//! "expect simpler reasoning" caveat when the tier is [`Tier::Basic`].
//!
//! Computed on a model/mode change and stored (see `providers::capability`),
//! never recomputed per render.

// The stored types (`Tier`, `Estimate`) live in `capability` — the module that
// persists them in the model-caps file. This module owns only the SCORING.
pub use super::capability::{Estimate, Tier};

/// The inputs the estimator scores. Assembled by the caller from whatever the
/// current mode makes available; `None` means "couldn't determine".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Signals {
    /// Billions of parameters, if known (7.0 for a 7B model).
    pub params_b: Option<f32>,
    /// Quantization bits-per-weight bucket, if known (see [`quant_is_heavy`]).
    pub heavy_quant: bool,
    /// True for a hosted frontier model (cloud, or a frontier-named BYO id).
    pub frontier: bool,
    /// True when this is a BYO endpoint (the model id is user-chosen and usually
    /// a real remote model, so the neutral default is Capable, not Basic).
    pub byo: bool,
}

/// Score the signals into a tier.
///
/// The rule, in order:
///   - A frontier hosted model is always [`Tier::Full`] — param count is moot.
///   - Otherwise, if a parameter count is known, bucket it: ≥30B Full, ≥7B
///     Capable, else Basic.
///   - With no param count: a BYO endpoint defaults to Capable (the user pointed
///     it at a real model), everything else to Basic (an unknown local model is
///     more likely small than large).
///   - Heavy quantization then demotes the result one notch — a Q3 13B behaves
///     closer to a smaller model.
pub fn score(sig: &Signals) -> Tier {
    if sig.frontier {
        return Tier::Full;
    }

    let base = match sig.params_b {
        Some(p) if p >= 30.0 => Tier::Full,
        Some(p) if p >= 7.0 => Tier::Capable,
        Some(_) => Tier::Basic,
        None if sig.byo => Tier::Capable,
        None => Tier::Basic,
    };

    if sig.heavy_quant { base.demote() } else { base }
}

/// Whether a BYO model id names a hosted FRONTIER model — Claude/GPT/Gemini/Grok
/// class. Matched by well-known family substrings, and a "small/mini/flash/nano/
/// haiku" qualifier drops it back out of frontier (those are the cheaper tiers).
///
/// Name-based and therefore best-effort: an unknown BYO id is NOT frontier, so it
/// falls through to the byo default (Capable), never over-promising Full.
pub fn is_frontier_model(model: &str) -> bool {
    let m = model.to_lowercase();
    // A small-tier qualifier must appear as its own token (delimited by a
    // non-alphanumeric or a string edge), NOT as a bare substring — otherwise
    // "gemini" false-matches "mini" and a frontier model is wrongly demoted.
    let is_token = |hay: &str, needle: &str| {
        hay.match_indices(needle).any(|(i, _)| {
            let before_ok = i == 0 || !hay.as_bytes()[i - 1].is_ascii_alphanumeric();
            let after = i + needle.len();
            let after_ok = after >= hay.len() || !hay.as_bytes()[after].is_ascii_alphanumeric();
            before_ok && after_ok
        })
    };
    // The cheaper/smaller tiers of otherwise-frontier families are not "Full".
    let small = [
        "mini", "flash", "nano", "haiku", "small", "lite", "8b", "7b", "1b", "3b",
    ];
    if small.iter().any(|s| is_token(&m, s)) {
        return false;
    }
    let frontier = [
        "claude",
        "gpt-4",
        "gpt-5",
        "o1",
        "o3",
        "gemini",
        "grok",
        "opus",
        "sonnet",
        "deepseek",
        "llama-3.1-405",
        "llama-3.3-70",
        "qwen2.5-72",
        "mistral-large",
    ];
    frontier.iter().any(|f| m.contains(f))
}

/// Compute the capability estimate for the current AI configuration.
///
/// `mode` is the AI mode ("cloud"/"byo"/"ollama"/"local"/…); `provider` the BYO
/// preset id; `model` the resolved model id for the active mode; `params_hint`
/// and `quant_hint` are optional structured values a caller already fetched
/// (Ollama `/api/show`, the built-in GGUF specs) — when absent, both are parsed
/// from the model id. Pure: all I/O (the `/api/show` call) happens in the caller.
pub fn estimate(
    mode: &str,
    _provider: &str,
    model: &str,
    params_hint: Option<f32>,
    quant_hint: Option<&str>,
) -> Estimate {
    let byo = mode == "byo";
    let frontier = mode == "cloud" || (byo && is_frontier_model(model));

    // Prefer a structured param count; else parse it from the model id.
    let params_b = params_hint.or_else(|| parse_params_b(model));
    // Quant: prefer the structured label; else parse from the id (e.g. "…-q4_0").
    let quant_raw = quant_hint
        .map(str::to_string)
        .unwrap_or_else(|| model.to_string());
    let heavy_quant = quant_is_heavy(&quant_raw);

    let sig = Signals {
        params_b,
        heavy_quant,
        frontier,
        byo,
    };
    let tier = score(&sig);

    let params_label = params_b.map(fmt_params).unwrap_or_default();
    let quant_label = quant_label(&quant_raw);
    Estimate {
        tier,
        params_label,
        quant_label,
    }
}

/// The model id the estimate should score, for a given AI mode. Mirrors how each
/// mode resolves its active model (`byo`/`cloud` → `model`, `ollama` →
/// `ollama_model`, `local` → `local_model`).
pub fn active_model(cfg: &crate::config::schema::AiConfig) -> &str {
    match cfg.mode.as_str() {
        "ollama" => &cfg.ollama_model,
        "local" => &cfg.local_model,
        _ => &cfg.model, // byo, cloud
    }
}

/// Compute the estimate for an AI config and store it against its `provider/model`
/// key (via `capability::record_estimate`). Called on a model/mode change; a
/// no-op when there is no model to score. Best-effort — a storage failure just
/// means the meter shows nothing until the next change.
pub fn compute_and_store(cfg: &crate::config::schema::AiConfig) {
    let model = active_model(cfg);
    if model.trim().is_empty() && cfg.mode != "cloud" {
        return;
    }
    let est = estimate(&cfg.mode, &cfg.provider, model, None, None);
    if let Err(e) = super::capability::record_estimate(&cfg.provider, model, est) {
        tracing::warn!("[potential] could not store capability estimate: {e}");
    }
}

/// Format a billions-of-params value for display: "3B", "1.5B", "135M".
fn fmt_params(b: f32) -> String {
    if b < 1.0 {
        format!("{}M", (b * 1000.0).round() as u32)
    } else if (b.fract()).abs() < 0.05 {
        format!("{}B", b.round() as u32)
    } else {
        format!("{b:.1}B")
    }
}

/// Parse a parameter count out of a free-form size string or model id.
///
/// Handles the shapes we actually see: Ollama's `details.parameter_size`
/// ("7B", "7.2B", "70B"), the tag inside a model id (`llama3:8b`,
/// `qwen2.5-1.5b-instruct`), and a bare number of billions. Returns billions.
///
/// Deliberately conservative: anything it can't confidently read returns `None`
/// so the caller falls back to the byo/local default rather than a wrong number.
pub fn parse_params_b(s: &str) -> Option<f32> {
    let lower = s.to_lowercase();
    // Scan for a `<number>b` or `<number>m` token. `b` = billions, `m` =
    // millions (→ fractional billions). Take the FIRST plausible match.
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            // Read the number (digits + optional single dot).
            let start = i;
            let mut seen_dot = false;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || (bytes[i] == b'.' && !seen_dot))
            {
                if bytes[i] == b'.' {
                    seen_dot = true;
                }
                i += 1;
            }
            let num: f32 = lower[start..i].parse().ok()?;
            // Unit suffix immediately after.
            match bytes.get(i) {
                Some(b'b') => return Some(num),
                Some(b'm') => return Some(num / 1000.0),
                _ => {
                    // A number with no b/m unit is not a param count we trust
                    // (could be a version, a date). Keep scanning.
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Whether a quantization label denotes a heavy (≤3 bit-ish) quant that visibly
/// degrades quality. Q2/Q3/IQ2/IQ3 count; Q4 and up do not.
pub fn quant_is_heavy(quant: &str) -> bool {
    let q = quant.to_lowercase();
    // Match a leading q/iq followed by a single digit ≤ 3.
    let digits: Vec<char> = q.chars().filter(|c| c.is_ascii_digit()).collect();
    match digits.first().and_then(|c| c.to_digit(10)) {
        Some(n) if q.contains('q') => n <= 3,
        _ => false,
    }
}

/// Extract a short display quant label from a longer string ("Q4_K_M" → "Q4",
/// "Q3_K_S" → "Q3"). Empty when there's nothing quant-shaped.
pub fn quant_label(quant: &str) -> String {
    let q = quant.to_uppercase();
    if let Some(pos) = q.find('Q') {
        // Q + following digits.
        let rest = &q[pos + 1..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return format!("Q{digits}");
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontier_is_always_full() {
        let s = Signals {
            frontier: true,
            params_b: Some(1.0), // even a tiny param count is overridden
            heavy_quant: true,
            byo: true,
        };
        assert_eq!(score(&s), Tier::Full);
    }

    #[test]
    fn param_buckets() {
        let mk = |p: f32| Signals {
            params_b: Some(p),
            ..Default::default()
        };
        assert_eq!(score(&mk(70.0)), Tier::Full);
        assert_eq!(score(&mk(30.0)), Tier::Full);
        assert_eq!(score(&mk(13.0)), Tier::Capable);
        assert_eq!(score(&mk(7.0)), Tier::Capable);
        assert_eq!(score(&mk(3.0)), Tier::Basic);
        assert_eq!(score(&mk(1.5)), Tier::Basic);
    }

    #[test]
    fn heavy_quant_demotes_one_tier() {
        let s = Signals {
            params_b: Some(13.0), // Capable …
            heavy_quant: true,    // … demoted to Basic
            ..Default::default()
        };
        assert_eq!(score(&s), Tier::Basic);

        let big = Signals {
            params_b: Some(70.0), // Full …
            heavy_quant: true,    // … demoted to Capable
            ..Default::default()
        };
        assert_eq!(score(&big), Tier::Capable);
    }

    #[test]
    fn heavy_quant_cannot_underflow_below_basic() {
        let s = Signals {
            params_b: Some(1.5), // Basic
            heavy_quant: true,   // stays Basic, not something invalid
            ..Default::default()
        };
        assert_eq!(score(&s), Tier::Basic);
    }

    #[test]
    fn byo_with_unknown_params_defaults_capable() {
        let s = Signals {
            byo: true,
            params_b: None,
            ..Default::default()
        };
        assert_eq!(score(&s), Tier::Capable);
    }

    #[test]
    fn unknown_local_defaults_basic() {
        // Not BYO, no param count → assume small.
        assert_eq!(score(&Signals::default()), Tier::Basic);
    }

    #[test]
    fn parse_params_from_ollama_size_string() {
        assert_eq!(parse_params_b("7B"), Some(7.0));
        assert_eq!(parse_params_b("70B"), Some(70.0));
        assert_eq!(parse_params_b("7.2B"), Some(7.2));
        assert_eq!(parse_params_b("1.5b"), Some(1.5));
    }

    #[test]
    fn parse_params_from_model_id_tag() {
        assert_eq!(parse_params_b("llama3:8b"), Some(8.0));
        assert_eq!(parse_params_b("qwen2.5-1.5b-instruct"), Some(1.5));
        assert_eq!(parse_params_b("mistral:7b-instruct-q4_0"), Some(7.0));
    }

    #[test]
    fn parse_params_handles_millions() {
        // SmolLM 135M → 0.135B.
        let p = parse_params_b("smollm-135m").unwrap();
        assert!((p - 0.135).abs() < 1e-4, "got {p}");
    }

    #[test]
    fn parse_params_rejects_bare_numbers() {
        // A version or date must NOT be read as a param count.
        assert_eq!(parse_params_b("qwen2.5-instruct"), None);
        assert_eq!(parse_params_b("model-2024"), None);
        assert_eq!(parse_params_b("gpt-4o"), None);
    }

    #[test]
    fn heavy_quant_detection() {
        assert!(quant_is_heavy("Q2_K"));
        assert!(quant_is_heavy("Q3_K_M"));
        assert!(quant_is_heavy("IQ3_XS"));
        assert!(!quant_is_heavy("Q4_K_M"));
        assert!(!quant_is_heavy("Q5_0"));
        assert!(!quant_is_heavy("Q8_0"));
        assert!(!quant_is_heavy("F16"));
        assert!(!quant_is_heavy(""));
    }

    #[test]
    fn quant_label_shortens() {
        assert_eq!(quant_label("Q4_K_M"), "Q4");
        assert_eq!(quant_label("Q3_K_S"), "Q3");
        assert_eq!(quant_label("q8_0"), "Q8");
        assert_eq!(quant_label("F16"), "");
        assert_eq!(quant_label(""), "");
    }

    #[test]
    fn tier_ordering_holds() {
        assert!(Tier::Basic < Tier::Capable);
        assert!(Tier::Capable < Tier::Full);
        assert!(Tier::Basic.is_low());
        assert!(!Tier::Capable.is_low());
        assert!(!Tier::Full.is_low());
    }

    #[test]
    fn frontier_model_names() {
        assert!(is_frontier_model("claude-sonnet-4-5"));
        assert!(is_frontier_model("gpt-4o"));
        assert!(is_frontier_model("gemini-2.0-pro"));
        assert!(is_frontier_model("grok-2"));
        // Cheaper tiers of frontier families are NOT frontier.
        assert!(!is_frontier_model("gpt-4o-mini"));
        assert!(!is_frontier_model("claude-haiku"));
        assert!(!is_frontier_model("gemini-1.5-flash"));
        assert!(!is_frontier_model("llama3:8b"));
        // Unknown ids are not frontier (fall through to byo default).
        assert!(!is_frontier_model("some-random-model"));
    }

    #[test]
    fn estimate_cloud_is_full() {
        let e = estimate("cloud", "", "", None, None);
        assert_eq!(e.tier, Tier::Full);
    }

    #[test]
    fn estimate_byo_frontier_is_full_small_is_capable() {
        assert_eq!(
            estimate("byo", "openai", "gpt-4o", None, None).tier,
            Tier::Full
        );
        // A small/mini BYO model isn't frontier, so it takes the byo default.
        assert_eq!(
            estimate("byo", "openai", "gpt-4o-mini", None, None).tier,
            Tier::Capable
        );
    }

    #[test]
    fn estimate_ollama_from_hints_and_labels() {
        // 3B Q4 → Basic, labelled for display.
        let e = estimate("ollama", "", "llama3.2:3b", Some(3.0), Some("Q4_K_M"));
        assert_eq!(e.tier, Tier::Basic);
        assert_eq!(e.params_label, "3B");
        assert_eq!(e.quant_label, "Q4");
        // 70B → Full.
        let big = estimate("ollama", "", "llama3.3:70b", Some(70.0), Some("Q4_K_M"));
        assert_eq!(big.tier, Tier::Full);
        assert_eq!(big.params_label, "70B");
    }

    #[test]
    fn estimate_local_builtin_is_basic() {
        // The built-in models (1.5–3B Q4) parse from the id and score Basic.
        let e = estimate("local", "", "qwen2.5-1.5b-instruct-q4", None, None);
        assert_eq!(e.tier, Tier::Basic);
        assert_eq!(e.params_label, "1.5B");
    }

    #[test]
    fn fmt_params_shapes() {
        assert_eq!(fmt_params(3.0), "3B");
        assert_eq!(fmt_params(1.5), "1.5B");
        assert_eq!(fmt_params(70.0), "70B");
        assert_eq!(fmt_params(0.135), "135M");
    }
}
