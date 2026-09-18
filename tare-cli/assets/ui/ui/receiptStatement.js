// Recomputation receipt as a typeset STATEMENT: the honesty surface reads like an
// itemized ledger — mono, tabular figures, hairline-ruled label→value rows — reinforcing
// "recomputation, not a cryptographic seal": the arithmetic you can re-add by hand. Reusable across the
// inline run-detail receipt and the Receipts screen. Framework-free, jsdom-testable.
import { el } from "./el.js";
import { fmtUsd, toDollarString } from "./format.js";
function receiptRow(label, value, title) {
    return el("div", { class: "receipt-row" }, [
        el("dt", { text: label }),
        el("dd", { class: "num", text: value, title: title ?? "" }),
    ]);
}
export function receiptStatement(v) {
    return el("dl", { class: "receipt-ledger" }, [
        receiptRow("Recomputed total", fmtUsd(v.recomputed_total_micros), toDollarString(v.recomputed_total_micros)),
        receiptRow("Rows recomputed", String(v.rows)),
        receiptRow("Pricing", v.pricing_version),
        receiptRow("Flamegraph", v.flamegraph_checked ? "matched" : "not checked"),
    ]);
}
