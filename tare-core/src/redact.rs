//! Secret/PII redaction for opt-in transcript capture. Before any request/response body
//! is stored (only under the `max_inspect` profile, in a SEPARATE store — never the counts DB), it
//! passes through here: known secret shapes (API keys, tokens, Bearer headers, emails) are masked
//! with a typed placeholder, and the result is truncated to a hard byte cap.
//!
//! HONEST CEILING: this is a best-effort scrubber over well-known patterns, NOT a guarantee that no
//! secret survives — a novel key format or a secret embedded in prose can slip through. It's a
//! defense-in-depth floor for a feature that is itself opt-in and locally-stored; treat the redacted
//! text as "scrubbed of the obvious", not "safe to publish". False positives (over-masking) are
//! deliberately preferred over false negatives (leaks).

use regex::Regex;
use std::sync::OnceLock;

/// Outcome of scrubbing a body: the redacted text, how many secrets were masked, and whether the
/// text was truncated at the byte cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrubResult {
    pub redacted: String,
    pub masked: usize,
    pub truncated: bool,
}

/// Compiled (pattern, placeholder) rules, most-specific first so a typed match wins over the generic
/// high-entropy catch-all. Built once. Ordering matters: earlier rules mask before later ones see
/// the text, so e.g. a GitHub token is tagged `key` before the generic rule could grab it.
fn rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        let r = |p: &str| Regex::new(p).expect("redaction pattern compiles");
        vec![
            // PEM private-key blocks FIRST (multi-line): mask the whole armored block as one unit, so
            // no short trailing base64 line slips under the generic 40-char catch-all (harden).
            (
                r(r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----"),
                "[REDACTED:key]",
            ),
            // JWTs (`eyJ…`.`…`.`…`) — the `eyJ` prefix is base64url of `{"`, so this shape is
            // near-unambiguous. The dot separators mean the generic 40-char rule can leak short
            // segments (header/signature), so mask the whole token as one unit (harden).
            (
                r(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
                "[REDACTED:token]",
            ),
            // Authorization: Bearer <token> / Basic <token> (case-insensitive scheme).
            (
                r(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}"),
                "[REDACTED:auth]",
            ),
            // OpenAI / Anthropic style: sk-, sk-ant-, sk-proj-…
            (r(r"\bsk-[A-Za-z0-9_-]{16,}"), "[REDACTED:key]"),
            // Stripe-style UNDERSCORE secret keys (sk_live_/sk_test_/rk_…): the hyphen `sk-` rule
            // misses them, and they're often < 40 chars so the generic catch-all misses them too — a
            // real leak gap (harden). `pk_` (publishable) is masked too: over-masking is safe.
            (r(r"\b[srp]k_(live|test)_[0-9A-Za-z]{8,}"), "[REDACTED:key]"),
            // GitHub PATs + tokens.
            (r(r"\bgh[pousr]_[A-Za-z0-9]{20,}"), "[REDACTED:key]"),
            (r(r"\bgithub_pat_[A-Za-z0-9_]{22,}"), "[REDACTED:key]"),
            // AWS access key ids.
            (r(r"\b(AKIA|ASIA)[0-9A-Z]{16}\b"), "[REDACTED:key]"),
            // Google API keys.
            (r(r"\bAIza[0-9A-Za-z_-]{35}"), "[REDACTED:key]"),
            // Slack tokens.
            (r(r"\bxox[baprs]-[A-Za-z0-9-]{10,}"), "[REDACTED:key]"),
            // GitLab PATs + HuggingFace tokens.
            (r(r"\bglpat-[A-Za-z0-9_-]{20,}"), "[REDACTED:key]"),
            (r(r"\bhf_[A-Za-z0-9]{30,}"), "[REDACTED:key]"),
            // Emails.
            (
                r(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"),
                "[REDACTED:email]",
            ),
            // Generic high-entropy token (JWTs, opaque keys): a long run of key-ish chars. Last, so
            // typed rules win; aggressive on purpose (a leaked secret is worse than a masked hash).
            (r(r"\b[A-Za-z0-9+/_-]{40,}={0,2}\b"), "[REDACTED:token]"),
        ]
    })
}

/// Truncate `s` to at most `max_bytes`, respecting UTF-8 char boundaries. Returns (truncated_string,
/// was_truncated).
fn truncate_bytes(s: &str, max_bytes: usize) -> (String, bool) {
    if s.len() <= max_bytes {
        return (s.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// Scrub a body of known secrets, then truncate to `max_bytes`. Deterministic; pure.
pub fn scrub_body(text: &str, max_bytes: usize) -> ScrubResult {
    let mut redacted = text.to_string();
    let mut masked = 0usize;
    for (re, placeholder) in rules() {
        // Count matches at THIS stage (earlier replacements may have removed some), then replace.
        let n = re.find_iter(&redacted).count();
        if n > 0 {
            masked += n;
            redacted = re.replace_all(&redacted, *placeholder).into_owned();
        }
    }
    let (redacted, truncated) = truncate_bytes(&redacted, max_bytes);
    ScrubResult {
        redacted,
        masked,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrub(s: &str) -> String {
        scrub_body(s, 1_000_000).redacted
    }

    #[test]
    fn masks_common_secret_shapes() {
        assert!(scrub("key sk-ant-api03-abcDEF1234567890xyz here").contains("[REDACTED:key]"));
        assert!(!scrub("sk-ant-api03-abcDEF1234567890xyz").contains("sk-ant"));
        assert!(scrub("Authorization: Bearer eyJhbGci0123456789abcdef").contains("[REDACTED:auth]"));
        assert!(scrub("token ghp_0123456789abcdef0123456789abcdefABCD").contains("[REDACTED:key]"));
        assert!(scrub("aws AKIAIOSFODNN7EXAMPLE end").contains("[REDACTED:key]"));
        assert!(scrub("mail me at alice@example.com please").contains("[REDACTED:email]"));
        assert!(!scrub("alice@example.com").contains("@example.com"));
    }

    #[test]
    fn masks_stripe_underscore_keys_and_pem_blocks_the_generic_rule_would_miss() {
        // Stripe-style underscore secret key — < 40 chars, so the generic catch-all misses it, and the
        // `sk-` (hyphen) rule doesn't apply. Must still be fully masked (harden).
        let stripe = format!("use {} now", ["sk", "live", "TESTKEY12345678"].join("_"));
        assert!(scrub(stripe).contains("[REDACTED:key]"));
        assert!(!scrub(stripe).contains("4eC39HqLyjWDarjtT1zdp7dc"));
        assert!(scrub("rk_test_51HabcdEFGH0123").contains("[REDACTED:key]"));
        // PEM private-key block: the whole armored block is masked as one unit — no trailing base64
        // fragment (the last line is short) survives.
        let pem =
            "-----BEGIN PRIVATE KEY-----\nMIIBVQIBADANBgkq\naGVsbG8=\n-----END PRIVATE KEY-----";
        let out = scrub(pem);
        assert!(out.contains("[REDACTED:key]"));
        assert!(!out.contains("MIIBVQIBADANBgkq"));
        assert!(!out.contains("aGVsbG8=")); // the short trailing base64 line is gone too
        assert!(!out.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn masks_a_bare_jwt_as_one_unit() {
        // A bare JWT (not after "Bearer") — the dot-separated segments would otherwise let short parts
        // slip past the generic rule. Whole token masked; no segment fragment survives (harden).
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N";
        let out = scrub(&format!("session={jwt} ok"));
        assert!(out.contains("[REDACTED:token]"));
        assert!(!out.contains("eyJhbGci"));
        assert!(!out.contains("dozjgNryP4J3")); // signature segment gone too
    }

    #[test]
    fn keeps_ordinary_prose_and_short_tokens() {
        // Short/ordinary words aren't masked (the generic rule needs 40+ chars).
        let s = "The quick brown fox costs 1234 tokens and $0.05.";
        assert_eq!(scrub(s), s);
    }

    #[test]
    fn counts_masks_and_truncates_at_a_char_boundary() {
        let r = scrub_body("a@b.com and sk-ABCDEFGHIJKLMNOP123", 1_000_000);
        assert_eq!(r.masked, 2); // one email + one key
        assert!(!r.truncated);
        // Truncation respects the byte cap (multibyte-safe: never splits a char).
        let long = "é".repeat(50); // 100 bytes
        let t = scrub_body(&long, 51);
        assert!(t.truncated);
        assert!(t.redacted.len() <= 51);
        assert!(t.redacted.chars().all(|c| c == 'é')); // no broken char
    }

    #[test]
    fn generic_high_entropy_token_is_masked() {
        let jwt = "header.payloadABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdef.sig";
        assert!(scrub(jwt).contains("[REDACTED:token]"));
    }
}
