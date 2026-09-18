// Canonical CohortSpec serialization + hash. Produces byte-identical
// output to `tare-core/src/cohort.rs`: the same canonical JSON string and the same 16-hex
// `fnv1a_64` cohort hash, so a scope hashes the same in the browser and the Rust core.
//
// Canonical form: every field explicit (Options -> null); `in.values` and `run_ids.ids` sorted +
// deduped; `step_refs.refs` sorted by (run_id, step_ordinal) + deduped; filters sorted by
// (op, dimension, canonical value) + deduped; all object keys sorted by UTF-8 byte order; no whitespace;
// integers as-is. Values are opaque short labels (no control characters), so JSON string escaping
// matches serde_json for the data this ever sees.

import type {
  CohortEntity,
  CohortFilter,
  CohortMetric,
  CohortSpec,
  MeteredOutcome,
  Normalization,
  OutcomeDenominator,
  PricingMode,
  StepRef,
} from "./types.js";
import type { BaselineSpec } from "./state.js";

const enc = new TextEncoder();

/// Compare two strings by their UTF-8 bytes — identical to Rust's `String` ordering (and to Rust
/// BTreeMap key order), so key/array/filter sorts agree across the two languages even for non-ASCII.
function byteCompare(a: string, b: string): number {
  const ab = enc.encode(a);
  const bb = enc.encode(b);
  const n = Math.min(ab.length, bb.length);
  for (let i = 0; i < n; i++) {
    if (ab[i] !== bb[i]) return ab[i] - bb[i];
  }
  return ab.length - bb.length;
}

/// FNV-1a/64 over bytes, matching `tare_core::canon::fnv1a_64` (u64 wrapping arithmetic).
export function fnv1a64(bytes: Uint8Array): bigint {
  const MASK = (1n << 64n) - 1n;
  const PRIME = 0x100000001b3n;
  let hash = 0xcbf29ce484222325n;
  for (const b of bytes) {
    hash ^= BigInt(b);
    hash = (hash * PRIME) & MASK;
  }
  return hash;
}

function dedupSort(values: string[]): string[] {
  const sorted = [...values].sort(byteCompare);
  const out: string[] = [];
  for (const v of sorted) {
    if (out.length === 0 || out[out.length - 1] !== v) out.push(v);
  }
  return out;
}

function dedupSortRefs(refs: StepRef[]): StepRef[] {
  const sorted = [...refs].sort(
    (a, b) => byteCompare(a.run_id, b.run_id) || a.step_ordinal - b.step_ordinal,
  );
  const out: StepRef[] = [];
  for (const r of sorted) {
    const last = out[out.length - 1];
    if (!last || last.run_id !== r.run_id || last.step_ordinal !== r.step_ordinal) out.push(r);
  }
  return out;
}

// Unit / record separators, matching the Rust filter_sort_key delimiters.
const US = "";
const RS = "";

/// Deterministic (op, dimension, canonical value) sort key — byte-identical to Rust.
function filterSortKey(f: CohortFilter): string {
  switch (f.op) {
    case "eq":
      return `eq${US}${f.dimension}${US}${f.value}`;
    case "in":
      return `in${US}${f.dimension}${US}${dedupSort(f.values).join(RS)}`;
    case "gte_micros":
      return `gte_micros${US}${US}${f.value}`;
    case "lte_micros":
      return `lte_micros${US}${US}${f.value}`;
    case "run_ids":
      return `run_ids${US}${US}${dedupSort(f.ids).join(RS)}`;
    case "step_refs":
      return `step_refs${US}${US}${dedupSortRefs(f.refs)
        .map((r) => `${r.run_id}:${r.step_ordinal}`)
        .join(RS)}`;
    case "tag":
      return `tag${US}${US}${f.value}`;
    case "quality_range":
      return `quality_range${US}${US}${f.min ?? ""}${RS}${f.max ?? ""}`;
  }
}

/// Normalize one filter's arrays for canonical form (sort + dedup); leaves scalar filters intact.
function filterToCanonical(f: CohortFilter): Record<string, unknown> {
  switch (f.op) {
    case "eq":
      return { op: "eq", dimension: f.dimension, value: f.value };
    case "in":
      return { op: "in", dimension: f.dimension, values: dedupSort(f.values) };
    case "gte_micros":
      return { op: "gte_micros", value: f.value };
    case "lte_micros":
      return { op: "lte_micros", value: f.value };
    case "run_ids":
      return { op: "run_ids", ids: dedupSort(f.ids) };
    case "step_refs":
      return {
        op: "step_refs",
        refs: dedupSortRefs(f.refs).map((r) => ({ run_id: r.run_id, step_ordinal: r.step_ordinal })),
      };
    case "tag":
      return { op: "tag", value: f.value };
    case "quality_range":
      return { op: "quality_range", min: f.min ?? null, max: f.max ?? null };
  }
}

/// Stringify a JSON-ish value with object keys sorted by byte order and no whitespace — matching
/// `serde_json::to_string` over a BTreeMap-keyed Value. Integers only (no floats in a CohortSpec).
function canonicalStringify(v: unknown): string {
  if (v === null || v === undefined) return "null";
  if (typeof v === "number") return String(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  if (typeof v === "string") return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(canonicalStringify).join(",")}]`;
  const obj = v as Record<string, unknown>;
  const keys = Object.keys(obj).sort(byteCompare);
  return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalStringify(obj[k])}`).join(",")}}`;
}

/// Canonical JSON for a CohortSpec — byte-identical to `CohortSpec::canonical_json` in Rust.
export function canonicalCohortJson(spec: CohortSpec): string {
  const sortedFilters = spec.filters
    .map((f) => ({ key: filterSortKey(f), value: filterToCanonical(f) }))
    .sort((a, b) => byteCompare(a.key, b.key) || byteCompare(canonicalStringify(a.value), canonicalStringify(b.value)));
  const filters: Record<string, unknown>[] = [];
  for (const filter of sortedFilters) {
    const value = canonicalStringify(filter.value);
    if (filters.length === 0 || canonicalStringify(filters[filters.length - 1]) !== value) {
      filters.push(filter.value);
    }
  }
  const canonical = {
    from: spec.from ?? null,
    to: spec.to ?? null,
    timezone: spec.timezone,
    entity: spec.entity,
    filters,
    pricing: spec.pricing,
    metric: spec.metric,
    normalization: spec.normalization,
    outcome_denominator: spec.outcome_denominator ?? null,
  };
  return canonicalStringify(canonical);
}

/// Stable cohort hash: 16-char lowercase hex `fnv1a_64` of the canonical JSON. Matches
/// `CohortSpec::cohort_hash` in Rust.
export function cohortHash(spec: CohortSpec): string {
  const json = canonicalCohortJson(spec);
  return fnv1a64(enc.encode(json)).toString(16).padStart(16, "0");
}

// ============================================================================
// Analysis URL codec
// ----------------------------------------------------------------------------
// Common analysis state is readable in query parameters (from/to/tz/entity/metric/norm/pricing/
// outcome/sheet/view). Arbitrary filters + selection + baseline ride in `f`/`sel`/`base` as
// base64url of canonical JSON — deterministic, padding-free, and Unicode-safe. Every enum is
// validated on decode and an unknown value fails VISIBLY (throws `AnalysisUrlError`) rather than
// silently degrading. Over the 1,800-char budget the caller must save the investigation and link it
// by id — this module reports the overflow; it never truncates.
// ============================================================================

/// A decode/encode failure the UI must surface. Unknown values are rejected visibly and never
/// swallowed into a partial/guessed state.
export class AnalysisUrlError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AnalysisUrlError";
  }
}

/// Max serialized hash length before an investigation id is required instead.
export const MAX_HASH_LEN = 1800;

const METERED_OUTCOMES: readonly MeteredOutcome[] = [
  "pull_requests",
  "commits",
  "lines_added_per1k",
  "active_hours",
  "sessions",
  "successful_runs",
];
const ENTITIES: readonly CohortEntity[] = ["run", "step"];
const METRICS: readonly CohortMetric[] = ["spend_micros", "tokens", "cache_hit_rate"];
const NORMS: readonly Normalization[] = [
  "absolute",
  "share_of_selection",
  "per_run",
  "per_outcome",
];
const BASELINE_KINDS: readonly BaselineSpec["kind"][] = [
  "prior_window",
  "rest_of_scope",
  "pinned_run",
  "explicit_cohort",
];

/// The shareable analysis state a URL carries. All fields optional — only what's set is
/// serialized. `filters`/`selection`/`baseline` become `f`/`sel`/`base`.
export interface AnalysisUrlState {
  from?: string;
  to?: string;
  tz?: string;
  entity?: CohortEntity;
  metric?: CohortMetric;
  norm?: Normalization;
  pricing?: PricingMode;
  outcome?: OutcomeDenominator;
  sheet?: string;
  view?: string;
  filters?: CohortFilter[];
  /// `null` is an explicit "whole scope" marker. Omitting the field keeps the durable in-memory
  /// selection while moving between workspaces; encoding `null` lets an in-workspace Clear action
  /// survive reload and browser back/forward instead of resurrecting the previous Selection A.
  selection?: CohortSpec | null;
  baseline?: CohortSpec;
  /// `base=` predates durable baseline metadata. These companion fields preserve the rule and the
  /// measured count while old links containing only `base=` continue to hydrate as explicit cohorts.
  baselineKind?: BaselineSpec["kind"];
  baselineSampleCount?: number;
}

/// base64url (RFC 4648 §5) of a UTF-8 string, padding-free — round-trips arbitrary Unicode.
export function base64urlEncode(text: string): string {
  const bytes = enc.encode(text);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/// Inverse of `base64urlEncode`. Throws `AnalysisUrlError` on malformed input.
export function base64urlDecode(s: string): string {
  try {
    const b64 = s.replace(/-/g, "+").replace(/_/g, "/");
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new AnalysisUrlError(`malformed base64url payload: ${s.slice(0, 24)}…`);
  }
}

/// `outcome=` value for a denominator: `unit:<name>` or `metered:<kind>`.
export function encodeOutcome(o: OutcomeDenominator): string {
  return o.kind === "work_unit" ? `unit:${o.name}` : `metered:${o.metered}`;
}

/// Parse an `outcome=` value; unknown metered kinds / empty unit names fail visibly.
export function parseOutcome(s: string): OutcomeDenominator {
  if (s.startsWith("unit:")) {
    const name = s.slice(5);
    if (!name) throw new AnalysisUrlError("outcome unit name is empty");
    return { kind: "work_unit", name };
  }
  if (s.startsWith("metered:")) {
    const kind = s.slice(8);
    if (!METERED_OUTCOMES.includes(kind as MeteredOutcome)) {
      throw new AnalysisUrlError(`unknown metered outcome: ${kind}`);
    }
    return { kind: "metered", metered: kind as MeteredOutcome };
  }
  throw new AnalysisUrlError(`unknown outcome (want unit:… or metered:…): ${s}`);
}

/// `pricing=` value: `effective_dated` | `latest` | `as_of:<date>`.
export function encodePricing(p: PricingMode): string {
  return p.mode === "as_of" ? `as_of:${p.date}` : p.mode;
}

/// Parse a `pricing=` value; unknown modes fail visibly.
export function parsePricing(s: string): PricingMode {
  if (s === "effective_dated" || s === "latest") return { mode: s };
  if (s.startsWith("as_of:")) {
    const date = s.slice(6);
    if (!date) throw new AnalysisUrlError("pricing as_of date is empty");
    return { mode: "as_of", date };
  }
  throw new AnalysisUrlError(`unknown pricing mode: ${s}`);
}

function oneOf<T extends string>(value: string, allowed: readonly T[], label: string): T {
  if (!allowed.includes(value as T)) {
    throw new AnalysisUrlError(`unknown ${label}: ${value}`);
  }
  return value as T;
}

/// Encode a CohortFilter[] for `f=` (base64url of canonical JSON — same scheme as `sel`/`base`).
export function encodeFilters(filters: CohortFilter[]): string {
  const canonical = filters
    .map((f) => ({ key: filterSortKey(f), value: filterToCanonical(f) }))
    .sort((a, b) => byteCompare(a.key, b.key))
    .map((x) => x.value);
  return base64urlEncode(canonicalStringify(canonical));
}

/// Decode an `f=` value back to CohortFilter[]; malformed payloads fail visibly.
export function decodeFilters(s: string): CohortFilter[] {
  const parsed = parseJson(base64urlDecode(s), "f (filters)");
  if (!Array.isArray(parsed)) throw new AnalysisUrlError("f payload is not a filter array");
  return parsed as CohortFilter[];
}

/// Encode a CohortSpec for `sel=`/`base=` (base64url of its canonical JSON).
export function encodeCohort(spec: CohortSpec): string {
  return base64urlEncode(canonicalCohortJson(spec));
}

/// Decode a `sel=`/`base=` value; a payload missing the CohortSpec shape fails visibly.
export function decodeCohort(s: string, param: string): CohortSpec {
  const parsed = parseJson(base64urlDecode(s), param) as Record<string, unknown>;
  if (
    !parsed ||
    typeof parsed.timezone !== "string" ||
    typeof parsed.entity !== "string" ||
    !Array.isArray(parsed.filters)
  ) {
    throw new AnalysisUrlError(`${param} payload is not a CohortSpec`);
  }
  return parsed as unknown as CohortSpec;
}

function parseJson(text: string, param: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    throw new AnalysisUrlError(`${param} payload is not valid JSON`);
  }
}

/// Encode analysis state into a query record. Only set fields appear; `f`/`sel`/`base`
/// carry filters/selection/baseline. Empty filter arrays are omitted (nothing to share).
export function encodeAnalysisQuery(s: AnalysisUrlState): Record<string, string> {
  const q: Record<string, string> = {};
  if (s.from) q.from = s.from;
  if (s.to) q.to = s.to;
  if (s.tz) q.tz = s.tz;
  if (s.entity) q.entity = s.entity;
  if (s.metric) q.metric = s.metric;
  if (s.norm) q.norm = s.norm;
  if (s.pricing) q.pricing = encodePricing(s.pricing);
  if (s.outcome) q.outcome = encodeOutcome(s.outcome);
  if (s.sheet) q.sheet = s.sheet;
  if (s.view) q.view = s.view;
  if (s.filters && s.filters.length > 0) q.f = encodeFilters(s.filters);
  if (s.selection === null) q.sel = "none";
  else if (s.selection) q.sel = encodeCohort(s.selection);
  if (s.baseline) q.base = encodeCohort(s.baseline);
  if (s.baselineKind) q.base_kind = s.baselineKind;
  if (s.baselineSampleCount != null) q.base_n = String(s.baselineSampleCount);
  return q;
}

/// Decode a query record into analysis state. Every recognized key is validated; an unknown
/// enum value or malformed payload throws `AnalysisUrlError` (visible failure, never a guess).
/// Unrecognized keys are ignored (they belong to the router/other features).
export function decodeAnalysisQuery(q: Record<string, string>): AnalysisUrlState {
  const s: AnalysisUrlState = {};
  if (q.from) s.from = q.from;
  if (q.to) s.to = q.to;
  if (q.tz) s.tz = q.tz;
  if (q.entity) s.entity = oneOf(q.entity, ENTITIES, "entity");
  if (q.metric) s.metric = oneOf(q.metric, METRICS, "metric");
  if (q.norm) s.norm = oneOf(q.norm, NORMS, "norm");
  if (q.pricing) s.pricing = parsePricing(q.pricing);
  if (q.outcome) s.outcome = parseOutcome(q.outcome);
  if (q.sheet) s.sheet = q.sheet;
  if (q.view) s.view = q.view;
  if (q.f) s.filters = decodeFilters(q.f);
  if (q.sel) s.selection = q.sel === "none" ? null : decodeCohort(q.sel, "sel");
  if (q.base) s.baseline = decodeCohort(q.base, "base");
  if (q.base_kind) s.baselineKind = oneOf(q.base_kind, BASELINE_KINDS, "baseline kind");
  if (q.base_n) {
    const count = Number(q.base_n);
    if (!Number.isSafeInteger(count) || count < 0) {
      throw new AnalysisUrlError(`invalid baseline sample count: ${q.base_n}`);
    }
    s.baselineSampleCount = count;
  }
  return s;
}

/// Whether the assembled hash exceeds the shareable budget. At/over the limit the caller
/// must persist the investigation and link it by id instead of inlining the state.
export function overflowsHashBudget(hash: string): boolean {
  return hash.length > MAX_HASH_LEN;
}
