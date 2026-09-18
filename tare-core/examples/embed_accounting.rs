//! Minimal embedder: cost a provider-reported usage vector with the `tare-core` facade —
//! no proxy, no CLI, no raw payload. Run with `cargo run -p tare-core --example embed_accounting`.

use tare_core::{account_from_usage, PricingTable, Provider, UsageTokens};

fn main() {
    // Ship-with pricing table; an embedder would supply its own.
    let pricing = PricingTable::from_json_str(include_str!("../../pricing/pricing.json"))
        .expect("bundled pricing parses");

    // A usage block exactly as a provider reports it (counts only — no re-tokenization).
    let usage = UsageTokens {
        fresh_input: 1_000,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 0,
        output: 500,
        reasoning: 0,
        audio_input: 0,
        audio_output: 0,
    };

    match account_from_usage(Provider::Anthropic, "claude-opus-4-8", &usage, &pricing) {
        Some(cost) => println!("estimated total: {}", cost.total.to_dollar_string()),
        None => println!("model is unpriced (no fabricated $0)"),
    }
}
