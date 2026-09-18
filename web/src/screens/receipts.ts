// Receipt verifier embedded in the Trust & pricing utility. It can include its own framing and
// pricing context when hosted independently in tests or another trusted surface.

import { el } from "../ui/el.js";
import { skelRows } from "../ui/skeleton.js";
import { emptyState } from "../ui/empty.js";
import { errorNode } from "../ui/errorNode.js";
import { lensSubtitle } from "../ui/lens.js";
import { receiptStatement } from "../ui/receiptStatement.js";
import type { TareClient } from "../client.js";

export interface ReceiptVerifierOptions {
  showLens?: boolean;
  showPricing?: boolean;
  initialRunId?: string;
}

/// Reusable receipt verifier. The Trust sheet supplies its own framing and pricing context; callers
/// may enable this component's preface when it is the primary surface.
export async function renderReceiptVerifier(
  root: HTMLElement,
  client: TareClient,
  options: ReceiptVerifierOptions = {}
): Promise<void> {
  const { showLens = true, showPricing = true, initialRunId } = options;
  root.replaceChildren(skelRows());

  // Pricing freshness is best-effort (non-critical). The run list is NOT: an outage must never be
  // conflated with genuinely-no-runs, which would render the empty state and falsely imply there's
  // nothing to attest; do not present missing data as $0.
  const pricing = showPricing ? await client.pricing().catch(() => null) : null;
  let runs: string[] | null = null;
  let runsError: unknown = null;
  try {
    runs = await client.listRuns();
  } catch (e) {
    runsError = e;
  }

  const frag = document.createDocumentFragment();
  if (showLens) {
    frag.appendChild(
      lensSubtitle("The bundled price book and a line-item cost receipt for any run. Every figure is estimated, never billed.")
    );
  }

  // Pricing freshness.
  if (showPricing && pricing) {
    frag.appendChild(
      el("section", { class: "section" }, [
        el("h2", {}, ["Pricing"]),
        el("p", { class: "caption" }, [
          `Version ${pricing.version}, effective ${pricing.effective_date}.`,
          pricing.note ? ` ${pricing.note}` : "",
        ]),
      ])
    );
  }

  const section = el("section", { class: "section" }, [
    el("h2", {}, ["Cost receipt"]),
    el("p", {
      class: "caption",
      text: "Transparent by construction. A recipient re-derives every figure offline with `tare verify`. No network and no trust in us. (It's a deterministic recomputation of an estimate, not an invoice.)",
    }),
  ]);

  if (runsError !== null) {
    // Load failure — NOT an empty history. Surface it inline; never fall through to the empty state.
    section.appendChild(
      errorNode(
        "Couldn't load runs. This is a load failure, not an empty history. Check that the capture service is running.",
        runsError,
        {
          actions: [
            {
              label: "Retry",
              primary: true,
              run: () => renderReceiptVerifier(root, client, options),
            },
          ],
        }
      )
    );
    frag.appendChild(section);
    root.replaceChildren(frag);
    return;
  }

  if (!runs || runs.length === 0) {
    section.appendChild(
      emptyState(
        "No runs to attest yet",
        "Capture a run, then create a recomputation receipt to share or verify its cost."
      )
    );
    frag.appendChild(section);
    root.replaceChildren(frag);
    return;
  }

  const sel = el(
    "select",
    {},
    runs.map((r) => el("option", { value: r, text: r }))
  ) as HTMLSelectElement;
  if (initialRunId && runs.includes(initialRunId)) sel.value = initialRunId;
  const priv = el("input", { type: "checkbox" }) as HTMLInputElement;
  const btn = el("button", { class: "btn", text: "Attest + verify" });
  const out = el("div");
  section.appendChild(
    el("div", { class: "diff-controls" }, [
      el("label", {}, ["Run ", sel]),
      el("label", { class: "toggle" }, [priv, " Max private (counts only, no content hashes)"]),
      btn,
    ])
  );
  section.appendChild(out);
  frag.appendChild(section);
  root.replaceChildren(frag);

  async function run(): Promise<void> {
    out.replaceChildren(el("p", { class: "skeleton", text: "Attesting + verifying…" }));
    let res;
    try {
      res = await client.receipt(sel.value, priv.checked);
    } catch (e) {
      out.replaceChildren(
        errorNode("Couldn't attest this run. You can retry or pick another run.", e, {
          actions: [{ label: "Retry", primary: true, run }],
        })
      );
      return;
    }
    const v = res.verify;
    // Typeset as an itemized recomputation statement, shared with the run-detail receipt.
    out.replaceChildren(
      receiptStatement(v),
      el("p", { class: "caption", text: `Scope: ${v.scope}. Re-derived offline from the bundled pricing — you just verified the number; no network required.` })
    );
  }

  btn.addEventListener("click", () => void run());
}
