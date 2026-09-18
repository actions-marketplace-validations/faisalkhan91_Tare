// Rail Commands row. The single in-app menu surface: a footer row in the
// rail (above Capture/Settings) that opens the contextual command palette. Replaces the removed
// topbar command button and the Windows/Linux topbar hamburger — one surface, not three. Linux F10
// focuses it (wired in main.ts).

import { el } from "../ui/el.js";
import { icon } from "../ui/icon.js";
import { ACTION } from "./registry.js";

/// Build the rail "Commands" row. `chordLabel` is the OS-aware palette chord ("⌘K" / "Ctrl K") shown
/// as a hint so the row teaches the global shortcut it mirrors. The `data-action` carries the shared
/// registry id so the row and the native menu reference the same action.
export function commandsRailRow(onOpen: () => void, chordLabel: string): HTMLElement {
  return el(
    "button",
    {
      class: "nav-commands",
      type: "button",
      "data-action": ACTION.openPalette,
      title: `Open the command palette (${chordLabel})`,
      "aria-label": `Open the command palette (${chordLabel})`,
      "aria-keyshortcuts": "Meta+K Control+K F10",
      onClick: () => onOpen(),
    },
    [
      icon("commands", { size: 16, class: "nav-commands-icon" }),
      el("span", { class: "nav-commands-label", text: "Commands" }),
      el("span", { class: "nav-commands-hint", text: chordLabel }),
    ]
  );
}
