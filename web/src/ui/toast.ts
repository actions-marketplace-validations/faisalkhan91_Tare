// In-app toasts: transient notices in a fixed corner stack. Pure DOM; auto-dismiss. The toast
// host is created lazily so any screen can post without shell coupling.
//
// Semantics: budget-over + anomaly toasts are ASSERTIVE alerts (`role=alert` /
// `aria-live=assertive`) so a screen reader interrupts for them; warn/info stay polite (`role=status`).
// Every toast has a manual close and pauses its auto-dismiss while hovered, and the whole stack sits
// above the ⌘K palette scrim so an alert is never hidden behind it.

import { el } from "./el.js";

export function showToast(
  message: string,
  level: "warn" | "over" | "anomaly" | "info" = "info",
  doc: Document = document,
  ttlMs = 6000
): HTMLElement {
  let host = doc.querySelector<HTMLElement>(".toasts");
  if (!host) {
    host = el("div", { class: "toasts" });
    doc.body.appendChild(host);
  }
  const assertive = level === "over" || level === "anomaly";
  const toast = el(
    "div",
    {
      class: `toast toast-${level}${assertive ? " toast-alert" : ""}`,
      role: assertive ? "alert" : "status",
      "aria-live": assertive ? "assertive" : "polite",
    },
    [
      el("span", { class: "toast-msg", text: message }),
      el("button", {
        class: "toast-close",
        type: "button",
        "aria-label": "Dismiss",
        text: "×",
        onClick: () => toast.remove(),
      }),
    ]
  );
  host.appendChild(toast);

  // Cap the visible stack: a burst of alerts — e.g. many spend anomalies detected on
  // first load — must not pile into a fixed-position wall covering the page. Keep the most recent
  // few and drop the oldest; each still auto-dismisses on its own.
  const MAX_TOASTS = 4;
  while (host.children.length > MAX_TOASTS) host.firstElementChild?.remove();

  // Auto-dismiss, paused while hovered so the user can read/act before it vanishes.
  let timer: ReturnType<typeof setTimeout> | undefined;
  const arm = (): void => {
    try {
      timer = setTimeout(() => toast.remove(), ttlMs);
    } catch {
      /* timers unavailable; leave it */
    }
  };
  const disarm = (): void => {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  };
  toast.addEventListener("mouseenter", disarm);
  toast.addEventListener("mouseleave", arm);
  arm();
  return toast;
}
