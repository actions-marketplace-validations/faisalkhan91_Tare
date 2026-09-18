//! `tare` CLI entrypoint. Thin argv parsing over `tare_cli`.

use std::io::IsTerminal;
use std::process::exit;
use tare_cli as cli;

const USAGE: &str = "\
tare — the local-first cost profiler for AI agents (estimates; on your machine)
pinpoints which prompt component, retry loop, or uncached context is burning your money

USAGE:
  tare run    [--db PATH] [--run-id ID] -- <command...>   Profile a command
  tare report [--db PATH] [--json] [--today] [--pricing F] Show the trim-list
  tare rollup [--db PATH] [--by step|component|parent|tool|agent|effort|mcp_server|commit|author|template] [--json]  Spend by correlation label (template = prompt-template fingerprint)
  tare lineage [NAME] [--db PATH] [--json] [--pricing F]   Cost-per-run across a prompt/config lineage's versions ([[lineage]] in tare.toml); no NAME lists all
  tare unit [--db PATH] [--json] [--pricing F]             Cost per unit of work — buckets runs into [[unit]] rules (task/PR/feature) in tare.toml
  tare sessions [--db PATH] [--json] [--pricing F]        Spend grouped by agent task/session
  tare loops  [--db PATH] [--json] [--pricing F]          Retry-loop waste by offending tool/agent
  tare failures [--db PATH] [--json] [--pricing F]        Cost of errored/refused steps by tool/agent
  tare budget [--db PATH] [--json] [--pricing F]          Periodic (weekly/monthly) spend-budget status
  tare explain <run-id> [--db PATH] [--pricing F]         Plain-language cost narrative
  tare whatif [--db PATH] --swap A=B | --swap-all-to MODEL | --recommend [--cross-provider] [--json]  Reprice on another model (estimate)
  tare whatif --route MODEL [--route-when-output-below N] [--json]  Reprice under a routing policy (small-output → cheaper model; approximate)
  tare advise [--db PATH] [--json]                        Prompt-cache recommendations (estimate)
  tare advisories [--db PATH] [--json]                    Non-floor advisories: batch/reasoning/compression/cache (at-risk, not recoverable)
  tare cache [--db PATH] [--json]                         Cache-economics anti-patterns per prompt-template (single-use write, uncached-repeat, volatile prefix)
  tare reconcile --invoice FILE [--db PATH] [--json]      Reconcile a provider invoice/usage CSV vs Tare's estimate (offline)
  tare streaks [--max-daily-usd D] [--min-cache-read-pct N] [--window N] [--json]  Budget goals + daily-return streak counter
  tare today [--date D] [--oneline] [--json]              Today's estimated spend (--oneline for a tmux/statusline widget)
  tare statusline [--db PATH] [--pricing F]               Claude Code statusLine command: reads Claude Code's JSON on stdin, prints a spend line
  tare heatmap [--window N] [--json]                      Calendar heatmap of daily spend intensity
  tare punchcard [--json]                                 Day×hour spend grid — when do I burn tokens?
  tare flamediff --a RUN --b RUN [--normalize] [--svg]    Node-level cost regression between two runs (red costlier / blue cheaper)
  tare share [--run RUN] [--title T] [--experiment-to M1,M2] [--out FILE]  Single-file redacted HTML report (savings ledger + optional flamegraph + cost experiment)
  tare savings [--db PATH] [--json] [--accept K|--unaccept K]  Ranked recoverable-$ ledger; --accept <kind>:<label> marks acted-on
  tare plan [--db PATH] [--json]                          One ranked worklist: recoverable + at-risk, every row $-quantified
  tare realized [--db PATH] [--window N] [--json]         Prove accepted savings: spend before vs after the accept date
  tare estimate --like <run-id> [--to MODEL] [--json]     Pre-flight cost band from a stored shape (no spend)
  tare digest [--db PATH] [--today D] [--out FILE] [--json]  Local weekly digest (WoW, drivers, anomalies, savings) — never sent
  tare backfill [--db PATH] [--dir PATH]                  Import Claude Code JSONL transcripts (pre-install/missed sessions); idempotent
  tare init ci [--gitlab] [--pre-push] [--force]          Scaffold a starter cost-gate CI workflow (+ optional pre-push hook)
  tare quality [<run-id> [SCORE 0-100]] [--all] [--clear] [--source cli|header|ci] [--json]  Attach/read a per-run quality scalar (integer 0-100)
  tare bisect [--db PATH] [--window N] [--threshold PCT] [--cause C|--git] [--json]  First cost-regression day (or commit, with --git)
  tare pricing [--db PATH] [--pricing F]                  Pricing version/age + unpriced models
  tare pricing refresh (--from FILE | --fetch litellm|modelsdev|merged) --out FILE [--date D] [--source litellm|modelsdev]  Import/fetch a LiteLLM or models.dev price map into a dated edition (merged = both, LiteLLM wins; --fetch needs a `pricing-fetch` build)
  tare trend  [--db PATH] [--from D] [--to D] [--by total|provider|model|cause] [--json|--svg F|--anomalies [--why]]  Spend over time (--why decomposes spikes: volume/size/efficiency + bisect link)
  tare export [--db PATH] [--run ID] [--out F] [--format otel|otel-metrics|report-bundle] [--profile max_private]  speedscope (default) / OTLP traces / OTLP metrics / shareable bundle
  tare import --otlp F [--db PATH]                       Import OTLP/JSON GenAI spans (degraded)
  tare diff   <before.json> <after.json> | --run A --run B | --from/--to + --from2/--to2  Compare reports
  tare attest [--db PATH] [--run ID] [--out F] [--profile max_private] [--pricing F]  Write a recomputable cost receipt
  tare verify <receipt.json> [--pricing F]                Offline-recompute + check a receipt (not a crypto seal)
  tare gate   [--db PATH] [--max-spend USD] [--baseline F|--baseline-run ID|--baseline-ref REF] [--fail-on-regression] [--max-unpriced-tokens N]
              [--max-system-prompt-tokens N] [--max-tool-def-tokens N] [--require-cache-read-ratio PCT] [--no-retry-loops] [--max-component-growth-pct PCT --baseline-run ID] [--github-comment]  CI cost-&-shape gate
  tare daemon [--db PATH] [--once] [--interval SECS] [--window N] [--threshold PCT] [--digest-dir DIR]  Local spend-anomaly alarm + weekly digest (fire-once)
  tare up     [--db PATH] [--port N] [--otlp-port N] [--no-open]  Start everything (proxy + receiver + UI) and open the browser — the front door
  tare serve  [--db PATH] [--port N] [--otlp-port N] [--pricing F]  Run a foreground proxy + OTLP receiver (--pricing prices local models)
  tare service install|uninstall|status [--port N] [--otlp-port N] [--no-load]  Always-on capture LaunchAgent (macOS); receiver + live sessions, UI-closed
  tare capture sync [--port N] [--otlp-port N]             Reconcile the login item to [capture].mode (always_on installs it; app_only/off remove it)
  tare detect [--wire] [--otlp-port N]                     Find installed agents (Claude Code, Codex); --wire connects them additively
  tare doctor [--db PATH] [--otlp-port N]                  Setup self-check: store, pricing, receiver, capture, agent wiring
  tare connect [--settings F] [--otlp-port N] [--force]    Wire Claude Code to export OTel to Tare (additive)
  tare disconnect [--settings F]                           Remove Tare's telemetry keys (reverses connect)
  tare revert [--settings F] [--global]                    Restore settings.json byte-for-byte from the latest `tare connect` backup
  tare codex-connect [--config F] [--otlp-port N] [--force] Wire Codex CLI's [otel] log exporter to Tare (additive)
  tare codex-disconnect [--config F]                       Remove Tare's Codex [otel] exporter (reverses codex-connect)
  tare gemini-connect [--config F] [--otlp-port N] [--force]  Wire Gemini CLI's settings.json telemetry to Tare (additive)
  tare gemini-disconnect [--config F]                      Remove Tare's Gemini telemetry (reverses gemini-connect)
  tare aider-connect [--env-file F] [--base URL]           Wire Aider's .env (AIDER_OPENAI_API_BASE) to Tare's proxy (additive, project .env)
  tare aider-disconnect [--env-file F]                     Remove Tare's Aider wiring (reverses aider-connect)
  tare agent  --scrape URL --hub URL [--interval S] [--identity NAME] [--queue F] [--once]  Ship self-hosted (vLLM/llama.cpp) usage to the hub (durable retry queue)
  tare render --db PATH --run ID --out FILE [--pricing F]  flamegraph SVG
  tare demo   [--db PATH] [--svg FILE] [--speedscope FILE]  Seed/render the bundled sample run
";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    for (i, a) in args.iter().enumerate() {
        // `--flag=value` form.
        if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return Some(v);
        }
        // `--flag value` form. Reject a value that is itself a flag: a missing value must read as
        // absent, not silently swallow the next flag — otherwise e.g.
        // `gate --max-spend --fail-on-regression` binds "--fail-on-regression" as the cap.
        if a == name {
            return args
                .get(i + 1)
                .map(|s| s.as_str())
                .filter(|v| !v.starts_with("--"));
        }
    }
    None
}

/// Parse a flag's value, distinguishing ABSENT (`Ok(None)`) from PRESENT-BUT-UNPARSEABLE (`Err`).
/// Use this for safety-critical flags (e.g. `tare gate` thresholds) so a typo or `=`-form mistake
/// fails the command instead of silently dropping the check.
fn flag_parsed<T: std::str::FromStr>(args: &[String], name: &str) -> Result<Option<T>, String> {
    match flag(args, name) {
        Some(s) => s
            .parse::<T>()
            .map(Some)
            .map_err(|_| format!("{name} expects a valid value, got {s:?}")),
        // A flag typed WITH NO VALUE (last arg, or immediately followed by another `--flag`) is a
        // mistake, not "absent" — for a safety-critical threshold it must FAIL CLOSED, not silently
        // become a no-op that lets `tare gate` pass green. `flag()` returns None in
        // both the truly-absent and the present-but-valueless cases, so disambiguate here.
        None if flag_present_without_value(args, name) => {
            Err(format!("{name} requires a value (none given)"))
        }
        None => Ok(None),
    }
}

/// True when `name` appears as a bare flag with no usable value after it — either it is the last
/// token, or the next token is itself a `--flag`. Distinguishes a valueless mistake from absence.
fn flag_present_without_value(args: &[String], name: &str) -> bool {
    for (i, a) in args.iter().enumerate() {
        if a == name {
            return match args.get(i + 1) {
                None => true,
                Some(n) => n.starts_with("--"),
            };
        }
    }
    false
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// Collect the positional (non-flag) arguments, skipping the VALUE token that follows a
/// value-taking flag. Without this, a `--db /path` value is mis-collected as a positional — e.g.
/// `tare quality --db /x.db <run> <score>` would read `/x.db` as the run-id. `--flag=value`
/// and boolean flags (not in `value_flags`) never consume a following token. Long options begin
/// with `--`; a negative positional number such as a quality score remains available for validation.
fn positionals<'a>(args: &'a [String], value_flags: &[&str]) -> Vec<&'a String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for (i, a) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            // A bare value-flag (`--db`, not `--db=…`) consumes the next token as its value.
            let is_bare_value_flag = value_flags.contains(&a.as_str());
            if is_bare_value_flag {
                // Only skip the next token if it exists and isn't itself a flag (mirror `flag()`).
                if let Some(n) = args.get(i + 1) {
                    if !n.starts_with("--") {
                        skip_next = true;
                    }
                }
            }
            continue;
        }
        out.push(a);
    }
    out
}

/// The value-taking flags shared across subcommands (so `positionals` knows which flags eat the
/// next token). Boolean flags like `--json`/`--all`/`--clear` are intentionally absent.
const VALUE_FLAGS: &[&str] = &["--db", "--pricing", "--source", "--to", "--out", "--date"];

#[derive(Clone, Copy)]
struct CommandOptions {
    values: &'static [&'static str],
    booleans: &'static [&'static str],
    repeatable: &'static [&'static str],
    trailing_command: bool,
}

/// Describe the options accepted by each command. Keeping this small schema at the argv boundary
/// prevents a misspelled option from being silently ignored by the command-specific helpers.
fn command_options(command: &str) -> Option<CommandOptions> {
    let options = match command {
        "run" => CommandOptions {
            values: &["--db", "--run-id"],
            booleans: &[],
            repeatable: &[],
            trailing_command: true,
        },
        "serve" => CommandOptions {
            values: &["--db", "--port", "--otlp-port", "--pricing"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "connect" => CommandOptions {
            values: &["--settings", "--otlp-port"],
            booleans: &["--force", "--global"],
            repeatable: &[],
            trailing_command: false,
        },
        "disconnect" => CommandOptions {
            values: &["--settings"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "revert" => CommandOptions {
            values: &["--settings"],
            booleans: &["--global"],
            repeatable: &[],
            trailing_command: false,
        },
        "codex-connect" | "gemini-connect" => CommandOptions {
            values: &["--config", "--otlp-port"],
            booleans: &["--force"],
            repeatable: &[],
            trailing_command: false,
        },
        "codex-disconnect" | "gemini-disconnect" => CommandOptions {
            values: &["--config"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "aider-connect" => CommandOptions {
            values: &["--env-file", "--base"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "aider-disconnect" => CommandOptions {
            values: &["--env-file"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "agent" => CommandOptions {
            values: &["--scrape", "--hub", "--interval", "--identity", "--queue"],
            booleans: &["--once"],
            repeatable: &[],
            trailing_command: false,
        },
        "report" => CommandOptions {
            values: &["--db", "--pricing"],
            booleans: &["--json", "--today"],
            repeatable: &[],
            trailing_command: false,
        },
        "rollup" => CommandOptions {
            values: &["--db", "--pricing", "--by"],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "lineage" | "unit" | "sessions" | "loops" | "failures" | "budget" | "advise"
        | "advisories" | "cache" | "plan" | "punchcard" => CommandOptions {
            values: &["--db", "--pricing"],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "explain" => CommandOptions {
            values: &["--db", "--pricing", "--run"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "pricing" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--out",
                "--date",
                "--fetch",
                "--from",
                "--source",
            ],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "whatif" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--route",
                "--route-when-output-below",
                "--swap-all-to",
                "--swap",
            ],
            booleans: &["--json", "--recommend", "--cross-provider"],
            repeatable: &["--swap"],
            trailing_command: false,
        },
        "bisect" => CommandOptions {
            values: &["--db", "--pricing", "--window", "--threshold", "--cause"],
            booleans: &["--git", "--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "heatmap" => CommandOptions {
            values: &["--db", "--pricing", "--window"],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "share" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--run",
                "--title",
                "--date",
                "--experiment-to",
                "--out",
            ],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "flamediff" => CommandOptions {
            values: &["--db", "--pricing", "--a", "--b"],
            booleans: &["--normalize", "--svg"],
            repeatable: &[],
            trailing_command: false,
        },
        "today" => CommandOptions {
            values: &["--db", "--pricing", "--date"],
            booleans: &["--json", "--oneline"],
            repeatable: &[],
            trailing_command: false,
        },
        "statusline" => CommandOptions {
            values: &["--db", "--pricing"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "streaks" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--max-daily-usd",
                "--min-cache-read-pct",
                "--window",
            ],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "reconcile" => CommandOptions {
            values: &["--db", "--pricing", "--invoice"],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "init" => CommandOptions {
            values: &[],
            booleans: &["--force", "--gitlab", "--pre-push"],
            repeatable: &[],
            trailing_command: false,
        },
        "backfill" => CommandOptions {
            values: &["--db", "--dir"],
            booleans: &[],
            repeatable: &["--dir"],
            trailing_command: false,
        },
        "savings" => CommandOptions {
            values: &["--db", "--pricing", "--accept", "--unaccept", "--today"],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "realized" => CommandOptions {
            values: &["--db", "--pricing", "--today", "--window"],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "digest" => CommandOptions {
            values: &["--db", "--pricing", "--today", "--out"],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "estimate" => CommandOptions {
            values: &["--db", "--pricing", "--like", "--to"],
            booleans: &["--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "quality" => CommandOptions {
            values: &["--db", "--source"],
            booleans: &["--json", "--all", "--clear"],
            repeatable: &[],
            trailing_command: false,
        },
        "trend" | "history" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--by",
                "--from",
                "--to",
                "--window",
                "--threshold",
                "--svg",
            ],
            booleans: &["--anomalies", "--why", "--json"],
            repeatable: &[],
            trailing_command: false,
        },
        "diff" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--run",
                "--from",
                "--to",
                "--from2",
                "--to2",
            ],
            booleans: &["--json"],
            repeatable: &["--run"],
            trailing_command: false,
        },
        "gate" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--max-spend",
                "--baseline",
                "--baseline-run",
                "--baseline-ref",
                "--max-unpriced-tokens",
                "--max-system-prompt-tokens",
                "--max-tool-def-tokens",
                "--require-cache-read-ratio",
                "--max-component-growth-pct",
            ],
            booleans: &[
                "--fail-on-regression",
                "--no-retry-loops",
                "--github-comment",
            ],
            repeatable: &[],
            trailing_command: false,
        },
        "export" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--run",
                "--out",
                "--format",
                "--profile",
            ],
            booleans: &["--otlp", "--otlp-metrics"],
            repeatable: &[],
            trailing_command: false,
        },
        "import" => CommandOptions {
            values: &["--db", "--otlp"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "render" => CommandOptions {
            values: &["--db", "--pricing", "--run", "--out"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "attest" => CommandOptions {
            values: &["--db", "--pricing", "--run", "--out", "--profile"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "verify" => CommandOptions {
            values: &["--pricing", "--receipt"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "service" => CommandOptions {
            values: &["--db", "--port", "--otlp-port"],
            booleans: &["--no-load", "--no-activate"],
            repeatable: &[],
            trailing_command: false,
        },
        "capture" => CommandOptions {
            values: &["--db", "--port", "--otlp-port"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "daemon" => CommandOptions {
            values: &[
                "--db",
                "--pricing",
                "--interval",
                "--window",
                "--threshold",
                "--digest-dir",
            ],
            booleans: &["--once"],
            repeatable: &[],
            trailing_command: false,
        },
        "demo" => CommandOptions {
            values: &["--db", "--pricing", "--svg", "--speedscope"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "up" => CommandOptions {
            values: &["--db", "--port", "--otlp-port", "--pricing"],
            booleans: &["--no-open"],
            repeatable: &[],
            trailing_command: false,
        },
        "detect" => CommandOptions {
            values: &["--otlp-port"],
            booleans: &["--wire"],
            repeatable: &[],
            trailing_command: false,
        },
        "doctor" => CommandOptions {
            values: &["--db", "--port", "--otlp-port"],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        "__emit" => CommandOptions {
            values: &[],
            booleans: &[],
            repeatable: &[],
            trailing_command: false,
        },
        _ => return None,
    };
    Some(options)
}

fn validate_command_args(command: &str, args: &[String]) -> Result<(), String> {
    let Some(options) = command_options(command) else {
        return Ok(());
    };
    let mut seen: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            if options.trailing_command {
                return if index + 1 < args.len() {
                    Ok(())
                } else {
                    Err(format!("{command}: expected a command after `--`"))
                };
            }
            return Err(format!("{command}: unexpected `--`"));
        }
        if !arg.starts_with("--") {
            index += 1;
            continue;
        }

        let (name, inline_value) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg.as_str(), None),
        };
        let takes_value = options.values.contains(&name);
        let is_boolean = options.booleans.contains(&name);
        if !takes_value && !is_boolean {
            return Err(format!("{command}: unknown option {name:?}"));
        }
        if seen.contains(&name) && !options.repeatable.contains(&name) {
            return Err(format!(
                "{command}: option {name} may only be supplied once"
            ));
        }
        seen.push(name);

        if is_boolean {
            if inline_value.is_some() {
                return Err(format!("{command}: option {name} does not take a value"));
            }
            index += 1;
            continue;
        }

        match inline_value {
            Some(value) if value.trim().is_empty() => {
                return Err(format!(
                    "{command}: option {name} requires a non-empty value"
                ));
            }
            Some(_) => index += 1,
            None => match args.get(index + 1) {
                Some(value) if !value.starts_with("--") && !value.trim().is_empty() => index += 2,
                _ => return Err(format!("{command}: option {name} requires a value")),
            },
        }
    }
    if options.trailing_command {
        return Err(format!("{command}: expected `-- <command...>`"));
    }
    validate_command_shape(command, args, options)
}

fn option_present(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| {
        arg == name
            || arg
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.starts_with('='))
    })
}

fn validate_command_shape(
    command: &str,
    args: &[String],
    options: CommandOptions,
) -> Result<(), String> {
    let positional = positionals(args, options.values);
    let reject_extra = |allowed: usize| {
        positional.get(allowed).map_or(Ok(()), |value| {
            Err(format!(
                "{command}: unexpected positional argument {value:?}"
            ))
        })
    };

    match command {
        "lineage" => reject_extra(1),
        "explain" => {
            reject_extra(1)?;
            if option_present(args, "--run") && !positional.is_empty() {
                return Err("explain: use either <run-id> or --run, not both".into());
            }
            Ok(())
        }
        "pricing" => {
            reject_extra(1)?;
            let refresh = positional
                .first()
                .is_some_and(|value| value.as_str() == "refresh");
            if !positional.is_empty() && !refresh {
                return Err("pricing: the only subcommand is `refresh`".into());
            }
            if refresh && args.first().map(String::as_str) != Some("refresh") {
                return Err("pricing: `refresh` must appear immediately after `pricing`".into());
            }
            let refresh_options = ["--out", "--date", "--fetch", "--from", "--source"];
            let status_options = ["--db", "--pricing"];
            let invalid = if refresh {
                status_options
                    .iter()
                    .find(|name| option_present(args, name))
            } else {
                refresh_options
                    .iter()
                    .find(|name| option_present(args, name))
            };
            if let Some(name) = invalid {
                return Err(format!(
                    "pricing: option {name} is not valid in this command mode"
                ));
            }
            if option_present(args, "--fetch") && option_present(args, "--source") {
                return Err("pricing refresh: --source is only used with --from".into());
            }
            Ok(())
        }
        "init" => {
            if args.first().map(String::as_str) != Some("ci") {
                return Err("usage: tare init ci [--gitlab] [--pre-push] [--force]".into());
            }
            reject_extra(1)
        }
        "service" => {
            reject_extra(1)?;
            if !positional.is_empty()
                && !matches!(positional[0].as_str(), "install" | "uninstall" | "status")
            {
                return Err(format!(
                    "service: unknown subcommand {:?} (use install | uninstall | status)",
                    positional[0]
                ));
            }
            if !positional.is_empty() && args.first() != Some(positional[0]) {
                return Err("service: the subcommand must come before its options".into());
            }
            let subcommand = positional
                .first()
                .map(|value| value.as_str())
                .unwrap_or("status");
            if subcommand != "install" {
                let ignored = [
                    "--db",
                    "--port",
                    "--otlp-port",
                    "--no-load",
                    "--no-activate",
                ]
                .iter()
                .find(|name| option_present(args, name));
                if let Some(name) = ignored {
                    return Err(format!("service {subcommand}: option {name} is not valid"));
                }
            }
            Ok(())
        }
        "capture" => {
            reject_extra(1)?;
            if let Some(subcommand) = positional.first() {
                if subcommand.as_str() != "sync" {
                    return Err(format!(
                        "capture: unknown subcommand {subcommand:?} (use sync)"
                    ));
                }
                if args.first() != Some(subcommand) {
                    return Err("capture: the subcommand must come before its options".into());
                }
            }
            Ok(())
        }
        "estimate" => {
            reject_extra(1)?;
            if option_present(args, "--like") && !positional.is_empty() {
                return Err("estimate: use either a positional run id or --like, not both".into());
            }
            Ok(())
        }
        "quality" => {
            reject_extra(2)?;
            if has(args, "--all") {
                if !positional.is_empty()
                    || has(args, "--clear")
                    || option_present(args, "--source")
                {
                    return Err(
                        "quality: --all cannot be combined with a run, --clear, or --source".into(),
                    );
                }
                return Ok(());
            }
            if has(args, "--clear") && positional.len() > 1 {
                return Err("quality: --clear cannot be combined with a score".into());
            }
            if option_present(args, "--source") && positional.len() < 2 {
                return Err("quality: --source is only valid when setting a score".into());
            }
            Ok(())
        }
        "verify" => {
            reject_extra(1)?;
            if option_present(args, "--receipt") && !positional.is_empty() {
                return Err("verify: use either <receipt.json> or --receipt, not both".into());
            }
            Ok(())
        }
        "diff" => {
            let has_runs = option_present(args, "--run");
            let window_flags = ["--from", "--to", "--from2", "--to2"];
            let window_count = window_flags
                .iter()
                .filter(|name| option_present(args, name))
                .count();
            if has_runs {
                if window_count > 0 || !positional.is_empty() {
                    return Err("diff: do not mix --run with date windows or report files".into());
                }
                if flags_all(args, "--run").len() != 2 {
                    return Err("diff: --run must be supplied exactly twice".into());
                }
            } else if window_count > 0 {
                if window_count != window_flags.len() || !positional.is_empty() {
                    return Err(
                        "diff: a date comparison requires --from, --to, --from2, and --to2 only"
                            .into(),
                    );
                }
            } else if positional.len() != 2 {
                return Err("diff: expected exactly two report files".into());
            }
            Ok(())
        }
        "whatif" => {
            reject_extra(0)?;
            let route = option_present(args, "--route");
            let recommend = has(args, "--recommend");
            let swap = option_present(args, "--swap") || option_present(args, "--swap-all-to");
            if usize::from(route) + usize::from(recommend) + usize::from(swap) > 1 {
                return Err("whatif: choose one of routing, recommendation, or swap mode".into());
            }
            if option_present(args, "--route-when-output-below") && !route {
                return Err("whatif: --route-when-output-below requires --route".into());
            }
            if has(args, "--cross-provider") && !recommend {
                return Err("whatif: --cross-provider requires --recommend".into());
            }
            Ok(())
        }
        "trend" | "history" => {
            reject_extra(0)?;
            let anomalies = has(args, "--anomalies");
            if has(args, "--why") && !anomalies {
                return Err("trend: --why requires --anomalies".into());
            }
            if !anomalies
                && (option_present(args, "--window") || option_present(args, "--threshold"))
            {
                return Err("trend: --window and --threshold require --anomalies".into());
            }
            if option_present(args, "--svg") && (anomalies || has(args, "--json")) {
                return Err("trend: --svg cannot be combined with --anomalies or --json".into());
            }
            Ok(())
        }
        "bisect" => {
            reject_extra(0)?;
            if has(args, "--git") && option_present(args, "--cause") {
                return Err("bisect: --cause is not valid with --git".into());
            }
            Ok(())
        }
        "today" => {
            reject_extra(0)?;
            if has(args, "--json") && has(args, "--oneline") {
                return Err("today: choose either --json or --oneline".into());
            }
            Ok(())
        }
        "savings" => {
            reject_extra(0)?;
            let accept = option_present(args, "--accept");
            let unaccept = option_present(args, "--unaccept");
            if accept && unaccept {
                return Err("savings: choose either --accept or --unaccept".into());
            }
            if option_present(args, "--today") && !accept {
                return Err("savings: --today is only valid with --accept".into());
            }
            Ok(())
        }
        "daemon" => {
            reject_extra(0)?;
            if has(args, "--once") && option_present(args, "--interval") {
                return Err("daemon: --interval is not used with --once".into());
            }
            Ok(())
        }
        "gate" => {
            reject_extra(0)?;
            let baselines = ["--baseline", "--baseline-run", "--baseline-ref"]
                .iter()
                .filter(|name| option_present(args, name))
                .count();
            if baselines > 1 {
                return Err(
                    "gate: choose only one of --baseline, --baseline-run, or --baseline-ref".into(),
                );
            }
            Ok(())
        }
        _ => reject_extra(0),
    }
}

fn db_path(args: &[String]) -> String {
    flag(args, "--db")
        .map(|s| s.to_string())
        .or_else(|| std::env::var("TARE_DB").ok())
        .unwrap_or_else(|| "tare.db".to_string())
}

/// Warn (once, on stderr) when an EXPLICITLY-requested store (`--db` flag or `TARE_DB`) does not
/// exist yet — a read/report against it would otherwise auto-create an empty store and print a
/// misleading `$0` as if that were real. We warn rather than hard-error so a
/// first-run capture that legitimately creates the store is unaffected; safety-critical `gate`
/// additionally FAILS CLOSED on an empty store (see `cli::gate`). No-op for the implicit default.
fn warn_if_explicit_db_missing(args: &[String]) {
    let explicit = flag(args, "--db")
        .map(|s| s.to_string())
        .or_else(|| std::env::var("TARE_DB").ok());
    if let Some(path) = explicit {
        if !std::path::Path::new(&path).exists() {
            eprintln!(
                "tare: warning — store {path:?} does not exist; reporting on an empty store ($0). \
                 Check the --db path (or that capture has run)."
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };

    let option_args = if cmd == "run" {
        &rest[..rest
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(rest.len())]
    } else {
        rest
    };
    if option_args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{USAGE}");
        return;
    }
    if let Err(error) = validate_command_args(cmd, rest) {
        eprintln!("tare: {error}");
        exit(2);
    }

    let result: Result<(), String> = match cmd {
        "run" => cmd_run(rest),
        "serve" => cmd_serve(rest),
        "connect" => cmd_connect(rest),
        "disconnect" => {
            let path = flag(rest, "--settings")
                .unwrap_or(".claude/settings.local.json")
                .to_string();
            cli::connect::disconnect_command(&path)
        }
        "revert" => claude_settings_path(rest).and_then(|path| cli::connect::revert_command(&path)),
        "codex-connect" => cmd_codex_connect(rest),
        "codex-disconnect" => {
            let path = flag(rest, "--config")
                .map(str::to_string)
                .unwrap_or_else(cli::codex_connect::default_codex_config);
            cli::codex_connect::codex_disconnect_command(&path)
        }
        "gemini-connect" => cmd_gemini_connect(rest),
        "gemini-disconnect" => {
            let path = flag(rest, "--config")
                .map(str::to_string)
                .unwrap_or_else(cli::gemini_connect::default_gemini_config);
            cli::gemini_connect::gemini_disconnect_command(&path)
        }
        "aider-connect" => {
            // Manage a reversible AIDER_OPENAI_API_BASE block in the project.env.
            let path = flag(rest, "--env-file")
                .map(str::to_string)
                .unwrap_or_else(cli::aider_connect::default_aider_env);
            let base = flag(rest, "--base")
                .map(str::to_string)
                .unwrap_or_else(cli::aider_connect::default_aider_base);
            cli::aider_connect::aider_connect_command(&path, &base)
        }
        "aider-disconnect" => {
            let path = flag(rest, "--env-file")
                .map(str::to_string)
                .unwrap_or_else(cli::aider_connect::default_aider_env);
            cli::aider_connect::aider_disconnect_command(&path)
        }
        "agent" => cmd_agent(rest),
        "report" => cmd_report(rest),
        "rollup" => cmd_rollup(rest),
        "lineage" => cmd_lineage(rest),
        "unit" => cmd_unit(rest),
        "sessions" => cmd_sessions(rest),
        "loops" => cmd_loops(rest),
        "failures" => cmd_failures(rest),
        "budget" => cmd_budget(rest),
        "explain" => {
            let pricing = cli::load_pricing(flag(rest, "--pricing"));
            // Skip flag VALUES (e.g. a `--db` path) when hunting the positional run-id, so
            // `tare explain --db /x.db` doesn't read `/x.db` as the run-id.
            let run = flag(rest, "--run")
                .or_else(|| positionals(rest, VALUE_FLAGS).first().map(|s| s.as_str()));
            match (pricing, run) {
                (Ok(p), Some(r)) => cli::explain_for(&db_path(rest), r, &p).map(|s| print!("{s}")),
                (Ok(_), None) => Err("explain: expected <run-id> (or --run ID)".into()),
                (Err(e), _) => Err(e),
            }
        }
        "pricing" => cmd_pricing(rest),
        "whatif" => cmd_whatif(rest),
        "bisect" => {
            let pricing = cli::load_pricing(flag(rest, "--pricing"));
            pricing.and_then(|p| {
                let window = flag_parsed(rest, "--window")?.unwrap_or(7);
                let threshold = flag_parsed(rest, "--threshold")?.unwrap_or(50);
                if window == 0 {
                    return Err("bisect: --window must be at least 1".into());
                }
                if threshold < 0 {
                    return Err("bisect: --threshold must not be negative".into());
                }
                // --git: bisect over git-commit history instead of the daily series.
                let reg = if has(rest, "--git") {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                    cli::bisect_git_for(&db_path(rest), &cwd, window, threshold, &p)?
                } else {
                    cli::bisect_for(&db_path(rest), flag(rest, "--cause"), window, threshold, &p)?
                };
                let unit = if has(rest, "--git") { "commit" } else { "day" };
                match reg {
                    Some(r) if has(rest, "--json") => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?
                        )
                    }
                    Some(r) => println!(
                        "regression entered {unit} {} : {} vs trailing median {} (+{}%)",
                        r.date,
                        tare_core::money::MicroUsd(r.value_micros).to_dollar_string(),
                        tare_core::money::MicroUsd(r.baseline_micros).to_dollar_string(),
                        r.pct_over
                    ),
                    None => println!("no cost regression found"),
                }
                Ok(())
            })
        }
        "advise" => {
            let pricing = cli::load_pricing(flag(rest, "--pricing"));
            pricing.and_then(|p| {
                let advice = cli::advise_for(&db_path(rest), &p)?;
                if has(rest, "--json") {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&advice).map_err(|e| e.to_string())?
                    );
                } else {
                    print!("{}", cli::render_advise_text(&advice));
                }
                Ok(())
            })
        }
        "cache" => {
            let pricing = cli::load_pricing(flag(rest, "--pricing"));
            pricing.and_then(|p| {
                let report = cli::cache_health_for(&db_path(rest), &p)?;
                if has(rest, "--json") {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                    );
                } else {
                    print!("{}", cli::render_cache_health_text(&report));
                }
                Ok(())
            })
        }
        "advisories" => {
            let pricing = cli::load_pricing(flag(rest, "--pricing"));
            pricing.and_then(|p| {
                let adv = cli::advisories_for(&db_path(rest), &p)?;
                if has(rest, "--json") {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&adv).map_err(|e| e.to_string())?
                    );
                } else {
                    print!("{}", cli::render_advisories_text(&adv));
                }
                Ok(())
            })
        }
        "heatmap" => cmd_heatmap(rest),
        "punchcard" => cmd_punchcard(rest),
        "share" => cmd_share(rest),
        "flamediff" => cmd_flamediff(rest),
        "today" => cmd_today(rest),
        "statusline" => cmd_statusline(rest),
        "streaks" => cmd_streaks(rest),
        "reconcile" => match flag(rest, "--invoice") {
            None => Err(
                "tare reconcile needs --invoice <file.csv> (a provider invoice/usage export)"
                    .to_string(),
            ),
            Some(path) => std::fs::read_to_string(path)
                .map_err(|e| format!("read {path}: {e}"))
                .and_then(|csv| {
                    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
                    let report = cli::invoice_reconcile_for(&db_path(rest), &pricing, &csv)?;
                    if has(rest, "--json") {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                        );
                    } else {
                        print!("{}", cli::render_invoice_reconcile_text(&report));
                    }
                    Ok(())
                }),
        },
        "init" => cmd_init(rest),
        "backfill" => cmd_backfill(rest),
        "savings" => cmd_savings(rest),
        "plan" => cmd_plan(rest),
        "realized" => cmd_realized(rest),
        "digest" => cmd_digest(rest),
        "estimate" => cmd_estimate(rest),
        "quality" => cmd_quality(rest),
        "trend" | "history" => cmd_trend(rest),
        "diff" => cmd_diff(rest),
        "gate" => cmd_gate(rest),
        "export" => cmd_export(rest),
        "import" => {
            let path =
                flag(rest, "--otlp").ok_or_else(|| "import: expected --otlp <file>".to_string());
            path.and_then(|p| {
                let n = cli::import_otlp(&db_path(rest), p)?;
                eprintln!("tare: imported {n} step(s) from {p}");
                Ok(())
            })
        }
        "render" => {
            let pricing = cli::load_pricing(flag(rest, "--pricing"));
            pricing.and_then(|p| {
                cli::render_svg_for(
                    &db_path(rest),
                    flag(rest, "--run").unwrap_or(""),
                    flag(rest, "--out").unwrap_or("flamegraph.svg"),
                    &p,
                )
            })
        }
        "attest" => cmd_attest(rest),
        "verify" => {
            let pricing = cli::load_pricing(flag(rest, "--pricing"));
            let positional = positionals(rest, &["--pricing", "--receipt"]);
            let path =
                flag(rest, "--receipt").or_else(|| positional.first().map(|value| value.as_str()));
            match (pricing, path) {
                (Ok(p), Some(f)) => cli::verify_receipt(f, &p).map(|s| print!("{s}")),
                (Ok(_), None) => Err("verify: expected <receipt.json>".into()),
                (Err(e), _) => Err(e),
            }
        }
        "service" => cmd_service(rest),
        "capture" => cmd_capture(rest),
        "daemon" => cmd_daemon(rest),
        "demo" => cmd_demo(rest),
        "up" => run_up(rest),
        "detect" => cmd_detect(rest),
        "doctor" => cmd_doctor(rest),
        "__emit" => cli::emit_selftest(),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            return;
        }
        "-V" | "--version" | "version" => {
            println!("tare {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        // Bare `tare` in an interactive terminal is the front door: start everything + open the UI.
        // Non-interactive (pipes, CI, `tare | …`) keeps the usage text so nothing surprising runs.
        "" if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() => run_up(rest),
        "" => {
            print!("{USAGE}");
            return;
        }
        other => {
            eprintln!("tare: unknown command `{other}`\n\n{USAGE}");
            exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("tare: {e}");
        exit(1);
    }
}

fn resolved_port(
    args: &[String],
    name: &str,
    configured: Option<u16>,
    default: u16,
) -> Result<u16, String> {
    let port = flag_parsed::<u16>(args, name)?
        .or(configured)
        .unwrap_or(default);
    if port == 0 {
        Err(format!("{name} must be between 1 and 65535"))
    } else {
        Ok(port)
    }
}

fn required_home(context: &str) -> Result<String, String> {
    cli::home_dir().ok_or_else(|| {
        format!("{context}: cannot resolve the home directory (HOME/USERPROFILE is unset)")
    })
}

fn claude_settings_path(args: &[String]) -> Result<String, String> {
    if let Some(path) = flag(args, "--settings") {
        if path.trim().is_empty() {
            return Err("--settings requires a non-empty path".into());
        }
        return Ok(path.to_string());
    }
    if has(args, "--global") {
        return Ok(format!(
            "{}/.claude/settings.json",
            required_home("Claude settings")?
        ));
    }
    Ok(".claude/settings.local.json".to_string())
}

fn service_log_path() -> Result<String, String> {
    Ok(format!(
        "{}/.tare/serve.log",
        required_home("capture service")?
    ))
}

fn cmd_serve(rest: &[String]) -> Result<(), String> {
    let cfg = cli::load_config_strict()?;
    let port = resolved_port(rest, "--port", cfg.proxy.port, 8788)?;
    let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
    let pricing = flag(rest, "--pricing")
        .map(str::to_string)
        .or(cfg.proxy.pricing);
    cli::serve_command(&db_path(rest), port, otlp_port, pricing.as_deref())
}

fn cmd_connect(rest: &[String]) -> Result<(), String> {
    let cfg = cli::load_config_strict()?;
    let path = claude_settings_path(rest)?;
    let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
    cli::connect::connect_command(
        &path,
        &format!("http://127.0.0.1:{otlp_port}"),
        has(rest, "--force"),
    )
}

fn cmd_codex_connect(rest: &[String]) -> Result<(), String> {
    let cfg = cli::load_config_strict()?;
    let path = flag(rest, "--config")
        .map(str::to_string)
        .unwrap_or_else(cli::codex_connect::default_codex_config);
    if path.trim().is_empty() {
        return Err("--config requires a non-empty path".into());
    }
    let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
    cli::codex_connect::codex_connect_command(
        &path,
        &format!("http://127.0.0.1:{otlp_port}/v1/logs"),
        has(rest, "--force"),
    )
}

fn cmd_gemini_connect(rest: &[String]) -> Result<(), String> {
    let cfg = cli::load_config_strict()?;
    let path = flag(rest, "--config")
        .map(str::to_string)
        .unwrap_or_else(cli::gemini_connect::default_gemini_config);
    if path.trim().is_empty() {
        return Err("--config requires a non-empty path".into());
    }
    let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
    cli::gemini_connect::gemini_connect_command(
        &path,
        &format!("http://127.0.0.1:{otlp_port}"),
        has(rest, "--force"),
    )
}

fn cmd_agent(rest: &[String]) -> Result<(), String> {
    let scrape = flag(rest, "--scrape")
        .filter(|value| !value.trim().is_empty())
        .ok_or("agent: requires --scrape <metrics-url>")?;
    let hub = flag(rest, "--hub")
        .filter(|value| !value.trim().is_empty())
        .ok_or("agent: requires --hub <receiver-url>")?;
    let interval = flag_parsed::<u64>(rest, "--interval")?.unwrap_or(15);
    if interval == 0 {
        return Err("agent: --interval must be at least 1 second".into());
    }
    let identity = flag(rest, "--identity")
        .map(str::to_string)
        .unwrap_or_else(|| std::env::var("HOSTNAME").unwrap_or_else(|_| "homelab".into()));
    if identity.trim().is_empty() {
        return Err("agent: --identity must not be empty".into());
    }
    let queue = match flag(rest, "--queue") {
        Some(path) if !path.trim().is_empty() => path.to_string(),
        Some(_) => return Err("agent: --queue must not be empty".into()),
        None => format!(
            "{}/.tare/agent-queue.jsonl",
            required_home("homelab agent queue")?
        ),
    };
    cli::agent::agent_command(
        scrape,
        hub,
        interval,
        &identity,
        &queue,
        has(rest, "--once"),
    )
}

fn max_private_profile(rest: &[String]) -> Result<bool, String> {
    match flag(rest, "--profile") {
        None if flag_present_without_value(rest, "--profile") => {
            Err("--profile requires a value".into())
        }
        None => Ok(false),
        Some("max_private") => Ok(true),
        Some(value) => Err(format!(
            "unknown --profile {value:?} (the only explicit profile is max_private)"
        )),
    }
}

fn cmd_export(rest: &[String]) -> Result<(), String> {
    if flag_present_without_value(rest, "--format") {
        return Err("export: --format requires a value".into());
    }
    let legacy_otel = has(rest, "--otlp");
    let legacy_metrics = has(rest, "--otlp-metrics");
    if legacy_otel && legacy_metrics {
        return Err("export: choose only one of --otlp and --otlp-metrics".into());
    }
    if flag(rest, "--format").is_some() && (legacy_otel || legacy_metrics) {
        return Err("export: do not combine --format with legacy --otlp flags".into());
    }
    let format = flag(rest, "--format").unwrap_or(if legacy_metrics {
        "otel-metrics"
    } else if legacy_otel {
        "otel"
    } else {
        "speedscope"
    });
    if !matches!(
        format,
        "speedscope" | "otel" | "otel-metrics" | "report-bundle"
    ) {
        return Err(format!(
            "export: unknown format {format:?} (expected speedscope|otel|otel-metrics|report-bundle)"
        ));
    }
    let max_private = max_private_profile(rest)?;
    if max_private && format != "report-bundle" {
        return Err("export: --profile is only valid with --format report-bundle".into());
    }
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    match format {
        "report-bundle" => {
            let json =
                cli::report_bundle(&db_path(rest), flag(rest, "--run"), max_private, &pricing)?;
            let out = flag(rest, "--out").unwrap_or("tare-bundle.json");
            std::fs::write(out, json).map_err(|e| format!("write {out}: {e}"))?;
            eprintln!("tare: wrote {out}");
            Ok(())
        }
        "otel-metrics" => cli::export_otel_metrics_for(
            &db_path(rest),
            flag(rest, "--run").unwrap_or(""),
            flag(rest, "--out").unwrap_or("metrics.otlp.json"),
            &pricing,
        ),
        "otel" => cli::export_otel_for(
            &db_path(rest),
            flag(rest, "--run").unwrap_or(""),
            flag(rest, "--out").unwrap_or("trace.otlp.json"),
            &pricing,
        ),
        "speedscope" => cli::export_speedscope(
            &db_path(rest),
            flag(rest, "--run").unwrap_or(""),
            flag(rest, "--out").unwrap_or("speedscope.json"),
            &pricing,
        ),
        _ => unreachable!("format validated above"),
    }
}

fn cmd_attest(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let max_private = max_private_profile(rest)?;
    let json = cli::attest_receipt(&db_path(rest), flag(rest, "--run"), max_private, &pricing)?;
    let out = flag(rest, "--out").unwrap_or("tare-receipt.json");
    std::fs::write(out, json).map_err(|e| format!("write {out}: {e}"))?;
    eprintln!("tare: wrote {out}");
    Ok(())
}

fn cmd_detect(rest: &[String]) -> Result<(), String> {
    let cfg = if has(rest, "--wire") {
        cli::load_config_strict()?
    } else {
        cli::load_config()
    };
    let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
    cli::detect_command(otlp_port, has(rest, "--wire"))
}

fn cmd_doctor(rest: &[String]) -> Result<(), String> {
    let cfg = cli::load_config();
    let http_port = resolved_port(rest, "--port", cfg.proxy.port, 8788)?;
    let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
    let code = cli::doctor_command(&db_path(rest), http_port, otlp_port);
    if code != 0 {
        exit(code);
    }
    Ok(())
}

fn cmd_run(rest: &[String]) -> Result<(), String> {
    let split = rest.iter().position(|a| a == "--");
    let Some(idx) = split else {
        return Err("run: expected `-- <command...>`".into());
    };
    let child: Vec<String> = rest[idx + 1..].to_vec();
    let opts = &rest[..idx];
    let run_id = flag(opts, "--run-id")
        .map(|s| s.to_string())
        .unwrap_or_else(default_run_id);
    if run_id.trim().is_empty() || run_id.len() > 512 || run_id.chars().any(char::is_control) {
        return Err("run: --run-id must be 1-512 bytes with no control characters".into());
    }
    let code = cli::run_command(&child, &db_path(opts), &run_id)?;
    eprintln!("tare: captured run `{run_id}` -> {}", db_path(opts));
    if code != 0 {
        exit(code);
    }
    Ok(())
}

fn cmd_report(rest: &[String]) -> Result<(), String> {
    warn_if_explicit_db_missing(rest);
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let report = cli::report_for(&db_path(rest), has(rest, "--today"), &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_report_text(&report));
    }
    Ok(())
}

fn cmd_rollup(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    // A typo'd dimension (`--by bogus`) must ERROR, not silently fall back to `step` and hand the
    // user wrong-lens output they think succeeded. Absent `--by` still defaults to step.
    let dim = match flag(rest, "--by") {
        None => tare_core::rollup::RollupDim::Step,
        Some(s) => tare_core::rollup::RollupDim::parse(s).ok_or_else(|| {
            format!(
                "rollup: unknown --by {s:?} (expected one of: step|component|parent|tool|agent|\
                 effort|mcp_server|commit|author|template)"
            )
        })?,
    };
    let rep = cli::rollup_for(&db_path(rest), dim, &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_rollup_text(&rep));
    }
    Ok(())
}

/// `tare lineage [<name>]`: cost-per-run across a prompt/config lineage's versions. With
/// no name, lists every configured lineage. Reads `[[lineage]]` from tare.toml.
fn cmd_lineage(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    // The lineage name is the first positional — skip flags and the VALUES of value-taking flags
    // (`--db PATH`, `--pricing PATH`) so `tare lineage --pricing p.toml checkout` picks "checkout".
    let value_flags = ["--db", "--pricing"];
    let name = rest.iter().enumerate().find_map(|(i, a)| {
        if a.starts_with("--") {
            return None;
        }
        if i > 0 && value_flags.contains(&rest[i - 1].as_str()) {
            return None;
        }
        Some(a.clone())
    });
    match name {
        Some(name) => {
            let rep = cli::lineage_for(&db_path(rest), &pricing, &name)?;
            if has(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
                );
            } else {
                print!("{}", cli::render_lineage_text(&rep));
            }
        }
        None => {
            let reps = cli::lineages_all(&db_path(rest), &pricing)?;
            if has(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reps).map_err(|e| e.to_string())?
                );
            } else if reps.is_empty() {
                println!("no lineages configured — add [[lineage]] entries to tare.toml");
            } else {
                for rep in &reps {
                    println!("{}", cli::render_lineage_text(rep));
                }
            }
        }
    }
    Ok(())
}

/// `tare unit`: cost per unit of work — buckets captured runs into the configured `[[unit]]` rules.
/// Reads `[[unit]]` from tare.toml.
fn cmd_unit(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let rep = cli::units_for(&db_path(rest), &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_units_text(&rep));
    }
    Ok(())
}

fn cmd_sessions(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let rep = cli::sessions_for(&db_path(rest), &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_sessions_text(&rep));
    }
    Ok(())
}

/// `tare service install|uninstall|status`: the always-on capture LaunchAgent (macOS), so the
/// OTLP receiver + live-session tracking run continuously with the UI closed.
fn cmd_service(rest: &[String]) -> Result<(), String> {
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "install" => {
            let cfg = cli::load_config_strict()?;
            let exe = std::env::current_exe()
                .map_err(|e| format!("current exe: {e}"))?
                .to_string_lossy()
                .to_string();
            let db = db_path(rest);
            let port = resolved_port(rest, "--port", cfg.proxy.port, 8788)?;
            let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
            let log = service_log_path()?;
            // --no-load / --no-activate just writes the service definition (e.g. for inspection);
            // the default also bootstraps (macOS) / enables (Linux) / registers (Windows) it.
            let activate = !(has(rest, "--no-load") || has(rest, "--no-activate"));
            println!(
                "{}",
                cli::service::install_current(&exe, &db, port, otlp_port, &log, activate)?
            );
            Ok(())
        }
        "uninstall" => {
            println!("{}", cli::service::uninstall_current(true)?);
            Ok(())
        }
        "status" => {
            println!(
                "Tare capture service: {}",
                if cli::service::is_installed_current() {
                    "installed"
                } else {
                    "not installed"
                }
            );
            Ok(())
        }
        other => Err(format!(
            "service: unknown subcommand {other:?} (use install | uninstall | status)"
        )),
    }
}

/// `tare capture sync`: reconcile the OS login item to `[capture].mode`. `always_on`
/// installs+activates it; `app_only`/`off` remove it if present. The desktop app shells out to this on
/// startup and after a Settings mode change, so the toggle actually drives the resident daemon.
fn cmd_capture(rest: &[String]) -> Result<(), String> {
    use tare_core::config::ServiceAction;
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("sync");
    match sub {
        "sync" => {
            // This operation can install or REMOVE a resident service, so malformed config must
            // stop the reconciliation rather than silently falling back to `app_only`.
            let cfg = cli::load_config_strict()?;
            let mode = cfg.capture.mode;
            let installed = cli::service::is_installed_current();
            match mode.service_action(installed) {
                ServiceAction::Install => {
                    let exe = std::env::current_exe()
                        .map_err(|e| format!("current exe: {e}"))?
                        .to_string_lossy()
                        .to_string();
                    let db = db_path(rest);
                    let port = resolved_port(rest, "--port", cfg.proxy.port, 8788)?;
                    let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
                    let log = service_log_path()?;
                    println!(
                        "capture sync ({}): {}",
                        mode.as_str(),
                        cli::service::install_current(&exe, &db, port, otlp_port, &log, true)?
                    );
                }
                ServiceAction::Uninstall => {
                    println!(
                        "capture sync ({}): {}",
                        mode.as_str(),
                        cli::service::uninstall_current(true)?
                    );
                }
                ServiceAction::Leave => {
                    println!(
                        "capture sync ({}): login item already {} — no change",
                        mode.as_str(),
                        if installed { "installed" } else { "absent" }
                    );
                }
            }
            Ok(())
        }
        other => Err(format!("capture: unknown subcommand {other:?} (use sync)")),
    }
}

fn cmd_budget(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let b = cli::period_budget_for(&db_path(rest), &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&b).map_err(|e| e.to_string())?
        );
    } else if b.cap_micros <= 0 {
        println!("No periodic budget configured (set [budget] period + period_max_spend_usd in tare.toml).");
    } else {
        println!(
            "This {}'s budget: {} of {} ({}%) — {}",
            b.period,
            tare_core::money::MicroUsd(b.spent_micros).to_dollar_string(),
            tare_core::money::MicroUsd(b.cap_micros).to_dollar_string(),
            b.pct,
            b.status
        );
    }
    Ok(())
}

fn cmd_loops(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let rep = cli::loop_waste_for(&db_path(rest), &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_loops_text(&rep));
    }
    Ok(())
}

fn cmd_failures(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let rep = cli::failure_waste_for(&db_path(rest), &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_failures_text(&rep));
    }
    Ok(())
}

/// `tare init ci [--gitlab] [--pre-push]`: scaffold a starter cost-gate CI workflow
/// (GitHub Actions by default, GitLab with --gitlab) and optionally a git pre-push hook. Static
/// templates; writes files (won't clobber an existing one without --force).
fn cmd_init(rest: &[String]) -> Result<(), String> {
    if rest.first().map(String::as_str) != Some("ci") {
        return Err("usage: tare init ci [--gitlab] [--pre-push] [--force]".into());
    }
    let force = has(rest, "--force");
    let write = |path: &str, body: String| -> Result<(), String> {
        let p = std::path::Path::new(path);
        if p.exists() && !force {
            println!("  skip {path} (exists; --force to overwrite)");
            return Ok(());
        }
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        }
        std::fs::write(p, body).map_err(|e| format!("write {path}: {e}"))?;
        println!("  wrote {path}");
        Ok(())
    };
    if has(rest, "--gitlab") {
        write(".gitlab-ci.tare.yml", cli::render_gitlab_ci())?;
    } else {
        write(".github/workflows/tare.yml", cli::render_github_workflow())?;
    }
    if has(rest, "--pre-push") {
        let path = ".git/hooks/pre-push";
        write(path, cli::render_prepush_hook())?;
        #[cfg(unix)]
        if std::path::Path::new(path).exists() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("make {path} executable: {e}"))?;
        }
    }
    println!("tare init: cost-gate scaffold written (review before committing).");
    Ok(())
}

/// `tare pricing` — status, or refresh a local/fetched price map into a dated Tare edition.
fn cmd_pricing(rest: &[String]) -> Result<(), String> {
    if rest.first().map(String::as_str) == Some("refresh") {
        let out = flag(rest, "--out").ok_or("pricing refresh: --out FILE is required")?;
        let date = flag(rest, "--date")
            .map(str::to_string)
            .unwrap_or_else(cli::today_local);
        // `--fetch litellm|modelsdev` auto-downloads the well-known URL; otherwise
        // `--from FILE` imports a local price map. --fetch validates its source even without the
        // TLS feature, then reports how to enable it — never a silent no-op.
        if flag(rest, "--fetch").is_some() && flag(rest, "--from").is_some() {
            return Err("pricing refresh: choose exactly one of --fetch or --from".into());
        }
        if let Some(source) = flag(rest, "--fetch") {
            // `merged` fetches both and prefers LiteLLM; else a single source's URL.
            if source != "merged" {
                cli::pricing_source_url(source)?; // validate source up front (litellm|modelsdev)
            }
            #[cfg(feature = "pricing-fetch")]
            {
                let n = if source == "merged" {
                    cli::pricing_refresh_merged(&date, out)?
                } else {
                    cli::pricing_refresh_from_url(source, &date, out)?
                };
                println!(
                    "fetched + wrote {n} model(s) -> {out} (effective {date}, source {source})"
                );
                return Ok(());
            }
            #[cfg(not(feature = "pricing-fetch"))]
            {
                return Err(format!(
                    "pricing refresh --fetch needs a TLS build: rebuild with `cargo build --features pricing-fetch` \
                     (the offline gate excludes it), or use --from FILE (source {source})"
                ));
            }
        }
        let from = flag(rest, "--from")
            .ok_or("pricing refresh: --from FILE (a local price map) or --fetch litellm|modelsdev is required")?;
        let source = flag(rest, "--source").unwrap_or("litellm");
        let n = cli::pricing_refresh_from_file(from, &date, out, source)?;
        println!("wrote {n} model(s) -> {out} (effective {date}, source {source})");
        return Ok(());
    }
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    print!(
        "{}",
        cli::pricing_status(&db_path(rest), &pricing, &cli::today_local())?
    );
    Ok(())
}

/// `tare backfill [--dir PATH]`: import Claude Code JSONL transcripts into the store
/// as `source=jsonl` steps — captures sessions that ran before Tare was installed / while it was
/// down. Idempotent (safe to re-run); skips sessions already captured live. `--dir` overrides the
/// auto-resolved `~/.claude/projects` roots (repeatable).
fn cmd_backfill(rest: &[String]) -> Result<(), String> {
    let dirs: Vec<std::path::PathBuf> = {
        if flag_present_without_value(rest, "--dir") {
            return Err("backfill: --dir requires a path".into());
        }
        let dir_values = flags_all(rest, "--dir");
        if dir_values.iter().any(|value| value.trim().is_empty()) {
            return Err("backfill: --dir requires a non-empty path".into());
        }
        let explicit: Vec<std::path::PathBuf> = dir_values
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect();
        if explicit.is_empty() {
            cli::transcript_watch::resolve_project_dirs()
        } else {
            explicit
        }
    };
    if dirs.is_empty() {
        println!("no Claude Code projects directory found (nothing to backfill)");
        return Ok(());
    }
    let n = cli::backfill_transcripts(&db_path(rest), &dirs)?;
    println!(
        "backfilled {n} step(s) from {} transcript dir(s)",
        dirs.len()
    );
    Ok(())
}

/// `tare savings [--json]`: the unified Savings Ledger as a ranked CLI lens — every
/// recoverable-dollar opportunity (loops, failures, wasted cache, cache advice, context-bloat,
/// rightsizing, model-swap) with its confidence, effort, and fix, plus capped potential + index.
fn cmd_savings(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    // Lifecycle actions: accept/un-accept an opportunity by "<kind>:<label>".
    if let Some(key) = flag(rest, "--accept") {
        let today = flag(rest, "--today")
            .map(str::to_string)
            .unwrap_or_else(cli::today_local);
        let recoverable = cli::accept_savings_for(&db_path(rest), key, &today, &pricing)?;
        println!(
            "accepted {key} on {today} (recoverable estimate {}) — run `tare realized` later to prove it",
            tare_core::money::MicroUsd(recoverable).to_dollar_string()
        );
        return Ok(());
    }
    if let Some(key) = flag(rest, "--unaccept") {
        cli::unaccept_savings_for(&db_path(rest), key)?;
        println!("un-accepted {key}");
        return Ok(());
    }
    let led = cli::savings_for(&db_path(rest), &pricing)?;
    let accepted = cli::accepted_savings_keys(&db_path(rest))?;
    if has(rest, "--json") {
        // Emit the ledger plus the accepted key-set so JSON consumers can mark rows too (the ledger
        // alone never carried acceptance state).
        let mut keys: Vec<&String> = accepted.iter().collect();
        keys.sort();
        let wrapped = serde_json::json!({ "ledger": led, "accepted": keys });
        println!(
            "{}",
            serde_json::to_string_pretty(&wrapped).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_savings_text_marked(&led, &accepted));
    }
    Ok(())
}

/// `tare plan`: ONE ranked worklist merging recoverable opportunities + at-risk
/// advisories, every row dollar-quantified. Recoverable floor and at-risk exposure stay separate.
fn cmd_plan(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let plan = cli::action_plan_for(&db_path(rest), &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_action_plan_text(&plan));
    }
    Ok(())
}

/// `tare realized [--window N] [--json]`: the savings-realization lifecycle — for
/// each accepted opportunity, total spend before vs after the accept date, proving whether it paid
/// off (open → accepted → realized). `--window` sets the comparison window length (default 7 days).
fn cmd_realized(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let today = flag(rest, "--today")
        .map(str::to_string)
        .unwrap_or_else(cli::today_local);
    let window = flag_parsed::<i64>(rest, "--window")?.unwrap_or(7);
    if window < 1 {
        return Err("realized: --window must be at least 1 day".into());
    }
    let led = cli::realized_for(&db_path(rest), &today, window, &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&led).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_realization_text(&led));
    }
    Ok(())
}

/// `tare digest [--out FILE] [--json]`: a local weekly digest — WoW spend delta, top
/// drivers, new anomalies, unclaimed savings — recomposed from stored counts. Written to stdout or
/// `--out FILE`; NEVER sent off-box.
fn cmd_digest(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let today = flag(rest, "--today")
        .map(str::to_string)
        .unwrap_or_else(cli::today_local);
    let d = cli::digest_for(&db_path(rest), &today, &pricing)?;
    let rendered = if has(rest, "--json") {
        serde_json::to_string_pretty(&d).map_err(|e| e.to_string())?
    } else {
        tare_core::digest::render_digest_text(&d)
    };
    match flag(rest, "--out") {
        Some(path) => {
            std::fs::write(path, &rendered).map_err(|e| format!("digest: write {path}: {e}"))?;
            println!("wrote digest to {path}");
        }
        None => print!("{rendered}"),
    }
    Ok(())
}

/// `tare estimate --like <run-id> [--to <model>]`: a pre-flight cost band repriced
/// from a stored run's shape — no model call, no payload. `--to` reprices onto a swap target
/// (widening the band by the cross-tokenizer caveat). `--json` for the raw band.
fn cmd_estimate(rest: &[String]) -> Result<(), String> {
    let run_id = flag(rest, "--like")
        .or_else(|| {
            positionals(rest, &["--db", "--pricing", "--to", "--like"])
                .first()
                .map(|s| s.as_str())
        })
        .ok_or("estimate: expected --like <run-id>")?;
    let target = flag(rest, "--to");
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let est = cli::estimate_for(&db_path(rest), run_id, target, &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&est).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_estimate_text(&est));
    }
    Ok(())
}

/// `tare quality`: attach or read a user-supplied quality scalar per run — the
/// cost×quality frontier's y-axis. Tare stores the number; it never computes, judges, or reads a
/// payload to derive it.
///   tare quality --all                 list every scored run
///   tare quality <run-id>              show one run's score
///   tare quality <run-id> <score>      set it (--source cli|header|ci, default cli)
///   tare quality <run-id> --clear      remove it
fn cmd_quality(rest: &[String]) -> Result<(), String> {
    let db = db_path(rest);
    let json = has(rest, "--json");
    if has(rest, "--all") {
        let all = cli::all_quality_for(&db)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?
            );
        } else if all.is_empty() {
            println!("no quality scores recorded");
        } else {
            for q in &all {
                println!("{:<24} {:>4}  ({})", q.run_id, q.score, q.source);
            }
        }
        return Ok(());
    }
    // First non-flag positional is the run id; an optional second is the score to set. Skip flag
    // VALUES (e.g. the `--db` path) so they aren't mistaken for the run-id/score.
    let positionals = positionals(rest, VALUE_FLAGS);
    let run_id = positionals
        .first()
        .ok_or("quality: expected <run-id> (or --all)")?;
    if has(rest, "--clear") {
        cli::clear_quality_for(&db, run_id)?;
        println!("cleared quality for {run_id}");
        return Ok(());
    }
    match positionals.get(1) {
        Some(score_str) => {
            let score: i64 = score_str
                .parse()
                .map_err(|_| format!("quality: score {score_str:?} must be an integer 0-100"))?;
            // Range-validate the 0-100 score; reject out-of-range values so a
            // fat-fingered `999` or `-5` fails loudly instead of being stored as a nonsense scalar.
            if !(0..=100).contains(&score) {
                return Err(format!(
                    "quality: score {score} out of range (expected 0-100)"
                ));
            }
            let source = flag(rest, "--source").unwrap_or("cli");
            if !matches!(source, "cli" | "header" | "ci") {
                return Err("quality: --source must be one of: cli | header | ci".into());
            }
            cli::set_quality_for(&db, run_id, score, source)?;
            println!("set quality for {run_id}: {score} ({source})");
        }
        None => match cli::quality_for(&db, run_id)? {
            Some(q) if json => println!(
                "{}",
                serde_json::to_string_pretty(&q).map_err(|e| e.to_string())?
            ),
            Some(q) => println!("{}: {} ({})", q.run_id, q.score, q.source),
            None => println!("no quality score for {run_id}"),
        },
    }
    Ok(())
}

/// `tare heatmap [--window N]`: a calendar heatmap of daily spend intensity. Its
/// day×hour companion is `tare punchcard`.
fn cmd_heatmap(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let window = flag_parsed::<usize>(rest, "--window")?;
    if window == Some(0) {
        return Err("heatmap: --window must be at least 1".into());
    }
    let m = cli::heatmap_for(&db_path(rest), &pricing, window)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", tare_core::heatmap::render_heatmap_text(&m));
    }
    Ok(())
}

/// `tare punchcard [--json]`: the day×hour "when do I burn tokens?" grid — each run's
/// priced cost bucketed by weekday × hour-of-day (the hour stamped at ingest from the turn's own
/// timestamp, UTC). Runs captured before the hour existed, or without a per-turn timestamp, are an
/// honest GAP (excluded). Offline; estimate.
fn cmd_punchcard(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let m = cli::punchcard_for(&db_path(rest), &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", tare_core::punchcard::render_punchcard_text(&m));
    }
    Ok(())
}

/// `tare share [--run RUN] [--out FILE]`: a single-file, self-contained, redacted
/// HTML report (Savings Ledger + optional run flamegraph). Writes to `--out` or stdout.
fn cmd_share(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let run = flag(rest, "--run");
    let title = flag(rest, "--title").unwrap_or("Tare cost report");
    let generated = flag(rest, "--date")
        .map(|s| s.to_string())
        .unwrap_or_else(cli::today_local);
    let experiment_to: Vec<String> = flag(rest, "--experiment-to")
        .map(|s| {
            s.split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let html = cli::share_html_for(
        &db_path(rest),
        &pricing,
        run,
        title,
        &generated,
        &experiment_to,
    )?;
    match flag(rest, "--out") {
        Some(path) => {
            std::fs::write(path, &html).map_err(|e| format!("write {path}: {e}"))?;
            println!("tare: wrote {path} ({} bytes)", html.len());
        }
        None => print!("{html}"),
    }
    Ok(())
}

/// `tare flamediff --a RUN --b RUN [--normalize] [--svg]`: the node-level regression
/// between two runs. Default output is the diff model JSON; `--svg` renders the red/blue picture.
fn cmd_flamediff(rest: &[String]) -> Result<(), String> {
    let a = flag(rest, "--a").ok_or("tare flamediff needs --a <run-id>")?;
    let b = flag(rest, "--b").ok_or("tare flamediff needs --b <run-id>")?;
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let model = cli::flame_diff_for(&db_path(rest), &pricing, a, b, has(rest, "--normalize"))?;
    if has(rest, "--svg") {
        println!("{}", tare_core::svg::render_diff_svg(&model));
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&model).map_err(|e| e.to_string())?
        );
    }
    Ok(())
}

/// `tare today [--oneline]`: today's estimated spend, with a compact one-line form
/// for a tmux/shell prompt or a menubar/statusline widget. The local calendar day is the boundary.
fn cmd_today(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let date = flag(rest, "--date")
        .map(|s| s.to_string())
        .unwrap_or_else(cli::today_local);
    let t = cli::today_spend_for(&db_path(rest), &date, &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&t).map_err(|e| e.to_string())?
        );
    } else if has(rest, "--oneline") {
        println!("{}", cli::render_today_text(&t, &date, true));
    } else {
        print!("{}", cli::render_today_text(&t, &date, false));
    }
    Ok(())
}

/// `tare statusline`: a Claude Code statusLine command. Reads Claude Code's JSON on
/// stdin and prints one line pairing Tare's own session estimate with Claude Code's reported
/// `total_cost_usd` as a vendor cross-check. Deliberately robust — Claude Code invokes it on every
/// message, so it always prints a line and never returns an error (a failure would blank the user's
/// status line). Configure it through `~/.claude/settings.json` `statusLine.command`; Tare does not
/// overwrite that shared slot automatically.
fn cmd_statusline(rest: &[String]) -> Result<(), String> {
    use std::io::Read;
    const STATUS_INPUT_CAP: u64 = 1024 * 1024;
    let mut input = String::new();
    // Status input is a small JSON object. Bound malformed pipes so this always-on command cannot
    // grow memory without limit; preserving its "always print a line" contract matters more than
    // surfacing a read error to Claude Code.
    let _ = std::io::stdin()
        .take(STATUS_INPUT_CAP)
        .read_to_string(&mut input);
    let line = match cli::load_pricing(flag(rest, "--pricing")) {
        Ok(pricing) => cli::statusline_for(&db_path(rest), &pricing, &input),
        // No pricing table → still render the vendor cross-check, just without a Tare figure.
        Err(_) => cli::render_statusline(None, &cli::parse_statusline_input(&input)),
    };
    println!("{line}");
    Ok(())
}

/// `tare streaks`: budget goals + a daily-return streak counter over the captured
/// daily series. Counts-only; goals come from flags (dollars → micro-USD at the boundary).
fn cmd_streaks(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let goals = tare_core::streaks::Goals {
        max_daily_micros: flag_parsed::<f64>(rest, "--max-daily-usd")?
            .map(|dollars| {
                tare_core::config::dollars_to_micros(dollars).ok_or_else(|| {
                    "streaks: --max-daily-usd must be a finite, non-negative amount".to_string()
                })
            })
            .transpose()?,
        min_cache_read_pct: flag_parsed::<i64>(rest, "--min-cache-read-pct")?,
    };
    goals
        .validate()
        .map_err(|error| format!("streaks: {error}"))?;
    let window = flag_parsed::<usize>(rest, "--window")?;
    if window == Some(0) {
        return Err("streaks: --window must be at least 1".into());
    }
    let rep = cli::streaks_for(&db_path(rest), &pricing, &goals, window)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_streaks_text(&rep));
    }
    Ok(())
}

fn cmd_whatif(rest: &[String]) -> Result<(), String> {
    use tare_core::whatif::{RoutingPolicy, Swap};
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    if let Some(to) = flag(rest, "--route") {
        let when_output_below = match flag(rest, "--route-when-output-below") {
            Some(s) => s
                .parse::<u64>()
                .map_err(|_| format!("--route-when-output-below expects an integer, got {s:?}"))?,
            None => 500,
        };
        let policy = RoutingPolicy {
            to: to.to_string(),
            when_output_below,
        };
        let rep = cli::route_whatif_for(&db_path(rest), &policy, &pricing)?;
        if has(rest, "--json") {
            println!(
                "{}",
                serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
            );
        } else {
            print!("{}", cli::render_whatif_text(&rep));
        }
        return Ok(());
    }
    if has(rest, "--recommend") {
        let rec =
            cli::whatif_recommend_for(&db_path(rest), has(rest, "--cross-provider"), &pricing)?;
        if has(rest, "--json") {
            println!(
                "{}",
                serde_json::to_string_pretty(&rec).map_err(|e| e.to_string())?
            );
        } else {
            print!("{}", cli::render_whatif_recommend_text(&rec));
        }
        return Ok(());
    }
    let mut swaps: Vec<Swap> = Vec::new();
    if let Some(to) = flag(rest, "--swap-all-to") {
        swaps.push(Swap::AllTo { to: to.to_string() });
    }
    for spec in flags_all(rest, "--swap") {
        let (from, to) = spec
            .split_once('=')
            .ok_or_else(|| format!("--swap expects A=B, got {spec:?}"))?;
        swaps.push(Swap::Model {
            from: from.to_string(),
            to: to.to_string(),
        });
    }
    if swaps.is_empty() {
        return Err("whatif: expected --swap A=B (repeatable) or --swap-all-to MODEL".into());
    }
    let rep = cli::whatif_for(&db_path(rest), &swaps, &pricing)?;
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_whatif_text(&rep));
    }
    Ok(())
}

fn cmd_trend(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let dim = match flag(rest, "--by") {
        None => tare_core::trend::TrendDimension::Total,
        Some(value) => tare_core::trend::TrendDimension::parse(value).ok_or_else(|| {
            format!("trend: unknown --by {value:?} (expected total|provider|model|cause)")
        })?,
    };
    let report = cli::trend_for(
        &db_path(rest),
        flag(rest, "--from"),
        flag(rest, "--to"),
        dim,
        &pricing,
    )?;
    let Some(report) = report else {
        println!("no spend recorded yet");
        return Ok(());
    };
    if has(rest, "--anomalies") {
        // --window/--threshold > [anomaly] window/threshold in tare.toml > built-in 7/50.
        let cfg = cli::load_config();
        let window = flag_parsed(rest, "--window")?
            .or(cfg.anomaly.window)
            .unwrap_or(7);
        let threshold = flag_parsed(rest, "--threshold")?
            .or(cfg.anomaly.threshold)
            .unwrap_or(50);
        if window == 0 {
            return Err("trend: --window must be at least 1".into());
        }
        if threshold < 0 {
            return Err("trend: --threshold must not be negative".into());
        }
        // --why: decompose each spike into volume × size × efficiency + a bisect link.
        if has(rest, "--why") {
            let whys = cli::anomaly_why_for(
                &db_path(rest),
                &pricing,
                flag(rest, "--from"),
                flag(rest, "--to"),
                dim,
                window,
                threshold,
            )?;
            if has(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&whys).map_err(|e| e.to_string())?
                );
            } else {
                print!("{}", cli::render_anomaly_why_text(&whys));
            }
            return Ok(());
        }
        let found = tare_core::anomaly::detect(&report, window, threshold);
        if has(rest, "--json") {
            println!(
                "{}",
                serde_json::to_string_pretty(&found).map_err(|e| e.to_string())?
            );
        } else if found.is_empty() {
            println!("no spend anomalies in this window");
        } else {
            for a in &found {
                println!(
                    "{} [{:?}] {} : {} (baseline {})",
                    a.date,
                    a.kind,
                    a.series_key,
                    tare_core::money::MicroUsd(a.value_micros).to_dollar_string(),
                    tare_core::money::MicroUsd(a.baseline_micros).to_dollar_string()
                );
            }
        }
        return Ok(());
    }
    if let Some(path) = flag(rest, "--svg") {
        let svg = tare_core::svg::render_trend_svg(&report);
        std::fs::write(path, svg).map_err(|e| format!("write svg {path}: {e}"))?;
        eprintln!("tare: wrote {path}");
    } else if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_trend_text(&report));
    }
    Ok(())
}

/// All values passed under a repeated flag, e.g. `--run A --run B` -> ["A","B"].
fn flags_all<'a>(args: &'a [String], name: &str) -> Vec<&'a str> {
    args.iter()
        .enumerate()
        .filter_map(|(index, arg)| {
            if arg == name {
                return args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .map(String::as_str);
            }
            arg.strip_prefix(name)
                .and_then(|suffix| suffix.strip_prefix('='))
        })
        .collect()
}

fn cmd_diff(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    let runs = flags_all(rest, "--run");
    let has_run_flag = rest
        .iter()
        .any(|arg| arg == "--run" || arg.starts_with("--run="));
    let d = if has_run_flag && runs.len() != 2 {
        return Err("diff: --run must be supplied exactly twice".into());
    } else if runs.len() == 2 {
        // Diff two stored runs (K7a).
        cli::diff_runs(&db_path(rest), runs[0], runs[1], &pricing)?
    } else if let (Some(f1), Some(t1), Some(f2), Some(t2)) = (
        flag(rest, "--from"),
        flag(rest, "--to"),
        flag(rest, "--from2"),
        flag(rest, "--to2"),
    ) {
        // Diff two stored date windows (K7a).
        cli::diff_windows(&db_path(rest), f1, t1, f2, t2, &pricing)?
    } else {
        let files = positionals(
            rest,
            &[
                "--db",
                "--pricing",
                "--run",
                "--from",
                "--to",
                "--from2",
                "--to2",
            ],
        );
        if files.len() < 2 {
            return Err(
                "diff: expected <before.json> <after.json>, or --run A --run B, or --from/--to + --from2/--to2"
                    .into(),
            );
        }
        cli::diff_files(files[0], files[1])?
    };
    if has(rest, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&d).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", cli::render_diff_text(&d));
    }
    Ok(())
}

fn cmd_gate(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    // A malformed threshold must error rather than silently drop the check and let the gate pass.
    let max = match flag_parsed::<f64>(rest, "--max-spend")? {
        Some(dollars) => Some(
            tare_core::config::dollars_to_micros(dollars).ok_or_else(|| {
                "--max-spend must be a finite, non-negative amount within micro-USD range"
                    .to_string()
            })?,
        ),
        None => None,
    };
    let max_unpriced = flag_parsed::<u64>(rest, "--max-unpriced-tokens")?;
    let shape = cli::ShapeGateOpts {
        max_system_prompt_tokens: flag_parsed(rest, "--max-system-prompt-tokens")?,
        max_tool_def_tokens: flag_parsed(rest, "--max-tool-def-tokens")?,
        require_cache_read_ratio_pct: flag_parsed(rest, "--require-cache-read-ratio")?,
        no_retry_loops: has(rest, "--no-retry-loops"),
        max_component_growth_pct: flag_parsed(rest, "--max-component-growth-pct")?,
    };
    let resolved_ref_baseline = match flag(rest, "--baseline-ref") {
        Some(reference) => {
            let cwd = std::env::current_dir().map_err(|error| {
                format!("gate: cannot resolve the current directory for --baseline-ref: {error}")
            })?;
            let commit = cli::detect_merge_base(&cwd, reference).ok_or_else(|| {
                format!("gate: cannot resolve merge-base for --baseline-ref {reference:?}")
            })?;
            Some(
                cli::baseline_from_commit(&db_path(rest), &pricing, &commit)?.ok_or_else(|| {
                    format!("gate: no captured spend is attributed to merge-base commit {commit}")
                })?,
            )
        }
        None => None,
    };
    let (passed, summary) = cli::gate_with_baseline_total(
        &db_path(rest),
        max,
        &pricing,
        flag(rest, "--baseline"),
        has(rest, "--fail-on-regression"),
        max_unpriced,
        flag(rest, "--baseline-run"),
        &shape,
        resolved_ref_baseline,
    )?;
    // The comment mode still exits non-zero on failure, so the CI step both comments and blocks.
    if has(rest, "--github-comment") {
        let (total, mut baseline) =
            cli::gate_totals(&db_path(rest), &pricing, flag(rest, "--baseline-run"))?;
        if let Some(path) = flag(rest, "--baseline") {
            baseline = Some(cli::load_report(path)?.total_micros);
        } else if resolved_ref_baseline.is_some() {
            baseline = resolved_ref_baseline;
        }
        println!(
            "{}",
            cli::render_gate_pr_comment(passed, total, baseline, &pricing.version)
        );
    } else {
        print!("{summary}");
    }
    if !passed {
        exit(1);
    }
    Ok(())
}

/// `tare daemon`: a fully local "something changed in my token bill" alarm. Each tick
/// builds the trend, detects anomalies, and delivers NEW ones once through the stderr sink.
/// One-shot with `--once` (gated path); otherwise loops on an interval. The loop/sleep lives
/// behind the `daemon` feature so it stays out of the offline gate; the delivery logic
/// (`cli::anomaly_delivery_tick`) is always compiled and tested.
fn cmd_daemon(rest: &[String]) -> Result<(), String> {
    use tare_daemon::AlertSink as _;
    let cfg = cli::load_config_strict()?;
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    // --window/--threshold > [anomaly] window/threshold in tare.toml > built-in 7/50.
    let window = flag_parsed(rest, "--window")?
        .or(cfg.anomaly.window)
        .unwrap_or(7);
    let threshold = flag_parsed(rest, "--threshold")?
        .or(cfg.anomaly.threshold)
        .unwrap_or(50);
    if window == 0 {
        return Err("daemon: --window must be at least 1".into());
    }
    if threshold < 0 {
        return Err("daemon: --threshold must not be negative".into());
    }
    // Where the weekly digest is written: --digest-dir, else next to the db, else cwd.
    let digest_dir = flag(rest, "--digest-dir")
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            std::path::Path::new(&db_path(rest))
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        });
    let emit_new = || -> Result<usize, String> {
        let fired = cli::monitor_delivery_tick(
            &db_path(rest),
            &pricing,
            &cli::today_local(),
            window,
            threshold,
        )?;
        for a in &fired {
            tare_daemon::StderrSink.emit(a).map_err(|e| e.to_string())?;
        }
        // Weekly digest: fire-once per ISO week, written to a file with a local notice.
        match cli::maybe_write_weekly_digest(
            &db_path(rest),
            &pricing,
            &cli::today_local(),
            &digest_dir,
        ) {
            Ok(Some(path)) => {
                eprintln!("tare daemon: wrote weekly digest → {path}");
                // Best-effort local desktop notification — the file is the source of truth.
                cli::fire_desktop_notification(
                    "Tare — weekly digest ready",
                    &format!("Your cost digest for the week was written to {path}"),
                );
            }
            Ok(None) => {}
            Err(e) => eprintln!("tare daemon: weekly digest skipped: {e}"),
        }
        // Weekly pricing refresh: fire-once per ISO week, silent-fallback. Only in a
        // `pricing-fetch` build; a plain daemon simply doesn't auto-refresh pricing.
        #[cfg(feature = "pricing-fetch")]
        match cli::maybe_refresh_pricing_weekly(
            &db_path(rest),
            &cli::today_local(),
            "litellm",
            &digest_dir,
        ) {
            Ok(Some(path)) => eprintln!("tare daemon: refreshed pricing → {path}"),
            Ok(None) => {}
            Err(e) => eprintln!("tare daemon: pricing refresh skipped: {e}"),
        }
        Ok(fired.len())
    };

    if has(rest, "--once") {
        let n = emit_new()?;
        eprintln!("tare daemon: delivered {n} new anomaly alert(s)");
        return Ok(());
    }

    #[cfg(feature = "daemon")]
    {
        let secs: u64 = flag_parsed::<u64>(rest, "--interval")?.unwrap_or(3600);
        if secs == 0 {
            return Err("daemon: --interval must be at least 1 second".into());
        }
        eprintln!("tare daemon: watching every {secs}s (Ctrl-C to stop)");
        loop {
            let _ = emit_new()?;
            std::thread::sleep(std::time::Duration::from_secs(secs));
        }
    }
    #[cfg(not(feature = "daemon"))]
    {
        Err("daemon loop is built behind the `daemon` feature; use `tare daemon --once` for a single check".into())
    }
}

/// Resolve serve args (like `serve`) and start everything, opening the UI unless --no-open.
fn run_up(rest: &[String]) -> Result<(), String> {
    let cfg = cli::load_config_strict()?;
    let port = resolved_port(rest, "--port", cfg.proxy.port, 8788)?;
    let otlp_port = resolved_port(rest, "--otlp-port", cfg.proxy.otlp_port, 4318)?;
    let pricing = flag(rest, "--pricing")
        .map(str::to_string)
        .or_else(|| cfg.proxy.pricing.clone());
    cli::up_command(
        &db_path(rest),
        port,
        otlp_port,
        pricing.as_deref(),
        !has(rest, "--no-open"),
    )
}

fn cmd_demo(rest: &[String]) -> Result<(), String> {
    let pricing = cli::load_pricing(flag(rest, "--pricing"))?;
    // `--db <path>`: seed the bundled demo run into a store so the dashboard is populated in
    // seconds. Tagged source/run "demo" so the UI flags it as sample data.
    if let Some(db) = flag(rest, "--db") {
        let run_id = cli::demo_seed_store(db)?;
        eprintln!(
            "tare: seeded sample run '{run_id}' into {db} — open the dashboard to explore it"
        );
        return Ok(());
    }
    if let Some(svg) = flag(rest, "--svg") {
        cli::demo_write_svg(svg, &pricing)?;
        eprintln!("tare: wrote {svg}");
    }
    if let Some(ss) = flag(rest, "--speedscope") {
        cli::demo_write_speedscope(ss, &pricing)?;
        eprintln!("tare: wrote {ss}");
    }
    if !has(rest, "--svg") && !has(rest, "--speedscope") {
        let model = cli::demo_model(&pricing)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&model).map_err(|e| e.to_string())?
        );
    }
    Ok(())
}

fn default_run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("run-{nanos}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::{
        flag, flag_parsed, flag_present_without_value, flags_all, positionals, resolved_port,
        validate_command_args, VALUE_FLAGS,
    };

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn positionals_skip_flag_values() {
        // `tare quality --db /x.db <run> <score>` must NOT read `/x.db` as the run-id.
        let a = args(&["--db", "/tmp/x.db", "run-123", "80", "--json"]);
        let p: Vec<&str> = positionals(&a, VALUE_FLAGS)
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            p,
            vec!["run-123", "80"],
            "flag value /tmp/x.db is not a positional"
        );

        // --db AFTER the positionals, and the `=`-form, both leave positionals intact.
        let a2 = args(&["run-9", "50", "--db=/x.db", "--source", "ci"]);
        let p2: Vec<&str> = positionals(&a2, VALUE_FLAGS)
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            p2,
            vec!["run-9", "50"],
            "--source value 'ci' is not a positional either"
        );

        // boolean flags never eat a following positional.
        let a3 = args(&["--all", "run-7"]);
        let p3: Vec<&str> = positionals(&a3, VALUE_FLAGS)
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(p3, vec!["run-7"]);

        let negative = args(&["run-7", "-5"]);
        let values: Vec<&str> = positionals(&negative, VALUE_FLAGS)
            .iter()
            .map(|value| value.as_str())
            .collect();
        assert_eq!(values, vec!["run-7", "-5"]);
    }

    #[test]
    fn valueless_safety_flag_fails_closed() {
        // `tare gate --max-spend` with no value must ERROR, not silently no-op.
        assert!(flag_present_without_value(
            &args(&["--max-spend"]),
            "--max-spend"
        ));
        assert!(flag_present_without_value(
            &args(&["--max-spend", "--json"]),
            "--max-spend"
        ));
        assert!(!flag_present_without_value(
            &args(&["--max-spend", "5.0"]),
            "--max-spend"
        ));
        // flag_parsed distinguishes valueless (Err, fail-closed) from absent (Ok(None)).
        assert!(!flag_present_without_value(
            &args(&["--json"]),
            "--max-spend"
        ));
        assert!(flag_parsed::<f64>(&args(&["--max-spend"]), "--max-spend").is_err());
        assert!(flag_parsed::<f64>(&args(&["--max-spend", "--json"]), "--max-spend").is_err());
        assert_eq!(
            flag_parsed::<f64>(&args(&["--json"]), "--max-spend").unwrap(),
            None,
            "truly-absent flag is still Ok(None), not an error"
        );
    }

    #[test]
    fn flag_reads_space_and_equals_forms() {
        assert_eq!(flag(&args(&["--port", "8788"]), "--port"), Some("8788"));
        assert_eq!(flag(&args(&["--port=8788"]), "--port"), Some("8788"));
        assert_eq!(flag(&args(&["--other", "x"]), "--port"), None);
    }

    #[test]
    fn flag_rejects_a_following_flag_as_value() {
        // A missing value must read as absent, not swallow the next flag.
        assert_eq!(
            flag(
                &args(&["--max-spend", "--fail-on-regression"]),
                "--max-spend"
            ),
            None
        );
        // ...but the swallowed flag is still found by its own lookup.
        assert!(super::has(
            &args(&["--max-spend", "--fail-on-regression"]),
            "--fail-on-regression"
        ));
    }

    #[test]
    fn flag_parsed_distinguishes_absent_from_malformed() {
        // Absent → Ok(None); present-but-unparseable → Err (gate fails closed).
        assert_eq!(
            flag_parsed::<f64>(&args(&["--x", "1.5"]), "--max-spend").unwrap(),
            None
        );
        assert_eq!(
            flag_parsed::<f64>(&args(&["--max-spend", "5.00"]), "--max-spend").unwrap(),
            Some(5.0)
        );
        assert!(flag_parsed::<f64>(&args(&["--max-spend", "abc"]), "--max-spend").is_err());
        // `=`-form typo that used to slip through as None now parses correctly.
        assert_eq!(
            flag_parsed::<f64>(&args(&["--max-spend=5.00"]), "--max-spend").unwrap(),
            Some(5.0)
        );
    }

    #[test]
    fn resolved_ports_reject_invalid_and_zero_values() {
        assert_eq!(
            resolved_port(&[], "--port", Some(9000), 8788).unwrap(),
            9000
        );
        assert_eq!(resolved_port(&[], "--port", None, 8788).unwrap(), 8788);
        assert!(resolved_port(&args(&["--port", "oops"]), "--port", None, 8788).is_err());
        assert!(resolved_port(&args(&["--port", "0"]), "--port", None, 8788).is_err());
    }

    #[test]
    fn repeated_flags_support_space_and_equals_forms_without_swallowing_options() {
        let values = args(&["--run", "first", "--run=second", "--run", "--json"]);
        assert_eq!(flags_all(&values, "--run"), vec!["first", "second"]);
    }

    #[test]
    fn command_validation_rejects_unknown_duplicate_and_empty_options() {
        assert!(validate_command_args("report", &args(&["--jsno"])).is_err());
        assert!(validate_command_args("report", &args(&["--json", "--json"])).is_err());
        assert!(validate_command_args("report", &args(&["--db="])).is_err());
        assert!(validate_command_args("report", &args(&["--db", "--json"])).is_err());
        assert!(validate_command_args("report", &args(&["--json=true"])).is_err());
    }

    #[test]
    fn command_validation_allows_declared_repeats_and_child_options() {
        assert!(validate_command_args(
            "diff",
            &args(&["--run", "before", "--run=after", "--json"])
        )
        .is_ok());
        assert!(validate_command_args(
            "run",
            &args(&["--db", "tare.db", "--", "child", "--unknown-to-tare"])
        )
        .is_ok());
        assert!(validate_command_args("run", &args(&["--db", "tare.db"])).is_err());
        assert!(validate_command_args("run", &args(&["--"])).is_err());
    }

    #[test]
    fn command_validation_rejects_ignored_modes_and_extra_positionals() {
        assert!(validate_command_args("today", &args(&["--json", "--oneline"])).is_err());
        assert!(validate_command_args("whatif", &args(&["--cross-provider"])).is_err());
        assert!(validate_command_args("trend", &args(&["--window", "7", "--json"])).is_err());
        assert!(
            validate_command_args("savings", &args(&["--accept", "x", "--unaccept", "y"])).is_err()
        );
        assert!(validate_command_args("quality", &args(&["run", "80", "ignored"])).is_err());
        assert!(validate_command_args("service", &args(&["status", "--port", "8788"])).is_err());
    }

    #[test]
    fn command_validation_requires_complete_diff_mode() {
        assert!(validate_command_args(
            "diff",
            &args(&["--from", "2026-01-01", "--to", "2026-01-02"])
        )
        .is_err());
        assert!(validate_command_args("diff", &args(&["before.json", "after.json"])).is_ok());
        assert!(validate_command_args(
            "diff",
            &args(&[
                "--from",
                "2026-01-01",
                "--to",
                "2026-01-02",
                "--from2",
                "2026-01-03",
                "--to2",
                "2026-01-04"
            ])
        )
        .is_ok());
    }
}
