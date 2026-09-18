import { describe, it, expect, vi } from "vitest";
import { errorNode } from "../src/ui/errorNode.js";

describe("errorNode", () => {
  it("shows a plain-language line and demotes the raw error to console + title", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    const raw = new Error("ECONNREFUSED 127.0.0.1:8788");
    const node = errorNode("Couldn't load runs. Retry.", raw);
    // Primary user text = the plain message, never the raw exception.
    expect(node.textContent).toBe("Couldn't load runs. Retry.");
    expect(node.textContent).not.toContain("ECONNREFUSED");
    // Raw error is available on hover + in the console for debugging.
    expect(node.getAttribute("title")).toContain("ECONNREFUSED");
    expect(spy).toHaveBeenCalledWith("Couldn't load runs. Retry.", raw);
    spy.mockRestore();
  });

  it("omits console/title when no raw error is given", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    const node = errorNode("Nothing to show.");
    expect(node.textContent).toBe("Nothing to show.");
    expect(node.getAttribute("title")).toBeNull();
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });

  it("offers real retry and back controls when recovery actions are supplied", async () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    let retries = 0;
    const node = errorNode("Couldn't load this view.", new Error("offline"), {
      actions: [
        { label: "Retry", primary: true, run: () => { retries += 1; } },
        { label: "Back to Pulse", href: "#/pulse" },
      ],
    });
    (node.querySelector("button") as HTMLButtonElement).click();
    await Promise.resolve();
    expect(retries).toBe(1);
    expect(node.querySelector("a")?.getAttribute("href")).toBe("#/pulse");
    spy.mockRestore();
  });
});
