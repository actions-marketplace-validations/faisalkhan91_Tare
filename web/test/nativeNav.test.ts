import { describe, it, expect, beforeEach } from "vitest";
import { wireNativeNav } from "../src/bootTauri.js";

// the native Go menu / tray emits a `navigate` event with a route string; wireNativeNav
// turns it into an in-app navigation (a hash change). The listen bridge is injected here.
describe("wireNativeNav", () => {
  beforeEach(() => {
    location.hash = "";
  });

  it("navigates on a {route} payload", () => {
    let cb: ((e: { payload: unknown }) => void) | null = null;
    wireNativeNav((event, fn) => {
      if (event === "navigate") cb = fn;
    });
    expect(cb).toBeTruthy();
    cb!({ payload: { route: "trends", param: null } });
    expect(location.hash).toBe("#/trends");
  });

  it("opens a specific run on the canonical profile from a {route:'runs', param} payload", () => {
    let cb: ((e: { payload: unknown }) => void) | null = null;
    wireNativeNav((_event, fn) => {
      cb = fn;
    });
    cb!({ payload: { route: "runs", param: "run-42" } });
    expect(location.hash).toBe("#/investigate/run/run-42");
  });

  it("still tolerates a bare-string payload", () => {
    let cb: ((e: { payload: unknown }) => void) | null = null;
    wireNativeNav((_event, fn) => {
      cb = fn;
    });
    cb!({ payload: "overview" });
    expect(location.hash).toBe("#/overview");
  });

  it("opens native Settings over the current canonical workspace", () => {
    location.hash = "#/investigate?entity=runs";
    let cb: ((e: { payload: unknown }) => void) | null = null;
    wireNativeNav((_event, fn) => {
      cb = fn;
    });
    cb!({ payload: { route: "settings" } });
    expect(location.hash).toBe("#/investigate?entity=runs&sheet=settings");
  });

  it("opens native Capture over the current canonical workspace", () => {
    location.hash = "#/investigate?entity=runs";
    let cb: ((e: { payload: unknown }) => void) | null = null;
    wireNativeNav((_event, fn) => {
      cb = fn;
    });
    cb!({ payload: { route: "connect" } });
    expect(location.hash).toBe("#/investigate?entity=runs&sheet=capture");
  });

  it("opens native Trust/Pricing routes over the current canonical workspace", () => {
    location.hash = "#/optimize?type=cache";
    let cb: ((e: { payload: unknown }) => void) | null = null;
    wireNativeNav((_event, fn) => {
      cb = fn;
    });
    cb!({ payload: { route: "pricing" } });
    expect(location.hash).toBe("#/optimize?sheet=trust&type=cache&view=pricing");
    location.hash = "#/investigate?entity=runs";
    cb!({ payload: { route: "receipts" } });
    expect(location.hash).toBe("#/investigate?entity=runs&sheet=trust");
  });

  it("routes native notification activations through the same scoped deep-link contract", () => {
    location.hash = "#/optimize?view=verifying";
    const callbacks = new Map<string, (e: { payload: unknown }) => void>();
    wireNativeNav((event, fn) => callbacks.set(event, fn));
    callbacks.get("notification-action")!({ payload: { route: "settings" } });
    expect(location.hash).toBe("#/optimize?sheet=settings&view=verifying");
    callbacks.get("notification-action")!({
      payload: { hash: "#/investigate/run/run-42?view=timeline" },
    });
    expect(location.hash).toBe("#/investigate/run/run-42?view=timeline");
  });

  it("ignores malformed payloads", () => {
    let cb: ((e: { payload: unknown }) => void) | null = null;
    wireNativeNav((_event, fn) => {
      cb = fn;
    });
    cb!({ payload: 42 });
    cb!({ payload: "" });
    cb!({ payload: { route: "" } });
    cb!({ payload: {} });
    expect(location.hash).toBe("");
  });

  it("restores a saved view via a {hash} payload", () => {
    let cb: ((e: { payload: unknown }) => void) | null = null;
    wireNativeNav((_event, fn) => {
      cb = fn;
    });
    cb!({ payload: { hash: "#/trends?by=model" } });
    expect(location.hash).toBe("#/trends?by=model");
    cb!({ payload: { hash: "#/investigate?investigation=inv-v2" } });
    expect(location.hash).toBe("#/investigate?investigation=inv-v2");
    // A non-in-app hash is ignored (defensive).
    location.hash = "";
    cb!({ payload: { hash: "https://evil.example" } });
    expect(location.hash).toBe("");
  });

  it("is a no-op when no listen bridge is available", () => {
    // Neither an injected listener nor a global __TAURI__ → must not throw.
    expect(() => wireNativeNav(undefined)).not.toThrow();
  });
});
