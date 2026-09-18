import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { DESKTOP_COMMANDS } from "../e2e/tauriStub.js";

// Static, storm-free guard on the JS↔Rust IPC seam (tare desktop-testing layer). Nothing else parses
// the Rust command registry from the JS side, so this is the cheapest check that the three definitions
// of the seam can't silently drift:
//   1. tauriClient.ts   — every command the desktop client actually invoke()s.
//   2. gui.rs           — the tauri::generate_handler![…] registry (the commands Rust exposes).
//   3. tauriStub.ts     — DESKTOP_COMMANDS, the served-twin stub's command map.
// It fails CI the moment the client names a command Rust doesn't register, or the stub is missing a
// command the client invokes. It replaces an older live-backend-dependent contract script.

const clientSrc = readFileSync(resolve(process.cwd(), "src/tauriClient.ts"), "utf8");
const guiSrc = readFileSync(resolve(process.cwd(), "../tare-tauri/src/gui.rs"), "utf8");

/// Commands the desktop client invokes: direct `invoke("cmd", …)` (object/void/raw returns) and the
/// `j<T>("cmd", …)` helper (JSON-string returns), incl. nested generics like j<AnalysisResponse<…>>.
function clientCommands(src: string): Set<string> {
  const cmds = new Set<string>();
  for (const m of src.matchAll(/\binvoke\(\s*"([a-z_]+)"/g)) cmds.add(m[1]);
  for (const m of src.matchAll(/\bj<[^(]*\(\s*"([a-z_]+)"/g)) cmds.add(m[1]);
  return cmds;
}

/// The commands registered in gui.rs's single `tauri::generate_handler![…]` block.
function registeredCommands(src: string): Set<string> {
  const block = src.match(/generate_handler!\s*\[([\s\S]*?)\]/);
  expect(block, "gui.rs must contain a tauri::generate_handler![…] block").toBeTruthy();
  const cmds = new Set<string>();
  for (const raw of block![1].split(/[\s,]+/)) {
    const id = raw.trim();
    if (/^[a-z_]+$/.test(id)) cmds.add(id);
  }
  return cmds;
}

describe("desktop IPC seam contract (tauriClient ↔ gui.rs ↔ tauriStub)", () => {
  const client = clientCommands(clientSrc);
  const registry = registeredCommands(guiSrc);
  const stub = new Set(Object.keys(DESKTOP_COMMANDS));

  it("extracts a plausible command surface from all three sources", () => {
    // Guards against a silently-broken extraction (e.g. a regex that stops matching) masking a drift.
    expect(client.size, "tauriClient invoke() surface").toBeGreaterThanOrEqual(80);
    expect(registry.size, "gui.rs generate_handler! registry").toBeGreaterThanOrEqual(80);
    expect(stub.size, "tauriStub DESKTOP_COMMANDS").toBeGreaterThanOrEqual(80);
  });

  it("registers every command the desktop client invokes (JS can't name a command Rust lacks)", () => {
    const missing = [...client].filter((c) => !registry.has(c)).sort();
    expect(missing, `commands invoked by tauriClient.ts but not in gui.rs generate_handler![]`).toEqual([]);
  });

  it("covers every client command in the served-twin stub (or the desktop spec renders blank)", () => {
    const missing = [...client].filter((c) => !stub.has(c)).sort();
    expect(missing, `commands invoked by tauriClient.ts but missing from tauriStub DESKTOP_COMMANDS`).toEqual([]);
  });

  it("does not invent stub commands Rust never registered", () => {
    const extra = [...stub].filter((c) => !registry.has(c)).sort();
    expect(extra, `DESKTOP_COMMANDS entries with no matching gui.rs command`).toEqual([]);
  });
});
