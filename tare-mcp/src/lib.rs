//! Tare MCP server: exposes the read-only cost views as Model Context Protocol tools so an
//! agent can ask "what did I spend / where should I trim?" mid-session. ALL tool logic lives
//! in the pure `dispatch_rpc` (a JSON-RPC request `Value` -> response `Value`), so it is fully
//! testable without a process, socket, or the stdio loop. Every dollar figure is ESTIMATED.
//!
//! Transports: stdio JSON-RPC (`serve_stdio`) is the shipped one. A loopback streamable-HTTP
//! transport would live behind a `serve` feature (out of the offline gate) and is not built here.

use serde_json::{json, Value};
use tare_core::{attribute, flamegraph::build_flamegraph, speedscope, svg, PricingTable};
use tare_store::Store;

pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The tool catalog (also the `tools/list` payload). Kept as data so the golden is stable.
pub fn tools() -> Value {
    json!([
        {"name": "tare_today", "description": "Today's estimated spend (micro-USD). `date` (YYYY-MM-DD) is optional and defaults to the current UTC date.",
         "inputSchema": {"type": "object", "properties": {"date": {"type": "string"}}}},
        {"name": "tare_report", "description": "Ranked 'trim here -> save $X' report (estimated).",
         "inputSchema": {"type": "object", "properties": {"today": {"type": "boolean"}}}},
        {"name": "tare_runs", "description": "List captured run ids (newest last).",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "tare_run_status", "description": "Settled spend status for a run: integer micro-USD, step count, top cause (live budget/decision arrive via the proxy x-tare-run-* response headers).",
         "inputSchema": {"type": "object", "properties": {"run_id": {"type": "string"}}, "required": ["run_id"]}},
        {"name": "tare_rollup", "description": "Estimated spend grouped by a correlation label.",
         "inputSchema": {"type": "object", "properties": {"by": {"type": "string", "enum": ["step","component","parent"]}}}},
        {"name": "tare_flamegraph_model", "description": "Flamegraph view-model for a run.",
         "inputSchema": {"type": "object", "properties": {"run_id": {"type": "string"}}, "required": ["run_id"]}},
        {"name": "tare_flamegraph_svg", "description": "Flamegraph SVG for a run.",
         "inputSchema": {"type": "object", "properties": {"run_id": {"type": "string"}}, "required": ["run_id"]}},
        {"name": "tare_flamegraph_speedscope", "description": "speedscope JSON for a run.",
         "inputSchema": {"type": "object", "properties": {"run_id": {"type": "string"}}, "required": ["run_id"]}},
        {"name": "tare_explain", "description": "Plain-language narrative of a run's estimated spend.",
         "inputSchema": {"type": "object", "properties": {"run_id": {"type": "string"}}, "required": ["run_id"]}},
        {"name": "tare_advise", "description": "Prompt-cache recommendations (estimated, retrospective).",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "tare_whatif", "description": "Reprice captured runs on another model, or recommend cheaper ones (ESTIMATE; approximate-stamped, unpriced target errors).",
         "inputSchema": {"type": "object", "properties": {"swap_all_to": {"type": "string"}, "from": {"type": "string"}, "to": {"type": "string"}, "recommend": {"type": "boolean"}, "cross_provider": {"type": "boolean"}}}},
        {"name": "tare_anomalies", "description": "Deterministic spend anomalies (spike/new/vanished) over the trend.",
         "inputSchema": {"type": "object", "properties": {"by": {"type": "string"}, "window": {"type": "integer"}, "threshold": {"type": "integer"}}}},
        {"name": "tare_trend", "description": "Spend over a dense calendar window, by total/provider/model/cause.",
         "inputSchema": {"type": "object", "properties": {"by": {"type": "string"}, "from": {"type": "string"}, "to": {"type": "string"}}}},
        {"name": "tare_pricing", "description": "Pricing version/effective-date + models with no bundled price.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "tare_bisect", "description": "First day spend crossed a threshold over the trailing median.",
         "inputSchema": {"type": "object", "properties": {"window": {"type": "integer"}, "threshold": {"type": "integer"}}}},
        {"name": "tare_diff", "description": "Diff two `tare report` JSON documents (before/after).",
         "inputSchema": {"type": "object", "properties": {"before": {"type": "object"}, "after": {"type": "object"}}, "required": ["before", "after"]}},
        {"name": "tare_gate", "description": "Self-check a run against a budget BEFORE finishing: pass/fail booleans for a spend cap (max_spend_usd) and/or step cap (max_steps). Estimated.",
         "inputSchema": {"type": "object", "properties": {"run_id": {"type": "string"}, "max_spend_usd": {"type": "number"}, "max_steps": {"type": "integer"}}, "required": ["run_id"]}},
        {"name": "tare_budget_remaining", "description": "Dollars left under a spend cap (max_spend_usd) for a run (run_id), a day (date), or all captured spend. Estimated integer micro-USD.",
         "inputSchema": {"type": "object", "properties": {"max_spend_usd": {"type": "number"}, "run_id": {"type": "string"}, "date": {"type": "string"}}, "required": ["max_spend_usd"]}},
        {"name": "tare_estimate", "description": "Pre-flight cost band from a stored run's shape (no new spend), optionally repriced on another model ('to'). Cross-model is tokenizer-approximate; unpriced target errors.",
         "inputSchema": {"type": "object", "properties": {"like": {"type": "string"}, "to": {"type": "string"}}, "required": ["like"]}}
    ])
}

fn text_content(s: String) -> Value {
    json!({"content": [{"type": "text", "text": s}], "isError": false})
}
/// Current unix seconds from the system clock (the MCP server is long-lived production code, so —
/// unlike a workflow script — reading the wall clock here is fine). Used to default `tare_today`'s
/// date. Saturates to 0 before the epoch rather than panicking.
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn tool_err(msg: String) -> Value {
    json!({"content": [{"type": "text", "text": msg}], "isError": true})
}

fn allowed_tool_args(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "tare_today" => &["date"],
        "tare_report" => &["today"],
        "tare_runs" | "tare_advise" | "tare_pricing" => &[],
        "tare_run_status"
        | "tare_flamegraph_model"
        | "tare_flamegraph_svg"
        | "tare_flamegraph_speedscope"
        | "tare_explain" => &["run_id"],
        "tare_rollup" => &["by"],
        "tare_whatif" => &["swap_all_to", "from", "to", "recommend", "cross_provider"],
        "tare_anomalies" => &["by", "window", "threshold"],
        "tare_trend" => &["by", "from", "to"],
        "tare_bisect" => &["window", "threshold"],
        "tare_diff" => &["before", "after"],
        "tare_gate" => &["run_id", "max_spend_usd", "max_steps"],
        "tare_budget_remaining" => &["max_spend_usd", "run_id", "date"],
        "tare_estimate" => &["like", "to"],
        _ => return None,
    })
}

fn optional_string_arg<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!("{key}: expected a string")),
    }
}

fn required_string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    let value = optional_string_arg(args, key)?.ok_or_else(|| format!("'{key}' required"))?;
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(format!(
            "{key}: expected a non-empty string without control characters"
        ));
    }
    Ok(value)
}

fn optional_bool_arg(args: &Value, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{key}: expected a boolean")),
    }
}

fn optional_u64_arg(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{key}: expected a non-negative integer")),
    }
}

fn optional_usize_arg(args: &Value, key: &str) -> Result<Option<usize>, String> {
    optional_u64_arg(args, key)?
        .map(|value| {
            usize::try_from(value).map_err(|_| format!("{key}: integer is too large for this host"))
        })
        .transpose()
}

fn optional_micros_arg(args: &Value, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(value) => {
            let dollars = value
                .as_f64()
                .ok_or_else(|| format!("{key}: expected a non-negative number"))?;
            tare_core::config::dollars_to_micros(dollars)
                .map(Some)
                .ok_or_else(|| format!("{key}: amount is negative, non-finite, or too large"))
        }
    }
}

/// Execute one tool by name. Pure: all logic flows through tare-core/tare-store. Returns an
/// MCP `tools/call` result object (content + isError).
/// Build a trend over the full stored date range (clock-free: bounds come from the store), or
/// an empty report when the store has no runs. Shared by the trend/anomaly/bisect tools.
fn trend_range(
    store: &Store,
    pricing: &PricingTable,
    dim: tare_core::trend::TrendDimension,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<tare_core::trend::TrendReport, String> {
    for (name, value) in [("from", from), ("to", to)] {
        if let Some(value) = value {
            if tare_core::calendar::parse_date(value).is_none() {
                return Err(format!("{name}: expected a valid YYYY-MM-DD date"));
            }
        }
    }
    if let (Some(from), Some(to)) = (from, to) {
        if tare_core::calendar::parse_date(from) > tare_core::calendar::parse_date(to) {
            return Err("from: must not be after to".into());
        }
    }
    match store.run_date_bounds()? {
        Some((stored_from, stored_to)) => {
            let from = from.unwrap_or(&stored_from);
            let to = to.unwrap_or(&stored_to);
            store.trend_in_range(from, to, pricing, dim)
        }
        None => Ok(tare_core::trend::TrendReport {
            dimension: dim.as_str().to_string(),
            from: from.unwrap_or_default().to_string(),
            to: to.unwrap_or_default().to_string(),
            days: Vec::new(),
            series: Vec::new(),
            pricing_version: pricing.version.clone(),
            estimated: true,
        }),
    }
}

pub fn call_tool(name: &str, args: &Value, store: &Store, pricing: &PricingTable) -> Value {
    let Some(args_object) = args.as_object() else {
        return tool_err("tool arguments must be a JSON object".into());
    };
    if let Some(allowed) = allowed_tool_args(name) {
        if let Some(key) = args_object
            .keys()
            .find(|key| !allowed.contains(&key.as_str()))
        {
            return tool_err(format!("unknown argument {key:?} for {name}"));
        }
    }
    let result: Result<String, String> = (|| match name {
        "tare_today" => {
            // Default `date` to the current UTC civil date when the caller omits it:
            // an agent asking "what did I spend today" shouldn't have to compute the date first. An
            // explicit `date` (any YYYY-MM-DD, for a specific day) still wins.
            let today = tare_core::calendar::civil_date_for(now_unix_secs(), 0);
            let date = optional_string_arg(args, "date")?.unwrap_or(&today);
            if tare_core::calendar::parse_date(date).is_none() {
                return Err("date: expected a valid YYYY-MM-DD date".into());
            }
            let t = store.today_spend(date, pricing)?;
            Ok(serde_json::to_string(&t).map_err(|e| e.to_string())?)
        }
        "tare_report" => {
            let today = optional_bool_arg(args, "today")?.unwrap_or(false);
            let runs = if today {
                let date = tare_core::calendar::civil_date_for(now_unix_secs(), 0);
                store.load_runs_on_date(&date)?
            } else {
                store.load_runs()?
            };
            let report = attribute::build_report(&runs, pricing);
            Ok(serde_json::to_string(&report).map_err(|e| e.to_string())?)
        }
        "tare_runs" => {
            let ids: Vec<String> = store.load_runs()?.into_iter().map(|r| r.run_id).collect();
            Ok(serde_json::to_string(&ids).map_err(|e| e.to_string())?)
        }
        "tare_run_status" => {
            // Settled spend status for a run. The live budget and decision are delivered
            // in-band via the proxy's `x-tare-run-*` response headers; this store-backed view
            // reports what's persisted: integer spend, step count, and the top cause.
            let run_id = required_string_arg(args, "run_id")?;
            let run = store
                .load_run(run_id)?
                .ok_or_else(|| format!("run `{run_id}` not found"))?;
            let report = attribute::build_report(std::slice::from_ref(&run), pricing);
            let top_cause = report.rows.first().map(|r| r.cause.clone());
            let status = serde_json::json!({
                "run_id": run_id,
                "micros": report.total_micros,
                "steps": run.steps.len(),
                "top_cause": top_cause,
            });
            Ok(serde_json::to_string(&status).map_err(|e| e.to_string())?)
        }
        "tare_gate" => {
            // Self-check a run against a budget before finishing. Estimated.
            let run_id = required_string_arg(args, "run_id")?;
            let run = store
                .load_run(run_id)?
                .ok_or_else(|| format!("run `{run_id}` not found"))?;
            let report = attribute::build_report(std::slice::from_ref(&run), pricing);
            let spend = report.total_micros;
            let steps = run.steps.len() as u64;
            let max_micros = optional_micros_arg(args, "max_spend_usd")?;
            let max_steps = optional_u64_arg(args, "max_steps")?;
            let over_spend = max_micros.map(|m| spend > m).unwrap_or(false);
            let over_steps = max_steps.map(|m| steps > m).unwrap_or(false);
            let out = serde_json::json!({
                "run_id": run_id,
                "pass": !over_spend && !over_steps,
                "spend_micros": spend,
                "steps": steps,
                "max_spend_micros": max_micros,
                "max_steps": max_steps,
                "over_spend": over_spend,
                "over_steps": over_steps,
            });
            Ok(serde_json::to_string(&out).map_err(|e| e.to_string())?)
        }
        "tare_budget_remaining" => {
            // Dollars left under a cap for a run / a day / all captured spend.
            let max_micros = optional_micros_arg(args, "max_spend_usd")?
                .ok_or("tare_budget_remaining: 'max_spend_usd' required")?;
            let run_id = optional_string_arg(args, "run_id")?;
            let date = optional_string_arg(args, "date")?;
            if run_id.is_some() && date.is_some() {
                return Err("tare_budget_remaining: provide only one of 'run_id' or 'date'".into());
            }
            let (scope, spent) = if let Some(run_id) = run_id {
                if run_id.trim().is_empty() || run_id.chars().any(char::is_control) {
                    return Err(
                        "run_id: expected a non-empty string without control characters".into(),
                    );
                }
                let run = store
                    .load_run(run_id)?
                    .ok_or_else(|| format!("run `{run_id}` not found"))?;
                let m = attribute::build_report(std::slice::from_ref(&run), pricing).total_micros;
                (format!("run:{run_id}"), m)
            } else if let Some(date) = date {
                if tare_core::calendar::parse_date(date).is_none() {
                    return Err("date: expected a valid YYYY-MM-DD date".into());
                }
                (
                    format!("date:{date}"),
                    store.today_spend(date, pricing)?.total_micros,
                )
            } else {
                let m = attribute::build_report(&store.load_runs()?, pricing).total_micros;
                ("all".to_string(), m)
            };
            let out = serde_json::json!({
                "scope": scope,
                "max_micros": max_micros,
                "spent_micros": spent,
                "remaining_micros": max_micros.saturating_sub(spent).max(0),
                "over_budget": spent > max_micros,
            });
            Ok(serde_json::to_string(&out).map_err(|e| e.to_string())?)
        }
        "tare_estimate" => {
            // Pre-flight cost band from a stored run's shape, no new spend.
            let run_id = required_string_arg(args, "like")?;
            let run = store
                .load_run(run_id)?
                .ok_or_else(|| format!("run `{run_id}` not found"))?;
            let to = optional_string_arg(args, "to")?;
            if to.is_some_and(|value| value.trim().is_empty()) {
                return Err("to: expected a non-empty model name".into());
            }
            let est = tare_core::estimate::estimate_like(&run, pricing, to)?;
            Ok(serde_json::to_string(&est).map_err(|e| e.to_string())?)
        }
        "tare_rollup" => {
            let dim = match optional_string_arg(args, "by")? {
                Some(raw) => tare_core::rollup::RollupDim::parse(raw)
                    .ok_or_else(|| format!("by: invalid rollup dimension {raw:?}"))?,
                None => tare_core::rollup::RollupDim::Step,
            };
            let rep = tare_core::rollup::rollup(&store.load_runs()?, pricing, dim);
            Ok(serde_json::to_string(&rep).map_err(|e| e.to_string())?)
        }
        "tare_flamegraph_model" | "tare_flamegraph_svg" | "tare_flamegraph_speedscope" => {
            let run_id = required_string_arg(args, "run_id")?;
            let run = store
                .load_run(run_id)?
                .ok_or_else(|| format!("run `{run_id}` not found"))?;
            let model = build_flamegraph(&run, pricing);
            match name {
                "tare_flamegraph_svg" => Ok(svg::render_svg(&model)),
                "tare_flamegraph_speedscope" => Ok(serde_json::to_string(&speedscope::export(
                    &model,
                    env!("CARGO_PKG_VERSION"),
                ))
                .map_err(|e| e.to_string())?),
                _ => Ok(serde_json::to_string(&model).map_err(|e| e.to_string())?),
            }
        }
        "tare_advise" => {
            let advice = tare_core::advise::advise(&store.load_runs()?, pricing);
            Ok(serde_json::to_string(&advice).map_err(|e| e.to_string())?)
        }
        "tare_whatif" => {
            // Recommend mode: rank cheaper priced models (nothing labeled exact).
            let recommend = optional_bool_arg(args, "recommend")?.unwrap_or(false);
            let cross = optional_bool_arg(args, "cross_provider")?.unwrap_or(false);
            let swap_all_to = optional_string_arg(args, "swap_all_to")?;
            let from = optional_string_arg(args, "from")?;
            let to = optional_string_arg(args, "to")?;
            if recommend {
                if swap_all_to.is_some() || from.is_some() || to.is_some() {
                    return Err(
                        "tare_whatif: recommendation mode cannot be combined with swap arguments"
                            .into(),
                    );
                }
                let rec = tare_core::whatif::recommend(&store.load_runs()?, pricing, cross);
                return serde_json::to_string(&rec).map_err(|e| e.to_string());
            }
            if cross {
                return Err("tare_whatif: 'cross_provider' requires 'recommend': true".into());
            }
            let mut swaps = Vec::new();
            if let Some(to) = swap_all_to {
                if to.trim().is_empty() {
                    return Err("swap_all_to: expected a non-empty model name".into());
                }
                swaps.push(tare_core::whatif::Swap::AllTo { to: to.to_string() });
            }
            match (from, to) {
                (Some(from), Some(to)) if !from.trim().is_empty() && !to.trim().is_empty() => {
                    swaps.push(tare_core::whatif::Swap::Model {
                        from: from.to_string(),
                        to: to.to_string(),
                    });
                }
                (None, None) => {}
                (Some(_), Some(_)) => {
                    return Err("tare_whatif: 'from' and 'to' must not be empty".into())
                }
                _ => return Err("tare_whatif: 'from' and 'to' must be supplied together".into()),
            }
            if swaps.is_empty() {
                return Err("tare_whatif: provide 'swap_all_to' or 'from'+'to'".into());
            }
            // Approximation markers and unpriced errors surface to the agent verbatim.
            let rep = tare_core::whatif::whatif(&store.load_runs()?, pricing, &swaps)?;
            Ok(serde_json::to_string(&rep).map_err(|e| e.to_string())?)
        }
        "tare_anomalies" => {
            let dim = match optional_string_arg(args, "by")? {
                Some(raw) => tare_core::trend::TrendDimension::parse(raw)
                    .ok_or_else(|| format!("by: invalid trend dimension {raw:?}"))?,
                None => tare_core::trend::TrendDimension::Total,
            };
            let window = optional_usize_arg(args, "window")?.unwrap_or(7);
            if window == 0 {
                return Err("window: expected an integer greater than zero".into());
            }
            let threshold = optional_u64_arg(args, "threshold")?
                .map(|value| {
                    i64::try_from(value).map_err(|_| "threshold: integer is too large".to_string())
                })
                .transpose()?
                .unwrap_or(50);
            let trend = trend_range(store, pricing, dim, None, None)?;
            let found = tare_core::anomaly::detect(&trend, window, threshold);
            Ok(serde_json::to_string(&found).map_err(|e| e.to_string())?)
        }
        "tare_trend" => {
            let dim = match optional_string_arg(args, "by")? {
                Some(raw) => tare_core::trend::TrendDimension::parse(raw)
                    .ok_or_else(|| format!("by: invalid trend dimension {raw:?}"))?,
                None => tare_core::trend::TrendDimension::Total,
            };
            let from = optional_string_arg(args, "from")?;
            let to = optional_string_arg(args, "to")?;
            let trend = trend_range(store, pricing, dim, from, to)?;
            Ok(serde_json::to_string(&trend).map_err(|e| e.to_string())?)
        }
        "tare_pricing" => {
            let unpriced = attribute::build_report(&store.load_runs()?, pricing).unpriced;
            Ok(serde_json::to_string(&serde_json::json!({
                "version": pricing.version,
                "effective_date": pricing.effective_date,
                "note": pricing.note,
                "unpriced": unpriced,
            }))
            .map_err(|e| e.to_string())?)
        }
        "tare_bisect" => {
            let window = optional_usize_arg(args, "window")?.unwrap_or(7);
            if window == 0 {
                return Err("window: expected an integer greater than zero".into());
            }
            let threshold = optional_u64_arg(args, "threshold")?
                .map(|value| {
                    i64::try_from(value).map_err(|_| "threshold: integer is too large".to_string())
                })
                .transpose()?
                .unwrap_or(50);
            let trend = trend_range(
                store,
                pricing,
                tare_core::trend::TrendDimension::Total,
                None,
                None,
            )?;
            let values: Vec<i64> = trend
                .series
                .first()
                .map(|s| s.per_day.clone())
                .unwrap_or_default();
            let reg = tare_core::bisect::bisect(&trend.days, &values, window, threshold);
            Ok(serde_json::to_string(&reg).map_err(|e| e.to_string())?)
        }
        "tare_explain" => {
            let run_id = required_string_arg(args, "run_id")?;
            let run = store
                .load_run(run_id)?
                .ok_or_else(|| format!("run `{run_id}` not found"))?;
            let runs = [run];
            let report = attribute::build_report(&runs, pricing);
            let ledger = tare_core::savings::savings(&runs, pricing);
            Ok(tare_core::explain::explain(&report, &ledger))
        }
        "tare_diff" => {
            let before: attribute::Report = serde_json::from_value(
                args.get("before")
                    .cloned()
                    .ok_or("tare_diff: 'before' required")?,
            )
            .map_err(|e| format!("before: {e}"))?;
            let after: attribute::Report = serde_json::from_value(
                args.get("after")
                    .cloned()
                    .ok_or("tare_diff: 'after' required")?,
            )
            .map_err(|e| format!("after: {e}"))?;
            let d = tare_core::diff::diff_reports(&before, &after);
            Ok(serde_json::to_string(&d).map_err(|e| e.to_string())?)
        }
        other => Err(format!("unknown tool: {other}")),
    })();
    match result {
        Ok(s) => text_content(s),
        Err(e) => tool_err(e),
    }
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn rpc_err(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Handle one JSON-RPC request. Notifications (no `id`) return `None`. Pure — no I/O.
pub fn dispatch_rpc(req: &Value, store: &Store, pricing: &PricingTable) -> Option<Value> {
    let structurally_valid = req.is_object()
        && req.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && req.get("method").and_then(Value::as_str).is_some()
        && req
            .get("id")
            .is_none_or(|id| id.is_null() || id.is_string() || id.is_number());
    if !structurally_valid {
        return Some(rpc_err(Value::Null, -32600, "invalid JSON-RPC request"));
    }

    let method = req["method"].as_str().expect("validated method");
    let notification = req.get("id").is_none();
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let invalid_params = |message: &str| rpc_err(id.clone(), -32602, message);
    let response = match method {
        "initialize" => {
            if req
                .get("params")
                .is_some_and(|params| !params.is_null() && !params.is_object())
            {
                invalid_params("initialize params must be an object")
            } else {
                rpc_ok(
                    id.clone(),
                    json!({
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "tare-mcp", "version": env!("CARGO_PKG_VERSION")}
                    }),
                )
            }
        }
        "tools/list" => {
            if req
                .get("params")
                .is_some_and(|params| !params.is_null() && !params.is_object())
            {
                invalid_params("tools/list params must be an object")
            } else {
                rpc_ok(id.clone(), json!({"tools": tools()}))
            }
        }
        "tools/call" => {
            let Some(params) = req.get("params").and_then(Value::as_object) else {
                let response = invalid_params("tools/call params must be an object");
                return (!notification).then_some(response);
            };
            let Some(name) = params
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            else {
                let response = invalid_params("tools/call requires a non-empty string name");
                return (!notification).then_some(response);
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !args.is_object() {
                invalid_params("tools/call arguments must be an object")
            } else {
                rpc_ok(id.clone(), call_tool(name, &args, store, pricing))
            }
        }
        "ping" => rpc_ok(id.clone(), json!({})),
        other => rpc_err(id.clone(), -32601, &format!("method not found: {other}")),
    };
    (!notification).then_some(response)
}

fn serve_stream<R: std::io::BufRead, W: std::io::Write>(
    reader: R,
    writer: &mut W,
    store: &Store,
    pricing: &PricingTable,
) -> Result<(), String> {
    for line in reader.lines() {
        let line = line.map_err(|e| format!("stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(req) => dispatch_rpc(&req, store, pricing),
            Err(_) => Some(rpc_err(Value::Null, -32700, "parse error")),
        };
        if let Some(resp) = response {
            let s = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
            writeln!(writer, "{s}").map_err(|e| format!("stdout: {e}"))?;
            writer.flush().map_err(|e| format!("flush: {e}"))?;
        }
    }
    Ok(())
}

/// stdio JSON-RPC loop: one JSON object per line in, one per line out (line-delimited).
pub fn serve_stdio(store: &Store, pricing: &PricingTable) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    serve_stream(stdin.lock(), &mut stdout, store, pricing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tare_core::ingest_step;
    use tare_core::model::Provider;

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    fn loaded_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        let req = include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json");
        let resp = include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json");
        let step = ingest_step("r1", 1, Provider::Anthropic, req, resp).unwrap();
        store.record_step(&step, "2026-06-24").unwrap();
        store
    }

    #[test]
    fn new_tools_equal_their_core_calls() {
        let store = loaded_store();
        let p = pricing();
        let text = |name: &str, args: serde_json::Value| -> String {
            let out = call_tool(name, &args, &store, &p);
            assert_eq!(out["isError"], false, "{name} errored: {out:?}");
            out["content"][0]["text"].as_str().unwrap().to_string()
        };

        // tare_whatif == whatif::whatif (and stays approximate-stamped).
        let wi = text("tare_whatif", json!({"swap_all_to": "gpt-5-mini"}));
        let direct = serde_json::to_value(
            tare_core::whatif::whatif(
                &store.load_runs().unwrap(),
                &p,
                &[tare_core::whatif::Swap::AllTo {
                    to: "gpt-5-mini".into(),
                }],
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&wi).unwrap(),
            direct
        );
        assert!(wi.contains("approximate_tokenizer"));

        // tare_trend / tare_anomalies / tare_bisect / tare_pricing all return without error.
        assert!(text("tare_trend", json!({"by": "model"})).contains("days"));
        let _ = text("tare_anomalies", json!({}));
        let _ = text("tare_bisect", json!({}));
        assert!(text("tare_pricing", json!({})).contains("version"));

        // tare_run_status: integer micros, steps, and top cause matching the store report.
        let status: serde_json::Value =
            serde_json::from_str(&text("tare_run_status", json!({"run_id": "r1"}))).unwrap();
        let report = attribute::build_report(&store.load_runs().unwrap(), &p);
        assert_eq!(status["micros"].as_i64().unwrap(), report.total_micros);
        assert_eq!(status["steps"].as_u64().unwrap(), 1);
        assert_eq!(
            status["top_cause"],
            json!(report.rows.first().map(|r| r.cause.clone()))
        );
        // A missing run id is an error, not a fabricated zero.
        assert_eq!(
            call_tool("tare_run_status", &json!({"run_id": "ghost"}), &store, &p)["isError"],
            true
        );

        // An unpriced what-if target returns an error through the tool.
        let bad = call_tool(
            "tare_whatif",
            &json!({"swap_all_to": "nope-model"}),
            &store,
            &p,
        );
        assert_eq!(bad["isError"], true);
    }

    #[test]
    fn cost_self_check_tools_gate_budget_estimate() {
        let store = loaded_store();
        let p = pricing();
        let json_of = |name: &str, args: serde_json::Value| -> serde_json::Value {
            let out = call_tool(name, &args, &store, &p);
            assert_eq!(out["isError"], false, "{name} errored: {out:?}");
            serde_json::from_str(out["content"][0]["text"].as_str().unwrap()).unwrap()
        };
        let spend = attribute::build_report(&store.load_runs().unwrap(), &p).total_micros;
        assert!(spend > 0, "fixture must have priced spend");

        // tare_gate: passes under a generous cap, fails a tight one; step cap independent.
        let pass = json_of(
            "tare_gate",
            json!({"run_id": "r1", "max_spend_usd": 1000.0}),
        );
        assert_eq!(pass["pass"], true);
        assert_eq!(pass["spend_micros"].as_i64().unwrap(), spend);
        let fail = json_of("tare_gate", json!({"run_id": "r1", "max_spend_usd": 0.0}));
        assert_eq!(fail["pass"], false);
        assert_eq!(fail["over_spend"], true);
        let steps_fail = json_of("tare_gate", json!({"run_id": "r1", "max_steps": 0}));
        assert_eq!(steps_fail["over_steps"], true);
        // Missing run -> error, never a fabricated pass.
        assert_eq!(
            call_tool("tare_gate", &json!({"run_id": "ghost"}), &store, &p)["isError"],
            true
        );

        // tare_budget_remaining: remaining = cap − spent (run scope), clamped at 0.
        let cap_usd = (spend as f64 / 1_000_000.0) + 1.0; // $1 headroom
        let rem = json_of(
            "tare_budget_remaining",
            json!({"run_id": "r1", "max_spend_usd": cap_usd}),
        );
        assert_eq!(rem["spent_micros"].as_i64().unwrap(), spend);
        // ~$1 headroom (the dollar→micros boundary can truncate by one micro-USD).
        assert!((rem["remaining_micros"].as_i64().unwrap() - 1_000_000).abs() <= 1);
        assert_eq!(rem["over_budget"], false);
        // Over-budget clamps remaining to 0.
        let over = json_of("tare_budget_remaining", json!({"max_spend_usd": 0.0}));
        assert_eq!(over["remaining_micros"].as_i64().unwrap(), 0);
        assert_eq!(over["over_budget"], true);

        // tare_estimate: matches the core estimate_like band for the stored run.
        let est = json_of("tare_estimate", json!({"like": "r1"}));
        let run = store.load_run("r1").unwrap().unwrap();
        let direct =
            serde_json::to_value(tare_core::estimate::estimate_like(&run, &p, None).unwrap())
                .unwrap();
        assert_eq!(est, direct);
        // An unpriced repricing target returns an error.
        assert_eq!(
            call_tool(
                "tare_estimate",
                &json!({"like": "r1", "to": "nope"}),
                &store,
                &p
            )["isError"],
            true
        );
    }

    #[test]
    fn report_tool_equals_core_call() {
        let store = loaded_store();
        let p = pricing();
        let out = call_tool("tare_report", &json!({}), &store, &p);
        let text = out["content"][0]["text"].as_str().unwrap();
        let via_tool: Value = serde_json::from_str(text).unwrap();
        let direct =
            serde_json::to_value(attribute::build_report(&store.load_runs().unwrap(), &p)).unwrap();
        assert_eq!(via_tool, direct);
    }

    #[test]
    fn flamegraph_svg_tool_is_byte_identical_to_core() {
        let store = loaded_store();
        let p = pricing();
        let out = call_tool("tare_flamegraph_svg", &json!({"run_id": "r1"}), &store, &p);
        let text = out["content"][0]["text"].as_str().unwrap();
        let run = store.load_run("r1").unwrap().unwrap();
        let direct = svg::render_svg(&build_flamegraph(&run, &p));
        assert_eq!(text, direct);
    }

    #[test]
    fn fake_stdio_initialize_list_call_sequence() {
        let store = loaded_store();
        let p = pricing();
        // initialize
        let init = dispatch_rpc(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
            &store,
            &p,
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        // a notification (no id) yields no response
        assert!(dispatch_rpc(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            &store,
            &p
        )
        .is_none());
        // tools/list
        let list = dispatch_rpc(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            &store,
            &p,
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"tare_report") && names.contains(&"tare_flamegraph_svg"));
        // tools/call
        let call = dispatch_rpc(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": "tare_runs", "arguments": {}}}),
            &store,
            &p,
        )
        .unwrap();
        assert_eq!(call["result"]["isError"], false);
        // unknown method
        let bad = dispatch_rpc(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "no/such"}),
            &store,
            &p,
        )
        .unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }

    #[test]
    fn tool_arguments_are_strict_and_bounded() {
        let store = loaded_store();
        let p = pricing();
        let is_error = |name: &str, args: Value| {
            assert_eq!(
                call_tool(name, &args, &store, &p)["isError"],
                true,
                "{name} unexpectedly accepted {args}"
            );
        };

        is_error("tare_runs", Value::Null);
        is_error("tare_runs", json!({"typo": true}));
        is_error("tare_today", json!({"date": "2026-02-30"}));
        is_error("tare_report", json!({"today": "yes"}));
        is_error("tare_run_status", json!({"run_id": ""}));
        is_error("tare_rollup", json!({"by": "mystery"}));
        is_error("tare_anomalies", json!({"by": "mystery"}));
        is_error("tare_anomalies", json!({"window": 0}));
        is_error("tare_anomalies", json!({"threshold": u64::MAX}));
        is_error(
            "tare_trend",
            json!({"from": "2026-06-25", "to": "2026-06-24"}),
        );
        is_error("tare_gate", json!({"run_id": "r1", "max_spend_usd": -1}));
        is_error(
            "tare_budget_remaining",
            json!({"max_spend_usd": 1, "run_id": "r1", "date": "2026-06-24"}),
        );
        is_error("tare_whatif", json!({"from": "claude-sonnet-4"}));
        is_error("tare_whatif", json!({"cross_provider": true}));

        // The advertised daily-report switch is implemented and yields a normal report even when
        // there are no rows on the current UTC day.
        assert_eq!(
            call_tool("tare_report", &json!({"today": true}), &store, &p)["isError"],
            false
        );
    }

    #[test]
    fn trend_tool_honors_explicit_bounds() {
        let store = loaded_store();
        let p = pricing();
        let req = include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json");
        let resp = include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json");
        let step = ingest_step("r2", 1, Provider::Anthropic, req, resp).unwrap();
        store.record_step(&step, "2026-06-25").unwrap();

        let out = call_tool(
            "tare_trend",
            &json!({"from": "2026-06-24", "to": "2026-06-24"}),
            &store,
            &p,
        );
        assert_eq!(out["isError"], false, "{out:?}");
        let trend: Value =
            serde_json::from_str(out["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(trend["from"], "2026-06-24");
        assert_eq!(trend["to"], "2026-06-24");
        assert_eq!(trend["days"], json!(["2026-06-24"]));
    }

    #[test]
    fn rpc_rejects_malformed_requests_and_params() {
        let store = loaded_store();
        let p = pricing();
        let invalid = dispatch_rpc(&json!({"id": 1, "method": "ping"}), &store, &p).unwrap();
        assert_eq!(invalid["error"]["code"], -32600);

        let invalid_params = dispatch_rpc(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": []}),
            &store,
            &p,
        )
        .unwrap();
        assert_eq!(invalid_params["error"]["code"], -32602);

        // A valid notification still produces no response.
        assert!(dispatch_rpc(&json!({"jsonrpc": "2.0", "method": "ping"}), &store, &p,).is_none());
    }

    #[test]
    fn stdio_returns_a_parse_error_and_keeps_serving() {
        let store = loaded_store();
        let p = pricing();
        let input = b"not-json\n{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
        let mut output = Vec::new();
        serve_stream(std::io::Cursor::new(input), &mut output, &store, &p).unwrap();
        let lines: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "notification must not emit a frame");
        assert_eq!(lines[0]["error"]["code"], -32700);
        assert_eq!(lines[1]["id"], 7);
        assert_eq!(lines[1]["result"], json!({}));
    }
}
