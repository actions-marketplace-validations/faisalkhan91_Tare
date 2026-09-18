// The Tare app shell: a grouped left sidebar (sections + pinned footer), a contextual toolbar
// (active-view title + today's spend + theme), a routed main pane, and a status bar. Framework-
// free (pure DOM + a tiny store/router) so it runs identically in the Tauri WebView and jsdom.
// Screens wrap the byte-stable SVG renderers untouched; all numbers come from the Rust core and
// are estimated.

import { el, clear } from "./ui/el.js";
import { icon, calibrationMark, type IconName } from "./ui/icon.js";
import { errorNode } from "./ui/errorNode.js";
import { skelScreen } from "./ui/skeleton.js";
import { onRoute, routeHash, routePath, hashOf, navigate, type Route } from "./ui/store.js";
import { redirectRoute, migrateDefaultScreen } from "./ui/routes.js";
import { isWorkspaceRoute, renderWorkspace } from "./shell/workbench.js";
import { createAnalysisStore, initialAnalysisState } from "./analysis/store.js";
import {
  investigationFromState,
  migratePrefsToState,
  routeToWorkspace,
  type SavedInvestigationV2,
} from "./analysis/investigation.js";
import {
  SECTIONS,
  LENS_ROUTES,
  FOOTER,
  allNav,
  sectionFor,
  type NavItem,
} from "./shell/rail.js";
import { topbarActionFor } from "./shell/toolbar.js";
import { timeLabel, createStatusBar } from "./shell/statusBar.js";
import { observeWorkbench } from "./shell/adaptivePanes.js";
import {
  initialTheme,
  applyTheme,
  watchOsTheme,
  initialIdentity,
  applyIdentity,
  type Theme,
} from "./ui/theme.js";
import {
  isOnboarded,
  density,
  applyDensity,
  rangePref,
  setRangePref,
  recentRuns,
  pinnedRuns,
  baselineRun,
  reconcileRunReferences,
  defaultScreen,
} from "./ui/prefs.js";
import { RANGE_OPTIONS, rangeToWindow, type RangeKey, type RangePref } from "./ui/range.js";
import { openPalette, installPaletteHotkey, type Command } from "./ui/palette.js";
import { formatShortcut, matchShortcut } from "./ui/keymap.js";
import { osClass, type OsClass } from "./ui/os.js";
import {
  ACTION,
  navId,
  runId,
  orderCommands,
  selectionCommands,
  type RegistryCommand,
} from "./commands/registry.js";
import type { CommandSelection } from "./commands/contexts.js";
import { commandsRailRow } from "./commands/sidebarCommands.js";
import { refreshSystemAccent } from "./ui/systemAccent.js";
import { installHotkeys, type Hotkey } from "./ui/hotkeys.js";
import { installSlashSearch } from "./ui/slashSearch.js";
import { openFind, installFindHotkey } from "./ui/findBar.js";
import { installCommandKey } from "./ui/commandKey.js";
import { renderOnboarding } from "./screens/onboarding.js";
import { emptyState } from "./ui/empty.js";
import { showToast } from "./ui/toast.js";
import { afterTransition, beginMotion } from "./ui/motion.js";
import { evaluateAlertNotices, selectNewAlertNotices } from "./alerts.js";
import type { Anomaly, BurnRate, PeriodBudget, TareClient, TodaySpend } from "./client.js";

type Screen = (root: HTMLElement, client: TareClient, param?: string) => Promise<void>;

// Standalone legacy routes redirect to the canonical workspaces. Onboarding is the only route that
// still renders directly from the shell; analytical and utility surfaces load through workbench.ts.
const SCREENS: Record<string, Screen> = {
  onboarding: renderOnboarding,
};

/// Screens whose content is scoped by the global analysis range. The range control is
/// shown only here; Pulse has a fixed budget period, and Compare/Run Profile own fixed evidence scopes.
function routeHonorsRange(route: Route): boolean {
  // The canonical Investigate root resolves every result through CohortSpec. Pulse is explicitly a
  // current budget-period forecast, while Run Profile / Compare own fixed evidence scopes; showing
  // the picker on those surfaces would imply backend scoping they do not support.
  return route.name === "investigate" && route.segments.length === 1;
}

/// Leader-key go-to chords: `g <letter>` jumps to a primary workspace. This is the source for both
/// the hotkey router and command-palette hints; child lenses and utilities remain palette-only.
const GOTO_CHORDS: Record<string, string> = {
  pulse: "g p",
  investigate: "g i",
  optimize: "g o",
};

/// Labels for routes that are on neither the rail nor the demoted list — drill-only destinations
/// reached from a parent screen or the palette.
const DRILL_LABELS: Record<string, string> = {
  compare: "Compare runs",
  onboarding: "Welcome",
};

/// The display label for a route name. ONE resolver, used by both the breadcrumb and the page title.
///
/// This was two `extra` maps with a "keep in sync" comment, and `baseLabel` returned the raw route id
/// as its fallback while `labelFor` Title-cased it. That is exactly how the canonical Investigate
/// workspace came to render a lowercase "investigate" crumb — and a literal "compare" as a page <h1>:
/// `investigate` was in no list, so it fell through to the raw name. The rail now
/// has an Investigate entry, but the Title-case floor stays as a second line of defence so no future
/// route can surface a raw id either.
function baseLabel(name: string): string {
  const known =
    allNav().find((n) => n.name === name)?.label ??
    LENS_ROUTES.find((n) => n.name === name)?.label ??
    DRILL_LABELS[name];
  return known ?? name.charAt(0).toUpperCase() + name.slice(1);
}

function labelFor(route: Route): string {
  return baseLabel(route.name);
}

function canonicalViewTitle(route: Route): string {
  if (route.name === "investigate" && route.segments[1] === "run" && route.segments[2]) {
    return `Run · ${route.segments[2]}`;
  }
  if (route.name === "investigate" && route.segments[1] === "compare") return "Compare runs";
  const query = route.query ?? {};
  if (route.name === "investigate") {
    if (query.view === "units") return "Investigate · Work units";
    if (query.mode === "lineage") return "Investigate · Prompt lineage";
    if (query.mode === "distinguish") return "Investigate · Correlations";
    if (query.mode === "timeline") return "Investigate · Timeline";
    const entity = query.entity;
    if (entity && ["runs", "sessions", "templates", "steps"].includes(entity)) {
      return `Investigate · ${entity.charAt(0).toUpperCase()}${entity.slice(1)}`;
    }
  }
  if (route.name === "optimize" && query.view === "scenarios") return "Optimize · Scenarios";
  if (route.name === "pulse" && query.mode === "now") return "Pulse · Now";
  return labelFor(route);
}

function investigationScopeLabel(inv: SavedInvestigationV2): string {
  const scope = inv.state.scope;
  const workspace = inv.state.workspace.charAt(0).toUpperCase() + inv.state.workspace.slice(1);
  if (!scope) return workspace;
  const period = scope.from && scope.to ? `${scope.from}–${scope.to}` : "All captured dates";
  const filters = scope.filters?.length ? ` · ${scope.filters.length} filter${scope.filters.length === 1 ? "" : "s"}` : "";
  return `${workspace} · ${period}${filters}`;
}

function investigationHref(inv: SavedInvestigationV2): string {
  // User-saved views use their canonical hash as a stable id. Rehydrate the stored analysis state
  // without discarding the route's mode/entity/nested target. Server-created investigations use an
  // opaque id and fall back to their owning workspace root.
  if (!inv.id.startsWith("#/")) {
    return routePath([inv.state.workspace], { investigation: inv.id });
  }
  const [path, rawQuery = ""] = inv.id.slice(1).split("?", 2);
  const params = new URLSearchParams(rawQuery);
  params.delete("sheet");
  params.set("investigation", inv.id);
  return `#${path}?${params.toString()}`;
}

/// Render the breadcrumb-as-nav-stack into `nav` (a <nav aria-label="Breadcrumb">) as an APG ordered
/// list: zone / screen / param. Ancestors are clickable links so nested routes always have a way up;
/// the section/zone is plain text (not a page); the LEAF carries aria-current="page" AND is the page's
/// <h1> (one element, correctly a heading + the current crumb). Separators are CSS-drawn (aria-hidden),
/// never text nodes. Left-aligned, replacing the old centered view title.
function setBreadcrumb(nav: HTMLElement, route: Route): void {
  const nestedRunId =
    route.name === "investigate" && route.segments[1] === "run" ? route.segments[2] : undefined;
  if (nestedRunId) {
    const { view: _view, ...scopeQuery } = route.query ?? {};
    const segs: Array<{ label: string; href?: string }> = [];
    const sec = sectionFor("investigate");
    if (sec && sec !== baseLabel("investigate")) segs.push({ label: sec });
    segs.push({
      label: baseLabel("investigate"),
      href: routePath(["investigate"], scopeQuery),
    });
    segs.push({ label: `Run · ${nestedRunId}` });
    const ol = el("ol", { class: "breadcrumb" });
    segs.forEach((s, i) => {
      const leaf = i === segs.length - 1;
      const node = leaf
        ? el("h1", { class: "crumb crumb-current", "aria-current": "page", text: s.label })
        : s.href
          ? el("a", { class: "crumb", href: s.href, text: s.label })
          : el("span", { class: "crumb", text: s.label });
      ol.appendChild(el("li", { class: "crumb-item" }, [node]));
    });
    nav.replaceChildren(ol);
    return;
  }
  const anchor = route.name;
  const segs: Array<{ label: string; href?: string }> = [];
  const sec = sectionFor(anchor);
  // Zone (Monitor/Investigate/Act) — text, not a destination. Skipped when it is simply the
  // destination's own name: the Investigate workspace sits in the Investigate zone, and
  // "Investigate / Investigate" is noise, not context.
  if (sec && sec !== baseLabel(anchor)) segs.push({ label: sec });
  segs.push({
    label: baseLabel(route.name),
    // link the screen only when we're deeper than it (a param drill); a rail leaf isn't self-linked.
    href: route.param ? routeHash(route.name) : undefined,
  });
  if (route.param) {
    // A nested canonical segment (investigate/compare) is a NAMED route, not an opaque id, so it gets
    // a real label. It used to push the raw segment, so #/investigate/compare rendered a lowercase
    // "compare" as the page <h1>. Opaque ids (run ids) still pass through as-is.
    segs.push({ label: DRILL_LABELS[route.param] ?? route.param });
  }

  const ol = el("ol", { class: "breadcrumb" });
  segs.forEach((s, i) => {
    const leaf = i === segs.length - 1;
    const node = leaf
      ? el("h1", { class: "crumb crumb-current", "aria-current": "page", text: s.label })
      : s.href
        ? el("a", { class: "crumb", href: s.href, text: s.label })
        : el("span", { class: "crumb", text: s.label });
    ol.appendChild(el("li", { class: "crumb-item" }, [node]));
  });
  nav.replaceChildren(ol);
}

/// The contextual toolbar action for a route, or null when the slot should collapse. Route-keyed so
/// screens stay pure render-into-container functions; the toolbar is the discoverable home for a
/// screen's primary jump. Kept small — data-coupled actions (Export a specific run, Apply a specific
/// saving) will arrive when Screens can return their own action node.

function placeholder(label: string): Screen {
  return async (root) => {
    // Keep the not-found state consistent with other empty states and offer a route back.
    root.replaceChildren(
      el("section", { class: "section" }, [
        emptyState("Page not found", `There's no ${label} screen here. It may have moved or been renamed.`, {
          glyph: "∅",
          actionLabel: "Go to Pulse →",
          actionHref: routeHash("pulse"),
        }),
      ])
    );
  };
}

/// Seed the run-workbench preferences (baseline + pinned runs) into the boot-owned AnalysisState.
/// Focus comes from the fresh state and remains transient. These are current UI prefs
/// (not v1 durable state) — they seed the ACTIVE investigation on every boot.
export function initialAnalysisStateFromPrefs() {
  return {
    ...initialAnalysisState(),
    ...migratePrefsToState(baselineRun(), pinnedRuns()),
  };
}

export async function mountApp(root: HTMLElement, client: TareClient): Promise<void> {
  // Load durable saved investigations and the active store's authoritative run ids before replacing
  // the static startup shell. The latter prunes stale UI-only recents/pins left by a purge or DB
  // switch, so the shell never advertises a Run Profile that the current store cannot open. Either
  // read may fail independently; an unavailable local API still cannot prevent the shell mounting.
  let savedInvestigations: SavedInvestigationV2[] = [];
  const [investigationsResult, runsResult] = await Promise.allSettled([
    // Promise.resolve().then also contains a synchronous missing-method error from older/partial
    // host bridges; boot must remain recoverable during a mixed-version desktop update.
    Promise.resolve().then(() => client.listInvestigations()),
    Promise.resolve().then(() => client.listRuns()),
  ]);
  if (investigationsResult.status === "fulfilled") savedInvestigations = investigationsResult.value;
  if (runsResult.status === "fulfilled") reconcileRunReferences(runsResult.value);
  clear(root);
  // One boot-owned analysis store is the interaction spine for every lazy workspace. Workspaces are
  // still dynamically imported; only the small state primitive belongs to the startup graph.
  const analysisStore = createAnalysisStore(initialAnalysisStateFromPrefs());
  // OS-aware palette chord label: "⌘K" on macOS, "Ctrl K" on Windows/Linux — one
  // source so the button, its title, and the empty-views hint never advertise the wrong key.
  const paletteChord = formatShortcut("Mod+K");
  applyIdentity(initialIdentity());
  applyDensity(density());
  let theme: Theme = initialTheme();
  applyTheme(theme);
  // Follow the OS appearance live until the user makes an explicit choice.
  watchOsTheme((t) => {
    theme = t;
    applyTheme(theme);
    refreshSystemAccent(); // Re-clamp the OS accent against the new surface.
  });

  // ---- brand + sidebar ----
  // `data-tauri-drag-region` makes the title band draggable — but ONLY on macOS:
  // there the web IS the title bar (Overlay), so the band should drag; Windows/Linux use a PLAIN native
  // title bar above the web content, so Tare's band is an ordinary toolbar and must not drag
  // the window. Tauri 2.11's marker is SELF-targeted: marking only a parent does not make its SVG/text/
  // flex children draggable. Stamp every actual NON-interactive click target below; controls remain
  // unmarked + no-drag. This yields the native Obsidian/Zoom-style unified toolbar affordance.
  const os: OsClass = (document.documentElement.dataset.os as OsClass) || osClass(navigator.userAgent);
  const dragRegion = os === "macos" ? { "data-tauri-drag-region": "" } : {};
  // Brand is just the mark + name now; the "cost profiler…" tagline moved to the sidebar footer
  // so the chrome header stays lean.
  const brandMark = calibrationMark();
  if (os === "macos") brandMark.setAttribute("data-tauri-drag-region", "");
  const navDrawerTrigger = el(
    "button",
    {
      class: "btn ghost nav-drawer-trigger",
      type: "button",
      "aria-label": "Open navigation",
      "aria-controls": "primary-navigation",
      "aria-expanded": "false",
    },
    [icon("commands", { size: 16 }), el("span", { text: "Menu" })]
  ) as HTMLButtonElement;
  const brand = el("div", {
    class: "brand",
    role: "region",
    "aria-label": "Tare application",
    ...dragRegion,
  }, [
    brandMark,
    el("span", { class: "brand-name", text: "TARE", ...dragRegion }),
    navDrawerTrigger,
  ]);

  const navEl = el("nav", {
    class: "sidebar",
    id: "primary-navigation",
    "aria-label": "Primary",
  });
  const navDrawerClose = el(
    "button",
    {
      class: "btn ghost nav-drawer-close",
      type: "button",
      "aria-label": "Close navigation",
    },
    [icon("close", { size: 16 })]
  ) as HTMLButtonElement;
  navEl.appendChild(
    el("header", { class: "nav-drawer-header" }, [
      el("span", { class: "nav-drawer-title", text: "Navigation" }),
      navDrawerClose,
    ])
  );
  const links: Record<string, HTMLElement> = {};
  const mkLink = (n: NavItem) => {
    const a = el(
      "a",
      {
        class: "nav-item nav-destination",
        href: routeHash(n.name),
        title: n.label,
        "aria-label": n.label,
      },
      [
        icon(n.icon ?? "dot", { size: 16, class: "nav-item-icon" }),
        el("span", { class: "nav-item-label", text: n.label }),
      ]
    );
    links[n.name] = a;
    return a;
  };
  for (const sec of SECTIONS) {
    navEl.appendChild(el("p", { class: "nav-section", text: sec.title }));
    // Links live in a list so screen readers announce "list, N items" + position (a11y).
    const list = el("ul", { class: "nav-list" });
    for (const it of sec.items) list.appendChild(el("li", {}, [mkLink(it)]));
    navEl.appendChild(list);
  }

  // Saved Views: user-pinned route+query destinations. Re-rendered in place whenever a
  // view is saved/removed (the sidebar itself is built once at mount).
  const viewsEl = el("div", { class: "nav-views" });
  const renderViews = (): void => {
    const views = savedInvestigations.map((inv) => ({
      id: inv.id,
      label: inv.label,
      href: investigationHref(inv),
      meta: investigationScopeLabel(inv),
      investigation: inv,
    }));
    const children: HTMLElement[] = [el("p", { class: "nav-section", text: "Views" })];
    if (views.length === 0) {
      children.push(el("p", { class: "nav-empty sub", text: `Save a view from ${paletteChord}` }));
    } else {
      for (const v of views) {
        const link = el(
          "a",
          {
            class: "nav-item nav-view",
            href: v.href,
            title: `${v.label} — ${v.meta}`,
            "aria-label": `Open saved view ${v.label}, ${v.meta}`,
          },
          [
            el("span", { class: "nav-view-copy" }, [
              el("span", { class: "nav-view-title", text: v.label }),
              el("span", { class: "nav-view-meta", text: v.meta }),
            ]),
          ]
        );
        const del = el(
          "button",
          {
            class: "nav-view-del",
            "aria-label": `Remove view ${v.label}`,
            title: "Remove view",
            onClick: (e: Event) => {
              e.preventDefault();
              e.stopPropagation();
              void (async () => {
                try {
                  await client.deleteInvestigation(v.id);
                  savedInvestigations = savedInvestigations.filter((inv) => inv.id !== v.id);
                } catch {
                  /* leave the authoritative v2 row visible when deletion fails */
                }
                renderViews();
              })();
            },
          },
          [icon("close", { size: 12 })]
        );
        children.push(el("div", { class: "nav-view-row" }, [link, del]));
      }
    }
    viewsEl.replaceChildren(...children);
  };
  renderViews();
  window.addEventListener("tare:investigations-changed", () => {
    void client
      .listInvestigations()
      .then((rows) => {
        savedInvestigations = rows;
        renderViews();
      })
      .catch(() => {
        /* Keep the last authoritative list visible when the local database is unavailable. */
      });
  });
  navEl.appendChild(viewsEl);

  // Recent + pinned runs: re-find the handful of opaque-id runs you touch. Re-rendered
  // on every route so opening a run surfaces it immediately; pins reuse the Runs-table pinned set.
  const runsNavEl = el("div", { class: "nav-runs" });
  const runNavMeta = new Map<string, { model?: string; date?: string }>();
  let runNavHydrationKey = "";
  const hydrateRunsNav = async (ids: string[]): Promise<void> => {
    const unique = [...new Set(ids)].slice(0, 12);
    const key = unique.join("\u0000");
    if (!key || key === runNavHydrationKey) return;
    runNavHydrationKey = key;
    try {
      const statuses = await client.runStatuses();
      for (const row of statuses) {
        if (unique.includes(row.run_id) && row.last_model) {
          runNavMeta.set(row.run_id, { ...runNavMeta.get(row.run_id), model: row.last_model });
        }
      }
    } catch {
      /* Metadata is progressive enhancement; opaque ids remain usable. */
    }
    await Promise.all(
      unique.map(async (id) => {
        try {
          const meta = await client.runMeta(id);
          runNavMeta.set(id, {
            ...runNavMeta.get(id),
            model: runNavMeta.get(id)?.model ?? meta.models[0],
            date: meta.created_date,
          });
        } catch {
          /* Keep the run link even if its provenance is unavailable. */
        }
      })
    );
    renderRunsNav();
  };
  const renderRunsNav = (): void => {
    const pinned = pinnedRuns();
    const recent = recentRuns().filter((r) => !pinned.includes(r)); // pinned shown once, on top
    const children: HTMLElement[] = [];
    const group = (title: string, ids: string[], glyph: IconName) => {
      if (ids.length === 0) return;
      children.push(el("p", { class: "nav-section", text: title }));
      for (const id of ids) {
        const meta = runNavMeta.get(id);
        const detail = [meta?.model, meta?.date].filter(Boolean).join(" · ") || "Run profile";
        children.push(
          el("a", {
            class: "nav-item nav-run",
            href: routePath(["investigate", "run", id]),
            title: id,
            "aria-label": `Open run ${id}, ${detail}`,
          }, [
            el("span", { class: "nav-run-glyph" }, [icon(glyph, { size: 13 })]),
            el("span", { class: "nav-run-copy" }, [
              el("span", { class: "nav-run-id", text: id }),
              el("span", { class: "nav-run-meta", text: detail }),
            ]),
          ])
        );
      }
    };
    group("Pinned runs", pinned.slice(0, 6), "pin");
    group("Recent runs", recent.slice(0, 6), "dot");
    if (pinned.length || recent.length) {
      children.push(
        el("a", {
          class: "nav-item nav-see-all",
          href: routePath(["investigate"], { entity: "runs" }),
          text: "See all runs →",
        })
      );
    }
    runsNavEl.replaceChildren(...children);
    void hydrateRunsNav([...pinned.slice(0, 6), ...recent.slice(0, 6)]);
  };
  renderRunsNav();
  navEl.appendChild(runsNavEl);

  const openSettingsInCurrentWorkspace = (): void => {
    if (currentRoute && isWorkspaceRoute(currentRoute.name)) {
      window.location.hash = routePath(currentRoute.segments, {
        ...(currentRoute.query ?? {}),
        sheet: "settings",
      });
      return;
    }
    // Cold/legacy contexts use the one-release redirect, whose canonical fallback is Pulse.
    navigate("settings");
  };

  const openTrustInCurrentWorkspace = (pricing = false): void => {
    if (currentRoute && isWorkspaceRoute(currentRoute.name)) {
      const query: Record<string, string> = { ...(currentRoute.query ?? {}), sheet: "trust" };
      if (pricing) {
        if (query.view && query.view !== "pricing") query.workspace_view = query.view;
        query.view = "pricing";
      }
      window.location.hash = routePath(currentRoute.segments, query);
      return;
    }
    // Cold/legacy contexts use the compatibility redirect, whose canonical fallback is Pulse.
    navigate(pricing ? "pricing" : "receipts");
  };

  const openCaptureInCurrentWorkspace = (): void => {
    if (currentRoute && isWorkspaceRoute(currentRoute.name)) {
      window.location.hash = routePath(currentRoute.segments, {
        ...(currentRoute.query ?? {}),
        sheet: "capture",
      });
      return;
    }
    navigate("connect");
  };

  const footer = el("div", { class: "nav-footer" }, [
    // The Commands row leads the footer, ABOVE Capture (Connect) and Settings — the single
    // in-app menu surface, opening the same contextual palette the ⌘K shortcut does.
    el("ul", { class: "nav-list" }, [
      el("li", {}, [commandsRailRow(() => void openCommandPalette(), paletteChord)]),
      ...FOOTER.map((f) => el("li", {}, [mkLink(f)])),
    ]),
  ]);
  // Relocated brand tagline: a quiet identity line at the foot of the sidebar.
  footer.appendChild(el("p", { class: "nav-tagline", text: "Cost profiler for AI agents" }));
  navEl.appendChild(footer);
  const settingsLink = links.settings;
  settingsLink?.setAttribute("data-utility-sheet-trigger", "settings");
  settingsLink?.addEventListener("click", (event) => {
    event.preventDefault();
    openSettingsInCurrentWorkspace();
  });
  const trustLink = links.pricing;
  trustLink?.setAttribute("data-utility-sheet-trigger", "trust");
  trustLink?.addEventListener("click", (event) => {
    event.preventDefault();
    openTrustInCurrentWorkspace();
  });
  const captureLink = links.connect;
  captureLink?.setAttribute("data-utility-sheet-trigger", "capture");
  captureLink?.addEventListener("click", (event) => {
    event.preventDefault();
    openCaptureInCurrentWorkspace();
  });

  // ---- contextual toolbar (topbar) ----
  // Breadcrumb-as-nav-stack: a left-aligned navigation landmark; setBreadcrumb fills it
  // with an ordered list whose leaf is the page <h1> + aria-current. Replaces the centered view-title.
  const breadcrumbNav = el("nav", { class: "breadcrumb-nav", "aria-label": "Breadcrumb" });
  // Theme is System/Light/Dark in Settings → Appearance only: no toggle
  // command, topbar control, or native menu item. The OS-follow above re-clamps the accent live.
  // The command surface is the rail "Commands" row — the single in-app menu.
  // The topbar command button and the Windows/Linux topbar hamburger are removed; the global ⌘K/Ctrl-K
  // shortcut stays, and Linux F10 focuses the rail row.
  // Global analysis range: a workspace window the Analyze screens share. Prefs-backed
  // so it survives navigation; changing it re-renders the active screen. Custom shows date inputs.
  const initialRange = rangePref();
  const rangeSel = el(
    "select",
    { class: "range-select", "aria-label": "Analysis time range" },
    RANGE_OPTIONS.map((o) =>
      el("option", { value: o.key, text: o.label, ...(o.key === initialRange.key ? { selected: "" } : {}) })
    )
  ) as HTMLSelectElement;
  const fromInput = el("input", { type: "date", class: "range-date", "aria-label": "From date", value: initialRange.from ?? "" }) as HTMLInputElement;
  const toInput = el("input", { type: "date", class: "range-date", "aria-label": "To date", value: initialRange.to ?? "" }) as HTMLInputElement;
  const rangeMessage = el("span", {
    class: "range-message caption sub",
    id: "analysis-range-message",
    role: "status",
    "aria-live": "polite",
  });
  rangeSel.setAttribute("aria-describedby", "analysis-range-message");
  fromInput.setAttribute("aria-describedby", "analysis-range-message");
  toInput.setAttribute("aria-describedby", "analysis-range-message");
  const syncCustom = (): void => {
    const custom = rangeSel.value === "custom";
    fromInput.hidden = !custom;
    toInput.hidden = !custom;
  };
  const validDate = (value: string): boolean => {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
    const date = new Date(`${value}T00:00:00Z`);
    return !Number.isNaN(date.valueOf()) && date.toISOString().slice(0, 10) === value;
  };
  let rangeAnchor: string | null = null;
  const latestCapturedDay = async (): Promise<string> => {
    if (rangeAnchor) return rangeAnchor;
    try {
      const trend = await client.trend({ by: "total" });
      if (validDate(trend.to)) rangeAnchor = trend.to;
    } catch {
      /* An empty/unavailable store falls back to today's UTC date for a well-formed initial scope. */
    }
    rangeAnchor ??= new Date().toISOString().slice(0, 10);
    return rangeAnchor;
  };
  const materializeRange = async (pref: RangePref): Promise<{ from: string; to: string }> => {
    if (pref.key === "custom" && validDate(pref.from ?? "") && validDate(pref.to ?? "")) {
      return { from: pref.from!, to: pref.to! };
    }
    return rangeToWindow(pref.key === "custom" ? { key: "30d" } : pref, await latestCapturedDay());
  };
  const rangeKeys = new Set<RangeKey>(["7d", "30d", "90d", "custom"]);
  const syncRangeControl = (
    query: Record<string, string> | undefined,
    scope?: { from?: string | null; to?: string | null }
  ): void => {
    const from = query?.from ?? scope?.from ?? "";
    const to = query?.to ?? scope?.to ?? "";
    const requested = query?.range as RangeKey | undefined;
    const key = requested && rangeKeys.has(requested) ? requested : from || to ? "custom" : rangePref().key;
    rangeSel.value = key;
    fromInput.value = from;
    toInput.value = to;
    rangeMessage.textContent = "";
    fromInput.removeAttribute("aria-invalid");
    toInput.removeAttribute("aria-invalid");
    syncCustom();
    setRangePref(key === "custom" ? { key, from: from || undefined, to: to || undefined } : { key });
  };
  let applyRangeToken = 0;
  const applyRange = async (): Promise<void> => {
    if (!currentRoute || !routeHonorsRange(currentRoute)) return;
    const mine = ++applyRangeToken;
    const key = rangeSel.value as RangeKey;
    let pref: RangePref = { key };
    if (key === "custom") {
      const from = fromInput.value;
      const to = toInput.value;
      const valid = validDate(from) && validDate(to) && from <= to;
      // ARIA state attributes require a token value. An empty boolean-style attribute is not
      // equivalent to aria-invalid="true" and can be announced as the default (valid) state.
      for (const input of [fromInput, toInput]) {
        if (valid) input.removeAttribute("aria-invalid");
        else input.setAttribute("aria-invalid", "true");
      }
      if (!valid) {
        rangeMessage.textContent = "Choose a valid From and To date; From must not be after To.";
        return;
      }
      pref = { key, from, to };
    }
    rangeMessage.textContent = key === "custom" ? "Applying custom range…" : `Applying ${key} range…`;
    const concrete = await materializeRange(pref);
    if (mine !== applyRangeToken || !currentRoute) return;
    setRangePref(pref);
    const current = analysisStore.get();
    analysisStore.set({
      scope: { ...current.scope, ...concrete },
      // A selection/baseline belongs to the old window. Keeping it would make the control appear to
      // change results while the canvas continued resolving a stale nested cohort.
      selection: null,
      baseline: null,
    });
    const query = {
      ...(currentRoute.query ?? {}),
      investigation: "",
      from: concrete.from,
      to: concrete.to,
      tz: current.scope.timezone,
      range: key,
    };
    window.location.hash = routePath(currentRoute.segments, query);
  };
  rangeSel.addEventListener("change", () => {
    syncCustom();
    if (rangeSel.value === "custom") {
      const scope = analysisStore.get().scope;
      fromInput.value ||= scope.from ?? "";
      toInput.value ||= scope.to ?? "";
      if (!fromInput.value || !toInput.value) {
        rangeMessage.textContent = "Choose both dates to apply a custom range.";
        fromInput.focus();
        return;
      }
    }
    void applyRange();
  });
  fromInput.addEventListener("change", () => void applyRange());
  toInput.addEventListener("change", () => void applyRange());
  syncCustom();
  const rangeControl = el("span", { class: "range-control", title: "Investigate time range" }, [rangeSel, fromInput, toInput, rangeMessage]);

  // Explicit leading / center / trailing zones instead of one elastic spacer: lead = breadcrumb,
  // optional range control, and contextual action; center = flexible drag space; trail = the
  // Max-inspect warning when active. Tauri ignores the `-webkit-app-region` CSS in desktop.css, and
  // its data marker is self-targeted, so each passive zone gets the marker; links/inputs stay active.
  const topbarActions = el("span", { class: "topbar-actions" });
  // a loud, always-visible indicator whenever Max inspect is active — redacted
  // request/response bodies are being stored. Privacy-first: make the trade explicit app-wide + link to
  // Settings (review/purge/disable). Hidden by default; shown once config resolves to max_inspect.
  const captureBanner = el(
    "a",
    {
      class: "capture-banner",
      href: routeHash("settings"),
      title: "Redacted request/response bodies are being captured (Max inspect). Click to review or purge.",
      hidden: "",
    },
    [icon("dot", { size: 12, class: "capture-dot" }), el("span", { text: "Capturing redacted bodies" })]
  );
  captureBanner.addEventListener("click", (event) => {
    event.preventDefault();
    openSettingsInCurrentWorkspace();
  });
  const topbar = el("header", { class: "topbar", ...dragRegion }, [
    el("div", { class: "topbar-lead" }, [breadcrumbNav, rangeControl, topbarActions]),
    el("div", { class: "topbar-center", ...dragRegion }, []),
    el("div", { class: "topbar-trail" }, [captureBanner]),
  ]);
  void (async () => {
    try {
      const cfg = await client.config();
      if (cfg?.privacy?.profile === "max_inspect") captureBanner.removeAttribute("hidden");
    } catch {
      /* config unavailable / unsupported → leave the banner hidden */
    }
  })();

  // id + tabindex make <main> the skip-link target (WCAG 2.4.1 Bypass Blocks).
  const main = el("main", { class: "main", id: "main", tabindex: "-1" });

  // Workspace status bar: capture/connection, the SCOPED cohort count+spend,
  // freshness, and any trust warning — NOT an unrelated Today total (Pulse owns the primary spend
  // figure; the native tray-title mirror keeps the at-a-glance total). Replaces the old Today/burn tape.
  const statusBar = createStatusBar(el);
  const statusbar = statusBar.el;

  // Skip-to-content bypass (WCAG 2.4.1): the first focusable element jumps past the ~20-link
  // sidebar straight to <main>, so keyboard/SR users don't re-tab it on every navigation.
  const skipLink = el("a", { class: "skip-link", href: "#main", text: "Skip to content" });
  const navDrawerBackdrop = el("div", {
    class: "nav-drawer-backdrop",
    hidden: "",
    "aria-hidden": "true",
  });
  const shell = el("div", { class: "shell" }, [
    skipLink,
    brand,
    topbar,
    navDrawerBackdrop,
    navEl,
    main,
    statusbar,
  ]);
  root.appendChild(shell);
  // At stacked/single widths the rail becomes an accessible modal drawer. It is the SAME navigation
  // node used on desktop, so destinations, saved views, recents, Commands, and utility-sheet links
  // cannot drift between responsive modes. Background chrome/content is inert while open; Tab stays
  // inside the drawer; Escape/backdrop close it; focus returns to the explicit workspace switcher.
  const drawerFocusable =
    'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), details > summary, [tabindex]:not([tabindex="-1"])';
  const drawerBackground = [skipLink, brand, topbar, main, statusbar];
  let navDrawerOpen = false;
  const closeNavDrawer = (restoreFocus = true): void => {
    if (!navDrawerOpen) return;
    navDrawerOpen = false;
    navDrawerTrigger.setAttribute("aria-expanded", "false");
    shell.setAttribute("data-nav-drawer-state", "closing");
    afterTransition(navEl, "transform", () => {
      shell.classList.remove("nav-drawer-open");
      shell.removeAttribute("data-nav-drawer-state");
      navDrawerBackdrop.setAttribute("hidden", "");
      for (const node of drawerBackground) node.removeAttribute("inert");
      if (restoreFocus && navDrawerTrigger.getClientRects().length > 0) {
        queueMicrotask(() => navDrawerTrigger.focus());
      }
    });
  };
  const openNavDrawer = (): void => {
    if (navDrawerOpen) return;
    navDrawerOpen = true;
    shell.classList.add("nav-drawer-open");
    navDrawerBackdrop.removeAttribute("hidden");
    beginMotion(shell, "data-nav-drawer-state");
    navDrawerTrigger.setAttribute("aria-expanded", "true");
    for (const node of drawerBackground) node.setAttribute("inert", "");
    queueMicrotask(() => navDrawerClose.focus());
  };
  navDrawerTrigger.addEventListener("click", openNavDrawer);
  navDrawerClose.addEventListener("click", () => closeNavDrawer());
  navDrawerBackdrop.addEventListener("click", () => closeNavDrawer());
  navEl.addEventListener("click", (event) => {
    if ((event.target as Element | null)?.closest("a[href]")) closeNavDrawer();
  });
  navEl.addEventListener("keydown", (event) => {
    if (!navDrawerOpen) return;
    if (event.key === "Escape") {
      event.preventDefault();
      closeNavDrawer();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = Array.from(navEl.querySelectorAll<HTMLElement>(drawerFocusable)).filter(
      (node) =>
        !node.hasAttribute("hidden") &&
        node.getAttribute("aria-hidden") !== "true" &&
        !node.closest("[hidden]")
    );
    if (focusable.length === 0) {
      event.preventDefault();
      navEl.focus();
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
  // Mark when the semantic frame reaches the DOM, before asynchronous data hydration. Startup tests
  // Ensure the app never waits for data before showing useful structure.
  try {
    performance.mark("tare:shell-ready");
  } catch {
    /* Performance API absent (sandbox) — instrumentation is best-effort. */
  }
  // Container-query layout state machine: stamp the mode on `.shell` so CSS re-flows the panes
  // by the workbench's own inline-size (Windows-Snap-safe), never a horizontal route strip.
  shell.dataset.layout = "full";
  observeWorkbench(shell, (mode) => {
    shell.dataset.layout = mode;
    if (mode === "full" || mode === "rail-icons") closeNavDrawer(false);
    main.querySelector<HTMLElement>("[data-adaptive-panes]")?.dispatchEvent(
      new CustomEvent("tare:layoutchange", { detail: mode })
    );
  });

  const ALERT_SEEN_KEY = "tare.alerts.seen.v1";
  let alertPollInFlight = false;
  const readSeenAlertKeys = (): string[] => {
    try {
      const parsed = JSON.parse(localStorage.getItem(ALERT_SEEN_KEY) ?? "[]");
      return Array.isArray(parsed) ? parsed.filter((key): key is string => typeof key === "string") : [];
    } catch {
      return [];
    }
  };
  const writeSeenAlertKeys = (keys: string[]): void => {
    try {
      localStorage.setItem(ALERT_SEEN_KEY, JSON.stringify(keys));
    } catch {
      /* Storage unavailable: alerts still render for this poll. */
    }
  };
  const dayAtOffset = (offset: number | undefined): string => {
    const safe = Number.isFinite(offset) ? Math.max(-840, Math.min(840, Math.trunc(offset!))) : 0;
    return new Date(Date.now() + safe * 60_000).toISOString().slice(0, 10);
  };

  // Evaluate alert rules locally from the same read APIs that drive Pulse. Persistent, bounded keys
  // make a 30-second poll fire once per day/anomaly rather than becoming notification spam.
  const refreshAlerts = async (today: TodaySpend): Promise<void> => {
    if (alertPollInFlight) return;
    alertPollInFlight = true;
    try {
      const cfg = await client.config();
      const rules = cfg.alert ?? [];
      const needsRunRate = rules.some((rule) => rule.metric === "run_rate");
      const needsEventCount = rules.some((rule) => rule.min_events != null);
      const [budget, burnrate, anomalies, statuses] = await Promise.all([
        client.budget().catch(
          (): PeriodBudget => ({
            period: cfg.budget.period ?? "month",
            spent_micros: 0,
            cap_micros: 0,
            warn_pct: 80,
            pct: 0,
            status: "ok",
          })
        ),
        needsRunRate ? client.burnrate().catch((): BurnRate | undefined => undefined) : undefined,
        client.anomalies({ by: "total" }).catch((): Anomaly[] => []),
        needsEventCount ? client.runStatuses().catch(() => []) : [],
      ]);
      const windows = [
        ...new Set(
          rules
            .filter((rule) => rule.metric === "anomaly_kind" && rule.window_days != null)
            .map((rule) => rule.window_days!)
        ),
      ];
      const anomaliesByWindow: Record<string, Anomaly[]> = {};
      await Promise.all(
        windows.map(async (window) => {
          anomaliesByWindow[String(window)] = await client
            .anomalies({ by: "total", window })
            .catch(() => []);
        })
      );
      const capturedEvents = statuses.reduce(
        (total, status) => Math.min(Number.MAX_SAFE_INTEGER, total + Math.max(0, status.steps)),
        0
      );
      const notices = evaluateAlertNotices(rules, {
        today,
        budget,
        burnrate,
        anomalies,
        anomaliesByWindow,
        capturedEvents,
        day: dayAtOffset(cfg.ui?.tz_offset_minutes),
      });
      const selected = selectNewAlertNotices(notices, readSeenAlertKeys());
      if (selected.fresh.length === 0) return;
      writeSeenAlertKeys(selected.seenKeys);

      const individual = selected.fresh.slice(0, 3);
      for (const notice of individual) {
        showToast(notice.message, notice.level);
        if (client.canNotify()) {
          void client.notify("Tare alert", notice.message).catch(() => undefined);
        }
      }
      const remaining = selected.fresh.length - individual.length;
      if (remaining > 0) {
        const message = `${remaining} additional alert${remaining === 1 ? "" : "s"} detected`;
        showToast(message, "anomaly");
        if (client.canNotify()) void client.notify("Tare alerts", message).catch(() => undefined);
      }
    } catch {
      // Monitoring is best-effort and must not turn a healthy Today heartbeat red when an auxiliary
      // endpoint or config read is temporarily unavailable.
    } finally {
      alertPollInFlight = false;
    }
  };

  // Poll `today()` as the connection/freshness heartbeat and as the local alert monitor's spend
  // observation. The status bar itself still leaves Today totals to Pulse / the native tray.
  function refreshToday(): void {
    client
      .today()
      .then((today) => {
        statusBar.setConnection(true);
        statusBar.setFreshness(`Updated ${timeLabel()}`);
        void refreshAlerts(today);
      })
      .catch(() => {
        statusBar.setConnection(false);
      });
  }

  // ---- routing ----
  let renderToken = 0;
  async function renderActive(route: Route): Promise<void> {
    const mine = ++renderToken;
    // Materialize an ambient rolling preference into concrete URL + CohortSpec dates before the
    // canonical Investigate workspace loads. A deep link (or saved investigation) always wins over
    // local preference; concrete dates keep reload/share/back behavior deterministic.
    if (routeHonorsRange(route) && !route.query?.investigation && !route.query?.from && !route.query?.to) {
      const pref = rangePref();
      const concrete = await materializeRange(pref);
      if (mine !== renderToken) return;
      route = {
        ...route,
        query: {
          ...(route.query ?? {}),
          from: concrete.from,
          to: concrete.to,
          tz: analysisStore.get().scope.timezone,
          range: pref.key === "custom" ? "custom" : pref.key,
        },
      };
      window.history.replaceState(null, "", hashOf(route));
      currentRoute = route;
    }
    // The pane itself may own shell-wide state (utility-sheet background inertness). Release that
    // before any route replaces it, including navigation triggered from inside an open sheet.
    main.firstElementChild?.dispatchEvent(new Event("tare:dispose"));
    // Workspaces may own a document-scoped shortcut while mounted (the Investigate Space-Peek
    // gesture). Give the outgoing workspace a deterministic teardown point before replacing it.
    main.querySelector<HTMLElement>("[data-adaptive-panes]")?.dispatchEvent(new Event("tare:dispose"));
    for (const n of allNav()) {
      const isActive = n.name === route.name;
      const link = links[n.name];
      link?.classList.toggle("active", isActive);
      // Expose the current location to assistive tech, not only via the .active class (WCAG 4.1.2).
      if (link) {
        if (isActive) link.setAttribute("aria-current", "page");
        else link.removeAttribute("aria-current");
      }
    }
    // Hide the global range control on screens that don't honor it, so it's never an
    // inert control the user can't trust; the center zone collapses when it's hidden.
    rangeControl.hidden = !routeHonorsRange(route);
    if (!rangeControl.hidden) syncRangeControl(route.query);
    setBreadcrumb(breadcrumbNav, route);
    // Per-screen contextual action in the toolbar's leading slot. Cleared on every navigation; the
    // slot collapses when empty (.topbar-actions:empty). Route-keyed so screens stay pure render-into-
    // container functions; data-coupled actions stay with the surfaces that own their data.
    const action = topbarActionFor(route);
    topbarActions.replaceChildren(...(action ? [action] : []));
    const pane = el("div", { class: "pane" });
    // Seed a skeleton so a view-switch shows structure immediately, not a blank pane, while the
    // screen does its async import/fetch (no-flash switch). The screen's own first
    // replaceChildren swaps it out.
    pane.appendChild(skelScreen());
    main.replaceChildren(pane);
    try {
      if (isWorkspaceRoute(route.name)) {
        // Canonical workspace: the workbench lazily imports the adapter and renders it, so an
        // unopened workspace is never in the boot graph. Never a placeholder.
        await renderWorkspace(route.name, pane, client, route, { analysis: analysisStore });
      } else {
        const screen = SCREENS[route.name] ?? placeholder(labelFor(route));
        await screen(pane, client, route.param);
      }
    } catch (e) {
      if (mine === renderToken) {
        main.replaceChildren(
          errorNode("Couldn't render this screen. Your current view and filters are unchanged.", e, {
            actions: [
              {
                label: "Retry",
                primary: true,
                run: () => (currentRoute ? renderActive(currentRoute) : Promise.resolve()),
              },
              { label: "Back to Pulse", href: routeHash("pulse") },
            ],
          })
        );
      }
    }
    if (mine === renderToken && !rangeControl.hidden && route.query?.investigation) {
      syncRangeControl(route.query, analysisStore.get().scope);
    }
    if (mine === renderToken) statusBar.setFreshness(`Updated ${timeLabel()}`);
  }

  // ---- command palette (⌘K) — one contextual registry ----
  // The selection's own id (from the open entity) so a run route can name it in the selection group.
  function selectionFromRoute(r: Route | null): CommandSelection | null {
    if (!r) return null;
    if (r.name === "runs" && r.param) return { kind: "run", id: r.param };
    if (r.name === "investigate" && r.segments[1] === "run" && r.segments[2]) {
      return { kind: "run", id: r.segments[2] };
    }
    return null;
  }
  async function buildCommands(): Promise<Command[]> {
    // Contextual commands for the current selection LEAD the palette.
    const selection = selectionCommands(
      { selection: selectionFromRoute(currentRoute) },
      {
        open: (id) => {
          window.location.hash = routePath(["investigate", "run", id]);
        },
        compare: (id) => {
          window.location.hash = routePath(["investigate", "compare"], { runs: id });
        },
      }
    );

    const nav: RegistryCommand[] = SECTIONS.flatMap((section) => section.items).map((n) => ({
      id: navId(n.name), // shared with the native menu's nav:<route> ids
      title: `Go to ${n.label}`,
      run: () => navigate(n.name),
      hint: GOTO_CHORDS[n.name], // teach the chord in the ⌘K row (undefined = no chord)
      group: "primary" as const,
    }));

    // Saved views remain reachable from the command surface when the rail collapses to 60px and
    // hides its text-heavy shortcut groups. This is the same canonical destination as the expanded
    // rail row, not a second navigation model.
    const savedViews: RegistryCommand[] = savedInvestigations.map((view) => ({
      id: `view:${view.id}`,
      title: view.label,
      subtitle: investigationScopeLabel(view),
      keywords: "open saved investigation view",
      run: () => {
        window.location.hash = investigationHref(view);
      },
      group: "saved" as const,
    }));

    const actions: RegistryCommand[] = [
      // Theme is not a command: System/Light/Dark lives only in Settings → Appearance.
      {
        // In-page find: the command-registry entry point, shared with the native
        // macOS Edit▸Find item + the desktop Mod+F hotkey (all route here).
        id: ACTION.find,
        title: "Find in page…",
        group: "actions",
        run: () => openFind(),
      },
      {
        id: ACTION.saveView,
        title: "Save current view…",
        group: "actions",
        run: async () => {
          // Capture the canonical analytical route without a transient utility sheet or a prior
          // investigation id. The visible title describes the view; scope details stay in metadata.
          const r = currentRoute;
          const cleanQuery = Object.fromEntries(
            Object.entries(r?.query ?? {}).filter(([key, value]) =>
              key !== "sheet" && key !== "investigation" && value !== ""
            )
          );
          const hash = r ? routePath(r.segments, cleanQuery) : "#/pulse";
          const label = r ? canonicalViewTitle(r) : "Pulse";
          const now = new Date().toISOString();
          const active = {
            ...analysisStore.get(),
            workspace: routeToWorkspace(r?.name ?? "investigate"),
          };
          const inv = investigationFromState(hash, label, active, now);
          const prior = savedInvestigations.find((row) => row.id === inv.id);
          if (prior) inv.created_at = prior.created_at;
          try {
            await client.saveInvestigation(inv);
            savedInvestigations = [inv, ...savedInvestigations.filter((row) => row.id !== inv.id)];
          } catch {
            /* v2 SQLite is the sole store; a failed write leaves the prior list unchanged */
          }
          renderViews();
        },
      },
    ];
    if (client.canControlProxy?.()) {
      actions.push({
        id: ACTION.proxy,
        title: "Start / stop proxy channel",
        group: "actions",
        run: async () => {
          const s = await client.proxyStatus();
          if (s.running) await client.proxyStop();
          else await client.proxyStart();
        },
      });
    }

    actions.push(
      {
        id: navId("connect"),
        title: "Open Capture",
        group: "actions",
        run: openCaptureInCurrentWorkspace,
      },
      {
        id: navId("pricing"),
        title: "Open Trust & pricing",
        group: "actions",
        run: () => openTrustInCurrentWorkspace(true),
      },
      {
        id: navId("settings"),
        title: "Open Settings",
        hint: formatShortcut("Mod+,"),
        group: "actions",
        run: openSettingsInCurrentWorkspace,
      }
    );

    const runs: RegistryCommand[] = recentRuns().slice(0, 10).map((id) => {
      const meta = runNavMeta.get(id);
      return {
        id: runId(id),
        title: id,
        subtitle: [meta?.model, meta?.date].filter(Boolean).join(" · ") || "Run profile",
        keywords: "open run",
        run: () => {
          window.location.hash = routePath(["investigate", "run", id]);
        },
        group: "recent" as const,
      };
    });

    const advanced: RegistryCommand[] = LENS_ROUTES.map((lens) => ({
      id: `lens:${lens.name}`,
      title: lens.label,
      subtitle: lens.description,
      keywords: "lens investigate optimize",
      run: () => {
        window.location.hash = lens.href;
      },
      group: "advanced" as const,
    }));

    // Stable, visible sections make a long command list scan like the product's information
    // architecture: context → primary → saved → recent → actions → advanced.
    return orderCommands([...selection, ...nav, ...savedViews, ...runs, ...actions, ...advanced]);
  }
  async function openCommandPalette(): Promise<void> {
    openPalette(await buildCommands());
  }
  installPaletteHotkey(() => void openCommandPalette());
  installCommandKey(() => void openCommandPalette()); // ":" opens the command surface.
  window.addEventListener("keydown", (event) => {
    if (matchShortcut(event, "Mod+,")) {
      event.preventDefault();
      openSettingsInCurrentWorkspace();
    }
  });
  // Linux menu convention: F10 opens the single in-app menu surface (the rail Commands row's
  // palette). Guardless of focus scope — F10 is a dedicated menu key, not an unscoped letter.
  window.addEventListener("keydown", (e) => {
    if (e.key === "F10" && !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey) {
      e.preventDefault();
      void openCommandPalette();
    }
  });
  // Native View-menu actions (desktop) reach the app via these DOM events, bridged from the Rust
  // menu emits by bootTauri.wireMenuActions.
  window.addEventListener("tare:open-palette", () => void openCommandPalette());
  // In-page find: the native macOS Edit▸Find item bridges here; the desktop Mod+F
  // hotkey opens it directly (browser Cmd/Ctrl-F stays native — installFindHotkey is desktop-gated).
  window.addEventListener("tare:find", () => openFind());
  installFindHotkey();
  // No tare:toggle-theme bridge: the native menu no longer emits a theme toggle.
  // Leader-key go-to router: `g <letter>` jumps to a destination — the keyboard
  // complement to ⌘K, from the same GOTO_CHORDS source of truth.
  const gotoHotkeys: Hotkey[] = Object.entries(GOTO_CHORDS).map(([name, seq]) => ({
    seq,
    label: `Go to ${baseLabel(name)}`,
    run: () => (name === "pricing" ? openTrustInCurrentWorkspace() : navigate(name)),
  }));
  installHotkeys(gotoHotkeys);
  installSlashSearch(); // "/" focuses page search without fighting native find-in-page.

  refreshToday();
  // Keep the today pill + connection dot fresh while the app is open (the tray already polls ~30s;
  // the web shell didn't) — stale while open. Lives for the session (SPA).
  setInterval(refreshToday, 30_000);

  if (!isOnboarded() && (window.location.hash === "" || window.location.hash === "#/")) {
    window.location.hash = routeHash("onboarding");
  } else if (window.location.hash === "" || window.location.hash === "#/") {
    // Honor the user's default landing screen when there's no deep-link hash,
    // migrated to its canonical workspace: a stored `live`/`overview` lands
    // directly on Pulse rather than taking a redirect hop.
    window.location.hash = routeHash(migrateDefaultScreen(defaultScreen()));
  }

  // Dedupe consecutive identical routes: navigating to the same hash (e.g. the immediate fire
  // plus a queued hashchange from setting location.hash) must not re-render and clobber in-screen
  // state (a clicked tab, a filled form).
  let activeKey = "";
  let currentRoute: Route | null = null;
  let firstDone: Promise<void> | null = null;
  onRoute((r) => {
    // Fully apply the compatibility matrix before dispatch. Old bookmarks and native-menu hashes
    // keep working, but users see one coherent Pulse / Investigate / Optimize information model.
    const routed = redirectRoute(r);
    if (routed !== r) window.history.replaceState(null, "", hashOf(routed));
    currentRoute = routed;
    // Key on name+param+query (sorted) so a view-state change (?by=…&win=…) re-renders too.
    const q = routed.query
      ? Object.keys(routed.query)
          .sort()
          .map((k) => `${k}=${routed.query![k]}`)
          .join("&")
      : "";
    const key = `${routed.name}/${routed.param ?? ""}?${q}`;
    if (key === activeKey) return;
    activeKey = key;
    const p = renderActive(routed);
    // Refresh the sidebar recents/pinned after the screen renders (a run detail records itself).
    void p.finally(() => renderRunsNav());
    if (!firstDone) firstDone = p;
  });
  await firstDone;
}
