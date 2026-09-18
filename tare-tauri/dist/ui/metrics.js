// Small, dependency-free metric derivations shared by comparison views.
/// Blended efficiency: micro-USD per 1M tokens (0 when a bucket has no tokens).
export function microsPerMtok(micros, tokens) {
    return tokens > 0 ? Math.round((micros / tokens) * 1000000) : 0;
}
