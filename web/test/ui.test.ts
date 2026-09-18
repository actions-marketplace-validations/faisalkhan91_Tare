import { describe, it, expect, beforeEach } from "vitest";
import { el, rawSvg, clear } from "../src/ui/el.js";
import { parseHash, routeHash, onRoute, navigate, type Route } from "../src/ui/store.js";
import {
  initialTheme,
  applyTheme,
  watchOsTheme,
  themePreference,
  setThemePreference,
  resolveTheme,
  initialIdentity,
  applyIdentity,
} from "../src/ui/theme.js";
import { fmtTokens, fmtSignedDollars, fmtUsd, fmtSignedUsd, fmtDuration, humanizeKey } from "../src/ui/format.js";

describe("el()", () => {
  it("sets class/attrs, attaches handlers, and inserts text safely", () => {
    let clicked = 0;
    const node = el("button", { class: "btn", "data-x": "1", onClick: () => clicked++ }, [
      "hello <b>not bold</b>",
    ]);
    expect(node.className).toBe("btn");
    expect(node.getAttribute("data-x")).toBe("1");
    // The string is a text node, never parsed as markup (XSS guard).
    expect(node.querySelector("b")).toBeNull();
    expect(node.textContent).toBe("hello <b>not bold</b>");
    node.dispatchEvent(new Event("click"));
    expect(clicked).toBe(1);
  });

  it("rawSvg is the only innerHTML sink; clear empties a node", () => {
    const host = el("div");
    rawSvg(host, "<svg><rect/></svg>");
    expect(host.querySelector("rect")).toBeTruthy();
    clear(host);
    expect(host.childNodes.length).toBe(0);
  });

  it("omits attrs whose value is false or undefined (conditional-attribute idiom)", () => {
    const node = el("button", { disabled: false, hidden: undefined, "data-on": true });
    expect(node.hasAttribute("disabled")).toBe(false);
    expect(node.hasAttribute("hidden")).toBe(false);
    // A `true` value is still rendered (as the string "true").
    expect(node.getAttribute("data-on")).toBe("true");
  });

  it("attaches an onChange listener", () => {
    let changed = 0;
    const node = el("input", { onChange: () => changed++ });
    node.dispatchEvent(new Event("change"));
    expect(changed).toBe(1);
  });
});

describe("router", () => {
  it("parseHash / routeHash round-trip including a slashy param", () => {
    // Router v2: every route carries `segments`; `param` remains as a compat field.
    expect(parseHash("")).toEqual({ name: "pulse", segments: ["pulse"] });
    expect(parseHash("#/")).toEqual({ name: "pulse", segments: ["pulse"] });
    expect(parseHash("#/runs/a%2Fb")).toEqual({
      name: "runs",
      segments: ["runs", "a/b"],
      param: "a/b",
    });
    expect(routeHash("runs", "a/b")).toBe("#/runs/a%2Fb");
    expect(routeHash("trends")).toBe("#/trends");
  });

  it("round-trips view-state query in the hash", () => {
    // Query parses into a record; keys serialize sorted + stable.
    expect(parseHash("#/trends?by=model&win=7d")).toEqual({
      name: "trends",
      segments: ["trends"],
      query: { by: "model", win: "7d" },
    });
    expect(parseHash("#/runs/a%2Fb?by=model")).toEqual({
      name: "runs",
      segments: ["runs", "a/b"],
      param: "a/b",
      query: { by: "model" },
    });
    expect(routeHash("trends", undefined, { win: "7d", by: "model" })).toBe(
      "#/trends?by=model&win=7d"
    );
    // Empty query values are dropped; no `?` when query is empty.
    expect(routeHash("trends", undefined, { by: "" })).toBe("#/trends");
    expect(parseHash("#/overview")).toEqual({ name: "overview", segments: ["overview"] });
  });

  it("setRouteQuery merges into the current route's query", async () => {
    const { setRouteQuery, currentQuery } = await import("../src/ui/store.js");
    const win = { location: { hash: "#/trends?by=model" } } as unknown as Window;
    setRouteQuery({ win: "30d" }, win);
    expect(win.location.hash).toBe("#/trends?by=model&win=30d");
    expect(currentQuery(win)).toEqual({ by: "model", win: "30d" });
  });

  it("onRoute fires once immediately, again on hashchange, and stops after unsubscribe", () => {
    // Minimal fake window: records listeners and exposes a mutable location.hash.
    let handler: (() => void) | null = null;
    const seen: Route[] = [];
    const win = {
      location: { hash: "#/runs/r1" },
      addEventListener: (_t: string, fn: () => void) => {
        handler = fn;
      },
      removeEventListener: () => {
        handler = null;
      },
    } as unknown as Window;

    const off = onRoute((r) => seen.push(r), win);
    expect(seen).toEqual([{ name: "runs", segments: ["runs", "r1"], param: "r1" }]); // fired once synchronously

    (win as unknown as { location: { hash: string } }).location.hash = "#/trends";
    (handler as (() => void) | null)?.();
    expect(seen[1]).toEqual({ name: "trends", segments: ["trends"] });

    off();
    expect(handler).toBeNull(); // unsubscribed -> listener removed

    navigate("runs", "x/y", win);
    expect((win as unknown as { location: { hash: string } }).location.hash).toBe("#/runs/x%2Fy");
  });
});

describe("theme", () => {
  beforeEach(() => localStorage.clear());
  it("tri-state preference: fresh install is System; explicit overrides persist", () => {
    // Fresh install (no stored value) → System (which resolves to dark with no matchMedia in jsdom).
    expect(themePreference()).toBe("system");
    expect(initialTheme()).toBe("dark");
    // Resolve maps an explicit preference straight through.
    expect(resolveTheme("light")).toBe("light");
    expect(resolveTheme("dark")).toBe("dark");
    // setThemePreference persists the choice and returns the resolved render theme to apply.
    expect(setThemePreference("light")).toBe("light");
    expect(themePreference()).toBe("light");
    expect(initialTheme()).toBe("light");
    applyTheme("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    // Choosing System persists the literal "system" (never a storage deletion), so the app keeps
    // tracking the OS rather than freezing today's resolved value.
    expect(setThemePreference("system")).toBe("dark");
    expect(localStorage.getItem("tare-theme")).toBe("system");
    expect(themePreference()).toBe("system");
  });

  it("follows OS theme changes live until an explicit choice is stored", () => {
    // Controllable matchMedia stub: capture the change handler so the test can flip the OS signal.
    let handler: ((e: { matches: boolean }) => void) | null = null;
    const orig = (globalThis as unknown as { matchMedia?: unknown }).matchMedia;
    (globalThis as unknown as { matchMedia: unknown }).matchMedia = () => ({
      matches: false,
      addEventListener: (_: string, h: (e: { matches: boolean }) => void) => (handler = h),
      removeEventListener: () => (handler = null),
    });
    try {
      localStorage.clear();
      const seen: string[] = [];
      const stop = watchOsTheme((t) => seen.push(t));
      // OS flips to light → callback fires (no override stored yet).
      handler!({ matches: true });
      expect(seen).toEqual(["light"]);
      // An explicit persisted choice now wins — later OS changes are ignored.
      localStorage.setItem("tare-theme", "light");
      handler!({ matches: false });
      expect(seen).toEqual(["light"]); // unchanged; the manual choice is authoritative
      stop();
      expect(handler).toBeNull(); // unsubscribed
    } finally {
      (globalThis as unknown as { matchMedia?: unknown }).matchMedia = orig;
      localStorage.clear();
    }
  });
});

describe("one committed visual identity", () => {
  beforeEach(() => localStorage.clear());
  it("is a single bench identity and stamps data-identity=bench", () => {
    expect(initialIdentity()).toBe("bench");
    applyIdentity("bench");
    expect(document.documentElement.getAttribute("data-identity")).toBe("bench");
  });
  it("migrates a legacy stored 'brass' identity to 'bench'", () => {
    localStorage.setItem("tare-identity", "brass");
    expect(initialIdentity()).toBe("bench");
    expect(localStorage.getItem("tare-identity")).toBe("bench"); // rewritten in place
  });
});

describe("density", () => {
  it("defaults to comfortable, persists, and applies to data-density", async () => {
    const { density, setDensity, applyDensity } = await import("../src/ui/prefs.js");
    localStorage.removeItem("tare-density");
    expect(density()).toBe("comfortable");
    setDensity("compact");
    expect(density()).toBe("compact");
    applyDensity("compact");
    expect(document.documentElement.getAttribute("data-density")).toBe("compact");
  });
});

describe("scatter", () => {
  it("renders one toned dot per point, deterministically", async () => {
    const { scatter } = await import("../src/ui/scatter.js");
    const svg = scatter(
      [
        { x: 10, y: 100, label: "a", tone: "cost-ok" },
        { x: 20, y: 50, label: "b", tone: "cost-high" },
        { x: 0, y: 0, label: "c" },
      ],
      { xLabel: "tokens →", yLabel: "spend ↑" }
    );
    const dots = svg.querySelectorAll(".scatter-dot");
    expect(dots.length).toBe(3);
    expect(svg.querySelector(".scatter-dot.cost-high")).toBeTruthy();
    expect(dots[0].querySelector("title")?.textContent).toBe("a");
    // Byte-stable: same input → identical markup.
    const again = scatter(
      [
        { x: 10, y: 100, label: "a", tone: "cost-ok" },
        { x: 20, y: 50, label: "b", tone: "cost-high" },
        { x: 0, y: 0, label: "c" },
      ],
      { xLabel: "tokens →", yLabel: "spend ↑" }
    );
    expect(svg.outerHTML).toBe(again.outerHTML);
  });
});

describe("parcoords", () => {
  it("draws one axis per dimension and one toned polyline per row, deterministically", async () => {
    const { parcoords } = await import("../src/ui/parcoords.js");
    const axes = ["tokens", "spend"];
    const rows = [
      { label: "a", values: [1000, 100], tone: "cost-ok" },
      { label: "b", values: [2000, 400], tone: "cost-high" },
    ];
    const svg = parcoords(axes, rows);
    expect(svg.querySelectorAll(".pc-axis").length).toBe(2);
    expect(svg.querySelectorAll(".pc-line").length).toBe(2);
    expect(svg.querySelector(".pc-line.cost-high")).toBeTruthy();
    expect(svg.querySelector(".pc-line title")?.textContent).toBe("a");
    // Byte-stable output.
    expect(parcoords(axes, rows).outerHTML).toBe(svg.outerHTML);
  });
});

describe("format", () => {
  it("groups tokens and signs exact deltas", () => {
    expect(fmtTokens(1234567)).toBe("1,234,567");
    expect(fmtSignedDollars(1_500_000)).toBe("+$1.500000");
    expect(fmtSignedDollars(-400_000)).toBe("-$0.400000");
  });

  it("fmtDuration: ms under 1s, seconds with one decimal above", () => {
    expect(fmtDuration(0)).toBe("0 ms");
    expect(fmtDuration(350)).toBe("350 ms");
    expect(fmtDuration(999)).toBe("999 ms");
    expect(fmtDuration(1000)).toBe("1.0 s");
    expect(fmtDuration(2400)).toBe("2.4 s");
  });

  it("fmtUsd: concise display — 2dp default, sub-cent trimmed, large compacted, zero", () => {
    expect(fmtUsd(0)).toBe("$0.00");
    expect(fmtUsd(50_000)).toBe("$0.05"); // $0.05
    expect(fmtUsd(12_400_000)).toBe("$12.40"); // 2dp
    expect(fmtUsd(1_234_560_000)).toBe("$1,234.56"); // thousands separators
    expect(fmtUsd(1_571)).toBe("$0.0016"); // sub-cent -> 2 sig figs, not $0.001571
    expect(fmtUsd(-400_000)).toBe("-$0.40"); // negative
    expect(fmtUsd(2_500_000 * 1_000_000)).toBe("$2.5M"); // compact at >= $1M
  });

  it("fmtUsd: $0.01 cutoff boundary and non-finite guard", () => {
    expect(fmtUsd(10_000)).toBe("$0.01"); // exactly SUBCENT -> 2dp branch
    expect(fmtUsd(9_999)).toBe("$0.01"); // just below -> sub-cent (rounds to 2 sig figs)
    expect(fmtUsd(-2_500_000 * 1_000_000)).toBe("-$2.5M"); // negative compact
    expect(fmtUsd(NaN)).toBe("N/A"); // never "$NaN"
    expect(fmtUsd(Infinity)).toBe("N/A");
  });

  it("fmtUsd: sub-cent stays rankable (2 sig figs, never a collapsed <$0.01) + compaction boundary", () => {
    // Two distinct sub-cent costs must render distinctly — collapsing both to "<$0.01" would destroy
    // the per-unit ranking the product exists for.
    expect(fmtUsd(1_600)).toBe("$0.0016");
    expect(fmtUsd(4_000)).toBe("$0.004");
    expect(fmtUsd(1_600)).not.toBe(fmtUsd(4_000));
    expect(fmtUsd(1_600)).not.toContain("<");
    // Compaction is at >= $1,000,000 exactly; there is no "$1.2K" compaction — thousands render full.
    expect(fmtUsd(1_000_000 * 1_000_000)).toBe("$1M"); // exactly the threshold compacts
    expect(fmtUsd(999_999_000_000)).toBe("$999,999.00"); // just below stays in full, no "K"
    expect(fmtUsd(1_240_000_000)).toBe("$1,240.00"); // thousands are NOT compacted to "$1.2K"
  });

  it("humanizeKey: snake/kebab → sentence case, value unchanged elsewhere", () => {
    expect(humanizeKey("anomaly_kind")).toBe("Anomaly kind");
    expect(humanizeKey("strict_counts")).toBe("Strict counts");
    expect(humanizeKey("today_spend")).toBe("Today spend");
    expect(humanizeKey("run-rate")).toBe("Run rate"); // kebab too
    expect(humanizeKey("fingerprint")).toBe("Fingerprint"); // single word
    expect(humanizeKey("")).toBe(""); // empty → empty (no crash)
  });

  it("fmtSignedUsd: explicit sign on concise display", () => {
    expect(fmtSignedUsd(1_500_000)).toBe("+$1.50");
    expect(fmtSignedUsd(-400_000)).toBe("-$0.40");
    expect(fmtSignedUsd(0)).toBe("+$0.00");
  });
});
