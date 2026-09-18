// Hash router and redirect matrix. Proves that segments encode and decode
// independently (slashy IDs round-trip), the pattern matcher, every ROUTES.md legacy case reaches
// its exact canonical state (query params and run IDs preserved, no placeholder), compatibility
// overloads still work, and defaultScreen migration remains safe.

import { describe, it, expect } from "vitest";
import {
  parseHash,
  routePath,
  routeHash,
  hashOf,
  matchRoute,
  type Route,
} from "../src/ui/store.js";
import { redirectRoute, migrateDefaultScreen } from "../src/ui/routes.js";

const CANONICAL_ROOTS = new Set(["pulse", "investigate", "optimize", "onboarding"]);

describe("Router v2 segment model", () => {
  it("encodes/decodes each segment independently so a slashy id round-trips", () => {
    const segs = ["investigate", "run", "org/team/a b"];
    const hash = routePath(segs);
    // The id's `/` and space are percent-encoded WITHIN one segment (not split into new segments).
    expect(hash).toBe("#/investigate/run/org%2Fteam%2Fa%20b");
    const r = parseHash(hash);
    expect(r.segments).toEqual(segs);
    expect(r.name).toBe("investigate");
  });

  it("cold-start empty hash -> pulse with segments", () => {
    // Default is the real current home (pulse), not the retired 'overview'.
    expect(parseHash("")).toEqual({ name: "pulse", segments: ["pulse"] });
    expect(parseHash("#/")).toEqual({ name: "pulse", segments: ["pulse"] });
  });

  it("malformed percent-escapes fall back to the raw segment (never throws)", () => {
    const r = parseHash("#/investigate/run/%E0%A4%A"); // truncated escape
    expect(r.name).toBe("investigate");
    expect(r.segments[2]).toBe("%E0%A4%A");
  });

  it("hashOf is the inverse of parseHash for multi-segment routes with query", () => {
    const r = parseHash("#/investigate/run/a%2Fb?metric=spend_micros&norm=absolute");
    expect(hashOf(r)).toBe("#/investigate/run/a%2Fb?metric=spend_micros&norm=absolute");
  });
});

describe("matchRoute", () => {
  it("captures a nested run id including a slash", () => {
    const r = parseHash("#/investigate/run/a%2Fb");
    expect(matchRoute(r, "investigate/run/:id")).toEqual({ id: "a/b" });
  });
  it("matches a literal nested route with no captures", () => {
    expect(matchRoute(parseHash("#/investigate/compare"), "investigate/compare")).toEqual({});
  });
  it("returns null on segment-count or literal mismatch", () => {
    expect(matchRoute(parseHash("#/investigate"), "investigate/run/:id")).toBeNull();
    expect(matchRoute(parseHash("#/investigate/run/x"), "investigate/compare")).toBeNull();
    expect(matchRoute(parseHash("#/optimize"), "investigate")).toBeNull();
  });
});

describe("compatibility overloads (one release)", () => {
  it("routeHash(name, param, query) still serializes as before", () => {
    expect(routeHash("runs", "a/b")).toBe("#/runs/a%2Fb");
    expect(routeHash("trends", undefined, { by: "model", win: "7d" })).toBe(
      "#/trends?by=model&win=7d"
    );
    expect(routeHash("trends", undefined, { by: "" })).toBe("#/trends");
  });
  it("param compat field mirrors the joined tail", () => {
    expect(parseHash("#/runs/abc").param).toBe("abc");
    expect(parseHash("#/investigate/run/a%2Fb").param).toBe("run/a/b");
    expect(parseHash("#/pulse").param).toBeUndefined();
  });
});

// Every legacy hash documented in ROUTES.md maps to its exact canonical Route.
describe("redirect matrix", () => {
  const cases: Array<[string, Route]> = [
    // Monitor -> Pulse
    ["#/live", { name: "pulse", segments: ["pulse"], query: { mode: "now" } }],
    ["#/overview", { name: "pulse", segments: ["pulse"] }],
    // Investigate
    ["#/runs", { name: "investigate", segments: ["investigate"], query: { entity: "runs" } }],
    [
      "#/runs/run-42",
      { name: "investigate", segments: ["investigate", "run", "run-42"], param: "run/run-42" },
    ],
    [
      "#/sessions",
      { name: "investigate", segments: ["investigate"], query: { entity: "sessions" } },
    ],
    ["#/trends", { name: "investigate", segments: ["investigate"], query: { mode: "timeline" } }],
    [
      "#/correlate",
      { name: "investigate", segments: ["investigate"], query: { mode: "distinguish" } },
    ],
    [
      "#/lineage",
      {
        name: "investigate",
        segments: ["investigate"],
        query: { entity: "templates", mode: "lineage" },
      },
    ],
    [
      "#/units",
      {
        name: "investigate",
        segments: ["investigate"],
        query: { metric: "spend_micros", norm: "per_outcome", view: "units" },
      },
    ],
    [
      "#/segments",
      { name: "investigate", segments: ["investigate"], query: { view: "facets" } },
    ],
    [
      "#/diff",
      { name: "investigate", segments: ["investigate", "compare"], param: "compare" },
    ],
    // Act -> Optimize
    ["#/experiments", { name: "optimize", segments: ["optimize"], query: { view: "scenarios" } }],
    ["#/advise", { name: "optimize", segments: ["optimize"], query: { type: "cache" } }],
    ["#/whatif", { name: "optimize", segments: ["optimize"], query: { view: "scenarios" } }],
    // Trust / utility sheets -> Pulse
    ["#/receipts", { name: "pulse", segments: ["pulse"], query: { sheet: "trust" } }],
    [
      "#/pricing",
      { name: "pulse", segments: ["pulse"], query: { sheet: "trust", view: "pricing" } },
    ],
    ["#/connect", { name: "pulse", segments: ["pulse"], query: { sheet: "capture" } }],
    ["#/settings", { name: "pulse", segments: ["pulse"], query: { sheet: "settings" } }],
  ];

  for (const [hash, expected] of cases) {
    it(`${hash} -> ${hashOf(expected)}`, () => {
      expect(redirectRoute(parseHash(hash))).toEqual(expected);
    });
  }

  it("preserves the trends `?by=` dimension", () => {
    expect(redirectRoute(parseHash("#/trends?by=model"))).toEqual({
      name: "investigate",
      segments: ["investigate"],
      query: { by: "model", mode: "timeline" },
    });
  });

  it("preserves the compare `?runs=` id list into investigate/compare", () => {
    expect(redirectRoute(parseHash("#/compare?runs=a,b,c"))).toEqual({
      name: "investigate",
      segments: ["investigate", "compare"],
      param: "compare",
      query: { runs: "a,b,c" },
    });
  });

  it("preserves a named work-unit outcome on the units view", () => {
    expect(redirectRoute(parseHash("#/units?outcome=unit:pull_requests"))).toEqual({
      name: "investigate",
      segments: ["investigate"],
      query: {
        metric: "spend_micros",
        norm: "per_outcome",
        outcome: "unit:pull_requests",
        view: "units",
      },
    });
  });

  it("best-effort: overview?tab=segments -> investigate?view=facets (tab dropped)", () => {
    expect(redirectRoute(parseHash("#/overview?tab=segments"))).toEqual({
      name: "investigate",
      segments: ["investigate"],
      query: { view: "facets" },
    });
  });

  it("canonical routes are identity (no rewrite)", () => {
    for (const h of ["#/pulse", "#/investigate", "#/optimize", "#/onboarding"]) {
      const r = parseHash(h);
      expect(redirectRoute(r)).toEqual(r);
    }
  });

  it("every legacy route reaches a canonical root — never a placeholder", () => {
    const legacy = [
      "live", "overview", "runs", "sessions", "trends", "correlate", "lineage", "units",
      "segments", "diff", "compare", "experiments", "advise", "whatif", "receipts", "pricing",
      "connect", "settings",
    ];
    for (const name of legacy) {
      const canonical = redirectRoute(parseHash(`#/${name}`));
      expect(CANONICAL_ROOTS.has(canonical.name)).toBe(true);
    }
  });

  it("redirected hashes round-trip back to the same canonical route", () => {
    for (const [hash] of cases) {
      const canonical = redirectRoute(parseHash(hash));
      expect(parseHash(hashOf(canonical))).toEqual(canonical);
    }
  });
});

describe("defaultScreen migration", () => {
  it("maps every legacy default to a canonical workspace", () => {
    expect(migrateDefaultScreen("overview")).toBe("pulse");
    expect(migrateDefaultScreen("live")).toBe("pulse");
    expect(migrateDefaultScreen("runs")).toBe("investigate");
    expect(migrateDefaultScreen("sessions")).toBe("investigate");
    expect(migrateDefaultScreen("trends")).toBe("investigate");
    expect(migrateDefaultScreen("optimize")).toBe("optimize");
    expect(migrateDefaultScreen("experiments")).toBe("optimize");
  });
  it("passes canonical values through and falls back to pulse", () => {
    expect(migrateDefaultScreen("pulse")).toBe("pulse");
    expect(migrateDefaultScreen("investigate")).toBe("investigate");
    expect(migrateDefaultScreen("nonsense")).toBe("pulse");
  });
});
