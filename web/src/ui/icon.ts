// Inline SVG icon primitive. One small, consistent, framework-free icon set
// that replaces platform-variable emoji/glyph controls (✕ ⋯ ⚙ 📌 •) and the amber brand dot that read
// as a fourth macOS traffic light. Every icon is built with createElementNS — NOT innerHTML — so the
// single sanctioned `rawSvg` sink (el.ts) stays reserved for the byte-stable renderer output; icon
// geometry is app-authored + static, so DOM construction is both safe and jsdom-testable.
//
// Invariants each icon upholds:
//  • Draws in `currentColor` (stroke, and fill where a shape needs it) — so it inherits the control's
//    text color AND is remapped by Windows forced-colors / high-contrast automatically, with no
// hard-coded hue for the OS to fight.
//  • Decorative by DEFAULT: aria-hidden + focusable="false", so an icon inside an already-labelled
//    control (a button with aria-label) adds no duplicate screen-reader noise. Pass `label` to promote
//    it to a standalone accessible image (role="img" + <title> + aria-label).
//  • Consistent sizing: a fixed 24-unit viewBox grid rendered at `size` px (default 16), so every glyph
//    aligns on the same optical box regardless of its path.

const NS = "http://www.w3.org/2000/svg";

/// One drawn element of an icon: an SVG tag plus its attributes (path/line/circle/polyline…).
interface Shape {
  tag: string;
  attrs: Record<string, string>;
}

// Icon geometry on a 0..24 grid. Stroked shapes inherit `currentColor` via the parent <svg> stroke;
// shapes that must be solid (dots) set `fill: currentColor` explicitly and opt out of the stroke.
const ICONS = {
  // Primary rail destinations. These stay deliberately geometric and outline-only so the 60px
  // collapsed rail remains legible in both themes and under forced-colors.
  pulse: [
    { tag: "polyline", attrs: { points: "3 12 7 12 9.5 6 13.5 18 16 12 21 12" } },
  ],
  investigate: [
    { tag: "circle", attrs: { cx: "10.5", cy: "10.5", r: "6.5" } },
    { tag: "line", attrs: { x1: "15.5", y1: "15.5", x2: "21", y2: "21" } },
  ],
  optimize: [
    { tag: "polyline", attrs: { points: "4 17 9 12 13 15 20 7" } },
    { tag: "polyline", attrs: { points: "15 7 20 7 20 12" } },
  ],
  commands: [
    { tag: "line", attrs: { x1: "8", y1: "7", x2: "20", y2: "7" } },
    { tag: "line", attrs: { x1: "8", y1: "12", x2: "20", y2: "12" } },
    { tag: "line", attrs: { x1: "8", y1: "17", x2: "20", y2: "17" } },
    { tag: "circle", attrs: { cx: "4", cy: "7", r: "1", fill: "currentColor", stroke: "none" } },
    { tag: "circle", attrs: { cx: "4", cy: "12", r: "1", fill: "currentColor", stroke: "none" } },
    { tag: "circle", attrs: { cx: "4", cy: "17", r: "1", fill: "currentColor", stroke: "none" } },
  ],
  capture: [
    { tag: "circle", attrs: { cx: "12", cy: "12", r: "8" } },
    { tag: "circle", attrs: { cx: "12", cy: "12", r: "2.5", fill: "currentColor", stroke: "none" } },
  ],
  trust: [
    { tag: "path", attrs: { d: "M12 3 L20 6 V12 C20 17 16.5 20 12 21 C7.5 20 4 17 4 12 V6 Z" } },
    { tag: "polyline", attrs: { points: "8.5 12 11 14.5 15.5 9.5" } },
  ],
  // ✕ — close / dismiss.
  close: [
    { tag: "line", attrs: { x1: "5", y1: "5", x2: "19", y2: "19" } },
    { tag: "line", attrs: { x1: "19", y1: "5", x2: "5", y2: "19" } },
  ],
  // ⋯ — overflow / more actions (three solid dots).
  more: [
    { tag: "circle", attrs: { cx: "5", cy: "12", r: "1.6", fill: "currentColor", stroke: "none" } },
    { tag: "circle", attrs: { cx: "12", cy: "12", r: "1.6", fill: "currentColor", stroke: "none" } },
    { tag: "circle", attrs: { cx: "19", cy: "12", r: "1.6", fill: "currentColor", stroke: "none" } },
  ],
  // ⚙ — settings (a sliders control: cleaner + lighter than a toothed gear at 16px).
  settings: [
    { tag: "line", attrs: { x1: "4", y1: "7", x2: "20", y2: "7" } },
    { tag: "line", attrs: { x1: "4", y1: "17", x2: "20", y2: "17" } },
    { tag: "circle", attrs: { cx: "9", cy: "7", r: "2.4" } },
    { tag: "circle", attrs: { cx: "15", cy: "17", r: "2.4" } },
  ],
  // 📌 — pinned / kept (a bookmark: unambiguously "saved", never a location pin).
  pin: [
    { tag: "path", attrs: { d: "M7 4 h10 v16 l-5 -3.5 l-5 3.5 z" } },
  ],
  // • — recent / list marker (a small solid dot, stable across platforms).
  dot: [
    { tag: "circle", attrs: { cx: "12", cy: "12", r: "3", fill: "currentColor", stroke: "none" } },
  ],
  // ⚠ — warning / severity (triangle + bang). Color comes from the host (e.g. .unpriced severity).
  warning: [
    { tag: "path", attrs: { d: "M12 3.5 L22 20 H2 z" } },
    { tag: "line", attrs: { x1: "12", y1: "10", x2: "12", y2: "14.5" } },
    { tag: "circle", attrs: { cx: "12", cy: "17.5", r: "1", fill: "currentColor", stroke: "none" } },
  ],
  // ▸ — breadcrumb / drill separator (a right chevron; decorative).
  chevron: [
    { tag: "polyline", attrs: { points: "9 6 15 12 9 18" } },
  ],
  // Calibration mark: graduated gauge ticks derived from the Tare Beam — a measurement/
  // zeroing motif, deliberately NOT a colored dot, so it can never read as a macOS traffic light.
  calibration: [
    { tag: "line", attrs: { x1: "5", y1: "15", x2: "5", y2: "9" } },
    { tag: "line", attrs: { x1: "12", y1: "18", x2: "12", y2: "6" } },
    { tag: "line", attrs: { x1: "19", y1: "15", x2: "19", y2: "9" } },
  ],
} satisfies Record<string, Shape[]>;

export type IconName = keyof typeof ICONS;

export interface IconOpts {
  /// Rendered box in px (both axes). Default 16.
  size?: number;
  /// Extra class(es) appended after the base `icon` class.
  class?: string;
  /// When set, the icon becomes a STANDALONE accessible image (role=img + aria-label + <title>).
  /// Omit for icons inside an already-labelled control — they stay decorative (aria-hidden).
  label?: string;
  /// Owning document for call sites that render into an injected DOM. Default
  /// is the global `document`.
  doc?: Document;
}

/// Build an inline SVG icon element. Unknown names throw at author time (dev guard) — the registry is
/// the single source of truth for the app's glyph vocabulary.
export function icon(name: IconName, opts: IconOpts = {}): SVGSVGElement {
  const shapes = ICONS[name];
  if (!shapes) throw new Error(`icon: unknown name "${String(name)}"`);
  const size = opts.size ?? 16;
  const d = opts.doc ?? document;
  const svg = d.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "2");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("class", opts.class ? `icon ${opts.class}` : "icon");

  if (opts.label) {
    svg.setAttribute("role", "img");
    svg.setAttribute("aria-label", opts.label);
    const title = d.createElementNS(NS, "title");
    title.textContent = opts.label;
    svg.appendChild(title);
  } else {
    // Decorative: hidden from the a11y tree, never a tab/focus stop.
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
  }

  for (const s of shapes) {
    const node = d.createElementNS(NS, s.tag);
    for (const [k, v] of Object.entries(s.attrs)) node.setAttribute(k, v);
    svg.appendChild(node);
  }
  return svg;
}

/// The brand calibration mark: the graduated-gauge glyph at wordmark scale, replacing the
/// legacy amber `.brand .dot`. Decorative — the adjacent "TARE" wordmark is the accessible name.
export function calibrationMark(size = 16): SVGSVGElement {
  return icon("calibration", { size, class: "brand-mark" });
}
