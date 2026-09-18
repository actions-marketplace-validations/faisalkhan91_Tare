// Command focus scopes + context. The ONE place that decides whether an
// unscoped single-letter shortcut is allowed to fire: never while a text field is focused. Both the
// hotkey router and the ":"/command-key surface import `isEditableTarget` so the guard can't drift
// between them (previously duplicated). `CommandContext` is what the registry reads to decide which
// contextual (selection) commands to offer, and in what order.

/// True when the event target is a text-editing surface (input/textarea/contenteditable), where
/// unscoped single-letter keys (goto chords, and later Space/X) must NOT be intercepted.
export function isEditableTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.tagName !== "string") return false;
  if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") return true;
  // `isContentEditable` is the reliable signal in a real browser; also accept the reflected
  // `contentEditable` property so a detached/unrendered node (e.g. under jsdom) is still recognized.
  return el.isContentEditable === true || el.contentEditable === "true";
}

/// A selected/pinned entity the contextual commands act on (mirrors the shape of `UiEntityRef` without
/// coupling the command layer to the analysis module).
export interface CommandSelection {
  kind: "run" | "step" | "session" | "template" | "cohort";
  id: string;
  label?: string;
}

/// The context a command registry reads. `selection` drives the contextual commands that
/// appear BEFORE navigation/global ones; more context (focus pane, workspace) can join later.
export interface CommandContext {
  selection: CommandSelection | null;
}
