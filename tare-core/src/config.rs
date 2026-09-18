//! Unified, GUI-editable configuration (`tare.toml`) — the single file the CLI and the desktop
//! app both read, so a user can configure capture without touching code or env vars. Sections:
//! `[budget]`, `[privacy]`, `[providers]`, `[proxy]`, `[anomaly]`. Precedence everywhere is
//! **explicit flag > environment > tare.toml > built-in default** (mirroring `PrivacyPolicy`),
//! so existing env/CI users are unaffected.
//!
//! Money note: a user types dollars; `max_spend_usd` is converted to integer micro-USD at
//! this boundary (exactly as `TARE_MAX_SPEND_USD` already is), and nothing downstream uses f64.

use crate::budget::Budget;
use crate::privacy::Profile;
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TareConfig {
    #[serde(default)]
    pub budget: BudgetSettings,
    #[serde(default)]
    pub privacy: PrivacySettings,
    #[serde(default)]
    pub providers: ProviderSettings,
    #[serde(default)]
    pub pricing: PricingSettings,
    #[serde(default)]
    pub proxy: ProxySettings,
    #[serde(default)]
    pub anomaly: AnomalySettings,
    #[serde(default)]
    pub ui: UiSettings,
    #[serde(default)]
    pub capture: CaptureSettings,
    /// Declarative alert rules: `[[alert]]` entries evaluated each monitor tick. Empty
    /// (default) keeps the built-in budget-75/100% + any-anomaly triggers; adding rules augments
    /// them. Channels stay local-first (toast + OS notify) — never webhooks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alert: Vec<AlertRule>,
    /// Prompt/config lineages: `[[lineage]]` entries binding human version labels to
    /// immutable component fingerprints, so `tare lineage <name>` plots cost-per-run across versions.
    /// Empty (default) → no lineages; purely a user-declared grouping over already-captured hashes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<crate::lineage::Lineage>,
    /// Units of work: `[[unit]]` entries with retroactive match rules so `tare unit`
    /// buckets captured runs into dev-meaningful denominators (cost per task / PR / feature). Empty
    /// (default) → no units; purely a user-declared bucketing over already-captured runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unit: Vec<crate::workunit::WorkUnit>,
}

/// One declarative alert rule. `metric` chooses the signal; `threshold` is compared per
/// the metric's unit (dollars for `today_spend`/`run_rate`, percent for `period_pct`, ignored for
/// `anomaly_kind` which matches `kind`). `min_events` suppresses low-volume noise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertRule {
    /// `today_spend` | `period_pct` | `run_rate` | `anomaly_kind`.
    pub metric: String,
    /// Numeric trigger (dollars or percent by metric). Optional for `anomaly_kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// For `anomaly_kind`: which kind to alert on (spike | new_series | vanished_series | any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional anomaly detector window in days. Only valid for `anomaly_kind` rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_days: Option<u32>,
    /// Suppress the rule until at least this many captured events exist (noise floor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_events: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSettings {
    /// Spend cap in dollars (converted to micro-USD at the boundary). `None` = no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spend_usd: Option<f64>,
    /// Soft (warn-only) spend line in dollars: warn at/above, but never block. `None`
    /// falls back to 80% of `max_spend_usd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_spend_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_repeats: Option<u32>,
    /// Periodic (calendar) spend budget, distinct from the per-run kill-switch: `period`
    /// ("week"|"month"), a dollar cap, and a pre-cap warn threshold (default 80%). Backward-
    /// looking only — surfaced as a status bar, never used for clock-anchored forecasting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_max_spend_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warn_pct: Option<i64>,
}

impl BudgetSettings {
    /// The integer-money `Budget` this describes (dollars -> micro-USD here, once).
    pub fn to_budget(&self) -> Budget {
        // Ignore a non-positive cap: a `max_spend_usd = 0` / negative typo would make
        // EVERY request exceed the budget and block all traffic — that's a footgun, not a cost cap.
        // A nonsensical cap reads as "no cap" (with the invariant that a real cap is > 0).
        Budget {
            // Round, don't truncate; a bare `as i64` shaves fractional-micro budgets down.
            max_micros: self
                .max_spend_usd
                .filter(|value| *value > 0.0)
                .and_then(dollars_to_micros),
            soft_micros: self
                .soft_spend_usd
                .filter(|value| *value > 0.0)
                .and_then(dollars_to_micros),
            max_steps: self.max_steps,
            max_identical_repeats: self.max_repeats,
        }
    }
}

/// Convert a non-negative dollar amount to integer micro-USD. Invalid and unrepresentable values
/// return `None` rather than relying on a saturating float-to-integer cast.
pub fn dollars_to_micros(usd: f64) -> Option<i64> {
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    let micros = (usd * 1_000_000.0).round();
    // `i64::MAX as f64` is rounded to 2^63, already one past the representable range.
    if !micros.is_finite() || micros < 0.0 || micros >= i64::MAX as f64 {
        return None;
    }
    Some(micros as i64)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt: Option<String>,
    /// Stamp captured runs with the local git commit/author so spend rolls up by
    /// commit/author. OFF by default — it records the current commit SHA + author name (short,
    /// opaque labels, never a diff/message), which some users prefer to keep out of the store.
    #[serde(default)]
    pub git_attribution: bool,
    /// Opt out of per-step latency capture. This is part of the unified config too, so a Settings
    /// rewrite cannot discard the flag consumed by `PrivacyPolicy`.
    #[serde(default)]
    pub suppress_latency: bool,
}

// No `Eq`: `local_overlay` carries f64 rates, which are only `PartialEq`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_upstream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_upstream: Option<String>,
    /// Gemini (Generative Language API) upstream override; the proxy has a first-class Gemini
    /// provider, so it gets a config knob alongside the other two. `None` = public default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_upstream: Option<String>,
    /// Azure OpenAI upstream override. Azure/Bedrock have a deliberately-failing built-in default,
    /// so they only route once given an explicit endpoint (Azure also reads AZURE_OPENAI_ENDPOINT).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure_openai_upstream: Option<String>,
    /// Bedrock (Converse API) upstream override (e.g. the regional bedrock-runtime endpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock_upstream: Option<String>,
    /// User-supplied cost overlay for self-hosted backends: so local runs read as a
    /// real (estimated, user-owned) figure instead of $0. Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_overlay: Vec<LocalOverlay>,
}

/// One self-hosted backend's user-supplied rate. The effective per-Mtok price is either
/// given directly (`usd_per_mtok`) or derived from an energy estimate (`kwh_per_mtok * usd_per_kwh`)
/// — whichever is present, direct wins. Applied to both input and output tokens (self-hosted
/// compute doesn't price the two sides differently). All figures are the user's own estimate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalOverlay {
    /// Backend label matching the captured vendor: `ollama` | `vllm` | `llama.cpp` | `tgi` | ….
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_per_mtok: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kwh_per_mtok: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_per_kwh: Option<f64>,
}

impl LocalOverlay {
    /// The effective micro-USD per Mtok, or `None` when neither a direct price nor a complete
    /// energy estimate is supplied (a bare backend label with no rate prices nothing).
    pub fn micro_per_mtok(&self) -> Option<i64> {
        let energy = match (self.kwh_per_mtok, self.usd_per_kwh) {
            (Some(kwh), Some(rate)) => Some(kwh * rate),
            _ => None,
        };
        let usd = self.usd_per_mtok.or(energy)?;
        dollars_to_micros(usd)
    }
}

/// `[pricing]` settings (slice): user-supplied per-model rate overrides, applied last in
/// [`crate::pricing::PricingTable::lookup`] so a user can price an otherwise-unpriced model (turning a
/// GAP into an estimate) or correct a stale rate. Opt-in: empty by default → pricing is unchanged.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingSettings {
    #[serde(default, alias = "override")]
    pub overrides: Vec<ModelOverride>,
    /// Repricing mode: `as-of` (default — price at the table's editions) or `latest`
    /// (reprice all history at the newest edition, "what would this cost at today's prices?"). Only
    /// affects a multi-edition table; single-edition pricing is identical under either. `#[serde(default)]`
    /// → an absent `reprice` key keeps the current behavior byte-identical.
    #[serde(default)]
    pub reprice: crate::pricing::PricingMode,
}

/// One per-model rate override, in USD per million tokens (the user's own estimate). Input + output
/// are the rates; `cache_read_usd_per_mtok` is optional and defaults to the input rate (a conservative
/// stand-in for an unpriced model, where cached ≈ fresh). Cache-write defaults to the input rate too.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOverride {
    /// Raw model id to match exactly (e.g. a local model name or a provider model id).
    pub model: String,
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_usd_per_mtok: Option<f64>,
}

/// `[capture]` — how the daemon runs. The desktop app reads `mode` to decide whether to
/// register an always-on login item, spawn a child only while the window is open, or capture nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSettings {
    #[serde(default)]
    pub mode: CaptureMode,
    /// Independent per-intake toggle for the zero-setup JSONL session-log lane.
    /// Default on — it's structurally counts-only (no prompt/response text) so there's nothing to
    /// leak — but a privacy-minded user can disable it (e.g. keep OTLP on, JSONL off) without
    /// switching the whole capture mode off.
    #[serde(default = "capture_jsonl_default")]
    pub jsonl: bool,
}

fn capture_jsonl_default() -> bool {
    true
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            mode: CaptureMode::default(),
            jsonl: true,
        }
    }
}

/// When capture runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Default: capture only while the app/tray is open; catch up the gap from JSONL on each open.
    #[default]
    AppOnly,
    /// Opt-in: an OS login item keeps the daemon capturing across restarts, even with the app closed.
    AlwaysOn,
    /// Capture nothing.
    Off,
}

impl CaptureMode {
    /// The wire/UI string form (matches the serde `snake_case` rename), for the read API + toggle.
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMode::AppOnly => "app_only",
            CaptureMode::AlwaysOn => "always_on",
            CaptureMode::Off => "off",
        }
    }

    /// What to do to the OS login item to match this mode, given whether one is currently installed.
    /// Pure decision — the caller performs the actual install/uninstall (the desktop
    /// app shells out to `tare service install|uninstall`). Only `always_on` keeps a login item; the
    /// other modes remove one if present so switching away never leaves a daemon running at login.
    pub fn service_action(self, installed: bool) -> ServiceAction {
        match self {
            CaptureMode::AlwaysOn if !installed => ServiceAction::Install,
            CaptureMode::AlwaysOn => ServiceAction::Leave,
            _ if installed => ServiceAction::Uninstall,
            _ => ServiceAction::Leave,
        }
    }
}

/// The install-side effect a [`CaptureMode`] implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Install,
    Uninstall,
    Leave,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<String>,
    /// Out-of-band OTLP receiver port for `tare serve` / `tare connect` (OTel default 4318).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_port: Option<u16>,
    /// Path to a pricing table that also prices self-hosted (`provider=local`) models, used by
    /// `tare serve --pricing`. `None` = the built-in frontier pricing only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<String>,
}

/// `[anomaly]` — defaults for the local spend-anomaly alarm (`tare daemon`, `trend --anomalies`,
/// and the desktop/web anomalies view). `window` = trailing days of baseline; `threshold` = the
/// percent jump over baseline that flags a day.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnomalySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<i64>,
    /// Acknowledged (dismissed false-positive) anomalies — `date:series:kind` keys.
    /// detect()'s output is filtered against this everywhere anomalies render/alert.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acknowledged: Vec<String>,
    /// Vantage-style noise floor: minimum absolute dollar impact (micro-USD) for a spike to surface.
    /// Omitted/0 = off. Kills sub-cent jitter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dollar_floor_micros: Option<i64>,
    /// Noise floor: minimum share of the day's total spend (percent) to surface.
    /// Omitted/0 = off. Kills a big number that's a rounding error against a huge day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pct_of_daily_floor: Option<i64>,
    /// Suppress a repeat spike of the same (series, kind) within this many days.
    /// Omitted/0 = off. A sustained shift alarms once, not every day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_window_days: Option<usize>,
}

impl AnomalySettings {
    /// The Vantage-style noise filters as configured — all-zero (permissive) by default,
    /// so an un-tuned config reproduces the pre-4e2s anomaly output byte-for-byte.
    pub fn noise_filters(&self) -> crate::anomaly::NoiseFilters {
        crate::anomaly::NoiseFilters {
            dollar_floor_micros: self.dollar_floor_micros.unwrap_or(0),
            pct_of_daily_floor: self.pct_of_daily_floor.unwrap_or(0),
            dedupe_window_days: self.dedupe_window_days.unwrap_or(0),
        }
    }
}

/// `[ui]` — presentation/boundary settings. `tz_offset_minutes` shifts day-bucketing for
/// "today"/trend/overview into the user's local calendar day (a daily-spend tool dated in UTC is
/// wrong near local midnight). The offset enters only at the date boundary; the cost path stays a
/// pure function of the stored date — no clock or timezone is read inside core accounting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz_offset_minutes: Option<i64>,
}

impl TareConfig {
    /// Parse and validate a `tare.toml` string. Unknown keys are rejected so a misspelled budget
    /// or privacy control cannot silently fall back to a less restrictive default.
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(s).map_err(|e| format!("tare.toml: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    /// Serialize to a `tare.toml` string (only the set fields, via skip_serializing_if).
    pub fn to_toml_string(&self) -> Result<String, String> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(|e| format!("serialize config: {e}"))
    }

    /// Validate semantic constraints that serde's field types cannot express.
    pub fn validate(&self) -> Result<(), String> {
        if self.proxy.port == Some(0) || self.proxy.otlp_port == Some(0) {
            return Err("tare.toml: proxy ports must be between 1 and 65535".into());
        }
        for (field, value) in [
            ("budget.max_spend_usd", self.budget.max_spend_usd),
            ("budget.soft_spend_usd", self.budget.soft_spend_usd),
            (
                "budget.period_max_spend_usd",
                self.budget.period_max_spend_usd,
            ),
        ] {
            if let Some(value) = value {
                validate_budget_dollars(field, value)?;
            }
        }
        if let (Some(max), Some(soft)) = (
            self.budget.max_spend_usd.and_then(dollars_to_micros),
            self.budget.soft_spend_usd.and_then(dollars_to_micros),
        ) {
            if max > 0 && soft > max {
                return Err(
                    "tare.toml: budget.soft_spend_usd must not exceed max_spend_usd".into(),
                );
            }
        }
        if let Some(period) = &self.budget.period {
            if !matches!(period.as_str(), "week" | "month") {
                return Err("tare.toml: budget.period must be \"week\" or \"month\"".into());
            }
        }
        if self
            .budget
            .warn_pct
            .is_some_and(|value| !(1..=100).contains(&value))
        {
            return Err("tare.toml: budget.warn_pct must be between 1 and 100".into());
        }

        let mut overlay_backends = std::collections::BTreeSet::new();
        for (index, overlay) in self.providers.local_overlay.iter().enumerate() {
            if overlay.backend.trim().is_empty() {
                return Err(format!(
                    "tare.toml: providers.local_overlay[{index}].backend must not be empty"
                ));
            }
            if !overlay_backends.insert(overlay.backend.as_str()) {
                return Err(format!(
                    "tare.toml: duplicate local-overlay backend {:?}",
                    overlay.backend
                ));
            }
            validate_optional_nonnegative(
                &format!("providers.local_overlay[{index}].usd_per_mtok"),
                overlay.usd_per_mtok,
                true,
            )?;
            validate_optional_nonnegative(
                &format!("providers.local_overlay[{index}].kwh_per_mtok"),
                overlay.kwh_per_mtok,
                false,
            )?;
            validate_optional_nonnegative(
                &format!("providers.local_overlay[{index}].usd_per_kwh"),
                overlay.usd_per_kwh,
                false,
            )?;
            if let (Some(kwh), Some(rate)) = (overlay.kwh_per_mtok, overlay.usd_per_kwh) {
                if dollars_to_micros(kwh * rate).is_none() {
                    return Err(format!(
                        "tare.toml: providers.local_overlay[{index}] energy price is out of range"
                    ));
                }
            }
        }

        let mut overridden_models = std::collections::BTreeSet::new();
        for (index, model) in self.pricing.overrides.iter().enumerate() {
            if model.model.trim().is_empty() {
                return Err(format!(
                    "tare.toml: pricing.overrides[{index}].model must not be empty"
                ));
            }
            if !overridden_models.insert(model.model.as_str()) {
                return Err(format!(
                    "tare.toml: duplicate pricing override for {:?}",
                    model.model
                ));
            }
            for (field, value) in [
                ("input_usd_per_mtok", Some(model.input_usd_per_mtok)),
                ("output_usd_per_mtok", Some(model.output_usd_per_mtok)),
                ("cache_read_usd_per_mtok", model.cache_read_usd_per_mtok),
            ] {
                validate_optional_nonnegative(
                    &format!("pricing.overrides[{index}].{field}"),
                    value,
                    true,
                )?;
            }
        }

        if self.anomaly.window == Some(0) {
            return Err("tare.toml: anomaly.window must be at least 1".into());
        }
        if self.anomaly.threshold.is_some_and(|value| value < 0) {
            return Err("tare.toml: anomaly.threshold must not be negative".into());
        }
        if self
            .anomaly
            .dollar_floor_micros
            .is_some_and(|value| value < 0)
        {
            return Err("tare.toml: anomaly.dollar_floor_micros must not be negative".into());
        }
        if self
            .anomaly
            .pct_of_daily_floor
            .is_some_and(|value| !(0..=100).contains(&value))
        {
            return Err("tare.toml: anomaly.pct_of_daily_floor must be between 0 and 100".into());
        }
        if let Some(key) = self
            .anomaly
            .acknowledged
            .iter()
            .find(|key| key.is_empty() || key.len() > 256 || key.chars().any(char::is_control))
        {
            return Err(format!(
                "tare.toml: acknowledged anomaly key must be 1-256 bytes (got {})",
                key.len()
            ));
        }

        if self
            .ui
            .tz_offset_minutes
            .is_some_and(|value| !(-840..=840).contains(&value))
        {
            return Err("tare.toml: ui.tz_offset_minutes must be between -840 and 840".into());
        }

        for (index, rule) in self.alert.iter().enumerate() {
            if !matches!(
                rule.metric.as_str(),
                "today_spend" | "period_pct" | "run_rate" | "anomaly_kind"
            ) {
                return Err(format!(
                    "tare.toml: alert[{index}].metric is not a supported metric"
                ));
            }
            if rule.metric != "anomaly_kind" && rule.threshold.is_none() {
                return Err(format!(
                    "tare.toml: alert[{index}].threshold is required for {}",
                    rule.metric
                ));
            }
            if let Some(value) = rule.threshold {
                validate_nonnegative_finite(&format!("alert[{index}].threshold"), value)?;
                if matches!(rule.metric.as_str(), "today_spend" | "run_rate")
                    && dollars_to_micros(value).is_none()
                {
                    return Err(format!(
                        "tare.toml: alert[{index}].threshold is out of micro-USD range"
                    ));
                }
            }
            if rule.metric == "anomaly_kind"
                && !matches!(
                    rule.kind.as_deref(),
                    Some("spike" | "new_series" | "vanished_series" | "any")
                )
            {
                return Err(format!(
                    "tare.toml: alert[{index}].kind must name an anomaly kind"
                ));
            }
            if rule.metric == "anomaly_kind" && rule.threshold.is_some() {
                return Err(format!(
                    "tare.toml: alert[{index}].threshold is not valid for anomaly_kind"
                ));
            }
            if rule.metric != "anomaly_kind" && rule.kind.is_some() {
                return Err(format!(
                    "tare.toml: alert[{index}].kind is only valid for anomaly_kind"
                ));
            }
            if rule.metric != "anomaly_kind" && rule.window_days.is_some() {
                return Err(format!(
                    "tare.toml: alert[{index}].window_days is only valid for anomaly_kind"
                ));
            }
            if rule
                .window_days
                .is_some_and(|days| !(1..=3650).contains(&days))
            {
                return Err(format!(
                    "tare.toml: alert[{index}].window_days must be between 1 and 3650"
                ));
            }
        }

        let mut lineage_names = std::collections::BTreeSet::new();
        for (index, lineage) in self.lineage.iter().enumerate() {
            validate_config_name(&format!("lineage[{index}].name"), &lineage.name)?;
            if !lineage_names.insert(lineage.name.as_str()) {
                return Err(format!(
                    "tare.toml: duplicate lineage name {:?}",
                    lineage.name
                ));
            }
            let mut labels = std::collections::BTreeSet::new();
            for (version_index, version) in lineage.versions.iter().enumerate() {
                validate_config_name(
                    &format!("lineage[{index}].versions[{version_index}].label"),
                    &version.label,
                )?;
                if !labels.insert(version.label.as_str()) {
                    return Err(format!(
                        "tare.toml: duplicate version label {:?} in lineage {:?}",
                        version.label, lineage.name
                    ));
                }
            }
        }

        let mut unit_names = std::collections::BTreeSet::new();
        for (index, unit) in self.unit.iter().enumerate() {
            validate_config_name(&format!("unit[{index}].name"), &unit.name)?;
            if !unit_names.insert(unit.name.as_str()) {
                return Err(format!("tare.toml: duplicate unit name {:?}", unit.name));
            }
            if unit.match_.run_prefix.as_deref() == Some("") {
                return Err(format!(
                    "tare.toml: unit[{index}].match.run_prefix must not be empty"
                ));
            }
            if unit
                .match_
                .sessions
                .iter()
                .chain(&unit.match_.commits)
                .any(|value| value.is_empty() || value.chars().any(char::is_control))
            {
                return Err(format!(
                    "tare.toml: unit[{index}] match values must not be empty or contain control characters"
                ));
            }
        }
        Ok(())
    }

    /// Load from a path, or the default config if the file is absent. A present-but-unparseable
    /// file is an error (never silently ignored).
    pub fn load(path: &str) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml_str(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("read {path}: {e}")),
        }
    }

    /// Validate and atomically replace a config file. The temporary file lives beside the target,
    /// so the final rename cannot cross filesystems; a failed write leaves the previous config
    /// intact. Fresh files inherit the temporary file's private permissions.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let contents = self.to_toml_string()?;
        let target = std::path::Path::new(path);
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create config directory {}: {error}", parent.display()))?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".tare-config-")
            .tempfile_in(parent)
            .map_err(|error| format!("create temporary config in {}: {error}", parent.display()))?;
        temporary
            .write_all(contents.as_bytes())
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|error| format!("write temporary config for {path}: {error}"))?;
        let persisted = temporary
            .persist(target)
            .map_err(|error| format!("replace {path}: {}", error.error))?;
        persisted
            .sync_all()
            .map_err(|error| format!("sync {path}: {error}"))?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync config directory {}: {error}", parent.display()))?;
        Ok(())
    }

    /// Merge an incoming config JSON — from a UI that may model only SOME sections — over `self`,
    /// section by section: every top-level key PRESENT in the payload replaces that section
    /// wholesale (the client owns what it sends), and every section the payload OMITS is preserved
    /// from `self`.
    ///
    /// This exists because saving is a whole-file rewrite and every field is `#[serde(default)]`,
    /// so deserializing a partial payload straight into `TareConfig` silently ZEROES the sections
    /// the client didn't model. That is how a Settings save (or finishing onboarding) used to delete
    /// `[[unit]]` and `[[lineage]]` from the user's tare.toml — the sections that drive
    /// `tare unit` / `tare lineage`. Merging here makes that class of data loss
    /// structurally impossible for EVERY client, present and future, rather than relying on each
    /// UI to remember to round-trip a section it never edits.
    ///
    /// Pure: no I/O. The caller loads, merges, then writes.
    pub fn merge_json(&self, incoming: &str) -> Result<Self, String> {
        self.validate()?;
        let patch: serde_json::Value =
            serde_json::from_str(incoming).map_err(|e| format!("config json: {e}"))?;
        let patch = patch
            .as_object()
            .ok_or_else(|| "config json: expected a JSON object".to_string())?;
        let mut merged = serde_json::to_value(self).map_err(|e| format!("config merge: {e}"))?;
        let obj = merged
            .as_object_mut()
            .ok_or_else(|| "config merge: expected a JSON object".to_string())?;
        for (key, value) in patch {
            obj.insert(key.clone(), value.clone());
        }
        // Validating here means a malformed payload can never reach the caller's write.
        let config: Self =
            serde_json::from_value(merged).map_err(|e| format!("config json: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    /// Append an anomaly key to `[anomaly].acknowledged` in the tare.toml at `path` (idempotent),
    /// creating the file if absent, so `detect()` hides that false positive going forward.
    /// Bounded key length; the TOML serializer escapes the value. Path-parameterized
    /// so the loopback write API, the desktop command, and tests all share one implementation.
    pub fn acknowledge_anomaly(path: &str, key: &str) -> Result<(), String> {
        if key.is_empty() || key.len() > 256 {
            return Err("anomaly key must be 1-256 chars".to_string());
        }
        let mut cfg = Self::load(path)?;
        if !cfg.anomaly.acknowledged.iter().any(|k| k == key) {
            cfg.anomaly.acknowledged.push(key.to_string());
        }
        cfg.save(path)
    }
}

fn validate_budget_dollars(field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("tare.toml: {field} must be finite"));
    }
    if value > 0.0 && dollars_to_micros(value).is_none() {
        return Err(format!(
            "tare.toml: {field} is out of signed 64-bit micro-USD range"
        ));
    }
    Ok(())
}

fn validate_nonnegative_finite(field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() || value < 0.0 {
        return Err(format!(
            "tare.toml: {field} must be finite and non-negative"
        ));
    }
    Ok(())
}

fn validate_config_name(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(format!(
            "tare.toml: {field} must not be empty or contain control characters"
        ));
    }
    Ok(())
}

fn validate_optional_nonnegative(
    field: &str,
    value: Option<f64>,
    must_fit_micros: bool,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    validate_nonnegative_finite(field, value)?;
    if must_fit_micros && dollars_to_micros(value).is_none() {
        return Err(format!(
            "tare.toml: {field} is out of signed 64-bit micro-USD range"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_mode_defaults_to_app_only_and_parses_the_toml_enum() {
        // absent [capture] → app_only (privacy-friendly default). An explicit mode parses.
        assert_eq!(TareConfig::default().capture.mode, CaptureMode::AppOnly);
        let cfg: TareConfig = toml::from_str("[capture]\nmode = \"always_on\"\n").unwrap();
        assert_eq!(cfg.capture.mode, CaptureMode::AlwaysOn);
        assert_eq!(cfg.capture.mode.as_str(), "always_on");
        // Round-trips through the serialized form.
        let back: TareConfig = toml::from_str(&cfg.to_toml_string().unwrap()).unwrap();
        assert_eq!(back.capture.mode, CaptureMode::AlwaysOn);
        let off: TareConfig = toml::from_str("[capture]\nmode = \"off\"\n").unwrap();
        assert_eq!(off.capture.mode, CaptureMode::Off);
    }

    #[test]
    fn capture_jsonl_defaults_on_and_is_independently_toggleable() {
        // JSONL lane is on by default (no config, or [capture] without the key).
        assert!(TareConfig::default().capture.jsonl);
        let implicit: TareConfig = toml::from_str("[capture]\nmode = \"app_only\"\n").unwrap();
        assert!(implicit.capture.jsonl);
        // Explicitly off — independent of mode (mode stays always_on, JSONL is off).
        let cfg: TareConfig =
            toml::from_str("[capture]\nmode = \"always_on\"\njsonl = false\n").unwrap();
        assert!(!cfg.capture.jsonl);
        assert_eq!(cfg.capture.mode, CaptureMode::AlwaysOn);
        // Round-trips through the serialized form.
        let back: TareConfig = toml::from_str(&cfg.to_toml_string().unwrap()).unwrap();
        assert!(!back.capture.jsonl);
    }

    #[test]
    fn service_action_installs_only_for_always_on_and_removes_otherwise() {
        // always_on wants a login item; app_only/off must remove one if present so
        // switching away never leaves a daemon running at login. No redundant install/uninstall.
        assert_eq!(
            CaptureMode::AlwaysOn.service_action(false),
            ServiceAction::Install
        );
        assert_eq!(
            CaptureMode::AlwaysOn.service_action(true),
            ServiceAction::Leave
        );
        assert_eq!(
            CaptureMode::AppOnly.service_action(true),
            ServiceAction::Uninstall
        );
        assert_eq!(
            CaptureMode::AppOnly.service_action(false),
            ServiceAction::Leave
        );
        assert_eq!(
            CaptureMode::Off.service_action(true),
            ServiceAction::Uninstall
        );
        assert_eq!(CaptureMode::Off.service_action(false), ServiceAction::Leave);
    }

    #[test]
    fn anomaly_noise_filters_map_and_round_trip() {
        // [anomaly] floors parse, map to NoiseFilters, and survive a serialize round-trip
        // alongside the acknowledged list.
        let toml = r#"
[anomaly]
window = 7
threshold = 50
acknowledged = ["2026-06-25:total:spike"]
dollar_floor_micros = 1000000
pct_of_daily_floor = 5
dedupe_window_days = 2
"#;
        let cfg = TareConfig::from_toml_str(toml).unwrap();
        let nf = cfg.anomaly.noise_filters();
        assert_eq!(nf.dollar_floor_micros, 1_000_000);
        assert_eq!(nf.pct_of_daily_floor, 5);
        assert_eq!(nf.dedupe_window_days, 2);
        // Full round-trip preserves both the floors and the acknowledged list.
        let back = TareConfig::from_toml_str(&cfg.to_toml_string().unwrap()).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.anomaly.acknowledged, vec!["2026-06-25:total:spike"]);
        // Default (no [anomaly] tuning) is fully permissive → byte-identical anomaly output.
        assert_eq!(
            TareConfig::default().anomaly.noise_filters(),
            crate::anomaly::NoiseFilters::default()
        );
    }

    #[test]
    fn local_overlay_resolves_direct_and_energy_rates() {
        // Direct $/Mtok.
        let direct = LocalOverlay {
            backend: "ollama".into(),
            usd_per_mtok: Some(0.5),
            ..Default::default()
        };
        assert_eq!(direct.micro_per_mtok(), Some(500_000));
        // Energy estimate: kWh/Mtok × $/kWh.
        let energy = LocalOverlay {
            backend: "vllm".into(),
            kwh_per_mtok: Some(2.0),
            usd_per_kwh: Some(0.15),
            ..Default::default()
        };
        assert_eq!(energy.micro_per_mtok(), Some(300_000));
        // Direct wins when both are present.
        let both = LocalOverlay {
            backend: "x".into(),
            usd_per_mtok: Some(1.0),
            kwh_per_mtok: Some(9.0),
            usd_per_kwh: Some(9.0),
        };
        assert_eq!(both.micro_per_mtok(), Some(1_000_000));
        // No rate, partial energy, and negative → None.
        assert_eq!(
            LocalOverlay {
                backend: "x".into(),
                ..Default::default()
            }
            .micro_per_mtok(),
            None
        );
        assert_eq!(
            LocalOverlay {
                backend: "x".into(),
                kwh_per_mtok: Some(2.0),
                ..Default::default()
            }
            .micro_per_mtok(),
            None
        );
        assert_eq!(
            LocalOverlay {
                backend: "x".into(),
                usd_per_mtok: Some(-1.0),
                ..Default::default()
            }
            .micro_per_mtok(),
            None
        );
        assert_eq!(
            LocalOverlay {
                backend: "x".into(),
                kwh_per_mtok: Some(f64::MAX),
                usd_per_kwh: Some(f64::MAX),
                ..Default::default()
            }
            .micro_per_mtok(),
            None
        );
    }

    #[test]
    fn dollar_conversion_rejects_nonfinite_and_out_of_range_values() {
        assert_eq!(dollars_to_micros(0.000_000_6), Some(1));
        assert_eq!(dollars_to_micros(-1.0), None);
        assert_eq!(dollars_to_micros(f64::NAN), None);
        assert_eq!(dollars_to_micros(f64::INFINITY), None);
        assert_eq!(dollars_to_micros(f64::MAX), None);
        assert_eq!(dollars_to_micros((i64::MAX as f64) / 1_000_000.0), None);

        let unsafe_budget = BudgetSettings {
            max_spend_usd: Some(f64::INFINITY),
            soft_spend_usd: Some(f64::NAN),
            ..Default::default()
        };
        assert_eq!(unsafe_budget.to_budget().max_micros, None);
        assert_eq!(unsafe_budget.to_budget().soft_micros, None);
    }

    #[test]
    fn acknowledge_anomaly_appends_idempotently_to_a_temp_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tare.toml");
        let p = path.to_str().unwrap();
        let key = "2026-06-20:claude-opus-4-8:spike";
        TareConfig::acknowledge_anomaly(p, key).unwrap();
        assert_eq!(TareConfig::load(p).unwrap().anomaly.acknowledged, vec![key]);
        // Idempotent — re-acknowledging doesn't duplicate.
        TareConfig::acknowledge_anomaly(p, key).unwrap();
        assert_eq!(TareConfig::load(p).unwrap().anomaly.acknowledged.len(), 1);
        // A second, distinct key appends.
        TareConfig::acknowledge_anomaly(p, "2026-06-21:gpt-5:spike").unwrap();
        assert_eq!(TareConfig::load(p).unwrap().anomaly.acknowledged.len(), 2);
        // Bad key rejected.
        assert!(TareConfig::acknowledge_anomaly(p, "").is_err());
    }

    #[test]
    fn save_atomically_replaces_config_and_preserves_all_privacy_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/tare.toml");
        let path_string = path.to_string_lossy();
        let mut cfg = TareConfig::default();
        cfg.budget.soft_spend_usd = Some(0.25);
        cfg.privacy.suppress_latency = true;
        cfg.privacy.git_attribution = true;
        cfg.save(&path_string).unwrap();

        let loaded = TareConfig::load(&path_string).unwrap();
        assert_eq!(loaded.budget.soft_spend_usd, Some(0.25));
        assert!(loaded.privacy.suppress_latency);
        assert!(loaded.privacy.git_attribution);
        assert!(std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tare-config-")));
    }

    #[test]
    fn round_trips_and_converts_dollars_to_micros() {
        let toml = r#"
            [budget]
            max_spend_usd = 0.5
            max_steps = 100

            [privacy]
            profile = "max_private"

            [providers]
            anthropic_upstream = "https://example.test"
            gemini_upstream = "https://gem.test"
            azure_openai_upstream = "https://az.test"
            bedrock_upstream = "https://br.test"

            [proxy]
            port = 8790
            otlp_port = 4319
            pricing = "local.toml"

            [anomaly]
            window = 14
            threshold = 30

            [ui]
            tz_offset_minutes = -480
        "#;
        let cfg = TareConfig::from_toml_str(toml).unwrap();
        assert_eq!(cfg.budget.to_budget().max_micros, Some(500_000));
        assert_eq!(cfg.budget.max_steps, Some(100));
        assert_eq!(cfg.privacy.profile, Some(Profile::MaxPrivate));
        assert_eq!(
            cfg.providers.anthropic_upstream.as_deref(),
            Some("https://example.test")
        );
        assert_eq!(
            cfg.providers.gemini_upstream.as_deref(),
            Some("https://gem.test")
        );
        assert_eq!(
            cfg.providers.azure_openai_upstream.as_deref(),
            Some("https://az.test")
        );
        assert_eq!(
            cfg.providers.bedrock_upstream.as_deref(),
            Some("https://br.test")
        );
        assert_eq!(cfg.proxy.port, Some(8790));
        assert_eq!(cfg.proxy.otlp_port, Some(4319));
        assert_eq!(cfg.proxy.pricing.as_deref(), Some("local.toml"));
        assert_eq!(cfg.anomaly.window, Some(14));
        assert_eq!(cfg.anomaly.threshold, Some(30));
        assert_eq!(cfg.ui.tz_offset_minutes, Some(-480));

        // Re-serialize -> re-parse is stable.
        let out = cfg.to_toml_string().unwrap();
        assert_eq!(TareConfig::from_toml_str(&out).unwrap(), cfg);
    }

    #[test]
    fn empty_is_default_and_absent_file_loads_default() {
        assert_eq!(
            TareConfig::from_toml_str("").unwrap(),
            TareConfig::default()
        );
        assert_eq!(
            TareConfig::load("/no/such/tare.toml.xyz").unwrap(),
            TareConfig::default()
        );
        // A default budget describes no caps.
        assert_eq!(TareConfig::default().budget.to_budget().max_micros, None);
    }

    #[test]
    fn nonpositive_budget_is_ignored_not_a_block_everything_cap() {
        // A zero or negative spend cap would make every request exceed budget and block
        // all traffic — treat it as "no cap" instead of bricking the proxy.
        let zero = TareConfig::from_toml_str("[budget]\nmax_spend_usd = 0\n").unwrap();
        assert_eq!(zero.budget.to_budget().max_micros, None);
        let neg = TareConfig::from_toml_str("[budget]\nmax_spend_usd = -5.0\n").unwrap();
        assert_eq!(neg.budget.to_budget().max_micros, None);
        // A real positive cap still resolves.
        let ok = TareConfig::from_toml_str("[budget]\nmax_spend_usd = 5.0\n").unwrap();
        assert_eq!(ok.budget.to_budget().max_micros, Some(5_000_000));
    }

    #[test]
    fn rejects_semantically_invalid_config_values() {
        for invalid in [
            "[budget]\nmax_spnd_usd = 1\n",
            "[budget]\nmax_spend_usd = inf\n",
            "[budget]\nmax_spend_usd = 1e30\n",
            "[budget]\nmax_spend_usd = 1\nsoft_spend_usd = 2\n",
            "[budget]\nperiod = \"quarter\"\n",
            "[budget]\nwarn_pct = 101\n",
            "[anomaly]\nwindow = 0\n",
            "[anomaly]\nthreshold = -1\n",
            "[anomaly]\ndollar_floor_micros = -1\n",
            "[anomaly]\npct_of_daily_floor = 101\n",
            "[ui]\ntz_offset_minutes = 841\n",
            "[proxy]\nport = 0\n",
            "[[providers.local_overlay]]\nbackend = \"x\"\nusd_per_mtok = nan\n",
            "[[providers.local_overlay]]\nbackend = \"x\"\n[[providers.local_overlay]]\nbackend = \"x\"\n",
            "[[pricing.overrides]]\nmodel = \"x\"\ninput_usd_per_mtok = -1\noutput_usd_per_mtok = 1\n",
            "[[pricing.overrides]]\nmodel = \"x\"\ninput_usd_per_mtok = 1\noutput_usd_per_mtok = 1\n[[pricing.overrides]]\nmodel = \"x\"\ninput_usd_per_mtok = 2\noutput_usd_per_mtok = 2\n",
            "[[alert]]\nmetric = \"unknown\"\nthreshold = 1\n",
            "[[alert]]\nmetric = \"today_spend\"\n",
            "[[alert]]\nmetric = \"anomaly_kind\"\nkind = \"unknown\"\n",
            "[[alert]]\nmetric = \"today_spend\"\nthreshold = 1\nwindow_days = 7\n",
            "[[alert]]\nmetric = \"anomaly_kind\"\nkind = \"any\"\nwindow_days = 0\n",
            "[[unit]]\nname = \"x\"\n[unit.match]\nrun_prefix = \"\"\n",
            "[[lineage]]\nname = \"x\"\n[[lineage]]\nname = \"x\"\n",
        ] {
            assert!(
                TareConfig::from_toml_str(invalid).is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }

        let base = TareConfig::default();
        assert!(base
            .merge_json(r#"{"ui":{"tz_offset_minutes":-841}}"#)
            .is_err());
    }

    #[test]
    fn unparseable_present_file_is_an_error() {
        assert!(TareConfig::from_toml_str("this is = = not toml").is_err());
    }

    /// Regression guard for Settings-shaped payloads that previously caused data loss: a payload that
    /// models only the sections the UI edits must NEVER delete the ones it doesn't. Before
    /// `merge_json`, saving this exact payload wiped `[[unit]]`, `[[lineage]]`, `[[alert]]`,
    /// `[capture]` and `[ui]` off the user's disk, because every field is `#[serde(default)]` and
    /// the save is a whole-file rewrite.
    #[test]
    fn merge_json_preserves_sections_the_payload_does_not_model() {
        let on_disk = TareConfig::from_toml_str(
            r#"
[budget]
max_spend_usd = 10.0

[capture]
mode = "always_on"

[ui]
tz_offset_minutes = -420

[[unit]]
name = "pull-request"

[[lineage]]
name = "system-prompt"

[[alert]]
metric = "today_spend"
threshold = 5.0
"#,
        )
        .unwrap();
        assert_eq!(on_disk.unit.len(), 1);
        assert_eq!(on_disk.lineage.len(), 1);

        // Exactly what the Settings sheet sends: the sections it edits, and nothing else.
        let merged = on_disk
            .merge_json(r#"{"budget":{"max_spend_usd":25.0},"privacy":{"profile":"max_private"}}"#)
            .unwrap();

        // The edited section wins wholesale...
        assert_eq!(merged.budget.max_spend_usd, Some(25.0));
        assert_eq!(
            merged.privacy.profile,
            Some(crate::privacy::Profile::MaxPrivate)
        );
        // ...and every unmodelled section survives.
        assert_eq!(
            merged.unit.len(),
            1,
            "[[unit]] must survive a Settings save"
        );
        assert_eq!(merged.unit[0].name, "pull-request");
        assert_eq!(
            merged.lineage.len(),
            1,
            "[[lineage]] must survive a Settings save"
        );
        assert_eq!(merged.lineage[0].name, "system-prompt");
        assert_eq!(merged.alert.len(), 1, "[[alert]] must survive");
        assert_eq!(merged.capture.mode, CaptureMode::AlwaysOn);
        assert_eq!(merged.ui.tz_offset_minutes, Some(-420));
    }

    #[test]
    fn merge_json_lets_a_client_clear_a_section_it_does_model() {
        // Sending a section explicitly replaces it wholesale — that is how a UI removes an entry.
        let on_disk = TareConfig::from_toml_str("[[unit]]\nname = \"pull-request\"\n").unwrap();
        let merged = on_disk.merge_json(r#"{"unit":[]}"#).unwrap();
        assert!(merged.unit.is_empty());
    }

    #[test]
    fn merge_json_rejects_malformed_payloads_without_touching_the_base() {
        let on_disk = TareConfig::from_toml_str("[[unit]]\nname = \"u\"\n").unwrap();
        assert!(on_disk.merge_json("not json").is_err());
        assert!(on_disk.merge_json("[1,2,3]").is_err(), "must be an object");
        assert!(
            on_disk
                .merge_json(r#"{"budget":{"max_spend_usd":"not a number"}}"#)
                .is_err(),
            "a type error must fail validation, not land on disk"
        );
        // The base is untouched either way (merge_json is pure).
        assert_eq!(on_disk.unit.len(), 1);
    }
}
