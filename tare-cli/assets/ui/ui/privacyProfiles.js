// Canonical privacy-profile metadata. Privacy is a high-stakes choice, so every option
// shows a humanized label AND a one-line, point-of-use description of what it RETAINS vs DROPS — never
// a bare enum token. Mirrors the Rust `Profile` enum; descriptions track tare-core/src/privacy.rs.
export const PRIVACY_PROFILES = [
    {
        value: "strict_counts",
        label: "Strict counts",
        description: "Token counts, structural weights, and unsalted content hashes for grouping. No prompt or response text is stored. The default.",
    },
    {
        value: "fingerprint",
        label: "Fingerprint",
        description: "Salts the content hashes with a machine-local secret so a stolen DB can't be brute-forced; still no raw text, and hashes aren't portable across machines.",
    },
    {
        value: "max_private",
        label: "Max private",
        description: "Token counts only. No content hashes or latency, so retry-loop and bloated-prompt detection degrade to unavailable.",
    },
    {
        value: "max_inspect",
        label: "Max inspect",
        description: "Same counts, hashes, and timing as Strict counts, plus opt-in redacted request/response bodies in a separate, purgeable store.",
    },
];
/// Metadata for one profile value, or `undefined` for an unknown/forward-compat token.
export function privacyProfileInfo(value) {
    return PRIVACY_PROFILES.find((p) => p.value === value);
}
/// One-line description for a profile value; "" when unknown (forward-compat) so callers render nothing.
export function privacyProfileDescription(value) {
    return privacyProfileInfo(value)?.description ?? "";
}
