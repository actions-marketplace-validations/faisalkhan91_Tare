// Workbench coordinator. The canonical Pulse, Investigate, and Optimize workspaces load through
// dynamic `import()` only when opened, keeping unopened workspaces out of the initial boot graph.
// `main.ts` delegates canonical-route rendering here and remains a thin mount/router coordinator.

import { setRouteQuery, type Route } from "../ui/store.js";
import type { TareClient } from "../client.js";
import { errorNode } from "../ui/errorNode.js";
import { createUtilitySheet } from "../ui/sheet.js";
import {
  defaultFocus,
  hydrateFromQuery,
  type AnalysisStore,
} from "../analysis/store.js";

export type WorkspaceName = "pulse" | "investigate" | "optimize";

/// Shared, boot-owned services available to every lazily loaded workspace. Keeping the analysis
/// store in this context gives Pulse, Investigate, and Optimize one interaction spine without
/// pulling any workspace module into the initial bundle.
export interface WorkspaceContext {
  analysis: AnalysisStore;
}

/// A workspace renderer reads the full Route (segments + query) so it can map canonical state onto
/// its content — unlike the legacy `Screen` signature that only saw an opaque `param`.
export type WorkspaceRenderer = (
  root: HTMLElement,
  client: TareClient,
  route: Route,
  context: WorkspaceContext
) => Promise<void>;

// Dynamic-import loaders: the workspace module is fetched on FIRST open, never at boot.
const LOADERS: Record<WorkspaceName, () => Promise<WorkspaceRenderer>> = {
  pulse: () => import("../workspaces/pulse.js").then((m) => m.renderPulse),
  investigate: () => import("../workspaces/investigate.js").then((m) => m.renderInvestigate),
  optimize: () => import("../workspaces/optimize.js").then((m) => m.renderOptimize),
};

// Utility code is loaded only when its route query opens a sheet. In particular, Settings does not
// join the startup graph or replace the current analytical workspace.
const SETTINGS_LOADER = () =>
  import("../screens/settings.js").then((module) => module.renderSettings);
const CAPTURE_LOADER = () =>
  import("../screens/capture.js").then((module) => module.renderCapture);
const TRUST_LOADER = () =>
  import("../screens/trust.js").then((module) => module.renderTrust);

/**
 * Make everything behind a utility sheet genuinely inert, not just the workspace fragment. The
 * dialog lives inside `.main`, so inerting `.main` itself would also disable the dialog; instead we
 * inert the pane's existing children plus every sibling shell region (rail, toolbar, status, brand).
 * Only attributes introduced here are removed, preserving any independently-inert ancestor state.
 */
function inertUtilityBackground(root: HTMLElement): () => void {
  const main = root.closest<HTMLElement>(".main");
  const shell = main?.closest<HTMLElement>(".shell");
  const candidates = [
    ...Array.from(root.children),
    ...Array.from(shell?.children ?? []).filter((child) => child !== main),
  ].filter((node): node is HTMLElement => node instanceof HTMLElement);
  const changed = candidates.filter((node) => !node.hasAttribute("inert"));
  for (const node of changed) node.setAttribute("inert", "");
  let released = false;
  return () => {
    if (released) return;
    released = true;
    for (const node of changed) node.removeAttribute("inert");
  };
}

async function renderSettingsSheet(
  root: HTMLElement,
  client: TareClient,
  route: Route
): Promise<void> {
  const releaseBackground = inertUtilityBackground(root);
  root.addEventListener("tare:dispose", releaseBackground, { once: true });
  const returnFocus = document.querySelector<HTMLElement>(
    '[data-utility-sheet-trigger="settings"]'
  );
  const sheet = createUtilitySheet("Settings", () => {
    releaseBackground();
    setRouteQuery({ sheet: "", settings: "" });
    queueMicrotask(() => returnFocus?.focus());
  });
  root.appendChild(sheet.backdrop);
  try {
    const renderSettings = await SETTINGS_LOADER();
    await renderSettings(sheet.body, client, route.query?.settings);
    sheet.focus();
  } catch (error) {
    releaseBackground();
    throw error;
  }
}

async function renderTrustSheet(
  root: HTMLElement,
  client: TareClient,
  route: Route,
  context: WorkspaceContext
): Promise<void> {
  const releaseBackground = inertUtilityBackground(root);
  root.addEventListener("tare:dispose", releaseBackground, { once: true });
  const returnFocus = document.querySelector<HTMLElement>(
    '[data-utility-sheet-trigger="trust"]'
  );
  const sheet = createUtilitySheet("Trust & pricing", () => {
    releaseBackground();
    const restoreView =
      route.query?.view === "pricing" ? route.query?.workspace_view ?? "" : route.query?.view ?? "";
    setRouteQuery({ sheet: "", view: restoreView, workspace_view: "" });
    queueMicrotask(() => returnFocus?.focus());
  });
  root.appendChild(sheet.backdrop);
  try {
    const renderTrust = await TRUST_LOADER();
    await renderTrust(sheet.body, client, route, context);
    sheet.focus();
  } catch (error) {
    releaseBackground();
    throw error;
  }
}

async function renderCaptureSheet(
  root: HTMLElement,
  client: TareClient
): Promise<void> {
  const releaseBackground = inertUtilityBackground(root);
  root.addEventListener("tare:dispose", releaseBackground, { once: true });
  const returnFocus = document.querySelector<HTMLElement>(
    '[data-utility-sheet-trigger="capture"]'
  );
  const sheet = createUtilitySheet("Capture", () => {
    releaseBackground();
    setRouteQuery({ sheet: "" });
    queueMicrotask(() => returnFocus?.focus());
  });
  root.appendChild(sheet.backdrop);
  try {
    const renderCapture = await CAPTURE_LOADER();
    await renderCapture(sheet.body, client);
    sheet.focus();
  } catch (error) {
    releaseBackground();
    throw error;
  }
}

/// Whether `name` is a canonical workspace route the workbench owns.
export function isWorkspaceRoute(name: string): name is WorkspaceName {
  return name === "pulse" || name === "investigate" || name === "optimize";
}

const INVESTIGATE_VIEW_ENTITIES = new Set(["runs", "sessions", "templates", "steps", "time"]);

/// Hydrate the boot-owned analysis store before a workspace renders. Investigate's plural
/// `entity=runs|sessions|templates|steps|time` values are workspace VIEW modes from the compatibility
/// route contract, not the cohort grain (`run|step`), so they are consumed by Investigate and
/// deliberately excluded from CohortSpec hydration. Unknown values still reach the decoder and fail
/// visibly rather than being guessed.
export async function hydrateWorkspaceAnalysis(
  store: AnalysisStore,
  name: WorkspaceName,
  route: Route,
  client?: TareClient
): Promise<void> {
  const investigationId = route.query?.investigation;
  if (investigationId) {
    if (!client) throw new Error("saved-investigation client is unavailable");
    const saved = (await client.listInvestigations()).find((row) => row.id === investigationId);
    if (!saved) throw new Error(`saved investigation ${investigationId} was not found`);
    // The route owns the active destination; the saved record owns every other durable field.
    // Focus is transient and is always reconstructed rather than read from persistence.
    store.set({ ...saved.state, workspace: name, focus: defaultFocus() });
    return;
  }
  const current = store.get();
  const query = { ...(route.query ?? {}) };
  if (name === "investigate" && query.entity && INVESTIGATE_VIEW_ENTITIES.has(query.entity)) {
    delete query.entity;
  }
  const base = {
    ...current,
    workspace: name,
    focus: current.workspace === name ? current.focus : defaultFocus(),
  };
  store.set(hydrateFromQuery(base, query));
}

/// Lazily load and render the canonical workspace for `route`. The import happens here so the
/// workspace's code stays out of the initial boot graph.
export async function renderWorkspace(
  name: WorkspaceName,
  root: HTMLElement,
  client: TareClient,
  route: Route,
  context: WorkspaceContext
): Promise<void> {
  try {
    await hydrateWorkspaceAnalysis(context.analysis, name, route, client);
  } catch (error) {
    // Keep ordinary inline-codec validation on the existing shell error path. This message is only
    // for a compact saved-investigation link that could not be resolved.
    if (!route.query?.investigation) throw error;
    root.replaceChildren(
      errorNode("Couldn't open the saved investigation. It may be unavailable or was removed.", error, {
        actions: [
          {
            label: "Retry",
            primary: true,
            run: () => renderWorkspace(name, root, client, route, context),
          },
          { label: "Back to Pulse", href: "#/pulse" },
        ],
      })
    );
    return;
  }
  const render = await LOADERS[name]();
  await render(root, client, route, context);
  if (route.query?.sheet === "settings") {
    await renderSettingsSheet(root, client, route);
  } else if (route.query?.sheet === "capture") {
    await renderCaptureSheet(root, client);
  } else if (route.query?.sheet === "trust") {
    await renderTrustSheet(root, client, route, context);
  }
}
