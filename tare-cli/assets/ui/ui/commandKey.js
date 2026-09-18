// ":" opens the command surface — a terminal-style docked-command entry point that
// AUGMENTS, never replaces, ⌘K / Ctrl-K (both open the same palette). Ignored while typing in a text
// field (so ":" types normally) and when a Cmd/Ctrl/Alt modifier is held (Shift is required to type ":"
// on most layouts, so it is deliberately allowed). Framework-free, jsdom-testable.
import { isEditableTarget } from "../commands/contexts.js";
export function installCommandKey(open, win = window) {
    const handler = (e) => {
        if (e.key !== ":" || e.metaKey || e.ctrlKey || e.altKey)
            return;
        if (isEditableTarget(e.target))
            return; // central focus-scope guard
        e.preventDefault();
        open();
    };
    win.addEventListener("keydown", handler);
    return () => win.removeEventListener("keydown", handler);
}
