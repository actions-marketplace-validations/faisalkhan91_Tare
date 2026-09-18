import { describe, it, expect } from "vitest";
import { addDays, rangeToWindow, RANGE_OPTIONS } from "../src/ui/range.js";
import { rangePref, setRangePref } from "../src/ui/prefs.js";

describe("addDays", () => {
  it("does pure UTC date arithmetic across month/year boundaries", () => {
    expect(addDays("2026-06-30", -6)).toBe("2026-06-24");
    expect(addDays("2026-03-01", -1)).toBe("2026-02-28");
    expect(addDays("2024-03-01", -1)).toBe("2024-02-29"); // leap year
    expect(addDays("2026-01-01", -1)).toBe("2025-12-31");
  });
});

describe("rangeToWindow", () => {
  const TO = "2026-06-30";
  it("anchors 7/30/90-day windows inclusively on the data's last day", () => {
    expect(rangeToWindow({ key: "7d" }, TO)).toEqual({ from: "2026-06-24", to: TO });
    expect(rangeToWindow({ key: "30d" }, TO)).toEqual({ from: "2026-06-01", to: TO });
    expect(rangeToWindow({ key: "90d" }, TO)).toEqual({ from: "2026-04-02", to: TO });
  });
  it("passes custom dates through, falling back to the anchor when unset", () => {
    expect(rangeToWindow({ key: "custom", from: "2026-05-01", to: "2026-05-15" }, TO)).toEqual({
      from: "2026-05-01",
      to: "2026-05-15",
    });
    expect(rangeToWindow({ key: "custom" }, TO)).toEqual({ from: TO, to: TO });
  });
  it("exposes the four picker options", () => {
    expect(RANGE_OPTIONS.map((o) => o.key)).toEqual(["7d", "30d", "90d", "custom"]);
  });
});

describe("rangePref", () => {
  it("defaults to 30d, round-trips, and tolerates corrupt JSON", () => {
    localStorage.removeItem("tare-range");
    expect(rangePref()).toEqual({ key: "30d" });
    setRangePref({ key: "custom", from: "2026-06-01", to: "2026-06-10" });
    expect(rangePref()).toEqual({ key: "custom", from: "2026-06-01", to: "2026-06-10" });
    setRangePref({ key: "7d" });
    expect(rangePref()).toEqual({ key: "7d", from: undefined, to: undefined });
    localStorage.setItem("tare-range", "{bad json");
    expect(rangePref()).toEqual({ key: "30d" });
    localStorage.removeItem("tare-range");
  });
});
