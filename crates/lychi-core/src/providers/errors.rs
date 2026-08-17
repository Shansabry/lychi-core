//! Turning raw provider errors into something a person can act on.
//!
//! Every AI provider reports failures as an HTTP status plus a JSON blob in its
//! own dialect. Surfacing that verbatim ("API returned 400 Bad Request:
//! {\"error\":{\"message\":\"messages[1].content must be a string\"…") tells the
//! user nothing about what THEY should do next.
//!
//! This module is the ONE place raw provider text becomes user-facing text, so
//! every path (streaming, non-streaming, routing) reports the same failure the
//! same way. It classifies by the signals providers actually agree on — the HTTP
//! status, plus a few unmistakable phrases — and never invents a diagnosis it
//! can't support: an unrecognized error keeps its original text rather than
//! being flattened into a useless "something went wrong".

use serde::{Deserialize, Serialize};

/// What went wrong, in terms of what the user can do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiErrorKind {
    /// The request carried images but the model can't see them.
    VisionUnsupported,
    /// The key is missing, wrong, or lacks access to this model.
    Auth,
    /// Rate limited or out of quota — retryable later.
    RateLimit,
    /// The request outgrew the provider tier's per-minute token budget (Groq
    /// free tier: "TPM: Limit 8000, Requested 10041"). Observed in the field:
    /// Groq's pre-check ESTIMATES tokens (~bytes/2.5, nearly 2x our real count)
    /// and an identical request can be accepted seconds later — so this IS
    /// worth a bounded retry. If retries exhaust, the fix is trimming the
    /// conversation, and the message says so.
    BudgetExceeded,
    /// The conversation (or an attachment) exceeded the model's context.
    TooLarge,
    /// The named model doesn't exist for this provider/key.
    UnknownModel,
    /// Couldn't reach the provider at all.
    Network,
    /// The provider is down or erroring on its side.
    ServerError,
    /// Anything we can't confidently classify.
    Unknown,
}

impl AiErrorKind {
    /// Whether retrying the same request could plausibly succeed.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimit | Self::BudgetExceeded | Self::Network | Self::ServerError
        )
    }
}

/// A classified failure: what happened, and what the user can do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiError {
    pub kind: AiErrorKind,
    /// One plain sentence, safe to show as-is.
    pub message: String,
    /// The raw provider text, kept for a "details" affordance — never the
    /// headline, but not thrown away either.
    pub detail: String,
}

/// Classify a raw provider error into something actionable.
///
/// `status` is the HTTP status when there was one (`None` for transport
/// failures). `raw` is whatever the provider said. `had_images` lets us
/// distinguish the single most confusing case — a vision request to a text-only
/// model — from an ordinary bad request.
pub fn classify(status: Option<u16>, raw: &str, had_images: bool) -> AiError {
    let lower = raw.to_lowercase();
    let kind = classify_kind(status, &lower, had_images);
    AiError {
        kind,
        message: message_for(kind, had_images),
        detail: raw.to_string(),
    }
}

fn classify_kind(status: Option<u16>, lower: &str, had_images: bool) -> AiErrorKind {
    // Transport: no status at all.
    let Some(status) = status else {
        return AiErrorKind::Network;
    };

    match status {
        401 | 403 => return AiErrorKind::Auth,
        429 => return AiErrorKind::RateLimit,
        500..=599 => return AiErrorKind::ServerError,
        _ => {}
    }

    // 400s need the body to disambiguate — every provider words these
    // differently, so match on the fragments they genuinely share.
    //
    // Checked BEFORE the context-limit fragments: Groq's free-tier
    // tokens-per-minute rejection (HTTP 413, "Request too large for model … on
    // tokens per minute (TPM): Limit …") talks about tokens and size, so the
    // context-limit match would claim it — but it is a RATE limit: the same
    // request succeeds a minute later, and telling the user their message is
    // too long sends them shortening text that was never the problem.
    if lower.contains("tokens per minute") || lower.contains("(tpm)") {
        // Two very different failures share this wording. When the request
        // needs FEWER tokens than the budget, the minute window is simply
        // spent — a true rate limit, retryable. When the request alone
        // OUTGROWS the budget, no amount of waiting helps and "try again"
        // is a lie. The provider states both numbers; believe them.
        if let Some((limit, requested)) = tpm_numbers(lower)
            && requested > limit
        {
            return AiErrorKind::BudgetExceeded;
        }
        return AiErrorKind::RateLimit;
    }
    if mentions_context_limit(lower) {
        return AiErrorKind::TooLarge;
    }
    if mentions_vision_rejection(lower, had_images) {
        return AiErrorKind::VisionUnsupported;
    }
    if lower.contains("model") && (lower.contains("not found") || lower.contains("does not exist"))
    {
        return AiErrorKind::UnknownModel;
    }
    if lower.contains("api key") || lower.contains("unauthorized") {
        return AiErrorKind::Auth;
    }
    AiErrorKind::Unknown
}

/// The `Limit N, Requested M` pair from a TPM rejection body, if both parse.
/// Scoped to the already-matched TPM arm — this never runs on other errors.
fn tpm_numbers(lower: &str) -> Option<(u64, u64)> {
    let num_after = |key: &str| -> Option<u64> {
        let idx = lower.find(key)? + key.len();
        let digits: String = lower[idx..]
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse().ok()
    };
    Some((num_after("limit")?, num_after("requested")?))
}

/// Context-window overflow, across dialects.
fn mentions_context_limit(lower: &str) -> bool {
    lower.contains("context length")
        || lower.contains("context_length")
        || lower.contains("too many tokens")
        || lower.contains("maximum context")
        || (lower.contains("token") && lower.contains("exceed"))
}

/// A text-only model rejecting image content blocks.
///
/// The tell is a complaint about the SHAPE of `content` on a request we know
/// carried images: a vision-capable endpoint accepts the block array, so
/// "content must be a string" can only mean this model wants plain text. We
/// require `had_images` so an unrelated malformed request isn't mislabeled.
fn mentions_vision_rejection(lower: &str, had_images: bool) -> bool {
    if !had_images {
        return false;
    }
    lower.contains("content must be a string")
        || lower.contains("must be a string")
        || lower.contains("invalid type for 'content'")
        || lower.contains("image")
            && (lower.contains("not supported")
                || lower.contains("unsupported")
                || lower.contains("does not support"))
}

fn message_for(kind: AiErrorKind, had_images: bool) -> String {
    match kind {
        AiErrorKind::VisionUnsupported => {
            "This model can't read images. Pick a vision-capable model in Settings, or remove the attachment."
                .to_string()
        }
        AiErrorKind::Auth => {
            "Your API key was rejected. Check it in Settings → AI.".to_string()
        }
        AiErrorKind::RateLimit => {
            "Rate limit reached. Wait a moment and try again.".to_string()
        }
        AiErrorKind::BudgetExceeded => {
            "This request is larger than your provider's per-minute token budget. Start a fresh chat or trim the input."
                .to_string()
        }
        AiErrorKind::TooLarge => {
            if had_images {
                "That's too much for this model's context — try a smaller attachment or a shorter conversation."
                    .to_string()
            } else {
                "That's too long for this model's context — try a shorter message or start a new chat."
                    .to_string()
            }
        }
        AiErrorKind::UnknownModel => {
            "That model isn't available for your key. Pick another in Settings → AI.".to_string()
        }
        AiErrorKind::Network => {
            "Couldn't reach the AI provider. Check your connection.".to_string()
        }
        AiErrorKind::ServerError => {
            "The AI provider is having trouble. Try again shortly.".to_string()
        }
        // Deliberately not a fabricated diagnosis — the caller shows the raw
        // detail alongside this, which is more useful than a vague guess.
        AiErrorKind::Unknown => "The AI request failed.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact Groq body that motivated this module.
    const GROQ_VISION_400: &str = r#"{"error":{"message":"messages[1].content must be a string","type":"invalid_request_error","param":"messages[1].content"}}"#;

    #[test]
    fn a_text_only_model_rejecting_images_is_named_precisely() {
        let e = classify(Some(400), GROQ_VISION_400, /* had_images */ true);
        assert_eq!(e.kind, AiErrorKind::VisionUnsupported);
        assert!(e.message.contains("can't read images"));
        // The raw text is preserved for a details view, not discarded.
        assert!(e.detail.contains("messages[1].content"));
    }

    #[test]
    fn the_same_body_without_images_is_not_blamed_on_vision() {
        // Without images the shape complaint means something else entirely —
        // guessing "vision" would send the user to change the wrong setting.
        let e = classify(Some(400), GROQ_VISION_400, /* had_images */ false);
        assert_ne!(e.kind, AiErrorKind::VisionUnsupported);
        assert_eq!(e.kind, AiErrorKind::Unknown);
    }

    #[test]
    fn auth_failures_point_at_settings() {
        for status in [401, 403] {
            let e = classify(Some(status), "{\"error\":\"forbidden\"}", false);
            assert_eq!(e.kind, AiErrorKind::Auth);
            assert!(e.message.contains("Settings"));
        }
    }

    #[test]
    fn rate_limit_is_retryable_and_server_errors_too() {
        assert!(classify(Some(429), "slow down", false).kind.is_retryable());
        assert!(
            classify(Some(503), "unavailable", false)
                .kind
                .is_retryable()
        );
        assert!(
            classify(None, "connection refused", false)
                .kind
                .is_retryable()
        );
        // A vision mismatch is NOT retryable — retrying changes nothing.
        assert!(
            !classify(Some(400), GROQ_VISION_400, true)
                .kind
                .is_retryable()
        );
    }

    #[test]
    fn context_overflow_is_recognized_across_dialects() {
        let bodies = [
            "maximum context length is 8192 tokens",
            "This model's context_length was exceeded",
            "prompt has too many tokens",
        ];
        for b in bodies {
            assert_eq!(
                classify(Some(400), b, false).kind,
                AiErrorKind::TooLarge,
                "{b}"
            );
        }
    }

    #[test]
    fn context_overflow_with_an_attachment_blames_the_attachment() {
        let e = classify(Some(400), "maximum context length exceeded", true);
        assert_eq!(e.kind, AiErrorKind::TooLarge);
        assert!(e.message.contains("attachment"));
    }

    #[test]
    fn an_unknown_model_is_distinguished_from_auth() {
        let e = classify(
            Some(404),
            r#"{"error":{"message":"The model `foo` does not exist"}}"#,
            false,
        );
        assert_eq!(e.kind, AiErrorKind::UnknownModel);
    }

    #[test]
    fn no_status_means_the_network_failed() {
        assert_eq!(
            classify(None, "error sending request: dns error", false).kind,
            AiErrorKind::Network
        );
    }

    #[test]
    fn an_unrecognized_error_keeps_its_raw_text_rather_than_being_flattened() {
        let e = classify(Some(400), "something entirely novel", false);
        assert_eq!(e.kind, AiErrorKind::Unknown);
        assert_eq!(e.detail, "something entirely novel");
    }

    #[test]
    fn an_explicit_image_unsupported_message_is_caught_too() {
        let e = classify(
            Some(400),
            r#"{"error":{"message":"This model does not support image input"}}"#,
            true,
        );
        assert_eq!(e.kind, AiErrorKind::VisionUnsupported);
    }

    #[test]
    fn tpm_request_over_budget_is_budget_exceeded_and_still_retryable() {
        // Groq's actual free-tier body: the request's ESTIMATE outgrows the
        // budget. Field evidence (2026-08-17 logs): the estimate is crude and
        // an identical request can pass seconds later — so this retries, but
        // keeps its own trim-your-input message for when retries exhaust.
        let raw = r#"{"error":{"message":"Request too large for model `openai/gpt-oss-120b` on tokens per minute (TPM): Limit 8000, Requested 10041, please reduce your message size and try again.","type":"tokens","code":"rate_limit_exceeded"}}"#;
        let err = classify(Some(413), raw, false);
        assert_eq!(err.kind, AiErrorKind::BudgetExceeded);
        assert!(err.kind.is_retryable());
        assert!(err.message.contains("Start a fresh chat"));
    }

    #[test]
    fn tpm_request_within_budget_is_a_true_rate_limit() {
        // Same wording, but the request FITS the budget — the minute is just
        // spent. Retrying after the window rolls succeeds.
        let raw = "Request too large for model on tokens per minute (TPM): Limit 8000, Requested 5000, please try again";
        let err = classify(Some(413), raw, false);
        assert_eq!(err.kind, AiErrorKind::RateLimit);
        assert!(err.kind.is_retryable());
    }

    #[test]
    fn tpm_without_parsable_numbers_stays_rate_limit() {
        // Unknown wording → the safe default (retryable) rather than a
        // confident "trim your input" we can't support.
        let err = classify(Some(413), "throttled on tokens per minute (TPM)", false);
        assert_eq!(err.kind, AiErrorKind::RateLimit);
    }
}
