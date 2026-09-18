// Workspace status bar. Replaces the old persistent Today/burn tape. Its
// aria-label is "Workspace status" and it shows ONLY: capture/connection state, the SCOPED result
// count + spend, data freshness, and any trust/provenance warning. It never shows an unrelated Today
// total — Pulse owns the primary current/period spend figure; other workspaces set their active
// cohort's compact scoped spend via `setScope`. Kept as `<footer class="statusbar">` so shared shell
// styling (e.g. the desktop no-select region) still applies.

import { fmtUsd } from "../ui/format.js";

export interface WorkspaceStatusBar {
  /// The `<footer>` element to mount in the shell.
  readonly el: HTMLElement;
  /// Connection/capture state: `true` live, `false` retrying, `null` connecting.
  setConnection(state: boolean | null): void;
  /// The active cohort's scoped result count + spend, or `null` to clear (no unrelated total).
  setScope(scope: { count?: number; spendMicros?: number } | null): void;
  /// Data freshness line (announced politely), e.g. "Updated 14:03".
  setFreshness(text: string): void;
  /// A trust/provenance warning, or `null` to hide it.
  setWarning(text: string | null): void;
}

/// A best-effort local wall-clock label for the status bar; falls back to a neutral phrase if the
/// environment has no locale time formatting.
export function timeLabel(): string {
  try {
    return new Date().toLocaleTimeString();
  } catch {
    return "just now";
  }
}

/// Build the workspace status bar. `mk` creates elements (injected so this module stays independent of
/// the shell's `el` helper import path); callers pass their `el`.
export function createStatusBar(
  mk: (
    tag: string,
    attrs?: Record<string, string>,
    children?: (Node | string)[]
  ) => HTMLElement
): WorkspaceStatusBar {
  const dot = mk("span", {
    class: "status-dot",
    role: "img",
    "aria-label": "Connecting to local service",
    title: "Connecting to local service",
  });
  const connText = mk("span", { class: "conn-text" });
  connText.textContent = "Connecting to local service";
  const conn = mk("span", { class: "conn-status" }, [dot, connText]);

  // Scoped result count + spend for the active cohort (empty until a workspace sets it).
  const scope = mk("span", { class: "status-scope" });

  const warning = mk("span", { class: "status-warning" });
  warning.setAttribute("hidden", "");

  const updated = mk("span", { class: "status-updated", "aria-live": "polite" });

  const el = mk(
    "footer",
    { class: "statusbar", "aria-label": "Workspace status" },
    [conn, scope, mk("span", { class: "spacer" }), warning, updated]
  );

  return {
    el,
    setConnection(state) {
      dot.classList.toggle("ok", state === true);
      const label =
        state === true
          ? "Local service connected"
          : state === false
            ? "Local service unavailable, retrying"
            : "Connecting to local service";
      dot.setAttribute("aria-label", label);
      dot.setAttribute("title", label);
      connText.textContent =
        state === true
          ? "Local service connected"
          : state === false
            ? "Local service retrying"
            : "Connecting to local service";
    },
    setScope(s) {
      if (!s || (s.count === undefined && s.spendMicros === undefined)) {
        scope.textContent = "";
        scope.removeAttribute("title");
        return;
      }
      const parts: string[] = [];
      if (s.count !== undefined) parts.push(`${s.count.toLocaleString()} ${s.count === 1 ? "run" : "runs"}`);
      if (s.spendMicros !== undefined) parts.push(fmtUsd(s.spendMicros));
      scope.textContent = parts.join(" · ");
      scope.title = "Scoped to the active cohort";
    },
    setFreshness(text) {
      updated.textContent = text;
    },
    setWarning(text) {
      if (text) {
        warning.textContent = text;
        warning.removeAttribute("hidden");
      } else {
        warning.textContent = "";
        warning.setAttribute("hidden", "");
      }
    },
  };
}
