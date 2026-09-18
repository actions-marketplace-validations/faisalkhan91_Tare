// Command palette (⌘K / Ctrl-K): the density-release valve. Fuzzy-jump to any destination, open a
// recent run, or run an action (toggle theme, start/stop proxy, compare runs). Pure DOM +
// keyboard; framework-free and jsdom-testable. The hotkey + command list are wired by the shell.

import { el } from "./el.js";
import { matchShortcut } from "./keymap.js";
import { osClass, type OsClass } from "./os.js";

export interface Command {
  id: string;
  title: string;
  run: () => void | Promise<void>;
  /// Optional keyboard-shortcut hint shown right-aligned in the row (e.g. "g o"), so ⌘K teaches the
  /// chords it mirrors.
  hint?: string;
  /// Consecutive commands with the same section render as one labelled group.
  section?: string;
  /// Secondary context such as a saved scope or recent run model/date.
  subtitle?: string;
  /// Search aliases that need not be shown in the row.
  keywords?: string;
}

/// Case-insensitive subsequence match ("orun" matches "Open run").
export function fuzzyMatch(query: string, text: string): boolean {
  const q = query.toLowerCase().trim();
  if (!q) return true;
  const t = text.toLowerCase();
  let i = 0;
  for (const ch of t) {
    if (ch === q[i]) i++;
    if (i === q.length) return true;
  }
  return false; // loop only falls through when the text was exhausted before matching all of q
}

export function filterCommands(cmds: Command[], query: string): Command[] {
  return cmds.filter((c) =>
    fuzzyMatch(query, [c.title, c.subtitle, c.section, c.keywords].filter(Boolean).join(" "))
  );
}

/// Open the palette over `commands`. Returns the overlay element (removed on close/activate).
export function openPalette(commands: Command[], doc: Document = document): HTMLElement {
  doc.querySelector(".palette-overlay")?.remove();
  // Remember who opened us so focus returns there on close (a11y: focus shouldn't fall to <body>).
  const opener = doc.activeElement as HTMLElement | null;
  let items = commands;
  let active = 0;

  const listboxId = "palette-listbox";
  const optId = (i: number): string => `palette-opt-${i}`;
  const input = el("input", {
    class: "palette-input",
    type: "text",
    placeholder: "Jump to a view, run, or action…",
    role: "combobox",
    "aria-expanded": "true",
    "aria-controls": listboxId,
    "aria-label": "Command palette",
  }) as HTMLInputElement;
  const list = el("div", { class: "palette-list", role: "listbox", id: listboxId });
  const panel = el("div", { class: "palette" }, [input, list]);
  // A real modal dialog: aria-modal + a name so assistive tech announces and scopes it.
  const overlay = el("div", {
    class: "palette-overlay",
    role: "dialog",
    "aria-modal": "true",
    "aria-label": "Command palette",
  }, [panel]);

  function render(): void {
    items = filterCommands(commands, input.value);
    if (active >= items.length) active = Math.max(0, items.length - 1);
    const groups: HTMLElement[] = [];
    let currentSection: string | undefined;
    let group: HTMLElement | undefined;
    items.forEach((c, i) => {
      if (!group || c.section !== currentSection) {
        currentSection = c.section;
        group = el("div", {
          class: "palette-group",
          role: "group",
          ...(currentSection ? { "aria-label": currentSection } : {}),
        });
        if (currentSection) {
          group.appendChild(el("p", { class: "palette-group-label", text: currentSection, "aria-hidden": "true" }));
        }
        groups.push(group);
      }
      group.appendChild(
        el(
          "div",
          {
            class: i === active ? "palette-item active" : "palette-item",
            role: "option",
            id: optId(i),
            "aria-selected": i === active ? "true" : "false",
            onClick: () => activate(i),
          },
          [
            el("span", { class: "palette-item-copy" }, [
              el("span", { class: "palette-item-title", text: c.title }),
              ...(c.subtitle ? [el("span", { class: "palette-item-subtitle", text: c.subtitle })] : []),
            ]),
            ...(c.hint ? [el("kbd", { class: "kbd-hint palette-item-hint", text: c.hint })] : []),
          ]
        )
      );
    });
    if (groups.length === 0) {
      groups.push(el("p", { class: "palette-empty", role: "status", text: "No matching commands" }));
    }
    list.replaceChildren(...groups);
    // Point the combobox at the active option so SR users hear the highlighted row.
    if (items.length) {
      input.setAttribute("aria-activedescendant", optId(active));
      // Keep the highlighted row visible when the list overflows. Guarded: jsdom (tests)
      // has no scrollIntoView, and it's a no-op cosmetic anyway.
      list.querySelector<HTMLElement>(`#${optId(active)}`)?.scrollIntoView?.({ block: "nearest" });
    } else {
      input.removeAttribute("aria-activedescendant");
    }
  }
  function close(): void {
    overlay.remove();
    // Restore focus to whatever was focused before the palette opened.
    if (opener && typeof opener.focus === "function" && opener.isConnected) opener.focus();
  }
  function activate(i: number): void {
    const c = items[i];
    close();
    if (c) void c.run();
  }

  input.addEventListener("input", render);
  overlay.addEventListener("keydown", (e: Event) => {
    const ev = e as KeyboardEvent;
    if (ev.key === "Escape") {
      ev.preventDefault();
      close();
    } else if (ev.key === "ArrowDown") {
      ev.preventDefault();
      active = Math.min(items.length - 1, active + 1);
      render();
    } else if (ev.key === "ArrowUp") {
      ev.preventDefault();
      active = Math.max(0, active - 1);
      render();
    } else if (ev.key === "Home") {
      ev.preventDefault();
      active = 0;
      render();
    } else if (ev.key === "End") {
      ev.preventDefault();
      active = Math.max(0, items.length - 1);
      render();
    } else if (ev.key === "Enter") {
      ev.preventDefault();
      activate(active);
    } else if (ev.key === "Tab") {
      // Focus trap: the only focusable control is the input, so keep focus here rather than
      // letting Tab escape the modal into the shell behind it.
      ev.preventDefault();
      try {
        input.focus();
      } catch {
        /* jsdom focus may no-op */
      }
    }
  });
  overlay.addEventListener("click", (e: Event) => {
    if (e.target === overlay) close();
  });

  doc.body.appendChild(overlay);
  render();
  try {
    input.focus();
  } catch {
    /* jsdom focus may no-op */
  }
  return overlay;
}

/// Install the palette summon hotkey (logical `Mod+K` — ⌘K on macOS, Ctrl-K elsewhere); returns an
/// unsubscribe. Routing through the Mod token means Ctrl-K on macOS is left to the
/// native readline chord instead of hijacking the palette. `os` is injectable for tests.
export function installPaletteHotkey(
  open: () => void,
  win: Window = window,
  os: OsClass = osClass(typeof navigator !== "undefined" ? navigator.userAgent : "")
): () => void {
  const handler = (e: KeyboardEvent) => {
    if (matchShortcut(e, "Mod+K", os)) {
      e.preventDefault();
      open();
    }
  };
  win.addEventListener("keydown", handler as EventListener);
  return () => win.removeEventListener("keydown", handler as EventListener);
}
