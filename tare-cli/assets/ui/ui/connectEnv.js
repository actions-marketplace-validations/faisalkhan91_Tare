// The per-provider environment a user sets so their agent's SDK talks to the Tare proxy instead
// of the provider directly. Mirrors tare-cli's base-URL overrides. Presets expose only the rows
// relevant to the selected provider; Tare never asks users to apply an unrelated wall of variables.
export const PROVIDER_PRESETS = [
    {
        id: "anthropic",
        label: "Anthropic (Claude / Claude Code)",
        hint: "Claude Code exports usage to the OTel channel. Use the recommended path above. To route requests through Tare instead, swap ANTHROPIC_BASE_URL.",
        capture: "otel",
    },
    {
        id: "openai",
        label: "OpenAI",
        hint: "Point the OpenAI SDK at Tare by setting OPENAI_BASE_URL to the proxy channel.",
        capture: "proxy",
    },
    {
        id: "azure",
        label: "Azure OpenAI",
        hint: "Set AZURE_OPENAI_ENDPOINT to the proxy channel.",
        capture: "proxy",
    },
    {
        id: "bedrock",
        label: "AWS Bedrock",
        hint: "The AWS SDK has no portable base-URL override. Use the recommended OTel path above, or set TARE_BEDROCK_UPSTREAM on the server.",
        capture: "otel",
    },
    {
        id: "gemini",
        label: "Google Gemini",
        hint: "Set GEMINI_BASE_URL to the proxy channel.",
        capture: "proxy",
    },
    {
        id: "openrouter",
        label: "OpenRouter",
        hint: "OpenRouter is OpenAI-compatible. Set OPENAI_BASE_URL to the proxy channel.",
        capture: "proxy",
    },
    {
        id: "local",
        label: "Local (vLLM / Ollama / LM Studio)",
        hint: "OpenAI-compatible servers use OPENAI_BASE_URL pointed at the proxy channel.",
        capture: "proxy",
    },
    {
        id: "other",
        label: "OpenAI-compatible (OpenRouter, Groq, Together, …)",
        hint: "Any OpenAI-compatible endpoint can use OPENAI_BASE_URL pointed at the proxy channel. Send the request headers x-tare-provider: openai_compatible and x-tare-vendor: <your vendor> so the proxy attributes it explicitly instead of guessing from the path; add a pricing.json row keyed by that vendor + model for dollars, otherwise models stay usage-only.",
        capture: "proxy",
    },
];
/// Look up a preset by id, falling back to Anthropic for an unknown/legacy stored value.
export function providerPreset(id) {
    return PROVIDER_PRESETS.find((p) => p.id === id) ?? PROVIDER_PRESETS[0];
}
/// The ONE capture mode to lead with for a provider, and a one-line why. Tare knows the
/// right answer per agent — decide it instead of presenting both modes with equal weight. The
/// mapping lives on each preset's `capture`; both modes stay available (the other is "Advanced").
export function captureRecommendation(id) {
    const preset = providerPreset(id);
    return preset.capture === "otel"
        ? {
            mode: "otel",
            label: "the OTel channel",
            why: "Live, out-of-band capture. It never sits in the request path and does not replace your existing base URL or corporate proxy.",
        }
        : {
            mode: "proxy",
            label: "the proxy channel",
            why: "A one-line base URL swap routes this client through Tare for full prompt-component attribution (system / tools / history).",
        };
}
/// The base-URL env rows for one provider. Base URLs only, never keys.
/// Bedrock returns no rows — it has no portable SDK base-URL override (OTel path is recommended).
export function connectEnvFor(id, baseUrl) {
    const b = baseUrl.replace(/\/+$/, "");
    const v1 = `${b}/v1`;
    switch (id) {
        case "anthropic":
            return [["ANTHROPIC_BASE_URL", b]];
        case "azure":
            return [["AZURE_OPENAI_ENDPOINT", b]];
        case "gemini":
            return [
                ["GEMINI_BASE_URL", b],
                ["GOOGLE_GEMINI_BASE_URL", b],
            ];
        case "bedrock":
            return [];
        case "openai":
        case "openrouter":
        case "local":
        case "other":
        default:
            return [
                ["OPENAI_BASE_URL", v1],
                ["OPENAI_API_BASE", v1],
            ];
    }
}
/// Map a picker provider id to the pricing-table provider key (which uses wire names), or null for
/// usage-only / OpenAI-compatible providers that carry no distinct bundled price rows.
export function pricingProviderKey(id) {
    switch (id) {
        case "anthropic":
            return "anthropic";
        case "openai":
            return "openai";
        case "azure":
            return "azure_openai";
        case "bedrock":
            return "bedrock_converse";
        case "gemini":
            return "gemini";
        case "local":
            return "local";
        case "openrouter":
        case "other":
        default:
            return null;
    }
}
/// A one-line pricing-freshness + coverage signal for a provider at setup: the bundled
/// table version/date, how many of this provider's models are priced, and the honest "others show
/// usage-only" affordance. `modelsByProvider` is the pricing endpoint's per-provider count map.
export function pricingFreshnessLine(version, effectiveDate, id, modelsByProvider) {
    const base = `Pricing table ${version} (effective ${effectiveDate})`;
    const key = pricingProviderKey(id);
    const n = key ? modelsByProvider?.[key] : undefined;
    if (n && n > 0) {
        return `${base} · ${n} model${n === 1 ? "" : "s"} priced for this provider. Models not listed show usage-only (no dollars). Add a row in pricing.json.`;
    }
    return `${base} · This provider is usage-only by default. Models show usage-only (no dollars) unless you add a row in pricing.json.`;
}
/// Copy-pasteable shell block for a single provider's base-URL rows.
export function connectEnvShellFor(id, baseUrl) {
    return connectEnvFor(id, baseUrl)
        .map(([k, v]) => `export ${k}=${v}`)
        .join("\n");
}
/// OTel channel capture: additive env that makes Claude Code (and Codex/SDKs) export token usage to
/// Tare's local OTLP receiver. These keys are orthogonal to `ANTHROPIC_BASE_URL`, so capture works
/// alongside whatever proxy/gateway the user already has and never sits in the model request path.
/// `receiverUrl` is the receiver's HTTP/JSON endpoint (default :4318).
export function otelEnv(receiverUrl) {
    const u = receiverUrl.replace(/\/+$/, "");
    return [
        ["CLAUDE_CODE_ENABLE_TELEMETRY", "1"],
        ["OTEL_METRICS_EXPORTER", "otlp"],
        ["OTEL_LOGS_EXPORTER", "otlp"],
        ["OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"],
        ["OTEL_EXPORTER_OTLP_ENDPOINT", u],
    ];
}
/// Copy-pasteable shell block for the out-of-band OTel capture env.
export function otelEnvShell(receiverUrl) {
    return otelEnv(receiverUrl)
        .map(([k, v]) => `export ${k}=${v}`)
        .join("\n");
}
/// Generic OTel-GenAI env for any agent that speaks OTLP (not just Claude Code). Drops the
/// Claude-Code-specific `CLAUDE_CODE_ENABLE_TELEMETRY` flag; the receiver ingests generic GenAI
/// semconv spans (otel.rs).
export function genericOtelEnvShell(receiverUrl) {
    const u = receiverUrl.replace(/\/+$/, "");
    return [
        ["OTEL_METRICS_EXPORTER", "otlp"],
        ["OTEL_LOGS_EXPORTER", "otlp"],
        ["OTEL_TRACES_EXPORTER", "otlp"],
        ["OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"],
        ["OTEL_EXPORTER_OTLP_ENDPOINT", u],
    ]
        .map(([k, v]) => `export ${k}=${v}`)
        .join("\n");
}
export const AGENT_RECIPES = [
    {
        id: "claude-code",
        label: "Claude Code",
        mode: "otel",
        note: "OTel channel capture. Or run `tare connect` to wire ~/.claude/settings.json for you.",
        render: (otlp) => otelEnvShell(otlp),
    },
    {
        id: "codex",
        label: "Codex CLI",
        mode: "otel",
        note: "Run `tare codex-connect` to wire the [otel] exporter in $CODEX_HOME/config.toml (reversible).",
        render: (otlp) => genericOtelEnvShell(otlp),
    },
    {
        id: "gemini-cli",
        label: "Gemini CLI",
        mode: "otel",
        note: "Gemini CLI exports GenAI-semconv OTel; point it at the OTel channel (the Gemini provider is already priced).",
        render: (otlp) => genericOtelEnvShell(otlp),
    },
    {
        id: "aider",
        label: "Aider",
        mode: "proxy",
        note: "Route Aider's OpenAI-compatible client through the proxy channel (env or --openai-api-base).",
        render: (_otlp, proxy) => `export OPENAI_API_BASE=${proxy.replace(/\/+$/, "")}/v1`,
    },
    {
        id: "continue",
        label: "Continue (VS Code / JetBrains)",
        mode: "proxy",
        note: "In ~/.continue/config.json, set the model's `apiBase` to the proxy channel.",
        render: (_otlp, proxy) => `"apiBase": "${proxy.replace(/\/+$/, "")}/v1"`,
    },
    {
        id: "cline",
        label: "Cline",
        mode: "proxy",
        note: "Cline settings → OpenAI-Compatible → Base URL = the proxy channel.",
        render: (_otlp, proxy) => `${proxy.replace(/\/+$/, "")}/v1`,
    },
    {
        id: "cursor",
        label: "Cursor",
        mode: "proxy",
        note: "Cursor → Settings → Models → Override OpenAI Base URL = the proxy channel.",
        render: (_otlp, proxy) => `${proxy.replace(/\/+$/, "")}/v1`,
    },
];
export function agentRecipe(id) {
    return AGENT_RECIPES.find((a) => a.id === id) ?? AGENT_RECIPES[0];
}
