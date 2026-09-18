// Small, dependency-free metric derivations shared by comparison views.

/// Blended efficiency: micro-USD per 1M tokens (0 when a bucket has no tokens).
export function microsPerMtok(micros: number, tokens: number): number {
  return tokens > 0 ? Math.round((micros / tokens) * 1_000_000) : 0;
}
