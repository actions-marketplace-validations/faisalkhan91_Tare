//! Deterministic spend-anomaly detection over a `TrendReport`'s dense daily series: spikes
//! (a day well above the trailing median), a series appearing, and a series vanishing. Pure,
//! integer (i128 cross-multiplication for the spike ratio — no f64), clock-free (dates come
//! from the trend), deterministic ordering. JSON only — no renderer, so #3 isn't engaged.

use crate::bisect::active_median;
use crate::cohort::CohortSpec;
use crate::money::scaled_div;
use crate::trend::TrendReport;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyKind {
    /// A day whose spend is more than `threshold_pct` above the trailing-window median.
    Spike,
    /// A series with no prior spend that starts spending.
    NewSeries,
    /// A series that had spend and then goes to zero for the rest of the window.
    VanishedSeries,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anomaly {
    pub date: String,
    pub series_key: String,
    pub kind: AnomalyKind,
    pub value_micros: i64,
    pub baseline_micros: i64,
    /// `minor` | `material` | `major`, from the dollar impact (|value − baseline|) AND
    /// its share of that day's total spend — so a small spike doesn't alarm like a real regression.
    pub materiality: String,
}

/// Tier an anomaly by impact + share of the day's total. Both must clear a bar (small-but-dominant
/// or large-but-tiny stay below `major`), so noise reads as `minor`. Deterministic integer math.
fn materiality_of(impact_micros: i64, day_total_micros: i64) -> &'static str {
    let impact = impact_micros.max(0);
    let share_pct = if day_total_micros > 0 {
        scaled_div(impact, 100, day_total_micros as u64).unwrap_or(0)
    } else {
        100 // no other spend that day -> this IS the day
    };
    if impact >= 1_000_000 && share_pct >= 25 {
        "major"
    } else if impact >= 100_000 && share_pct >= 5 {
        "material"
    } else {
        "minor"
    }
}

/// Stable identity of an anomaly for acknowledgement: `date:series:kind`.
pub fn anomaly_key(a: &Anomaly) -> String {
    let kind = match a.kind {
        AnomalyKind::Spike => "spike",
        AnomalyKind::NewSeries => "new_series",
        AnomalyKind::VanishedSeries => "vanished_series",
    };
    format!("{}:{}:{}", a.date, a.series_key, kind)
}

/// Drop anomalies whose `anomaly_key` is in `acknowledged` (a persisted false-positive list).
pub fn filter_acknowledged(anomalies: Vec<Anomaly>, acknowledged: &[String]) -> Vec<Anomaly> {
    if acknowledged.is_empty() {
        return anomalies;
    }
    let acked: std::collections::BTreeSet<&str> = acknowledged.iter().map(|s| s.as_str()).collect();
    anomalies
        .into_iter()
        .filter(|a| !acked.contains(anomaly_key(a).as_str()))
        .collect()
}

/// Vantage-style noise filters: the guardrails that keep detection from crying wolf.
/// The trailing-average ratio is the caller's `threshold_pct`; these are the *additional* floors and
/// the repeat-suppression window applied AFTER an anomaly is found. All-zero (the [`Default`]) is
/// fully permissive, so `detect` reproduces the pre-4e2s output byte-for-byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoiseFilters {
    /// Minimum absolute dollar impact (micro-USD) to surface. 0 = off. Kills sub-cent jitter.
    pub dollar_floor_micros: i64,
    /// Minimum share of that day's total spend (percent) to surface. 0 = off. Kills a big number
    /// that's a rounding error against a huge day.
    pub pct_of_daily_floor: i64,
    /// Suppress a repeat anomaly of the same (series, kind) within this many days of an already-kept
    /// one — a sustained shift alarms once, not every day. 0 = off.
    pub dedupe_window_days: usize,
}

/// Detect anomalies across every series in `trend`. `window` = trailing days for the spike
/// baseline; `threshold_pct` = how far over the median counts as a spike. Results are sorted by
/// (date, series_key) for determinism. Fully permissive (no Vantage floors/dedupe).
pub fn detect(trend: &TrendReport, window: usize, threshold_pct: i64) -> Vec<Anomaly> {
    detect_filtered(trend, window, threshold_pct, NoiseFilters::default())
}

/// As [`detect`], then apply Vantage-style noise filters: drop anomalies below the
/// dollar / share-of-day floors, and suppress a repeat of the same (series, kind) within
/// `dedupe_window_days` of an earlier kept one. `NoiseFilters::default()` → identical to `detect`.
pub fn detect_filtered(
    trend: &TrendReport,
    window: usize,
    threshold_pct: i64,
    filters: NoiseFilters,
) -> Vec<Anomaly> {
    let mut out = detect_raw(trend, window, threshold_pct);
    // Day-index for dollar/share lookups and for measuring the dedupe gap in *days*.
    let day_idx: std::collections::BTreeMap<&str, usize> = trend
        .days
        .iter()
        .enumerate()
        .map(|(i, d)| (d.as_str(), i))
        .collect();
    let n_days = trend.days.len();
    let mut day_total = vec![0i64; n_days];
    for s in &trend.series {
        for (i, &val) in s.per_day.iter().enumerate() {
            if i < n_days {
                day_total[i] = day_total[i].saturating_add(val.max(0));
            }
        }
    }
    // Floors: an anomaly's impact = |value − baseline|; its share is impact / that-day's total.
    if filters.dollar_floor_micros > 0 || filters.pct_of_daily_floor > 0 {
        out.retain(|a| {
            let impact = a
                .value_micros
                .saturating_sub(a.baseline_micros)
                .saturating_abs();
            if impact < filters.dollar_floor_micros {
                return false;
            }
            if filters.pct_of_daily_floor > 0 {
                let total = day_idx
                    .get(a.date.as_str())
                    .and_then(|&i| day_total.get(i))
                    .copied()
                    .unwrap_or(0);
                let share = if total > 0 {
                    scaled_div(impact, 100, total as u64).unwrap_or(0)
                } else {
                    100
                };
                if share < filters.pct_of_daily_floor {
                    return false;
                }
            }
            true
        });
    }
    // Dedupe: within a (series, kind) group, keep an anomaly only if it's more than the window past
    // the last kept one. `out` is already sorted by date, so a single pass per group suffices.
    if filters.dedupe_window_days > 0 {
        let mut last_kept: std::collections::BTreeMap<(String, u8), usize> =
            std::collections::BTreeMap::new();
        out.retain(|a| {
            let Some(&idx) = day_idx.get(a.date.as_str()) else {
                return true;
            };
            let key = (a.series_key.clone(), a.kind as u8);
            match last_kept.get(&key) {
                Some(&prev) if idx.saturating_sub(prev) <= filters.dedupe_window_days => false,
                _ => {
                    last_kept.insert(key, idx);
                    true
                }
            }
        });
    }
    out
}

fn detect_raw(trend: &TrendReport, window: usize, threshold_pct: i64) -> Vec<Anomaly> {
    let window = window.max(1);
    let threshold_pct = threshold_pct.max(0);
    // Per-day total spend across ALL series — the denominator for an anomaly's share-of-day,
    // which drives its materiality tier.
    let n_days = trend.days.len();
    let mut day_total = vec![0i64; n_days];
    for s in &trend.series {
        for (i, &val) in s.per_day.iter().enumerate() {
            if i < n_days {
                day_total[i] = day_total[i].saturating_add(val.max(0));
            }
        }
    }
    let mat = |impact: i64, idx: usize| {
        materiality_of(impact, day_total.get(idx).copied().unwrap_or(0)).to_string()
    };

    let mut out = Vec::new();
    for s in &trend.series {
        let v = &s.per_day;
        let n = v.len().min(trend.days.len());

        // New series: first nonzero day, when everything before it was zero AND it isn't day 0.
        if let Some(first_nz) = (0..n).find(|&i| v[i] > 0) {
            if first_nz > 0 {
                out.push(Anomaly {
                    date: trend.days[first_nz].clone(),
                    series_key: s.key.clone(),
                    kind: AnomalyKind::NewSeries,
                    value_micros: v[first_nz],
                    baseline_micros: 0,
                    materiality: mat(v[first_nz], first_nz),
                });
            }
            // Vanished series: had spend, then zero from some day to the end (and >1 day total).
            if let Some(last_nz) = (0..n).rev().find(|&i| v[i] > 0) {
                if last_nz + 1 < n && first_nz <= last_nz {
                    out.push(Anomaly {
                        date: trend.days[last_nz + 1].clone(),
                        series_key: s.key.clone(),
                        kind: AnomalyKind::VanishedSeries,
                        value_micros: 0,
                        baseline_micros: v[last_nz],
                        materiality: mat(v[last_nz], last_nz + 1),
                    });
                }
            }
        }

        // Spikes: each day past the trailing-window median by > threshold_pct.
        for i in 1..n {
            let start = i.saturating_sub(window);
            let baseline = active_median(&v[start..i]); // Ignore idle days in the baseline.
            if baseline <= 0 {
                continue;
            }
            if (v[i] as i128) * 100 > (baseline as i128) * (100 + threshold_pct as i128) {
                out.push(Anomaly {
                    date: trend.days[i].clone(),
                    series_key: s.key.clone(),
                    kind: AnomalyKind::Spike,
                    value_micros: v[i],
                    baseline_micros: baseline,
                    materiality: mat(v[i].saturating_sub(baseline).saturating_abs(), i),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(a.series_key.cmp(&b.series_key))
            .then((a.kind as u8).cmp(&(b.kind as u8)))
    });
    out
}

/// A series' aggregate for one day (or an averaged baseline): requests, total tokens, and cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayAgg {
    pub steps: u64,
    pub tokens: u64,
    pub micros: i64,
}

/// A spend change decomposed into the three controllable levers: the "what changed
/// and why" behind an anomaly. Volume + size + efficiency sum EXACTLY to `total_delta_micros`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnomalyWhy {
    pub series_key: String,
    /// `day.micros − baseline.micros`.
    pub total_delta_micros: i64,
    /// Δ$ from more/fewer REQUESTS (steps), valued at the baseline cost/request.
    pub volume_micros: i64,
    /// Δ$ from larger/smaller requests (tokens/request), valued at the baseline $/token.
    pub size_micros: i64,
    /// Δ$ from a change in $/token — worse/better caching or a pricier model mix (the residual, so
    /// the three sum exactly to the total).
    pub efficiency_micros: i64,
    /// One-sentence "what changed and why", naming the dominant lever.
    pub headline: String,
    /// A ready-to-run command that isolates the causing change (bisect link).
    pub bisect_hint: String,
    /// The single cost-class (fresh input / cache-write / cache-read / output) that drove > half of
    /// the delta, if one did. `None` = a diffuse change with no dominant cause. Omitted
    /// from JSON when absent so existing consumers/goldens are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dominant_cause: Option<SpikeCause>,
}

/// Dimension for an anomaly-explanation request. Distinct from
/// [`crate::trend::TrendDimension`]'s wire tags (`by_provider`): the request contract uses the short
/// forms `total`/`provider`/`model`/`cause`. `cause` is not a step attribute, so it yields no
/// decomposition (an empty result) — the caller learns that honestly rather than getting a guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnomalyDimension {
    Total,
    Provider,
    Model,
    Cause,
}

impl AnomalyDimension {
    pub fn to_trend(self) -> crate::trend::TrendDimension {
        use crate::trend::TrendDimension;
        match self {
            AnomalyDimension::Total => TrendDimension::Total,
            AnomalyDimension::Provider => TrendDimension::ByProvider,
            AnomalyDimension::Model => TrendDimension::ByModel,
            AnomalyDimension::Cause => TrendDimension::ByCause,
        }
    }
}

/// Scoped anomaly-explanation request. When `scope` is present it is resolved
/// to its run set BEFORE detection, so detection runs on the scoped subset — never a post-filter of
/// already-detected anomaly rows. `window`/`threshold` are optional; the clock-owning edge fills the
/// store's `[anomaly]` config / 7 / 50 defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnomalyWhyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub dimension: AnomalyDimension,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<CohortSpec>,
}

fn signed_usd(micros: i64) -> String {
    let sign = if micros >= 0 { "+" } else { "-" };
    format!(
        "{sign}{}",
        crate::money::MicroUsd(micros.saturating_abs()).to_dollar_string_2dp()
    )
}

fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Compute `signed_magnitude * factor / denominator` without overflowing intermediate products.
/// The denominator is at most u64, so after cancelling common factors any remaining u128 multiply
/// that still overflows necessarily has an out-of-i64 quotient and can saturate immediately.
fn signed_mul_div(mut magnitude: u128, mut negative: bool, factor: i64, denominator: u64) -> i64 {
    if magnitude == 0 || factor == 0 || denominator == 0 {
        return 0;
    }
    negative ^= factor.is_negative();
    let mut factor = u128::from(factor.unsigned_abs());
    let mut denominator = u128::from(denominator);

    let g = gcd_u128(magnitude, denominator);
    magnitude /= g;
    denominator /= g;
    let g = gcd_u128(factor, denominator);
    factor /= g;
    denominator /= g;

    let Some(product) = magnitude.checked_mul(factor) else {
        return if negative { i64::MIN } else { i64::MAX };
    };
    let quotient = product / denominator;
    if negative {
        let min_magnitude = u128::from(i64::MAX as u64) + 1;
        if quotient >= min_magnitude {
            i64::MIN
        } else {
            -(quotient as i64)
        }
    } else {
        quotient.min(u128::from(i64::MAX as u64)) as i64
    }
}

/// Decompose a series' spend change (`baseline` → `day`) into volume × size × efficiency. Pure,
/// deterministic integer math (i128 intermediates); efficiency is the residual so the three
/// components sum EXACTLY to the total delta. A from/to-nothing move (new/vanished series, or a
/// degenerate zero baseline) attributes the whole change to volume.
pub fn decompose_change(series: &str, day: DayAgg, baseline: DayAgg) -> AnomalyWhy {
    let delta = day.micros.saturating_sub(baseline.micros);
    let (volume, size) = if baseline.steps == 0 || baseline.tokens == 0 || day.steps == 0 {
        (delta, 0)
    } else {
        let (bs, bt, bm) = (baseline.steps, baseline.tokens, baseline.micros);
        let (ds, dt) = (day.steps, day.tokens);
        // volume: extra/fewer requests at the baseline cost/request.
        let (volume_negative, volume_magnitude) = if ds >= bs {
            (false, u128::from(ds - bs))
        } else {
            (true, u128::from(bs - ds))
        };
        let volume = signed_mul_div(volume_magnitude, volume_negative, bm, bs);
        // size: change in tokens-per-request (holding request count at day's), at baseline $/token.
        let base_tokens_at_day_volume = u128::from(bt) * u128::from(ds) / u128::from(bs);
        let (size_negative, size_magnitude) = if u128::from(dt) >= base_tokens_at_day_volume {
            (false, u128::from(dt) - base_tokens_at_day_volume)
        } else {
            (true, base_tokens_at_day_volume - u128::from(dt))
        };
        let size = signed_mul_div(size_magnitude, size_negative, bm, bt);
        (volume, size)
    };
    let efficiency = delta.saturating_sub(volume).saturating_sub(size);

    // Name the dominant lever (by absolute contribution) for the headline.
    let dominant = [
        ("more/fewer requests", volume),
        ("larger/smaller requests", size),
        ("$/token (caching or model mix)", efficiency),
    ]
    .into_iter()
    .max_by_key(|(_, v)| v.unsigned_abs())
    .map(|(name, _)| name)
    .unwrap_or("mixed factors");
    let dir = if delta >= 0 { "rose" } else { "fell" };
    let headline = format!(
        "Spend on {series} {dir} {} ({} → {}): {} volume, {} size, {} efficiency — mostly {dominant}.",
        signed_usd(delta),
        crate::money::MicroUsd(baseline.micros).to_dollar_string_2dp(),
        crate::money::MicroUsd(day.micros).to_dollar_string_2dp(),
        signed_usd(volume),
        signed_usd(size),
        signed_usd(efficiency),
    );
    AnomalyWhy {
        series_key: series.to_string(),
        total_delta_micros: delta,
        volume_micros: volume,
        size_micros: size,
        efficiency_micros: efficiency,
        headline,
        bisect_hint: "tare bisect --git   # isolate the commit that moved spend".to_string(),
        // The caller attributes the dominant cost-class after this lever decomposition,
        // when it has the per-component breakdown; the pure lever math leaves it None.
        dominant_cause: None,
    }
}

/// One sub-cause's contribution to a spend delta: a component / cache-class / step and
/// how many micro-USD of the day's change it accounts for. `delta_micros` = its day − baseline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausePart {
    pub label: String,
    pub delta_micros: i64,
}

/// The single sub-cause that drove the majority of a spend delta. Absent when the change
/// was diffuse (no one cause cleared half) — the honest "attribute none" outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpikeCause {
    pub label: String,
    pub delta_micros: i64,
    /// This cause's share of the total delta, percent. Always > 50 when present.
    pub share_pct: i64,
}

/// Attribute a spend delta to the SINGLE sub-cause that drove more than half of it — the 4e2s thesis
/// ("detection becomes attribution"). Only same-direction parts count (a cost-increaser during an
/// up-spike); the largest by absolute contribution wins, tie-broken by label for determinism. If its
/// share of the total delta is ≤ 50%, the change is diffuse and we attribute NONE (returns `None`).
/// Pure, integer (i128 intermediates), no clock.
pub fn attribute_delta(total_delta_micros: i64, parts: &[CausePart]) -> Option<SpikeCause> {
    if total_delta_micros == 0 {
        return None;
    }
    let up = total_delta_micros > 0;
    let mut same_dir: Vec<&CausePart> = parts
        .iter()
        .filter(|p| p.delta_micros != 0 && (p.delta_micros > 0) == up)
        .collect();
    // Largest absolute contribution first; deterministic label tiebreak.
    same_dir.sort_by(|a, b| {
        b.delta_micros
            .unsigned_abs()
            .cmp(&a.delta_micros.unsigned_abs())
            .then_with(|| a.label.cmp(&b.label))
    });
    let top = same_dir.first()?;
    let share = i128::from(top.delta_micros) * 100 / i128::from(total_delta_micros);
    if share > 50 {
        Some(SpikeCause {
            label: top.label.clone(),
            delta_micros: top.delta_micros,
            // Clamp the reported share to 100: when opposite-direction parts
            // offset the net delta, one cause's magnitude can exceed the net total, yielding a
            // nonsensical ">100% of the change". The dominance gate above still uses the raw ratio.
            share_pct: share.min(100) as i64,
        })
    } else {
        None
    }
}

/// Decompose each SPIKE anomaly into volume × size × efficiency over a prebuilt trend
/// and the dated runs it was built from. Pure and deterministic: the caller supplies the run set
/// (whole store, or a resolved cohort scope), so scope resolution happens BEFORE detection rather
/// than as a post-filter of anomaly rows. Only decomposable dimensions
/// (total/provider/model) produce rows — a by-cause anomaly isn't a step attribute. Unpriced steps
/// are excluded (a gap surfaced elsewhere), never treated as $0. Reuses the existing `decompose_change`
/// and `attribute_delta` primitives — no reimplementation.
pub fn decompose_spikes(
    report: &TrendReport,
    dated: &[crate::trend::DatedRun],
    dim: crate::trend::TrendDimension,
    window: usize,
    threshold: i64,
    pricing: &crate::pricing::PricingTable,
) -> Vec<AnomalyWhy> {
    use crate::trend::TrendDimension;
    use std::collections::{BTreeMap, HashMap};

    // The cost-classes we attribute a spike to, in a fixed order for determinism.
    const CAUSE_LABELS: [&str; 4] = ["fresh input", "cache-write", "cache-read", "output"];

    let window = window.max(1);
    let anomalies = detect(report, window, threshold);

    let key_of = |step: &crate::model::StepRecord| -> String {
        match dim {
            TrendDimension::Total => "total".to_string(),
            TrendDimension::ByProvider => step.provider.as_str().to_string(),
            TrendDimension::ByModel => step.model.clone(),
            TrendDimension::ByCause => String::new(),
        }
    };
    // (series-key, date) -> (steps, tokens, micros).
    let mut agg: BTreeMap<(String, String), (u64, u64, i64)> = BTreeMap::new();
    // (series-key, date) -> per-cost-class micros [fresh, cache-write, cache-read, output] for the
    // single-cause attribution.
    let mut comp_agg: BTreeMap<(String, String), [i64; 4]> = BTreeMap::new();
    for dr in dated {
        for step in &dr.run.steps {
            let Some(rates) = pricing.lookup_as_of(
                step.provider,
                step.shape.vendor.as_deref(),
                &step.model,
                Some(&dr.date),
            ) else {
                continue; // unpriced → excluded (a gap, surfaced elsewhere)
            };
            let cost = crate::account::cost_usage(&step.usage, rates, &step.shape);
            let m = cost.total.micros();
            let k = (key_of(step), dr.date.clone());
            let e = agg.entry(k.clone()).or_insert((0, 0, 0));
            e.0 = e.0.saturating_add(1);
            e.1 =
                e.1.saturating_add(step.usage.total())
                    .saturating_add(step.usage.audio_input)
                    .saturating_add(step.usage.audio_output);
            e.2 = e.2.saturating_add(m);
            let c = comp_agg.entry(k).or_insert([0; 4]);
            c[0] = c[0].saturating_add(cost.fresh.micros());
            c[1] = c[1].saturating_add(cost.cache_write.micros());
            c[2] = c[2].saturating_add(cost.cache_read.micros());
            c[3] = c[3].saturating_add(cost.output.micros());
        }
    }
    let day_pos: HashMap<&str, usize> = report
        .days
        .iter()
        .enumerate()
        .map(|(i, d)| (d.as_str(), i))
        .collect();

    let mut out = Vec::new();
    for a in anomalies.iter().filter(|a| a.kind == AnomalyKind::Spike) {
        let day = agg
            .get(&(a.series_key.clone(), a.date.clone()))
            .copied()
            .unwrap_or((0, 0, 0));
        let Some(&pos) = day_pos.get(a.date.as_str()) else {
            continue;
        };
        // Baseline = mean per-day aggregate over the `window` days preceding the spike.
        let start = pos.saturating_sub(window);
        let (mut bs, mut bt, mut bm, mut days) = (0u64, 0u64, 0i64, 0u64);
        for d in &report.days[start..pos] {
            let g = agg
                .get(&(a.series_key.clone(), d.clone()))
                .copied()
                .unwrap_or((0, 0, 0));
            bs = bs.saturating_add(g.0);
            bt = bt.saturating_add(g.1);
            bm = bm.saturating_add(g.2);
            days = days.saturating_add(1);
        }
        // Mean over the window; when there are no baseline days the sums are all 0 anyway, so
        // dividing by max(1) yields the correct all-zero baseline (a new/from-nothing series).
        let divisor = days.max(1);
        let baseline = DayAgg {
            steps: bs / divisor,
            tokens: bt / divisor,
            micros: scaled_div(bm, 1, divisor).unwrap_or(0),
        };
        let day_agg = DayAgg {
            steps: day.0,
            tokens: day.1,
            micros: day.2,
        };
        let mut why = decompose_change(&a.series_key, day_agg, baseline);

        // Single-cause attribution: diff each cost-class (day vs the window mean) and
        // blame the one that drove > half of the delta. Same window/divisor as the lever baseline.
        let day_comp = comp_agg
            .get(&(a.series_key.clone(), a.date.clone()))
            .copied()
            .unwrap_or([0; 4]);
        let mut base_comp = [0i64; 4];
        for d in &report.days[start..pos] {
            if let Some(c) = comp_agg.get(&(a.series_key.clone(), d.clone())) {
                for i in 0..4 {
                    base_comp[i] = base_comp[i].saturating_add(c[i]);
                }
            }
        }
        let parts: Vec<CausePart> = (0..4)
            .map(|i| CausePart {
                label: CAUSE_LABELS[i].to_string(),
                delta_micros: day_comp[i]
                    .saturating_sub(scaled_div(base_comp[i], 1, divisor).unwrap_or(0)),
            })
            .collect();
        why.dominant_cause = attribute_delta(why.total_delta_micros, &parts);
        out.push(why);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trend::{TrendReport, TrendSeries};

    fn trend(days: &[&str], series: Vec<(&str, Vec<i64>)>) -> TrendReport {
        TrendReport {
            dimension: "by_model".into(),
            from: days.first().unwrap_or(&"").to_string(),
            to: days.last().unwrap_or(&"").to_string(),
            days: days.iter().map(|s| s.to_string()).collect(),
            series: series
                .into_iter()
                .map(|(k, per_day)| TrendSeries {
                    key: k.into(),
                    total_micros: per_day.iter().sum(),
                    per_day,
                })
                .collect(),
            pricing_version: "v".into(),
            estimated: true,
        }
    }

    #[test]
    fn detects_spike_new_and_vanished() {
        let t = trend(
            &["d1", "d2", "d3", "d4"],
            vec![
                ("steady", vec![100, 100, 400, 100]), // spike on d3
                ("appears", vec![0, 0, 50, 60]),      // new series on d3
                ("leaves", vec![80, 90, 0, 0]),       // vanished on d3
            ],
        );
        let a = detect(&t, 7, 100);
        assert!(a
            .iter()
            .any(|x| x.kind == AnomalyKind::Spike && x.series_key == "steady" && x.date == "d3"));
        assert!(a.iter().any(|x| x.kind == AnomalyKind::NewSeries
            && x.series_key == "appears"
            && x.date == "d3"));
        assert!(a.iter().any(|x| x.kind == AnomalyKind::VanishedSeries
            && x.series_key == "leaves"
            && x.date == "d3"));
        // Deterministic: same input -> same output.
        assert_eq!(a, detect(&t, 7, 100));
    }

    #[test]
    fn flat_series_has_no_anomalies() {
        let t = trend(&["d1", "d2", "d3"], vec![("x", vec![100, 100, 100])]);
        assert!(detect(&t, 7, 50).is_empty());
    }

    #[test]
    fn materiality_tiers_by_impact_and_share() {
        // Big absolute jump that dominates the day -> major; tiny jump -> minor (won't alarm).
        assert_eq!(materiality_of(5_000_000, 6_000_000), "major"); // $5 impact, 83% of day
        assert_eq!(materiality_of(300_000, 1_000_000), "material"); // $0.30, 30% of day
        assert_eq!(materiality_of(2_000, 5_000_000), "minor"); // $0.002 — noise
                                                               // A large impact that is a tiny share is NOT major (needs both).
        assert_eq!(materiality_of(2_000_000, 1_000_000_000), "minor");
        assert_eq!(
            materiality_of(i64::MAX, i64::MAX),
            "major",
            "large equal values are still a 100% share"
        );
    }

    #[test]
    fn negative_threshold_is_treated_as_zero() {
        let t = trend(&["d1", "d2"], vec![("x", vec![100, 100])]);
        assert!(detect(&t, 7, -100).is_empty());
    }

    #[test]
    fn detect_stamps_materiality_and_ack_filters() {
        let t = trend(
            &["d1", "d2", "d3"],
            vec![("steady", vec![2_000_000, 2_000_000, 8_000_000])], // $6 spike, 100% of day -> major
        );
        let a = detect(&t, 7, 100);
        let spike = a.iter().find(|x| x.kind == AnomalyKind::Spike).unwrap();
        assert_eq!(spike.materiality, "major");
        // Acknowledging it by key removes it everywhere.
        let key = anomaly_key(spike);
        assert_eq!(key, "d3:steady:spike");
        assert!(filter_acknowledged(a.clone(), &[key]).is_empty());
        assert_eq!(filter_acknowledged(a.clone(), &[]).len(), a.len()); // empty ack = no-op
    }

    // ---- Vantage-style noise filters ----
    #[test]
    fn noise_filters_default_is_byte_identical() {
        let t = trend(
            &["d1", "d2", "d3"],
            vec![("s", vec![1_000_000, 1_000_000, 3_000_000])],
        );
        assert_eq!(
            detect(&t, 7, 100),
            detect_filtered(&t, 7, 100, NoiseFilters::default())
        );
    }

    #[test]
    fn dollar_floor_drops_subcent_spikes() {
        // $2 spike (impact = 3M − 1M baseline).
        let t = trend(
            &["d1", "d2", "d3"],
            vec![("s", vec![1_000_000, 1_000_000, 3_000_000])],
        );
        // Floor above the $2 impact → dropped; floor below → kept.
        let hi = NoiseFilters {
            dollar_floor_micros: 5_000_000,
            ..Default::default()
        };
        let lo = NoiseFilters {
            dollar_floor_micros: 1_000_000,
            ..Default::default()
        };
        assert!(detect_filtered(&t, 7, 100, hi).is_empty());
        assert!(detect_filtered(&t, 7, 100, lo)
            .iter()
            .any(|a| a.kind == AnomalyKind::Spike));
    }

    #[test]
    fn pct_of_daily_floor_drops_rounding_errors() {
        // A real $2 spike, but drowned by a huge steady series → a tiny share of the day.
        let t = trend(
            &["d1", "d2", "d3"],
            vec![
                ("small", vec![1_000_000, 1_000_000, 3_000_000]),
                ("huge", vec![1_000_000_000, 1_000_000_000, 1_000_000_000]),
            ],
        );
        let f = NoiseFilters {
            pct_of_daily_floor: 5,
            ..Default::default()
        };
        // ~0.2% of the day → below the 5% floor → suppressed.
        assert!(detect_filtered(&t, 7, 100, f).is_empty());
        // Same spike surfaces with no floor.
        assert!(detect_filtered(&t, 7, 100, NoiseFilters::default())
            .iter()
            .any(|a| a.series_key == "small" && a.kind == AnomalyKind::Spike));
    }

    #[test]
    fn dedupe_window_suppresses_repeats() {
        // A sustained step-up: spikes on d3 AND d4 (each still above its trailing median).
        let t = trend(
            &["d1", "d2", "d3", "d4", "d5"],
            vec![(
                "s",
                vec![1_000_000, 1_000_000, 3_000_000, 3_000_000, 3_000_000],
            )],
        );
        let raw = detect_filtered(&t, 7, 100, NoiseFilters::default());
        assert_eq!(
            raw.iter().filter(|a| a.kind == AnomalyKind::Spike).count(),
            2
        );
        // A 2-day dedupe window collapses the consecutive repeat to a single alarm.
        let f = NoiseFilters {
            dedupe_window_days: 2,
            ..Default::default()
        };
        let deduped = detect_filtered(&t, 7, 100, f);
        assert_eq!(
            deduped
                .iter()
                .filter(|a| a.kind == AnomalyKind::Spike)
                .count(),
            1
        );
        assert_eq!(deduped[0].date, "d3"); // the first occurrence is kept
    }

    // ---- Single-cause spike attribution ----
    fn part(label: &str, delta: i64) -> CausePart {
        CausePart {
            label: label.into(),
            delta_micros: delta,
        }
    }

    #[test]
    fn attribute_delta_picks_the_majority_cause() {
        // +$6 total: system-prompt cache-write drove $5 of it (83%) → attributed.
        let got = attribute_delta(
            6_000_000,
            &[part("cache-write", 5_000_000), part("output", 1_000_000)],
        )
        .unwrap();
        assert_eq!(got.label, "cache-write");
        assert_eq!(got.delta_micros, 5_000_000);
        assert_eq!(got.share_pct, 83);
    }

    #[test]
    fn attribute_delta_share_is_clamped_to_100_when_offset_by_opposite_parts() {
        // Net +$2, but system-prompt drove +$10 while a $8 saving offset it. The reported share of
        // the NET change must not read a nonsensical ">100%".
        let got = attribute_delta(
            2_000_000,
            &[
                part("system-prompt", 10_000_000),
                part("cache-savings", -8_000_000),
            ],
        )
        .unwrap();
        assert_eq!(got.label, "system-prompt");
        assert!(
            (0..=100).contains(&got.share_pct),
            "share {} must be 0..=100",
            got.share_pct
        );
        assert_eq!(got.share_pct, 100);
    }

    #[test]
    fn attribute_delta_diffuse_returns_none() {
        // No single cause clears half → honestly attribute none.
        assert!(attribute_delta(
            6_000_000,
            &[
                part("a", 2_000_000),
                part("b", 2_000_000),
                part("c", 2_000_000)
            ]
        )
        .is_none());
        // Zero delta → nothing to attribute.
        assert!(attribute_delta(0, &[part("a", 1_000_000)]).is_none());
    }

    #[test]
    fn attribute_delta_ignores_opposite_direction_and_is_deterministic() {
        // During an up-spike, a cause that FELL doesn't get blamed; the dominant riser wins.
        let got = attribute_delta(
            4_000_000,
            &[part("riser", 5_000_000), part("faller", -1_000_000)],
        )
        .unwrap();
        assert_eq!(got.label, "riser");
        // Down-spike: the biggest DROP is attributed (same-direction as the negative total).
        let down =
            attribute_delta(-4_000_000, &[part("x", -3_000_000), part("y", -1_000_000)]).unwrap();
        assert_eq!(down.label, "x");
        // Deterministic label tiebreak on equal magnitude.
        let tie =
            attribute_delta(3_000_000, &[part("b", 2_000_000), part("a", 2_000_000)]).unwrap();
        assert_eq!(tie.label, "a");
        let extreme = attribute_delta(i64::MIN, &[part("all", i64::MIN)]).unwrap();
        assert_eq!(extreme.share_pct, 100);
    }

    // ---- Volume × size × efficiency decomposition ----
    fn agg(steps: u64, tokens: u64, micros: i64) -> DayAgg {
        DayAgg {
            steps,
            tokens,
            micros,
        }
    }

    #[test]
    fn decompose_isolates_pure_volume() {
        // 2× the requests, same tokens/request, same $/token → all volume.
        let w = decompose_change("m", agg(4, 400, 2000), agg(2, 200, 1000));
        assert_eq!(w.total_delta_micros, 1000);
        assert_eq!(
            (w.volume_micros, w.size_micros, w.efficiency_micros),
            (1000, 0, 0)
        );
    }

    #[test]
    fn decompose_isolates_pure_size() {
        // Same requests, 2× tokens/request, same $/token → all size.
        let w = decompose_change("m", agg(2, 400, 2000), agg(2, 200, 1000));
        assert_eq!(
            (w.volume_micros, w.size_micros, w.efficiency_micros),
            (0, 1000, 0)
        );
    }

    #[test]
    fn decompose_isolates_pure_efficiency() {
        // Same requests + tokens, 2× $/token (worse caching / pricier mix) → all efficiency.
        let w = decompose_change("m", agg(2, 200, 2000), agg(2, 200, 1000));
        assert_eq!(
            (w.volume_micros, w.size_micros, w.efficiency_micros),
            (0, 0, 1000)
        );
    }

    #[test]
    fn decompose_new_series_is_all_volume_and_sums_exactly() {
        let w = decompose_change("new", agg(3, 300, 1500), agg(0, 0, 0));
        assert_eq!(w.volume_micros, 1500);
        assert_eq!(w.size_micros + w.efficiency_micros, 0);
        // Mixed case: components always sum to the total delta exactly.
        let m = decompose_change("m", agg(5, 800, 5000), agg(2, 200, 1000));
        assert_eq!(
            m.volume_micros + m.size_micros + m.efficiency_micros,
            m.total_delta_micros
        );
        assert!(m.headline.contains("Spend on m rose"));
    }

    #[test]
    fn extreme_decomposition_and_formatting_do_not_overflow() {
        let w = decompose_change("m", agg(1, 1, i64::MIN), agg(1, 1, i64::MAX));
        assert_eq!(w.total_delta_micros, i64::MIN);
        assert_eq!(w.efficiency_micros, i64::MIN);
        assert!(w.headline.contains("-$9223372036854.78"));

        let wide = decompose_change(
            "m",
            agg(u64::MAX, u64::MAX, i64::MAX),
            agg(1, u64::MAX, i64::MAX),
        );
        assert_eq!(wide.volume_micros, i64::MAX);
        assert_eq!(wide.size_micros, i64::MIN);
    }

    #[test]
    fn spike_explanation_uses_each_runs_effective_price() {
        use crate::model::{Provider, RunRecord, StepRecord, UsageTokens};
        use crate::pricing::PricingTable;
        use crate::privacy::PrivacyPolicy;
        use crate::trend::{trend as build_trend, DatedRun, TrendDimension};

        let pricing = PricingTable::from_toml_str(
            r#"
version = "dated"
effective_date = "2026-01-01"
[[model]]
provider = "anthropic"
model_id = "m"
input_micro_per_mtok = 1000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
[[model]]
provider = "anthropic"
model_id = "m"
effective_date = "2026-06-01"
input_micro_per_mtok = 3000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#,
        )
        .unwrap();
        let make = |date: &str| {
            let run_id = date.to_string();
            DatedRun {
                date: run_id.clone(),
                run: RunRecord {
                    run_id: run_id.clone(),
                    steps: vec![StepRecord {
                        run_id,
                        step_ordinal: 1,
                        provider: Provider::Anthropic,
                        model: "m".into(),
                        usage: UsageTokens {
                            fresh_input: 1_000_000,
                            ..Default::default()
                        },
                        shape: crate::wire::anthropic_request_shape(
                            br#"{"model":"m","messages":[]}"#,
                            &PrivacyPolicy::default(),
                        )
                        .unwrap(),
                        stop_reason: None,
                        duration_ms: 0,
                        start_unix_nano: None,
                        trace_id: None,
                        span_id: None,
                        parent_span_id: None,
                    }],
                },
            }
        };
        let dated = vec![make("2026-05-31"), make("2026-06-01")];
        let report = build_trend(
            &dated,
            "2026-05-31",
            "2026-06-01",
            &pricing,
            TrendDimension::Total,
        );
        let whys = decompose_spikes(&report, &dated, TrendDimension::Total, 1, 50, &pricing);
        assert_eq!(whys.len(), 1);
        assert_eq!(whys[0].total_delta_micros, 2_000_000);
        assert_eq!(whys[0].efficiency_micros, 2_000_000);
    }
}
