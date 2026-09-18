// A read-through cache + in-flight dedupe around any TareClient. Framework-free and
// transport-agnostic, so it sits above BOTH the HTTP and Tauri transports.
//
// Why: every tab switch destroys the pane and re-runs the screen, which re-fetches all its data
// from scratch (client.ts caches nothing). A single Overview visit fires ~10 read calls, several
// of them duplicates within the same render. This wrapper makes repeat navigation instant within a
// short freshness window and collapses concurrent identical reads into one round-trip — a win on
// the browser and a bigger one in the WebView, where every `invoke` JSON-serializes its payload.
//
// Model (the standard stale-while-revalidate primitives, minus background revalidation for a first
// cut): a read served within `freshMs` returns the cached promise with NO refetch; an in-flight
// call is shared by key; rejections are never cached; entries older than `evictMs` are dropped.
// Realtime reads (the Live poll) bypass the cache; user-initiated writes clear it so the next read
// reflects the write.
// Realtime or side-effecting reads — always hit the transport, never cached. The Live screen polls
// these on its own cadence, so caching them would defeat the poll; the rest are side effects with
// no cacheable value.
const NO_CACHE = new Set([
    "today",
    "burnrate",
    "recentSteps",
    "sessionsLive",
    "otlpStatus",
    "proxyStatus",
    // Capture health and action verification are live diagnostic reads. Caching either makes a
    // self-check or lifecycle refresh repeat the state from before the user's remedy/intervention.
    "coverage",
    "verifySavings",
    "reconcile",
    "notify",
    "exportRun",
    // Synchronous capability predicates: these return a bare boolean, not a Promise.
    // The default caching branch wraps every return in Promise.resolve(...), which would turn a `false`
    // capability into a truthy Promise and defeat `if (client.canX())` gates (e.g. a browser wrongly
    // rendering a desktop-only control). Passing them through unwrapped keeps them synchronous + honest.
    "canNotify",
    "canControlProxy",
    "canBackgroundOnClose",
]);
// User-initiated writes — pass through, then clear the whole read cache so the next read reflects
// the mutation. Coarse but correct, and these fire rarely (a click), so it costs nothing in practice.
const MUTATIONS = new Set([
    "saveConfig",
    "saveQuality",
    "saveRunNote",
    "deleteRunNote",
    "acknowledgeAnomaly",
    "purgeTranscripts",
    "seedDemo",
    "proxyStart",
    "proxyStop",
    "setBackgroundOnClose",
    "saveInvestigation",
    "deleteInvestigation",
    // Lifecycle writes must invalidate savingsActions/savings immediately. Without these, the real
    // browser/WebView can acknowledge Apply/Dismiss/Restore while rendering the cached old queue.
    "acceptSavings",
    "dismissSavings",
    "unacceptSavings",
]);
function keyOf(name, args) {
    let a = "";
    try {
        a = JSON.stringify(args);
    }
    catch {
        a = String(args.length); // unserializable args (rare on this API) → don't share, but don't throw
    }
    return name + ":" + a;
}
/// Wrap `inner` so read methods are cached + deduped. Returns a value that satisfies TareClient, so
/// callers are unchanged. `inner` is never mutated.
export function cachingClient(inner, opts = {}) {
    const freshMs = opts.freshMs ?? 10000;
    const evictMs = opts.evictMs ?? 300000;
    const now = opts.now ?? (() => Date.now());
    const cache = new Map();
    return new Proxy(inner, {
        get(target, prop, receiver) {
            const orig = Reflect.get(target, prop, receiver);
            if (typeof orig !== "function" || typeof prop !== "string")
                return orig;
            const name = prop;
            const fn = orig;
            if (MUTATIONS.has(name)) {
                return (...args) => {
                    cache.clear(); // optimistic: stale reads must not win a race with the write
                    const r = Promise.resolve(fn.apply(target, args));
                    return r.finally(() => cache.clear());
                };
            }
            if (NO_CACHE.has(name))
                return (...args) => fn.apply(target, args);
            return (...args) => {
                const key = keyOf(name, args);
                const t = now();
                const hit = cache.get(key);
                if (hit && t - hit.at < freshMs)
                    return hit.promise; // fresh hit OR in-flight dedupe
                const promise = Promise.resolve(fn.apply(target, args));
                cache.set(key, { at: t, promise });
                // Never cache a failure — drop the entry so the next call retries the transport.
                promise.catch(() => {
                    if (cache.get(key)?.promise === promise)
                        cache.delete(key);
                });
                // Opportunistic eviction, only when the map has grown — keeps memory bounded without a timer.
                if (cache.size > 256) {
                    for (const [k, v] of cache)
                        if (t - v.at > evictMs)
                            cache.delete(k);
                }
                return promise;
            };
        },
    });
}
