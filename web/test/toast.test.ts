import { describe, it, expect, beforeEach, vi } from "vitest";
import { showToast } from "../src/ui/toast.js";

describe("showToast", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  it("creates exactly one .toasts host and reuses it across calls", () => {
    showToast("first");
    showToast("second");
    const hosts = document.querySelectorAll(".toasts");
    expect(hosts.length).toBe(1);
    expect(hosts[0].children.length).toBe(2);
  });

  it("maps levels to classes and ARIA live-region behavior", () => {
    for (const level of ["warn", "info"] as const) {
      const t = showToast(`m-${level}`, level);
      expect(t.className).toBe(`toast toast-${level}`);
      expect(t.getAttribute("role")).toBe("status");
      expect(t.getAttribute("aria-live")).toBe("polite");
      expect(t.querySelector(".toast-msg")?.textContent).toBe(`m-${level}`);
    }
    for (const level of ["over", "anomaly"] as const) {
      const t = showToast(`m-${level}`, level);
      expect(t.className).toBe(`toast toast-${level} toast-alert`);
      expect(t.getAttribute("role")).toBe("alert");
      expect(t.getAttribute("aria-live")).toBe("assertive");
      expect(t.querySelector(".toast-msg")?.textContent).toBe(`m-${level}`);
    }
  });

  it("has a manual close button that dismisses the toast", () => {
    const t = showToast("closable", "info");
    expect(t.isConnected).toBe(true);
    const close = t.querySelector<HTMLButtonElement>(".toast-close");
    expect(close?.getAttribute("aria-label")).toBe("Dismiss");
    close!.click();
    expect(t.isConnected).toBe(false);
  });

  it("pauses auto-dismiss while hovered and resumes on leave", () => {
    vi.useFakeTimers();
    try {
      const t = showToast("hover", "info", document, 1000);
      t.dispatchEvent(new Event("mouseenter"));
      vi.advanceTimersByTime(5000);
      expect(t.isConnected).toBe(true); // paused — survives past the ttl
      t.dispatchEvent(new Event("mouseleave"));
      vi.advanceTimersByTime(1000);
      expect(t.isConnected).toBe(false); // resumed
    } finally {
      vi.useRealTimers();
    }
  });

  it("auto-removes itself after the ttl", () => {
    vi.useFakeTimers();
    try {
      const t = showToast("bye", "info", document, 1000);
      expect(t.isConnected).toBe(true);
      vi.advanceTimersByTime(1000);
      expect(t.isConnected).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("mounts into an injected Document, not the global one", () => {
    const otherDoc = document.implementation.createHTMLDocument("other");
    showToast("scoped", "info", otherDoc);
    expect(otherDoc.querySelector(".toasts")?.children.length).toBe(1);
    expect(document.querySelector(".toasts")).toBeNull();
  });
});
