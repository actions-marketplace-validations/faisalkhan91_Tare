# Tare design guide

This is the current source of truth for Tare's product structure, visual language, and interaction
rules. Route behavior lives in [ROUTES.md](ROUTES.md), and native-shell verification in
[DESKTOP_TESTING.md](DESKTOP_TESTING.md). When prose and the verified implementation disagree, fix
the stale side in the same change.

Primary implementation anchors:

- `web/src/ui/tokens.css`: color, type, space, radius, elevation, and motion tokens
- `web/src/ui/app.css`, `components.css`, `workspaces.css`, and `shell.css`: component and layout rules
- `web/src/ui/desktop.css`: Tauri-only shell behavior
- `web/src/main.ts` and `web/src/shell/`: shell composition and routing
- `scripts/verify-contrast.py`: token contrast and palette constraints

## 1. Product character

Tare is a local-first cost workbench, not a collection of interchangeable dashboards.

Its core loop is: see the current answer or exception, narrow an exact cohort, inspect the run or
step that explains it, apply or model a change, and verify the result against a stored baseline.
The cause-attributed data graph is the data spine; persistent scope, selection, and baseline are the
interaction spine.

- Lead each view with one answer, exception, or decision. Supporting evidence should form quiet
  rows, ledgers, timelines, or comparison matrices instead of a wall of equal-weight cards.
- Use one rationed action color. Data marks use neutral ink or stable category colors; cost severity
  uses its own labelled scale.
- Prefer borders and spacing to decorative shadows. Shadows belong to floating surfaces such as
  sheets, palettes, menus, and toasts.
- Keep data areas opaque. Native translucency is allowed only on platform chrome, with an opaque
  fallback.
- Label estimates, unknowns, capture gaps, and aggregate associations honestly. Missing or
  unavailable data never becomes a reassuring zero.
- Accessibility is part of the component contract: keyboard operation, visible focus, robust
  reflow, reduced motion, and WCAG 2.2 AA contrast are required.

## 2. Color and identity

Tare has one visual identity: `bench`. The `brass` selector in `tokens.css` is a compatibility alias
for previously stored preferences, not a second theme. Appearance is an independent
`data-theme="light|dark"` axis. The `system` preference follows the OS live; an explicit light or
dark choice persists and wins.

The palette combines warm paper/ink neutrals, a cool registrar-ink action color, and an aged-brass
location marker. These roles must not be exchanged:

| Token | Role |
|---|---|
| `--bg`, `--surface`, `--surface-2`, `--surface-raised`, `--surface-inset` | Background and elevation ladder |
| `--border`, `--border-strong` | Region separation and load-bearing rules |
| `--text`, `--muted`, `--faint` | Text hierarchy; every role remains AA-legible |
| `--data-ink` | Neutral single-series marks |
| `--cat-1` through `--cat-6` | Stable labelled categories |
| `--accent`, `--accent-text`, `--accent-weak`, `--on-accent` | Brand actions, links, and supporting states |
| `--accent-system`, `--on-accent-system` | Contrast-clamped OS affordance bridge |
| `--brass` | Persistent rail marker and tab underline only |
| `--cost-ok`, `--cost-warn`, `--cost-high` | Labelled cost/severity scale |
| `--state-hover`, `--state-active`, `--state-selected` | Transient interaction layers |
| `--focus-ring` | Keyboard focus outline |
| `--shadow-sm`, `--shadow-lg`, `--shadow` | Floating elevation |

Rules:

- Components consume tokens; do not add raw color literals to component CSS.
- `--accent` is not a generic decoration or data-series fill.
- Persistent location uses brass. Keyboard-selected data rows use `--state-selected` plus a visible
  accent marker. Hover, current location, focus, and selection must remain distinguishable.
- Severity and category color always have a visible word, glyph, pattern, or key.
- `forced-colors` and increased-contrast preferences override decorative color and native material.
- Keep `scripts/verify-contrast.py` green when any palette token changes.

## 3. Typography and numeric evidence

| Role | Size and weight | Use |
|---|---|---|
| Display answer | `--fs-display`, 700 | The view's dominant answer |
| Secondary figure | `--fs-xl` or `--fs-2xl`, 700 | Supporting totals and comparison figures |
| Section heading | `--fs-lg`, 600 | Named content regions |
| Body | `--fs-md`, 400 | Default prose and controls |
| Label/caption | `--fs-sm`, 400–600 | Secondary description without sacrificing contrast |

- Atkinson Hyperlegible Next is the body face, Archivo is the display face, and IBM Plex Mono is
  used for aligned figures, identifiers, code, and receipts. System stacks are required fallbacks.
- Hierarchy comes from size, weight, position, and space, not low contrast or indiscriminate caps.
- Use sentence case. Reserve uppercase for very short eyebrow labels.
- Money and counts use tabular figures; numeric table columns and headers align right.
- A score names its direction and domain. A value without a unit or direction is incomplete.
- Truncated identifiers retain the full value in an accessible secondary surface.

### Money formatting

Money remains integer micro-USD through storage and computation.

- `toDollarString` is the exact, deterministic export representation. Do not route it through
  locale formatting or alter byte-stable export output casually.
- `fmtUsd` is the screen representation: ordinary values show two decimals, sub-cent values retain
  enough significant digits to preserve ranking, and only values at or above $1,000,000 compact.
- Zero, negative, unavailable, unpriced, rounded-to-zero, and not-applicable are distinct states.
- Exact values remain available in receipts and exports; a hover-only tooltip is never the sole
  source of precision.

## 4. Space, shape, and motion

- Use the `--s-*` 4/8px rhythm. Add a token when a repeated value is genuinely missing instead of
  scattering literals.
- Use `--radius-sm`, `--radius`, `--radius-lg`, and `--radius-pill` by semantic role. Pills are for
  compact states and badges, not every control.
- Comfortable density is the browser default; compact density reduces space without shrinking the
  type scale or hit targets below the minimum.
- Interactive targets are at least 24×24 CSS px. Primary controls in touch-plausible browser
  contexts should approach 44px.
- Use short tokenized transitions. Edge sheets animate with transform only, remain opaque while
  moving, and finish their exit before removal.
- Never animate layout properties for routine interaction. Disable nonessential transition and
  animation under `prefers-reduced-motion`.
- Keyboard focus uses a visible 2px outline with separation from the control. Scroll owners provide
  enough `scroll-padding` that sticky chrome cannot fully obscure the focused item.

## 5. Shell, workspaces, and routes

The shell is a fixed frame containing the rail, top toolbar, routed main pane, status bar, and one
utility-sheet host.

- **Pulse** owns current-period spend, pace, forecast, trust summary, attention, drivers, and the
  bounded Now feed.
- **Investigate** owns cohort scope, entity and facet modes, timeline, saved investigations, Run
  Profile, and Compare.
- **Optimize** owns opportunities, action state, verification, and offline scenarios.
- **Capture**, **Trust & pricing**, and **Settings** open as sheets over the active workspace.
- Correlations, lineage, work units, and scenarios are palette destinations or workspace modes, not
  additional top-level products.
- Pulse is the only current-period landing surface; do not recreate separate Live or Overview
  implementations.
- Run Profile is a bounded workbench with a navigator, analysis canvas, and contextual inspector,
  not a long document page.
- Compare keeps Baseline B fixed and candidates ordered. A matrix that cannot honor the active
  metric or normalization must say so rather than silently changing meaning.
- Optimize keeps capped potential, applied exposure, and observed reduction separate. They are not
  additive, and aggregate-only matches are associations rather than causal savings.

Legacy hashes are compatibility inputs. `web/src/ui/routes.ts` redirects them into canonical
workspace state; deleted screens must not regain independent render paths.

Persistent rail location uses a short brass rule and semibold label. Within-workspace modes use a
flat tab baseline and brass underline. Transient row selection uses a filled state layer and accent
marker. Do not turn these three meanings into one generic selected pill.

### Layout and scrolling

- The document does not scroll. The shell owns the viewport and `.main` is the ordinary content
  scroll owner.
- Every pane has at most one scroll owner per axis. Grid and flex children that own overflow need
  the appropriate `min-width: 0` or `min-height: 0` constraint.
- Run Profile intentionally transfers vertical ownership to its bounded navigator, canvas, and
  inspector panes. Those owners are siblings, not same-axis scroll regions nested inside each other.
- Use `overscroll-behavior: contain` where a pane owns scrolling. Live refresh, filtering,
  selection, and focus must not jump the viewport.
- Prefer container-driven adaptation through `adaptivePanes.ts`; use viewport media queries for
  platform/accessibility traits and straightforward fallbacks.
- At narrow widths, navigation becomes a drawer. Content must reflow to 320px/400% zoom without
  clipped controls or unintended two-dimensional page scrolling.

## 6. Interaction and keyboard behavior

One command registry and one durable state model serve every entry point.

- The command palette is the discoverability surface. Rail Commands, contextual actions, native
  menu bridges, and palette rows resolve stable identifiers from `commands/registry.ts`.
- `g p`, `g i`, and `g o` navigate Pulse, Investigate, and Optimize. Multi-key navigation chords
  remain inert while the user edits a field.
- Lists may opt into `j`, `k`, arrow, Enter, and Escape handling through `listNav.ts`. Single-character
  shortcuts are allowed only while that list owns focus.
- Selection is keyed by a stable item identifier, not a row index or DOM focus. Re-rendering,
  sorting, searching, polling, and virtualization must restore selection to the same item.
- When updating a visible collection, preserve keyed node identity where focus, selection, or scroll
  depends on it.
- Asynchronous renderers use cancellation or sequence guards so stale results cannot overwrite a
  newer route or mode.

## 7. States, performance, and resilience

Every asynchronous surface distinguishes loading, empty, partial, error, and populated states.

- Loading uses an immediate skeleton or progress message; it never flashes an empty result or $0.
- Errors render into a connected live region and preserve enough context to retry or recover.
- Partial failures identify the missing source without discarding successful evidence.
- Common local navigation, filtering, and read paths target a 100ms response budget. If work is
  slower, acknowledge it immediately instead of leaving a blank pane.
- Window long collections. Run Profile's navigator is the established virtualization pattern;
  `dataTable` preserves keyed rows and selection for ordinary result sets.
- Optimistic writes may update memory first only when failure can be reconciled visibly. Never imply
  persistence before a high-stakes write has succeeded.
- Polling is bounded and must stop when its owner is removed or inactive.

## 8. Charts and structural views

Screen graphics and export graphics are separate contracts.

- `renderSvg` and `renderTrendSvg` are deterministic export renderers. The Rust/TypeScript parity and
  golden output are intentional compatibility constraints.
- `renderSvgThemed` and `renderTrendSvgThemed` are on-screen renderers. They use live tokens and
  accessible labels; their output need not share export colors.
- The on-screen flamegraph may use cost or token width. `cullSubPixel` must receive the same weight
  mode as the renderer so invisible frames do not create unnecessary DOM.
- Prefer position and length for quantity. Bars start at zero; compressed or logarithmic axes are
  reserved for suitable non-bar marks and are labelled explicitly.
- Quantitative axes show ticks, scale, and unit on the chart. A money axis uses money formatting,
  not a generic count formatter.
- Empty, singleton, and degenerate domains have explicit render behavior.
- Every visual encoding has a visible key and an accessible name. Estimated cost charts show their
  evidence/provenance near the chart.
- Flamegraphs explain nesting and width in plain language. Differential flamegraphs include their
  diverging-color key inline.
- Offline scenarios model declared changes over captured work; they do not claim re-execution or
  fabricate quality outcomes.

## 9. Copy, trust, and security

- Use plain language and canonical names. A concept should not change labels between workspaces.
- Action labels are short and verb-led. Errors name the specific problem and a useful next step.
- Use “estimated” for priced estimates and “approximate” only for weaker modeled comparisons.
  Receipts are recomputation evidence, not cryptographic proof.
- Explain domain terms at first use through `ui/glossary.ts`. Tooltips can reinforce meaning but
  cannot be the only explanation available to keyboard or touch users.
- Humanize enum labels, especially privacy and retention choices, and describe what each choice
  retains or drops before the user commits it.
- State pills expose a shared definition in an accessible, discoverable surface rather than a title
  that merely repeats the visible label.
- Preserve literal case for model IDs, environment variables, flags, filenames, and other code.
- Untrusted values flow through `textContent`/the `el()` builder. Trusted, deterministic SVG is the
  narrow exception; never interpolate payload data into an HTML string.

Canonical product terms:

| Concept | Label |
|---|---|
| Local ingest process | **capture service** |
| Its two inputs | **proxy channel**, **OTel channel** |
| Average request cost | **Avg cost / call** |
| Blended rate | **Blended $/1M tokens** |
| Fraction of spend | **Share of spend** |
| Pricing utility | **Trust & pricing** |

## 10. Desktop shell

The interior is shared across platforms; native chrome follows each OS.

- macOS uses an overlay title bar, native traffic lights, drag regions, and optional
  `UnderWindowBackground` material on shell chrome. Windows and Linux keep their native title bars.
- The opaque shell is the mandatory fallback for unsupported platforms, inactive windows, Reduce
  Transparency, forced colors, and increased contrast.
- The contrast-clamped OS accent may drive system affordances without changing the brand/category
  palette used by charts.
- `--traffic-light-reserve` in `desktop.css` and the fallback in `gui.rs` must stay synchronized.
- The desktop served twin covers CSS geometry and the command contract. WKWebView material,
  traffic-light rendering, drag hit-testing, native menus/tray, and rasterization remain manual
  release checks. See [DESKTOP_TESTING.md](DESKTOP_TESTING.md).

## 11. Change checklist

- [ ] The change reinforces the Pulse → Investigate → Optimize loop and does not revive a retired screen.
- [ ] New colors, spacing, radii, elevation, and motion use the established tokens.
- [ ] Light, dark, increased-contrast, forced-color, and reduced-motion behavior remain coherent.
- [ ] Keyboard focus, selection, activation, and escape behavior work without a pointer.
- [ ] Loading, empty, partial, error, and populated states remain honest and distinct.
- [ ] Numeric evidence has the right unit, precision, alignment, and estimate label.
- [ ] Each pane has deliberate scroll ownership and narrow layouts reflow cleanly.
- [ ] On-screen charts are themed and accessible; export bytes change only intentionally.
- [ ] If a captured surface (Pulse, Run Profile, Optimize) changed shape, the README screenshots are refreshed via `scripts/gen-ui-shots.sh` (`assets/screenshots/*.png`).
- [ ] Relevant unit, browser, desktop-stub, Rust, contrast, and asset-sync checks pass.

## Sources

[Apple Human Interface Guidelines](https://developer.apple.com/design/human-interface-guidelines/),
[GitHub Primer](https://primer.style/), [Radix Colors](https://www.radix-ui.com/colors),
[Shopify Polaris](https://polaris.shopify.com/),
[WAI-ARIA Authoring Practices](https://www.w3.org/WAI/ARIA/apg/), and
[WCAG 2.2](https://www.w3.org/TR/WCAG22/).
