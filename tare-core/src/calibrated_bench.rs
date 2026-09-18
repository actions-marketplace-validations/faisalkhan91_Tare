//! Deterministic multi-week "Calibrated Bench" fixture.
//!
//! A richer sibling of [`crate::demo::seed_weeks`]: where `seed_weeks` clones one Anthropic run
//! across a date range, this builds a realistic dataset that exercises every dimension the
//! Calibrated Bench UI reasons about — multiple providers, models, prompt templates, sessions,
//! priced AND unpriced usage, all six cache classes, retry loops, provider failures/refusals,
//! user quality scores, a clear cost anomaly day, and an active-session tail.
//!
//! Like `demo`, this module is **store-free, clock-free, and RNG-free**: it returns plain records
//! (dates from [`crate::calendar`], token counts from fixed arithmetic) so the CLI example
//! `seed_calibrated_bench` can persist it and tests can assert stable totals without any capture,
//! wall clock, or randomness. Same inputs → byte-identical output, every time.
//!
//! Honesty invariants are respected by construction: unpriced usage is
//! carried on [`Provider::Local`] runs (the bundled table prices no local model, so they surface as
//! unpriced token usage rather than `$0`), and no field carries prompt/response payload text.

use crate::model::{
    CacheTtl, Component, ComponentWeight, Provider, RequestShape, RunRecord, StepRecord,
    UsageTokens,
};

/// First calendar day of the fixture window (inclusive). Fixed and obviously historical.
pub const START_DATE: &str = "2026-05-01";
/// Number of days generated. At least 30 days keeps weekly and monthly views representative.
pub const DAYS: i64 = 32;
/// The single day whose spend/volume spikes far above the surrounding baseline — the anomaly the
/// Pulse attention queue and anomaly-why analysis are meant to surface.
pub const ANOMALY_DATE: &str = "2026-05-20";
/// A fixed "recent" instant (2026-06-01T12:00:00Z) used to stamp the active-session tail. Passed to
/// the store as data — the core stays clock-free.
pub const NOW_UNIX: i64 = 1_780_315_200;
/// Fixed RFC 3339 instant for user-authored quality/notes so seeding is reproducible.
pub const SEED_UPDATED_AT: &str = "2026-06-01T12:00:00Z";

/// Four distinct prompt-template identities (`RequestShape.system_hash`) so template/lineage
/// grouping has real spread. Content-based hashes, not byte lengths.
const TEMPLATES: [u64; 4] = [
    0x7a1c_9f3e_00d1_0001,
    0x7a1c_9f3e_00d1_0002,
    0x7a1c_9f3e_00d1_0003,
    0x7a1c_9f3e_00d1_0004,
];

/// Owning session/conversation ids. The last two are the "active tail" (also emitted as recent
/// session beats).
const SESSIONS: [&str; 6] = [
    "sess-refactor",
    "sess-review",
    "sess-docs",
    "sess-triage",
    "sess-ci",
    "sess-adhoc",
];

const COMMITS: [&str; 3] = ["a1b2c3d", "d4e5f6a", "0f1e2d3"];
const AUTHORS: [&str; 3] = ["Ada", "Bhavna", "Chen"];

/// One seeded run plus the store-side annotations (date/hour/provenance, optional quality + note)
/// the CLI example applies. `run` is a normal [`RunRecord`]; everything else is metadata the store
/// stamps alongside the steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeededRun {
    pub date: String,
    pub hour: Option<u8>,
    pub source: &'static str,
    pub profile: &'static str,
    /// User-supplied quality score (0–100) on a subset of runs. `None` = never scored.
    pub quality: Option<i64>,
    pub tags: Vec<&'static str>,
    pub note: &'static str,
    pub starred: bool,
    /// True iff this run's provider is priced by the bundled table (everything except `Local`).
    pub priced: bool,
    pub run: RunRecord,
}

/// One durable session-activity beat for the active tail (mirrors `Store::record_session_beat`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeededSession {
    pub source: &'static str,
    pub session: &'static str,
    pub last_unix: i64,
    pub last_model: &'static str,
}

/// Stable expected totals over the generated dataset. Computed from the runs (single source of
/// truth) and asserted against baked literals in tests so accidental generator drift is caught.
/// Token sums are pricing-edition-independent, so they stay stable as `pricing.json` updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedTotals {
    pub distinct_dates: usize,
    pub total_runs: usize,
    pub total_steps: usize,
    pub priced_runs: usize,
    pub unpriced_runs: usize,
    pub error_steps: usize,
    pub refusal_steps: usize,
    pub retry_steps: usize,
    pub quality_runs: usize,
    pub sessions: usize,
    pub distinct_providers: usize,
    pub distinct_models: usize,
    pub distinct_templates: usize,
    pub fresh_input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
    pub reasoning: u64,
    pub audio_input: u64,
    pub audio_output: u64,
    pub anomaly_date: &'static str,
}

/// The complete fixture: dated runs, the active-session tail, and the expected totals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibratedBench {
    pub runs: Vec<SeededRun>,
    pub sessions: Vec<SeededSession>,
    pub totals: ExpectedTotals,
}

/// A run archetype: the fixed shape of one kind of workload. Token counts below are the *base*
/// values; each generated instance scales them by a small deterministic factor.
#[derive(Clone, Copy)]
struct Archetype {
    provider: Provider,
    model: &'static str,
    vendor: Option<&'static str>,
    effort: Option<&'static str>,
    mcp_server: Option<&'static str>,
    template: usize,
    source: &'static str,
    profile: &'static str,
    ttl: CacheTtl,
    /// Base per-run usage, distributed across steps below.
    fresh_input: u64,
    cache_write_5m: u64,
    cache_write_1h: u64,
    cache_read: u64,
    output: u64,
    reasoning: u64,
    audio_input: u64,
    /// Number of normal (successful) steps.
    steps: u32,
    priced: bool,
}

/// The eight everyday archetypes. Between them they cover 5 providers, 8 models, all 6 cache
/// classes, multimodal audio, reasoning tokens, MCP-server attribution, and both cache TTLs.
const ARCHETYPES: [Archetype; 8] = [
    // 0 — deep coding on the flagship model: cache writes + reads + reasoning. The dominant driver.
    Archetype {
        provider: Provider::Anthropic,
        model: "claude-opus-4-8",
        vendor: None,
        effort: Some("high"),
        mcp_server: None,
        template: 0,
        source: "claude-code",
        profile: "counts_plus",
        ttl: CacheTtl::FiveMin,
        fresh_input: 9_000,
        cache_write_5m: 24_000,
        cache_write_1h: 0,
        cache_read: 180_000,
        output: 6_500,
        reasoning: 2_400,
        audio_input: 0,
        steps: 3,
        priced: true,
    },
    // 1 — long-lived agent session: 1h cache tier, heavy cache reads, filesystem MCP.
    Archetype {
        provider: Provider::Anthropic,
        model: "claude-sonnet-4-6",
        vendor: None,
        effort: Some("medium"),
        mcp_server: Some("filesystem"),
        template: 1,
        source: "otel",
        profile: "counts_plus",
        ttl: CacheTtl::OneHour,
        fresh_input: 5_000,
        cache_write_5m: 0,
        cache_write_1h: 30_000,
        cache_read: 120_000,
        output: 4_000,
        reasoning: 0,
        audio_input: 0,
        steps: 2,
        priced: true,
    },
    // 2 — cheap, high-volume triage on Haiku.
    Archetype {
        provider: Provider::Anthropic,
        model: "claude-haiku-4-5",
        vendor: None,
        effort: Some("low"),
        mcp_server: None,
        template: 2,
        source: "claude-code",
        profile: "counts_plus",
        ttl: CacheTtl::FiveMin,
        fresh_input: 3_000,
        cache_write_5m: 1_500,
        cache_write_1h: 0,
        cache_read: 8_000,
        output: 900,
        reasoning: 0,
        audio_input: 0,
        steps: 1,
        priced: true,
    },
    // 3 — GPT-5 analysis pass.
    Archetype {
        provider: Provider::Openai,
        model: "gpt-5",
        vendor: None,
        effort: None,
        mcp_server: None,
        template: 1,
        source: "proxy",
        profile: "counts_plus",
        ttl: CacheTtl::FiveMin,
        fresh_input: 12_000,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 20_000,
        output: 5_000,
        reasoning: 0,
        audio_input: 0,
        steps: 2,
        priced: true,
    },
    // 4 — GPT-5-mini batch classification.
    Archetype {
        provider: Provider::Openai,
        model: "gpt-5-mini",
        vendor: None,
        effort: None,
        mcp_server: None,
        template: 3,
        source: "proxy",
        profile: "counts_plus",
        ttl: CacheTtl::FiveMin,
        fresh_input: 4_000,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 0,
        output: 1_200,
        reasoning: 0,
        audio_input: 0,
        steps: 1,
        priced: true,
    },
    // 5 — Gemini Pro with a multimodal (audio) input.
    Archetype {
        provider: Provider::Gemini,
        model: "gemini-2.5-pro",
        vendor: None,
        effort: None,
        mcp_server: None,
        template: 0,
        source: "proxy",
        profile: "counts_plus",
        ttl: CacheTtl::FiveMin,
        fresh_input: 8_000,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 6_000,
        output: 3_000,
        reasoning: 0,
        audio_input: 1_500,
        steps: 1,
        priced: true,
    },
    // 6 — OpenAI-compatible endpoint priced via vendor label (groq).
    Archetype {
        provider: Provider::OpenAiCompatible,
        model: "llama-3.3-70b-versatile",
        vendor: Some("groq"),
        effort: None,
        mcp_server: None,
        template: 3,
        source: "proxy",
        profile: "counts_plus",
        ttl: CacheTtl::FiveMin,
        fresh_input: 6_000,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 0,
        output: 2_000,
        reasoning: 0,
        audio_input: 0,
        steps: 1,
        priced: true,
    },
    // 7 — self-hosted model captured out-of-band: UNPRICED by the bundled table.
    Archetype {
        provider: Provider::Local,
        model: "qwen2.5-coder-32b",
        vendor: None,
        effort: Some("medium"),
        mcp_server: None,
        template: 2,
        source: "homelab",
        profile: "counts_plus",
        ttl: CacheTtl::FiveMin,
        fresh_input: 15_000,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 0,
        output: 4_500,
        reasoning: 0,
        audio_input: 0,
        steps: 2,
        priced: false,
    },
];

/// Tiny FNV-1a/64 over a byte slice — a stable, dependency-free hash for `request_hash` values
/// (retry-loop detection keys on request_hash equality).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Deterministic 80–120% scaling factor for run instance `(day, slot)`, giving token spread without
/// randomness. Same `(d, j)` → same factor, always.
fn scale(base: u64, d: i64, j: u32) -> u64 {
    let idx = (d as u64)
        .wrapping_mul(31)
        .wrapping_add((j as u64).wrapping_mul(17));
    let pct = 80 + (idx % 41); // 80..=120
    base.saturating_mul(pct) / 100
}

/// Build one step with a given usage split and shape.
#[allow(clippy::too_many_arguments)]
fn step(
    run_id: &str,
    ordinal: u32,
    arch: &Archetype,
    usage: UsageTokens,
    stop_reason: Option<&str>,
    attempt: Option<u32>,
    request_hash: u64,
    session: &'static str,
    duration_ms: u64,
) -> StepRecord {
    // Byte-weights track the token split so prompt anatomy reads realistically (bytes ≈ 4×tokens).
    let mut weights = vec![
        ComponentWeight {
            component: Component::System,
            bytes: usage
                .cache_write()
                .saturating_add(usage.cache_read)
                .saturating_mul(3),
        },
        ComponentWeight {
            component: Component::Tools,
            bytes: usage.fresh_input.saturating_mul(2),
        },
        ComponentWeight {
            component: Component::UserMessage,
            bytes: usage.fresh_input.saturating_mul(2),
        },
        ComponentWeight {
            component: Component::Output,
            bytes: usage.output.saturating_mul(4),
        },
    ];
    weights.retain(|w| w.bytes > 0);
    let shape = RequestShape {
        model: arch.model.to_string(),
        provider: arch.provider,
        stream: false,
        ttl: arch.ttl,
        has_cache_control: usage.cache_write() > 0 || usage.cache_read > 0,
        cached_component: if usage.cache_write() > 0 || usage.cache_read > 0 {
            Some(Component::System)
        } else {
            None
        },
        system_hash: Some(TEMPLATES[arch.template]),
        weights,
        request_hash: Some(request_hash),
        step_label: None,
        component_label: None,
        parent_label: None,
        attempt,
        session: Some(session.to_string()),
        workload_key: None,
        effort: arch.effort.map(|s| s.to_string()),
        mcp_server: arch.mcp_server.map(|s| s.to_string()),
        vendor: arch.vendor.map(|s| s.to_string()),
        commit: Some(COMMITS[(request_hash % 3) as usize].to_string()),
        author: Some(AUTHORS[(request_hash % 3) as usize].to_string()),
    };
    StepRecord {
        run_id: run_id.to_string(),
        step_ordinal: ordinal,
        provider: arch.provider,
        model: arch.model.to_string(),
        usage,
        shape,
        stop_reason: stop_reason.map(|s| s.to_string()),
        duration_ms,
        start_unix_nano: None,
        trace_id: None,
        span_id: None,
        parent_span_id: None,
    }
}

/// Distribute an archetype's base usage across its `steps`, scaled for instance `(d, j)`. The bulk
/// of prompt tokens land on step 1 (cache writes happen once), later steps read cache.
fn build_run(arch: &Archetype, d: i64, j: u32, run_id: &str, session: &'static str) -> RunRecord {
    let mut run = RunRecord::new(run_id);
    let base_hash = fnv1a(run_id.as_bytes());
    for s in 0..arch.steps {
        let first = s == 0;
        let usage = UsageTokens {
            fresh_input: if first {
                scale(arch.fresh_input, d, j)
            } else {
                scale(arch.fresh_input / 3, d, j)
            },
            cache_write_5m: if first {
                scale(arch.cache_write_5m, d, j)
            } else {
                0
            },
            cache_write_1h: if first {
                scale(arch.cache_write_1h, d, j)
            } else {
                0
            },
            cache_read: scale(arch.cache_read / arch.steps as u64, d, j),
            output: scale(arch.output / arch.steps as u64, d, j),
            reasoning: scale(arch.reasoning / arch.steps as u64, d, j),
            audio_input: if first {
                scale(arch.audio_input, d, j)
            } else {
                0
            },
            audio_output: 0,
        };
        run.steps.push(step(
            run_id,
            s + 1,
            arch,
            usage,
            Some("end_turn"),
            None,
            base_hash.wrapping_add(s as u64),
            session,
            1_000 + (base_hash % 4_000) + s as u64 * 250,
        ));
    }
    run
}

/// Build the complete deterministic fixture.
pub fn build() -> CalibratedBench {
    let base = crate::calendar::parse_date(START_DATE).unwrap_or(0);
    let anomaly = crate::calendar::parse_date(ANOMALY_DATE).unwrap_or(-1) - base;
    let mut runs: Vec<SeededRun> = Vec::new();

    for d in 0..DAYS {
        let date = crate::calendar::format_date(base + d);
        let dow = ((base + d) % 7 + 7) % 7; // 0..6, stable weekly cycle
        let weekend = dow == 5 || dow == 6;
        let is_anomaly = d == anomaly;

        // Baseline volume: quieter on weekends, a sharp spike on the anomaly day.
        let n: u32 = if is_anomaly {
            30
        } else if weekend {
            5
        } else {
            9
        };

        for j in 0..n {
            let session = SESSIONS[((d as usize) * 3 + j as usize) % SESSIONS.len()];
            // On the anomaly day, funnel most volume onto the flagship-model archetype so the spike
            // is a real driver concentration, not uniform growth.
            let arch_idx = if is_anomaly && j % 5 != 0 {
                0
            } else {
                ((d as usize) * 7 + j as usize) % ARCHETYPES.len()
            };
            let arch = &ARCHETYPES[arch_idx];
            let run_id = format!("cb-{date}-{j:02}");
            let hour = Some((((d * 3 + j as i64) % 24) as u8).clamp(0, 23));

            let mut run = build_run(arch, d, j, &run_id, session);

            // Weave in retries and failures on a deterministic subset (~every 11th run slot on a
            // priced Anthropic archetype): a provider error on step 1, then a same-request_hash
            // retry (attempt=2), then success. Both an error step AND a retry step.
            let woven = j % 11 == 3 && matches!(arch.provider, Provider::Anthropic);
            if woven {
                let base_hash = fnv1a(run_id.as_bytes()) ^ 0xdead_beef;
                let err_usage = UsageTokens::default(); // an error carries no usable tokens
                let mut retried = Vec::with_capacity(run.steps.len() + 2);
                retried.push(step(
                    &run_id,
                    1,
                    arch,
                    err_usage,
                    Some("provider_error"),
                    None,
                    base_hash,
                    session,
                    800,
                ));
                let retry_usage = UsageTokens {
                    fresh_input: scale(arch.fresh_input, d, j),
                    output: scale(arch.output / arch.steps as u64, d, j),
                    ..UsageTokens::default()
                };
                retried.push(step(
                    &run_id,
                    2,
                    arch,
                    retry_usage,
                    Some("end_turn"),
                    Some(2),
                    base_hash,
                    session,
                    1_500,
                ));
                // Renumber the original successful steps after the retry pair.
                for (k, mut s) in run.steps.clone().into_iter().enumerate() {
                    s.step_ordinal = (k as u32) + 3;
                    retried.push(s);
                }
                run.steps = retried;
            }

            // A separate deterministic subset produces a refusal (safety decline) — a failure that
            // is not retry-worthy tokens-wise but must surface as a non-success outcome.
            let refusal = j % 13 == 7 && matches!(arch.provider, Provider::Anthropic);
            if refusal {
                if let Some(last) = run.steps.last_mut() {
                    last.stop_reason = Some("refusal".to_string());
                }
            }

            // Quality scores on a deterministic subset (~1 in 4), spread across the 0–100 range.
            let quality = if j % 4 == 0 {
                Some(55 + ((d * 7 + j as i64) % 45))
            } else {
                None
            };
            let starred = j % 17 == 5;
            let (tags, note): (Vec<&'static str>, &'static str) = if is_anomaly {
                (
                    vec!["anomaly", "investigate"],
                    "spend spike under investigation",
                )
            } else if quality.is_some() {
                (vec!["reviewed"], "")
            } else {
                (vec![], "")
            };

            runs.push(SeededRun {
                date: date.clone(),
                hour,
                source: arch.source,
                profile: arch.profile,
                quality,
                tags,
                note,
                starred,
                priced: arch.priced,
                run,
            });
        }
    }

    // Active-session tail: the last two sessions beat at recent instants so Pulse shows live work.
    let sessions = vec![
        SeededSession {
            source: "claude-code",
            session: "sess-ci",
            last_unix: NOW_UNIX - 120,
            last_model: "claude-opus-4-8",
        },
        SeededSession {
            source: "claude-code",
            session: "sess-adhoc",
            last_unix: NOW_UNIX - 45,
            last_model: "claude-haiku-4-5",
        },
        SeededSession {
            source: "otel",
            session: "sess-refactor",
            last_unix: NOW_UNIX - 600,
            last_model: "claude-sonnet-4-6",
        },
    ];

    let totals = compute_totals(&runs, &sessions);
    CalibratedBench {
        runs,
        sessions,
        totals,
    }
}

fn compute_totals(runs: &[SeededRun], sessions: &[SeededSession]) -> ExpectedTotals {
    use std::collections::BTreeSet;
    let mut dates = BTreeSet::new();
    let mut providers = BTreeSet::new();
    let mut models = BTreeSet::new();
    let mut templates = BTreeSet::new();
    let mut t = ExpectedTotals {
        distinct_dates: 0,
        total_runs: runs.len(),
        total_steps: 0,
        priced_runs: 0,
        unpriced_runs: 0,
        error_steps: 0,
        refusal_steps: 0,
        retry_steps: 0,
        quality_runs: 0,
        sessions: sessions.len(),
        distinct_providers: 0,
        distinct_models: 0,
        distinct_templates: 0,
        fresh_input: 0,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 0,
        output: 0,
        reasoning: 0,
        audio_input: 0,
        audio_output: 0,
        anomaly_date: ANOMALY_DATE,
    };
    for sr in runs {
        dates.insert(sr.date.clone());
        if sr.priced {
            t.priced_runs += 1;
        } else {
            t.unpriced_runs += 1;
        }
        if sr.quality.is_some() {
            t.quality_runs += 1;
        }
        for step in &sr.run.steps {
            t.total_steps += 1;
            providers.insert(step.provider.as_str());
            models.insert(step.model.clone());
            if let Some(h) = step.shape.system_hash {
                templates.insert(h);
            }
            if step.is_error() {
                t.error_steps += 1;
            }
            if step.is_refusal() {
                t.refusal_steps += 1;
            }
            if step.shape.attempt.is_some_and(|a| a > 1) {
                t.retry_steps += 1;
            }
            t.fresh_input += step.usage.fresh_input;
            t.cache_write_5m += step.usage.cache_write_5m;
            t.cache_write_1h += step.usage.cache_write_1h;
            t.cache_read += step.usage.cache_read;
            t.output += step.usage.output;
            t.reasoning += step.usage.reasoning;
            t.audio_input += step.usage.audio_input;
            t.audio_output += step.usage.audio_output;
        }
    }
    t.distinct_dates = dates.len();
    t.distinct_providers = providers.len();
    t.distinct_models = models.len();
    t.distinct_templates = templates.len();
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_is_deterministic() {
        assert_eq!(
            build(),
            build(),
            "same inputs must produce identical output"
        );
    }

    #[test]
    fn totals_are_internally_consistent() {
        let b = build();
        let recomputed = compute_totals(&b.runs, &b.sessions);
        assert_eq!(recomputed, b.totals, "returned totals must match the runs");
    }

    #[test]
    fn covers_every_required_dimension() {
        let b = build();
        let t = &b.totals;
        assert!(t.distinct_dates >= 30, "≥30 days: {}", t.distinct_dates);
        assert_eq!(t.distinct_dates, DAYS as usize);
        assert!(t.distinct_providers >= 4, "multiple providers");
        assert!(t.distinct_models >= 6, "multiple models");
        assert_eq!(t.distinct_templates, TEMPLATES.len());
        assert!(
            t.priced_runs > 0 && t.unpriced_runs > 0,
            "priced AND unpriced"
        );
        // All six cache classes present in the aggregate.
        assert!(t.fresh_input > 0, "fresh");
        assert!(t.cache_write_5m > 0, "cache_write_5m");
        assert!(t.cache_write_1h > 0, "cache_write_1h");
        assert!(t.cache_read > 0, "cache_read");
        assert!(t.output > 0, "output");
        assert!(t.reasoning > 0, "reasoning");
        assert!(t.audio_input > 0, "multimodal audio");
        assert!(t.error_steps > 0, "provider failures");
        assert!(t.refusal_steps > 0, "refusals");
        assert!(t.retry_steps > 0, "retry loops");
        assert!(t.quality_runs > 0, "user quality scores");
        assert!(t.sessions >= 3, "active-session tail");
        assert_eq!(t.anomaly_date, ANOMALY_DATE);
    }

    #[test]
    fn anomaly_day_spends_far_above_baseline() {
        let b = build();
        // Sum output+fresh tokens per date; the anomaly day must dwarf the median day.
        use std::collections::BTreeMap;
        let mut by_date: BTreeMap<&str, u64> = BTreeMap::new();
        for sr in &b.runs {
            let v: u64 = sr
                .run
                .steps
                .iter()
                .map(|s| s.usage.fresh_input + s.usage.output + s.usage.cache_read)
                .sum();
            *by_date.entry(sr.date.as_str()).or_default() += v;
        }
        let anomaly = by_date[ANOMALY_DATE];
        let mut others: Vec<u64> = by_date
            .iter()
            .filter(|(d, _)| **d != ANOMALY_DATE)
            .map(|(_, v)| *v)
            .collect();
        others.sort_unstable();
        let median = others[others.len() / 2];
        assert!(
            anomaly > median * 2,
            "anomaly {anomaly} should exceed 2× median {median}"
        );
    }

    /// The stable "golden" totals recorded in fixtures/calibrated_bench/README.md. If the generator
    /// changes on purpose, update both this literal and the README in the same change.
    #[test]
    fn totals_match_recorded_golden() {
        let t = build().totals;
        assert_eq!(t.distinct_dates, 32);
        assert_eq!(t.total_runs, 281);
        assert_eq!(t.total_steps, 520);
        assert_eq!(t.priced_runs, 246);
        assert_eq!(t.unpriced_runs, 35);
        assert_eq!(t.error_steps, 13);
        assert_eq!(t.refusal_steps, 13);
        assert_eq!(t.retry_steps, 13);
        assert_eq!(t.quality_runs, 94);
        assert_eq!(t.sessions, 3);
        assert_eq!(t.distinct_providers, 5);
        assert_eq!(t.distinct_models, 8);
        assert_eq!(t.distinct_templates, 4);
        assert_eq!(t.fresh_input, 2_967_548);
        assert_eq!(t.cache_write_5m, 1_395_405);
        assert_eq!(t.cache_write_1h, 1_017_300);
        assert_eq!(t.cache_read, 15_174_540);
        assert_eq!(t.output, 1_047_211);
        assert_eq!(t.reasoning, 134_592);
        assert_eq!(t.audio_input, 46_005);
        assert_eq!(t.audio_output, 0);
    }
}
