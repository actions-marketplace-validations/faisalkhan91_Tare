//! The bundled sample run shared by every surface that seeds or previews demo data:
//! the CLI `tare demo`, the desktop `seed_demo` command, and the flamegraph preview. Kept in core
//! (pure, store-free) so tare-cli and tare-tauri seed the identical run without duplicating the
//! fixtures. Built from the `bloated_system_prompt` fixtures — a deliberately wasteful run that
//! shows Tare finding a trimmable system prompt.

use crate::ingest_step;
use crate::model::{Provider, RunRecord};

/// The demo run's id and its fixed seed date — deterministic and obviously not "today", so a seeded
/// sample is easy to spot (and the UI flags it with a SAMPLE banner).
pub const DEMO_RUN_ID: &str = "demo";
pub const DEMO_DATE: &str = "2026-06-01";

/// The sample run as a `RunRecord` (three steps). Store-free: callers seed it or render it.
pub fn demo_run() -> Result<RunRecord, String> {
    let steps = [
        (
            &include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json")[..],
            &include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json")[..],
        ),
        (
            &include_bytes!("../../fixtures/bloated_system_prompt/step2.request.json")[..],
            &include_bytes!("../../fixtures/bloated_system_prompt/step2.response.json")[..],
        ),
        (
            &include_bytes!("../../fixtures/bloated_system_prompt/step3.request.json")[..],
            &include_bytes!("../../fixtures/bloated_system_prompt/step3.response.json")[..],
        ),
    ];
    let mut run = RunRecord::new(DEMO_RUN_ID);
    for (i, (req, resp)) in steps.iter().enumerate() {
        run.steps.push(ingest_step(
            DEMO_RUN_ID,
            i as u32 + 1,
            Provider::Anthropic,
            req,
            resp,
        )?);
    }
    Ok(run)
}

/// A deterministic multi-week dataset: `weeks × 7 × runs_per_day` dated runs spanning
/// `[start_date, start_date + weeks·7)`, each a copy of the demo run with a unique `(date, i)` id.
/// Store-free + clock-free (dates from `calendar`), so it drives read-path/range tests and previews
/// without any real capture. Returns `(created_date, RunRecord)` pairs, oldest first. Invalid dates
/// and requests above the fixture safety bound return an empty set.
pub fn seed_weeks(start_date: &str, weeks: u32, runs_per_day: u32) -> Vec<(String, RunRecord)> {
    const MAX_SEEDED_RUNS: u64 = 100_000;
    let Some(base) = crate::calendar::parse_date(start_date) else {
        return Vec::new();
    };
    let days = u64::from(weeks).saturating_mul(7);
    let total = days.saturating_mul(u64::from(runs_per_day));
    if total > MAX_SEEDED_RUNS {
        return Vec::new();
    }
    let template = demo_run().unwrap_or_else(|_| RunRecord::new("seed"));
    let mut out = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    for d in 0..days {
        let date =
            crate::calendar::format_date(base.saturating_add(i64::try_from(d).unwrap_or(i64::MAX)));
        for i in 0..runs_per_day {
            let id = format!("seed-{date}-{i}");
            let mut run = RunRecord::new(&id);
            run.steps = template
                .steps
                .iter()
                .enumerate()
                .map(|(j, s)| {
                    let mut s = s.clone();
                    s.run_id = id.clone();
                    s.step_ordinal = j as u32 + 1;
                    s
                })
                .collect();
            out.push((date.clone(), run));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_run_has_three_anthropic_steps() {
        let run = demo_run().unwrap();
        assert_eq!(run.run_id, DEMO_RUN_ID);
        assert_eq!(run.steps.len(), 3);
        assert!(run.steps.iter().all(|s| s.provider == Provider::Anthropic));
    }

    #[test]
    fn seed_weeks_spans_the_window_with_unique_dated_runs() {
        let seeded = seed_weeks("2026-06-01", 3, 2);
        assert_eq!(seeded.len(), 3 * 7 * 2, "weeks × 7 × runs_per_day");
        // Dates span exactly 21 days, oldest first, no gaps.
        let dates: std::collections::BTreeSet<&str> =
            seeded.iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(dates.len(), 21);
        assert_eq!(seeded.first().unwrap().0, "2026-06-01");
        assert_eq!(seeded.last().unwrap().0, "2026-06-21");
        // Run ids are unique, and each step's run_id matches its run.
        let ids: std::collections::BTreeSet<&str> =
            seeded.iter().map(|(_, r)| r.run_id.as_str()).collect();
        assert_eq!(ids.len(), seeded.len());
        assert!(seeded
            .iter()
            .all(|(_, r)| r.steps.iter().all(|s| s.run_id == r.run_id)));
        // Deterministic: same call, same output.
        assert_eq!(seed_weeks("2026-06-01", 3, 2), seeded);
        assert!(seed_weeks("not-a-date", 1, 1).is_empty());
        assert!(seed_weeks("2026-06-01", u32::MAX, u32::MAX).is_empty());
    }
}
