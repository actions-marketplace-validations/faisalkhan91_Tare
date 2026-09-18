import { describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import { renderInvestigate } from "../src/workspaces/investigate.js";
import { renderRunProfile } from "../src/workspaces/runProfile.js";
import type { WorkspaceContext } from "../src/shell/workbench.js";
import type { Route } from "../src/ui/store.js";
import type { RunStep, TareClient } from "../src/client.js";
import { fakeClient } from "./fakeClient.js";

const runId = "org/team/run 7";

function route(view = "profile"): Route {
  return {
    name: "investigate",
    segments: ["investigate", "run", runId],
    query: {
      from: "2026-07-01",
      to: "2026-07-14",
      tz: "America/Los_Angeles",
      model: "claude-sonnet-4",
      view,
    },
  };
}

function client(overrides: Partial<TareClient> = {}): TareClient {
  return fakeClient({
    runStatus: async (id) => ({
      run_id: id,
      micros: 2_400_000,
      steps: 2,
      tokens: 12_000,
      last_model: "claude-sonnet-4",
      top_cause: "fresh input",
    }),
    runMeta: async (id) => ({
      run_id: id,
      created_date: "2026-07-14",
      privacy_policy_id: "strict_counts",
      profile: "strict_counts",
      steps: 2,
      models: ["claude-sonnet-4"],
      providers: ["anthropic"],
      sources: ["proxy"],
      stop_reasons: ["end_turn"],
      pricing_version: "2026-07-01",
      effective_date: "2026-07-01",
    }),
    runSteps: async () => [
      {
        ordinal: 1,
        provider: "anthropic",
        model: "claude-sonnet-4",
        fresh_input: 6_000,
        cache_read: 0,
        cache_write: 0,
        output: 800,
        reasoning: 0,
        tokens: 6_800,
        micros: 1_600_000,
        stop_reason: "tool_use",
        duration_ms: 250,
        anatomy: {
          components: [{ component: "system", label: "System", bytes: 1_200, cached: false }],
          total_bytes: 1_200,
          system_hash: "opaque-system-hash",
          request_hash: "opaque-request-hash",
          stream: true,
          cache_control: true,
          ttl: "1h",
          effort: "high",
        },
      },
      {
        ordinal: 2,
        provider: "anthropic",
        model: "claude-sonnet-4",
        fresh_input: 2_000,
        cache_read: 2_500,
        cache_write: 0,
        output: 700,
        reasoning: 0,
        tokens: 5_200,
        micros: 800_000,
        stop_reason: "end_turn",
        duration_ms: 180,
      },
    ],
    sessionAutopsy: async (id) => ({
      run_id: id,
      total_micros: 2_400_000,
      classes: [
        { class: "fresh_input", micros: 1_500_000 },
        { class: "cache_read", micros: 200_000 },
        { class: "cache_write", micros: 100_000 },
        { class: "output", micros: 600_000 },
      ],
      reasoning_micros: 0,
      cache_hit_pct: 24,
      vs_median_pct: 160,
      fidelity: "component",
      headline: {
        kind: "waste",
        detail: {
          kind: "cache",
          label: "Repeated uncached system context",
          recoverable_micros: 500_000,
          confidence: "measured",
          fix_text: "Cache the stable system prefix.",
          effort: "S",
        },
      },
      opportunities: [
        {
          kind: "cache",
          label: "Repeated uncached system context",
          recoverable_micros: 500_000,
          confidence: "measured",
          fix_text: "Cache the stable system prefix.",
          effort: "S",
        },
      ],
    }),
    profile: async (id) => ({
      run_id: id,
      pricing_version: "2026-07-01",
      sort: "cum",
      total_micros: 2_400_000,
      total_tokens: 12_000,
      rows: [
        { name: "run", self_micros: 0, cum_micros: 2_400_000, self_tokens: 0, cum_tokens: 12_000 },
        { name: "step 1 · claude-sonnet-4", self_micros: 0, cum_micros: 1_600_000, self_tokens: 0, cum_tokens: 6_800 },
        { name: "step 2 · claude-sonnet-4", self_micros: 0, cum_micros: 800_000, self_tokens: 0, cum_tokens: 5_200 },
        { name: "system", self_micros: 1_500_000, cum_micros: 1_500_000, self_tokens: 6_000, cum_tokens: 6_000 },
      ],
    }),
    flamegraph: async (id) => ({
      run_id: id,
      pricing_version: "2026-07-01",
      effective_date: "2026-07-01",
      root: {
        name: `run ${id}`,
        tokens: 12_000,
        micros: 2_400_000,
        children: [
          {
            name: "step 1 · claude-sonnet-4",
            tokens: 6_800,
            micros: 1_600_000,
            children: [
              {
                name: "System",
                tokens: 6_000,
                micros: 1_500_000,
                children: [{ name: "fresh", tokens: 6_000, micros: 1_500_000, cache_class: "fresh", children: [] }],
              },
              {
                name: "Output",
                tokens: 800,
                micros: 100_000,
                children: [{ name: "output", tokens: 800, micros: 100_000, cache_class: "output", children: [] }],
              },
            ],
          },
          {
            name: "step 2 · claude-sonnet-4",
            tokens: 5_200,
            micros: 800_000,
            children: [
              {
                name: "Conversation",
                tokens: 4_500,
                micros: 600_000,
                children: [{ name: "cache read", tokens: 4_500, micros: 600_000, cache_class: "cache_read", children: [] }],
              },
              {
                name: "Output",
                tokens: 700,
                micros: 200_000,
                children: [{ name: "output", tokens: 700, micros: 200_000, cache_class: "output", children: [] }],
              },
            ],
          },
        ],
      },
    }),
    frontier: async () => ({
      points: [{ run_id: runId, cost_micros: 2_400_000, quality: 91, on_frontier: true }],
      has_quality: true,
      pricing_version: "2026-07-01",
      estimated: true,
    }),
    getRunNote: async () => ({
      run_id: runId,
      tags: ["known-good"],
      note_text: "Reviewed locally",
      starred: true,
      updated_at: "2026-07-14T12:00:00Z",
    }),
    ...overrides,
  });
}

function context(): WorkspaceContext {
  const state = initialAnalysisState("investigate");
  state.selection = { ...state.scope, filters: [{ op: "eq", dimension: "model", value: "claude-sonnet-4" }] };
  state.baseline = {
    kind: "prior_window",
    label: "Prior 14 days",
    cohort: { ...state.scope, from: "2026-06-17", to: "2026-06-30" },
    sampleCount: 2,
  };
  return { analysis: createAnalysisStore(state) };
}

async function render(view = "profile", overrides: Partial<TareClient> = {}) {
  const root = document.createElement("div");
  const ctx = context();
  const c = client({
    resolveCohort: async (spec) => ({
      data: {
        run_ids: ["baseline-a", "baseline-b"],
        run_count: 2,
        step_count: 4,
        total_micros: 2_000_000,
        entity_rows: [],
      },
      provenance: { scope: spec } as never,
    }),
    ...overrides,
  });
  await renderRunProfile(root, c, route(view), ctx);
  return { root, ctx, client: c };
}

describe("Run Profile workspace", () => {
  it("replaces the nested run route's legacy long document with the full workspace", async () => {
    const root = document.createElement("div");
    await renderInvestigate(root, client(), route(), context());
    expect(root.querySelector(".run-profile")).toBeTruthy();
    expect(root.querySelector(".master-detail")).toBeNull();
    expect(root.querySelector(".statement-detail")).toBeNull();
  });

  it("keeps the summary and Why this cost context above the routed canvas", async () => {
    const { root } = await render();
    expect(root.querySelector(".run-profile-summary")).toBeTruthy();
    expect(root.querySelector(".run-profile-summary")?.textContent).toContain("Est. priced spend");
    expect(root.querySelector(".run-profile-summary")?.textContent).toContain("+$1.40");
    expect(root.querySelector(".run-profile-summary")?.textContent).toContain("Quality");
    expect(root.querySelector(".run-profile-summary")?.textContent).toContain("91");
    expect(root.querySelector(".run-profile-why")?.textContent).toContain("Repeated uncached system context");
    expect(root.querySelector(".run-profile-why")?.textContent).toContain("$0.50");
    expect(root.querySelector(".tare-beam.beam-run")).toBeTruthy();
    const css = readFileSync(resolve(process.cwd(), "src/ui/workspaces.css"), "utf8");
    expect(css).toMatch(/\.run-profile-header\s*\{[^}]*position:\s*sticky/s);
    expect(css).toMatch(/\.run-profile-inspector\s*\{[^}]*overflow-y:\s*auto/s);
  });

  it("uses bounded entity/canvas/inspector panes and virtualizes a thousand-run navigator", async () => {
    const ids = Array.from({ length: 1_000 }, (_, i) => `run-${String(i).padStart(4, "0")}`);
    const { root } = await render("profile", { listRuns: async () => ids });
    const workbench = root.querySelector<HTMLElement>(".run-profile[data-adaptive-panes]")!;
    expect(workbench).toBeTruthy();
    expect(workbench.querySelector('[data-pane="entities"]')).toBeTruthy();
    expect(workbench.querySelector('[data-pane="canvas"]')).toBeTruthy();
    expect(workbench.querySelector('[data-pane="inspector"]')).toBeTruthy();
    expect(workbench.querySelectorAll(".run-profile-run-row").length).toBeLessThanOrEqual(24);
    expect(workbench.querySelector(".run-profile-run-count")?.textContent).toContain("1001 of 1001");
    expect(workbench.querySelector(".master-detail, .statement-detail")).toBeNull();
  });

  it("exposes Timeline/Profile/Shape/Provenance as scope-preserving routed tabs", async () => {
    const { root } = await render();
    const tabs = Array.from(root.querySelectorAll<HTMLAnchorElement>('.run-profile-tabs [role="tab"]'));
    expect(tabs.map((tab) => tab.textContent)).toEqual(["Timeline", "Profile", "Shape", "Provenance"]);
    for (const tab of tabs) {
      expect(tab.href).toContain("investigate/run/org%2Fteam%2Frun%207");
      expect(tab.href).toContain("from=2026-07-01");
      expect(tab.href).toContain("to=2026-07-14");
      expect(tab.href).toContain("tz=America%2FLos_Angeles");
    }
    const back = root.querySelector<HTMLAnchorElement>(".run-profile-back")!;
    expect(back.href).toContain("#/investigate?");
    expect(back.href).toContain("model=claude-sonnet-4");
    expect(back.href).not.toContain("view=profile");

    const tablist = root.querySelector<HTMLElement>('[role="tablist"]')!;
    const panel = root.querySelector<HTMLElement>('[role="tabpanel"]')!;
    expect(panel.getAttribute("aria-labelledby")).toBe(tabs[1].id);
    expect(tabs[1].tabIndex).toBe(0);
    expect(tabs[0].tabIndex).toBe(-1);
    document.body.replaceChildren(root);
    const activateShape = vi.spyOn(tabs[2], "click").mockImplementation(() => undefined);
    tabs[1].focus();
    tablist.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(document.activeElement).toBe(tabs[2]);
    expect(activateShape).toHaveBeenCalledOnce();
  });

  it("labels opportunity confidence without upgrading projections to measured evidence", async () => {
    const { root } = await render("profile", {
      sessionAutopsy: async (id) => {
        const autopsy = await client().sessionAutopsy(id);
        const opportunity = { ...autopsy.opportunities[0], confidence: "projected" };
        return {
          ...autopsy,
          headline: { kind: "waste" as const, detail: opportunity },
          opportunities: [opportunity],
        };
      },
    });
    const why = root.querySelector(".run-profile-why")?.textContent ?? "";
    expect(why).toContain("Top projected opportunity");
    expect(why).not.toContain("Top measured opportunity");
  });

  it("renders honest foundational content for every tab without implying missing timing", async () => {
    const timeline = (await render("timeline")).root;
    expect(timeline.querySelector("[role=tabpanel]")?.textContent).toContain("Step order");
    expect(timeline.querySelector(".run-profile-timeline-axis")).toBeNull();
    expect(timeline.textContent).not.toContain("concurrent execution is confirmed");
    expect((await render("profile")).root.querySelector(".run-profile-flame")).toBeTruthy();
    const shape = (await render("shape")).root.querySelector("[role=tabpanel]")?.textContent ?? "";
    expect(shape).toContain("opaque-system-hash");
    expect(shape).toContain("opaque-request-hash");
    expect(shape).toContain("Streaming");
    expect(shape).toContain("1 hour");
    expect(shape).toContain("cache-controlled component");
    const provenance = (await render("provenance")).root.querySelector("[role=tabpanel]")?.textContent ?? "";
    expect(provenance).toContain("strict_counts");
    expect(provenance).toContain("Stored run row");
    expect(provenance).toContain("Read-time local pricing table");
    expect(provenance).toContain("Completeness unknown");
  });

  it("omits Inspect entirely for counts-only runs and never requests a transcript", async () => {
    const transcript = vi.fn(async () => ({ req: "must not load", resp: "must not load", truncated: false }));
    const purgeTranscripts = vi.fn(async () => undefined);
    const { root } = await render("shape", { transcript, purgeTranscripts });
    expect(root.querySelector(".run-profile-inspect")).toBeNull();
    expect(root.textContent).not.toContain("Inspect stored bodies");
    expect(transcript).not.toHaveBeenCalled();
    expect(purgeTranscripts).not.toHaveBeenCalled();
  });

  it("gates max-inspect bodies behind explicit load, renders text safely, exposes caps, and purges in two steps", async () => {
    const transcript = vi.fn(async (_id: string, step: number) =>
      step === 1
        ? {
            req: '<img src=x onerror="unsafe()">',
            resp: "capped response",
            truncated: true,
          }
        : null
    );
    const purgeTranscripts = vi.fn(async () => undefined);
    const { root } = await render("shape", {
      runMeta: async (id) => ({ ...(await client().runMeta(id)), profile: "max_inspect" }),
      transcript,
      purgeTranscripts,
    });
    const inspect = root.querySelector<HTMLElement>(".run-profile-inspect")!;
    expect(inspect).toBeTruthy();
    expect(inspect.textContent).toContain("locally stored");
    expect(inspect.textContent).toContain("best-effort redacted");
    expect(inspect.textContent).toContain("capped");
    expect(inspect.textContent).toContain("potentially sensitive");
    expect(transcript).not.toHaveBeenCalled();

    inspect.querySelector<HTMLButtonElement>('[data-inspect-step="1"]')!.click();
    await vi.waitFor(() => expect(transcript).toHaveBeenCalledWith(runId, 1));
    expect(inspect.querySelector("img")).toBeNull();
    expect(inspect.querySelector(".run-profile-transcript")?.textContent).toContain("<img src=x");
    expect(inspect.textContent).toContain("Body was truncated at the capture cap");

    const purge = inspect.querySelector<HTMLButtonElement>(".run-profile-purge")!;
    purge.click();
    expect(purgeTranscripts).not.toHaveBeenCalled();
    expect(purge.textContent).toContain("Confirm");
    purge.click();
    await vi.waitFor(() => expect(purgeTranscripts).toHaveBeenCalledOnce());
    expect(inspect.textContent).toContain("All locally stored bodies were purged");
  });

  it("renders elapsed BigInt positions and confirms concurrency only for overlapping sibling spans", async () => {
    const base = 18_446_744_073_709_551_615n;
    const steps = await client().runSteps(runId);
    const { root } = await render("timeline", {
      runSteps: async () => [
        {
          ...steps[0],
          start_unix_nano: String(base),
          end_unix_nano: String(base + 250_000_000n),
          trace_id: "trace-1",
          span_id: "span-1",
          parent_span_id: "root-span",
        },
        {
          ...steps[1],
          start_unix_nano: String(base + 100_000_000n),
          end_unix_nano: String(base + 280_000_000n),
          trace_id: "trace-1",
          span_id: "span-2",
          parent_span_id: "root-span",
        },
      ],
    });
    expect(root.querySelector("[role=tabpanel] h2")?.textContent).toBe("Timeline");
    expect(root.querySelector(".run-profile-timeline-axis")?.textContent).toContain("280 ms");
    expect(root.querySelector("[data-concurrency-evidence=confirmed]")?.textContent).toContain("timestamp, trace, and shared-parent evidence");
    const second = root.querySelector<HTMLElement>('[data-step-ordinal="2"] .run-profile-timeline-bar')!;
    expect(second.style.left).toBe("35.71%");
    expect(second.style.width).toContain("64.28%");
    expect(root.querySelector('[data-step-ordinal="2"]')?.textContent).toContain("+100 ms");
  });

  it("shows timestamp overlap without upgrading it to concurrency when ancestry is absent", async () => {
    const steps = await client().runSteps(runId);
    const { root } = await render("timeline", {
      runSteps: async () => [
        { ...steps[0], start_unix_nano: "100", end_unix_nano: "300", trace_id: "trace", span_id: "one" },
        { ...steps[1], start_unix_nano: "200", end_unix_nano: "400", trace_id: "trace", span_id: "two" },
      ],
    });
    const evidence = root.querySelector("[data-concurrency-evidence=not-claimed]")?.textContent ?? "";
    expect(evidence).toContain("timestamp overlap");
    expect(evidence).toContain("concurrency is not claimed");
    expect(evidence).not.toContain("confirmed");
  });

  it("keeps one step selection stable across Timeline, Beam, profile, table, and inspector", async () => {
    const ctx = context();
    const c = client();
    const root = document.createElement("div");
    await renderRunProfile(root, c, route("timeline"), ctx);
    root.querySelector<HTMLButtonElement>('[data-step-ordinal="2"] .run-profile-step-select')!.click();
    expect(ctx.analysis.get().focus.stepOrdinal).toBe(2);
    expect(ctx.analysis.get().focus.highlighted?.kind).toBe("step");
    expect(root.querySelector(".run-profile-selected-step")?.textContent).toContain("Selected Step 2");
    expect(root.querySelector('[data-beam-key="cache_read"]')?.classList).toContain("is-cross-highlighted");

    await renderRunProfile(root, c, route("profile"), ctx);
    expect(root.querySelectorAll('[data-step-ordinal="2"][aria-pressed="true"]').length).toBeGreaterThan(0);
    expect(root.querySelector('tr[data-step-ordinals="2"]')?.classList).toContain("is-cross-highlighted");
    expect(root.querySelector(".run-profile-selected-step")?.textContent).toContain("Selected Step 2");

    await renderRunProfile(root, c, route("timeline"), ctx);
    expect(root.querySelector('[data-step-ordinal="2"] button')?.getAttribute("aria-pressed")).toBe("true");
  });

  it("changes only on-screen geometry between Cost and Tokens and keeps frame selection keyboard accessible", async () => {
    const { root, ctx } = await render("profile");
    const stepWidth = () => Number(
      root.querySelector<SVGElement>('.run-profile-flame [data-frame-kind="step"][data-step-ordinal="1"]')
        ?.getAttribute("width")
    );
    const costWidth = stepWidth();
    root.querySelector<HTMLButtonElement>('[data-profile-weight="tokens"]')!.click();
    const tokenWidth = stepWidth();
    expect(costWidth).toBeGreaterThan(0);
    expect(tokenWidth).toBeGreaterThan(0);
    expect(tokenWidth).not.toBe(costWidth);
    expect(root.querySelector(".run-profile-profile-caption")?.textContent).toContain("captured tokens");

    const component = root.querySelector<SVGElement>(
      '.run-profile-flame [data-frame-kind="component"][data-step-ordinal="1"]'
    )!;
    expect(component.getAttribute("tabindex")).toBe("0");
    component.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(ctx.analysis.get().focus.stepOrdinal).toBe(1);
    expect(component.getAttribute("aria-pressed")).toBe("true");
  });

  it("provides recursive aggregation, explicit Sandwich, profiler actions, search, and calls/cost-per-call", async () => {
    const { root } = await render("profile");
    root.querySelector<HTMLButtonElement>('[data-profile-order="aggregated"]')!.click();
    expect(root.querySelectorAll('.run-profile-flame [data-frame-kind="step"]')).toHaveLength(1);
    expect(root.querySelector(".run-profile-flame")?.textContent).toContain("2 calls");
    const table = root.querySelector(".run-profile-cost-table")?.textContent ?? "";
    expect(table).toContain("Calls");
    expect(table).toContain("Cost / call");
    expect(table).toContain("Self tokens");

    // Sandwich never guesses a component: mode first shows the explicit-selection state.
    root.querySelector<HTMLButtonElement>('[data-profile-order="sandwich"]')!.click();
    expect(root.querySelector(".run-profile-flame")?.textContent).toContain("Choose a Sandwich component");
    const component = root.querySelector<HTMLSelectElement>(".run-profile-component-select")!;
    const system = Array.from(component.options).find((option) => option.textContent?.startsWith("System"))!;
    component.value = system.value;
    component.dispatchEvent(new Event("change"));
    expect(root.querySelector(".run-profile-flame")?.textContent).toContain("Sandwich");
    expect(root.querySelector(".run-profile-cost-table")?.textContent).toContain("Caller");
    expect(root.querySelector(".run-profile-cost-table")?.textContent).toContain("Selected");
    expect(root.querySelector(".run-profile-cost-table")?.textContent).toContain("Callee");

    // Search filters the path-aware table and profiler actions reshape only the themed screen model.
    const search = root.querySelector<HTMLInputElement>(".run-profile-frame-search")!;
    search.value = "fresh";
    search.dispatchEvent(new Event("input"));
    expect(root.querySelectorAll("tr[data-profile-row]").length).toBeGreaterThan(0);
    expect(root.querySelector(".run-profile-cost-table")?.textContent?.toLowerCase()).toContain("fresh");

    const target = root.querySelector<HTMLSelectElement>(".run-profile-frame-target")!;
    target.selectedIndex = 1;
    target.dispatchEvent(new Event("change"));
    root.querySelector<HTMLButtonElement>('[data-profile-action="focus"]')!.click();
    expect(root.querySelector<HTMLButtonElement>('[data-profile-action="focus"]')?.getAttribute("aria-pressed")).toBe("true");
    expect(root.querySelector<HTMLButtonElement>(".run-profile-frame-actions .ghost")?.disabled).toBe(false);
  });

  it("keeps a thousand-step timeline DOM bounded while paging through every captured row", async () => {
    const many: RunStep[] = Array.from({ length: 1_000 }, (_, index) => ({
      ordinal: index + 1,
      provider: "anthropic",
      model: "claude-sonnet-4",
      fresh_input: 10,
      cache_read: 0,
      cache_write: 0,
      output: 2,
      reasoning: 0,
      tokens: 12,
      micros: 1_000,
      stop_reason: null,
      duration_ms: 10,
    }));
    const { root } = await render("timeline", {
      runStatus: async (id) => ({ run_id: id, micros: 1_000_000, steps: 1_000, tokens: 12_000, top_cause: null }),
      runSteps: async () => many,
    });
    expect(root.querySelectorAll(".run-profile-step")).toHaveLength(100);
    expect(root.querySelector(".run-profile-more")?.textContent).toContain("Showing 1–100 of 1000");
    root.querySelectorAll<HTMLButtonElement>(".run-profile-more button")[1].click();
    expect(root.querySelectorAll(".run-profile-step")).toHaveLength(100);
    expect(root.querySelector(".run-profile-more")?.textContent).toContain("Showing 101–200 of 1000");
  });

  it("keeps notes, export, and attestation in a persistent contextual inspector", async () => {
    const saveRunNote = vi.fn(async () => undefined);
    const exportRun = vi.fn(async () => "{\"ok\":true}");
    const receipt = vi.fn(async () => ({
      receipt: {},
      verify: {
        scope: `run:${runId}`,
        pricing_version: "2026-07-01",
        recomputed_total_micros: 2_400_000,
        rows: 2,
        flamegraph_checked: true,
        digest: 7,
      },
    }));
    const { root } = await render("shape", { saveRunNote, exportRun, receipt });
    const inspector = root.querySelector(".run-profile-inspector")!;
    expect(inspector.textContent).toContain("Run context");
    expect(inspector.textContent).toContain("Notes");
    expect(inspector.textContent).toContain("Export");
    expect(inspector.textContent).toContain("Attestation");

    const textarea = inspector.querySelector<HTMLTextAreaElement>(".run-profile-note")!;
    textarea.value = "Updated note";
    inspector.querySelector<HTMLButtonElement>(".run-profile-note-save")!.click();
    await vi.waitFor(() => expect(saveRunNote).toHaveBeenCalled());

    inspector.querySelector<HTMLButtonElement>('[data-export="receipt"]')!.click();
    await vi.waitFor(() => expect(exportRun).toHaveBeenCalledWith(runId, "receipt"));

    inspector.querySelector<HTMLButtonElement>(".run-profile-attest")!.click();
    await vi.waitFor(() => expect(receipt).toHaveBeenCalledWith(runId, false));
    expect(inspector.querySelector(".receipt-ledger")?.textContent).toContain("$2.40");
  });

  it("preserves the investigation state and records the open run as the pinned entity", async () => {
    const { ctx } = await render();
    expect(ctx.analysis.get().selection?.filters).toEqual([
      { op: "eq", dimension: "model", value: "claude-sonnet-4" },
    ]);
    expect(ctx.analysis.get().baseline?.label).toBe("Prior 14 days");
    expect(ctx.analysis.get().pinned).toEqual({ kind: "run", id: runId, label: runId });
  });

  it("never renders unpriced usage as $0 or invents a duration", async () => {
    const { root } = await render("profile", {
      runStatus: async (id) => ({ run_id: id, micros: 0, steps: 1, tokens: 9_000, top_cause: null, unpriced: true }),
      runSteps: async () => [{
        ordinal: 1,
        provider: "local",
        model: "unpriced-local",
        fresh_input: 9_000,
        cache_read: 0,
        cache_write: 0,
        output: 0,
        reasoning: 0,
        tokens: 9_000,
        micros: 0,
        stop_reason: null,
      }],
      flamegraph: async (id) => ({
        run_id: id,
        pricing_version: "test",
        effective_date: "2026-07-01",
        root: {
          name: `run ${id}`,
          tokens: 9_000,
          micros: 0,
          children: [{
            name: "step 1 · unpriced-local",
            tokens: 9_000,
            micros: 0,
            children: [{ name: "fresh", tokens: 9_000, micros: 0, cache_class: "fresh", children: [] }],
          }],
        },
      }),
    });
    const summary = root.querySelector(".run-profile-summary")?.textContent ?? "";
    expect(summary).toContain("Unpriced");
    expect(summary).not.toContain("$0.00");
    expect(summary).toContain("Not captured");
    expect(root.querySelector(".tare-beam-gap")?.textContent).toContain("not priced");
    expect(root.querySelector('[data-profile-weight="tokens"]')?.getAttribute("aria-pressed")).toBe("true");
    expect(root.querySelectorAll(".run-profile-flame [data-frame]").length).toBeGreaterThan(1);
  });

  it("shows an explicit failure instead of a partial/misleading profile", async () => {
    const root = document.createElement("div");
    await renderRunProfile(root, client({
      runStatus: async () => { throw new Error("store unavailable"); },
      listRuns: async () => { throw new Error("store unavailable"); },
    }), route(), context());
    expect(root.querySelector(".error")?.textContent).toContain("Couldn't load Run Profile");
    expect(root.querySelector(".error")?.textContent).toContain("Check that capture is running");
    expect(root.querySelector(".run-profile-summary")).toBeNull();
  });

  it("recognizes a stale Run Profile link and removes its persisted destinations", async () => {
    localStorage.setItem("tare-pinned-runs", JSON.stringify([runId, "still-here"]));
    localStorage.setItem("tare-recent-runs", JSON.stringify([runId, "still-here"]));
    localStorage.setItem("tare-baseline-run", runId);
    const root = document.createElement("div");
    const ctx = context();
    ctx.analysis.setPinned({ kind: "run", id: runId, label: runId });

    await renderRunProfile(root, client({
      runStatus: async () => { throw new Error("missing run"); },
      listRuns: async () => ["still-here"],
    }), route(), ctx);

    expect(root.querySelector(".error")?.textContent).toContain("is no longer available in the active capture store");
    expect(root.querySelectorAll(".error-actions .btn")).toHaveLength(2);
    expect(JSON.parse(localStorage.getItem("tare-pinned-runs") ?? "[]")).toEqual(["still-here"]);
    expect(JSON.parse(localStorage.getItem("tare-recent-runs") ?? "[]")).toEqual(["still-here"]);
    expect(localStorage.getItem("tare-baseline-run")).toBeNull();
    expect(ctx.analysis.get().pinned).toBeNull();
  });
});
