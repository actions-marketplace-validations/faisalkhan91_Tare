import { describe, it, expect } from "vitest";
import { skelCard, skelCardRow, skelRows, skelScreen } from "../src/ui/skeleton.js";

describe("loading skeletons", () => {
  it("builds shaped placeholders, not text", () => {
    expect(skelCard().classList.contains("skeleton-block")).toBe(true);
    expect(skelCardRow(4).querySelectorAll(".skeleton-block").length).toBe(4);
    expect(skelRows(5).querySelectorAll(".skel-row").length).toBe(5);
  });

  it("skelScreen mirrors a dashboard layout (a card row + rows) and marks aria-busy", () => {
    const s = skelScreen();
    expect(s.getAttribute("aria-busy")).toBe("true");
    expect(s.getAttribute("role")).toBe("status");
    expect(s.getAttribute("aria-label")).toBe("Loading content");
    expect(s.querySelector(".cards .skeleton-block")).toBeTruthy();
    expect(s.querySelectorAll(".skel-row").length).toBeGreaterThan(0);
  });
});
