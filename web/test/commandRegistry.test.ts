// Unified command registry, focus-scope guard, and native/rail action-id parity.

import { describe, it, expect } from "vitest";
import {
  ACTION,
  navId,
  runId,
  orderCommands,
  selectionCommands,
  GROUP_ORDER,
  type RegistryCommand,
} from "../src/commands/registry.js";
import { isEditableTarget, type CommandContext } from "../src/commands/contexts.js";

function cmd(id: string, group: RegistryCommand["group"]): RegistryCommand {
  return { id, title: id, run: () => {}, group };
}

describe("command ordering", () => {
  it("orders the six visible palette sections from context through advanced lenses", () => {
    const shuffled: RegistryCommand[] = [
      cmd("lens:units", "advanced"),
      cmd("action:x", "actions"),
      cmd("run:1", "recent"),
      cmd("view:1", "saved"),
      cmd("nav:pulse", "primary"),
      cmd("sel:1", "context"),
    ];
    expect(orderCommands(shuffled).map((c) => c.group)).toEqual([
      "context",
      "primary",
      "saved",
      "recent",
      "actions",
      "advanced",
    ]);
    expect(GROUP_ORDER).toEqual(["context", "primary", "saved", "recent", "actions", "advanced"]);
    expect(orderCommands(shuffled).map((c) => c.section)).toEqual([
      "Context",
      "Primary navigation",
      "Saved views",
      "Recent runs",
      "Actions",
      "Advanced",
    ]);
  });

  it("is stable within a group (no frecency reordering of destructive/nav actions)", () => {
    const navs: RegistryCommand[] = [
      cmd("nav:a", "primary"),
      cmd("nav:b", "primary"),
      cmd("nav:c", "primary"),
    ];
    expect(orderCommands(navs).map((c) => c.id)).toEqual(["nav:a", "nav:b", "nav:c"]);
  });
});

describe("contextual selection commands", () => {
  const handlers = { open: () => {}, compare: () => {} };
  it("are empty when nothing is selected", () => {
    const ctx: CommandContext = { selection: null };
    expect(selectionCommands(ctx, handlers)).toEqual([]);
  });
  it("offer open + compare for a run, all in the context group (so they lead)", () => {
    const ctx: CommandContext = { selection: { kind: "run", id: "run-42", label: "Run 42" } };
    const cmds = selectionCommands(ctx, handlers);
    expect(cmds.every((c) => c.group === "context")).toBe(true);
    expect(cmds.map((c) => c.title)).toEqual(["Open Run 42", "Compare Run 42"]);
  });
  it("offer only open for a session (compare is run/step-grain)", () => {
    const ctx: CommandContext = { selection: { kind: "session", id: "s1" } };
    expect(selectionCommands(ctx, handlers).map((c) => c.title)).toEqual(["Open session s1"]);
  });
});

describe("native / rail action-id parity", () => {
  it("navigation ids use the native nav:<route> scheme (gui.rs NAV_DESTINATIONS)", () => {
    expect(navId("overview")).toBe("nav:overview");
    expect(navId("optimize")).toBe("nav:optimize");
    expect(runId("run-a")).toBe("run:run-a");
  });
  it("global action ids carry the action names the native menu bridges", () => {
    // bootTauri re-dispatches the native "open-palette" event; the rail/palette action carries the
    // same logical name so both surfaces invoke one registry entry. Theme is no longer a command,
    // so there is no toggle-theme action id.
    expect(ACTION.openPalette).toBe("action:open-palette");
    expect(ACTION.openPalette.slice("action:".length)).toBe("open-palette");
    expect("toggleTheme" in ACTION).toBe(false);
  });
});

describe("central focus-scope guard", () => {
  it("treats text-editing surfaces as editable, other elements as not", () => {
    const input = document.createElement("input");
    const textarea = document.createElement("textarea");
    const div = document.createElement("div");
    const editableDiv = document.createElement("div");
    editableDiv.contentEditable = "true";
    const button = document.createElement("button");
    expect(isEditableTarget(input)).toBe(true);
    expect(isEditableTarget(textarea)).toBe(true);
    expect(isEditableTarget(editableDiv)).toBe(true);
    expect(isEditableTarget(div)).toBe(false);
    expect(isEditableTarget(button)).toBe(false);
    expect(isEditableTarget(null)).toBe(false);
  });
});
