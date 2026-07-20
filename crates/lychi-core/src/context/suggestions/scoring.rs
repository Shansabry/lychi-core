//! Pure suggestion scoring — no redb, no Tauri, no clock. All ranking math
//! lives here as testable functions so the pipeline in `mod.rs` stays flat.
//!
//! A candidate's final score blends four signals:
//! - `prior`: the provider's static relevance (0–100).
//! - `learned_boost`: positive frecency boost from past acceptances.
//! - acceptance CTR (`accepts`/`impressions`): DEMOTES chronically-ignored
//!   suggestions — the self-tuning signal no mainstream launcher has.
//! - `time_affinity`: circadian multiplier (1.0 neutral) from usage timestamps.

// ── Tuning constants ────────────────────────────────────────────────────

/// Beta-Bernoulli smoothing priors for the acceptance rate. A brand-new
/// suggestion (0 accepts / 0 impressions) reads as `α/(α+β) = 0.2` — neutral,
/// NOT penalized. It takes several ignored impressions to sink.
const ALPHA: f64 = 1.0;
const BETA: f64 = 4.0;

/// The acceptance rate of a brand-new (0/0) suggestion — `α/(α+β)`. The CTR
/// factor is centered here so an unproven suggestion ranks at ITS PRIOR (1.0×);
/// only proven acceptance boosts it and proven rejection demotes it.
const NEUTRAL_RATE: f64 = ALPHA / (ALPHA + BETA); // 0.20

/// Hard-suppress an item only once it has been shown this many times…
const SUPPRESS_MIN_IMPRESSIONS: u32 = 8;
/// …AND its smoothed acceptance rate is below this. Both must hold — so an
/// unproven suggestion is never suppressed for lack of data. Set just above
/// `smoothed_rate(0, 8)=0.077` so ~8 impressions with no accepts suppresses,
/// while a single accept (→0.15) rescues.
const SUPPRESS_RATE: f64 = 0.08;

/// CTR factor bounds: chronically-ignored → 0.5× (strong demote), neutral →
/// 1.0× (rank at prior), well-accepted → 2.0× (strong boost).
const CTR_FACTOR_MIN: f64 = 0.5;
const CTR_FACTOR_MAX: f64 = 2.0;
/// Slope of the factor around the neutral rate.
const CTR_FACTOR_SLOPE: f64 = 2.5;

/// What the ranker should do with a scored candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScoredOutcome {
    /// Keep it, with this final relevance (caller still applies the gate/caps).
    Rank(f64),
    /// Drop it entirely — shown enough and reliably ignored.
    Suppress,
}

/// Smoothed acceptance rate (Laplace / Beta-Bernoulli). Never divides by zero;
/// cold-start (0,0) returns the neutral prior `α/(α+β)`.
pub fn smoothed_rate(accepts: u32, impressions: u32) -> f64 {
    (accepts as f64 + ALPHA) / (impressions as f64 + ALPHA + BETA)
}

/// Map an acceptance rate ∈ [0,1] to a multiplicative factor ∈ [0.5, 2.0],
/// centered so the neutral (cold-start) rate maps to 1.0×. Monotonic: more
/// acceptance → higher factor.
pub fn ctr_factor(rate: f64) -> f64 {
    (1.0 + CTR_FACTOR_SLOPE * (rate - NEUTRAL_RATE)).clamp(CTR_FACTOR_MIN, CTR_FACTOR_MAX)
}

/// Whether an item has earned hard-suppression: shown ≥ SUPPRESS_MIN_IMPRESSIONS
/// times AND acceptance rate below SUPPRESS_RATE. Pure function of current
/// (decayed) counts — never a sticky flag, so recovery is automatic once the
/// counts change.
pub fn should_suppress(accepts: u32, impressions: u32) -> bool {
    impressions >= SUPPRESS_MIN_IMPRESSIONS && smoothed_rate(accepts, impressions) < SUPPRESS_RATE
}

/// The one scoring entry point. Combines prior + learned boost, modulated by
/// acceptance CTR and time affinity. Returns `Suppress` for reliably-ignored
/// items, else `Rank(final_score)`.
pub fn score_suggestion(
    prior: u16,
    learned_boost: f64,
    accepts: u32,
    impressions: u32,
    time_affinity: f64,
) -> ScoredOutcome {
    if should_suppress(accepts, impressions) {
        return ScoredOutcome::Suppress;
    }
    let base = prior as f64 + learned_boost;
    let ctr = ctr_factor(smoothed_rate(accepts, impressions));
    ScoredOutcome::Rank(base * ctr * time_affinity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(prior: u16, boost: f64, accepts: u32, imps: u32) -> ScoredOutcome {
        score_suggestion(prior, boost, accepts, imps, 1.0)
    }

    fn rank_val(o: ScoredOutcome) -> f64 {
        match o {
            ScoredOutcome::Rank(v) => v,
            ScoredOutcome::Suppress => panic!("expected Rank, got Suppress"),
        }
    }

    #[test]
    fn smoothed_rate_no_div_zero() {
        assert!((smoothed_rate(0, 0) - 0.20).abs() < 1e-9);
    }

    #[test]
    fn cold_start_is_neutral() {
        // A brand-new suggestion ranks at its PRIOR (factor 1.0) — not
        // penalized, not boosted, not suppressed.
        let o = score(100, 0.0, 0, 0);
        assert!(matches!(o, ScoredOutcome::Rank(_)));
        assert!((rank_val(o) - 100.0).abs() < 1e-6);
    }

    #[test]
    fn never_accepted_gets_suppressed() {
        assert_eq!(score(100, 0.0, 0, 8), ScoredOutcome::Suppress);
    }

    #[test]
    fn below_threshold_impressions_not_suppressed() {
        // 7 impressions is under the floor — downranked but still visible.
        assert!(matches!(score(100, 0.0, 0, 7), ScoredOutcome::Rank(_)));
    }

    #[test]
    fn single_accept_rescues() {
        // One acceptance lifts rate above SUPPRESS_RATE → no longer suppressed.
        assert!(matches!(score(100, 0.0, 1, 8), ScoredOutcome::Rank(_)));
    }

    #[test]
    fn high_ctr_boosts() {
        let boosted = rank_val(score(60, 0.0, 8, 10));
        let cold = rank_val(score(60, 0.0, 0, 0));
        assert!(boosted > cold, "high acceptance must rank higher");
    }

    #[test]
    fn ctr_factor_monotonic_bounded() {
        // Neutral rate → 1.0×; monotonic non-decreasing; bounded [0.5, 2.0].
        assert!((ctr_factor(0.20) - 1.0).abs() < 1e-9);
        let mut prev = ctr_factor(0.0);
        assert!((0.5..=2.0).contains(&prev));
        for i in 1..=10 {
            let f = ctr_factor(i as f64 / 10.0);
            assert!(f >= prev, "factor must be non-decreasing");
            assert!((0.5..=2.0).contains(&f));
            prev = f;
        }
        assert!((ctr_factor(1.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn ignored_high_prior_loses_to_accepted_low_prior() {
        // The headline behavior: a prior-100 item shown 10× and never accepted
        // must rank below a prior-80 item accepted 6 of 8 times.
        let ignored = score(100, 0.0, 0, 10);
        let accepted = rank_val(score(80, 0.0, 6, 8));
        match ignored {
            ScoredOutcome::Suppress => {} // suppressed is "loses" a fortiori
            ScoredOutcome::Rank(v) => assert!(v < accepted, "ignored high-prior must lose"),
        }
    }

    #[test]
    fn time_affinity_multiplies() {
        let neutral = score_suggestion(100, 0.0, 0, 0, 1.0);
        let peak = score_suggestion(100, 0.0, 0, 0, 1.15);
        assert!(rank_val(peak) > rank_val(neutral));
    }
}
