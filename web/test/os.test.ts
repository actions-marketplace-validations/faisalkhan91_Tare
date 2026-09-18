import { describe, it, expect } from "vitest";
import { osClass, applyOsClass, preferredMaterial } from "../src/ui/os.js";
import { consumeShellInit } from "../src/bootTauri.js";

describe("osClass", () => {
  it("classifies the common desktop userAgents", () => {
    expect(osClass("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")).toBe("macos");
    expect(osClass("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")).toBe("windows");
    expect(osClass("Mozilla/5.0 (X11; Linux x86_64)")).toBe("linux");
    expect(osClass("Mozilla/5.0 (Unknown)")).toBe("other");
  });

  it("applyOsClass stamps data-os from UA when none was injected (browser transport)", () => {
    const mac = document.createElement("html");
    expect(applyOsClass("Mozilla/5.0 (Macintosh; Intel Mac OS X)", mac)).toBe("macos");
    expect(mac.dataset.os).toBe("macos");
    const win = document.createElement("html");
    expect(applyOsClass("Mozilla/5.0 (Windows NT 10.0)", win)).toBe("windows");
    expect(win.dataset.os).toBe("windows");
  });

  it("prefers an injected data-os (native init-script) over UA sniffing", () => {
    const root = document.createElement("html");
    root.dataset.os = "windows"; // authoritative, injected before boot
    // A macOS UA must NOT override the injected value.
    expect(applyOsClass("Mozilla/5.0 (Macintosh; Intel Mac OS X)", root)).toBe("windows");
    expect(root.dataset.os).toBe("windows");
  });

  it("stamps data-material=flat as the pre-paint default but never overwrites a Rust-injected value", () => {
    const root = document.createElement("html");
    applyOsClass("Mozilla/5.0 (Macintosh; Intel Mac OS X)", root);
    expect(root.dataset.material).toBe("flat");

    // A shell that already confirmed vibrancy/mica (via initialization_script) must be preserved.
    const pre = document.createElement("html");
    pre.dataset.material = "vibrancy";
    applyOsClass("Mozilla/5.0 (Macintosh; Intel Mac OS X)", pre);
    expect(pre.dataset.material).toBe("vibrancy");
  });

  it("preferredMaterial: vibrancy only on macOS + Tauri without Reduce Transparency", () => {
    expect(preferredMaterial("macos", true, false)).toBe("vibrancy");
    expect(preferredMaterial("macos", true, true)).toBe("flat"); // Reduce Transparency → flat
    expect(preferredMaterial("macos", false, false)).toBe("flat"); // browser (no window material)
    expect(preferredMaterial("windows", true, false)).toBe("flat"); // Mica hardware-gated
    expect(preferredMaterial("linux", true, false)).toBe("flat"); // flat by design
    expect(preferredMaterial("other", true, false)).toBe("flat");
  });

  it("consumeShellInit applies the injected traffic-light reserve", () => {
    const root = document.createElement("html");
    (globalThis as unknown as { __TARE_SHELL_INIT__?: unknown }).__TARE_SHELL_INIT__ = {
      os: "macos",
      material: "vibrancy",
      traffic_light_reserve_px: 78,
    };
    consumeShellInit(root);
    expect(root.style.getPropertyValue("--traffic-light-reserve")).toBe("78px");
    // No snapshot (browser transport) → leaves the CSS default (desktop.css 96px) untouched.
    const bare = document.createElement("html");
    delete (globalThis as unknown as { __TARE_SHELL_INIT__?: unknown }).__TARE_SHELL_INIT__;
    consumeShellInit(bare);
    expect(bare.style.getPropertyValue("--traffic-light-reserve")).toBe("");
  });
});
