//! Privacy policy: redaction by default.
//!
//! Tare never persists request/response payload text — only provider-reported token COUNTS,
//! structural byte-length weights, and non-reversible content HASHES used for grouping
//! (retry-loop / bloated-system detection). The `PrivacyPolicy` selects how aggressively even
//! those derived signals are protected:
//!
//! - `strict_counts` (DEFAULT): counts + weights + UNSALTED content hashes. No payload text is
//!   ever stored. Deterministic across machines (hashes reproducible) — this is what every
//!   golden is pinned to.
//! - `fingerprint`: content hashes are SALTED with a machine-local secret, so an attacker who
//!   obtains the DB cannot brute-force a small known plaintext (e.g. a yes/no answer) back out
//!   of a hash. Grouping still works within one machine; hashes are no longer portable.
//! - `max_private`: NO content hashes at all (`None`). Retry-loop and bloated-system detection
//!   degrade honestly to "unavailable"; only run→step→usage→cost survive.
//!
//! The active policy is identified by `policy_id()` (a hash of its canonical form, salt
//! excluded) and stamped into every run so a report can state which profile produced it.

use crate::canon;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How aggressively derived signals (hashes, structural detail) are protected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Counts + weights + unsalted content hashes (deterministic). The safe default.
    #[default]
    StrictCounts,
    /// Salted content hashes — not brute-forceable, not portable across machines.
    Fingerprint,
    /// No content hashes; retry/bloat detection degrades to unavailable.
    MaxPrivate,
    /// IDENTICAL counts/hashes to `strict_counts` — the ledger is byte-for-byte the same — but ALSO
    /// opts into capturing redacted request/response bodies to the SEPARATE transcript store.
    /// The only profile that stores any text, and it's scrubbed + isolated + purgeable.
    MaxInspect,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::StrictCounts => "strict_counts",
            Profile::Fingerprint => "fingerprint",
            Profile::MaxPrivate => "max_private",
            Profile::MaxInspect => "max_inspect",
        }
    }
}

/// Resolved privacy policy. Salt is machine-local and NEVER serialized into the store or the
/// `policy_id` (so the id is portable while the salt stays secret).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivacyPolicy {
    pub profile: Profile,
    /// Machine-local secret for the `fingerprint` profile. Ignored by other profiles.
    salt: Option<String>,
    /// Opt out of recording observed per-step latency. `false` (default) records it;
    /// `true` forces `duration_ms` to 0 at the capture edge. Phrased as suppress-flag so the
    /// derived `Default` (false) keeps the privacy-friendly behavior of recording a local timing
    /// measurement — it carries no payload. Excluded from `policy_id` (keeps the id portable).
    suppress_latency: bool,
}

impl PrivacyPolicy {
    pub fn strict_counts() -> Self {
        PrivacyPolicy::default()
    }

    pub fn fingerprint(salt: impl Into<String>) -> Self {
        PrivacyPolicy {
            profile: Profile::Fingerprint,
            salt: Some(salt.into()),
            suppress_latency: false,
        }
    }

    pub fn max_private() -> Self {
        // Privacy-max records no per-step timing either (belt-and-suspenders: latency is a local
        // measurement, but max_private opts out of every derived signal).
        PrivacyPolicy {
            profile: Profile::MaxPrivate,
            salt: None,
            suppress_latency: true,
        }
    }

    /// `max_inspect`: byte-for-byte the same ledger as `strict_counts` (same counts + hashes +
    /// latency), plus opt-in redacted-transcript capture. Identical to `strict_counts` in EVERY
    /// derived-signal method below — the only difference is `captures_transcript()`.
    pub fn max_inspect() -> Self {
        PrivacyPolicy {
            profile: Profile::MaxInspect,
            salt: None,
            suppress_latency: false,
        }
    }

    /// Whether the capture edge should record observed per-step latency.
    pub fn should_record_latency(&self) -> bool {
        !self.suppress_latency
    }

    /// Whether the edge should capture redacted request/response bodies into the SEPARATE transcript
    /// store. True ONLY under `max_inspect`; every other profile stores no text at all.
    pub fn captures_transcript(&self) -> bool {
        matches!(self.profile, Profile::MaxInspect)
    }

    /// The effective profile (no downgrade logic — every profile means what it says).
    pub fn effective_profile(&self) -> Profile {
        self.profile
    }

    /// Hash a structural content fragment for grouping, honoring the profile:
    /// `None` under `max_private`; salted under `fingerprint` (when a salt is set); raw FNV
    /// otherwise. Never stores the content itself — only this derived u64.
    pub fn hash_content(&self, bytes: &[u8]) -> Option<u64> {
        match self.profile {
            Profile::MaxPrivate => None,
            Profile::Fingerprint => match &self.salt {
                Some(s) => Some(canon::keyed_fnv1a_64(s.as_bytes(), bytes)),
                None => Some(canon::fnv1a_64(bytes)),
            },
            // max_inspect hashes IDENTICALLY to strict_counts — the ledger is byte-for-byte the same.
            Profile::StrictCounts | Profile::MaxInspect => Some(canon::fnv1a_64(bytes)),
        }
    }

    /// Hash a canonical request `Value` for retry detection, honoring the profile.
    pub fn hash_request(&self, v: &Value) -> Option<u64> {
        match self.profile {
            Profile::MaxPrivate => None,
            Profile::Fingerprint => match &self.salt {
                Some(s) => Some(canon::request_hash_salted(s.as_bytes(), v)),
                None => Some(canon::request_hash(v)),
            },
            Profile::StrictCounts | Profile::MaxInspect => Some(canon::request_hash(v)),
        }
    }

    /// Stable, portable identifier for the policy (salt excluded), stamped into each run.
    pub fn policy_id(&self) -> String {
        let canonical = format!("profile={}", self.effective_profile().as_str());
        format!("{:016x}", canon::fnv1a_64(canonical.as_bytes()))
    }

    // ---- Configuration (precedence: explicit flag > env > ./tare.toml > default) ----

    /// Parse a `[privacy]` TOML table. Unknown keys are ignored (forward-compatible).
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Doc {
            privacy: Option<Table>,
        }
        #[derive(Deserialize)]
        struct Table {
            profile: Option<Profile>,
            salt: Option<String>,
            /// `[privacy] suppress_latency = true` opts out of per-step latency recording.
            suppress_latency: Option<bool>,
        }
        let doc: Doc = toml::from_str(s).map_err(|e| format!("privacy toml: {e}"))?;
        let mut p = PrivacyPolicy::default();
        if let Some(t) = doc.privacy {
            if let Some(prof) = t.profile {
                p.profile = prof;
            }
            p.salt = t.salt;
            if let Some(sl) = t.suppress_latency {
                p.suppress_latency = sl;
            }
        }
        Ok(p)
    }

    /// Resolve a policy with field-by-field precedence: an explicit `flag_profile` wins, else
    /// `TARE_PRIVACY_PROFILE`/`TARE_PRIVACY_SALT` env, else the parsed `./tare.toml` table,
    /// else the built-in `strict_counts` default.
    pub fn resolve(
        flag_profile: Option<Profile>,
        env_profile: Option<&str>,
        env_salt: Option<&str>,
        file_toml: Option<&str>,
    ) -> Result<Self, String> {
        // Start from file (lowest non-default precedence), then layer env, then flag.
        let mut p = match file_toml {
            Some(s) => PrivacyPolicy::from_toml_str(s)?,
            None => PrivacyPolicy::default(),
        };
        if let Some(s) = env_profile {
            p.profile = parse_profile(s)?;
        }
        if let Some(salt) = env_salt {
            p.salt = Some(salt.to_string());
        }
        if let Some(prof) = flag_profile {
            p.profile = prof;
        }
        Ok(p)
    }
}

fn parse_profile(s: &str) -> Result<Profile, String> {
    match s {
        "strict_counts" => Ok(Profile::StrictCounts),
        "fingerprint" => Ok(Profile::Fingerprint),
        "max_private" => Ok(Profile::MaxPrivate),
        "max_inspect" => Ok(Profile::MaxInspect),
        other => Err(format!("unknown privacy profile: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_strict_counts() {
        let p = PrivacyPolicy::default();
        assert_eq!(p.effective_profile(), Profile::StrictCounts);
        // Deterministic, unsalted: identical content -> identical hash.
        assert_eq!(p.hash_content(b"hello"), p.hash_content(b"hello"));
        assert!(p.hash_content(b"hello").is_some());
    }

    #[test]
    fn max_private_drops_all_hashes() {
        let p = PrivacyPolicy::max_private();
        assert_eq!(p.hash_content(b"anything"), None);
        assert_eq!(p.hash_request(&serde_json::json!({"a":1})), None);
    }

    #[test]
    fn max_inspect_matches_strict_counts_byte_for_byte_plus_transcript() {
        // the ledger under max_inspect is IDENTICAL to strict_counts — same content +
        // request hashes + latency — so every counts/attribution/shape golden is unaffected.
        let strict = PrivacyPolicy::strict_counts();
        let inspect = PrivacyPolicy::max_inspect();
        assert_eq!(
            inspect.hash_content(b"hello"),
            strict.hash_content(b"hello")
        );
        let v = serde_json::json!({"model":"m","messages":[{"role":"user"}]});
        assert_eq!(inspect.hash_request(&v), strict.hash_request(&v));
        assert_eq!(
            inspect.should_record_latency(),
            strict.should_record_latency()
        );
        // The ONLY behavioral difference: transcript capture is on for max_inspect, off for the rest.
        assert!(inspect.captures_transcript());
        assert!(!strict.captures_transcript());
        assert!(!PrivacyPolicy::max_private().captures_transcript());
        // Parse round-trip.
        assert_eq!(parse_profile("max_inspect").unwrap(), Profile::MaxInspect);
        assert_eq!(Profile::MaxInspect.as_str(), "max_inspect");
    }

    #[test]
    fn salted_hash_not_brute_forceable_demo() {
        // A 3-line brute-forcer recovers the plaintext under raw FNV but fails under salt.
        let space = ["yes", "no", "maybe"];
        let secret = "yes";
        // Raw: attacker recomputes fnv of each candidate and matches.
        let raw = PrivacyPolicy::strict_counts().hash_content(secret.as_bytes());
        let recovered = space
            .iter()
            .find(|c| PrivacyPolicy::strict_counts().hash_content(c.as_bytes()) == raw);
        assert_eq!(recovered, Some(&"yes"));
        // Salted: without the machine salt the attacker's unsalted recompute never matches.
        let fp = PrivacyPolicy::fingerprint("machine-secret-salt");
        let salted = fp.hash_content(secret.as_bytes());
        let recovered_salted = space
            .iter()
            .find(|c| PrivacyPolicy::strict_counts().hash_content(c.as_bytes()) == salted);
        assert_eq!(recovered_salted, None, "salt must defeat the brute-forcer");
    }

    #[test]
    fn precedence_merges_field_by_field() {
        let file = r#"
            [privacy]
            profile = "fingerprint"
            salt = "from-file"
        "#;
        // File sets fingerprint+salt; env overrides profile only; salt persists from file.
        let p = PrivacyPolicy::resolve(None, Some("max_private"), None, Some(file)).unwrap();
        assert_eq!(p.profile, Profile::MaxPrivate);
        assert_eq!(p.salt.as_deref(), Some("from-file"));
        // Explicit flag wins over env.
        let p2 =
            PrivacyPolicy::resolve(Some(Profile::StrictCounts), Some("max_private"), None, None)
                .unwrap();
        assert_eq!(p2.profile, Profile::StrictCounts);
    }

    #[test]
    fn latency_opt_out_via_toml_and_max_private() {
        // Default records latency; max_private and the [privacy] toml flag both opt out.
        assert!(PrivacyPolicy::default().should_record_latency());
        assert!(!PrivacyPolicy::max_private().should_record_latency());
        let off = PrivacyPolicy::from_toml_str("[privacy]\nsuppress_latency = true\n").unwrap();
        assert!(!off.should_record_latency());
        let on = PrivacyPolicy::from_toml_str("[privacy]\nprofile = \"strict_counts\"\n").unwrap();
        assert!(on.should_record_latency());
        // suppress_latency is excluded from policy_id (keeps the id portable).
        assert_eq!(off.policy_id(), PrivacyPolicy::default().policy_id());
    }

    #[test]
    fn policy_id_is_stable_and_salt_independent() {
        let a = PrivacyPolicy::fingerprint("salt-a");
        let b = PrivacyPolicy::fingerprint("salt-b");
        // Same profile, different salt -> same id (salt excluded from the id).
        assert_eq!(a.policy_id(), b.policy_id());
        assert_ne!(a.policy_id(), PrivacyPolicy::max_private().policy_id());
    }
}
