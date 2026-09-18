import { describe, it, expect } from "vitest";
import { renderPricing } from "../src/screens/pricing.js";
import { fakeClient } from "./fakeClient.js";
import type { Report } from "../src/client.js";

describe("Pricing/Models screen", () => {
  it("renders a sortable model catalog with rates + provenance", async () => {
    const client = fakeClient({
      pricing: async () => ({
        version: "2026.06.01",
        effective_date: "2026-06-01",
        note: null,
        models: [
          { provider: "anthropic", model_id: "claude-opus-4-8", tier: "standard", input_micro_per_mtok: 5_000_000, output_micro_per_mtok: 25_000_000, cache_read_micro_per_mtok: 500_000, cache_write_5m_micro_per_mtok: 6_250_000, cache_write_1h_micro_per_mtok: 10_000_000 },
          { provider: "anthropic", model_id: "claude-haiku-4-5", tier: "standard", input_micro_per_mtok: 1_000_000, output_micro_per_mtok: 5_000_000, cache_read_micro_per_mtok: 100_000, cache_write_5m_micro_per_mtok: 1_250_000, cache_write_1h_micro_per_mtok: 2_000_000 },
        ],
      }),
    });
    const root = document.createElement("div");
    await renderPricing(root, client);
    expect(root.textContent).toContain("Model pricing"); // canonical screen name
    expect(root.querySelector(".lens")?.textContent).toContain("each model costs");
    expect(root.textContent).toContain("Pricing table 2026.06.01");
    // Inline legend decodes the encoded rate columns: cache read/write + 5m/1h lifetimes.
    expect(root.textContent).toContain("cache lifetime");
    expect(root.textContent).toContain("5m (5-minute)");
    const rows = root.querySelectorAll(".datatable tbody tr");
    expect(rows.length).toBe(2);
    expect(root.textContent).toContain("claude-opus-4-8");
    // Opus input rate $5.00 / 1M tok.
    expect(Array.from(root.querySelectorAll(".dollars")).some((d) => d.textContent === "$5.00")).toBe(true);
  });

  it("lists unpriced models seen in the store", async () => {
    const client = fakeClient({
      pricing: async () => ({ version: "x", effective_date: "2026-06-01", note: null, models: [] }),
      report: async () =>
        ({
          pricing_version: "x",
          effective_date: "2026-06-01",
          estimated: true,
          total_micros: 0,
          rows: [],
          unpriced: [{ provider: "openai", model: "gpt-future", token_total: 4242 }],
        }) as unknown as Report,
    });
    const root = document.createElement("div");
    await renderPricing(root, client);
    expect(root.textContent).toContain("Unpriced models seen in your store");
    expect(root.textContent).toContain("openai/gpt-future");
    expect(root.textContent).toContain("4,242");
  });
});
