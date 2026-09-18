// Framework-free hash router.

export interface Route {
  name: string;
  /// The full decoded path segments, e.g. `#/investigate/run/a%2Fb` -> `["investigate","run","a/b"]`.
  /// `segments[0] === name`. Each segment is encoded/decoded INDEPENDENTLY so an id containing `/`
  /// round-trips as a single segment instead of being split. New code reads `segments`.
  segments: string[];
  /// Compatibility view of the route tail: `segments.slice(1).join("/")`, or absent when empty.
  param?: string;
  /// View state encoded after `?` (dimension/window/filter/group) so any drilled-in view is
  /// deep-linkable, reloadable, and saveable. Absent when there's no query.
  query?: Record<string, string>;
}

/// decodeURIComponent that never throws (a malformed `%` sequence in a hand-typed hash falls back
/// to the raw segment rather than crashing the router).
function safeDecode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/// Assemble a Route from decoded segments, keeping the object shape stable: `param`/`query` keys are
/// present only when non-empty (so existing `toEqual` shape expectations hold, plus the new
/// `segments`). `segments` is always present.
function makeRoute(segments: string[], query?: Record<string, string>): Route {
  const name = segments[0] ?? "";
  const r: Route = { name, segments };
  if (segments.length > 1) r.param = segments.slice(1).join("/");
  if (query) r.query = query;
  return r;
}

/// Parse a location hash into a route. The path is `#/seg0[/seg1[/seg2…]]`; an optional `?k=v&…`
/// tail becomes `query`. Each segment is decoded independently so a run id containing `/` (encoded
/// `%2F`) stays a single segment. e.g. `#/investigate/run/a%2Fb?by=model` ->
/// {name:"investigate", segments:["investigate","run","a/b"], param:"run/a/b", query:{by:"model"}}.
/// "" / "#" / "#/" -> {name:"pulse", segments:["pulse"]} (the cold-start default). Was "overview"
/// A retired screen only worked because a redirect rewrote it; default
/// straight to the real current home so the empty hash resolves even if the redirect gate changes.
export function parseHash(hash: string): Route {
  const raw = hash.replace(/^#\/?/, "");
  const [path, queryStr] = raw.split("?", 2);
  const query = parseQueryStr(queryStr);
  if (path === "") return makeRoute(["pulse"], query);
  const segments = path.split("/").map(safeDecode);
  return makeRoute(segments, query);
}

function parseQueryStr(s: string | undefined): Record<string, string> | undefined {
  if (!s) return undefined;
  const out: Record<string, string> = {};
  new URLSearchParams(s).forEach((v, k) => {
    out[k] = v;
  });
  return Object.keys(out).length ? out : undefined;
}

/// Serialize a sorted, empty-dropped query string (shared by `routePath`/`routeHash`). Returns "" for
/// an absent/empty query so callers can decide whether to append `?`.
function queryString(query?: Record<string, string>): string {
  if (!query) return "";
  const p = new URLSearchParams();
  for (const k of Object.keys(query).sort()) {
    if (query[k] !== undefined && query[k] !== "") p.set(k, query[k]);
  }
  return p.toString();
}

/// Build a hash from decoded segments (Router v2), encoding each segment independently so an id with
/// `/` round-trips. e.g. (["investigate","run","a/b"], {by:"model"}) -> "#/investigate/run/a%2Fb?by=model".
/// `query` keys are sorted so a view always serializes identically.
export function routePath(segments: string[], query?: Record<string, string>): string {
  const base = `#/${segments.map(encodeURIComponent).join("/")}`;
  const qs = queryString(query);
  return qs ? `${base}?${qs}` : base;
}

/// The canonical hash for a Route object (inverse of `parseHash`).
export function hashOf(route: Route): string {
  return routePath(route.segments, route.query);
}

/// COMPATIBILITY overload: the old name+param signature translates to segments
/// — `param` becomes a single trailing segment (encoded whole, so an id with `/` still round-trips).
/// e.g. ("runs","a/b",{by:"model"}) -> "#/runs/a%2Fb?by=model".
export function routeHash(name: string, param?: string, query?: Record<string, string>): string {
  const segments = param === undefined ? [name] : [name, param];
  return routePath(segments, query);
}

/// Merge `patch` into the CURRENT route's query and navigate (a screen updating its own view
/// state). Removing a key: pass an empty string. Reads/writes through the hash so it's
/// deep-linkable. `win` injectable for tests.
export function setRouteQuery(patch: Record<string, string>, win: Window = window): void {
  const cur = parseHash(win.location.hash);
  const merged = { ...(cur.query ?? {}), ...patch };
  // Rebuild from segments (not param) so a multi-segment canonical route (e.g. investigate/run/:id)
  // keeps its structure — the compat `param` join would re-encode `/` and collapse the segments.
  win.location.hash = routePath(cur.segments, merged);
}

/// Subscribe to route changes; fires once immediately with the current route. Returns an
/// unsubscribe. `win` is injectable for tests.
export function onRoute(fn: (r: Route) => void, win: Window = window): () => void {
  const handler = () => fn(parseHash(win.location.hash));
  win.addEventListener("hashchange", handler);
  handler();
  return () => win.removeEventListener("hashchange", handler);
}

/// Navigate by name with an optional opaque parameter.
export function navigate(name: string, param?: string, win: Window = window): void {
  win.location.hash = routeHash(name, param);
}

/// The current route's query record (view state), or `{}`. `win` injectable for tests.
export function currentQuery(win: Window = window): Record<string, string> {
  return parseHash(win.location.hash).query ?? {};
}

/// Match a route's segments against a `/`-joined pattern where `:name` captures one segment.
/// Returns the captured params (`{}` when the pattern has no captures) on an exact-length match, or
/// `null` otherwise. e.g. matchRoute({segments:["investigate","run","a/b"]}, "investigate/run/:id")
/// -> {id:"a/b"}; matchRoute(inv, "investigate/compare") -> {} when segments === ["investigate","compare"].
export function matchRoute(route: Route, pattern: string): Record<string, string> | null {
  const pat = pattern.replace(/^#?\/?/, "").split("/");
  const segs = route.segments;
  if (pat.length !== segs.length) return null;
  const params: Record<string, string> = {};
  for (let i = 0; i < pat.length; i++) {
    const p = pat[i];
    if (p.startsWith(":")) params[p.slice(1)] = segs[i];
    else if (p !== segs[i]) return null;
  }
  return params;
}
