// Unified command registry. ONE source of commands for the palette, the
// rail Commands row, context menus, and the native menus — so an action has a single stable id and a
// single implementation across every surface. Commands are grouped, and the group order guarantees
// the interaction contract: contextual (selection) commands appear BEFORE navigation, which precede global
// actions; destructive actions never reorder by frecency (this registry does no frecency sort at all).

import type { Command } from "../ui/palette.js";
import type { CommandContext } from "./contexts.js";

export type CommandGroup = "context" | "primary" | "saved" | "recent" | "actions" | "advanced";

/// A registry command = a palette `Command` tagged with its group (drives ordering + surfacing).
export interface RegistryCommand extends Command {
  group: CommandGroup;
}

/// The one true group order: selection first, then navigation, then global/actions.
export const GROUP_ORDER: readonly CommandGroup[] = [
  "context",
  "primary",
  "saved",
  "recent",
  "actions",
  "advanced",
];

export const GROUP_LABEL: Record<CommandGroup, string> = {
  context: "Context",
  primary: "Primary navigation",
  saved: "Saved views",
  recent: "Recent runs",
  actions: "Actions",
  advanced: "Advanced",
};

/// Stable action-id scheme shared with the native menus (`nav:<route>` / `action:<name>` — the exact
/// ids `bootTauri` dispatches from the macOS menu and `gui.rs` emits), so the web surfaces and the OS
/// menu invoke the SAME registry entry. One place, no drift.
export const navId = (route: string): string => `nav:${route}`;
export const runId = (id: string): string => `run:${id}`;
export const ACTION = {
  openPalette: "action:open-palette",
  settings: "action:settings",
  saveView: "action:save-view",
  proxy: "action:proxy",
  compare: "action:compare",
  find: "action:find",
} as const;

/// Order commands by group (stable within a group, preserving caller order — so navigation keeps its
/// rail order and runs keep recency). Contextual selection commands always lead.
export function orderCommands(cmds: RegistryCommand[]): RegistryCommand[] {
  return cmds
    .map((c, i) => ({ c, i }))
    .sort((a, b) => {
      const g = GROUP_ORDER.indexOf(a.c.group) - GROUP_ORDER.indexOf(b.c.group);
      return g !== 0 ? g : a.i - b.i; // stable within group
    })
    .map((x) => ({ ...x.c, section: GROUP_LABEL[x.c.group] }));
}

/// The contextual (selection) commands for the current selection, or `[]` when nothing is selected.
/// These lead the palette so the most relevant action is first.
export function selectionCommands(
  ctx: CommandContext,
  handlers: { open: (id: string) => void; compare: (id: string) => void }
): RegistryCommand[] {
  const sel = ctx.selection;
  if (!sel) return [];
  const noun = sel.label ?? `${sel.kind} ${sel.id}`;
  const out: RegistryCommand[] = [
    { id: `${runId(sel.id)}:open`, title: `Open ${noun}`, run: () => handlers.open(sel.id), group: "context" },
  ];
  if (sel.kind === "run" || sel.kind === "step") {
    out.push({
      id: `${runId(sel.id)}:compare`,
      title: `Compare ${noun}`,
      run: () => handlers.compare(sel.id),
      group: "context",
    });
  }
  return out;
}
