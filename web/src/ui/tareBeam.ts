// The Tare Beam: the reusable analytical component — a single strict-unit
// lane plus a ranked, keyboard-navigable text outline that IS the source of truth (the track is
// decorative + aria-hidden). Four modes:
//   • pulse    — estimated priced spend partitioned into recoverable + remainder; unpriced usage is a
//                DETACHED token-share gap marker, never a fabricated dollar segment.
//   • run      — run→step→component→cache-class segmentation of one unit.
//   • compare  — a zero-centered cheaper/costlier delta (signed values diverge from center).
//   • optimize — one expected-savings lane partitioned by Open/Applied/Verifying; observed reduction is
//                a SEPARATE measured marker, never subtracted inside the additive lane.
//
// Invariants the component enforces structurally:
//   – One lane never mixes units: every segment is formatted in the model's single `unit`.
//   – Partition segments are proportional to their share of the lane sum (they visibly sum to 100%).
//   – Unknown/unpriced value lives OUTSIDE the dollar lane (the `gap`), labeled in tokens/share.
//   – Semantics survive with no color: every segment carries a pattern + the outline text states
//     label · exact value · share; forced-colors renders borders + labels, not fills.
// Framework-free + deterministic (byte-stable for a given model — the aria id is slugged from the
// title, no counters/Date/random), so each mode gets a deterministic fixture + component test.

import { el } from "./el.js";
import { fmtUsd, fmtTokens, fmtPct } from "./format.js";

export type BeamMode = "pulse" | "run" | "compare" | "optimize";
/// The ONE unit a lane speaks. A single Beam lane never mixes these.
export type BeamUnit = "usd" | "tokens" | "percent" | "count";

export interface BeamSegment {
  /// Stable key (selection id + dedup); also the pattern seed so a segment's pattern is deterministic.
  key: string;
  label: string;
  /// Magnitude in the lane's `unit`. May be negative in `compare` (cheaper) vs positive (costlier).
  value: number;
  /// Semantic tone class (e.g. cost-ok / cost-warn / cost-high / cat-1…); color is SECONDARY.
  tone?: string;
}

/// A detached, non-dollar marker (pulse unpriced usage): rendered outside the lane, labeled in tokens.
export interface BeamGap {
  label: string;
  /// Absolute unpriced token count, when known. Omit when only the SHARE is available (pulse mode uses
  /// the unpriced token SHARE — the marker then reads as a percentage, never a fabricated 0.
  tokens?: number;
  /// Optional 0..1 share of total tokens, shown as a percentage.
  share?: number;
}

export interface BeamModel {
  mode: BeamMode;
  title: string;
  unit: BeamUnit;
  segments: BeamSegment[];
  /// Detached token/share marker (pulse). Never a dollar segment.
  gap?: BeamGap;
  /// Optimize: observed reduction shown as a SEPARATE measured marker (not folded into `segments`).
  measured?: { label: string; value: number };
  /// When set, selecting a segment changes the analysis → outline rows are buttons; else plain text.
  onSelect?: (key: string) => void;
}

const PATTERN_COUNT = 6;

/// Deterministic slug for the aria id (no counters → byte-stable output).
function slug(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "beam";
}

/// Format a value in the lane's single unit. `usd` values are micros (the app's money unit).
function fmtUnit(value: number, unit: BeamUnit): string {
  switch (unit) {
    case "usd":
      return fmtUsd(value);
    case "tokens":
      return fmtTokens(value);
    case "percent":
      return fmtPct(value);
    case "count":
      return fmtTokens(value);
  }
}

/// Signed, zero-centered label for compare deltas ("−$0.40 cheaper" / "+$0.40 costlier").
function fmtDelta(value: number, unit: BeamUnit): string {
  const mag = fmtUnit(Math.abs(value), unit);
  if (value < 0) return `−${mag} cheaper`;
  if (value > 0) return `+${mag} costlier`;
  return `${mag} · no change`;
}

/// Percent share of the lane sum, rounded to one decimal (deterministic).
function sharePct(value: number, sum: number): string {
  if (sum <= 0) return "0%";
  return `${((Math.abs(value) / sum) * 100).toFixed(1)}%`;
}

/// Build the Tare Beam. Returns the normative <section> DOM. Pure + deterministic.
export function tareBeam(model: BeamModel): HTMLElement {
  const compare = model.mode === "compare";
  // Drop zero-value segments from the track (they carry no proportion) but keep the model order for
  // pattern assignment stability.
  const segs = model.segments.filter((s) => s.value !== 0 || compare);
  const absSum = segs.reduce((a, s) => a + Math.abs(s.value), 0);
  const maxAbs = segs.reduce((a, s) => Math.max(a, Math.abs(s.value)), 0);
  const titleId = `beam-title-${slug(model.title)}`;

  const section = el("section", {
    class: `tare-beam beam-${model.mode}`,
    "aria-labelledby": titleId,
  });
  section.appendChild(el("h2", { id: titleId, class: "tare-beam-heading" }, [model.title]));

  // ---- Track (decorative; the outline is the accessible source of truth) ----
  const track = el("div", {
    class: compare ? "tare-beam-track tare-beam-track--centered" : "tare-beam-track",
    "aria-hidden": "true",
  });
  if (compare) track.appendChild(el("span", { class: "tare-beam-center" }));
  model.segments.forEach((s, i) => {
    if (s.value === 0) return;
    const pat = `beam-pattern-${i % PATTERN_COUNT}`;
    if (compare) {
      const w = maxAbs > 0 ? (Math.abs(s.value) / maxAbs) * 50 : 0;
      track.appendChild(
        el("span", {
          class: `tare-beam-seg ${s.tone ?? ""} ${pat} ${s.value < 0 ? "beam-cheaper" : "beam-costlier"}`.trim(),
          "data-beam-key": s.key,
          style: `width:${w.toFixed(2)}%`,
          title: `${s.label}: ${fmtDelta(s.value, model.unit)}`,
        })
      );
    } else {
      const w = absSum > 0 ? (Math.abs(s.value) / absSum) * 100 : 0;
      track.appendChild(
        el("span", {
          class: `tare-beam-seg ${s.tone ?? ""} ${pat}`.trim(),
          "data-beam-key": s.key,
          style: `width:${w.toFixed(2)}%`,
          title: `${s.label}: ${fmtUnit(s.value, model.unit)} · ${sharePct(s.value, absSum)}`,
        })
      );
    }
  });
  section.appendChild(track);

  // ---- Ranked outline (accessible, keyboard-navigable) ----
  const ranked = model.segments
    .map((s, i) => ({ s, i }))
    .sort((a, b) => Math.abs(b.s.value) - Math.abs(a.s.value));
  const outline = el("ol", { class: "tare-beam-outline" });
  for (const { s, i } of ranked) {
    const pat = `beam-pattern-${i % PATTERN_COUNT}`;
    const swatch = el("span", { class: `tare-beam-swatch ${s.tone ?? ""} ${pat}`.trim(), "aria-hidden": "true" });
    const valueText = compare
      ? fmtDelta(s.value, model.unit)
      : `${fmtUnit(s.value, model.unit)} · ${sharePct(s.value, absSum)}`;
    const label = `${s.label} · ${valueText}`;
    const row = model.onSelect
      ? el("button", {
          type: "button",
          class: "tare-beam-row",
          "data-beam-key": s.key,
          onClick: () => model.onSelect!(s.key),
        }, [swatch, label])
      : el("span", { class: "tare-beam-row", "data-beam-key": s.key }, [swatch, label]);
    outline.appendChild(el("li", {}, [row]));
  }
  section.appendChild(outline);

  // ---- Optimize: observed reduction as a SEPARATE measured marker (never inside the lane sum) ----
  if (model.measured) {
    section.appendChild(
      el("p", { class: "tare-beam-measured" }, [
        el("span", { class: "tare-beam-measured-label", text: `${model.measured.label}: ` }),
        el("span", { class: "num", text: fmtUnit(model.measured.value, model.unit) }),
      ])
    );
  }

  // ---- Detached unpriced/unknown gap: tokens + share, OUTSIDE the dollar lane (never a $ segment) ----
  if (model.gap) {
    // The marker reads from whatever is honest: an absolute token count when known, otherwise the
    // token SHARE alone (pulse mode carries only the unpriced share). Never fabricate "0 tokens".
    const shareText = model.gap.share != null ? `${(model.gap.share * 100).toFixed(1)}% of tokens` : "";
    let detail: string;
    if (model.gap.tokens != null) {
      detail = `${fmtTokens(model.gap.tokens)} tokens${shareText ? ` · ${shareText}` : ""}`;
    } else {
      detail = shareText || "share unknown";
    }
    section.appendChild(
      el("p", { class: "tare-beam-gap" }, [
        el("span", { class: "tare-beam-gap-mark", "aria-hidden": "true" }),
        el("span", { text: `${model.gap.label}: ${detail} — not priced` }),
      ])
    );
  }

  return section;
}
