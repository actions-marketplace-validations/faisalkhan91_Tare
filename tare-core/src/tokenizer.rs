//! Per-model tokenizer generation. Anthropic re-tokenized starting with Opus 4.7:
//! the Opus 4.7+/4.8, Sonnet 5, and Fable/Mythos 5 families run ~30% MORE tokens than the baseline
//! (Opus 4.6 and earlier, Sonnet 4.6 and earlier, Haiku) on the same text. This does NOT affect the
//! ledger — cost is recomputed from `message.usage`'s real counts, so rates are unaffected — but it
//! DOES bite token ESTIMATORS (`tare advise` / pre-flight budgets / cross-model repricing): an
//! estimate that holds captured counts fixed while pricing a NEWER-generation model under-counts by
//! ~30%. This module is the per-model note that lets estimators apply the inflation precisely.

/// The ~30% inflation of the Opus 4.7+ tokenizer generation over the baseline, in percent. Shared by
/// the estimator's high-bound widening (see `estimate`).
pub const INFLATED_GENERATION_PCT: u64 = 30;

/// Whether `model` is on the inflated (Opus 4.7+) tokenizer generation.
/// - `Some(true)`  — inflated generation (Opus 4.7 / 4.8, Sonnet 5, Fable 5, Mythos 5).
/// - `Some(false)` — a confidently-known baseline generation (Opus 4.6 / 4.5 / 4.1, Sonnet 4.6 / 4.5,
///   Haiku 4.5) that predates the re-tokenization.
/// - `None` — unknown (older Claude naming, non-Anthropic tokenizer families, or unrecognized). The
///   estimator treats `None` conservatively (keeps its blanket cross-model inflation) so ignorance
///   never NARROWS the honest band.
///
/// Matches on substrings so dated suffixes (`claude-opus-4-8-20260601`) and vendor prefixes classify
/// the same. Only ever returns `Some(false)` for models we're confident predate the inflation — a
/// wrong `false` would under-count, which the estimate-honesty rule forbids.
pub fn inflated_generation(model: &str) -> Option<bool> {
    let m = model.to_ascii_lowercase();
    // Inflated generation first (a positive match wins over any baseline substring).
    const INFLATED: [&str; 6] = [
        "opus-4-7", "opus-4-8", "opus-4-9", "sonnet-5", "fable-5", "mythos-5",
    ];
    if INFLATED.iter().any(|p| m.contains(p)) {
        return Some(true);
    }
    // Confidently-known baseline generation (predates the Opus 4.7 re-tokenization).
    const BASELINE: [&str; 8] = [
        "opus-4-6",
        "opus-4-5",
        "opus-4-1",
        "opus-4-0",
        "sonnet-4-6",
        "sonnet-4-5",
        "sonnet-4-0",
        "haiku-4-5",
    ];
    if BASELINE.iter().any(|p| m.contains(p)) {
        return Some(false);
    }
    None
}

/// Should a cross-model estimate inflate the high bound for tokenizer-generation drift?
/// Precise when both generations are known: inflate ONLY on a baseline→inflated swap (the genuine
/// under-count direction); never inflate for same-or-lower generation. When either side is unknown,
/// stay conservative and inflate (never narrow the band out of ignorance).
pub fn crosses_into_inflated(source_model: &str, target_model: &str) -> bool {
    match (
        inflated_generation(source_model),
        inflated_generation(target_model),
    ) {
        (Some(false), Some(true)) => true, // baseline → inflated: real ~30% under-count
        (Some(_), Some(_)) => false,       // known same-or-lower generation: no inflation
        _ => true,                         // unknown either side: conservative blanket inflation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_inflated_and_baseline_generations() {
        // Inflated (Opus 4.7+ / Sonnet 5 / Fable 5), incl. dated suffixes + vendor prefixes.
        assert_eq!(inflated_generation("claude-opus-4-8"), Some(true));
        assert_eq!(inflated_generation("claude-opus-4-7-20260101"), Some(true));
        assert_eq!(inflated_generation("claude-fable-5"), Some(true));
        assert_eq!(inflated_generation("claude-sonnet-5"), Some(true));
        // Baseline (predates the re-tokenization).
        assert_eq!(inflated_generation("claude-opus-4-6"), Some(false));
        assert_eq!(inflated_generation("claude-opus-4-1-20250805"), Some(false));
        assert_eq!(inflated_generation("claude-sonnet-4-5"), Some(false));
        assert_eq!(inflated_generation("claude-haiku-4-5"), Some(false));
        // Unknown: old naming / non-Anthropic families.
        assert_eq!(inflated_generation("claude-3-5-sonnet"), None);
        assert_eq!(inflated_generation("gpt-5"), None);
        assert_eq!(inflated_generation("gemini-2.5-pro"), None);
    }

    #[test]
    fn cross_inflation_is_precise_when_known_conservative_when_not() {
        // Baseline → inflated: the genuine under-count direction → inflate.
        assert!(crosses_into_inflated("claude-opus-4-6", "claude-opus-4-8"));
        // Inflated → baseline (fewer tokens) and same-generation: do NOT over-inflate.
        assert!(!crosses_into_inflated("claude-opus-4-8", "claude-opus-4-6"));
        assert!(!crosses_into_inflated("claude-opus-4-8", "claude-opus-4-7"));
        assert!(!crosses_into_inflated(
            "claude-opus-4-6",
            "claude-sonnet-4-5"
        ));
        // Unknown either side → stay conservative (inflate).
        assert!(crosses_into_inflated("big", "small"));
        assert!(crosses_into_inflated("claude-opus-4-8", "gpt-5"));
    }
}
