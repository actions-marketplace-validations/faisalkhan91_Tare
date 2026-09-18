// A user-facing error line: the PRIMARY text is a plain-language message describing
// the effect (and, where useful, the fix) — never a raw `${String(e)}` exception. The raw error is
// demoted to the console (for debugging) and the element's `title` (revealed on hover), so users
// see language they can act on while the detail stays one hover / one devtools-open away.
import { el } from "./el.js";
export function errorNode(message, raw, options = {}) {
    if (raw !== undefined)
        console.error(message, raw);
    const attrs = { class: "error", text: message, role: "alert" };
    if (raw !== undefined)
        attrs.title = String(raw);
    const line = el("p", attrs);
    if (!options.actions?.length)
        return line;
    const controls = options.actions.map((action) => {
        const className = action.primary ? "btn primary" : "btn ghost";
        if (action.href)
            return el("a", { class: className, href: action.href, text: action.label });
        const button = el("button", { class: className, type: "button", text: action.label });
        button.addEventListener("click", () => {
            if (!action.run || button.disabled)
                return;
            const original = button.textContent ?? action.label;
            button.disabled = true;
            button.textContent = action.label.toLowerCase().startsWith("retry") ? "Retrying…" : `${action.label}…`;
            void Promise.resolve(action.run()).catch(() => {
                button.disabled = false;
                button.textContent = original;
            });
        });
        return button;
    });
    return el("div", { class: "error-state" }, [line, el("div", { class: "error-actions" }, controls)]);
}
