// Accessible utility-sheet primitive. The workspace remains mounted underneath while
// the sheet is open; native controls keep normal keyboard behavior, Escape closes, and Tab is
// contained within the modal surface. Route ownership stays with shell/workbench.

import { el } from "./el.js";
import { afterTransition, beginMotion } from "./motion.js";

let nextSheetId = 0;

export interface UtilitySheet {
  backdrop: HTMLElement;
  dialog: HTMLElement;
  body: HTMLElement;
  focus(): void;
}

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), details > summary, [tabindex]:not([tabindex="-1"])';

export function createUtilitySheet(title: string, onClose: () => void): UtilitySheet {
  const id = `utility-sheet-title-${++nextSheetId}`;
  let closed = false;
  const close = (): void => {
    if (closed) return;
    closed = true;
    backdrop.setAttribute("data-motion-state", "closing");
    afterTransition(dialog, "transform", onClose);
  };
  const closeButton = el("button", {
    class: "btn ghost utility-sheet-close",
    type: "button",
    text: "Close",
    "aria-label": `Close ${title}`,
    onClick: close,
  });
  const body = el("div", { class: "utility-sheet-body" });
  const dialog = el(
    "section",
    {
      class: "utility-sheet",
      role: "dialog",
      "aria-modal": "true",
      "aria-labelledby": id,
      tabindex: "-1",
    },
    [
      el("header", { class: "utility-sheet-header" }, [el("h1", { id, text: title }), closeButton]),
      body,
    ]
  );
  const backdrop = el("div", {
    class: "utility-sheet-backdrop",
    "data-motion-state": "opening",
  }, [dialog]);

  backdrop.addEventListener("click", (event) => {
    if (event.target === backdrop) close();
  });
  dialog.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      close();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (node) =>
        !node.hasAttribute("hidden") &&
        node.getAttribute("aria-hidden") !== "true" &&
        !node.closest("[hidden]") &&
        !node.closest("details:not([open]) :not(summary)")
    );
    if (focusable.length === 0) {
      event.preventDefault();
      dialog.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  });

  beginMotion(backdrop, "data-motion-state");

  return {
    backdrop,
    dialog,
    body,
    focus: () => closeButton.focus(),
  };
}
